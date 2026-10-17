//! marlin-vcs's tests (quick tier, docs/design/0014-testing.md §5): the Git backend against
//! throwaway repositories; needs `git` on the PATH.
//! Run alone with `cargo test -p marlin-vcs --test unit`.

mod git;
