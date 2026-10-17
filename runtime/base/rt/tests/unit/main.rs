//! marlin-rt's tests (quick tier, docs/design/0014-testing.md §5): the process API and the CPU
//! feature check, on the host. No hardware needed. The hosted startup and panic report run in a
//! child: see `isolated_hosted_panic`.
//! Run alone with `cargo test -p marlin-rt --test unit`.

mod cpu;
mod process;
