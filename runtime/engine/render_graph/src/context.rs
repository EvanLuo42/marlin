//! What a pass's recording closure gets (0007 §3.2): the bindless indices of the views of what the
//! pass declared, and the commands. Barriers, render passes and the RHI objects stay the graph's.

use alloc::boxed::Box;
use alloc::vec::Vec;

use marlin_containers::ArrayVec;
use marlin_jobs::JobSystem;
use marlin_jobs::sync::Mutex;
use marlin_rhi::{
    Buffer, BufferTextureCopy, BufferViewDesc, BufferViewKind, ClearColor, ColorAttachment,
    CommandList, CommandPool, ComputePipeline, DepthAttachment, Device, GraphicsPipeline,
    IndexFormat, IndirectArgs, LoadOp, MAX_COLOR_ATTACHMENTS, MAX_PUSH_CONSTANTS, MeshPipeline,
    QueueType, Rect, RenderPassDesc, Result, StoreOp, Texture, TextureCopy, Viewport,
};
use marlin_serial::cooked::Pod;

use crate::access::{Access, Use};
use crate::backend::{Backend, Rhi};
use crate::frame::Upload;
use crate::graph::{AccessRec, Res};
use crate::handle::{BufferHandle, Range, TextureHandle};

/// A view index that was not created: the access has no shader view.
pub(crate) const NO_VIEW: u32 = u32::MAX;

/// Records one pass: resolves the pass's handles to bindless indices and issues its commands.
/// Barriers and render pass boundaries are the graph's.
pub struct PassContext<'a, B: Backend = Rhi> {
    pub(crate) parts: B::Parts<'a>,
}

/// A command list and the pool it records from: lists recorded at the same time need pools of
/// their own.
pub(crate) struct Recorder {
    pub pool: CommandPool,
    pub list: CommandList,
}

/// The lists one job records, in submission order: the finished ones and the one being
/// recorded.
pub(crate) struct ListSeq {
    pub current: Recorder,
    pub done: Vec<Recorder>,
}

impl ListSeq {
    /// Ends the current list and continues in `next` (begun).
    fn switch(&mut self, next: Recorder) {
        let finished = core::mem::replace(&mut self.current, next);
        self.done.push(finished);
        let last = self.done.last_mut().expect("just pushed");
        last.list.end().expect("ending a command list");
    }
}

/// Hands out begun command lists of one queue, reusing the frame slot's free ones.
pub(crate) struct Lists<'a> {
    pub device: &'a Device,
    pub queue: QueueType,
    pub free: &'a Mutex<Vec<Recorder>>,
}

impl Lists<'_> {
    pub(crate) fn take(&self) -> Result<Recorder> {
        let mut recorder = match self.free.lock().pop() {
            Some(r) => r,
            None => {
                let pool = self.device.create_command_pool_for(self.queue)?;
                let list = self.device.create_command_list(&pool)?;
                Recorder { pool, list }
            }
        };
        recorder.list.begin(&mut recorder.pool)?;
        Ok(recorder)
    }
}

/// A raster pass's attachments, with their load and store ops.
#[derive(Clone)]
pub(crate) struct RenderPass<'a> {
    pub colors: ArrayVec<ColorAttachment<'a>, MAX_COLOR_ATTACHMENTS>,
    pub depth: Option<DepthAttachment<'a>>,
    /// The first attachment's size: the default viewport and scissor.
    pub size: Option<(u32, u32)>,
    /// Recorded in sub-lists ([`PassContext::parallel`]): no render pass in the pass's own list.
    pub parallel: bool,
    /// The sub-lists have run (only once per pass).
    pub done: bool,
}

