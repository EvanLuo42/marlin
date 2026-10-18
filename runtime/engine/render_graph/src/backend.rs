//! What the graph needs from the objects it imports and the device it sizes memory with. The
//! RHI is the backend of the engine; [`Recording`](crate::recording::Recording) stands in for it
//! in tests that run without a GPU (0007 §12).

use marlin_rhi::{BufferDesc, MemoryRequirements, Result, TextureDesc};

pub trait Backend: Sized + 'static {
    /// Sizes and aligns placed resources.
    type Device: ?Sized + Sync;
    type Texture: Sync;
    type Buffer: Sync;

    fn texture_desc(texture: &Self::Texture) -> &TextureDesc;
    fn buffer_desc(buffer: &Self::Buffer) -> &BufferDesc;
    fn texture_requirements(
        device: &Self::Device,
        desc: &TextureDesc,
    ) -> Result<MemoryRequirements>;
    fn buffer_requirements(device: &Self::Device, desc: &BufferDesc) -> MemoryRequirements;
}

/// The engine's backend: [`marlin_rhi`] objects.
pub enum Rhi {}

impl Backend for Rhi {
    type Device = marlin_rhi::Device;
    type Texture = marlin_rhi::Texture;
    type Buffer = marlin_rhi::Buffer;

    fn texture_desc(texture: &Self::Texture) -> &TextureDesc {
        texture.desc()
    }

    fn buffer_desc(buffer: &Self::Buffer) -> &BufferDesc {
        buffer.desc()
    }

    fn texture_requirements(
        device: &Self::Device,
        desc: &TextureDesc,
    ) -> Result<MemoryRequirements> {
        device.texture_requirements(desc)
    }

    fn buffer_requirements(device: &Self::Device, desc: &BufferDesc) -> MemoryRequirements {
        device.buffer_requirements(desc)
    }
}
