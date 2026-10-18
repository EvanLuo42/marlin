//! The engine's default material (`Marlin.Engine.DefaultMaterial`): what a mesh draws with when
//! nothing names its material.

use alloc::vec::Vec;

use marlin_render::scene::MaterialDesc;

/// The default material's resolve permutation: the first a material system registers.
pub const DEFAULT_PERMUTATION: u32 = 0;

/// `Marlin.Engine.DefaultMaterial`'s parameters. The instance's color
/// (`SurfaceContext.CustomData`) tints the base color.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DefaultMaterial {
    /// Linear.
    pub base_color: [f32; 3],
    pub roughness: f32,
    pub metallic: f32,
}

impl Default for DefaultMaterial {
    fn default() -> Self {
        Self {
            base_color: [1.0; 3],
            roughness: 0.5,
            metallic: 0.0,
        }
    }
}

impl DefaultMaterial {
    /// Words of its parameter block (the shader struct's scalar layout).
    pub const WORDS: u32 = 5;

    /// Its parameter block.
    pub fn params(&self) -> Vec<u32> {
        let [r, g, b] = self.base_color;
        [r, g, b, self.roughness, self.metallic]
            .iter()
            .map(|f| f.to_bits())
            .collect()
    }

    /// The material instance with these parameters.
    pub fn desc(&self) -> MaterialDesc {
        MaterialDesc {
            permutation: DEFAULT_PERMUTATION,
            params: self.params(),
        }
    }
}
