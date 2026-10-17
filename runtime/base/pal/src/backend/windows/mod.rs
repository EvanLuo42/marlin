//! Win32 backend. The Xbox backend reuses most of it.

#[cfg(marlin_ext = "crash_dump")]
mod crash_dump;
mod debug;
#[cfg(marlin_ext = "dylib")]
mod dylib;
pub(crate) mod error;
mod fiber;
pub(super) mod file;
mod fs;
mod futex;
#[cfg(marlin_ext = "hybrid_cores")]
mod hybrid_cores;
#[cfg(marlin_ext = "large_pages")]
mod large_pages;
mod process;
mod sysinfo;
mod thread;
mod time;
mod tls;
mod vmem;
#[cfg(marlin_ext = "vmem_alias")]
mod vmem_alias;
mod watch;

use core::ptr;

use windows_sys::Win32::Storage::FileSystem::GetFullPathNameW;

use crate::file::MAX_PATH_LEN;
use crate::{Error, ErrorKind, Result};

pub(crate) struct Backend;

impl crate::contract::Platform for Backend {}

/// Encodes `s` as NUL-terminated UTF-16 on the stack for `f`. `InvalidInput` if `s` is longer than
/// `MAX_PATH_LEN` bytes or contains NUL.
fn with_wide<R>(s: &str, f: impl FnOnce(*const u16) -> Result<R>) -> Result<R> {
    if s.len() > MAX_PATH_LEN || s.as_bytes().contains(&0) {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    // UTF-16 never needs more units than UTF-8 needs bytes.
    let mut wide = [0u16; MAX_PATH_LEN + 1];
    let mut len = 0;
    for unit in s.encode_utf16() {
        wide[len] = unit;
        len += 1;
    }
    wide[len] = 0;
    f(wide.as_ptr())
}

/// Converts `path`, followed by `suffix`, to a NUL-terminated `\\?\` path for `f`, on the stack.
/// The caller has checked `path` (non-empty, at most `MAX_PATH_LEN` bytes, no NUL).
///
/// `GetFullPathNameW` does what the Win32 layer would: resolves relative paths, `.` and `..`, and
/// turns `/` into `\`. The prefix then lifts the 260-character limit, which `\\?\` paths are
/// exempt from because they skip that normalization.
fn with_wide_path<R>(
    path: &str,
    suffix: &str,
    f: impl FnOnce(*const u16) -> Result<R>,
) -> Result<R> {
    // UTF-16 never needs more units than UTF-8 needs bytes.
    let mut wide = [0u16; MAX_PATH_LEN + 1];
    let mut len = 0;
    for unit in path.encode_utf16() {
        wide[len] = unit;
        len += 1;
    }
    wide[len] = 0;

    // Room for the prefix in front, a cwd in front of relative paths, and the suffix.
    const PREFIX: usize = 8;
    const SUFFIX: usize = 8;
    let mut full = [0u16; PREFIX + 2 * MAX_PATH_LEN + SUFFIX];
    let room = (full.len() - PREFIX - SUFFIX) as u32;
    // SAFETY: `wide` is NUL-terminated; the output holds `room` units; a null file part is allowed.
    let n = unsafe {
        GetFullPathNameW(
            wide.as_ptr(),
            room,
            full[PREFIX..].as_mut_ptr(),
            ptr::null_mut(),
        )
    };
    if n == 0 {
        return Err(error::last_error());
    }
    if n >= room {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    let mut end = PREFIX + n as usize;
    // Verbatim paths are not normalized, so never double a separator (`C:\` + `\*`).
    let suffix = match suffix.strip_prefix('\\') {
        Some(rest) if full[end - 1] == u16::from(b'\\') => rest,
        _ => suffix,
    };
    if suffix.encode_utf16().count() >= SUFFIX {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    for unit in suffix.encode_utf16() {
        full[end] = unit;
        end += 1;
    }
    full[end] = 0;

    let bs = u16::from(b'\\');
    let resolved = &full[PREFIX..end];
    let start = if resolved.starts_with(&[bs, bs, u16::from(b'?'), bs])
        || resolved.starts_with(&[bs, bs, u16::from(b'.'), bs])
    {
        // Already a device or verbatim path.
        PREFIX
    } else if resolved.starts_with(&[bs, bs]) {
        // \\server\share -> \\?\UNC\server\share, overwriting the first backslash.
        for (dst, &src) in full[PREFIX - 6..=PREFIX].iter_mut().zip(br"\\?\UNC") {
            *dst = u16::from(src);
        }
        PREFIX - 6
    } else {
        for (dst, &src) in full[PREFIX - 4..PREFIX].iter_mut().zip(br"\\?\") {
            *dst = u16::from(src);
        }
        PREFIX - 4
    };
    f(full[start..].as_ptr())
}

/// Converts UTF-16 from the OS to UTF-8 in `out`, returning the full UTF-8 length; `out` holds the
/// whole string only when it is long enough. `InvalidData` for unpaired surrogates.
fn utf16_to_utf8(src: &[u16], out: &mut [u8]) -> Result<usize> {
    let mut writer = Utf8Writer::new(out);
    for &unit in src {
        writer.push(unit)?;
    }
    writer.finish()
}

/// Decodes UTF-16 one unit at a time into a UTF-8 buffer, counting what does not fit.
struct Utf8Writer<'a> {
    out: &'a mut [u8],
    len: usize,
    high: Option<u16>,
}

impl<'a> Utf8Writer<'a> {
    fn new(out: &'a mut [u8]) -> Self {
        Self {
            out,
            len: 0,
            high: None,
        }
    }

    fn push(&mut self, unit: u16) -> Result<()> {
        let c = match (self.high.take(), unit) {
            (None, 0xD800..=0xDBFF) => {
                self.high = Some(unit);
                return Ok(());
            }
            (Some(high), 0xDC00..=0xDFFF) => {
                let c = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                char::from_u32(c)
            }
            (None, _) => char::from_u32(u32::from(unit)),
            (Some(_), _) => None,
        };
        let c = c.ok_or(Error::from_kind(ErrorKind::InvalidData))?;
        let mut utf8 = [0u8; 4];
        let bytes = c.encode_utf8(&mut utf8).as_bytes();
        if let Some(dst) = self.out.get_mut(self.len..self.len + bytes.len()) {
            dst.copy_from_slice(bytes);
        }
        self.len += bytes.len();
        Ok(())
    }

    fn finish(self) -> Result<usize> {
        match self.high {
            Some(_) => Err(Error::from_kind(ErrorKind::InvalidData)),
            None => Ok(self.len),
        }
    }
}

/// Encodes `s` as UTF-16 in chunks of at most 255 code units, each followed by a NUL (included in
/// the slice passed to `f`). Chunks never split a surrogate pair.
fn for_each_wide_chunk(s: &str, mut f: impl FnMut(&[u16])) {
    let mut buf = [0u16; 256];
    let mut len = 0;
    for c in s.chars() {
        if len + 2 >= buf.len() {
            buf[len] = 0;
            f(&buf[..=len]);
            len = 0;
        }
        len += c.encode_utf16(&mut buf[len..]).len();
    }
    if len > 0 {
        buf[len] = 0;
        f(&buf[..=len]);
    }
}
