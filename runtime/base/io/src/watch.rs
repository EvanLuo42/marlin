//! Watching a directory tree for changes, for hot reload. Events are hints: treat each as "look
//! at this path again" and [`WatchKind::Overflow`] as "rescan everything". See
//! `marlin_pal::watch` for what each platform reports.

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use marlin_pal::watch as pal;
pub use marlin_pal::watch::WatchKind;

use crate::{Path, PathBuf, Result};

/// One change under a [`Watcher`]'s root.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WatchEvent {
    pub kind: WatchKind,
    /// Relative to the root, normalized; empty for [`WatchKind::Overflow`].
    pub path: PathBuf,
}

/// Watches everything under a directory. Dropping it stops watching.
pub struct Watcher {
    raw: pal::Watcher,
    buf: Vec<u8>,
}

impl Watcher {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            raw: pal::Watcher::new(root.as_ref().as_str())?,
            buf: alloc::vec![0; 4096],
        })
    }

    /// The next change if one is ready, without waiting.
    pub fn poll(&mut self) -> Result<Option<WatchEvent>> {
        self.next(Some(Duration::ZERO))
    }

    /// Waits up to `timeout` (`None` = forever) for the next change. `None` on timeout.
    pub fn wait(&mut self, timeout: Option<Duration>) -> Result<Option<WatchEvent>> {
        self.next(timeout)
    }

    fn next(&mut self, timeout: Option<Duration>) -> Result<Option<WatchEvent>> {
        let Some(event) = self.raw.next(&mut self.buf, timeout)? else {
            return Ok(None);
        };
        // A path longer than the buffer is cut; 4 KiB covers any path the PAL accepts.
        let len = event.path_len.min(self.buf.len());
        let path = String::from_utf8_lossy(&self.buf[..len]);
        Ok(Some(WatchEvent {
            kind: event.kind,
            path: PathBuf::from(&*path),
        }))
    }
}
