//! Heaps and placed resources on the hardware adapter, through both barrier paths: memory
//! requirements, placement rules, and resources aliasing one another's memory.
#![cfg(marlin_platform = "windows")]

mod support;

use marlin_rhi::{
    BufferBarrier, BufferDesc, BufferState, BufferTextureCopy, BufferUsage, ClearValue,
    DepthAttachment, DepthStencilViewDesc, Device, ErrorKind, Format, Heap, HeapDesc, HeapKind,
    INDIRECT_CONSTANT_WORD, LoadOp, MemoryLocation, PLACEMENT_ALIGNMENT, RenderPassDesc, StoreOp,
    TextureBarrier, TextureDesc, TextureState, TextureUsage,
};
use support::{
    COLUMNS, ColumnTarget, buffer, columns_pipeline, devices, read_bytes, read_words, readback,
    run, upload, words_to_bytes,
};

fn heap(device: &Device, size: u64, memory: MemoryLocation, kind: HeapKind) -> Heap {
    device
        .create_heap(&HeapDesc { size, memory, kind })
        .unwrap()
}

fn buffer_desc(size: u64, usage: BufferUsage, memory: MemoryLocation) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        memory,
    }
}

#[test]
fn memory_requirements() {
    let device = support::hardware_device();
    let b = device.buffer_requirements(&buffer_desc(
        100,
        BufferUsage::SHADER_READ,
        MemoryLocation::GpuOnly,
    ));
    assert_eq!(b.alignment, PLACEMENT_ALIGNMENT);
    assert!(b.size >= 100);

    // Small sampled textures may sit at 4 KiB; render targets take 64 KiB.
    let small = device
        .texture_requirements(&TextureDesc::new_2d(
            64,
            64,
            Format::Rgba8Unorm,
            TextureUsage::SAMPLED,
        ))
        .unwrap();
    assert_eq!(small.alignment, 4096);
    assert!(small.size >= 64 * 64 * 4);
    let target = device.texture_requirements(&ColumnTarget::desc()).unwrap();
    assert_eq!(target.alignment, PLACEMENT_ALIGNMENT);
    let large = device
        .texture_requirements(&TextureDesc::new_2d(
            2048,
            2048,
            Format::Bc7Unorm,
            TextureUsage::SAMPLED,
        ))
        .unwrap();
    assert_eq!(large.alignment, PLACEMENT_ALIGNMENT);
    assert!(large.size >= 2048 * 2048);
    // Invalid descriptions are rejected, not queried.
    let err = device
        .texture_requirements(&TextureDesc::new_2d(
            0,
            4,
            Format::Rgba8Unorm,
            TextureUsage::SAMPLED,
        ))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn placement_rules() {
    let device = support::hardware_device();
    let buffers = heap(device, 1 << 20, MemoryLocation::GpuOnly, HeapKind::Buffers);
    let textures = heap(device, 1 << 20, MemoryLocation::GpuOnly, HeapKind::Textures);
    let gpu = buffer_desc(1024, BufferUsage::SHADER_READ, MemoryLocation::GpuOnly);
    let invalid =
        |r: marlin_rhi::Result<_>| assert_eq!(r.unwrap_err().kind(), ErrorKind::InvalidInput);

    // Offsets are aligned and resources fit.
    invalid(device.create_placed_buffer(&buffers, 4096, &gpu).map(drop));
    invalid(
        device
            .create_placed_buffer(&buffers, 1 << 20, &gpu)
            .map(drop),
    );
    device
        .create_placed_buffer(&buffers, (1 << 20) - PLACEMENT_ALIGNMENT, &gpu)
        .unwrap();
    // Kinds and memory locations match.
    invalid(device.create_placed_buffer(&textures, 0, &gpu).map(drop));
    let sampled = TextureDesc::new_2d(64, 64, Format::Rgba8Unorm, TextureUsage::SAMPLED);
    invalid(
        device
            .create_placed_texture(&buffers, 0, &sampled)
            .map(drop),
    );
    invalid(
        device
            .create_placed_texture(&textures, 0, &ColumnTarget::desc())
            .map(drop),
    );
    device
        .create_placed_texture(&textures, 4096, &sampled)
        .unwrap();
    let upload_desc = buffer_desc(256, BufferUsage::COPY_SRC, MemoryLocation::Upload);
    invalid(
        device
            .create_placed_buffer(&buffers, 0, &upload_desc)
            .map(drop),
    );
    invalid(
        device
            .create_heap(&HeapDesc {
                size: 1 << 16,
                memory: MemoryLocation::Upload,
                kind: HeapKind::Textures,
            })
            .map(drop),
    );
    invalid(
        device
            .create_heap(&HeapDesc {
                size: 0,
                memory: MemoryLocation::GpuOnly,
                kind: HeapKind::Buffers,
            })
            .map(drop),
    );
    // Mixed heaps exist only where the device has them.
    let any = device.create_heap(&HeapDesc {
        size: 1 << 20,
        memory: MemoryLocation::GpuOnly,
        kind: HeapKind::Any,
    });
    match device.features().mixed_heaps {
        true => {
            let any = any.unwrap();
            device.create_placed_buffer(&any, 0, &gpu).unwrap();
            device
                .create_placed_texture(&any, PLACEMENT_ALIGNMENT, &ColumnTarget::desc())
                .unwrap();
            device
                .create_placed_texture(&any, 2 * PLACEMENT_ALIGNMENT, &sampled)
                .unwrap();
        }
        false => invalid(any.map(drop)),
    }
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn placed_buffers_alias() {
    for device in devices() {
        let heap = heap(device, 1 << 16, MemoryLocation::GpuOnly, HeapKind::Buffers);
        let desc = buffer_desc(
            256,
            BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        let a = device.create_placed_buffer(&heap, 0, &desc).unwrap();
        let b = device.create_placed_buffer(&heap, 0, &desc).unwrap();
        let first = upload(
            device,
            &words_to_bytes(&[1, 2, 3, 4]),
            BufferUsage::default(),
        );
        let second = upload(
            device,
            &words_to_bytes(&[9, 8, 7, 6]),
            BufferUsage::default(),
        );
        let out = readback(device, 32);
        run(device, |list| {
            for (buffer, src, at) in [(&a, &first, 0), (&b, &second, 16)] {
                // Taking over the memory: nothing before.
                list.buffer_barriers(&[BufferBarrier {
                    buffer,
                    before: BufferState::NONE,
                    after: BufferState::COPY_DST,
                }]);
                list.copy_buffer(src, 0, buffer, 0, 16);
                list.buffer_barriers(&[BufferBarrier {
                    buffer,
                    before: BufferState::COPY_DST,
                    after: BufferState::COPY_SRC,
                }]);
                list.copy_buffer(buffer, 0, &out, at, 16);
            }
        });
        assert_eq!(read_words(&out, 0, 8), [1, 2, 3, 4, 9, 8, 7, 6]);
        // Both name the same memory: `a` now reads what `b` wrote.
        run(device, |list| list.copy_buffer(&a, 0, &out, 0, 16));
        assert_eq!(read_words(&out, 0, 4), [9, 8, 7, 6]);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn placed_upload_buffers_map() {
    let device = support::hardware_device();
    let heap = heap(device, 1 << 16, MemoryLocation::Upload, HeapKind::Buffers);
    let staging = device
        .create_placed_buffer(
            &heap,
            0,
            &buffer_desc(64, BufferUsage::COPY_SRC, MemoryLocation::Upload),
        )
        .unwrap();
    // SAFETY: no GPU work uses it yet.
    unsafe { staging.write(0, &words_to_bytes(&[5, 6, 7])) };
    let out = readback(device, 16);
    run(device, |list| list.copy_buffer(&staging, 0, &out, 0, 12));
    assert_eq!(read_words(&out, 0, 3), [5, 6, 7]);
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn placed_resources_keep_their_heap_alive() {
    let device = support::hardware_device();
    let heap = heap(device, 1 << 16, MemoryLocation::GpuOnly, HeapKind::Buffers);
    let placed = device
        .create_placed_buffer(
            &heap,
            0,
            &buffer_desc(
                64,
                BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
                MemoryLocation::GpuOnly,
            ),
        )
        .unwrap();
    drop(heap);
    let src = upload(device, &words_to_bytes(&[42]), BufferUsage::default());
    let out = readback(device, 4);
    run(device, |list| {
        list.copy_buffer(&src, 0, &placed, 0, 4);
        list.buffer_barriers(&[BufferBarrier {
            buffer: &placed,
            before: BufferState::COPY_DST,
            after: BufferState::COPY_SRC,
        }]);
        list.copy_buffer(&placed, 0, &out, 0, 4);
    });
    assert_eq!(read_words(&out, 0, 1), [42]);
    assert_eq!(device.validation_errors(), 0);
}

/// Render targets and depth taking turns in one heap's memory, each starting from `UNDEFINED`
/// (a discard).
#[test]
fn placed_targets_alias() {
    for device in devices() {
        let heap = heap(device, 1 << 20, MemoryLocation::GpuOnly, HeapKind::Targets);
        let a = ColumnTarget::with_texture(
            device,
            device
                .create_placed_texture(&heap, 0, &ColumnTarget::desc())
                .unwrap(),
        );
        let b = ColumnTarget::with_texture(
            device,
            device
                .create_placed_texture(&heap, 0, &ColumnTarget::desc())
                .unwrap(),
        );
        let depth_desc = TextureDesc {
            clear_value: Some(ClearValue::Depth(0.0)),
            ..TextureDesc::new_2d(
                64,
                64,
                Format::D32Float,
                TextureUsage::DEPTH_STENCIL | TextureUsage::COPY_SRC,
            )
        };
        let depth = device.create_placed_texture(&heap, 0, &depth_desc).unwrap();
        let depth_view = device
            .create_depth_stencil_view(&depth, &DepthStencilViewDesc::default())
            .unwrap();
        let depth_out = readback(device, 256 * 64);
        let pipeline = columns_pipeline(device, None);

        let draw = |target: &ColumnTarget, column: u32| {
            run(device, |list| {
                target.begin(list);
                list.set_graphics_pipeline(&pipeline);
                list.push_constants(0, &[COLUMNS, 1, 0]);
                list.push_constants(INDIRECT_CONSTANT_WORD, &[column]);
                list.draw(6, 1, 0, 0);
                target.finish(list);
            });
            target.columns()
        };
        let only = |column: u32| -> Vec<u32> {
            (0..COLUMNS)
                .map(|c| if c == column { c + 1 } else { 0 })
                .collect()
        };
        assert_eq!(draw(&a, 1), only(1));
        assert_eq!(draw(&b, 2), only(2));
        run(device, |list| {
            list.texture_barriers(&[TextureBarrier::new(
                &depth,
                TextureState::UNDEFINED,
                TextureState::DEPTH_WRITE,
            )]);
            list.begin_render_pass(&RenderPassDesc {
                color: &[],
                depth: Some(DepthAttachment {
                    view: &depth_view,
                    load: LoadOp::Clear(0.0),
                    store: StoreOp::Store,
                    stencil_load: LoadOp::DontCare,
                    stencil_store: StoreOp::DontCare,
                }),
            });
            list.end_render_pass();
            list.texture_barriers(&[TextureBarrier::new(
                &depth,
                TextureState::DEPTH_WRITE,
                TextureState::COPY_SRC,
            )]);
            list.copy_texture_to_buffer(
                &depth,
                &depth_out,
                &BufferTextureCopy::whole_mip(depth.desc(), 0, 0, 0),
            );
        });
        let row = read_bytes(&depth_out, 0, 4);
        assert_eq!(f32::from_le_bytes(row.try_into().unwrap()), 0.0);
        // Back to `a`: its old contents are gone, its new draw is all there is.
        assert_eq!(draw(&a, 3), only(3));
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn placed_sampled_textures_round_trip() {
    for device in devices() {
        let heap = heap(device, 1 << 20, MemoryLocation::GpuOnly, HeapKind::Textures);
        let desc = TextureDesc::new_2d(
            16,
            16,
            Format::Rgba8Unorm,
            TextureUsage::SAMPLED | TextureUsage::COPY_DST | TextureUsage::COPY_SRC,
        );
        let req = device.texture_requirements(&desc).unwrap();
        // Two textures side by side at the small alignment.
        let textures: Vec<_> = (0..2)
            .map(|i| {
                let offset = (i * req.size).next_multiple_of(req.alignment);
                device.create_placed_texture(&heap, offset, &desc).unwrap()
            })
            .collect();
        let pixels: Vec<u32> = (0..256).collect();
        let staging = buffer(
            device,
            4096 * 2,
            BufferUsage::COPY_SRC,
            MemoryLocation::Upload,
        );
        // Rows 256 bytes apart: 16 texels of 4 bytes needs 64, aligned up to 256.
        for row in 0..16u64 {
            let words: Vec<u32> = pixels[row as usize * 16..][..16].to_vec();
            // SAFETY: no GPU work uses it yet.
            unsafe { staging.write(row * 256, &words_to_bytes(&words)) };
            let inverted: Vec<u32> = words.iter().map(|w| !w).collect();
            // SAFETY: as above.
            unsafe { staging.write(4096 + row * 256, &words_to_bytes(&inverted)) };
        }
        let out = readback(device, 4096 * 2);
        run(device, |list| {
            for (i, texture) in textures.iter().enumerate() {
                let region = BufferTextureCopy::whole_mip(texture.desc(), 0, 0, i as u64 * 4096);
                list.texture_barriers(&[TextureBarrier::new(
                    texture,
                    TextureState::UNDEFINED,
                    TextureState::COPY_DST,
                )]);
                list.copy_buffer_to_texture(&staging, texture, &region);
                list.texture_barriers(&[TextureBarrier::new(
                    texture,
                    TextureState::COPY_DST,
                    TextureState::COPY_SRC,
                )]);
                list.copy_texture_to_buffer(texture, &out, &region);
            }
        });
        for row in 0..16usize {
            let a = read_words(&out, row as u64 * 256, 16);
            let b = read_words(&out, 4096 + row as u64 * 256, 16);
            assert_eq!(a, pixels[row * 16..][..16]);
            assert_eq!(
                b,
                pixels[row * 16..][..16]
                    .iter()
                    .map(|w| !w)
                    .collect::<Vec<_>>()
            );
        }
        assert_eq!(device.validation_errors(), 0);
    }
}
