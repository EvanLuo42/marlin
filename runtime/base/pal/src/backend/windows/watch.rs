//! Directory watching with `ReadDirectoryChangesW`.
//!
//! One overlapped read is in flight at a time, into a buffer in its own pages (the PAL does not
//! allocate). Between reads the OS keeps collecting changes for the handle, so nothing is lost
//! while a batch is handed out; a batch that overflows comes back empty, which is reported as
//! `Overflow`.

use core::ptr::{self, NonNull};
use core::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_INCOMPLETE, ERROR_NOTIFY_ENUM_DIR, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
    FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

use super::error::{check, last_error};
use super::file::timeout_ms;
use super::{Backend, Utf8Writer, with_wide_path};
use crate::contract::{VMem, Watch};
use crate::watch::{WatchEvent, WatchKind};
use crate::{Protection, Result};

/// The largest buffer `ReadDirectoryChangesW` accepts for network shares.
const BUFFER: usize = 64 << 10;

/// Lives at the start of the watcher's pages, so the OVERLAPPED and the buffer never move.
#[repr(C)]
struct Shared {
    ov: OVERLAPPED,
    buf: [u32; BUFFER / 4],
}

pub(crate) struct Watcher {
    dir: HANDLE,
    event: HANDLE,
    shared: NonNull<Shared>,
    pages: usize,
    /// A read is in flight.
    reading: bool,
    /// Bytes of events in the buffer, and the offset of the next one to hand out.
    filled: usize,
    next: usize,
}

// SAFETY: the handles may be used from any thread; the pages are owned by the watcher.
unsafe impl Send for Watcher {}

impl Watcher {
    fn issue(&mut self) -> Result<()> {
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_DIR_NAME
            | FILE_NOTIFY_CHANGE_ATTRIBUTES
            | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE
            | FILE_NOTIFY_CHANGE_CREATION;
        let shared = self.shared.as_ptr();
        // SAFETY: the pages are committed and ours; the OVERLAPPED and buffer stay put until the
        // read completes or is cancelled in `drop`. The event is reset first: it is manual-reset.
        unsafe {
            ResetEvent(self.event);
            (*shared).ov = OVERLAPPED {
                hEvent: self.event,
                ..Default::default()
            };
            check(ReadDirectoryChangesW(
                self.dir,
                (&raw mut (*shared).buf).cast(),
                BUFFER as u32,
                1,
                filter,
                ptr::null_mut(),
                &raw mut (*shared).ov,
                None,
            ))?;
        }
        self.reading = true;
        Ok(())
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: we own the handles and the pages; the read is cancelled and waited for before
        // the buffer is released.
        unsafe {
            if self.reading {
                let ov = &raw mut (*self.shared.as_ptr()).ov;
                CancelIoEx(self.dir, ov);
                let mut n = 0;
                GetOverlappedResult(self.dir, ov, &mut n, 1);
            }
            CloseHandle(self.dir);
            CloseHandle(self.event);
            let _ = Backend::release(self.shared.cast(), self.pages);
        }
    }
}

impl Watch for Backend {
    type Watcher = Watcher;

    fn watch_new(root: &str) -> Result<Watcher> {
        let dir = with_wide_path(root, "", |path| {
            // SAFETY: `path` is NUL-terminated; null attributes and template are allowed.
            let handle = unsafe {
                CreateFileW(
                    path,
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                Err(last_error())
            } else {
                Ok(handle)
            }
        })?;
        // SAFETY: manual-reset, unsignaled, unnamed; null attributes are allowed.
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            let error = last_error();
            // SAFETY: we own the directory handle.
            unsafe { CloseHandle(dir) };
            return Err(error);
        }
        let pages = size_of::<Shared>().next_multiple_of(Backend::allocation_granularity());
        let shared = Backend::reserve(pages).and_then(|base| {
            // SAFETY: commits the reservation just made.
            unsafe { Backend::commit(base, pages, Protection::ReadWrite) }.map(|()| base)
        });
        let shared = match shared {
            Ok(base) => base.cast::<Shared>(),
            Err(error) => {
                // SAFETY: we own both handles.
                unsafe {
                    CloseHandle(dir);
                    CloseHandle(event);
                }
                return Err(error);
            }
        };
        let mut watcher = Watcher {
            dir,
            event,
            shared,
            pages,
            reading: false,
            filled: 0,
            next: 0,
        };
        // Changes are collected from the first call on, so start reading now.
        watcher.issue()?;
        Ok(watcher)
    }

    fn watch_next(
        w: &mut Watcher,
        path: &mut [u8],
        timeout: Option<Duration>,
    ) -> Result<Option<WatchEvent>> {
        loop {
            if w.next < w.filled {
                // SAFETY: `next` is the offset of a complete record the OS wrote into the buffer;
                // records are DWORD aligned and hold `FileNameLength` bytes of name.
                let (event, step) = unsafe {
                    let base = (&raw const (*w.shared.as_ptr()).buf).cast::<u8>();
                    let info = base.add(w.next).cast::<FILE_NOTIFY_INFORMATION>();
                    let name = core::slice::from_raw_parts(
                        (&raw const (*info).FileName).cast::<u16>(),
                        (*info).FileNameLength as usize / 2,
                    );
                    let kind = match (*info).Action {
                        FILE_ACTION_ADDED => WatchKind::Created,
                        FILE_ACTION_REMOVED => WatchKind::Removed,
                        FILE_ACTION_RENAMED_OLD_NAME => WatchKind::RenamedFrom,
                        FILE_ACTION_RENAMED_NEW_NAME => WatchKind::RenamedTo,
                        _ => WatchKind::Modified,
                    };
                    let mut writer = Utf8Writer::new(path);
                    for &unit in name {
                        writer.push(if unit == u16::from(b'\\') {
                            u16::from(b'/')
                        } else {
                            unit
                        })?;
                    }
                    let path_len = writer.finish()?;
                    (
                        WatchEvent { kind, path_len },
                        (*info).NextEntryOffset as usize,
                    )
                };
                w.next = if step == 0 { w.filled } else { w.next + step };
                if w.next >= w.filled {
                    w.issue()?;
                }
                return Ok(Some(event));
            }

            // SAFETY: the event belongs to the read in flight.
            match unsafe { WaitForSingleObject(w.event, timeout_ms(timeout)) } {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => return Ok(None),
                _ => return Err(last_error()),
            }
            let mut n = 0;
            // SAFETY: the OVERLAPPED belongs to the read in flight; it has finished.
            let ok = unsafe {
                GetOverlappedResult(w.dir, &raw const (*w.shared.as_ptr()).ov, &mut n, 0)
            };
            w.reading = false;
            if ok == 0 {
                // SAFETY: no preconditions.
                match unsafe { GetLastError() } {
                    ERROR_IO_INCOMPLETE => {
                        w.reading = true;
                        continue;
                    }
                    // The changes did not fit the buffer.
                    ERROR_NOTIFY_ENUM_DIR => {}
                    _ => return Err(last_error()),
                }
            }
            if n == 0 {
                w.filled = 0;
                w.next = 0;
                w.issue()?;
                return Ok(Some(WatchEvent {
                    kind: WatchKind::Overflow,
                    path_len: 0,
                }));
            }
            w.filled = n as usize;
            w.next = 0;
        }
    }
}
