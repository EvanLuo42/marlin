//! Shared helpers for the workspace's integration tests (dev-dependency only). The conventions
//! they serve are in docs/design/0014-testing.md.
//!
//! It uses std and nothing from the workspace, so every crate, from `runtime/base` up, can take it
//! as a dev-dependency without a cycle. GPU helpers are in `marlin-gpu-test-support`; helpers
//! that need another workspace crate (an asset database) stay in that crate's `tests/support`.
//! Anything the engine itself could use (an RNG, float comparison) belongs in the runtime.

mod alloc;
mod drop;
mod env;
mod exclusive;
mod fs;
mod git;
pub mod golden;
pub mod isolated;
mod panic;
mod server;
mod time;
mod wait;

pub use alloc::MemCap;
pub use drop::{DropCounter, DropToken, Tracked, Tracker};
pub use env::{bless, data_path, record_skip, scale, seed, test_uuid};
pub use exclusive::one_at_a_time;
pub use fs::{TempDir, TempFile};
pub use git::{git, git_init, git_output};
pub use panic::{assert_panics_with, panic_message};
pub use server::BackgroundServer;
pub use time::{CLOCK_SLACK, assert_duration_in, assert_elapsed, assert_waited, with_timeout};
pub use wait::wait_until;
