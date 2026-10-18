//! A compiled graph: what runs, in which order, with which barriers and memory (0007 §5.2).
//! Plain data that outlives the frame, so the plan cache can reuse it while the topology holds.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use marlin_rhi::{
    BufferState, BufferUsage, HeapKind, Layout, QueueType, SubresourceRange, TextureState,
    TextureUsage,
};

use crate::access::PassKind;
use crate::handle::{BufferHandle, TextureHandle};

/// A pass that survived culling.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlannedPass {
    /// Its position in declaration order.
    pub node: u32,
    pub name: String,
    pub kind: PassKind,
    pub queue: QueueType,
    pub(crate) texture_barriers: Range<u32>,
    pub(crate) buffer_barriers: Range<u32>,
}

/// From the first to the last pass using a resource, as positions in [`CompiledPlan::passes`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Lifetime {
    pub first: u32,
    pub last: u32,
}

impl Lifetime {
    pub const fn overlaps(self, other: Self) -> bool {
        self.first <= other.last && other.first <= self.last
    }
}

/// Where a transient lives in the frame slot's transient heaps.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Placement {
    pub heap: HeapKind,
    pub offset: u64,
    pub size: u64,
    pub alignment: u64,
}

impl Placement {
    pub const fn end(&self) -> u64 {
        self.offset + self.size
    }

    pub(crate) const fn overlaps(&self, other: &Self) -> bool {
        self.heap as u32 == other.heap as u32
            && self.offset < other.end()
            && other.offset < self.end()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlannedTexture {
    /// Every way the retained passes use it. For imports, what this frame needs of the
    /// object's usage.
    pub usage: TextureUsage,
    /// `None` when no retained pass uses it.
    pub lifetime: Option<Lifetime>,
    /// Transients that are used.
    pub placement: Option<Placement>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlannedBuffer {
    pub usage: BufferUsage,
    pub lifetime: Option<Lifetime>,
    pub placement: Option<Placement>,
}

/// A texture barrier before a pass, over one range of subresources.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureTransition {
    /// The texture's slot ([`TextureHandle::index`]).
    pub texture: u32,
    pub range: SubresourceRange,
    pub before: TextureState,
    pub after: TextureState,
    /// With `before.layout` [`Layout::Undefined`]: the layout the range is really in, for legacy
    /// D3D12 ([`marlin_rhi::TextureBarrier::undefined_from`]). For a transient's first use it is
    /// the layout the plan leaves it in, which is where the same placed texture stands when the
    /// plan runs again; a placed texture that is new is in [`Layout::Common`] instead.
    pub undefined_from: Layout,
}

/// A buffer barrier before a pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferTransition {
    pub buffer: u32,
    pub before: BufferState,
    pub after: BufferState,
}

/// The state an imported texture's subresources end the frame in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureFinal {
    pub texture: u32,
    pub range: SubresourceRange,
    pub state: TextureState,
}

/// The output of compiling a graph.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompiledPlan {
    pub(crate) key: u64,
    pub(crate) passes: Vec<PlannedPass>,
    pub(crate) culled: Vec<u32>,
    pub(crate) edges: Vec<(u32, u32)>,
    pub(crate) textures: Vec<PlannedTexture>,
    pub(crate) buffers: Vec<PlannedBuffer>,
    pub(crate) texture_barriers: Vec<TextureTransition>,
    pub(crate) buffer_barriers: Vec<BufferTransition>,
    pub(crate) texture_finals: Vec<TextureFinal>,
    pub(crate) buffer_finals: Vec<(u32, BufferState)>,
    /// Bytes of each transient heap kind: buffers, textures, targets.
    pub(crate) heap_sizes: [u64; 3],
}

impl CompiledPlan {
    /// The topology key it was compiled for.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// The retained passes in execution order.
    pub fn passes(&self) -> &[PlannedPass] {
        &self.passes
    }

    /// Declaration positions of the passes culling removed.
    pub fn culled(&self) -> &[u32] {
        &self.culled
    }

    /// Dependencies between retained passes (positions in [`passes`](Self::passes)), from
    /// earlier to later: read after write, write after read and write after write, each pair
    /// once, sorted.
    pub fn edges(&self) -> &[(u32, u32)] {
        &self.edges
    }

    pub fn texture(&self, handle: TextureHandle<'_>) -> &PlannedTexture {
        &self.textures[handle.index as usize]
    }

    pub fn buffer(&self, handle: BufferHandle<'_>) -> &PlannedBuffer {
        &self.buffers[handle.index as usize]
    }

    /// The texture barriers recorded before retained pass `pass`.
    pub fn texture_barriers(&self, pass: usize) -> &[TextureTransition] {
        let r = &self.passes[pass].texture_barriers;
        &self.texture_barriers[r.start as usize..r.end as usize]
    }

    /// The buffer barriers recorded before retained pass `pass`.
    pub fn buffer_barriers(&self, pass: usize) -> &[BufferTransition] {
        let r = &self.passes[pass].buffer_barriers;
        &self.buffer_barriers[r.start as usize..r.end as usize]
    }

    /// Where imported textures end the frame (only subresources the frame touched).
    pub fn texture_finals(&self) -> &[TextureFinal] {
        &self.texture_finals
    }

    /// Where imported buffers the frame touched end it.
    pub fn buffer_finals(&self) -> &[(u32, BufferState)] {
        &self.buffer_finals
    }

    /// Bytes the transient heap of `kind` needs (`HeapKind::Any` is not used).
    pub fn heap_size(&self, kind: HeapKind) -> u64 {
        match kind {
            HeapKind::Buffers => self.heap_sizes[0],
            HeapKind::Textures => self.heap_sizes[1],
            HeapKind::Targets => self.heap_sizes[2],
            HeapKind::Any => 0,
        }
    }
}
