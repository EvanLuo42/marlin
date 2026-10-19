//! The UI renderer's engine font, found by name.

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, platform_id, unit_id};
use marlin_asset::{AssetError, AssetServer, MemorySource, Uuid, Vfs};
use marlin_font::Font;
use marlin_reflect::type_info;
use marlin_ui_render::{UI_FONT, UiModule, load_ui_font};
use marlin_world::{Modules, Resources};

const FONT: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0f0f);

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

/// The UI module's loaders.
fn loaders() -> marlin_asset::Loaders {
    let modules = Modules::new(vec![Box::new(UiModule)]).unwrap();
    modules.table().loaders(&Resources::new())
}
