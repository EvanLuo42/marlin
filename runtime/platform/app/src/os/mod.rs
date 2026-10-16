//! Platform-specific extensions (like `std::os`): native handles and raw OS message hooks for the
//! other L1 crates.

#[cfg(marlin_platform = "windows")]
pub mod windows;
