//! `marlin_shaderc_ir::parse_module`: the shader IR's text format on any text.
//!
//! Input: the text as UTF-8. Parsing must not panic, and a module it accepts must print to
//! text that parses back to the same printed form. The verifier must not panic on it, and on a
//! module that verifies, neither must the interpreter (each function called on zero arguments,
//! each compute entry point dispatched once, under a small step limit). The replay test is
//! `fuzz_finds_stay_fixed` in compiler/shader/ir/tests/unit/robustness.rs.
#![no_main]

use marlin_shaderc_ir::{Interpreter, Resources, Val, parse_module, print_module, verify_module};

fn run(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(module) = parse_module(text) else {
        return;
    };
    let printed = print_module(&module);
    let again = parse_module(&printed).expect("printed IR parses");
    assert_eq!(print_module(&again), printed);
    if !verify_module(&module).is_empty() {
        return;
    }
    let mut it = Interpreter::new(&module);
    it.options.step_limit = 20_000;
    for (id, f) in module.funcs.iter() {
        if module.entry_of(id).is_some() {
            let mut res = Resources {
                buffers: vec![vec![0; 256]; 4],
                push: None,
            };
            let _ = it.dispatch(id, [1, 1, 1], &mut res);
            continue;
        }
        let args: Option<Vec<Val>> = f
            .params()
            .iter()
            .map(|&p| Val::zero(&module, f.ty(p)).ok())
            .collect();
        if let Some(args) = args {
            let _ = it.call(id, &args);
        }
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
