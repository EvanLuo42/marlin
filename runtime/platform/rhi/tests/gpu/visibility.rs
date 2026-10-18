//! What the visibility-buffer renderer needs (docs/design/0009-scene-and-geometry.md §4.3), on
//! the hardware adapter: pixel derivatives, 64-bit integer arithmetic, 64-bit atomics on
//! buffers and on `R64Uint` storage textures, and 32-bit texture atomics. The shaders are
//! `Test.Slopes` and the others in gpu-test-support's `Visibility.mshader`.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    BufferBarrier, BufferState, BufferTextureCopy, BufferUsage, ClearColor, ColorTargetState,
    Device, Format, FormatFeatures, SubresourceRange, Texture, TextureBarrier, TextureDesc,
    TextureState, TextureUsage, TextureViewDesc, TextureViewDimension,
};
use support::{
    ColorTarget, compute_pipeline, devices, read_bytes, read_words, readback, run, solid_constants,
    solid_pipeline, storage, upload,
};

const SIZE: u32 = 8;

fn barrier(
    list: &mut marlin_rhi::CommandList,
    texture: &Texture,
    before: TextureState,
    after: TextureState,
) {
    list.texture_barriers(&[TextureBarrier::new(texture, before, after)]);
}

/// The texels of a `SIZE`² `Rgba32Uint` target that `pixel` drew over all of.
fn draw_slopes(device: &Device, pixel: &str) -> Vec<[u32; 4]> {
    let target = ColorTarget::new(device, SIZE, Format::Rgba32Uint, 1);
    let pipeline = solid_pipeline(
        device,
        pixel,
        ColorTargetState::new(Format::Rgba32Uint),
        |_| {},
    )
    .unwrap();
    run(device, |list| {
        target.begin(list, [0.0; 4]);
        list.set_graphics_pipeline(&pipeline);
        list.push_constants(0, &solid_constants([0.0; 4], 0, 0.5, [0.0; 4]));
        list.draw(3, 1, 0, 0);
        target.finish(list);
    });
    let mut texels = Vec::new();
    for y in 0..SIZE {
        let row = read_words(&target.readback, y as u64 * 256, (SIZE * 4) as usize);
        texels.extend(row.as_chunks::<4>().0.iter().copied());
    }
    texels
}

