//! Startup for binaries that link std (the editor and tools): std owns the entry point and panics.

/// Initializes the runtime under std. Register [`marlin_mem::MarlinHeap`] as the global allocator
/// first.
pub fn init() {}
