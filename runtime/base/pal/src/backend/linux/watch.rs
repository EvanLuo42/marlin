//! Directory watching with inotify, which watches single directories: the watcher adds one watch
//! per directory of the tree and remembers each one's parent and name to rebuild relative paths.
//!
//! That bookkeeping lives in one reservation committed up front (Linux commits physical pages on
//! first touch, so this costs only what is used): a node table, a watch-descriptor index and an
//! append-only name arena.

use core::ffi::{CStr, c_char};
use core::mem;
use core::ptr::NonNull;
use core::time::Duration;

use crate::backend::unix::{Backend, errno, last_error};
use crate::contract::{VMem, Watch};
use crate::file::{Deadline, MAX_PATH_LEN};
use crate::watch::{WatchEvent, WatchKind};
use crate::{Error, ErrorKind, Protection, Result};

const NODES: usize = 1 << 20;
const WDS: usize = 1 << 22;
const NAMES: usize = 32 << 20;
const RESERVE: usize = NODES * size_of::<Node>() + WDS * 4 + NAMES;
const NONE: u32 = u32::MAX;
/// Longest path built: the root plus a relative path.
const PATH_CAP: usize = MAX_PATH_LEN * 4;

const MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MODIFY
    | libc::IN_ATTRIB
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_ONLYDIR
    | libc::IN_EXCL_UNLINK;

#[derive(Clone, Copy)]
#[repr(C)]
struct Node {
    parent: u32,
    name_off: u32,
    name_len: u32,
    alive: u32,
}

pub(crate) struct Watcher {
    fd: libc::c_int,
    base: NonNull<u8>,
    nodes: usize,
    names: usize,
    /// The root's path as given, at the start of the name arena.
    root_len: usize,
    /// Events read from the descriptor and the offset of the next one.
    buf: [u32; 1024],
    filled: usize,
    next: usize,
}

// SAFETY: the descriptor and the reservation are owned by the watcher.
unsafe impl Send for Watcher {}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: we own the descriptor (closing it removes every watch) and the reservation.
        unsafe {
            libc::close(self.fd);
            let _ = Backend::release(self.base, RESERVE);
        }
    }
}

impl Watcher {
    fn node(&self, index: u32) -> Node {
        // SAFETY: `index < self.nodes`, inside the committed node table.
        unsafe { *self.base.as_ptr().cast::<Node>().add(index as usize) }
    }

    fn set_node(&mut self, index: u32, node: Node) {
        // SAFETY: `index < NODES`, inside the committed node table.
        unsafe { *self.base.as_ptr().cast::<Node>().add(index as usize) = node };
    }

    fn wd_slot(&self, wd: i32) -> Option<*mut u32> {
        let wd = usize::try_from(wd).ok().filter(|&wd| wd < WDS)?;
        // SAFETY: `wd < WDS`, inside the committed descriptor index after the node table.
        Some(unsafe {
            self.base
                .as_ptr()
                .add(NODES * size_of::<Node>())
                .cast::<u32>()
                .add(wd)
        })
    }

    fn name(&self, off: u32, len: u32) -> &[u8] {
        // SAFETY: the range was written by `push_name`, inside the committed arena.
        unsafe {
            core::slice::from_raw_parts(
                self.base
                    .as_ptr()
                    .add(NODES * size_of::<Node>() + WDS * 4 + off as usize),
                len as usize,
            )
        }
    }

    fn push_name(&mut self, name: &[u8]) -> Option<(u32, u32)> {
        if self.names + name.len() > NAMES {
            return None;
        }
        let off = self.names;
        // SAFETY: the range lies inside the committed arena.
        unsafe {
            self.base
                .as_ptr()
                .add(NODES * size_of::<Node>() + WDS * 4 + off)
                .copy_from_nonoverlapping(name.as_ptr(), name.len());
        }
        self.names += name.len();
        Some((off as u32, name.len() as u32))
    }