#[test]
fn derivatives_follow_the_quad() {
    for device in devices() {
        // f = x·y at pixel centers (x + 0.5, y + 0.5); every value is doubled.
        let slopes = draw_slopes(device, "Slopes");
        let widths = draw_slopes(device, "SlopeWidths");
        for y in 0..SIZE {
            for x in 0..SIZE {
                let at = |t: &[[u32; 4]], x: u32, y: u32| t[(y * SIZE + x) as usize];
                let [fine_x, fine_y, coarse_x, coarse_y] = at(&slopes, x, y);
                // Fine: the pixel's own row and column of the quad.
                assert_eq!(fine_x, 2 * y + 1, "fine ddx at ({x}, {y})");
                assert_eq!(fine_y, 2 * x + 1, "fine ddy at ({x}, {y})");
                // Coarse: one row and one column of the quad, the same for all four pixels.
                let (qx, qy) = (x & !1, y & !1);
                assert!(
                    [2 * qy + 1, 2 * qy + 3].contains(&coarse_x),
                    "coarse ddx {coarse_x} at ({x}, {y})"
                );
                assert!(
                    [2 * qx + 1, 2 * qx + 3].contains(&coarse_y),
                    "coarse ddy {coarse_y} at ({x}, {y})"
                );
                let first = at(&slopes, qx, qy);
                assert_eq!(
                    [coarse_x, coarse_y],
                    [first[2], first[3]],
                    "quad of ({x}, {y})"
                );
                // `Ddx`, `Ddy` and `Fwidth` are the coarse ones; `FwidthFine` the fine ones.
                let [width, width_fine, ddx, ddy] = at(&widths, x, y);
                assert_eq!([ddx, ddy], [coarse_x, coarse_y], "Ddx, Ddy at ({x}, {y})");
                assert_eq!(width, ddx + ddy, "Fwidth at ({x}, {y})");
                assert_eq!(width_fine, fine_x + fine_y, "FwidthFine at ({x}, {y})");
            }
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

fn wide_value(i: u32, seed: u32) -> u64 {
    (u64::from(i + 1) << 35) | u64::from(i.wrapping_mul(2654435761) ^ seed)
}

fn u64s(bytes: &[u8]) -> Vec<u64> {
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| u64::from_le_bytes(*b))
        .collect()
}

#[test]
fn wide_integers_and_their_buffer_atomics() {
    const SEED: u32 = 0x5eed_1234;
    for device in devices() {
        support::require_feature!(device, shader_int64);
        support::require_feature!(device, atomic_int64);
        // Elements 64 to 74 start as each atomic's identity.
        let mut initial = vec![0u64; 75];
        initial[65] = u64::MAX;
        initial[69] = u64::MAX;
        initial[74] = i64::MIN as u64;
        let bytes: Vec<u8> = initial.iter().flat_map(|v| v.to_le_bytes()).collect();
        let size = bytes.len() as u64;
        let staging = upload(device, &bytes, BufferUsage::default());
        let (out, view) = storage(device, size / 4);
        let rb = readback(device, size);
        let pipeline = compute_pipeline(device, "Wide");
        run(device, |list| {
            list.buffer_barriers(&[BufferBarrier {
                buffer: &out,
                before: BufferState::NONE,
                after: BufferState::COPY_DST,
            }]);
            list.copy_buffer(&staging, 0, &out, 0, size);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &out,
                before: BufferState::COPY_DST,
                after: BufferState::COMPUTE_WRITE,
            }]);
            list.set_compute_pipeline(&pipeline);
            list.push_constants(0, &[view.index(), view.index(), SEED]);
            list.dispatch(1, 1, 1);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &out,
                before: BufferState::COMPUTE_WRITE,
                after: BufferState::COPY_SRC,
            }]);
            list.copy_buffer(&out, 0, &rb, 0, size);
        });
        let got = u64s(&read_bytes(&rb, 0, size as usize));
        let values: Vec<u64> = (0..64).map(|i| wide_value(i, SEED)).collect();
        for (i, &v) in values.iter().enumerate() {
            let mixed = v.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (v >> 29);
            let negative = (mixed as i64).wrapping_neg();
            let want = (mixed / (i as u64 + 3)).wrapping_add((negative >> 7) as u64);
            assert_eq!(got[i], want, "arithmetic of thread {i}");
        }
        let sum = values.iter().fold(0u64, |s, &v| s.wrapping_add(v));
        let negatives = values.iter().map(|&v| (v as i64).wrapping_neg());
        assert_eq!(got[64], *values.iter().max().unwrap(), "AtomicMax");
        assert_eq!(got[65], *values.iter().min().unwrap(), "AtomicMin");
        assert_eq!(got[66], sum, "AtomicAdd");
        assert_eq!(got[67], sum.wrapping_neg(), "AtomicSub");
        assert_eq!(got[68], values.iter().fold(0, |a, &v| a | v), "AtomicOr");
        let and = values
            .iter()
            .fold(u64::MAX, |a, &v| a & !(v & 0xF0F0_F0F0_F0F0_F0F0));
        assert_eq!(got[69], and, "AtomicAnd");
        assert_eq!(got[70], values.iter().fold(0, |a, &v| a ^ v), "AtomicXor");
        assert!(
            values.contains(&got[71]),
            "AtomicExchange left {:#x}",
            got[71]
        );
        assert!(
            values.contains(&got[72]),
            "AtomicCompareExchange left {:#x}",
            got[72]
        );
        assert_eq!(
            got[73] as i64,
            negatives.clone().min().unwrap(),
            "signed AtomicMin"
        );
        assert_eq!(got[74] as i64, negatives.max().unwrap(), "signed AtomicMax");
        assert_eq!(device.validation_errors(), 0);
    }
}

fn texel_depth(x: u32, y: u32, z: u32) -> u32 {
    x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ z.wrapping_mul(83492791)
}

/// A `SIZE`² storage texture of `format` (`layers` deep, as an array or a volume) and a writable
/// view of it.
fn storage_texture(
    device: &Device,
    format: Format,
    layers: u32,
    dimension: TextureViewDimension,
) -> (Texture, marlin_rhi::ShaderView) {
    let usage = TextureUsage::STORAGE | TextureUsage::COPY_SRC;
    let desc = match dimension {
        TextureViewDimension::D3 => TextureDesc::new_3d(SIZE, SIZE, layers, format, usage),
        _ => TextureDesc {
            array_layers: layers,
            ..TextureDesc::new_2d(SIZE, SIZE, format, usage)
        },
    };
    let texture = device.create_texture(&desc).unwrap();
    let view = device
        .create_texture_view(
            &texture,
            &TextureViewDesc {
                writable: true,
                dimension,
                format: None,
                range: SubresourceRange::ALL,
            },
        )
        .unwrap();
    (texture, view)
}

/// Copies every layer (or slice) of `texture` out after compute work, into `rb` at 256-byte
/// rows.
fn copy_texture_out(
    list: &mut marlin_rhi::CommandList,
    texture: &Texture,
    rb: &marlin_rhi::Buffer,
) {
    barrier(
        list,
        texture,
        TextureState::COMPUTE_WRITE,
        TextureState::COPY_SRC,
    );
    let d = texture.desc();
    // A volume's slices come in one copy, one after another.
    for layer in 0..d.array_layers {
        let at = u64::from(layer * d.height * 256);
        list.copy_texture_to_buffer(texture, rb, &BufferTextureCopy::whole_mip(d, 0, layer, at));
    }
}

#[test]
fn r64_uint_takes_storage_and_atomics() {
    for device in devices() {
        support::require_feature!(device, texture_atomic_int64);
        let support = device.format_support(Format::R64Uint);
        let want = FormatFeatures::STORAGE_WRITE
            | FormatFeatures::STORAGE_READ
            | FormatFeatures::STORAGE_ATOMIC;
        assert!(support.features.contains(want), "{:?}", support.features);
        assert_eq!(Format::R64Uint.block_size(), 8);
        assert!(Format::R64Uint.is_unsigned_integer());
    }
}

