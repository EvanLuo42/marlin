//! The cross-frame state table: where each imported object stood at the end of the last frame
//! that used it, so imports need not report their state (0007 §2.3); and the graph's own
//! history and persistent resources (0007 §8), with their objects' usage and states.

use alloc::vec::Vec;
use core::marker::PhantomData;

use marlin_containers::{HashMap, HashMapExt};
use marlin_rhi::{BufferState, BufferUsage, TextureState, TextureUsage};

use crate::backend::{Backend, Rhi};
use crate::graph::{RenderGraph, Res, Source};
use crate::plan::CompiledPlan;
use crate::retained::{BUFFER_BASE, RETIRE_FRAMES, RetainedEntry, RetainedUse, TEXTURE_BASE};

struct Entry<S> {
    state: S,
    /// The last frame that used the object.
    frame: u64,
}

/// States of imported objects by object key ([`Backend::texture_key`]), and the graph's history
/// and persistent resources.
///
/// An import's entry lives until its object is forgotten, or until it has gone unused for
/// [`TTL`](Self::TTL) frames. Forget an object before dropping it: a new object may reuse its
/// native handle, and would then start in the old one's state. A graph-owned resource's entry
/// lives until it has gone [`RETIRE_FRAMES`] frames without being requested.
pub struct StateTable<B: Backend = Rhi> {
    /// One state per subresource.
    textures: HashMap<u64, Entry<Vec<TextureState>>>,
    buffers: HashMap<u64, Entry<BufferState>>,
    retained: HashMap<u64, RetainedEntry>,
    frame: u64,
    _backend: PhantomData<fn() -> B>,
}

/// One object slot of a graph-owned resource, as a frame left it.
#[derive(Clone, PartialEq, Debug)]
struct RetainedUpdate {
    key: u64,
    /// The request that made it, for a new entry.
    entry: RetainedEntry,
    same: bool,
    slot: u8,
    cur: bool,
    /// A retained pass used it; the rest is only set then.
    used: bool,
    texture_usage: TextureUsage,
    buffer_usage: BufferUsage,
    texture_states: Option<Vec<TextureState>>,
    buffer_state: Option<BufferState>,
}

/// What a frame leaves the table to record, taken from the graph and plan so the table can be
/// updated once the graph (which borrows it) is gone.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct FrameStates {
    textures: Vec<(u64, Vec<TextureState>)>,
    buffers: Vec<(u64, BufferState)>,
    retained: Vec<RetainedUpdate>,
}

impl<B: Backend> StateTable<B> {
    /// Frames an import's entry survives unused. Long: an object imported now and then must
    /// keep its state, and stale entries are cheap.
    pub const TTL: u64 = 1024;

    pub fn new() -> Self {
        Self {
            textures: HashMap::new(),
            buffers: HashMap::new(),
            retained: HashMap::new(),
            frame: 0,
            _backend: PhantomData,
        }
    }

    /// Frames ended so far: the number of the frame being declared.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// The states of a texture's subresources (layer-major), if the table knows it.
    pub fn texture_states(&self, key: u64) -> Option<&[TextureState]> {
        self.textures.get(&key).map(|e| e.state.as_slice())
    }

    pub fn buffer_state(&self, key: u64) -> Option<BufferState> {
        self.buffers.get(&key).map(|e| e.state)
    }

    pub fn forget_texture(&mut self, texture: &B::Texture) {
        self.textures.remove(&B::texture_key(texture));
    }

    pub fn forget_buffer(&mut self, buffer: &B::Buffer) {
        self.buffers.remove(&B::buffer_key(buffer));
    }

    pub(crate) fn retained(&self, key: u64) -> Option<&RetainedEntry> {
        self.retained.get(&key)
    }

    /// Whether the table holds the graph-owned resource (`name`, `scope`).
    pub fn has_retained(&self, name: &str, scope: u64) -> bool {
        self.retained
            .contains_key(&crate::retained::retained_key(name, scope))
    }

    /// The usage the object of slot `slot` of graph-owned resource `key` is made with, if any.
    pub(crate) fn retained_usage(&self, key: u64, slot: u8) -> (TextureUsage, BufferUsage) {
        self.retained.get(&key).map_or_else(Default::default, |e| {
            let s = &e.slots[slot as usize];
            (s.texture_usage, s.buffer_usage)
        })
    }

    /// Records where the frame left its imports and graph-owned resources and moves to the
    /// next frame; drops entries unused for too long.
    pub fn end_frame(&mut self, states: FrameStates) {
        let frame = self.frame;
        for (key, state) in states.textures {
            self.textures.insert(key, Entry { state, frame });
        }
        for (key, state) in states.buffers {
            self.buffers.insert(key, Entry { state, frame });
        }
        for u in states.retained {
            let entry = match self.retained.get_mut(&u.key) {
                Some(e) if u.same => e,
                _ => {
                    self.retained.insert(u.key, u.entry);
                    self.retained.get_mut(&u.key).expect("just inserted")
                }
            };
            entry.frame = frame;
            if !u.used {
                continue;
            }
            let slot = &mut entry.slots[u.slot as usize];
            slot.texture_usage |= u.texture_usage;
            slot.buffer_usage |= u.buffer_usage;
            slot.texture_states = u.texture_states;
            slot.buffer_state = u.buffer_state;
            if u.cur {
                entry.last_cur = Some(u.slot);
            }
        }
        self.textures.retain(|_, e| frame - e.frame < Self::TTL);
        self.buffers.retain(|_, e| frame - e.frame < Self::TTL);
        self.retained.retain(|_, e| frame - e.frame < RETIRE_FRAMES);
        self.frame += 1;
    }

