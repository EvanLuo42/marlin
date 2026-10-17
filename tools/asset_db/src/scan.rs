//! Walking the asset tree and reading changed files.
//!
//! The walk only lists directories: names, sizes and modification times come from directory
//! enumeration without opening files. Only files whose size or time changed are opened, then
//! hashed and, for `.masset` files, extracted; both stages run on all cores.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use marlin_hash::xxh3_128;

use crate::MASSET_EXT;
use crate::db::{Content, FileRecord, ObjectRecord};

/// What the directory listing says about a file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Stat {
    /// Relative to the root, `/`-separated.
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) mtime: u64,
}

pub(crate) struct WalkOptions<'a> {
    pub(crate) exclude: &'a [String],
    pub(crate) threads: usize,
}

/// Names never indexed: hidden entries (`.git`, the index's own `.marlin`) and excluded names.
pub(crate) fn skipped(name: &str, exclude: &[String]) -> bool {
    name.starts_with('.') || exclude.iter().any(|e| e == name)
}

pub(crate) fn mtime_of(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// Lists one directory: its files, and its subdirectories to visit next.
fn list(
    root: &Path,
    dir: &str,
    options: &WalkOptions<'_>,
    files: &mut Vec<Stat>,
    dirs: &mut Vec<String>,
    skipped_names: &mut usize,
) -> io::Result<()> {
    let entries = match fs::read_dir(root.join(dir)) {
        Ok(entries) => entries,
        // Deleted between listing its parent and now.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            *skipped_names += 1;
            continue;
        };
        if skipped(&name, options.exclude) {
            continue;
        }
        // On Windows the type and metadata come with the directory entry: no file is opened.
        let kind = entry.file_type()?;
        if kind.is_dir() {
            dirs.push(join(dir, &name));
        } else if kind.is_file() {
            let meta = entry.metadata()?;
            files.push(Stat {
                path: join(dir, &name),
                size: meta.len(),
                mtime: mtime_of(&meta),
            });
        }
    }
    Ok(())
}

/// The files directly in `dir` (none if it no longer exists), and the count of non-UTF-8 names
/// skipped.
pub(crate) fn list_files(
    root: &Path,
    dir: &str,
    options: &WalkOptions<'_>,
) -> io::Result<(Vec<Stat>, usize)> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut skipped_names = 0;
    list(
        root,
        dir,
        options,
        &mut files,
        &mut dirs,
        &mut skipped_names,
    )?;
    Ok((files, skipped_names))
}

/// One thread's share of a level: files, subdirectories, skipped names.
type Listing = (Vec<Stat>, Vec<String>, usize);

/// What a walk found.
pub(crate) struct Walk {
    /// Sorted by path.
    pub(crate) files: Vec<Stat>,
    /// Every directory visited, the start included, parents before children.
    pub(crate) dirs: Vec<String>,
    /// Names that are not UTF-8, left out.
    pub(crate) skipped_names: usize,
}

/// Every file under `start` (a relative directory; empty for the root), breadth first with each
/// level's directories spread over threads.
pub(crate) fn walk(root: &Path, start: &str, options: &WalkOptions<'_>) -> io::Result<Walk> {
    let mut files = Vec::new();
    let mut all_dirs = Vec::new();
    let mut skipped_names = 0;
    let mut level = vec![start.to_owned()];
    while !level.is_empty() {
        all_dirs.extend(level.iter().cloned());
        let results: Mutex<Vec<Listing>> = Mutex::new(Vec::new());
        let error: Mutex<Option<io::Error>> = Mutex::new(None);
        let threads = options.threads.clamp(1, level.len());
        let chunk = level.len().div_ceil(threads);
        std::thread::scope(|s| {
            for dirs in level.chunks(chunk) {
                let (results, error) = (&results, &error);
                s.spawn(move || {
                    let mut out = (Vec::new(), Vec::new(), 0);
                    for dir in dirs {
                        if let Err(e) = list(root, dir, options, &mut out.0, &mut out.1, &mut out.2)
                        {
                            error
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get_or_insert(e);
                            return;
                        }
                    }
                    results.lock().unwrap_or_else(|e| e.into_inner()).push(out);
                });
            }
        });
        if let Some(e) = error.into_inner().unwrap_or_else(|e| e.into_inner()) {
            return Err(e);
        }
        let mut next = Vec::new();
        for (f, d, n) in results.into_inner().unwrap_or_else(|e| e.into_inner()) {
            files.extend(f);
            next.extend(d);
            skipped_names += n;
        }
        next.sort_unstable();
        level = next;
    }
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(Walk {
        files,
        dirs: all_dirs,
        skipped_names,
    })
}

