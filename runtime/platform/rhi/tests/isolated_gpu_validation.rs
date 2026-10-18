//! GPU-based validation. Its own test binary: the debug layer's GPU validation is process-wide
//! and must be chosen by the first instance that turns validation on.
#![cfg(marlin_platform = "windows")]

use marlin_gpu_test_support as support;

use marlin_rhi::{AdapterPreference, DeviceDesc, Instance, InstanceDesc};
use support::{compute_pipeline, copy_out, read_words, readback, storage};

#[test]
fn bindless_work_passes_gpu_validation() {
    let instance = Instance::new(&InstanceDesc {
        validation: true,
        gpu_validation: true,
        ..InstanceDesc::default()
    })
    .unwrap();
    let device = instance
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::HighPerformance,
            ..DeviceDesc::default()
        })
        .unwrap();
    let (out, view) = storage(&device, 64);
    let rb = readback(&device, 256);
    let pipeline = compute_pipeline(&device, "Fill");
    support::run(&device, |list| {
        list.set_compute_pipeline(&pipeline);
        list.push_constants(0, &[view.index(), 2]);
        list.dispatch(1, 1, 1);
        copy_out(list, &out, &rb, 64);
    });
    assert_eq!(read_words(&rb, 0, 2), [2, 5]);
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn gpu_validation_needs_validation() {
    let error = Instance::new(&InstanceDesc {
        validation: false,
        gpu_validation: true,
        ..InstanceDesc::default()
    })
    .unwrap_err();
    assert_eq!(error.kind(), marlin_rhi::ErrorKind::InvalidInput);
}
