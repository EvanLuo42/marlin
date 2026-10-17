//! The file system beyond reading and writing open files: metadata, flushing, removing, renaming,
//! creating and listing directories, and memory-mapped views.
//!
//! Paths follow the rules of [`crate::file`]: UTF-8, at most [`MAX_PATH_LEN`] bytes, `/` or `\`
//! separators on Windows (where they are passed with a `\\?\` prefix), relative to the current
//! directory unless absolute. Names come back as UTF-8 written into a caller buffer, as in
//! [`crate::process`].

use core::ptr::NonNull;

use crate::backend::imp::Backend;
use crate::contract::Fs as Contract;
use crate::file::{File, MAX_PATH_LEN};
use crate::{Error, ErrorKind, Result};

/// What a path names.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FileKind {
    File,
    Dir,
    /// Only reported by [`ReadDir`], which does not follow links; [`metadata`] does.
    Symlink,
    /// Devices, pipes, sockets and the like.
    Other,
}

/// What [`metadata`] and [`File::metadata`] report.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Metadata {
    pub kind: FileKind,
    /// Size in bytes (0 for directories on most systems).
    pub len: u64,
    /// Last modification, as seconds and nanoseconds since the Unix epoch (see
    /// [`time::system_time`](crate::time::system_time)).
    pub modified: (i64, u32),
    /// The read-only attribute on Windows; no write permission bit at all on POSIX.
    pub readonly: bool,
}

/// One entry of a [`ReadDir`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DirEntry {
    pub kind: FileKind,
    /// Full length of the name in bytes; the buffer passed to [`ReadDir::next`] holds the name
    /// when this is at most its length.
    pub name_len: usize,
}

fn check_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > MAX_PATH_LEN || path.as_bytes().contains(&0) {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    Ok(())
}

/// Metadata of what `path` names, following symbolic links. `NotFound` if nothing is there.
pub fn metadata(path: &str) -> Result<Metadata> {
    check_path(path)?;
    Backend::metadata(path)
}

/// Removes a file (not a directory). On Windows a file others still have open disappears once
/// they close it.
pub fn remove_file(path: &str) -> Result<()> {
    check_path(path)?;
    Backend::remove_file(path)
}

/// Creates a directory; its parent must exist. `AlreadyExists` if something is there.
pub fn create_dir(path: &str) -> Result<()> {
    check_path(path)?;
    Backend::create_dir(path)
}

/// Removes an empty directory; `DirectoryNotEmpty` otherwise.
pub fn remove_dir(path: &str) -> Result<()> {
    check_path(path)?;
    Backend::remove_dir(path)
}

/// Renames `from` to `to`, replacing `to` if it is a file. Atomic within one volume.
pub fn rename(from: &str, to: &str) -> Result<()> {
    check_path(from)?;
    check_path(to)?;
    Backend::rename(from, to)
}

impl File {
    /// Flushes the file's data and metadata to the storage device (`FlushFileBuffers`, `fsync`,
    /// `F_FULLFSYNC` on macOS, whose `fsync` stops at the drive's cache). Needs write access on
    /// Windows.
    pub fn sync_all(&self) -> Result<()> {
        Backend::sync(self.raw(), false)
    }

    /// Like [`sync_all`](Self::sync_all), but may skip metadata not needed to read the data back
    /// (`fdatasync` on Linux; the same as `sync_all` elsewhere).
    pub fn sync_data(&self) -> Result<()> {
        Backend::sync(self.raw(), true)
    }

    pub fn metadata(&self) -> Result<Metadata> {
        Backend::file_metadata(self.raw())
    }
}

/// The entries of a directory, without `.` and `..`, in no particular order. Dropping it closes
/// the directory.
pub struct ReadDir(<Backend as Contract>::Dir);

impl ReadDir {
    /// Opens `path` for listing: `NotFound` if it does not exist, `NotADirectory` if it is a file.
    pub fn open(path: &str) -> Result<Self> {
        check_path(path)?;
        Backend::read_dir(path).map(Self)
    }

    /// Writes the next entry's name to `name` and returns the entry, or `None` at the end. A name
    /// that is not valid Unicode gives `InvalidData`; listing continues with the next call.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self, name: &mut [u8]) -> Result<Option<DirEntry>> {
        Backend::dir_next(&mut self.0, name)
    }
}

/// A memory-mapped view of part of a file. Dropping it unmaps the view; the file may be closed
/// before that.
///
/// The view reflects changes made to the file by anyone, at any time, so the PAL hands out raw
/// pointers only.
pub struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
    raw: <Backend as Contract>::Mapping,
}

// SAFETY: the view is plain memory; access goes through raw pointers whose use the caller
// synchronizes.
unsafe impl Send for Mapping {}
// SAFETY: as above.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Maps `len` bytes of `file` from `offset`, read-only or (with `writable`, which needs a
    /// file opened for writing) shared read-write: writes reach the file. `offset` need not be
    /// aligned. `InvalidInput` for `len == 0` or a range past the end of the file.
    pub fn new(file: &File, offset: u64, len: usize, writable: bool) -> Result<Self> {
        let end = offset.checked_add(len as u64);
        if len == 0 || end.is_none_or(|end| end > file.size().unwrap_or(0)) {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        let (ptr, raw) = Backend::map(file.raw(), offset, len, writable)?;
        Ok(Self { ptr, len, raw })
    }

    /// The first mapped byte, which is the byte at `offset` in the file.
    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Writes modified pages back to the file (`FlushViewOfFile`, `msync`). On Windows that
    /// reaches the OS cache; follow with [`File::sync_all`] for durability across power loss.
    pub fn flush(&self) -> Result<()> {
        Backend::map_flush(&self.raw, self.ptr, self.len)
    }
}
