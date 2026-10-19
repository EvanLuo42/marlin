//! [`ViewportCamera`]: an editor viewport's own camera (0019 §8, §13.2). Not an entity: levels
//! never save it and scripts never see it. It flies by the engine's spectator bindings, through
//! an input user of its own, with the spectators' flight ([`marlin_gameplay::fly`]).

use std::sync::Arc;

use marlin_gameplay::{FlyInput, SpectatorActions, SpectatorLook, fly, look_of};
use marlin_input_action::{CompiledContext, InputFrame, InputUser};
use marlin_math::{DVec3, Quat, Transform as RenderTransform};
use marlin_renderer::{Camera, RENDER_ORIGIN, RenderView, ViewId, ViewTarget};
use marlin_world::{GlobalTransform, Transform};

/// An editor viewport's camera.
pub struct ViewportCamera {
    pub translation: DVec3,
    look: SpectatorLook,
    pub camera: Camera,
    /// Metres per second.
    pub speed: f32,
    user: InputUser,
    actions: SpectatorActions,
}

impl ViewportCamera {
    /// A camera at `at`, flown by the spectator `context` (whose actions are `actions`).
    pub fn new(context: Arc<CompiledContext>, actions: SpectatorActions, at: Transform) -> Self {
        let mut user = InputUser::new();
        user.add_context(context, 0);
        Self {
            translation: at.translation,
            look: look_of(at.rotation),
            camera: Camera::default(),
            speed: 10.0,
            user,
            actions,
        }
    }

    pub fn rotation(&self) -> Quat {
        Quat::from_euler_yxz(self.look.yaw, self.look.pitch, 0.0)
    }

    /// Moves it to `at` (to look through a world's camera).
    pub fn place(&mut self, at: Transform) {
        self.translation = at.translation;
        self.look = look_of(at.rotation);
    }

    /// One frame of flight on `input` (what the viewport routes to it while captured).
    pub fn update(&mut self, input: &InputFrame, dt: f32) {
        self.user.evaluate(input, dt);
        let fly_input = FlyInput::read(&self.actions, |a| self.user.action(a));
        if fly_input != FlyInput::default() {
            fly(
                &mut self.look,
                &mut self.translation,
                &fly_input,
                self.speed,
                dt,
            );
        }
    }

    /// Its view, for a world's view list: `id`, drawn into `target`.
    pub fn view(&self, id: ViewId, target: ViewTarget) -> RenderView {
        let global = GlobalTransform {
            translation: self.translation,
            rotation: self.rotation(),
            scale: marlin_math::Vec3::ONE,
        };
        let transform: RenderTransform = global.relative_to(RENDER_ORIGIN);
        RenderView {
            id,
            target,
            // Above any camera of the world drawing into the same target.
            priority: i32::MAX,
            transform,
            camera: self.camera.to_render(),
        }
    }
}
