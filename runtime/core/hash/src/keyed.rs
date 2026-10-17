//! Keyed hashing for maps fed untrusted keys: SipHash with a secret per-map key.
//!
//! [`FastState`](crate::FastState) is fast but can be flooded: some collisions do not even depend
//! on its seed, so whoever chooses the keys can turn every lookup into a linear scan. SipHash is a
//! keyed PRF: without the key, colliding inputs cannot be found faster than by brute force.
//! [`KeyedState`] gives each map its own key, derived from a per-process secret.
//!
//! The secret comes from the CPU timestamp counter mixed with stack and static addresses (ASLR):
//! there is no OS RNG below the platform layer yet. That is unpredictable from outside the
//! process, not cryptographic randomness.

use core::hash::{BuildHasher, Hasher};
use core::sync::atomic::{AtomicU64, Ordering};
use core::{fmt, ptr};

use crate::folded_multiply;

/// SipHash with `C` compression and `D` finalization rounds, 64-bit output. Keyed by 128 bits.
///
/// Like std's `SipHasher`, integer writes use the host's byte order: this is an in-memory map
/// hasher, not a stable hash.
#[derive(Clone)]
pub struct SipHasher<const C: usize, const D: usize> {
    v0: u64,
    v1: u64,
    v2: u64,
    v3: u64,
    /// Up to 7 pending bytes, little-endian.
    tail: u64,
    ntail: usize,
    length: u64,
}

/// SipHash-1-3: the flooding-resistant map hasher (std's `HashMap` default).
pub type SipHasher13 = SipHasher<1, 3>;

/// SipHash-2-4: the original parameters, for formats and protocols that specify it.
pub type SipHasher24 = SipHasher<2, 4>;

impl<const C: usize, const D: usize> SipHasher<C, D> {
    #[inline]
    pub const fn new_with_keys(k0: u64, k1: u64) -> Self {
        Self {
            v0: k0 ^ 0x736f_6d65_7073_6575,
            v1: k1 ^ 0x646f_7261_6e64_6f6d,
            v2: k0 ^ 0x6c79_6765_6e65_7261,
            v3: k1 ^ 0x7465_6462_7974_6573,
            tail: 0,
            ntail: 0,
            length: 0,
        }
    }

    #[inline(always)]
    fn round(&mut self) {
        self.v0 = self.v0.wrapping_add(self.v1);
        self.v1 = self.v1.rotate_left(13) ^ self.v0;
        self.v0 = self.v0.rotate_left(32);
        self.v2 = self.v2.wrapping_add(self.v3);
        self.v3 = self.v3.rotate_left(16) ^ self.v2;
        self.v0 = self.v0.wrapping_add(self.v3);
        self.v3 = self.v3.rotate_left(21) ^ self.v0;
        self.v2 = self.v2.wrapping_add(self.v1);
        self.v1 = self.v1.rotate_left(17) ^ self.v2;
        self.v2 = self.v2.rotate_left(32);
    }

    #[inline(always)]
    fn compress(&mut self, m: u64) {
        self.v3 ^= m;
        for _ in 0..C {
            self.round();
        }
        self.v0 ^= m;
    }
}

/// Up to 8 bytes as a little-endian word, zero-extended.
#[inline(always)]
fn le_word(bytes: &[u8]) -> u64 {
    let mut word = [0; 8];
    word[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(word)
}

impl<const C: usize, const D: usize> Hasher for SipHasher<C, D> {
    fn write(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        if self.ntail > 0 {
            let n = (8 - self.ntail).min(bytes.len());
            self.tail |= le_word(&bytes[..n]) << (8 * self.ntail);
            self.ntail += n;
            bytes = &bytes[n..];
            if self.ntail < 8 {
                return;
            }
            let m = self.tail;
            self.compress(m);
            self.tail = 0;
            self.ntail = 0;
        }
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.compress(u64::from_le_bytes(*w));
        }
        self.tail = le_word(rest);
        self.ntail = rest.len();
    }

    fn finish(&self) -> u64 {
        let mut s = self.clone();
        let b = (self.length << 56) | self.tail;
        s.compress(b);
        s.v2 ^= 0xff;
        for _ in 0..D {
            s.round();
        }
        s.v0 ^ s.v1 ^ s.v2 ^ s.v3
    }
}

impl<const C: usize, const D: usize> fmt::Debug for SipHasher<C, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The state would leak the key.
        f.write_str("SipHasher { .. }")
    }
}

/// [`BuildHasher`] for hash maps whose keys an adversary may choose: network messages, mod and
/// user files, console input, anything loaded from outside the project. Each `new` state has its
/// own key, so learning one map's iteration order says nothing about another's.
///
/// Several rounds per 8 bytes, so slower than [`FastState`](crate::FastState); use `FastState` for
/// maps keyed by engine data (handles, asset ids, interned names), which is nearly all of them.
/// Iteration order differs from run to run.
#[derive(Clone, Copy)]
pub struct KeyedState {
    k0: u64,
    k1: u64,
}

impl KeyedState {
    /// A fresh key derived from the process secret.
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let secret = process_secret();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self {
            k0: folded_multiply(secret ^ n, 0x9e37_79b9_7f4a_7c15),
            k1: folded_multiply(secret.rotate_left(32) ^ n, 0xd6e8_feb8_6659_fd93),
        }
    }

    /// A fixed key, for reproducing a run. Defeats the purpose if the key is public.
    #[inline]
    pub const fn with_keys(k0: u64, k1: u64) -> Self {
        Self { k0, k1 }
    }
}

impl Default for KeyedState {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl BuildHasher for KeyedState {
    type Hasher = SipHasher13;

    #[inline]
    fn build_hasher(&self) -> SipHasher13 {
        SipHasher13::new_with_keys(self.k0, self.k1)
    }
}

impl fmt::Debug for KeyedState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyedState { .. }")
    }
}

/// The per-process secret, made on first use. Never 0 (0 means "not yet").
fn process_secret() -> u64 {
    static SECRET: AtomicU64 = AtomicU64::new(0);
    let secret = SECRET.load(Ordering::Relaxed);
    if secret != 0 {
        return secret;
    }
    let stack = 0u8;
    let t0 = timestamp();
    let a = ptr::from_ref(&stack).addr() as u64;
    let b = ptr::from_ref(&SECRET).addr() as u64;
    let t1 = timestamp();
    let mixed = folded_multiply(t0 ^ 0x243f_6a88_85a3_08d3, a ^ 0x1319_8a2e_0370_7344)
        ^ folded_multiply(t1 ^ 0xa409_3822_299f_31d0, b ^ 0x082e_fa98_ec4e_6c89);
    let fresh = mixed | 1;
    // Racing first users agree on whichever value landed first.
    match SECRET.compare_exchange(0, fresh, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => fresh,
        Err(winner) => winner,
    }
}

/// The CPU's timestamp counter (`rdtsc`, `cntvct_el0`), read with the instruction itself. This
/// crate cannot reach the platform layer: the derive crates use it on the build host, where no
/// platform is configured. The instructions need no OS support, like SIMD; other architectures
/// fall back to the address entropy alone.
#[inline]
fn timestamp() -> u64 {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: rdtsc exists on every x86-64 CPU and has no memory effects.
    let value = unsafe { core::arch::x86_64::_rdtsc() };
    #[cfg(target_arch = "aarch64")]
    let value = {
        let value: u64;
        // SAFETY: cntvct_el0 is readable at EL0 on every OS Marlin targets.
        unsafe {
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack));
        }
        value
    };
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let value = 0;
    value
}
