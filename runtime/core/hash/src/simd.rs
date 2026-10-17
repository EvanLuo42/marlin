//! XXH3's long-input accumulation on portable SIMD: the eight accumulator lanes in one `u64x8`,
//! the layout the reference implementation keeps in SSE2/AVX2/AVX-512/NEON registers.
//!
//! The 32×32→64-bit multiply is written as `(k & 0xffff_ffff) * (k >> 32)` on 64-bit lanes, which
//! LLVM lowers to `pmuludq` / `umull`. Must stay bit-identical to `stable::long_accumulate`.

use core::simd::prelude::*;
use core::simd::simd_swizzle;

use crate::stable::{
    BLOCK_LEN, INIT_ACC, P32_1, SECRET_LEN, STRIPE_LEN, STRIPES_PER_BLOCK, Secret,
};

/// Eight little-endian u64s.
#[inline(always)]
fn load(bytes: &[u8; 64]) -> u64x8 {
    let chunk = *bytes;
    #[cfg(target_endian = "little")]
    {
        // SAFETY: both are 64 bytes of plain integers; on a little-endian host the byte order
        // of each lane is the one XXH3 defines.
        unsafe { core::mem::transmute::<[u8; 64], u64x8>(chunk) }
    }
    #[cfg(target_endian = "big")]
    {
        u64x8::from_array(core::array::from_fn(|k| {
            u64::from_le_bytes(*chunk[8 * k..].first_chunk().unwrap())
        }))
    }
}

/// `stable::ascii_lower_swar` on all eight lanes.
#[inline(always)]
fn ascii_lower(x: u64x8) -> u64x8 {
    let low7 = u64x8::splat(0x7f7f_7f7f_7f7f_7f7f);
    let high = u64x8::splat(0x8080_8080_8080_8080);
    let h = x & low7;
    let ge_a = h + u64x8::splat(0x3f3f_3f3f_3f3f_3f3f);
    let gt_z = h + u64x8::splat(0x2525_2525_2525_2525);
    let upper = ge_a & !gt_z & !x & high;
    x | (upper >> 2)
}

#[inline(always)]
fn accumulate_512<const L: bool>(acc: u64x8, stripe: &[u8; 64], key: u64x8) -> u64x8 {
    let mut data = load(stripe);
    if L {
        data = ascii_lower(data);
    }
    let k = data ^ key;
    let product = (k & u64x8::splat(0xffff_ffff)) * (k >> 32);
    // Each lane's input goes to its neighbor: acc[i ^ 1] += data[i].
    acc + simd_swizzle!(data, [1, 0, 3, 2, 5, 4, 7, 6]) + product
}

#[inline(always)]
fn scramble(acc: u64x8, key: u64x8) -> u64x8 {
    (acc ^ (acc >> 47) ^ key) * u64x8::splat(P32_1)
}

pub(crate) fn long_accumulate<const L: bool>(input: &[u8], secret: &Secret) -> [u64; 8] {
    let len = input.len();
    let scramble_key = load(window(secret, SECRET_LEN - STRIPE_LEN));
    let mut acc = u64x8::from_array(INIT_ACC);

    let blocks = (len - 1) / BLOCK_LEN;
    for block in input[..blocks * BLOCK_LEN].as_chunks::<BLOCK_LEN>().0 {
        for (s, stripe) in block.as_chunks::<STRIPE_LEN>().0.iter().enumerate() {
            acc = accumulate_512::<L>(acc, stripe, key(secret, s));
        }
        acc = scramble(acc, scramble_key);
    }

    let tail = &input[blocks * BLOCK_LEN..];
    let stripes = (tail.len() - 1) / STRIPE_LEN;
    for (s, stripe) in tail
        .as_chunks::<STRIPE_LEN>()
        .0
        .iter()
        .take(stripes)
        .enumerate()
    {
        acc = accumulate_512::<L>(acc, stripe, key(secret, s));
    }
    // The last stripe always covers the final 64 bytes, overlapping the previous one.
    let last_key = load(window(secret, SECRET_LEN - STRIPE_LEN - 7));
    let last = input
        .last_chunk::<STRIPE_LEN>()
        .expect("long inputs exceed 240 bytes");
    acc = accumulate_512::<L>(acc, last, last_key);
    acc.to_array()
}

/// Streaming XXH3: feeds whole stripes, `stripes` of the current block already consumed, and
/// scrambles at each block end. `data.len()` is a multiple of the stripe length.
pub(crate) fn consume_stripes(
    acc: &mut [u64; 8],
    stripes: &mut usize,
    data: &[u8],
    secret: &Secret,
) {
    debug_assert!(data.len().is_multiple_of(STRIPE_LEN));
    let scramble_key = load(window(secret, SECRET_LEN - STRIPE_LEN));
    let mut v = u64x8::from_array(*acc);
    for stripe in data.as_chunks::<STRIPE_LEN>().0 {
        v = accumulate_512::<false>(v, stripe, key(secret, *stripes));
        *stripes += 1;
        if *stripes == STRIPES_PER_BLOCK {
            v = scramble(v, scramble_key);
            *stripes = 0;
        }
    }
    *acc = v.to_array();
}

/// Streaming XXH3: the final stripe (the input's last 64 bytes), with its own key.
pub(crate) fn last_stripe(acc: &mut [u64; 8], stripe: &[u8; 64], secret: &Secret) {
    let v = u64x8::from_array(*acc);
    let last_key = load(window(secret, SECRET_LEN - STRIPE_LEN - 7));
    *acc = accumulate_512::<false>(v, stripe, last_key).to_array();
}

/// The 64 bytes of `secret` at `offset` (secret offsets are constants within bounds).
#[inline(always)]
fn window(secret: &Secret, offset: usize) -> &[u8; 64] {
    secret[offset..]
        .first_chunk()
        .expect("secret window out of bounds")
}

/// The key of stripe `s` within a block. Loaded per stripe rather than all up front: the secret is
/// in L1 anyway, and short inputs use only a few keys.
#[inline(always)]
fn key(secret: &Secret, s: usize) -> u64x8 {
    load(window(secret, 8 * s))
}
