//! SipHash and KeyedState.

use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};

use marlin_hash::{KeyedState, SipHasher13, SipHasher24};

#[test]
#[allow(deprecated)]
fn siphash24_matches_std_for_every_length_and_split() {
    // std's deprecated `SipHasher` is specified as SipHash-2-4.
    let data: Vec<u8> = (0..64u8).collect();
    let (k0, k1) = (0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908);
    for len in 0..=data.len() {
        let mut reference = std::hash::SipHasher::new_with_keys(k0, k1);
        reference.write(&data[..len]);
        let expected = reference.finish();
        for cut in 0..=len {
            let mut h = SipHasher24::new_with_keys(k0, k1);
            h.write(&data[..cut]);
            h.write(&data[cut..len]);
            assert_eq!(h.finish(), expected, "len {len} cut {cut}");
        }
    }
    // The first vectors of the SipHash paper's reference table (key 00..0f, input 00..).
    let paper = [
        0x726f_db47_dd0e_0e31,
        0x74f8_39c5_93dc_67fd,
        0x0d6c_8009_d9a9_4f5a,
    ];
    for (len, expected) in paper.into_iter().enumerate() {
        let mut h = SipHasher24::new_with_keys(k0, k1);
        h.write(&data[..len]);
        assert_eq!(h.finish(), expected, "len {len}");
    }
}

#[test]
fn siphash13_vectors() {
    // Cross-checked against std's `DefaultHasher` (SipHash-1-3 with zero keys) when written.
    let data: Vec<u8> = (0..64u8).collect();
    let vectors: [(usize, u64); 9] = [
        (0, 0xd1fb_a762_150c_532c),
        (1, 0x68a9_1412_8e01_e473),
        (7, 0x2f09_8ab0_c751_325a),
        (8, 0xead4_11e6_7ebe_2eea),
        (9, 0x7592_7f9d_9512_4362),
        (15, 0xf30e_b725_bb91_c9ea),
        (16, 0x8972_1884_33a5_c5b7),
        (63, 0x385d_3e39_e5f3_7359),
        (64, 0x75e0_5fd5_bbc8_70c6),
    ];
    for (len, expected) in vectors {
        for cut in 0..=len {
            let mut h = SipHasher13::new_with_keys(0, 0);
            h.write(&data[..cut]);
            h.write(&data[cut..len]);
            assert_eq!(h.finish(), expected, "len {len} cut {cut}");
        }
    }
}

#[test]
fn keyed_states_differ_and_work_in_maps() {
    let a = KeyedState::new();
    let b = KeyedState::new();
    assert_ne!(a.hash_one("marlin"), b.hash_one("marlin"), "per-state keys");
    assert_eq!(a.hash_one("marlin"), a.hash_one("marlin"));
    let fixed = KeyedState::with_keys(1, 2);
    assert_eq!(
        fixed.hash_one(42u64),
        KeyedState::with_keys(1, 2).hash_one(42u64)
    );
    assert_eq!(format!("{a:?}"), "KeyedState { .. }");

    let mut seen = HashSet::new();
    for i in 0..100_000u32 {
        assert!(seen.insert(a.hash_one(i)), "collision at {i}");
    }

    let mut map: std::collections::HashMap<String, u32, KeyedState> =
        std::collections::HashMap::with_hasher(KeyedState::new());
    for i in 0..1000 {
        map.insert(format!("key{i}"), i);
    }
    assert_eq!(map["key500"], 500);
}

#[test]
fn keyed_states_from_many_threads() {
    let hashes: Vec<u64> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| s.spawn(|| KeyedState::new().hash_one(7u64)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let unique: HashSet<_> = hashes.iter().collect();
    assert_eq!(unique.len(), hashes.len());
}
