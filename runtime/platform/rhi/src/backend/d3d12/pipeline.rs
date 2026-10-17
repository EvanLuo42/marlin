//! The global root signature, the bindless tables, indirect command signatures, pipeline state
//! objects and the pipeline cache.
//!
//! Every pipeline shares one root signature: [`MAX_PUSH_CONSTANTS`] root constants at `b0,
//! space0` (the push constants), with the CBV/SRV/UAV and sampler heaps directly indexed. Shaders
//! therefore need no root signature of their own, and binding never changes between draws.
//!
//! Indirect commands run through command signatures that first set push-constant word
//! [`INDIRECT_CONSTANT_WORD`] from each record, then draw or dispatch.
//!
//! Pipelines are built as pipeline state streams. A [`PipelineCache`] is a D3D12 pipeline
//! library keyed by a 128-bit hash of the whole public description, shader bytecode included.

use core::ffi::c_void;
use core::hash::{Hash, Hasher};
use core::ptr;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_sync::Mutex;

use super::com::{self, *};
use super::ffi::*;
use super::resource::{CpuDescriptors, DescriptorHeap, compare_func, dxgi_format};
use super::{Device, error, unsupported};
use crate::{
    BlendFactor, BlendOp, ColorTargetState, ComputePipelineDesc, CullMode, DepthState,
    DispatchIndirectArgs, DrawIndexedIndirectArgs, DrawIndirectArgs, Error, ErrorKind, FrontFace,
    GraphicsPipelineDesc, INDIRECT_CONSTANT_WORD, IndexFormat, MAX_BINDLESS_RESOURCES,
    MAX_BINDLESS_SAMPLERS, MAX_PUSH_CONSTANTS, MeshPipelineDesc, MultisampleState, PolygonMode,
    PrimitiveTopology, RasterState, Result, ShaderModule, StencilFace, StencilOp,
};

/// Slots per chunk of the CPU-only render target, depth and clear view heaps.
const RTV_CHUNK: u32 = 1024;
const DSV_CHUNK: u32 = 256;
const CLEAR_CHUNK: u32 = 1024;

