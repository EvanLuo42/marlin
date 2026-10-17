//! Render passes (dynamic rendering), graphics state and draws, mesh dispatches, and indirect
//! commands through device-generated commands (0016 §6).

use super::command::CommandList;
use super::pipeline::{GraphicsPipeline, MeshPipeline};
use crate::{
    IndexFormat, IndirectArgs, Rect, RenderPassDesc, ShadingRate, ShadingRateCombiner, Viewport,
};

/// Recording state of the draw area, reset at every `begin`.
#[derive(Default)]
pub(crate) struct DrawState {}

pub(crate) fn begin_render_pass(list: &mut CommandList, desc: &RenderPassDesc<'_>) {
    let _ = (list, desc);
    todo!("vulkan: begin_render_pass")
}

pub(crate) fn end_render_pass(list: &mut CommandList) {
    let _ = list;
    todo!("vulkan: end_render_pass")
}

pub(crate) fn set_graphics_pipeline(list: &mut CommandList, pipeline: &GraphicsPipeline) {
    let _ = (list, pipeline);
    todo!("vulkan: set_graphics_pipeline")
}

pub(crate) fn set_mesh_pipeline(list: &mut CommandList, pipeline: &MeshPipeline) {
    let _ = (list, pipeline);
    todo!("vulkan: set_mesh_pipeline")
}

pub(crate) fn set_viewports(list: &mut CommandList, viewports: &[Viewport]) {
    let _ = (list, viewports);
    todo!("vulkan: set_viewports")
}

pub(crate) fn set_scissors(list: &mut CommandList, rects: &[Rect]) {
    let _ = (list, rects);
    todo!("vulkan: set_scissors")
}

pub(crate) fn set_stencil_reference(list: &mut CommandList, reference: u8) {
    let _ = (list, reference);
    todo!("vulkan: set_stencil_reference")
}

pub(crate) fn set_blend_constants(list: &mut CommandList, constants: [f32; 4]) {
    let _ = (list, constants);
    todo!("vulkan: set_blend_constants")
}

pub(crate) fn set_depth_bounds(list: &mut CommandList, min: f32, max: f32) {
    let _ = (list, min, max);
    todo!("vulkan: set_depth_bounds")
}

pub(crate) fn set_shading_rate(
    list: &mut CommandList,
    rate: ShadingRate,
    combiners: [ShadingRateCombiner; 2],
) {
    let _ = (list, rate, combiners);
    todo!("vulkan: set_shading_rate")
}

pub(crate) fn set_shading_rate_image(list: &mut CommandList, image: Option<&crate::Texture>) {
    let _ = (list, image);
    todo!("vulkan: set_shading_rate_image")
}

pub(crate) fn set_index_buffer(
    list: &mut CommandList,
    buffer: &crate::Buffer,
    offset: u64,
    format: IndexFormat,
) {
    let _ = (list, buffer, offset, format);
    todo!("vulkan: set_index_buffer")
}

pub(crate) fn draw(
    list: &mut CommandList,
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
) {
    let _ = (
        list,
        vertex_count,
        instance_count,
        first_vertex,
        first_instance,
    );
    todo!("vulkan: draw")
}

pub(crate) fn draw_indexed(
    list: &mut CommandList,
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) {
    let _ = (
        list,
        index_count,
        instance_count,
        first_index,
        vertex_offset,
        first_instance,
    );
    todo!("vulkan: draw_indexed")
}

pub(crate) fn dispatch_mesh(list: &mut CommandList, x: u32, y: u32, z: u32) {
    let _ = (list, x, y, z);
    todo!("vulkan: dispatch_mesh")
}

pub(crate) fn draw_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
    let _ = (list, args);
    todo!("vulkan: draw_indirect")
}

pub(crate) fn draw_indexed_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
    let _ = (list, args);
    todo!("vulkan: draw_indexed_indirect")
}

pub(crate) fn dispatch_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
    let _ = (list, args);
    todo!("vulkan: dispatch_indirect")
}

pub(crate) fn dispatch_mesh_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
    let _ = (list, args);
    todo!("vulkan: dispatch_mesh_indirect")
}
