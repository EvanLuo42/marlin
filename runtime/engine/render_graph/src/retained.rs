//! History and persistent resources (0007 §8): resources the graph owns across frames.
//!
//! In a frame's graph they are like imports: not placed, not aliased, their contents observable
//! after the frame, their states carried in the [`StateTable`]. The difference is who owns the
//! object: the graph's backend side (the frame context) creates it the first time a frame uses
//! it, keeps it while the state table keeps its entry, and replaces it when its description
//! changes or a frame needs a usage it was not made with.
//!
//! Usage: every object gets the frame's usage plus a base set (textures: sampled and copies;
//! buffers: every ordinary usage),
//! so a history written one way and read back in the next frame does not need a new object. A
//! frame that needs more (written as storage after being a render target, say) gets a new object
//! with the union, and its contents start undefined: a development-time event, since a frame's
//! way of using its history rarely changes.

use alloc::string::{String, ToString};

use marlin_hash::fast_hash;
use marlin_rhi::{BufferState, BufferUsage, TextureState, TextureUsage};

use crate::backend::Backend;
use crate::desc::{BufferDesc, TextureDesc};
use crate::frame::FRAMES_IN_FLIGHT;
use crate::graph::{BufferNode, Imported, RenderGraph, Res, Source, TextureNode};
use crate::handle::{BufferHandle, TextureHandle};
use crate::state::StateTable;

/// The scope of resources the whole frame shares, rather than one view's.
pub const GLOBAL_SCOPE: u64 = u64::MAX;

/// Frames a history or persistent resource survives without being requested.
pub const RETIRE_FRAMES: u64 = 2 * FRAMES_IN_FLIGHT as u64 + 2;

/// Usage every graph-owned texture gets on top of what the frame needs.
pub(crate) const TEXTURE_BASE: TextureUsage = TextureUsage::SAMPLED
    .union(TextureUsage::COPY_SRC)
    .union(TextureUsage::COPY_DST);
/// Usage every graph-owned buffer gets on top of what the frame needs: every ordinary one,
/// since buffer usage costs nothing (no compression or layout depends on it).
pub(crate) const BUFFER_BASE: BufferUsage = BufferUsage::SHADER_READ
    .union(BufferUsage::SHADER_WRITE)
    .union(BufferUsage::UNIFORM)
    .union(BufferUsage::INDIRECT)
    .union(BufferUsage::INDEX)
    .union(BufferUsage::COPY_SRC)
    .union(BufferUsage::COPY_DST);

/// A history resource this frame: what the last frame that requested it wrote, and what this
/// frame writes for the next one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct History<H> {
    /// `None` the first time, after the description changed, or when the view's history was
    /// invalidated this frame ([`RenderGraph::invalidate_history`]).
    pub prev: Option<H>,
    pub cur: H,
}

/// A persistent resource this frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Persistent<H> {
    pub handle: H,
    /// Its contents are new: the resource was just created, or its description changed.
    pub fresh: bool,
}

/// A description of a graph-owned resource: [`TextureDesc`] or [`BufferDesc`].
pub trait RetainedDesc: Copy + sealed::Sealed {
    type Handle<'g>: Copy;
    #[doc(hidden)]
    fn kind(self) -> RetainedKind;
    #[doc(hidden)]
    /// The handle of the texture or buffer in slot `index`.
    fn handle<'g>(index: u32) -> Self::Handle<'g>;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::TextureDesc {}
    impl Sealed for crate::BufferDesc {}
}

impl RetainedDesc for TextureDesc {
    type Handle<'g> = TextureHandle<'g>;

    fn kind(self) -> RetainedKind {
        RetainedKind::Texture(self)
    }

    fn handle<'g>(index: u32) -> TextureHandle<'g> {
        TextureHandle::new(index, 0)
    }
}

impl RetainedDesc for BufferDesc {
    type Handle<'g> = BufferHandle<'g>;

    fn kind(self) -> RetainedKind {
        RetainedKind::Buffer(self)
    }

    fn handle<'g>(index: u32) -> BufferHandle<'g> {
        BufferHandle::new(index, 0)
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RetainedKind {
    Texture(TextureDesc),
    Buffer(BufferDesc),
}

/// One request of a graph-owned resource in a frame: one object slot of it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetainedUse<'g> {
    pub key: u64,
    pub name: &'g str,
    pub scope: u64,
    pub history: bool,
    /// Which of the two objects (persistent resources have only 0).
    pub slot: u8,
    /// The object this frame writes for the next one (a history's `cur`; persistent's only).
    pub cur: bool,
    /// The registry entry continues (same description and kind) rather than starts over.
    pub same: bool,
    pub kind: RetainedKind,
    pub res: Res,
}

/// What the state table knows of one object slot.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct SlotEntry {
    /// What its object was made with: the frame context makes it with exactly this.
    pub texture_usage: TextureUsage,
    pub buffer_usage: BufferUsage,
    /// Where the last frame that used it left it.
    pub texture_states: Option<alloc::vec::Vec<TextureState>>,
    pub buffer_state: Option<BufferState>,
}

/// The state table's entry for one graph-owned resource.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct RetainedEntry {
    pub name: String,
    pub scope: u64,
    pub history: bool,
    pub kind: RetainedKind,
    /// The slot the last frame that used the resource wrote.
    pub last_cur: Option<u8>,
    pub slots: [SlotEntry; 2],
    /// The last frame that requested it.
    pub frame: u64,
}

