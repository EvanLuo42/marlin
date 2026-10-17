//! marlin-text's tests (quick tier, docs/design/0014-testing.md §5): names, UTF-8 and UTF-16,
//! Unicode algorithms, formatting, string tables and localized messages. The UCD conformance
//! tests skip unless `MARLIN_UCD_DIR` is set. Platform-independent.
//! Run alone with `cargo test -p marlin-text --test unit`.

mod fmt;
mod inline_string;
mod loc;
mod message;
mod name;
mod name_threads;
mod string_table;
mod unicode;
mod unicode_conformance;
mod utf16;
mod utf8;

use marlin_std::mem::MarlinHeap;

/// `name_threads` interns from many threads at once, through the engine allocator.
#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();
