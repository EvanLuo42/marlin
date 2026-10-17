//! Directory watching with FSEvents (per-file events), delivered on a private dispatch queue.
//!
//! The callback runs on the queue's thread and appends records to a byte ring in pages the
//! watcher owns; `watch_next` takes them out under a small futex lock. FSEvents flags accumulate
//! an item's recent history (a file written right after its creation reports both), so one
//! FSEvents event can become several `WatchEvent`s, in the order the flags imply.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_void};
use core::mem;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use crate::backend::unix::{Backend, last_error};
use crate::contract::{Futex, VMem, Watch};
use crate::file::{Deadline, MAX_PATH_LEN};
use crate::watch::{WatchEvent, WatchKind};
use crate::{Error, ErrorKind, Protection, Result};

const PATH_CAP: usize = MAX_PATH_LEN * 4;
const RING: usize = 1 << 20;

const CREATE_FLAG_NO_DEFER: u32 = 0x02;
const CREATE_FLAG_FILE_EVENTS: u32 = 0x10;
const EVENT_ID_SINCE_NOW: u64 = u64::MAX;

const FLAG_MUST_SCAN_SUB_DIRS: u32 = 0x0001;
const FLAG_USER_DROPPED: u32 = 0x0002;
const FLAG_KERNEL_DROPPED: u32 = 0x0004;
const FLAG_ITEM_CREATED: u32 = 0x0100;
const FLAG_ITEM_REMOVED: u32 = 0x0200;
const FLAG_ITEM_INODE_META_MOD: u32 = 0x0400;
const FLAG_ITEM_RENAMED: u32 = 0x0800;
const FLAG_ITEM_MODIFIED: u32 = 0x1000;
const FLAG_ITEM_CHANGE_OWNER: u32 = 0x4000;
const FLAG_ITEM_XATTR_MOD: u32 = 0x8000;

const UTF8: u32 = 0x0800_0100;

type CfRef = *const c_void;

#[repr(C)]
struct StreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

#[repr(C)]
struct ArrayCallBacks {
    version: isize,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

type StreamCallback = extern "C" fn(
    stream: *mut c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: CfRef,
        callback: StreamCallback,
        context: *const StreamContext,
        paths: CfRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
    fn FSEventStreamStart(stream: *mut c_void) -> u8;
    fn FSEventStreamStop(stream: *mut c_void);
    fn FSEventStreamInvalidate(stream: *mut c_void);
    fn FSEventStreamRelease(stream: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: ArrayCallBacks;
    fn CFStringCreateWithBytes(
        allocator: CfRef,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: u8,
    ) -> CfRef;
    fn CFArrayCreate(
        allocator: CfRef,
        values: *const CfRef,
        count: isize,
        callbacks: *const ArrayCallBacks,
    ) -> CfRef;
    fn CFRelease(cf: CfRef);
}

// libdispatch is part of libSystem.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    fn dispatch_sync_f(queue: *mut c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_release(object: *mut c_void);
}

/// Shared between the callback and the watcher, in the watcher's own pages. Only the atomics are
/// touched without the lock; `data` only with it.
#[repr(C)]
struct Shared {
    lock: AtomicU32,
    /// Bumped on every push; `watch_next` sleeps on it.
    seq: AtomicU32,
    data: UnsafeCell<Data>,
}

#[repr(C)]
struct Data {
    overflow: bool,
    read: usize,
    used: usize,
    root_len: usize,
    root: [u8; PATH_CAP],
    ring: [u8; RING],
}

impl Shared {
    /// Runs `f` with the data, under the lock.
    fn with<R>(&self, f: impl FnOnce(&mut Data) -> R) -> R {
        while self
            .lock
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            Backend::wait(&self.lock, 1, None);
        }
        // SAFETY: the lock gives exclusive access to the data.
        let result = f(unsafe { &mut *self.data.get() });
        self.lock.store(0, Ordering::Release);
        Backend::wake_one(&self.lock);
        result
    }
}

impl Data {
    /// Appends a record `[kind][len: u16][path]`; sets `overflow` if it does not fit.
    fn push(&mut self, kind: WatchKind, path: &[u8]) {
        let len = path.len().min(u16::MAX as usize);
        if self.used + 3 + len > RING {
            self.overflow = true;
            return;
        }
        let header = [kind as u8, len as u8, (len >> 8) as u8];
        for &b in header.iter().chain(&path[..len]) {
            self.ring[(self.read + self.used) % RING] = b;
            self.used += 1;
        }
    }

