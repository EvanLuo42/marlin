//! The tables: files, objects, links and file links, as columns of string ids and numbers.
//!
//! Rows are appended; changing a file kills its old object and link rows and appends new ones,
//! so the tables only grow between compactions. Compaction rebuilds everything from live rows.

use marlin_asset::Uuid;
use marlin_hash::xxh3_128;

use crate::column::Column;
use crate::strings::{NONE, StringPool};
use crate::table::KeyTable;

/// A row in the file table.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FileId(pub u32);

/// A row in the object table.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ObjectId(pub u32);

#[derive(Default)]
pub(crate) struct Files {
    pub(crate) dir: Column<u32>,
    pub(crate) stem: Column<u32>,
    pub(crate) ext: Column<u32>,
    pub(crate) size: Column<u64>,
    /// Last write time, nanoseconds since the Unix epoch.
    pub(crate) mtime: Column<u64>,
    /// xxh3-128 of the contents.
    pub(crate) hash: Column<u128>,
    pub(crate) objects_start: Column<u32>,
    pub(crate) objects_len: Column<u32>,
    /// Parse error of a `.masset` file (messages pool), or `NONE`.
    pub(crate) error: Column<u32>,
    pub(crate) alive: Column<u8>,
}

#[derive(Default)]
pub(crate) struct Objects {
    pub(crate) uuid: Column<u128>,
    pub(crate) name: Column<u32>,
    pub(crate) ty: Column<u32>,
    pub(crate) file: Column<u32>,
    pub(crate) links_start: Column<u32>,
    pub(crate) links_len: Column<u32>,
    pub(crate) file_links_start: Column<u32>,
    pub(crate) file_links_len: Column<u32>,
    pub(crate) alive: Column<u8>,
}

#[derive(Default)]
pub(crate) struct Links {
    /// 0 when the link was written without a UUID.
    pub(crate) target: Column<u128>,
    /// The `path` part as written (refs pool), empty for same-file links.
    pub(crate) path: Column<u32>,
    pub(crate) name: Column<u32>,
}

#[derive(Default)]
pub(crate) struct FileLinks {
    pub(crate) scheme: Column<u32>,
    pub(crate) path: Column<u32>,
}

/// Indexed directories by NTFS file reference number, for resolving change journal records
/// (see `journal`). Only the persisted form: `AssetDb` works on a map built from it.
#[derive(Default)]
pub(crate) struct Dirs {
    pub(crate) frn: Column<u64>,
    pub(crate) parent: Column<u64>,
    /// Names pool.
    pub(crate) name: Column<u32>,
}

#[derive(Default)]
pub(crate) struct Pools {
    pub(crate) dirs: StringPool,
    pub(crate) stems: StringPool,
    pub(crate) exts: StringPool,
    /// Object names, link names and file-link schemes.
    pub(crate) names: StringPool,
    pub(crate) types: StringPool,
    /// Link paths and source file paths.
    pub(crate) refs: StringPool,
    pub(crate) messages: StringPool,
}

#[derive(Default)]
pub(crate) struct Db {
    pub(crate) files: Files,
    pub(crate) objects: Objects,
    pub(crate) links: Links,
    pub(crate) file_links: FileLinks,
    pub(crate) dirs: Dirs,
    pub(crate) pools: Pools,
    /// xxh3-128 of the relative path to its live file row.
    pub(crate) by_path: KeyTable,
    /// UUID to the first live object row holding it.
    pub(crate) by_uuid: KeyTable,
    /// Further live rows of UUIDs held by more than one object (an error the validator reports),
    /// built on the first change: a mapped index is only read until then.
    duplicates: Option<std::collections::HashMap<u128, Vec<u32>>>,
}

/// What a scan learned about one file.
pub(crate) struct FileRecord {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) mtime: u64,
    pub(crate) hash: u128,
    pub(crate) content: Content,
}

pub(crate) enum Content {
    /// Not a `.masset` file: indexed as a file only.
    Binary,
    Objects(Vec<ObjectRecord>),
    Error(String),
}

pub(crate) struct ObjectRecord {
    pub(crate) uuid: Uuid,
    pub(crate) name: String,
    pub(crate) type_name: String,
    pub(crate) links: Vec<(Option<Uuid>, String, String)>,
    pub(crate) file_links: Vec<(String, String)>,
}

pub(crate) fn path_key(path: &str) -> u128 {
    let key = xxh3_128(path.as_bytes());
    // Keep clear of the table's empty and deleted markers.
    if KeyTable::valid_key(key) { key } else { 1 }
}

/// `dir`, `stem`, `ext` of a `/`-separated relative path (`ext` without the dot).
pub(crate) fn split_path(path: &str) -> (&str, &str, &str) {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (dir, stem, ext),
        _ => (dir, file, ""),
    }
}

