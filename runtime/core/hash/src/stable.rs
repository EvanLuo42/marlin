//! Stable hashes: the same output on every platform, endianness, compiler and run, forever.
//!
//! Everything here is a `const fn`, so IDs can be computed at compile time. At runtime, XXH3 on
//! inputs over 240 bytes runs its accumulation loop on portable SIMD (8 × u64 lanes, as the
//! reference implementation's SSE2/AVX2/NEON paths do); short inputs are scalar in the reference
//! too. Both produce the same bits; [`scalar`] forces the scalar loop.
//!
//!
//! ```
//! const PLAYER: u64 = marlin_hash::xxh3_64(b"Player");
//! assert_eq!(PLAYER, marlin_hash::xxh3_64("Player".as_bytes()));
//! ```
//!
//! - [`xxh3_64`] / [`xxh3_128`]: XXH3 (xxHash 0.8), bit-exact with the reference implementation.
//!   The default choice; the 128-bit variant is for asset GUIDs, where 64 bits would make a
//!   collision across a large project's lifetime plausible.
//! - [`xxh3_64_ascii_lowercase`]: XXH3-64 of the input with ASCII lowercased, without copying it:
//!   the case-insensitive hash behind names.
//! - [`fnv1a_64`] / [`fnv1a_32`]: FNV-1a, for when bytes arrive piecewise ([`fnv1a_64_extend`])
//!   or a 32-bit hash is needed. One multiply per byte, so prefer XXH3 for anything long.
//!
//! The test vectors at the bottom pin every code path; they must never change.

// --- FNV-1a ---

/// FNV-1a 64-bit offset basis: the hash of the empty input.
pub const FNV1A_64_INIT: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_64_PRIME: u64 = 0x0000_0100_0000_01b3;
const FNV1A_32_INIT: u32 = 0x811c_9dc5;
const FNV1A_32_PRIME: u32 = 0x0100_0193;

/// 64-bit FNV-1a.
#[inline]
pub const fn fnv1a_64(bytes: &[u8]) -> u64 {
    fnv1a_64_extend(FNV1A_64_INIT, bytes)
}

/// Continues a 64-bit FNV-1a hash: `fnv1a_64_extend(fnv1a_64(a), b) == fnv1a_64(a ++ b)`.
#[inline]
pub const fn fnv1a_64_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV1A_64_PRIME);
        i += 1;
    }
    hash
}

/// 32-bit FNV-1a.
#[inline]
pub const fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut hash = FNV1A_32_INIT;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(FNV1A_32_PRIME);
        i += 1;
    }
    hash
}

// --- XXH3 ---

pub(crate) const P32_1: u64 = 0x9e37_79b1;
const P32_2: u64 = 0x85eb_ca77;
const P32_3: u64 = 0xc2b2_ae3d;
const P64_1: u64 = 0x9e37_79b1_85eb_ca87;
const P64_2: u64 = 0xc2b2_ae3d_27d4_eb4f;
const P64_3: u64 = 0x1656_67b1_9e37_79f9;
const P64_4: u64 = 0x85eb_ca77_c2b2_ae63;
const P64_5: u64 = 0x27d4_eb2f_1656_67c5;
const PMX_1: u64 = 0x1656_6791_9e37_79f9;
const PMX_2: u64 = 0x9fb2_1c65_1e98_df25;

pub(crate) const SECRET_LEN: usize = 192;
pub(crate) type Secret = [u8; SECRET_LEN];

