//! The Marlin editor.

// Link the whole runtime and every editor library.
use marlin as _;
use marlin_editor_asset_tools as _;
use marlin_editor_core as _;
use marlin_editor_ui_core as _;
use marlin_editor_ui_theme as _;
use marlin_editor_ui_widgets as _;
use marlin_editor_viewport as _;
use marlin_editor_world_tools as _;

#[global_allocator]
static HEAP: marlin_mem::MarlinHeap = marlin_mem::MarlinHeap::new();

fn main() {
    if let Err(missing) = marlin_rt::hosted::init() {
        eprintln!("marlin-editor: {missing}");
        std::process::exit(1);
    }
}
