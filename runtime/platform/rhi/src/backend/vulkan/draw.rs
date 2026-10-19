//! Render passes (dynamic rendering), graphics state and draws, mesh dispatches, and indirect
//! commands through device-generated commands (0016 §6).
//!
//! Indirect commands: every argument record starts with a `constant` word that must reach
//! push-constant word [`INDIRECT_CONSTANT_WORD`] for that record's draw or dispatch. Each kind of
//! indirect command has one indirect commands layout: a push data token copying the record's
//! first word to `INDIRECT_CONSTANT_WORD * 4`, then the action token reading the rest. Count
//! buffers become the sequence count. There is no execution set: the bound pipeline is passed
//! along (`VkGeneratedCommandsPipelineInfoEXT`), and the commands preprocess implicitly into
//! preprocess memory the list sub-allocates. Commands without the constant
//! (`IndirectArgs::constant`) use the native indirect commands instead, reading each record past
//! its constant word; a counted dispatch, which has no native form, still goes through DGC.

use core::ptr;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::command::CommandList;
use super::device::Shared;
use super::ffi::*;
use super::memory::{Dedicated, Memory};
use super::pipeline::{GraphicsPipeline, MeshPipeline};
use super::resource::Buffer;
use crate::{
    DepthAttachment, INDIRECT_CONSTANT_WORD, IndexFormat, IndirectArgs, LoadOp, MemoryLocation,
    Rect, RenderPassDesc, ShadingRate, ShadingRateCombiner, StoreOp, VariableRateShading, Viewport,
};

/// Recording state of the draw area. `CommandList` resets it at every `begin`; the indirect
/// commands layouts and preprocess memory survive that (the previous recording is no longer
/// pending by then), so lists reuse them.
#[derive(Default)]
pub(crate) struct DrawState {
    /// Dynamic state defaults (stencil reference, blend constants, depth bounds, shading rate)
    /// are recorded.
    defaults: bool,
    /// The render pass being recorded.
    pass: Option<Pass>,
    /// The shading-rate image's view and texel size, attached to every render pass begun while
    /// it is set.
    shading_rate_image: Option<(VkImageView, u32)>,
    /// The last graphics or mesh pipeline bound, and the last compute pipeline, for indirect
    /// commands.
    graphics_pipeline: VkPipeline,
    pub(crate) compute_pipeline: VkPipeline,
    indirect: Option<Indirect>,
}

impl DrawState {
    /// Starts a new recording.
    pub(crate) fn reset(&mut self) {
        let indirect = self.indirect.take().map(|mut i| {
            i.cursor = (0, 0);
            i
        });
        *self = Self {
            indirect,
            ..Self::default()
        };
    }
}

/// An open render pass: its attachments, kept to begin it again when the shading-rate image
/// changes inside it.
struct Pass {
    colors: Vec<VkRenderingAttachmentInfo>,
    depth: Option<VkRenderingAttachmentInfo>,
    stencil: Option<VkRenderingAttachmentInfo>,
    extent: VkExtent2D,
}

// --- Render passes ---

/// Records the API's dynamic state defaults once per recording (0 stencil reference and blend
/// constants, depth bounds 0..1, a 1×1 shading rate that keeps the draw's rate), before the
/// first graphics command that could need them.
fn defaults(list: &mut CommandList) {
    if list.draw.defaults {
        return;
    }
    list.draw.defaults = true;
    let s = list.shared();
    let f = &s.fns;
    let raw = list.raw();
    // SAFETY: a recording graphics buffer; dynamic state may be set inside and outside render
    // passes.
    unsafe {
        (f.vkCmdSetStencilReference)(raw, VK_STENCIL_FACE_FRONT_AND_BACK, 0);
        (f.vkCmdSetBlendConstants)(raw, [0.0f32; 4].as_ptr());
        if s.features.depth_bounds {
            (f.vkCmdSetDepthBounds)(raw, 0.0, 1.0);
        }
    }
    if s.features.variable_rate_shading != VariableRateShading::None {
        shading_rate(list, ShadingRate::R1x1, [ShadingRateCombiner::Keep; 2]);
    }
}