/// The reference implementation's default secret (`XXH3_kSecret`).
#[rustfmt::skip]
const SECRET: Secret = [
    0xb8, 0xfe, 0x6c, 0x39, 0x23, 0xa4, 0x4b, 0xbe, 0x7c, 0x01, 0x81, 0x2c, 0xf7, 0x21, 0xad, 0x1c,
    0xde, 0xd4, 0x6d, 0xe9, 0x83, 0x90, 0x97, 0xdb, 0x72, 0x40, 0xa4, 0xa4, 0xb7, 0xb3, 0x67, 0x1f,
    0xcb, 0x79, 0xe6, 0x4e, 0xcc, 0xc0, 0xe5, 0x78, 0x82, 0x5a, 0xd0, 0x7d, 0xcc, 0xff, 0x72, 0x21,
    0xb8, 0x08, 0x46, 0x74, 0xf7, 0x43, 0x24, 0x8e, 0xe0, 0x35, 0x90, 0xe6, 0x81, 0x3a, 0x26, 0x4c,
    0x3c, 0x28, 0x52, 0xbb, 0x91, 0xc3, 0x00, 0xcb, 0x88, 0xd0, 0x65, 0x8b, 0x1b, 0x53, 0x2e, 0xa3,
    0x71, 0x64, 0x48, 0x97, 0xa2, 0x0d, 0xf9, 0x4e, 0x38, 0x19, 0xef, 0x46, 0xa9, 0xde, 0xac, 0xd8,
    0xa8, 0xfa, 0x76, 0x3f, 0xe3, 0x9c, 0x34, 0x3f, 0xf9, 0xdc, 0xbb, 0xc7, 0xc7, 0x0b, 0x4f, 0x1d,
    0x8a, 0x51, 0xe0, 0x4b, 0xcd, 0xb4, 0x59, 0x31, 0xc8, 0x9f, 0x7e, 0xc9, 0xd9, 0x78, 0x73, 0x64,
    0xea, 0xc5, 0xac, 0x83, 0x34, 0xd3, 0xeb, 0xc3, 0xc5, 0x81, 0xa0, 0xff, 0xfa, 0x13, 0x63, 0xeb,
    0x17, 0x0d, 0xdd, 0x51, 0xb7, 0xf0, 0xda, 0x49, 0xd3, 0x16, 0x55, 0x26, 0x29, 0xd4, 0x68, 0x9e,
    0x2b, 0x16, 0xbe, 0x58, 0x7d, 0x47, 0xa1, 0xfc, 0x8f, 0xf8, 0xb8, 0xd1, 0x7a, 0xd0, 0x31, 0xce,
    0x45, 0xcb, 0x3a, 0x8f, 0x95, 0x16, 0x04, 0x28, 0xaf, 0xd7, 0xfb, 0xca, 0xbb, 0x4b, 0x40, 0x7e,
];

pub(crate) const STRIPE_LEN: usize = 64;
pub(crate) const STRIPES_PER_BLOCK: usize = (SECRET_LEN - STRIPE_LEN) / 8;
pub(crate) const BLOCK_LEN: usize = STRIPE_LEN * STRIPES_PER_BLOCK;
pub(crate) const INIT_ACC: [u64; 8] = [P32_3, P64_1, P64_2, P64_3, P64_4, P32_2, P64_5, P32_1];

/// XXH3 64-bit with seed 0.
#[inline]
pub const fn xxh3_64(bytes: &[u8]) -> u64 {
    xxh3_64_with_seed(bytes, 0)
}

/// XXH3 64-bit (`XXH3_64bits_withSeed`).
pub const fn xxh3_64_with_seed(input: &[u8], seed: u64) -> u64 {
    xxh3_64_impl::<false, false>(input, seed)
}

/// XXH3 64-bit of `input` with ASCII `A-Z` mapped to `a-z`, without a temporary copy:
/// `xxh3_64_ascii_lowercase(s) == xxh3_64(s.to_ascii_lowercase())`. The case-insensitive stable
/// hash used for names. Bytes >= 0x80 are untouched, so UTF-8 passes through intact.
#[inline]
pub const fn xxh3_64_ascii_lowercase(input: &[u8]) -> u64 {
    xxh3_64_impl::<true, false>(input, 0)
}

