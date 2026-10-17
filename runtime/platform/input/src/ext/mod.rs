//! Capability extensions.
//!
//! A module here exists only when the platform manifest lists the capability: "this platform has
//! the API". Each one exposes a `get()` returning a token only when this input system can use it;
//! individual gamepads may still lack the hardware, which the calls report as `Unsupported`.
//!
//! ```ignore
//! #[cfg(marlin_ext = "gyro")]
//! if let Some(gyro) = marlin_input::ext::gyro::Gyro::get(&input) {
//!     if let Some(motion) = gyro.motion(input.state().gamepad(0)) { aim(motion.gyro); }
//! }
//! ```

#[cfg(marlin_ext = "adaptive_triggers")]
pub mod adaptive_triggers;
#[cfg(marlin_ext = "gyro")]
pub mod gyro;
#[cfg(marlin_ext = "haptics")]
pub mod haptics;
