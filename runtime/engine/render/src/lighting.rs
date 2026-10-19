//! Direct lighting, R1's minimal set (0010): the environment's sun (lux) and ambient sky (nits)
//! over the material buffer's proxy lobe (`Marlin.Engine.Lighting.DirectLight`), Lambert diffuse
//! and GGX specular (R4d), into an [`HDR_FORMAT`] scene for the post chain. The local lights and
//! GI are ReSTIR's ([`crate::restir`]).

use marlin_render_graph::{Access, PassContext, RenderGraph, TextureDesc, TextureHandle};
use marlin_rhi::{
    ClearValue, ComputePipeline, ComputePipelineDesc, Device, LoadOp, Result, ShaderModule, StoreOp,
};
use marlin_serial::cooked::Pod;
use marlin_shader::bind::EntryDesc;

use marlin_math::Transform;

use crate::meshes::{Camera, Environment};
use crate::post::HDR_FORMAT;

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct LightConstants {
    inverse_view_projection: [f32; 16],
    eye: [f32; 4],
    direction: [f32; 4],
    color: [f32; 4],
    ambient: [f32; 4],
    clear: [f32; 4],
    width: u32,
    height: u32,
    spare: [u32; 2],
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct DirectArgs {
    light: u32,
    material: u32,
    depth: u32,
    output: u32,
    shadow_mask: u32,
    shadowed: u32,
}

/// What [`Lighting::add_passes`] lights: the view's material buffer (`marlin_material`'s
/// `MATERIAL_FORMAT`), its depth (`R32Float`, reverse-Z) and the camera that drew them.
#[derive(Clone, Copy)]
pub struct LitView<'g> {
    pub material: TextureHandle<'g>,
    pub depth: TextureHandle<'g>,
    pub camera: (Transform, Camera),
}

/// The lighting pass's pipeline.
pub struct Lighting {
    direct: ComputePipeline,
}

impl Lighting {
    /// The direct lighting entry.
    pub const ENTRY: EntryDesc =
        EntryDesc::compute("Marlin.Engine.Lighting.DirectLight").push::<DirectArgs>();

    pub fn new(device: &Device, direct: ShaderModule<'_>) -> Result<Self> {
        Ok(Self {
            direct: device.create_compute_pipeline(&ComputePipelineDesc {
                shader: direct,
                cache: None,
            })?,
        })
    }

    /// Declares the lighting of `view` into a new `width` × `height` [`HDR_FORMAT`] scene, and
    /// returns it; the sun is multiplied by `shadow` (a shadow mask, `crate::shadows`) if there
    /// is one. Without a view (no camera), the scene is the clear color.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        view: Option<LitView<'g>>,
        shadow: Option<TextureHandle<'g>>,
        env: &Environment,
        width: u32,
        height: u32,
    ) -> TextureHandle<'g> {
        let desc = TextureDesc::new_2d(width, height, HDR_FORMAT);
        let Some(view) = view else {
            // A render target, cleared: its clear value is the one it is made for.
            let clear = env.clear_color;
            let scene = g.create_texture("Scene", desc.with_clear(ClearValue::Color(clear)));
            return g.add_raster("Sky", |b| {
                let scene = b.color(scene, LoadOp::Clear(clear), StoreOp::Store);
                (scene, |_: &mut PassContext<'_>| {})
            });
        };
        let scene = g.create_texture("Scene", desc);
        let d = env.light_direction.normalize_or_zero();
        // A Lambert surface facing the sun reflects albedo · E / π: the color the pass adds.
        let sun = env.sun_illuminance / core::f32::consts::PI;
        let [r, gr, b] = env.light_color.map(|c| c * sun);
        let [ar, ag, ab] = env.ambient;
        let eye = view.camera.0.translation;
        let light = LightConstants {
            inverse_view_projection: crate::view_projection(view.camera, width, height)
                .inverse()
                .to_cols_array(),
            eye: [eye.x, eye.y, eye.z, 0.0],
            direction: [d.x, d.y, d.z, 0.0],
            color: [r, gr, b, 0.0],
            ambient: [ar, ag, ab, 0.0],
            clear: env.clear_color,
            width,
            height,
            spare: [0; 2],
        };
        g.add_compute("DirectLight", |pass| {
            let material = pass.read_texture(view.material, Access::ComputeRead);
            let depth = pass.read_texture(view.depth, Access::ComputeRead);
            let shadow = shadow.map(|s| pass.read_texture(s, Access::ComputeRead));
            let scene = pass.write_texture(scene, Access::ComputeWrite);
            (scene, move |ctx: &mut PassContext<'_>| {
                let light = ctx.upload(&light);
                ctx.set_pipeline(&self.direct);
                ctx.push(&DirectArgs {
                    light,
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    output: ctx.uav(scene),
                    shadow_mask: shadow.map_or(0, |s| ctx.srv(s)),
                    shadowed: u32::from(shadow.is_some()),
                });
                ctx.dispatch(width.div_ceil(8), height.div_ceil(8), 1);
            })
        })
    }
}
