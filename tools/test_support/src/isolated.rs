//! Tests that need a process of their own (a crash, an abort, a process-wide setting) re-run
//! themselves as a child: the test calls [`run`], which starts the current test binary filtered
//! to `child_test` with [`CHILD_ENV`] set; `child_test` is an `#[ignore]` test that returns at
//! once unless [`is_child`].
//!
//! ```ignore
//! #[test]
//! fn crash_writes_a_dump() {
//!     let out = isolated::run("crash_child", &[("MARLIN_TEST_DUMP_DIR", dir.as_ref())]);
//!     assert!(!out.status.success());
//! }
//!
//! #[test]
//! #[ignore = "child of crash_writes_a_dump"]
//! fn crash_child() {
//!     if !isolated::is_child() {
//!         return;
//!     }
//!     // ... crash ...
//! }
//! ```

use std::ffi::OsStr;
use std::process::{Command, Output};

/// Set in the child's environment by [`run`].
pub const CHILD_ENV: &str = "MARLIN_ISOLATED_CHILD";

/// Runs the `#[ignore]` test `child_test` of this binary in a child process with `env` added,
/// and returns its output, whatever its exit status.
pub fn run(child_test: &str, env: &[(&str, &OsStr)]) -> Output {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            child_test,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, child_test);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().unwrap()
}

/// Whether this process is a child started by [`run`].
pub fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some()
}