/// `LoadOp` as a Vulkan load op and clear value.
fn load<T>(
    op: LoadOp<T>,
    clear: impl FnOnce(T) -> VkClearValue,
) -> (VkAttachmentLoadOp, VkClearValue) {
    match op {
        LoadOp::Load => (VK_ATTACHMENT_LOAD_OP_LOAD, zero_clear()),
        LoadOp::Clear(v) => (VK_ATTACHMENT_LOAD_OP_CLEAR, clear(v)),
        LoadOp::DontCare => (VK_ATTACHMENT_LOAD_OP_DONT_CARE, zero_clear()),
    }
}

fn zero_clear() -> VkClearValue {
    VkClearValue {
        color: VkClearColorValue { uint32: [0; 4] },
    }
}

/// `StoreOp` as a Vulkan store op. Where a shading-rate image can make a render pass begin
/// again midway, every attachment stores, so the second part loads what the first drew.
fn store(list: &CommandList, op: StoreOp) -> VkAttachmentStoreOp {
    let restartable = list.shared().features.variable_rate_shading == VariableRateShading::Image;
    match op {
        StoreOp::Store => VK_ATTACHMENT_STORE_OP_STORE,
        StoreOp::DontCare if restartable => VK_ATTACHMENT_STORE_OP_STORE,
        StoreOp::DontCare => VK_ATTACHMENT_STORE_OP_DONT_CARE,
    }
}

/// Whether a color format holds unsigned (`Some(false)`) or signed (`Some(true)`) integers, which
/// clear to integer values (the clear color converted, as D3D12 does).
fn integer_format(format: VkFormat) -> Option<bool> {
    match format {
        VK_FORMAT_R8_UINT
        | VK_FORMAT_R8G8_UINT
        | VK_FORMAT_R8G8B8A8_UINT
        | VK_FORMAT_B8G8R8A8_UINT
        | VK_FORMAT_A2B10G10R10_UINT_PACK32
        | VK_FORMAT_A2R10G10B10_UINT_PACK32
        | VK_FORMAT_R16_UINT
        | VK_FORMAT_R16G16_UINT
        | VK_FORMAT_R16G16B16A16_UINT
        | VK_FORMAT_R32_UINT
        | VK_FORMAT_R32G32_UINT
        | VK_FORMAT_R32G32B32A32_UINT => Some(false),
        VK_FORMAT_R8_SINT
        | VK_FORMAT_R8G8_SINT
        | VK_FORMAT_R8G8B8A8_SINT
        | VK_FORMAT_B8G8R8A8_SINT
        | VK_FORMAT_A2B10G10R10_SINT_PACK32
        | VK_FORMAT_A2R10G10B10_SINT_PACK32
        | VK_FORMAT_R16_SINT
        | VK_FORMAT_R16G16_SINT
        | VK_FORMAT_R16G16B16A16_SINT
        | VK_FORMAT_R32_SINT
        | VK_FORMAT_R32G32_SINT
        | VK_FORMAT_R32G32B32A32_SINT => Some(true),
        _ => None,
    }
}

fn depth_attachments(
    list: &CommandList,
    d: &DepthAttachment<'_>,
) -> (VkRenderingAttachmentInfo, Option<VkRenderingAttachmentInfo>) {
    let view = &d.view.raw;
    let read_only = view.read_only;
    let layout = match read_only {
        true => VK_IMAGE_LAYOUT_DEPTH_STENCIL_READ_ONLY_OPTIMAL,
        false => VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    };
    // Nothing writes a read-only attachment, so nothing needs storing.
    let store_op = |op| match read_only {
        true => VK_ATTACHMENT_STORE_OP_NONE,
        false => store(list, op),
    };
    let (load_op, clear) = load(d.load, |depth| VkClearValue {
        depthStencil: VkClearDepthStencilValue { depth, stencil: 0 },
    });
    let depth = VkRenderingAttachmentInfo {
        imageView: view.view,
        imageLayout: layout,
        loadOp: load_op,
        storeOp: store_op(d.store),
        clearValue: clear,
        ..Default::default()
    };
    let stencil = view.stencil.then(|| {
        let (load_op, clear) = load(d.stencil_load, |stencil| VkClearValue {
            depthStencil: VkClearDepthStencilValue {
                depth: 0.0,
                stencil: stencil.into(),
            },
        });
        VkRenderingAttachmentInfo {
            loadOp: load_op,
            storeOp: store_op(d.stencil_store),
            clearValue: clear,
            ..depth
        }
    });
    (depth, stencil)
}

