//! Stable and fast hash functions.
//!
//! Two families with opposite contracts:
//!
//! - [`FastHasher`] / [`FastState`]: for in-memory hash maps. Tuned for speed on short keys, not
//!   for hash-flooding resistance, and free to change between engine versions. Never persist its
//!   output. [`PrehashedState`] passes keys that already are good hashes straight through.
//! - [`stable`]: xxh3-64, xxh3-128, xxh32, xxh64 and FNV-1a. Their output is fixed forever: identical on every
//!   platform, endianness, compiler and run, and computable in `const fn`. Use them for anything
//!   that leaves the process: asset IDs and GUIDs, name hashes in cooked data, network IDs.
#![cfg_attr(not(test), no_std)]
#![feature(const_eval_select, core_intrinsics, portable_simd)]
// `const_eval_select` is an intrinsic: the const and runtime paths are tested to agree.
#![allow(internal_features)]

mod fast;
mod simd;
pub mod stable;

pub use fast::{FastHasher, FastState, PrehashedHasher, PrehashedState, fast_hash};
pub use stable::{
    fnv1a_32, fnv1a_64, xxh3_64, xxh3_64_ascii_lowercase, xxh3_64_with_seed, xxh3_128,
    xxh3_128_with_seed, xxh32, xxh64,
};

/// 64x64 -> 128-bit multiply, folded back to 64 bits by xoring the halves. The core mixing step of
/// both families: one `mul` (+ `umulh` on aarch64) mixes every input bit into every output bit.
#[inline(always)]
const fn folded_multiply(x: u64, y: u64) -> u64 {
    let full = (x as u128).wrapping_mul(y as u128);
    (full as u64) ^ ((full >> 64) as u64)
}
