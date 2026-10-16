//! Formatting without allocation.
//!
//! ```
//! use marlin_text::{InlineString, format_inline, format_to};
//!
//! // Into an inline string; fails if the result does not fit.
//! let s: InlineString<32> = format_inline!("frame {} took {:.1} ms", 120, 16.67).unwrap();
//! assert_eq!(s, "frame 120 took 16.7 ms");
//!
//! // Into a caller's buffer; on overflow, the truncated text comes back as `Err`.
//! let mut buf = [0u8; 8];
//! assert_eq!(format_to!(&mut buf, "{}-{}", 1, 2), Ok("1-2"));
//! assert_eq!(format_to!(&mut buf, "{}", "too long to fit"), Err("too long"));
//! ```

use core::{fmt, str};

use crate::utf8::floor_char_boundary;

/// A [`fmt::Write`] sink over a byte buffer. When the buffer fills up it keeps what fits (cut at
/// a char boundary), remembers that it truncated, and fails the write so formatting stops early.
pub struct SliceWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
    truncated: bool,
}

impl<'a> SliceWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            truncated: false,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether some output did not fit.
    #[inline]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        // SAFETY: only whole `str`s, or prefixes of them cut at char boundaries, were copied in.
        unsafe { str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    /// The written text, borrowing the buffer for its full lifetime.
    pub fn into_str(self) -> &'a str {
        // SAFETY: as in `as_str`.
        unsafe { str::from_utf8_unchecked(&self.buf[..self.len]) }
    }
}

impl fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        let room = self.buf.len() - self.len;
        let n = if s.len() <= room {
            s.len()
        } else {
            self.truncated = true;
            floor_char_boundary(s, room)
        };
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        if self.truncated {
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}

/// Formats `args` into `buf`. Returns the text, as `Err` if it had to be truncated.
pub fn format_to<'a>(buf: &'a mut [u8], args: fmt::Arguments<'_>) -> Result<&'a str, &'a str> {
    let mut w = SliceWriter::new(buf);
    // A formatting error without truncation can only come from a broken `Display` impl; the
    // partial output is returned as if it were complete.
    let _ = fmt::Write::write_fmt(&mut w, args);
    if w.is_truncated() {
        Err(w.into_str())
    } else {
        Ok(w.into_str())
    }
}

/// `format!` into an [`InlineString`](crate::InlineString) whose capacity is inferred:
/// `Result<InlineString<N>, CapacityError>`.
#[macro_export]
macro_rules! format_inline {
    ($($arg:tt)*) => {
        $crate::InlineString::from_fmt(::core::format_args!($($arg)*))
    };
}

/// `format!` into a `&mut [u8]`: `Ok(&str)`, or `Err(&str)` holding the truncated text.
#[macro_export]
macro_rules! format_to {
    ($buf:expr, $($arg:tt)*) => {
        $crate::fmt::format_to($buf, ::core::format_args!($($arg)*))
    };
}
