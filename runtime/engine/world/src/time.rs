//! Simulation time: the frame's [`Time`], the fixed-step clock [`FixedTime`], and
//! [`Interpolated`] motion between fixed steps (0019 §4.3).

use marlin_config::{CvarF64, CvarI64};
use marlin_reflect::Reflect;

use crate::transform::Transform;

pub static T_MAX_DT: CvarF64 = CvarF64::new(
    "t.max_dt",
    0.1,
    "Longest frame step the simulation takes, in seconds; longer frames run slow.",
)
.range(0.001, 1.0);
pub static T_FIXED_HZ: CvarI64 = CvarI64::new(
    "t.fixed_hz",
    60,
    "Fixed-step groups' (physics') steps per second.",
)
.range(1, 1000);
pub static T_MAX_SUBSTEPS: CvarI64 = CvarI64::new(
    "t.max_substeps",
    4,
    "Most fixed steps one frame runs; time beyond them is dropped.",
)
.range(1, 64);

/// How a world's clocks advance: from the `t.*` cvars by default ([`from_cvars`]); a host may
/// set it per world.
///
/// [`from_cvars`]: Self::from_cvars
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TimeSettings {
    /// Frame steps are clamped to this, in seconds.
    pub max_delta: f32,
    /// One fixed step, in seconds.
    pub fixed_step: f64,
    pub max_substeps: u32,
}

impl Default for TimeSettings {
    fn default() -> Self {
        Self {
            max_delta: 0.1,
            fixed_step: 1.0 / 60.0,
            max_substeps: 4,
        }
    }
}

impl TimeSettings {
    pub fn from_cvars() -> Self {
        Self {
            max_delta: T_MAX_DT.get() as f32,
            fixed_step: 1.0 / T_FIXED_HZ.get() as f64,
            max_substeps: T_MAX_SUBSTEPS.get() as u32,
        }
    }
}

/// The frame's clock, a world resource.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Time {
    delta: f32,
    unscaled_delta: f32,
    elapsed: f64,
    unscaled_elapsed: f64,
    frame: u64,
    /// Multiplies simulated time: 0.5 is slow motion. Not below 0.
    pub scale: f32,
    /// A paused world's clocks stand still and its gameplay groups do not run.
    pub paused: bool,
}

impl Time {
    pub fn new() -> Self {
        Self {
            scale: 1.0,
            ..Self::default()
        }
    }

    /// Advances by a frame `interval` seconds long (wall clock), clamped to `max_delta`.
    pub fn advance(&mut self, interval: f32, max_delta: f32) {
        self.frame += 1;
        let unscaled = interval.clamp(0.0, max_delta);
        self.unscaled_delta = unscaled;
        self.unscaled_elapsed += unscaled as f64;
        self.delta = if self.paused {
            0.0
        } else {
            unscaled * self.scale.max(0.0)
        };
        self.elapsed += self.delta as f64;
    }

    /// Simulated seconds this frame: clamped, scaled, 0 while paused.
    pub fn delta(&self) -> f32 {
        self.delta
    }

    /// This frame's clamped step, ignoring scale and pause (UI, the editor's camera).
    pub fn unscaled_delta(&self) -> f32 {
        self.unscaled_delta
    }

    /// Simulated seconds since the world started.
    pub fn elapsed(&self) -> f64 {
        self.elapsed
    }

    pub fn unscaled_elapsed(&self) -> f64 {
        self.unscaled_elapsed
    }

    /// Frames run, this one included.
    pub fn frame(&self) -> u64 {
        self.frame
    }
}

/// The fixed-step clock, a world resource. Each frame banks the simulated time and runs as many
/// whole steps as it holds, at most `max_substeps`; time beyond those is dropped so a slow frame
/// cannot make the next one slower.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct FixedTime {
    step: f64,
    accumulator: f64,
    tick: u64,
    alpha: f32,
}

impl FixedTime {
    pub fn new(step: f64) -> Self {
        Self {
            step,
            ..Self::default()
        }
    }

    /// Banks `delta` seconds and returns how many steps to run now.
    pub fn accumulate(&mut self, delta: f32, settings: &TimeSettings) -> u32 {
        self.step = settings.fixed_step;
        self.accumulator += delta as f64;
        let whole = (self.accumulator / self.step) as u64;
        let steps = whole.min(settings.max_substeps as u64);
        self.accumulator -= whole as f64 * self.step;
        steps as u32
    }

    /// Records a step run.
    pub fn finish_step(&mut self) {
        self.tick += 1;
    }

    /// Records where the frame is between the last step and the next.
    pub fn finish_frame(&mut self) {
        self.alpha = (self.accumulator / self.step).clamp(0.0, 1.0) as f32;
    }

    /// One step, in seconds.
    pub fn step(&self) -> f64 {
        self.step
    }

    /// Steps run since the world started: the simulation's tick number (networking, replays).
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// How far the frame is from the last step toward the next, in [0, 1]: what drawing
    /// interpolates [`Interpolated`] motion with.
    pub fn alpha(&self) -> f32 {
        self.alpha
    }
}

/// Moves in fixed steps and is drawn between them: the world records the entity's
/// [`Transform`] before and after each step, and drawing takes [`at`](Self::at)
/// [`FixedTime::alpha`]. For roots (physics bodies); a child moves with its parent.
#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_world::Interpolated", default)]
pub struct Interpolated {
    #[reflect(transient)]
    pub previous: Transform,
    #[reflect(transient)]
    pub current: Transform,
}

impl Interpolated {
    pub fn at(&self, alpha: f32) -> Transform {
        let (a, b) = (&self.previous, &self.current);
        Transform {
            translation: a.translation.lerp(b.translation, alpha as f64),
            rotation: a.rotation.slerp(b.rotation, alpha),
            scale: a.scale + (b.scale - a.scale) * alpha,
        }
    }
}
