//! Backend selection: the only place in the crate that dispatches on `marlin_platform`.
//!
//! Console backends live in `platforms/<name>/rhi/` (see marlin-pal's `backend/mod.rs` for why
//! they are `#[path]` modules marked `#[rustfmt::skip]`). Xbox reuses most of `d3d12`; the
//! desktop platforms use Vulkan (docs/design/0016-rhi-vulkan.md).

#[cfg(any(marlin_platform = "windows", marlin_platform = "linux"))]
pub(crate) mod vulkan;
#[cfg(any(marlin_platform = "windows", marlin_platform = "linux"))]
pub(crate) use vulkan as imp;

#[cfg(marlin_platform = "xbox")]
pub(crate) mod d3d12;

// Metal (macOS) is not written yet.
#[cfg(marlin_platform = "macos")]
pub(crate) mod stub;
#[cfg(marlin_platform = "macos")]
pub(crate) use stub as imp;

#[cfg(marlin_platform = "ps5")]
#[path = "../../../../../platforms/ps5/rhi/mod.rs"]
#[rustfmt::skip]
pub(crate) mod ps5;
#[cfg(marlin_platform = "ps5")]
pub(crate) use ps5 as imp;

#[cfg(marlin_platform = "xbox")]
#[path = "../../../../../platforms/xbox/rhi/mod.rs"]
#[rustfmt::skip]
pub(crate) mod xbox;
#[cfg(marlin_platform = "xbox")]
pub(crate) use xbox as imp;

#[cfg(marlin_platform = "switch2")]
#[path = "../../../../../platforms/switch2/rhi/mod.rs"]
#[rustfmt::skip]
pub(crate) mod switch2;
#[cfg(marlin_platform = "switch2")]
pub(crate) use switch2 as imp;
