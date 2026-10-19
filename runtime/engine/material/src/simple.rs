//! Simplified materials (0011 §9.1–9.2): what ray hits shade with. Each material row's
//! simplified material is baked at runtime into a [`SIMPLE_TILE`]² tile of an atlas (the tile is
//! the row's: [`SIMPLE_TILES_PER_ROW`] a row of [`SIMPLE_ATLAS`]²), evaluating its type's `Surface`
//! over the tile's UVs; each instance row gets a constant factor for its per-instance data
//! (`CustomData`), its material evaluated with that data over the same without it (the user's
//! choice, 0011 §9.2). Rebaked as the scene changes (`RenderScene::simple_changes`), all of it
//! when the atlas or the factors are new. `Material/Simple.mshader` holds the bake
//! (`BakeSimple<M>`, a permutation per material type) and what hits read (`SimpleOfHit`).

use alloc::vec::Vec;

use marlin_render::raytracing::SimpleOutput;
use marlin_render::{GpuScene, RenderScene};
use marlin_render_graph::{
    Access, BufferDesc, GLOBAL_SCOPE, PassContext, RenderGraph, TextureDesc,
};
use marlin_rhi::Format;
use marlin_serial::cooked::Pod;
use marlin_shader::bind::EntryDesc;

use crate::Materials;

/// Texels a side of a material row's tile.
pub const SIMPLE_TILE: u32 = 32;
/// Tiles a row of the atlas.
pub const SIMPLE_TILES_PER_ROW: u32 = 64;
/// The atlas's side: [`SIMPLE_TILES_PER_ROW`]² material rows.
pub const SIMPLE_ATLAS: u32 = SIMPLE_TILE * SIMPLE_TILES_PER_ROW;
/// The material rows the atlas holds; rows past it have no simplified material yet.
pub const SIMPLE_MATERIALS: u32 = SIMPLE_TILES_PER_ROW * SIMPLE_TILES_PER_ROW;

/// A material type's bake entry: `Marlin.Engine.Material.BakeSimple` instantiated with it.
pub const BAKE: EntryDesc =
    EntryDesc::compute("Marlin.Engine.Material.BakeSimple").push::<SimpleBakeArgs>();

/// `Marlin.Engine.Material.SimpleBakeArgs<M>`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
pub struct SimpleBakeArgs {
    instances: u32,
    materials: u32,
    params: u32,
    rows: u32,
    material_count: u32,
    instance_count: u32,
    albedo: u32,
    specular: u32,
    emission: u32,
    factors: u32,
}

impl Materials {
    /// Declares the frame's simplified-material bakes for `scene` (after its applied updates),
    /// reading the GPU Scene `gpu`; returns the simplified materials.
    pub fn add_simple_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        scene: &RenderScene,
        gpu: GpuScene<'g>,
    ) -> SimpleOutput<'g> {
        let atlas = |g: &mut RenderGraph<'g>, name, format| {
            g.persistent(
                name,
                GLOBAL_SCOPE,
                TextureDesc::new_2d(SIMPLE_ATLAS, SIMPLE_ATLAS, format),
            )
        };
        let albedo = atlas(g, "SimpleAlbedo", Format::Rgba8Unorm);
        let specular = atlas(g, "SimpleSpecular", Format::Rgba8Unorm);
        let emission = atlas(g, "SimpleEmission", Format::Rgba16Float);
        // Two float4 an instance row, for rows in powers of two (a resize rebakes them all).
        let rows = scene.instance_rows().max(1).next_power_of_two().max(1024);
        let factors = g.persistent(
            "SimpleFactors",
            GLOBAL_SCOPE,
            BufferDesc::structured::<[f32; 4]>(2 * u64::from(rows)),
        );
        let everything = albedo.fresh || specular.fresh || emission.fresh || factors.fresh;
        let (materials, instances): (Vec<u32>, Vec<u32>) = if everything {
            (
                (0..scene.material_rows()).collect(),
                (0..scene.instance_rows()).collect(),
            )
        } else {
            let (m, i) = scene.simple_changes();
            (m.to_vec(), i.to_vec())
        };
        let mut out = SimpleOutput {
            albedo: albedo.handle,
            specular: specular.handle,
            emission: emission.handle,
            factors: factors.handle,
        };
        // Each permutation bakes its own materials and the instances that use them.
        let permutations = (self.types().len()).min(gpu.material_params.len());
        for p in 0..permutations {
            let mut rows: Vec<u32> = materials
                .iter()
                .copied()
                .filter(|&m| {
                    m < SIMPLE_MATERIALS && scene.material_permutation(m) == Some(p as u32)
                })
                .collect();
            let material_count = rows.len() as u32;
            rows.extend(instances.iter().copied().filter(|&i| {
                scene
                    .instance_material_row(i)
                    .and_then(|m| scene.material_permutation(m))
                    == Some(p as u32)
            }));
            let instance_count = rows.len() as u32 - material_count;
            if rows.is_empty() {
                continue;
            }
            let threads = material_count * SIMPLE_TILE * SIMPLE_TILE + instance_count;
            let pipeline = &self.types()[p].bake;
            let params = gpu.material_params[p];
            let before = out;
            out = g.add_compute("BakeSimple", |b| {
                let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
                let materials = b.read_buffer(gpu.materials, Access::ComputeRead);
                let params = b.read_buffer(params, Access::ComputeRead);
                let albedo = b.write_texture(before.albedo, Access::ComputeWrite);
                let specular = b.write_texture(before.specular, Access::ComputeWrite);
                let emission = b.write_texture(before.emission, Access::ComputeWrite);
                let factors = b.write_buffer(before.factors, Access::ComputeWrite);
                let written = SimpleOutput {
                    albedo,
                    specular,
                    emission,
                    factors,
                };
                (written, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    let args = SimpleBakeArgs {
                        instances: ctx.srv(instances),
                        materials: ctx.srv(materials),
                        params: ctx.srv(params),
                        rows: ctx.upload_slice(&rows),
                        material_count,
                        instance_count,
                        albedo: ctx.uav(albedo),
                        specular: ctx.uav(specular),
                        emission: ctx.uav(emission),
                        factors: ctx.uav(factors),
                    };
                    ctx.push(&args);
                    ctx.dispatch(threads.div_ceil(64), 1, 1);
                })
            });
        }
        out
    }
}
