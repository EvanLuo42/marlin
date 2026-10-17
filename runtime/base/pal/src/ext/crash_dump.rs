//! Minidumps of the running process, and a hook for crashes nothing else handles.
//!
//! Windows only for now: `MiniDumpWriteDump` from dbghelp, written by a helper thread so the
//! crashing thread's stack appears as it was, and `SetUnhandledExceptionFilter` for the hook.
//! Dumps open in Visual Studio and WinDbg with the build's PDBs. `__fastfail` (what
//! [`process::abort`](crate::process::abort) uses) bypasses every handler, so code that aborts on
//! purpose writes its dump first.

use core::ffi::c_void;

use crate::backend::imp::Backend;
use crate::contract::ext::CrashDump as Contract;
use crate::file::MAX_PATH_LEN;
use crate::{Error, ErrorKind, Result};

/// Called on the crashing thread for an unhandled crash; when it returns, the OS ends the
/// process (reporting it to WER on Windows). Runs in a damaged process: keep it short, do not
/// allocate if it can be avoided, and expect little stack after a stack overflow.
pub type CrashHandler = fn(&CrashInfo);

/// An unhandled crash, as seen by a [`CrashHandler`].
pub struct CrashInfo {
    /// The exception code (`EXCEPTION_ACCESS_VIOLATION` = `0xC0000005`, ...).
    pub code: u32,
    /// Where it happened.
    pub address: usize,
    /// For access violations: the address accessed, and whether it was a write.
    pub access: Option<(usize, bool)>,
    pub(crate) raw: *mut c_void,
}

impl CrashInfo {
    /// Writes a dump of the process with the crash recorded in it, so debuggers open at the
    /// faulting instruction.
    pub fn write_dump(&self, path: &str, full_memory: bool) -> Result<()> {
        check_path(path)?;
        // SAFETY: `raw` came from the handler running now, which is the only place a
        // `CrashInfo` exists.
        unsafe { Backend::write_dump(path, self.raw, full_memory) }
    }
}

fn check_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > MAX_PATH_LEN || path.as_bytes().contains(&0) {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    Ok(())
}

/// Proof that this platform writes crash dumps. Obtain one with [`CrashDump::get`].
#[derive(Clone, Copy, Debug)]
pub struct CrashDump(());

impl CrashDump {
    #[inline]
    pub fn get() -> Option<Self> {
        Some(Self(()))
    }

    /// Writes a dump of the process as it is now, threads and their stacks included, to `path`
    /// (created or truncated). `full_memory` includes all memory: large, but it shows heap state.
    pub fn write(self, path: &str, full_memory: bool) -> Result<()> {
        check_path(path)?;
        // SAFETY: no crash record.
        unsafe { Backend::write_dump(path, core::ptr::null_mut(), full_memory) }
    }

    /// Installs `handler` for unhandled crashes, replacing any previous one; `None` uninstalls.
    /// Not called while a debugger is attached (the debugger sees the crash first).
    pub fn set_handler(self, handler: Option<CrashHandler>) {
        Backend::set_crash_handler(handler)
    }
}
