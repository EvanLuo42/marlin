//! Losing the device and recovering from it. Its own test binary with a single test: a forced
//! loss can take down every device on the adapter in the process (D3D12 shares one device per
//! adapter; Vulkan drivers may reset them all), and with them any test running beside it.
#![cfg(marlin_platform = "windows")]

use marlin_gpu_test_support as support;

use std::time::Duration;

use marlin_rhi::{AdapterPreference, DeviceDesc, ErrorKind, Instance, InstanceDesc};
use support::{compute_pipeline, copy_out, read_words, readback, run, storage};

#[test]
fn reports_loss_then_recovers_with_a_new_device() {
    let instance = Instance::new(&InstanceDesc {
        validation: true,
        crash_diagnostics: true,
        ..InstanceDesc::default()
    })
    .unwrap();
    let desc = DeviceDesc {
        adapter: AdapterPreference::HighPerformance,
        ..DeviceDesc::default()
    };

    {
        let device = instance.create_device(&desc).unwrap();
        let (out, view) = storage(&device, 64);
        let pipeline = compute_pipeline(&device, "Fill");
        let fence = device.create_fence(0).unwrap();
        let mut pool = device.create_command_pool().unwrap();
        let mut list = device.create_command_list(&pool).unwrap();
        list.begin(&mut pool).unwrap();
        list.set_compute_pipeline(&pipeline);
        list.push_constants(0, &[view.index(), 0]);
        list.dispatch(1, 1, 1);
        list.end().unwrap();
        assert!(device.status().is_ok());

        device.force_device_lost();
        let lost = device.status().unwrap_err();
        assert_eq!(lost.kind(), ErrorKind::DeviceLost);
        // Every path that touches the GPU says so instead of hanging or pretending success.
        // SAFETY: the device is gone; nothing will run.
        let submitted = unsafe { device.queue().submit(&[&list], Some((&fence, 1))) };
        if let Err(e) = submitted {
            assert_eq!(e.kind(), ErrorKind::DeviceLost);
        }
        let waited = fence.wait(1, Some(Duration::from_secs(5)));
        assert_eq!(waited.unwrap_err().kind(), ErrorKind::DeviceLost);
        assert_eq!(
            device.queue().wait_idle().unwrap_err().kind(),
            ErrorKind::DeviceLost
        );
        // Dropping everything (the device last) must not hang either.
        drop((out, view, pipeline, list, pool, fence));
    }

    // A new device works as if nothing happened.
    let device = instance.create_device(&desc).unwrap();
    assert!(device.status().is_ok());
    let (out, view) = storage(&device, 64);
    let rb = readback(&device, 256);
    let pipeline = compute_pipeline(&device, "Fill");
    run(&device, |list| {
        list.set_compute_pipeline(&pipeline);
        list.push_constants(0, &[view.index(), 1]);
        list.dispatch(1, 1, 1);
        copy_out(list, &out, &rb, 64);
    });
    assert_eq!(read_words(&rb, 0, 2), [1, 4]);
}
