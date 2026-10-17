//! Hardware conformance: our encodings, uploaded through marlin-rhi and read back texel by texel
//! on the hardware adapter (validation on), must decode exactly as the reference decoders say.
//!
//! BC6H and BC7 decoding is integer-exact in the spec: every GPU must match bit for bit. BC1–BC5
//! interpolate in precision the spec leaves open, and vendors stray a few 8-bit steps from the
//! exact formulas. On NVIDIA the [`Interpolation::Nvidia`] decoders must match bit for bit;
//! elsewhere the exact decoders must come within [`INTERPOLATION_TOLERANCE`]. Besides encoder
//! output, every format decodes random blocks, which reach modes, partitions and endpoint
//! combinations no encoder emits.
//!
//! The readback shader is a PLACEHOLDER for the in-house shader language: texel_load.hlsl,
//! compiled once by hand with the Windows SDK's dxc and committed as texel_load.cs.dxil:
//!
//! ```text
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T cs_6_6 -E main -Fo texel_load.cs.dxil texel_load.hlsl
//! ```
#![cfg(marlin_platform = "windows")]

mod common;

use std::sync::OnceLock;

use common::Rng;
use marlin_cook_texture::{
    self as ct, EncodeOptions, EncodedTexture, Format, Image, Interpolation, MipOptions, Quality,
    Subresource, UploadAlignment,
};
use marlin_rhi::{
    self as rhi, AdapterPreference, BufferBarrier, BufferDesc, BufferState, BufferTextureCopy,
    BufferUsage, BufferViewDesc, BufferViewKind, ComputePipelineDesc, Device, DeviceDesc, Instance,
    InstanceDesc, MemoryLocation, ShaderModule, SubresourceRange, TextureBarrier, TextureDesc,
    TextureState, TextureUsage, TextureViewDesc, TextureViewDimension,
};

/// Largest difference allowed between unprofiled hardware and the exact BC1–BC5 decoders, in
/// [0, 1] units. NVIDIA's strays up to 6.75 8-bit steps (BC4's six-value mode weighs 48/257
/// where the spec says 1/5).
const INTERPOLATION_TOLERANCE: f32 = 8.0 / 255.0;

const NVIDIA: u32 = 0x10de;

fn shader() -> ShaderModule<'static> {
    ShaderModule {
        code: include_bytes!("shaders/texel_load.cs.dxil"),
        entry: "main",
    }
}

fn rhi_format(format: Format) -> rhi::Format {
    match format {
        Format::Bc1Unorm => rhi::Format::Bc1Unorm,
        Format::Bc1UnormSrgb => rhi::Format::Bc1UnormSrgb,
        Format::Bc3Unorm => rhi::Format::Bc3Unorm,
        Format::Bc3UnormSrgb => rhi::Format::Bc3UnormSrgb,
        Format::Bc4Unorm => rhi::Format::Bc4Unorm,
        Format::Bc5Unorm => rhi::Format::Bc5Unorm,
        Format::Bc6hUfloat => rhi::Format::Bc6hUfloat,
        Format::Bc7Unorm => rhi::Format::Bc7Unorm,
        Format::Bc7UnormSrgb => rhi::Format::Bc7UnormSrgb,
        Format::Bc6hSfloat => panic!("marlin-rhi has no signed BC6H format yet"),
    }
}

/// The hardware adapter with validation on, shared by every test in this binary and never
/// dropped: tearing down `Device`s while other threads use the same adapter can deadlock the
/// NVIDIA driver (see the RHI tests' `support::devices`).
fn device() -> &'static Device {
    static DEVICE: OnceLock<Device> = OnceLock::new();
    DEVICE.get_or_init(|| {
        Instance::new(&InstanceDesc {
            validation: true,
            ..InstanceDesc::default()
        })
        .unwrap()
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::HighPerformance,
            ..DeviceDesc::default()
        })
        .unwrap()
    })
}

