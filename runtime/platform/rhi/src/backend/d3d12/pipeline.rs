//! The global root signature, the bindless tables, and pipeline state objects.
//!
//! Every pipeline shares one root signature: [`MAX_PUSH_CONSTANTS`] root constants at `b0,
//! space0` (the push constants), with the CBV/SRV/UAV and sampler heaps directly indexed. Shaders
//! therefore need no root signature of their own, and binding never changes between draws.

use core::ffi::c_void;
use core::ptr;

use alloc::sync::Arc;

use super::com::{self, *};
use super::ffi::*;
use super::resource::{DescriptorHeap, compare_func, dxgi_format};
use super::{Device, error, unsupported};
use crate::{
    BlendFactor, BlendOp, ComputePipelineDesc, CullMode, Error, ErrorKind, FrontFace,
    GraphicsPipelineDesc, MAX_BINDLESS_RESOURCES, MAX_BINDLESS_SAMPLERS, MAX_PUSH_CONSTANTS,
    PolygonMode, PrimitiveTopology, Result, ShaderModule,
};

/// CPU-only render target and depth view heaps: enough for any frame's attachments.
const RTV_CAPACITY: u32 = 1024;
const DSV_CAPACITY: u32 = 256;

/// The device-wide binding state: descriptor heaps and the root signature.
pub(super) struct Bindless {
    pub(super) resources: Arc<DescriptorHeap>,
    pub(super) samplers: Arc<DescriptorHeap>,
    pub(super) rtv: Arc<DescriptorHeap>,
    pub(super) dsv: Arc<DescriptorHeap>,
    pub(super) root: Com<ID3D12RootSignature>,
}

impl Bindless {
    pub(super) fn new(device: &Com<ID3D12Device>) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            resources: DescriptorHeap::new(
                device,
                D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                MAX_BINDLESS_RESOURCES,
                true,
            )?,
            samplers: DescriptorHeap::new(
                device,
                D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER,
                MAX_BINDLESS_SAMPLERS,
                true,
            )?,
            rtv: DescriptorHeap::new(device, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, RTV_CAPACITY, false)?,
            dsv: DescriptorHeap::new(device, D3D12_DESCRIPTOR_HEAP_TYPE_DSV, DSV_CAPACITY, false)?,
            root: create_root_signature(device)?,
        }))
    }
}

fn create_root_signature(device: &Com<ID3D12Device>) -> Result<Com<ID3D12RootSignature>> {
    let serialize = d3d12_serialize_versioned_root_signature().ok_or_else(unsupported)?;
    let push_constants = D3D12_ROOT_PARAMETER1 {
        ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
        Constants: D3D12_ROOT_CONSTANTS {
            ShaderRegister: 0,
            RegisterSpace: 0,
            Num32BitValues: MAX_PUSH_CONSTANTS,
            _union: 0,
        },
        ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
    };
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
        Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
            NumParameters: 1,
            pParameters: &push_constants,
            NumStaticSamplers: 0,
            pStaticSamplers: ptr::null(),
            Flags: D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED
                | D3D12_ROOT_SIGNATURE_FLAG_SAMPLER_HEAP_DIRECTLY_INDEXED,
        },
    };
    let mut blob = ptr::null_mut();
    let mut errors = ptr::null_mut();
    // SAFETY: valid description and out pointers.
    let hr = unsafe { serialize(&desc, &mut blob, &mut errors) };
    // SAFETY: each out pointer is null or an owned blob.
    let (blob, errors) = unsafe {
        (
            Com::<ID3DBlob>::from_raw(blob),
            Com::<ID3DBlob>::from_raw(errors),
        )
    };
    if let Some(errors) = errors {
        // SAFETY: valid blob; its contents are a NUL-terminated message.
        let message = unsafe {
            core::slice::from_raw_parts(
                (errors.vtbl().GetBufferPointer)(errors.as_raw()).cast::<u8>(),
                (errors.vtbl().GetBufferSize)(errors.as_raw()),
            )
        };
        marlin_diag::error!(
            "root signature: {}",
            core::str::from_utf8(message).unwrap_or("<invalid UTF-8>")
        );
    }
    let blob = match (hr, blob) {
        (0.., Some(blob)) => blob,
        (hr, _) => return Err(error(hr)),
    };
    // SAFETY: valid interfaces; the blob holds the serialized signature.
    com::create(|iid, out| unsafe {
        (device.vtbl().CreateRootSignature)(
            device.as_raw(),
            0,
            (blob.vtbl().GetBufferPointer)(blob.as_raw()),
            (blob.vtbl().GetBufferSize)(blob.as_raw()),
            iid,
            out,
        )
    })
    .map_err(error)
}

