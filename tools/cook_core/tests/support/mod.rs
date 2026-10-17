//! marlin-cook-core's test helpers: the [`Project`] shared with marlin-asset-db's tests, and
//! cooking it (`cook.rs`, which the other cook crates include too).

#[path = "../../../asset_db/tests/support/project.rs"]
mod project;

mod cook;

pub use project::{Project, uuid};
