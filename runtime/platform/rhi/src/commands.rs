//! Command recording: the methods of [`CommandList`], validated once for every backend.
//!
//! Misuse while recording (a range out of bounds, a missing usage, a command the list's queue
//! type cannot run) panics, as slice indexing does; the GPU never sees it.

use core::ffi::c_void;

use crate::backend::imp::Backend;
use crate::contract::{Commands, Handles, Object};
use crate::{
    ACCELERATION_STRUCTURE_ALIGNMENT, AccelerationStructure, AccelerationStructureCopy,
    AccelerationStructureGeometry, AccelerationStructureInputs, Access, Buffer, BufferBarrier,
    BufferState, BufferTextureCopy, BufferUsage, BuildAccelerationStructure, BuildFlags,
    ClearColor, CommandList, CommandPool, ComputePipeline, DispatchIndirectArgs,
    DrawIndexedIndirectArgs, DrawIndirectArgs, GraphicsPipeline, INSTANCE_ALIGNMENT, IndexFormat,
    IndirectArgs, Layout, MAX_COLOR_ATTACHMENTS, MAX_PUSH_CONSTANTS, MarkerPoint, MeshPipeline,
    PredicateSkip, QueryKind, QueryPool, QueueType, Rect, RenderPassDesc, Result, ShadingRate,
    ShadingRateCombiner, SubresourceRange, TEXTURE_OFFSET_ALIGNMENT, TEXTURE_ROW_PITCH_ALIGNMENT,
    Texture, TextureBarrier, TextureCopy, TextureDesc, TextureResolve, TextureUsage,
    VariableRateShading, Viewport,
};

/// Most viewports and scissors one call sets (Vulkan's and D3D12's guaranteed minimum).
pub const MAX_VIEWPORTS: usize = 16;

/// The accesses [`CommandList::memory_barrier`] orders: the ones that need no layout or state
/// change (legacy D3D12 orders them with a UAV barrier naming no resource).
pub const MEMORY_BARRIER_ACCESS: Access = Access::UNORDERED_ACCESS
    .union(Access::ACCELERATION_STRUCTURE_READ)
    .union(Access::ACCELERATION_STRUCTURE_WRITE);

fn index_size(format: IndexFormat) -> u64 {
    match format {
        IndexFormat::U16 => 2,
        IndexFormat::U32 => 4,
    }
}

/// Mips and layers `range` covers in `desc`, panicking if any lies outside it.
fn range_in(range: &SubresourceRange, desc: &TextureDesc) -> (u32, u32) {
    let mips = match range.mip_count {
        u32::MAX => desc.mip_levels.saturating_sub(range.base_mip),
        n => n,
    };
    let layers = match range.layer_count {
        u32::MAX => desc.array_layers.saturating_sub(range.base_layer),
        n => n,
    };
    assert!(
        mips > 0
            && range
                .base_mip
                .checked_add(mips)
                .is_some_and(|e| e <= desc.mip_levels),
        "subresource mips out of range"
    );
    assert!(
        layers > 0
            && range
                .base_layer
                .checked_add(layers)
                .is_some_and(|e| e <= desc.array_layers),
        "subresource layers out of range"
    );
    (mips, layers)
}

/// `[offset, offset + size)` lies inside `buffer`.
fn in_buffer(buffer: &Buffer, offset: u64, size: u64) -> bool {
    offset
        .checked_add(size)
        .is_some_and(|end| end <= buffer.desc.size)
}

impl CommandList {
    #[inline]
    pub fn queue_type(&self) -> QueueType {
        self.ty
    }

    fn graphics(&self, what: &str) {
        assert!(
            self.ty == QueueType::Graphics,
            "{what} needs a graphics list, not {:?}",
            self.ty
        );
    }

    fn compute(&self, what: &str) {
        assert!(
            self.ty != QueueType::Copy,
            "{what} needs a graphics or compute list, not a copy list"
        );
    }

    /// Starts recording into `pool` (of the list's queue type), discarding what was recorded
    /// before. The previous recording must not be pending on the GPU.
    pub fn begin(&mut self, pool: &mut CommandPool) -> Result<()> {
        crate::api::ensure(pool.ty == self.ty)?;
        self.recording = true;
        Backend::begin(&mut self.raw, &mut pool.raw)
    }

    /// Finishes recording. Reports errors the API detected while recording; a list that failed
    /// to end cannot be submitted until recorded again. `InvalidInput` if the list is not
    /// recording (never begun, or already ended).
    pub fn end(&mut self) -> Result<()> {
        crate::api::ensure(self.recording)?;
        Backend::end(&mut self.raw)?;
        self.recording = false;
        Ok(())
    }

