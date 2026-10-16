//! Capability extension contracts.
//!
//! A backend implements a trait here exactly when its platform manifest lists the capability.
//! Listing it without implementing it fails to compile in the matching `crate::ext` module.

#[cfg(marlin_ext = "large_pages")]
mod large_pages;

#[cfg(marlin_ext = "large_pages")]
pub(crate) use large_pages::LargePages;
