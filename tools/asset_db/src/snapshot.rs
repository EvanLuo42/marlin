//! The on-disk index: every column, 64-byte aligned, behind a section table, so a memory map of
//! the file is the index.
//!
//! ```text
//! header   magic "MARLINDB" version:u32 sections:u32 generation:u64
//!          journal: volume:u64 id:u64 next_usn:u64 root:u64 valid:u64
//! table    [id:u32 element_size:u32 offset:u64 count:u64] * sections
//! data     column bytes, each at a 64-byte boundary
//! ```
//!
//! Snapshots are immutable: a save writes `index-<generation>.madb` next to the old one and then
//! points `CURRENT` at it. On Windows a file mapped by another process (an editor reading the
//! index) cannot be replaced, so overwriting in place is not an option anyway.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;

use crate::column::{Column, Pod};
use crate::db::Db;
use crate::journal::JournalState;
use crate::table::KeyTable;

const MAGIC: &[u8; 8] = b"MARLINDB";
const VERSION: u32 = 1;
const ALIGN: usize = 64;
const HEADER: usize = 8 + 4 + 4 + 8 + 40;
const SECTION: usize = 4 + 4 + 8 + 8;

/// Visits every column of a `Db` with a stable section id.
macro_rules! columns {
    ($db:expr, $visit:ident) => {
        $visit!(1, $db.files.dir);
        $visit!(2, $db.files.stem);
        $visit!(3, $db.files.ext);
        $visit!(4, $db.files.size);
        $visit!(5, $db.files.mtime);
        $visit!(6, $db.files.hash);
        $visit!(7, $db.files.objects_start);
        $visit!(8, $db.files.objects_len);
        $visit!(9, $db.files.error);
        $visit!(10, $db.files.alive);
        $visit!(20, $db.objects.uuid);
        $visit!(21, $db.objects.name);
        $visit!(22, $db.objects.ty);
        $visit!(23, $db.objects.file);
        $visit!(24, $db.objects.links_start);
        $visit!(25, $db.objects.links_len);
        $visit!(26, $db.objects.file_links_start);
        $visit!(27, $db.objects.file_links_len);
        $visit!(28, $db.objects.alive);
        $visit!(40, $db.links.target);
        $visit!(41, $db.links.path);
        $visit!(42, $db.links.name);
        $visit!(50, $db.file_links.scheme);
        $visit!(51, $db.file_links.path);
        $visit!(55, $db.dirs.frn);
        $visit!(56, $db.dirs.parent);
        $visit!(57, $db.dirs.name);
        $visit!(60, $db.pools.dirs.bytes);
        $visit!(61, $db.pools.dirs.offsets);
        $visit!(62, $db.pools.stems.bytes);
        $visit!(63, $db.pools.stems.offsets);
        $visit!(64, $db.pools.exts.bytes);
        $visit!(65, $db.pools.exts.offsets);
        $visit!(66, $db.pools.names.bytes);
        $visit!(67, $db.pools.names.offsets);
        $visit!(68, $db.pools.types.bytes);
        $visit!(69, $db.pools.types.offsets);
        $visit!(70, $db.pools.refs.bytes);
        $visit!(71, $db.pools.refs.offsets);
        $visit!(72, $db.pools.messages.bytes);
        $visit!(73, $db.pools.messages.offsets);
        $visit!(80, $db.by_path.keys);
        $visit!(81, $db.by_path.rows);
        $visit!(82, $db.by_uuid.keys);
        $visit!(83, $db.by_uuid.rows);
    };
}

fn element_size<T: Pod>(_: &Column<T>) -> u32 {
    size_of::<T>() as u32
}

/// Writes `db` as `dir/index-<generation>.madb` and makes it current. Returns the file's path.
pub(crate) fn save(
    dir: &Path,
    db: &Db,
    generation: u64,
    journal: &JournalState,
) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let mut sections: Vec<(u32, u32, &[u8], u64)> = Vec::new();
    macro_rules! add {
        ($id:literal, $column:expr) => {
            sections.push((
                $id,
                element_size(&$column),
                $column.as_bytes(),
                $column.len() as u64,
            ));
        };
    }
    columns!(db, add);

    let table_end = HEADER + SECTION * sections.len();
    let mut offset = table_end.next_multiple_of(ALIGN);
    let mut header = Vec::with_capacity(offset);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&VERSION.to_le_bytes());
    header.extend_from_slice(&(sections.len() as u32).to_le_bytes());
    header.extend_from_slice(&generation.to_le_bytes());
    for word in journal.to_words() {
        header.extend_from_slice(&word.to_le_bytes());
    }
    for &(id, size, bytes, count) in &sections {
        header.extend_from_slice(&id.to_le_bytes());
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&(offset as u64).to_le_bytes());
        header.extend_from_slice(&count.to_le_bytes());
        offset = (offset + bytes.len()).next_multiple_of(ALIGN);
    }
    header.resize(table_end.next_multiple_of(ALIGN), 0);

    let name = format!("index-{generation:016x}.madb");
    let path = dir.join(&name);
    let temp = dir.join(format!("{name}.tmp"));
    {
        let mut out = io::BufWriter::with_capacity(1 << 20, fs::File::create(&temp)?);
        out.write_all(&header)?;
        let mut written = header.len();
        for &(_, _, bytes, _) in &sections {
            out.write_all(bytes)?;
            written += bytes.len();
            let padded = written.next_multiple_of(ALIGN);
            out.write_all(&[0; ALIGN][..padded - written])?;
            written = padded;
        }
        out.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()?;
    }
    fs::rename(&temp, &path)?;
    let current = dir.join("CURRENT.tmp");
    fs::write(&current, &name)?;
    fs::rename(&current, dir.join("CURRENT"))?;

    // Older snapshots: best effort, another process may still map one.
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            if file.starts_with("index-") && file.ends_with(".madb") && file != name {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(path)
}

