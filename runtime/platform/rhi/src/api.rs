//! Public types: thin wrappers forwarding to the selected backend.

use core::time::Duration;

use crate::backend::imp::Backend;
use crate::contract::{
    Commands, Device as _, Fences, Instance as _, Queue as _, Swapchain as _, Types,
};
use crate::{
    AdapterInfo, DeviceDesc, Features, Format, InstanceDesc, PresentMode, PresentStatus,
    RenderPassDesc, Result, SurfaceHandle, SwapchainDesc, TextureBarrier,
};

/// The API entry point (DXGI factory, `VkInstance`). Enumerates adapters and creates devices.
pub struct Instance {
    raw: <Backend as Types>::Instance,
}

impl Instance {
    pub fn new(desc: &InstanceDesc) -> Result<Self> {
        Ok(Self {
            raw: Backend::create_instance(desc)?,
        })
    }

    /// Every adapter, highest performance first; software adapters last.
    pub fn adapters(&self) -> impl Iterator<Item = AdapterInfo> + '_ {
        (0..).map_while(|i| Backend::adapter(&self.raw, i))
    }

    pub fn create_device(&self, desc: &DeviceDesc) -> Result<Device> {
        let (raw, queue) = Backend::create_device(&self.raw, desc)?;
        Ok(Device {
            queue: Queue { raw: queue },
            raw,
        })
    }
}

/// A GPU. Owns the graphics queue. Dropping it waits for the queue to go idle.
pub struct Device {
    queue: Queue,
    raw: <Backend as Types>::Device,
}

impl Device {
    #[inline]
    pub fn adapter_info(&self) -> &AdapterInfo {
        Backend::adapter_info(&self.raw)
    }

    #[inline]
    pub fn features(&self) -> Features {
        Backend::features(&self.raw)
    }

    /// The graphics queue (also does compute and copies).
    #[inline]
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    pub fn create_fence(&self, initial_value: u64) -> Result<Fence> {
        Ok(Fence {
            raw: Backend::create_fence(&self.raw, initial_value)?,
        })
    }

    /// A command pool for the graphics queue.
    pub fn create_command_pool(&self) -> Result<CommandPool> {
        Ok(CommandPool {
            raw: Backend::create_command_pool(&self.raw)?,
        })
    }

    /// A command list whose memory comes from `pool` (and from whichever pool it is begun with
    /// later).
    pub fn create_command_list(&self, pool: &CommandPool) -> Result<CommandList> {
        Ok(CommandList {
            raw: Backend::create_command_list(&self.raw, &pool.raw)?,
        })
    }

    /// A swapchain presenting from the graphics queue.
    ///
    /// # Safety
    /// `surface` must be a live window, and it must outlive the swapchain.
    pub unsafe fn create_swapchain(
        &self,
        surface: SurfaceHandle,
        desc: &SwapchainDesc,
    ) -> Result<Swapchain> {
        Ok(Swapchain {
            // SAFETY: forwarded from the caller.
            raw: unsafe { Backend::create_swapchain(&self.raw, &self.queue.raw, surface, desc)? },
        })
    }

    /// Errors (and corruption reports) the validation layer has logged so far. 0 without
    /// validation. Tests assert this stays 0.
    ///
    /// D3D12 returns the same device for every device created on one adapter in a process, so
    /// such `Device`s share one message queue: a message is counted by whichever drains it first.
    #[inline]
    pub fn validation_errors(&self) -> u64 {
        Backend::validation_errors(&self.raw)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.queue.wait_idle();
    }
}

/// A GPU queue: executes command lists in submission order.
pub struct Queue {
    raw: <Backend as Types>::Queue,
}

impl Queue {
    /// Executes `lists` in order, then sets `signal.0` to `signal.1` once they finish.
    ///
    /// # Safety
    /// Until the GPU finishes these lists (observe it through the signalled fence): every
    /// resource they use stays alive, and neither the lists nor their pools are reset or dropped.
    pub unsafe fn submit(
        &self,
        lists: &[&CommandList],
        signal: Option<(&Fence, u64)>,
    ) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { Backend::submit(&self.raw, lists, signal) }
    }

    /// Blocks until everything submitted so far has finished.
    pub fn wait_idle(&self) -> Result<()> {
        Backend::wait_idle(&self.raw)
    }
}

/// A monotonically increasing 64-bit counter shared by CPU and GPU: D3D12 fence, Vulkan timeline
/// semaphore, Metal shared event.
pub struct Fence {
    pub(crate) raw: <Backend as Types>::Fence,
}

impl Fence {
    #[inline]
    pub fn completed_value(&self) -> u64 {
        Backend::completed_value(&self.raw)
    }

    /// Blocks until the fence reaches `value`. Returns `false` if `timeout` passed first.
    pub fn wait(&self, value: u64, timeout: Option<Duration>) -> Result<bool> {
        Backend::wait(&self.raw, value, timeout)
    }

    /// Sets the value from the CPU.
    pub fn signal(&self, value: u64) -> Result<()> {
        Backend::signal(&self.raw, value)
    }
}

