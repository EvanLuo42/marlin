//! Allocation-free formatting target.

use core::fmt;

/// A fixed-capacity string for formatting on the stack. Output past the capacity is dropped (at a
/// char boundary) and the buffer remembers it was truncated.
///
/// `write_str` returns `Err` once full, which stops the remaining formatting work early; callers
/// that only care about the text can ignore it.
pub struct FixedBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
    truncated: bool,
}

impl<const N: usize> FixedBuf<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            truncated: false,
        }
    }

    pub fn as_str(&self) -> &str {
        // SAFETY: only whole `&str`s or prefixes cut at char boundaries are ever copied in.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.truncated = false;
    }

    /// Ends the contents with `'\n'`, replacing the tail with `"...\n"` if text was dropped or
    /// there is no room. Needs `N >= 4`.
    pub fn end_line(&mut self) {
        const MARK: &str = "...\n";
        if !self.truncated && self.len < N {
            self.buf[self.len] = b'\n';
            self.len += 1;
            return;
        }
        let mut cut = self.len.min(N.saturating_sub(MARK.len()));
        while !self.as_str().is_char_boundary(cut) {
            cut -= 1;
        }
        self.len = cut;
        let end = (cut + MARK.len()).min(N);
        self.buf[cut..end].copy_from_slice(&MARK.as_bytes()[..end - cut]);
        self.len = end;
        self.truncated = true;
    }
}

impl<const N: usize> Default for FixedBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for FixedBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // Keep dropping once truncated, or a later short write would follow a gap.
        if self.truncated {
            return Err(fmt::Error);
        }
        let room = N - self.len;
        if s.len() <= room {
            self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
            return Ok(());
        }
        let mut take = room;
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        self.truncated = true;
        Err(fmt::Error)
    }
}

impl<const N: usize> fmt::Debug for FixedBuf<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}
