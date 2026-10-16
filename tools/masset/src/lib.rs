//! The `.masset` source format (`docs/design/0003-asset-pipeline.md` §1).
//!
//! Every piece of content is text: objects with a type, a UUID and a name, typed attribute
//! values, links to other objects (`<path:Name #uuid>`) and to source files
//! (`` `source:path` ``). Names in text are PascalCase and convert to the Rust snake_case
//! automatically.
//!
//! ```text
//! !Version 1
//!
//! Texture
//! {
//!     !Uuid = "01832501-7918-7ee4-b177-2f200aab91b3"
//!     !Name = "RockAlbedo"
//!     Format = Bc7
//!     MipMaps = true
//!     Source = `source:props/rock_albedo.png`
//! }
//! ```
//!
//! Layers, each usable on its own:
//!
//! - [`parse`] / [`write`]: text to an untyped [`Document`] and back. Writing is canonical (one
//!   layout per tree), so saves diff cleanly.
//! - [`Schema`]: binds objects to reflected Rust types ([`Schema::read_object`],
//!   [`Schema::write_object`]), with default omission, unknown-attribute preservation and
//!   per-type schema upgrades.
//! - [`extract()`]: the indexing fast path: types, UUIDs, names and links only, borrowed from the
//!   text.

mod ast;
mod bind;
pub mod case;
mod error;
mod extract;
mod lex;
mod parse;
mod schema;
mod uuid;
mod write;

pub use ast::{
    Attr, Document, FORMAT_VERSION, FileRef, LinkRef, Location, Node, NodeKind, Object,
    SOURCE_SCHEME, Span,
};
pub use bind::{DocumentLinks, FoundLink, Links, NoLinks, ReadObject, WriteError};
pub use error::{Diagnostic, ParseError, SchemaError, Severity};
pub use extract::{Extract, ExtractedFile, ExtractedLink, ExtractedObject, extract};
pub use parse::{parse, parse_value};
pub use schema::{Schema, Upgrade};
pub use uuid::new_uuid;
pub use write::{MAX_WIDTH, inline, write, write_object};
