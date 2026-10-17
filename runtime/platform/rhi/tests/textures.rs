//! Textures on the hardware adapter, through both barrier paths: volume textures, the integer,
//! 16-bit and BC6H signed formats, texture-to-texture copies, and depth-stencil with stencil
//! testing.
#![cfg(marlin_platform = "windows")]

mod support;

use marlin_rhi::{
    BufferBarrier, BufferState, BufferTextureCopy, BufferUsage, BufferViewDesc, BufferViewKind,
    ClearValue, ColorAttachment, CompareOp, ComputePipelineDesc, DepthAttachment, DepthState,
    DepthStencilViewDesc, Device, ErrorKind, Format, INDIRECT_CONSTANT_WORD, LoadOp,
    MemoryLocation, Rect, RenderPassDesc, RenderTargetViewDesc, StencilFace, StencilOp,
    StencilState, StoreOp, SubresourceRange, Texture, TextureBarrier, TextureCopy, TextureDesc,
    TextureDimension, TextureState, TextureUsage, TextureViewDesc, TextureViewDimension, Viewport,
    aligned_row_pitch,
};
use support::{
    COLUMN_HEIGHT, COLUMN_WIDTH, COLUMNS, ColumnTarget, Stage, buffer, columns_pipeline, devices,
    read_bytes, read_words, readback, run, shader, upload, words_to_bytes,
};

fn barrier(
    list: &mut marlin_rhi::CommandList,
    texture: &Texture,
    before: TextureState,
    after: TextureState,
) {
    list.texture_barriers(&[TextureBarrier::new(texture, before, after)]);
}

// --- Volume textures ---

const VOLUME: u32 = 8;

fn texel(x: u32, y: u32, z: u32) -> u32 {
    x + 16 * y + 256 * z + 1
}