/// The device-wide binding state: descriptor heaps, the root signature and the command
/// signatures that use it.
pub(super) struct Bindless {
    pub(super) resources: Arc<DescriptorHeap>,
    pub(super) samplers: Arc<DescriptorHeap>,
    pub(super) rtv: CpuDescriptors,
    pub(super) dsv: CpuDescriptors,
    /// CPU-only CBV/SRV/UAV slots: the CPU half of `ClearUnorderedAccessView*`.
    pub(super) clear: CpuDescriptors,
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
            rtv: CpuDescriptors::new(device, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, RTV_CHUNK),
            dsv: CpuDescriptors::new(device, D3D12_DESCRIPTOR_HEAP_TYPE_DSV, DSV_CHUNK),
            clear: CpuDescriptors::new(device, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, CLEAR_CHUNK),
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

fn optional_bytecode(module: &Option<ShaderModule<'_>>) -> Result<D3D12_SHADER_BYTECODE> {
    match module {
        Some(module) => bytecode(module),
        None => Ok(D3D12_SHADER_BYTECODE::NONE),
    }
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
        BlendFactor::SrcAlphaSaturated => 11,
        BlendFactor::Constant => 14,
        BlendFactor::OneMinusConstant => 15,
        BlendFactor::Src1Color => 16,
        BlendFactor::OneMinusSrc1Color => 17,
        BlendFactor::Src1Alpha => 18,
        BlendFactor::OneMinusSrc1Alpha => 19,
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

/// Blend state and render target formats.
fn blend_desc(
    color_targets: &[ColorTargetState],
    multisample: &MultisampleState,
) -> (D3D12_BLEND_DESC, D3D12_RT_FORMAT_ARRAY) {
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
        AlphaToCoverageEnable: multisample.alpha_to_coverage as i32,
        IndependentBlendEnable: 1,
        RenderTarget: targets,
    };
    (blend, formats)
}

fn rasterizer_desc(raster: &RasterState, multisample: &MultisampleState) -> D3D12_RASTERIZER_DESC {
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
        // Only changes line rasterization: multisampled lines are quadrilaterals.
        MultisampleEnable: (multisample.count > 1) as i32,
        AntialiasedLineEnable: 0,
        ForcedSampleCount: 0,
        ConservativeRaster: match raster.conservative {
            true => D3D12_CONSERVATIVE_RASTERIZATION_MODE_ON,
            false => 0,
        },
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
fn depth_stencil_desc(depth: &Option<DepthState>) -> (D3D12_DEPTH_STENCIL_DESC1, u32) {
    let stencil = depth.and_then(|d| d.stencil).unwrap_or_default();
    let desc = D3D12_DEPTH_STENCIL_DESC1 {
        DepthEnable: depth.is_some() as i32,
        DepthWriteMask: depth.is_some_and(|d| d.write) as i32,
        DepthFunc: depth.map_or(D3D12_COMPARISON_FUNC_ALWAYS, |d| compare_func(d.compare)),
        StencilEnable: depth.is_some_and(|d| d.stencil.is_some()) as i32,
        StencilReadMask: stencil.read_mask,
        StencilWriteMask: stencil.write_mask,
        FrontFace: stencil_face(&stencil.front),
        BackFace: stencil_face(&stencil.back),
        DepthBoundsTestEnable: depth.is_some_and(|d| d.bounds_test) as i32,
    };
    let format = depth.map_or(DXGI_FORMAT_UNKNOWN, |d| {
        super::resource::depth_format(d.format)
    });
    (desc, format)
}

// --- Pipeline state streams ---

/// One pipeline state stream subobject: its type, then its payload, aligned to a pointer
/// (`CD3DX12_PIPELINE_STATE_STREAM_SUBOBJECT`).
#[repr(C, align(8))]
struct Subobject<T> {
    ty: u32,
    inner: T,
}

/// A pipeline state stream under construction: pointer-aligned subobjects back to back.
struct Stream {
    words: Vec<u64>,
}

impl Stream {
    fn new(root: &Com<ID3D12RootSignature>) -> Self {
        let mut stream = Self {
            words: Vec::with_capacity(128),
        };
        stream.push(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE,
            root.as_raw(),
        );
        stream
    }

    fn push<T: Copy>(&mut self, ty: u32, inner: T) {
        let subobject = Subobject { ty, inner };
        let words = size_of::<Subobject<T>>() / 8;
        let at = self.words.len();
        self.words.resize(at + words, 0);
        // SAFETY: the new words hold `size_of::<Subobject<T>>()` bytes, 8-aligned like the
        // subobject; `T` is plain data.
        unsafe {
            ptr::write(
                self.words.as_mut_ptr().add(at).cast::<Subobject<T>>(),
                subobject,
            )
        };
    }

    fn desc(&mut self) -> D3D12_PIPELINE_STATE_STREAM_DESC {
        D3D12_PIPELINE_STATE_STREAM_DESC {
            SizeInBytes: self.words.len() * 8,
            pPipelineStateSubobjectStream: self.words.as_mut_ptr().cast(),
        }
    }
}

/// The fixed-function subobjects graphics and mesh pipelines share.
fn push_output_state(
    stream: &mut Stream,
    raster: &RasterState,
    depth: &Option<DepthState>,
    color_targets: &[ColorTargetState],
    multisample: &MultisampleState,
) {
    let (blend, formats) = blend_desc(color_targets, multisample);
    let (depth_stencil, dsv_format) = depth_stencil_desc(depth);
    stream.push(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND, blend);
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK,
        multisample.mask,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER,
        rasterizer_desc(raster, multisample),
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1,
        depth_stencil,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS,
        formats,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT,
        dsv_format,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC,
        DXGI_SAMPLE_DESC {
            Count: multisample.count,
            Quality: 0,
        },
    );
}

/// Creates the pipeline `stream` describes, from `cache`'s compiled blob for `key` when it has
/// one, storing the driver's blob there when it has not.
fn create_pso(
    device: &Device,
    stream: &mut Stream,
    cache: Option<&crate::PipelineCache>,
    key: impl FnOnce() -> Key,
) -> Result<Com<ID3D12PipelineState>> {
    let d = &device.device5;
    let create = |stream: &mut Stream| {
        let desc = stream.desc();
        // SAFETY: valid interface; the stream is pointer-aligned subobjects whose pointers
        // outlive the call; valid IID and out pointer.
        com::create::<ID3D12PipelineState>(|iid, out| unsafe {
            (d.vtbl().CreatePipelineState)(d.as_raw(), &desc, iid, out)
        })
    };
    let Some(cache) = cache.map(|c| &c.raw) else {
        return create(stream).map_err(error);
    };
    let key = key().finish128();
    if let Some(blob) = cache.get(&key) {
        let mut cached = Stream {
            words: stream.words.clone(),
        };
        cached.push(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_CACHED_PSO,
            D3D12_CACHED_PIPELINE_STATE {
                pCachedBlob: blob.as_ptr().cast(),
                CachedBlobSizeInBytes: blob.len(),
            },
        );
        match create(&mut cached) {
            Ok(pso) => return Ok(pso),
            // A blob the driver no longer takes: compile afresh and replace it.
            Err(_) => cache.remove(&key),
        }
    }
    let pso = create(stream).map_err(error)?;
    if let Some(blob) = cached_blob(&pso) {
        cache.insert(key, blob);
    }
    Ok(pso)
}

/// The driver's compiled form of `pso`.
fn cached_blob(pso: &Com<ID3D12PipelineState>) -> Option<Arc<[u8]>> {
    let mut blob = ptr::null_mut();
    // SAFETY: valid interface and out pointer.
    if unsafe { (pso.vtbl().GetCachedBlob)(pso.as_raw(), &mut blob) } < 0 {
        return None;
    }
    // SAFETY: success returned an owned blob.
    let blob = unsafe { Com::<ID3DBlob>::from_raw(blob) }?;
    // SAFETY: valid blob; its pointer covers its size.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (blob.vtbl().GetBufferPointer)(blob.as_raw()).cast::<u8>(),
            (blob.vtbl().GetBufferSize)(blob.as_raw()),
        )
    };
    Some(Arc::from(bytes))
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
    let strip_cut = match desc.primitive_restart {
        None => D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED,
        Some(IndexFormat::U16) => D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_0XFFFF,
        Some(IndexFormat::U32) => D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_0XFFFFFFFF,
    };
    let mut stream = Stream::new(&device.bindless.root);
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_VS,
        bytecode(&desc.vertex)?,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS,
        optional_bytecode(&desc.pixel)?,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_IB_STRIP_CUT_VALUE,
        strip_cut,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PRIMITIVE_TOPOLOGY,
        topology_type,
    );
    push_output_state(
        &mut stream,
        &desc.raster,
        &desc.depth,
        desc.color_targets,
        &desc.multisample,
    );
    let pso = create_pso(device, &mut stream, desc.cache, || {
        let mut key = Key::new(b'g');
        desc.vertex.hash(&mut key);
        desc.pixel.hash(&mut key);
        desc.topology.hash(&mut key);
        desc.primitive_restart.hash(&mut key);
        desc.raster.hash(&mut key);
        desc.depth.hash(&mut key);
        desc.color_targets.hash(&mut key);
        desc.multisample.hash(&mut key);
        key
    })?;
    Ok(GraphicsPipeline { pso, topology })
}

