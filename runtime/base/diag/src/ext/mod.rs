//! Capability extensions of the PAL that belong to diagnostics, present when the platform
//! manifest lists them (see `marlin_pal::ext`).

#[cfg(marlin_ext = "crash_dump")]
pub mod crash_dump;