/// A loaded snapshot: the tables point into the map.
pub(crate) struct Loaded {
    pub(crate) db: Db,
    pub(crate) generation: u64,
    pub(crate) journal: JournalState,
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap_or_default())
}

fn read_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

fn load_column<T: Pod>(
    column: &mut Column<T>,
    map: &Arc<Mmap>,
    sections: &[(u32, u32, u64, u64)],
    id: u32,
) -> Option<()> {
    let &(_, size, offset, count) = sections.iter().find(|s| s.0 == id)?;
    if size as usize != size_of::<T>() {
        return None;
    }
    *column = Column::mapped(map, offset.try_into().ok()?, count.try_into().ok()?)?;
    Some(())
}

/// Opens the current snapshot in `dir`. `None` when there is none or it is unreadable (a
/// different version, a truncated file): the caller rebuilds.
pub(crate) fn load(dir: &Path) -> Option<Loaded> {
    let name = fs::read_to_string(dir.join("CURRENT")).ok()?;
    let file = fs::File::open(dir.join(name.trim())).ok()?;
    // SAFETY: snapshots are never modified after `save` renames them into place; a new save
    // writes a new file. The map is read-only.
    let map = Arc::new(unsafe { Mmap::map(&file) }.ok()?);
    let b: &[u8] = &map;
    if b.len() < HEADER || &b[..8] != MAGIC || read_u32(b, 8) != VERSION {
        return None;
    }
    let count = read_u32(b, 12) as usize;
    let generation = read_u64(b, 16);
    let journal = JournalState::from_words([
        read_u64(b, 24),
        read_u64(b, 32),
        read_u64(b, 40),
        read_u64(b, 48),
        read_u64(b, 56),
    ]);
    if b.len() < HEADER + SECTION * count {
        return None;
    }
    let sections: Vec<(u32, u32, u64, u64)> = (0..count)
        .map(|i| {
            let at = HEADER + SECTION * i;
            (
                read_u32(b, at),
                read_u32(b, at + 4),
                read_u64(b, at + 8),
                read_u64(b, at + 16),
            )
        })
        .collect();

    let mut db = Db::default();
    let mut ok = true;
    macro_rules! read {
        ($id:literal, $column:expr) => {
            ok &= load_column(&mut $column, &map, &sections, $id).is_some();
        };
    }
    columns!(db, read);
    if !ok {
        return None;
    }
    db.by_path = KeyTable::from_columns(
        std::mem::take(&mut db.by_path.keys),
        std::mem::take(&mut db.by_path.rows),
    )?;
    db.by_uuid = KeyTable::from_columns(
        std::mem::take(&mut db.by_uuid.keys),
        std::mem::take(&mut db.by_uuid.rows),
    )?;
    validate(&db).then_some(Loaded {
        db,
        generation,
        journal,
    })
}

/// Cheap structural checks: matching column lengths and well-formed pool offsets. Row contents
/// are trusted: only `save` writes snapshots, and it renames them into place whole.
fn validate(db: &Db) -> bool {
    let f = &db.files;
    let n = f.alive.len();
    let files_ok = [
        f.dir.len(),
        f.stem.len(),
        f.ext.len(),
        f.size.len(),
        f.mtime.len(),
        f.hash.len(),
        f.objects_start.len(),
        f.objects_len.len(),
        f.error.len(),
    ]
    .iter()
    .all(|&l| l == n);
    let o = &db.objects;
    let m = o.alive.len();
    let objects_ok = [
        o.uuid.len(),
        o.name.len(),
        o.ty.len(),
        o.file.len(),
        o.links_start.len(),
        o.links_len.len(),
        o.file_links_start.len(),
        o.file_links_len.len(),
    ]
    .iter()
    .all(|&l| l == m);
    let links_ok = db.links.path.len() == db.links.target.len()
        && db.links.name.len() == db.links.target.len()
        && db.file_links.scheme.len() == db.file_links.path.len()
        && db.dirs.parent.len() == db.dirs.frn.len()
        && db.dirs.name.len() == db.dirs.frn.len();
    let p = &db.pools;
    let pools_ok = [
        &p.dirs,
        &p.stems,
        &p.exts,
        &p.names,
        &p.types,
        &p.refs,
        &p.messages,
    ]
    .iter()
    .all(|pool| {
        pool.offsets.windows(2).all(|w| w[0] <= w[1])
            && pool
                .offsets
                .last()
                .is_none_or(|&end| end as usize <= pool.bytes.len())
    });
    files_ok && objects_ok && links_ok && pools_ok
}
