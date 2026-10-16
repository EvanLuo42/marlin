//! Startup for binaries that link std (the editor and tools): std owns the entry point and panics.

use crate::cpu::{self, MissingFeatures};

/// Initializes the runtime under std. Register [`marlin_mem::MarlinHeap`] as the global allocator
/// first. Fails if the CPU lacks an instruction set the build requires; report it and exit.
pub fn init() -> Result<(), MissingFeatures> {
    cpu::check()
}
