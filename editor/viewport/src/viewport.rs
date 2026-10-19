//! [`SceneViewport`]: a scene view of the editor (0019 §9.3, §13.2): its input state, its camera
//! and whether it edits or plays.

use marlin_framework::input::{ViewportEnv, ViewportInput, ViewportState};
use marlin_input::InputState;
use marlin_input_action::{CursorMode, InputFrame, InputSettings};
use marlin_renderer::{ViewId, ViewTarget};

use crate::camera::ViewportCamera;

/// What a scene viewport shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewportMode {
    /// The editor world, through the viewport's camera; input flies the camera while the right
    /// button is held.
    Edit,
    /// A play-in-editor world, through its own cameras; input goes to its player 0 while the
    /// viewport has it.
    Play,
}

/// What a viewport made of a frame's input.
#[derive(Clone, PartialEq, Debug)]
pub struct ViewportOutput {
    /// For the played world's player 0 (empty while editing).
    pub world: InputFrame,
    pub cursor: CursorMode,
}

/// A scene viewport.
pub struct SceneViewport {
    index: u32,
    input: ViewportInput,
    pub camera: ViewportCamera,
    mode: ViewportMode,
}

impl SceneViewport {
    /// The viewport `index` (distinct per editor), editing through `camera`.
    pub fn new(index: u32, camera: ViewportCamera) -> Self {
        Self {
            index,
            input: ViewportInput::editor(),
            camera,
            mode: ViewportMode::Edit,
        }
    }

    pub fn mode(&self) -> ViewportMode {
        self.mode
    }

    pub fn input_state(&self) -> ViewportState {
        self.input.state()
    }

    /// The id of its camera's view: the key of the view's history in the renderer.
    pub fn view_id(&self) -> ViewId {
        // "Viewport" in ASCII, then the index.
        ViewId::new(0x5669_6577_706f_7274 ^ u64::from(self.index))
    }

    /// Where it draws: its own offscreen target, which the editor's UI shows.
    pub fn target(&self) -> ViewTarget {
        ViewTarget::Offscreen(self.index)
    }

    /// Plays: the input goes to the played world from now on, captured at once.
    pub fn play(&mut self) {
        self.mode = ViewportMode::Play;
        self.input = ViewportInput::game();
    }

    /// Back to editing.
    pub fn stop(&mut self) {
        self.mode = ViewportMode::Edit;
        self.input = ViewportInput::editor();
    }

    /// Routes a frame's input (what the layers above left) by `env` and the played world's
    /// `settings`; editing, flies the camera with it, `dt` seconds after the last frame.
    pub fn route(
        &mut self,
        state: &InputState,
        frame: InputFrame,
        env: ViewportEnv,
        settings: InputSettings,
        dt: f32,
    ) -> ViewportOutput {
        let route = self.input.route(state, frame, env, settings);
        match self.mode {
            ViewportMode::Edit => {
                // Evaluated every frame, so actions end when the capture does.
                self.camera.update(&route.world, dt);
                ViewportOutput {
                    world: InputFrame::empty(),
                    cursor: route.cursor,
                }
            }
            ViewportMode::Play => ViewportOutput {
                world: route.world,
                cursor: route.cursor,
            },
        }
    }
}
