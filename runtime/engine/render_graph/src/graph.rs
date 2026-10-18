//! Declaring a frame: resources, passes and their accesses (0007 §1–3).

use alloc::boxed::Box;
use alloc::vec::Vec;

use marlin_containers::ArrayVec;
use marlin_rhi::{
    BufferState, BufferUsage, LoadOp, MAX_COLOR_ATTACHMENTS, QueueType, StoreOp, TextureState,
    TextureUsage,
};
use marlin_std::mem::Arena;

use crate::access::{Access, Effect, PassKind, Use};
use crate::backend::{Backend, Rhi};
use crate::context::PassContext;
use crate::desc::{BufferDesc, TextureDesc};
use crate::handle::{BufferHandle, Range, TextureHandle};

pub(crate) type Exec<'g, B> = Box<dyn FnOnce(&mut PassContext<'_, B>) + Send + 'g, &'g Arena>;

/// How an imported texture enters the frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImportTexture {
    /// The state the texture is in when the frame starts.
    pub initial_state: TextureState,
    /// Passes may only read it; Development builds panic on a write.
    pub read_only: bool,
}

impl Default for ImportTexture {
    fn default() -> Self {
        Self {
            initial_state: TextureState::UNDEFINED,
            read_only: false,
        }
    }
}

/// How an imported buffer enters the frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImportBuffer {
    pub initial_state: BufferState,
    pub read_only: bool,
}

impl Default for ImportBuffer {
    fn default() -> Self {
        Self {
            initial_state: BufferState::NONE,
            read_only: false,
        }
    }
}

pub(crate) struct Imported<T, S, U> {
    #[expect(dead_code, reason = "read when the frame context runs plans (0007 §7)")]
    pub object: T,
    pub initial: S,
    pub read_only: bool,
    /// The object's usage: accesses must stay within it.
    pub usage: U,
}

pub(crate) struct TextureNode<'g, B: Backend> {
    pub name: &'g str,
    pub desc: TextureDesc,
    /// The newest version; handles of older ones are stale.
    pub version: u32,
    pub import: Option<Imported<&'g B::Texture, TextureState, TextureUsage>>,
}

pub(crate) struct BufferNode<'g, B: Backend> {
    pub name: &'g str,
    pub desc: BufferDesc,
    pub version: u32,
    pub import: Option<Imported<&'g B::Buffer, BufferState, BufferUsage>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Res {
    Texture(u32),
    Buffer(u32),
}

/// One access a pass declared.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AccessRec {
    pub res: Res,
    /// Resolved against the texture (explicit counts); [`Range::ALL`] for buffers.
    pub range: Range,
    /// Bytes of a buffer the pass's views cover.
    pub bytes: (u64, u64),
    pub usage: Use,
    pub effect: Effect,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ColorTarget {
    pub texture: u32,
    pub mip: u32,
    pub layer: u32,
    pub load: LoadOp,
    pub store: StoreOp,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DepthTarget {
    pub texture: u32,
    pub mip: u32,
    pub layer: u32,
    pub load: LoadOp<f32>,
    pub store: StoreOp,
    pub stencil_load: LoadOp<u8>,
    pub stencil_store: StoreOp,
    pub read_only: bool,
}

pub(crate) struct PassNode<'g, B: Backend> {
    pub name: &'g str,
    pub kind: PassKind,
    pub queue: QueueType,
    pub side_effect: bool,
    /// Recording cost hint, microseconds.
    pub cost_us: Option<u32>,
    /// Range into [`RenderGraph::accesses`].
    pub accesses: core::ops::Range<u32>,
    pub colors: ArrayVec<ColorTarget, MAX_COLOR_ATTACHMENTS>,
    pub depth: Option<DepthTarget>,
    pub exec: Option<Exec<'g, B>>,
}

/// One frame's graph. Built from scratch every frame in the frame's arena: declaration order is
/// execution order; compiling only culls, aliases and (later) moves work between queues, none of
/// which changes what a pass reads (0007 §1).
pub struct RenderGraph<'g, B: Backend = Rhi> {
    pub(crate) arena: &'g Arena,
    pub(crate) textures: Vec<TextureNode<'g, B>, &'g Arena>,
    pub(crate) buffers: Vec<BufferNode<'g, B>, &'g Arena>,
    pub(crate) passes: Vec<PassNode<'g, B>, &'g Arena>,
    pub(crate) accesses: Vec<AccessRec, &'g Arena>,
}

