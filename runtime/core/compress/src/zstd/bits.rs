//! zstd bit streams: the backward stream of Huffman and FSE data, and the forward stream of FSE
//! table descriptions.

use super::corrupt;
use crate::Result;

/// Reads a backward bit stream (RFC 8878 4.1): written LSB-first from the start, read from the
/// end. The last byte holds a 1 marker above the final bits. Reads past the start yield zero
/// bits and drive [`Self::remaining`] negative, which callers check.
pub(crate) struct RevBits<'a> {
    src: &'a [u8],
    /// `container` holds `src[ptr..ptr + 8]` (or all of a shorter `src`, in its low bytes).
    ptr: usize,
    container: u64,
    /// Bits consumed from the top of `container`; above 64 once the stream is overread.
    consumed: u32,
}

impl<'a> RevBits<'a> {
    pub fn new(src: &'a [u8]) -> Result<Self> {
        let Some(&last) = src.last() else {
            return Err(corrupt("empty bit stream"));
        };
        if last == 0 {
            return Err(corrupt("bit stream without end marker"));
        }
        // Skip the zero padding and the marker bit.
        let padding = last.leading_zeros() + 1;
        let (ptr, container, consumed) = if src.len() >= 8 {
            let ptr = src.len() - 8;
            (ptr, load(src, ptr), padding)
        } else {
            let mut word = [0u8; 8];
            word[..src.len()].copy_from_slice(src);
            (
                0,
                u64::from_le_bytes(word),
                padding + 8 * (8 - src.len() as u32),
            )
        };
        Ok(Self {
            src,
            ptr,
            container,
            consumed,
        })
    }

    /// Unread bits; negative once more bits were read than the stream holds.
    #[inline]
    pub fn remaining(&self) -> isize {
        (self.ptr * 8) as isize + 64 - self.consumed as isize
    }

    /// Refills so at least 56 bits can be read (fewer only near the start of the stream).
    #[inline]
    pub fn reload(&mut self) {
        if self.consumed > 64 || self.src.len() < 8 {
            return;
        }
        let back = ((self.consumed / 8) as usize).min(self.ptr);
        if back > 0 {
            self.ptr -= back;
            self.consumed -= back as u32 * 8;
            self.container = load(self.src, self.ptr);
        }
    }

    /// The next `n` bits (`n <= 56`, and at most what the last reload guaranteed) without
    /// consuming them.
    #[inline]
    pub fn peek(&self, n: u32) -> u64 {
        debug_assert!(n <= 56);
        if n == 0 {
            return 0;
        }
        match self.container.checked_shl(self.consumed) {
            Some(top) => top >> (64 - n),
            None => 0,
        }
    }

    #[inline]
    pub fn consume(&mut self, n: u32) {
        self.consumed = self.consumed.saturating_add(n);
    }

    #[inline]
    pub fn read(&mut self, n: u32) -> u64 {
        let v = self.peek(n);
        self.consume(n);
        v
    }
}

#[inline]
fn load(src: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(src[at..at + 8].try_into().unwrap())
}

/// Reads a forward LSB-first bit stream, for FSE table descriptions. Past the end it yields
/// zeros; [`Self::overread`] tells.
pub(crate) struct FwdBits<'a> {
    src: &'a [u8],
    bit: usize,
}

impl<'a> FwdBits<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        Self { src, bit: 0 }
    }

    /// The next `n <= 32` bits without consuming them.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        let byte = self.bit / 8;
        let mut word = [0u8; 8];
        if let Some(rest) = self.src.get(byte..) {
            let k = rest.len().min(8);
            word[..k].copy_from_slice(&rest[..k]);
        }
        let v = u64::from_le_bytes(word) >> (self.bit % 8);
        (v & ((1u64 << n) - 1)) as u32
    }

    #[inline]
    pub fn consume(&mut self, n: u32) {
        self.bit += n as usize;
    }

    pub fn overread(&self) -> bool {
        self.bit > self.src.len() * 8
    }

    /// Bytes used, rounding the last partial byte up.
    pub fn bytes_used(&self) -> usize {
        self.bit.div_ceil(8)
    }
}

/// Writes a forward LSB-first bit stream; closed with a 1 marker bit it reads back with
/// [`RevBits`].
#[cfg(feature = "encode")]
pub(crate) struct BitWriter<'a> {
    out: &'a mut alloc::vec::Vec<u8>,
    acc: u64,
    n: u32,
}

#[cfg(feature = "encode")]
impl<'a> BitWriter<'a> {
    pub fn new(out: &'a mut alloc::vec::Vec<u8>) -> Self {
        Self { out, acc: 0, n: 0 }
    }

    /// Appends the low `n <= 32` bits of `v`.
    #[inline]
    pub fn put(&mut self, v: u64, n: u32) {
        debug_assert!(n <= 32);
        if n == 0 {
            return;
        }
        self.acc |= (v & ((1u64 << n) - 1)) << self.n;
        self.n += n;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    /// Flushes whole and partial bytes, without a marker (FSE table descriptions).
    pub fn finish(mut self) {
        while self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n = self.n.saturating_sub(8);
        }
    }

    /// Adds the end marker and flushes: the form [`RevBits`] reads.
    pub fn close(mut self) {
        self.put(1, 1);
        self.finish();
    }
}