pub(super) fn create_compute_pipeline(
    device: &Device,
    desc: &ComputePipelineDesc<'_>,
) -> Result<ComputePipeline> {
    let mut stream = Stream::new(&device.bindless.root);
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_CS,
        bytecode(&desc.shader)?,
    );
    let pso = create_pso(device, &mut stream, desc.cache, || {
        let mut key = Key::new(b'c');
        desc.shader.hash(&mut key);
        key
    })?;
    Ok(ComputePipeline { pso })
}

pub(super) fn create_mesh_pipeline(
    device: &Device,
    desc: &MeshPipelineDesc<'_>,
) -> Result<MeshPipeline> {
    let mut stream = Stream::new(&device.bindless.root);
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_AS,
        optional_bytecode(&desc.amplification)?,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_MS,
        bytecode(&desc.mesh)?,
    );
    stream.push(
        D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS,
        optional_bytecode(&desc.pixel)?,
    );
    push_output_state(
        &mut stream,
        &desc.raster,
        &desc.depth,
        desc.color_targets,
        &desc.multisample,
    );
    let pso = create_pso(device, &mut stream, desc.cache, || {
        let mut key = Key::new(b'm');
        desc.amplification.hash(&mut key);
        desc.mesh.hash(&mut key);
        desc.pixel.hash(&mut key);
        desc.raster.hash(&mut key);
        desc.depth.hash(&mut key);
        desc.color_targets.hash(&mut key);
        desc.multisample.hash(&mut key);
        key
    })?;
    Ok(MeshPipeline { pso })
}

/// The raw pointers for `SetDescriptorHeaps`.
pub(super) fn heaps(bindless: &Bindless) -> [*mut c_void; 2] {
    [bindless.resources.as_raw(), bindless.samplers.as_raw()]
}

// --- Pipeline cache ---

/// Bumped when the key's contents or the stream layout change, so old entries are never
/// mistaken for new ones.
const KEY_VERSION: u64 = 1;

/// A 128-bit pipeline key: two keyed SipHash-1-3 streams over the whole description.
#[allow(
    deprecated,
    reason = "core's SipHasher is the only keyed hasher in core"
)]
struct Key {
    a: core::hash::SipHasher,
    b: core::hash::SipHasher,
}