/// Every subresource of `texture` as the GPU reads it through a view of the linear format.
fn gpu_decode(device: &Device, texture: &EncodedTexture) -> Vec<Vec<[f32; 4]>> {
    let layout = texture.upload_layout(UploadAlignment::RHI);
    let mut staging_data = vec![0; layout.size as usize];
    texture.write_upload(&layout, &mut staging_data);
    let staging = device
        .create_buffer(&BufferDesc {
            size: layout.size,
            usage: BufferUsage::COPY_SRC,
            memory: MemoryLocation::Upload,
        })
        .unwrap();
    // SAFETY: no GPU work uses the buffer yet.
    unsafe { staging.write(0, &staging_data) };

    let gpu_texture = device
        .create_texture(&TextureDesc {
            width: texture.width,
            height: texture.height,
            array_layers: texture.array_layers,
            mip_levels: texture.mip_levels,
            format: rhi_format(texture.format),
            usage: TextureUsage::SAMPLED | TextureUsage::COPY_DST,
            clear_value: None,
            dimension: rhi::TextureDimension::D2,
            depth: 1,
            sample_count: 1,
        })
        .unwrap();
    let view = device
        .create_texture_view(
            &gpu_texture,
            &TextureViewDesc {
                writable: false,
                dimension: TextureViewDimension::D2Array,
                format: Some(rhi_format(texture.format.linear())),
                range: SubresourceRange::ALL,
            },
        )
        .unwrap();

    let offsets: Vec<u64> = texture
        .subresources
        .iter()
        .scan(0u64, |at, s| {
            let offset = *at;
            *at += s.width as u64 * s.height as u64 * 16;
            Some(offset)
        })
        .collect();
    let s = texture.subresources.last().unwrap();
    let size = offsets.last().unwrap() + s.width as u64 * s.height as u64 * 16;
    let output = device
        .create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
            memory: MemoryLocation::GpuOnly,
        })
        .unwrap();
    let output_view = device
        .create_buffer_view(&output, &BufferViewDesc::whole(BufferViewKind::Raw, true))
        .unwrap();
    let readback = device
        .create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::COPY_DST,
            memory: MemoryLocation::Readback,
        })
        .unwrap();
    let pipeline = device
        .create_compute_pipeline(&ComputePipelineDesc {
            shader: shader(),
            cache: None,
        })
        .unwrap();

    let mut pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    let fence = device.create_fence(0).unwrap();
    list.begin(&mut pool).unwrap();
    list.texture_barriers(&[TextureBarrier::new(
        &gpu_texture,
        TextureState::UNDEFINED,
        TextureState::COPY_DST,
    )]);
    for r in &layout.regions {
        list.copy_buffer_to_texture(
            &staging,
            &gpu_texture,
            &BufferTextureCopy {
                buffer_offset: r.buffer_offset,
                bytes_per_row: r.bytes_per_row,
                mip: r.mip,
                layer: r.layer,
                x: 0,
                y: 0,
                z: 0,
                width: r.width,
                height: r.height,
                depth: 1,
            },
        );
    }
    list.texture_barriers(&[TextureBarrier::new(
        &gpu_texture,
        TextureState::COPY_DST,
        TextureState::SHADER_READ,
    )]);
    list.set_compute_pipeline(&pipeline);
    for (s, &offset) in texture.subresources.iter().zip(&offsets) {
        list.push_constants(
            0,
            &[
                view.index(),
                output_view.index(),
                s.width,
                s.height,
                s.mip,
                s.layer,
                offset as u32,
            ],
        );
        list.dispatch(s.width.div_ceil(8), s.height.div_ceil(8), 1);
    }
    list.buffer_barriers(&[BufferBarrier {
        buffer: &output,
        before: BufferState::COMPUTE_WRITE,
        after: BufferState::COPY_SRC,
    }]);
    list.copy_buffer(&output, 0, &readback, 0, size);
    list.end().unwrap();
    // SAFETY: everything the list uses outlives the wait below.
    unsafe { device.queue().submit(&[&list], Some((&fence, 1))).unwrap() };
    fence.wait(1, None).unwrap();

    let mut bytes = vec![0; size as usize];
    // SAFETY: the GPU work that wrote it has finished.
    unsafe { readback.read(0, &mut bytes) };
    texture
        .subresources
        .iter()
        .zip(&offsets)
        .map(|(s, &offset)| {
            bytes[offset as usize..][..s.width as usize * s.height as usize * 16]
                .as_chunks::<16>()
                .0
                .iter()
                .map(|t| {
                    core::array::from_fn(|c| {
                        f32::from_le_bytes(t[c * 4..][..4].try_into().unwrap())
                    })
                })
                .collect()
        })
        .collect()
}

