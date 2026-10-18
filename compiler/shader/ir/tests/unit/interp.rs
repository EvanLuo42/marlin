use marlin_math::Rng;
use marlin_shaderc_ir::num::{f16_to_f32, f32_to_f16};
use marlin_shaderc_ir::{Interpreter, Module, Resources, Val, math, parse_module, verify_module};
use marlin_test_support::{data_path, scale, seed};

fn module(name: &str) -> Module {
    let text = std::fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), name)).unwrap();
    let m = parse_module(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    let errors = verify_module(&m);
    assert!(errors.is_empty(), "{name}: {errors:?}");
    m
}

fn call(m: &Module, name: &str, args: &[Val]) -> Vec<Val> {
    let f = m.find_function(name).unwrap();
    Interpreter::new(m)
        .call(f, args)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn ints(vals: &[Val]) -> Vec<i64> {
    vals.iter().map(|v| v.as_i32().unwrap() as i64).collect()
}

#[test]
fn integer_operations_wrap_and_divide_by_zero_to_zero() {
    let m = module("interp/ctfe.ir");
    let cases: [((i32, i32), [i64; 7]); 3] = [
        ((-7, 2), [-5, -3, -1, 2147483644, -28, -2, 1]),
        (
            (i32::MIN, -1),
            [2147483647, i32::MIN as i64, 0, 0, 0, -1, 2147483647],
        ),
        ((5, 0), [5, 0, 0, 0, 5, 5, 0]),
    ];
    for ((a, b), want) in cases {
        let got = ints(&call(&m, "Int", &[Val::i32(a), Val::i32(b)]));
        assert_eq!(got, want, "Int({a}, {b})");
    }
}

#[test]
fn float_to_int_saturates_and_nan_is_zero() {
    let m = module("interp/ctfe.ir");
    let run = |x: f32| {
        let r = call(&m, "Convert", &[Val::f32(x)]);
        let ints: Vec<i64> = vec![
            r[0].as_i32().unwrap() as i64,
            r[1].as_i32().unwrap() as i64,
            r[2].bits().unwrap() as u16 as i16 as i64,
            r[3].bits().unwrap() as i64,
        ];
        (ints, r[4].bits().unwrap() as u16)
    };
    assert_eq!(
        run(1e10),
        (vec![i32::MAX as i64, -1, 32767, i32::MAX as i64], 0x7c00)
    );
    let (nan_ints, nan_half) = run(f32::NAN);
    assert_eq!(nan_ints, vec![0, 0, 0, 0]);
    assert!(f16_to_f32(nan_half).is_nan());
    assert_eq!(run(-3.7), (vec![-3, 0, -3, -3], f32_to_f16(-3.7)));
}

#[test]
fn half_arithmetic_rounds_once_ties_to_even() {
    let m = module("interp/ctfe.ir");
    // 1 + 2^-11 lies halfway between 1 and the next half: ties to even gives 1.
    let r = call(&m, "Half", &[Val::Bits(0x3c00), Val::Bits(0x1000)]);
    assert_eq!(r, vec![Val::Bits(0x3c00), Val::Bits(0x1000)]);
}

#[test]
fn structured_control_flow_and_calls() {
    let m = module("interp/ctfe.ir");
    assert_eq!(ints(&call(&m, "Loop", &[Val::i32(10)])), vec![27]);
    assert_eq!(
        ints(&call(&m, "Loop", &[Val::i32(100)])),
        vec![61],
        "breaks out of the loop"
    );
    for (k, want) in [(0, 20), (1, 40), (-1, 60), (7, 198)] {
        assert_eq!(
            ints(&call(&m, "Pick", &[Val::i32(k)])),
            vec![want],
            "Pick({k})"
        );
    }
}

#[test]
fn vectors_matrices_and_memory() {
    let m = module("interp/ctfe.ir");
    let v = |xs: [f32; 3]| Val::Agg(xs.iter().map(|&x| Val::f32(x)).collect());
    let mat = Val::Agg(vec![
        v([1.0, 0.0, 0.0]),
        v([0.0, 2.0, 0.0]),
        v([0.0, 0.0, 3.0]),
    ]);
    let r = call(&m, "Vectors", &[v([1.0, 2.0, 3.0]), mat]);
    assert_eq!(r[0], Val::f32(14.0));
    assert_eq!(r[1], v([-7.0, 5.0, -1.0]));
    assert_eq!(r[2], v([1.0, 4.0, 9.0]));
    assert_eq!(r[3], Val::f32(14f32.sqrt()));

    assert_eq!(
        call(&m, "Memory", &[Val::i32(1)]),
        vec![Val::f32(2.0), Val::f32(3.0)]
    );
    // Out of range: the read gives zero and the write is dropped.
    assert_eq!(
        call(&m, "Memory", &[Val::i32(7)]),
        vec![Val::f32(0.0), Val::f32(3.0)]
    );
}

#[test]
fn a_loop_that_never_ends_hits_the_step_limit() {
    let m = module("interp/ctfe.ir");
    let mut it = Interpreter::new(&m);
    it.options.step_limit = 10_000;
    let err = it.call(m.find_function("Spin").unwrap(), &[]).unwrap_err();
    assert!(err.to_string().contains("step limit"), "{err}");
}

fn read_i32s(bytes: &[u8]) -> Vec<i32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| i32::from_le_bytes(*c))
        .collect()
}

