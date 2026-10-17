//! marlin-io's tests (quick tier, docs/design/0014-testing.md §5): files, the IO queue's engines,
//! paths, file system operations, mapping and watching, on the host's file system and temp
//! directory. No other hardware needed.
//! Run alone with `cargo test -p marlin-io --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod file;
mod fs;
mod path;
mod queue;

#[cfg(marlin_platform = "windows")]
mod platform {
    mod windows;
}
