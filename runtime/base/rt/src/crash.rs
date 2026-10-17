//! Where crash dumps go.
//!
//! The startup sequence installs `marlin_diag`'s crash handler (on platforms with the
//! `crash_dump` extension) with dumps in, by priority:
//!
//! 1. the directory named by the `MARLIN_CRASH_DIR` environment variable;
//! 2. a `crashes` directory next to the executable, created if missing.
//!
//! If neither can be used the handler is not installed and crashes end the process the OS's way.
//! The panic handler writes a dump to the same place before aborting.

use alloc::string::String;

/// Installs the crash handler with the default directory; does nothing without the extension.
pub(crate) fn install_default() {
    #[cfg(marlin_ext = "crash_dump")]
    if let Some(dir) = default_dir() {
        let _ = marlin_diag::ext::crash_dump::install(&dir);
    }
}

/// The directory crash dumps go to by default, created if needed; `None` if there is none.
pub fn default_dir() -> Option<String> {
    if let Some(dir) = crate::process::env_var("MARLIN_CRASH_DIR")
        && marlin_pal::fs::metadata(&dir).is_ok_and(|m| m.kind == marlin_pal::fs::FileKind::Dir)
    {
        return Some(dir);
    }
    let exe = crate::process::current_exe().ok()?;
    let parent = exe.rfind(['/', '\\'])?;
    let mut dir = String::from(&exe[..parent]);
    dir.push_str("/crashes");
    match marlin_pal::fs::create_dir(&dir) {
        Ok(()) => Some(dir),
        Err(e) if e.kind() == marlin_pal::ErrorKind::AlreadyExists => Some(dir),
        Err(_) => None,
    }
}