    /// Panics if a barrier's range lies outside its texture, or a barrier from
    /// [`Layout::Undefined`] names `Undefined` as its
    /// [`undefined_from`](TextureBarrier::undefined_from).
    pub fn texture_barriers(&mut self, barriers: &[TextureBarrier<'_>]) {
        for b in barriers {
            range_in(&b.range, &b.texture.desc);
            assert!(
                b.after.layout != Layout::Undefined,
                "a barrier cannot move to Undefined"
            );
            assert!(
                b.before.layout != Layout::Undefined || b.undefined_from != Layout::Undefined,
                "undefined_from names the layout behind Undefined"
            );
        }
        Backend::texture_barriers(&mut self.raw, barriers);
    }

    pub fn buffer_barriers(&mut self, barriers: &[BufferBarrier<'_>]) {
        Backend::buffer_barriers(&mut self.raw, barriers);
    }

    /// An execution and memory dependency on every resource without naming one: the
    /// unordered-access and acceleration structure accesses of `before` complete and become
    /// visible before those of `after` start. For many resources at once, such as a batch of
    /// bottom-level builds followed by the top-level build reading them. It changes no layout or
    /// state, so both sides take only [`MEMORY_BARRIER_ACCESS`] (or [`Access::NONE`]);
    /// other hazards need a buffer or texture barrier.
    pub fn memory_barrier(&mut self, before: BufferState, after: BufferState) {
        self.compute("a memory barrier");
        assert!(
            MEMORY_BARRIER_ACCESS.contains(before.access)
                && MEMORY_BARRIER_ACCESS.contains(after.access),
            "memory barriers order unordered-access and acceleration structure accesses only"
        );
        Backend::memory_barrier(&mut self.raw, before, after);
    }

    /// Binds render targets and applies their load ops. No barriers or copies are allowed until
    /// [`end_render_pass`](Self::end_render_pass).
    pub fn begin_render_pass(&mut self, desc: &RenderPassDesc<'_>) {
        self.graphics("a render pass");
        assert!(desc.color.len() <= MAX_COLOR_ATTACHMENTS);
        assert!(
            desc.area.is_some() || !desc.color.is_empty() || desc.depth.is_some(),
            "a render pass without attachments needs an area"
        );
        Backend::begin_render_pass(&mut self.raw, desc);
    }

    pub fn end_render_pass(&mut self) {
        Backend::end_render_pass(&mut self.raw);
    }

    /// Copies `size` bytes. Panics if a range is out of bounds or a usage is missing.
    pub fn copy_buffer(
        &mut self,
        src: &Buffer,
        src_offset: u64,
        dst: &Buffer,
        dst_offset: u64,
        size: u64,
    ) {
        assert!(src.desc.usage.contains(BufferUsage::COPY_SRC));
        assert!(dst.desc.usage.contains(BufferUsage::COPY_DST));
        assert!(in_buffer(src, src_offset, size));
        assert!(in_buffer(dst, dst_offset, size));
        Backend::copy_buffer(&mut self.raw, src, src_offset, dst, dst_offset, size);
    }

    /// Copies buffer rows into a texture region. Panics if the region is invalid (see
    /// [`BufferTextureCopy`]) or a usage is missing.
    pub fn copy_buffer_to_texture(
        &mut self,
        src: &Buffer,
        dst: &Texture,
        region: &BufferTextureCopy,
    ) {
        assert!(src.desc.usage.contains(BufferUsage::COPY_SRC));
        assert!(dst.desc.usage.contains(TextureUsage::COPY_DST));
        check_copy(src, dst, region);
        Backend::copy_buffer_to_texture(&mut self.raw, src, dst, region);
    }

    /// Copies a texture region into buffer rows. Panics like
    /// [`copy_buffer_to_texture`](Self::copy_buffer_to_texture).
    pub fn copy_texture_to_buffer(
        &mut self,
        src: &Texture,
        dst: &Buffer,
        region: &BufferTextureCopy,
    ) {
        assert!(src.desc.usage.contains(TextureUsage::COPY_SRC));
        assert!(dst.desc.usage.contains(BufferUsage::COPY_DST));
        check_copy(dst, src, region);
        Backend::copy_texture_to_buffer(&mut self.raw, src, dst, region);
    }

    /// Copies a box between textures of the same format (or its sRGB / linear twin) and sample
    /// count. Panics if either side is out of range, misaligned to blocks, or lacks its copy
    /// usage. Depth-stencil copies move both planes; multisampled copies cover whole
    /// subresources.
    pub fn copy_texture(&mut self, src: &Texture, dst: &Texture, region: &TextureCopy) {
        assert!(src.desc.usage.contains(TextureUsage::COPY_SRC));
        assert!(dst.desc.usage.contains(TextureUsage::COPY_DST));
        let (sf, df) = (src.desc.format, dst.desc.format);
        assert!(
            sf.linear() == df.linear(),
            "copy between incompatible formats"
        );
        assert!(
            src.desc.sample_count == dst.desc.sample_count,
            "copy between different sample counts"
        );
        let fits = |t: &TextureDesc, mip: u32, layer: u32, offset: [u32; 3]| {
            let (w, h) = t.mip_size(mip);
            let block = t.format.block_extent();
            let (w, h) = (w.next_multiple_of(block), h.next_multiple_of(block));
            let d = t.mip_depth(mip);
            mip < t.mip_levels
                && layer < t.array_layers
                && [w, h, d].iter().zip(offset.iter().zip(&region.size)).all(
                    |(&extent, (&o, &s))| s > 0 && o.checked_add(s).is_some_and(|e| e <= extent),
                )
                && [offset[0], offset[1], region.size[0], region.size[1]]
                    .iter()
                    .all(|v| v.is_multiple_of(block))
        };
        assert!(
            fits(
                &src.desc,
                region.src_mip,
                region.src_layer,
                region.src_offset
            ),
            "copy source out of range"
        );
        assert!(
            fits(
                &dst.desc,
                region.dst_mip,
                region.dst_layer,
                region.dst_offset
            ),
            "copy destination out of range"
        );
        if sf.is_depth() || src.desc.sample_count > 1 {
            assert!(
                region.src_offset == [0; 3]
                    && region.dst_offset == [0; 3]
                    && (region.size[0], region.size[1]) == src.desc.mip_size(region.src_mip),
                "depth and multisampled copies cover the whole subresource"
            );
        }
        Backend::copy_texture(&mut self.raw, src, dst, region);
    }

    /// Averages the samples of a multisampled color texture into a single-sampled one of the
    /// same size and format family, on graphics lists. The source must be in
    /// [`TextureState::RESOLVE_SRC`](crate::TextureState::RESOLVE_SRC), the destination in
    /// [`RESOLVE_DST`](crate::TextureState::RESOLVE_DST). Depth resolves (min, max, one sample)
    /// are shader work.
    pub fn resolve_texture(&mut self, src: &Texture, dst: &Texture, resolve: &TextureResolve) {
        self.graphics("a resolve");
        let (s, d) = (&src.desc, &dst.desc);
        assert!(
            s.sample_count > 1 && d.sample_count == 1,
            "resolves go from a multisampled to a single-sampled texture"
        );
        assert!(
            !s.format.is_depth() && !s.format.is_integer(),
            "only float and normalized color textures resolve"
        );
        let format = resolve.format.unwrap_or(d.format);
        assert!(
            format.linear() == d.format.linear() && s.format.linear() == d.format.linear(),
            "resolve between incompatible formats"
        );
        assert!(
            resolve.src_mip < s.mip_levels
                && resolve.src_layer < s.array_layers
                && resolve.dst_mip < d.mip_levels
                && resolve.dst_layer < d.array_layers,
            "resolve subresource out of range"
        );
        assert!(
            s.mip_size(resolve.src_mip) == d.mip_size(resolve.dst_mip),
            "resolve between different sizes"
        );
        let resolve = TextureResolve {
            format: Some(format),
            ..*resolve
        };
        Backend::resolve_texture(&mut self.raw, src, dst, &resolve);
    }

    /// Sets `size` bytes from `offset` (both multiples of 4) to repeated `value`, on graphics and
    /// compute lists: counters, histograms, indirect argument resets. The buffer needs
    /// [`BufferUsage::SHADER_WRITE`] and must be in
    /// [`BufferState::CLEAR_STORAGE`](crate::BufferState::CLEAR_STORAGE) (unordered access).
    pub fn fill_buffer(&mut self, buffer: &Buffer, offset: u64, size: u64, value: u32) {
        self.compute("a buffer fill");
        assert!(buffer.desc.usage.contains(BufferUsage::SHADER_WRITE));
        assert!(
            offset.is_multiple_of(4) && size.is_multiple_of(4) && size > 0,
            "fills cover whole 32-bit words"
        );
        assert!(
            in_buffer(buffer, offset, size),
            "fill past the end of the buffer"
        );
        Backend::fill_buffer(&mut self.raw, buffer, offset, size, value);
    }

    /// Clears the layers of one mip of a [`TextureUsage::STORAGE`] texture, in
    /// [`TextureState::CLEAR_STORAGE`](crate::TextureState::CLEAR_STORAGE), to `value` (integer
    /// values for integer formats, floats otherwise), on graphics and compute lists. Render
    /// targets and depth clear with a render pass's load op instead.
    pub fn clear_storage_texture(
        &mut self,
        texture: &Texture,
        range: &SubresourceRange,
        value: ClearColor,
    ) {
        self.compute("a storage clear");
        let d = &texture.desc;
        assert!(d.usage.contains(TextureUsage::STORAGE));
        let (mips, layers) = range_in(range, d);
        assert!(mips == 1, "storage clears cover one mip");
        let integer = d.format.is_integer();
        assert!(
            integer == !matches!(value, ClearColor::Float(_)),
            "clear integer formats with integers and others with floats"
        );
        let range = SubresourceRange {
            base_mip: range.base_mip,
            mip_count: 1,
            base_layer: range.base_layer,
            layer_count: layers,
        };
        Backend::clear_storage_texture(&mut self.raw, texture, &range, value);
    }

    /// Writes `value` into the `u32` at `offset` of `buffer` when the GPU reaches this point or
    /// once earlier work has finished: breadcrumbs that show how far the GPU got before a crash
    /// (put them in a [`Readback`](crate::MemoryLocation::Readback) buffer, which survives a lost
    /// device). The buffer needs [`BufferUsage::COPY_DST`] and must be in
    /// [`BufferState::COPY_DST`](crate::BufferState::COPY_DST) (readback buffers always are).
    /// Requires [`Features::buffer_markers`](crate::Features::buffer_markers).
    pub fn write_marker(&mut self, buffer: &Buffer, offset: u64, value: u32, point: MarkerPoint) {
        assert!(
            self.features.buffer_markers,
            "buffer markers are not supported"
        );
        assert!(buffer.desc.usage.contains(BufferUsage::COPY_DST));
        assert!(
            offset.is_multiple_of(4) && in_buffer(buffer, offset, 4),
            "marker out of range"
        );
        Backend::write_marker(&mut self.raw, buffer, offset, value, point);
    }

    pub fn set_graphics_pipeline(&mut self, pipeline: &GraphicsPipeline) {
        self.graphics("a graphics pipeline");
        Backend::set_graphics_pipeline(&mut self.raw, &pipeline.raw);
    }

    pub fn set_compute_pipeline(&mut self, pipeline: &ComputePipeline) {
        self.compute("a compute pipeline");
        Backend::set_compute_pipeline(&mut self.raw, &pipeline.raw);
    }

    /// Binds a mesh pipeline; push constants then go to it, as for a graphics pipeline.
    pub fn set_mesh_pipeline(&mut self, pipeline: &MeshPipeline) {
        self.graphics("a mesh pipeline");
        Backend::set_mesh_pipeline(&mut self.raw, &pipeline.raw);
    }

    /// Writes `data` at word `offset` of the push constants of the pipeline kind bound last
    /// (graphics if none). Panics past [`MAX_PUSH_CONSTANTS`] words.
    pub fn push_constants(&mut self, offset: u32, data: &[u32]) {
        self.compute("push constants");
        assert!(
            (offset as usize)
                .checked_add(data.len())
                .is_some_and(|end| end <= MAX_PUSH_CONSTANTS as usize)
        );
        Backend::push_constants(&mut self.raw, offset, data);
    }

    pub fn set_viewport(&mut self, viewport: &Viewport) {
        self.set_viewports(core::slice::from_ref(viewport));
    }

    /// Viewports `0..n` for shaders that pick one (`SV_ViewportArrayIndex`); at most
    /// [`MAX_VIEWPORTS`].
    pub fn set_viewports(&mut self, viewports: &[Viewport]) {
        self.graphics("a viewport");
        assert!((1..=MAX_VIEWPORTS).contains(&viewports.len()));
        Backend::set_viewports(&mut self.raw, viewports);
    }

    pub fn set_scissor(&mut self, rect: &Rect) {
        self.set_scissors(core::slice::from_ref(rect));
    }

    /// One scissor per viewport; at most [`MAX_VIEWPORTS`].
    pub fn set_scissors(&mut self, rects: &[Rect]) {
        self.graphics("a scissor");
        assert!((1..=MAX_VIEWPORTS).contains(&rects.len()));
        Backend::set_scissors(&mut self.raw, rects);
    }

    /// The reference value of stencil tests and [`StencilOp::Replace`](crate::StencilOp::Replace)
    /// (0 at the start of each recording).
    pub fn set_stencil_reference(&mut self, reference: u8) {
        self.graphics("a stencil reference");
        Backend::set_stencil_reference(&mut self.raw, reference);
    }

    /// The RGBA that [`BlendFactor::Constant`](crate::BlendFactor::Constant) reads (0 at the
    /// start of each recording).
    pub fn set_blend_constants(&mut self, constants: [f32; 4]) {
        self.graphics("blend constants");
        Backend::set_blend_constants(&mut self.raw, constants);
    }

    /// The stored-depth range of [`DepthState::bounds_test`](crate::DepthState::bounds_test),
    /// `0 <= min <= max <= 1`. Requires
    /// [`Features::depth_bounds`](crate::Features::depth_bounds).
    pub fn set_depth_bounds(&mut self, min: f32, max: f32) {
        self.graphics("depth bounds");
        assert!(self.features.depth_bounds, "depth bounds are not supported");
        assert!((0.0..=1.0).contains(&min) && (min..=1.0).contains(&max));
        Backend::set_depth_bounds(&mut self.raw, min, max);
    }

    /// The draw shading rate, and how it combines with the per-primitive rate and then the
    /// shading-rate image's. Requires [`Features::variable_rate_shading`]; combiners other than
    /// `Keep` need [`VariableRateShading::Image`]. Reset to 1×1 at the start of each recording.
    ///
    /// [`Features::variable_rate_shading`]: crate::Features::variable_rate_shading
    pub fn set_shading_rate(&mut self, rate: ShadingRate, combiners: [ShadingRateCombiner; 2]) {
        self.graphics("a shading rate");
        let vrs = self.features.variable_rate_shading;
        assert!(
            vrs != VariableRateShading::None,
            "variable-rate shading is not supported"
        );
        assert!(
            vrs == VariableRateShading::Image || combiners == [ShadingRateCombiner::Keep; 2],
            "rate combiners need image-based variable-rate shading"
        );
        Backend::set_shading_rate(&mut self.raw, rate, combiners);
    }

    /// The screen-space shading-rate image draws read (one [`ShadingRate::encoded`] texel per
    /// tile), in [`TextureState::SHADING_RATE`](crate::TextureState::SHADING_RATE); `None` for
    /// none. Requires [`VariableRateShading::Image`] and [`TextureUsage::SHADING_RATE`].
    pub fn set_shading_rate_image(&mut self, image: Option<&Texture>) {
        self.graphics("a shading-rate image");
        assert!(
            self.features.variable_rate_shading == VariableRateShading::Image,
            "shading-rate images are not supported"
        );
        assert!(image.is_none_or(|t| t.desc.usage.contains(TextureUsage::SHADING_RATE)));
        Backend::set_shading_rate_image(&mut self.raw, image);
    }

    /// Panics without [`BufferUsage::INDEX`] or with an offset past the end or misaligned.
    pub fn set_index_buffer(&mut self, buffer: &Buffer, offset: u64, format: IndexFormat) {
        self.graphics("an index buffer");
        assert!(buffer.desc.usage.contains(BufferUsage::INDEX));
        assert!(offset < buffer.desc.size && offset.is_multiple_of(index_size(format)));
        Backend::set_index_buffer(&mut self.raw, buffer, offset, format);
    }

    /// Shaders see vertex indices `first_vertex..` and instance indices from 0 on every backend
    /// (D3D12 shaders cannot see the instance offset, so none do; 0002 §3): pass it in push
    /// constants when needed.
    pub fn draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        self.graphics("a draw");
        Backend::draw(
            &mut self.raw,
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        );
    }

    /// Shaders see vertex index `index + vertex_offset`; instances as in [`draw`](Self::draw).
    pub fn draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        self.graphics("a draw");
        Backend::draw_indexed(
            &mut self.raw,
            index_count,
            instance_count,
            first_index,
            vertex_offset,
            first_instance,
        );
    }