#[test]
fn a_dispatch_uses_groupshared_waves_atomics_and_reconverges() {
    let m = module("interp/compute.ir");
    let entry = m.find_function("Game.Work").unwrap();
    for wave in [4u32, 16, 64] {
        let mut it = Interpreter::new(&m);
        it.options.wave_size = wave;
        let w = wave as i32;
        let mut res = Resources {
            buffers: vec![
                vec![0; 128 * 4],
                vec![0; (128 / wave as usize) * 4],
                vec![0; 4],
                vec![0; 128 * 4],
            ],
            push: Some(Val::Agg((0..4).map(Val::u32).collect())),
        };
        let out = it.dispatch(entry, [2, 1, 1], &mut res).unwrap();
        assert_eq!(out.out_of_bounds, 0);

        let mirror = read_i32s(&res.buffers[0]);
        let want: Vec<i32> = (0..128).map(|x| (x / 64) * 64 + 63 - x % 64).collect();
        assert_eq!(
            mirror, want,
            "wave {wave}: the barrier makes every lane's store visible"
        );

        let sums = read_i32s(&res.buffers[1]);
        let want: Vec<i32> = (0..128 / w)
            .map(|k| (0..w).map(|j| k * w + j).sum())
            .collect();
        assert_eq!(sums, want, "wave {wave}: one sum per wave");

        assert_eq!(read_i32s(&res.buffers[2]), vec![(0..128).sum::<i32>()]);

        let iters = read_i32s(&res.buffers[3]);
        let all = w / 4 * 6; // lanes loop 0, 1, 2, 3 times
        let evens = w / 4 * 2; // even lanes loop 0 or 2 times
        for (x, &got) in iters.iter().enumerate() {
            let x = x as i32;
            let want = if x % 2 == 1 {
                0
            } else if x % w == 0 {
                evens
            } else {
                all
            };
            assert_eq!(got, want, "wave {wave}, lane {x}");
        }

        // The same run again gives the same bytes.
        let before = res.buffers.clone();
        for b in &mut res.buffers {
            b.fill(0);
        }
        it.dispatch(entry, [2, 1, 1], &mut res).unwrap();
        assert_eq!(res.buffers, before, "wave {wave}: runs are deterministic");
    }
}

/// `ours` is within `ulps` units in the last place of `reference`.
fn close(ours: f64, reference: f64, ulps: f64) -> bool {
    if reference.is_nan() {
        return ours.is_nan();
    }
    if reference.is_infinite() || reference == 0.0 {
        return ours == reference || (ours - reference).abs() < 1e-300;
    }
    let ulp = f64::EPSILON * reference.abs().max(f64::MIN_POSITIVE);
    (ours - reference).abs() <= ulps * ulp
}

#[test]
fn deterministic_math_matches_the_host_closely() {
    let seed = seed(0x0e_0001);
    let mut rng = Rng::new(seed);
    let mut unit = || rng.next_u32() as f64 / u32::MAX as f64;
    type F = fn(f64) -> f64;
    let unary: [(&str, F, F, f64, f64); 12] = [
        ("exp", math::exp, f64::exp, -700.0, 700.0),
        ("exp2", math::exp2, f64::exp2, -1000.0, 1000.0),
        ("ln", math::ln, f64::ln, 1e-300, 1e300),
        ("log2", math::log2, f64::log2, 1e-300, 1e300),
        ("sin", math::sin, f64::sin, -100.0, 100.0),
        ("cos", math::cos, f64::cos, -100.0, 100.0),
        ("tan", math::tan, f64::tan, -1.5, 1.5),
        ("atan", math::atan, f64::atan, -1e6, 1e6),
        ("asin", math::asin, f64::asin, -1.0, 1.0),
        ("sinh", math::sinh, f64::sinh, -700.0, 700.0),
        ("cosh", math::cosh, f64::cosh, -700.0, 700.0),
        ("tanh", math::tanh, f64::tanh, -30.0, 30.0),
    ];
    for (name, ours, host, lo, hi) in unary {
        let log_scale = name == "ln" || name == "log2" || name == "atan";
        for _ in 0..scale(2000) {
            let t = unit();
            let x = if log_scale {
                // Spread over magnitudes; atan also over signs.
                let mag = (lo.abs().max(1e-300).ln() * (1.0 - t) + hi.abs().ln() * t).exp();
                if name == "atan" && unit() < 0.5 {
                    -mag
                } else {
                    mag
                }
            } else {
                lo + (hi - lo) * t
            };
            let (a, b) = (ours(x), host(x));
            // The trig functions lose accuracy near their zeros; compare absolutely there.
            let ok = close(a, b, 64.0) || (a - b).abs() < 1e-14;
            assert!(ok, "seed {seed}: {name}({x:e}) = {a:e}, host says {b:e}");
        }
    }
    for _ in 0..scale(2000) {
        let (x, y) = ((unit() - 0.5) * 1e6, (unit() - 0.5) * 1e3);
        assert_eq!(
            math::fmod(x, y).to_bits(),
            (x % y).to_bits(),
            "fmod({x}, {y})"
        );
        let a = math::atan2(y, x);
        assert!(close(a, y.atan2(x), 64.0), "atan2({y}, {x}) = {a}");
        let (b, e) = (unit() * 10.0, (unit() - 0.5) * 20.0);
        assert!(close(math::pow(b, e), b.powf(e), 4096.0), "pow({b}, {e})");
    }
    assert!(math::pow(-2.0, 3.0) == -8.0 && math::pow(-2.0, 0.5).is_nan());
    assert_eq!(math::fmod(-5.5, 2.0), -1.5);
}
