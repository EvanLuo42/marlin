//! Backend selection: the only place in the crate that dispatches on `marlin_platform`.
//!
//! Console backends are private repositories cloned into `platforms/<name>/` (see
//! `platforms/README.md`). Their modules are only loaded when their cfg is active, so the public
//! tree builds without them. rustfmt ignores cfg, so each one is `#[rustfmt::skip]` to keep
//! `cargo fmt` working without the private repositories (format those from their own repository).

#[cfg(any(marlin_platform = "linux", marlin_platform = "macos"))]
pub(crate) mod unix;

#[cfg(any(marlin_platform = "windows", marlin_platform = "xbox"))]
pub(crate) mod windows;
#[cfg(marlin_platform = "windows")]
pub(crate) use windows as imp;

#[cfg(marlin_platform = "linux")]
pub(crate) mod linux;
#[cfg(marlin_platform = "linux")]
pub(crate) use linux as imp;

#[cfg(marlin_platform = "macos")]
pub(crate) mod macos;
#[cfg(marlin_platform = "macos")]
pub(crate) use macos as imp;

#[cfg(marlin_platform = "ps5")]
#[path = "../../../../../platforms/ps5/pal/mod.rs"]
#[rustfmt::skip]
pub(crate) mod ps5;
#[cfg(marlin_platform = "ps5")]
pub(crate) use ps5 as imp;

// Reuses the Win32 backend above through `super::windows`.
#[cfg(marlin_platform = "xbox")]
#[path = "../../../../../platforms/xbox/pal/mod.rs"]
#[rustfmt::skip]
pub(crate) mod xbox;
#[cfg(marlin_platform = "xbox")]
pub(crate) use xbox as imp;

#[cfg(marlin_platform = "switch2")]
#[path = "../../../../../platforms/switch2/pal/mod.rs"]
#[rustfmt::skip]
pub(crate) mod switch2;
#[cfg(marlin_platform = "switch2")]
pub(crate) use switch2 as imp;

#[cfg(not(any(
    marlin_platform = "windows",
    marlin_platform = "linux",
    marlin_platform = "macos",
    marlin_platform = "ps5",
    marlin_platform = "xbox",
    marlin_platform = "switch2",
)))]
compile_error!(
    "marlin-pal: no platform selected. Build through `cargo xtask`, or run `cargo xtask gen-cfg` \
     so .cargo/config.toml sets `--cfg marlin_platform=\"...\"` for this host. A RUSTFLAGS \
     environment variable overrides that config."
);
