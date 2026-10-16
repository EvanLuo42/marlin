//! Accuracy of the in-house transcendental functions against std's libm (via f64).

use marlin_math::scalar::*;

/// Distance in units in the last place, with ±0 equal and NaN equal to NaN.
fn ulps(a: f32, b: f32) -> u32 {
    if a.is_nan() && b.is_nan() || a == b {
        return 0;
    }
    if a.is_nan() || b.is_nan() || a.is_sign_negative() != b.is_sign_negative() {
        return u32::MAX;
    }
    a.to_bits().abs_diff(b.to_bits())
}

fn sweep(lo: f32, hi: f32, n: usize) -> impl Iterator<Item = f32> {
    (0..=n).map(move |i| lo + (hi - lo) * (i as f32 / n as f32))
}

fn check(
    name: &str,
    ours: impl Fn(f32) -> f32,
    reference: impl Fn(f64) -> f64,
    xs: impl Iterator<Item = f32>,
    max_ulps: u32,
) {
    let mut worst = (0, 0.0);
    for x in xs {
        let expected = reference(x as f64) as f32;
        let got = ours(x);
        // Ulps of results near zero are meaningless; accept a tiny absolute error there.
        let err = if expected.abs() < 1e-6 && (got - expected).abs() < 1e-12 {
            0
        } else {
            ulps(got, expected)
        };
        if err > worst.0 {
            worst = (err, x);
        }
    }
    assert!(
        worst.0 <= max_ulps,
        "{name}: {} ulps at x = {}",
        worst.0,
        worst.1
    );
}

#[test]
fn trig_matches_libm() {
    check("sin", sin, f64::sin, sweep(-1000.0, 1000.0, 400_000), 1);
    check("cos", cos, f64::cos, sweep(-1000.0, 1000.0, 400_000), 1);
    check("tan", tan, f64::tan, sweep(-1.5, 1.5, 100_000), 2);
    check("atan", atan, f64::atan, sweep(-100.0, 100.0, 200_000), 1);
    check("asin", asin, f64::asin, sweep(-1.0, 1.0, 100_000), 1);
    check("acos", acos, f64::acos, sweep(-1.0, 1.0, 100_000), 1);
    for x in sweep(-10.0, 10.0, 10_000) {
        assert_eq!(sin_cos(x), (sin(x), cos(x)));
    }
}

#[test]
fn exp_log_match_libm() {
    check("exp", exp, f64::exp, sweep(-87.0, 88.0, 200_000), 1);
    check("exp2", exp2, f64::exp2, sweep(-126.0, 127.0, 200_000), 1);
    let ln_inputs = sweep(1e-30, 1e30, 200_000).chain(sweep(0.01, 10.0, 100_000));
    check("ln", ln, f64::ln, ln_inputs, 1);
    check("log2", log2, f64::log2, sweep(0.001, 1e6, 200_000), 1);
    check("log10", log10, f64::log10, sweep(0.001, 1e6, 200_000), 1);
    let y = 2.7f32;
    check(
        "pow",
        |x| powf(x, y),
        |x| x.powf(y as f64),
        sweep(0.0, 100.0, 100_000),
        2,
    );
    check(
        "pow odd",
        |x| powf(x, 3.0),
        |x| x.powf(3.0),
        sweep(-50.0, 50.0, 100_000),
        2,
    );
}

#[test]
fn atan2_quadrants_and_specials() {
    for (y, x) in [
        (1.0, 1.0),
        (1.0, -1.0),
        (-1.0, -1.0),
        (-1.0, 1.0),
        (0.3, -7.0),
        (5.0, 0.001),
    ] {
        assert!(
            ulps(atan2(y, x), (y as f64).atan2(x as f64) as f32) <= 1,
            "({y}, {x})"
        );
    }
    let inf = f32::INFINITY;
    let specials = [
        (0.0, 0.0),
        (-0.0, 0.0),
        (0.0, -0.0),
        (-0.0, -0.0),
        (1.0, 0.0),
        (inf, inf),
        (-inf, -inf),
        (1.0, -inf),
        (-1.0, inf),
    ];
    for (y, x) in specials {
        let expected = (y as f64).atan2(x as f64) as f32;
        assert_eq!(atan2(y, x).to_bits(), expected.to_bits(), "({y}, {x})");
    }
}

#[test]
fn special_values() {
    assert!(sin(f32::INFINITY).is_nan() && cos(f32::NAN).is_nan());
    assert_eq!(exp(-200.0), 0.0);
    assert_eq!(exp(200.0), f32::INFINITY);
    assert_eq!(ln(0.0), f32::NEG_INFINITY);
    assert!(ln(-1.0).is_nan() && asin(1.5).is_nan() && acos(-1.01).is_nan());
    assert_eq!(powf(-2.0, 3.0), -8.0);
    assert!(powf(-2.0, 0.5).is_nan());
    assert_eq!(powf(0.0, -1.0), f32::INFINITY);
    assert_eq!(powf(-0.0, -3.0), f32::NEG_INFINITY);
    assert_eq!(acos(1.0), 0.0);
    assert_eq!(asin(-0.0).to_bits(), (-0.0f32).to_bits());
}

#[test]
fn interpolation_helpers() {
    assert_eq!(lerp(2.0, 4.0, 0.5), 3.0);
    assert_eq!(inverse_lerp(2.0, 4.0, 3.0), 0.5);
    assert_eq!(remap(5.0, 0.0, 10.0, 100.0, 200.0), 150.0);
    assert_eq!(saturate(1.5), 1.0);
    assert_eq!(smoothstep(0.0, 1.0, 0.5), 0.5);
    assert_eq!(fract(-1.25), 0.75);
}
