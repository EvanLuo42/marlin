//! Path operations, directory listing and file mapping over POSIX, shared by Linux and macOS.

use core::ffi::{CStr, c_char, c_int};
use core::mem;
use core::ptr::{self, NonNull};

use super::file::File;
use super::process::copy_utf8;
use super::{Backend, errno, last_error, set_errno, with_c_path};
use crate::contract::{Fs, VMem};
use crate::fs::{DirEntry, FileKind, Metadata};
use crate::{Error, ErrorKind, Result};

pub(crate) struct Dir(NonNull<libc::DIR>);

// SAFETY: a DIR stream may be used from any thread, one at a time (`&mut`).
unsafe impl Send for Dir {}

impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: we own the stream.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

pub(crate) struct Mapping {
    base: *mut libc::c_void,
    len: usize,
}

// SAFETY: a mapping may be unmapped from any thread.
unsafe impl Send for Mapping {}
// SAFETY: `Mapping` has no `&self` methods that mutate.
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: we own the mapping and nothing uses it any more.
        unsafe { libc::munmap(self.base, self.len) };
    }
}

fn check(ret: c_int) -> Result<()> {
    if ret == 0 { Ok(()) } else { Err(last_error()) }
}

/// Retries `f` while it fails with EINTR.
fn retry(mut f: impl FnMut() -> c_int) -> Result<()> {
    loop {
        if f() == 0 {
            return Ok(());
        }
        if errno() != libc::EINTR {
            return Err(last_error());
        }
    }
}

fn kind_of(mode: libc::mode_t) -> FileKind {
    match mode & libc::S_IFMT {
        libc::S_IFREG => FileKind::File,
        libc::S_IFDIR => FileKind::Dir,
        libc::S_IFLNK => FileKind::Symlink,
        _ => FileKind::Other,
    }
}

fn metadata_of(st: &libc::stat) -> Metadata {
    Metadata {
        kind: kind_of(st.st_mode),
        len: st.st_size as u64,
        modified: (st.st_mtime, st.st_mtime_nsec as u32),
        readonly: st.st_mode & 0o222 == 0,
    }
}

// SAFETY: `map` returns a pointer into a mapping of exactly the requested range, unmapped on drop.
unsafe impl Fs for Backend {
    type Dir = Dir;
    type Mapping = Mapping;

    fn sync(file: &File, data_only: bool) -> Result<()> {
        // macOS fsync stops at the drive's cache; F_FULLFSYNC flushes it, where supported.
        #[cfg(target_vendor = "apple")]
        {
            let _ = data_only;
            // SAFETY: the descriptor is open.
            if unsafe { libc::fcntl(file.fd, libc::F_FULLFSYNC) } == 0 {
                return Ok(());
            }
            // SAFETY: as above.
            retry(|| unsafe { libc::fsync(file.fd) })
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            // SAFETY: the descriptor is open.
            retry(|| unsafe {
                if data_only {
                    libc::fdatasync(file.fd)
                } else {
                    libc::fsync(file.fd)
                }
            })
        }
    }

    fn file_metadata(file: &File) -> Result<Metadata> {
        // SAFETY: plain data that fstat fills in.
        let mut st: libc::stat = unsafe { mem::zeroed() };
        // SAFETY: `st` is a valid out pointer.
        check(unsafe { libc::fstat(file.fd, &mut st) })?;
        Ok(metadata_of(&st))
    }

    fn metadata(path: &str) -> Result<Metadata> {
        with_c_path(path, |path| {
            // SAFETY: plain data that stat fills in.
            let mut st: libc::stat = unsafe { mem::zeroed() };
            // SAFETY: `path` is NUL-terminated; `st` is a valid out pointer.
            check(unsafe { libc::stat(path, &mut st) })?;
            Ok(metadata_of(&st))
        })
    }

    fn remove_file(path: &str) -> Result<()> {
        // SAFETY: `path` is NUL-terminated.
        with_c_path(path, |path| check(unsafe { libc::unlink(path) }))
    }