// `L`: lowercase ASCII while reading the input (monomorphized, so free when false).
const fn xxh3_64_impl<const L: bool, const S: bool>(input: &[u8], seed: u64) -> u64 {
    let len = input.len();
    if len <= 16 {
        if len > 8 {
            let bitflip1 = (r64(&SECRET, 24) ^ r64(&SECRET, 32)).wrapping_add(seed);
            let bitflip2 = (r64(&SECRET, 40) ^ r64(&SECRET, 48)).wrapping_sub(seed);
            let lo = in64::<L>(input, 0) ^ bitflip1;
            let hi = in64::<L>(input, len - 8) ^ bitflip2;
            let acc = (len as u64)
                .wrapping_add(lo.swap_bytes())
                .wrapping_add(hi)
                .wrapping_add(folded_multiply(lo, hi));
            avalanche(acc)
        } else if len >= 4 {
            let seed = seed ^ (((seed as u32).swap_bytes() as u64) << 32);
            let in1 = in32::<L>(input, 0);
            let in2 = in32::<L>(input, len - 4);
            let bitflip = (r64(&SECRET, 8) ^ r64(&SECRET, 16)).wrapping_sub(seed);
            rrmxmx(in2.wrapping_add(in1 << 32) ^ bitflip, len as u64)
        } else if len > 0 {
            let bitflip = (r32(&SECRET, 0) ^ r32(&SECRET, 4)).wrapping_add(seed);
            xxh64_avalanche(combine_1to3::<L>(input) as u64 ^ bitflip)
        } else {
            xxh64_avalanche(seed ^ r64(&SECRET, 56) ^ r64(&SECRET, 64))
        }
    } else if len <= 128 {
        let mut acc = (len as u64).wrapping_mul(P64_1);
        // Pairs of 16-byte blocks from both ends, working inwards.
        let mut i = (len - 1) / 32;
        loop {
            acc = acc.wrapping_add(mix16::<L>(input, 16 * i, &SECRET, 32 * i, seed));
            acc = acc.wrapping_add(mix16::<L>(
                input,
                len - 16 * (i + 1),
                &SECRET,
                32 * i + 16,
                seed,
            ));
            if i == 0 {
                break;
            }
            i -= 1;
        }
        avalanche(acc)
    } else if len <= 240 {
        let mut acc = (len as u64).wrapping_mul(P64_1);
        let mut i = 0;
        while i < 8 {
            acc = acc.wrapping_add(mix16::<L>(input, 16 * i, &SECRET, 16 * i, seed));
            i += 1;
        }
        acc = avalanche(acc);
        while i < len / 16 {
            acc = acc.wrapping_add(mix16::<L>(input, 16 * i, &SECRET, 16 * (i - 8) + 3, seed));
            i += 1;
        }
        acc = acc.wrapping_add(mix16::<L>(input, len - 16, &SECRET, 136 - 17, seed));
        avalanche(acc)
    } else {
        let secret = derive_secret(seed);
        let acc = accumulate_long::<L, S>(input, &secret);
        merge_accs(&acc, &secret, 11, (len as u64).wrapping_mul(P64_1))
    }
}

/// XXH3 128-bit with seed 0. The high half of XXH128's result is the high 64 bits of the `u128`.
#[inline]
pub const fn xxh3_128(bytes: &[u8]) -> u128 {
    xxh3_128_with_seed(bytes, 0)
}

/// XXH3 128-bit (`XXH3_128bits_withSeed`).
pub const fn xxh3_128_with_seed(input: &[u8], seed: u64) -> u128 {
    xxh3_128_impl::<false, false>(input, seed)
}

