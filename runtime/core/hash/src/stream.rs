//! Streaming XXH64 and XXH3: input in any number of pieces, the same bits as the one-shot
//! functions over the concatenation, however it is split.
//!
//! Both also implement [`Hasher`], with integers written little-endian (and `usize` as `u64`) so
//! the bytes do not depend on the host. `#[derive(Hash)]` output is still not a stable encoding:
//! the standard library may change how its types feed a hasher. Use [`crate::StableHash`] for
//! values whose hash is persisted.

use core::hash::Hasher;
use core::{cmp, fmt};

use crate::simd;
use crate::stable::{
    P64_1, P64_2, P64_5, SECRET_LEN, STRIPE_LEN, Secret, derive_secret, merge_accs,
    xxh3_64_with_seed, xxh3_128_with_seed, xxh64_finish, xxh64_lanes, xxh64_merge, xxh64_stripe,
};

/// Integer writes in little-endian order, `usize` widened to `u64`.
macro_rules! le_writes {
    () => {
        #[inline]
        fn write_u8(&mut self, i: u8) {
            self.update(&[i]);
        }

        #[inline]
        fn write_u16(&mut self, i: u16) {
            self.update(&i.to_le_bytes());
        }

        #[inline]
        fn write_u32(&mut self, i: u32) {
            self.update(&i.to_le_bytes());
        }

        #[inline]
        fn write_u64(&mut self, i: u64) {
            self.update(&i.to_le_bytes());
        }

        #[inline]
        fn write_u128(&mut self, i: u128) {
            self.update(&i.to_le_bytes());
        }

        #[inline]
        fn write_usize(&mut self, i: usize) {
            self.update(&(i as u64).to_le_bytes());
        }
    };
}

// --- XXH64 ---

/// Streaming [`xxh64`](crate::xxh64): `update` any number of times, then [`digest`](Self::digest).
#[derive(Clone)]
pub struct Xxh64 {
    lanes: [u64; 4],
    buffer: [u8; 32],
    buffered: usize,
    total: u64,
    seed: u64,
}

impl Xxh64 {
    #[inline]
    pub const fn new(seed: u64) -> Self {
        Self {
            lanes: xxh64_lanes(seed),
            buffer: [0; 32],
            buffered: 0,
            total: 0,
            seed,
        }
    }

    pub fn update(&mut self, mut input: &[u8]) {
        self.total = self.total.wrapping_add(input.len() as u64);
        if self.buffered > 0 {
            let n = cmp::min(32 - self.buffered, input.len());
            self.buffer[self.buffered..self.buffered + n].copy_from_slice(&input[..n]);
            self.buffered += n;
            input = &input[n..];
            if self.buffered < 32 {
                return;
            }
            xxh64_stripe(&mut self.lanes, &self.buffer, 0);
            self.buffered = 0;
        }
        let whole = input.len() / 32 * 32;
        let mut i = 0;
        while i < whole {
            xxh64_stripe(&mut self.lanes, input, i);
            i += 32;
        }
        let rest = &input[whole..];
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    /// The hash of everything so far. Does not reset; more input may follow.
    pub fn digest(&self) -> u64 {
        let h = if self.total >= 32 {
            xxh64_merge(&self.lanes)
        } else {
            self.seed.wrapping_add(P64_5)
        };
        xxh64_finish(h.wrapping_add(self.total), &self.buffer[..self.buffered], 0)
    }

    /// Back to the empty input, keeping the seed.
    pub fn reset(&mut self) {
        *self = Self::new(self.seed);
    }
}

impl Default for Xxh64 {
    /// Seed 0.
    #[inline]
    fn default() -> Self {
        Self::new(0)
    }
}

impl Hasher for Xxh64 {
    #[inline]
    fn finish(&self) -> u64 {
        self.digest()
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }

    le_writes!();
}

impl fmt::Debug for Xxh64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Xxh64").field("len", &self.total).finish()
    }
}

// --- XXH3 ---

/// Inputs up to this many bytes stay buffered, so a short input is hashed by the one-shot code at
/// the end. Must be at least 240 (the longest short input) and a multiple of the stripe length.
const BUFFER_LEN: usize = 256;

/// Streaming [`xxh3_64`](crate::xxh3_64) and [`xxh3_128`](crate::xxh3_128): `update` any number
/// of times, then [`digest`](Self::digest) or [`digest128`](Self::digest128) (or both).
///
/// About 600 bytes of state: keep it on the stack or in the object that owns the stream.
#[derive(Clone)]
pub struct Xxh3 {
    acc: [u64; 8],
    secret: Secret,
    /// Input not yet consumed: 1..=256 bytes once there is any input. A stripe is consumed only
    /// when more input follows it, so the final stripe is always available to `digest`.
    buffer: [u8; BUFFER_LEN],
    /// The 64 input bytes just before `buffer`, for a final stripe that reaches back past it.
    prev: [u8; STRIPE_LEN],
    buffered: usize,
    /// Stripes consumed in the current block.
    stripes: usize,
    total: u64,
    seed: u64,
}

