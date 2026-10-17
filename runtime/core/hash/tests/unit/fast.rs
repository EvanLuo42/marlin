//! FastHasher / PrehashedHasher behaviour: determinism, collisions and bit spread.

use std::collections::HashSet;
use std::hash::{BuildHasher, Hash};

use marlin_hash::{FastState, PrehashedState, fast_hash};

fn h<T: Hash + ?Sized>(v: &T) -> u64 {
    fast_hash(v)
}

#[test]
fn deterministic_and_seed_dependent() {
    assert_eq!(h("marlin"), h("marlin"));
    assert_eq!(h(&42u64), h(&42u64));
    let a = FastState::with_seed(1).hash_one("marlin");
    let b = FastState::with_seed(2).hash_one("marlin");
    assert_ne!(a, b);
    assert_ne!(FastState::with_seed(0).hash_one(0u64), 0);
}

#[test]
fn distinguishes_lengths_and_order() {
    assert_ne!(h(""), h("\0"));
    assert_ne!(h(&[0u8; 0][..]), h(&[0u8; 1][..]));
    assert_ne!(h(&(1u32, 2u32)), h(&(2u32, 1u32)));
    assert_ne!(h(&("ab", "c")), h(&("a", "bc")));
    assert_ne!(h(&(1u64, 2u64, 3u64)), h(&(1u64, 3u64, 2u64)));
}

#[test]
fn no_collisions_on_sequential_integers() {
    let mut seen = HashSet::new();
    for i in 0..200_000u32 {
        assert!(seen.insert(h(&i)), "collision at {i}");
    }
}

#[test]
fn no_collisions_on_every_byte_length() {
    // Covers every branch of hash_bytes, including the 32-byte loop and its overlapping tail.
    let data: Vec<u8> = (0..1024u32).map(|i| (i * 7 + 3) as u8).collect();
    let mut seen = HashSet::new();
    for len in 0..=data.len() {
        assert!(seen.insert(h(&data[..len])), "collision at len {len}");
    }
    // Flipping any single bit changes the hash.
    for len in [1usize, 3, 4, 7, 8, 15, 16, 17, 31, 32, 33, 64, 100] {
        let base = h(&data[..len]);
        for bit in 0..len * 8 {
            let mut v = data[..len].to_vec();
            v[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(h(&v[..]), base, "len {len} bit {bit}");
        }
    }
}

#[test]
fn spreads_into_high_and_low_bits() {
    // hashbrown takes the bucket from the low bits and the tag from the top 7.
    let mut low = [0u32; 256];
    let mut high = [0u32; 128];
    for i in 0..65_536u32 {
        let v = h(&i);
        low[(v & 0xff) as usize] += 1;
        high[(v >> 57) as usize] += 1;
    }
    assert!(low.iter().all(|&c| (128..=384).contains(&c)), "{low:?}");
    assert!(high.iter().all(|&c| (256..=768).contains(&c)), "{high:?}");
}

#[test]
fn prehashed_is_identity_for_u64() {
    let s = PrehashedState;
    assert_eq!(s.hash_one(0x1234_5678_9abc_def0u64), 0x1234_5678_9abc_def0);
    assert_ne!(s.hash_one((1u64, 2u64)), s.hash_one((2u64, 1u64)));
    assert_ne!(s.hash_one("a"), s.hash_one("b"));
}