    /// Watches the directory at `path[..len]` (NUL at `path[len]`) and, recursively, the
    /// directories below it. Fails only for the directory itself; unreadable children are
    /// skipped.
    fn add_tree(
        &mut self,
        path: &mut [u8; PATH_CAP],
        len: usize,
        parent: u32,
        name: &[u8],
    ) -> Result<()> {
        path[len] = 0;
        // SAFETY: `path` is NUL-terminated at `len`.
        let wd = unsafe { libc::inotify_add_watch(self.fd, path.as_ptr().cast(), MASK) };
        if wd < 0 {
            return Err(last_error());
        }
        let Some(slot) = self.wd_slot(wd) else {
            return Err(Error::from_kind(ErrorKind::OutOfMemory));
        };
        // SAFETY: the slot lies in the committed index.
        if unsafe { *slot } != 0 {
            // Already watched (the kernel hands out one descriptor per inode).
            return Ok(());
        }
        if self.nodes >= NODES {
            return Err(Error::from_kind(ErrorKind::OutOfMemory));
        }
        let (name_off, name_len) = self
            .push_name(name)
            .ok_or(Error::from_kind(ErrorKind::OutOfMemory))?;
        let index = self.nodes as u32;
        self.set_node(
            index,
            Node {
                parent,
                name_off,
                name_len,
                alive: 1,
            },
        );
        self.nodes += 1;
        // SAFETY: as above.
        unsafe { *slot = index + 1 };

        // SAFETY: `path` is NUL-terminated.
        let Some(dir) = NonNull::new(unsafe { libc::opendir(path.as_ptr().cast()) }) else {
            return Ok(());
        };
        loop {
            // SAFETY: the stream is live.
            let entry = unsafe { libc::readdir(dir.as_ptr()) };
            if entry.is_null() {
                break;
            }
            // SAFETY: readdir returned a valid entry, live until the next call on this stream.
            let (d_type, child) = unsafe {
                (
                    (*entry).d_type,
                    CStr::from_ptr((&raw const (*entry).d_name).cast::<c_char>()),
                )
            };
            let child = child.to_bytes();
            if child == b"."
                || child == b".."
                || len + 1 + child.len() >= PATH_CAP
                || core::str::from_utf8(child).is_err()
            {
                continue;
            }
            let is_dir = match d_type {
                libc::DT_DIR => true,
                libc::DT_UNKNOWN => {
                    // SAFETY: plain data that fstatat fills in.
                    let mut st: libc::stat = unsafe { mem::zeroed() };
                    // SAFETY: the stream's descriptor is live; the name is NUL-terminated.
                    let r = unsafe {
                        libc::fstatat(
                            libc::dirfd(dir.as_ptr()),
                            (&raw const (*entry).d_name).cast(),
                            &mut st,
                            libc::AT_SYMLINK_NOFOLLOW,
                        )
                    };
                    r == 0 && st.st_mode & libc::S_IFMT == libc::S_IFDIR
                }
                _ => false,
            };
            if !is_dir {
                continue;
            }
            // Copy the name before the next readdir invalidates it.
            let mut name = [0u8; 256];
            let n = child.len().min(name.len());
            name[..n].copy_from_slice(&child[..n]);
            path[len] = b'/';
            path[len + 1..len + 1 + n].copy_from_slice(&name[..n]);
            let _ = self.add_tree(path, len + 1 + n, index, &name[..n]);
        }
        // SAFETY: we own the stream.
        unsafe { libc::closedir(dir.as_ptr()) };
        path[len] = 0;
        Ok(())
    }

    /// Writes the path of `node` relative to the root, plus `/name` if `name` is not empty, into
    /// `out` and returns its full length.
    fn relative_path(&self, node: u32, name: &[u8], out: &mut [u8]) -> usize {
        // Measure, then fill from the end.
        let mut len = name.len();
        let mut cur = node;
        while cur != 0 && cur != NONE {
            let n = self.node(cur);
            len += n.name_len as usize + 1;
            cur = n.parent;
        }
        if name.is_empty() {
            len = len.saturating_sub(1);
        }
        let mut end = len;
        let mut put = |bytes: &[u8], end: &mut usize| {
            let start = *end - bytes.len();
            if let Some(dst) = out.get_mut(start..*end) {
                dst.copy_from_slice(bytes);
            } else {
                for (i, &b) in bytes.iter().enumerate() {
                    if let Some(slot) = out.get_mut(start + i) {
                        *slot = b;
                    }
                }
            }
            *end = start;
        };
        if !name.is_empty() {
            put(name, &mut end);
        }
        let mut cur = node;
        let mut first = name.is_empty();
        while cur != 0 && cur != NONE {
            let n = self.node(cur);
            if !first {
                put(b"/", &mut end);
            }
            first = false;
            put(self.name(n.name_off, n.name_len), &mut end);
            cur = n.parent;
        }
        len
    }
}

impl Watch for Backend {
    type Watcher = Watcher;

