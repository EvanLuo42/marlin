//! Debug output and debugger interaction.

use core::fmt;

use crate::backend::imp::Backend;
use crate::contract::Debug;

/// Writes `s` to stderr and, when a debugger is attached, to the debugger.
///
/// Never allocates or takes locks, so it is safe to call while panicking.
#[inline]
pub fn output(s: &str) {
    Backend::output(s)
}

#[inline]
pub fn is_debugger_present() -> bool {
    Backend::is_debugger_present()
}

/// Raises a breakpoint trap. Without a debugger this usually terminates the process.
#[inline]
pub fn break_point() {
    Backend::break_point()
}

/// Formats into a fixed buffer and hands it to [`output`] when full, on [`flush`](Self::flush) or
/// on drop, so a formatted line costs one call instead of one per fragment. Never allocates.
pub struct OutputBuffer {
    buf: [u8; 512],
    len: usize,
}

impl OutputBuffer {
    pub const fn new() -> Self {
        Self {
            buf: [0; 512],
            len: 0,
        }
    }

    pub fn flush(&mut self) {
        if self.len > 0 {
            // SAFETY: the buffer only ever holds whole `&str`s, so it is valid UTF-8.
            output(unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) });
            self.len = 0;
        }
    }
}

impl Default for OutputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Write for OutputBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.len + s.len() > self.buf.len() {
            self.flush();
        }
        if s.len() > self.buf.len() {
            output(s);
        } else {
            self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
        }
        Ok(())
    }
}

impl Drop for OutputBuffer {
    fn drop(&mut self) {
        self.flush();
    }
}