impl<'a> RenderPass<'a> {
    /// Begins the render pass with the loads of the first instance (`first`) or loading what
    /// the previous one stored, and the stores of the last instance (`last`) or storing, and
    /// sets the default viewport and scissor.
    pub(crate) fn begin(&self, list: &mut CommandList, first: bool, last: bool) {
        let mut colors = self.colors.clone();
        for c in &mut colors {
            if !first {
                c.load = LoadOp::Load;
            }
            if !last {
                c.store = StoreOp::Store;
            }
        }
        let depth = self.depth.map(|mut d| {
            if !first {
                d.load = LoadOp::Load;
                d.stencil_load = LoadOp::Load;
            }
            if !last {
                d.store = StoreOp::Store;
                d.stencil_store = StoreOp::Store;
            }
            d
        });
        list.begin_render_pass(&RenderPassDesc {
            color: &colors,
            depth,
        });
        if let Some((w, h)) = self.size {
            list.set_viewport(&Viewport::full(w, h));
            list.set_scissor(&Rect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            });
        }
    }
}

/// What a context records with on the RHI.
pub struct RhiParts<'a> {
    pub(crate) seq: &'a mut ListSeq,
    pub(crate) lists: &'a Lists<'a>,
    pub(crate) jobs: Option<&'a JobSystem>,
    pub(crate) device: &'a Device,
    pub(crate) pass: &'a str,
    /// The pass's accesses and, for each, its view's index or [`NO_VIEW`].
    pub(crate) recs: &'a [AccessRec],
    pub(crate) views: &'a [u32],
    /// Every texture and buffer slot's object (`None` for transients the plan does not use).
    pub(crate) textures: &'a [Option<&'a Texture>],
    pub(crate) buffers: &'a [Option<&'a Buffer>],
    /// Mips and layers of every texture slot, to resolve handle ranges.
    pub(crate) shapes: &'a [(u32, u32)],
    pub(crate) upload: &'a Mutex<Upload>,
    /// Objects to drop once this frame's GPU work is done.
    pub(crate) retire: &'a Mutex<Vec<Box<dyn Send>>>,
    /// A raster pass's render pass.
    pub(crate) render: Option<RenderPass<'a>>,
    /// A sub-list of [`PassContext::parallel`]: it cannot split again.
    pub(crate) nested: bool,
}

impl RhiParts<'_> {
    fn list(&mut self) -> &mut CommandList {
        &mut self.seq.current.list
    }

    /// Draws need the render pass, which a pass recorded in sub-lists only has inside them.
    fn draws(&self) {
        assert!(
            !self.render.as_ref().is_some_and(|r| r.parallel) || self.nested,
            "pass {}: records in parallel; draw inside ctx.parallel",
            self.pass
        );
    }
}

/// What a view lookup wants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    Read,
    Write,
    Uniform,
}

impl Want {
    const fn matches(self, usage: Use) -> bool {
        matches!(
            (self, usage),
            (
                Self::Read,
                Use::Shader(
                    Access::ComputeRead | Access::VertexRead | Access::PixelRead | Access::MeshRead,
                ),
            ) | (
                Self::Write,
                Use::Shader(Access::ComputeWrite | Access::PixelWrite)
            ) | (Self::Uniform, Use::Shader(Access::Uniform))
        )
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Read => "a shader read",
            Self::Write => "a shader write",
            Self::Uniform => "a uniform read",
        }
    }
}

/// A handle a view can be looked up by: [`TextureHandle`] or [`BufferHandle`].
pub trait ViewHandle: Copy + sealed::Sealed {
    #[doc(hidden)]
    fn key(self) -> ViewKey;
}

/// What a view lookup goes by: the resource, the texture range and the buffer bytes.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct ViewKey(Res, Range, (u64, u64));

mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::TextureHandle<'_> {}
    impl Sealed for crate::BufferHandle<'_> {}
}

impl ViewHandle for TextureHandle<'_> {
    fn key(self) -> ViewKey {
        ViewKey(Res::Texture(self.index), self.range, (0, u64::MAX))
    }
}

impl ViewHandle for BufferHandle<'_> {
    fn key(self) -> ViewKey {
        ViewKey(
            Res::Buffer(self.index),
            Range::ALL,
            (self.offset, self.size),
        )
    }
}

