use marlin_shaderc_ir::Val;
use marlin_test_support::data_path;

use crate::support::{World, world};

fn calls() -> World {
    let text = std::fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), "Calls.mshader"))
        .unwrap()
        .replace("\r\n", "\n");
    world(&text)
}

fn f(v: &[Val]) -> f32 {
    v[0].as_f32().unwrap()
}

fn fs(v: &[Val]) -> Vec<f32> {
    match &v[0] {
        Val::Agg(items) => items.iter().map(|x| x.as_f32().unwrap()).collect(),
        other => panic!("not a vector: {other:?}"),
    }
}

fn int(v: &[Val]) -> i32 {
    v[0].bits().unwrap() as u32 as i32
}

fn uint(v: &[Val]) -> u32 {
    v[0].bits().unwrap() as u32
}

fn v3(x: f32, y: f32, z: f32) -> Val {
    Val::Agg(vec![Val::f32(x), Val::f32(y), Val::f32(z)])
}

fn close(a: f32, b: f32, what: &str) {
    assert!(
        (a - b).abs() <= 1e-4 * b.abs().max(1.0),
        "{what}: {a} is not {b}"
    );
}

#[test]
fn the_core_library_type_checks() {
    let w = world("namespace Game;\n");
    assert_eq!(w.diagnostics(), "");
    assert_eq!(
        calls().diagnostics(),
        "",
        "calls into the library type-check"
    );
}

#[test]
fn number_functions_pick_their_operation_by_kind() {
    let w = calls();
    assert_eq!(f(&w.run("Game.ClampF", &[Val::f32(2.5)])), 1.0);
    assert_eq!(f(&w.run("Game.ClampF", &[Val::f32(-0.5)])), -0.5);
    assert_eq!(int(&w.run("Game.ClampI", &[Val::i32(-7)])), -3);
    assert_eq!(int(&w.run("Game.MinI", &[Val::i32(-2), Val::i32(1)])), -2);
    // `umax`: as unsigned, 0xFFFF_FFFF is the larger.
    assert_eq!(
        uint(&w.run("Game.MaxU", &[Val::u32(u32::MAX), Val::u32(1)])),
        u32::MAX
    );
    assert_eq!(
        f(&w.run("Game.MinF", &[Val::f32(0.5), Val::f32(-1.0)])),
        -1.0
    );
    assert_eq!(f(&w.run("Game.AbsF", &[Val::f32(-3.0)])), 3.0);
    assert_eq!(int(&w.run("Game.AbsI", &[Val::i32(-3)])), 3);
    assert_eq!(f(&w.run("Game.SignF", &[Val::f32(-0.25)])), -1.0);
    assert_eq!(int(&w.run("Game.SignI", &[Val::i32(9)])), 1);
}

#[test]
fn float_functions_match_rust() {
    let w = calls();
    assert_eq!(f(&w.run("Game.Sat", &[Val::f32(1.5)])), 1.0);
    assert_eq!(
        fs(&w.run("Game.Sat3", &[v3(-1.0, 0.25, 9.0)])),
        [0.0, 0.25, 1.0]
    );
    assert_eq!(
        f(&w.run("Game.Mix", &[Val::f32(2.0), Val::f32(4.0), Val::f32(0.25)])),
        2.5
    );
    assert_eq!(
        fs(&w.run(
            "Game.Mix3",
            &[v3(0.0, 0.0, 0.0), v3(2.0, 4.0, 8.0), v3(0.5, 0.5, 0.25)]
        )),
        [1.0, 2.0, 2.0]
    );
    // Floor -2, Ceil -1, Trunc -1, Round (halves to even) -2.
    assert_eq!(
        f(&w.run("Game.Rounding", &[Val::f32(-1.5)])),
        -2000.0 - 100.0 - 10.0 - 2.0
    );
    close(f(&w.run("Game.Fraction", &[Val::f32(-1.25)])), 0.75, "Frac");
    close(
        f(&w.run("Game.Roots", &[Val::f32(4.0)])),
        2.0 + 5.0 + 25.0,
        "roots",
    );
    assert_eq!(
        f(&w.run("Game.Fused", &[Val::f32(2.0), Val::f32(3.0), Val::f32(1.0)])),
        7.0
    );
    let x = 1.5f32;
    close(
        f(&w.run("Game.Powers", &[Val::f32(x)])),
        x.powf(2.5) + x.exp() + x.exp2() + x.ln() + x.log2(),
        "powers",
    );
    close(
        f(&w.run("Game.Trig", &[Val::f32(x)])),
        x.sin() + x.cos() + x.tan() + (x / 4.0).asin() + (x / 4.0).acos() + x.atan(),
        "trig",
    );
    close(
        f(&w.run("Game.Angle", &[Val::f32(1.0), Val::f32(-1.0)])),
        1.0f32.atan2(-1.0),
        "Atan2",
    );
    close(
        f(&w.run("Game.Hyperbolic", &[Val::f32(x)])),
        x.sinh() + x.cosh() + x.tanh(),
        "hyperbolic",
    );
    // Smoothstep(0, 2, 1.5): t = 0.75, so 0.75² (3 - 1.5) = 0.84375; Step(1, 1.5) = 1.
    close(
        f(&w.run("Game.Steps", &[Val::f32(1.5)])),
        0.84375 + 10.0,
        "steps",
    );
    assert_eq!(uint(&w.run("Game.Special", &[Val::f32(f32::INFINITY)])), 1);
    assert_eq!(uint(&w.run("Game.Special", &[Val::f32(3.0)])), 0);
    assert_eq!(
        w.run("Game.Special3", &[v3(f32::NAN, 1.0, f32::NAN)])[0],
        Val::Agg(vec![Val::Bits(1), Val::Bits(0), Val::Bits(1)])
    );
}

