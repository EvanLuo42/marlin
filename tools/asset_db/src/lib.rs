//! The local asset index (`docs/design/0003-asset-pipeline.md` §2).
//!
//! An index of every file under an asset root, every object in its `.masset` files and every
//! link between them, so tools answer questions about the whole project without opening files:
//! all objects of a type, name search, who references an object, an object's dependency closure,
//! dangling links. It indexes only what all content has in common (types, UUIDs, names, links),
//! read with [`marlin_masset::extract`], never the type-specific attributes.
//!
//! - **Storage**: columnar tables of string ids and numbers, strings interned in pools, hash
//!   tables for UUID and path lookups. [`AssetDb::save`] writes them as a snapshot that
//!   [`AssetDb::open`] memory-maps: opening costs nothing until queries touch the data.
//! - **Updates**: [`AssetDb::update`] finds changed files through the NTFS change journal where
//!   available, else by listing directories (sizes and times only). Only changed files are read;
//!   hashing and extraction run on all cores.
//! - Each machine has its own index of its own working copy; nothing here is shared.
//!
//! Row ids ([`FileId`], [`ObjectId`]) are valid until the next `update` or `save`.

mod column;
mod db;
mod journal;
mod scan;
mod snapshot;
mod strings;
mod table;
mod validate;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use marlin_asset::Uuid;

use crate::db::{Db, path_key};
use crate::journal::{
    Journal, JournalState, REASON_CREATE, REASON_DELETE, REASON_RENAME_NEW, REASON_RENAME_OLD,
};
use crate::scan::{Stat, Walk, WalkOptions};
use crate::strings::NONE;

pub use db::{FileId, ObjectId};
pub use validate::{Issue, IssueKind};

/// Extension of source text files.
pub const MASSET_EXT: &str = "masset";

#[derive(Clone, Debug)]
pub struct Options {
    /// Directory and file names never indexed, besides hidden ones (starting with `.`).
    pub exclude: Vec<String>,
    /// Where snapshots go; default `<root>/.marlin/index`.
    pub index_dir: Option<PathBuf>,
    /// Scan threads; 0 means one per logical CPU.
    pub threads: usize,
    /// Use the change journal when the volume has one.
    pub journal: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            exclude: vec!["target".into()],
            index_dir: None,
            threads: 0,
            journal: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum UpdateMode {
    /// Changes came from the change journal.
    Journal,
    /// Every directory was listed.
    #[default]
    FullScan,
}

/// What an update did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct UpdateReport {
    pub mode: UpdateMode,
    /// Files whose size and time were checked.
    pub checked: usize,
    /// Files opened (new or changed).
    pub read: usize,
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    /// File names that are not UTF-8, left out of the index.
    pub skipped_names: usize,
}

#[derive(Debug, marlin_error::Error)]
pub enum Error {
    #[error("{path}")]
    Io { path: String, source: io::Error },
}

pub type Result<T> = core::result::Result<T, Error>;

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.display().to_string(),
        source,
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileInfo<'a> {
    /// Relative to the root, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub mtime: u64,
    /// xxh3-128 of the contents.
    pub hash: u128,
    /// Why a `.masset` file could not be read.
    pub parse_error: Option<&'a str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjectInfo<'a> {
    pub uuid: Uuid,
    pub name: &'a str,
    pub type_name: &'a str,
    pub file: FileId,
}

/// A link as written in the source.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LinkInfo<'a> {
    /// `None` when written by hand without one.
    pub target: Option<Uuid>,
    /// Empty for the same file.
    pub path: &'a str,
    pub name: &'a str,
}

/// A file link (`` `scheme:path` ``) as written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SourceLink<'a> {
    pub scheme: &'a str,
    pub path: &'a str,
}

