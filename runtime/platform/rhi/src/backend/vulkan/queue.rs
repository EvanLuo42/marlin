//! Queues, fences (timeline semaphores) and submission.

use core::ptr;
use core::time::Duration;

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_sync::Mutex;

use super::device::{Device, Shared};
use super::ffi::*;
use super::{error, unsupported};
use crate::{Fence as PublicFence, FenceWait, Result, SubmitDesc};

/// What `vkQueue*` calls share: the lock they need (queues are externally synchronized) and the
/// binary semaphores the next submission waits on (swapchain acquires, 0016 §7.1).
#[derive(Default)]
pub(crate) struct QueueState {
    pub(crate) pending_waits: Vec<VkSemaphore>,
}

pub(crate) struct Queue {
    shared: Arc<Shared>,
    raw: VkQueue,
    pub(crate) family: u32,
    pub(crate) state: Arc<Mutex<QueueState>>,
}

// SAFETY: every `vkQueue*` call holds `state`'s lock.
unsafe impl Send for Queue {}
// SAFETY: as above.
unsafe impl Sync for Queue {}

/// A queue shared with an object that outlives the borrow it was made from; the `VkQueue` lives as
/// long as the device, which the object keeps alive.
#[derive(Clone)]
pub(crate) struct QueueHandle {
    pub(crate) raw: VkQueue,
    state: Arc<Mutex<QueueState>>,
}

// SAFETY: as `Queue`: every `vkQueue*` call holds `state`'s lock.
unsafe impl Send for QueueHandle {}
// SAFETY: as above.
unsafe impl Sync for QueueHandle {}

impl QueueHandle {
    /// As [`Queue::with_lock`].
    pub(crate) fn with_lock<R>(&self, f: impl FnOnce(VkQueue, &mut QueueState) -> R) -> R {
        let mut state = self.state.lock();
        f(self.raw, &mut state)
    }
}

fn timeout_ns(timeout: Option<Duration>) -> u64 {
    timeout.map_or(u64::MAX, |t| t.as_nanos().min(u64::MAX as u128 - 1) as u64)
}

impl Queue {
    pub(crate) fn new(shared: &Arc<Shared>, family: u32) -> Self {
        let info = VkDeviceQueueInfo2 {
            queueFamilyIndex: family,
            queueIndex: 0,
            ..Default::default()
        };
        let mut raw = ptr::null_mut();
        // SAFETY: the device was created with one queue in this family.
        unsafe { (shared.fns.vkGetDeviceQueue2)(shared.raw, &info, &mut raw) };
        Self {
            shared: shared.clone(),
            raw,
            family,
            state: Arc::new(Mutex::new(QueueState::default())),
        }
    }

    pub(crate) fn raw(&self) -> VkQueue {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    /// # Safety
    /// As `contract::Queue::submit`.
    pub(crate) unsafe fn submit(&self, desc: &SubmitDesc<'_>) -> Result<()> {
        let s = &self.shared;
        s.lost.check()?;
        let wait = |semaphore, value| VkSemaphoreSubmitInfo {
            semaphore,
            value,
            stageMask: VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            ..Default::default()
        };
        let mut waits: Vec<VkSemaphoreSubmitInfo> = desc
            .waits
            .iter()
            .map(|(f, v)| wait(f.raw.raw, *v))
            .collect();
        let lists: Vec<VkCommandBufferSubmitInfo> = desc
            .lists
            .iter()
            .map(|l| VkCommandBufferSubmitInfo {
                commandBuffer: l.raw.raw(),
                ..Default::default()
            })
            .collect();
        let signals: Vec<VkSemaphoreSubmitInfo> = desc
            .signals
            .iter()
            .map(|(f, v)| wait(f.raw.raw, *v))
            .collect();
        let mut state = self.state.lock();
        waits.extend(state.pending_waits.drain(..).map(|s| wait(s, 0)));
        // New textures' first layout and sparse binds come first (resource.rs).
        if let Some((semaphore, value)) = s.internal.before_submit(s, self.raw, self.family)? {
            waits.push(wait(semaphore, value));
        }
        let info = VkSubmitInfo2 {
            waitSemaphoreInfoCount: waits.len() as u32,
            pWaitSemaphoreInfos: waits.as_ptr(),
            commandBufferInfoCount: lists.len() as u32,
            pCommandBufferInfos: lists.as_ptr(),
            signalSemaphoreInfoCount: signals.len() as u32,
            pSignalSemaphoreInfos: signals.as_ptr(),
            ..Default::default()
        };
        // SAFETY: the queue is locked; the lists are ended and of this queue's family (checked by
        // `api.rs`); the caller keeps what they use alive.
        s.lost
            .result(unsafe { (s.fns.vkQueueSubmit2)(self.raw, 1, &info, 0) })
    }

    /// Runs `f` with the queue locked, for other `vkQueue*` calls (present, sparse binding).
    pub(crate) fn with_lock<R>(&self, f: impl FnOnce(VkQueue, &mut QueueState) -> R) -> R {
        let mut state = self.state.lock();
        f(self.raw, &mut state)
    }

    /// The queue for an object that outlives this borrow of it (a swapchain).
    pub(crate) fn handle(&self) -> QueueHandle {
        QueueHandle {
            raw: self.raw,
            state: self.state.clone(),
        }
    }

    pub(crate) fn timestamp_frequency(&self) -> Result<u64> {
        let period = self.shared.properties.limits.timestampPeriod;
        let bits = self.shared.families.properties[self.family as usize].timestampValidBits;
        if bits == 0 || period <= 0.0 {
            return Err(unsupported());
        }
        Ok((1.0e9 / period as f64 + 0.5) as u64)
    }

    pub(crate) fn wait_idle(&self) -> Result<()> {
        let s = &self.shared;
        let _state = self.state.lock();
        s.lost.check()?;
        // SAFETY: the queue is locked.
        s.lost.result(unsafe { (s.fns.vkQueueWaitIdle)(self.raw) })
    }
}

/// A timeline semaphore.
pub(crate) struct Fence {
    shared: Arc<Shared>,
    pub(crate) raw: VkSemaphore,
    /// Created with export info (`create_shared_fence`): OS handles can be made of it.
    exportable: bool,
}

impl Fence {
    pub(crate) fn new(device: &Device, initial: u64, shared: bool) -> Result<Self> {
        Self::create(&device.shared, initial, shared)
    }

