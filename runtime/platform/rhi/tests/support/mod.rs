//! Shared test helpers: a hardware device and the test shaders.
//!
//! Shaders are looked up by name and stage. This is the shape the planned test-shader crate will
//! have: its build.rs compiles every Marlin shader under `tests/shaders/` with the Marlin shader
//! compiler library and generates this lookup. Until that compiler exists, the blobs are
//! PLACEHOLDERS: the `.hlsl` files beside them, compiled once by hand with the Windows SDK's dxc:
//!
//! ```text
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T cs_6_6 -E main    -Fo fill.cs.dxil fill.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T vs_6_6 -E vs_main -Fo quad.vs.dxil quad.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T ps_6_6 -E ps_main -Fo quad.ps.dxil quad.hlsl
//! ```
#![allow(dead_code, reason = "each test file uses a subset")]

use marlin_rhi::{AdapterPreference, Device, DeviceDesc, Instance, InstanceDesc, ShaderModule};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Vertex,
    Pixel,
    Compute,
}

/// The compiled shader `name` for `stage`. Panics if there is none.
pub fn shader(name: &str, stage: Stage) -> ShaderModule<'static> {
    let (code, entry): (&'static [u8], _) = match (name, stage) {
        ("fill", Stage::Compute) => (include_bytes!("../shaders/fill.cs.dxil"), "main"),
        ("quad", Stage::Vertex) => (include_bytes!("../shaders/quad.vs.dxil"), "vs_main"),
        ("quad", Stage::Pixel) => (include_bytes!("../shaders/quad.ps.dxil"), "ps_main"),
        _ => panic!("no test shader {name} for {stage:?}"),
    };
    ShaderModule { code, entry }
}

/// A device on the highest-performance hardware adapter, with validation on. GPU tests run on
/// real hardware, never on a software rasterizer.
pub fn hardware_device() -> Device {
    let instance = Instance::new(&InstanceDesc { validation: true }).unwrap();
    instance
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::HighPerformance,
            ..DeviceDesc::default()
        })
        .unwrap()
}

/// Records with `record`, submits, and waits for the GPU.
pub fn run(device: &Device, record: impl FnOnce(&mut marlin_rhi::CommandList)) {
    let mut pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    let fence = device.create_fence(0).unwrap();
    list.begin(&mut pool).unwrap();
    record(&mut list);
    list.end().unwrap();
    // SAFETY: everything the list uses outlives the wait below.
    unsafe { device.queue().submit(&[&list], Some((&fence, 1))).unwrap() };
    fence.wait(1, None).unwrap();
}
