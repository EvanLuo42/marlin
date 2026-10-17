use core::ptr;

use windows_sys::Win32::Foundation::{
    ERROR_ENVVAR_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, SetLastError,
};
use windows_sys::Win32::System::Environment::{
    GetCommandLineW, GetCurrentDirectoryW, GetEnvironmentVariableW, SetCurrentDirectoryW,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Threading::{ExitProcess, GetCurrentProcessId};

use super::error::{check, last_error};
use super::{Backend, Utf8Writer, utf16_to_utf8, with_wide};
use crate::contract::Process;
use crate::{Error, ErrorKind, Result};

/// `FAST_FAIL_FATAL_APP_EXIT`, the code std uses too.
const FAST_FAIL_FATAL_APP_EXIT: u32 = 7;

/// UTF-16 units read on the stack for paths and environment values. Paths longer than this come
/// back as `InvalidInput`; Windows itself caps environment values at 32767 units.
const WIDE_BUF: usize = 8192;

impl Process for Backend {
    fn abort() -> ! {
        // `__fastfail`: terminates immediately, reports to WER, cannot be intercepted.
        // SAFETY: never returns and touches no memory.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            core::arch::asm!("int $$0x29", in("ecx") FAST_FAIL_FATAL_APP_EXIT, options(noreturn, nostack));
            #[cfg(target_arch = "aarch64")]
            core::arch::asm!("brk 0xF003", in("x0") FAST_FAIL_FATAL_APP_EXIT, options(noreturn, nostack));
        }
    }

    fn exit(code: i32) -> ! {
        // SAFETY: no preconditions; never returns.
        unsafe { ExitProcess(code as u32) }
    }

    fn id() -> u32 {
        // SAFETY: no preconditions.
        unsafe { GetCurrentProcessId() }
    }

    fn arg_count() -> usize {
        let mut count = 0;
        split_command_line(command_line(), |_, unit| {
            if unit.is_none() {
                count += 1;
            }
        });
        count
    }

    fn arg(index: usize, out: &mut [u8]) -> Result<Option<usize>> {
        let mut writer = Utf8Writer::new(out);
        let mut found = false;
        let mut result = Ok(());
        split_command_line(command_line(), |i, unit| {
            if i != index || result.is_err() {
                return;
            }
            match unit {
                Some(unit) => result = writer.push(unit),
                None => found = true,
            }
        });
        result?;
        if found {
            writer.finish().map(Some)
        } else {
            Ok(None)
        }
    }

    fn current_exe(out: &mut [u8]) -> Result<usize> {
        let mut buf = [0u16; WIDE_BUF];
        // SAFETY: a null module means the executable; `buf` holds `WIDE_BUF` units.
        let n = unsafe { GetModuleFileNameW(ptr::null_mut(), buf.as_mut_ptr(), WIDE_BUF as u32) };
        if n == 0 {
            return Err(last_error());
        }
        // A full buffer means the path was truncated.
        if n as usize >= WIDE_BUF {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        utf16_to_utf8(strip_verbatim(&buf[..n as usize]), out)
    }

    fn current_dir(out: &mut [u8]) -> Result<usize> {
        let mut buf = [0u16; WIDE_BUF];
        // SAFETY: `buf` holds `WIDE_BUF` units.
        let n = unsafe { GetCurrentDirectoryW(WIDE_BUF as u32, buf.as_mut_ptr()) };
        if n == 0 {
            return Err(last_error());
        }
        // Too small: `n` is the size needed.
        if n as usize >= WIDE_BUF {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        utf16_to_utf8(strip_verbatim(&buf[..n as usize]), out)
    }

    fn set_current_dir(path: &str) -> Result<()> {
        // SAFETY: `path` is NUL-terminated.
        with_wide(path, |path| check(unsafe { SetCurrentDirectoryW(path) }))
    }

    fn env_var(name: &str, out: &mut [u8]) -> Result<Option<usize>> {
        let mut buf = [0u16; WIDE_BUF];
        let n = with_wide(name, |name| {
            // An empty value also returns 0 and leaves the last error alone, so clear it first.
            // SAFETY: `name` is NUL-terminated; `buf` holds `WIDE_BUF` units.
            Ok(unsafe {
                SetLastError(0);
                GetEnvironmentVariableW(name, buf.as_mut_ptr(), WIDE_BUF as u32)
            })
        })?;
        if n == 0 {
            let error = last_error();
            return match error.raw_os_error() as u32 {
                0 => Ok(Some(0)),
                ERROR_ENVVAR_NOT_FOUND => Ok(None),
                _ => Err(error),
            };
        }
        // Too small: `n` is the size needed, including the NUL.
        if n as usize >= WIDE_BUF {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                ERROR_INSUFFICIENT_BUFFER as i32,
            ));
        }
        utf16_to_utf8(&buf[..n as usize], out).map(Some)
    }
}

