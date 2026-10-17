use core::ffi::c_void;

use crate::Result;
use crate::ext::crash_dump::CrashHandler;

pub(crate) trait CrashDump {
    /// Writes a minidump of this process to `path` (checked by the caller). `exception` is the
    /// platform's record of a crash being handled, or null.
    ///
    /// # Safety
    /// `exception` is null or came from the crash handler currently running on this thread.
    unsafe fn write_dump(path: &str, exception: *mut c_void, full_memory: bool) -> Result<()>;
    /// Replaces the handler called for unhandled crashes (access violations, illegal
    /// instructions, ...) before the process ends; `None` restores the OS default.
    fn set_crash_handler(handler: Option<CrashHandler>);
}
