//! Capability extensions; each module exists only where the platform manifest lists it.

/// P/E-core power hints (EcoQoS), independent of [`ThreadPriority`](crate::ThreadPriority).
#[cfg(marlin_ext = "hybrid_cores")]
pub mod qos {
    pub use marlin_pal::ext::hybrid_cores::{EfficiencyMode, HybridCores};
}
