//! `marlin_shaderc_ir::parse_module`: the shader IR's text format on any text.
//!
//! Input: the text as UTF-8. Parsing must not panic, and a module it accepts must print to
//! text that parses back to the same printed form. The replay test is `fuzz_finds_stay_fixed`
//! in compiler/shader/ir/tests/unit/robustness.rs.
#![no_main]

use marlin_shaderc_ir::{parse_module, print_module};

fn run(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(module) = parse_module(text) {
        let printed = print_module(&module);
        let again = parse_module(&printed).expect("printed IR parses");
        assert_eq!(print_module(&again), printed);
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
