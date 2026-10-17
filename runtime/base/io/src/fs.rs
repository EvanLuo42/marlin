//! File system operations: metadata, creating, removing and renaming, and listing directories.

use alloc::string::String;
use alloc::vec::Vec;

use marlin_pal::fs as pal;
pub use marlin_pal::fs::{FileKind, Metadata};

use crate::{Error, ErrorKind, Path, PathBuf, Result};

/// Metadata of what `path` names, following symbolic links.
pub fn metadata(path: impl AsRef<Path>) -> Result<Metadata> {
    pal::metadata(path.as_ref().as_str())
}

/// Whether something exists at `path` (following links). Errors other than `NotFound` count as
/// "exists", since something is there that could not be inspected.
pub fn exists(path: impl AsRef<Path>) -> bool {
    match metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != ErrorKind::NotFound,
    }
}

/// Removes a file (not a directory).
pub fn remove_file(path: impl AsRef<Path>) -> Result<()> {
    pal::remove_file(path.as_ref().as_str())
}

/// Creates a directory whose parent exists. `AlreadyExists` if something is there.
pub fn create_dir(path: impl AsRef<Path>) -> Result<()> {
    pal::create_dir(path.as_ref().as_str())
}

/// Creates a directory and any missing parents. Succeeds if the directory already exists.
pub fn create_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    if path.is_empty() {
        return Ok(());
    }
    match pal::create_dir(path.as_str()) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return if metadata(path)?.kind == FileKind::Dir {
                Ok(())
            } else {
                Err(error)
            };
        }
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }
    // The parent is missing: create it first, then retry.
    match path.parent() {
        Some(parent) if !parent.is_empty() && parent != path => create_dir_all(parent)?,
        _ => return Err(Error::from_kind(ErrorKind::NotFound)),
    }
    match pal::create_dir(path.as_str()) {
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

/// Removes an empty directory; `DirectoryNotEmpty` otherwise.
pub fn remove_dir(path: impl AsRef<Path>) -> Result<()> {
    pal::remove_dir(path.as_ref().as_str())
}

/// Removes a directory and everything in it. Symbolic links inside are removed, not followed.
pub fn remove_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    for entry in read_dir(path)? {
        let entry = entry?;
        match entry.kind {
            FileKind::Dir => remove_dir_all(&entry.path)?,
            // A directory link is removed as a directory on Windows, as a file elsewhere.
            FileKind::Symlink if cfg!(windows) && metadata(&entry.path)?.kind == FileKind::Dir => {
                remove_dir(&entry.path)?
            }
            _ => remove_file(&entry.path)?,
        }
    }
    remove_dir(path)
}

/// Renames `from` to `to`, replacing `to` if it is a file.
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<()> {
    pal::rename(from.as_ref().as_str(), to.as_ref().as_str())
}

/// One entry of a directory listing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    /// The listed directory joined with the entry's name.
    pub path: PathBuf,
    /// What the entry is, without following links.
    pub kind: FileKind,
}

impl DirEntry {
    pub fn file_name(&self) -> &str {
        self.path.file_name().unwrap_or("")
    }
}

/// Lists the entries of `path`, without `.` and `..`, in no particular order.
pub fn read_dir(path: impl AsRef<Path>) -> Result<ReadDir> {
    let path = path.as_ref();
    Ok(ReadDir {
        raw: pal::ReadDir::open(path.as_str())?,
        dir: path.to_path_buf(),
        name: alloc::vec![0; 256],
    })
}

/// The entries of a directory; see [`read_dir`].
pub struct ReadDir {
    raw: pal::ReadDir,
    dir: PathBuf,
    name: Vec<u8>,
}

impl Iterator for ReadDir {
    type Item = Result<DirEntry>;

    fn next(&mut self) -> Option<Result<DirEntry>> {
        match self.raw.next(&mut self.name) {
            Ok(None) => None,
            Err(error) => Some(Err(error)),
            Ok(Some(entry)) => {
                if entry.name_len > self.name.len() {
                    // The entry is consumed; names are at most 255 UTF-16 units, which is 765
                    // bytes, so size for the worst case and report this one as unreadable.
                    self.name.resize(1024, 0);
                    return Some(Err(Error::from_kind(ErrorKind::InvalidData)));
                }
                let name = &self.name[..entry.name_len];
                // The PAL checked it is UTF-8.
                let name = String::from_utf8_lossy(name);
                Some(Ok(DirEntry {
                    path: self.dir.join(&*name),
                    kind: entry.kind,
                }))
            }
        }
    }
}

/// Walks the tree under `root` depth first, yielding every entry below it (not `root` itself).
/// Directories come before their contents; symbolic links are reported, not followed.
///
/// Development builds use this to scan source trees; shipping code reads packages instead.
pub fn walk_dir(root: impl AsRef<Path>) -> Result<WalkDir> {
    Ok(WalkDir {
        stack: alloc::vec![read_dir(root)?],
    })
}

/// See [`walk_dir`].
pub struct WalkDir {
    stack: Vec<ReadDir>,
}

impl Iterator for WalkDir {
    type Item = Result<DirEntry>;

    fn next(&mut self) -> Option<Result<DirEntry>> {
        loop {
            let top = self.stack.last_mut()?;
            match top.next() {
                None => {
                    self.stack.pop();
                }
                Some(Ok(entry)) => {
                    if entry.kind == FileKind::Dir {
                        match read_dir(&entry.path) {
                            Ok(dir) => self.stack.push(dir),
                            Err(error) => return Some(Err(error)),
                        }
                    }
                    return Some(Ok(entry));
                }
                Some(Err(error)) => return Some(Err(error)),
            }
        }
    }
}