const fn xxh3_128_impl<const L: bool, const S: bool>(input: &[u8], seed: u64) -> u128 {
    let len = input.len();
    let (lo, hi) = if len <= 16 {
        if len > 8 {
            let bitflipl = (r64(&SECRET, 32) ^ r64(&SECRET, 40)).wrapping_sub(seed);
            let bitfliph = (r64(&SECRET, 48) ^ r64(&SECRET, 56)).wrapping_add(seed);
            let in_lo = in64::<L>(input, 0);
            let in_hi = in64::<L>(input, len - 8);
            let (mut m_lo, mut m_hi) = mul128(in_lo ^ in_hi ^ bitflipl, P64_1);
            m_lo = m_lo.wrapping_add((len as u64 - 1) << 54);
            let in_hi = in_hi ^ bitfliph;
            m_hi = m_hi
                .wrapping_add(in_hi)
                .wrapping_add((in_hi & 0xffff_ffff).wrapping_mul(P32_2 - 1));
            m_lo ^= m_hi.swap_bytes();
            let (h_lo, h_hi) = mul128(m_lo, P64_2);
            (
                avalanche(h_lo),
                avalanche(h_hi.wrapping_add(m_hi.wrapping_mul(P64_2))),
            )
        } else if len >= 4 {
            let seed = seed ^ (((seed as u32).swap_bytes() as u64) << 32);
            let input64 = in32::<L>(input, 0).wrapping_add(in32::<L>(input, len - 4) << 32);
            let bitflip = (r64(&SECRET, 16) ^ r64(&SECRET, 24)).wrapping_add(seed);
            let (mut lo, mut hi) = mul128(input64 ^ bitflip, P64_1.wrapping_add((len as u64) << 2));
            hi = hi.wrapping_add(lo << 1);
            lo ^= hi >> 3;
            lo ^= lo >> 35;
            lo = lo.wrapping_mul(PMX_2);
            lo ^= lo >> 28;
            (lo, avalanche(hi))
        } else if len > 0 {
            let combined_lo = combine_1to3::<L>(input);
            let combined_hi = combined_lo.swap_bytes().rotate_left(13);
            let bitflip_lo = (r32(&SECRET, 0) ^ r32(&SECRET, 4)).wrapping_add(seed);
            let bitflip_hi = (r32(&SECRET, 8) ^ r32(&SECRET, 12)).wrapping_sub(seed);
            (
                xxh64_avalanche(combined_lo as u64 ^ bitflip_lo),
                xxh64_avalanche(combined_hi as u64 ^ bitflip_hi),
            )
        } else {
            (
                xxh64_avalanche(seed ^ r64(&SECRET, 64) ^ r64(&SECRET, 72)),
                xxh64_avalanche(seed ^ r64(&SECRET, 80) ^ r64(&SECRET, 88)),
            )
        }
    } else if len <= 240 {
        let mut acc = ((len as u64).wrapping_mul(P64_1), 0);
        if len <= 128 {
            let mut i = (len - 1) / 32;
            loop {
                acc = mix32::<L>(acc, input, 16 * i, len - 16 * (i + 1), 32 * i, seed);
                if i == 0 {
                    break;
                }
                i -= 1;
            }
        } else {
            let mut i = 32;
            while i < 160 {
                acc = mix32::<L>(acc, input, i - 32, i - 16, i - 32, seed);
                i += 32;
            }
            acc = (avalanche(acc.0), avalanche(acc.1));
            while i <= len {
                acc = mix32::<L>(acc, input, i - 32, i - 16, 3 + i - 160, seed);
                i += 32;
            }
            acc = mix32::<L>(
                acc,
                input,
                len - 16,
                len - 32,
                136 - 17 - 16,
                0u64.wrapping_sub(seed),
            );
        }
        let lo = acc.0.wrapping_add(acc.1);
        let hi = acc
            .0
            .wrapping_mul(P64_1)
            .wrapping_add(acc.1.wrapping_mul(P64_4))
            .wrapping_add((len as u64).wrapping_sub(seed).wrapping_mul(P64_2));
        (avalanche(lo), 0u64.wrapping_sub(avalanche(hi)))
    } else {
        let secret = derive_secret(seed);
        let acc = accumulate_long::<L, S>(input, &secret);
        (
            merge_accs(&acc, &secret, 11, (len as u64).wrapping_mul(P64_1)),
            merge_accs(
                &acc,
                &secret,
                SECRET_LEN - STRIPE_LEN - 11,
                !(len as u64).wrapping_mul(P64_2),
            ),
        )
    };
    ((hi as u128) << 64) | lo as u128
}

// Little-endian reads: the hashes are defined on LE words regardless of the host.
#[inline(always)]
const fn r64(b: &[u8], i: usize) -> u64 {
    let Some(word) = b.split_at(i).1.first_chunk::<8>() else {
        panic!("read past end");
    };
    u64::from_le_bytes(*word)
}

#[inline(always)]
const fn r32(b: &[u8], i: usize) -> u64 {
    let Some(word) = b.split_at(i).1.first_chunk::<4>() else {
        panic!("read past end");
    };
    u32::from_le_bytes(*word) as u64
}

// Input reads, optionally ASCII-lowercased. Secret reads always use `r64` / `r32`.
#[inline(always)]
const fn in64<const L: bool>(b: &[u8], i: usize) -> u64 {
    let x = r64(b, i);
    if L { ascii_lower_swar(x) } else { x }
}

