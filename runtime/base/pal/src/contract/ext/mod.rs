//! Capability extension contracts.
//!
//! A backend implements a trait here exactly when its platform manifest lists the capability.
//! Listing it without implementing it fails to compile in the matching `crate::ext` module.

#[cfg(marlin_ext = "crash_dump")]
mod crash_dump;
#[cfg(marlin_ext = "dylib")]
mod dylib;
#[cfg(marlin_ext = "hybrid_cores")]
mod hybrid_cores;
#[cfg(marlin_ext = "large_pages")]
mod large_pages;
#[cfg(marlin_ext = "vmem_alias")]
mod vmem_alias;

#[cfg(marlin_ext = "crash_dump")]
pub(crate) use crash_dump::CrashDump;
#[cfg(marlin_ext = "dylib")]
pub(crate) use dylib::Dylib;
#[cfg(marlin_ext = "hybrid_cores")]
pub(crate) use hybrid_cores::HybridCores;
#[cfg(marlin_ext = "large_pages")]
pub(crate) use large_pages::LargePages;
#[cfg(marlin_ext = "vmem_alias")]
pub(crate) use vmem_alias::VmemAlias;