impl Xxh3 {
    /// Seed 0.
    #[inline]
    pub const fn new() -> Self {
        Self::with_seed(0)
    }

    pub const fn with_seed(seed: u64) -> Self {
        Self {
            acc: crate::stable::INIT_ACC,
            secret: derive_secret(seed),
            buffer: [0; BUFFER_LEN],
            prev: [0; STRIPE_LEN],
            buffered: 0,
            stripes: 0,
            total: 0,
            seed,
        }
    }

    pub fn update(&mut self, mut input: &[u8]) {
        self.total = self.total.wrapping_add(input.len() as u64);
        let free = BUFFER_LEN - self.buffered;
        if input.len() <= free {
            self.buffer[self.buffered..self.buffered + input.len()].copy_from_slice(input);
            self.buffered += input.len();
            return;
        }
        if self.buffered > 0 {
            // Input follows the full buffer, so all of it can be consumed.
            let (head, rest) = input.split_at(free);
            self.buffer[self.buffered..].copy_from_slice(head);
            input = rest;
            simd::consume_stripes(&mut self.acc, &mut self.stripes, &self.buffer, &self.secret);
            self.prev
                .copy_from_slice(&self.buffer[BUFFER_LEN - STRIPE_LEN..]);
            self.buffered = 0;
        }
        if input.len() > STRIPE_LEN {
            // Every whole stripe that is not the last byte's.
            let n = (input.len() - 1) / STRIPE_LEN * STRIPE_LEN;
            let (stripes, rest) = input.split_at(n);
            simd::consume_stripes(&mut self.acc, &mut self.stripes, stripes, &self.secret);
            self.prev.copy_from_slice(&stripes[n - STRIPE_LEN..]);
            input = rest;
        }
        self.buffer[..input.len()].copy_from_slice(input);
        self.buffered = input.len();
    }

    /// XXH3-64 of everything so far. Does not reset; more input may follow.
    pub fn digest(&self) -> u64 {
        if self.total <= 240 {
            return xxh3_64_with_seed(&self.buffer[..self.buffered], self.seed);
        }
        let acc = self.long_acc();
        merge_accs(&acc, &self.secret, 11, self.total.wrapping_mul(P64_1))
    }

    /// XXH3-128 of everything so far. Does not reset; more input may follow.
    pub fn digest128(&self) -> u128 {
        if self.total <= 240 {
            return xxh3_128_with_seed(&self.buffer[..self.buffered], self.seed);
        }
        let acc = self.long_acc();
        let lo = merge_accs(&acc, &self.secret, 11, self.total.wrapping_mul(P64_1));
        let hi = merge_accs(
            &acc,
            &self.secret,
            SECRET_LEN - STRIPE_LEN - 11,
            !self.total.wrapping_mul(P64_2),
        );
        ((hi as u128) << 64) | lo as u128
    }

    /// Back to the empty input, keeping the seed.
    pub fn reset(&mut self) {
        self.acc = crate::stable::INIT_ACC;
        self.buffered = 0;
        self.stripes = 0;
        self.total = 0;
    }

    /// The accumulators with the buffered stripes and the final stripe consumed (long inputs).
    fn long_acc(&self) -> [u64; 8] {
        let mut acc = self.acc;
        let mut stripes = self.stripes;
        let b = self.buffered;
        let n = (b - 1) / STRIPE_LEN * STRIPE_LEN;
        simd::consume_stripes(&mut acc, &mut stripes, &self.buffer[..n], &self.secret);
        let mut last = [0; STRIPE_LEN];
        if b >= STRIPE_LEN {
            last.copy_from_slice(&self.buffer[b - STRIPE_LEN..b]);
        } else {
            last[..STRIPE_LEN - b].copy_from_slice(&self.prev[b..]);
            last[STRIPE_LEN - b..].copy_from_slice(&self.buffer[..b]);
        }
        simd::last_stripe(&mut acc, &last, &self.secret);
        acc
    }
}

impl Default for Xxh3 {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher for Xxh3 {
    /// The 64-bit digest.
    #[inline]
    fn finish(&self) -> u64 {
        self.digest()
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }

    le_writes!();
}

impl fmt::Debug for Xxh3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Xxh3").field("len", &self.total).finish()
    }
}
