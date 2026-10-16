//! Backend selection: the only place in the crate that dispatches on `marlin_platform`.
//!
//! Console backends live in `platforms/<name>/input/` (see marlin-pal's `backend/mod.rs` for why
//! they are `#[path]` modules marked `#[rustfmt::skip]`).

#[cfg(any(marlin_platform = "windows", marlin_platform = "xbox"))]
pub(crate) mod windows;
#[cfg(marlin_platform = "windows")]
pub(crate) use windows as imp;

#[cfg(any(marlin_platform = "linux", marlin_platform = "macos"))]
pub(crate) mod stub;
#[cfg(any(marlin_platform = "linux", marlin_platform = "macos"))]
pub(crate) use stub as imp;

#[cfg(marlin_platform = "ps5")]
#[path = "../../../../../platforms/ps5/input/mod.rs"]
#[rustfmt::skip]
pub(crate) mod ps5;
#[cfg(marlin_platform = "ps5")]
pub(crate) use ps5 as imp;

#[cfg(marlin_platform = "xbox")]
#[path = "../../../../../platforms/xbox/input/mod.rs"]
#[rustfmt::skip]
pub(crate) mod xbox;
#[cfg(marlin_platform = "xbox")]
pub(crate) use xbox as imp;

#[cfg(marlin_platform = "switch2")]
#[path = "../../../../../platforms/switch2/input/mod.rs"]
#[rustfmt::skip]
pub(crate) mod switch2;
#[cfg(marlin_platform = "switch2")]
pub(crate) use switch2 as imp;
