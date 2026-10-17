//! The memory budget (`VK_EXT_memory_budget`) and residency (`VK_EXT_pageable_device_local_memory`
//! priorities; Vulkan has no explicit evict / make-resident).

use super::device::Device;
use crate::{MemoryBudget, Pageable, ResidencyPriority, Result};

pub(crate) fn memory_budget(device: &Device) -> Result<MemoryBudget> {
    let _ = device;
    todo!("vulkan: memory_budget")
}

pub(crate) fn set_memory_reservation(device: &Device, local: u64, non_local: u64) -> Result<()> {
    let _ = (device, local, non_local);
    todo!("vulkan: set_memory_reservation")
}

pub(crate) fn make_resident(device: &Device, objects: &[Pageable<'_>]) -> Result<()> {
    let _ = (device, objects);
    todo!("vulkan: make_resident")
}

pub(crate) fn evict(device: &Device, objects: &[Pageable<'_>]) -> Result<()> {
    let _ = (device, objects);
    todo!("vulkan: evict")
}

pub(crate) fn set_residency_priority(
    device: &Device,
    objects: &[Pageable<'_>],
    priority: ResidencyPriority,
) -> Result<()> {
    let _ = (device, objects, priority);
    todo!("vulkan: set_residency_priority")
}
