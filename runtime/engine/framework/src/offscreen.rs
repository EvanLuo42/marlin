//! Rendering without a window: into a texture, read back. For tests, screenshots and servers.

use alloc::vec::Vec;

use marlin_rhi::{
    Buffer, BufferDesc, BufferTextureCopy, BufferUsage, ClearValue, Device, Format, MemoryLocation,
    RenderTargetViewDesc, Texture, TextureBarrier, TextureDesc, TextureState, TextureUsage,
    aligned_row_pitch,
};

use crate::delta::{RenderDelta, RenderSide};

/// An RGBA8 target the size of the renderer, with a readback buffer.
pub struct Offscreen {
    pub width: u32,
    pub height: u32,
    target: Texture,
    /// The clear color the target was created for (D3D12 clears fastest, and validates
    /// quietly, when the clear matches the resource's).
    clear: [f32; 4],
    readback: Buffer,
    row_pitch: u32,
}

fn create_target(
    device: &Device,
    width: u32,
    height: u32,
    clear: [f32; 4],
) -> marlin_rhi::Result<Texture> {
    device.create_texture(&TextureDesc {
        clear_value: Some(ClearValue::Color(clear)),
        ..TextureDesc::new_2d(
            width,
            height,
            OFFSCREEN_FORMAT,
            TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC,
        )
    })
}

/// The color format offscreen renderers must be created with.
pub const OFFSCREEN_FORMAT: Format = Format::Rgba8Unorm;

impl Offscreen {
    pub fn new(device: &Device, width: u32, height: u32) -> marlin_rhi::Result<Self> {
        let clear = marlin_render::Environment::default().clear_color;
        let target = create_target(device, width, height, clear)?;
        let row_pitch = aligned_row_pitch(OFFSCREEN_FORMAT, width);
        let readback = device.create_buffer(&BufferDesc {
            size: row_pitch as u64 * height as u64,
            usage: BufferUsage::COPY_DST,
            memory: MemoryLocation::Readback,
        })?;
        Ok(Self {
            width,
            height,
            target,
            clear,
            readback,
            row_pitch,
        })
    }

    /// Applies `delta`, renders it, waits, and returns the pixels: RGBA8 as little-endian
    /// `u32`s, rows top to bottom.
    pub fn render(
        &mut self,
        device: &Device,
        render: &mut RenderSide,
        delta: &RenderDelta,
    ) -> marlin_rhi::Result<Vec<u32>> {
        render.apply(device, delta)?;
        if delta.environment.clear_color != self.clear {
            // The last call waited for the GPU, so the old target is free.
            self.clear = delta.environment.clear_color;
            self.target = create_target(device, self.width, self.height, self.clear)?;
        }
        let view =
            device.create_render_target_view(&self.target, &RenderTargetViewDesc::default())?;
        let mut pool = device.create_command_pool()?;
        let mut list = device.create_command_list(&pool)?;
        let fence = device.create_fence(0)?;
        list.begin(&mut pool)?;
        list.texture_barriers(&[TextureBarrier::new(
            &self.target,
            TextureState::UNDEFINED,
            TextureState::RENDER_TARGET,
        )]);
        // SAFETY: slot 0's previous work was waited for at the end of the last call, and this
        // call waits for its own below.
        unsafe { render.record(device, &mut list, 0, &view)? };
        list.texture_barriers(&[TextureBarrier::new(
            &self.target,
            TextureState::RENDER_TARGET,
            TextureState::COPY_SRC,
        )]);
        list.copy_texture_to_buffer(
            &self.target,
            &self.readback,
            &BufferTextureCopy::whole_mip(self.target.desc(), 0, 0, 0),
        );
        // The next call starts from UNDEFINED, which legacy barriers take as the common state.
        list.texture_barriers(&[TextureBarrier::new(
            &self.target,
            TextureState::COPY_SRC,
            TextureState::PRESENT,
        )]);
        list.end()?;
        // SAFETY: everything the list uses outlives the wait below.
        unsafe { device.queue().submit(&[&list], Some((&fence, 1)))? };
        fence.wait(1, None)?;
        let mut bytes = alloc::vec![0u8; self.readback.desc().size as usize];
        // SAFETY: the GPU finished writing the buffer (the fence).
        unsafe { self.readback.read(0, &mut bytes) };
        let mut pixels = Vec::with_capacity((self.width * self.height) as usize);
        for row in 0..self.height as usize {
            let line = &bytes[row * self.row_pitch as usize..][..self.width as usize * 4];
            pixels.extend(
                line.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]])),
            );
        }
        Ok(pixels)
    }
}
