//! The renderer's asset types: what `.masset` text authors and what the cooker makes of it.
//!
//! Source types ([`Texture`], [`Mesh`], [`PairingPattern`]) are reflected, so source text binds
//! to them and links name them; their cooked forms are marlin-serial cooked layouts read in place
//! ([`TextureView`], [`MeshView`], with GPU data in bulk blobs; [`PairingPatternView`], whole).

pub mod cluster;
pub mod mesh;
pub mod pairing;
pub mod texture;

pub use mesh::{CookedMesh, Mesh, MeshError, MeshView, PositionPrecision};
pub use pairing::{PairingError, PairingPattern, PairingPatternView};
pub use texture::{
    CookedTexture, PixelFormat, Texture, TextureCompression, TextureError, TextureRegion,
    TextureShape, TextureUsage, TextureView,
};