#[allow(
    deprecated,
    reason = "core's SipHasher is the only keyed hasher in core"
)]
impl Key {
    fn new(kind: u8) -> Self {
        let mut key = Self {
            a: core::hash::SipHasher::new_with_keys(0x6d61_726c_696e_0001, KEY_VERSION),
            b: core::hash::SipHasher::new_with_keys(0x6d61_726c_696e_0002, !KEY_VERSION),
        };
        key.write_u8(kind);
        key
    }

    fn finish128(&self) -> [u64; 2] {
        [self.a.finish(), self.b.finish()]
    }
}

#[allow(
    deprecated,
    reason = "core's SipHasher is the only keyed hasher in core"
)]
impl Hasher for Key {
    fn write(&mut self, bytes: &[u8]) {
        self.a.write(bytes);
        self.b.write(bytes);
    }

    fn finish(&self) -> u64 {
        self.a.finish()
    }
}

/// Compiled pipelines by key: the driver's cached blobs (`GetCachedBlob`), fed back through
/// `CachedPSO` when the same description is created again.
///
/// Per-pipeline blobs rather than an `ID3D12PipelineLibrary`: libraries deadlock in the driver
/// when used while other threads work on the device, and pipelines loaded from one depend on
/// memory the library does not own.
///
/// Serialized caches start with a header naming the adapter and driver they were built with and
/// checksumming the entries, so a cache from another machine, a driver update or a damaged file
/// is dropped before the driver sees it.
pub(crate) struct PipelineCache {
    entries: Mutex<BTreeMap<[u64; 2], Arc<[u8]>>>,
    identity: [u64; 3],
}

const CACHE_MAGIC: [u8; 8] = *b"MRLNPSO2";
/// Magic, adapter and driver identity, entry count, entries' checksum.
const CACHE_HEADER: usize = 8 + 24 + 8 + 8;

/// FNV-1a: a checksum against truncated or damaged files, not an adversary.
fn checksum(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The entries in `data` if its header matches this adapter and driver and nothing is damaged.
fn parse_cache(data: &[u8], identity: &[u64; 3]) -> Option<BTreeMap<[u64; 2], Arc<[u8]>>> {
    let word = |at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
    };
    if data.get(..8)? != CACHE_MAGIC {
        return None;
    }
    if [word(8)?, word(16)?, word(24)?] != *identity {
        return None;
    }
    let (count, sum) = (word(32)?, word(40)?);
    if checksum(&data[CACHE_HEADER..]) != sum {
        return None;
    }
    let mut entries = BTreeMap::new();
    let mut at = CACHE_HEADER;
    for _ in 0..count {
        let key = [word(at)?, word(at + 8)?];
        let len = usize::try_from(word(at + 16)?).ok()?;
        let blob = data.get(at + 24..at.checked_add(24 + len)?)?;
        entries.insert(key, Arc::from(blob));
        at += 24 + len.next_multiple_of(8);
    }
    (at == data.len()).then_some(entries)
}

impl PipelineCache {
    pub(super) fn new(device: &Device, data: &[u8]) -> Result<Self> {
        let identity = device.cache_identity;
        let entries = match data.is_empty() {
            true => BTreeMap::new(),
            false => parse_cache(data, &identity).unwrap_or_else(|| {
                marlin_diag::info!(
                    "pipeline cache from another adapter or driver, or damaged: starting empty"
                );
                BTreeMap::new()
            }),
        };
        Ok(Self {
            entries: Mutex::new(entries),
            identity,
        })
    }

    fn get(&self, key: &[u64; 2]) -> Option<Arc<[u8]>> {
        self.entries.lock().get(key).cloned()
    }

    fn insert(&self, key: [u64; 2], blob: Arc<[u8]>) {
        self.entries.lock().insert(key, blob);
    }

    fn remove(&self, key: &[u64; 2]) {
        self.entries.lock().remove(key);
    }

    pub(super) fn serialize(&self) -> Result<Vec<u8>> {
        let entries = self.entries.lock();
        let mut data = Vec::with_capacity(
            CACHE_HEADER + entries.values().map(|b| 24 + b.len() + 8).sum::<usize>(),
        );
        data.extend_from_slice(&CACHE_MAGIC);
        for word in self.identity {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        data.extend_from_slice(&[0; 8]);
        for (key, blob) in entries.iter() {
            for word in [key[0], key[1], blob.len() as u64] {
                data.extend_from_slice(&word.to_le_bytes());
            }
            data.extend_from_slice(blob);
            data.resize(data.len().next_multiple_of(8), 0);
        }
        let sum = checksum(&data[CACHE_HEADER..]);
        data[40..48].copy_from_slice(&sum.to_le_bytes());
        Ok(data)
    }
}
