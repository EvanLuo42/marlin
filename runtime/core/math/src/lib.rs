//! Vectors, matrices, quaternions and geometry for the engine.
//!
//! # Conventions
//!
//! - **Right-handed, Y up.** +X is right, +Y up, and cameras look down -Z ([`Vec3::FORWARD`]).
//! - **Column vectors.** Transforms apply as `M * v`, and `A * B` applies `B` first. Matrices are
//!   stored column-major, matching GLSL/HLSL `column_major` and GPU constant buffers.
//! - **Reverse-Z clip space.** Projections map depth to [0, 1] with the near plane at 1 and the
//!   far plane (or infinity) at 0, for D3D12, Vulkan and Metal. Any Y flip between APIs is the
//!   RHI's job, not the math library's.
//! - Angles are radians. Rotations are counter-clockwise when looking down the axis toward the
//!   origin.
//!
//! `Vec4`, `Quat` and `Mat4` columns are 16-byte-aligned `core::simd::f32x4`. `Vec2`/`Vec3` are plain
//! `repr(C)` floats (8 and 12 bytes) for tight GPU vertex layouts. Transcendental functions are
//! implemented in [`scalar`] (`core` has none) and are bit-identical across platforms.
#![cfg_attr(not(test), no_std)]
#![feature(core_float_math, portable_simd)]

mod geom;
mod mat;
mod quat;
pub mod scalar;
mod transform;
mod vec;

pub use geom::{Aabb, Frustum, Plane, Ray, Sphere};
pub use mat::{Mat3, Mat4};
pub use quat::Quat;
pub use transform::Transform;
pub use vec::{IVec2, IVec3, UVec2, Vec2, Vec3, Vec4, Xyzw};
