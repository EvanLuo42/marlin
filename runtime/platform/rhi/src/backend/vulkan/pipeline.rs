//! Pipelines: shader stages (SPIR-V plus the descriptor heap mapping, 0016 §4.2), compute
//! pipelines, the pipeline cache, and graphics and mesh pipelines (draw area). No pipeline has a
//! `VkPipelineLayout`: all are created with `VK_PIPELINE_CREATE_2_DESCRIPTOR_HEAP_BIT_EXT`.

use core::ptr;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::device::{Device, Shared};
use super::diag::c_name;
use super::ffi::*;
use super::{check, invalid};
use crate::{ComputePipelineDesc, GraphicsPipelineDesc, MeshPipelineDesc, Result, ShaderModule};

const SPIRV_MAGIC: u32 = 0x0723_0203;

/// One shader stage of a pipeline being created: the SPIR-V (copied to `u32` alignment), its
/// entry point, and the set 0 mapping onto the descriptor heaps.
pub(crate) struct ShaderStage {
    code: Vec<u32>,
    entry: Vec<u8>,
    stage: VkShaderStageFlags,
    module: VkShaderModuleCreateInfo,
    mappings: [VkDescriptorSetAndBindingMappingEXT; 3],
    mapping: VkShaderDescriptorSetAndBindingMappingInfoEXT,
}

impl ShaderStage {
    /// `InvalidInput` unless `module` is SPIR-V.
    pub(crate) fn new(
        shared: &Shared,
        module: &ShaderModule<'_>,
        stage: VkShaderStageFlags,
    ) -> Result<Self> {
        let bytes = module.code;
        if bytes.len() < 20 || !bytes.len().is_multiple_of(4) {
            return Err(invalid());
        }
        let code: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        if code[0] != SPIRV_MAGIC {
            return Err(invalid());
        }
        Ok(Self {
            code,
            entry: c_name(module.entry),
            stage,
            module: VkShaderModuleCreateInfo::default(),
            mappings: shared.bindless.mappings(),
            mapping: VkShaderDescriptorSetAndBindingMappingInfoEXT::default(),
        })
    }

    /// The stage's create info. It points into `self`, which must not move or drop until the
    /// pipeline is created.
    pub(crate) fn info(&mut self) -> VkPipelineShaderStageCreateInfo {
        self.module.codeSize = self.code.len() * 4;
        self.module.pCode = self.code.as_ptr();
        self.mapping.pNext = (&raw const self.module).cast();
        self.mapping.mappingCount = self.mappings.len() as u32;
        self.mapping.pMappings = self.mappings.as_ptr();
        VkPipelineShaderStageCreateInfo {
            pNext: (&raw const self.mapping).cast(),
            stage: self.stage as VkShaderStageFlagBits,
            module: 0,
            pName: self.entry.as_ptr().cast(),
            ..Default::default()
        }
    }
}

/// The create flags every pipeline takes, to chain into its create info.
pub(crate) fn heap_flags() -> VkPipelineCreateFlags2CreateInfo {
    VkPipelineCreateFlags2CreateInfo {
        flags: VK_PIPELINE_CREATE_2_DESCRIPTOR_HEAP_BIT_EXT,
        ..Default::default()
    }
}

/// A `VkPipeline`, destroyed on drop.
pub(crate) struct Pipeline {
    shared: Arc<Shared>,
    raw: VkPipeline,
}

impl Pipeline {
    pub(crate) fn new(shared: &Arc<Shared>, raw: VkPipeline) -> Self {
        Self {
            shared: shared.clone(),
            raw,
        }
    }

    pub(crate) fn raw(&self) -> VkPipeline {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // SAFETY: no pending submission uses the pipeline (the public API's contract).
        unsafe { (self.shared.fns.vkDestroyPipeline)(self.shared.raw, self.raw, ptr::null()) };
    }
}

pub(crate) type ComputePipeline = Pipeline;

