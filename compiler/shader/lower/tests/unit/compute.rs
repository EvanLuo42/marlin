use marlin_shaderc_ir::{Resources, Val, parse_module, print_module};
use marlin_test_support::{data_path, golden};

use crate::support::{World, world};

fn compute() -> World {
    world(&["game/Compute.mshader"])
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    u32s(bytes).into_iter().map(f32::from_bits).collect()
}

/// `Args { Out = 0, Total = 1, Scale }` over an output of `n` words and two words of totals.
fn args(n: usize, scale: u32) -> Resources {
    Resources {
        buffers: vec![vec![0; n * 4], vec![0; 8]],
        push: Some(Val::Agg(vec![Val::u32(0), Val::u32(1), Val::u32(scale)])),
    }
}

#[test]
fn system_values_push_groupshared_and_atomics() {
    let w = compute();
    let mut res = args(128, 3);
    let out = w.dispatch("Game.Mirror", &[], &[], [2, 1, 1], &mut res);
    assert_eq!(out.out_of_bounds, 0);
    let want: Vec<u32> = (0..128u32)
        .map(|x| ((x / 64) * 64 + 63 - x % 64) * 3)
        .collect();
    assert_eq!(
        u32s(&res.buffers[0]),
        want,
        "the barrier makes the tile whole"
    );
    assert_eq!(u32s(&res.buffers[1]), vec![(0..128).sum::<u32>(), 127]);
}

#[test]
fn buffer_elements_are_written_in_place() {
    let w = compute();
    // Three particles of 16 bytes: Pos (12), Mass (4).
    let mut data = Vec::new();
    for k in 0..3 {
        for v in [1.0f32, 2.0, 3.0, k as f32 + 1.0] {
            data.extend(v.to_le_bytes());
        }
    }
    let mut res = Resources {
        buffers: vec![data],
        push: Some(Val::Agg(vec![Val::u32(0), Val::f32(0.5)])),
    };
    let out = w.dispatch("Game.Move", &[], &[], [1, 1, 1], &mut res);
    assert_eq!(out.out_of_bounds, 0, "lanes past the end return first");
    assert_eq!(
        f32s(&res.buffers[0]),
        vec![1.0, 2.5, 3.0, 1.0, 1.0, 3.0, 3.0, 2.0, 1.0, 3.5, 3.0, 3.0]
    );
}

#[test]
fn generic_entries_take_type_arguments() {
    let w = compute();
    for (op, want) in [
        ("Game.Doubler", (0..8u32).map(|x| x * 2).collect::<Vec<_>>()),
        ("Game.Squarer", (0..8u32).map(|x| x * x).collect()),
    ] {
        let mut res = args(8, 1);
        w.dispatch("Game.MapOp", &[op], &[], [1, 1, 1], &mut res);
        assert_eq!(u32s(&res.buffers[0]), want, "{op}");
    }
}

#[test]
fn static_parameters_are_overridden_per_request() {
    let w = compute();
    let mut res = args(4, 1);
    let module = w.try_entry("Game.Statics", &[], &[]).unwrap();
    assert_eq!(
        module.entries[0].stage,
        marlin_shaderc_ir::Stage::Compute([4, 1, 1]),
        "the group size is a constant"
    );
    w.dispatch("Game.Statics", &[], &[], [1, 1, 1], &mut res);
    assert_eq!(u32s(&res.buffers[0]), vec![6, 7, 8, 9]);

    // Constants that depend on an overridden one follow it.
    let statics = [
        ("Game.Rock.Factor", Val::u32(5)),
        ("Game.Rock.UseMoss", Val::Bits(1)),
    ];
    let mut res = args(4, 1);
    w.dispatch("Game.Statics", &[], &statics, [1, 1, 1], &mut res);
    assert_eq!(u32s(&res.buffers[0]), vec![110, 111, 112, 113]);
}

#[test]
fn const_functions_run_at_compile_time() {
    let w = compute();
    let mut res = args(1, 1);
    w.dispatch("Game.ConstFunctions", &[], &[], [1, 1, 1], &mut res);
    // Triangle(4) = 10, and the groupshared array's length 3 comes from a constant.
    assert_eq!(u32s(&res.buffers[0]), vec![13]);
}

#[test]
fn bad_requests_are_errors() {
    let w = compute();
    for (path, type_args, statics, want) in [
        (
            "Game.UsesBadTable",
            &[][..],
            &[][..],
            "compile-time evaluation calls only `const` functions; `Game.NotConst` is not one",
        ),
        (
            "Game.NotAnEntry",
            &[],
            &[],
            "`Game.NotAnEntry` is not an entry point",
        ),
        (
            "Game.MapOp",
            &[],
            &[],
            "`Game.MapOp` takes 1 generic argument(s); 0 given",
        ),
        (
            "Game.Statics",
            &[],
            &[("Game.Table.Len", Val::Agg(vec![Val::u32(1), Val::u32(2)]))][..],
            "this value does not fit `Game.Table.Len` (`int`)",
        ),
        (
            "Game.Statics",
            &[],
            &[("Game.NotConst", Val::u32(1))][..],
            "`Game.NotConst` is not a static parameter (a `const` field of a struct)",
        ),
    ] {
        let err = w.try_entry(path, type_args, statics).unwrap_err();
        assert!(err.contains(want), "{path}: {err}");
    }
}

#[test]
fn entries_lower_to_wrappers_over_memory_places() {
    let w = compute();
    let mut text = String::new();
    for path in ["Game.Mirror", "Game.Move"] {
        let module = w.try_entry(path, &[], &[]).unwrap();
        let printed = print_module(&module);
        // The text format reads back what lowering made.
        let again = parse_module(&printed).unwrap();
        assert_eq!(print_module(&again), printed, "{path}");
        text.push_str(&printed);
        text.push('\n');
    }
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "compute/entries.ir"),
        &text,
    );
}

#[test]
fn compute_entries_go_through_the_whole_pipeline() {
    let w = compute();
    let mut text = String::new();
    for path in ["Game.Mirror", "Game.Move"] {
        let module = w.try_entry(path, &[], &[]).unwrap();
        let optimized = crate::support::optimize(&module, path);
        // One function per entry is left: the instance it called is inlined and gone.
        assert_eq!(optimized.funcs.len(), 1, "{}", print_module(&optimized));
        text.push_str(&print_module(&optimized));
        text.push('\n');
    }
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "compute/optimized.ir"),
        &text,
    );
}
