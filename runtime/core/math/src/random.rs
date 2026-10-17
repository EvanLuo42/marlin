//! Deterministic pseudo-random numbers: [`Rng`], xoshiro256** seeded through SplitMix64.
//!
//! Not for cryptography. The sequence for a given seed is part of the engine's contract (replays,
//! procedural content and tests depend on it), so the algorithm, the seeding and the way each
//! method consumes outputs must never change.

use crate::scalar::{TAU, sqrt};
use crate::{Quat, Vec3};

/// A xoshiro256** generator: 256 bits of state, period 2^256 − 1, fast and statistically strong.
///
/// [`Rng::jump`] advances by 2^128 outputs, which splits one seed into non-overlapping streams
/// (one per thread or system).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rng {
    s: [u64; 4],
}

/// SplitMix64: expands a seed into well-mixed state words.
const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Rng {
    /// The generator for `seed`; nearby seeds give unrelated sequences.
    pub const fn new(seed: u64) -> Self {
        let mut sm = seed;
        Self {
            s: [
                splitmix64(&mut sm),
                splitmix64(&mut sm),
                splitmix64(&mut sm),
                splitmix64(&mut sm),
            ],
        }
    }

    /// Starts from exactly `state` (for example one saved with [`Rng::state`]), which must not be
    /// all zero.
    pub const fn from_state(state: [u64; 4]) -> Self {
        assert!(
            state[0] | state[1] | state[2] | state[3] != 0,
            "xoshiro never leaves the zero state"
        );
        Self { s: state }
    }

    /// The current state, to save and later resume with [`Rng::from_state`].
    pub const fn state(&self) -> [u64; 4] {
        self.s
    }

    #[inline]
    pub const fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// The high half of [`Rng::next_u64`] (the strongest bits).
    #[inline]
    pub const fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform in `0..n`, without modulo bias (Lemire's method). `n` must not be zero.
    #[inline]
    pub const fn below_u64(&mut self, n: u64) -> u64 {
        assert!(n != 0, "empty range");
        let mut m = self.next_u64() as u128 * n as u128;
        if (m as u64) < n {
            let threshold = n.wrapping_neg() % n;
            while (m as u64) < threshold {
                m = self.next_u64() as u128 * n as u128;
            }
        }
        (m >> 64) as u64
    }

    /// Uniform in `0..n`, as [`Rng::below_u64`].
    #[inline]
    pub const fn below(&mut self, n: usize) -> usize {
        self.below_u64(n as u64) as usize
    }

    /// Uniform in [0, 1), from the top 24 bits.
    #[inline]
    pub const fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 * (1.0 / (1u32 << 24) as f32)
    }

    /// Uniform in [lo, hi).
    #[inline]
    pub const fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f32()
    }

    /// True with probability one half.
    #[inline]
    pub const fn bool(&mut self) -> bool {
        (self.next_u64() as i64) < 0
    }

    /// True with probability `p` (clamped to [0, 1]).
    #[inline]
    pub const fn chance(&mut self, p: f32) -> bool {
        self.f32() < p
    }

    /// Fills `bytes`, eight per output, little-endian.
    pub fn fill_bytes(&mut self, bytes: &mut [u8]) {
        let (chunks, tail) = bytes.as_chunks_mut::<8>();
        for chunk in chunks {
            *chunk = self.next_u64().to_le_bytes();
        }
        if !tail.is_empty() {
            let last = self.next_u64().to_le_bytes();
            tail.copy_from_slice(&last[..tail.len()]);
        }
    }

    /// Fisher–Yates shuffle: every permutation equally likely.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }

    /// A uniformly chosen element, or `None` if `items` is empty.
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            Some(&items[self.below(items.len())])
        }
    }

    /// Uniform on the unit sphere.
    pub fn unit_vec3(&mut self) -> Vec3 {
        let z = self.range_f32(-1.0, 1.0);
        let phi = TAU * self.f32();
        let r = sqrt((1.0 - z * z).max(0.0));
        Vec3::new(r * crate::scalar::cos(phi), r * crate::scalar::sin(phi), z)
    }

    /// A uniformly distributed rotation (Shoemake's method).
    pub fn rotation(&mut self) -> Quat {
        let (u1, u2, u3) = (self.f32(), TAU * self.f32(), TAU * self.f32());
        let (a, b) = (sqrt(1.0 - u1), sqrt(u1));
        let (s2, c2) = crate::scalar::sin_cos(u2);
        let (s3, c3) = crate::scalar::sin_cos(u3);
        Quat::from_xyzw(a * s2, a * c2, b * s3, b * c3)
    }

    /// Advances by 2^128 outputs. Calling it on clones gives streams that never overlap:
    /// `let mut b = a.clone(); b.jump();`.
    pub const fn jump(&mut self) {
        const JUMP: [u64; 4] = [
            0x180e_c6d3_3cfd_0aba,
            0xd5a6_1266_f0c9_392c,
            0xa958_2618_e03f_c9aa,
            0x39ab_dc45_29b1_661c,
        ];
        let mut acc = [0u64; 4];
        let mut i = 0;
        while i < 4 {
            let mut bit = 0;
            while bit < 64 {
                if JUMP[i] & (1 << bit) != 0 {
                    acc[0] ^= self.s[0];
                    acc[1] ^= self.s[1];
                    acc[2] ^= self.s[2];
                    acc[3] ^= self.s[3];
                }
                self.next_u64();
                bit += 1;
            }
            i += 1;
        }
        self.s = acc;
    }
}