#[inline(always)]
const fn in32<const L: bool>(b: &[u8], i: usize) -> u64 {
    // The upper half is zero, which the SWAR lowering leaves alone.
    let x = r32(b, i);
    if L { ascii_lower_swar(x) } else { x }
}

#[inline(always)]
const fn in8<const L: bool>(b: &[u8], i: usize) -> u32 {
    let x = b[i];
    (if L { x.to_ascii_lowercase() } else { x }) as u32
}

/// `to_ascii_lowercase` on all eight bytes at once. Per byte, with `h` its low seven bits:
/// `h + 0x3f` sets bit 7 iff `h >= b'A'`, `h + 0x25` iff `h > b'Z'`; neither carries into the
/// next byte. Bytes with bit 7 already set (non-ASCII) are excluded.
#[inline(always)]
const fn ascii_lower_swar(x: u64) -> u64 {
    const LOW7: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let h = x & LOW7;
    let ge_a = h + 0x3f3f_3f3f_3f3f_3f3f;
    let gt_z = h + 0x2525_2525_2525_2525;
    let upper = ge_a & !gt_z & !x & HIGH;
    x | (upper >> 2)
}

#[inline(always)]
const fn folded_multiply(x: u64, y: u64) -> u64 {
    crate::folded_multiply(x, y)
}

#[inline(always)]
const fn mul128(x: u64, y: u64) -> (u64, u64) {
    let full = (x as u128).wrapping_mul(y as u128);
    (full as u64, (full >> 64) as u64)
}

#[inline(always)]
const fn xxh64_avalanche(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(P64_2);
    h ^= h >> 29;
    h = h.wrapping_mul(P64_3);
    h ^ (h >> 32)
}

#[inline(always)]
const fn avalanche(mut h: u64) -> u64 {
    h ^= h >> 37;
    h = h.wrapping_mul(PMX_1);
    h ^ (h >> 32)
}

#[inline(always)]
const fn rrmxmx(mut h: u64, len: u64) -> u64 {
    h ^= h.rotate_left(49) ^ h.rotate_left(24);
    h = h.wrapping_mul(PMX_2);
    h ^= (h >> 35).wrapping_add(len);
    h = h.wrapping_mul(PMX_2);
    h ^ (h >> 28)
}

#[inline(always)]
const fn combine_1to3<const L: bool>(input: &[u8]) -> u32 {
    let len = input.len();
    let c1 = in8::<L>(input, 0);
    let c2 = in8::<L>(input, len >> 1);
    let c3 = in8::<L>(input, len - 1);
    (c1 << 16) | (c2 << 24) | c3 | ((len as u32) << 8)
}

#[inline(always)]
const fn mix16<const L: bool>(input: &[u8], i: usize, secret: &Secret, s: usize, seed: u64) -> u64 {
    folded_multiply(
        in64::<L>(input, i) ^ r64(secret, s).wrapping_add(seed),
        in64::<L>(input, i + 8) ^ r64(secret, s + 8).wrapping_sub(seed),
    )
}

#[inline(always)]
const fn mix32<const L: bool>(
    acc: (u64, u64),
    input: &[u8],
    i1: usize,
    i2: usize,
    s: usize,
    seed: u64,
) -> (u64, u64) {
    let mut lo = acc.0.wrapping_add(mix16::<L>(input, i1, &SECRET, s, seed));
    lo ^= in64::<L>(input, i2).wrapping_add(in64::<L>(input, i2 + 8));
    let mut hi = acc
        .1
        .wrapping_add(mix16::<L>(input, i2, &SECRET, s + 16, seed));
    hi ^= in64::<L>(input, i1).wrapping_add(in64::<L>(input, i1 + 8));
    (lo, hi)
}

/// The secret a seeded long hash uses (`XXH3_initCustomSecret`); seed 0 yields the default.
const fn derive_secret(seed: u64) -> Secret {
    if seed == 0 {
        return SECRET;
    }
    let mut out = [0; SECRET_LEN];
    let mut i = 0;
    while i < SECRET_LEN {
        let lo = r64(&SECRET, i).wrapping_add(seed).to_le_bytes();
        let hi = r64(&SECRET, i + 8).wrapping_sub(seed).to_le_bytes();
        let mut j = 0;
        while j < 8 {
            out[i + j] = lo[j];
            out[i + 8 + j] = hi[j];
            j += 1;
        }
        i += 16;
    }
    out
}

