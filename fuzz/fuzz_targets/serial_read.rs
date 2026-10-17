//! `marlin_serial` reading: any bytes into a typed value (`from_bytes`, `read_into` over an
//! existing value, a shallow `max_depth`) and into a dynamic value through a registry.
#![no_main]

use marlin_fuzz::{Everything, Node, Shape, registry};
use marlin_reflect::Mut;
use marlin_serial::{ReadOptions, from_bytes, read_header, read_into_with, read_value};

fn run(data: &[u8]) {
    let _ = read_header(data);
    let _ = from_bytes::<Everything>(data);
    let _ = from_bytes::<Vec<Shape>>(data);
    if let Ok(mut node) = from_bytes::<Node>(data) {
        // Reading over a value that already has contents.
        let _ = read_into_with(Mut::new(&mut node), data, &ReadOptions { max_depth: 4 });
    }
    thread_local! {
        static REGISTRY: marlin_reflect::Registry = registry();
    }
    REGISTRY.with(|registry| {
        let _ = read_value(data, registry);
    });
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
