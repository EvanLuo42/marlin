//! `InlineString<N>`: a fixed-capacity UTF-8 string stored inline.

use core::borrow::Borrow;
use core::hash::{Hash, Hasher};
use core::ops::{Deref, DerefMut};
use core::str::FromStr;
use core::{cmp, fmt, str};

use marlin_containers::CapacityError;

use crate::utf8::floor_char_boundary;

/// A UTF-8 string of at most `N` bytes (`N <= 65535`), stored inline and `Copy`. Never allocates:
/// the `try_` methods report overflow, the others panic.
///
/// Implements [`fmt::Write`], so it is also a stack formatting buffer; see
/// [`format_inline!`](crate::format_inline).
#[derive(Clone, Copy)]
pub struct InlineString<const N: usize> {
    len: u16,
    bytes: [u8; N],
}

impl<const N: usize> InlineString<N> {
    pub const fn new() -> Self {
        const {
            assert!(
                N <= u16::MAX as usize,
                "InlineString capacity must fit in u16"
            )
        };
        Self {
            len: 0,
            bytes: [0; N],
        }
    }

    /// Copies `s`, or fails if it is longer than `N` bytes. Usable in `const`.
    pub const fn try_from_str(s: &str) -> Result<Self, CapacityError> {
        let mut out = Self::new();
        if s.len() > N {
            return Err(CapacityError(()));
        }
        let (dst, _) = out.bytes.split_at_mut(s.len());
        dst.copy_from_slice(s.as_bytes());
        out.len = s.len() as u16;
        Ok(out)
    }

    /// Formats `args`, or fails if the result does not fit.
    pub fn from_fmt(args: fmt::Arguments<'_>) -> Result<Self, CapacityError> {
        let mut out = Self::new();
        fmt::Write::write_fmt(&mut out, args).map_err(|_| CapacityError(()))?;
        Ok(out)
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    #[inline]
    pub const fn remaining_capacity(&self) -> usize {
        N - self.len as usize
    }

    #[inline]
    pub const fn as_bytes(&self) -> &[u8] {
        self.bytes.split_at(self.len as usize).0
    }

    #[inline]
    pub const fn as_str(&self) -> &str {
        // SAFETY: only whole UTF-8 strings and chars are ever appended, and truncation only
        // happens at char boundaries.
        unsafe { str::from_utf8_unchecked(self.as_bytes()) }
    }

    #[inline]
    pub fn as_mut_str(&mut self) -> &mut str {
        let len = self.len as usize;
        // SAFETY: as in `as_str`; `&mut str` cannot break UTF-8 validity in safe code.
        unsafe { str::from_utf8_unchecked_mut(&mut self.bytes[..len]) }
    }

    /// Appends `s` if it fits entirely; otherwise leaves the string unchanged.
    pub fn try_push_str(&mut self, s: &str) -> Result<(), CapacityError> {
        if s.len() > self.remaining_capacity() {
            return Err(CapacityError(()));
        }
        let len = self.len as usize;
        self.bytes[len..len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len() as u16;
        Ok(())
    }

    /// Appends `s`. Panics if it does not fit.
    #[track_caller]
    pub fn push_str(&mut self, s: &str) {
        if self.try_push_str(s).is_err() {
            capacity_overflow::<N>();
        }
    }

    /// Appends as much of `s` as fits, cut at a char boundary. Returns whether all of it fit.
    pub fn push_str_truncating(&mut self, s: &str) -> bool {
        let n = floor_char_boundary(s, self.remaining_capacity());
        // Cannot fail: n <= remaining capacity.
        let _ = self.try_push_str(&s[..n]);
        n == s.len()
    }

    /// Appends `c` if it fits, else hands it back.
    pub fn try_push(&mut self, c: char) -> Result<(), CapacityError<char>> {
        let mut buf = [0; 4];
        self.try_push_str(c.encode_utf8(&mut buf))
            .map_err(|_| CapacityError(c))
    }

    /// Appends `c`. Panics if it does not fit.
    #[track_caller]
    pub fn push(&mut self, c: char) {
        if self.try_push(c).is_err() {
            capacity_overflow::<N>();
        }
    }

    pub fn pop(&mut self) -> Option<char> {
        let c = self.as_str().chars().next_back()?;
        self.len -= c.len_utf8() as u16;
        Some(c)
    }

    /// Shortens to `new_len` bytes. Panics if that is not a char boundary.
    pub fn truncate(&mut self, new_len: usize) {
        if new_len < self.len() {
            assert!(
                self.as_str().is_char_boundary(new_len),
                "truncate: {new_len} is not a char boundary"
            );
            self.len = new_len as u16;
        }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

#[cold]
#[track_caller]
fn capacity_overflow<const N: usize>() -> ! {
    panic!("InlineString capacity {N} exceeded");
}

impl<const N: usize> fmt::Write for InlineString<N> {
    /// Fails, appending nothing of `s`, if it does not fit.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.try_push_str(s).map_err(|_| fmt::Error)
    }

    fn write_char(&mut self, c: char) -> fmt::Result {
        self.try_push(c).map_err(|_| fmt::Error)
    }
}

impl<const N: usize> Default for InlineString<N> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Deref for InlineString<N> {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl<const N: usize> DerefMut for InlineString<N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut str {
        self.as_mut_str()
    }
}

impl<const N: usize> AsRef<str> for InlineString<N> {
    fn as_ref(&self) -> &str {
        self
    }
}

impl<const N: usize> AsRef<[u8]> for InlineString<N> {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<const N: usize> Borrow<str> for InlineString<N> {
    fn borrow(&self) -> &str {
        self
    }
}

impl<const N: usize> TryFrom<&str> for InlineString<N> {
    type Error = CapacityError;

    fn try_from(s: &str) -> Result<Self, CapacityError> {
        Self::try_from_str(s)
    }
}

impl<const N: usize> FromStr for InlineString<N> {
    type Err = CapacityError;

    fn from_str(s: &str) -> Result<Self, CapacityError> {
        Self::try_from_str(s)
    }
}

impl<const N: usize> fmt::Display for InlineString<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl<const N: usize> fmt::Debug for InlineString<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

// Comparisons and hashing go through `str`, consistent with `Borrow<str>`.
impl<const N: usize, const M: usize> PartialEq<InlineString<M>> for InlineString<N> {
    fn eq(&self, other: &InlineString<M>) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<const N: usize> PartialEq<str> for InlineString<N> {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl<const N: usize> PartialEq<&str> for InlineString<N> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl<const N: usize> Eq for InlineString<N> {}

impl<const N: usize> PartialOrd for InlineString<N> {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<const N: usize> Ord for InlineString<N> {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl<const N: usize> Hash for InlineString<N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}
