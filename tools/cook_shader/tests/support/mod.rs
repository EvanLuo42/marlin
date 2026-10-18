//! The [`Project`] shared with the other cook crates (`project.rs` from marlin-asset-db's tests,
//! `cook.rs` from marlin-cook-core's).
#![allow(dead_code, unused_imports, reason = "each test binary uses a subset")]

#[path = "../../../asset_db/tests/support/project.rs"]
mod project;

#[path = "../../../cook_core/tests/support/cook.rs"]
mod cook;

pub use cook::CookedUnit;
pub use project::{Project, uuid};