/// A single-subresource texture of raw blocks.
fn from_blocks(format: Format, width: u32, height: u32, data: Vec<u8>) -> EncodedTexture {
    let (blocks_x, blocks_y) = format.blocks(width, height);
    assert_eq!(data.len(), format.surface_size(width, height));
    EncodedTexture {
        format,
        width,
        height,
        mip_levels: 1,
        array_layers: 1,
        data,
        subresources: vec![Subresource {
            mip: 0,
            layer: 0,
            width,
            height,
            offset: 0,
            row_pitch: (blocks_x * format.block_size()) as usize,
            rows: blocks_y,
        }],
    }
}

/// Layers of one mip each, from equally sized images.
fn layered(format: Format, images: &[(Image, EncodeOptions)]) -> EncodedTexture {
    let mut texture = ct::encode_texture(&[vec![images[0].0.clone()]], format, &images[0].1);
    for (image, options) in &images[1..] {
        let data = ct::encode(image, format, options);
        let mut s = texture.subresources[0];
        s.layer = texture.array_layers;
        s.offset = texture.data.len();
        texture.data.extend(data);
        texture.subresources.push(s);
        texture.array_layers += 1;
    }
    texture
}

#[derive(Default)]
struct Report {
    texels: usize,
    /// Of the spec-exact decoder, which no GPU need match bit for bit on BC1–BC5.
    max_exact_difference: f32,
    mismatches: usize,
}

/// The decoder profile this adapter must match bit for bit, if we know it.
fn profile(device: &Device) -> Option<Interpolation> {
    (device.adapter_info().vendor_id == NVIDIA).then_some(Interpolation::Nvidia)
}

/// Compares the GPU's decoding of `texture` with the reference decoders.
fn check(device: &Device, texture: &EncodedTexture, report: &mut Report, what: &str) {
    let spec_exact = matches!(
        texture.format,
        Format::Bc6hUfloat | Format::Bc7Unorm | Format::Bc7UnormSrgb
    );
    let profile = profile(device);
    let gpu = gpu_decode(device, texture);
    for (s, gpu) in texture.subresources.iter().zip(gpu) {
        let data = texture.subresource_data(s.mip, s.layer);
        let decode = |i| ct::decode_with(data, s.width, s.height, texture.format, i);
        let exact = decode(Interpolation::Exact);
        let ours = profile.map_or_else(|| exact.clone(), decode);
        for (i, ((a, e), g)) in ours.iter().zip(&exact).zip(&gpu).enumerate() {
            report.texels += 1;
            for c in 0..4 {
                let difference = (e[c] - g[c]).abs();
                report.max_exact_difference = report.max_exact_difference.max(difference);
                let bad = if spec_exact || profile.is_some() {
                    a[c].to_bits() != g[c].to_bits()
                } else {
                    difference > INTERPOLATION_TOLERANCE
                };
                if bad {
                    if report.mismatches < 8 {
                        let block = (i % s.width as usize) / 4
                            + (i / s.width as usize) / 4 * s.width.div_ceil(4) as usize;
                        let size = texture.format.block_size() as usize;
                        eprintln!(
                            "{what}: {:?} mip {} layer {} texel {i} channel {c}: ours {} gpu {} block {:02x?}",
                            texture.format,
                            s.mip,
                            s.layer,
                            a[c],
                            g[c],
                            &data[block * size..][..size]
                        );
                    }
                    report.mismatches += 1;
                }
            }
        }
    }
}

fn options(quality: Quality, mode_mask: u32) -> EncodeOptions {
    EncodeOptions {
        quality,
        mode_mask,
        ..EncodeOptions::default()
    }
}