pub(crate) fn begin_render_pass(list: &mut CommandList, desc: &RenderPassDesc<'_>) {
    defaults(list);
    let colors: Vec<VkRenderingAttachmentInfo> = desc
        .color
        .iter()
        .map(|c| {
            let view = &c.view.raw;
            let (load_op, clear) = load(c.load, |rgba| VkClearValue {
                color: match integer_format(view.format) {
                    None => VkClearColorValue { float32: rgba },
                    Some(false) => VkClearColorValue {
                        uint32: rgba.map(|v| v as u32),
                    },
                    Some(true) => VkClearColorValue {
                        int32: rgba.map(|v| v as i32),
                    },
                },
            });
            VkRenderingAttachmentInfo {
                imageView: view.view,
                imageLayout: VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                loadOp: load_op,
                storeOp: store(list, c.store),
                clearValue: clear,
                ..Default::default()
            }
        })
        .collect();
    let (depth, stencil) = match &desc.depth {
        Some(d) => {
            let (depth, stencil) = depth_attachments(list, d);
            (Some(depth), stencil)
        }
        None => (None, None),
    };
    let extent = match (desc.color.first(), &desc.depth) {
        _ if desc.area.is_some() => {
            let (width, height) = desc.area.unwrap_or_default();
            VkExtent2D { width, height }
        }
        (Some(c), _) => VkExtent2D {
            width: c.view.raw.width,
            height: c.view.raw.height,
        },
        (None, Some(d)) => VkExtent2D {
            width: d.view.raw.width,
            height: d.view.raw.height,
        },
        (None, None) => VkExtent2D::default(),
    };
    let pass = Pass {
        colors,
        depth,
        stencil,
        extent,
    };
    begin_rendering(list, &pass);
    list.draw.pass = Some(pass);
}

fn begin_rendering(list: &CommandList, pass: &Pass) {
    let shading_rate = list.draw.shading_rate_image.map(|(view, texel)| {
        VkRenderingFragmentShadingRateAttachmentInfoKHR {
            imageView: view,
            imageLayout: VK_IMAGE_LAYOUT_FRAGMENT_SHADING_RATE_ATTACHMENT_OPTIMAL_KHR,
            shadingRateAttachmentTexelSize: VkExtent2D {
                width: texel,
                height: texel,
            },
            ..Default::default()
        }
    });
    let info = VkRenderingInfo {
        pNext: shading_rate
            .as_ref()
            .map_or(ptr::null(), |s| ptr::from_ref(s).cast()),
        renderArea: VkRect2D {
            offset: VkOffset2D::default(),
            extent: pass.extent,
        },
        layerCount: 1,
        colorAttachmentCount: pass.colors.len() as u32,
        pColorAttachments: pass.colors.as_ptr(),
        pDepthAttachment: pass.depth.as_ref().map_or(ptr::null(), ptr::from_ref),
        pStencilAttachment: pass.stencil.as_ref().map_or(ptr::null(), ptr::from_ref),
        ..Default::default()
    };
    // SAFETY: a recording graphics buffer outside a render pass; the attachments are in the
    // layouts named (the API's contract) and everything `info` points to is alive here.
    unsafe { (list.fns().vkCmdBeginRendering)(list.raw(), &info) };
}

pub(crate) fn end_render_pass(list: &mut CommandList) {
    list.draw.pass = None;
    // SAFETY: a recording buffer inside a render pass.
    unsafe { (list.fns().vkCmdEndRendering)(list.raw()) };
}

// --- Pipelines and dynamic state ---

fn bind_graphics(list: &mut CommandList, pipeline: VkPipeline) {
    list.draw.graphics_pipeline = pipeline;
    // SAFETY: a recording graphics buffer and a live pipeline of this device.
    unsafe {
        (list.fns().vkCmdBindPipeline)(list.raw(), VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline)
    };
}

pub(crate) fn set_graphics_pipeline(list: &mut CommandList, pipeline: &GraphicsPipeline) {
    bind_graphics(list, pipeline.raw());
}

pub(crate) fn set_mesh_pipeline(list: &mut CommandList, pipeline: &MeshPipeline) {
    bind_graphics(list, pipeline.raw());
}

