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
//! Still to come: the validator (step 9b), the other stages, textures, samplers and
//! `debug.*` (step 10).

mod body;
mod dis;
pub mod spirv;
mod write;

use std::collections::BTreeSet;

use marlin_shaderc_ir::{FuncId, Module, Stage};
use marlin_shaderc_target::{Backend, BackendError, Blob, Target};

pub use dis::disassemble;

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
    let Stage::Compute(size) = e.stage else {
        return write::unsupported(format!("a {} entry point", e.stage.name()));
    };
    for (id, _) in m.funcs.iter() {
        if id != entry {
            return write::unsupported(
                "a module with more than one function (calls are inlined first)",
            );
        }
    }
    let f = &m.funcs[entry];
    let mut w = write::Writer::new(m);
    let id = w.id();
    let code = body::Body::new(&mut w, f).function(id)?;
    Ok(w.finish(id, size, &f.name, code))
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

    fn validate(&self, _blob: &Blob, _target: &Target) -> Vec<String> {
        Vec::new()
    }
}