impl RetainedEntry {
    pub(crate) fn new(u: &RetainedUse<'_>) -> Self {
        Self {
            name: u.name.to_string(),
            scope: u.scope,
            history: u.history,
            kind: u.kind,
            last_cur: None,
            slots: Default::default(),
            frame: 0,
        }
    }
}

/// The texture or buffer slot of `res`.
const fn slot_of(res: Res) -> u32 {
    match res {
        Res::Texture(i) | Res::Buffer(i) => i,
    }
}

pub(crate) fn retained_key(name: &str, scope: u64) -> u64 {
    fast_hash(&(name, scope))
}

impl<'g, B: Backend> RenderGraph<'g, B> {
    /// A double-buffered resource keyed by (`name`, `view`): `cur` is written this frame and is
    /// `prev` the next frame that requests it. Requires a graph with a state table.
    pub fn history<D: RetainedDesc>(
        &mut self,
        name: &str,
        view: u64,
        desc: D,
    ) -> History<D::Handle<'g>> {
        let key = retained_key(name, view);
        let kind = desc.kind();
        let entry = self.retained_entry(key, name, view);
        let same = entry.is_some_and(|e| e.history && e.kind == kind);
        let last = if same {
            entry.and_then(|e| e.last_cur)
        } else {
            None
        };
        let cur_slot = last.map_or(0, |s| 1 - s);
        let prev = match last {
            Some(slot) if !self.invalidated.contains(&view) => Some(D::handle(slot_of(
                self.retained_node(key, name, view, true, slot, false, same, kind),
            ))),
            _ => None,
        };
        let cur = D::handle(slot_of(
            self.retained_node(key, name, view, true, cur_slot, true, same, kind),
        ));
        History { prev, cur }
    }

    /// A single resource keyed by (`name`, `scope`) whose contents last across frames:
    /// caches, accumulation buffers. Use [`GLOBAL_SCOPE`] for one the whole frame shares.
    /// Requires a graph with a state table.
    pub fn persistent<D: RetainedDesc>(
        &mut self,
        name: &str,
        scope: u64,
        desc: D,
    ) -> Persistent<D::Handle<'g>> {
        let key = retained_key(name, scope);
        let kind = desc.kind();
        let entry = self.retained_entry(key, name, scope);
        let same = entry.is_some_and(|e| !e.history && e.kind == kind);
        let fresh = !same || entry.is_some_and(|e| e.last_cur.is_none());
        let res = self.retained_node(key, name, scope, false, 0, true, same, kind);
        Persistent {
            handle: D::handle(slot_of(res)),
            fresh,
        }
    }

    /// This frame continues no history of `view` (a camera cut): its histories requested after
    /// this have no `prev`.
    pub fn invalidate_history(&mut self, view: u64) {
        if !self.invalidated.contains(&view) {
            self.invalidated.push(view);
        }
    }

    fn retained_entry(&self, key: u64, name: &str, scope: u64) -> Option<&'g RetainedEntry> {
        let states: &'g StateTable<B> = self
            .states
            .expect("history and persistent resources need a graph with a state table");
        assert!(
            !self.retained.iter().any(|u| u.key == key),
            "{name} (scope {scope}) requested twice in one frame"
        );
        states.retained(key)
    }

    #[allow(clippy::too_many_arguments)]
    fn retained_node(
        &mut self,
        key: u64,
        name: &str,
        scope: u64,
        history: bool,
        slot: u8,
        cur: bool,
        same: bool,
        kind: RetainedKind,
    ) -> Res {
        let entry = self
            .states
            .and_then(|s| s.retained(key))
            .filter(|_| same)
            .map(|e| &e.slots[slot as usize]);
        let name = self.arena.alloc_str(name);
        let index_of_use = self.retained.len() as u32;
        let res = match kind {
            RetainedKind::Texture(desc) => {
                let n = desc.subresources() as usize;
                let initial: &'g [TextureState] =
                    match entry.and_then(|e| e.texture_states.as_ref()) {
                        Some(states) if states.len() == n => self.arena.alloc_slice_copy(states),
                        _ => self.arena.alloc_slice_copy(&[TextureState::UNDEFINED]),
                    };
                let index = self.textures.len() as u32;
                self.textures.push(TextureNode {
                    name,
                    desc,
                    version: 0,
                    import: Some(Imported {
                        object: Source::Retained(index_of_use),
                        initial,
                        read_only: false,
                        usage: entry.map_or(TextureUsage::default(), |e| e.texture_usage),
                    }),
                });
                Res::Texture(index)
            }
            RetainedKind::Buffer(desc) => {
                assert!(desc.size > 0, "buffer {name} is empty");
                let index = self.buffers.len() as u32;
                self.buffers.push(BufferNode {
                    name,
                    desc,
                    version: 0,
                    import: Some(Imported {
                        object: Source::Retained(index_of_use),
                        initial: entry
                            .and_then(|e| e.buffer_state)
                            .unwrap_or(BufferState::NONE),
                        read_only: false,
                        usage: entry.map_or(BufferUsage::default(), |e| e.buffer_usage),
                    }),
                });
                Res::Buffer(index)
            }
        };
        self.retained.push(RetainedUse {
            key,
            name,
            scope,
            history,
            slot,
            cur,
            same,
            kind,
            res,
        });
        res
    }
}
