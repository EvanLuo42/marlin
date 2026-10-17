//! Shared helpers for the workspace's integration tests (dev-dependency only).
//!
//! It uses std and nothing from the workspace, so every crate, from `runtime/base` up, can take it
//! as a dev-dependency without a cycle. Helpers that need a workspace crate (a GPU device, an
//! asset database) stay in that crate's `tests/common` or `tests/support` module.

mod fs;
mod git;
mod wait;

pub use fs::TempDir;
pub use git::{git, git_init, git_output};
pub use wait::wait_until;
