//! The parser must not panic on any text, and what it accepts must print to text it reads back
//! the same way, and the verifier and (on what verifies) the interpreter must not panic either:
//! seeded mutations of the fixtures, checked under the binary's memory cap.

use marlin_math::Rng;
use marlin_shaderc_ir::{Interpreter, Module, Val, parse_module, print_module, verify_module};
use marlin_test_support::{data_path, regress, scale, seed};

/// Characters the format gives meaning to, for insertions.
const NOISE: &[char] = &[
    '(', ')', '[', ']', '{', '}', ',', '%', '@', '"', '-', '.', ':', '\n', '0', 'x', '=',
];

/// Calls every function that takes plain data with zero arguments, and dispatches every
/// compute entry point once: errors are fine, panics are not.
fn run_everything(m: &Module) {
    let mut it = Interpreter::new(m);
    it.options.step_limit = 20_000;
    for (id, f) in m.funcs.iter() {
        if m.entry_of(id).is_some() {
            let mut res = marlin_shaderc_ir::Resources {
                buffers: vec![vec![0; 256]; 4],
                push: None,
            };
            let _ = it.dispatch(id, [1, 1, 1], &mut res);
            continue;
        }
        let args: Option<Vec<Val>> = f
            .params()
            .iter()
            .map(|&p| Val::zero(m, f.ty(p)).ok())
            .collect();
        if let Some(args) = args {
            let _ = it.call(id, &args);
        }
    }
}

fn check(text: &str, seed: u64) {
    let result = std::panic::catch_unwind(|| {
        if let Ok(module) = parse_module(text) {
            if verify_module(&module).is_empty() {
                run_everything(&module);
            }
            let printed = print_module(&module);
            let again = parse_module(&printed)
                .unwrap_or_else(|e| panic!("printed text does not parse: {e}\n{printed}"));
            assert_eq!(print_module(&again), printed);
        }
    });
    assert!(result.is_ok(), "seed {seed}: panicked on {text:?}");
}

#[test]
fn mutated_text_never_panics() {
    let seed = seed(0x1e_0001);
    let mut rng = Rng::new(seed);
    for name in [
        "text/ops.ir",
        "text/blur.ir",
        "interp/ctfe.ir",
        "interp/compute.ir",
    ] {
        let source: Vec<char> =
            std::fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), name))
                .unwrap()
                .chars()
                .collect();
        for _ in 0..scale(150) {
            let mut mutated = source.clone();
            for _ in 0..1 + rng.below(3) {
                let at = rng.below(mutated.len() + 1);
                let end = (at + rng.below(20)).min(mutated.len());
                match rng.below(3) {
                    0 => {
                        mutated.drain(at..end);
                    }
                    1 => {
                        let copy: Vec<char> = mutated[at..end].to_vec();
                        let to = rng.below(mutated.len() + 1);
                        mutated.splice(to..to, copy);
                    }
                    _ => mutated.insert(at, NOISE[rng.below(NOISE.len())]),
                }
            }
            check(&mutated.into_iter().collect::<String>(), seed);
        }
    }
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let deep_type = format!(
        "global @g : ptr<push, {}f32{}>\n",
        "[".repeat(10_000),
        "; 2]".repeat(10_000)
    );
    assert!(parse_module(&deep_type).is_err());
    let deep_operand = format!(
        "func @f() {{\nblock0:\n    %x = neg {}i32 1{}\n}}\n",
        "(neg ".repeat(10_000),
        ")".repeat(10_000)
    );
    assert!(parse_module(&deep_operand).is_err());
}

#[test]
fn fuzz_finds_stay_fixed() {
    regress::for_each(env!("CARGO_MANIFEST_DIR"), "shaderc_ir_parse", |bytes| {
        if let Ok(text) = std::str::from_utf8(bytes) {
            check(text, 0);
        }
    });
}
