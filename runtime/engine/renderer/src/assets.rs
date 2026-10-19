//! The render module's engine assets ([`RenderAssets`]), loaded at startup by name: the engine's
//! shaders, the cooked `Marlin.Engine` library from the mounted content
//! (`content/engine/shaders/`, `docs/design/0003-asset-pipeline.md` §4.4), found by package name.
//! Which entries a subsystem takes from it, and what it uses them as, is the subsystem's
//! (`PostShaders::bind`, `UiShaders::bind`..., `marlin_shader::bind`): this module only loads.

use core::fmt;

use marlin_asset::{AssetError, AssetServer, Handle, InPlace, LoadState, Vfs};
use marlin_jobs::JobSystem;
use marlin_shader::asset::ShaderLibrary;

use marlin_render::asset::PairingPattern;

/// The engine's shader package.
pub const ENGINE_SHADERS: &str = "Marlin.Engine";
/// The default material's resolve permutation (`content/engine/shaders/DefaultMaterialResolve.masset`).
pub const DEFAULT_RESOLVE: &str = "Marlin.Engine.DefaultMaterial.Resolve";
/// The default material's simplified-material bake (`content/engine/shaders/DefaultMaterialBake.masset`).
pub const DEFAULT_BAKE: &str = "Marlin.Engine.DefaultMaterial.Bake";

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

/// Loads the default material's simplified-material bake (a cooked `Shader`, found by name
/// [`DEFAULT_BAKE`]), blocking as [`load_engine_shaders`] does.
pub fn load_default_bake(
    assets: &mut AssetServer,
    vfs: &Vfs,
    jobs: Option<&JobSystem>,
) -> Result<Handle<ShaderLibrary>, ShaderError> {
    load_named(assets, vfs, DEFAULT_BAKE, jobs)
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

/// ReSTIR's pairing patterns (`content/engine/lighting/PairingPattern.masset`).
pub const PAIRING_PATTERN: &str = "Marlin.Engine.Lighting.PairingPattern";

/// Loads ReSTIR's pairing patterns ([`PAIRING_PATTERN`]), blocking like
/// [`load_engine_shaders`]; the world renderer takes them (`WorldRenderer::new`).
pub fn load_pairing_pattern(
    assets: &mut AssetServer,
    vfs: &Vfs,
    jobs: Option<&JobSystem>,
) -> Result<Handle<InPlace<PairingPattern>>, AssetError> {
    let uuid = vfs.named(PAIRING_PATTERN).ok_or(AssetError::NotFound)?;
    let handle = assets.load::<InPlace<PairingPattern>>(uuid);
    assets.update_blocking(vfs, jobs);
    match assets.state(&handle) {
        LoadState::Loaded => Ok(handle),
        LoadState::Failed(e) => Err(e),
        LoadState::Loading => Err(AssetError::NotFound),
    }
}

/// What every world renderer is made with, loaded once when the render module starts (an
/// engine resource): the engine's shader library, the default material's resolve permutation and
/// bake, and ReSTIR's pairing patterns.
pub struct RenderAssets {
    pub library: Handle<ShaderLibrary>,
    pub resolve: Handle<ShaderLibrary>,
    pub bake: Handle<ShaderLibrary>,
    pub pairing: Handle<InPlace<PairingPattern>>,
}

impl RenderAssets {
    /// Loads them from `vfs`, blocking.
    pub fn load(
        assets: &mut AssetServer,
        vfs: &Vfs,
        jobs: Option<&JobSystem>,
    ) -> Result<Self, ShaderError> {
        Ok(Self {
            library: load_engine_shaders(assets, vfs, jobs)?,
            resolve: load_default_resolve(assets, vfs, jobs)?,
            bake: load_default_bake(assets, vfs, jobs)?,
            pairing: load_pairing_pattern(assets, vfs, jobs).map_err(ShaderError::Load)?,
        })
    }

    /// The engine's shader library, which other subsystems bind their entries from (UI, the
    /// stats overlay).
    pub fn library<'a>(&self, assets: &'a AssetServer) -> Option<&'a ShaderLibrary> {
        assets.get(&self.library)
    }
}