/// A pipeline a pass can bind: graphics, compute or mesh.
pub trait Pipeline {
    #[doc(hidden)]
    fn bind(&self, list: &mut CommandList);
}

impl Pipeline for GraphicsPipeline {
    fn bind(&self, list: &mut CommandList) {
        list.set_graphics_pipeline(self);
    }
}

impl Pipeline for ComputePipeline {
    fn bind(&self, list: &mut CommandList) {
        list.set_compute_pipeline(self);
    }
}

impl Pipeline for MeshPipeline {
    fn bind(&self, list: &mut CommandList) {
        list.set_mesh_pipeline(self);
    }
}

impl<'a> PassContext<'a, Rhi> {
    /// The read-only view of what `h` covers, declared with a shader read.
    pub fn srv(&self, h: impl ViewHandle) -> u32 {
        self.view(h, Want::Read)
    }

    /// The writable view of what `h` covers (one mip of a texture), declared with a shader write.
    pub fn uav(&self, h: impl ViewHandle) -> u32 {
        self.view(h, Want::Write)
    }

    /// The uniform view of a buffer declared with [`Access::Uniform`].
    pub fn cbv(&self, h: BufferHandle<'_>) -> u32 {
        self.view(h, Want::Uniform)
    }

    fn view(&self, h: impl ViewHandle, want: Want) -> u32 {
        let p = &self.parts;
        let i = p.find(h.key(), |u| want.matches(u), want.name());
        let view = p.views[i];
        assert!(
            view != NO_VIEW,
            "pass {}: {} has no view; writable texture views cover one mip",
            p.pass,
            want.name()
        );
        view
    }

    pub fn set_pipeline(&mut self, pipeline: &impl Pipeline) {
        pipeline.bind(self.parts.list());
    }

