//! The global root signature, the bindless tables, indirect command signatures and pipeline state
//! objects.
//!
//! Every pipeline shares one root signature: [`MAX_PUSH_CONSTANTS`] root constants at `b0,
//! space0` (the push constants), with the CBV/SRV/UAV and sampler heaps directly indexed. Shaders
//! therefore need no root signature of their own, and binding never changes between draws.
//!
//! Indirect commands run through command signatures that first set push-constant word
//! [`INDIRECT_CONSTANT_WORD`] from each record, then draw or dispatch.

use core::ffi::c_void;
use core::ptr;

use alloc::sync::Arc;

use super::com::{self, *};
use super::ffi::*;
use super::resource::{DescriptorHeap, compare_func, dxgi_format};
use super::{Device, error, unsupported};
use crate::{
    BlendFactor, BlendOp, ColorTargetState, ComputePipelineDesc, CullMode, DepthState,
    DispatchIndirectArgs, DrawIndexedIndirectArgs, DrawIndirectArgs, Error, ErrorKind, FrontFace,
    GraphicsPipelineDesc, INDIRECT_CONSTANT_WORD, MAX_BINDLESS_RESOURCES, MAX_BINDLESS_SAMPLERS,
    MAX_PUSH_CONSTANTS, MeshPipelineDesc, PolygonMode, PrimitiveTopology, RasterState, Result,
    ShaderModule, StencilFace, StencilOp,
};

/// CPU-only render target and depth view heaps: enough for any frame's attachments.
const RTV_CAPACITY: u32 = 1024;
const DSV_CAPACITY: u32 = 256;

/// The device-wide binding state: descriptor heaps, the root signature and the command
/// signatures that use it.
pub(super) struct Bindless {
    pub(super) resources: Arc<DescriptorHeap>,
    pub(super) samplers: Arc<DescriptorHeap>,
    pub(super) rtv: Arc<DescriptorHeap>,
    pub(super) dsv: Arc<DescriptorHeap>,
    pub(super) root: Com<ID3D12RootSignature>,
    pub(super) signatures: Signatures,
}

/// One command signature per indirect command.
pub(super) struct Signatures {
    pub(super) draw: Com<ID3D12CommandSignature>,
    pub(super) draw_indexed: Com<ID3D12CommandSignature>,
    pub(super) dispatch: Com<ID3D12CommandSignature>,
    pub(super) dispatch_mesh: Com<ID3D12CommandSignature>,
}

impl Bindless {
    pub(super) fn new(device: &Com<ID3D12Device>) -> Result<Arc<Self>> {
        let root = create_root_signature(device)?;
        let signature = |ty, stride: usize| create_signature(device, &root, ty, stride as u32);
        let signatures = Signatures {
            draw: signature(
                D3D12_INDIRECT_ARGUMENT_TYPE_DRAW,
                size_of::<DrawIndirectArgs>(),
            )?,
            draw_indexed: signature(
                D3D12_INDIRECT_ARGUMENT_TYPE_DRAW_INDEXED,
                size_of::<DrawIndexedIndirectArgs>(),
            )?,
            dispatch: signature(
                D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH,
                size_of::<DispatchIndirectArgs>(),
            )?,
            dispatch_mesh: signature(
                D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH_MESH,
                size_of::<DispatchIndirectArgs>(),
            )?,
        };
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
            root,
            signatures,
        }))
    }
}