/// The common cases for `format`: a mip chain of two layers at every quality, forced modes, and
/// random blocks.
fn conformance(format: Format, sources: [Image; 2], modes: &[(u32, Image, EncodeOptions)]) {
    let device = device();
    let mut report = Report::default();

    for quality in [Quality::Fast, Quality::Normal, Quality::High] {
        let mip_options = MipOptions::default();
        let layers = sources
            .iter()
            .map(|image| ct::generate_mips(image, &mip_options))
            .collect::<Vec<_>>();
        let texture = ct::encode_texture(&layers, format, &EncodeOptions::with_quality(quality));
        check(device, &texture, &mut report, &format!("mips {quality:?}"));
    }

    if !modes.is_empty() {
        let images: Vec<(Image, EncodeOptions)> =
            modes.iter().map(|(_, i, o)| (i.clone(), *o)).collect();
        let texture = layered(format, &images);
        for (layer, (mode, ..)) in modes.iter().enumerate() {
            let data = texture.subresource_data(0, layer as u32);
            let used = data
                .chunks_exact(format.block_size() as usize)
                .filter(|b| ct::block_mode(format, b) == Some(*mode))
                .count();
            assert!(used > 0, "{format:?}: no block uses forced mode {mode}");
        }
        check(device, &texture, &mut report, "forced modes");
    }

    let mut rng = Rng::new(format as u64 + 1);
    let mut blocks = vec![0; format.surface_size(128, 64)];
    rng.fill_bytes(&mut blocks);
    let random = from_blocks(format, 128, 64, blocks);
    check(device, &random, &mut report, "random blocks");

    println!(
        "{format:?}: {} texels, {} mismatches; spec-exact decoder within {:.2} / 255 of the GPU",
        report.texels,
        report.mismatches,
        report.max_exact_difference * 255.0
    );
    assert_eq!(report.mismatches, 0, "{format:?} differs from the hardware");
    assert_eq!(device.validation_errors(), 0);
}

fn forced(
    count: u32,
    image: impl Fn(u32) -> Image,
    base: EncodeOptions,
) -> Vec<(u32, Image, EncodeOptions)> {
    (0..count)
        .map(|m| {
            (
                m,
                image(m),
                EncodeOptions {
                    mode_mask: 1 << m,
                    ..base
                },
            )
        })
        .collect()
}

#[test]
fn bc1() {
    let mut modes = forced(
        2,
        |m| common::natural(32, 32, m as u64),
        options(Quality::Normal, 0),
    );
    // 1-bit alpha: transparent texels force the three-color mode.
    modes.push((
        1,
        common::cutout(32, 32),
        EncodeOptions {
            alpha_cutoff: Some(0.5),
            ..EncodeOptions::default()
        },
    ));
    conformance(
        Format::Bc1Unorm,
        [common::natural(36, 20, 1), common::edges(36, 20)],
        &modes,
    );
    conformance(
        Format::Bc1UnormSrgb,
        [common::gradient(36, 20), common::noise(36, 20, 2)],
        &[],
    );
}

#[test]
fn bc3() {
    let modes = forced(
        2,
        |_| common::alpha_ramp(32, 32),
        options(Quality::Normal, 0),
    );
    conformance(
        Format::Bc3Unorm,
        [common::alpha_ramp(36, 20), common::noise(36, 20, 3)],
        &modes,
    );
    conformance(
        Format::Bc3UnormSrgb,
        [common::cutout(36, 20), common::natural(36, 20, 4)],
        &[],
    );
}

#[test]
fn bc4() {
    let modes = forced(
        2,
        |m| common::natural(32, 32, m as u64),
        options(Quality::Normal, 0),
    );
    conformance(
        Format::Bc4Unorm,
        [common::single_channel(36, 20, 4), common::noise(36, 20, 5)],
        &modes,
    );
}

#[test]
fn bc5() {
    let modes = forced(
        2,
        |_| common::normal_map(32, 32),
        options(Quality::Normal, 0),
    );
    conformance(
        Format::Bc5Unorm,
        [common::normal_map(36, 20), common::noise(36, 20, 6)],
        &modes,
    );
}

#[test]
fn bc6h() {
    let modes = forced(
        14,
        |m| common::hdr(32, 32, m as u64),
        options(Quality::Fast, 0),
    );
    conformance(
        Format::Bc6hUfloat,
        [common::hdr(36, 20, 7), common::hdr(36, 20, 8)],
        &modes,
    );
}

#[test]
fn bc7() {
    let modes = forced(
        8,
        |m| {
            if m >= 4 {
                common::alpha_ramp(32, 32)
            } else {
                common::natural(32, 32, m as u64)
            }
        },
        options(Quality::Fast, 0),
    );
    conformance(
        Format::Bc7Unorm,
        [common::natural(36, 20, 9), common::alpha_ramp(36, 20)],
        &modes,
    );
    conformance(
        Format::Bc7UnormSrgb,
        [common::edges(36, 20), common::cutout(36, 20)],
        &[],
    );
}
