//! `marlin-cookd`: the cook service (`docs/design/0003-asset-pipeline.md` §6).
//!
//! Runs on each workstation next to the editor and the game. It holds the project's asset index,
//! cooks units on demand (a cache hit in the local or shared DDC costs a read; otherwise worker
//! threads cook), and serves them to games over the dev protocol (`marlin_asset::dev`). It
//! watches the project: when a unit a client was served has a new cook key (its text, a source
//! file, a type or a link target's unit changed), the client is told, and its asset server
//! reloads the unit.
//!
//! Requests for the same unit coalesce: ten clients asking for one texture cook it once.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use marlin_asset::cooked::unit_id;
use marlin_asset::dev::*;
use marlin_asset::wire::{Frame, Reader, Writer};
use marlin_asset::{Uuid, name_hash};
use marlin_asset_db::{Content, ContentFile};
use marlin_cook_core::{
    Cookers, Ddc, Platform, Profile, fetch_unit, named_objects, source_units, unit_key,
};
use marlin_store::{ConnId, Outbox, Server, ServerError, Service};

/// The project as the service sees it: every mount, the engine's included, so engine content
/// (its shaders) is served and reloaded like the game's.
struct Project {
    content: Content,
    /// Unit id → its `.masset` file.
    units: HashMap<u64, ContentFile>,
    /// Every object and its unit, and the named objects.
    table: ObjectTable,
}

impl Project {
    fn new(content: Content, cookers: &Cookers) -> Self {
        let mut p = Self {
            content,
            units: HashMap::new(),
            table: ObjectTable::default(),
        };
        p.reindex(cookers);
        p
    }

    fn reindex(&mut self, cookers: &Cookers) {
        let content = &self.content;
        self.units = source_units(content)
            .into_iter()
            .map(|f| (unit_id(&content.path(f)), f))
            .collect();
        // Bottom mount first, sorted stably: a UUID two mounts use stays with the lower one.
        let mut objects: Vec<(Uuid, u64)> = content
            .objects()
            .map(|o| {
                let uuid = content.object_info(o).uuid;
                (uuid, unit_id(&content.path(content.object_file(o))))
            })
            .collect();
        objects.sort_by_key(|o| o.0);
        objects.dedup_by_key(|o| o.0);
        // A name given twice names nothing (the cook reports it); the rest are served.
        let mut names: Vec<(u64, Uuid)> = named_objects(&self.content, cookers)
            .0
            .into_iter()
            .map(|(name, uuid)| (name_hash(&name), uuid))
            .collect();
        names.sort_unstable();
        self.table = ObjectTable { objects, names };
    }
}

/// What cookd cooks for: games under development.
const PROFILE: Profile = Profile::Development;

struct Shared {
    project: RwLock<Project>,
    cookers: Cookers,
    ddc: Box<dyn Ddc>,
}

struct Job {
    unit: u64,
    platform: &'static Platform,
}

struct Done {
    unit: u64,
    platform: &'static Platform,
    /// The bytes and their key, or why not and the key the attempt had (if it got one).
    result: Result<(u128, Vec<u8>), (String, Option<u128>)>,
}

struct Client {
    platform: Option<&'static Platform>,
    /// Units served, with the key they were served at: `None` for a failure without one (a unit
    /// that does not parse). Failures count, so fixing them tells the client too.
    served: HashMap<u64, Option<u128>>,
}

/// Settings of a [`Cookd`].
#[derive(Clone, Debug)]
pub struct CookdOptions {
    /// Cook threads; 0: one per logical CPU.
    pub workers: usize,
    /// How often the project is checked for changes.
    pub watch_interval: Duration,
}

impl Default for CookdOptions {
    fn default() -> Self {
        Self {
            workers: 0,
            watch_interval: Duration::from_millis(250),
        }
    }
}

