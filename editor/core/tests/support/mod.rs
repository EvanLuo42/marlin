//! marlin-editor-core's test helpers: the throwaway [`Project`] of marlin-asset-db's tests.

#[path = "../../../../tools/asset_db/tests/support/project.rs"]
mod project;

pub use project::{Project, uuid};
