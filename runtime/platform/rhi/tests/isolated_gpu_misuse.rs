//! A deliberate API misuse, in its own test binary: validation messages are not tied to one
//! device (D3D12 hands out one device per adapter per process; a Vulkan debug messenger belongs
//! to the instance), so they would otherwise reach other tests' devices.
#![cfg(marlin_platform = "windows")]

use marlin_rhi::{
    AdapterPreference, DeviceDesc, GRAPHICS_API, GraphicsApi, Instance, InstanceDesc,
};

#[test]
fn validation_reports_errors() {
    let instance = Instance::new(&InstanceDesc {
        validation: true,
        ..InstanceDesc::default()
    })
    .unwrap();
    let device = instance
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::HighPerformance,
            ..DeviceDesc::default()
        })
        .unwrap();
    let pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    // Lists start closed; closing again is an API misuse the validation layer reports.
    let ended = list.end();
    assert!(device.validation_errors() > 0);
    // D3D12 also fails the call (Vulkan's vkEndCommandBuffer has no error for it).
    if GRAPHICS_API == GraphicsApi::D3D12 {
        assert!(ended.is_err());
    }
}
