//! The engine's shaders: the cooked `Marlin.Engine` library from the mounted content
//! (`content/engine/shaders/`, `docs/design/0003-asset-pipeline.md` §4.4), found by package name
//! and then by entry path. Nothing here names a shader by path or UUID.

use core::fmt;

use marlin_asset::{AssetError, AssetServer, Handle, LoadState, Vfs};
use marlin_jobs::JobSystem;
use marlin_material::MaterialShaders;
use marlin_render::{PostShaders, StreamingShaders, VisibilityShaders};
use marlin_rhi::ShaderModule;
use marlin_shader::asset::{LibraryEntry, ShaderLibrary};
use marlin_shader::reflect::Stage;
use marlin_ui_render::UiShaders;

use crate::loaders::InPlace;
use marlin_font::Font;

/// The engine's shader package.
pub const ENGINE_SHADERS: &str = "Marlin.Engine";
/// The default material's resolve permutation (`content/engine/shaders/DefaultMaterialResolve.masset`).
pub const DEFAULT_RESOLVE: &str = "Marlin.Engine.DefaultMaterial.Resolve";

/// The entry point the compiler gives every module.
const MODULE_ENTRY: &str = "main";

#[derive(Debug)]
pub enum ShaderError {
    /// No mounted layer has the package: the engine package is not mounted.
    NotMounted,
    Load(AssetError),
    /// The library lacks an entry the engine uses.
    MissingEntry(&'static str),
    /// An entry is not what the engine uses it as.
    Mismatch {
        entry: &'static str,
        problem: &'static str,
    },
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
            Self::MissingEntry(entry) => write!(f, "`{ENGINE_SHADERS}` has no entry `{entry}`"),
            Self::Mismatch { entry, problem } => write!(f, "`{entry}`: {problem}"),
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

fn module(entry: &LibraryEntry) -> ShaderModule<'_> {
    ShaderModule {
        code: &entry.code,
        entry: MODULE_ENTRY,
    }
}

/// The post chain's entries (`Marlin.Engine.Post`).
pub const POST: [&str; 6] = [
    "Marlin.Engine.Post.BloomDown",
    "Marlin.Engine.Post.BloomUp",
    "Marlin.Engine.Post.Histogram",
    "Marlin.Engine.Post.Exposure",
    "Marlin.Engine.Post.FullscreenVs",
    "Marlin.Engine.Post.TonemapPs",
];

/// The post chain's shaders from the engine's library, checked for their stages and the push
/// constants marlin-render's post chain sets.
pub fn post_shaders(library: &ShaderLibrary) -> Result<PostShaders<'_>, ShaderError> {
    let get = |name: &'static str, stage: Stage, push: Option<u32>| {
        let e = library.entry(name).ok_or(ShaderError::MissingEntry(name))?;
        let mismatch = |problem| ShaderError::Mismatch {
            entry: name,
            problem,
        };
        if e.reflection.entry.stage != stage {
            return Err(mismatch("not the stage the post chain uses it as"));
        }
        if e.reflection.push.as_ref().map(|p| p.size) != push {
            return Err(mismatch(
                "its push constants are not what the post chain sets",
            ));
        }
        Ok(module(e))
    };
    Ok(PostShaders {
        bloom_down: get(POST[0], Stage::Compute, Some(16))?,
        bloom_up: get(POST[1], Stage::Compute, Some(20))?,
        histogram: get(POST[2], Stage::Compute, Some(16))?,
        exposure: get(POST[3], Stage::Compute, Some(40))?,
        fullscreen: get(POST[4], Stage::Vertex, None)?,
        tonemap: get(POST[5], Stage::Pixel, Some(20))?,
    })
}

/// The GPU Scene's scatter (`Marlin.Engine.Scene.Scatter`).
pub const SCATTER: &str = "Marlin.Engine.Scene.Scatter";

/// The scatter shader from the engine's library, checked for its stage and the push constants
/// marlin-render's GPU Scene sets (`SCATTER_PUSH_BYTES`).
pub fn scatter_shader(library: &ShaderLibrary) -> Result<ShaderModule<'_>, ShaderError> {
    let e = library
        .entry(SCATTER)
        .ok_or(ShaderError::MissingEntry(SCATTER))?;
    let mismatch = |problem| ShaderError::Mismatch {
        entry: SCATTER,
        problem,
    };
    if e.reflection.entry.stage != Stage::Compute {
        return Err(mismatch("not a compute shader"));
    }
    if e.reflection.push.as_ref().map(|p| p.size) != Some(marlin_render::scene::SCATTER_PUSH_BYTES)
    {
        return Err(mismatch(
            "its push constants are not what the GPU Scene's scatter sets",
        ));
    }
    Ok(module(e))
}

