//! The renderer's quality cvars (0008 §2.1, 0010 §3; R4f): the scalability groups and the
//! feature cvars their levels set, read into a [`RenderSettings`] each frame and put into the
//! [`RenderSide`]'s passes.
//!
//! | group           | levels                         | sets                                   |
//! |-----------------|--------------------------------|----------------------------------------|
//! | `sg.quality`    | low, medium, high, ultra, cinematic (0008 §2.3's PC presets) | the groups below |
//! | `sg.lighting`   | 0–4: the lighting tiers L0–L4 (0010 §3) | `r.restir.*`, `r.sharc.*`     |
//! | `sg.geometry`   | 0–4                            | `r.geo.error_px`                       |
//! | `sg.resolution` | 0–4                            | `r.upscale.mode`                       |
//!
//! The levels' values are data: the engine's scalability table
//! (`content/engine/config/Scalability.masset`, [`crate::scalability`]), which a game's merges
//! over. The cvars' defaults here are `sg.quality = medium`'s, so a build without the table
//! draws as before. The renderer's crates keep plain settings fields (their tests set them); the
//! framework is the one place that reads cvars into them.
//!
//! Not every cell of 0010 §3's table has a knob yet: the reservoirs at a quarter of the pixels
//! (L0, L1: the checkerboard's half is the least so far), specular tracing below the full
//! resolution, L3's second bounce and its sun by ReSTIR and ray-traced soft shadows wait for
//! their features; a group level sets what exists.

use marlin_config::{CvarBool, CvarEnum, CvarF64, CvarI64};
use marlin_render::upscale::UpscaleMode;

use crate::RenderSide;

pub static SG_QUALITY: CvarEnum = CvarEnum::new(
    "sg.quality",
    1,
    &["low", "medium", "high", "ultra", "cinematic"],
    "Overall quality: sets the sg.* groups (0008 §2.3).",
);
pub static SG_LIGHTING: CvarI64 = CvarI64::new(
    "sg.lighting",
    2,
    "Lighting tier L0-L4 (0010 §3): sets r.restir.* and r.sharc.*.",
)
.range(0, 4);
pub static SG_GEOMETRY: CvarI64 =
    CvarI64::new("sg.geometry", 2, "Geometry detail: sets r.geo.error_px.").range(0, 4);
pub static SG_RESOLUTION: CvarI64 = CvarI64::new(
    "sg.resolution",
    3,
    "Internal resolution: sets r.upscale.mode.",
)
.range(0, 4);

pub static R_RESTIR_CANDIDATES: CvarI64 = CvarI64::new(
    "r.restir.candidates",
    16,
    "Lights a pixel draws from its light tile (0010 §3: 4, 8, 16, 32).",
)
.range(1, 64);
pub static R_RESTIR_NEIGHBORS: CvarI64 = CvarI64::new(
    "r.restir.neighbors",
    2,
    "Paired spatial neighbors of a reservoir; 0 turns spatial reuse off.",
)
.range(0, 8);
pub static R_RESTIR_HISTORY: CvarF64 = CvarF64::new(
    "r.restir.history",
    20.0,
    "The most confidence a reservoir takes from last frame (0010 §3: 10, 15, 20).",
)
.range(1.0, 64.0);
pub static R_RESTIR_CHECKERBOARD: CvarBool = CvarBool::new(
    "r.restir.checkerboard",
    true,
    "Reservoirs for half the pixels, in a checkerboard; off, one a pixel.",
);
pub static R_RESTIR_SPATIAL_VISIBILITY: CvarBool = CvarBool::new(
    "r.restir.spatial_visibility",
    true,
    "A shadow ray for each sample shifted to a neighbor (0010 §2.6).",
);
pub static R_RESTIR_GI: CvarBool = CvarBool::new(
    "r.restir.gi",
    true,
    "The reservoirs own indirect light (0010 §2.9); off, the ambient light stands in.",
);
pub static R_RESTIR_GI_BOUNCE: CvarBool = CvarBool::new(
    "r.restir.gi_bounce",
    true,
    "At a bounce's surface: its next event and one more bounce (L2 and up); off, the cache's.",
);
pub static R_RESTIR_SPECULAR: CvarBool = CvarBool::new(
    "r.restir.specular",
    true,
    "Trace the reflection of smooth lobes (0010 §2.7).",
);
pub static R_SHARC_CAPACITY: CvarI64 = CvarI64::new(
    "r.sharc.capacity",
    1 << 21,
    "World radiance cache entries, a power of two (0010 §3: 2^19 to 2^22); 56 bytes each.",
)
.range(1 << 10, 1 << 24);
pub static R_SHARC_STRIDE: CvarI64 = CvarI64::new(
    "r.sharc.stride",
    4,
    "A cache update path per stride x stride pixels (0010 §3: 1/32 to 1/4 of the pixels).",
)
.range(1, 16);
pub static R_DENOISE: CvarBool =
    CvarBool::new("r.denoise", true, "Denoise the lighting signals (0010 §6).");