pub(crate) fn create_compute_pipeline(
    device: &Device,
    desc: &ComputePipelineDesc<'_>,
) -> Result<ComputePipeline> {
    let s = &device.shared;
    s.lost.check()?;
    let mut stage = ShaderStage::new(s, &desc.shader, VK_SHADER_STAGE_COMPUTE_BIT)?;
    let flags = heap_flags();
    let info = VkComputePipelineCreateInfo {
        pNext: (&raw const flags).cast(),
        stage: stage.info(),
        layout: 0,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let cache = desc.cache.map_or(0, |c| c.raw.raw);
    let mut raw = 0;
    // SAFETY: a valid create info chain (pointing into `stage` and `flags`, alive here) and out
    // pointer.
    s.lost.result(unsafe {
        (s.fns.vkCreateComputePipelines)(s.raw, cache, 1, &info, ptr::null(), &mut raw)
    })?;
    Ok(Pipeline::new(s, raw))
}

// --- Graphics and mesh pipelines (draw area) ---

use super::resource::vk_format;
use crate::{
    BlendFactor, BlendOp, ColorTargetState, ColorWrites, CompareOp, CullMode, DepthState,
    FrontFace, MultisampleState, PolygonMode, PrimitiveTopology, RasterState, StencilFace,
    StencilOp, VariableRateShading,
};

pub(crate) type GraphicsPipeline = Pipeline;
pub(crate) type MeshPipeline = Pipeline;

pub(crate) fn create_graphics_pipeline(
    device: &Device,
    desc: &GraphicsPipelineDesc<'_>,
) -> Result<GraphicsPipeline> {
    let s = &device.shared;
    s.lost.check()?;
    let mut stages = Vec::with_capacity(2);
    stages.push(ShaderStage::new(
        s,
        &desc.vertex,
        VK_SHADER_STAGE_VERTEX_BIT,
    )?);
    if let Some(pixel) = &desc.pixel {
        stages.push(ShaderStage::new(s, pixel, VK_SHADER_STAGE_FRAGMENT_BIT)?);
    }
    // Vertex pulling: no bindings or attributes.
    let vertex_input = VkPipelineVertexInputStateCreateInfo::default();
    let input_assembly = VkPipelineInputAssemblyStateCreateInfo {
        topology: match desc.topology {
            PrimitiveTopology::PointList => VK_PRIMITIVE_TOPOLOGY_POINT_LIST,
            PrimitiveTopology::LineList => VK_PRIMITIVE_TOPOLOGY_LINE_LIST,
            PrimitiveTopology::LineStrip => VK_PRIMITIVE_TOPOLOGY_LINE_STRIP,
            PrimitiveTopology::TriangleList => VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
            PrimitiveTopology::TriangleStrip => VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,
        },
        // Vulkan restarts at the all-ones index of whichever index type is bound; the API's
        // restart format only says which one the draws will use.
        primitiveRestartEnable: bool32(desc.primitive_restart.is_some()),
        ..Default::default()
    };
    create(
        device,
        &mut stages,
        Some((&vertex_input, &input_assembly)),
        &Output {
            raster: &desc.raster,
            depth: &desc.depth,
            color_targets: desc.color_targets,
            multisample: &desc.multisample,
        },
        desc.cache,
    )
}

pub(crate) fn create_mesh_pipeline(
    device: &Device,
    desc: &MeshPipelineDesc<'_>,
) -> Result<MeshPipeline> {
    let s = &device.shared;
    s.lost.check()?;
    let mut stages = Vec::with_capacity(3);
    if let Some(amplification) = &desc.amplification {
        stages.push(ShaderStage::new(
            s,
            amplification,
            VK_SHADER_STAGE_TASK_BIT_EXT,
        )?);
    }
    stages.push(ShaderStage::new(
        s,
        &desc.mesh,
        VK_SHADER_STAGE_MESH_BIT_EXT,
    )?);
    if let Some(pixel) = &desc.pixel {
        stages.push(ShaderStage::new(s, pixel, VK_SHADER_STAGE_FRAGMENT_BIT)?);
    }
    create(
        device,
        &mut stages,
        None,
        &Output {
            raster: &desc.raster,
            depth: &desc.depth,
            color_targets: desc.color_targets,
            multisample: &desc.multisample,
        },
        desc.cache,
    )
}

/// The fixed-function state graphics and mesh pipelines share.
struct Output<'a> {
    raster: &'a RasterState,
    depth: &'a Option<DepthState>,
    color_targets: &'a [ColorTargetState],
    multisample: &'a MultisampleState,
}

