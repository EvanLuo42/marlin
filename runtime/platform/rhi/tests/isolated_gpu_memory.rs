//! Driver allocations through the memory allocator (docs/design/0017-rhi-memory.md): small
//! resources share blocks, and resources created and dropped every frame make no driver
//! allocations. A binary of its own, so the counts see no other tests' resources; its tests run
//! one at a time.
#![cfg(marlin_platform = "windows")]

use marlin_gpu_test_support as support;

use std::sync::Mutex;

use marlin_rhi::{BufferUsage, Device, Format, MemoryLocation, TextureDesc, TextureUsage};
use support::{buffer, hardware_device, readback};

static SERIAL: Mutex<()> = Mutex::new(());

/// Driver allocations `make` adds.
fn driver_allocations(device: &Device, make: impl FnOnce()) -> u64 {
    let before = device.memory_stats().driver_allocations;
    make();
    device.memory_stats().driver_allocations - before
}

#[test]
fn small_buffers_share_blocks() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let device = hardware_device();
    let mut keep = Vec::new();
    let added = driver_allocations(device, || {
        for _ in 0..256 {
            keep.push(readback(device, 256));
            keep.push(buffer(
                device,
                4096,
                BufferUsage::SHADER_READ,
                MemoryLocation::GpuOnly,
            ));
        }
    });
    // A block per memory type, perhaps one more as blocks grow.
    assert!(
        added <= 4,
        "{added} driver allocations for 512 small buffers"
    );
    let stats = device.memory_stats();
    assert!(stats.local.suballocations >= 256, "{stats:?}");
    assert!(stats.non_local.suballocations >= 256, "{stats:?}");
    assert!(stats.local.block_bytes >= stats.local.suballocated_bytes);
    drop(keep);
    let after = device.memory_stats();
    assert!(after.local.suballocations <= stats.local.suballocations - 256);
}

#[test]
fn per_frame_readbacks_reuse_memory() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let device = hardware_device();
    let frame = || {
        let frame: Vec<_> = (0..3).map(|i| readback(device, 64 << i)).collect();
        drop(frame);
    };
    // The first frame may need a block; the next 200 reuse it.
    frame();
    let added = driver_allocations(device, || (0..200).for_each(|_| frame()));
    assert_eq!(
        added, 0,
        "readbacks created per frame allocate driver memory"
    );
}

#[test]
fn freed_texture_memory_is_reused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let device = hardware_device();
    let frame = || {
        let textures: Vec<_> = (0..8)
            .map(|_| {
                device
                    .create_texture(&TextureDesc::new_2d(
                        64,
                        64,
                        Format::Rgba8Unorm,
                        TextureUsage::SAMPLED,
                    ))
                    .unwrap()
            })
            .collect();
        drop(textures);
    };
    frame();
    let added = driver_allocations(device, || (0..50).for_each(|_| frame()));
    assert_eq!(
        added, 0,
        "small textures created and dropped allocate driver memory"
    );
}
