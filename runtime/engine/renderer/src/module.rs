//! [`RenderModule`]: the renderer's part in the engine (0019 §7).

use alloc::sync::Arc;

use marlin_asset::BlobStreamer;
use marlin_ecs::IntoSystem;
use marlin_reflect::type_info;
use marlin_render::asset::{Mesh, PairingPattern, Texture};
use marlin_shader::asset::ShaderLibrary;
use marlin_world::{
    ComponentFlags, EngineContext, Module, ModuleError, Registry, TickGroup, World, WorldModule,
};

use crate::assets::RenderAssets;
use crate::components::{Camera, LevelSettings, MeshRenderer, PointLight, SpotLight};
use crate::extract::{RenderExtract, RenderFrame, extract, finish_frame, frame_ready};
use crate::meshes::{LoadedMesh, load_mesh};

/// The render module: its components and their extract, the loaders of what it draws, and its
/// engine assets ([`RenderAssets`], an engine resource once started).
///
/// The world renderers themselves are the host's ([`crate::WorldRenderer`]): they draw on the
/// render side while their worlds simulate the next frame.
pub struct RenderModule;

impl RenderModule {
    pub const NAME: &'static str = "Render";
}

impl Module for RenderModule {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &[WorldModule::NAME]
    }

    fn register(&self, r: &mut Registry) {
        r.components()
            .add::<MeshRenderer>(ComponentFlags::SAVED)
            .add::<Camera>(ComponentFlags::SAVED)
            .add::<PointLight>(ComponentFlags::SAVED)
            .add::<SpotLight>(ComponentFlags::SAVED)
            .add::<LevelSettings>(ComponentFlags::SAVED);
        r.add_system(TickGroup::Extract, || {
            (extract as fn(&mut marlin_ecs::World)).named("render_extract")
        });
        r.add_loaders(|loaders, resources| {
            // Meshes read their streamed pages through the engine's streamer.
            let streamer = resources
                .get::<Arc<BlobStreamer>>()
                .cloned()
                .unwrap_or_else(|| Arc::new(BlobStreamer::new()));
            loaders.register::<LoadedMesh>(type_info::<Mesh>().id.0, move |ctx| {
                load_mesh(ctx, &streamer)
            });
            loaders.register_in_place::<Texture>();
            loaders.register_in_place::<PairingPattern>();
            loaders.register_serial::<ShaderLibrary>();
        });
        crate::settings::add_cvars(r);
    }

    fn startup(&mut self, cx: &mut EngineContext<'_, '_>) -> Result<(), ModuleError> {
        let assets = RenderAssets::load(cx.assets, cx.vfs, Some(cx.jobs))
            .map_err(|e| ModuleError::new(Self::NAME, alloc::format!("{e}")))?;
        cx.resources.insert(assets);
        Ok(())
    }

    fn init_world(&mut self, world: &mut World, _: &mut EngineContext<'_, '_>) {
        world.ecs_mut().insert_resource(RenderExtract::new());
        world.ecs_mut().insert_resource(RenderFrame::default());
    }

    fn post_world_frame(&mut self, world: &mut World, cx: &mut EngineContext<'_, '_>) {
        finish_frame(world, cx.assets);
    }

    fn world_ready(&self, world: &World) -> bool {
        frame_ready(world)
    }
}
