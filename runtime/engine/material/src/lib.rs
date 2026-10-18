//! Materials (0011): material types and instances on the host, and the passes that turn a
//! visibility buffer into a material buffer: classify every pixel by its material's resolve
//! permutation, bin the pixels, then one indirect dispatch per permutation runs that material's
//! `Surface` and writes the closures (R1k: only the default Slab, so only the dense part).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod default;

pub use default::{DEFAULT_PERMUTATION, DefaultMaterial};
mod passes;

pub use passes::{
    CLASSIFY_PUSH_BYTES, DEBUG_PUSH_BYTES, MATERIAL_FORMAT, MaterialDebug, MaterialOutput,
    MaterialShaders, MaterialType, Materials, RESOLVE_PUSH_BYTES, VELOCITY_FORMAT,
};
