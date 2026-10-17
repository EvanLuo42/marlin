//! Platform-specific escape hatches (like `std::os`), for L1 backends that call the OS directly.

#[cfg(any(marlin_platform = "linux", marlin_platform = "macos"))]
pub mod unix;
#[cfg(marlin_platform = "windows")]
pub mod windows;

/// Linux: the POSIX helpers.
#[cfg(marlin_platform = "linux")]
pub mod linux {
    pub use super::unix::*;
}

/// macOS: the POSIX helpers.
#[cfg(marlin_platform = "macos")]
pub mod macos {
    pub use super::unix::*;
}
