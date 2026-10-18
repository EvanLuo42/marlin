//! The engine's shaders: the cooked `Marlin.Engine` library from the mounted content
//! (`content/engine/shaders/`, `docs/design/0003-asset-pipeline.md` §4.4), found by package name
//! and then by entry path. Nothing here names a shader by path or UUID.

use alloc::vec::Vec;
use core::fmt;

use marlin_asset::{AssetError, AssetServer, Handle, LoadState, Vfs};
use marlin_jobs::JobSystem;
use marlin_render::{FORWARD_PUSH_BYTES, ForwardShaders, PostShaders};
use marlin_rhi::ShaderModule;
use marlin_shader::asset::{LibraryEntry, ShaderLibrary};
use marlin_shader::reflect::{Stage, StageBinding};

/// The engine's shader package.
pub const ENGINE_SHADERS: &str = "Marlin.Engine";
pub const FORWARD_VS: &str = "Marlin.Engine.ForwardVs";
pub const FORWARD_PS: &str = "Marlin.Engine.ForwardPs";

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
    let uuid = vfs.named(ENGINE_SHADERS).ok_or(ShaderError::NotMounted)?;
    let handle = assets.load::<ShaderLibrary>(uuid);
    assets.update_blocking(vfs, jobs);
    match assets.state(&handle) {
        LoadState::Loaded => Ok(handle),
        LoadState::Failed(e) => Err(ShaderError::Load(e)),
        LoadState::Loading => Err(ShaderError::Load(AssetError::NotFound)),
    }
}

/// The forward pass's shaders from the engine's library, checked against what the renderer
/// expects (stages, push constants, one color target) and against each other (every pixel
/// input is a vertex output of the same type), so a mismatch is a clear error at startup rather
/// than a pipeline the driver rejects.
pub fn forward_shaders(library: &ShaderLibrary) -> Result<ForwardShaders<'_>, ShaderError> {
    let vs = entry(library, FORWARD_VS, Stage::Vertex)?;
    let ps = entry(library, FORWARD_PS, Stage::Pixel)?;
    let mismatch = |problem| ShaderError::Mismatch {
        entry: FORWARD_PS,
        problem,
    };
    for input in &ps.reflection.inputs {
        let StageBinding::Location(at) = input.binding else {
            continue;
        };
        match vs
            .reflection
            .outputs
            .iter()
            .find(|o| o.binding == StageBinding::Location(at))
        {
            Some(output) if output.ty == input.ty => {}
            Some(_) => return Err(mismatch("an input's type differs from the vertex output's")),
            None => return Err(mismatch("reads an input the vertex shader does not write")),
        }
    }
    let targets: Vec<u32> = ps
        .reflection
        .outputs
        .iter()
        .filter_map(|o| match o.binding {
            StageBinding::Target { index, .. } => Some(index),
            _ => None,
        })
        .collect();
    if targets != [0] {
        return Err(mismatch("does not write exactly render target 0"));
    }
    Ok(ForwardShaders {
        vertex: module(vs),
        pixel: module(ps),
    })
}

fn entry<'a>(
    library: &'a ShaderLibrary,
    name: &'static str,
    stage: Stage,
) -> Result<&'a LibraryEntry, ShaderError> {
    let e = library.entry(name).ok_or(ShaderError::MissingEntry(name))?;
    let mismatch = |problem| ShaderError::Mismatch {
        entry: name,
        problem,
    };
    if e.reflection.entry.stage != stage {
        return Err(mismatch("not the stage the forward pass uses it as"));
    }
    match &e.reflection.push {
        Some(push) if push.size == FORWARD_PUSH_BYTES => Ok(e),
        _ => Err(mismatch(
            "its push constants are not the forward pass's six 32-bit words",
        )),
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
/// marlin-render's GPU Scene sets (eight 32-bit words).
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
    if e.reflection.push.as_ref().map(|p| p.size) != Some(32) {
        return Err(mismatch(
            "its push constants are not what the GPU Scene's scatter sets",
        ));
    }
    Ok(module(e))
}