/// NDC y points up (0002 §4): each viewport is flipped with a negative height.
pub(crate) fn set_viewports(list: &mut CommandList, viewports: &[Viewport]) {
    let raw: Vec<VkViewport> = viewports
        .iter()
        .map(|v| VkViewport {
            x: v.x,
            y: v.y + v.height,
            width: v.width,
            height: -v.height,
            minDepth: v.min_depth,
            maxDepth: v.max_depth,
        })
        .collect();
    // SAFETY: a recording graphics buffer; at most `MAX_VIEWPORTS` viewports (checked by
    // `commands.rs`).
    unsafe { (list.fns().vkCmdSetViewportWithCount)(list.raw(), raw.len() as u32, raw.as_ptr()) };
}

/// Vulkan scissors cannot start left of or above the target: the part outside is cut off.
pub(crate) fn set_scissors(list: &mut CommandList, rects: &[Rect]) {
    let raw: Vec<VkRect2D> = rects
        .iter()
        .map(|r| {
            let (x, width) = clip(r.x, r.width);
            let (y, height) = clip(r.y, r.height);
            VkRect2D {
                offset: VkOffset2D { x, y },
                extent: VkExtent2D { width, height },
            }
        })
        .collect();
    // SAFETY: a recording graphics buffer; non-negative offsets.
    unsafe { (list.fns().vkCmdSetScissorWithCount)(list.raw(), raw.len() as u32, raw.as_ptr()) };
}

fn clip(start: i32, size: u32) -> (i32, u32) {
    match start {
        0.. => (start, size),
        _ => (0, size.saturating_sub(start.unsigned_abs())),
    }
}

pub(crate) fn set_stencil_reference(list: &mut CommandList, reference: u8) {
    defaults(list);
    // SAFETY: a recording graphics buffer.
    unsafe {
        (list.fns().vkCmdSetStencilReference)(
            list.raw(),
            VK_STENCIL_FACE_FRONT_AND_BACK,
            reference.into(),
        )
    };
}

pub(crate) fn set_blend_constants(list: &mut CommandList, constants: [f32; 4]) {
    defaults(list);
    // SAFETY: a recording graphics buffer.
    unsafe { (list.fns().vkCmdSetBlendConstants)(list.raw(), constants.as_ptr()) };
}

pub(crate) fn set_depth_bounds(list: &mut CommandList, min: f32, max: f32) {
    defaults(list);
    // SAFETY: a recording graphics buffer of a device with depth bounds (checked by
    // `commands.rs`).
    unsafe { (list.fns().vkCmdSetDepthBounds)(list.raw(), min, max) };
}

fn shading_rate(list: &CommandList, rate: ShadingRate, combiners: [ShadingRateCombiner; 2]) {
    let (width, height) = match rate {
        ShadingRate::R1x1 => (1, 1),
        ShadingRate::R1x2 => (1, 2),
        ShadingRate::R2x1 => (2, 1),
        ShadingRate::R2x2 => (2, 2),
        ShadingRate::R2x4 => (2, 4),
        ShadingRate::R4x2 => (4, 2),
        ShadingRate::R4x4 => (4, 4),
    };
    // D3D12's SUM adds log2 rates: the sizes multiply.
    let ops = combiners.map(|c| match c {
        ShadingRateCombiner::Keep => VK_FRAGMENT_SHADING_RATE_COMBINER_OP_KEEP_KHR,
        ShadingRateCombiner::Replace => VK_FRAGMENT_SHADING_RATE_COMBINER_OP_REPLACE_KHR,
        ShadingRateCombiner::Min => VK_FRAGMENT_SHADING_RATE_COMBINER_OP_MIN_KHR,
        ShadingRateCombiner::Max => VK_FRAGMENT_SHADING_RATE_COMBINER_OP_MAX_KHR,
        ShadingRateCombiner::Sum => VK_FRAGMENT_SHADING_RATE_COMBINER_OP_MUL_KHR,
    });
    let set = list
        .fns()
        .vkCmdSetFragmentShadingRateKHR
        .expect("variable-rate shading without VK_KHR_fragment_shading_rate");
    // SAFETY: a recording graphics buffer of a device with pipeline shading rates (checked by
    // `commands.rs`).
    unsafe { set(list.raw(), &VkExtent2D { width, height }, ops.as_ptr()) };
}

pub(crate) fn set_shading_rate(
    list: &mut CommandList,
    rate: ShadingRate,
    combiners: [ShadingRateCombiner; 2],
) {
    defaults(list);
    shading_rate(list, rate, combiners);
}