/// The long-input accumulation: this file's scalar loop in const evaluation (or when `S` forces
/// it), [`crate::simd`]'s vector version at runtime. Both produce identical lanes.
#[inline(always)]
const fn accumulate_long<const L: bool, const S: bool>(input: &[u8], secret: &Secret) -> [u64; 8] {
    if S {
        return long_accumulate::<L>(input, secret);
    }
    core::intrinsics::const_eval_select(
        (input, secret),
        long_accumulate::<L>,
        crate::simd::long_accumulate::<L>,
    )
}

/// The same hashes with the long-input path forced to scalar code at runtime: the portable
/// reference the SIMD path is verified and benchmarked against. Prefer the functions above.
pub mod scalar {
    use super::{xxh3_64_impl, xxh3_128_impl};

    pub fn xxh3_64_with_seed(input: &[u8], seed: u64) -> u64 {
        xxh3_64_impl::<false, true>(input, seed)
    }

    pub fn xxh3_64_ascii_lowercase(input: &[u8]) -> u64 {
        xxh3_64_impl::<true, true>(input, 0)
    }

    pub fn xxh3_128_with_seed(input: &[u8], seed: u64) -> u128 {
        xxh3_128_impl::<false, true>(input, seed)
    }
}

/// Inputs over 240 bytes: eight lanes over 64-byte stripes, scrambled after every 1 KiB block.
const fn long_accumulate<const L: bool>(input: &[u8], secret: &Secret) -> [u64; 8] {
    let len = input.len();
    let mut acc = INIT_ACC;
    let blocks = (len - 1) / BLOCK_LEN;
    let mut n = 0;
    while n < blocks {
        let mut s = 0;
        while s < STRIPES_PER_BLOCK {
            accumulate_512::<L>(
                &mut acc,
                input,
                n * BLOCK_LEN + s * STRIPE_LEN,
                secret,
                s * 8,
            );
            s += 1;
        }
        scramble(&mut acc, secret);
        n += 1;
    }
    let stripes = ((len - 1) - blocks * BLOCK_LEN) / STRIPE_LEN;
    let mut s = 0;
    while s < stripes {
        accumulate_512::<L>(
            &mut acc,
            input,
            blocks * BLOCK_LEN + s * STRIPE_LEN,
            secret,
            s * 8,
        );
        s += 1;
    }
    // The last stripe always covers the final 64 bytes, overlapping the previous one.
    accumulate_512::<L>(
        &mut acc,
        input,
        len - STRIPE_LEN,
        secret,
        SECRET_LEN - STRIPE_LEN - 7,
    );
    acc
}

#[inline(always)]
const fn accumulate_512<const L: bool>(
    acc: &mut [u64; 8],
    input: &[u8],
    i: usize,
    secret: &Secret,
    s: usize,
) {
    let mut lane = 0;
    while lane < 8 {
        let value = in64::<L>(input, i + 8 * lane);
        let key = value ^ r64(secret, s + 8 * lane);
        acc[lane ^ 1] = acc[lane ^ 1].wrapping_add(value);
        acc[lane] = acc[lane].wrapping_add((key & 0xffff_ffff).wrapping_mul(key >> 32));
        lane += 1;
    }
}

#[inline(always)]
const fn scramble(acc: &mut [u64; 8], secret: &Secret) {
    let mut lane = 0;
    while lane < 8 {
        let mut a = acc[lane];
        a ^= a >> 47;
        a ^= r64(secret, SECRET_LEN - STRIPE_LEN + 8 * lane);
        acc[lane] = a.wrapping_mul(P32_1);
        lane += 1;
    }
}

const fn merge_accs(acc: &[u64; 8], secret: &Secret, s: usize, start: u64) -> u64 {
    let mut result = start;
    let mut i = 0;
    while i < 4 {
        result = result.wrapping_add(folded_multiply(
            acc[2 * i] ^ r64(secret, s + 16 * i),
            acc[2 * i + 1] ^ r64(secret, s + 16 * i + 8),
        ));
        i += 1;
    }
    avalanche(result)
}
