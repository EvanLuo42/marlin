//! `marlin_shaderc_opt::pipeline`: the optimization passes on any IR that verifies.
//!
//! Input: IR text as UTF-8. On a module that parses and verifies, the pipeline (verifying after
//! every pass) must not panic and must leave a module that verifies; each function that takes
//! plain data and finishes under a small step limit on zero arguments must return the same
//! after it. The replay test is `fuzz_finds_stay_fixed` in
//! compiler/shader/opt/tests/unit/passes.rs.
#![no_main]

use marlin_shaderc_ir::{Interpreter, Module, Val, parse_module, verify_module};
use marlin_shaderc_opt::{Options, Profile, pipeline};

fn results(m: &Module) -> Vec<Option<Vec<Val>>> {
    let mut it = Interpreter::new(m);
    it.options.step_limit = 20_000;
    m.funcs
        .iter()
        .map(|(id, f)| {
            if m.entry_of(id).is_some() {
                return None;
            }
            let args: Option<Vec<Val>> = f
                .params()
                .iter()
                .map(|&p| Val::zero(m, f.ty(p)).ok())
                .collect();
            it.call(id, &args?).ok()
        })
        .collect()
}

fn run(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(module) = parse_module(text) else {
        return;
    };
    if !verify_module(&module).is_empty() {
        return;
    }
    let before = results(&module);
    let mut opt = module.clone();
    let options = Options {
        profile: Profile::Development,
        verify_each: true,
    };
    if let Err(e) = pipeline(&options).run(&mut opt) {
        panic!("{e}");
    }
    for (b, a) in before.iter().zip(results(&opt)) {
        if let (Some(b), Some(a)) = (b, a) {
            assert_eq!(*b, a, "a function computes something else after the pipeline");
        }
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
