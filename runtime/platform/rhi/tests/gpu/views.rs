//! Views and samplers beyond the basics: min/max sampler reduction, stencil views, cube arrays
//! and 16-bit depth, read by compute shaders. Hardware adapter, enhanced and legacy barriers,
//! validation on.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    AddressMode, BufferTextureCopy, BufferUsage, DepthAttachment, DepthStencilViewDesc, Device,
    Filter, Format, LoadOp, RenderPassDesc, SamplerDesc, SamplerReduction, StoreOp,
    SubresourceRange, Texture, TextureBarrier, TextureDesc, TextureState, TextureUsage,
    TextureViewDesc, TextureViewDimension,
};
use support::{compute_pipeline, copy_out, devices, read_words, readback, run, storage};

/// Runs `shader` (one of the `sample` entries) once over `view` with `sampler` and returns
/// `words` results.
/// What the `sample` shaders read: a texture view, a sampler, an integer argument and a
/// coordinate.
struct Read {
    view: u32,
    sampler: u32,
    x: u32,
    uv: (f32, f32),
}

fn sample(
    device: &Device,
    shader: &str,
    read: Read,
    words: u64,
    before: impl FnOnce(&mut marlin_rhi::CommandList),
) -> Vec<u32> {
    let pipeline = compute_pipeline(device, shader);
    let (out, out_view) = storage(device, words);
    let rb = readback(device, words * 4);
    run(device, |list| {
        before(list);
        list.set_compute_pipeline(&pipeline);
        list.push_constants(
            0,
            &[
                read.view,
                read.sampler,
                out_view.index(),
                read.x,
                read.uv.0.to_bits(),
                read.uv.1.to_bits(),
            ],
        );
        list.dispatch(1, 1, 1);
        copy_out(list, &out, &rb, words);
    });
    read_words(&rb, 0, words as usize)
}

/// A sampled texture and the staging data for `layers` of tightly packed rows; `upload`
/// records the copies and leaves the texture readable by shaders.
struct Filled {
    texture: Texture,
    staging: marlin_rhi::Buffer,
    desc: TextureDesc,
    layers: u32,
    layer_size: u64,
}

impl Filled {
    fn new(device: &Device, desc: &TextureDesc, layers: &[Vec<u8>]) -> Self {
        let texture = device.create_texture(desc).unwrap();
        let row = (desc.width * desc.format.block_size()) as usize;
        let pitch = marlin_rhi::aligned_row_pitch(desc.format, desc.width) as usize;
        let layer_size = (pitch * desc.height as usize).next_multiple_of(512);
        let mut staging = vec![0u8; layer_size * layers.len()];
        for (l, bytes) in layers.iter().enumerate() {
            for y in 0..desc.height as usize {
                let at = l * layer_size + y * pitch;
                staging[at..at + row].copy_from_slice(&bytes[y * row..(y + 1) * row]);
            }
        }
        Self {
            texture,
            staging: support::upload(device, &staging, BufferUsage::COPY_SRC),
            desc: *desc,
            layers: layers.len() as u32,
            layer_size: layer_size as u64,
        }
    }

    fn upload(&self, list: &mut marlin_rhi::CommandList) {
        list.texture_barriers(&[TextureBarrier::new(
            &self.texture,
            TextureState::UNDEFINED,
            TextureState::COPY_DST,
        )]);
        for l in 0..self.layers {
            list.copy_buffer_to_texture(
                &self.staging,
                &self.texture,
                &BufferTextureCopy::whole_mip(&self.desc, 0, l, l as u64 * self.layer_size),
            );
        }
        list.texture_barriers(&[TextureBarrier::new(
            &self.texture,
            TextureState::COPY_DST,
            TextureState::SHADER_READ,
        )]);
    }
}