impl Db {
    pub(crate) fn file_path(&self, file: FileId) -> String {
        let f = &self.files;
        let i = file.0 as usize;
        let dir = self.pools.dirs.get(f.dir[i]);
        let stem = self.pools.stems.get(f.stem[i]);
        let ext = self.pools.exts.get(f.ext[i]);
        let mut path = String::with_capacity(dir.len() + stem.len() + ext.len() + 2);
        if !dir.is_empty() {
            path.push_str(dir);
            path.push('/');
        }
        path.push_str(stem);
        if !ext.is_empty() {
            path.push('.');
            path.push_str(ext);
        }
        path
    }

    pub(crate) fn file_alive(&self, file: FileId) -> bool {
        self.files.alive[file.0 as usize] != 0
    }

    pub(crate) fn object_alive(&self, object: ObjectId) -> bool {
        self.objects.alive[object.0 as usize] != 0
    }

    pub(crate) fn live_files(&self) -> impl Iterator<Item = FileId> + '_ {
        (0..self.files.alive.len() as u32)
            .map(FileId)
            .filter(|&f| self.file_alive(f))
    }

    pub(crate) fn live_objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        (0..self.objects.alive.len() as u32)
            .map(ObjectId)
            .filter(|&o| self.object_alive(o))
    }

    pub(crate) fn find_file(&self, path: &str) -> Option<FileId> {
        self.by_path.get(path_key(path)).map(FileId)
    }

    pub(crate) fn objects_of(&self, file: FileId) -> std::ops::Range<u32> {
        let i = file.0 as usize;
        let start = self.files.objects_start[i];
        start..start + self.files.objects_len[i]
    }

    /// Removes a file's rows; true if it was indexed.
    pub(crate) fn remove_file(&mut self, path: &str) -> bool {
        let Some(row) = self.by_path.remove(path_key(path)) else {
            return false;
        };
        let file = FileId(row);
        self.files.alive.set(row as usize, 0);
        for o in self.objects_of(file) {
            self.kill_object(ObjectId(o));
        }
        true
    }

    fn duplicates(&mut self) -> &mut std::collections::HashMap<u128, Vec<u32>> {
        if self.duplicates.is_none() {
            let mut map: std::collections::HashMap<u128, Vec<u32>> = Default::default();
            for o in self.live_objects() {
                let uuid = self.objects.uuid[o.0 as usize];
                if self.by_uuid.get(uuid).is_some_and(|first| first != o.0) {
                    map.entry(uuid).or_default().push(o.0);
                }
            }
            self.duplicates = Some(map);
        }
        self.duplicates
            .as_mut()
            .unwrap_or_else(|| unreachable!("built above"))
    }

    fn kill_object(&mut self, object: ObjectId) {
        let i = object.0 as usize;
        self.objects.alive.set(i, 0);
        let uuid = self.objects.uuid[i];
        let first = self.by_uuid.get(uuid) == Some(object.0);
        let duplicates = self.duplicates();
        let promote = match duplicates.get_mut(&uuid) {
            Some(rows) => {
                rows.retain(|&r| r != object.0);
                let promote = if first { rows.pop() } else { None };
                if rows.is_empty() {
                    duplicates.remove(&uuid);
                }
                promote
            }
            None => None,
        };
        if first {
            self.by_uuid.remove(uuid);
            // Another live object with the same UUID takes over the entry.
            if let Some(row) = promote {
                self.by_uuid.insert(uuid, row);
            }
        }
    }

    /// Adds or replaces the rows for `record.path`.
    pub(crate) fn put_file(&mut self, record: &FileRecord) -> FileId {
        self.remove_file(&record.path);
        let (dir, stem, ext) = split_path(&record.path);
        let dir = self.pools.dirs.intern(dir);
        let stem = self.pools.stems.intern(stem);
        let ext = self.pools.exts.intern(ext);
        let file = FileId(self.files.alive.len() as u32);
        let objects_start = self.objects.alive.len() as u32;
        let mut error = NONE;
        let mut objects_len = 0;
        match &record.content {
            Content::Binary => {}
            Content::Error(message) => error = self.pools.messages.intern(message),
            Content::Objects(objects) => {
                objects_len = objects.len() as u32;
                for object in objects {
                    self.push_object(file, object);
                }
            }
        }
        let f = &mut self.files;
        f.dir.push(dir);
        f.stem.push(stem);
        f.ext.push(ext);
        f.size.push(record.size);
        f.mtime.push(record.mtime);
        f.hash.push(record.hash);
        f.objects_start.push(objects_start);
        f.objects_len.push(objects_len);
        f.error.push(error);
        f.alive.push(1);
        self.by_path.insert(path_key(&record.path), file.0);
        file
    }

    fn push_object(&mut self, file: FileId, record: &ObjectRecord) {
        // Built from the rows as they were, before this one exists.
        self.duplicates();
        let row = self.objects.alive.len() as u32;
        let links_start = self.links.target.len() as u32;
        for (uuid, path, name) in &record.links {
            self.links.target.push(uuid.map_or(0, Uuid::as_u128));
            self.links.path.push(self.pools.refs.intern(path));
            self.links.name.push(self.pools.names.intern(name));
        }
        let file_links_start = self.file_links.path.len() as u32;
        for (scheme, path) in &record.file_links {
            self.file_links.scheme.push(self.pools.names.intern(scheme));
            self.file_links.path.push(self.pools.refs.intern(path));
        }
        let o = &mut self.objects;
        o.uuid.push(record.uuid.as_u128());
        o.name.push(self.pools.names.intern(&record.name));
        o.ty.push(self.pools.types.intern(&record.type_name));
        o.file.push(file.0);
        o.links_start.push(links_start);
        o.links_len.push(record.links.len() as u32);
        o.file_links_start.push(file_links_start);
        o.file_links_len.push(record.file_links.len() as u32);
        o.alive.push(1);
        let uuid = record.uuid.as_u128();
        if KeyTable::valid_key(uuid) {
            if self.by_uuid.get(uuid).is_none() {
                self.by_uuid.insert(uuid, row);
            } else {
                self.duplicates().entry(uuid).or_default().push(row);
            }
        }
    }

    /// Rows holding a UUID that another live object also holds.
    pub(crate) fn duplicate_rows(&mut self) -> Vec<(u128, Vec<u32>)> {
        let mut out: Vec<(u128, Vec<u32>)> = self
            .duplicates()
            .iter()
            .map(|(&u, rows)| (u, rows.clone()))
            .collect();
        out.sort_unstable();
        out
    }

    /// The record of a live file, for compaction.
    fn record(&self, file: FileId) -> FileRecord {
        let i = file.0 as usize;
        let content = if self.files.error[i] != NONE {
            Content::Error(self.pools.messages.get(self.files.error[i]).to_owned())
        } else if self.pools.exts.get(self.files.ext[i]) == crate::MASSET_EXT {
            Content::Objects(
                self.objects_of(file)
                    .map(|o| self.object_record(ObjectId(o)))
                    .collect(),
            )
        } else {
            Content::Binary
        };
        FileRecord {
            path: self.file_path(file),
            size: self.files.size[i],
            mtime: self.files.mtime[i],
            hash: self.files.hash[i],
            content,
        }
    }

    fn object_record(&self, object: ObjectId) -> ObjectRecord {
        let i = object.0 as usize;
        let o = &self.objects;
        let p = &self.pools;
        let links = (o.links_start[i]..o.links_start[i] + o.links_len[i])
            .map(|l| {
                let l = l as usize;
                let target = self.links.target[l];
                (
                    (target != 0).then_some(Uuid::from_u128(target)),
                    p.refs.get(self.links.path[l]).to_owned(),
                    p.names.get(self.links.name[l]).to_owned(),
                )
            })
            .collect();
        let file_links = (o.file_links_start[i]..o.file_links_start[i] + o.file_links_len[i])
            .map(|l| {
                let l = l as usize;
                (
                    p.names.get(self.file_links.scheme[l]).to_owned(),
                    p.refs.get(self.file_links.path[l]).to_owned(),
                )
            })
            .collect();
        ObjectRecord {
            uuid: Uuid::from_u128(o.uuid[i]),
            name: p.names.get(o.name[i]).to_owned(),
            type_name: p.types.get(o.ty[i]).to_owned(),
            links,
            file_links,
        }
    }

    /// Fraction of object rows that are dead.
    pub(crate) fn garbage(&self) -> f64 {
        let total = self.files.alive.len() + self.objects.alive.len();
        if total == 0 {
            return 0.0;
        }
        let live = self.by_path.len + self.objects.alive.iter().filter(|&&a| a != 0).count();
        1.0 - live as f64 / total as f64
    }

    /// Rebuilds the tables from live rows, dropping dead rows and unused strings.
    pub(crate) fn compact(&self) -> Db {
        let mut files: Vec<FileId> = self.live_files().collect();
        // Path order: stable snapshots, and files of a directory stay together.
        files.sort_by_cached_key(|&f| self.file_path(f));
        let mut db = Db::default();
        for file in files {
            db.put_file(&self.record(file));
        }
        db
    }
}
