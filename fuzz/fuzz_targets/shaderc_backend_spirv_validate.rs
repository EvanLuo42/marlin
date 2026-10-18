//! `marlin_shaderc_backend_spirv::validate`: the SPIR-V validator on any words.
//!
//! Input: a SPIR-V binary (little-endian words). Neither reading the words nor validating them
//! may panic, and a module the validator accepts must disassemble. The replay test is
//! `fuzz_finds_stay_fixed` in compiler/shader/backend_spirv/tests/unit/broken.rs.
#![no_main]

use marlin_shaderc_backend_spirv::{disassemble, from_bytes, validate};

fn run(data: &[u8]) {
    let Some(words) = from_bytes(data) else {
        return;
    };
    if validate(&words).is_empty() {
        let _ = disassemble(&words);
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
