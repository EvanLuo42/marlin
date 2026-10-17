//! Deterministic test data. The interop fixtures were generated from these exact functions, so
//! they must not change. That is also why this keeps its own frozen xorshift64 instead of the
//! engine's `marlin_math::Rng`.
#![allow(dead_code)]

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

pub fn random(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    (0..n).map(|_| (r.next() >> 32) as u8).collect()
}

/// English-like text: words from a small vocabulary with skewed frequencies.
pub fn text(n: usize, seed: u64) -> Vec<u8> {
    const WORDS: [&str; 32] = [
        "the",
        "of",
        "and",
        "to",
        "in",
        "a",
        "is",
        "that",
        "for",
        "it",
        "as",
        "was",
        "with",
        "be",
        "by",
        "on",
        "not",
        "he",
        "this",
        "are",
        "or",
        "his",
        "from",
        "at",
        "which",
        "engine",
        "texture",
        "mesh",
        "shader",
        "package",
        "stream",
        "compression",
    ];
    let mut r = Rng::new(seed);
    let mut out = Vec::with_capacity(n + 16);
    while out.len() < n {
        let i = (r.below(32) * r.below(32) / 31) as usize;
        out.extend_from_slice(WORDS[i].as_bytes());
        match r.below(16) {
            0 => out.extend_from_slice(b".\n"),
            1 => out.extend_from_slice(b", "),
            _ => out.push(b' '),
        }
    }
    out.truncate(n);
    out
}

/// A short pattern repeated with rare mutations: long matches, small offsets.
pub fn repetitive(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let pattern: Vec<u8> = (0..37).map(|_| r.below(256) as u8).collect();
    let mut out: Vec<u8> = (0..n).map(|i| pattern[i % pattern.len()]).collect();
    for _ in 0..n / 1000 {
        let i = r.below(n as u64) as usize;
        out[i] = r.below(256) as u8;
    }
    out
}

/// Structured binary, like cooked mesh data: little-endian floats and indices.
pub fn binary(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = Vec::with_capacity(n + 16);
    let mut i = 0u32;
    while out.len() < n {
        let x = (i as f32 * 0.25).sin() * 100.0 + (r.below(4) as f32);
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&(i / 3 + (r.below(3) as u32)).to_le_bytes());
        i += 1;
    }
    out.truncate(n);
    out
}

/// Every shape, by name.
pub fn shapes(n: usize, seed: u64) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("zeros", vec![0; n]),
        ("random", random(n, seed)),
        ("text", text(n, seed)),
        ("repetitive", repetitive(n, seed)),
        ("binary", binary(n, seed)),
    ]
}
