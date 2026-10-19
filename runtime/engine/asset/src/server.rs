//! The asset server: requests by link, the load pipeline and object lifetimes (0003 §8).
//!
//! ```text
//! load(uuid) ─► unit of the object (Vfs) ─► read (IoQueue, or ready bytes)
//!            ─► decode chunks (jobs) ─► parse the unit, request the units it imports
//!            ─► once those are parsed: run each object's loader ─► Loaded
//! ```
//!
//! A unit is the load unit: requesting one object loads every object of its unit, and of the
//! units it links to (imports), so a loader can turn links into handles that will resolve. There
//! is no load-by-path: everything is reached through links from a root set (the boot config's).
//! A unit stays loaded while any of its objects has a live [`Handle`] or a loaded unit imports
//! it; [`AssetServer::update`] unloads the rest.
//!
//! The server is driven by `update`, once a frame: it submits reads, polls completions,
//! decompresses on the job system, and runs loaders on the calling thread.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::{Any, TypeId};
use core::fmt;
use core::time::Duration;

use marlin_containers::{HashMap, HashMapExt, HashSet, HashSetExt, SlotMap};
use marlin_io::{IoBuffer, IoQueue, RequestId};
use marlin_jobs::JobSystem;
use marlin_reflect::{Reflect, type_info};
use marlin_sync::Mutex;

use crate::Uuid;
use crate::cooked::{BulkKind, Encoding, UnitView};
use crate::handle::{AssetRef, Handle, Shared, Slot};
use crate::vfs::{Read, SourceError, UnitBytes, Vfs};

/// Why an object did not load.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AssetError {
    /// No mounted source has the object (or its unit).
    NotFound,
    /// No loader is registered for the object's cooked type.
    NoLoader { type_id: u64 },
    /// The handle's type is not what the loader makes.
    WrongType,
    /// Reading the unit failed.
    Source(String),
    /// The unit's bytes are not a valid cooked unit.
    Corrupt(String),
    /// The loader refused the object.
    Load(String),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found in any mounted source"),
            Self::NoLoader { type_id } => write!(f, "no loader for type {type_id:#018x}"),
            Self::WrongType => f.write_str("loaded as another type"),
            Self::Source(e) => write!(f, "reading: {e}"),
            Self::Corrupt(e) => write!(f, "corrupt unit: {e}"),
            Self::Load(e) => write!(f, "loading: {e}"),
        }
    }
}

impl core::error::Error for AssetError {}

impl From<SourceError> for AssetError {
    fn from(e: SourceError) -> Self {
        match e {
            SourceError::NotFound => Self::NotFound,
            other => Self::Source(other.to_string()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LoadState {
    Loading,
    Loaded,
    Failed(AssetError),
}

/// What a loader sees of one object.
pub struct LoadContext<'a> {
    pub uuid: Uuid,
    /// The cooked type's stable id.
    pub type_id: u64,
    pub encoding: Encoding,
    /// Aligned for in-place use.
    pub payload: &'a [u8],
    /// The object's bulk blobs, in order. A streamed blob the unit was loaded without has no
    /// bytes here: `blobs` says so, and a [`BlobStreamer`](crate::BlobStreamer) reads it.
    pub bulk: &'a [(BulkKind, &'a [u8])],
    /// What each of `bulk`'s blobs is.
    pub blobs: &'a [BlobInfo],
    /// The unit's id, which streamed blobs are requested by.
    pub unit_id: u64,
    unit: &'a Arc<UnitBytes>,
    table: &'a mut ObjectTable,
}

impl LoadContext<'_> {
    /// The object `uuid` a link names. Non-owning: the linked unit is an import of this one, so
    /// it stays loaded as long as this object does. A link to an object no unit has (a dangling
    /// link) reads `None`.
    pub fn link<T: 'static>(&mut self, uuid: Uuid) -> AssetRef<T> {
        let slot = self.table.by_uuid.get(&uuid).copied().unwrap_or(Slot::NULL);
        AssetRef::new(slot, uuid)
    }

    /// A strong handle to a linked object, for loaders whose objects must keep it loaded
    /// beyond the unit's lifetime. Beware cycles: two objects holding handles to each other
    /// never unload.
    pub fn handle<T: 'static>(&mut self, uuid: Uuid) -> Handle<T> {
        self.table.handle(uuid).0
    }

    /// The unit's bytes, for loaders that keep using cooked data in place: `payload` and `bulk`
    /// point into it.
    pub fn unit_bytes(&self) -> Arc<UnitBytes> {
        self.unit.clone()
    }
}

