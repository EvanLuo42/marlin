//! Render graph: resource lifetimes, barriers and async compute scheduling
//! (`docs/design/0007-render-graph.md`).
//!
//! Every frame declares one graph from scratch in the frame arena: transient resources the graph
//! creates and imported ones the caller owns, and passes that declare how they access them
//! through a builder and return a closure that records them. Declaration order is the logical
//! execution order, as in a single-threaded program. Compiling the graph only makes changes that
//! cannot be observed:
//!
//! - **culling**: passes whose results nobody reads (side-effect passes and writes to imported
//!   resources are what the frame shows);
//! - **aliasing**: transients that are never alive at the same time share memory;
//! - **barriers**: derived per subresource from the accesses; consecutive reads share one state.
//!
//! A compiled plan is cached by the graph's topology key, so a frame whose structure did not
//! change skips compiling.
//!
//! What runs the plan (the frame context with its heaps and upload ring), the async compute and
//! copy queues and parallel recording come in the next steps of 0007.
//!
//! The graph is generic over a [`Backend`]: [`Rhi`] in the engine, [`recording::Recording`] in
//! tests that run without a GPU.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod access;
mod alias;
mod backend;
mod cache;
mod compile;
mod context;
mod desc;
mod graph;
mod handle;
mod key;
mod plan;
pub mod recording;

pub use access::{Access, PassKind};
pub use backend::{Backend, Rhi};
pub use cache::{Lookup, PlanCache};
pub use context::PassContext;
pub use desc::{BufferDesc, TextureDesc};
pub use graph::{ImportBuffer, ImportTexture, PassBuilder, RenderGraph};
pub use handle::{BufferHandle, TextureHandle};
pub use plan::{
    BufferTransition, CompiledPlan, Lifetime, Placement, PlannedBuffer, PlannedPass,
    PlannedTexture, TextureFinal, TextureTransition,
};

impl<B: Backend> RenderGraph<'_, B> {
    /// The hash of everything compiling depends on: two graphs with the same key compile to the
    /// same plan.
    pub fn topology_key(&self) -> u64 {
        key::topology_key(self)
    }

    /// Compiles the graph without the cache.
    pub fn compile(&self, device: &B::Device) -> marlin_rhi::Result<CompiledPlan> {
        compile::compile(self, device, self.topology_key())
    }
}
