//! The denoiser (0010 §6, R3d): one for every noisy lighting signal. A signal is
//! a [`NoisySignal`]: demodulated light with the distance to what lit it, and the two hints its
//! producer fills in (0010 §6.1): `Novelty`, the share of the frame's estimate that is new this
//! frame, and `CorrelationRadius`, its noise's blob size in pixels. Plain Monte Carlo writes 1
//! and 1; ReSTIR ([`crate::restir`]) writes its reservoirs' fresh share and a radius from its
//! duplication map. The denoiser knows no producer: its accumulation counts novelty, not
//! frames, and its statistics and blur span the correlation radius, so persistent and spread
//! noise is not taken for detail.
//!
//! The passes are `Marlin.Engine.Lighting`'s (`Lighting/Denoise.mshader`, whose file comment
//! describes them): slow and fast temporal accumulations (history resources per view), the slow
//! one held near the fast one, and two Poisson blurs; then [`Denoiser::add_compose`] multiplies
//! the albedo back and adds the light to the scene.
//!
//! Diffuse only for now; the specular signal joins with the final shading's specular.

use marlin_math::Transform;
use marlin_math::scalar::tan;
use marlin_render_graph::{
    Access, PassContext, RenderGraph, TextureDesc as GraphTextureDesc, TextureHandle,
};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Format, Result, ShaderModule};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};

use crate::meshes::Camera;

/// A signal's light (demodulated, RGB) and the distance to what lit it (alpha; 0: unknown).
pub const SIGNAL_FORMAT: Format = Format::Rgba16Float;
/// A signal's hints: `Novelty`, `CorrelationRadius`.
pub const HINTS_FORMAT: Format = Format::Rg16Float;
/// The slow accumulation: light and the frames behind it. 32-bit: it averages up to
/// `max_frames / novelty` frames, 3200 at a novelty of 0.01, and a half-float average that long
/// stops short of the mean.
const ACCUMULATION_FORMAT: Format = Format::Rgba32Float;
/// The fast accumulation and the blurs: weights of a quarter and more.
const FAST_FORMAT: Format = Format::Rgba16Float;
/// Last frame's surfaces: normal and distance to the eye.
const SURFACE_FORMAT: Format = Format::Rgba16Float;

/// A noisy signal for the denoiser (see the module docs), both textures the view's size.
#[derive(Clone, Copy)]
pub struct NoisySignal<'g> {
    pub light: TextureHandle<'g>,
    pub hints: TextureHandle<'g>,
}

/// What the denoiser reads of the view besides the signal.
#[derive(Clone, Copy)]
pub struct DenoiseInputs<'g> {
    /// Keys the view's history.
    pub view: u64,
    /// The material buffer (`marlin_material`'s `MATERIAL_FORMAT`): normals and albedo.
    pub material: TextureHandle<'g>,
    /// The camera's depth (`R32Float`, reverse-Z).
    pub depth: TextureHandle<'g>,
    /// The material passes' motion vectors.
    pub velocity: TextureHandle<'g>,
    /// The signal is specular (R4d3): where the lobe is smooth its history follows the
    /// reflection's virtual motion (which needs last frame's camera, `previous`), and its blur
    /// narrows with the roughness.
    pub specular: bool,
    pub previous: Option<(Transform, Camera)>,
}

/// The denoiser's shaders (`Marlin.Engine.Lighting`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DenoiseShaders<'a> {
    pub temporal: ShaderModule<'a>,
    pub clamp: ShaderModule<'a>,
    pub blur: ShaderModule<'a>,
    pub compose: ShaderModule<'a>,
}

impl<'a> DenoiseShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 4] = [
        EntryDesc::compute("Marlin.Engine.Lighting.DenoiseTemporal").push::<DenoiseArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.DenoiseClamp").push::<DenoiseArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.DenoiseBlur").push::<DenoiseArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.DenoiseCompose").push::<DenoiseArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules([temporal, clamp, blur, compose]: [ShaderModule<'a>; 4]) -> Self {
        Self {
            temporal,
            clamp,
            blur,
            compose,
        }
    }

    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// `Marlin.Engine.Lighting.DenoiseConstants`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct Constants {
    inverse_view_projection: [f32; 16],
    eye: [f32; 4],
    width: u32,
    height: u32,
    has_history: u32,
    seed: u32,
    max_frames: f32,
    max_radius: f32,
    min_radius: f32,
    clamp_sigma: f32,
    firefly_sigma: f32,
    radius_scale: f32,
    pixel_angle: f32,
    fast_frames: f32,
    taps: u32,
    target_error: f32,
    has_specular: u32,
    specular: u32,
    previous_view_projection: [f32; 16],
}

