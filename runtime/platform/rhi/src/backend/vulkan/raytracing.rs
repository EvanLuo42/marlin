//! Acceleration structures (`VK_KHR_acceleration_structure`) for inline ray queries.

use alloc::sync::Arc;

use super::command::CommandList;
use super::device::{Device, Shared};
use super::resource::Buffer;
use crate::{
    AccelerationStructureCopy, AccelerationStructureInputs, AccelerationStructureKind,
    AccelerationStructureSizes, BuildAccelerationStructure, BuildFlags, Result,
};

pub(crate) struct AccelerationStructure {
    shared: Arc<Shared>,
    raw: u64,
}

impl AccelerationStructure {
    pub(crate) fn raw(&self) -> u64 {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    pub(crate) fn index(&self) -> u32 {
        todo!("vulkan: acceleration structure index")
    }

    pub(crate) fn address(&self) -> u64 {
        todo!("vulkan: acceleration structure address")
    }
}

pub(crate) fn sizes(
    device: &Device,
    inputs: &AccelerationStructureInputs<'_>,
    flags: BuildFlags,
) -> AccelerationStructureSizes {
    let _ = (device, inputs, flags);
    todo!("vulkan: acceleration_structure_sizes")
}

pub(crate) fn create(
    device: &Device,
    storage: &Buffer,
    offset: u64,
    kind: AccelerationStructureKind,
) -> Result<(Buffer, AccelerationStructure)> {
    let _ = (device, storage, offset, kind);
    todo!("vulkan: create_acceleration_structure")
}

pub(crate) fn build_sizes(
    list: &mut CommandList,
    inputs: &AccelerationStructureInputs<'_>,
    flags: BuildFlags,
) -> AccelerationStructureSizes {
    let _ = (list, inputs, flags);
    todo!("vulkan: build_sizes")
}

pub(crate) fn build(list: &mut CommandList, build: &BuildAccelerationStructure<'_>) {
    let _ = (list, build);
    todo!("vulkan: build_acceleration_structure")
}

pub(crate) fn copy(
    list: &mut CommandList,
    src: &crate::AccelerationStructure,
    dst: &crate::AccelerationStructure,
    mode: AccelerationStructureCopy,
) {
    let _ = (list, src, dst, mode);
    todo!("vulkan: copy_acceleration_structure")
}

pub(crate) fn write_compacted_sizes(
    list: &mut CommandList,
    structures: &[&crate::AccelerationStructure],
    dst: &crate::Buffer,
    offset: u64,
) {
    let _ = (list, structures, dst, offset);
    todo!("vulkan: write_compacted_sizes")
}
