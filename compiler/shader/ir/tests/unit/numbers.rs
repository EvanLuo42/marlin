use marlin_math::Rng;
use marlin_shaderc_ir::num::{f16_to_f32, f32_to_f16};
use marlin_test_support::{scale, seed};

#[test]
fn every_half_survives_a_trip_through_f32() {
    for h in 0..=u16::MAX {
        let x = f16_to_f32(h);
        if x.is_nan() {
            assert!(f16_to_f32(f32_to_f16(x)).is_nan());
            continue;
        }
        assert_eq!(f32_to_f16(x), h, "{h:#06x} = {x:?}");
    }
}

/// The halves next to `h` on the number line (by value), for checking rounding.
fn neighbours(h: u16) -> [u16; 2] {
    let sign = h & 0x8000;
    let mag = h & 0x7fff;
    let up = sign | (mag + 1).min(0x7c00);
    let down = if mag == 0 {
        0x8001 ^ sign
    } else {
        sign | (mag - 1)
    };
    [up, down]
}

#[test]
fn floats_round_to_the_nearest_half_ties_to_even() {
    let seed = seed(0x0f16_0001);
    let mut rng = Rng::new(seed);
    for _ in 0..scale(200_000) {
        // Values around the half range: magnitudes from 2^-26 to 2^17.
        let x = f32::from_bits(rng.next_u32() & 0x8fff_ffff | 0x3000_0000);
        let x = if rng.bool() { x } else { x * 2f32.powi(-12) };
        let h = f32_to_f16(x);
        let got = f16_to_f32(h) as f64;
        let err = (got - x as f64).abs();
        for n in neighbours(h) {
            let other = f16_to_f32(n) as f64;
            if other.is_infinite() {
                continue;
            }
            let other_err = (other - x as f64).abs();
            assert!(
                err < other_err || (err == other_err && h & 1 == 0) || got.is_infinite(),
                "seed {seed}: {x:?} gave {got:?}, but {other:?} is nearer"
            );
        }
    }
}

#[test]
fn half_edge_cases() {
    assert_eq!(f32_to_f16(65504.0), 0x7bff);
    assert_eq!(f32_to_f16(65520.0), 0x7c00, "rounds up to infinity");
    assert_eq!(f32_to_f16(65519.0), 0x7bff);
    assert_eq!(f32_to_f16(f32::INFINITY), 0x7c00);
    assert_eq!(f32_to_f16(-0.0), 0x8000);
    assert_eq!(f32_to_f16(2f32.powi(-24)), 0x0001, "smallest subnormal");
    assert_eq!(
        f32_to_f16(2f32.powi(-25)),
        0x0000,
        "half of it ties to even zero"
    );
    assert_eq!(f32_to_f16(1.5 * 2f32.powi(-25)), 0x0001);
    assert_eq!(
        f32_to_f16(2f32.powi(-14) * (1.0 - 2f32.powi(-12))),
        0x0400,
        "carry into normal"
    );
    assert!(f16_to_f32(0x7e00).is_nan());
}