/// The service: the [`Service`] a [`Server`] runs.
pub struct Cookd {
    shared: Arc<Shared>,
    jobs: Option<Sender<Job>>,
    done: Receiver<Done>,
    workers: Vec<JoinHandle<()>>,
    clients: HashMap<ConnId, Client>,
    /// (unit, platform) being cooked → who waits for it.
    in_flight: HashMap<(u64, &'static str), Vec<(ConnId, u32)>>,
    /// A second index of the project, owned by the network thread: the watch updates it outside
    /// any lock and swaps it in only when it changed (see [`watch`](Self::watch)). `None` when
    /// it could not be opened; the watch then updates the served index in place.
    spare: Option<Content>,
    /// The spare saw changes the served index has not: swap at the next check regardless.
    spare_ahead: bool,
    watch_interval: Duration,
    last_watch: Instant,
}

impl Cookd {
    pub fn new(
        content: Content,
        cookers: Cookers,
        ddc: Box<dyn Ddc>,
        options: &CookdOptions,
    ) -> Self {
        let spare = content.reopen().ok();
        let shared = Arc::new(Shared {
            project: RwLock::new(Project::new(content, &cookers)),
            cookers,
            ddc,
        });
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done) = mpsc::channel::<Done>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let count = match options.workers {
            0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
            n => n,
        };
        let workers = (0..count)
            .map(|i| {
                let (shared, job_rx, done_tx) = (shared.clone(), job_rx.clone(), done_tx.clone());
                std::thread::Builder::new()
                    .name(format!("cookd-worker-{i}"))
                    .spawn(move || worker(&shared, &job_rx, &done_tx))
                    .expect("spawning a cook worker")
            })
            .collect();
        Self {
            shared,
            jobs: Some(jobs),
            done,
            workers,
            clients: HashMap::new(),
            in_flight: HashMap::new(),
            spare,
            spare_ahead: false,
            watch_interval: options.watch_interval,
            last_watch: Instant::now(),
        }
    }

    pub fn object_count(&self) -> usize {
        self.shared
            .project
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .table
            .objects
            .len()
    }

    /// Serves on `server` until `stop` is set.
    pub fn serve(&mut self, server: &mut Server, stop: &AtomicBool) -> Result<(), ServerError> {
        server.run(self, stop, Duration::from_millis(20))
    }

    /// Checks the project for changes now and tells clients about units that changed.
    ///
    /// Workers hold the project's read lock for whole cooks, so taking the write lock waits for
    /// them, stalling the network loop meanwhile. So the check runs on the spare index, outside
    /// the lock, and only a change takes the write lock, just to swap the indexes. The index
    /// swapped out is then brought up to date, again outside the lock; if that saw changes made
    /// since (it now differs from the served one), the next check swaps again.
    fn watch(&mut self, out: &mut Outbox) {
        fn changed(update: marlin_asset_db::Result<marlin_asset_db::UpdateReport>) -> bool {
            update.is_ok_and(|r| r.added + r.changed + r.removed > 0)
        }
        let objects_moved = match self.spare.take() {
            Some(mut spare) => {
                if !changed(spare.update()) && !self.spare_ahead {
                    self.spare = Some(spare);
                    return;
                }
                let mut next = Project::new(spare, &self.shared.cookers);
                let mut project = self
                    .shared
                    .project
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let moved = next.table != project.table;
                std::mem::swap(&mut *project, &mut next);
                drop(project);
                let mut old = next.content;
                let _ = old.update();
                let project = self
                    .shared
                    .project
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                self.spare_ahead = !same_files(&old, &project.content);
                drop(project);
                self.spare = Some(old);
                moved
            }
            None => {
                let mut project = self
                    .shared
                    .project
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                if !changed(project.content.update()) {
                    return;
                }
                let old = std::mem::take(&mut project.table);
                project.reindex(&self.shared.cookers);
                old != project.table
            }
        };
        let project = self
            .shared
            .project
            .read()
            .unwrap_or_else(|e| e.into_inner());
        for (&conn, client) in &mut self.clients {
            let Some(platform) = client.platform else {
                continue;
            };
            let mut stale = Vec::new();
            for (&unit, &key) in &client.served {
                let now = project.units.get(&unit).and_then(|&f| {
                    unit_key(&project.content, &self.shared.cookers, f, platform, PROFILE).ok()
                });
                if now != key {
                    stale.push((unit, now));
                }
            }
            if stale.is_empty() && !objects_moved {
                continue;
            }
            // Remember what the client was told, not what it fetched: if its reload fails on its
            // side (the unit vanished from the table while it did not parse), fixing the unit
            // still changes the key and tells it again.
            for &(unit, now) in &stale {
                client.served.insert(unit, now);
            }
            let mut stale: Vec<u64> = stale.into_iter().map(|(u, _)| u).collect();
            stale.sort_unstable();
            let table = objects_moved.then_some(&project.table);
            out.send(conn, CHANGED, 0, &changed_payload(&stale, table));
        }
    }
}

/// Whether two copies of a project's indexes saw the same files, by path and contents.
fn same_files(a: &Content, b: &Content) -> bool {
    a.mount_ids().all(|m| {
        let (a, b) = (a.db(m), b.db(m));
        a.files().count() == b.files().count()
            && a.files().all(|f| {
                let info = a.file_info(f);
                b.file(&info.path)
                    .is_some_and(|g| b.file_info(g).hash == info.hash)
            })
    })
}