fn bool32(b: bool) -> VkBool32 {
    if b { VK_TRUE } else { VK_FALSE }
}

/// Creates a graphics pipeline from `stages`, with vertex input and input assembly state for
/// vertex pipelines (`None` for mesh pipelines).
fn create(
    device: &Device,
    stages: &mut [ShaderStage],
    vertex: Option<(
        &VkPipelineVertexInputStateCreateInfo,
        &VkPipelineInputAssemblyStateCreateInfo,
    )>,
    out: &Output<'_>,
    cache: Option<&crate::PipelineCache>,
) -> Result<Pipeline> {
    let s = &device.shared;
    let features = &s.features;
    let stage_infos: Vec<VkPipelineShaderStageCreateInfo> =
        stages.iter_mut().map(ShaderStage::info).collect();

    let color_formats: Vec<VkFormat> = out
        .color_targets
        .iter()
        .map(|t| vk_format(t.format))
        .collect();
    let (depth_format, stencil_format) = match out.depth {
        Some(d) => (
            vk_format(d.format),
            match d.format.has_stencil() {
                true => vk_format(d.format),
                false => VK_FORMAT_UNDEFINED,
            },
        ),
        None => (VK_FORMAT_UNDEFINED, VK_FORMAT_UNDEFINED),
    };
    let rendering = VkPipelineRenderingCreateInfo {
        colorAttachmentCount: color_formats.len() as u32,
        pColorAttachmentFormats: color_formats.as_ptr(),
        depthAttachmentFormat: depth_format,
        stencilAttachmentFormat: stencil_format,
        ..Default::default()
    };
    let mut flags = heap_flags();
    flags.pNext = (&raw const rendering).cast();
    if features.variable_rate_shading == VariableRateShading::Image {
        flags.flags |= VK_PIPELINE_CREATE_2_RENDERING_FRAGMENT_SHADING_RATE_ATTACHMENT_BIT_KHR;
    }

    // Viewports and scissors are dynamic, counts included.
    let viewport = VkPipelineViewportStateCreateInfo::default();

    let r = out.raster;
    let conservative = VkPipelineRasterizationConservativeStateCreateInfoEXT {
        conservativeRasterizationMode: VK_CONSERVATIVE_RASTERIZATION_MODE_OVERESTIMATE_EXT,
        extraPrimitiveOverestimationSize: 0.0,
        ..Default::default()
    };
    let depth_bias = r.depth_bias != 0 || r.depth_bias_slope != 0.0;
    let raster = VkPipelineRasterizationStateCreateInfo {
        pNext: match r.conservative {
            true => (&raw const conservative).cast(),
            false => ptr::null(),
        },
        // Clamping depth turns near and far clipping off (D3D12's `DepthClipEnable = FALSE`).
        depthClampEnable: bool32(!r.depth_clip),
        polygonMode: match r.polygon {
            PolygonMode::Fill => VK_POLYGON_MODE_FILL,
            PolygonMode::Line => VK_POLYGON_MODE_LINE,
        },
        cullMode: match r.cull {
            CullMode::None => VK_CULL_MODE_NONE,
            CullMode::Front => VK_CULL_MODE_FRONT_BIT,
            CullMode::Back => VK_CULL_MODE_BACK_BIT,
        },
        // The negative-height viewport keeps on-screen winding as the API states it.
        frontFace: match r.front_face {
            FrontFace::CounterClockwise => VK_FRONT_FACE_COUNTER_CLOCKWISE,
            FrontFace::Clockwise => VK_FRONT_FACE_CLOCKWISE,
        },
        depthBiasEnable: bool32(depth_bias),
        depthBiasConstantFactor: r.depth_bias as f32,
        depthBiasClamp: r.depth_bias_clamp,
        depthBiasSlopeFactor: r.depth_bias_slope,
        lineWidth: 1.0,
        ..Default::default()
    };

    let m = out.multisample;
    let sample_mask = [m.mask];
    let multisample = VkPipelineMultisampleStateCreateInfo {
        rasterizationSamples: m.count as VkSampleCountFlagBits,
        pSampleMask: sample_mask.as_ptr(),
        alphaToCoverageEnable: bool32(m.alpha_to_coverage),
        ..Default::default()
    };

    let depth_stencil = match out.depth {
        Some(d) => {
            let (stencil, read, write) = match d.stencil {
                Some(st) => (true, st.read_mask, st.write_mask),
                None => (false, 0, 0),
            };
            let face = |f: StencilFace| VkStencilOpState {
                failOp: stencil_op(f.fail),
                passOp: stencil_op(f.pass),
                depthFailOp: stencil_op(f.depth_fail),
                compareOp: compare_op(f.compare),
                compareMask: read.into(),
                writeMask: write.into(),
                reference: 0,
            };
            let (front, back) = d
                .stencil
                .map_or((Default::default(), Default::default()), |st| {
                    (st.front, st.back)
                });
            VkPipelineDepthStencilStateCreateInfo {
                depthTestEnable: VK_TRUE,
                depthWriteEnable: bool32(d.write),
                depthCompareOp: compare_op(d.compare),
                depthBoundsTestEnable: bool32(d.bounds_test),
                stencilTestEnable: bool32(stencil),
                front: face(front),
                back: face(back),
                minDepthBounds: 0.0,
                maxDepthBounds: 1.0,
                ..Default::default()
            }
        }
        None => VkPipelineDepthStencilStateCreateInfo::default(),
    };

    let attachments: Vec<VkPipelineColorBlendAttachmentState> = out
        .color_targets
        .iter()
        .map(|t| {
            let mut a = VkPipelineColorBlendAttachmentState {
                colorWriteMask: color_writes(t.write_mask),
                ..Default::default()
            };
            if let Some(b) = t.blend {
                a.blendEnable = VK_TRUE;
                a.srcColorBlendFactor = blend_factor(b.color.src_factor, false);
                a.dstColorBlendFactor = blend_factor(b.color.dst_factor, false);
                a.colorBlendOp = blend_op(b.color.op);
                a.srcAlphaBlendFactor = blend_factor(b.alpha.src_factor, true);
                a.dstAlphaBlendFactor = blend_factor(b.alpha.dst_factor, true);
                a.alphaBlendOp = blend_op(b.alpha.op);
            }
            a
        })
        .collect();
    let blend = VkPipelineColorBlendStateCreateInfo {
        attachmentCount: attachments.len() as u32,
        pAttachments: attachments.as_ptr(),
        ..Default::default()
    };

    let mut dynamic_states = alloc::vec![
        VK_DYNAMIC_STATE_VIEWPORT_WITH_COUNT,
        VK_DYNAMIC_STATE_SCISSOR_WITH_COUNT,
        VK_DYNAMIC_STATE_STENCIL_REFERENCE,
        VK_DYNAMIC_STATE_BLEND_CONSTANTS,
    ];
    if features.depth_bounds {
        dynamic_states.push(VK_DYNAMIC_STATE_DEPTH_BOUNDS);
    }
    if features.variable_rate_shading != VariableRateShading::None {
        dynamic_states.push(VK_DYNAMIC_STATE_FRAGMENT_SHADING_RATE_KHR);
    }
    let dynamic = VkPipelineDynamicStateCreateInfo {
        dynamicStateCount: dynamic_states.len() as u32,
        pDynamicStates: dynamic_states.as_ptr(),
        ..Default::default()
    };

    let (vertex_input, input_assembly) = match vertex {
        Some((v, i)) => (ptr::from_ref(v), ptr::from_ref(i)),
        None => (ptr::null(), ptr::null()),
    };
    let info = VkGraphicsPipelineCreateInfo {
        pNext: (&raw const flags).cast(),
        stageCount: stage_infos.len() as u32,
        pStages: stage_infos.as_ptr(),
        pVertexInputState: vertex_input,
        pInputAssemblyState: input_assembly,
        pViewportState: &viewport,
        pRasterizationState: &raster,
        pMultisampleState: &multisample,
        pDepthStencilState: &depth_stencil,
        pColorBlendState: &blend,
        pDynamicState: &dynamic,
        layout: 0,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let cache = cache.map_or(0, |c| c.raw.raw);
    let mut raw = 0;
    // SAFETY: a valid create info chain; everything it points to (stages, states, formats) is
    // alive here.
    s.lost.result(unsafe {
        (s.fns.vkCreateGraphicsPipelines)(s.raw, cache, 1, &info, ptr::null(), &mut raw)
    })?;
    Ok(Pipeline::new(s, raw))
}

pub(crate) fn compare_op(op: CompareOp) -> VkCompareOp {
    match op {
        CompareOp::Never => VK_COMPARE_OP_NEVER,
        CompareOp::Less => VK_COMPARE_OP_LESS,
        CompareOp::Equal => VK_COMPARE_OP_EQUAL,
        CompareOp::LessEqual => VK_COMPARE_OP_LESS_OR_EQUAL,
        CompareOp::Greater => VK_COMPARE_OP_GREATER,
        CompareOp::NotEqual => VK_COMPARE_OP_NOT_EQUAL,
        CompareOp::GreaterEqual => VK_COMPARE_OP_GREATER_OR_EQUAL,
        CompareOp::Always => VK_COMPARE_OP_ALWAYS,
    }
}

fn stencil_op(op: StencilOp) -> VkStencilOp {
    match op {
        StencilOp::Keep => VK_STENCIL_OP_KEEP,
        StencilOp::Zero => VK_STENCIL_OP_ZERO,
        StencilOp::Replace => VK_STENCIL_OP_REPLACE,
        StencilOp::IncrementClamp => VK_STENCIL_OP_INCREMENT_AND_CLAMP,
        StencilOp::DecrementClamp => VK_STENCIL_OP_DECREMENT_AND_CLAMP,
        StencilOp::Invert => VK_STENCIL_OP_INVERT,
        StencilOp::IncrementWrap => VK_STENCIL_OP_INCREMENT_AND_WRAP,
        StencilOp::DecrementWrap => VK_STENCIL_OP_DECREMENT_AND_WRAP,
    }
}

/// `alpha`: the factor of the alpha channel, where the blend constant is its alpha (D3D12 reads
/// the constant's alpha there too).
fn blend_factor(factor: BlendFactor, alpha: bool) -> VkBlendFactor {
    match factor {
        BlendFactor::Zero => VK_BLEND_FACTOR_ZERO,
        BlendFactor::One => VK_BLEND_FACTOR_ONE,
        BlendFactor::SrcColor => VK_BLEND_FACTOR_SRC_COLOR,
        BlendFactor::OneMinusSrcColor => VK_BLEND_FACTOR_ONE_MINUS_SRC_COLOR,
        BlendFactor::SrcAlpha => VK_BLEND_FACTOR_SRC_ALPHA,
        BlendFactor::OneMinusSrcAlpha => VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
        BlendFactor::DstColor => VK_BLEND_FACTOR_DST_COLOR,
        BlendFactor::OneMinusDstColor => VK_BLEND_FACTOR_ONE_MINUS_DST_COLOR,
        BlendFactor::DstAlpha => VK_BLEND_FACTOR_DST_ALPHA,
        BlendFactor::OneMinusDstAlpha => VK_BLEND_FACTOR_ONE_MINUS_DST_ALPHA,
        BlendFactor::SrcAlphaSaturated => VK_BLEND_FACTOR_SRC_ALPHA_SATURATE,
        BlendFactor::Constant if alpha => VK_BLEND_FACTOR_CONSTANT_ALPHA,
        BlendFactor::Constant => VK_BLEND_FACTOR_CONSTANT_COLOR,
        BlendFactor::OneMinusConstant if alpha => VK_BLEND_FACTOR_ONE_MINUS_CONSTANT_ALPHA,
        BlendFactor::OneMinusConstant => VK_BLEND_FACTOR_ONE_MINUS_CONSTANT_COLOR,
        BlendFactor::Src1Color => VK_BLEND_FACTOR_SRC1_COLOR,
        BlendFactor::OneMinusSrc1Color => VK_BLEND_FACTOR_ONE_MINUS_SRC1_COLOR,
        BlendFactor::Src1Alpha => VK_BLEND_FACTOR_SRC1_ALPHA,
        BlendFactor::OneMinusSrc1Alpha => VK_BLEND_FACTOR_ONE_MINUS_SRC1_ALPHA,
    }
}

fn blend_op(op: BlendOp) -> VkBlendOp {
    match op {
        BlendOp::Add => VK_BLEND_OP_ADD,
        BlendOp::Subtract => VK_BLEND_OP_SUBTRACT,
        BlendOp::ReverseSubtract => VK_BLEND_OP_REVERSE_SUBTRACT,
        BlendOp::Min => VK_BLEND_OP_MIN,
        BlendOp::Max => VK_BLEND_OP_MAX,
    }
}

fn color_writes(mask: ColorWrites) -> VkColorComponentFlags {
    [
        (ColorWrites::R, VK_COLOR_COMPONENT_R_BIT),
        (ColorWrites::G, VK_COLOR_COMPONENT_G_BIT),
        (ColorWrites::B, VK_COLOR_COMPONENT_B_BIT),
        (ColorWrites::A, VK_COLOR_COMPONENT_A_BIT),
    ]
    .into_iter()
    .filter(|(w, _)| mask.contains(*w))
    .fold(0, |bits, (_, b)| bits | b)
}

// --- Pipeline cache ---

pub(crate) struct PipelineCache {
    shared: Arc<Shared>,
    pub(crate) raw: VkPipelineCache,
}

impl PipelineCache {
    /// Seeds the cache with `data` only if its header names this device and driver; anything
    /// else starts empty.
    pub(crate) fn new(device: &Device, data: &[u8]) -> Result<Self> {
        let s = &device.shared;
        let p = &s.properties;
        let header_len = size_of::<VkPipelineCacheHeaderVersionOne>();
        let matches = data.len() >= header_len && {
            // SAFETY: at least a header's worth of bytes; read unaligned.
            let h = unsafe {
                ptr::read_unaligned(data.as_ptr().cast::<VkPipelineCacheHeaderVersionOne>())
            };
            h.headerVersion == VK_PIPELINE_CACHE_HEADER_VERSION_ONE
                && h.vendorID == p.vendor_id
                && h.deviceID == p.device_id
                && h.pipelineCacheUUID == p.pipeline_cache_uuid
        };
        let initial: &[u8] = if matches { data } else { &[] };
        let info = VkPipelineCacheCreateInfo {
            initialDataSize: initial.len(),
            pInitialData: initial.as_ptr().cast(),
            ..Default::default()
        };
        let mut raw = 0;
        // SAFETY: a valid create info and out pointer.
        s.lost.result(unsafe {
            (s.fns.vkCreatePipelineCache)(s.raw, &info, ptr::null(), &mut raw)
        })?;
        Ok(Self {
            shared: s.clone(),
            raw,
        })
    }

    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        let s = &self.shared;
        let mut len = 0;
        // SAFETY: a live cache; size query.
        check(unsafe {
            (s.fns.vkGetPipelineCacheData)(s.raw, self.raw, &mut len, ptr::null_mut())
        })?;
        let mut data = alloc::vec![0u8; len];
        // SAFETY: `data` holds `len` bytes.
        check(unsafe {
            (s.fns.vkGetPipelineCacheData)(s.raw, self.raw, &mut len, data.as_mut_ptr().cast())
        })?;
        data.truncate(len);
        Ok(data)
    }
}

impl Drop for PipelineCache {
    fn drop(&mut self) {
        // SAFETY: no pipeline creation uses the cache any more (`&mut self`).
        unsafe { (self.shared.fns.vkDestroyPipelineCache)(self.shared.raw, self.raw, ptr::null()) };
    }
}
