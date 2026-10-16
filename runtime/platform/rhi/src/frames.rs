use alloc::vec::Vec;

use crate::{CommandList, CommandPool, Device, Fence, Queue, Result};

struct Slot {
    pool: CommandPool,
    list: CommandList,
    /// Fence value that marks this slot's last submission finished.
    value: u64,
}

/// Frames in flight: N command pools and lists used round-robin, so the CPU records frame N+1
/// while the GPU still runs frame N. Each frame waits only for the frame that last used its slot.
///
/// The safe way to get command lists: a slot's pool is reset only after the fence shows the GPU
/// finished it.
///
/// ```ignore
/// let list = frames.begin()?;
/// list.texture_barriers(...);
/// frames.submit(device.queue())?;
/// swapchain.present()?;
/// ```
pub struct FramesInFlight {
    fence: Fence,
    slots: Vec<Slot>,
    current: usize,
    /// Value the next submission signals.
    next_value: u64,
    recording: bool,
}

impl FramesInFlight {
    /// `count` is usually 2: one frame recording while one executes. Panics if 0.
    pub fn new(device: &Device, count: usize) -> Result<Self> {
        assert!(count > 0);
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            let pool = device.create_command_pool()?;
            let list = device.create_command_list(&pool)?;
            slots.push(Slot {
                pool,
                list,
                value: 0,
            });
        }
        Ok(Self {
            fence: device.create_fence(0)?,
            slots,
            current: 0,
            next_value: 1,
            recording: false,
        })
    }

    /// How many frames may be in flight.
    #[inline]
    pub fn count(&self) -> usize {
        self.slots.len()
    }

    /// Index of the current slot, for per-frame resources the caller keeps alongside.
    #[inline]
    pub fn index(&self) -> usize {
        self.current
    }

    /// Waits until the GPU is done with the current slot, then returns its list, recording.
    /// Panics if the previous frame was not submitted.
    pub fn begin(&mut self) -> Result<&mut CommandList> {
        assert!(!self.recording, "FramesInFlight::begin called twice");
        let slot = &mut self.slots[self.current];
        self.fence.wait(slot.value, None)?;
        // SAFETY: the fence shows the GPU finished the slot's last submission, the only work
        // recorded from this pool.
        unsafe { slot.pool.reset()? };
        slot.list.begin(&mut slot.pool)?;
        self.recording = true;
        Ok(&mut slot.list)
    }

    /// Ends the current list, submits it to `queue` and moves to the next slot.
    ///
    /// The caller keeps every resource the list uses alive until the GPU finishes it (as
    /// [`Queue::submit`] requires): for swapchain images that holds because resizing and dropping
    /// a swapchain wait for the GPU.
    pub fn submit(&mut self, queue: &Queue) -> Result<()> {
        assert!(self.recording, "FramesInFlight::submit without begin");
        self.recording = false;
        let value = self.next_value;
        let slot = &mut self.slots[self.current];
        slot.list.end()?;
        // SAFETY: the pool is reset only after the fence passes `value` (`begin`), and the list is
        // not touched before that either. Resource lifetimes are the caller's (see above).
        unsafe { queue.submit(&[&slot.list], Some((&self.fence, value)))? };
        slot.value = value;
        self.next_value += 1;
        self.current = (self.current + 1) % self.slots.len();
        Ok(())
    }

    /// Blocks until every submitted frame has finished.
    pub fn wait_idle(&self) -> Result<()> {
        self.fence.wait(self.next_value - 1, None).map(drop)
    }
}

impl Drop for FramesInFlight {
    fn drop(&mut self) {
        // Pools and lists must outlive their GPU work.
        let _ = self.wait_idle();
    }
}