/// The image is a render pass attachment in Vulkan: set inside a render pass, it ends the pass
/// and begins it again (loading what was drawn) with the new image.
pub(crate) fn set_shading_rate_image(list: &mut CommandList, image: Option<&crate::Texture>) {
    let texel = list.shared().features.shading_rate_tile_size;
    let new = image.map(|t| (t.raw.shading_rate_view(), texel));
    if new == list.draw.shading_rate_image {
        return;
    }
    list.draw.shading_rate_image = new;
    if let Some(mut pass) = list.draw.pass.take() {
        // SAFETY: a recording buffer inside a render pass.
        unsafe { (list.fns().vkCmdEndRendering)(list.raw()) };
        let attachments = pass
            .colors
            .iter_mut()
            .chain(pass.depth.as_mut())
            .chain(pass.stencil.as_mut());
        for a in attachments {
            a.loadOp = VK_ATTACHMENT_LOAD_OP_LOAD;
        }
        begin_rendering(list, &pass);
        list.draw.pass = Some(pass);
    }
}

pub(crate) fn set_index_buffer(
    list: &mut CommandList,
    buffer: &crate::Buffer,
    offset: u64,
    format: IndexFormat,
) {
    let ty = match format {
        IndexFormat::U16 => VK_INDEX_TYPE_UINT16,
        IndexFormat::U32 => VK_INDEX_TYPE_UINT32,
    };
    // SAFETY: a recording graphics buffer and an index buffer of this device; the offset is
    // within it and aligned (checked by `commands.rs`).
    unsafe {
        (list.fns().vkCmdBindIndexBuffer2)(list.raw(), buffer.raw.raw(), offset, VK_WHOLE_SIZE, ty)
    };
}

// --- Draws ---

/// Instance indices start at 0 in shaders whatever `first_instance` is (0002 §3): the
/// placeholder SPIR-V subtracts `BaseInstance`, so Vulkan's own semantics apply unchanged.
pub(crate) fn draw(
    list: &mut CommandList,
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
) {
    // SAFETY: a recording buffer inside a render pass with a graphics pipeline bound.
    unsafe {
        (list.fns().vkCmdDraw)(
            list.raw(),
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        )
    };
}

pub(crate) fn draw_indexed(
    list: &mut CommandList,
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) {
    // SAFETY: as `draw`, with an index buffer bound.
    unsafe {
        (list.fns().vkCmdDrawIndexed)(
            list.raw(),
            index_count,
            instance_count,
            first_index,
            vertex_offset,
            first_instance,
        )
    };
}

pub(crate) fn dispatch_mesh(list: &mut CommandList, x: u32, y: u32, z: u32) {
    // SAFETY: a recording buffer inside a render pass with a mesh pipeline bound.
    unsafe { (list.fns().vkCmdDrawMeshTasksEXT)(list.raw(), x, y, z) };
}

// --- Indirect commands (device-generated commands) ---

#[derive(Clone, Copy)]
enum Kind {
    Draw,
    DrawIndexed,
    Dispatch,
    Mesh,
}

impl Kind {
    /// Bytes per argument record: the constant word, then the Vulkan indirect command.
    fn stride(self) -> u32 {
        match self {
            Self::Draw => 20,
            Self::DrawIndexed => 24,
            Self::Dispatch | Self::Mesh => 16,
        }
    }

    fn token(self) -> VkIndirectCommandsTokenTypeEXT {
        match self {
            Self::Draw => VK_INDIRECT_COMMANDS_TOKEN_TYPE_DRAW_EXT,
            Self::DrawIndexed => VK_INDIRECT_COMMANDS_TOKEN_TYPE_DRAW_INDEXED_EXT,
            Self::Dispatch => VK_INDIRECT_COMMANDS_TOKEN_TYPE_DISPATCH_EXT,
            Self::Mesh => VK_INDIRECT_COMMANDS_TOKEN_TYPE_DRAW_MESH_TASKS_EXT,
        }
    }

    fn stages(self) -> VkShaderStageFlags {
        match self {
            Self::Draw | Self::DrawIndexed => {
                VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT
            }
            Self::Dispatch => VK_SHADER_STAGE_COMPUTE_BIT,
            Self::Mesh => {
                VK_SHADER_STAGE_TASK_BIT_EXT
                    | VK_SHADER_STAGE_MESH_BIT_EXT
                    | VK_SHADER_STAGE_FRAGMENT_BIT
            }
        }
    }
}

