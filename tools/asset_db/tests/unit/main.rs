//! marlin-asset-db's tests (quick tier, docs/design/0014-testing.md §5): building, updating,
//! validating and persisting the index over throwaway projects on disk, plus the NTFS change
//! journal on Windows. The submit checks need `git` on the PATH.
//! Run alone with `cargo test -p marlin-asset-db --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod index;
mod submit;
mod validate;

#[cfg(marlin_platform = "windows")]
#[path = "platform/windows.rs"]
mod windows;
