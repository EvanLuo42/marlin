//! The engine's shaders from mounted content: found by package name, checked against what the
//! forward pass and the GPU Scene use.

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, platform_id, unit_id};
use marlin_asset::{AssetServer, MemorySource, Uuid, Vfs};
use marlin_framework::loaders::engine_loaders;
use marlin_framework::shaders::{ENGINE_SHADERS, FORWARD_PS, FORWARD_VS, SCATTER};
use marlin_framework::{ShaderError, forward_shaders, load_engine_shaders, scatter_shader};
use marlin_reflect::type_info;
use marlin_render::FORWARD_PUSH_BYTES;
use marlin_shader::asset::{LibraryEntry, ShaderLibrary};
use marlin_shader::reflect::{
    EntryInfo, ShaderReflection, Stage, StageBinding, StageVariable, StructLayout,
};

const PACKAGE: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0001);

fn var(name: &str, ty: &str, binding: StageBinding) -> StageVariable {
    StageVariable {
        name: name.into(),
        ty: ty.into(),
        binding,
    }
}

/// An entry with the forward pass's stage I/O: the vertex stage writes a normal and a color, the
/// pixel stage reads them and writes target 0.
fn entry(name: &str, stage: Stage, push_size: u32) -> LibraryEntry {
    let varyings = vec![
        var("Normal", "f32x3", StageBinding::Location(0)),
        var("Color", "f32x4", StageBinding::Location(1)),
    ];
    let (inputs, outputs) = match stage {
        Stage::Pixel => (
            varyings,
            vec![var(
                "",
                "f32x4",
                StageBinding::Target {
                    index: 0,
                    source: 0,
                },
            )],
        ),
        _ => (Vec::new(), varyings),
    };
    LibraryEntry {
        name: name.into(),
        code: 0x0723_0203u32.to_le_bytes().to_vec(),
        reflection: ShaderReflection {
            entry: EntryInfo {
                name: name.into(),
                stage,
                group_size: [1; 3],
            },
            push: Some(StructLayout {
                name: "Marlin.Engine.ForwardArgs".into(),
                size: push_size,
                fields: Vec::new(),
            }),
            inputs,
            outputs,
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

fn forward() -> ShaderLibrary {
    library(vec![
        entry(FORWARD_PS, Stage::Pixel, FORWARD_PUSH_BYTES),
        entry(FORWARD_VS, Stage::Vertex, FORWARD_PUSH_BYTES),
    ])
}

/// Content with `library` cooked under the package's name (`name`: whether it is named).
fn content(library: &ShaderLibrary, name: bool) -> Vfs {
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
    if name {
        source.name(ENGINE_SHADERS, PACKAGE);
    }
    let mut vfs = Vfs::new();
    vfs.mount("engine", source);
    vfs
}

#[test]
fn the_library_loads_by_package_name() {
    let vfs = content(&forward(), true);
    let mut assets = AssetServer::new(engine_loaders());
    let handle = load_engine_shaders(&mut assets, &vfs, None).unwrap();
    let lib = assets.get(&handle).unwrap();
    assert_eq!(lib, &forward());
    let shaders = forward_shaders(lib).unwrap();
    assert_eq!(
        shaders.vertex.code,
        &lib.entry(FORWARD_VS).unwrap().code[..]
    );
    assert_eq!(shaders.pixel.code, &lib.entry(FORWARD_PS).unwrap().code[..]);
    assert_eq!(shaders.vertex.entry, "main");
}

#[test]
fn content_without_the_package_says_so() {
    let vfs = content(&forward(), false);
    let mut assets = AssetServer::new(engine_loaders());
    let e = load_engine_shaders(&mut assets, &vfs, None).unwrap_err();
    assert!(matches!(e, ShaderError::NotMounted), "{e}");
    assert!(e.to_string().contains("content/engine"), "{e}");
}

#[test]
fn entries_must_be_what_the_forward_pass_uses() {
    let missing = library(vec![entry(FORWARD_VS, Stage::Vertex, FORWARD_PUSH_BYTES)]);
    assert!(matches!(
        forward_shaders(&missing),
        Err(ShaderError::MissingEntry(FORWARD_PS))
    ));
    let wrong_stage = library(vec![
        entry(FORWARD_PS, Stage::Pixel, FORWARD_PUSH_BYTES),
        entry(FORWARD_VS, Stage::Pixel, FORWARD_PUSH_BYTES),
    ]);
    assert!(matches!(
        forward_shaders(&wrong_stage),
        Err(ShaderError::Mismatch {
            entry: FORWARD_VS,
            ..
        })
    ));
    let wrong_push = library(vec![
        entry(FORWARD_PS, Stage::Pixel, 12),
        entry(FORWARD_VS, Stage::Vertex, FORWARD_PUSH_BYTES),
    ]);
    let e = forward_shaders(&wrong_push).unwrap_err();
    assert!(e.to_string().contains("push constants"), "{e}");
}

#[test]
fn the_pixel_stage_must_read_what_the_vertex_stage_writes() {
    let with_ps = |edit: fn(&mut ShaderReflection)| {
        let mut lib = forward();
        let ps = lib
            .entries
            .iter_mut()
            .find(|e| e.name == FORWARD_PS)
            .unwrap();
        edit(&mut ps.reflection);
        lib
    };
    assert!(forward_shaders(&forward()).is_ok());

    let unwritten = with_ps(|r| r.inputs.push(var("Uv", "f32x2", StageBinding::Location(2))));
    let e = forward_shaders(&unwritten).unwrap_err();
    assert!(e.to_string().contains("does not write"), "{e}");

    let retyped = with_ps(|r| r.inputs[1].ty = "f32x3".into());
    let e = forward_shaders(&retyped).unwrap_err();
    assert!(e.to_string().contains("type"), "{e}");

    let two_targets = with_ps(|r| {
        r.outputs.push(var(
            "",
            "f32x4",
            StageBinding::Target {
                index: 1,
                source: 0,
            },
        ))
    });
    let e = forward_shaders(&two_targets).unwrap_err();
    assert!(e.to_string().contains("render target 0"), "{e}");
}

#[test]
fn the_scatter_is_a_compute_shader_with_the_gpu_scenes_push() {
    assert!(matches!(
        scatter_shader(&forward()),
        Err(ShaderError::MissingEntry(SCATTER))
    ));
    let ok = library(vec![entry(SCATTER, Stage::Compute, 32)]);
    assert!(scatter_shader(&ok).is_ok());
    let wrong_push = library(vec![entry(SCATTER, Stage::Compute, 28)]);
    assert!(matches!(
        scatter_shader(&wrong_push),
        Err(ShaderError::Mismatch { entry: SCATTER, .. })
    ));
    let wrong_stage = library(vec![entry(SCATTER, Stage::Pixel, 32)]);
    assert!(scatter_shader(&wrong_stage).is_err());
}