pub(crate) struct GraphicsPipeline {
    pub(super) pso: Com<ID3D12PipelineState>,
    pub(super) topology: i32,
}

pub(crate) struct ComputePipeline {
    pub(super) pso: Com<ID3D12PipelineState>,
}

/// DXIL arrives in a DXBC-style container; anything else (SPIR-V) is a caller error, not a
/// driver crash.
fn bytecode(module: &ShaderModule<'_>) -> Result<D3D12_SHADER_BYTECODE> {
    if !module.code.starts_with(b"DXBC") {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    Ok(D3D12_SHADER_BYTECODE {
        pShaderBytecode: module.code.as_ptr().cast(),
        BytecodeLength: module.code.len(),
    })
}

fn blend_factor(factor: BlendFactor) -> i32 {
    match factor {
        BlendFactor::Zero => 1,
        BlendFactor::One => 2,
        BlendFactor::SrcColor => 3,
        BlendFactor::OneMinusSrcColor => 4,
        BlendFactor::SrcAlpha => 5,
        BlendFactor::OneMinusSrcAlpha => 6,
        BlendFactor::DstAlpha => 7,
        BlendFactor::OneMinusDstAlpha => 8,
        BlendFactor::DstColor => 9,
        BlendFactor::OneMinusDstColor => 10,
    }
}

fn blend_op(op: BlendOp) -> i32 {
    match op {
        BlendOp::Add => 1,
        BlendOp::Subtract => 2,
        BlendOp::ReverseSubtract => 3,
        BlendOp::Min => 4,
        BlendOp::Max => 5,
    }
}

pub(super) fn create_graphics_pipeline(
    device: &Device,
    desc: &GraphicsPipelineDesc<'_>,
) -> Result<GraphicsPipeline> {
    let (topology_type, topology) = match desc.topology {
        PrimitiveTopology::PointList => (
            D3D12_PRIMITIVE_TOPOLOGY_TYPE_POINT,
            D3D_PRIMITIVE_TOPOLOGY_POINTLIST,
        ),
        PrimitiveTopology::LineList => (
            D3D12_PRIMITIVE_TOPOLOGY_TYPE_LINE,
            D3D_PRIMITIVE_TOPOLOGY_LINELIST,
        ),
        PrimitiveTopology::LineStrip => (
            D3D12_PRIMITIVE_TOPOLOGY_TYPE_LINE,
            D3D_PRIMITIVE_TOPOLOGY_LINESTRIP,
        ),
        PrimitiveTopology::TriangleList => (
            D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
        ),
        PrimitiveTopology::TriangleStrip => (
            D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
        ),
    };

    const NO_BLEND: D3D12_RENDER_TARGET_BLEND_DESC = D3D12_RENDER_TARGET_BLEND_DESC {
        BlendEnable: 0,
        LogicOpEnable: 0,
        SrcBlend: 2,
        DestBlend: 1,
        BlendOp: 1,
        SrcBlendAlpha: 2,
        DestBlendAlpha: 1,
        BlendOpAlpha: 1,
        LogicOp: D3D12_LOGIC_OP_NOOP,
        RenderTargetWriteMask: 0xf,
    };
    let mut targets = [NO_BLEND; 8];
    let mut formats = [DXGI_FORMAT_UNKNOWN; 8];
    for (i, target) in desc.color_targets.iter().enumerate() {
        formats[i] = dxgi_format(target.format);
        let t = &mut targets[i];
        t.RenderTargetWriteMask = target.write_mask.bits() as u8;
        if let Some(blend) = target.blend {
            t.BlendEnable = 1;
            t.SrcBlend = blend_factor(blend.color.src_factor);
            t.DestBlend = blend_factor(blend.color.dst_factor);
            t.BlendOp = blend_op(blend.color.op);
            t.SrcBlendAlpha = blend_factor(blend.alpha.src_factor);
            t.DestBlendAlpha = blend_factor(blend.alpha.dst_factor);
            t.BlendOpAlpha = blend_op(blend.alpha.op);
        }
    }

    let raster = &desc.raster;
    let keep = D3D12_DEPTH_STENCILOP_DESC {
        StencilFailOp: D3D12_STENCIL_OP_KEEP,
        StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
        StencilPassOp: D3D12_STENCIL_OP_KEEP,
        StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
    };
    let pso_desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        pRootSignature: device.bindless.root.as_raw(),
        VS: bytecode(&desc.vertex)?,
        PS: match &desc.pixel {
            Some(pixel) => bytecode(pixel)?,
            None => D3D12_SHADER_BYTECODE::NONE,
        },
        DS: D3D12_SHADER_BYTECODE::NONE,
        HS: D3D12_SHADER_BYTECODE::NONE,
        GS: D3D12_SHADER_BYTECODE::NONE,
        StreamOutput: D3D12_STREAM_OUTPUT_DESC {
            pSODeclaration: ptr::null(),
            NumEntries: 0,
            pBufferStrides: ptr::null(),
            NumStrides: 0,
            RasterizedStream: 0,
        },
        BlendState: D3D12_BLEND_DESC {
            AlphaToCoverageEnable: 0,
            IndependentBlendEnable: 1,
            RenderTarget: targets,
        },
        SampleMask: u32::MAX,
        RasterizerState: D3D12_RASTERIZER_DESC {
            FillMode: match raster.polygon {
                PolygonMode::Fill => D3D12_FILL_MODE_SOLID,
                PolygonMode::Line => D3D12_FILL_MODE_WIREFRAME,
            },
            CullMode: match raster.cull {
                CullMode::None => D3D12_CULL_MODE_NONE,
                CullMode::Front => D3D12_CULL_MODE_FRONT,
                CullMode::Back => D3D12_CULL_MODE_BACK,
            },
            FrontCounterClockwise: (raster.front_face == FrontFace::CounterClockwise) as i32,
            DepthBias: raster.depth_bias,
            DepthBiasClamp: raster.depth_bias_clamp,
            SlopeScaledDepthBias: raster.depth_bias_slope,
            DepthClipEnable: raster.depth_clip as i32,
            MultisampleEnable: 0,
            AntialiasedLineEnable: 0,
            ForcedSampleCount: 0,
            ConservativeRaster: 0,
        },
        DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
            DepthEnable: desc.depth.is_some() as i32,
            DepthWriteMask: desc.depth.is_some_and(|d| d.write) as i32,
            DepthFunc: desc
                .depth
                .map_or(D3D12_COMPARISON_FUNC_ALWAYS, |d| compare_func(d.compare)),
            StencilEnable: 0,
            StencilReadMask: 0xff,
            StencilWriteMask: 0xff,
            FrontFace: keep,
            BackFace: keep,
        },
        InputLayout: D3D12_INPUT_LAYOUT_DESC {
            pInputElementDescs: ptr::null(),
            NumElements: 0,
        },
        IBStripCutValue: 0,
        PrimitiveTopologyType: topology_type,
        NumRenderTargets: desc.color_targets.len() as u32,
        RTVFormats: formats,
        DSVFormat: desc
            .depth
            .map_or(DXGI_FORMAT_UNKNOWN, |d| dxgi_format(d.format)),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        NodeMask: 0,
        CachedPSO: D3D12_CACHED_PIPELINE_STATE {
            pCachedBlob: ptr::null(),
            CachedBlobSizeInBytes: 0,
        },
        Flags: 0,
    };
    let d = &device.device;
    // SAFETY: valid interface, description (its pointers outlive the call), IID and out pointer.
    let pso = com::create(|iid, out| unsafe {
        (d.vtbl().CreateGraphicsPipelineState)(d.as_raw(), &pso_desc, iid, out)
    })
    .map_err(error)?;
    Ok(GraphicsPipeline { pso, topology })
}

pub(super) fn create_compute_pipeline(
    device: &Device,
    desc: &ComputePipelineDesc<'_>,
) -> Result<ComputePipeline> {
    let pso_desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
        pRootSignature: device.bindless.root.as_raw(),
        CS: bytecode(&desc.shader)?,
        NodeMask: 0,
        CachedPSO: D3D12_CACHED_PIPELINE_STATE {
            pCachedBlob: ptr::null(),
            CachedBlobSizeInBytes: 0,
        },
        Flags: 0,
    };
    let d = &device.device;
    // SAFETY: valid interface, description, IID and out pointer.
    let pso = com::create(|iid, out| unsafe {
        (d.vtbl().CreateComputePipelineState)(d.as_raw(), &pso_desc, iid, out)
    })
    .map_err(error)?;
    Ok(ComputePipeline { pso })
}

/// The raw pointers for `SetDescriptorHeaps`.
pub(super) fn heaps(bindless: &Bindless) -> [*mut c_void; 2] {
    [bindless.resources.as_raw(), bindless.samplers.as_raw()]
}
