//! The SPIR-V backend (`docs/design/0005-shader-compiler.md` §2, step 9 of §11): optimized IR to
//! SPIR-V 1.6 for Vulkan.
//!
//! Step 9a, compute entry points:
//! - Resources through the native descriptor heap (`SPV_EXT_descriptor_heap`, 0002 §6): a
//!   resource index is a slot of the `ResourceHeapEXT` variable, whose slot width the shader
//!   computes from `OpConstantSizeOfEXT` (the larger of an image's and a buffer's descriptor),
//!   and a buffer is reached through `OpBufferPointerEXT`. Push constants are a `PushConstant`
//!   block, groupshared memory `Workgroup` variables, constant tables `Private` ones.
//! - Types in scalar layout (0004 §16) where the API lays memory out, plain elsewhere.
//! - Structured control flow as merge instructions, block parameters as `OpPhi`.
//! - What SPIR-V leaves undefined is made defined, as the IR has it: shift counts are taken
//!   modulo the width, float-to-integer conversions saturate (NaN to 0).
//!
//! Step 10c: mesh and task shaders (`SPV_EXT_mesh_shader`): mesh outputs are `Output` arrays
//! (`PerPrimitiveEXT` for primitives, the index list a `Primitive*IndicesEXT` built-in); the
//! output counts and topology come from those arrays; `OpSetMeshOutputsEXT`; and
//! `OpEmitMeshTasksEXT` with the payload variable, which ends the block in place of its
//! return.
//!
//! Step 10b: textures and samplers. Their handles are loaded from the resource and sampler
//! heaps (arrays of `OpTypeImage` and `OpTypeSampler`, the sampler slot
//! `OpConstantSizeOfEXT(sampler)` wide); samples and fetches take four components and narrow
//! them to the texel; storage images are read and written without a format, four components
//! at a time; integer texels are unsigned. Non-uniform descriptor indices get `NonUniform`.
//!
//! Step 10a: vertex and fragment entry points: `Input`/`Output` variables with `BuiltIn`,
//! `Location` (and `Index` for target 0's second blend source) and interpolation decorations;
//! `OriginUpperLeft`, and `DepthReplacing` when the depth is written; `[Position]` is
//! `FragCoord` as a pixel input; `[InstanceId]` is `InstanceIndex - BaseInstance`, from 0 as on
//! every backend (0002 §3). The validator checks those built-ins, a `Location` on every other
//! stage variable, and `Flat` integer fragment inputs. Clip distances, primitive ids and
//! coverage come later.
//!
//! Step 9d: uniforms (a `Uniform` block reached through `OpBufferPointerEXT`) and byte
//! buffers (blocks of 32-bit words, addressed by byte offset / 4; their length in bytes).
//!
//! Step 9b, the validator ([`validate`], also [`Backend::validate`]): our own check of the
//! SPIR-V rules the code above relies on, with no outside tools (0005 §4.6).
//!
//! Still to come: the other stages, textures, samplers and `debug.*` (step 10).

mod body;
mod dis;
pub mod spirv;
mod validate;
mod write;

use std::collections::BTreeSet;

use marlin_shaderc_ir::{FuncId, Module};
use marlin_shaderc_target::{Backend, BackendError, Blob, Target};

pub use dis::disassemble;
pub use validate::validate;

/// The Vulkan 1.4 target (0016 §3): SPIR-V 1.6 with descriptor heaps.
pub fn vulkan_1_4() -> Target {
    Target {
        name: "vulkan-1.4".into(),
        version: 1,
        // The baseline every device the RHI accepts has; optional features (16-bit and 64-bit
        // types, ...) come with the core library's capability names.
        capabilities: BTreeSet::new(),
        wave_sizes: (4, 128),
        calls: true,
        vectors: true,
        register_budget: 128,
    }
}

/// The SPIR-V module, as words, for entry point `entry` of `m`.
pub fn write_module(m: &Module, entry: FuncId) -> Result<Vec<u32>, BackendError> {
    let e = m.entry_of(entry).ok_or_else(|| BackendError {
        message: "not an entry point".into(),
    })?;
    for (id, _) in m.funcs.iter() {
        if id != entry {
            return write::unsupported(
                "a module with more than one function (calls are inlined first)",
            );
        }
    }
    let f = &m.funcs[entry];
    let mut w = write::Writer::new(m, e.stage);
    let id = w.id();
    let code = body::Body::new(&mut w, f).function(id)?;
    Ok(w.finish(id, &f.name, code))
}

/// Little-endian bytes of SPIR-V words.
pub fn to_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The words of little-endian SPIR-V bytes (`None` unless whole words).
pub fn from_bytes(bytes: &[u8]) -> Option<Vec<u32>> {
    let (words, rest) = bytes.as_chunks::<4>();
    rest.is_empty()
        .then(|| words.iter().map(|w| u32::from_le_bytes(*w)).collect())
}

pub struct SpirvBackend;

impl Backend for SpirvBackend {
    fn targets(&self) -> Vec<Target> {
        vec![vulkan_1_4()]
    }

    fn write(
        &self,
        module: &Module,
        entry: FuncId,
        _target: &Target,
    ) -> Result<Blob, BackendError> {
        Ok(Blob {
            bytes: to_bytes(&write_module(module, entry)?),
        })
    }

    fn validate(&self, blob: &Blob, _target: &Target) -> Vec<String> {
        match from_bytes(&blob.bytes) {
            Some(words) => validate(&words),
            None => vec!["not a whole number of words".into()],
        }
    }
}