fn worker(shared: &Shared, jobs: &Mutex<Receiver<Job>>, done: &Sender<Done>) {
    loop {
        let job = match jobs.lock().unwrap_or_else(|e| e.into_inner()).recv() {
            Ok(job) => job,
            Err(_) => return, // the service is gone
        };
        let result = {
            let project = shared.project.read().unwrap_or_else(|e| e.into_inner());
            match project.units.get(&job.unit) {
                None => Err((format!("no unit {:#018x} in the project", job.unit), None)),
                Some(&file) => fetch_unit(
                    &project.content,
                    &shared.cookers,
                    shared.ddc.as_ref(),
                    job.platform,
                    PROFILE,
                    file,
                )
                .map(|(key, bytes, _, _)| (key, bytes))
                .map_err(|e| {
                    let key = unit_key(
                        &project.content,
                        &shared.cookers,
                        file,
                        job.platform,
                        PROFILE,
                    )
                    .ok();
                    (e.to_string(), key)
                }),
            }
        };
        if done
            .send(Done {
                unit: job.unit,
                platform: job.platform,
                result,
            })
            .is_err()
        {
            return;
        }
    }
}

fn message(text: &str) -> Vec<u8> {
    let mut w = Writer::new();
    w.str(text);
    w.finish()
}

impl Service for Cookd {
    fn opened(&mut self, conn: ConnId) {
        self.clients.insert(
            conn,
            Client {
                platform: None,
                served: HashMap::new(),
            },
        );
    }

    fn closed(&mut self, conn: ConnId) {
        self.clients.remove(&conn);
    }

    fn request(&mut self, conn: ConnId, frame: Frame, out: &mut Outbox) {
        let mut r = Reader::new(&frame.payload);
        match frame.kind {
            HELLO => {
                let Ok(name) = r.str() else {
                    out.send(conn, ERROR, frame.id, &message("HELLO: malformed"));
                    return;
                };
                let platform = match Platform::find(&name) {
                    Ok(platform) => platform,
                    Err(e) => {
                        out.send(conn, ERROR, frame.id, &message(&format!("HELLO: {e}")));
                        return;
                    }
                };
                if let Some(c) = self.clients.get_mut(&conn) {
                    c.platform = Some(platform);
                }
                let project = self
                    .shared
                    .project
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                out.send(conn, OBJECTS, frame.id, &table_payload(&project.table));
            }
            UNIT => {
                let (Ok(unit), Ok(())) = (r.u64(), r.end()) else {
                    out.send(conn, ERROR, frame.id, &message("UNIT: malformed"));
                    return;
                };
                let Some(platform) = self.clients.get(&conn).and_then(|c| c.platform) else {
                    out.send(conn, ERROR, frame.id, &message("UNIT before HELLO"));
                    return;
                };
                let waiters = self.in_flight.entry((unit, platform.name)).or_default();
                waiters.push((conn, frame.id));
                if waiters.len() == 1
                    && let Some(jobs) = &self.jobs
                {
                    let _ = jobs.send(Job { unit, platform });
                }
            }
            other => out.send(
                conn,
                ERROR,
                frame.id,
                &message(&format!("unknown request {other}")),
            ),
        }
    }

    fn tick(&mut self, out: &mut Outbox) {
        // Workers only return once the service is gone: one that finished panicked, and its
        // job's waiters would wait forever. A cooker bug takes the service down with it.
        if let Some(i) = self.workers.iter().position(JoinHandle::is_finished)
            && let Err(panic) = self.workers.swap_remove(i).join()
        {
            std::panic::resume_unwind(panic);
        }
        while let Ok(done) = self.done.try_recv() {
            let waiters = self
                .in_flight
                .remove(&(done.unit, done.platform.name))
                .unwrap_or_default();
            for (conn, id) in waiters {
                let Some(client) = self.clients.get_mut(&conn) else {
                    continue;
                };
                match &done.result {
                    Ok((key, bytes)) => {
                        client.served.insert(done.unit, Some(*key));
                        out.send(conn, UNIT_DATA, id, bytes);
                    }
                    Err((e, key)) => {
                        client.served.insert(done.unit, *key);
                        out.send(conn, UNIT_FAILED, id, &message(e));
                    }
                }
            }
        }
        if self.last_watch.elapsed() >= self.watch_interval {
            self.last_watch = Instant::now();
            self.watch(out);
        }
    }
}

impl Drop for Cookd {
    fn drop(&mut self) {
        // Closing the job channel ends the workers.
        self.jobs = None;
        for w in self.workers.drain(..) {
            if let Err(panic) = w.join()
                && !std::thread::panicking()
            {
                std::panic::resume_unwind(panic);
            }
        }
    }
}

/// Binds a server for a cookd on `addr`.
pub fn bind(addr: SocketAddr) -> Result<Server, ServerError> {
    Server::bind(addr)
}