#[test]
fn volume_textures_are_written_read_and_copied() {
    for device in devices() {
        let volume = device
            .create_texture(&TextureDesc::new_3d(
                VOLUME,
                VOLUME,
                VOLUME,
                Format::R32Uint,
                TextureUsage::STORAGE
                    | TextureUsage::SAMPLED
                    | TextureUsage::COPY_SRC
                    | TextureUsage::COPY_DST,
            ))
            .unwrap();
        assert_eq!(volume.desc().dimension, TextureDimension::D3);
        let write_view = device
            .create_texture_view(
                &volume,
                &TextureViewDesc {
                    writable: true,
                    dimension: TextureViewDimension::D3,
                    format: None,
                    range: SubresourceRange::ALL,
                },
            )
            .unwrap();
        let read_view = device
            .create_texture_view(
                &volume,
                &TextureViewDesc {
                    dimension: TextureViewDimension::D3,
                    ..TextureViewDesc::SAMPLED
                },
            )
            .unwrap();
        let count = (VOLUME * VOLUME * VOLUME) as u64;
        let loaded = buffer(
            device,
            count * 4,
            BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
            MemoryLocation::GpuOnly,
        );
        let loaded_view = device
            .create_buffer_view(&loaded, &BufferViewDesc::whole(BufferViewKind::Raw, true))
            .unwrap();
        let write = device
            .create_compute_pipeline(&ComputePipelineDesc {
                shader: shader("volume.write", Stage::Compute),
                cache: None,
            })
            .unwrap();
        let read = device
            .create_compute_pipeline(&ComputePipelineDesc {
                shader: shader("volume.read", Stage::Compute),
                cache: None,
            })
            .unwrap();
        // Slices 2..5 straight to a buffer: 8 rows of 32 bytes, 256 apart, per slice.
        let slices = readback(device, 256 * VOLUME as u64 * 3);
        let shader_out = readback(device, count * 4);
        // A second volume gets a box of the first.
        let other = device
            .create_texture(&TextureDesc::new_3d(
                VOLUME,
                VOLUME,
                VOLUME,
                Format::R32Uint,
                TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
            ))
            .unwrap();
        let other_out = readback(device, 256 * VOLUME as u64 * VOLUME as u64);
        run(device, |list| {
            barrier(
                list,
                &volume,
                TextureState::UNDEFINED,
                TextureState::COMPUTE_WRITE,
            );
            list.set_compute_pipeline(&write);
            list.push_constants(0, &[write_view.index(), 0, VOLUME]);
            list.dispatch(VOLUME / 4, VOLUME / 4, VOLUME / 4);
            barrier(
                list,
                &volume,
                TextureState::COMPUTE_WRITE,
                TextureState::SHADER_READ,
            );
            list.set_compute_pipeline(&read);
            list.push_constants(0, &[read_view.index(), loaded_view.index(), VOLUME]);
            list.dispatch(VOLUME / 4, VOLUME / 4, VOLUME / 4);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &loaded,
                before: BufferState::COMPUTE_WRITE,
                after: BufferState::COPY_SRC,
            }]);
            list.copy_buffer(&loaded, 0, &shader_out, 0, count * 4);

            barrier(
                list,
                &volume,
                TextureState::SHADER_READ,
                TextureState::COPY_SRC,
            );
            list.copy_texture_to_buffer(
                &volume,
                &slices,
                &BufferTextureCopy {
                    buffer_offset: 0,
                    bytes_per_row: 256,
                    mip: 0,
                    layer: 0,
                    x: 0,
                    y: 0,
                    z: 2,
                    width: VOLUME,
                    height: VOLUME,
                    depth: 3,
                },
            );
            barrier(
                list,
                &other,
                TextureState::UNDEFINED,
                TextureState::COPY_DST,
            );
            // Texels (1..4, 2..6, 3..5) to (4.., 0.., 6..).
            list.copy_texture(
                &volume,
                &other,
                &TextureCopy {
                    src_mip: 0,
                    src_layer: 0,
                    src_offset: [1, 2, 3],
                    dst_mip: 0,
                    dst_layer: 0,
                    dst_offset: [4, 0, 6],
                    size: [3, 4, 2],
                },
            );
            barrier(list, &other, TextureState::COPY_DST, TextureState::COPY_SRC);
            list.copy_texture_to_buffer(
                &other,
                &other_out,
                &BufferTextureCopy::whole_mip(other.desc(), 0, 0, 0),
            );
        });

        let words = read_words(&shader_out, 0, count as usize);
        for z in 0..VOLUME {
            for y in 0..VOLUME {
                for x in 0..VOLUME {
                    let i = ((z * VOLUME + y) * VOLUME + x) as usize;
                    assert_eq!(words[i], texel(x, y, z), "loaded ({x}, {y}, {z})");
                }
            }
        }
        for s in 0..3 {
            for y in 0..VOLUME {
                let row = read_words(&slices, (s * VOLUME + y) as u64 * 256, VOLUME as usize);
                let expected: Vec<u32> = (0..VOLUME).map(|x| texel(x, y, s + 2)).collect();
                assert_eq!(row, expected, "slice {} row {y}", s + 2);
            }
        }
        for (dz, sz) in [(6, 3), (7, 4)] {
            for (dy, sy) in (0..4).zip(2..6) {
                let row = read_words(&other_out, (dz * VOLUME + dy) as u64 * 256, VOLUME as usize);
                let expected: Vec<u32> = (1..4).map(|sx| texel(sx, sy, sz)).collect();
                assert_eq!(row[4..7], expected, "copied slice {dz} row {dy}");
            }
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn volume_mips_round_trip() {
    for device in devices() {
        let desc = TextureDesc {
            mip_levels: 4,
            ..TextureDesc::new_3d(
                16,
                8,
                8,
                Format::Rgba8Unorm,
                TextureUsage::SAMPLED | TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
            )
        };
        let volume = device.create_texture(&desc).unwrap();
        // Mip 1: 8 × 4 × 4, rows of 32 bytes padded to 256, 4 rows per slice.
        let region = BufferTextureCopy::whole_mip(&desc, 1, 0, 0);
        assert_eq!((region.width, region.height, region.depth), (8, 4, 4));
        let size = region.bytes_per_row as u64 * 4 * 4;
        let texels: Vec<u32> = (0..size as u32 / 4)
            .map(|i| i.wrapping_mul(2654435761))
            .collect();
        let staging = upload(device, &words_to_bytes(&texels), BufferUsage::default());
        let out = readback(device, size);
        run(device, |list| {
            barrier(
                list,
                &volume,
                TextureState::UNDEFINED,
                TextureState::COPY_DST,
            );
            list.copy_buffer_to_texture(&staging, &volume, &region);
            barrier(
                list,
                &volume,
                TextureState::COPY_DST,
                TextureState::COPY_SRC,
            );
            list.copy_texture_to_buffer(&volume, &out, &region);
        });
        for row in 0..16u64 {
            assert_eq!(
                read_words(&out, row * 256, 8),
                texels[row as usize * 64..][..8],
                "row {row}"
            );
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn volume_texture_rules() {
    let device = support::hardware_device();
    let invalid = |desc: TextureDesc| {
        let err = device.create_texture(&desc).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput, "{desc:?}");
    };
    let sampled = TextureUsage::SAMPLED;
    invalid(TextureDesc::new_3d(4, 4, 0, Format::Rgba8Unorm, sampled));
    invalid(TextureDesc::new_3d(4, 4, 4096, Format::Rgba8Unorm, sampled));
    invalid(TextureDesc::new_3d(
        4,
        4,
        4,
        Format::D32Float,
        TextureUsage::DEPTH_STENCIL,
    ));
    invalid(TextureDesc::new_3d(
        4,
        4,
        4,
        Format::Rgba8Unorm,
        TextureUsage::RENDER_TARGET,
    ));
    invalid(TextureDesc {
        array_layers: 2,
        ..TextureDesc::new_3d(4, 4, 4, Format::Rgba8Unorm, sampled)
    });
    // Mips run down the largest of the three sides.
    device
        .create_texture(&TextureDesc {
            mip_levels: 6,
            ..TextureDesc::new_3d(4, 4, 32, Format::Rgba8Unorm, sampled)
        })
        .unwrap();
    invalid(TextureDesc {
        mip_levels: 7,
        ..TextureDesc::new_3d(4, 4, 32, Format::Rgba8Unorm, sampled)
    });
    invalid(TextureDesc {
        depth: 2,
        ..TextureDesc::new_2d(4, 4, Format::Rgba8Unorm, sampled)
    });
    // 3D views only on volumes, and volumes only through 3D views.
    let volume = device
        .create_texture(&TextureDesc::new_3d(4, 4, 4, Format::Rgba8Unorm, sampled))
        .unwrap();
    let flat = device
        .create_texture(&TextureDesc::new_2d(4, 4, Format::Rgba8Unorm, sampled))
        .unwrap();
    let d3 = TextureViewDesc {
        dimension: TextureViewDimension::D3,
        ..TextureViewDesc::SAMPLED
    };
    assert_eq!(
        device.create_texture_view(&flat, &d3).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        device
            .create_texture_view(&volume, &TextureViewDesc::SAMPLED)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    device.create_texture_view(&volume, &d3).unwrap();
    assert_eq!(device.validation_errors(), 0);
}

// --- Formats ---

/// Clears a 4 × 4 target of `format` to `color` and returns its first texel's bytes.
fn cleared_texel(device: &Device, format: Format, color: [f32; 4]) -> Vec<u8> {
    let texture = device
        .create_texture(&TextureDesc {
            clear_value: Some(ClearValue::Color(color)),
            ..TextureDesc::new_2d(
                4,
                4,
                format,
                TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC | TextureUsage::SAMPLED,
            )
        })
        .unwrap();
    let view = device
        .create_render_target_view(&texture, &RenderTargetViewDesc::default())
        .unwrap();
    // A sampled view of every new format, too.
    device
        .create_texture_view(&texture, &TextureViewDesc::SAMPLED)
        .unwrap();
    let out = readback(device, 256 * 4);
    run(device, |list| {
        barrier(
            list,
            &texture,
            TextureState::UNDEFINED,
            TextureState::RENDER_TARGET,
        );
        list.begin_render_pass(&RenderPassDesc {
            color: &[ColorAttachment {
                view: &view,
                load: LoadOp::Clear(color),
                store: StoreOp::Store,
            }],
            depth: None,
        });
        list.end_render_pass();
        barrier(
            list,
            &texture,
            TextureState::RENDER_TARGET,
            TextureState::COPY_SRC,
        );
        list.copy_texture_to_buffer(
            &texture,
            &out,
            &BufferTextureCopy::whole_mip(texture.desc(), 0, 0, 0),
        );
    });
    read_bytes(&out, 0, format.block_size() as usize)
}

fn u16s(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn new_formats_render_and_read_back() {
    for device in devices() {
        let cases: [(Format, [f32; 4], Vec<u8>); 8] = [
            (Format::R8Uint, [7.0, 0.0, 0.0, 0.0], vec![7]),
            (Format::R16Uint, [1234.0, 0.0, 0.0, 0.0], u16s(&[1234])),
            (Format::R16Unorm, [1.0, 0.0, 0.0, 0.0], u16s(&[0xffff])),
            (
                Format::Rg16Uint,
                [3.0, 60000.0, 0.0, 0.0],
                u16s(&[3, 60000]),
            ),
            (
                Format::Rgba16Unorm,
                [1.0, 0.0, 1.0, 0.0],
                u16s(&[0xffff, 0, 0xffff, 0]),
            ),
            (
                Format::Rgba16Uint,
                [1.0, 2.0, 3.0, 4.0],
                u16s(&[1, 2, 3, 4]),
            ),
            (
                Format::Rg32Uint,
                [100000.0, 7.0, 0.0, 0.0],
                words_to_bytes(&[100000, 7]),
            ),
            (
                Format::Rgba32Uint,
                [1.0, 2.0, 3.0, 16777216.0],
                words_to_bytes(&[1, 2, 3, 16777216]),
            ),
        ];
        for (format, color, expected) in cases {
            assert_eq!(expected.len(), format.block_size() as usize, "{format:?}");
            assert_eq!(cleared_texel(device, format, color), expected, "{format:?}");
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn integer_formats_are_integer() {
    for format in [
        Format::R8Uint,
        Format::R16Uint,
        Format::Rg16Uint,
        Format::Rgba16Uint,
        Format::R32Uint,
        Format::Rg32Uint,
        Format::Rgba32Uint,
    ] {
        assert!(format.is_integer(), "{format:?}");
    }
    for format in [Format::R16Unorm, Format::Rgba16Unorm, Format::Rgba16Float] {
        assert!(!format.is_integer(), "{format:?}");
    }
    assert!(Format::D32FloatS8.is_depth() && Format::D32FloatS8.has_stencil());
    assert!(!Format::D32Float.has_stencil());
    assert_eq!(Format::D32FloatS8.copy_block_size(), 4);
    assert_eq!(aligned_row_pitch(Format::D32FloatS8, 100), 512);
    assert!(Format::Bc6hSfloat.is_compressed());
}

#[test]
fn bc6h_signed_blocks_round_trip() {
    for device in devices() {
        let desc = TextureDesc::new_2d(
            8,
            4,
            Format::Bc6hSfloat,
            TextureUsage::SAMPLED | TextureUsage::COPY_DST | TextureUsage::COPY_SRC,
        );
        let texture = device.create_texture(&desc).unwrap();
        device
            .create_texture_view(&texture, &TextureViewDesc::SAMPLED)
            .unwrap();
        // Two arbitrary 16-byte blocks: copies move blocks verbatim.
        let blocks: Vec<u8> = (0..32u8).map(|i| i.wrapping_mul(37) ^ 0x5a).collect();
        let staging = upload(device, &blocks, BufferUsage::default());
        let out = readback(device, 32);
        let region = BufferTextureCopy::whole_mip(&desc, 0, 0, 0);
        run(device, |list| {
            barrier(
                list,
                &texture,
                TextureState::UNDEFINED,
                TextureState::COPY_DST,
            );
            list.copy_buffer_to_texture(&staging, &texture, &region);
            barrier(
                list,
                &texture,
                TextureState::COPY_DST,
                TextureState::COPY_SRC,
            );
            list.copy_texture_to_buffer(&texture, &out, &region);
        });
        assert_eq!(read_bytes(&out, 0, 32), blocks);
        assert_eq!(device.validation_errors(), 0);
    }
}

// --- Texture copies ---

/// Texel (x, y) of layer `l`, mip `m` of the copy source.
fn pattern(l: u32, m: u32, x: u32, y: u32) -> u32 {
    l << 24 | m << 16 | y << 8 | x
}

#[test]
fn copies_boxes_between_textures() {
    for device in devices() {
        const SIZE: u32 = 16;
        let usage = TextureUsage::COPY_SRC | TextureUsage::COPY_DST;
        let src_desc = TextureDesc {
            array_layers: 2,
            mip_levels: 2,
            ..TextureDesc::new_2d(SIZE, SIZE, Format::Rgba8Unorm, usage)
        };
        let src = device.create_texture(&src_desc).unwrap();
        // The sRGB twin of the source's format.
        let dst_desc = TextureDesc::new_2d(SIZE, SIZE, Format::Rgba8UnormSrgb, usage);
        let dst = device.create_texture(&dst_desc).unwrap();
        // Source subresources at 4 KiB apart, then 4 KiB of zeros for the destination.
        let mut staging_words = vec![0u32; 5 * 1024];
        for (s, (l, m)) in [(0, 0), (0, 1), (1, 0), (1, 1)].into_iter().enumerate() {
            let (w, h) = src_desc.mip_size(m);
            for y in 0..h {
                for x in 0..w {
                    staging_words[s * 1024 + (y * 64 + x) as usize] = pattern(l, m, x, y);
                }
            }
        }
        let staging = upload(
            device,
            &words_to_bytes(&staging_words),
            BufferUsage::default(),
        );
        let out = readback(device, 4096);
        run(device, |list| {
            barrier(list, &src, TextureState::UNDEFINED, TextureState::COPY_DST);
            barrier(list, &dst, TextureState::UNDEFINED, TextureState::COPY_DST);
            for (s, (l, m)) in [(0, 0), (0, 1), (1, 0), (1, 1)].into_iter().enumerate() {
                list.copy_buffer_to_texture(
                    &staging,
                    &src,
                    &BufferTextureCopy::whole_mip(&src_desc, m, l, s as u64 * 4096),
                );
            }
            list.copy_buffer_to_texture(
                &staging,
                &dst,
                &BufferTextureCopy::whole_mip(&dst_desc, 0, 0, 4 * 4096),
            );
            barrier(list, &src, TextureState::COPY_DST, TextureState::COPY_SRC);
            // Layer 1, mip 0: an 8 × 4 box from (4, 2) to (1, 3).
            list.copy_texture(
                &src,
                &dst,
                &TextureCopy {
                    src_mip: 0,
                    src_layer: 1,
                    src_offset: [4, 2, 0],
                    dst_mip: 0,
                    dst_layer: 0,
                    dst_offset: [1, 3, 0],
                    size: [8, 4, 1],
                },
            );
            // Layer 0, mip 1 (8 × 8) whole, to (8, 8).
            list.copy_texture(
                &src,
                &dst,
                &TextureCopy {
                    src_mip: 1,
                    src_layer: 0,
                    src_offset: [0; 3],
                    dst_mip: 0,
                    dst_layer: 0,
                    dst_offset: [8, 8, 0],
                    size: [8, 8, 1],
                },
            );
            barrier(list, &dst, TextureState::COPY_DST, TextureState::COPY_SRC);
            list.copy_texture_to_buffer(
                &dst,
                &out,
                &BufferTextureCopy::whole_mip(&dst_desc, 0, 0, 0),
            );
        });
        for y in 0..SIZE {
            let row = read_words(&out, y as u64 * 256, SIZE as usize);
            for x in 0..SIZE {
                let expected = if (1..9).contains(&x) && (3..7).contains(&y) {
                    pattern(1, 0, x - 1 + 4, y - 3 + 2)
                } else if x >= 8 && y >= 8 {
                    pattern(0, 1, x - 8, y - 8)
                } else {
                    0
                };
                assert_eq!(row[x as usize], expected, "({x}, {y})");
            }
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn copies_compressed_blocks_between_textures() {
    for device in devices() {
        let usage = TextureUsage::COPY_SRC | TextureUsage::COPY_DST | TextureUsage::SAMPLED;
        let desc = TextureDesc::new_2d(8, 8, Format::Bc1Unorm, usage);
        let src = device.create_texture(&desc).unwrap();
        let dst = device.create_texture(&desc).unwrap();
        // 2 × 2 blocks of 8 bytes; rows of blocks 256 apart.
        let mut bytes = vec![0u8; 512];
        for (i, b) in bytes.iter_mut().enumerate() {
            if i % 256 < 16 {
                *b = i as u8 ^ 0xc3;
            }
        }
        let staging = upload(device, &bytes, BufferUsage::default());
        let out = readback(device, 512);
        let region = BufferTextureCopy::whole_mip(&desc, 0, 0, 0);
        run(device, |list| {
            barrier(list, &src, TextureState::UNDEFINED, TextureState::COPY_DST);
            barrier(list, &dst, TextureState::UNDEFINED, TextureState::COPY_DST);
            list.copy_buffer_to_texture(&staging, &src, &region);
            list.copy_buffer_to_texture(&staging, &dst, &region);
            barrier(list, &src, TextureState::COPY_DST, TextureState::COPY_SRC);
            // The bottom-right block onto the top-left one.
            list.copy_texture(
                &src,
                &dst,
                &TextureCopy {
                    src_mip: 0,
                    src_layer: 0,
                    src_offset: [4, 4, 0],
                    dst_mip: 0,
                    dst_layer: 0,
                    dst_offset: [0; 3],
                    size: [4, 4, 1],
                },
            );
            barrier(list, &dst, TextureState::COPY_DST, TextureState::COPY_SRC);
            list.copy_texture_to_buffer(&dst, &out, &region);
        });
        let result = read_bytes(&out, 0, 512);
        assert_eq!(result[0..8], bytes[256 + 8..256 + 16], "moved block");
        assert_eq!(result[8..16], bytes[8..16], "untouched block");
        assert_eq!(result[256..272], bytes[256..272], "untouched row");
        assert_eq!(device.validation_errors(), 0);
    }
}

fn copy_list(device: &Device) -> (marlin_rhi::CommandPool, marlin_rhi::CommandList) {
    let mut pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    list.begin(&mut pool).unwrap();
    (pool, list)
}

#[test]
#[should_panic(expected = "copy between incompatible formats")]
fn texture_copies_keep_the_format() {
    let device = support::hardware_device();
    let usage = TextureUsage::COPY_SRC | TextureUsage::COPY_DST;
    let a = device
        .create_texture(&TextureDesc::new_2d(4, 4, Format::Rgba8Unorm, usage))
        .unwrap();
    let b = device
        .create_texture(&TextureDesc::new_2d(4, 4, Format::R32Uint, usage))
        .unwrap();
    let (_pool, mut list) = copy_list(device);
    list.copy_texture(
        &a,
        &b,
        &TextureCopy {
            src_mip: 0,
            src_layer: 0,
            src_offset: [0; 3],
            dst_mip: 0,
            dst_layer: 0,
            dst_offset: [0; 3],
            size: [4, 4, 1],
        },
    );
}

#[test]
#[should_panic(expected = "copy source out of range")]
fn compressed_copies_are_block_aligned() {
    let device = support::hardware_device();
    let usage = TextureUsage::COPY_SRC | TextureUsage::COPY_DST;
    let desc = TextureDesc::new_2d(8, 8, Format::Bc7Unorm, usage);
    let a = device.create_texture(&desc).unwrap();
    let b = device.create_texture(&desc).unwrap();
    let (_pool, mut list) = copy_list(device);
    list.copy_texture(
        &a,
        &b,
        &TextureCopy {
            src_mip: 0,
            src_layer: 0,
            src_offset: [2, 0, 0],
            dst_mip: 0,
            dst_layer: 0,
            dst_offset: [0; 3],
            size: [4, 4, 1],
        },
    );
}

// --- Depth-stencil ---

fn depth_stencil_texture(device: &Device, usage: TextureUsage) -> Texture {
    device
        .create_texture(&TextureDesc {
            clear_value: Some(ClearValue::Depth(0.0)),
            ..TextureDesc::new_2d(
                COLUMNS * COLUMN_WIDTH,
                COLUMN_HEIGHT,
                Format::D32FloatS8,
                TextureUsage::DEPTH_STENCIL | usage,
            )
        })
        .unwrap()
}

fn stencil(compare: CompareOp, pass: StencilOp) -> DepthState {
    let face = StencilFace {
        compare,
        pass,
        ..StencilFace::default()
    };
    DepthState {
        format: Format::D32FloatS8,
        write: true,
        compare: CompareOp::Always,
        stencil: Some(StencilState {
            front: face,
            back: face,
            ..StencilState::default()
        }),
        bounds_test: false,
    }
}

/// Starts a pass over `target` and `depth`, clearing color to 0 and (when given) depth to 0 and
/// stencil to `clear_stencil`.
fn begin(
    list: &mut marlin_rhi::CommandList,
    target: &ColumnTarget,
    depth: &marlin_rhi::DepthStencilView,
    clear_stencil: Option<u8>,
) {
    list.begin_render_pass(&RenderPassDesc {
        color: &[ColorAttachment {
            view: &target.view,
            load: LoadOp::Clear([0.0; 4]),
            store: StoreOp::Store,
        }],
        depth: Some(DepthAttachment {
            view: depth,
            load: match clear_stencil {
                Some(_) => LoadOp::Clear(0.0),
                None => LoadOp::Load,
            },
            store: StoreOp::Store,
            stencil_load: clear_stencil.map_or(LoadOp::Load, LoadOp::Clear),
            stencil_store: StoreOp::Store,
        }),
    });
    let (w, h) = (COLUMNS * COLUMN_WIDTH, COLUMN_HEIGHT);
    list.set_viewport(&Viewport::full(w, h));
    list.set_scissor(&Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    });
}

/// Draws columns `first..first + span`, valued `first + 1 + offset`.
fn columns(list: &mut marlin_rhi::CommandList, first: u32, span: u32, offset: u32) {
    list.push_constants(0, &[COLUMNS, span, offset]);
    list.push_constants(INDIRECT_CONSTANT_WORD, &[first]);
    list.draw(6, 1, 0, 0);
}

#[test]
fn stencil_masks_draws_and_copies_with_the_depth() {
    for device in devices() {
        let mark = columns_pipeline(device, Some(stencil(CompareOp::Always, StencilOp::Replace)));
        let masked = columns_pipeline(device, Some(stencil(CompareOp::Equal, StencilOp::Keep)));
        let depth = depth_stencil_texture(device, TextureUsage::COPY_SRC | TextureUsage::SAMPLED);
        let depth_view = device
            .create_depth_stencil_view(&depth, &DepthStencilViewDesc::default())
            .unwrap();
        // The depth plane is sampled as R32Float.
        device
            .create_texture_view(&depth, &TextureViewDesc::SAMPLED)
            .unwrap();
        let copy = depth_stencil_texture(device, TextureUsage::COPY_DST);
        let copy_view = device
            .create_depth_stencil_view(&copy, &DepthStencilViewDesc::default())
            .unwrap();
        let first = ColumnTarget::new(device);
        let second = ColumnTarget::new(device);
        let depth_out = readback(device, 512 * COLUMN_HEIGHT as u64);

        run(device, |list| {
            barrier(
                list,
                &first.texture,
                TextureState::UNDEFINED,
                TextureState::RENDER_TARGET,
            );
            barrier(
                list,
                &depth,
                TextureState::UNDEFINED,
                TextureState::DEPTH_WRITE,
            );
            begin(list, &first, &depth_view, Some(0));
            // Mark columns 2..6 with stencil 5, then draw everywhere stencil is 5.
            list.set_graphics_pipeline(&mark);
            list.set_stencil_reference(5);
            columns(list, 2, 4, 0);
            list.set_graphics_pipeline(&masked);
            columns(list, 0, COLUMNS, 100);
            // A reference matching nothing draws nothing.
            list.set_stencil_reference(6);
            columns(list, 0, COLUMNS, 50);
            first.finish(list);

            // Both planes move to the copy; masking through it marks the same columns.
            barrier(
                list,
                &depth,
                TextureState::DEPTH_WRITE,
                TextureState::COPY_SRC,
            );
            barrier(list, &copy, TextureState::UNDEFINED, TextureState::COPY_DST);
            list.copy_texture(
                &depth,
                &copy,
                &TextureCopy {
                    src_mip: 0,
                    src_layer: 0,
                    src_offset: [0; 3],
                    dst_mip: 0,
                    dst_layer: 0,
                    dst_offset: [0; 3],
                    size: [COLUMNS * COLUMN_WIDTH, COLUMN_HEIGHT, 1],
                },
            );
            list.copy_texture_to_buffer(
                &depth,
                &depth_out,
                &BufferTextureCopy::whole_mip(depth.desc(), 0, 0, 0),
            );
            barrier(
                list,
                &copy,
                TextureState::COPY_DST,
                TextureState::DEPTH_WRITE,
            );
            barrier(
                list,
                &second.texture,
                TextureState::UNDEFINED,
                TextureState::RENDER_TARGET,
            );
            begin(list, &second, &copy_view, None);
            list.set_graphics_pipeline(&masked);
            list.set_stencil_reference(5);
            columns(list, 0, COLUMNS, 200);
            second.finish(list);
        });

        let marked = |c: u32| (2..6).contains(&c);
        let first_columns = first.columns();
        let second_columns = second.columns();
        let depth_row = read_bytes(&depth_out, 512 * (COLUMN_HEIGHT as u64 / 2), 512);
        for c in 0..COLUMNS {
            // `masked` draws start at column 0: value 0 + 1 + offset.
            assert_eq!(
                first_columns[c as usize],
                if marked(c) { 101 } else { 0 },
                "column {c}"
            );
            assert_eq!(
                second_columns[c as usize],
                if marked(c) { 201 } else { 0 },
                "column {c}"
            );
            let x = (c * COLUMN_WIDTH + COLUMN_WIDTH / 2) as usize * 4;
            let d = f32::from_le_bytes(depth_row[x..x + 4].try_into().unwrap());
            assert_eq!(d, if marked(c) { 0.5 } else { 0.0 }, "depth of column {c}");
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn stencil_needs_a_stencil_format() {
    let device = support::hardware_device();
    let state = DepthState {
        format: Format::D32Float,
        ..stencil(CompareOp::Always, StencilOp::Replace)
    };
    let err = device
        .create_graphics_pipeline(&marlin_rhi::GraphicsPipelineDesc {
            vertex: shader("columns", Stage::Vertex),
            pixel: Some(shader("columns", Stage::Pixel)),
            topology: marlin_rhi::PrimitiveTopology::TriangleList,
            raster: marlin_rhi::RasterState::default(),
            depth: Some(state),
            color_targets: &ColumnTarget::color_targets(),
            multisample: marlin_rhi::MultisampleState::NONE,
            primitive_restart: None,
            cache: None,
        })
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
}

#[test]
#[should_panic(expected = "copy past the end of the buffer")]
fn copy_rows_do_not_wrap() {
    let device = support::hardware_device();
    let desc = TextureDesc::new_2d(4, 4, Format::R32Uint, TextureUsage::COPY_DST);
    let texture = device.create_texture(&desc).unwrap();
    let src = upload(device, &[0; 512], BufferUsage::COPY_SRC);
    run(device, |list| {
        // The last row ends past u64::MAX, which wraps to a small offset where overflow checks
        // are off.
        list.copy_buffer_to_texture(
            &src,
            &texture,
            &BufferTextureCopy::whole_mip(&desc, 0, 0, u64::MAX - 511),
        );
    });
}

#[test]
#[should_panic(expected = "subresource mips out of range")]
fn barrier_ranges_stay_in_the_texture() {
    let device = support::hardware_device();
    let texture = device
        .create_texture(&TextureDesc {
            mip_levels: 2,
            ..TextureDesc::new_2d(4, 4, Format::R32Uint, TextureUsage::COPY_DST)
        })
        .unwrap();
    run(device, |list| {
        list.texture_barriers(&[TextureBarrier {
            range: SubresourceRange {
                base_mip: 1,
                mip_count: 2,
                ..SubresourceRange::ALL
            },
            ..TextureBarrier::new(&texture, TextureState::UNDEFINED, TextureState::COPY_DST)
        }]);
    });
}

#[test]
#[should_panic(expected = "subresource layers out of range")]
fn barrier_layers_stay_in_the_texture() {
    let device = support::hardware_device();
    let texture = device
        .create_texture(&TextureDesc::new_2d(
            4,
            4,
            Format::R32Uint,
            TextureUsage::COPY_DST,
        ))
        .unwrap();
    run(device, |list| {
        list.texture_barriers(&[TextureBarrier {
            range: SubresourceRange {
                base_layer: 1,
                ..SubresourceRange::ALL
            },
            ..TextureBarrier::new(&texture, TextureState::UNDEFINED, TextureState::COPY_DST)
        }]);
    });
}
