//! Interned names, UTF-8 and UTF-16 utilities, Unicode algorithms and formatting.
//!
//! - [`Name`]: an interned string in the spirit of Unreal's `FName`: 12 bytes, `Copy`, O(1)
//!   compare and hash, ASCII-case-insensitive but case-preserving, with a split-off number suffix.
//! - [`InlineString<N>`]: a UTF-8 string of at most `N` bytes stored inline.
//! - [`fmt`]: formatting without allocating, into an [`InlineString`] ([`format_inline!`]) or a
//!   caller-provided buffer ([`format_to!`]).
//! - [`utf8`]: ASCII-case-insensitive comparison and search, char-boundary helpers.
//! - [`utf16`]: encoding to and decoding from UTF-16, strict or lossy.
//! - [`unicode`]: case folding and caseless comparison, normalization (NFC/NFD/NFKC/NFKD),
//!   grapheme clusters and word boundaries, from our own generated UCD tables.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod fmt;
mod inline_string;
mod name;
pub mod unicode;
pub mod utf16;
pub mod utf8;

pub use inline_string::InlineString;
pub use marlin_containers::CapacityError;
pub use name::{Name, name_hash};