/// An indirect commands layout, destroyed on drop.
struct Layout {
    shared: Arc<Shared>,
    raw: VkIndirectCommandsLayoutEXT,
}

impl Drop for Layout {
    fn drop(&mut self) {
        // SAFETY: no pending recording uses the layout (the list is not pending when it is
        // dropped).
        unsafe {
            (self.shared.fns.vkDestroyIndirectCommandsLayoutEXT)(
                self.shared.raw,
                self.raw,
                ptr::null(),
            )
        };
    }
}

/// Smallest preprocess buffer a list allocates.
const PREPROCESS_CHUNK: u64 = 1 << 20;

/// What indirect commands keep across recordings: a layout per kind, and preprocess memory
/// handed out from `cursor` (chunk, offset) on, a fresh region per command.
#[derive(Default)]
struct Indirect {
    layouts: [Option<Layout>; 4],
    chunks: Vec<(Buffer, u64)>,
    cursor: (usize, u64),
}

impl Indirect {
    fn layout(&mut self, shared: &Arc<Shared>, kind: Kind) -> VkIndirectCommandsLayoutEXT {
        if let Some(layout) = &self.layouts[kind as usize] {
            return layout.raw;
        }
        let push = VkIndirectCommandsPushConstantTokenEXT {
            updateRange: VkPushConstantRange {
                stageFlags: VK_SHADER_STAGE_ALL,
                offset: INDIRECT_CONSTANT_WORD * 4,
                size: 4,
            },
        };
        let tokens = [
            VkIndirectCommandsLayoutTokenEXT {
                r#type: VK_INDIRECT_COMMANDS_TOKEN_TYPE_PUSH_DATA_EXT,
                data: VkIndirectCommandsTokenDataEXT {
                    pPushConstant: &push,
                },
                offset: 0,
                ..Default::default()
            },
            VkIndirectCommandsLayoutTokenEXT {
                r#type: kind.token(),
                offset: 4,
                ..Default::default()
            },
        ];
        let info = VkIndirectCommandsLayoutCreateInfoEXT {
            shaderStages: kind.stages(),
            indirectStride: kind.stride(),
            pipelineLayout: 0,
            tokenCount: tokens.len() as u32,
            pTokens: tokens.as_ptr(),
            ..Default::default()
        };
        let mut raw = 0;
        // SAFETY: a valid create info (pointing at `tokens` and `push`, alive here) and out
        // pointer.
        let r = unsafe {
            (shared.fns.vkCreateIndirectCommandsLayoutEXT)(shared.raw, &info, ptr::null(), &mut raw)
        };
        // Creation only fails when out of memory; the command is then skipped.
        if r < 0 {
            marlin_diag::error!("vkCreateIndirectCommandsLayoutEXT failed: {r}");
            return 0;
        }
        self.layouts[kind as usize] = Some(Layout {
            shared: shared.clone(),
            raw,
        });
        raw
    }

    /// The device address of `size` bytes of preprocess memory no earlier command of this
    /// recording uses, from memory of a type in `types`.
    fn preprocess(
        &mut self,
        shared: &Arc<Shared>,
        size: u64,
        alignment: u64,
        types: u32,
    ) -> Option<u64> {
        let fits = |len: u64, offset: u64| {
            let start = offset.next_multiple_of(alignment);
            (start + size <= len).then_some(start)
        };
        loop {
            let (chunk, offset) = self.cursor;
            let Some(entry) = self.chunks.get(chunk) else {
                break;
            };
            match fits(entry.1, offset) {
                Some(start) => {
                    self.cursor = (chunk, start + size);
                    return Some(entry.0.address() + start);
                }
                None => self.cursor = (chunk + 1, 0),
            }
        }
        let len = size.max(PREPROCESS_CHUNK);
        let buffer = preprocess_buffer(shared, len, types)?;
        let address = buffer.address();
        self.chunks.push((buffer, len));
        self.cursor = (self.chunks.len() - 1, size);
        Some(address)
    }
}

