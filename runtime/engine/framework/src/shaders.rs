//! The engine's shaders: the cooked `Marlin.Engine` library from the mounted content
//! (`content/engine/shaders/`, `docs/design/0003-asset-pipeline.md` §4.4), found by package name.
//! Which entries a subsystem takes from it, and what it uses them as, is the subsystem's
//! (`PostShaders::bind`, `UiShaders::bind`..., `marlin_shader::bind`): this module only loads.

use core::fmt;

use marlin_asset::{AssetError, AssetServer, Handle, LoadState, Vfs};
use marlin_jobs::JobSystem;
use marlin_shader::asset::ShaderLibrary;

use crate::loaders::InPlace;
use marlin_font::Font;

/// The engine's shader package.
pub const ENGINE_SHADERS: &str = "Marlin.Engine";
/// The default material's resolve permutation (`content/engine/shaders/DefaultMaterialResolve.masset`).
pub const DEFAULT_RESOLVE: &str = "Marlin.Engine.DefaultMaterial.Resolve";

#[derive(Debug)]
pub enum ShaderError {
    /// No mounted layer has the package: the engine package is not mounted.
    NotMounted,
    Load(AssetError),
}

impl fmt::Display for ShaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMounted => write!(
                f,
                "no mounted content has the shader package `{ENGINE_SHADERS}`: cook and package \
                 the project (its build includes the engine's content/engine) and mount the \
                 package (content.package)"
            ),
            Self::Load(e) => write!(f, "loading `{ENGINE_SHADERS}`: {e:?}"),
        }
    }
}

impl core::error::Error for ShaderError {}

/// Loads the engine's shader library through `assets` (which needs the engine's loaders),
/// blocking until it is there; tools and the game start with it.
pub fn load_engine_shaders(
    assets: &mut AssetServer,
    vfs: &Vfs,
    jobs: Option<&JobSystem>,
) -> Result<Handle<ShaderLibrary>, ShaderError> {
    load_named(assets, vfs, ENGINE_SHADERS, jobs)
}

/// Loads the default material's resolve permutation (a cooked `Shader`, found by name
/// [`DEFAULT_RESOLVE`]), blocking as [`load_engine_shaders`] does.
pub fn load_default_resolve(
    assets: &mut AssetServer,
    vfs: &Vfs,
    jobs: Option<&JobSystem>,
) -> Result<Handle<ShaderLibrary>, ShaderError> {
    load_named(assets, vfs, DEFAULT_RESOLVE, jobs)
}

fn load_named(
    assets: &mut AssetServer,
    vfs: &Vfs,
    name: &str,
    jobs: Option<&JobSystem>,
) -> Result<Handle<ShaderLibrary>, ShaderError> {
    let uuid = vfs.named(name).ok_or(ShaderError::NotMounted)?;
    let handle = assets.load::<ShaderLibrary>(uuid);
    assets.update_blocking(vfs, jobs);
    match assets.state(&handle) {
        LoadState::Loaded => Ok(handle),
        LoadState::Failed(e) => Err(ShaderError::Load(e)),
        LoadState::Loading => Err(ShaderError::Load(AssetError::NotFound)),
    }
}

/// The engine's UI font (`content/engine/fonts/Inter.masset`).
pub const UI_FONT: &str = "Marlin.Engine.Fonts.Inter";

/// Loads the engine's UI font ([`UI_FONT`]), blocking like [`load_engine_shaders`].
pub fn load_ui_font(
    assets: &mut AssetServer,
    vfs: &Vfs,
    jobs: Option<&JobSystem>,
) -> Result<Handle<InPlace<Font>>, AssetError> {
    let uuid = vfs.named(UI_FONT).ok_or(AssetError::NotFound)?;
    let handle = assets.load::<InPlace<Font>>(uuid);
    assets.update_blocking(vfs, jobs);
    match assets.state(&handle) {
        LoadState::Loaded => Ok(handle),
        LoadState::Failed(e) => Err(e),
        LoadState::Loading => Err(AssetError::NotFound),
    }
}
