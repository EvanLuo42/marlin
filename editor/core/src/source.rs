//! The project's source content as the editor sees it ([`SourceContent`]): the asset index of
//! every mount and the cook's text schema, so a source object reads exactly as the cook reads it.

use marlin_asset::Uuid;
use marlin_asset_db::{Content, Options, Project};
use marlin_cook_core::{Cookers, Dependency, read_source_object};
use marlin_reflect::Reflect;

/// The project's sources: indexed, and read through the engine's cookers' schema.
pub struct SourceContent {
    content: Content,
    cookers: Cookers,
}

impl SourceContent {
    /// The sources of `project`, read with `cookers`' schema (the engine's:
    /// `marlin_cooker::engine_cookers`).
    pub fn open(project: &Project, cookers: Cookers) -> Result<Self, String> {
        let (content, _) = project
            .open_content(&Options::default())
            .map_err(|e| format!("opening the project's content: {e}"))?;
        Ok(Self { content, cookers })
    }

    /// Sources from an index made elsewhere (tests, tools).
    pub fn from_parts(content: Content, cookers: Cookers) -> Self {
        Self { content, cookers }
    }

    pub fn content(&self) -> &Content {
        &self.content
    }

    /// Rescans the files that changed on disk; returns whether any did.
    pub fn update(&mut self) -> Result<bool, String> {
        let report = self
            .content
            .update()
            .map_err(|e| format!("updating the asset index: {e}"))?;
        Ok(report.added + report.changed + report.removed > 0)
    }

    /// The source object `uuid`, read and bound to its type.
    pub fn read(&self, uuid: Uuid) -> Result<Dependency, String> {
        read_source_object(&self.content, self.cookers.schema(), uuid)
    }

    /// The source object `uuid` as a `T`.
    pub fn read_as<T: Reflect + Clone>(&self, uuid: Uuid) -> Result<T, String> {
        let object = self.read(uuid)?;
        object
            .value
            .as_ref()
            .downcast::<T>()
            .cloned()
            .ok_or_else(|| {
                format!(
                    "{uuid} ({}) is a `{}`, not a `{}`",
                    object.name,
                    object.type_name,
                    marlin_reflect::type_info::<T>().name
                )
            })
    }

    /// The content hash of the file that defines `uuid`, as indexed: what documents compare to
    /// notice their sources changed.
    pub fn source_hash(&self, uuid: Uuid) -> Option<u128> {
        let object = self.content.object(uuid)?;
        let file = self.content.object_info(object).file;
        Some(self.content.db(object.mount).file_info(file).hash)
    }
}