    fn pop_byte(&mut self) -> u8 {
        let b = self.ring[self.read];
        self.read = (self.read + 1) % RING;
        self.used -= 1;
        b
    }
}

const KINDS: [WatchKind; 6] = [
    WatchKind::Created,
    WatchKind::Removed,
    WatchKind::Modified,
    WatchKind::RenamedFrom,
    WatchKind::RenamedTo,
    WatchKind::Overflow,
];

extern "C" fn callback(
    _stream: *mut c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    // SAFETY: `info` is the watcher's `Shared`, alive until the queue is drained in `drop`.
    let shared = unsafe { &*info.cast::<Shared>() };
    let paths = paths.cast::<*const c_char>();
    shared.with(|data| {
        for i in 0..count {
            // SAFETY: FSEvents passes `count` flags and C strings (no CF types were requested).
            let (flags, path) = unsafe { (*flags.add(i), CStr::from_ptr(*paths.add(i))) };
            if flags & (FLAG_MUST_SCAN_SUB_DIRS | FLAG_USER_DROPPED | FLAG_KERNEL_DROPPED) != 0 {
                data.overflow = true;
                continue;
            }
            let full = path.to_bytes();
            let Some(rel) = full.strip_prefix(&data.root[..data.root_len]) else {
                continue;
            };
            let rel = rel.strip_prefix(b"/").unwrap_or(rel);
            let mut rel_buf = [0u8; PATH_CAP];
            let n = rel.len().min(PATH_CAP);
            rel_buf[..n].copy_from_slice(&rel[..n]);
            let rel = &rel_buf[..n];

            // SAFETY: plain data that lstat fills in.
            let mut st: libc::stat = unsafe { mem::zeroed() };
            // SAFETY: the path is NUL-terminated; `st` is a valid out pointer.
            let exists = unsafe { libc::lstat(path.as_ptr(), &mut st) } == 0;
            if flags & FLAG_ITEM_CREATED != 0 {
                data.push(WatchKind::Created, rel);
            }
            let modified = FLAG_ITEM_MODIFIED
                | FLAG_ITEM_INODE_META_MOD
                | FLAG_ITEM_XATTR_MOD
                | FLAG_ITEM_CHANGE_OWNER;
            if flags & modified != 0 && exists {
                data.push(WatchKind::Modified, rel);
            }
            if flags & FLAG_ITEM_RENAMED != 0 {
                let kind = if exists {
                    WatchKind::RenamedTo
                } else {
                    WatchKind::RenamedFrom
                };
                data.push(kind, rel);
            }
            if flags & FLAG_ITEM_REMOVED != 0 && !exists {
                data.push(WatchKind::Removed, rel);
            }
        }
    });
    shared.seq.fetch_add(1, Ordering::Release);
    Backend::wake_all(&shared.seq);
}

extern "C" fn noop(_: *mut c_void) {}

pub(crate) struct Watcher {
    stream: *mut c_void,
    queue: *mut c_void,
    shared: NonNull<Shared>,
    pages: usize,
}

// SAFETY: the stream and queue may be used from any thread; the pages are owned by the watcher.
unsafe impl Send for Watcher {}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: we own the stream, the queue and the pages. Stopping and invalidating the stream
        // ends deliveries; draining the queue waits for a callback already running before the
        // pages go.
        unsafe {
            if !self.stream.is_null() {
                FSEventStreamStop(self.stream);
                FSEventStreamInvalidate(self.stream);
                FSEventStreamRelease(self.stream);
            }
            dispatch_sync_f(self.queue, ptr::null_mut(), noop);
            dispatch_release(self.queue);
            let _ = Backend::release(self.shared.cast(), self.pages);
        }
    }
}

impl Watch for Backend {
    type Watcher = Watcher;

    fn watch_new(root: &str) -> Result<Watcher> {
        // FSEvents reports real paths, so resolve links (/tmp -> /private/tmp) in the root.
        let mut c_root = [0u8; MAX_PATH_LEN + 1];
        c_root[..root.len()].copy_from_slice(root.as_bytes());
        let mut real = [0u8; libc::PATH_MAX as usize + 1];
        // SAFETY: both buffers are valid; `real` holds PATH_MAX bytes.
        if unsafe { libc::realpath(c_root.as_ptr().cast(), real.as_mut_ptr().cast()) }.is_null() {
            return Err(last_error());
        }
        let real_len = real.iter().position(|&b| b == 0).unwrap_or(0);
        // SAFETY: plain data that stat fills in.
        let mut st: libc::stat = unsafe { mem::zeroed() };
        // SAFETY: `real` is NUL-terminated.
        if unsafe { libc::stat(real.as_ptr().cast(), &mut st) } != 0 {
            return Err(last_error());
        }
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(Error::from_kind(ErrorKind::NotADirectory));
        }
        if real_len > PATH_CAP {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }

        let pages = size_of::<Shared>().next_multiple_of(Backend::page_size());
        let base = Backend::reserve(pages)?;
        // SAFETY: commits the reservation just made; fresh pages are zero, a valid `Shared`.
        if let Err(error) = unsafe { Backend::commit(base, pages, Protection::ReadWrite) } {
            // SAFETY: releases the reservation just made.
            let _ = unsafe { Backend::release(base, pages) };
            return Err(error);
        }
        let shared = base.cast::<Shared>();
        // SAFETY: the pages are ours and zeroed; nothing else sees them yet.
        unsafe {
            let data = &mut *(*shared.as_ptr()).data.get();
            data.root[..real_len].copy_from_slice(&real[..real_len]);
            data.root_len = real_len;
        }

        // SAFETY: a serial queue with a static label.
        let queue = unsafe { dispatch_queue_create(c"marlin.watch".as_ptr(), ptr::null()) };
        let mut watcher = Watcher {
            stream: ptr::null_mut(),
            queue,
            shared,
            pages,
        };
        // SAFETY: CF objects are created and released here; the context points at `shared`,
        // which outlives the stream (see `drop`).
        unsafe {
            let path =
                CFStringCreateWithBytes(ptr::null(), real.as_ptr(), real_len as isize, UTF8, 0);
            if path.is_null() {
                return Err(Error::from_kind(ErrorKind::OutOfMemory));
            }
            let paths = CFArrayCreate(ptr::null(), &path, 1, &kCFTypeArrayCallBacks);
            CFRelease(path);
            if paths.is_null() {
                return Err(Error::from_kind(ErrorKind::OutOfMemory));
            }
            let context = StreamContext {
                version: 0,
                info: shared.as_ptr().cast(),
                retain: ptr::null(),
                release: ptr::null(),
                copy_description: ptr::null(),
            };
            watcher.stream = FSEventStreamCreate(
                ptr::null(),
                callback,
                &context,
                paths,
                EVENT_ID_SINCE_NOW,
                0.05,
                CREATE_FLAG_FILE_EVENTS | CREATE_FLAG_NO_DEFER,
            );
            CFRelease(paths);
            if watcher.stream.is_null() {
                return Err(Error::from_kind(ErrorKind::Other));
            }
            FSEventStreamSetDispatchQueue(watcher.stream, queue);
            if FSEventStreamStart(watcher.stream) == 0 {
                return Err(Error::from_kind(ErrorKind::Other));
            }
        }
        Ok(watcher)
    }

    fn watch_next(
        w: &mut Watcher,
        path: &mut [u8],
        timeout: Option<Duration>,
    ) -> Result<Option<WatchEvent>> {
        // SAFETY: `shared` is live while the watcher is.
        let shared = unsafe { w.shared.as_ref() };
        let deadline = Deadline::after(timeout);
        loop {
            let seq = shared.seq.load(Ordering::Acquire);
            let event = shared.with(|data| {
                if data.overflow {
                    data.overflow = false;
                    return Some(WatchEvent {
                        kind: WatchKind::Overflow,
                        path_len: 0,
                    });
                }
                if data.used == 0 {
                    return None;
                }
                let kind = KINDS[usize::from(data.pop_byte()).min(KINDS.len() - 1)];
                let len = usize::from(data.pop_byte()) | (usize::from(data.pop_byte()) << 8);
                for i in 0..len {
                    let b = data.pop_byte();
                    if let Some(slot) = path.get_mut(i) {
                        *slot = b;
                    }
                }
                Some(WatchEvent {
                    kind,
                    path_len: len,
                })
            });
            if let Some(event) = event {
                let len = event.path_len;
                if len <= path.len() && core::str::from_utf8(&path[..len]).is_err() {
                    return Err(Error::from_kind(ErrorKind::InvalidData));
                }
                return Ok(Some(event));
            }
            let remaining = deadline.remaining();
            if remaining == Some(Duration::ZERO) {
                return Ok(None);
            }
            // Pushes bump `seq` after unlocking, so a push since the load above wakes us at once.
            Backend::wait(&shared.seq, seq, remaining);
        }
    }
}