/// One of an object's bulk blobs, as its [`LoadContext`] sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlobInfo {
    /// Its index in the unit's bulk table: what [`BlobStreamer::request`] takes.
    ///
    /// [`BlobStreamer::request`]: crate::BlobStreamer::request
    pub index: u32,
    pub size: u64,
    /// Marked `BULK_STREAMED`.
    pub streamed: bool,
    /// Its bytes are in the unit (a streamed blob may be absent).
    pub present: bool,
}

type Loaded = Box<dyn Any + Send + Sync>;
type LoadFn = Box<dyn Fn(&mut LoadContext<'_>) -> Result<Loaded, String> + Send + Sync>;

/// Loaders by cooked type id.
#[derive(Default)]
pub struct Loaders {
    by_type: HashMap<u64, LoadFn>,
}

impl Loaders {
    pub fn new() -> Self {
        Self::default()
    }

    /// Objects of cooked type `type_id` load as `T` through `load`.
    pub fn register<T: Send + Sync + 'static>(
        &mut self,
        type_id: u64,
        load: impl Fn(&mut LoadContext<'_>) -> Result<T, String> + Send + Sync + 'static,
    ) {
        self.by_type.insert(
            type_id,
            Box::new(move |ctx| load(ctx).map(|v| Box::new(v) as Loaded)),
        );
    }

    /// Objects of reflected type `T` cooked by the generic cooker (marlin-serial) load as `T`.
    pub fn register_serial<T: Reflect + Send + Sync>(&mut self) {
        self.register::<T>(type_info::<T>().id.0, |ctx| {
            if ctx.encoding != Encoding::Serial {
                return Err("not a marlin-serial payload".into());
            }
            marlin_serial::from_bytes::<T>(ctx.payload).map_err(|e| format!("{e:?}"))
        });
    }
}

pub(crate) struct ObjectSlot {
    uuid: Uuid,
    shared: Weak<Shared>,
    state: State,
    value: Option<Loaded>,
    unit: Option<u64>,
    /// Times the value was replaced by a reload.
    version: u32,
}

#[derive(Clone, PartialEq, Debug)]
enum State {
    /// Requested; its unit not yet known.
    Queued,
    /// Its unit is on the way.
    Waiting,
    Loaded,
    Failed(AssetError),
}

/// Object slots and the handles to them: the part loaders may touch.
struct ObjectTable {
    slots: SlotMap<ObjectSlot>,
    by_uuid: HashMap<Uuid, Slot>,
    drops: Arc<Mutex<Vec<Slot>>>,
}

impl ObjectTable {
    /// The slot of `uuid` (created queued if new) and a handle to it; whether it was created.
    fn handle<T>(&mut self, uuid: Uuid) -> (Handle<T>, Slot, bool) {
        let mut created = false;
        let slot = *self.by_uuid.entry(uuid).or_insert_with(|| {
            created = true;
            self.slots.insert(ObjectSlot {
                uuid,
                shared: Weak::new(),
                state: State::Queued,
                value: None,
                unit: None,
                version: 0,
            })
        });
        let object = &mut self.slots[slot];
        let shared = match object.shared.upgrade() {
            Some(shared) => shared,
            None => {
                let shared = Arc::new(Shared {
                    slot,
                    uuid,
                    drops: self.drops.clone(),
                });
                object.shared = Arc::downgrade(&shared);
                shared
            }
        };
        (Handle::new(shared), slot, created)
    }

    /// The slot of `uuid`, created for unit `unit` if new.
    fn slot_in(&mut self, uuid: Uuid, unit: u64) -> Slot {
        let slot = *self.by_uuid.entry(uuid).or_insert_with(|| {
            self.slots.insert(ObjectSlot {
                uuid,
                shared: Weak::new(),
                state: State::Waiting,
                value: None,
                unit: Some(unit),
                version: 0,
            })
        });
        let object = &mut self.slots[slot];
        object.unit = Some(unit);
        if object.state == State::Queued {
            object.state = State::Waiting;
        }
        slot
    }

    fn remove(&mut self, slot: Slot) {
        if let Some(object) = self.slots.remove(slot) {
            self.by_uuid.remove(&object.uuid);
        }
    }
}

enum Stage {
    Requested,
    Reading,
    /// Asked of a source that delivers later ([`Read::Pending`]).
    Polling,
    /// Read, waiting to be decoded.
    Read(IoBuffer, usize),
    Decoded,
    /// Parsed; loaders run once its imports are parsed too.
    Parsed,
    Resident,
    Failed(AssetError),
}

struct UnitSlot {
    stage: Stage,
    /// The bytes its loaded objects came from.
    bytes: Option<Arc<UnitBytes>>,
    /// New bytes, decoded but not loaded yet.
    incoming: Option<Arc<UnitBytes>>,
    /// Its objects (once parsed).
    objects: Vec<Slot>,
    /// Units it links to.
    imports: Vec<u64>,
    /// Objects requested expecting to be in this unit.
    waiters: Vec<Slot>,
    /// This load replaces a loaded version (hot reload): its objects keep their old values until
    /// the new ones are made, and a failure keeps the old version.
    reload: bool,
    /// Invalidated again while loading: reload once this load is done.
    again: bool,
}

impl UnitSlot {
    fn new() -> Self {
        Self {
            stage: Stage::Requested,
            bytes: None,
            incoming: None,
            objects: Vec::new(),
            imports: Vec::new(),
            waiters: Vec::new(),
            reload: false,
            again: false,
        }
    }

    fn parsed(&self) -> bool {
        matches!(
            self.stage,
            Stage::Parsed | Stage::Resident | Stage::Failed(_)
        )
    }
}

/// What one [`AssetServer::update`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct UpdateStats {
    pub reads_submitted: usize,
    pub units_decoded: usize,
    pub objects_loaded: usize,
    pub objects_failed: usize,
    pub units_unloaded: usize,
}

