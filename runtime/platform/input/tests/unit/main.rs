//! marlin-input's platform-independent tests (quick tier, docs/design/0014-testing.md §5): the
//! input state machine and HID report parsing and encoding. Run alone with
//! `cargo test -p marlin-input --test unit`.

mod hid;
mod state;
