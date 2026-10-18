//! Acceleration structures (`VK_KHR_acceleration_structure`) for inline ray queries.

use core::ptr;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::command::CommandList;
use super::descriptor::{Descriptor, Table};
use super::device::{Device, Shared};
use super::ffi::*;
use super::query;
use super::resource::Buffer;
use super::unsupported;
use crate::{
    AccelerationStructureCopy, AccelerationStructureGeometry, AccelerationStructureInputs,
    AccelerationStructureKind, AccelerationStructureSizes, BuildAccelerationStructure, BuildFlags,
    GeometryFlags, IndexFormat, Result, VertexFormat,
};

pub(crate) struct AccelerationStructure {
    shared: Arc<Shared>,
    raw: VkAccelerationStructureKHR,
    address: u64,
    /// Its resource-heap slot: the structure's descriptor for top-level structures (what ray
    /// queries open), left null for bottom-level ones, which shaders cannot open.
    descriptor: Descriptor,
    /// One `ACCELERATION_STRUCTURE_COMPACTED_SIZE` query for `write_compacted_sizes` (Vulkan
    /// writes structure properties into query pools, not buffers).
    compacted_size: VkQueryPool,
}

impl AccelerationStructure {
    pub(crate) fn raw(&self) -> u64 {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    pub(crate) fn index(&self) -> u32 {
        self.descriptor.index()
    }

    pub(crate) fn address(&self) -> u64 {
        self.address
    }
}

impl Drop for AccelerationStructure {
    fn drop(&mut self) {
        let s = &self.shared;
        query::destroy_pool(s, self.compacted_size);
        if let Some(destroy) = s.fns.vkDestroyAccelerationStructureKHR {
            // SAFETY: no pending submission uses the structure (the public API's contract).
            unsafe { destroy(s.raw, self.raw, ptr::null()) };
        }
    }
}

fn kind_type(kind: AccelerationStructureKind) -> VkAccelerationStructureTypeKHR {
    match kind {
        AccelerationStructureKind::BottomLevel => VK_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL_KHR,
        AccelerationStructureKind::TopLevel => VK_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL_KHR,
    }
}

fn build_flags(flags: BuildFlags) -> VkBuildAccelerationStructureFlagsKHR {
    [
        (
            BuildFlags::ALLOW_UPDATE,
            VK_BUILD_ACCELERATION_STRUCTURE_ALLOW_UPDATE_BIT_KHR,
        ),
        (
            BuildFlags::ALLOW_COMPACTION,
            VK_BUILD_ACCELERATION_STRUCTURE_ALLOW_COMPACTION_BIT_KHR,
        ),
        (
            BuildFlags::PREFER_FAST_TRACE,
            VK_BUILD_ACCELERATION_STRUCTURE_PREFER_FAST_TRACE_BIT_KHR,
        ),
        (
            BuildFlags::PREFER_FAST_BUILD,
            VK_BUILD_ACCELERATION_STRUCTURE_PREFER_FAST_BUILD_BIT_KHR,
        ),
        (
            BuildFlags::MINIMIZE_MEMORY,
            VK_BUILD_ACCELERATION_STRUCTURE_LOW_MEMORY_BIT_KHR,
        ),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .fold(0, |all, (_, bit)| all | bit)
}

fn geometry_flags(flags: GeometryFlags) -> VkGeometryFlagsKHR {
    let mut bits = 0;
    if flags.contains(GeometryFlags::OPAQUE) {
        bits |= VK_GEOMETRY_OPAQUE_BIT_KHR;
    }
    if flags.contains(GeometryFlags::NO_DUPLICATE_ANY_HIT) {
        bits |= VK_GEOMETRY_NO_DUPLICATE_ANY_HIT_INVOCATION_BIT_KHR;
    }
    bits
}

fn vertex_format(format: VertexFormat) -> VkFormat {
    match format {
        VertexFormat::Float3 => VK_FORMAT_R32G32B32_SFLOAT,
        VertexFormat::Float2 => VK_FORMAT_R32G32_SFLOAT,
        VertexFormat::Half4 => VK_FORMAT_R16G16B16A16_SFLOAT,
        VertexFormat::Half2 => VK_FORMAT_R16G16_SFLOAT,
        VertexFormat::Snorm16x4 => VK_FORMAT_R16G16B16A16_SNORM,
        VertexFormat::Snorm16x2 => VK_FORMAT_R16G16_SNORM,
    }
}

fn address(at: u64) -> VkDeviceOrHostAddressConstKHR {
    VkDeviceOrHostAddressConstKHR { deviceAddress: at }
}

/// The inputs as Vulkan geometries, with each geometry's primitive count.
struct Geometries {
    ty: VkAccelerationStructureTypeKHR,
    geometries: Vec<VkAccelerationStructureGeometryKHR>,
    ranges: Vec<VkAccelerationStructureBuildRangeInfoKHR>,
}

impl Geometries {
    fn new(inputs: &AccelerationStructureInputs<'_>) -> Self {
        let mut geometries = Vec::new();
        let mut ranges = Vec::new();
        let mut push = |ty, data, flags, count| {
            geometries.push(VkAccelerationStructureGeometryKHR {
                geometryType: ty,
                geometry: data,
                flags,
                ..Default::default()
            });
            ranges.push(VkAccelerationStructureBuildRangeInfoKHR {
                primitiveCount: count,
                ..Default::default()
            });
        };
        match inputs {
            AccelerationStructureInputs::BottomLevel(list) => {
                for geometry in *list {
                    match geometry {
                        AccelerationStructureGeometry::Triangles(t) => {
                            let (index_type, index_data) = match t.indices {
                                Some((buffer, offset, format)) => (
                                    match format {
                                        IndexFormat::U16 => VK_INDEX_TYPE_UINT16,
                                        IndexFormat::U32 => VK_INDEX_TYPE_UINT32,
                                    },
                                    buffer.raw.address() + offset,
                                ),
                                None => (VK_INDEX_TYPE_NONE_KHR, 0),
                            };
                            let transform = t
                                .transform
                                .map_or(0, |(buffer, offset)| buffer.raw.address() + offset);
                            let triangles = VkAccelerationStructureGeometryTrianglesDataKHR {
                                vertexFormat: vertex_format(t.vertex_format),
                                vertexData: address(t.vertices.raw.address() + t.vertex_offset),
                                vertexStride: t.vertex_stride,
                                maxVertex: t.vertex_count - 1,
                                indexType: index_type,
                                indexData: address(index_data),
                                // Row-major 3×4, as `VkTransformMatrixKHR`.
                                transformData: address(transform),
                                ..Default::default()
                            };
                            push(
                                VK_GEOMETRY_TYPE_TRIANGLES_KHR,
                                VkAccelerationStructureGeometryDataKHR { triangles },
                                geometry_flags(t.flags),
                                t.triangle_count,
                            );
                        }
                        AccelerationStructureGeometry::Aabbs(a) => {
                            let aabbs = VkAccelerationStructureGeometryAabbsDataKHR {
                                data: address(a.buffer.raw.address() + a.offset),
                                stride: a.stride,
                                ..Default::default()
                            };
                            push(
                                VK_GEOMETRY_TYPE_AABBS_KHR,
                                VkAccelerationStructureGeometryDataKHR { aabbs },
                                geometry_flags(a.flags),
                                a.count,
                            );
                        }
                    }
                }
            }
            AccelerationStructureInputs::TopLevel {
                instances,
                offset,
                count,
            } => {
                // `AccelerationStructureInstance` is `VkAccelerationStructureInstanceKHR`'s layout
                // (and D3D12's), with the bottom level named by its device address.
                let instances = VkAccelerationStructureGeometryInstancesDataKHR {
                    arrayOfPointers: VK_FALSE,
                    data: address(instances.raw.address() + offset),
                    ..Default::default()
                };
                push(
                    VK_GEOMETRY_TYPE_INSTANCES_KHR,
                    VkAccelerationStructureGeometryDataKHR { instances },
                    0,
                    *count,
                );
            }
        }
        Self {
            ty: kind_type(inputs.kind()),
            geometries,
            ranges,
        }
    }

    /// The build info, pointing into `self`.
    fn info(&self, flags: BuildFlags) -> VkAccelerationStructureBuildGeometryInfoKHR {
        VkAccelerationStructureBuildGeometryInfoKHR {
            r#type: self.ty,
            flags: build_flags(flags),
            mode: VK_BUILD_ACCELERATION_STRUCTURE_MODE_BUILD_KHR,
            geometryCount: self.geometries.len() as u32,
            pGeometries: self.geometries.as_ptr(),
            ..Default::default()
        }
    }
}

fn sizes_of(
    s: &Shared,
    inputs: &AccelerationStructureInputs<'_>,
    flags: BuildFlags,
) -> AccelerationStructureSizes {
    let Some(get) = s.fns.vkGetAccelerationStructureBuildSizesKHR else {
        return AccelerationStructureSizes::default();
    };
    let geometries = Geometries::new(inputs);
    let info = geometries.info(flags);
    let counts: Vec<u32> = geometries.ranges.iter().map(|r| r.primitiveCount).collect();
    let mut sizes = VkAccelerationStructureBuildSizesInfoKHR::default();
    // SAFETY: a valid build info (addresses are ignored) with one count per geometry.
    unsafe {
        get(
            s.raw,
            VK_ACCELERATION_STRUCTURE_BUILD_TYPE_DEVICE_KHR,
            &info,
            counts.as_ptr(),
            &mut sizes,
        )
    };
    AccelerationStructureSizes {
        size: sizes.accelerationStructureSize,
        build_scratch_size: sizes.buildScratchSize,
        update_scratch_size: sizes.updateScratchSize,
    }
}

pub(crate) fn sizes(
    device: &Device,
    inputs: &AccelerationStructureInputs<'_>,
    flags: BuildFlags,
) -> AccelerationStructureSizes {
    sizes_of(&device.shared, inputs, flags)
}

pub(crate) fn create(
    device: &Device,
    storage: &Buffer,
    offset: u64,
    size: u64,
    kind: AccelerationStructureKind,
) -> Result<(Buffer, AccelerationStructure)> {
    let s = &device.shared;
    s.lost.check()?;
    let f = &s.fns;
    let (Some(create), Some(destroy), Some(get_address)) = (
        f.vkCreateAccelerationStructureKHR,
        f.vkDestroyAccelerationStructureKHR,
        f.vkGetAccelerationStructureDeviceAddressKHR,
    ) else {
        return Err(unsupported());
    };
    let info = VkAccelerationStructureCreateInfoKHR {
        buffer: storage.raw(),
        offset,
        size,
        r#type: kind_type(kind),
        ..Default::default()
    };
    let mut raw = 0;
    // SAFETY: a valid create info: an aligned range of an acceleration structure storage buffer
    // (checked by `api.rs`).
    let r = unsafe { create(s.raw, &info, ptr::null(), &mut raw) };
    s.lost.result(r)?;
    let fail = |e| {
        // SAFETY: the structure was just created and nothing uses it.
        unsafe { destroy(s.raw, raw, ptr::null()) };
        e
    };
    let address_info = VkAccelerationStructureDeviceAddressInfoKHR {
        accelerationStructure: raw,
        ..Default::default()
    };
    // SAFETY: a live structure.
    let address = unsafe { get_address(s.raw, &address_info) };
    let descriptor = Descriptor::new(s, Table::Resources).map_err(fail)?;
    if kind == AccelerationStructureKind::TopLevel {
        let range = VkDeviceAddressRangeKHR { address, size };
        let write = VkResourceDescriptorInfoEXT {
            r#type: VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR,
            data: VkResourceDescriptorDataEXT {
                pAddressRange: &range,
            },
            ..Default::default()
        };
        descriptor.write_resource(&write).map_err(fail)?;
    }
    let compacted_size = query::create_raw_pool(
        s,
        VK_QUERY_TYPE_ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR,
        1,
    )
    .map_err(fail)?;
    Ok((
        storage.clone(),
        AccelerationStructure {
            shared: s.clone(),
            raw,
            address,
            descriptor,
            compacted_size,
        },
    ))
}

pub(crate) fn build_sizes(
    list: &mut CommandList,
    inputs: &AccelerationStructureInputs<'_>,
    flags: BuildFlags,
) -> AccelerationStructureSizes {
    sizes_of(list.shared(), inputs, flags)
}

pub(crate) fn build(list: &mut CommandList, build: &BuildAccelerationStructure<'_>) {
    let Some(cmd) = list.fns().vkCmdBuildAccelerationStructuresKHR else {
        return;
    };
    let geometries = Geometries::new(&build.inputs);
    let mut info = geometries.info(build.flags);
    if let Some(src) = build.update {
        info.mode = VK_BUILD_ACCELERATION_STRUCTURE_MODE_UPDATE_KHR;
        info.srcAccelerationStructure = src.raw.raw;
    }
    info.dstAccelerationStructure = build.dst.raw.raw;
    info.scratchData = VkDeviceOrHostAddressKHR {
        deviceAddress: build.scratch.raw.address() + build.scratch_offset,
    };
    let ranges = geometries.ranges.as_ptr();
    // SAFETY: a recording buffer outside a render pass; inputs, scratch and destination checked
    // by `commands.rs`; one range per geometry.
    unsafe { cmd(list.raw(), 1, &info, &ranges) };
}

pub(crate) fn copy(
    list: &mut CommandList,
    src: &crate::AccelerationStructure,
    dst: &crate::AccelerationStructure,
    mode: AccelerationStructureCopy,
) {
    let Some(cmd) = list.fns().vkCmdCopyAccelerationStructureKHR else {
        return;
    };
    let info = VkCopyAccelerationStructureInfoKHR {
        src: src.raw.raw,
        dst: dst.raw.raw,
        mode: match mode {
            AccelerationStructureCopy::Clone => VK_COPY_ACCELERATION_STRUCTURE_MODE_CLONE_KHR,
            AccelerationStructureCopy::Compact => VK_COPY_ACCELERATION_STRUCTURE_MODE_COMPACT_KHR,
        },
        ..Default::default()
    };
    // SAFETY: a recording buffer outside a render pass and two built structures of one kind.
    unsafe { cmd(list.raw(), &info) };
}

/// Each structure writes its size into its own query, which is copied into `dst` (in the
/// caller's `EMIT_ACCELERATION_STRUCTURE_INFO` state, which maps to the build stage): barriers
/// around the copy order it after earlier build-stage work and before later build-stage work, so
/// the caller's own barriers chain through it.
pub(crate) fn write_compacted_sizes(
    list: &mut CommandList,
    structures: &[&crate::AccelerationStructure],
    dst: &crate::Buffer,
    offset: u64,
) {
    let Some(write) = list.fns().vkCmdWriteAccelerationStructuresPropertiesKHR else {
        return;
    };
    let (fns, raw) = (list.fns(), list.raw());
    let dst = dst.raw.raw();
    let barrier = |src_stage, src_access, dst_stage, dst_access| {
        let barrier = VkMemoryBarrier2 {
            srcStageMask: src_stage,
            srcAccessMask: src_access,
            dstStageMask: dst_stage,
            dstAccessMask: dst_access,
            ..Default::default()
        };
        let info = VkDependencyInfo {
            memoryBarrierCount: 1,
            pMemoryBarriers: &barrier,
            ..Default::default()
        };
        // SAFETY: a recording buffer and a valid barrier.
        unsafe { (fns.vkCmdPipelineBarrier2)(raw, &info) };
    };
    let build_stage = VK_PIPELINE_STAGE_2_ACCELERATION_STRUCTURE_BUILD_BIT_KHR;
    let build_access = VK_ACCESS_2_ACCELERATION_STRUCTURE_READ_BIT_KHR
        | VK_ACCESS_2_ACCELERATION_STRUCTURE_WRITE_BIT_KHR;
    for structure in structures {
        let s = &structure.raw;
        // SAFETY: a recording buffer outside a render pass; the query is reset before the write
        // (query commands run in order), and the structure was built with compaction allowed.
        unsafe {
            (fns.vkCmdResetQueryPool)(raw, s.compacted_size, 0, 1);
            write(
                raw,
                1,
                &s.raw,
                VK_QUERY_TYPE_ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR,
                s.compacted_size,
                0,
            );
        }
    }
    barrier(
        build_stage,
        build_access,
        VK_PIPELINE_STAGE_2_COPY_BIT,
        VK_ACCESS_2_TRANSFER_WRITE_BIT,
    );
    for (i, structure) in structures.iter().enumerate() {
        let pool = structure.raw.compacted_size;
        // SAFETY: a recording buffer outside a render pass; the word lies within `dst` (checked
        // by `commands.rs`). The query was written above and is reset before its next write.
        unsafe {
            (fns.vkCmdCopyQueryPoolResults)(
                raw,
                pool,
                0,
                1,
                dst,
                offset + 8 * i as u64,
                8,
                VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT,
            )
        };
    }
    barrier(
        VK_PIPELINE_STAGE_2_COPY_BIT,
        VK_ACCESS_2_TRANSFER_WRITE_BIT,
        build_stage,
        build_access,
    );
}
