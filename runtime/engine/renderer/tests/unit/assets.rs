//! The engine's shaders from mounted content: found by name (the package and the default
//! material's resolve permutation), then bound by the subsystems that use them.

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, platform_id, unit_id};
use marlin_asset::{AssetServer, MemorySource, Uuid, Vfs};
use marlin_material::RESOLVE;
use marlin_reflect::type_info;
use marlin_render::{Lighting, ScatterShader};
use marlin_renderer::{
    DEFAULT_RESOLVE, ENGINE_SHADERS, ShaderError, load_default_resolve, load_engine_shaders,
};
use marlin_shader::asset::{LibraryEntry, ShaderLibrary};
use marlin_shader::bind::bind_one;
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
    let push = Lighting::ENTRY.push.unwrap();
    library(vec![entry(Lighting::ENTRY.name, Stage::Compute, push)])
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
fn a_subsystem_binds_its_entries_from_the_loaded_library() {
    let vfs = content(&lighting(), Some(ENGINE_SHADERS));
    let mut assets = AssetServer::new(loaders());
    let handle = load_engine_shaders(&mut assets, &vfs, None).unwrap();
    let lib = assets.get(&handle).unwrap();
    let module = bind_one(lib, &Lighting::ENTRY).unwrap();
    assert_eq!(
        module.code,
        &lib.entry(Lighting::ENTRY.name).unwrap().code[..]
    );
    assert_eq!(module.entry, "main");
    let e = bind_one(lib, &ScatterShader::ENTRY).unwrap_err();
    assert!(e.to_string().contains(ScatterShader::ENTRY.name), "{e}");
}

#[test]
fn the_default_resolve_loads_by_name() {
    let permutation = library(vec![entry(
        RESOLVE.name,
        Stage::Compute,
        RESOLVE.push.unwrap(),
    )]);
    let vfs = content(&permutation, Some(DEFAULT_RESOLVE));
    let mut assets = AssetServer::new(loaders());
    let handle = load_default_resolve(&mut assets, &vfs, None).unwrap();
    assert!(bind_one(assets.get(&handle).unwrap(), &RESOLVE).is_ok());
    let short = library(vec![entry(RESOLVE.name, Stage::Compute, 64)]);
    assert!(bind_one(&short, &RESOLVE).is_err());
}

/// The render module's loaders (with a streamer nothing drives: these assets stream nothing).
fn loaders() -> marlin_asset::Loaders {
    crate::modules()
        .table()
        .loaders(&marlin_world::Resources::new())
}
