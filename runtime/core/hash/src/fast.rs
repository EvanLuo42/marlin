//! In-memory hashing for hash maps (foldhash-style).
//!
//! Integer writes are buffered in a 128-bit sponge and folded into the accumulator with one
//! multiply when it fills, so a `u32` or `u64` key costs a single folded multiply. Byte strings
//! are hashed in 16- and 32-byte steps, each step one folded multiply per 16 bytes.
//!
//! Not flooding resistant: an attacker who knows the seed can force collisions (and a zero operand
//! erases a step's state, as in wyhash). Engine maps are keyed by engine data, so speed wins.

use core::hash::{BuildHasher, Hash, Hasher};

use crate::folded_multiply;

/// Fractional digits of pi: arbitrary odd-looking constants with no structure to exploit.
const K: [u64; 6] = [
    0x243f_6a88_85a3_08d3,
    0x1319_8a2e_0370_7344,
    0xa409_3822_299f_31d0,
    0x082e_fa98_ec4e_6c89,
    0x4528_21e6_38d0_1377,
    0xbe54_66cf_34e9_0c6c,
];

/// Seed used by [`FastState::new`]. Fixed rather than random so map iteration order, and hence
/// anything derived from it, reproduces from run to run.
const DEFAULT_SEED: u64 = 0xc0ac_29b7_c97c_50dd;

/// Fast non-cryptographic hasher for in-memory hash maps. Output is not stable across engine
/// versions; use [`crate::stable`] for anything persisted.
#[derive(Clone, Debug)]
pub struct FastHasher {
    acc: u64,
    sponge: u128,
    sponge_bits: u32,
}

impl FastHasher {
    /// Creates a hasher whose output depends on `seed`.
    #[inline]
    pub const fn with_seed(seed: u64) -> Self {
        Self {
            // Pre-mixed so that weak seeds (0, small integers) still give a well-spread start.
            acc: folded_multiply(seed ^ K[5], DEFAULT_SEED),
            sponge: 0,
            sponge_bits: 0,
        }
    }

    #[inline(always)]
    fn write_num(&mut self, x: u128, bits: u32) {
        if self.sponge_bits + bits > 128 {
            self.acc = fold_sponge(self.acc, self.sponge);
            self.sponge = x;
            self.sponge_bits = bits;
        } else {
            // `sponge_bits < 128` here (bits > 0), so the shift is in range.
            self.sponge |= x << self.sponge_bits;
            self.sponge_bits += bits;
        }
    }
}

impl Default for FastHasher {
    #[inline]
    fn default() -> Self {
        Self::with_seed(DEFAULT_SEED)
    }
}

#[inline(always)]
const fn fold_sponge(acc: u64, sponge: u128) -> u64 {
    folded_multiply(sponge as u64 ^ acc, (sponge >> 64) as u64 ^ K[0])
}

#[inline(always)]
fn r64(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

#[inline(always)]
fn r32(b: &[u8], i: usize) -> u64 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as u64
}

#[inline]
fn hash_bytes(acc: u64, bytes: &[u8]) -> u64 {
    let len = bytes.len();
    // Mixing the length in keeps "" and "\0" (and every other zero-padded pair) apart.
    let s = acc.wrapping_add(len as u64);
    if len <= 16 {
        // Overlapping reads cover every byte without a loop or a tail.
        let (a, b) = if len >= 8 {
            (r64(bytes, 0), r64(bytes, len - 8))
        } else if len >= 4 {
            (r32(bytes, 0), r32(bytes, len - 4))
        } else if len > 0 {
            let b = ((bytes[len / 2] as u64) << 8) | bytes[len - 1] as u64;
            (bytes[0] as u64, b)
        } else {
            (0, 0)
        };
        return folded_multiply(a ^ s ^ K[1], b ^ K[2]);
    }
    hash_bytes_long(s, bytes)
}

// Out of line so the short-key path above stays small enough to inline everywhere.
#[inline(never)]
fn hash_bytes_long(s: u64, bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mut s0 = s ^ K[1];
    let mut s1 = s ^ K[3];
    let mut i = 0;
    // Two independent lanes so the multiplies overlap in the pipeline.
    while len - i > 32 {
        s0 = folded_multiply(r64(bytes, i) ^ s0, r64(bytes, i + 8) ^ K[2]);
        s1 = folded_multiply(r64(bytes, i + 16) ^ s1, r64(bytes, i + 24) ^ K[4]);
        i += 32;
    }
    // The last 32 bytes (or the first and last 16 when len <= 32), overlapping what came before.
    let t = len.saturating_sub(32);
    s0 = folded_multiply(r64(bytes, t) ^ s0, r64(bytes, t + 8) ^ K[2]);
    s1 = folded_multiply(r64(bytes, len - 16) ^ s1, r64(bytes, len - 8) ^ K[4]);
    s0 ^ s1
}

impl Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        if self.sponge_bits > 0 {
            fold_sponge(self.acc, self.sponge)
        } else {
            self.acc
        }
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.acc = hash_bytes(self.acc, bytes);
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.write_num(i as u128, 8);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.write_num(i as u128, 16);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.write_num(i as u128, 32);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.write_num(i as u128, 64);
    }

    #[inline]
    fn write_u128(&mut self, i: u128) {
        self.write_num(i, 128);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.write_num(i as u128, usize::BITS);
    }
}

/// [`BuildHasher`] for [`FastHasher`]; the hasher of `marlin_containers::HashMap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FastState {
    seed: u64,
}

impl FastState {
    /// The default, fixed seed.
    #[inline]
    pub const fn new() -> Self {
        Self { seed: DEFAULT_SEED }
    }

    /// A custom seed, e.g. a per-process random one for maps keyed by untrusted input.
    #[inline]
    pub const fn with_seed(seed: u64) -> Self {
        Self { seed }
    }
}

impl Default for FastState {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl BuildHasher for FastState {
    type Hasher = FastHasher;

    #[inline]
    fn build_hasher(&self) -> FastHasher {
        FastHasher::with_seed(self.seed)
    }
}

/// Hashes `value` with the default [`FastState`].
#[inline]
pub fn fast_hash<T: Hash + ?Sized>(value: &T) -> u64 {
    FastState::new().hash_one(value)
}

/// Hasher for keys that already are well-distributed hashes (asset IDs, `xxh3_64` values): a key
/// that writes one `u64` hashes to exactly that value. Anything else is folded in, so it stays
/// correct for other keys, just not faster.
#[derive(Clone, Copy, Debug, Default)]
pub struct PrehashedHasher(u64);

impl Hasher for PrehashedHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.0 = hash_bytes(self.0, bytes);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        // Starts at 0, so a single write is the identity; later writes still depend on order.
        self.0 = self.0.wrapping_mul(K[0]) ^ i;
    }

    #[inline]
    fn write_u128(&mut self, i: u128) {
        self.write_u64(i as u64);
        self.write_u64((i >> 64) as u64);
    }
}

/// [`BuildHasher`] for [`PrehashedHasher`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrehashedState;

impl BuildHasher for PrehashedState {
    type Hasher = PrehashedHasher;

    #[inline]
    fn build_hasher(&self) -> PrehashedHasher {
        PrehashedHasher(0)
    }
}