/// Records of `stride` bytes: the push-constant word, then the command's arguments.
fn create_signature(
    device: &Com<ID3D12Device>,
    root: &Com<ID3D12RootSignature>,
    ty: i32,
    stride: u32,
) -> Result<Com<ID3D12CommandSignature>> {
    let arguments = [
        D3D12_INDIRECT_ARGUMENT_DESC {
            Type: D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT,
            RootParameterIndex: 0,
            DestOffsetIn32BitValues: INDIRECT_CONSTANT_WORD,
            Num32BitValuesToSet: 1,
        },
        D3D12_INDIRECT_ARGUMENT_DESC {
            Type: ty,
            ..Default::default()
        },
    ];
    let desc = D3D12_COMMAND_SIGNATURE_DESC {
        ByteStride: stride,
        NumArgumentDescs: arguments.len() as u32,
        pArgumentDescs: arguments.as_ptr(),
        NodeMask: 0,
    };
    // SAFETY: valid interfaces, description (its arguments outlive the call), IID and out
    // pointer. The signature changes root arguments, so it names the root signature.
    com::create(|iid, out| unsafe {
        (device.vtbl().CreateCommandSignature)(device.as_raw(), &desc, root.as_raw(), iid, out)
    })
    .map_err(error)
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

pub(crate) struct MeshPipeline {
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

fn optional_bytecode(module: &Option<ShaderModule<'_>>) -> Result<D3D12_SHADER_BYTECODE> {
    match module {
        Some(module) => bytecode(module),
        None => Ok(D3D12_SHADER_BYTECODE::NONE),
    }
}

/// Blend state and render target formats.
fn blend_desc(color_targets: &[ColorTargetState]) -> (D3D12_BLEND_DESC, D3D12_RT_FORMAT_ARRAY) {
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
    let mut formats = D3D12_RT_FORMAT_ARRAY {
        RTFormats: [DXGI_FORMAT_UNKNOWN; 8],
        NumRenderTargets: color_targets.len() as u32,
    };
    for (i, target) in color_targets.iter().enumerate() {
        formats.RTFormats[i] = dxgi_format(target.format);
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
    let blend = D3D12_BLEND_DESC {
        AlphaToCoverageEnable: 0,
        IndependentBlendEnable: 1,
        RenderTarget: targets,
    };
    (blend, formats)
}

fn rasterizer_desc(raster: &RasterState) -> D3D12_RASTERIZER_DESC {
    D3D12_RASTERIZER_DESC {
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
    }
}

fn stencil_op(op: StencilOp) -> i32 {
    match op {
        StencilOp::Keep => D3D12_STENCIL_OP_KEEP,
        StencilOp::Zero => 2,
        StencilOp::Replace => 3,
        StencilOp::IncrementClamp => 4,
        StencilOp::DecrementClamp => 5,
        StencilOp::Invert => 6,
        StencilOp::IncrementWrap => 7,
        StencilOp::DecrementWrap => 8,
    }
}

fn stencil_face(face: &StencilFace) -> D3D12_DEPTH_STENCILOP_DESC {
    D3D12_DEPTH_STENCILOP_DESC {
        StencilFailOp: stencil_op(face.fail),
        StencilDepthFailOp: stencil_op(face.depth_fail),
        StencilPassOp: stencil_op(face.pass),
        StencilFunc: compare_func(face.compare),
    }
}

/// Depth-stencil state and the depth view format.
fn depth_stencil_desc(depth: &Option<DepthState>) -> (D3D12_DEPTH_STENCIL_DESC, u32) {
    let stencil = depth.and_then(|d| d.stencil).unwrap_or_default();
    let desc = D3D12_DEPTH_STENCIL_DESC {
        DepthEnable: depth.is_some() as i32,
        DepthWriteMask: depth.is_some_and(|d| d.write) as i32,
        DepthFunc: depth.map_or(D3D12_COMPARISON_FUNC_ALWAYS, |d| compare_func(d.compare)),
        StencilEnable: depth.is_some_and(|d| d.stencil.is_some()) as i32,
        StencilReadMask: stencil.read_mask,
        StencilWriteMask: stencil.write_mask,
        FrontFace: stencil_face(&stencil.front),
        BackFace: stencil_face(&stencil.back),
    };
    let format = depth.map_or(DXGI_FORMAT_UNKNOWN, |d| dxgi_format(d.format));
    (desc, format)
}

const NO_CACHE: D3D12_CACHED_PIPELINE_STATE = D3D12_CACHED_PIPELINE_STATE {
    pCachedBlob: ptr::null(),
    CachedBlobSizeInBytes: 0,
};

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
    let (blend, formats) = blend_desc(desc.color_targets);
    let (depth_stencil, dsv_format) = depth_stencil_desc(&desc.depth);
    let pso_desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        pRootSignature: device.bindless.root.as_raw(),
        VS: bytecode(&desc.vertex)?,
        PS: optional_bytecode(&desc.pixel)?,
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
        BlendState: blend,
        SampleMask: u32::MAX,
        RasterizerState: rasterizer_desc(&desc.raster),
        DepthStencilState: depth_stencil,
        InputLayout: D3D12_INPUT_LAYOUT_DESC {
            pInputElementDescs: ptr::null(),
            NumElements: 0,
        },
        IBStripCutValue: 0,
        PrimitiveTopologyType: topology_type,
        NumRenderTargets: formats.NumRenderTargets,
        RTVFormats: formats.RTFormats,
        DSVFormat: dsv_format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        NodeMask: 0,
        CachedPSO: NO_CACHE,
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
        CachedPSO: NO_CACHE,
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

/// One pipeline state stream subobject: its type, then its payload, aligned to a pointer
/// (`CD3DX12_PIPELINE_STATE_STREAM_SUBOBJECT`).
#[repr(C, align(8))]
struct Subobject<T> {
    ty: u32,
    inner: T,
}

impl<T> Subobject<T> {
    fn new(ty: u32, inner: T) -> Self {
        Self { ty, inner }
    }
}

/// A mesh pipeline as a state stream (`CD3DX12_PIPELINE_MESH_STATE_STREAM` without the
/// subobjects left at their defaults).
#[repr(C)]
struct MeshStream {
    root: Subobject<*mut c_void>,
    amplification: Subobject<D3D12_SHADER_BYTECODE>,
    mesh: Subobject<D3D12_SHADER_BYTECODE>,
    pixel: Subobject<D3D12_SHADER_BYTECODE>,
    blend: Subobject<D3D12_BLEND_DESC>,
    sample_mask: Subobject<u32>,
    raster: Subobject<D3D12_RASTERIZER_DESC>,
    depth_stencil: Subobject<D3D12_DEPTH_STENCIL_DESC>,
    render_targets: Subobject<D3D12_RT_FORMAT_ARRAY>,
    depth_format: Subobject<u32>,
    samples: Subobject<DXGI_SAMPLE_DESC>,
}

pub(super) fn create_mesh_pipeline(
    device: &Device,
    desc: &MeshPipelineDesc<'_>,
) -> Result<MeshPipeline> {
    let (blend, formats) = blend_desc(desc.color_targets);
    let (depth_stencil, dsv_format) = depth_stencil_desc(&desc.depth);
    let mut stream = MeshStream {
        root: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE,
            device.bindless.root.as_raw(),
        ),
        amplification: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_AS,
            optional_bytecode(&desc.amplification)?,
        ),
        mesh: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_MS,
            bytecode(&desc.mesh)?,
        ),
        pixel: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS,
            optional_bytecode(&desc.pixel)?,
        ),
        blend: Subobject::new(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND, blend),
        sample_mask: Subobject::new(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK, u32::MAX),
        raster: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER,
            rasterizer_desc(&desc.raster),
        ),
        depth_stencil: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL,
            depth_stencil,
        ),
        render_targets: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS,
            formats,
        ),
        depth_format: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT,
            dsv_format,
        ),
        samples: Subobject::new(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC,
            DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
        ),
    };
    let stream_desc = D3D12_PIPELINE_STATE_STREAM_DESC {
        SizeInBytes: size_of::<MeshStream>(),
        pPipelineStateSubobjectStream: (&raw mut stream).cast(),
    };
    let d = &device.device2;
    // SAFETY: valid interface; the stream is pointer-aligned subobjects whose pointers outlive
    // the call; valid IID and out pointer.
    let pso = com::create(|iid, out| unsafe {
        (d.vtbl().CreatePipelineState)(d.as_raw(), &stream_desc, iid, out)
    })
    .map_err(error)?;
    Ok(MeshPipeline { pso })
}

/// The raw pointers for `SetDescriptorHeaps`.
pub(super) fn heaps(bindless: &Bindless) -> [*mut c_void; 2] {
    [bindless.resources.as_raw(), bindless.samplers.as_raw()]
}
