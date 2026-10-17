//! Capability extensions.
//!
//! A module here exists only when the platform manifest lists the capability: "this platform has
//! the API". Each one exposes a `get()` returning a token only when this process, on this machine,
//! can actually use it.
//!
//! ```ignore
//! #[cfg(marlin_ext = "large_pages")]
//! if let Some(lp) = marlin_pal::ext::large_pages::LargePages::get() {
//!     let p = lp.alloc(lp.page_size(), Protection::ReadWrite)?;
//! }
//! ```

#[cfg(marlin_ext = "crash_dump")]
pub mod crash_dump;
#[cfg(marlin_ext = "dylib")]
pub mod dylib;
#[cfg(marlin_ext = "hybrid_cores")]
pub mod hybrid_cores;
#[cfg(marlin_ext = "large_pages")]
pub mod large_pages;
#[cfg(marlin_ext = "vmem_alias")]
pub mod vmem_alias;