impl<'g, B: Backend> RenderGraph<'g, B> {
    /// An empty graph allocating from `arena`, the frame's arena.
    pub fn new(arena: &'g Arena) -> Self {
        Self {
            arena,
            textures: Vec::new_in(arena),
            buffers: Vec::new_in(arena),
            passes: Vec::new_in(arena),
            accesses: Vec::new_in(arena),
        }
    }

    /// A transient texture: created by the graph, alive from its first use to its last, its
    /// memory shared with transients whose lifetimes do not overlap.
    pub fn create_texture(&mut self, name: &str, desc: TextureDesc) -> TextureHandle<'g> {
        assert!(
            desc.width > 0 && desc.height > 0 && desc.depth > 0,
            "texture {name} is empty"
        );
        assert!(
            desc.mip_levels > 0 && desc.array_layers > 0 && desc.sample_count > 0,
            "texture {name} has no mips, layers or samples"
        );
        let index = self.textures.len() as u32;
        self.textures.push(TextureNode {
            name: self.arena.alloc_str(name),
            desc,
            version: 0,
            import: None,
        });
        TextureHandle::new(index, 0)
    }

    /// A transient buffer; see [`create_texture`](Self::create_texture).
    pub fn create_buffer(&mut self, name: &str, desc: BufferDesc) -> BufferHandle<'g> {
        assert!(desc.size > 0, "buffer {name} is empty");
        let index = self.buffers.len() as u32;
        self.buffers.push(BufferNode {
            name: self.arena.alloc_str(name),
            desc,
            version: 0,
            import: None,
        });
        BufferHandle::new(index, 0)
    }

    /// A texture the caller owns, starting the frame undefined.
    pub fn import_texture(&mut self, name: &str, texture: &'g B::Texture) -> TextureHandle<'g> {
        self.import_texture_with(name, texture, ImportTexture::default())
    }

    pub fn import_texture_with(
        &mut self,
        name: &str,
        texture: &'g B::Texture,
        import: ImportTexture,
    ) -> TextureHandle<'g> {
        let rhi = B::texture_desc(texture);
        let index = self.textures.len() as u32;
        self.textures.push(TextureNode {
            name: self.arena.alloc_str(name),
            desc: TextureDesc::from_rhi(rhi),
            version: 0,
            import: Some(Imported {
                object: texture,
                initial: import.initial_state,
                read_only: import.read_only,
                usage: rhi.usage,
            }),
        });
        TextureHandle::new(index, 0)
    }

    /// A buffer the caller owns, starting the frame with no pending GPU access.
    pub fn import_buffer(&mut self, name: &str, buffer: &'g B::Buffer) -> BufferHandle<'g> {
        self.import_buffer_with(name, buffer, ImportBuffer::default())
    }

    pub fn import_buffer_with(
        &mut self,
        name: &str,
        buffer: &'g B::Buffer,
        import: ImportBuffer,
    ) -> BufferHandle<'g> {
        let rhi = B::buffer_desc(buffer);
        let index = self.buffers.len() as u32;
        self.buffers.push(BufferNode {
            name: self.arena.alloc_str(name),
            desc: BufferDesc::new(rhi.size),
            version: 0,
            import: Some(Imported {
                object: buffer,
                initial: import.initial_state,
                read_only: import.read_only,
                usage: rhi.usage,
            }),
        });
        BufferHandle::new(index, 0)
    }

    /// A pass that draws into attachments (`b.color`, `b.depth`). `setup` runs now: it declares
    /// the pass's accesses through the builder and returns the pass's outputs (usually the new
    /// handles) and the closure that records it.
    pub fn add_raster<R, E>(
        &mut self,
        name: &str,
        setup: impl FnOnce(&mut PassBuilder<'_, 'g, B>) -> (R, E),
    ) -> R
    where
        E: FnOnce(&mut PassContext<'_, B>) + Send + 'g,
    {
        self.add_pass(name, PassKind::Raster, setup)
    }

    /// A compute pass; see [`add_raster`](Self::add_raster).
    pub fn add_compute<R, E>(
        &mut self,
        name: &str,
        setup: impl FnOnce(&mut PassBuilder<'_, 'g, B>) -> (R, E),
    ) -> R
    where
        E: FnOnce(&mut PassContext<'_, B>) + Send + 'g,
    {
        self.add_pass(name, PassKind::Compute, setup)
    }

    /// A pass of copies and clears; see [`add_raster`](Self::add_raster).
    pub fn add_copy<R, E>(
        &mut self,
        name: &str,
        setup: impl FnOnce(&mut PassBuilder<'_, 'g, B>) -> (R, E),
    ) -> R
    where
        E: FnOnce(&mut PassContext<'_, B>) + Send + 'g,
    {
        self.add_pass(name, PassKind::Copy, setup)
    }

    fn add_pass<R, E>(
        &mut self,
        name: &str,
        kind: PassKind,
        setup: impl FnOnce(&mut PassBuilder<'_, 'g, B>) -> (R, E),
    ) -> R
    where
        E: FnOnce(&mut PassContext<'_, B>) + Send + 'g,
    {
        let start = self.accesses.len() as u32;
        let index = self.passes.len();
        self.passes.push(PassNode {
            name: self.arena.alloc_str(name),
            kind,
            queue: QueueType::Graphics,
            side_effect: false,
            cost_us: None,
            accesses: start..start,
            colors: ArrayVec::new(),
            depth: None,
            exec: None,
        });
        let arena = self.arena;
        let mut builder = PassBuilder {
            graph: self,
            pass: index,
            written: Vec::new_in(arena),
        };
        let (out, exec) = setup(&mut builder);
        let written = builder.written;
        for res in written {
            match res {
                Res::Texture(i) => self.textures[i as usize].version += 1,
                Res::Buffer(i) => self.buffers[i as usize].version += 1,
            }
        }
        let end = self.accesses.len() as u32;
        let pass = &mut self.passes[index];
        pass.accesses = start..end;
        let exec: Exec<'g, B> = Box::new_in(exec, arena);
        pass.exec = Some(exec);
        out
    }

    /// Passes declared so far.
    pub fn pass_count(&self) -> usize {
        self.passes.len()
    }

    /// The name of the pass declared `index`th.
    pub fn pass_name(&self, index: usize) -> &str {
        self.passes[index].name
    }

    pub fn texture_name(&self, handle: TextureHandle<'g>) -> &str {
        self.textures[handle.index as usize].name
    }

    pub fn buffer_name(&self, handle: BufferHandle<'g>) -> &str {
        self.buffers[handle.index as usize].name
    }

    /// The description of a texture, imported or not.
    pub fn texture_desc(&self, handle: TextureHandle<'g>) -> &TextureDesc {
        &self.textures[handle.index as usize].desc
    }

    pub fn buffer_desc(&self, handle: BufferHandle<'g>) -> &BufferDesc {
        &self.buffers[handle.index as usize].desc
    }

    fn res_name(&self, res: Res) -> &str {
        match res {
            Res::Texture(i) => self.textures[i as usize].name,
            Res::Buffer(i) => self.buffers[i as usize].name,
        }
    }
}

/// Declares one pass's accesses and options, inside the setup closure of
/// [`RenderGraph::add_raster`] and friends.
pub struct PassBuilder<'b, 'g, B: Backend = Rhi> {
    graph: &'b mut RenderGraph<'g, B>,
    pass: usize,
    /// Resources this pass writes: their versions advance when setup ends.
    written: Vec<Res, &'g Arena>,
}

impl<'g, B: Backend> PassBuilder<'_, 'g, B> {
    /// Declares a read; returns the same handle.
    pub fn read_texture(&mut self, h: TextureHandle<'g>, access: Access) -> TextureHandle<'g> {
        assert!(
            !access.is_write(),
            "pass {}: {access:?} writes; declare it with write_texture",
            self.pass_name()
        );
        self.texture_access(h, Use::Shader(access), Effect::Read)
    }

    /// Declares a write (which also reads: storage views keep the old contents) of what `h`
    /// covers. Returns the next version, covering the whole texture: versions count writes to
    /// any part of it, so `hzb = b.write_texture(hzb.mip(i), ..)` keeps a handle to every mip.
    pub fn write_texture(&mut self, h: TextureHandle<'g>, access: Access) -> TextureHandle<'g> {
        assert!(
            access.is_write(),
            "pass {}: {access:?} only reads; declare it with read_texture",
            self.pass_name()
        );
        self.texture_access(h, Use::Shader(access), Effect::Modify)
    }

    pub fn read_buffer(&mut self, h: BufferHandle<'g>, access: Access) -> BufferHandle<'g> {
        assert!(
            !access.is_write(),
            "pass {}: {access:?} writes; declare it with write_buffer",
            self.pass_name()
        );
        self.buffer_access(h, Use::Shader(access), Effect::Read)
    }

    /// Declares a write of what `h` covers; returns the next version, covering the whole buffer.
    pub fn write_buffer(&mut self, h: BufferHandle<'g>, access: Access) -> BufferHandle<'g> {
        assert!(
            access.is_write(),
            "pass {}: {access:?} only reads; declare it with read_buffer",
            self.pass_name()
        );
        self.buffer_access(h, Use::Shader(access), Effect::Modify)
    }

    /// The next color attachment: one mip and layer of `h`. Returns the next version.
    pub fn color(
        &mut self,
        h: TextureHandle<'g>,
        load: LoadOp,
        store: StoreOp,
    ) -> TextureHandle<'g> {
        let effect = match load {
            LoadOp::Load => Effect::Modify,
            LoadOp::Clear(_) | LoadOp::DontCare => Effect::Overwrite,
        };
        let out = self.texture_access(h, Use::Color, effect);
        let (mip, layer) = self.single_subresource(out, "color attachment");
        let pass = &mut self.graph.passes[self.pass];
        assert!(
            pass.colors
                .try_push(ColorTarget {
                    texture: h.index,
                    mip,
                    layer,
                    load,
                    store,
                })
                .is_ok(),
            "pass {}: more than {MAX_COLOR_ATTACHMENTS} color attachments",
            pass.name
        );
        out
    }

    /// The depth attachment, written; the stencil plane (if any) is not kept.
    pub fn depth(
        &mut self,
        h: TextureHandle<'g>,
        load: LoadOp<f32>,
        store: StoreOp,
    ) -> TextureHandle<'g> {
        self.depth_stencil(h, load, store, LoadOp::DontCare, StoreOp::DontCare)
    }

    /// The depth attachment with explicit stencil operations.
    pub fn depth_stencil(
        &mut self,
        h: TextureHandle<'g>,
        load: LoadOp<f32>,
        store: StoreOp,
        stencil_load: LoadOp<u8>,
        stencil_store: StoreOp,
    ) -> TextureHandle<'g> {
        let has_stencil = self.graph.textures[h.index as usize]
            .desc
            .format
            .has_stencil();
        let effect = if matches!(load, LoadOp::Load)
            || has_stencil && matches!(stencil_load, LoadOp::Load)
        {
            Effect::Modify
        } else {
            Effect::Overwrite
        };
        let out = self.texture_access(h, Use::DepthWrite, effect);
        self.set_depth(h, load, store, stencil_load, stencil_store, false);
        out
    }

    /// The depth attachment, read-only (depth testing without writes). Returns the same handle.
    pub fn depth_read(&mut self, h: TextureHandle<'g>) -> TextureHandle<'g> {
        let out = self.texture_access(h, Use::DepthRead, Effect::Read);
        self.set_depth(
            h,
            LoadOp::Load,
            StoreOp::Store,
            LoadOp::Load,
            StoreOp::Store,
            true,
        );
        out
    }

    fn set_depth(
        &mut self,
        h: TextureHandle<'g>,
        load: LoadOp<f32>,
        store: StoreOp,
        stencil_load: LoadOp<u8>,
        stencil_store: StoreOp,
        read_only: bool,
    ) {
        let (mip, layer) = self.single_subresource(h, "depth attachment");
        let pass = &mut self.graph.passes[self.pass];
        assert!(
            pass.depth.is_none(),
            "pass {}: two depth attachments",
            pass.name
        );
        pass.depth = Some(DepthTarget {
            texture: h.index,
            mip,
            layer,
            load,
            store,
            stencil_load,
            stencil_store,
            read_only,
        });
    }

    /// A transient texture, as [`RenderGraph::create_texture`].
    pub fn create_texture(&mut self, name: &str, desc: TextureDesc) -> TextureHandle<'g> {
        self.graph.create_texture(name, desc)
    }

    /// A transient buffer, as [`RenderGraph::create_buffer`].
    pub fn create_buffer(&mut self, name: &str, desc: BufferDesc) -> BufferHandle<'g> {
        self.graph.create_buffer(name, desc)
    }

    /// Runs the pass on the compute queue (0007 §4.3).
    pub fn async_compute(&mut self) {
        let pass = &mut self.graph.passes[self.pass];
        assert!(
            pass.kind == PassKind::Compute,
            "pass {}: only compute passes run on the compute queue",
            pass.name
        );
        pass.queue = QueueType::Compute;
    }

    /// Runs the copy pass on the copy queue.
    pub fn copy_queue(&mut self) {
        let pass = &mut self.graph.passes[self.pass];
        assert!(
            pass.kind == PassKind::Copy,
            "pass {}: only copy passes run on the copy queue",
            pass.name
        );
        pass.queue = QueueType::Copy;
    }

    /// Keeps the pass even when nothing reads what it writes: readbacks, presents, captures.
    pub fn side_effect(&mut self) {
        self.graph.passes[self.pass].side_effect = true;
    }

    /// How long recording the pass takes, for splitting work between command lists before the
    /// pass has been timed.
    pub fn cost(&mut self, microseconds: u32) {
        self.graph.passes[self.pass].cost_us = Some(microseconds);
    }

    fn pass_name(&self) -> &str {
        self.graph.passes[self.pass].name
    }

    fn single_subresource(&self, h: TextureHandle<'g>, what: &str) -> (u32, u32) {
        let node = &self.graph.textures[h.index as usize];
        let range = h.range.resolve(node.desc.mip_levels, node.desc.layers());
        assert!(
            range.mip_count == 1 && range.layer_count == 1,
            "pass {}: the {what} {} must be one mip and one layer",
            self.pass_name(),
            node.name
        );
        (range.base_mip, range.base_layer)
    }

    fn texture_access(
        &mut self,
        h: TextureHandle<'g>,
        usage: Use,
        effect: Effect,
    ) -> TextureHandle<'g> {
        let node = &self.graph.textures[h.index as usize];
        let range = h.range.resolve(node.desc.mip_levels, node.desc.layers());
        let res = Res::Texture(h.index);
        self.validate(res, h.version, usage, effect);
        assert!(
            usage.texture_state().is_some(),
            "pass {}: textures have no {} access ({})",
            self.pass_name(),
            usage.name(),
            node.name
        );
        if cfg!(debug_assertions)
            && let Some(import) = &node.import
        {
            assert!(
                import.usage.contains(usage.texture_usage()),
                "pass {}: {} access to {}, which was created without {:?}",
                self.pass_name(),
                usage.name(),
                node.name,
                usage.texture_usage()
            );
        }
        self.push(AccessRec {
            res,
            range,
            bytes: (0, u64::MAX),
            usage,
            effect,
        });
        if effect.writes() {
            TextureHandle::new(h.index, h.version + 1)
        } else {
            h
        }
    }

    fn buffer_access(
        &mut self,
        h: BufferHandle<'g>,
        usage: Use,
        effect: Effect,
    ) -> BufferHandle<'g> {
        let node = &self.graph.buffers[h.index as usize];
        let size = if h.size == u64::MAX {
            node.desc.size - h.offset.min(node.desc.size)
        } else {
            h.size
        };
        assert!(
            h.offset
                .checked_add(size)
                .is_some_and(|end| end <= node.desc.size),
            "pass {}: range {}..+{size} outside buffer {} of {} bytes",
            self.pass_name(),
            h.offset,
            node.name,
            node.desc.size
        );
        let res = Res::Buffer(h.index);
        self.validate(res, h.version, usage, effect);
        assert!(
            usage.buffer_state().is_some(),
            "pass {}: buffers have no {} access ({})",
            self.pass_name(),
            usage.name(),
            node.name
        );
        if cfg!(debug_assertions)
            && let Some(import) = &node.import
        {
            assert!(
                import.usage.contains(usage.buffer_usage()),
                "pass {}: {} access to {}, which was created without {:?}",
                self.pass_name(),
                usage.name(),
                node.name,
                usage.buffer_usage()
            );
        }
        self.push(AccessRec {
            res,
            range: Range::ALL,
            bytes: (h.offset, size),
            usage,
            effect,
        });
        if effect.writes() {
            BufferHandle::new(h.index, h.version + 1)
        } else {
            h
        }
    }

    /// Development checks of one access (0007 §10.2). Misuse panics with the pass and resource.
    fn validate(&self, res: Res, version: u32, usage: Use, effect: Effect) {
        if !cfg!(debug_assertions) {
            return;
        }
        let g = &*self.graph;
        let pass = &g.passes[self.pass];
        let name = g.res_name(res);
        let (current, import_read_only, transient) = match res {
            Res::Texture(i) => {
                let n = &g.textures[i as usize];
                (
                    n.version,
                    n.import.as_ref().is_some_and(|i| i.read_only),
                    n.import.is_none(),
                )
            }
            Res::Buffer(i) => {
                let n = &g.buffers[i as usize];
                (
                    n.version,
                    n.import.as_ref().is_some_and(|i| i.read_only),
                    n.import.is_none(),
                )
            }
        };
        assert!(
            version == current,
            "pass {}: {name} used at version {version}, but version {current} replaced it; use the \
             handle the last write returned",
            pass.name
        );
        assert!(
            usage.allowed_in(pass.kind),
            "pass {}: a {:?} pass cannot use {name} as {}",
            pass.name,
            pass.kind,
            usage.name()
        );
        assert!(
            !(effect.writes() && import_read_only),
            "pass {}: writes {name}, imported read-only",
            pass.name
        );
        assert!(
            !(transient && version == 0 && effect == Effect::Read),
            "pass {}: reads {name} before any pass wrote it",
            pass.name
        );
    }

    fn push(&mut self, rec: AccessRec) {
        let g = &mut *self.graph;
        let start = g.passes[self.pass].accesses.start as usize;
        if cfg!(debug_assertions) {
            for prior in &g.accesses[start..] {
                if prior.res != rec.res || !overlaps(prior, &rec) {
                    continue;
                }
                let name = g.res_name(rec.res);
                let pass = g.passes[self.pass].name;
                assert!(
                    !prior.effect.writes() && !rec.effect.writes(),
                    "pass {pass}: declares {name} as {} and as {}; a write already reads, \
                     declare it once",
                    prior.usage.name(),
                    rec.usage.name()
                );
                if let (Res::Texture(_), Some(a), Some(b)) = (
                    rec.res,
                    prior.usage.texture_state(),
                    rec.usage.texture_state(),
                ) {
                    assert!(
                        a.layout == b.layout,
                        "pass {pass}: reads {name} as {} and as {}, which need different layouts",
                        prior.usage.name(),
                        rec.usage.name()
                    );
                }
            }
        }
        if rec.effect.writes() && !self.written.contains(&rec.res) {
            self.written.push(rec.res);
        }
        g.accesses.push(rec);
    }
}

/// Whether two accesses of the same resource touch common subresources (buffers: always).
pub(crate) fn overlaps(a: &AccessRec, b: &AccessRec) -> bool {
    match a.res {
        Res::Buffer(_) => true,
        Res::Texture(_) => {
            let (ra, rb) = (a.range, b.range);
            ra.base_mip < rb.base_mip + rb.mip_count
                && rb.base_mip < ra.base_mip + ra.mip_count
                && ra.base_layer < rb.base_layer + rb.layer_count
                && rb.base_layer < ra.base_layer + ra.layer_count
        }
    }
}
