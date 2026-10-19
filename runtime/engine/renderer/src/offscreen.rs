//! Rendering without a window: into a transient target of the frame's graph, read back. For
//! tests, screenshots and servers.

use alloc::vec::Vec;

use marlin_render_graph::{FrameContext, FrameContextDesc, PassTiming, TextureDesc};
use marlin_rhi::{ClearValue, Device, Format};

use crate::delta::RenderDelta;
use crate::world_renderer::WorldRenderer;

/// Frames rendered into an RGBA8 target of a fixed size, read back.
pub struct Offscreen {
    pub width: u32,
    pub height: u32,
    frame: FrameContext,
}

/// The color format offscreen renderers must be created with.
pub const OFFSCREEN_FORMAT: Format = Format::Rgba8Unorm;

impl Offscreen {
    pub fn new(device: &Device, width: u32, height: u32) -> marlin_rhi::Result<Self> {
        Ok(Self {
            width,
            height,
            // As the game runs its frames.
            frame: FrameContext::new(
                device,
                &FrameContextDesc {
                    overlap: true,
                    ..FrameContextDesc::default()
                },
            )?,
        })
    }

    /// The GPU time of each pass of the latest frame whose timings are back (profiling).
    pub fn gpu_timings(&self) -> &[PassTiming] {
        self.frame.gpu_timings()
    }

    /// Applies `delta`, renders it, waits, and returns the pixels: RGBA8 as little-endian
    /// `u32`s, rows top to bottom.
    pub fn render(
        &mut self,
        device: &Device,
        render: &mut WorldRenderer,
        delta: &RenderDelta,
    ) -> marlin_rhi::Result<Vec<u32>> {
        render.collect_feedback(&mut self.frame);
        render.apply(device, delta)?;
        let (width, height) = (self.width, self.height);
        let ticket = self.frame.execute(device, |g, _| {
            // The tonemap overwrites the target: it is made for a black clear (D3D12 validates
            // quietly when a clear matches the resource's).
            let clear = [0.0, 0.0, 0.0, 1.0];
            let target = g.create_texture(
                "Offscreen",
                TextureDesc::new_2d(width, height, OFFSCREEN_FORMAT)
                    .with_clear(ClearValue::Color(clear)),
            );
            let target = render.add_passes(g, target);
            g.readback_texture(target)
        })?;
        let bytes = self.frame.wait_readback(ticket)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| u32::from_le_bytes(*p))
            .collect())
    }
}
