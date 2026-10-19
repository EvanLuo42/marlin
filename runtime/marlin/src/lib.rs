//! The Marlin runtime: every runtime crate in one library, linked by the game and the editor,
//! and the engine modules of each build target ([`game_modules`]; 0019 §3.2).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub use marlin_std as std;

pub use marlin_anim as anim;
pub use marlin_app as app;
pub use marlin_asset as asset;
pub use marlin_audio as audio;
pub use marlin_audio_device as audio_device;
pub use marlin_compress as compress;
pub use marlin_config as config;
pub use marlin_containers as containers;
pub use marlin_ecs as ecs;
pub use marlin_font as font;
pub use marlin_framework as framework;
pub use marlin_gameplay as gameplay;
pub use marlin_hash as hash;
pub use marlin_input as input;
pub use marlin_input_action as input_action;
pub use marlin_jobs as jobs;
pub use marlin_math as math;
pub use marlin_nav as nav;
pub use marlin_net as net;
pub use marlin_physics as physics;
pub use marlin_profile as profile;
pub use marlin_reflect as reflect;
pub use marlin_render as render;
pub use marlin_render_graph as render_graph;
pub use marlin_renderer as renderer;
pub use marlin_rhi as rhi;
pub use marlin_serial as serial;
pub use marlin_services as services;
pub use marlin_shader as shader;
pub use marlin_text as text;
pub use marlin_ui_render as ui_render;
pub use marlin_vfx as vfx;
pub use marlin_world as world;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use marlin_world::Module;

/// The engine modules of the game build target, in dependency order. Projects do not choose
/// modules: every game has these (0019 §3.2). The editor adds its own; a server build would drop
/// the renderer, the UI and the development tools.
pub fn game_modules() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(marlin_world::WorldModule),
        Box::new(marlin_anim::AnimModule),
        Box::new(marlin_input_action::InputModule::new()),
        Box::new(marlin_ui_render::UiModule),
        Box::new(marlin_renderer::RenderModule),
        Box::new(marlin_gameplay::GameplayModule::new()),
        #[cfg(not(marlin_build = "shipping"))]
        Box::new(marlin_framework::DevToolsModule),
    ]
}
