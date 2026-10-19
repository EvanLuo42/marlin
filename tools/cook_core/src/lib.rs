//! The cook pipeline (`docs/design/0003-asset-pipeline.md` §4–§5).
//!
//! One `.masset` file cooks into one unit (`marlin_asset::cooked`): every object in it, turned
//! into runtime data by the [`Cooker`] registered for its type, with its source files read
//! through the object's `source:` links only. A unit's [cook key](unit_key) hashes all of its
//! inputs, so results are cached in a [`Ddc`] forever and only units whose inputs changed are
//! cooked again. [`cook`] runs units in parallel; [`cook_all`] also produces the build
//! [`Manifest`] that names every unit's result.
//!
//! [`write_package`] packs a build's units into a `.mpak` for shipping.
//!
//! Inputs come from the asset index (`marlin-asset-db`), so deciding what needs cooking opens no
//! files.

mod cook;
mod cooker;
mod ddc;
mod manifest;
mod package;
mod platform;

pub use cook::{
    COOK_FORMAT, CookError, CookOptions, CookReport, UnitOutcome, cook, cook_all, cook_unit,
    fetch_unit, named_objects, publish_manifest, source_units, unit_key,
};
pub use cooker::{
    CookContext, Cooked, Cooker, Cookers, Dependency, SourceObject, fingerprint, read_source_object,
};
pub use ddc::{Ddc, LayeredDdc, LocalDdc};
pub use manifest::{Manifest, ManifestError, ManifestUnit};
pub use package::{PackageOptions, PackageReport, PackageWriteError, write_package};
pub use platform::{Platform, Profile, TextureFamily, TextureLayout, UnknownPlatform};
