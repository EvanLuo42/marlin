//! What the engine's cooking tools share: the engine's cookers and the derived data cache setup.
//! The `marlin-cook` CLI and `marlin-cookd` are built on it.

use std::io;
use std::net::SocketAddr;
use std::path::Path;

use marlin_anim::asset::{AnimationClip, Skeleton};
use marlin_cook_audio as _;
use marlin_cook_core::{Cookers, Ddc, LayeredDdc, LocalDdc};
use marlin_cook_font::FontCooker;
use marlin_cook_level as _;
use marlin_cook_mesh::{AnimationCooker, MeshCooker, SkeletonCooker};
use marlin_cook_shader::{ShaderCooker, ShaderPackageCooker};
use marlin_cook_text::{StringTable, StringTableCooker};
use marlin_cook_texture::TextureCooker;
use marlin_font::Font;
use marlin_framework::Level;
use marlin_render::asset::{Mesh, Texture};
use marlin_shader::asset::{Shader, ShaderPackage};
use marlin_store::RemoteDdc;

/// Every asset type the engine knows, with its cooker.
pub fn engine_cookers() -> Cookers {
    let mut cookers = Cookers::new();
    let registered = cookers
        .register::<Texture>(TextureCooker)
        .and_then(|()| cookers.register::<Mesh>(MeshCooker))
        .and_then(|()| cookers.register::<Skeleton>(SkeletonCooker))
        .and_then(|()| cookers.register::<AnimationClip>(AnimationCooker))
        .and_then(|()| cookers.register::<StringTable>(StringTableCooker))
        // The engine finds its UI font by name.
        .and_then(|()| cookers.register_named::<Font>(FontCooker))
        .and_then(|()| cookers.register_named::<ShaderPackage>(ShaderPackageCooker::new()))
        // Permutations are found by name too (the default material's resolve).
        .and_then(|()| cookers.register_named::<Shader>(ShaderCooker::new()))
        .and_then(|()| cookers.register_generic::<Level>());
    if let Err(e) = registered {
        // Two engine types with one text name: a programming error, caught by any run.
        panic!("engine asset types: {e}");
    }
    cookers
}

/// A cache: the local one in `dir` (a project's `Project::ddc_dir`), in front of the shared store
/// at `store` if given.
pub fn open_ddc(dir: &Path, store: Option<SocketAddr>) -> io::Result<Box<dyn Ddc>> {
    let local = LocalDdc::open(dir)?;
    Ok(match store {
        None => Box::new(local),
        Some(addr) => {
            let shared = RemoteDdc::connect(addr).map_err(io::Error::from)?;
            Box::new(LayeredDdc::new(local, shared))
        }
    })
}
