use core::time::Duration;

use crate::Result;
use crate::watch::WatchEvent;

pub(crate) trait Watch {
    /// Watches a directory tree; dropping it stops watching.
    type Watcher: Send;

    /// `root` is non-empty, at most `MAX_PATH_LEN` bytes and has no NUL (checked by the caller).
    fn watch_new(root: &str) -> Result<Self::Watcher>;
    /// The next change, waiting up to `timeout` (`None` = forever), its path relative to the root
    /// with `/` separators written like `crate::process` strings. `None` on timeout.
    fn watch_next(
        watcher: &mut Self::Watcher,
        path: &mut [u8],
        timeout: Option<Duration>,
    ) -> Result<Option<WatchEvent>>;
}