    pub fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.compute("a dispatch");
        Backend::dispatch(&mut self.raw, x, y, z);
    }

    /// Launches `x * y * z` amplification (or, without one, mesh) shader groups.
    pub fn dispatch_mesh(&mut self, x: u32, y: u32, z: u32) {
        self.graphics("a mesh dispatch");
        Backend::dispatch_mesh(&mut self.raw, x, y, z);
    }

    fn check_indirect(&self, args: &IndirectArgs<'_>, stride: u64) {
        let b = &args.buffer.desc;
        assert!(
            b.usage.contains(BufferUsage::INDIRECT),
            "argument buffer without INDIRECT usage"
        );
        assert!(
            args.offset.is_multiple_of(4),
            "argument offset not a multiple of 4"
        );
        // One execution runs a count of at most this many records; the device would not clamp a
        // larger count to it (0016 §6).
        assert!(
            args.count.is_none() || args.max_count <= self.features.max_indirect_count,
            "max_count of a counted indirect command above Features::max_indirect_count"
        );
        assert!(
            (args.max_count as u64)
                .checked_mul(stride)
                .and_then(|n| n.checked_add(args.offset))
                .is_some_and(|e| e <= b.size),
            "indirect arguments past the end of the buffer"
        );
        if let Some((count, offset)) = args.count {
            assert!(
                count.desc.usage.contains(BufferUsage::INDIRECT),
                "count buffer without INDIRECT usage"
            );
            assert!(
                offset.is_multiple_of(4) && in_buffer(count, offset, 4),
                "count out of range"
            );
        }
    }

    /// Draws as [`draw`](Self::draw) does, once per [`DrawIndirectArgs`] record.
    pub fn draw_indirect(&mut self, args: &IndirectArgs<'_>) {
        self.graphics("a draw");
        self.check_indirect(args, size_of::<DrawIndirectArgs>() as u64);
        Backend::draw_indirect(&mut self.raw, args);
    }

    /// Draws as [`draw_indexed`](Self::draw_indexed) does, once per [`DrawIndexedIndirectArgs`]
    /// record.
    pub fn draw_indexed_indirect(&mut self, args: &IndirectArgs<'_>) {
        self.graphics("a draw");
        self.check_indirect(args, size_of::<DrawIndexedIndirectArgs>() as u64);
        Backend::draw_indexed_indirect(&mut self.raw, args);
    }

    /// Dispatches once per [`DispatchIndirectArgs`] record.
    pub fn dispatch_indirect(&mut self, args: &IndirectArgs<'_>) {
        self.compute("a dispatch");
        self.check_indirect(args, size_of::<DispatchIndirectArgs>() as u64);
        Backend::dispatch_indirect(&mut self.raw, args);
    }

    /// Mesh-dispatches once per [`DispatchIndirectArgs`] record.
    pub fn dispatch_mesh_indirect(&mut self, args: &IndirectArgs<'_>) {
        self.graphics("a mesh dispatch");
        self.check_indirect(args, size_of::<DispatchIndirectArgs>() as u64);
        Backend::dispatch_mesh_indirect(&mut self.raw, args);
    }

    /// Writes the GPU clock into timestamp query `index` once the work before it has finished.
    pub fn write_timestamp(&mut self, pool: &QueryPool, index: u32) {
        let kind = pool.desc.kind;
        assert!(
            matches!(kind, QueryKind::Timestamp | QueryKind::CopyTimestamp),
            "{kind:?} queries are scoped: begin_query / end_query"
        );
        assert!(
            (kind == QueryKind::CopyTimestamp) == (self.ty == QueueType::Copy),
            "copy lists write CopyTimestamp queries, other lists Timestamp queries"
        );
        assert!(index < pool.desc.count, "query index out of range");
        Backend::write_timestamp(&mut self.raw, &pool.raw, index);
    }

    fn check_scoped(&self, pool: &QueryPool, index: u32) {
        let kind = pool.desc.kind;
        assert!(kind.is_scoped(), "{kind:?} queries are written, not scoped");
        match kind {
            QueryKind::PipelineStatistics => self.compute("pipeline statistics"),
            _ => self.graphics("an occlusion query"),
        }
        assert!(index < pool.desc.count, "query index out of range");
    }

    /// Starts counting into scoped query `index` ([`QueryKind::Occlusion`],
    /// [`BinaryOcclusion`](QueryKind::BinaryOcclusion),
    /// [`PipelineStatistics`](QueryKind::PipelineStatistics)). Each begin pairs with an
    /// [`end_query`](Self::end_query) in the same list. At most one occlusion query (of either
    /// kind) and one pipeline statistics query are active at a time (Vulkan's rule).
    pub fn begin_query(&mut self, pool: &QueryPool, index: u32) {
        self.check_scoped(pool, index);
        Backend::begin_query(&mut self.raw, &pool.raw, index);
    }

    pub fn end_query(&mut self, pool: &QueryPool, index: u32) {
        self.check_scoped(pool, index);
        Backend::end_query(&mut self.raw, &pool.raw, index);
    }

    /// Writes queries `first..first + count` at `offset` (a multiple of 8) of `dst`
    /// ([`BufferUsage::COPY_DST`], in [`Access::COPY_DST`](crate::Access::COPY_DST)), each
    /// [`QueryKind::result_size`] bytes: little-endian `u64`s, or [`PipelineStatistics`]
    /// records.
    ///
    /// [`PipelineStatistics`]: crate::PipelineStatistics
    pub fn resolve_queries(
        &mut self,
        pool: &QueryPool,
        first: u32,
        count: u32,
        dst: &Buffer,
        offset: u64,
    ) {
        assert!(
            first
                .checked_add(count)
                .is_some_and(|e| e <= pool.desc.count)
                && count > 0,
            "query range out of bounds"
        );
        assert!(dst.desc.usage.contains(BufferUsage::COPY_DST));
        assert!(
            offset.is_multiple_of(8)
                && in_buffer(dst, offset, count as u64 * pool.desc.kind.result_size()),
            "query results out of range"
        );
        Backend::resolve_queries(&mut self.raw, &pool.raw, first, count, dst, offset);
    }

    /// Skips the draws, dispatches and copies that follow while the 64-bit value at `offset`
    /// (a multiple of 8) of `buffer` matches `skip` (occlusion culling with last frame's
    /// resolved occlusion queries); `None` stops skipping. The buffer needs
    /// [`BufferUsage::INDIRECT`] and must be in [`BufferState::INDIRECT`](crate::BufferState).
    pub fn set_predication(&mut self, predicate: Option<(&Buffer, u64, PredicateSkip)>) {
        self.compute("predication");
        if let Some((buffer, offset, _)) = predicate {
            assert!(buffer.desc.usage.contains(BufferUsage::INDIRECT));
            assert!(
                offset.is_multiple_of(8) && in_buffer(buffer, offset, 8),
                "predicate out of range"
            );
        }
        Backend::set_predication(&mut self.raw, predicate);
    }

    /// Builds (or, with [`BuildAccelerationStructure::update`], refits) an acceleration
    /// structure on graphics and compute lists. Inputs must be in
    /// [`BufferState::BUILD_INPUT`](crate::BufferState::BUILD_INPUT), the scratch memory in
    /// [`BUILD_SCRATCH`](crate::BufferState::BUILD_SCRATCH), the destination's buffer in
    /// [`ACCELERATION_STRUCTURE_WRITE`](crate::BufferState::ACCELERATION_STRUCTURE_WRITE) and the
    /// bottom-level structures a top-level build instances in
    /// [`ACCELERATION_STRUCTURE_READ`](crate::BufferState::ACCELERATION_STRUCTURE_READ). Panics
    /// unless the scratch memory holds the build's (or update's) scratch size and `dst` the
    /// build's size ([`Device::acceleration_structure_sizes`]).
    ///
    /// [`Device::acceleration_structure_sizes`]: crate::Device::acceleration_structure_sizes
    pub fn build_acceleration_structure(&mut self, build: &BuildAccelerationStructure<'_>) {
        self.compute("an acceleration structure build");
        assert!(
            self.features.ray_query,
            "acceleration structures are not supported"
        );
        crate::api::validate_inputs(&build.inputs).expect("geometries mix triangles and AABBs");
        assert!(
            build.dst.kind() == build.inputs.kind(),
            "the inputs build another kind of structure"
        );
        if let Some(src) = build.update {
            assert!(
                build.flags.contains(BuildFlags::ALLOW_UPDATE) && src.kind() == build.dst.kind(),
                "updates refit a structure built with ALLOW_UPDATE"
            );
        }
        assert!(build.scratch.desc.usage.contains(BufferUsage::SHADER_WRITE));
        assert!(
            build
                .scratch_offset
                .is_multiple_of(ACCELERATION_STRUCTURE_ALIGNMENT)
                && build.scratch_offset < build.scratch.desc.size,
            "scratch offset misaligned or out of range"
        );
        let input = |buffer: &Buffer| {
            assert!(
                buffer
                    .desc
                    .usage
                    .contains(BufferUsage::ACCELERATION_STRUCTURE_INPUT),
                "build input without ACCELERATION_STRUCTURE_INPUT usage"
            );
        };
        match build.inputs {
            AccelerationStructureInputs::BottomLevel(geometries) => {
                for geometry in geometries {
                    match geometry {
                        AccelerationStructureGeometry::Triangles(t) => {
                            input(t.vertices);
                            let size = t.vertex_format.component_size();
                            assert!(
                                t.vertex_offset.is_multiple_of(size)
                                    && t.vertex_stride.is_multiple_of(size)
                                    && t.vertex_count > 0,
                                "vertices misaligned"
                            );
                            let span = (t.vertex_count as u64 - 1)
                                .checked_mul(t.vertex_stride)
                                .and_then(|last| last.checked_add(t.vertex_format.size()));
                            assert!(
                                span.is_some_and(|s| in_buffer(t.vertices, t.vertex_offset, s)),
                                "vertices past the end of the buffer"
                            );
                            match t.indices {
                                Some((indices, offset, format)) => {
                                    input(indices);
                                    let bytes = index_size(format) * 3 * t.triangle_count as u64;
                                    assert!(
                                        offset.is_multiple_of(index_size(format))
                                            && in_buffer(indices, offset, bytes),
                                        "indices out of range"
                                    );
                                }
                                None => assert!(
                                    t.triangle_count.checked_mul(3) == Some(t.vertex_count),
                                    "unindexed triangles take three vertices each"
                                ),
                            }
                            if let Some((transform, offset)) = t.transform {
                                input(transform);
                                assert!(
                                    offset.is_multiple_of(16) && in_buffer(transform, offset, 48),
                                    "transform out of range"
                                );
                            }
                        }
                        AccelerationStructureGeometry::Aabbs(a) => {
                            input(a.buffer);
                            assert!(
                                a.offset.is_multiple_of(8)
                                    && a.stride.is_multiple_of(8)
                                    && a.stride >= 24
                                    && a.count > 0,
                                "AABBs misaligned"
                            );
                            let span = (a.count as u64 - 1)
                                .checked_mul(a.stride)
                                .and_then(|last| last.checked_add(24));
                            assert!(
                                span.is_some_and(|s| in_buffer(a.buffer, a.offset, s)),
                                "AABBs past the end of the buffer"
                            );
                        }
                    }
                }
            }
            AccelerationStructureInputs::TopLevel {
                instances,
                offset,
                count,
            } => {
                input(instances);
                assert!(
                    offset.is_multiple_of(INSTANCE_ALIGNMENT)
                        && in_buffer(instances, offset, count as u64 * 64),
                    "instances out of range"
                );
            }
        }
        let sizes = Backend::build_sizes(&mut self.raw, &build.inputs, build.flags);
        let scratch = match build.update {
            Some(_) => sizes.update_scratch_size,
            None => sizes.build_scratch_size,
        };
        assert!(
            in_buffer(build.scratch, build.scratch_offset, scratch),
            "scratch memory smaller than the build's scratch size"
        );
        assert!(
            build.dst.size() >= sizes.size,
            "destination smaller than the build's result"
        );
        Backend::build_acceleration_structure(&mut self.raw, build);
    }

    /// Copies `src` into `dst` (of the same kind): a clone, or a compaction into a structure of
    /// the size [`write_compacted_sizes`](Self::write_compacted_sizes) reported. `src` in
    /// [`ACCELERATION_STRUCTURE_READ`](crate::BufferState::ACCELERATION_STRUCTURE_READ), `dst` in
    /// [`ACCELERATION_STRUCTURE_WRITE`](crate::BufferState::ACCELERATION_STRUCTURE_WRITE).
    pub fn copy_acceleration_structure(
        &mut self,
        src: &AccelerationStructure,
        dst: &AccelerationStructure,
        mode: AccelerationStructureCopy,
    ) {
        self.compute("an acceleration structure copy");
        assert!(src.kind() == dst.kind(), "copy between kinds of structure");
        Backend::copy_acceleration_structure(&mut self.raw, src, dst, mode);
    }

    /// Writes each structure's compacted size (a `u64`, for structures built with
    /// [`BuildFlags::ALLOW_COMPACTION`]) at `offset` (a multiple of 8) of `dst`
    /// ([`BufferUsage::SHADER_WRITE`], in [`BufferState::COMPUTE_WRITE`]-style unordered access
    /// at [`Stage::EMIT_ACCELERATION_STRUCTURE_INFO`]).
    ///
    /// [`BufferState::COMPUTE_WRITE`]: crate::BufferState::COMPUTE_WRITE
    /// [`Stage::EMIT_ACCELERATION_STRUCTURE_INFO`]: crate::Stage::EMIT_ACCELERATION_STRUCTURE_INFO
    pub fn write_compacted_sizes(
        &mut self,
        structures: &[&AccelerationStructure],
        dst: &Buffer,
        offset: u64,
    ) {
        self.compute("acceleration structure sizes");
        assert!(!structures.is_empty());
        assert!(dst.desc.usage.contains(BufferUsage::SHADER_WRITE));
        assert!(
            offset.is_multiple_of(8) && in_buffer(dst, offset, 8 * structures.len() as u64),
            "sizes out of range"
        );
        Backend::write_compacted_sizes(&mut self.raw, structures, dst, offset);
    }

    /// Opens a named region for GPU debuggers and profilers (RenderDoc, Nsight, PIX). Regions
    /// nest; close each with [`end_event`](Self::end_event) in the same list.
    pub fn begin_event(&mut self, name: &str) {
        Backend::begin_event(&mut self.raw, name);
    }

    pub fn end_event(&mut self) {
        Backend::end_event(&mut self.raw);
    }

    /// A named point in the GPU timeline, for debuggers.
    pub fn set_marker(&mut self, name: &str) {
        Backend::set_marker(&mut self.raw, name);
    }

    /// The native list (Vulkan: `VkCommandBuffer`; D3D12: `ID3D12GraphicsCommandList*`),
    /// borrowed, for libraries that record into it (upscalers). Whatever they bind, the next draw
    /// or dispatch needs its pipeline set again.
    pub fn as_raw(&self) -> *mut c_void {
        Backend::native(Object::CommandList(&self.raw))
    }

    /// Names the list for debuggers and crash reports (which list the GPU was in).
    pub fn set_name(&self, name: &str) {
        Backend::set_name(Object::CommandList(&self.raw), name);
    }
}