/// Loads objects by UUID from the units of a [`Vfs`].
pub struct AssetServer {
    loaders: Loaders,
    table: ObjectTable,
    units: HashMap<u64, UnitSlot>,
    queued: Vec<Slot>,
    requested: Vec<u64>,
    reads: HashMap<RequestId, u64>,
    collect: bool,
    /// Stage changes so far: whether a blocking round got anywhere.
    progress: u64,
    /// Objects reloaded since the last [`take_reloaded`](Self::take_reloaded).
    reloaded: Vec<Uuid>,
    /// Reloads that failed (the old version stays) since the last
    /// [`take_reload_errors`](Self::take_reload_errors).
    reload_errors: Vec<(u64, AssetError)>,
}

impl AssetServer {
    pub fn new(loaders: Loaders) -> Self {
        Self {
            loaders,
            table: ObjectTable {
                slots: SlotMap::new(),
                by_uuid: HashMap::new(),
                drops: Arc::new(Mutex::new(Vec::new())),
            },
            units: HashMap::new(),
            queued: Vec::new(),
            requested: Vec::new(),
            reads: HashMap::new(),
            collect: false,
            progress: 0,
            reloaded: Vec::new(),
            reload_errors: Vec::new(),
        }
    }

    /// A handle to object `uuid`, loading it (and its unit, and what that links to) if needed.
    /// `T` is what the object's loader makes.
    pub fn load<T: 'static>(&mut self, uuid: Uuid) -> Handle<T> {
        let (handle, slot, created) = self.table.handle::<T>(uuid);
        if created {
            self.queued.push(slot);
        }
        handle
    }

    pub fn state<T: 'static>(&self, handle: &Handle<T>) -> LoadState {
        match self.table.slots.get(handle.slot()) {
            None => LoadState::Failed(AssetError::NotFound),
            Some(o) => match &o.state {
                State::Queued | State::Waiting => LoadState::Loading,
                State::Failed(e) => LoadState::Failed(e.clone()),
                State::Loaded => match &o.value {
                    Some(v) if (**v).type_id() == TypeId::of::<T>() => LoadState::Loaded,
                    _ => LoadState::Failed(AssetError::WrongType),
                },
            },
        }
    }

    /// The object a non-owning reference names, if it is loaded.
    pub fn get_ref<T: 'static>(&self, r: AssetRef<T>) -> Option<&T> {
        self.table
            .slots
            .get(r.slot)?
            .value
            .as_ref()?
            .downcast_ref::<T>()
    }

    /// A strong handle to what `r` names (loading it again if it was unloaded).
    pub fn upgrade<T: 'static>(&mut self, r: AssetRef<T>) -> Handle<T> {
        self.load(r.uuid)
    }

    /// The loaded object, once it is.
    pub fn get<T: 'static>(&self, handle: &Handle<T>) -> Option<&T> {
        self.table
            .slots
            .get(handle.slot())?
            .value
            .as_ref()?
            .downcast_ref::<T>()
    }

    /// Objects known to the server (loaded or not).
    pub fn object_count(&self) -> usize {
        self.table.slots.len()
    }

    /// Units loaded or on the way.
    pub fn unit_count(&self) -> usize {
        self.units.len()
    }

    /// Whether `unit` is loaded.
    pub fn is_resident(&self, unit: u64) -> bool {
        self.units
            .get(&unit)
            .is_some_and(|u| matches!(u.stage, Stage::Resident))
    }

    /// How many times the object was replaced by a reload (0: as first loaded). Systems that
    /// derive data from an object (GPU resources, caches) compare it to rebuild.
    pub fn version<T: 'static>(&self, handle: &Handle<T>) -> u32 {
        self.table.slots.get(handle.slot()).map_or(0, |o| o.version)
    }

    /// Objects whose values were replaced by reloads since the last call.
    pub fn take_reloaded(&mut self) -> Vec<Uuid> {
        core::mem::take(&mut self.reloaded)
    }

    /// Reloads that failed since the last call. Their units keep the version loaded before.
    pub fn take_reload_errors(&mut self) -> Vec<(u64, AssetError)> {
        core::mem::take(&mut self.reload_errors)
    }

    /// Loads `unit` again (if it is loaded): its objects keep their handles and get new values.
    /// Sources that know when units change ([`UnitSource::take_invalidated`]) trigger this on
    /// their own.
    ///
    /// [`UnitSource::take_invalidated`]: crate::UnitSource::take_invalidated
    pub fn reload(&mut self, unit: u64) {
        let Some(u) = self.units.get_mut(&unit) else {
            return;
        };
        match u.stage {
            Stage::Resident | Stage::Failed(_) => {
                u.reload = matches!(u.stage, Stage::Resident);
                u.stage = Stage::Requested;
                u.again = false;
                self.requested.push(unit);
                self.progress += 1;
            }
            // In flight: what arrives may already be stale.
            _ => u.again = true,
        }
    }

    /// Whether nothing is in flight.
    pub fn is_idle(&self) -> bool {
        self.queued.is_empty()
            && self.requested.is_empty()
            && self.reads.is_empty()
            && self
                .units
                .values()
                .all(|u| matches!(u.stage, Stage::Resident | Stage::Failed(_)))
    }

    /// Advances every load: submits reads to `io`, takes its completions, decompresses on `jobs`
    /// (or here), parses, runs loaders and unloads what nothing references any more.
    pub fn update<'f>(
        &mut self,
        vfs: &'f Vfs,
        io: &mut IoQueue<'f>,
        jobs: Option<&JobSystem>,
    ) -> UpdateStats {
        self.step(vfs, Some(io), jobs)
    }

    /// Like [`update`](Self::update) with synchronous reads, repeated until nothing is in
    /// flight: for tools, tests and loading screens.
    pub fn update_blocking(&mut self, vfs: &Vfs, jobs: Option<&JobSystem>) -> UpdateStats {
        let mut total = UpdateStats::default();
        loop {
            let before = self.progress;
            let s = self.step(vfs, None, jobs);
            total.reads_submitted += s.reads_submitted;
            total.units_decoded += s.units_decoded;
            total.objects_loaded += s.objects_loaded;
            total.objects_failed += s.objects_failed;
            total.units_unloaded += s.units_unloaded;
            if self.is_idle() {
                return total;
            }
            if self.progress == before && s == UpdateStats::default() {
                // Nothing moved. Units on their way from a remote source: wait for them;
                // otherwise another round would change nothing.
                if !self
                    .units
                    .values()
                    .any(|u| matches!(u.stage, Stage::Polling))
                {
                    return total;
                }
                vfs.wait(Duration::from_millis(5));
            }
        }
    }

    fn request_unit(&mut self, unit: u64) {
        if !self.units.contains_key(&unit) {
            self.units.insert(unit, UnitSlot::new());
            self.requested.push(unit);
        }
    }

    fn fail_unit(&mut self, unit: u64, error: AssetError, stats: &mut UpdateStats) {
        let Some(u) = self.units.get_mut(&unit) else {
            return;
        };
        self.progress += 1;
        if u.reload {
            // A failed reload keeps the version that is loaded. Objects asked for meanwhile that
            // only the new version would have had fail.
            u.stage = Stage::Resident;
            u.incoming = None;
            u.reload = false;
            for slot in core::mem::take(&mut u.waiters) {
                if let Some(o) = self.table.slots.get_mut(slot)
                    && o.state == State::Waiting
                {
                    o.state = State::Failed(error.clone());
                    stats.objects_failed += 1;
                }
            }
            self.reload_errors.push((unit, error));
            return;
        }
        u.stage = Stage::Failed(error.clone());
        u.bytes = None;
        u.incoming = None;
        for &slot in u.objects.iter().chain(&u.waiters) {
            if let Some(o) = self.table.slots.get_mut(slot)
                && !matches!(o.state, State::Loaded | State::Failed(_))
            {
                o.state = State::Failed(error.clone());
                stats.objects_failed += 1;
            }
        }
        self.collect = true;
    }

    fn step<'f>(
        &mut self,
        vfs: &'f Vfs,
        mut io: Option<&mut IoQueue<'f>>,
        jobs: Option<&JobSystem>,
    ) -> UpdateStats {
        let mut stats = UpdateStats::default();

        // Changed units: load them again.
        for unit in vfs.take_invalidated() {
            self.reload(unit);
        }

        // Queued objects: find their units.
        for slot in core::mem::take(&mut self.queued) {
            let Some(object) = self.table.slots.get(slot) else {
                continue;
            };
            if object.state != State::Queued {
                continue;
            }
            let Some(unit) = vfs.object_unit(object.uuid) else {
                self.table.slots[slot].state = State::Failed(AssetError::NotFound);
                stats.objects_failed += 1;
                self.collect = true;
                continue;
            };
            self.request_unit(unit);
            let object = &mut self.table.slots[slot];
            object.state = State::Waiting;
            self.progress += 1;
            object.unit = Some(unit);
            let u = self.units.get_mut(&unit).expect("just requested");
            match &u.stage {
                // Resident, yet the object is not in it: the source's object table and the
                // unit disagree.
                Stage::Resident if !u.objects.contains(&slot) => {
                    object.state = State::Failed(AssetError::NotFound);
                    stats.objects_failed += 1;
                }
                Stage::Resident => {}
                Stage::Failed(e) => {
                    object.state = State::Failed(e.clone());
                    stats.objects_failed += 1;
                }
                _ => u.waiters.push(slot),
            }
        }

        // Requested units: start reading.
        let mut retry = Vec::new();
        for unit in core::mem::take(&mut self.requested) {
            let Some(source) = vfs.source(unit) else {
                self.fail_unit(unit, AssetError::NotFound, &mut stats);
                continue;
            };
            match source.begin(unit) {
                Err(e) => self.fail_unit(unit, e.into(), &mut stats),
                Ok(Read::Ready(bytes)) => {
                    let u = self.units.get_mut(&unit).expect("requested");
                    u.incoming = Some(Arc::new(bytes));
                    u.stage = Stage::Decoded;
                    self.progress += 1;
                }
                Ok(Read::Pending) => {
                    self.units.get_mut(&unit).expect("requested").stage = Stage::Polling;
                    self.progress += 1;
                }
                Ok(Read::File { file, offset, len }) => match io.as_deref_mut() {
                    Some(queue) => match queue.read(file, IoBuffer::new(len), offset) {
                        Ok(id) => {
                            self.reads.insert(id, unit);
                            self.units.get_mut(&unit).expect("requested").stage = Stage::Reading;
                            stats.reads_submitted += 1;
                        }
                        Err(e) if e.error.kind() == marlin_io::ErrorKind::WouldBlock => {
                            retry.push(unit)
                        }
                        Err(e) => {
                            self.fail_unit(unit, AssetError::Source(e.to_string()), &mut stats)
                        }
                    },
                    None => {
                        let mut buffer = IoBuffer::new(len);
                        match file.read_exact_at(&mut buffer, offset) {
                            Ok(()) => {
                                self.units.get_mut(&unit).expect("requested").stage =
                                    Stage::Read(buffer, len);
                                stats.reads_submitted += 1;
                            }
                            Err(e) => {
                                self.fail_unit(unit, AssetError::Source(e.to_string()), &mut stats)
                            }
                        }
                    }
                },
            }
        }
        self.requested = retry;

        // Units on their way from sources that deliver later.
        let polling: Vec<u64> = self
            .units
            .iter()
            .filter(|(_, u)| matches!(u.stage, Stage::Polling))
            .map(|(&k, _)| k)
            .collect();
        for unit in polling {
            let Some(source) = vfs.source(unit) else {
                self.fail_unit(unit, AssetError::NotFound, &mut stats);
                continue;
            };
            match source.poll(unit) {
                None => {}
                Some(Ok(bytes)) => {
                    let u = self.units.get_mut(&unit).expect("polling");
                    u.incoming = Some(Arc::new(bytes));
                    u.stage = Stage::Decoded;
                    self.progress += 1;
                    stats.units_decoded += 1;
                }
                Some(Err(e)) => self.fail_unit(unit, e.into(), &mut stats),
            }
        }

        // Completed reads.
        if let Some(queue) = io {
            loop {
                match queue.poll() {
                    Ok(Some(done)) => {
                        let Some(unit) = self.reads.remove(&done.id) else {
                            continue;
                        };
                        match done.result {
                            Ok(n) if n == done.buffer.len() => {
                                if let Some(u) = self.units.get_mut(&unit) {
                                    u.stage = Stage::Read(done.buffer, n);
                                }
                            }
                            Ok(n) => self.fail_unit(
                                unit,
                                AssetError::Source(format!(
                                    "short read: {n} of {} bytes",
                                    done.buffer.len()
                                )),
                                &mut stats,
                            ),
                            Err(e) => {
                                self.fail_unit(unit, AssetError::Source(e.to_string()), &mut stats)
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        // The queue itself failed: everything in flight with it.
                        for (_, unit) in core::mem::take(&mut self.reads) {
                            self.fail_unit(unit, AssetError::Source(e.to_string()), &mut stats);
                        }
                        break;
                    }
                }
            }
        }

        // Decode what was read, in parallel.
        let mut to_decode: Vec<(u64, IoBuffer, usize)> = Vec::new();
        for (&unit, u) in self.units.iter_mut() {
            if matches!(u.stage, Stage::Read(..)) {
                let Stage::Read(buffer, n) = core::mem::replace(&mut u.stage, Stage::Reading)
                else {
                    unreachable!()
                };
                to_decode.push((unit, buffer, n));
            }
        }
        let mut decoded: Vec<Option<Result<UnitBytes, AssetError>>> =
            (0..to_decode.len()).map(|_| None).collect();
        let decode = |(unit, buffer, n): &(u64, IoBuffer, usize)| -> Result<UnitBytes, AssetError> {
            let source = vfs.source(*unit).ok_or(AssetError::NotFound)?;
            Ok(source.finish(*unit, &buffer[..*n])?)
        };
        match jobs {
            Some(jobs) if to_decode.len() > 1 => jobs.scope(|s| {
                for (job, out) in to_decode.iter().zip(decoded.iter_mut()) {
                    let decode = &decode;
                    s.spawn(move || *out = Some(decode(job)));
                }
            }),
            _ => {
                for (job, out) in to_decode.iter().zip(decoded.iter_mut()) {
                    *out = Some(decode(job));
                }
            }
        }
        for ((unit, _, _), result) in to_decode.iter().zip(decoded) {
            match result.expect("every job ran") {
                Ok(bytes) => {
                    let u = self.units.get_mut(unit).expect("reading");
                    u.incoming = Some(Arc::new(bytes));
                    u.stage = Stage::Decoded;
                    self.progress += 1;
                    stats.units_decoded += 1;
                }
                Err(e) => self.fail_unit(*unit, e, &mut stats),
            }
        }

        // Parse decoded units; request their imports.
        let decoded_units: Vec<u64> = self
            .units
            .iter()
            .filter(|(_, u)| matches!(u.stage, Stage::Decoded))
            .map(|(&k, _)| k)
            .collect();
        for unit in decoded_units {
            let bytes = self.units[&unit].incoming.clone().expect("decoded");
            let parsed = UnitView::new(bytes.as_slice()).and_then(|view| {
                if view.header().unit != unit {
                    return Err(crate::cooked::UnitError::Truncated);
                }
                let mut objects = Vec::with_capacity(view.object_count());
                for i in 0..view.object_count() {
                    objects.push(view.object(i)?.uuid);
                }
                let mut imports = Vec::with_capacity(view.import_count());
                for i in 0..view.import_count() {
                    imports.push(view.import(i)?.unit);
                }
                Ok((objects, imports))
            });
            match parsed {
                Err(e) => self.fail_unit(unit, AssetError::Corrupt(format!("{e:?}")), &mut stats),
                Ok((objects, mut imports)) => {
                    imports.sort_unstable();
                    imports.dedup();
                    imports.retain(|&i| i != unit);
                    let slots: Vec<Slot> = objects
                        .iter()
                        .map(|&uuid| self.table.slot_in(uuid, unit))
                        .collect();
                    for &i in &imports {
                        self.request_unit(i);
                    }
                    // A reload may have lost objects: moved to another unit (load them from
                    // there), or deleted.
                    let old =
                        core::mem::take(&mut self.units.get_mut(&unit).expect("decoded").objects);
                    for slot in old.into_iter().filter(|s| !slots.contains(s)) {
                        let Some(o) = self.table.slots.get_mut(slot) else {
                            continue;
                        };
                        o.value = None;
                        o.version += 1;
                        match vfs.object_unit(o.uuid).filter(|&u| u != unit) {
                            Some(_) => {
                                o.state = State::Queued;
                                o.unit = None;
                                self.queued.push(slot);
                            }
                            None => {
                                o.state = State::Failed(AssetError::NotFound);
                                stats.objects_failed += 1;
                            }
                        }
                    }
                    let u = self.units.get_mut(&unit).expect("decoded");
                    u.objects = slots;
                    u.imports = imports;
                    u.stage = Stage::Parsed;
                    self.progress += 1;
                }
            }
        }

        // Load units whose imports are all parsed (cycles load together).
        let ready: Vec<u64> = self
            .units
            .iter()
            .filter(|(_, u)| matches!(u.stage, Stage::Parsed))
            .filter(|(_, u)| {
                u.imports
                    .iter()
                    .all(|i| self.units.get(i).is_none_or(UnitSlot::parsed))
            })
            .map(|(&k, _)| k)
            .collect();
        for unit in ready {
            self.load_unit(unit, &mut stats);
        }

        // Lifetimes.
        let dropped: Vec<Slot> = core::mem::take(&mut *self.table.drops.lock());
        if !dropped.is_empty() || self.collect || stats.objects_loaded > 0 {
            self.collect = false;
            stats.units_unloaded += self.collect_garbage();
        }
        stats
    }

    fn load_unit(&mut self, unit: u64, stats: &mut UpdateStats) {
        let (bytes, slots, reload) = {
            let u = self.units.get_mut(&unit).expect("parsed");
            let bytes = u.incoming.take().expect("parsed");
            u.bytes = Some(bytes.clone());
            (bytes, u.objects.clone(), u.reload)
        };
        let view = match UnitView::new(bytes.as_slice()) {
            Ok(v) => v,
            Err(e) => return self.fail_unit(unit, AssetError::Corrupt(format!("{e:?}")), stats),
        };
        for (i, &slot) in slots.iter().enumerate() {
            let entry = match view.object(i) {
                Ok(e) => e,
                Err(e) => {
                    return self.fail_unit(unit, AssetError::Corrupt(format!("{e:?}")), stats);
                }
            };
            let bulk: Vec<(BulkKind, &[u8])> = view
                .object_bulk(&entry)
                .map(|b| (b.kind, view.bulk_bytes(&b)))
                .collect();
            let blobs: Vec<BlobInfo> = view
                .object_bulk(&entry)
                .enumerate()
                .map(|(i, b)| BlobInfo {
                    index: entry.bulk_first + i as u32,
                    size: b.size,
                    streamed: b.flags & crate::cooked::BULK_STREAMED != 0,
                    present: view.bulk_present(&b),
                })
                .collect();
            let result = match self.loaders.by_type.get(&entry.type_id) {
                None => Err(AssetError::NoLoader {
                    type_id: entry.type_id,
                }),
                Some(load) => {
                    let mut ctx = LoadContext {
                        uuid: entry.uuid,
                        type_id: entry.type_id,
                        encoding: entry.encoding,
                        payload: view.payload(&entry),
                        bulk: &bulk,
                        blobs: &blobs,
                        unit_id: unit,
                        unit: &bytes,
                        table: &mut self.table,
                    };
                    load(&mut ctx).map_err(AssetError::Load)
                }
            };
            let object = &mut self.table.slots[slot];
            let had_value = object.value.is_some();
            match result {
                Ok(value) => {
                    object.value = Some(value);
                    object.state = State::Loaded;
                    stats.objects_loaded += 1;
                }
                Err(e) => {
                    // A reload's failed object keeps nothing stale: its old value went with
                    // the old bytes it may point into.
                    object.value = None;
                    object.state = State::Failed(e);
                    stats.objects_failed += 1;
                }
            }
            if reload && had_value {
                object.version += 1;
                self.reloaded.push(object.uuid);
            }
        }
        let u = self.units.get_mut(&unit).expect("parsed");
        u.stage = Stage::Resident;
        u.reload = false;
        self.progress += 1;
        if core::mem::take(&mut u.again) {
            self.reload(unit);
        }
        let u = self.units.get_mut(&unit).expect("parsed");
        // Requested here but not in the unit: the source's tables disagreed with the unit.
        for slot in core::mem::take(&mut u.waiters) {
            if let Some(o) = self.table.slots.get_mut(slot)
                && o.state == State::Waiting
            {
                o.state = State::Failed(AssetError::NotFound);
                stats.objects_failed += 1;
            }
        }
    }

    /// Unloads resident units nothing holds, and forgets unreferenced objects of no unit.
    fn collect_garbage(&mut self) -> usize {
        let held = |slot: Slot, table: &ObjectTable| {
            table
                .slots
                .get(slot)
                .is_some_and(|o| o.shared.strong_count() > 0)
        };
        let mut keep: HashSet<u64> = HashSet::new();
        let mut stack: Vec<u64> = Vec::new();
        for (&unit, u) in &self.units {
            let resident = matches!(u.stage, Stage::Resident);
            let held_here = u.objects.iter().any(|&s| held(s, &self.table));
            // In-flight units are kept as they are; resident ones only when held.
            if (!resident || held_here) && keep.insert(unit) {
                stack.push(unit);
            }
        }
        while let Some(unit) = stack.pop() {
            let Some(u) = self.units.get(&unit) else {
                continue;
            };
            // Failed units hold nothing; in-flight units' imports are not known yet.
            for &i in &u.imports {
                if keep.insert(i) {
                    stack.push(i);
                }
            }
        }
        let gone: Vec<u64> = self
            .units
            .keys()
            .copied()
            .filter(|u| !keep.contains(u))
            .collect();
        for unit in &gone {
            if let Some(u) = self.units.remove(unit) {
                for slot in u.objects {
                    self.table.remove(slot);
                }
            }
        }
        // Objects without handles whose unit is gone or failed (failed lookups, say).
        let orphans: Vec<Slot> = self
            .table
            .slots
            .iter()
            .filter(|(_, o)| o.shared.strong_count() == 0)
            .filter(|(_, o)| match o.unit {
                None => matches!(o.state, State::Failed(_)),
                Some(unit) => match self.units.get(&unit) {
                    None => true,
                    Some(u) => matches!(u.stage, Stage::Failed(_)),
                },
            })
            .map(|(s, _)| s)
            .collect();
        for slot in orphans {
            self.table.remove(slot);
        }
        // Failed units with nobody waiting on them are forgotten, so a later load retries.
        let failed: Vec<u64> = self
            .units
            .iter()
            .filter(|(_, u)| matches!(u.stage, Stage::Failed(_)))
            .filter(|(_, u)| {
                !u.objects
                    .iter()
                    .chain(&u.waiters)
                    .any(|&s| held(s, &self.table))
            })
            .map(|(&k, _)| k)
            .collect();
        for unit in failed {
            self.units.remove(&unit);
        }
        gone.len()
    }
}

impl fmt::Debug for AssetServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssetServer")
            .field("objects", &self.table.slots.len())
            .field("units", &self.units.len())
            .finish_non_exhaustive()
    }
}
