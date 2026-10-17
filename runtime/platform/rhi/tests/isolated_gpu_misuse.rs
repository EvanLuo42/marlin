//! A deliberate API misuse, in its own test binary: D3D12 hands out one device per adapter per
//! process, so its validation messages would otherwise reach other tests' devices.
#![cfg(marlin_platform = "windows")]

use marlin_rhi::{AdapterPreference, DeviceDesc, Instance, InstanceDesc};

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
    // Lists start closed; closing again is an API misuse the debug layer reports.
    assert!(list.end().is_err());
    assert!(device.validation_errors() > 0);
}
