//! [`LevelDocument`]: a level opened from its source (0019 §13.2), with every Prefab it places.

use std::collections::HashMap;

use marlin_asset::Uuid;
use marlin_world::{CookError, CookKind, CookedLevel, Level, Prefab, cook_level, read_prefabs};

use crate::source::SourceContent;

/// A level as authored: its source value, the Prefabs it places (nested ones too) and the
/// content hashes of the files they were read from. Editing works on documents; worlds show them
/// cooked ([`cook`](Self::cook)).
pub struct LevelDocument {
    pub uuid: Uuid,
    pub level: Level,
    pub prefabs: HashMap<Uuid, Prefab>,
    /// The sources read, each with its file's hash then.
    sources: Vec<(Uuid, Option<u128>)>,
}

impl LevelDocument {
    /// The level `uuid` from `sources`, with its Prefabs.
    pub fn open(sources: &SourceContent, uuid: Uuid) -> Result<Self, String> {
        let level: Level = sources.read_as(uuid)?;
        let prefabs = read_prefabs(&level.instances, |p| sources.read_as::<Prefab>(p))?;
        let read = core::iter::once(uuid).chain(prefabs.keys().copied());
        let sources = read.map(|u| (u, sources.source_hash(u))).collect();
        Ok(Self {
            uuid,
            level,
            prefabs: prefabs.into_iter().collect(),
            sources,
        })
    }

    /// The level cooked, exactly as the cook makes it: what an editor world shows and a play in
    /// editor session spawns.
    pub fn cook(&self) -> Result<CookedLevel, CookError> {
        let prefabs: marlin_containers::HashMap<Uuid, Prefab> =
            self.prefabs.iter().map(|(k, v)| (*k, v.clone())).collect();
        cook_level(
            &self.level.entities,
            &self.level.instances,
            &prefabs,
            CookKind::Level,
        )
    }

    /// Whether a file it was read from changed (as `sources` indexes it now).
    pub fn is_stale(&self, sources: &SourceContent) -> bool {
        self.sources
            .iter()
            .any(|&(uuid, hash)| sources.source_hash(uuid) != hash)
    }
}