/// The index of one asset root. See the crate docs.
pub struct AssetDb {
    root: PathBuf,
    options: Options,
    db: Db,
    /// Directory FRN to (parent FRN, name), for the change journal.
    dirs: HashMap<u64, (u64, String)>,
    journal: Option<Journal>,
    state: JournalState,
    generation: u64,
    /// Target UUID to the objects linking to it; built on first use after each change.
    incoming: OnceLock<HashMap<u128, Vec<u32>>>,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn is_under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl AssetDb {
    /// Opens the index of `root`: the saved snapshot if there is one, then an update to bring
    /// it in line with the disk.
    pub fn open(root: impl Into<PathBuf>, options: Options) -> Result<(Self, UpdateReport)> {
        let mut db = Self::empty(root.into(), options);
        if let Some(loaded) = snapshot::load(&db.index_dir()) {
            db.generation = loaded.generation;
            db.state = loaded.journal;
            let d = &loaded.db.dirs;
            db.dirs = (0..d.frn.len())
                .map(|i| {
                    let name = loaded.db.pools.names.get(d.name[i]).to_owned();
                    (d.frn[i], (d.parent[i], name))
                })
                .collect();
            db.db = loaded.db;
        }
        let report = db.update()?;
        Ok((db, report))
    }

    /// Indexes `root` from scratch, ignoring any snapshot.
    pub fn build(root: impl Into<PathBuf>, options: Options) -> Result<Self> {
        let mut db = Self::empty(root.into(), options);
        db.update()?;
        Ok(db)
    }

    fn empty(root: PathBuf, options: Options) -> Self {
        let journal = if options.journal {
            Journal::open(&root)
        } else {
            None
        };
        Self {
            root,
            options,
            db: Db::default(),
            dirs: HashMap::new(),
            journal,
            state: JournalState::default(),
            generation: 0,
            incoming: OnceLock::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn index_dir(&self) -> PathBuf {
        self.options
            .index_dir
            .clone()
            .unwrap_or_else(|| self.root.join(".marlin").join("index"))
    }

    /// Increases with every save.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether updates can use the change journal.
    pub fn has_journal(&self) -> bool {
        self.journal.is_some()
    }

    fn threads(&self) -> usize {
        match self.options.threads {
            0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
            n => n,
        }
    }

    // --- Updating ---

    /// Brings the index in line with the disk.
    pub fn update(&mut self) -> Result<UpdateReport> {
        self.incoming = OnceLock::new();
        let journal = self.journal.take();
        let result = self.update_with(journal.as_ref());
        self.journal = journal;
        result
    }

    fn update_with(&mut self, journal: Option<&Journal>) -> Result<UpdateReport> {
        // Read before scanning: changes made during the scan are seen again next time, which
        // is harmless, rather than missed.
        let position = journal.and_then(|j| j.position().ok());
        let mut report = UpdateReport::default();
        if let (Some(j), Some(position)) = (journal, position)
            && self.state.valid
            && let Ok(Some((records, end))) = j.read(&self.state)
        {
            report.mode = UpdateMode::Journal;
            self.apply_records(j, records, &mut report)?;
            self.state = JournalState {
                next_usn: end,
                ..position
            };
            return Ok(report);
        }
        self.full_scan(journal, &mut report)?;
        self.state = position.unwrap_or_default();
        Ok(report)
    }

    fn walk(&self, start: &str) -> Result<Walk> {
        scan::walk(
            &self.root,
            start,
            &WalkOptions {
                exclude: &self.options.exclude,
                threads: self.threads(),
            },
        )
        .map_err(io_error(&self.root.join(start)))
    }

    fn full_scan(&mut self, journal: Option<&Journal>, report: &mut UpdateReport) -> Result<()> {
        let walk = self.walk("")?;
        if let Some(j) = journal {
            self.dirs.clear();
            self.record_dirs(j, &walk.dirs);
        }
        self.apply_walk("", walk, report)
    }

    /// Learns the FRNs of `dirs` (relative, parents before children).
    fn record_dirs(&mut self, journal: &Journal, dirs: &[String]) {
        let threads = self.threads().clamp(1, dirs.len().max(1));
        let chunk = dirs.len().div_ceil(threads).max(1);
        let root = &self.root;
        let frns: Vec<Option<u64>> = std::thread::scope(|s| {
            let handles: Vec<_> = dirs
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || {
                        part.iter()
                            .map(|d| journal.dir_frn(&root.join(d)).ok())
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        });
        let mut by_path: HashMap<&str, u64> = HashMap::new();
        by_path.insert("", journal.root());
        for (dir, frn) in dirs.iter().zip(frns) {
            let Some(frn) = frn else { continue };
            if dir.is_empty() {
                continue;
            }
            by_path.insert(dir, frn);
            let (parent, name) = dir.rsplit_once('/').unwrap_or(("", dir));
            if let Some(&parent_frn) = by_path.get(parent) {
                self.dirs.insert(frn, (parent_frn, name.to_owned()));
            }
        }
    }

    /// Paths of live files under `dir` (everything for the root).
    fn files_under(&self, dir: &str) -> Vec<String> {
        if dir.is_empty() {
            return self.db.live_files().map(|f| self.db.file_path(f)).collect();
        }
        let dirs = self
            .db
            .pools
            .dirs
            .matching(|d| d == dir || is_under(d, dir));
        self.db
            .live_files()
            .filter(|f| dirs.get(self.db.files.dir[f.0 as usize] as usize) == Some(&true))
            .map(|f| self.db.file_path(f))
            .collect()
    }

    /// Reconciles the index under `dir` with a walk of it.
    fn apply_walk(&mut self, dir: &str, walk: Walk, report: &mut UpdateReport) -> Result<()> {
        report.checked += walk.files.len();
        report.skipped_names += walk.skipped_names;
        let present: HashSet<&str> = walk.files.iter().map(|s| s.path.as_str()).collect();
        for path in self.files_under(dir) {
            if !present.contains(path.as_str()) && self.db.remove_file(&path) {
                report.removed += 1;
            }
        }
        let changed: Vec<Stat> = walk
            .files
            .into_iter()
            .filter(|s| self.is_stale(s))
            .collect();
        self.read_and_put(&changed, report)
    }

    fn is_stale(&self, stat: &Stat) -> bool {
        match self.db.find_file(&stat.path) {
            Some(f) => {
                let i = f.0 as usize;
                self.db.files.size[i] != stat.size || self.db.files.mtime[i] != stat.mtime
            }
            None => true,
        }
    }

    fn read_and_put(&mut self, stats: &[Stat], report: &mut UpdateReport) -> Result<()> {
        report.read += stats.len();
        let records =
            scan::read_all(&self.root, stats, self.threads()).map_err(io_error(&self.root))?;
        for record in &records {
            if self.db.find_file(&record.path).is_some() {
                report.changed += 1;
            } else {
                report.added += 1;
            }
            self.db.put_file(record);
        }
        Ok(())
    }

    /// The relative path of a directory FRN, if it is in the tree.
    fn dir_path(&self, mut frn: u64) -> Option<String> {
        let mut names = Vec::new();
        for _ in 0..1024 {
            if frn == self.state.root {
                names.reverse();
                return Some(names.join("/"));
            }
            let (parent, name) = self.dirs.get(&frn)?;
            names.push(name.as_str());
            frn = *parent;
        }
        None
    }

    fn apply_records(
        &mut self,
        journal: &Journal,
        records: Vec<journal::Record>,
        report: &mut UpdateReport,
    ) -> Result<()> {
        // Records carry FRNs but, read without elevation, no names: a file record marks its
        // directory for a listing, a directory record is resolved to its current path by FRN.
        let mut listed = BTreeSet::new();
        let mut subtrees = BTreeSet::new();
        let skipped =
            |rel: &str, exclude: &[String]| rel.split('/').any(|c| scan::skipped(c, exclude));
        for r in records {
            if !r.directory {
                if let Some(parent) = self.dir_path(r.parent) {
                    listed.insert(parent);
                }
                continue;
            }
            if r.reason & (REASON_DELETE | REASON_RENAME_OLD) != 0
                && let Some(path) = self.dir_path(r.frn)
            {
                subtrees.insert(path);
            }
            if r.reason & REASON_DELETE != 0 {
                self.dirs.remove(&r.frn);
                continue;
            }
            if r.reason & (REASON_CREATE | REASON_RENAME_NEW) != 0 {
                match journal.relative_path(r.frn) {
                    Some(rel) if !rel.is_empty() && !skipped(&rel, &self.options.exclude) => {
                        let name = rel.rsplit_once('/').map_or(rel.as_str(), |(_, n)| n);
                        self.dirs.insert(r.frn, (r.parent, name.to_owned()));
                        subtrees.insert(rel);
                    }
                    // Gone again, or moved out of the tree (its old place was marked above).
                    _ => {
                        self.dirs.remove(&r.frn);
                    }
                }
            }
        }
        // Outermost subtrees only; a subtree covers the files under it.
        let mut roots: Vec<String> = Vec::new();
        for dir in subtrees {
            if !roots.iter().any(|r| is_under(&dir, r) || *r == dir) {
                roots.push(dir);
            }
        }
        for dir in &roots {
            let walk = if self.root.join(dir).is_dir() {
                let walk = self.walk(dir)?;
                self.record_dirs(journal, &walk.dirs);
                walk
            } else {
                Walk {
                    files: Vec::new(),
                    dirs: Vec::new(),
                    skipped_names: 0,
                }
            };
            self.apply_walk(dir, walk, report)?;
        }
        for dir in listed {
            if roots.iter().any(|r| is_under(&dir, r) || *r == dir) {
                continue;
            }
            let options = WalkOptions {
                exclude: &self.options.exclude,
                threads: 1,
            };
            let (files, skipped_names) = scan::list_files(&self.root, &dir, &options)
                .map_err(io_error(&self.root.join(&dir)))?;
            self.apply_listing(&dir, files, skipped_names, report)?;
        }
        Ok(())
    }

    /// Reconciles the files directly in `dir` (not below it) with a listing of it.
    fn apply_listing(
        &mut self,
        dir: &str,
        files: Vec<Stat>,
        skipped_names: usize,
        report: &mut UpdateReport,
    ) -> Result<()> {
        report.checked += files.len();
        report.skipped_names += skipped_names;
        let present: HashSet<&str> = files.iter().map(|s| s.path.as_str()).collect();
        let dir_ids = self.db.pools.dirs.matching(|d| d == dir);
        let indexed: Vec<String> = self
            .db
            .live_files()
            .filter(|f| dir_ids.get(self.db.files.dir[f.0 as usize] as usize) == Some(&true))
            .map(|f| self.db.file_path(f))
            .collect();
        for path in indexed {
            if !present.contains(path.as_str()) && self.db.remove_file(&path) {
                report.removed += 1;
            }
        }
        let stale: Vec<Stat> = files.into_iter().filter(|s| self.is_stale(s)).collect();
        self.read_and_put(&stale, report)
    }

    /// Writes the index as a new snapshot; [`open`](Self::open) maps the latest one.
    pub fn save(&mut self) -> Result<PathBuf> {
        if self.db.garbage() > 0.25 {
            self.db = self.db.compact();
            self.incoming = OnceLock::new();
        }
        let mut dirs: Vec<(u64, u64, &str)> = self
            .dirs
            .iter()
            .map(|(&frn, (parent, name))| (frn, *parent, name.as_str()))
            .collect();
        dirs.sort_unstable();
        let names: Vec<u32> = dirs
            .iter()
            .map(|d| self.db.pools.names.intern(d.2))
            .collect();
        self.db.dirs.frn = dirs.iter().map(|d| d.0).collect::<Vec<_>>().into();
        self.db.dirs.parent = dirs.iter().map(|d| d.1).collect::<Vec<_>>().into();
        self.db.dirs.name = names.into();
        self.generation += 1;
        let dir = self.index_dir();
        snapshot::save(&dir, &self.db, self.generation, &self.state).map_err(io_error(&dir))
    }

    // --- Queries ---

    pub fn file_count(&self) -> usize {
        self.db.by_path.len
    }

    pub fn object_count(&self) -> usize {
        self.db.live_objects().count()
    }

    pub fn files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.db.live_files()
    }

    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.db.live_objects()
    }

    /// The file at `path` (relative, `/`-separated).
    pub fn file(&self, path: &str) -> Option<FileId> {
        self.db.find_file(path)
    }

    pub fn file_info(&self, file: FileId) -> FileInfo<'_> {
        let i = file.0 as usize;
        let f = &self.db.files;
        FileInfo {
            path: self.db.file_path(file),
            size: f.size[i],
            mtime: f.mtime[i],
            hash: f.hash[i],
            parse_error: (f.error[i] != NONE).then(|| self.db.pools.messages.get(f.error[i])),
        }
    }

    /// The path links use for a file: relative, without the `.masset` extension.
    pub fn asset_path(&self, file: FileId) -> String {
        let i = file.0 as usize;
        let dir = self.db.pools.dirs.get(self.db.files.dir[i]);
        join(dir, self.db.pools.stems.get(self.db.files.stem[i]))
    }

    pub fn objects_in(&self, file: FileId) -> impl Iterator<Item = ObjectId> + '_ {
        self.db.objects_of(file).map(ObjectId)
    }

