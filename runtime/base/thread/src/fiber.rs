//! Fibers for schedulers above L1, which may not use the PAL directly.
//!
//! This is the raw, unsafe PAL API, re-exported as is: a safe fiber abstraction is a scheduler's
//! job (see `marlin-jobs`). A suspended fiber may resume on another thread, so code running on
//! fibers must not keep a thread-local's address across a switch.

pub use marlin_pal::fiber::{Context, Entry, Stack, make_context, switch};