/// `Marlin.Engine.Lighting.DenoiseArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct DenoiseArgs {
    constants: u32,
    material: u32,
    depth: u32,
    velocity: u32,
    signal: u32,
    hints: u32,
    history: u32,
    previous_surface: u32,
    previous_fast: u32,
    fast: u32,
    input: u32,
    variance: u32,
    output: u32,
    fast_out: u32,
    position_out: u32,
    position: u32,
    current_surface: u32,
    surface_out: u32,
    previous_hit: u32,
    hit_out: u32,
    variance_out: u32,
    scene: u32,
}

/// The denoiser's pipelines and settings (see the module docs).
pub struct Denoiser {
    temporal_pass: ComputePipeline,
    clamp_pass: ComputePipeline,
    blur_pass: ComputePipeline,
    compose_pass: ComputePipeline,
    /// The most independent samples' worth (frames times the novelty) the slow accumulation
    /// averages: more is smoother and slower to follow changes the clamp does not catch.
    pub max_frames: f32,
    /// The blur radius where nothing is accumulated yet, in pixels at 1080p (scaled with the
    /// view's height).
    pub max_radius: f32,
    /// The least blur radius, in pixels.
    pub min_radius: f32,
    /// How far from the neighborhood's mean the history may be, in deviations.
    pub clamp_sigma: f32,
    /// How far above the neighborhood's mean a pixel may be, in deviations.
    pub firefly_sigma: f32,
    /// The real frames the fast accumulation, which the slow one is held near, averages.
    pub fast_frames: f32,
    /// The relative error (deviation over value, measured) at which a blur takes its full radius;
    /// a more precise accumulation is blurred less. 0: the radius follows the samples alone.
    pub target_error: f32,
    /// Specular signals follow their reflections' virtual motion and blur by roughness (R4d3); off,
    /// they are denoised as diffuse ones (for comparison).
    pub virtual_motion: bool,
}

