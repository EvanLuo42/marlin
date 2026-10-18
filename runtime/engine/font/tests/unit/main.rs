//! marlin-font's tests (quick tier, docs/design/0014-testing.md §5): rasterization against
//! supersampled references, the skyline packer, the glyph cache's ids, LRU and table, and
//! layout. Fonts are the engine's Inter, cooked here with marlin-cook-font.
//! Run alone with `cargo test -p marlin-font --test unit`.

mod cache;
mod layout;
mod pack;
mod raster;

use std::sync::OnceLock;

use marlin_font::FontView;

/// The cooked payload of the engine's Inter.
pub fn inter_payload() -> &'static [u8] {
    static PAYLOAD: OnceLock<Vec<u8>> = OnceLock::new();
    PAYLOAD.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../content/engine/fonts/Inter-Regular.ttf");
        let file = std::fs::read(path)
            .expect("content/engine/fonts/Inter-Regular.ttf (Git LFS: run `git lfs pull`)");
        marlin_cook_font::cook_font(&file, 0).unwrap()
    })
}

pub fn inter() -> FontView<'static> {
    FontView::new(inter_payload()).unwrap()
}
