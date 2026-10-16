//! The Marlin runtime: every runtime crate in one library, linked by the game and the editor.
#![cfg_attr(not(test), no_std)]

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
pub use marlin_hash as hash;
pub use marlin_input as input;
pub use marlin_jobs as jobs;
pub use marlin_math as math;
pub use marlin_nav as nav;
pub use marlin_net as net;
pub use marlin_physics as physics;
pub use marlin_profile as profile;
pub use marlin_reflect as reflect;
pub use marlin_render as render;
pub use marlin_render_graph as render_graph;
pub use marlin_rhi as rhi;
pub use marlin_script as script;
pub use marlin_serial as serial;
pub use marlin_services as services;
pub use marlin_shader as shader;
pub use marlin_text as text;
pub use marlin_ui_render as ui_render;
pub use marlin_vfx as vfx;
pub use marlin_vm as vm;
pub use marlin_vm_bytecode as vm_bytecode;
pub use marlin_world as world;
