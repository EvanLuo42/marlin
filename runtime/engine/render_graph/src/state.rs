//! The cross-frame state table: where each imported object stood at the end of the last frame
//! that used it, so imports need not report their state (0007 §2.3).

use alloc::vec::Vec;
use core::marker::PhantomData;

use marlin_containers::{HashMap, HashMapExt};
use marlin_rhi::{BufferState, TextureState};

use crate::backend::{Backend, Rhi};
use crate::graph::RenderGraph;
use crate::plan::CompiledPlan;

struct Entry<S> {
    state: S,
    /// The last frame that used the object.
    frame: u64,
}

/// States of imported objects by object key ([`Backend::texture_key`]).
///
/// An entry lives until its object is forgotten, or until it has gone unused for
/// [`TTL`](Self::TTL) frames. Forget an object before dropping it: a new object may reuse its
/// native handle, and would then start in the old one's state.
pub struct StateTable<B: Backend = Rhi> {
    /// One state per subresource.
    textures: HashMap<u64, Entry<Vec<TextureState>>>,
    buffers: HashMap<u64, Entry<BufferState>>,
    frame: u64,
    _backend: PhantomData<fn() -> B>,
}

/// What a frame leaves its imports in, taken from the graph and plan so the table can be
/// updated once the graph (which borrows it) is gone.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ImportStates {
    textures: Vec<(u64, Vec<TextureState>)>,
    buffers: Vec<(u64, BufferState)>,
}

impl<B: Backend> StateTable<B> {
    /// Frames an entry survives unused. Long: an object imported now and then must keep its
    /// state, and stale entries are cheap.
    pub const TTL: u64 = 1024;

    pub fn new() -> Self {
        Self {
            textures: HashMap::new(),
            buffers: HashMap::new(),
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

    /// Records where the frame left its imports and moves to the next frame; drops entries
    /// unused for [`TTL`](Self::TTL) frames.
    pub fn end_frame(&mut self, states: ImportStates) {
        for (key, state) in states.textures {
            self.textures.insert(
                key,
                Entry {
                    state,
                    frame: self.frame,
                },
            );
        }
        for (key, state) in states.buffers {
            self.buffers.insert(
                key,
                Entry {
                    state,
                    frame: self.frame,
                },
            );
        }
        let frame = self.frame;
        self.textures.retain(|_, e| frame - e.frame < Self::TTL);
        self.buffers.retain(|_, e| frame - e.frame < Self::TTL);
        self.frame += 1;
    }

    /// Objects the table knows.
    pub fn len(&self) -> usize {
        self.textures.len() + self.buffers.len()
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

impl<B: Backend> RenderGraph<'_, B> {
    /// Where `plan` (compiled from this graph) leaves the graph's imports: the states the frame
    /// started with, overwritten by the last access of each subresource it touched.
    pub fn import_states(&self, plan: &CompiledPlan) -> ImportStates {
        let mut out = ImportStates::default();
        for (t, node) in self.textures.iter().enumerate() {
            let Some(import) = &node.import else { continue };
            if plan.textures[t].lifetime.is_none() {
                continue;
            }
            let n = node.desc.subresources() as usize;
            let mut states: Vec<TextureState> = match import.initial.len() {
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
            out.textures.push((B::texture_key(import.object), states));
        }
        for &(b, state) in &plan.buffer_finals {
            let import = self.buffers[b as usize]
                .import
                .as_ref()
                .expect("finals are of imports");
            out.buffers.push((B::buffer_key(import.object), state));
        }
        out
    }
}
