//! The UI renderer's engine assets: its shader entries checked, and the UI font found by name.

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, platform_id, unit_id};
use marlin_asset::{AssetError, AssetServer, MemorySource, Uuid, Vfs};
use marlin_font::Font;
use marlin_framework::loaders::engine_loaders;
use marlin_framework::shaders::ENGINE_SHADERS;
use marlin_framework::{ShaderError, UI_FONT, load_ui_font, ui_shaders};
use marlin_reflect::type_info;
use marlin_shader::asset::{LibraryEntry, ShaderLibrary};
use marlin_shader::reflect::{EntryInfo, ShaderReflection, Stage, StructLayout};
use marlin_ui_render::{UI_ENTRIES, UI_PUSH_BYTES};

const FONT: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0f0f);

/// The UI entries, sorted by name as cooked libraries are.
fn library(push: impl Fn(usize) -> Option<u32>) -> ShaderLibrary {
    let stages = [
        Stage::Compute,
        Stage::Compute,
        Stage::Compute,
        Stage::Compute,
        Stage::Compute,
        Stage::Vertex,
        Stage::Pixel,
        Stage::Compute,
    ];
    let mut entries: Vec<LibraryEntry> = (0..UI_ENTRIES.len())
        .map(|i| LibraryEntry {
            name: UI_ENTRIES[i].into(),
            code: 0x0723_0203u32.to_le_bytes().to_vec(),
            reflection: ShaderReflection {
                entry: EntryInfo {
                    name: UI_ENTRIES[i].into(),
                    stage: stages[i],
                    group_size: [16, 16, 1],
                },
                push: push(i).map(|size| StructLayout {
                    name: "Args".into(),
                    size,
                    fields: Vec::new(),
                }),
                ..ShaderReflection::default()
            },
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    ShaderLibrary {
        package: ENGINE_SHADERS.into(),
        target: "vulkan-1.4".into(),
        entries,
    }
}

#[test]
fn ui_entries_must_take_what_the_renderer_pushes() {
    assert!(ui_shaders(&library(|i| UI_PUSH_BYTES[i])).is_ok());
    let Err(e) = ui_shaders(&library(|i| UI_PUSH_BYTES[i].map(|s| s + 4))) else {
        panic!("wrong push constants pass")
    };
    assert!(
        matches!(e, ShaderError::Mismatch { entry, .. } if entry == UI_ENTRIES[0]),
        "{e}"
    );
    let mut missing = library(|i| UI_PUSH_BYTES[i]);
    missing.entries.retain(|e| e.name != UI_ENTRIES[4]);
    assert!(matches!(
        ui_shaders(&missing),
        Err(ShaderError::MissingEntry(e)) if e == UI_ENTRIES[4]
    ));
}

fn content(name: Option<&str>) -> Vfs {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../content/engine/fonts/Inter-Regular.ttf");
    let file = std::fs::read(path).expect("content/engine/fonts/Inter-Regular.ttf (Git LFS)");
    let bytes = unit(
        unit_id("fonts/Inter"),
        platform_id("windows"),
        vec![UnitObject {
            uuid: FONT,
            type_id: type_info::<Font>().id.0,
            encoding: Encoding::Cooked,
            payload: marlin_cook_font::cook_font(&file, 0).unwrap(),
            bulk: Vec::new(),
        }],
        &[],
    );
    let mut source = MemorySource::new();
    source.insert(bytes).unwrap();
    if let Some(name) = name {
        source.name(name, FONT);
    }
    let mut vfs = Vfs::new();
    vfs.mount("engine", source);
    vfs
}

#[test]
fn the_ui_font_loads_by_name() {
    let vfs = content(Some(UI_FONT));
    let mut assets = AssetServer::new(loaders());
    let handle = load_ui_font(&mut assets, &vfs, None).unwrap();
    let font = assets.get(&handle).unwrap().view();
    assert_eq!(font.header.units_per_em, 2048);
    assert!(font.glyph_of('A').is_some());

    let vfs = content(None);
    let mut assets = AssetServer::new(loaders());
    assert!(matches!(
        load_ui_font(&mut assets, &vfs, None),
        Err(AssetError::NotFound)
    ));
}

/// The engine's loaders, with a streamer nothing drives (these assets stream nothing).
fn loaders() -> marlin_asset::Loaders {
    engine_loaders(&std::sync::Arc::new(marlin_asset::BlobStreamer::new()))
}