    pub fn object(&self, uuid: Uuid) -> Option<ObjectId> {
        self.db.by_uuid.get(uuid.as_u128()).map(ObjectId)
    }

    pub fn object_info(&self, object: ObjectId) -> ObjectInfo<'_> {
        let i = object.0 as usize;
        let o = &self.db.objects;
        ObjectInfo {
            uuid: Uuid::from_u128(o.uuid[i]),
            name: self.db.pools.names.get(o.name[i]),
            type_name: self.db.pools.types.get(o.ty[i]),
            file: FileId(o.file[i]),
        }
    }

    pub fn links(&self, object: ObjectId) -> impl Iterator<Item = LinkInfo<'_>> + '_ {
        let i = object.0 as usize;
        let start = self.db.objects.links_start[i];
        (start..start + self.db.objects.links_len[i]).map(move |l| {
            let l = l as usize;
            let target = self.db.links.target[l];
            LinkInfo {
                target: (target != 0).then_some(Uuid::from_u128(target)),
                path: self.db.pools.refs.get(self.db.links.path[l]),
                name: self.db.pools.names.get(self.db.links.name[l]),
            }
        })
    }

    pub fn source_links(&self, object: ObjectId) -> impl Iterator<Item = SourceLink<'_>> + '_ {
        let i = object.0 as usize;
        let start = self.db.objects.file_links_start[i];
        (start..start + self.db.objects.file_links_len[i]).map(move |l| {
            let l = l as usize;
            SourceLink {
                scheme: self.db.pools.names.get(self.db.file_links.scheme[l]),
                path: self.db.pools.refs.get(self.db.file_links.path[l]),
            }
        })
    }

    /// The target of a link of `object`: by UUID, or by path and name for hand-written ones.
    pub fn link_target(&self, object: ObjectId, link: LinkInfo<'_>) -> Option<ObjectId> {
        match link.target {
            Some(uuid) => self.object(uuid),
            None => {
                let path = if link.path.is_empty() {
                    self.asset_path(self.object_info(object).file)
                } else {
                    link.path.to_owned()
                };
                self.resolve(&path, link.name).and_then(|u| self.object(u))
            }
        }
    }

    /// The UUID of object `name` in the file links call `path`.
    pub fn resolve(&self, path: &str, name: &str) -> Option<Uuid> {
        let file = self.file(&format!("{path}.{MASSET_EXT}"))?;
        self.objects_in(file)
            .map(|o| self.object_info(o))
            .find(|o| o.name == name)
            .map(|o| o.uuid)
    }

    /// Every object of a type.
    pub fn objects_of_type(&self, type_name: &str) -> Vec<ObjectId> {
        let wanted = self.db.pools.types.matching(|t| t == type_name);
        self.par_filter(|o| wanted.get(self.db.objects.ty[o] as usize) == Some(&true))
    }

    /// Objects whose name contains `needle`, ignoring ASCII case.
    pub fn search(&self, needle: &str) -> Vec<ObjectId> {
        let needle = needle.to_ascii_lowercase();
        // Each distinct name is tested once, then objects are matched by name id.
        let names = self
            .db
            .pools
            .names
            .matching(|n| n.to_ascii_lowercase().contains(&needle));
        self.par_filter(|o| names.get(self.db.objects.name[o] as usize) == Some(&true))
    }

    /// Live objects (by row) accepted by `keep`, in parallel.
    fn par_filter(&self, keep: impl Fn(usize) -> bool + Sync) -> Vec<ObjectId> {
        let n = self.db.objects.alive.len();
        let threads = self.threads().clamp(1, n.max(1));
        let chunk = n.div_ceil(threads).max(1);
        let keep = &keep;
        let alive = &self.db.objects.alive;
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..n)
                .step_by(chunk)
                .map(|start| {
                    s.spawn(move || {
                        (start..(start + chunk).min(n))
                            .filter(|&o| alive[o] != 0 && keep(o))
                            .map(|o| ObjectId(o as u32))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        })
    }

    fn incoming(&self) -> &HashMap<u128, Vec<u32>> {
        self.incoming.get_or_init(|| {
            let mut map: HashMap<u128, Vec<u32>> = HashMap::new();
            for o in self.db.live_objects() {
                for link in self.links(o) {
                    if let Some(target) = link.target {
                        map.entry(target.as_u128()).or_default().push(o.0);
                    }
                }
            }
            for rows in map.values_mut() {
                rows.dedup();
            }
            map
        })
    }

    /// Objects with a link to `uuid`.
    pub fn references_to(&self, uuid: Uuid) -> Vec<ObjectId> {
        self.incoming()
            .get(&uuid.as_u128())
            .map(|rows| rows.iter().map(|&r| ObjectId(r)).collect())
            .unwrap_or_default()
    }

    /// Everything `uuid` links to, directly or not (itself excluded), in breadth-first order.
    pub fn dependencies(&self, uuid: Uuid) -> Vec<ObjectId> {
        let Some(start) = self.object(uuid) else {
            return Vec::new();
        };
        let mut seen = HashSet::from([start]);
        let mut queue = VecDeque::from([start]);
        let mut out = Vec::new();
        while let Some(o) = queue.pop_front() {
            for link in self.links(o) {
                if let Some(target) = self.link_target(o, link)
                    && seen.insert(target)
                {
                    out.push(target);
                    queue.push_back(target);
                }
            }
        }
        out
    }

    /// Objects with a file link to source file `path`.
    pub fn users_of_source(&self, path: &str) -> Vec<ObjectId> {
        let matches = self.db.pools.refs.matching(|p| p == path);
        let Some(id) = matches.iter().position(|&m| m) else {
            return Vec::new();
        };
        self.par_filter(|o| {
            let start = self.db.objects.file_links_start[o];
            (start..start + self.db.objects.file_links_len[o])
                .any(|l| self.db.file_links.path[l as usize] as usize == id)
        })
    }

    /// Problems in the indexed content: see [`IssueKind`].
    pub fn validate(&mut self) -> Vec<Issue> {
        validate::run(self)
    }
}

impl marlin_masset::Links for AssetDb {
    fn resolve(&self, path: &str, name: &str) -> Option<Uuid> {
        AssetDb::resolve(self, path, name)
    }

    fn describe(&self, target: Uuid) -> Option<(String, String)> {
        let o = self.object(target)?;
        let info = self.object_info(o);
        Some((self.asset_path(info.file), info.name.to_owned()))
    }
}

/// A stable key for a relative path, as stored by the index (for tools keeping their own maps).
pub fn path_hash(path: &str) -> u128 {
    path_key(path)
}

impl AssetDb {
    /// Pre-submit validation: errors in the files `vcs` would submit, links and source files
    /// they use that would not reach anyone else (neither tracked nor submitted), and links into
    /// files the submit deletes. The index should be up to date first ([`update`](Self::update)).
    pub fn validate_submit(&mut self, vcs: &dyn marlin_vcs::Vcs) -> marlin_vcs::Result<Vec<Issue>> {
        validate::run_submit(self, vcs)
    }
}