pub static R_GEO_ERROR_PX: CvarF64 = CvarF64::new(
    "r.geo.error_px",
    1.0,
    "The screen-space error, in pixels, the LOD cut may have (0008 §2.3).",
)
.range(0.1, 16.0);
pub static R_UPSCALE_MODE: CvarEnum = CvarEnum::new(
    "r.upscale.mode",
    1,
    &[
        "off",
        "native",
        "quality",
        "balanced",
        "performance",
        "ultra_performance",
    ],
    "The upscaler's ratio (0010 §7); off draws at the output size without TSR.",
);

/// Registers the quality cvars (part of [`crate::register_cvars`]).
pub fn register_cvars() {
    let _ = marlin_config::register_all(&[
        &SG_QUALITY,
        &SG_LIGHTING,
        &SG_GEOMETRY,
        &SG_RESOLUTION,
        &R_RESTIR_CANDIDATES,
        &R_RESTIR_NEIGHBORS,
        &R_RESTIR_HISTORY,
        &R_RESTIR_CHECKERBOARD,
        &R_RESTIR_SPATIAL_VISIBILITY,
        &R_RESTIR_GI,
        &R_RESTIR_GI_BOUNCE,
        &R_RESTIR_SPECULAR,
        &R_SHARC_CAPACITY,
        &R_SHARC_STRIDE,
        &R_DENOISE,
        &R_GEO_ERROR_PX,
        &R_UPSCALE_MODE,
    ]);
}

/// The quality cvars' values, as plain data.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderSettings {
    pub restir_candidates: u32,
    pub restir_neighbors: u32,
    pub restir_history: f32,
    pub restir_checkerboard: bool,
    pub restir_spatial_visibility: bool,
    pub restir_gi: bool,
    pub restir_gi_bounce: bool,
    pub restir_specular: bool,
    pub sharc_capacity: u32,
    pub sharc_stride: u32,
    pub denoise: bool,
    pub lod_error_pixels: f32,
    pub upscale: Option<UpscaleMode>,
}

impl RenderSettings {
    /// The cvars' current values.
    pub fn from_cvars() -> Self {
        Self {
            restir_candidates: R_RESTIR_CANDIDATES.get() as u32,
            restir_neighbors: R_RESTIR_NEIGHBORS.get() as u32,
            restir_history: R_RESTIR_HISTORY.get() as f32,
            restir_checkerboard: R_RESTIR_CHECKERBOARD.get(),
            restir_spatial_visibility: R_RESTIR_SPATIAL_VISIBILITY.get(),
            restir_gi: R_RESTIR_GI.get(),
            restir_gi_bounce: R_RESTIR_GI_BOUNCE.get(),
            restir_specular: R_RESTIR_SPECULAR.get(),
            sharc_capacity: R_SHARC_CAPACITY.get() as u32,
            sharc_stride: R_SHARC_STRIDE.get() as u32,
            denoise: R_DENOISE.get(),
            lod_error_pixels: R_GEO_ERROR_PX.get() as f32,
            upscale: upscale_mode(R_UPSCALE_MODE.get()),
        }
    }

    /// Puts the settings into `render`'s passes. Changes take effect from the next frame; a
    /// new cache capacity starts an empty table.
    pub fn apply(&self, render: &mut RenderSide) {
        let r = &mut render.restir;
        r.candidates = self.restir_candidates;
        r.spatial_neighbors = self.restir_neighbors;
        r.max_history = self.restir_history;
        r.checkerboard = self.restir_checkerboard;
        r.spatial_visibility = self.restir_spatial_visibility;
        r.gi = self.restir_gi;
        r.gi_bounce = self.restir_gi_bounce;
        r.specular_tracing = self.restir_specular;
        if let Some(sharc) = &mut render.sharc {
            sharc.capacity = self.sharc_capacity;
            sharc.stride = self.sharc_stride;
        }
        render.denoise = self.denoise;
        render.visibility.lod_error_pixels = self.lod_error_pixels;
        render.upscale = self.upscale;
    }
}

/// `r.upscale.mode`'s level as a mode.
fn upscale_mode(level: u32) -> Option<UpscaleMode> {
    match level {
        0 => None,
        2 => Some(UpscaleMode::Quality),
        3 => Some(UpscaleMode::Balanced),
        4 => Some(UpscaleMode::Performance),
        5 => Some(UpscaleMode::UltraPerformance),
        _ => Some(UpscaleMode::Native),
    }
}
