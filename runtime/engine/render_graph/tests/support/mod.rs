//! Graphs over the recording backend, and shorthands for passes that only declare accesses.

use marlin_render_graph::recording::{self, Recording};
use marlin_render_graph::{
    Access, BufferHandle, CompiledPlan, PassBuilder, PassContext, RenderGraph, TextureHandle,
};
use marlin_rhi::{BufferDesc, BufferUsage, Format, MemoryLocation, TextureDesc, TextureUsage};
use marlin_std::mem::Arena;

pub type Graph<'g> = RenderGraph<'g, Recording>;
pub type Builder<'b, 'g> = PassBuilder<'b, 'g, Recording>;

pub static DEVICE: recording::Device = recording::Device::ASYNC;

/// A frame arena for one test.
pub fn arena() -> Arena {
    Arena::new(16 << 20).expect("reserve the test arena")
}

/// A recording closure that records nothing.
pub fn noop() -> impl FnOnce(&mut PassContext<'_, Recording>) + Send {
    |_| {}
}

pub fn compile(g: &Graph<'_>) -> CompiledPlan {
    g.compile(&DEVICE).expect("compile")
}

/// Names of the retained passes, in order.
pub fn names(plan: &CompiledPlan) -> Vec<&str> {
    plan.passes().iter().map(|p| p.name.as_str()).collect()
}

/// A compute pass reading `reads` and writing `writes` with compute accesses; returns the new
/// versions of `writes`.
pub fn compute<'g, const N: usize>(
    g: &mut Graph<'g>,
    name: &str,
    reads: &[TextureHandle<'g>],
    writes: [TextureHandle<'g>; N],
) -> [TextureHandle<'g>; N] {
    g.add_compute(name, |b: &mut Builder<'_, 'g>| {
        for &r in reads {
            b.read_texture(r, Access::ComputeRead);
        }
        (
            writes.map(|w| b.write_texture(w, Access::ComputeWrite)),
            noop(),
        )
    })
}

/// A compute pass reading and writing buffers.
pub fn compute_buffers<'g, const N: usize>(
    g: &mut Graph<'g>,
    name: &str,
    reads: &[BufferHandle<'g>],
    writes: [BufferHandle<'g>; N],
) -> [BufferHandle<'g>; N] {
    g.add_compute(name, |b: &mut Builder<'_, 'g>| {
        for &r in reads {
            b.read_buffer(r, Access::ComputeRead);
        }
        (
            writes.map(|w| b.write_buffer(w, Access::ComputeWrite)),
            noop(),
        )
    })
}

/// A side-effect pass reading `reads` in compute shaders: something the frame shows.
pub fn sink<'g>(g: &mut Graph<'g>, name: &str, reads: &[TextureHandle<'g>]) {
    g.add_compute(name, |b: &mut Builder<'_, 'g>| {
        b.side_effect();
        for &r in reads {
            b.read_texture(r, Access::ComputeRead);
        }
        ((), noop())
    });
}

/// An imported texture's object: a 2D texture with every usage but sharing.
pub fn imported_texture(width: u32, height: u32, format: Format) -> recording::Texture {
    let usage = TextureUsage::COPY_SRC
        | TextureUsage::COPY_DST
        | TextureUsage::SAMPLED
        | TextureUsage::STORAGE
        | TextureUsage::RENDER_TARGET;
    recording::Texture(TextureDesc::new_2d(width, height, format, usage))
}

pub fn imported_buffer(size: u64) -> recording::Buffer {
    recording::Buffer(BufferDesc {
        size,
        usage: BufferUsage::SHADER_READ
            | BufferUsage::SHADER_WRITE
            | BufferUsage::INDIRECT
            | BufferUsage::COPY_SRC
            | BufferUsage::COPY_DST,
        memory: MemoryLocation::GpuOnly,
    })
}

pub type Table = marlin_render_graph::StateTable<Recording>;

/// Runs one frame over `table`: declares with `declare`, compiles, records the states the frame
/// leaves. `&'g &'o ()` lets `declare` import objects that live for `'o` (it implies `'o: 'g`).
pub fn frame<'o>(
    table: &mut Table,
    declare: impl for<'g> FnOnce(&mut Graph<'g>, &'g &'o ()),
) -> CompiledPlan {
    let arena = arena();
    let (plan, states) = {
        let mut g = Graph::with_states(&arena, table);
        declare(&mut g, &&());
        let plan = compile(&g);
        let states = g.frame_states(&plan);
        (plan, states)
    };
    table.end_frame(states);
    plan
}