#[test]
fn the_nearest_triangle_wins_with_64_bit_texture_atomics() {
    const TRIANGLES: u32 = 4;
    for device in devices() {
        support::require_feature!(device, shader_int64);
        support::require_feature!(device, texture_atomic_int64);
        let (vis, vis_view) = storage_texture(device, Format::R64Uint, 1, TextureViewDimension::D2);
        let (sums, sums_view) =
            storage_texture(device, Format::R64Uint, 1, TextureViewDimension::D2);
        let (clear, raster) = (
            compute_pipeline(device, "VisClear"),
            compute_pipeline(device, "VisRaster"),
        );
        let (vis_rb, sums_rb) = (
            readback(device, 256 * SIZE as u64),
            readback(device, 256 * SIZE as u64),
        );
        run(device, |list| {
            for t in [&vis, &sums] {
                barrier(
                    list,
                    t,
                    TextureState::UNDEFINED,
                    TextureState::COMPUTE_WRITE,
                );
            }
            list.set_compute_pipeline(&clear);
            list.push_constants(0, &[vis_view.index(), sums_view.index()]);
            list.dispatch(1, 1, 1);
            for t in [&vis, &sums] {
                barrier(
                    list,
                    t,
                    TextureState::COMPUTE_WRITE,
                    TextureState::COMPUTE_WRITE,
                );
            }
            list.set_compute_pipeline(&raster);
            list.push_constants(0, &[vis_view.index(), sums_view.index()]);
            list.dispatch(1, 1, 1);
            copy_texture_out(list, &vis, &vis_rb);
            copy_texture_out(list, &sums, &sums_rb);
        });
        for y in 0..SIZE {
            let vis_row = u64s(&read_bytes(&vis_rb, y as u64 * 256, SIZE as usize * 8));
            let sums_row = u64s(&read_bytes(&sums_rb, y as u64 * 256, SIZE as usize * 8));
            for x in 0..SIZE {
                let nearest = (0..TRIANGLES)
                    .map(|z| (u64::from(texel_depth(x, y, z)) << 32) | u64::from(z + 1))
                    .max()
                    .unwrap();
                assert_eq!(
                    vis_row[x as usize],
                    nearest.max(7),
                    "visibility at ({x}, {y})"
                );
                let added: u64 = (0..TRIANGLES).map(|z| (1 << 32) + u64::from(z)).sum();
                assert_eq!(
                    sums_row[x as usize],
                    0xFFFF_FFFF + added,
                    "sum at ({x}, {y})"
                );
            }
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn texture_atomics_on_32_bit_texels() {
    const THREADS_Z: u32 = 4;
    for device in devices() {
        let (counts, counts_view) =
            storage_texture(device, Format::R32Uint, 2, TextureViewDimension::D2Array);
        let (lowest, lowest_view) =
            storage_texture(device, Format::R32Sint, 2, TextureViewDimension::D3);
        let pipeline = compute_pipeline(device, "Atomic32");
        let rows = 256 * SIZE as u64;
        let (counts_rb, lowest_rb) = (readback(device, 2 * rows), readback(device, 2 * rows));
        run(device, |list| {
            for t in [&counts, &lowest] {
                barrier(
                    list,
                    t,
                    TextureState::UNDEFINED,
                    TextureState::CLEAR_STORAGE,
                );
            }
            list.clear_storage_texture(&counts, &SubresourceRange::ALL, ClearColor::Uint([0; 4]));
            list.clear_storage_texture(&lowest, &SubresourceRange::ALL, ClearColor::Sint([0; 4]));
            for t in [&counts, &lowest] {
                barrier(
                    list,
                    t,
                    TextureState::CLEAR_STORAGE,
                    TextureState::COMPUTE_WRITE,
                );
            }
            list.set_compute_pipeline(&pipeline);
            list.push_constants(0, &[counts_view.index(), lowest_view.index()]);
            list.dispatch(1, 1, 1);
            copy_texture_out(list, &counts, &counts_rb);
            copy_texture_out(list, &lowest, &lowest_rb);
        });
        for layer in 0..2u64 {
            for y in 0..SIZE as u64 {
                let at = layer * rows + y * 256;
                let count_row = read_words(&counts_rb, at, SIZE as usize);
                let lowest_row = read_words(&lowest_rb, at, SIZE as usize);
                for x in 0..SIZE {
                    let (xu, yu) = (x, y as u32);
                    assert_eq!(
                        count_row[x as usize],
                        THREADS_Z / 2,
                        "count {layer} ({x}, {y})"
                    );
                    let want = if layer == 0 {
                        (0..THREADS_Z)
                            .map(|z| -((texel_depth(xu, yu, z) % 1000) as i32))
                            .min()
                            .unwrap()
                    } else {
                        5
                    };
                    assert_eq!(
                        lowest_row[x as usize] as i32, want,
                        "slice {layer} ({x}, {y})"
                    );
                }
            }
        }
        assert_eq!(device.validation_errors(), 0);
    }
}
