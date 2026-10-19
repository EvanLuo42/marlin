//! Quality presets as assets (0008 §2.2, R4f): scalability tables and device profiles, authored
//! as `.masset` text, cooked, found by name and applied at boot into marlin-config's layers.
//!
//! As in Unreal: a **scalability group** is a cvar (`sg.lighting = 0..4`) whose levels each set a
//! list of cvars ([`Scalability`]); an overall group (`sg.quality`, Low to Cinematic) sets the
//! other groups; a **device profile** ([`DeviceProfile`]) sets groups and cvars for a platform or
//! a class of hardware. Every value goes through a cvar, so the console, the user's settings and
//! the command line override any of them (marlin-config's layers: scalability < project <
//! device profile < user < command line < console).
//!
//! ```text
//! Scalability
//! {
//!     !Uuid = "…"
//!     !Name = "Marlin.Engine.Scalability"
//!     Levels = [
//!         { Group = "sg.lighting", Level = "2", Values = [
//!             { Name = "r.restir.candidates", Value = "16" },
//!             { Name = "r.restir.neighbors", Value = "2" },
//!         ] },
//!     ]
//! }
//! ```
//!
//! At boot ([`apply_presets`]): the engine's table ([`ENGINE_SCALABILITY`]), then the game's
//! ([`GAME_SCALABILITY`], optional), which merges over it level by level; then the device
//! profile `game.device_profile` names (`Marlin.Engine.DeviceProfiles.<name>`, the platform's
//! by default), into the device profile layer.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use marlin_asset::{AssetError, AssetServer, LoadState, Loaders, Vfs};
use marlin_config::{ApplyError, CvarString, Source};
use marlin_reflect::Reflect;

/// The engine's scalability table (`content/engine/config/Scalability.masset`).
pub const ENGINE_SCALABILITY: &str = "Marlin.Engine.Scalability";
/// A game's own table, merged over the engine's: optional.
pub const GAME_SCALABILITY: &str = "Game.Scalability";
/// Device profiles are named this, then `.` and the profile.
pub const DEVICE_PROFILES: &str = "Marlin.Engine.DeviceProfiles";

pub static GAME_DEVICE_PROFILE: CvarString = CvarString::new(
    "game.device_profile",
    marlin_rt::PLATFORM,
    "Device profile applied at boot (Marlin.Engine.DeviceProfiles.<name>); defaults to the platform.",
);

/// A cvar and its value, as text the cvar parses (`"16"`, `"true"`, `"high"`).
#[derive(Clone, PartialEq, Eq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct CvarValue {
    pub name: String,
    pub value: String,
}

/// One level of a scalability group: the cvars it sets.
#[derive(Clone, PartialEq, Eq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct ScalabilityLevel {
    /// The group cvar (`sg.lighting`).
    pub group: String,
    /// The level, as the group parses it: a number, or an enum group's name.
    pub level: String,
    pub values: Vec<CvarValue>,
}

/// Scalability tables: levels of groups.
#[derive(Clone, PartialEq, Eq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct Scalability {
    pub levels: Vec<ScalabilityLevel>,
}

/// A device profile: the values a platform, or a class of hardware, starts with.
#[derive(Clone, PartialEq, Eq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct DeviceProfile {
    pub values: Vec<CvarValue>,
}

/// Why presets did not apply.
#[derive(Debug, marlin_error::Error)]
pub enum PresetError {
    #[error("loading {name}")]
    Load {
        name: String,
        #[source]
        source: AssetError,
    },
    #[error("{name}: {errors}")]
    Values { name: String, errors: String },
}

impl Scalability {
    /// Adds the levels to marlin-config's groups (merging over levels added before) and
    /// applies the groups. Returns the entries that failed, with their level.
    pub fn apply(&self) -> Vec<(String, ApplyError)> {
        let mut failed = Vec::new();
        for level in &self.levels {
            let values: Vec<(&str, &str)> = level
                .values
                .iter()
                .map(|v| (v.name.as_str(), v.value.as_str()))
                .collect();
            for e in marlin_config::add_scalability_level(&level.group, &level.level, &values) {
                failed.push((format!("{}.{}", level.group, level.level), e));
            }
        }
        failed
    }
}

impl DeviceProfile {
    /// Replaces the device profile layer with the profile's values. Returns the entries that
    /// failed.
    pub fn apply(&self) -> Vec<ApplyError> {
        marlin_config::clear_layer(Source::DeviceProfile);
        let mut failed = Vec::new();
        for (i, v) in self.values.iter().enumerate() {
            if let Err(e) = marlin_config::set(&v.name, &v.value, Source::DeviceProfile) {
                failed.push(ApplyError {
                    location: marlin_config::Location::Entry(i as u32),
                    key: v.name.clone(),
                    source: marlin_config::Cause::Set(e),
                });
            }
        }
        failed
    }
}

/// Loads the object named `name` from `vfs`, if there is one.
fn load_named<T: Reflect + Clone + Send + Sync>(
    vfs: &Vfs,
    name: &str,
) -> Result<Option<T>, PresetError> {
    let Some(uuid) = vfs.named(name) else {
        return Ok(None);
    };
    let mut loaders = Loaders::new();
    loaders.register_serial::<T>();
    let mut assets = AssetServer::new(loaders);
    let handle = assets.load::<T>(uuid);
    assets.update_blocking(vfs, None);
    let fail = |source| PresetError::Load {
        name: name.into(),
        source,
    };
    match assets.state(&handle) {
        LoadState::Loaded => Ok(assets.get(&handle).cloned()),
        LoadState::Failed(e) => Err(fail(e)),
        LoadState::Loading => Err(fail(AssetError::NotFound)),
    }
}

/// Applies the presets `vfs` holds (see the module docs). A build without the engine's table
/// (an old one) applies none; values that do not apply are errors, for the boot to report.
pub fn apply_presets(vfs: &Vfs) -> Result<(), PresetError> {
    let report = |name: &str, errors: Vec<String>| {
        if errors.is_empty() {
            Ok(())
        } else {
            Err(PresetError::Values {
                name: name.into(),
                errors: errors.join("; "),
            })
        }
    };
    for name in [ENGINE_SCALABILITY, GAME_SCALABILITY] {
        if let Some(table) = load_named::<Scalability>(vfs, name)? {
            let errors = table
                .apply()
                .into_iter()
                .map(|(level, e)| format!("{level}: {e}"))
                .collect();
            report(name, errors)?;
        }
    }
    let profile = format!("{DEVICE_PROFILES}.{}", GAME_DEVICE_PROFILE.get());
    if let Some(p) = load_named::<DeviceProfile>(vfs, &profile)? {
        let errors = p.apply().into_iter().map(|e| format!("{e}")).collect();
        report(&profile, errors)?;
    }
    Ok(())
}