    pub(crate) fn create(shared: &Arc<Shared>, initial: u64, exportable: bool) -> Result<Self> {
        let export = VkExportSemaphoreCreateInfo {
            handleTypes: if cfg!(marlin_platform = "windows") {
                VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_WIN32_BIT
            } else {
                VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT
            },
            ..Default::default()
        };
        let timeline = VkSemaphoreTypeCreateInfo {
            pNext: if exportable {
                (&raw const export).cast()
            } else {
                ptr::null()
            },
            semaphoreType: VK_SEMAPHORE_TYPE_TIMELINE,
            initialValue: initial,
            ..Default::default()
        };
        let info = VkSemaphoreCreateInfo {
            pNext: (&raw const timeline).cast(),
            ..Default::default()
        };
        let mut raw = 0;
        // SAFETY: a valid create info chain and out pointer.
        shared.lost.result(unsafe {
            (shared.fns.vkCreateSemaphore)(shared.raw, &info, ptr::null(), &mut raw)
        })?;
        let mut fence = Self::from_raw(shared, raw);
        fence.exportable = exportable;
        Ok(fence)
    }

    /// Wraps a timeline semaphore of this device (created or imported), destroyed on drop.
    pub(crate) fn from_raw(shared: &Arc<Shared>, raw: VkSemaphore) -> Self {
        Self {
            shared: shared.clone(),
            raw,
            exportable: false,
        }
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    pub(crate) fn raw(&self) -> VkSemaphore {
        self.raw
    }

    pub(crate) fn exportable(&self) -> bool {
        self.exportable
    }

    pub(crate) fn completed_value(&self) -> u64 {
        let s = &self.shared;
        let mut value = 0;
        // SAFETY: a live timeline semaphore and out pointer.
        let r = unsafe { (s.fns.vkGetSemaphoreCounterValue)(s.raw, self.raw, &mut value) };
        let _ = s.lost.result(r);
        value
    }

    pub(crate) fn wait(&self, value: u64, timeout: Option<Duration>) -> Result<bool> {
        wait(&self.shared, &[self.raw], &[value], FenceWait::All, timeout)
    }

    pub(crate) fn signal(&self, value: u64) -> Result<()> {
        let s = &self.shared;
        s.lost.check()?;
        let info = VkSemaphoreSignalInfo {
            semaphore: self.raw,
            value,
            ..Default::default()
        };
        // SAFETY: a live timeline semaphore.
        s.lost
            .result(unsafe { (s.fns.vkSignalSemaphore)(s.raw, &info) })
    }
}

impl Drop for Fence {
    fn drop(&mut self) {
        // SAFETY: no pending submission uses the semaphore (the public API's contract).
        unsafe { (self.shared.fns.vkDestroySemaphore)(self.shared.raw, self.raw, ptr::null()) };
    }
}

fn wait(
    shared: &Shared,
    semaphores: &[VkSemaphore],
    values: &[u64],
    mode: FenceWait,
    timeout: Option<Duration>,
) -> Result<bool> {
    shared.lost.check()?;
    let info = VkSemaphoreWaitInfo {
        flags: match mode {
            FenceWait::All => 0,
            FenceWait::Any => VK_SEMAPHORE_WAIT_ANY_BIT,
        },
        semaphoreCount: semaphores.len() as u32,
        pSemaphores: semaphores.as_ptr(),
        pValues: values.as_ptr(),
        ..Default::default()
    };
    // SAFETY: live semaphores; both arrays hold `semaphoreCount` entries.
    match unsafe { (shared.fns.vkWaitSemaphores)(shared.raw, &info, timeout_ns(timeout)) } {
        VK_SUCCESS => shared.lost.check().map(|()| true),
        VK_TIMEOUT => Ok(false),
        r => {
            shared.lost.result(r)?;
            Err(error(r))
        }
    }
}

pub(crate) fn wait_for_fences(
    device: &Device,
    fences: &[(&PublicFence, u64)],
    mode: FenceWait,
    timeout: Option<Duration>,
) -> Result<bool> {
    let semaphores: Vec<VkSemaphore> = fences.iter().map(|(f, _)| f.raw.raw).collect();
    let values: Vec<u64> = fences.iter().map(|(_, v)| *v).collect();
    wait(&device.shared, &semaphores, &values, mode, timeout)
}