#[test]
fn samplers_reduce_to_min_max_or_average() {
    for device in devices() {
        support::require_feature!(device, sampler_min_max);
        let desc = TextureDesc::new_2d(
            2,
            2,
            Format::R32Float,
            TextureUsage::SAMPLED | TextureUsage::COPY_DST,
        );
        let texels: Vec<u8> = [1.0f32, 2.0, 3.0, 4.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let mut results = Vec::new();
        for reduction in [
            SamplerReduction::WeightedAverage,
            SamplerReduction::Minimum,
            SamplerReduction::Maximum,
        ] {
            let filled = Filled::new(device, &desc, std::slice::from_ref(&texels));
            let view = device
                .create_texture_view(&filled.texture, &TextureViewDesc::SAMPLED)
                .unwrap();
            let sampler = device
                .create_sampler(&SamplerDesc {
                    address_u: AddressMode::ClampToEdge,
                    address_v: AddressMode::ClampToEdge,
                    mip_filter: Filter::Nearest,
                    reduction,
                    ..SamplerDesc::default()
                })
                .unwrap();
            let r = sample(
                device,
                "sample",
                Read {
                    view: view.index(),
                    sampler: sampler.index(),
                    x: 0,
                    uv: (0.5, 0.5),
                },
                1,
                |list| filled.upload(list),
            );
            results.push(f32::from_bits(r[0]));
        }
        assert_eq!(results, [2.5, 1.0, 4.0]);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn min_max_samplers_take_no_comparison() {
    let device = support::hardware_device();
    let error = device
        .create_sampler(&SamplerDesc {
            reduction: SamplerReduction::Maximum,
            compare: Some(marlin_rhi::CompareOp::Less),
            ..SamplerDesc::default()
        })
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        marlin_rhi::ErrorKind::InvalidInput | marlin_rhi::ErrorKind::Unsupported
    ));
}

/// Clears depth (and stencil) in a depth-only pass, then leaves the texture readable by shaders.
fn cleared_depth(
    list: &mut marlin_rhi::CommandList,
    texture: &Texture,
    view: &marlin_rhi::DepthStencilView,
    depth: f32,
    stencil: u8,
) {
    list.texture_barriers(&[TextureBarrier::new(
        texture,
        TextureState::UNDEFINED,
        TextureState::DEPTH_WRITE,
    )]);
    list.begin_render_pass(&RenderPassDesc {
        color: &[],
        depth: Some(DepthAttachment {
            view,
            load: LoadOp::Clear(depth),
            store: StoreOp::Store,
            stencil_load: LoadOp::Clear(stencil),
            stencil_store: StoreOp::Store,
        }),
    });
    list.end_render_pass();
    list.texture_barriers(&[TextureBarrier::new(
        texture,
        TextureState::DEPTH_WRITE,
        TextureState::SHADER_READ,
    )]);
}

#[test]
fn stencil_and_16_bit_depth_read_through_views() {
    for device in devices() {
        let stencil_texture = device
            .create_texture(&TextureDesc::new_2d(
                4,
                1,
                Format::D32FloatS8,
                TextureUsage::DEPTH_STENCIL | TextureUsage::SAMPLED,
            ))
            .unwrap();
        let dsv = device
            .create_depth_stencil_view(&stencil_texture, &DepthStencilViewDesc::default())
            .unwrap();
        let stencil_view = device
            .create_texture_view(
                &stencil_texture,
                &TextureViewDesc {
                    format: Some(Format::R8Uint),
                    ..TextureViewDesc::SAMPLED
                },
            )
            .unwrap();
        let stencil = sample(
            device,
            "sample.stencil",
            Read {
                view: stencil_view.index(),
                sampler: 0,
                x: 0,
                uv: (0.0, 0.0),
            },
            4,
            |list| {
                cleared_depth(list, &stencil_texture, &dsv, 1.0, 0x5a);
            },
        );
        assert_eq!(stencil, [0x5a; 4]);

        let depth16 = device
            .create_texture(&TextureDesc::new_2d(
                2,
                2,
                Format::D16Unorm,
                TextureUsage::DEPTH_STENCIL | TextureUsage::SAMPLED,
            ))
            .unwrap();
        let dsv16 = device
            .create_depth_stencil_view(&depth16, &DepthStencilViewDesc::default())
            .unwrap();
        let view16 = device
            .create_texture_view(&depth16, &TextureViewDesc::SAMPLED)
            .unwrap();
        let point = device
            .create_sampler(&SamplerDesc {
                min_filter: Filter::Nearest,
                mag_filter: Filter::Nearest,
                mip_filter: Filter::Nearest,
                ..SamplerDesc::default()
            })
            .unwrap();
        let depth = sample(
            device,
            "sample",
            Read {
                view: view16.index(),
                sampler: point.index(),
                x: 0,
                uv: (0.5, 0.5),
            },
            1,
            |list| {
                cleared_depth(list, &depth16, &dsv16, 0.5, 0);
            },
        );
        let depth = f32::from_bits(depth[0]);
        assert!((depth - 0.5).abs() < 1e-4, "{depth}");
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn cube_arrays_sample_each_cube() {
    for device in devices() {
        let desc = TextureDesc {
            array_layers: 12,
            ..TextureDesc::new_2d(
                4,
                4,
                Format::Rgba8Unorm,
                TextureUsage::SAMPLED | TextureUsage::COPY_DST,
            )
        };
        let layers: Vec<Vec<u8>> = (0..12u8)
            .map(|l| [(l + 1) * 20, 0, 0, 255].repeat(16))
            .collect();
        let filled = Filled::new(device, &desc, &layers);
        let texture = &filled.texture;
        let view = device
            .create_texture_view(
                texture,
                &TextureViewDesc {
                    dimension: TextureViewDimension::CubeArray,
                    ..TextureViewDesc::SAMPLED
                },
            )
            .unwrap();
        let sampler = device.create_sampler(&SamplerDesc::default()).unwrap();
        let mut uploaded = false;
        let mut cube = |index: u32| {
            let first = !uploaded;
            uploaded = true;
            sample(
                device,
                "sample.cube",
                Read {
                    view: view.index(),
                    sampler: sampler.index(),
                    x: index,
                    uv: (0.0, 0.0),
                },
                1,
                |list| {
                    if first {
                        filled.upload(list);
                    }
                },
            )[0]
        };
        // +X is each cube's first face: layers 0 and 6.
        assert_eq!(cube(0), 20);
        assert_eq!(cube(1), 140);
        // A cube array needs whole cubes.
        let error = device
            .create_texture_view(
                texture,
                &TextureViewDesc {
                    dimension: TextureViewDimension::CubeArray,
                    range: SubresourceRange {
                        layer_count: 7,
                        ..SubresourceRange::ALL
                    },
                    ..TextureViewDesc::SAMPLED
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), marlin_rhi::ErrorKind::InvalidInput);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn view_ranges_do_not_wrap() {
    let device = support::hardware_device();
    let texture = device
        .create_texture(&TextureDesc {
            mip_levels: 3,
            ..TextureDesc::new_2d(4, 4, Format::Rgba8Unorm, TextureUsage::SAMPLED)
        })
        .unwrap();
    // 2 + (u32::MAX - 1) mips wraps to 0 where overflow checks are off.
    let error = device
        .create_texture_view(
            &texture,
            &TextureViewDesc {
                range: SubresourceRange {
                    base_mip: 2,
                    mip_count: u32::MAX - 1,
                    ..SubresourceRange::ALL
                },
                ..TextureViewDesc::SAMPLED
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), marlin_rhi::ErrorKind::InvalidInput);
}