/// Files larger than this are hashed through a memory map instead of being read.
const MAP_THRESHOLD: u64 = 16 << 20;

fn hash_file(path: &Path, size: u64) -> io::Result<(u128, Option<Vec<u8>>)> {
    if size >= MAP_THRESHOLD {
        let file = fs::File::open(path)?;
        // SAFETY: the map is read-only and dropped before returning. Another process writing the
        // file meanwhile changes what is hashed, not memory safety; the next scan sees its new
        // time and hashes it again.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        return Ok((xxh3_128(&map), None));
    }
    let bytes = fs::read(path)?;
    Ok((xxh3_128(&bytes), Some(bytes)))
}

fn record(root: &Path, stat: &Stat) -> io::Result<FileRecord> {
    let full = root.join(&stat.path);
    let is_masset = stat
        .path
        .rsplit_once('.')
        .is_some_and(|(_, e)| e == MASSET_EXT);
    let (hash, bytes) = if is_masset {
        // Text is always read: it is extracted too.
        let bytes = fs::read(&full)?;
        (xxh3_128(&bytes), Some(bytes))
    } else {
        hash_file(&full, stat.size)?
    };
    let content = if !is_masset {
        Content::Binary
    } else {
        let bytes = bytes.unwrap_or_default();
        match std::str::from_utf8(&bytes) {
            Err(_) => Content::Error("not UTF-8 text".into()),
            Ok(text) => match marlin_masset::extract(text) {
                Err(e) => Content::Error(e.to_string()),
                Ok(extract) => Content::Objects(
                    extract
                        .objects
                        .into_iter()
                        .map(|o| ObjectRecord {
                            uuid: o.uuid,
                            name: o.name.into_owned(),
                            type_name: o.type_name.to_owned(),
                            links: o
                                .links
                                .iter()
                                .map(|l| (l.uuid, l.path.to_owned(), l.name.to_owned()))
                                .collect(),
                            file_links: o
                                .files
                                .iter()
                                .map(|f| (f.scheme.to_owned(), f.path.to_owned()))
                                .collect(),
                        })
                        .collect(),
                ),
            },
        }
    };
    Ok(FileRecord {
        path: stat.path.clone(),
        size: stat.size,
        mtime: stat.mtime,
        hash,
        content,
    })
}

/// Reads, hashes and extracts `stats` in parallel. A file that vanished in the meantime is
/// left out; other IO errors fail the scan.
pub(crate) fn read_all(root: &Path, stats: &[Stat], threads: usize) -> io::Result<Vec<FileRecord>> {
    if stats.is_empty() {
        return Ok(Vec::new());
    }
    let threads = threads.clamp(1, stats.len());
    let chunk = stats.len().div_ceil(threads);
    let parts: Vec<io::Result<Vec<FileRecord>>> = std::thread::scope(|s| {
        let handles: Vec<_> = stats
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    let mut out = Vec::with_capacity(part.len());
                    for stat in part {
                        match record(root, stat) {
                            Ok(r) => out.push(r),
                            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e),
                        }
                    }
                    Ok(out)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    let mut records = Vec::with_capacity(stats.len());
    for part in parts {
        records.extend(part?);
    }
    Ok(records)
}
