//! Resource, view, sampler and pipeline creation on the hardware adapter, with validation on.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    BufferDesc, BufferUsage, BufferViewDesc, BufferViewKind, ClearValue, ColorTargetState,
    ComputePipelineDesc, DepthState, DepthStencilViewDesc, ErrorKind, Format, GraphicsPipelineDesc,
    MemoryLocation, PrimitiveTopology, RasterState, RenderTargetViewDesc, SHADER_FORMAT,
    SamplerDesc, ShaderFormat, ShaderModule, TextureDesc, TextureUsage, TextureViewDesc,
    TextureViewDimension, UNIFORM_ALIGNMENT,
};
use support::{gpu_test, hardware_device, shader, skip};

#[test]
fn baseline_features() {
    let features = hardware_device().features();
    // Every backend refuses devices without mesh shaders.
    assert!(features.mesh_shader, "{features:?}");
}

#[gpu_test(api = d3d12)]
fn d3d12_baseline_is_shader_model_6_6() {
    let features = hardware_device().features();
    assert!(features.shader_model >= (6, 6), "{features:?}");
}

#[gpu_test(api = vulkan)]
fn vulkan_barriers_are_always_enhanced() {
    let features = hardware_device().features();
    assert!(features.enhanced_barriers, "{features:?}");
}