    /// Imported objects and graph-owned resources the table knows.
    pub fn len(&self) -> usize {
        self.textures.len() + self.buffers.len() + self.retained.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<B: Backend> Default for StateTable<B> {
    fn default() -> Self {
        Self::new()
    }
}

/// The usage a graph-owned object of a slot that had `stored` is made with for a frame that
/// needs `planned`: unchanged when it has enough, else the union with the base set.
pub(crate) fn texture_object_usage(stored: TextureUsage, planned: TextureUsage) -> TextureUsage {
    if stored.contains(planned) && !stored.is_empty() {
        stored
    } else {
        stored | planned | TEXTURE_BASE
    }
}

pub(crate) fn buffer_object_usage(stored: BufferUsage, planned: BufferUsage) -> BufferUsage {
    if stored.contains(planned) && !stored.is_empty() {
        stored
    } else {
        stored | planned | BUFFER_BASE
    }
}

impl<B: Backend> RenderGraph<'_, B> {
    /// What `plan` (compiled from this graph) leaves for the state table: the states the frame
    /// started its imports and graph-owned resources in, overwritten by the last access of each
    /// subresource it touched; and the usage graph-owned objects are made with.
    pub fn frame_states(&self, plan: &CompiledPlan) -> FrameStates {
        let mut out = FrameStates::default();
        for (t, node) in self.textures.iter().enumerate() {
            let Some(import) = &node.import else { continue };
            let planned = &plan.textures[t];
            if planned.lifetime.is_none() {
                continue;
            }
            let n = node.desc.subresources() as usize;
            // A new graph-owned object starts undefined (as compiling assumed).
            let renewed = matches!(import.object, Source::Retained(_))
                && !import.usage.contains(planned.usage);
            let mut states: Vec<TextureState> = match import.initial.len() {
                _ if renewed => alloc::vec![TextureState::UNDEFINED; n],
                1 => alloc::vec![import.initial[0]; n],
                _ => import.initial.to_vec(),
            };
            let mips = node.desc.mip_levels;
            for f in plan.texture_finals.iter().filter(|f| f.texture == t as u32) {
                for layer in f.range.base_layer..f.range.base_layer + f.range.layer_count {
                    for mip in f.range.base_mip..f.range.base_mip + f.range.mip_count {
                        states[(layer * mips + mip) as usize] = f.state;
                    }
                }
            }
            if let Source::Object(object) = import.object {
                out.textures.push((B::texture_key(object), states));
            } else {
                self.retained_states(&mut out, Res::Texture(t as u32), Some(states), None, plan);
            }
        }
        for &(b, state) in &plan.buffer_finals {
            let import = self.buffers[b as usize]
                .import
                .as_ref()
                .expect("finals are of imports");
            match import.object {
                Source::Object(object) => out.buffers.push((B::buffer_key(object), state)),
                Source::Retained(_) => {
                    self.retained_states(&mut out, Res::Buffer(b), None, Some(state), plan)
                }
            }
        }
        // Requested but unused: the entry is kept alive, nothing else changes.
        for u in &self.retained {
            if !out
                .retained
                .iter()
                .any(|r| r.key == u.key && r.slot == u.slot)
            {
                out.retained.push(update(u, false, plan));
            }
        }
        out
    }

    fn retained_states(
        &self,
        out: &mut FrameStates,
        res: Res,
        texture_states: Option<Vec<TextureState>>,
        buffer_state: Option<BufferState>,
        plan: &CompiledPlan,
    ) {
        let u = self
            .retained
            .iter()
            .find(|u| u.res == res)
            .expect("graph-owned resources are requested");
        let mut r = update(u, true, plan);
        r.texture_states = texture_states;
        r.buffer_state = buffer_state;
        out.retained.push(r);
    }
}

fn update(u: &RetainedUse<'_>, used: bool, plan: &CompiledPlan) -> RetainedUpdate {
    let (texture_usage, buffer_usage) = match u.res {
        Res::Texture(t) => (
            texture_object_usage(TextureUsage::default(), plan.textures[t as usize].usage),
            BufferUsage::default(),
        ),
        Res::Buffer(b) => (
            TextureUsage::default(),
            buffer_object_usage(BufferUsage::default(), plan.buffers[b as usize].usage),
        ),
    };
    RetainedUpdate {
        key: u.key,
        entry: RetainedEntry::new(u),
        same: u.same,
        slot: u.slot,
        cur: u.cur,
        used,
        texture_usage,
        buffer_usage,
        texture_states: None,
        buffer_state: None,
    }
}
