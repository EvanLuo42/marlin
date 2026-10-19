//! The engine's shaders from mounted content: found by name (the package and the default
//! material's resolve permutation), each entry checked for the stage and push constants the
//! engine's passes use.

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, platform_id, unit_id};
use marlin_asset::{AssetServer, MemorySource, Uuid, Vfs};
use marlin_framework::loaders::engine_loaders;
use marlin_framework::shaders::{
    DEFAULT_RESOLVE, DIRECT_LIGHT, ENGINE_SHADERS, RESOLVE, SCATTER, STREAMING,
};
use marlin_framework::{
    ShaderError, lighting_shader, load_default_resolve, load_engine_shaders, resolve_shader,
    scatter_shader, streaming_shaders,
};
use marlin_material::RESOLVE_PUSH_BYTES;
use marlin_reflect::type_info;
use marlin_render::scene::SCATTER_PUSH_BYTES;
use marlin_render::{CLEAR_PUSH_BYTES, LIGHTING_PUSH_BYTES, UPLOAD_PUSH_BYTES};
use marlin_shader::asset::{LibraryEntry, ShaderLibrary};
use marlin_shader::reflect::{EntryInfo, ShaderReflection, Stage, StructLayout};

const PACKAGE: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0001);

fn entry(name: &str, stage: Stage, push_size: u32) -> LibraryEntry {
    LibraryEntry {
        name: name.into(),
        code: 0x0723_0203u32.to_le_bytes().to_vec(),
        reflection: ShaderReflection {
            entry: EntryInfo {
                name: name.into(),
                stage,
                group_size: [8, 8, 1],
            },
            push: Some(StructLayout {
                name: "Args".into(),
                size: push_size,
                fields: Vec::new(),
            }),
            ..ShaderReflection::default()
        },
    }
}

fn library(entries: Vec<LibraryEntry>) -> ShaderLibrary {
    ShaderLibrary {
        package: ENGINE_SHADERS.into(),
        target: "vulkan-1.4".into(),
        entries,
    }
}

fn lighting() -> ShaderLibrary {
    library(vec![entry(
        DIRECT_LIGHT,
        Stage::Compute,
        LIGHTING_PUSH_BYTES,
    )])
}

/// Content with `library` cooked, under `name` if given.
fn content(library: &ShaderLibrary, name: Option<&str>) -> Vfs {
    let bytes = unit(
        unit_id("shaders/package"),
        platform_id("windows"),
        vec![UnitObject {
            uuid: PACKAGE,
            type_id: type_info::<ShaderLibrary>().id.0,
            encoding: Encoding::Serial,
            payload: marlin_serial::to_bytes(library).unwrap(),
            bulk: Vec::new(),
        }],
        &[],
    );
    let mut source = MemorySource::new();
    source.insert(bytes).unwrap();
    if let Some(name) = name {
        source.name(name, PACKAGE);
    }
    let mut vfs = Vfs::new();
    vfs.mount("engine", source);
    vfs
}

#[test]
fn the_library_loads_by_package_name() {
    let vfs = content(&lighting(), Some(ENGINE_SHADERS));
    let mut assets = AssetServer::new(loaders());
    let handle = load_engine_shaders(&mut assets, &vfs, None).unwrap();
    let lib = assets.get(&handle).unwrap();
    assert_eq!(lib, &lighting());
    let module = lighting_shader(lib).unwrap();
    assert_eq!(module.code, &lib.entry(DIRECT_LIGHT).unwrap().code[..]);
    assert_eq!(module.entry, "main");
}

#[test]
fn content_without_the_package_says_so() {
    let vfs = content(&lighting(), None);
    let mut assets = AssetServer::new(loaders());
    let e = load_engine_shaders(&mut assets, &vfs, None).unwrap_err();
    assert!(matches!(e, ShaderError::NotMounted), "{e}");
    assert!(e.to_string().contains("content/engine"), "{e}");
}

#[test]
fn entries_must_have_the_stage_and_push_constants_the_engine_uses() {
    assert!(matches!(
        lighting_shader(&library(Vec::new())),
        Err(ShaderError::MissingEntry(DIRECT_LIGHT))
    ));
    let wrong_stage = library(vec![entry(DIRECT_LIGHT, Stage::Pixel, LIGHTING_PUSH_BYTES)]);
    let e = lighting_shader(&wrong_stage).unwrap_err();
    assert!(e.to_string().contains("stage"), "{e}");
    let wrong_push = library(vec![entry(DIRECT_LIGHT, Stage::Compute, 12)]);
    let e = lighting_shader(&wrong_push).unwrap_err();
    assert!(e.to_string().contains("push constants"), "{e}");
}

#[test]
fn the_default_resolve_loads_by_name() {
    let permutation = library(vec![entry(RESOLVE, Stage::Compute, RESOLVE_PUSH_BYTES)]);
    let vfs = content(&permutation, Some(DEFAULT_RESOLVE));
    let mut assets = AssetServer::new(loaders());
    let handle = load_default_resolve(&mut assets, &vfs, None).unwrap();
    assert!(resolve_shader(assets.get(&handle).unwrap()).is_ok());
    let short = library(vec![entry(RESOLVE, Stage::Compute, 64)]);
    assert!(resolve_shader(&short).is_err());
}

#[test]
fn the_scatter_is_a_compute_shader_with_the_gpu_scenes_push() {
    assert!(matches!(
        scatter_shader(&lighting()),
        Err(ShaderError::MissingEntry(SCATTER))
    ));
    let ok = library(vec![entry(SCATTER, Stage::Compute, SCATTER_PUSH_BYTES)]);
    assert!(scatter_shader(&ok).is_ok());
    let wrong_push = library(vec![entry(SCATTER, Stage::Compute, SCATTER_PUSH_BYTES + 4)]);
    assert!(matches!(
        scatter_shader(&wrong_push),
        Err(ShaderError::Mismatch { entry: SCATTER, .. })
    ));
    let wrong_stage = library(vec![entry(SCATTER, Stage::Pixel, SCATTER_PUSH_BYTES)]);
    assert!(scatter_shader(&wrong_stage).is_err());
}

#[test]
fn the_streaming_passes_are_compute_shaders_with_the_pools_push() {
    // Entries sorted by name, as the cooker writes them.
    let ok = library(vec![
        entry(STREAMING[1], Stage::Compute, CLEAR_PUSH_BYTES),
        entry(STREAMING[0], Stage::Compute, UPLOAD_PUSH_BYTES),
    ]);
    assert!(streaming_shaders(&ok).is_ok());
    assert!(matches!(
        streaming_shaders(&library(vec![entry(STREAMING[0], Stage::Compute, UPLOAD_PUSH_BYTES)])),
        Err(ShaderError::MissingEntry(e)) if e == STREAMING[1]
    ));
    let wrong_push = library(vec![
        entry(STREAMING[1], Stage::Compute, CLEAR_PUSH_BYTES),
        entry(STREAMING[0], Stage::Compute, UPLOAD_PUSH_BYTES + 4),
    ]);
    assert!(matches!(
        streaming_shaders(&wrong_push),
        Err(ShaderError::Mismatch { .. })
    ));
}

/// The engine's loaders, with a streamer nothing drives (these assets stream nothing).
fn loaders() -> marlin_asset::Loaders {
    engine_loaders(&std::sync::Arc::new(marlin_asset::BlobStreamer::new()))
}