#[test]
fn enum_operators_compare_and_combine_flags() {
    let w = calls();
    assert_eq!(uint(&w.run("Game.IsTriangle", &[Val::bool(true)])), 1);
    assert_eq!(uint(&w.run("Game.IsTriangle", &[Val::bool(false)])), 0);
    // CullBackFace | Opaque has the back-face bit, and is not Opaque alone.
    assert_eq!(uint(&w.run("Game.FlagOps", &[Val::bool(true)])), 1);
    assert_eq!(uint(&w.run("Game.FlagOps", &[Val::bool(false)])), 2);
}

#[test]
fn vector_functions_match_rust() {
    let w = calls();
    assert_eq!(
        f(&w.run("Game.DotF", &[v3(1.0, 2.0, 3.0), v3(4.0, 5.0, 6.0)])),
        32.0
    );
    assert_eq!(
        fs(&w.run("Game.CrossF", &[v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0)])),
        [0.0, 0.0, 1.0]
    );
    // |(3, 4, 0)| = 5; its distance to (1, 0, 0) is the square root of 20.
    close(
        f(&w.run("Game.Len", &[v3(3.0, 4.0, 0.0)])),
        5.0 + 20f32.sqrt() * 10.0,
        "Length",
    );
    let u = fs(&w.run("Game.Unit", &[v3(3.0, 0.0, 4.0)]));
    close(u[0], 0.6, "Normalize x");
    close(u[2], 0.8, "Normalize z");
    assert_eq!(
        fs(&w.run("Game.Mirror", &[v3(1.0, -1.0, 0.0), v3(0.0, 1.0, 0.0)])),
        [1.0, 1.0, 0.0]
    );
    assert_eq!(uint(&w.run("Game.Positive", &[v3(1.0, 2.0, 3.0)])), 1);
    assert_eq!(uint(&w.run("Game.Positive", &[v3(1.0, -2.0, 3.0)])), 0);
    assert_eq!(uint(&w.run("Game.AnyPositive", &[v3(-1.0, -2.0, 3.0)])), 1);
    assert_eq!(
        fs(&w.run("Game.Larger", &[v3(1.0, 5.0, 3.0), v3(2.0, 4.0, 3.0)])),
        [2.0, 5.0, 3.0]
    );
    assert_eq!(
        f(&w.run("Game.Pick", &[Val::Bits(0), Val::f32(1.0), Val::f32(2.0)])),
        2.0
    );
}

#[test]
fn integer_functions_match_rust() {
    let w = calls();
    let x = 0b1011_0000u32;
    assert_eq!(
        uint(&w.run("Game.Bits", &[Val::u32(x)])),
        x.count_ones() * 1000 + (31 - x.leading_zeros()) * 10 + x.trailing_zeros()
    );
    assert_eq!(
        uint(&w.run("Game.Reversed", &[Val::u32(x)])),
        x.reverse_bits()
    );
    // For a negative value, the highest clear bit: -8 is ...1111_1000, so bit 2.
    assert_eq!(uint(&w.run("Game.HighSigned", &[Val::i32(-8)])), 2);
}

#[test]
fn generic_code_uses_zero_and_one() {
    let w = calls();
    assert_eq!(f(&w.run("Game.TwiceF", &[Val::f32(1.5)])), 3.0);
    assert_eq!(
        fs(&w.run("Game.TwiceV", &[v3(1.0, 2.0, 3.0)])),
        [2.0, 4.0, 6.0]
    );
    assert_eq!(
        w.run("Game.Units", &[])[0],
        Val::Agg(vec![Val::u32(1), Val::u32(1), Val::u32(1)])
    );
}