#[test]
fn buffers_and_mappings() {
    let device = hardware_device();
    let gpu = device
        .create_buffer(&BufferDesc {
            size: 1000,
            usage: BufferUsage::SHADER_READ | BufferUsage::SHADER_WRITE | BufferUsage::COPY_DST,
            memory: MemoryLocation::GpuOnly,
        })
        .unwrap();
    assert!(gpu.mapped().is_none());
    assert_eq!(gpu.size(), 1000);

    let upload = device
        .create_buffer(&BufferDesc {
            size: 64,
            usage: BufferUsage::COPY_SRC,
            memory: MemoryLocation::Upload,
        })
        .unwrap();
    // SAFETY: no GPU work exists.
    unsafe { upload.write(0, &[7; 64]) };

    let readback = device
        .create_buffer(&BufferDesc {
            size: 64,
            usage: BufferUsage::COPY_DST,
            memory: MemoryLocation::Readback,
        })
        .unwrap();
    assert!(readback.mapped().is_some());

    let invalid = |desc| device.create_buffer(&desc).unwrap_err().kind();
    assert_eq!(
        invalid(BufferDesc {
            size: 0,
            usage: BufferUsage::COPY_DST,
            memory: MemoryLocation::GpuOnly,
        }),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        invalid(BufferDesc {
            size: 64,
            usage: BufferUsage::SHADER_WRITE,
            memory: MemoryLocation::Upload,
        }),
        ErrorKind::InvalidInput
    );
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn buffer_views_take_distinct_slots_and_reuse_freed_ones() {
    let device = hardware_device();
    let buffer = device
        .create_buffer(&BufferDesc {
            size: 4096,
            usage: BufferUsage::UNIFORM | BufferUsage::SHADER_READ | BufferUsage::SHADER_WRITE,
            memory: MemoryLocation::GpuOnly,
        })
        .unwrap();
    let uniform = device
        .create_buffer_view(
            &buffer,
            &BufferViewDesc {
                kind: BufferViewKind::Uniform,
                writable: false,
                offset: UNIFORM_ALIGNMENT,
                size: 100,
            },
        )
        .unwrap();
    let structured = device
        .create_buffer_view(
            &buffer,
            &BufferViewDesc::whole(BufferViewKind::Structured { stride: 16 }, false),
        )
        .unwrap();
    let raw = device
        .create_buffer_view(&buffer, &BufferViewDesc::whole(BufferViewKind::Raw, true))
        .unwrap();
    let mut indices = [uniform.index(), structured.index(), raw.index()];
    indices.sort();
    assert!(indices.windows(2).all(|w| w[0] != w[1]));

    let freed = raw.index();
    drop(raw);
    let again = device
        .create_buffer_view(&buffer, &BufferViewDesc::whole(BufferViewKind::Raw, false))
        .unwrap();
    assert_eq!(again.index(), freed);

    let invalid = |desc| {
        device
            .create_buffer_view(&buffer, &desc)
            .unwrap_err()
            .kind()
    };
    // Misaligned uniform offset; stride not dividing the offset; past the end.
    for desc in [
        BufferViewDesc {
            kind: BufferViewKind::Uniform,
            writable: false,
            offset: 16,
            size: 16,
        },
        BufferViewDesc {
            kind: BufferViewKind::Structured { stride: 12 },
            writable: false,
            offset: 16,
            size: 24,
        },
        BufferViewDesc {
            kind: BufferViewKind::Raw,
            writable: false,
            offset: 4000,
            size: 200,
        },
    ] {
        assert_eq!(invalid(desc), ErrorKind::InvalidInput, "{desc:?}");
    }
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn textures_and_views() {
    let device = hardware_device();
    let color = device
        .create_texture(&TextureDesc {
            array_layers: 6,
            mip_levels: 3,
            clear_value: Some(ClearValue::Color([0.0, 0.0, 0.0, 1.0])),
            ..TextureDesc::new_2d(
                64,
                64,
                Format::Rgba16Float,
                TextureUsage::RENDER_TARGET | TextureUsage::SAMPLED | TextureUsage::STORAGE,
            )
        })
        .unwrap();
    device
        .create_render_target_view(
            &color,
            &RenderTargetViewDesc {
                mip: 1,
                layer: 5,
                ..Default::default()
            },
        )
        .unwrap();
    device
        .create_texture_view(
            &color,
            &TextureViewDesc {
                dimension: TextureViewDimension::Cube,
                ..TextureViewDesc::SAMPLED
            },
        )
        .unwrap();
    device
        .create_texture_view(
            &color,
            &TextureViewDesc {
                writable: true,
                dimension: TextureViewDimension::D2Array,
                format: None,
                range: marlin_rhi::SubresourceRange {
                    base_mip: 2,
                    mip_count: 1,
                    base_layer: 0,
                    layer_count: u32::MAX,
                },
            },
        )
        .unwrap();

    let depth = device
        .create_texture(&TextureDesc {
            clear_value: Some(ClearValue::Depth(0.0)),
            ..TextureDesc::new_2d(
                128,
                64,
                Format::D32Float,
                TextureUsage::DEPTH_STENCIL | TextureUsage::SAMPLED,
            )
        })
        .unwrap();
    device
        .create_depth_stencil_view(&depth, &DepthStencilViewDesc::default())
        .unwrap();
    // Depth reads as R32Float.
    device
        .create_texture_view(&depth, &TextureViewDesc::SAMPLED)
        .unwrap();

    let invalid = |desc: TextureDesc| device.create_texture(&desc).unwrap_err().kind();
    for desc in [
        TextureDesc::new_2d(0, 4, Format::Rgba8Unorm, TextureUsage::SAMPLED),
        TextureDesc::new_2d(4, 4, Format::D32Float, TextureUsage::RENDER_TARGET),
        TextureDesc::new_2d(4, 4, Format::Rgba8UnormSrgb, TextureUsage::STORAGE),
        TextureDesc::new_2d(6, 4, Format::Bc7Unorm, TextureUsage::SAMPLED),
        TextureDesc {
            mip_levels: 4,
            ..TextureDesc::new_2d(4, 4, Format::Rgba8Unorm, TextureUsage::SAMPLED)
        },
    ] {
        assert_eq!(invalid(desc), ErrorKind::InvalidInput, "{desc:?}");
    }
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn samplers() {
    let device = hardware_device();
    let a = device.create_sampler(&SamplerDesc::default()).unwrap();
    let b = device
        .create_sampler(&SamplerDesc {
            max_anisotropy: 16,
            compare: Some(marlin_rhi::CompareOp::Greater),
            ..SamplerDesc::default()
        })
        .unwrap();
    assert_ne!(a.index(), b.index());
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn pipelines() {
    let device = hardware_device();
    device
        .create_compute_pipeline(&ComputePipelineDesc {
            shader: shader("Fill"),
            cache: None,
        })
        .unwrap();
    device
        .create_graphics_pipeline(&GraphicsPipelineDesc {
            vertex: shader("QuadVs"),
            pixel: Some(shader("QuadPs")),
            topology: PrimitiveTopology::TriangleList,
            raster: RasterState::default(),
            depth: Some(DepthState::reverse_z(Format::D32Float)),
            color_targets: &[ColorTargetState::new(Format::Rgba8Unorm)],
            multisample: marlin_rhi::MultisampleState::NONE,
            primitive_restart: None,
            cache: None,
        })
        .unwrap();
    assert_eq!(device.validation_errors(), 0);
}

/// Bytecode in another format than the backend's is refused, not passed to the driver.
#[test]
fn shaders_in_another_format_are_rejected() {
    let device = hardware_device();
    // The start of the format the backend does not take.
    let other: &[u8] = match SHADER_FORMAT {
        // A SPIR-V magic number.
        ShaderFormat::Dxil => &[0x03, 0x02, 0x23, 0x07, 0, 0, 0, 0],
        // A DXBC container header.
        ShaderFormat::SpirV => &[b'D', b'X', b'B', b'C', 0, 0, 0, 0],
        format => skip!("no foreign bytecode for {format:?}"),
    };
    let error = device
        .create_compute_pipeline(&ComputePipelineDesc {
            shader: ShaderModule {
                code: other,
                entry: "main",
            },
            cache: None,
        })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert_eq!(device.validation_errors(), 0);
}
