//! Sparse buffers and textures bound tile by tile, and CPU-written device-local memory. Hardware
//! adapter, each barrier path, validation on; skipped where the device lacks them.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    BufferBarrier, BufferDesc, BufferState, BufferTextureCopy, BufferUsage, ClearColor, Format,
    HeapDesc, HeapKind, MemoryLocation, SPARSE_TILE_SIZE, SparseBind, SparseTextureRegion,
    SubresourceRange, TextureBarrier, TextureDesc, TextureState, TextureUsage,
};
use support::{devices, read_words, readback, run};

#[test]
fn sparse_buffers_read_zero_where_unbound() {
    for device in devices() {
        support::require_feature!(device, sparse_resources);
        let sparse = device
            .create_sparse_buffer(&BufferDesc {
                size: 4 * SPARSE_TILE_SIZE,
                usage: BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
                memory: MemoryLocation::GpuOnly,
            })
            .unwrap();
        assert!(sparse.is_sparse());
        let heap = device
            .create_heap(&HeapDesc {
                size: 2 * SPARSE_TILE_SIZE,
                memory: MemoryLocation::GpuOnly,
                kind: HeapKind::Buffers,
            })
            .unwrap();
        // SAFETY: the heap outlives the bindings; no work uses the buffer yet.
        unsafe {
            device
                .queue()
                .bind_sparse(&[
                    SparseBind::Buffer {
                        buffer: &sparse,
                        tile_offset: 0,
                        tile_count: 1,
                        memory: Some((&heap, 0)),
                    },
                    SparseBind::Buffer {
                        buffer: &sparse,
                        tile_offset: 2,
                        tile_count: 1,
                        memory: Some((&heap, 1)),
                    },
                ])
                .unwrap()
        };
        let out = readback(device, 4 * SPARSE_TILE_SIZE);
        run(device, |list| {
            list.buffer_barriers(&[BufferBarrier {
                buffer: &sparse,
                before: BufferState::NONE,
                after: BufferState::CLEAR_STORAGE,
            }]);
            // Writes to unbound tiles are dropped.
            list.fill_buffer(&sparse, 0, 4 * SPARSE_TILE_SIZE, 0xabab_abab);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &sparse,
                before: BufferState::CLEAR_STORAGE,
                after: BufferState::COPY_SRC,
            }]);
            list.copy_buffer(&sparse, 0, &out, 0, 4 * SPARSE_TILE_SIZE);
        });
        let words_per_tile = (SPARSE_TILE_SIZE / 4) as usize;
        for tile in 0..4u64 {
            let words = read_words(&out, tile * SPARSE_TILE_SIZE, words_per_tile);
            let expect = if tile % 2 == 0 { 0xabab_abab } else { 0 };
            assert!(words.iter().all(|&w| w == expect), "tile {tile}");
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn sparse_textures_bind_tiles_and_packed_mips() {
    for device in devices() {
        support::require_feature!(device, sparse_resources);
        let desc = TextureDesc {
            mip_levels: 10,
            ..TextureDesc::new_2d(
                512,
                512,
                Format::Rgba8Unorm,
                TextureUsage::STORAGE | TextureUsage::SAMPLED | TextureUsage::COPY_SRC,
            )
        };
        let texture = device.create_sparse_texture(&desc).unwrap();
        let layout = device.sparse_texture_layout(&texture);
        // 64 KiB of 4-byte texels: 128 x 128 tiles.
        assert_eq!(layout.tile_extent, [128, 128, 1], "{layout:?}");
        assert!(
            layout.standard_mips >= 1 && layout.standard_mips < 10,
            "{layout:?}"
        );
        let heap = device
            .create_heap(&HeapDesc {
                size: (1 + layout.packed_mip_tiles as u64) * SPARSE_TILE_SIZE,
                memory: MemoryLocation::GpuOnly,
                kind: HeapKind::Textures,
            })
            .unwrap();
        let mut binds = vec![SparseBind::Texture {
            texture: &texture,
            region: SparseTextureRegion::Tiles {
                mip: 0,
                layer: 0,
                origin: [1, 1, 0],
                size: [1, 1, 1],
            },
            memory: Some((&heap, 0)),
        }];
        if layout.packed_mip_tiles > 0 {
            binds.push(SparseBind::Texture {
                texture: &texture,
                region: SparseTextureRegion::PackedMips { layer: 0 },
                memory: Some((&heap, 1)),
            });
        }
        // SAFETY: the heap outlives the bindings; no work uses the texture yet.
        unsafe { device.queue().bind_sparse(&binds).unwrap() };
        let out = readback(device, 512 * 2048);
        run(device, |list| {
            list.texture_barriers(&[TextureBarrier::new(
                &texture,
                TextureState::UNDEFINED,
                TextureState::CLEAR_STORAGE,
            )]);
            list.clear_storage_texture(
                &texture,
                &SubresourceRange {
                    mip_count: 1,
                    ..SubresourceRange::ALL
                },
                ClearColor::Float([1.0; 4]),
            );
            list.texture_barriers(&[TextureBarrier::new(
                &texture,
                TextureState::CLEAR_STORAGE,
                TextureState::COPY_SRC,
            )]);
            list.copy_texture_to_buffer(
                &texture,
                &out,
                &BufferTextureCopy::whole_mip(&desc, 0, 0, 0),
            );
        });
        let texel = |x: u64, y: u64| read_words(&out, y * 2048 + x * 4, 1)[0];
        assert_eq!(texel(130, 200), 0xffff_ffff, "inside the bound tile");
        assert_eq!(texel(5, 5), 0, "an unbound tile");
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn gpu_upload_buffers_are_written_by_the_cpu() {
    let device = support::hardware_device();
    support::require_feature!(device, gpu_upload_heap);
    let shared = support::buffer(
        device,
        256,
        BufferUsage::COPY_SRC,
        MemoryLocation::GpuUpload,
    );
    let words: Vec<u32> = (0..64).collect();
    // SAFETY: no GPU work uses it yet.
    unsafe { shared.write(0, &support::words_to_bytes(&words)) };
    let out = readback(device, 256);
    run(device, |list| list.copy_buffer(&shared, 0, &out, 0, 256));
    assert_eq!(read_words(&out, 0, 64), words);
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn sparse_and_gpu_upload_need_their_features() {
    let device = support::hardware_device();
    let features = device.features();
    let gpu_upload = device.create_buffer(&BufferDesc {
        size: 64,
        usage: BufferUsage::COPY_SRC,
        memory: MemoryLocation::GpuUpload,
    });
    assert_eq!(gpu_upload.is_ok(), features.gpu_upload_heap);
    let sparse_depth = device.create_sparse_texture(&TextureDesc::new_2d(
        256,
        256,
        Format::D32Float,
        TextureUsage::DEPTH_STENCIL,
    ));
    assert!(sparse_depth.is_err());
}

#[test]
fn sparse_texture_binds_stay_in_the_texture() {
    let device = support::hardware_device();
    support::require_feature!(device, sparse_resources);
    let desc = TextureDesc {
        mip_levels: 10,
        ..TextureDesc::new_2d(512, 512, Format::Rgba8Unorm, TextureUsage::SAMPLED)
    };
    let texture = device.create_sparse_texture(&desc).unwrap();
    let layout = device.sparse_texture_layout(&texture);
    assert_eq!(layout.tile_extent, [128, 128, 1], "{layout:?}");
    let heap = device
        .create_heap(&HeapDesc {
            size: SPARSE_TILE_SIZE,
            memory: MemoryLocation::GpuOnly,
            kind: HeapKind::Textures,
        })
        .unwrap();
    let bind_at = |region, first| {
        support::panic_message(|| {
            // SAFETY: refused before anything is bound.
            let _ = unsafe {
                device.queue().bind_sparse(&[SparseBind::Texture {
                    texture: &texture,
                    region,
                    memory: Some((&heap, first)),
                }])
            };
        })
    };
    let bind = |region| bind_at(region, 0);
    let tiles = |mip, origin, size| SparseTextureRegion::Tiles {
        mip,
        layer: 0,
        origin,
        size,
    };
    // Mip 0 is 4 x 4 tiles.
    assert_eq!(
        bind(tiles(0, [3, 0, 0], [2, 1, 1])),
        "sparse texture tiles out of range"
    );
    assert_eq!(
        bind(tiles(0, [u32::MAX, 0, 0], [1, 1, 1])),
        "sparse texture tiles out of range"
    );
    assert_eq!(
        bind(tiles(0, [0, 0, 0], [1, 1 << 16, 1])),
        "sparse texture tiles out of range"
    );
    assert_eq!(
        bind(tiles(layout.standard_mips, [0, 0, 0], [1, 1, 1])),
        "sparse subresource out of range or packed"
    );
    // Packed mips bind all their tiles: none fit after the heap's only tile.
    if layout.packed_mip_tiles > 0 {
        assert_eq!(
            bind_at(SparseTextureRegion::PackedMips { layer: 0 }, 1),
            "heap tiles out of range"
        );
    }
}