#[test]
fn type_arguments_must_implement_the_bounds() {
    let w = world(
        "namespace Game;\n\
         public int Root(int x) => Sqrt(x);\n\
         public uint3 Roots(uint3 v) => Sqrt(v);\n\
         public float Bits(float x) => CountBits(x);\n\
         public bool Least(bool a, bool b) => Min(a, b);\n",
    );
    let d = w.diagnostics();
    for message in [
        "`int` does not implement `IFloat<int>`, which `Sqrt` requires of `T`",
        "`uint3` does not implement `IFloat<uint3>`, which `Sqrt` requires of `T`",
        "`float` does not implement `IInteger<float>`, which `CountBits` requires of `T`",
        "`bool` does not implement `IArithmetic<bool>`, which `Min` requires of `T`",
    ] {
        assert!(d.contains(message), "`{message}` is missing from:\n{d}");
    }
    assert_eq!(d.matches("error[").count(), 4, "one error per call:\n{d}");
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

fn bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

#[test]
fn resources_barriers_atomics_and_waves_run() {
    let text = std::fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), "Gpu.mshader"))
        .unwrap()
        .replace("\r\n", "\n");
    let w = world(&text);
    assert_eq!(w.diagnostics(), "");
    let input: Vec<u32> = (0..64).collect();
    let mut res = marlin_shaderc_ir::Resources {
        buffers: vec![
            vec![0; 64 * 4],
            bytes(&input),
            bytes(&input),
            bytes(&[3, 1]),
            bytes(&[0, 0, u32::MAX, 0, 0]),
        ],
        // Args { Out = 0, In = 1, Bytes = 2, P = 3, Counters = 4 }.
        push: Some(Val::Agg((0..5).map(Val::u32).collect())),
    };
    let prints = w.dispatch("Game.Kernel", [1, 1, 1], &mut res);
    let mirrored: Vec<u32> = (0..64u32).map(|x| (63 - x) * 3 + 1).collect();
    assert_eq!(
        words(&res.buffers[0]),
        mirrored,
        "the barrier makes the tile whole"
    );
    assert_eq!(words(&res.buffers[2]), (1..65).collect::<Vec<u32>>());
    // 64 lanes; the largest id 63; the smallest id + 5 is 5; one sum of all ids; 64 lanes.
    assert_eq!(
        words(&res.buffers[4]),
        [64, 63, 5, (0..64).sum::<u32>(), 64]
    );
    // Lane 63 fails its assertion; `{u}` and `{i}` holes print unsigned and signed.
    assert_eq!(
        prints,
        ["length = 64", "assertion failed: id.x < 63: lane 63, -1"]
    );
}

#[test]
fn groupshared_arrays_take_atomics() {
    let text = std::fs::read_to_string(data_path(
        env!("CARGO_MANIFEST_DIR"),
        "GroupAtomics.mshader",
    ))
    .unwrap()
    .replace("\r\n", "\n");
    let w = world(&text);
    assert_eq!(w.diagnostics(), "");
    // Values 0..64 * 3: bins of `3i % 8` take 8 each.
    let values: Vec<u32> = (0..64).map(|i| i * 3).collect();
    let mut res = marlin_shaderc_ir::Resources {
        buffers: vec![bytes(&values), vec![0; 9 * 4], vec![0; 64 * 4]],
        // HistogramArgs { Values = 0, Bins = 1, Before = 2 }.
        push: Some(Val::Agg((0..3).map(Val::u32).collect())),
    };
    w.dispatch("Game.Histogram", [1, 1, 1], &mut res);
    let mut expected = [0u32; 9];
    for v in &values {
        expected[(v % 8) as usize] += 1;
    }
    // AtomicMin on an `int` array: the smallest `id - 10` is -10.
    expected[8] = 0;
    assert_eq!(words(&res.buffers[1]), expected);
    // Every lane got the bin's count before its own add: each bin's 8 lanes saw 0..8 once.
    let before = words(&res.buffers[2]);
    for bin in 0..8u32 {
        let mut seen: Vec<u32> = (0..64)
            .filter(|&i| values[i] % 8 == bin)
            .map(|i| before[i])
            .collect();
        seen.sort();
        assert_eq!(seen, (0..8).collect::<Vec<u32>>(), "bin {bin}");
    }
}

#[test]
fn atomics_on_a_local_array_are_an_error() {
    // No other lane sees a local array: its atomics mean nothing.
    let text = "namespace Game;\n\
                public struct A { public Buffer<uint> B; }\n\
                [Compute(1, 1, 1)]\n\
                public void K([Push] in A a)\n\
                {\n    \
                    uint[4] local;\n    \
                    local[1] = 0;\n    \
                    a.B[0] = local.AtomicAdd(1, 1);\n\
                }\n";
    let w = world(text);
    let shown = w.lowering_error("Game.K");
    assert!(
        shown.contains("E1309")
            && shown.contains("atomics on an array need a `[GroupShared]` array"),
        "{shown}"
    );
}
