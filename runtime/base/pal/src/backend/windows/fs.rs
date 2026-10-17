//! Path operations, directory listing and file mapping over Win32.

use core::ptr::{self, NonNull};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, FILETIME, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, DeleteFileW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch,
    FindFirstFileExW, FindNextFileW, FlushFileBuffers, GetFileInformationByHandle,
    MOVEFILE_REPLACE_EXISTING, MoveFileExW, OPEN_EXISTING, RemoveDirectoryW, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, FlushViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, PAGE_READONLY, PAGE_READWRITE, UnmapViewOfFile,
};

use super::error::{check, last_error};
use super::file::File;
use super::{Backend, utf16_to_utf8, with_wide_path};
use crate::contract::{Fs, VMem};
use crate::fs::{DirEntry, FileKind, Metadata};
use crate::{Error, ErrorKind, Result};

/// The name-surrogate bit of a reparse tag (`IsReparseTagNameSurrogate` in `winnt.h`).
const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;

/// 100 ns intervals from 1601-01-01 (the FILETIME epoch) to 1970-01-01.
const EPOCH_DIFFERENCE: i64 = 116_444_736_000_000_000;

pub(crate) struct Dir {
    handle: HANDLE,
    data: WIN32_FIND_DATAW,
    /// `data` holds an entry not returned yet (the one FindFirstFileExW found).
    pending: bool,
}

// SAFETY: find handles may be used from any thread, one at a time (`&mut`).
unsafe impl Send for Dir {}

impl Drop for Dir {
    fn drop(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            // SAFETY: we own the find handle.
            unsafe { FindClose(self.handle) };
        }
    }
}

pub(crate) struct Mapping {
    base: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: a view may be unmapped from any thread.
unsafe impl Send for Mapping {}
// SAFETY: `Mapping` has no `&self` methods that mutate.
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `base` is a view we mapped and nothing uses it any more.
        unsafe { UnmapViewOfFile(self.base) };
    }
}

// SAFETY: `map` returns a pointer into a view of exactly the requested range, unmapped on drop.
unsafe impl Fs for Backend {
    type Dir = Dir;
    type Mapping = Mapping;

    fn sync(file: &File, _data_only: bool) -> Result<()> {
        // SAFETY: the handle is live.
        check(unsafe { FlushFileBuffers(file.handle) })
    }

    fn file_metadata(file: &File) -> Result<Metadata> {
        handle_metadata(file.handle)
    }