fn check_copy(buffer: &Buffer, texture: &Texture, r: &BufferTextureCopy) {
    let d = &texture.desc;
    assert!(
        d.sample_count == 1,
        "multisampled textures do not copy to buffers"
    );
    assert!(
        r.mip < d.mip_levels && r.layer < d.array_layers,
        "copy subresource out of range"
    );
    let (w, h) = d.mip_size(r.mip);
    let block = d.format.block_extent();
    // Block-compressed mips are stored whole blocks wide, even below a block.
    let (w, h) = (w.next_multiple_of(block), h.next_multiple_of(block));
    assert!(
        r.x.checked_add(r.width).is_some_and(|e| e <= w)
            && r.y.checked_add(r.height).is_some_and(|e| e <= h)
            && r.z
                .checked_add(r.depth)
                .is_some_and(|e| e <= d.mip_depth(r.mip))
            && r.width > 0
            && r.height > 0
            && r.depth > 0,
        "copy region outside the subresource"
    );
    assert!(
        [r.x, r.y, r.width, r.height]
            .iter()
            .all(|v| v.is_multiple_of(block)),
        "copy region not block-aligned"
    );
    if d.format.is_depth() {
        assert!(
            r.x == 0 && r.y == 0 && (r.width, r.height) == (w, h),
            "depth copies cover the whole subresource"
        );
    }
    assert!(
        r.bytes_per_row.is_multiple_of(TEXTURE_ROW_PITCH_ALIGNMENT)
            && r.buffer_offset.is_multiple_of(TEXTURE_OFFSET_ALIGNMENT),
        "copy rows misaligned"
    );
    let row = (r.width / block) as u64 * d.format.copy_block_size() as u64;
    assert!(r.bytes_per_row as u64 >= row, "copy rows overlap");
    // Slices follow each other, `height / block` rows apart.
    let rows = (r.height / block) as u64 * r.depth as u64;
    let span = (rows - 1)
        .checked_mul(r.bytes_per_row as u64)
        .and_then(|n| n.checked_add(row));
    assert!(
        span.is_some_and(|s| in_buffer(buffer, r.buffer_offset, s)),
        "copy past the end of the buffer"
    );
}
