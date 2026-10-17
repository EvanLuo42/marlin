//! Watching a directory tree for changes, for hot reload.
//!
//! A [`Watcher`] reports changes anywhere under its root, one [`WatchEvent`] at a time, with the
//! path relative to the root (`/` separators) written into a caller buffer. Events are hints:
//! the OS coalesces, reorders and drops them under load, so treat each as "look at this path
//! again", and an [`Overflow`](WatchKind::Overflow) as "rescan everything".
//!
//! | Platform | Mechanism | Notes |
//! |---|---|---|
//! | Windows | `ReadDirectoryChangesW` on the root, recursive | 64 KiB of events buffered between calls |
//! | Linux | inotify, one watch per directory | Directories created later are watched once their creation is read; files created in them before that are not reported, so rescan a new directory |
//! | macOS | FSEvents with per-file events | Delivered with about 50 ms of latency; a rename is reported as `RenamedFrom` or `RenamedTo` depending on whether the path still exists |

use core::time::Duration;

use crate::backend::imp::Backend;
use crate::contract::Watch as Contract;
use crate::file::MAX_PATH_LEN;
use crate::{Error, ErrorKind, Result};

/// What changed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WatchKind {
    Created,
    Removed,
    /// Contents or attributes changed.
    Modified,
    /// The old name of a renamed (or moved-out) entry.
    RenamedFrom,
    /// The new name of a renamed (or moved-in) entry.
    RenamedTo,
    /// Events were lost; the path is empty. Rescan the tree.
    Overflow,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WatchEvent {
    pub kind: WatchKind,
    /// Full length of the relative path in bytes; the buffer passed to [`Watcher::next`] holds it
    /// when this is at most its length.
    pub path_len: usize,
}

/// Watches a directory tree. See the [module docs](self). Dropping it stops watching.
pub struct Watcher(<Backend as Contract>::Watcher);

impl Watcher {
    /// Starts watching everything under the directory `root`.
    pub fn new(root: &str) -> Result<Self> {
        if root.is_empty() || root.len() > MAX_PATH_LEN || root.as_bytes().contains(&0) {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        Backend::watch_new(root).map(Self)
    }

    /// The next change, waiting up to `timeout` (`None` = forever, zero = don't wait). Writes its
    /// path relative to the root into `path`. `None` on timeout.
    #[allow(clippy::should_implement_trait)]
    pub fn next(
        &mut self,
        path: &mut [u8],
        timeout: Option<Duration>,
    ) -> Result<Option<WatchEvent>> {
        Backend::watch_next(&mut self.0, path, timeout)
    }
}
