//! [`PieSession`]: play in editor (0019 §13): the edited level cooked as the cook makes it,
//! spawned into a world of its own, simulated until the session ends.

use std::sync::Arc;

use marlin_editor_core::LevelDocument;
use marlin_framework::Engine;
use marlin_jobs::JobSystem;
use marlin_world::{CookError, Transform, World, WorldKind};

/// A play-in-editor session: its world.
pub struct PieSession {
    pub world: World,
}

impl PieSession {
    /// Starts playing `document`: cooked (the same data a packaged game loads), spawned into a
    /// new `Pie` world of `engine`. Editing data is never touched by what the game does.
    pub fn start(
        engine: &mut Engine<'_>,
        document: &LevelDocument,
        jobs: Option<&JobSystem>,
    ) -> Result<Self, CookError> {
        let cooked = document.cook()?;
        let mut world = engine.create_world(WorldKind::Pie, jobs);
        world.spawn_level(Arc::new(cooked), Transform::IDENTITY);
        Ok(Self { world })
    }
}
