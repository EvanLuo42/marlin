//! Prefab test scenes for the renderer's GPU tests: each a few meshes (cooked by the real mesh
//! cooker, [`cook`]), instances with their albedo, lights, an environment and a camera
//! ([`Scene`], [`scenes`]); and a CPU reference of their direct lighting to test algorithms
//! against ([`reference`]). A harness loads a scene its own way (uploads, instances, lights);
//! tests of later algorithms reuse the same scenes and references.

pub mod cook;
pub mod reference;
pub mod scenes;

pub use scenes::{Instance, Scene, SceneMesh};