/// The streaming passes' entries (`Marlin.Engine.Streaming`).
pub const STREAMING: [&str; 2] = [
    "Marlin.Engine.Streaming.PageUpload",
    "Marlin.Engine.Streaming.ClearWords",
];

/// The page pool's streaming shaders from the engine's library, checked for their stage and the
/// push constants marlin-render's `Meshes` sets.
pub fn streaming_shaders(library: &ShaderLibrary) -> Result<StreamingShaders<'_>, ShaderError> {
    let get = |name: &'static str, push: u32| {
        let e = library.entry(name).ok_or(ShaderError::MissingEntry(name))?;
        let mismatch = |problem| ShaderError::Mismatch {
            entry: name,
            problem,
        };
        if e.reflection.entry.stage != Stage::Compute {
            return Err(mismatch("not a compute shader"));
        }
        if e.reflection.push.as_ref().map(|p| p.size) != Some(push) {
            return Err(mismatch(
                "its push constants are not what the streaming passes set",
            ));
        }
        Ok(module(e))
    };
    Ok(StreamingShaders {
        upload: get(STREAMING[0], marlin_render::UPLOAD_PUSH_BYTES)?,
        clear: get(STREAMING[1], marlin_render::CLEAR_PUSH_BYTES)?,
    })
}

/// The visibility pipeline's entries (`Marlin.Engine.Visibility`), in
/// `VisibilityShaders::ENTRIES` order.
pub const VISIBILITY: [&str; 17] = [
    "Marlin.Engine.Visibility.Reset",
    "Marlin.Engine.Visibility.InstanceCull",
    "Marlin.Engine.Visibility.InstanceRetestArgs",
    "Marlin.Engine.Visibility.InstanceRetest",
    "Marlin.Engine.Visibility.NodeArgs",
    "Marlin.Engine.Visibility.NodeCull",
    "Marlin.Engine.Visibility.PartArgs",
    "Marlin.Engine.Visibility.ClusterCull",
    "Marlin.Engine.Visibility.ClusterRetestArgs",
    "Marlin.Engine.Visibility.ClusterRetest",
    "Marlin.Engine.Visibility.FinishArgs",
    "Marlin.Engine.Visibility.VisMesh",
    "Marlin.Engine.Visibility.VisPs",
    "Marlin.Engine.Visibility.VisClear",
    "Marlin.Engine.Visibility.VisDepth",
    "Marlin.Engine.Visibility.HzbFirst",
    "Marlin.Engine.Visibility.HzbDown",
];

/// The visibility pipeline's shaders from the engine's library, checked for their stages and
/// the push constants marlin-render's visibility passes set.
pub fn visibility_shaders(library: &ShaderLibrary) -> Result<VisibilityShaders<'_>, ShaderError> {
    use marlin_render::visibility::{
        CULL_PUSH_BYTES, HZB_PUSH_BYTES, RASTER_PUSH_BYTES, TARGET_PUSH_BYTES,
    };
    let get = |name: &'static str| {
        let short = name.rsplit('.').next().unwrap_or(name);
        let (stage, push) = match short {
            "VisMesh" => (Stage::Mesh, RASTER_PUSH_BYTES),
            "VisPs" => (Stage::Pixel, RASTER_PUSH_BYTES),
            "VisClear" | "VisDepth" => (Stage::Compute, TARGET_PUSH_BYTES),
            "HzbFirst" | "HzbDown" => (Stage::Compute, HZB_PUSH_BYTES),
            _ => (Stage::Compute, CULL_PUSH_BYTES),
        };
        let e = library.entry(name).ok_or(ShaderError::MissingEntry(name))?;
        let mismatch = |problem| ShaderError::Mismatch {
            entry: name,
            problem,
        };
        if e.reflection.entry.stage != stage {
            return Err(mismatch("not the stage the visibility passes use it as"));
        }
        if e.reflection.push.as_ref().map(|p| p.size) != Some(push) {
            return Err(mismatch(
                "its push constants are not what the visibility passes set",
            ));
        }
        Ok(module(e))
    };
    let mut modules = alloc::vec::Vec::with_capacity(VISIBILITY.len());
    for name in VISIBILITY {
        modules.push(get(name)?);
    }
    let mut next = modules.into_iter();
    Ok(VisibilityShaders::from_fn(|_| {
        next.next().expect("one module an entry")
    }))
}