/// A device-local buffer for preprocessing generated commands.
fn preprocess_buffer(shared: &Arc<Shared>, size: u64, types: u32) -> Option<Buffer> {
    let s = shared;
    let usage = VkBufferUsageFlags2CreateInfo {
        usage: VK_BUFFER_USAGE_2_PREPROCESS_BUFFER_BIT_EXT
            | VK_BUFFER_USAGE_2_SHADER_DEVICE_ADDRESS_BIT,
        ..Default::default()
    };
    let (sharing, family_count, families) = s.families.sharing();
    let info = VkBufferCreateInfo {
        pNext: (&raw const usage).cast(),
        size,
        sharingMode: sharing,
        queueFamilyIndexCount: family_count,
        pQueueFamilyIndices: families,
        ..Default::default()
    };
    let mut raw = 0;
    // SAFETY: a valid create info chain and out pointer.
    if unsafe { (s.fns.vkCreateBuffer)(s.raw, &info, ptr::null(), &mut raw) } < 0 {
        return None;
    }
    let destroy = || {
        // SAFETY: a buffer nothing uses.
        unsafe { (s.fns.vkDestroyBuffer)(s.raw, raw, ptr::null()) };
    };
    let req_info = VkBufferMemoryRequirementsInfo2 {
        buffer: raw,
        ..Default::default()
    };
    let mut req = VkMemoryRequirements2::default();
    // SAFETY: a live buffer and valid structures.
    unsafe { (s.fns.vkGetBufferMemoryRequirements2)(s.raw, &req_info, &mut req) };
    let r = req.memoryRequirements;
    let Some(memory_type) = s
        .memory
        .find_type(r.memoryTypeBits & types, MemoryLocation::GpuOnly)
    else {
        destroy();
        return None;
    };
    let Ok(memory) = Memory::allocate(s, r.size, memory_type, Dedicated::Buffer(raw), ptr::null())
    else {
        destroy();
        return None;
    };
    let bind = VkBindBufferMemoryInfo {
        buffer: raw,
        memory: memory.raw,
        memoryOffset: 0,
        ..Default::default()
    };
    // SAFETY: an unbound buffer and memory of a type it accepts.
    if unsafe { (s.fns.vkBindBufferMemory2)(s.raw, 1, &bind) } < 0 {
        destroy();
        return None;
    }
    Some(Buffer::from_parts(
        s,
        raw,
        None,
        Some(Arc::new(memory)),
        true,
    ))
}

/// Runs `args.max_count` (or the count buffer's, at most that many) records of `kind` with the
/// bound `pipeline`.
///
/// One execution runs at most `maxIndirectSequenceCount` records: longer runs without a count
/// split into several executions; runs with a count fit one, as `CommandList` checks
/// (`Features::max_indirect_count`), since the device would not clamp a larger count to its
/// preprocess memory (0016 §6).
fn execute(list: &mut CommandList, kind: Kind, pipeline: VkPipeline, args: &IndirectArgs<'_>) {
    if !args.constant && native(list, kind, args) {
        return;
    }
    let limit = list.shared.properties.max_indirect_sequences;
    list.draw.indirect.get_or_insert_with(Indirect::default);
    let address = args.buffer.raw.address() + args.offset;
    let count = args
        .count
        .map(|(buffer, offset)| buffer.raw.address() + offset);
    debug_assert!(count.is_none() || args.max_count <= limit);
    let mut first = 0;
    while first < args.max_count {
        let records = (args.max_count - first).min(limit);
        let start = address + u64::from(first) * u64::from(kind.stride());
        execute_range(list, kind, pipeline, start, records, count);
        if count.is_some() {
            break;
        }
        first += records;
    }
}