    fn watch_new(root: &str) -> Result<Watcher> {
        let base = Backend::reserve(RESERVE)?;
        // SAFETY: commits the reservation just made; pages cost nothing until touched.
        if let Err(error) = unsafe { Backend::commit(base, RESERVE, Protection::ReadWrite) } {
            // SAFETY: releases the reservation just made.
            let _ = unsafe { Backend::release(base, RESERVE) };
            return Err(error);
        }
        // SAFETY: no preconditions.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            let error = last_error();
            // SAFETY: as above.
            let _ = unsafe { Backend::release(base, RESERVE) };
            return Err(error);
        }
        let mut watcher = Watcher {
            fd,
            base,
            nodes: 0,
            names: 0,
            root_len: 0,
            buf: [0; 1024],
            filled: 0,
            next: 0,
        };
        let root = root
            .strip_suffix('/')
            .filter(|r| !r.is_empty())
            .unwrap_or(root);
        watcher.push_name(root.as_bytes());
        watcher.root_len = root.len();
        let mut path = [0u8; PATH_CAP];
        path[..root.len()].copy_from_slice(root.as_bytes());
        // The root is node 0, with an empty name.
        watcher.add_tree(&mut path, root.len(), NONE, b"")?;
        Ok(watcher)
    }

    fn watch_next(
        w: &mut Watcher,
        path: &mut [u8],
        timeout: Option<Duration>,
    ) -> Result<Option<WatchEvent>> {
        let deadline = Deadline::after(timeout);
        loop {
            if w.next < w.filled {
                let mut name_buf = [0u8; 256];
                // SAFETY: `next` is the offset of a complete inotify_event the kernel wrote; the
                // buffer is 4-byte aligned like the struct, and its name follows inside `len`.
                let (wd, mask, name_len) = unsafe {
                    let p = w.buf.as_ptr().cast::<u8>().add(w.next);
                    let event = p.cast::<libc::inotify_event>().read();
                    let name = core::slice::from_raw_parts(
                        p.add(size_of::<libc::inotify_event>()),
                        event.len as usize,
                    );
                    w.next += size_of::<libc::inotify_event>() + event.len as usize;
                    // NUL-padded; names are at most 255 bytes (NAME_MAX).
                    let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                    let len = len.min(name_buf.len());
                    name_buf[..len].copy_from_slice(&name[..len]);
                    (event.wd, event.mask, len)
                };
                let name = &name_buf[..name_len];
                if core::str::from_utf8(name).is_err() {
                    return Err(Error::from_kind(ErrorKind::InvalidData));
                }

                if mask & libc::IN_Q_OVERFLOW != 0 {
                    return Ok(Some(WatchEvent {
                        kind: WatchKind::Overflow,
                        path_len: 0,
                    }));
                }
                let Some(slot) = w.wd_slot(wd) else {
                    continue;
                };
                // SAFETY: the slot lies in the committed index.
                let node = unsafe { *slot };
                if node == 0 {
                    continue;
                }
                let node = node - 1;
                if mask & libc::IN_IGNORED != 0 {
                    // The directory went away; its parent reports the removal.
                    // SAFETY: as above.
                    unsafe { *slot = 0 };
                    let mut n = w.node(node);
                    n.alive = 0;
                    w.set_node(node, n);
                    continue;
                }
                let kind = if mask & libc::IN_CREATE != 0 {
                    WatchKind::Created
                } else if mask & libc::IN_DELETE != 0 {
                    WatchKind::Removed
                } else if mask & libc::IN_MOVED_FROM != 0 {
                    WatchKind::RenamedFrom
                } else if mask & libc::IN_MOVED_TO != 0 {
                    WatchKind::RenamedTo
                } else if mask & (libc::IN_MODIFY | libc::IN_ATTRIB) != 0 {
                    WatchKind::Modified
                } else {
                    continue;
                };

                // A new directory: watch it (and anything already inside it).
                if mask & libc::IN_ISDIR != 0 && mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                    let mut abs = [0u8; PATH_CAP];
                    let root = w.name(0, w.root_len as u32);
                    let root_len = root.len();
                    abs[..root_len].copy_from_slice(root);
                    abs[root_len] = b'/';
                    let rel = w.relative_path(node, name, &mut abs[root_len + 1..]);
                    if root_len + 1 + rel < PATH_CAP {
                        let mut child = [0u8; 256];
                        let n = name.len().min(child.len());
                        child[..n].copy_from_slice(&name[..n]);
                        let _ = w.add_tree(&mut abs, root_len + 1 + rel, node, &child[..n]);
                    }
                }

                let path_len = w.relative_path(node, name, path);
                return Ok(Some(WatchEvent { kind, path_len }));
            }

            let ms = match deadline.remaining() {
                None => -1,
                Some(t) => t.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32,
            };
            let mut pfd = libc::pollfd {
                fd: w.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is one valid pollfd.
            let r = unsafe { libc::poll(&mut pfd, 1, ms) };
            if r < 0 {
                if errno() == libc::EINTR {
                    continue;
                }
                return Err(last_error());
            }
            if r == 0 {
                return Ok(None);
            }
            // SAFETY: `buf` is valid for its size in bytes.
            let n = unsafe { libc::read(w.fd, w.buf.as_mut_ptr().cast(), size_of_val(&w.buf)) };
            if n < 0 {
                match errno() {
                    libc::EAGAIN | libc::EINTR => continue,
                    _ => return Err(last_error()),
                }
            }
            w.filled = n as usize;
            w.next = 0;
        }
    }
}