/// The process command line, without its NUL.
fn command_line() -> &'static [u16] {
    // SAFETY: GetCommandLineW returns a NUL-terminated string that lives as long as the process.
    unsafe {
        let p = GetCommandLineW();
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        core::slice::from_raw_parts(p, len)
    }
}

/// Removes a `\\?\` prefix (but not `\\?\UNC\`, which only makes sense with it).
fn strip_verbatim(path: &[u16]) -> &[u16] {
    let prefix = br"\\?\".map(u16::from);
    match path.strip_prefix(&prefix[..]) {
        Some(rest) if !rest.starts_with(&b"UNC".map(u16::from)) => rest,
        _ => path,
    }
}

/// Splits a command line by the rules of the MSVC runtime (2008 and later), as Rust std does.
/// Calls `f(index, Some(unit))` for each code unit of argument `index` and `f(index, None)` at
/// its end.
fn split_command_line(cmd: &[u16], mut f: impl FnMut(usize, Option<u16>)) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let is_blank = |w: u16| w == u16::from(b' ') || w == u16::from(b'\t');

    let mut i = 0;
    // The program name: quotes toggle, nothing escapes, unquoted blanks end it.
    let mut in_quotes = false;
    while i < cmd.len() {
        let w = cmd[i];
        i += 1;
        match w {
            QUOTE => in_quotes = !in_quotes,
            w if is_blank(w) && !in_quotes => break,
            w => f(0, Some(w)),
        }
    }
    f(0, None);
    while i < cmd.len() && is_blank(cmd[i]) {
        i += 1;
    }

    let mut index = 1;
    // An argument is pending once anything (even an empty pair of quotes) was seen.
    let mut pending = false;
    in_quotes = false;
    while i < cmd.len() {
        let w = cmd[i];
        i += 1;
        match w {
            w if is_blank(w) && !in_quotes => {
                f(index, None);
                index += 1;
                pending = false;
                while i < cmd.len() && is_blank(cmd[i]) {
                    i += 1;
                }
                continue;
            }
            // Backslashes escape only when a quote follows the run.
            BACKSLASH => {
                let mut count = 1;
                while i < cmd.len() && cmd[i] == BACKSLASH {
                    count += 1;
                    i += 1;
                }
                if cmd.get(i) == Some(&QUOTE) {
                    for _ in 0..count / 2 {
                        f(index, Some(BACKSLASH));
                    }
                    if count % 2 == 1 {
                        f(index, Some(QUOTE));
                        i += 1;
                    }
                } else {
                    for _ in 0..count {
                        f(index, Some(BACKSLASH));
                    }
                }
            }
            // In quotes, `""` is a literal quote; a lone quote ends the quoted part.
            QUOTE if in_quotes => {
                if cmd.get(i) == Some(&QUOTE) {
                    f(index, Some(QUOTE));
                    i += 1;
                } else {
                    in_quotes = false;
                }
            }
            QUOTE => in_quotes = true,
            w => f(index, Some(w)),
        }
        pending = true;
    }
    if pending || in_quotes {
        f(index, None);
    }
}