impl Denoiser {
    /// Defaults: 32 independent samples' worth, a 4-frame fast accumulation the slow one is held
    /// within 1.5 deviations of, a 32-pixel blur at 1080p shrinking with the samples and with the
    /// error left (full at 10%), fireflies above 4 deviations cut.
    pub fn new(device: &Device, shaders: &DenoiseShaders<'_>) -> Result<Self> {
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            temporal_pass: compute(shaders.temporal)?,
            clamp_pass: compute(shaders.clamp)?,
            blur_pass: compute(shaders.blur)?,
            compose_pass: compute(shaders.compose)?,
            max_frames: 32.0,
            max_radius: 32.0,
            min_radius: 1.0,
            clamp_sigma: 1.5,
            firefly_sigma: 4.0,
            fast_frames: 4.0,
            target_error: 0.1,
            virtual_motion: true,
        })
    }

    /// Declares the denoising of `signal`, seen by `camera` (`width` × `height`) with `seed` (a
    /// new one each frame); returns the denoised light, demodulated ([`SIGNAL_FORMAT`]).
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        signal: NoisySignal<'g>,
        inputs: DenoiseInputs<'g>,
        (eye, camera): (Transform, Camera),
        width: u32,
        height: u32,
        seed: u32,
    ) -> TextureHandle<'g> {
        let view_projection = crate::view_projection((eye, camera), width, height);
        let p = eye.translation;
        let DenoiseInputs {
            view,
            material,
            depth,
            velocity,
            specular,
            previous: previous_camera,
        } = inputs;
        // Last frame's camera without its jitter, as the motion vectors are; this frame's without a
        // previous one (no virtual motion then).
        let previous_view_projection = crate::view_projection(
            previous_camera.map_or((eye, camera.unjittered()), |(t, c)| (t, c.unjittered())),
            width,
            height,
        )
        .to_cols_array();
        let specular = specular && self.virtual_motion;
        let accumulation = g.history(
            "DnDiffuse",
            view,
            GraphTextureDesc::new_2d(width, height, ACCUMULATION_FORMAT),
        );
        let fast = g.history(
            "DnDiffuseFast",
            view,
            GraphTextureDesc::new_2d(width, height, FAST_FORMAT),
        );
        let surface = g.history(
            "DnSurface",
            view,
            GraphTextureDesc::new_2d(width, height, SURFACE_FORMAT),
        );
        let previous = accumulation
            .prev
            .zip(fast.prev)
            .zip(surface.prev)
            .map(|((h, f), s)| (h, f, s));
        let constants = |radius_scale: f32| Constants {
            inverse_view_projection: view_projection.inverse().to_cols_array(),
            eye: [p.x, p.y, p.z, 0.0],
            width,
            height,
            has_history: u32::from(previous.is_some()),
            seed,
            max_frames: self.max_frames.max(1.0),
            max_radius: self.max_radius * height as f32 / 1080.0,
            min_radius: self.min_radius,
            clamp_sigma: self.clamp_sigma,
            firefly_sigma: self.firefly_sigma,
            radius_scale,
            pixel_angle: 2.0 * tan(camera.fov_y * 0.5) / height.max(1) as f32,
            fast_frames: self.fast_frames.max(1.0),
            taps: 16,
            target_error: self.target_error.max(0.0),
            has_specular: 0,
            specular: u32::from(specular),
            previous_view_projection,
        };
        let groups = (width.div_ceil(8), height.div_ceil(8));
        let texture = |g: &mut RenderGraph<'g>, name, format| {
            g.create_texture(name, GraphTextureDesc::new_2d(width, height, format))
        };
        let slow = texture(g, "DnSlow", ACCUMULATION_FORMAT);
        let signal_variance = texture(g, "DnSignalVariance", Format::R16Float);
        let variance = texture(g, "DnVariance", Format::R16Float);
        let position = texture(g, "DnPosition", Format::Rgba16Float);

        let full = constants(1.0);
        // A specular signal's hit distances, last frame's and this frame's.
        let hit = specular.then(|| {
            let h = g.history(
                "DnHit",
                view,
                GraphTextureDesc::new_2d(width, height, Format::R16Float),
            );
            (h.prev, h.cur)
        });
        let (slow, fast_cur, signal_variance, position, surface_cur) =
            g.add_compute("DenoiseTemporal", |b| {
                let hit = hit.map(|(prev, cur)| {
                    (
                        prev.map(|p| b.read_texture(p, Access::ComputeRead)),
                        b.write_texture(cur, Access::ComputeWrite),
                    )
                });
                let material = b.read_texture(material, Access::ComputeRead);
                let depth = b.read_texture(depth, Access::ComputeRead);
                let velocity = b.read_texture(velocity, Access::ComputeRead);
                let light = b.read_texture(signal.light, Access::ComputeRead);
                let hints = b.read_texture(signal.hints, Access::ComputeRead);
                let previous = previous.map(|(h, f, s)| {
                    (
                        b.read_texture(h, Access::ComputeRead),
                        b.read_texture(f, Access::ComputeRead),
                        b.read_texture(s, Access::ComputeRead),
                    )
                });
                let output = b.write_texture(slow, Access::ComputeWrite);
                let fast_out = b.write_texture(fast.cur, Access::ComputeWrite);
                let surface_out = b.write_texture(surface.cur, Access::ComputeWrite);
                let position_out = b.write_texture(position, Access::ComputeWrite);
                let variance = b.write_texture(signal_variance, Access::ComputeWrite);
                (
                    (output, fast_out, variance, position_out, surface_out),
                    move |ctx: &mut PassContext<'_>| {
                        ctx.set_pipeline(&self.temporal_pass);
                        // Without history its textures go unread: the signal stands in.
                        let (history, previous_fast, previous_surface) = previous.map_or(
                            (ctx.srv(light), ctx.srv(light), ctx.srv(light)),
                            |(h, f, s)| (ctx.srv(h), ctx.srv(f), ctx.srv(s)),
                        );
                        let args = DenoiseArgs {
                            constants: ctx.upload(&full),
                            material: ctx.srv(material),
                            depth: ctx.srv(depth),
                            velocity: ctx.srv(velocity),
                            signal: ctx.srv(light),
                            hints: ctx.srv(hints),
                            history,
                            previous_surface,
                            previous_fast,
                            output: ctx.uav(output),
                            fast_out: ctx.uav(fast_out),
                            surface_out: ctx.uav(surface_out),
                            position_out: ctx.uav(position_out),
                            variance_out: ctx.uav(variance),
                            // Unread without a specular signal or its last frame.
                            previous_hit: hit.and_then(|(p, _)| p).map_or(0, |p| ctx.srv(p)),
                            hit_out: hit.map_or(0, |(_, c)| ctx.uav(c)),
                            ..DenoiseArgs::default()
                        };
                        ctx.push(&args);
                        ctx.dispatch(groups.0, groups.1, 1);
                    },
                )
            });

        let (accumulated, variance) = g.add_compute("DenoiseClamp", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let hints = b.read_texture(signal.hints, Access::ComputeRead);
            let slow = b.read_texture(slow, Access::ComputeRead);
            let fast = b.read_texture(fast_cur, Access::ComputeRead);
            let signal_variance = b.read_texture(signal_variance, Access::ComputeRead);
            let output = b.write_texture(accumulation.cur, Access::ComputeWrite);
            let variance = b.write_texture(variance, Access::ComputeWrite);
            ((output, variance), move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.clamp_pass);
                let args = DenoiseArgs {
                    constants: ctx.upload(&full),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    hints: ctx.srv(hints),
                    input: ctx.srv(slow),
                    fast: ctx.srv(fast),
                    variance: ctx.srv(signal_variance),
                    output: ctx.uav(output),
                    variance_out: ctx.uav(variance),
                    ..DenoiseArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });

        let mut blurred = accumulated;
        for (name, scale, taps) in [("DenoiseBlur", 1.0, 16), ("DenoisePostBlur", 0.5, 8)] {
            let target =
                g.create_texture(name, GraphTextureDesc::new_2d(width, height, FAST_FORMAT));
            let c = Constants {
                taps,
                ..constants(scale)
            };
            let input = blurred;
            blurred = g.add_compute(name, |b| {
                let position = b.read_texture(position, Access::ComputeRead);
                let surface = b.read_texture(surface_cur, Access::ComputeRead);
                let light = b.read_texture(signal.light, Access::ComputeRead);
                let hints = b.read_texture(signal.hints, Access::ComputeRead);
                let input = b.read_texture(input, Access::ComputeRead);
                let material = b.read_texture(material, Access::ComputeRead);
                let variance = b.read_texture(variance, Access::ComputeRead);
                let output = b.write_texture(target, Access::ComputeWrite);
                (output, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(&self.blur_pass);
                    let args = DenoiseArgs {
                        constants: ctx.upload(&c),
                        material: ctx.srv(material),
                        position: ctx.srv(position),
                        current_surface: ctx.srv(surface),
                        signal: ctx.srv(light),
                        hints: ctx.srv(hints),
                        input: ctx.srv(input),
                        variance: ctx.srv(variance),
                        output: ctx.uav(output),
                        ..DenoiseArgs::default()
                    };
                    ctx.push(&args);
                    ctx.dispatch(groups.0, groups.1, 1);
                })
            });
        }
        blurred
    }

    /// Declares adding `light` (demodulated diffuse, [`SIGNAL_FORMAT`] or the denoiser's output)
    /// times the material's albedo, and `specular` (demodulated) times the lobe's specular albedo
    /// as `camera` sees it, to `scene`; returns `scene` after. Composes signals not denoised too.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_compose<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        scene: TextureHandle<'g>,
        light: TextureHandle<'g>,
        specular: Option<TextureHandle<'g>>,
        material: TextureHandle<'g>,
        depth: TextureHandle<'g>,
        camera: (Transform, Camera),
        width: u32,
        height: u32,
    ) -> TextureHandle<'g> {
        let eye = camera.0.translation;
        let constants = Constants {
            inverse_view_projection: crate::view_projection(camera, width, height)
                .inverse()
                .to_cols_array(),
            eye: [eye.x, eye.y, eye.z, 0.0],
            width,
            height,
            has_history: 0,
            seed: 0,
            max_frames: 0.0,
            max_radius: 0.0,
            min_radius: 0.0,
            clamp_sigma: 1.5,
            firefly_sigma: 0.0,
            radius_scale: 0.0,
            pixel_angle: 0.0,
            fast_frames: 0.0,
            taps: 0,
            target_error: 0.1,
            has_specular: u32::from(specular.is_some()),
            specular: 0,
            previous_view_projection: [0.0; 16],
        };
        g.add_compute("DenoiseCompose", |b| {
            let light = b.read_texture(light, Access::ComputeRead);
            let specular = specular.map(|s| b.read_texture(s, Access::ComputeRead));
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let scene = b.write_texture(scene, Access::ComputeWrite);
            (scene, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.compose_pass);
                let args = DenoiseArgs {
                    constants: ctx.upload(&constants),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    input: ctx.srv(light),
                    // Not read without (`HasSpecular` 0).
                    signal: ctx.srv(specular.unwrap_or(light)),
                    scene: ctx.uav(scene),
                    ..DenoiseArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(width.div_ceil(8), height.div_ceil(8), 1);
            })
        })
    }
}
