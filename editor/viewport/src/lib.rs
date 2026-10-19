//! Scene viewport, gizmos and play-in-editor (0019 §13.2).
//!
//! - [`ViewportCamera`]: a viewport's own camera, flown by the engine's spectator bindings; not
//!   an entity.
//! - [`SceneViewport`]: a scene view's input state (§9.3), camera and mode (editing or playing).
//! - [`PieSession`]: play in editor: the edited level cooked and spawned into a `Pie` world.

mod camera;
mod pie;
mod viewport;

pub use camera::ViewportCamera;
pub use pie::PieSession;
pub use viewport::{SceneViewport, ViewportMode, ViewportOutput};