    /// Writes `value` into the push constants (0002 §2.1: at most 128 bytes, whole words).
    pub fn push<T: Pod>(&mut self, value: &T) {
        let bytes = bytes_of(core::slice::from_ref(value));
        assert!(
            bytes.len().is_multiple_of(4) && bytes.len() <= 4 * MAX_PUSH_CONSTANTS as usize,
            "pass {}: push constants are whole words, at most {} bytes; got {}",
            self.parts.pass,
            4 * MAX_PUSH_CONSTANTS,
            bytes.len()
        );
        let mut words = [0u32; MAX_PUSH_CONSTANTS as usize];
        for (w, chunk) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *w = u32::from_ne_bytes(*chunk);
        }
        self.push_words(&words[..bytes.len() / 4]);
    }

    pub fn push_words(&mut self, words: &[u32]) {
        assert!(
            words.len() <= MAX_PUSH_CONSTANTS as usize,
            "pass {}: {} push constant words, at most {MAX_PUSH_CONSTANTS}",
            self.parts.pass,
            words.len()
        );
        self.parts.list().push_constants(0, words);
    }

    /// Copies `value` into this frame's upload memory; returns a uniform view of it.
    pub fn upload<T: Pod>(&mut self, value: &T) -> u32 {
        let bytes = bytes_of(core::slice::from_ref(value));
        let size = (bytes.len() as u64).next_multiple_of(16);
        self.upload_view(bytes, 256, BufferViewKind::Uniform, size)
    }

    /// Copies `values` into this frame's upload memory; returns a read-only structured view of
    /// them.
    pub fn upload_slice<T: Pod>(&mut self, values: &[T]) -> u32 {
        let stride = size_of::<T>() as u32;
        assert!(
            !values.is_empty() && stride.is_multiple_of(4) && stride <= 2048,
            "pass {}: uploads are non-empty, of elements of whole words up to 2048 bytes",
            self.parts.pass
        );
        let bytes = bytes_of(values);
        self.upload_view(
            bytes,
            u64::from(stride),
            BufferViewKind::Structured { stride },
            bytes.len() as u64,
        )
    }

    fn upload_view(&mut self, bytes: &[u8], align: u64, kind: BufferViewKind, size: u64) -> u32 {
        let p = &self.parts;
        let view = {
            let mut upload = p.upload.lock();
            let (buffer, offset) = upload.write(p.device, bytes, align, size);
            p.device
                .create_buffer_view(
                    buffer,
                    &BufferViewDesc {
                        kind,
                        writable: false,
                        offset,
                        size,
                    },
                )
                .expect("a view of upload memory")
        };
        let index = view.index();
        p.retire.lock().push(Box::new(view));
        index
    }

    /// Records the pass's work as `n` command lists recorded at once (on the job system, when
    /// the frame runs on one), then continues in the pass's own list: `f(i, sub)` records list
    /// `i`, and the lists run in order of `i` (0007 §6.3). For the shadow views of many lights,
    /// say, or a large pass split by ranges of draws.
    ///
    /// A raster pass that records this way declares it (`b.record_in_parallel()`); its draws are
    /// all inside `f`, each sub-list opens and closes the render pass, the first one with the
    /// pass's load ops and the last one with its store ops. A raster pass splits once; compute
    /// and copy passes any number of times. Sub-lists cannot split again.
    pub fn parallel(&mut self, n: usize, f: impl Fn(usize, &mut PassContext<'_, Rhi>) + Sync) {
        let p = &mut self.parts;
        assert!(!p.nested, "pass {}: a sub-list cannot split again", p.pass);
        if let Some(r) = &p.render {
            assert!(
                r.parallel,
                "pass {}: a raster pass splits in parallel only when declared with \
                 b.record_in_parallel()",
                p.pass
            );
            assert!(!r.done, "pass {}: a raster pass splits once", p.pass);
        }
        if n == 0 {
            return;
        }
        let mut seqs: Vec<ListSeq> = (0..n)
            .map(|_| ListSeq {
                current: p.lists.take().expect("a command list"),
                done: Vec::new(),
            })
            .collect();
        let render = p.render.clone();
        let (device, lists, jobs, pass, recs, views, textures, buffers, shapes, upload, retire) = (
            p.device, p.lists, p.jobs, p.pass, p.recs, p.views, p.textures, p.buffers, p.shapes,
            p.upload, p.retire,
        );
        let record = |i: usize, seq: &mut ListSeq| {
            if let Some(r) = &render {
                r.begin(&mut seq.current.list, i == 0, i == n - 1);
            }
            let mut sub = PassContext {
                parts: RhiParts {
                    seq,
                    lists,
                    jobs: None,
                    device,
                    pass,
                    recs,
                    views,
                    textures,
                    buffers,
                    shapes,
                    upload,
                    retire,
                    render: render.clone(),
                    nested: true,
                },
            };
            f(i, &mut sub);
            if render.is_some() {
                sub.parts.list().end_render_pass();
            }
        };
        match jobs {
            Some(jobs) => jobs.scope(|s| {
                for (i, seq) in seqs.iter_mut().enumerate() {
                    let record = &record;
                    s.spawn(move || record(i, seq));
                }
            }),
            None => {
                for (i, seq) in seqs.iter_mut().enumerate() {
                    record(i, seq);
                }
            }
        }
        // This list ends here; the sub-lists follow it, then a new one for the rest.
        let next = p.lists.take().expect("a command list");
        p.seq.switch(next);
        for mut seq in seqs {
            seq.current.list.end().expect("ending a command list");
            p.seq.done.extend(seq.done);
            p.seq.done.push(seq.current);
        }
        if let Some(r) = &mut p.render {
            r.done = true;
        }
    }

    pub fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.parts.list().dispatch(x, y, z);
    }

    pub fn dispatch_mesh(&mut self, x: u32, y: u32, z: u32) {
        self.parts.draws();
        self.parts.list().dispatch_mesh(x, y, z);
    }

    pub fn draw(&mut self, vertices: u32, instances: u32, first_vertex: u32, first_instance: u32) {
        self.parts.draws();
        self.parts
            .list()
            .draw(vertices, instances, first_vertex, first_instance);
    }

    pub fn draw_indexed(
        &mut self,
        indices: u32,
        instances: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        self.parts.draws();
        self.parts.list().draw_indexed(
            indices,
            instances,
            first_index,
            vertex_offset,
            first_instance,
        );
    }

    /// Binds a buffer declared with [`Access::Index`].
    pub fn index_buffer(&mut self, h: BufferHandle<'_>, format: IndexFormat) {
        let buffer = self.buffer(h, Access::Index);
        self.parts.list().set_index_buffer(buffer, h.offset, format);
    }

    /// `max_count` dispatches from arguments at `args` (declared [`Access::Indirect`]); with
    /// `count`, the first as many as the `u32` there says.
    pub fn dispatch_indirect(
        &mut self,
        args: BufferHandle<'_>,
        max_count: u32,
        count: Option<BufferHandle<'_>>,
    ) {
        let args = self.indirect(args, max_count, count);
        self.parts.list().dispatch_indirect(&args);
    }

    pub fn dispatch_mesh_indirect(
        &mut self,
        args: BufferHandle<'_>,
        max_count: u32,
        count: Option<BufferHandle<'_>>,
    ) {
        self.parts.draws();
        let args = self.indirect(args, max_count, count);
        self.parts.list().dispatch_mesh_indirect(&args);
    }

    pub fn draw_indirect(
        &mut self,
        args: BufferHandle<'_>,
        max_count: u32,
        count: Option<BufferHandle<'_>>,
    ) {
        self.parts.draws();
        let args = self.indirect(args, max_count, count);
        self.parts.list().draw_indirect(&args);
    }

    pub fn draw_indexed_indirect(
        &mut self,
        args: BufferHandle<'_>,
        max_count: u32,
        count: Option<BufferHandle<'_>>,
    ) {
        self.parts.draws();
        let args = self.indirect(args, max_count, count);
        self.parts.list().draw_indexed_indirect(&args);
    }

    fn indirect(
        &self,
        args: BufferHandle<'_>,
        max_count: u32,
        count: Option<BufferHandle<'_>>,
    ) -> IndirectArgs<'a> {
        let buffer = self.buffer(args, Access::Indirect);
        let count = count.map(|c| (self.buffer(c, Access::Indirect), c.offset));
        IndirectArgs {
            buffer,
            offset: args.offset,
            max_count,
            count,
        }
    }

    /// Raster passes start with the viewport and scissor covering their first attachment.
    pub fn set_viewport(&mut self, viewport: &Viewport) {
        self.parts.list().set_viewport(viewport);
    }

    pub fn set_scissor(&mut self, rect: &Rect) {
        self.parts.list().set_scissor(rect);
    }

    pub fn set_stencil_reference(&mut self, reference: u8) {
        self.parts.list().set_stencil_reference(reference);
    }

    /// Copies `size` bytes between buffers declared [`Access::CopySrc`] and [`Access::CopyDst`],
    /// at offsets into what the handles cover.
    pub fn copy_buffer(
        &mut self,
        src: BufferHandle<'_>,
        src_offset: u64,
        dst: BufferHandle<'_>,
        dst_offset: u64,
        size: u64,
    ) {
        let s = self.buffer(src, Access::CopySrc);
        let d = self.buffer(dst, Access::CopyDst);
        self.parts
            .list()
            .copy_buffer(s, src.offset + src_offset, d, dst.offset + dst_offset, size);
    }

    pub fn copy_texture(
        &mut self,
        src: TextureHandle<'_>,
        dst: TextureHandle<'_>,
        region: &TextureCopy,
    ) {
        let s = self.texture(src, Access::CopySrc);
        let d = self.texture(dst, Access::CopyDst);
        self.parts.list().copy_texture(s, d, region);
    }

    pub fn copy_buffer_to_texture(
        &mut self,
        src: BufferHandle<'_>,
        dst: TextureHandle<'_>,
        region: &BufferTextureCopy,
    ) {
        let s = self.buffer(src, Access::CopySrc);
        let d = self.texture(dst, Access::CopyDst);
        self.parts.list().copy_buffer_to_texture(s, d, region);
    }

    pub fn copy_texture_to_buffer(
        &mut self,
        src: TextureHandle<'_>,
        dst: BufferHandle<'_>,
        region: &BufferTextureCopy,
    ) {
        let s = self.texture(src, Access::CopySrc);
        let d = self.buffer(dst, Access::CopyDst);
        self.parts.list().copy_texture_to_buffer(s, d, region);
    }

    /// Fills what `h` covers (declared [`Access::Clear`]) with `value`.
    pub fn fill_buffer(&mut self, h: BufferHandle<'_>, value: u32) {
        let buffer = self.buffer(h, Access::Clear);
        let size = match h.size {
            u64::MAX => buffer.desc().size - h.offset,
            n => n,
        };
        self.parts.list().fill_buffer(buffer, h.offset, size, value);
    }

    /// Clears what `h` covers (one mip, declared [`Access::Clear`]) to `value`.
    pub fn clear_storage_texture(&mut self, h: TextureHandle<'_>, value: ClearColor) {
        let clear = Use::Shader(Access::Clear);
        let i = self.parts.find(h.key(), |u| u == clear, clear.name());
        let range = self.parts.recs[i].range.to_rhi();
        let texture = self.texture(h, Access::Clear);
        self.parts
            .list()
            .clear_storage_texture(texture, &range, value);
    }

    fn buffer(&self, h: BufferHandle<'_>, access: Access) -> &'a Buffer {
        let p = &self.parts;
        let usage = Use::Shader(access);
        p.find(h.key(), |u| u == usage, usage.name());
        p.buffers[h.index as usize].expect("a buffer the pass uses exists")
    }

    fn texture(&self, h: TextureHandle<'_>, access: Access) -> &'a Texture {
        let p = &self.parts;
        let usage = Use::Shader(access);
        p.find(h.key(), |u| u == usage, usage.name());
        p.textures[h.index as usize].expect("a texture the pass uses exists")
    }
}

impl RhiParts<'_> {
    /// The access of this pass a handle names: of the same resource and kind, covering what the
    /// handle covers; or the only such access, for a handle covering the whole resource (writes
    /// return those). Panics if the pass did not declare it.
    fn find(
        &self,
        ViewKey(res, range, bytes): ViewKey,
        kind: impl Fn(Use) -> bool,
        what: &str,
    ) -> usize {
        let resolved = match res {
            Res::Texture(t) => {
                let (mips, layers) = self.shapes[t as usize];
                range.resolve(mips, layers)
            }
            Res::Buffer(_) => range,
        };
        let mut only = None;
        let mut count = 0;
        for (i, rec) in self.recs.iter().enumerate() {
            if rec.res != res || !kind(rec.usage) {
                continue;
            }
            let same = match res {
                Res::Texture(_) => rec.range == resolved,
                Res::Buffer(_) => {
                    rec.bytes.0 == bytes.0 && (bytes.1 == u64::MAX || rec.bytes.1 == bytes.1)
                }
            };
            if same {
                return i;
            }
            only = Some(i);
            count += 1;
        }
        let whole = match res {
            Res::Texture(_) => range == Range::ALL,
            Res::Buffer(_) => bytes == (0, u64::MAX),
        };
        match only {
            Some(i) if count == 1 && whole => i,
            _ => panic!(
                "pass {}: uses a handle as {what} without declaring that access",
                self.pass
            ),
        }
    }
}

/// The bytes of `values`.
fn bytes_of<T: Pod>(values: &[T]) -> &[u8] {
    // SAFETY: `T: Pod` has no padding bytes, so every byte of the slice is initialized; the
    // length is the slice's size in bytes.
    unsafe { core::slice::from_raw_parts(values.as_ptr().cast(), size_of_val(values)) }
}
