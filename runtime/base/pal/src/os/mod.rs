//! Platform-specific escape hatches (like `std::os`), for L1 backends that call the OS directly.

#[cfg(marlin_platform = "windows")]
pub mod windows;
