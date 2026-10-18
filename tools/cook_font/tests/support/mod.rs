//! marlin-cook-font's test helpers: the [`Project`] shared with the other cook crates
//! (`project.rs` from marlin-asset-db's tests, `cook.rs` from marlin-cook-core's).

#[path = "../../../asset_db/tests/support/project.rs"]
mod project;

#[path = "../../../cook_core/tests/support/cook.rs"]
mod cook;

pub use cook::CookedUnit;
pub use project::{Project, uuid};
