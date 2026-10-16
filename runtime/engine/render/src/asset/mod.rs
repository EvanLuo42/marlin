//! The renderer's asset types: what `.masset` text authors and what the cooker makes of it.
//!
//! Source types ([`Texture`], [`Mesh`]) are reflected, so source text binds to them and links
//! name them; their cooked forms are marlin-serial cooked layouts read in place
//! ([`TextureView`], [`MeshView`]), with GPU data in bulk blobs.

pub mod mesh;
pub mod texture;

pub use mesh::{CookedMesh, LodSettings, Mesh, MeshError, MeshView, Meshlet, PositionPrecision};
pub use texture::{
    CookedTexture, PixelFormat, Texture, TextureCompression, TextureError, TextureRegion,
    TextureShape, TextureUsage, TextureView,
};