/// `args` without the constant through the native indirect commands, which read each record
/// past its constant word; false (nothing recorded) for a counted dispatch, which has none.
fn native(list: &mut CommandList, kind: Kind, args: &IndirectArgs<'_>) -> bool {
    let fns = list.fns();
    let raw = list.raw();
    let buffer = args.buffer.raw.raw();
    let offset = args.offset + 4;
    let stride = kind.stride();
    let count = args.count.map(|(b, o)| (b.raw.raw(), o));
    // SAFETY: a recording buffer with the right pipeline bound (inside a render pass for draws,
    // outside for dispatches); the argument and count ranges were checked by `commands.rs`.
    unsafe {
        match (kind, count) {
            (Kind::Dispatch, Some(_)) => return false,
            (Kind::Dispatch, None) => {
                for i in 0..u64::from(args.max_count) {
                    (fns.vkCmdDispatchIndirect)(raw, buffer, offset + i * u64::from(stride));
                }
            }
            (Kind::Draw, None) => {
                (fns.vkCmdDrawIndirect)(raw, buffer, offset, args.max_count, stride)
            }
            (Kind::Draw, Some((c, at))) => {
                (fns.vkCmdDrawIndirectCount)(raw, buffer, offset, c, at, args.max_count, stride)
            }
            (Kind::DrawIndexed, None) => {
                (fns.vkCmdDrawIndexedIndirect)(raw, buffer, offset, args.max_count, stride)
            }
            (Kind::DrawIndexed, Some((c, at))) => (fns.vkCmdDrawIndexedIndirectCount)(
                raw,
                buffer,
                offset,
                c,
                at,
                args.max_count,
                stride,
            ),
            (Kind::Mesh, None) => {
                (fns.vkCmdDrawMeshTasksIndirectEXT)(raw, buffer, offset, args.max_count, stride)
            }
            (Kind::Mesh, Some((c, at))) => (fns.vkCmdDrawMeshTasksIndirectCountEXT)(
                raw,
                buffer,
                offset,
                c,
                at,
                args.max_count,
                stride,
            ),
        }
    }
    true
}

/// One execution of `records` records from `address`, or of the count at `count` if fewer.
fn execute_range(
    list: &mut CommandList,
    kind: Kind,
    pipeline: VkPipeline,
    address: u64,
    records: u32,
    count: Option<u64>,
) {
    let shared = list.shared.clone();
    let s = &shared;
    let Some(indirect) = list.draw.indirect.as_mut() else {
        return;
    };
    let layout = indirect.layout(s, kind);
    if layout == 0 {
        return;
    }
    let mut pipeline_info = VkGeneratedCommandsPipelineInfoEXT {
        pipeline,
        ..Default::default()
    };
    let req_info = VkGeneratedCommandsMemoryRequirementsInfoEXT {
        pNext: (&raw mut pipeline_info).cast_const().cast(),
        indirectExecutionSet: 0,
        indirectCommandsLayout: layout,
        maxSequenceCount: records,
        maxDrawCount: 0,
        ..Default::default()
    };
    let mut req = VkMemoryRequirements2::default();
    // SAFETY: valid structures; the layout and pipeline are live.
    unsafe { (s.fns.vkGetGeneratedCommandsMemoryRequirementsEXT)(s.raw, &req_info, &mut req) };
    let r = req.memoryRequirements;
    let preprocess = match r.size {
        0 => 0,
        size => match indirect.preprocess(s, size, r.alignment.max(1), r.memoryTypeBits) {
            Some(address) => address,
            None => {
                marlin_diag::error!("out of memory for {size} bytes of indirect preprocessing");
                return;
            }
        },
    };
    let info = VkGeneratedCommandsInfoEXT {
        pNext: (&raw mut pipeline_info).cast_const().cast(),
        shaderStages: kind.stages(),
        indirectExecutionSet: 0,
        indirectCommandsLayout: layout,
        indirectAddress: address,
        indirectAddressSize: u64::from(records) * u64::from(kind.stride()),
        preprocessAddress: preprocess,
        preprocessSize: r.size,
        maxSequenceCount: records,
        // The GPU runs min(count, records) sequences.
        sequenceCountAddress: count.unwrap_or(0),
        maxDrawCount: 0,
        ..Default::default()
    };
    // SAFETY: a recording buffer with `pipeline` bound (inside a render pass for draws, outside
    // for dispatches); the argument and count ranges were checked by `commands.rs`.
    unsafe { (s.fns.vkCmdExecuteGeneratedCommandsEXT)(list.raw(), VK_FALSE, &info) };
}

pub(crate) fn draw_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
    let pipeline = list.draw.graphics_pipeline;
    execute(list, Kind::Draw, pipeline, args);
}

pub(crate) fn draw_indexed_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
    let pipeline = list.draw.graphics_pipeline;
    execute(list, Kind::DrawIndexed, pipeline, args);
}

pub(crate) fn dispatch_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
    let pipeline = list.draw.compute_pipeline;
    execute(list, Kind::Dispatch, pipeline, args);
}

pub(crate) fn dispatch_mesh_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
    let pipeline = list.draw.graphics_pipeline;
    execute(list, Kind::Mesh, pipeline, args);
}
