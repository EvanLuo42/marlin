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
    // Panics go through marlin_diag (with a backtrace and a crash dump) instead of std's printer.
    std::panic::set_hook(Box::new(|info| {
        let message = info.payload_as_str().unwrap_or("Box<dyn Any>");
        marlin_rt::hosted::report_panic(&message, info.location());
    }));
}
