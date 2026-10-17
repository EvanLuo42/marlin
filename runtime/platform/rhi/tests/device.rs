//! Device-level tests that need no window: they run on the hardware adapter with validation on,
//! and require it to report no errors.
#![cfg(marlin_platform = "windows")]

mod support;

use std::time::Duration;

use marlin_rhi::{
    AdapterKind, AdapterPreference, DeviceDesc, ErrorKind, FramesInFlight, Instance, InstanceDesc,
};
use support::hardware_device;

#[test]
fn enumerates_adapters() {
    let instance = Instance::new(&InstanceDesc::default()).unwrap();
    let adapters: Vec<_> = instance.adapters().collect();
    assert!(
        !adapters.is_empty(),
        "DXGI always lists the Basic Render Driver"
    );
    for (i, adapter) in adapters.iter().enumerate() {
        assert_eq!(adapter.index, i as u32);
        assert!(!adapter.name().is_empty());
    }
    // Hardware first: once a software adapter appears, no hardware follows.
    let first_software = adapters
        .iter()
        .position(|a| a.kind == AdapterKind::Software)
        .unwrap_or(adapters.len());
    assert!(
        adapters[first_software..]
            .iter()
            .all(|a| a.kind == AdapterKind::Software)
    );
}

#[test]
fn creates_hardware_device() {
    let device = hardware_device();
    let info = device.adapter_info();
    assert_eq!(info.kind, AdapterKind::Hardware, "{}", info.name());
    assert!(device.features().shader_model >= (6, 6));
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn index_past_the_end_is_not_found() {
    let instance = Instance::new(&InstanceDesc::default()).unwrap();
    let count = instance.adapters().count() as u32;
    let err = instance
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::Index(count),
            ..DeviceDesc::default()
        })
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn fence_cpu_signal_and_wait() {
    let device = hardware_device();
    let fence = device.create_fence(3).unwrap();
    assert_eq!(fence.completed_value(), 3);
    assert!(fence.wait(2, None).unwrap());
    assert!(!fence.wait(4, Some(Duration::from_millis(10))).unwrap());

    // A waiter on another thread wakes when the CPU signals.
    std::thread::scope(|s| {
        let waiter = s.spawn(|| fence.wait(10, Some(Duration::from_secs(10))).unwrap());
        std::thread::sleep(Duration::from_millis(20));
        fence.signal(10).unwrap();
        assert!(waiter.join().unwrap());
    });
    assert_eq!(fence.completed_value(), 10);
}

#[test]
fn queue_signals_fence_after_submit() {
    let device = hardware_device();
    let fence = device.create_fence(0).unwrap();
    let mut pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    for value in 1..=3 {
        list.begin(&mut pool).unwrap();
        list.end().unwrap();
        // SAFETY: the list uses no resources; it is not reused before the fence wait below.
        unsafe {
            device
                .queue()
                .submit(&[&list], Some((&fence, value)))
                .unwrap()
        };
        assert!(fence.wait(value, Some(Duration::from_secs(10))).unwrap());
        // SAFETY: the fence shows the GPU finished the only list recorded from the pool.
        unsafe { pool.reset().unwrap() };
    }
    device.queue().wait_idle().unwrap();
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn frames_in_flight_cycle() {
    let device = hardware_device();
    let mut frames = FramesInFlight::new(device, 2).unwrap();
    assert_eq!(frames.count(), 2);
    for frame in 0..6 {
        assert_eq!(frames.index(), frame % 2);
        frames.begin().unwrap();
        // SAFETY: the lists use no resources.
        unsafe { frames.submit(device.queue()).unwrap() };
    }
    frames.wait_idle().unwrap();
    drop(frames);
    assert_eq!(device.validation_errors(), 0);
}
