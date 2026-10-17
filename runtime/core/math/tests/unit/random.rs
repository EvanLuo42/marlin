//! The engine RNG: reference outputs (the sequence must never change), ranges and distributions.

use marlin_math::Rng;
use marlin_math::scalar::abs_diff_eq;

#[test]
fn xoshiro256starstar_reference_outputs() {
    // From the reference implementation, state {1, 2, 3, 4}.
    let mut rng = Rng::from_state([1, 2, 3, 4]);
    let out: [u64; 4] = core::array::from_fn(|_| rng.next_u64());
    assert_eq!(out, [11520, 0, 1509978240, 1215971899390074240]);
}

#[test]
fn seeding_is_splitmix64() {
    // SplitMix64 from seed 0 starts 0xe220a8397b1dcdaf.
    assert_eq!(Rng::new(0).state()[0], 0xe220_a839_7b1d_cdaf);
}

#[test]
fn same_seed_same_sequence() {
    let (mut a, mut b) = (Rng::new(42), Rng::new(42));
    for _ in 0..1000 {
        assert_eq!(a.next_u64(), b.next_u64());
    }
    let mut c = Rng::new(43);
    assert_ne!(Rng::new(42).next_u64(), c.next_u64());
}

#[test]
fn state_round_trips() {
    let mut a = Rng::new(7);
    a.next_u64();
    let mut b = Rng::from_state(a.state());
    assert_eq!(a.next_u64(), b.next_u64());
}

#[test]
#[should_panic]
fn zero_state_is_rejected() {
    Rng::from_state([0; 4]);
}

#[test]
fn below_stays_in_range_and_covers_it() {
    let mut rng = Rng::new(1);
    let mut seen = [0u32; 7];
    for _ in 0..70_000 {
        seen[rng.below(7)] += 1;
    }
    // Each bucket expects 10 000; 5 sigma is about ±475.
    assert!(
        seen.iter().all(|&n| (9_500..10_500).contains(&n)),
        "{seen:?}"
    );
    assert_eq!(rng.below(1), 0);
    for n in [2u64, 3, 1 << 40, u64::MAX] {
        assert!(rng.below_u64(n) < n);
    }
}

#[test]
fn floats_stay_in_range() {
    let mut rng = Rng::new(2);
    let mut sum = 0.0f64;
    for _ in 0..100_000 {
        let x = rng.f32();
        assert!((0.0..1.0).contains(&x));
        sum += x as f64;
        let y = rng.range_f32(-3.0, 5.0);
        assert!((-3.0..5.0).contains(&y));
    }
    assert!((sum / 100_000.0 - 0.5).abs() < 0.01);
}

#[test]
fn chance_and_bool() {
    let mut rng = Rng::new(3);
    let hits = (0..100_000).filter(|_| rng.chance(0.25)).count();
    assert!((24_000..26_000).contains(&hits), "{hits}");
    let heads = (0..100_000).filter(|_| rng.bool()).count();
    assert!((49_000..51_000).contains(&heads), "{heads}");
    assert!(!(0..1000).any(|_| rng.chance(0.0)));
    assert!((0..1000).all(|_| rng.chance(1.0)));
}

#[test]
fn fill_bytes_is_little_endian_outputs() {
    let mut a = Rng::new(4);
    let mut b = a.clone();
    let mut bytes = [0u8; 21];
    a.fill_bytes(&mut bytes);
    let mut expected = Vec::new();
    for _ in 0..3 {
        expected.extend(b.next_u64().to_le_bytes());
    }
    assert_eq!(bytes[..], expected[..21]);
}

#[test]
fn shuffle_permutes_and_choose_picks() {
    let mut rng = Rng::new(5);
    let mut v: Vec<u32> = (0..100).collect();
    rng.shuffle(&mut v);
    assert_ne!(v, (0..100).collect::<Vec<_>>());
    let mut sorted = v.clone();
    sorted.sort();
    assert_eq!(sorted, (0..100).collect::<Vec<_>>());
    assert_eq!(rng.choose::<u32>(&[]), None);
    assert!(v.contains(rng.choose(&v).unwrap()));
}

#[test]
fn unit_vectors_and_rotations_are_unit_and_spread() {
    let mut rng = Rng::new(6);
    let mut mean = [0.0f32; 3];
    for _ in 0..20_000 {
        let v = rng.unit_vec3();
        assert!(abs_diff_eq(v.length(), 1.0, 1e-5));
        mean = [mean[0] + v.x, mean[1] + v.y, mean[2] + v.z];
        assert!(rng.rotation().is_normalized());
    }
    assert!(mean.iter().all(|m| (m / 20_000.0).abs() < 0.02), "{mean:?}");
}

#[test]
fn jump_gives_a_different_stream() {
    let a = Rng::new(8);
    let mut b = a.clone();
    b.jump();
    let mut c = a.clone();
    c.jump();
    assert_eq!(b, c, "jumping is deterministic");
    let mut a = a;
    assert_ne!(a.next_u64(), b.next_u64());
}

#[test]
fn seeded_and_jumped_sequences_are_pinned() {
    // Our own seeding and jump: replays and saved games depend on these never changing.
    let mut rng = Rng::new(42);
    let out: [u64; 3] = core::array::from_fn(|_| rng.next_u64());
    assert_eq!(
        out,
        [
            0x1578_0b2e_0c2e_c716,
            0x6104_d986_6d11_3a7e,
            0xae17_5332_39e4_99a1
        ]
    );
    let mut rng = Rng::from_state([1, 2, 3, 4]);
    rng.jump();
    assert_eq!(
        rng.state(),
        [
            0x8c7a_1539_56b5_f3d1,
            0x701f_1a71_3401_d85e,
            0x6527_f66a_6546_9085,
            0x8386_b786_c440_8050
        ]
    );
}
