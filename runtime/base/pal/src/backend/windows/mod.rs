//! Win32 backend. The Xbox backend reuses most of it.

mod debug;
pub(crate) mod error;
mod fiber;
mod file;
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