    fn create_dir(path: &str) -> Result<()> {
        // SAFETY: `path` is NUL-terminated.
        with_c_path(path, |path| check(unsafe { libc::mkdir(path, 0o777) }))
    }

    fn remove_dir(path: &str) -> Result<()> {
        // SAFETY: `path` is NUL-terminated.
        with_c_path(path, |path| check(unsafe { libc::rmdir(path) }))
    }

    fn rename(from: &str, to: &str) -> Result<()> {
        with_c_path(from, |from| {
            // SAFETY: both paths are NUL-terminated.
            with_c_path(to, |to| check(unsafe { libc::rename(from, to) }))
        })
    }

    fn read_dir(path: &str) -> Result<Dir> {
        with_c_path(path, |path| {
            // SAFETY: `path` is NUL-terminated.
            let dir = unsafe { libc::opendir(path) };
            NonNull::new(dir).map(Dir).ok_or_else(last_error)
        })
    }

    fn dir_next(dir: &mut Dir, name: &mut [u8]) -> Result<Option<DirEntry>> {
        loop {
            // readdir reports errors only through errno, and leaves it alone at the end.
            set_errno(0);
            // SAFETY: the stream is live.
            let entry = unsafe { libc::readdir(dir.0.as_ptr()) };
            if entry.is_null() {
                return match errno() {
                    0 => Ok(None),
                    code => Err(super::from_errno(code)),
                };
            }
            // SAFETY: readdir returned a valid entry, live until the next call on this stream.
            let (d_type, file_name) = unsafe {
                (
                    (*entry).d_type,
                    CStr::from_ptr((&raw const (*entry).d_name).cast::<c_char>()),
                )
            };
            let bytes = file_name.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let kind = match d_type {
                libc::DT_REG => FileKind::File,
                libc::DT_DIR => FileKind::Dir,
                libc::DT_LNK => FileKind::Symlink,
                libc::DT_UNKNOWN => {
                    // Some file systems do not fill in d_type; ask without following links.
                    // SAFETY: plain data that fstatat fills in.
                    let mut st: libc::stat = unsafe { mem::zeroed() };
                    // SAFETY: the stream's descriptor is live; the name is NUL-terminated.
                    let r = unsafe {
                        libc::fstatat(
                            libc::dirfd(dir.0.as_ptr()),
                            file_name.as_ptr(),
                            &mut st,
                            libc::AT_SYMLINK_NOFOLLOW,
                        )
                    };
                    if r == 0 {
                        kind_of(st.st_mode)
                    } else {
                        FileKind::Other
                    }
                }
                _ => FileKind::Other,
            };
            let name_len = copy_utf8(bytes, name)?;
            return Ok(Some(DirEntry { kind, name_len }));
        }
    }

    fn map(file: &File, offset: u64, len: usize, writable: bool) -> Result<(NonNull<u8>, Mapping)> {
        let page = Backend::page_size() as u64;
        let start = offset - offset % page;
        let delta = (offset - start) as usize;
        let total = delta + len;
        let prot = if writable {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        let start =
            libc::off_t::try_from(start).map_err(|_| Error::from_kind(ErrorKind::InvalidInput))?;
        // SAFETY: a shared mapping of an open file at a kernel-chosen address; the caller checked
        // the range lies inside the file.
        let base = unsafe {
            libc::mmap(
                ptr::null_mut(),
                total,
                prot,
                libc::MAP_SHARED,
                file.fd,
                start,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(last_error());
        }
        // SAFETY: mmap never returns null on success, and `delta` lies inside the mapping.
        let ptr = unsafe { NonNull::new_unchecked(base.cast::<u8>().add(delta)) };
        Ok((ptr, Mapping { base, len: total }))
    }

    fn map_flush(_mapping: &Mapping, ptr: NonNull<u8>, len: usize) -> Result<()> {
        let page = Backend::page_size();
        let start = ptr.as_ptr().map_addr(|a| a - a % page);
        let len = len + (ptr.as_ptr().addr() - start.addr());
        // SAFETY: the page-aligned range lies inside a live mapping.
        check(unsafe { libc::msync(start.cast(), len, libc::MS_SYNC) })
    }
}