    fn metadata(path: &str) -> Result<Metadata> {
        // Opening the path follows links; backup semantics allow directories.
        let handle = with_wide_path(path, "", |path| {
            // SAFETY: `path` is NUL-terminated; null attributes and template are allowed.
            let handle = unsafe {
                CreateFileW(
                    path,
                    FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                Err(last_error())
            } else {
                Ok(handle)
            }
        })?;
        let result = handle_metadata(handle);
        // SAFETY: we own the handle.
        unsafe { CloseHandle(handle) };
        result
    }

    fn remove_file(path: &str) -> Result<()> {
        // SAFETY: `path` is NUL-terminated.
        with_wide_path(path, "", |path| check(unsafe { DeleteFileW(path) }))
    }

    fn create_dir(path: &str) -> Result<()> {
        with_wide_path(path, "", |path| {
            // SAFETY: `path` is NUL-terminated; null security attributes are allowed.
            check(unsafe { CreateDirectoryW(path, ptr::null()) })
        })
    }

    fn remove_dir(path: &str) -> Result<()> {
        // SAFETY: `path` is NUL-terminated.
        with_wide_path(path, "", |path| check(unsafe { RemoveDirectoryW(path) }))
    }

    fn rename(from: &str, to: &str) -> Result<()> {
        with_wide_path(from, "", |from| {
            with_wide_path(to, "", |to| {
                // SAFETY: both paths are NUL-terminated.
                check(unsafe { MoveFileExW(from, to, MOVEFILE_REPLACE_EXISTING) })
            })
        })
    }

    fn read_dir(path: &str) -> Result<Dir> {
        // FindFirstFile on a file reports success with that file; check first.
        if Self::metadata(path)?.kind != FileKind::Dir {
            return Err(Error::from_kind(ErrorKind::NotADirectory));
        }
        let mut dir = Dir {
            handle: INVALID_HANDLE_VALUE,
            data: WIN32_FIND_DATAW::default(),
            pending: false,
        };
        with_wide_path(path, "\\*", |pattern| {
            // SAFETY: `pattern` is NUL-terminated; `data` is a valid out buffer.
            let handle = unsafe {
                FindFirstFileExW(
                    pattern,
                    FindExInfoBasic,
                    (&raw mut dir.data).cast(),
                    FindExSearchNameMatch,
                    ptr::null(),
                    FIND_FIRST_EX_LARGE_FETCH,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                // SAFETY: no preconditions.
                return match unsafe { GetLastError() } {
                    // A drive root has no `.` entries, so it can be empty.
                    ERROR_FILE_NOT_FOUND => Ok(()),
                    _ => Err(last_error()),
                };
            }
            dir.handle = handle;
            dir.pending = true;
            Ok(())
        })?;
        Ok(dir)
    }

    fn dir_next(dir: &mut Dir, name: &mut [u8]) -> Result<Option<DirEntry>> {
        loop {
            if dir.handle == INVALID_HANDLE_VALUE {
                return Ok(None);
            }
            if !dir.pending {
                // SAFETY: the find handle is live; `data` is a valid out buffer.
                if unsafe { FindNextFileW(dir.handle, &mut dir.data) } == 0 {
                    // SAFETY: no preconditions.
                    let error = unsafe { GetLastError() };
                    // SAFETY: we own the handle and mark it closed.
                    unsafe { FindClose(dir.handle) };
                    dir.handle = INVALID_HANDLE_VALUE;
                    return if error == ERROR_NO_MORE_FILES {
                        Ok(None)
                    } else {
                        Err(super::error::from_win32(error))
                    };
                }
            }
            dir.pending = false;
            let file_name = &dir.data.cFileName;
            let len = file_name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(file_name.len());
            let file_name = &file_name[..len];
            let dot = u16::from(b'.');
            if file_name == [dot] || file_name == [dot, dot] {
                continue;
            }
            let attributes = dir.data.dwFileAttributes;
            // Symbolic links, junctions and other name surrogates (`IsReparseTagNameSurrogate`)
            // stand for another path; listing reports them as links so that nothing walks or
            // deletes through them. Other reparse points (cloud files, dedup) are what they hold.
            let kind = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                && dir.data.dwReserved0 & REPARSE_TAG_NAME_SURROGATE != 0
            {
                FileKind::Symlink
            } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                FileKind::Dir
            } else {
                FileKind::File
            };
            let name_len = utf16_to_utf8(file_name, name)?;
            return Ok(Some(DirEntry { kind, name_len }));
        }
    }

    fn map(file: &File, offset: u64, len: usize, writable: bool) -> Result<(NonNull<u8>, Mapping)> {
        // Views start at a multiple of the allocation granularity.
        let granularity = Backend::allocation_granularity() as u64;
        let start = offset - offset % granularity;
        let delta = (offset - start) as usize;
        let (protect, access) = if writable {
            (PAGE_READWRITE, FILE_MAP_READ | FILE_MAP_WRITE)
        } else {
            (PAGE_READONLY, FILE_MAP_READ)
        };
        // SAFETY: the handle is live; a zero size maps the whole file; null attributes and name
        // are allowed.
        let section =
            unsafe { CreateFileMappingW(file.handle, ptr::null(), protect, 0, 0, ptr::null()) };
        if section.is_null() {
            return Err(last_error());
        }
        // SAFETY: the section is live; the caller checked the range lies inside the file.
        let base = unsafe {
            MapViewOfFile(
                section,
                access,
                (start >> 32) as u32,
                start as u32,
                delta + len,
            )
        };
        let error = last_error();
        // The view keeps the section alive.
        // SAFETY: we own the section handle.
        unsafe { CloseHandle(section) };
        let Some(view) = NonNull::new(base.Value.cast::<u8>()) else {
            return Err(error);
        };
        // SAFETY: `delta` lies inside the view.
        let ptr = unsafe { view.add(delta) };
        Ok((ptr, Mapping { base }))
    }

    fn map_flush(_mapping: &Mapping, ptr: NonNull<u8>, len: usize) -> Result<()> {
        // SAFETY: the range lies inside a live view.
        check(unsafe { FlushViewOfFile(ptr.as_ptr().cast(), len) })
    }
}

fn handle_metadata(handle: HANDLE) -> Result<Metadata> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is live; `info` is a valid out pointer.
    check(unsafe { GetFileInformationByHandle(handle, &mut info) })?;
    let kind = if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        FileKind::Dir
    } else {
        FileKind::File
    };
    Ok(Metadata {
        kind,
        len: u64::from(info.nFileSizeHigh) << 32 | u64::from(info.nFileSizeLow),
        modified: unix_time(info.ftLastWriteTime),
        readonly: info.dwFileAttributes & FILE_ATTRIBUTE_READONLY != 0,
    })
}

fn unix_time(ft: FILETIME) -> (i64, u32) {
    let intervals = (u64::from(ft.dwHighDateTime) << 32 | u64::from(ft.dwLowDateTime)) as i64
        - EPOCH_DIFFERENCE;
    (
        intervals.div_euclid(10_000_000),
        (intervals.rem_euclid(10_000_000) * 100) as u32,
    )
}