/// The material passes' entries (`Marlin.Engine.Material`).
pub const MATERIAL: [&str; 4] = [
    "Marlin.Engine.Material.Classify",
    "Marlin.Engine.Material.Bins",
    "Marlin.Engine.Material.Scatter",
    "Marlin.Engine.Material.MaterialDebug",
];

/// The direct lighting entry.
pub const DIRECT_LIGHT: &str = "Marlin.Engine.Lighting.DirectLight";

/// An entry of `library` checked for its stage and push constant size.
fn checked<'a>(
    library: &'a ShaderLibrary,
    name: &'static str,
    stage: Stage,
    push: u32,
) -> Result<ShaderModule<'a>, ShaderError> {
    let e = library.entry(name).ok_or(ShaderError::MissingEntry(name))?;
    let mismatch = |problem| ShaderError::Mismatch {
        entry: name,
        problem,
    };
    if e.reflection.entry.stage != stage {
        return Err(mismatch("not the stage the engine uses it as"));
    }
    if e.reflection.push.as_ref().map(|p| p.size) != Some(push) {
        return Err(mismatch("its push constants are not what the engine sets"));
    }
    Ok(module(e))
}

/// The material passes' shaders from the engine's library (marlin-material's sizes).
pub fn material_shaders(library: &ShaderLibrary) -> Result<MaterialShaders<'_>, ShaderError> {
    use marlin_material::{CLASSIFY_PUSH_BYTES, DEBUG_PUSH_BYTES};
    Ok(MaterialShaders {
        classify: checked(library, MATERIAL[0], Stage::Compute, CLASSIFY_PUSH_BYTES)?,
        bins: checked(library, MATERIAL[1], Stage::Compute, CLASSIFY_PUSH_BYTES)?,
        scatter: checked(library, MATERIAL[2], Stage::Compute, CLASSIFY_PUSH_BYTES)?,
        debug: checked(library, MATERIAL[3], Stage::Compute, DEBUG_PUSH_BYTES)?,
    })
}

/// The direct lighting shader from the engine's library.
pub fn lighting_shader(library: &ShaderLibrary) -> Result<ShaderModule<'_>, ShaderError> {
    checked(
        library,
        DIRECT_LIGHT,
        Stage::Compute,
        marlin_render::LIGHTING_PUSH_BYTES,
    )
}

/// The resolve entry of a material's permutation library (a cooked `Shader` of
/// `Marlin.Engine.Material.ResolvePixel`).
pub fn resolve_shader(permutation: &ShaderLibrary) -> Result<ShaderModule<'_>, ShaderError> {
    checked(
        permutation,
        RESOLVE,
        Stage::Compute,
        marlin_material::RESOLVE_PUSH_BYTES,
    )
}

/// The material resolve entry every permutation instantiates.
pub const RESOLVE: &str = "Marlin.Engine.Material.ResolvePixel";

/// The UI renderer's shaders from the engine's library (`Marlin.Engine.Ui`), checked for their
/// stages and the push constants marlin-ui-render sets.
pub fn ui_shaders(library: &ShaderLibrary) -> Result<UiShaders<'_>, ShaderError> {
    use marlin_ui_render::{UI_ENTRIES, UI_PUSH_BYTES};
    const STAGES: [Stage; 7] = [
        Stage::Compute,
        Stage::Compute,
        Stage::Compute,
        Stage::Compute,
        Stage::Compute,
        Stage::Vertex,
        Stage::Pixel,
    ];
    let get = |i: usize| {
        let name = UI_ENTRIES[i];
        let e = library.entry(name).ok_or(ShaderError::MissingEntry(name))?;
        let mismatch = |problem| ShaderError::Mismatch {
            entry: name,
            problem,
        };
        if e.reflection.entry.stage != STAGES[i] {
            return Err(mismatch("not the stage the UI renderer uses it as"));
        }
        if e.reflection.push.as_ref().map(|p| p.size) != UI_PUSH_BYTES[i] {
            return Err(mismatch(
                "its push constants are not what the UI renderer sets",
            ));
        }
        Ok(module(e))
    };
    Ok(UiShaders {
        glyph_upload: get(0)?,
        bounds: get(1)?,
        bin: get(2)?,
        coarse: get(3)?,
        fine: get(4)?,
        encode_vs: get(5)?,
        encode_ps: get(6)?,
    })
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
