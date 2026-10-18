//! The memory budget (`VK_EXT_memory_budget`) and residency (`VK_EXT_pageable_device_local_memory`
//! priorities; Vulkan has no explicit evict / make-resident).

use super::device::Device;
use super::ffi::*;
use super::unsupported;
use crate::{MemoryBudget, MemoryUsage, Pageable, ResidencyPriority, Result};

/// Local is the device-local heaps, non-local the rest. Without `VK_EXT_memory_budget` the
/// budget is the heaps' size and the usage unknown (0). Vulkan has no reservations.
pub(crate) fn memory_budget(device: &Device) -> Result<MemoryBudget> {
    let s = &device.shared;
    s.lost.check()?;
    let mut budget = VkPhysicalDeviceMemoryBudgetPropertiesEXT::default();
    let mut props = VkPhysicalDeviceMemoryProperties2 {
        pNext: if s.ext.memory_budget {
            (&raw mut budget).cast()
        } else {
            core::ptr::null_mut()
        },
        ..Default::default()
    };
    // SAFETY: the device's physical device and a valid output chain.
    unsafe { (s.instance.fns.vkGetPhysicalDeviceMemoryProperties2)(s.physical, &mut props) };
    let p = &props.memoryProperties;
    let mut result = MemoryBudget::default();
    for (i, heap) in p.memoryHeaps[..p.memoryHeapCount as usize]
        .iter()
        .enumerate()
    {
        let usage: &mut MemoryUsage = match heap.flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT {
            0 => &mut result.non_local,
            _ => &mut result.local,
        };
        if s.ext.memory_budget {
            usage.budget += budget.heapBudget[i];
            usage.usage += budget.heapUsage[i];
        } else {
            usage.budget += heap.size;
        }
    }
    Ok(result)
}

/// Vulkan cannot reserve memory: reporting `available_for_reservation` as 0, it accepts only a
/// reservation of nothing.
pub(crate) fn set_memory_reservation(device: &Device, local: u64, non_local: u64) -> Result<()> {
    device.lost.check()?;
    match (local, non_local) {
        (0, 0) => Ok(()),
        _ => Err(unsupported()),
    }
}

/// Priorities in `[0, 1]`, 0.5 being Vulkan's default.
fn priority_value(priority: ResidencyPriority) -> f32 {
    match priority {
        ResidencyPriority::Minimum => 0.1,
        ResidencyPriority::Low => 0.3,
        ResidencyPriority::Normal => 0.5,
        ResidencyPriority::High => 0.7,
        ResidencyPriority::Maximum => 1.0,
    }
}

/// Sets each object's memory priority, where the device takes priorities (else nothing happens:
/// the driver pages on its own).
fn set_priority(device: &Device, objects: &[Pageable<'_>], priority: f32) -> Result<()> {
    let s = &device.shared;
    s.lost.check()?;
    let Some(set) = s
        .fns
        .vkSetDeviceMemoryPriorityEXT
        .filter(|_| s.ext.pageable_device_local_memory)
    else {
        return Ok(());
    };
    for object in objects {
        let memory = match object {
            Pageable::Heap(h) => Some(h.raw.raw()),
            Pageable::Buffer(b) => b.raw.memory().map(|m| m.raw),
            Pageable::AccelerationStructure(a) => a.buffer().raw.memory().map(|m| m.raw),
            // TEMPORARY: textures' memory is the resource area's (0016 §2); they page at the
            // default priority until `Texture` exposes it.
            Pageable::Texture(_) => None,
        };
        if let Some(memory) = memory {
            // SAFETY: live memory of this device (committed resources own theirs; `api.rs`
            // refuses placed ones).
            unsafe { set(s.raw, memory, priority) };
        }
    }
    Ok(())
}

/// Back to the default priority: Vulkan pages memory in on use, so there is nothing to wait for.
pub(crate) fn make_resident(device: &Device, objects: &[Pageable<'_>]) -> Result<()> {
    set_priority(device, objects, priority_value(ResidencyPriority::Normal))
}

/// The lowest priority: the first memory the driver pages out under pressure.
pub(crate) fn evict(device: &Device, objects: &[Pageable<'_>]) -> Result<()> {
    set_priority(device, objects, 0.0)
}

pub(crate) fn set_residency_priority(
    device: &Device,
    objects: &[Pageable<'_>],
    priority: ResidencyPriority,
) -> Result<()> {
    set_priority(device, objects, priority_value(priority))
}
