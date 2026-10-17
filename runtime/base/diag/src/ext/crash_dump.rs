//! Crash dumps: a minidump and a logged report for crashes nothing else handles, and dumps on
//! demand (the panic handler writes one before aborting).
//!
//! Dumps go to `<dir>/crash-<UTC date>-<time>-<pid>.dmp`. The directory is set once by
//! [`install`]; until then [`write_now`] does nothing.

use core::cell::UnsafeCell;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use marlin_pal::ext::crash_dump::{CrashDump, CrashInfo};
use marlin_pal::file::MAX_PATH_LEN;

use crate::backtrace::Backtrace;
use crate::{FixedBuf, Level, Record};

struct Dir(UnsafeCell<FixedBuf<MAX_PATH_LEN>>);

// SAFETY: written once by `install` before `READY` is released, only read after it is acquired.
unsafe impl Sync for Dir {}

static DIR: Dir = Dir(UnsafeCell::new(FixedBuf::new()));
static READY: AtomicBool = AtomicBool::new(false);
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// Why [`install`] failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallError {
    /// This platform cannot write dumps.
    Unsupported,
    /// Already installed; the directory cannot change.
    AlreadyInstalled,
    /// The directory path is empty or too long for the file names built from it.
    InvalidDir,
}

impl core::fmt::Display for InstallError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "crash dumps are not supported here",
            Self::AlreadyInstalled => "the crash handler is already installed",
            Self::InvalidDir => "invalid crash dump directory",
        })
    }
}

impl core::error::Error for InstallError {}

/// Installs the crash handler, writing dumps to `dir` (an existing directory). On a crash it
/// writes a dump, logs the exception, its backtrace and the dump's path at `Error`, flushes
/// every sink, and lets the process end.
pub fn install(dir: &str) -> Result<(), InstallError> {
    let dump = CrashDump::get().ok_or(InstallError::Unsupported)?;
    // Room for "/crash-YYYYMMDD-HHMMSS-<pid>.dmp".
    if dir.is_empty() || dir.len() + 40 > MAX_PATH_LEN {
        return Err(InstallError::InvalidDir);
    }
    if INSTALLING.swap(true, Ordering::Acquire) {
        return Err(InstallError::AlreadyInstalled);
    }
    // SAFETY: only the first `install` gets here, and nothing reads `DIR` before `READY`.
    unsafe {
        let buf = &mut *DIR.0.get();
        let _ = buf.write_str(dir.trim_end_matches(['/', '\\']));
    }
    READY.store(true, Ordering::Release);
    dump.set_handler(Some(on_crash));
    Ok(())
}

/// Whether [`install`] succeeded.
pub fn is_installed() -> bool {
    READY.load(Ordering::Acquire)
}

/// Writes a dump of the process now, without a crash record, and returns its path. `None` if the
/// handler is not installed or the dump could not be written. Used by the panic handler.
pub fn write_now() -> Option<FixedBuf<MAX_PATH_LEN>> {
    let path = dump_path()?;
    CrashDump::get()?.write(path.as_str(), false).ok()?;
    Some(path)
}

fn dump_path() -> Option<FixedBuf<MAX_PATH_LEN>> {
    if !READY.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: `READY` was acquired, so `DIR` is fully written and never written again.
    let dir = unsafe { (*DIR.0.get()).as_str() };
    let (secs, _) = marlin_pal::time::system_time();
    let (year, month, day, hour, minute, second) = civil(secs);
    let mut path = FixedBuf::<MAX_PATH_LEN>::new();
    let _ = write!(
        path,
        "{dir}/crash-{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}-{}.dmp",
        marlin_pal::process::id()
    );
    Some(path)
}

fn on_crash(info: &CrashInfo) {
    let path = dump_path();
    let written = path
        .as_ref()
        .is_some_and(|path| info.write_dump(path.as_str(), false).is_ok());
    let backtrace = Backtrace::capture();
    let mut access = FixedBuf::<64>::new();
    if let Some((addr, write)) = info.access {
        let verb = if write { "writing" } else { "reading" };
        let _ = write!(access, " ({verb} {addr:#x})");
    }
    let dump = match (&path, written) {
        (Some(path), true) => path.as_str(),
        _ => "(no dump written)",
    };
    let thread_id = marlin_pal::thread::current_id();
    crate::dispatch(&Record::new(
        Level::Error,
        module_path!(),
        file!(),
        line!(),
        thread_id,
        format_args!(
            "unhandled exception {:#010x} at {:#x}{}; dump: {dump}\n{backtrace}",
            info.code,
            info.address,
            access.as_str()
        ),
    ));
    crate::flush();
}

/// Converts Unix seconds to a UTC (year, month, day, hour, minute, second), by Howard Hinnant's
/// `civil_from_days`.
fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, rem / 3600, rem / 60 % 60, rem % 60)
}
