//! Diagnostics, memory, clocks, native objects and sharing, on the hardware adapter with
//! validation on.

use marlin_gpu_test_support as support;

use std::time::Duration;

use marlin_rhi::{
    BufferBarrier, BufferDesc, BufferState, BufferUsage, FenceWait, Format, FormatFeatures,
    HeapDesc, HeapKind, MarkerPoint, MemoryLocation, Pageable, QueueType, ResidencyPriority,
    TextureDesc, TextureUsage,
};
use support::{
    buffer, compute_pipeline, copy_out, devices, gpu_test, hardware_device, read_words, readback,
    run, run_on, storage,
};

#[test]
fn objects_take_debug_names() {
    let device = hardware_device();
    device.set_name("test device");
    device.queue().set_name("test graphics queue");
    let b = buffer(device, 256, BufferUsage::COPY_DST, MemoryLocation::GpuOnly);
    b.set_name("named buffer");
    let t = device
        .create_texture(&TextureDesc::new_2d(
            4,
            4,
            Format::Rgba8Unorm,
            TextureUsage::SAMPLED,
        ))
        .unwrap();
    t.set_name("named texture ✓ unicode");
    let fence = device.create_fence(0).unwrap();
    fence.set_name("named fence");
    let pool = device.create_command_pool().unwrap();
    pool.set_name("named pool");
    let list = device.create_command_list(&pool).unwrap();
    list.set_name("named list");
    compute_pipeline(device, "Fill").set_name("named pipeline");
    assert!(device.status().is_ok());
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn markers_record_progress_on_every_queue() {
    let device = hardware_device();
    support::require_feature!(device, buffer_markers);
    let markers = readback(device, 64);
    for (i, ty) in [QueueType::Graphics, QueueType::Compute, QueueType::Copy]
        .into_iter()
        .enumerate()
    {
        run_on(device, ty, |list| {
            let at = i as u64 * 8;
            list.write_marker(&markers, at, 100 + i as u32, MarkerPoint::Reached);
            list.write_marker(&markers, at + 4, 200 + i as u32, MarkerPoint::Completed);
        });
    }
    assert_eq!(read_words(&markers, 0, 6), [100, 200, 101, 201, 102, 202]);
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn memory_budget_and_residency() {
    let device = hardware_device();
    let budget = device.memory_budget().unwrap();
    assert!(budget.local.budget > 0, "{budget:?}");
    let heap = device
        .create_heap(&HeapDesc {
            size: 1 << 20,
            memory: MemoryLocation::GpuOnly,
            kind: HeapKind::Buffers,
        })
        .unwrap();
    let committed = buffer(
        device,
        1 << 16,
        BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
        MemoryLocation::GpuOnly,
    );
    let after = device.memory_budget().unwrap();
    assert!(after.local.budget > 0, "{after:?}");
    let objects = [Pageable::Heap(&heap), Pageable::Buffer(&committed)];
    device
        .set_residency_priority(&objects, ResidencyPriority::High)
        .unwrap();
    device.evict(&objects).unwrap();
    device.make_resident(&objects).unwrap();
    // Placed resources page with their heap.
    let placed = device
        .create_placed_buffer(
            &heap,
            0,
            &BufferDesc {
                size: 256,
                usage: BufferUsage::COPY_DST,
                memory: MemoryLocation::GpuOnly,
            },
        )
        .unwrap();
    assert!(device.evict(&[Pageable::Buffer(&placed)]).is_err());
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn clocks_calibrate_on_every_queue() {
    let device = hardware_device();
    for ty in [QueueType::Graphics, QueueType::Compute, QueueType::Copy] {
        let queue = device.queue_for(ty);
        let first = queue.clock_calibration().unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let second = queue.clock_calibration().unwrap();
        assert!(second.cpu > first.cpu && second.gpu >= first.gpu, "{ty:?}");
    }
}

#[test]
fn waits_for_all_or_any_fence() {
    let device = hardware_device();
    let a = device.create_fence(0).unwrap();
    let b = device.create_fence(0).unwrap();
    let fences = [(&a, 1), (&b, 1)];
    let short = Some(Duration::from_millis(10));
    assert!(
        !device
            .wait_for_fences(&fences, FenceWait::Any, short)
            .unwrap()
    );
    a.signal(1).unwrap();
    assert!(
        device
            .wait_for_fences(&fences, FenceWait::Any, short)
            .unwrap()
    );
    assert!(
        !device
            .wait_for_fences(&fences, FenceWait::All, short)
            .unwrap()
    );
    b.signal(1).unwrap();
    assert!(
        device
            .wait_for_fences(&fences, FenceWait::All, None)
            .unwrap()
    );
}

#[test]
fn native_objects_wrap_and_share() {
    let device = hardware_device();
    assert!(!device.as_raw().is_null());
    assert!(!device.queue().as_raw().is_null());
    let source = buffer(
        device,
        256,
        BufferUsage::COPY_SRC | BufferUsage::COPY_DST | BufferUsage::SHARED,
        MemoryLocation::GpuOnly,
    );
    // A second wrapper over the same resource, as an upscaler or XR runtime hands one over.
    // SAFETY: a live buffer of this device that the description matches.
    let wrapped = unsafe { device.buffer_from_raw(source.as_raw(), source.desc()) }.unwrap();
    // And a third, through an OS handle, as another process would open it.
    let handle = device.export_buffer(&source).unwrap();
    assert!(!handle.as_raw().is_null());
    // SAFETY: the handle names `source`, which the description matches.
    let imported = unsafe { device.import_buffer(&handle, source.desc()) }.unwrap();
    let staging = support::upload(
        device,
        &support::words_to_bytes(&[7; 64]),
        BufferUsage::COPY_SRC,
    );
    let out = readback(device, 512);
    run(device, |list| {
        list.copy_buffer(&staging, 0, &wrapped, 0, 256);
        list.buffer_barriers(&[BufferBarrier {
            buffer: &imported,
            before: BufferState::COPY_DST,
            after: BufferState::COPY_SRC,
        }]);
        list.copy_buffer(&imported, 0, &out, 0, 256);
    });
    assert!(read_words(&out, 0, 64).iter().all(|&w| w == 7));

    let fence = device.create_shared_fence(0).unwrap();
    let fence_handle = device.export_fence(&fence).unwrap();
    // SAFETY: the handle names a fence.
    let opened = unsafe { device.import_fence(&fence_handle) }.unwrap();
    fence.signal(5).unwrap();
    assert_eq!(opened.completed_value(), 5);
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn textures_wrap_native_resources() {
    let device = hardware_device();
    let desc = TextureDesc::new_2d(8, 8, Format::Rgba8Unorm, TextureUsage::COPY_DST);
    let texture = device.create_texture(&desc).unwrap();
    // SAFETY: a live texture of this device that `desc` describes.
    let wrapped = unsafe { device.texture_from_raw(texture.as_raw(), &desc) }.unwrap();
    assert_eq!(wrapped.as_raw(), texture.as_raw());
    assert!(device.export_texture(&texture).is_err(), "not SHARED");
}

#[test]
fn pipeline_cache_round_trips() {
    for device in devices() {
        let cache = device.create_pipeline_cache(&[]).unwrap();
        let desc = |cache| marlin_rhi::ComputePipelineDesc {
            shader: support::shader("Fill"),
            cache,
        };
        let empty = cache.serialize().unwrap().len();
        device.create_compute_pipeline(&desc(Some(&cache))).unwrap();
        // The second creation finds the first in the cache.
        device.create_compute_pipeline(&desc(Some(&cache))).unwrap();
        let bytes = cache.serialize().unwrap();
        assert!(
            bytes.len() > empty,
            "the cache holds the compiled pipeline: {} bytes, {empty} empty",
            bytes.len()
        );
        let reloaded = device.create_pipeline_cache(&bytes).unwrap();
        let pipeline = device
            .create_compute_pipeline(&desc(Some(&reloaded)))
            .unwrap();
        // Garbage (another driver's cache, a truncated file) starts an empty cache.
        let garbage = device.create_pipeline_cache(&[0x5a; 300]).unwrap();
        device
            .create_compute_pipeline(&desc(Some(&garbage)))
            .unwrap();
        // A cached pipeline works like a fresh one.
        let (out, view) = storage(device, 64);
        let rb = readback(device, 256);
        run(device, |list| {
            list.set_compute_pipeline(&pipeline);
            list.push_constants(0, &[view.index(), 5]);
            list.dispatch(1, 1, 1);
            copy_out(list, &out, &rb, 64);
        });
        assert_eq!(read_words(&rb, 0, 3), [5, 8, 11]);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn format_support_reports_capabilities() {
    let device = hardware_device();
    let rgba = device.format_support(Format::Rgba8Unorm);
    assert!(rgba.features.contains(
        FormatFeatures::SAMPLED
            | FormatFeatures::FILTER
            | FormatFeatures::RENDER_TARGET
            | FormatFeatures::BLEND
    ));
    assert!(
        rgba.supports_samples(1) && rgba.supports_samples(4),
        "{rgba:?}"
    );
    let depth = device.format_support(Format::D32Float);
    assert!(
        depth
            .features
            .contains(FormatFeatures::DEPTH_STENCIL | FormatFeatures::SAMPLED)
    );
    let uint = device.format_support(Format::R32Uint);
    assert!(
        uint.features
            .contains(FormatFeatures::STORAGE_WRITE | FormatFeatures::STORAGE_READ)
    );
    assert!(!uint.features.contains(FormatFeatures::FILTER));
    let shared_exponent = device.format_support(Format::Rgb9e5Ufloat);
    assert!(shared_exponent.features.contains(FormatFeatures::SAMPLED));
    assert!(
        !shared_exponent
            .features
            .contains(FormatFeatures::RENDER_TARGET)
    );
    // Formats not every device has: one it lacks reports no support and cannot be made, one it
    // samples can.
    for format in [Format::Astc4x4Unorm, Format::Etc2Rgb8Unorm] {
        let support = device.format_support(format);
        let result =
            device.create_texture(&TextureDesc::new_2d(8, 8, format, TextureUsage::SAMPLED));
        if support == Default::default() {
            assert_eq!(
                result.unwrap_err().kind(),
                marlin_rhi::ErrorKind::Unsupported,
                "{format:?}"
            );
        } else if support.features.contains(FormatFeatures::SAMPLED) {
            result.unwrap();
        }
    }
    assert_eq!(device.validation_errors(), 0);
}

#[gpu_test(api = d3d12)]
fn d3d12_has_no_astc_or_etc2() {
    let device = hardware_device();
    for format in [Format::Astc4x4Unorm, Format::Etc2Rgb8Unorm] {
        assert_eq!(device.format_support(format), Default::default());
    }
}

#[test]
fn new_formats_create_and_copy() {
    let device = hardware_device();
    let formats = [
        Format::R8Snorm,
        Format::Rg8Uint,
        Format::Rgba8Sint,
        Format::Rg16Snorm,
        Format::R32Sint,
        Format::Rgba16Sint,
        Format::Rgb10a2Uint,
        Format::Rgb9e5Ufloat,
        Format::Bc2Unorm,
        Format::Bc4Snorm,
        Format::Bc5Snorm,
    ];
    for format in formats {
        let desc = TextureDesc::new_2d(
            8,
            8,
            format,
            TextureUsage::SAMPLED | TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
        );
        let texture = device.create_texture(&desc).unwrap();
        let region = marlin_rhi::BufferTextureCopy::whole_mip(&desc, 0, 0, 0);
        let bytes: Vec<u8> = (0..2048).map(|i| (i * 7 + 3) as u8).collect();
        let staging = support::upload(device, &bytes, BufferUsage::COPY_SRC);
        let out = readback(device, 2048);
        run(device, |list| {
            use marlin_rhi::{TextureBarrier, TextureState};
            list.texture_barriers(&[TextureBarrier::new(
                &texture,
                TextureState::UNDEFINED,
                TextureState::COPY_DST,
            )]);
            list.copy_buffer_to_texture(&staging, &texture, &region);
            list.texture_barriers(&[TextureBarrier::new(
                &texture,
                TextureState::COPY_DST,
                TextureState::COPY_SRC,
            )]);
            list.copy_texture_to_buffer(&texture, &out, &region);
        });
        let rows = (8 / format.block_extent()) as usize;
        let row = (8 / format.block_extent() * format.block_size()) as usize;
        let got = support::read_bytes(&out, 0, 2048);
        for r in 0..rows {
            let at = r * region.bytes_per_row as usize;
            assert_eq!(got[at..at + row], bytes[at..at + row], "{format:?} row {r}");
        }
    }
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn stable_power_state_needs_developer_mode() {
    let device = hardware_device();
    // It may be refused (D3D12 without Windows developer mode, APIs without the call); either way
    // the device stays usable.
    let _ = device.set_stable_power_state(true);
    let _ = device.set_stable_power_state(false);
    assert!(device.status().is_ok());
}

#[test]
fn outputs_enumerate() {
    let instance = marlin_rhi::Instance::new(&Default::default()).unwrap();
    for adapter in instance.adapters() {
        for output in instance.outputs(adapter.index) {
            assert!(!output.name().is_empty());
            assert!(
                output.desktop.width > 0 && output.desktop.height > 0,
                "{output:?}"
            );
        }
    }
}