/// Memory that command lists record into (D3D12 command allocator, `VkCommandPool`). One list
/// records from a pool at a time; reset it once the GPU is done with everything recorded.
pub struct CommandPool {
    raw: <Backend as Types>::CommandPool,
}

impl CommandPool {
    /// Frees everything recorded from this pool for reuse.
    ///
    /// # Safety
    /// The GPU has finished executing every command list recorded from this pool since the last
    /// reset.
    pub unsafe fn reset(&mut self) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { Backend::reset_pool(&mut self.raw) }
    }
}

/// A command list. [`begin`](Self::begin) to record, [`end`](Self::end), then submit.
#[repr(transparent)]
pub struct CommandList {
    pub(crate) raw: <Backend as Types>::CommandList,
}

impl CommandList {
    /// Starts recording into `pool`, discarding what was recorded before. The previous recording
    /// must not be pending on the GPU.
    pub fn begin(&mut self, pool: &mut CommandPool) -> Result<()> {
        Backend::begin(&mut self.raw, &mut pool.raw)
    }

    /// Finishes recording. Reports errors the API detected while recording.
    pub fn end(&mut self) -> Result<()> {
        Backend::end(&mut self.raw)
    }

    pub fn texture_barriers(&mut self, barriers: &[TextureBarrier<'_>]) {
        Backend::texture_barriers(&mut self.raw, barriers);
    }

    /// Binds render targets and applies their load ops. No barriers or copies are allowed until
    /// [`end_render_pass`](Self::end_render_pass).
    pub fn begin_render_pass(&mut self, desc: &RenderPassDesc<'_>) {
        Backend::begin_render_pass(&mut self.raw, desc);
    }

    pub fn end_render_pass(&mut self) {
        Backend::end_render_pass(&mut self.raw);
    }
}

/// A GPU texture.
pub struct Texture {
    #[cfg_attr(
        any(marlin_platform = "linux", marlin_platform = "macos"),
        expect(dead_code, reason = "the stub backend has no textures")
    )]
    pub(crate) raw: <Backend as Types>::Texture,
}

/// A view of a texture as a color render target. Borrowed from its owner (for now, the
/// swapchain).
pub struct RenderTargetView {
    #[cfg_attr(
        any(marlin_platform = "linux", marlin_platform = "macos"),
        expect(dead_code, reason = "the stub backend has no views")
    )]
    pub(crate) raw: <Backend as Types>::RenderTargetView,
}

/// The images a window presents, rotated by the presentation engine.
///
/// Per frame: [`acquire`](Self::acquire) an image, record work that moves it from
/// [`TextureState::PRESENT`](crate::TextureState::PRESENT) to a render target and back, submit,
/// then [`present`](Self::present). Dropping the swapchain waits for its queue to go idle.
pub struct Swapchain {
    raw: <Backend as Types>::Swapchain,
}

impl Swapchain {
    /// Blocks until the CPU may start the next frame (at most
    /// [`SwapchainDesc::max_frame_latency`] frames queued), then returns the index of the image to
    /// render. Call once per [`present`](Self::present).
    pub fn acquire(&mut self, timeout: Option<Duration>) -> Result<u32> {
        Backend::acquire(&mut self.raw, timeout)
    }

    /// Queues the acquired image for display once the work submitted before this call finishes.
    pub fn present(&mut self) -> Result<PresentStatus> {
        Backend::present(&mut self.raw)
    }

    /// Recreates the images at a new size (after a window resize). Waits for the GPU first, so
    /// command lists that used the old images must not be resubmitted.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        Backend::resize(&mut self.raw, width, height)
    }

    /// Takes effect at the next [`present`](Self::present).
    pub fn set_present_mode(&mut self, mode: PresentMode) {
        Backend::set_present_mode(&mut self.raw, mode);
    }

    #[inline]
    pub fn size(&self) -> (u32, u32) {
        Backend::size(&self.raw)
    }

    #[inline]
    pub fn format(&self) -> Format {
        Backend::format(&self.raw)
    }

    #[inline]
    pub fn image_count(&self) -> u32 {
        Backend::textures(&self.raw).len() as u32
    }

    /// Panics if `index >= image_count()`.
    #[inline]
    pub fn texture(&self, index: u32) -> &Texture {
        &Backend::textures(&self.raw)[index as usize]
    }

    /// The render target view of image `index`, in the swapchain's format (sRGB if requested).
    #[inline]
    pub fn view(&self, index: u32) -> &RenderTargetView {
        &Backend::views(&self.raw)[index as usize]
    }
}

macro_rules! opaque_debug {
    ($($ty:ident),*) => {$(
        impl core::fmt::Debug for $ty {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        }
    )*};
}

opaque_debug!(
    Instance,
    Queue,
    Fence,
    CommandPool,
    CommandList,
    Texture,
    RenderTargetView,
    Swapchain
);

impl core::fmt::Debug for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Device")
            .field("adapter", &self.adapter_info().name())
            .field("features", &self.features())
            .finish_non_exhaustive()
    }
}
