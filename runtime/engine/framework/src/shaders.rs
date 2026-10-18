//! The engine's shaders: the cooked `Marlin.Engine` library from the mounted content
//! (`content/engine/shaders/`, `docs/design/0003-asset-pipeline.md` §4.4), found by package name
//! and then by entry path. Nothing here names a shader by path or UUID.

use alloc::vec::Vec;
use core::fmt;

use marlin_asset::{AssetError, AssetServer, Handle, LoadState, Vfs};
use marlin_jobs::JobSystem;
use marlin_render::{FORWARD_PUSH_BYTES, ForwardShaders};
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
                 content/engine and mount it (content.package)"
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
            "its push constants are not the forward pass's four 32-bit words",
        )),
    }
}

fn module(entry: &LibraryEntry) -> ShaderModule<'_> {
    ShaderModule {
        code: &entry.code,
        entry: MODULE_ENTRY,
    }
}
