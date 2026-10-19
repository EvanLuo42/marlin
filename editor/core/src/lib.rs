//! Editor project model, undo/redo, selection, commands and source asset database.
//!
//! - [`SourceContent`]: the project's sources, indexed and read through the cook's schema.
//! - [`LevelDocument`]: a level opened from its source with the Prefabs it places, cooked by
//!   the same code as the cook for the worlds that show it (0019 §13.2).

mod level;
mod source;

pub use level::LevelDocument;
pub use source::SourceContent;
