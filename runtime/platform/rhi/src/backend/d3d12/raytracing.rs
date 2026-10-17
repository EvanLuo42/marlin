//! Acceleration structures for inline ray queries (DXR 1.1 `RayQuery`): storage, bindless
//! views, sizes and the translation of build inputs.
//!
//! A structure is a range of a committed buffer created in, and never leaving, the
//! `RAYTRACING_ACCELERATION_STRUCTURE` state; its bindless slot holds an SRV of dimension
//! `RAYTRACING_ACCELERATION_STRUCTURE` that shaders open as `RaytracingAccelerationStructure`.

use alloc::vec::Vec;

use super::Device;
use super::com::*;
use super::ffi::*;
use super::resource::{Buffer, Descriptor, dxgi_format};
use crate::{
    AccelerationStructureGeometry, AccelerationStructureInputs, AccelerationStructureKind,
    AccelerationStructureSizes, BuildFlags, Format, IndexFormat, Result, VertexFormat,
};

pub(crate) struct AccelerationStructure {
    /// The storage buffer's resource (also held by the public wrapper's `Buffer`).
    pub(super) resource: Com<ID3D12Resource>,
    pub(super) address: u64,
    pub(super) view: Descriptor,
}

pub(super) fn create(
    device: &Device,
    _kind: AccelerationStructureKind,
    size: u64,
) -> Result<(Buffer, AccelerationStructure)> {
    let buffer = super::resource::create_acceleration_structure_buffer(device, size)?;
    let view = Descriptor::new(&device.bindless.resources)?;
    // The view's union holds `RaytracingAccelerationStructure.Location`: the address, low word
    // first. The resource itself is not named.
    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_UNKNOWN,
        ViewDimension: D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        u: ViewUnion([
            buffer.address as u32,
            (buffer.address >> 32) as u32,
            0,
            0,
            0,
            0,
        ]),
    };
    let d = &device.device;
    // SAFETY: valid device; acceleration structure views take no resource; the slot is ours.
    unsafe {
        (d.vtbl().CreateShaderResourceView)(d.as_raw(), core::ptr::null_mut(), &desc, view.handle())
    };
    let structure = AccelerationStructure {
        resource: buffer.resource.clone(),
        address: buffer.address,
        view,
    };
    Ok((buffer, structure))
}

fn vertex_format(format: VertexFormat) -> u32 {
    match format {
        VertexFormat::Float3 => DXGI_FORMAT_R32G32B32_FLOAT,
        VertexFormat::Float2 => DXGI_FORMAT_R32G32_FLOAT,
        VertexFormat::Half4 => dxgi_format(Format::Rgba16Float),
        VertexFormat::Half2 => dxgi_format(Format::Rg16Float),
        VertexFormat::Snorm16x4 => dxgi_format(Format::Rgba16Snorm),
        VertexFormat::Snorm16x2 => dxgi_format(Format::Rg16Snorm),
    }
}

/// D3D12's inputs for `inputs`, with the geometry descriptions they point at (keep `geometries`
/// alive while the inputs are used).
pub(super) fn inputs(
    inputs: &AccelerationStructureInputs<'_>,
    flags: BuildFlags,
    geometries: &mut Vec<D3D12_RAYTRACING_GEOMETRY_DESC>,
) -> D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
    match *inputs {
        AccelerationStructureInputs::BottomLevel(list) => {
            geometries.clear();
            geometries.extend(list.iter().map(|g| match *g {
                AccelerationStructureGeometry::Triangles(t) => {
                    let (index_buffer, index_format, index_count) = match t.indices {
                        Some((buffer, offset, format)) => (
                            buffer.raw.address + offset,
                            match format {
                                IndexFormat::U16 => DXGI_FORMAT_R16_UINT,
                                IndexFormat::U32 => DXGI_FORMAT_R32_UINT,
                            },
                            t.triangle_count * 3,
                        ),
                        None => (0, DXGI_FORMAT_UNKNOWN, 0),
                    };
                    D3D12_RAYTRACING_GEOMETRY_DESC {
                        Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
                        Flags: t.flags.bits(),
                        Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                            Transform3x4: t
                                .transform
                                .map_or(0, |(buffer, offset)| buffer.raw.address + offset),
                            IndexFormat: index_format,
                            VertexFormat: vertex_format(t.vertex_format),
                            IndexCount: index_count,
                            VertexCount: t.vertex_count,
                            IndexBuffer: index_buffer,
                            VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                                StartAddress: t.vertices.raw.address + t.vertex_offset,
                                StrideInBytes: t.vertex_stride,
                            },
                        },
                    }
                }
                AccelerationStructureGeometry::Aabbs(a) => D3D12_RAYTRACING_GEOMETRY_DESC::aabbs(
                    a.flags.bits(),
                    D3D12_RAYTRACING_GEOMETRY_AABBS_DESC {
                        AABBCount: a.count as u64,
                        AABBs: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                            StartAddress: a.buffer.raw.address + a.offset,
                            StrideInBytes: a.stride,
                        },
                    },
                ),
            }));
            D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                Flags: flags.bits(),
                NumDescs: geometries.len() as u32,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Descs: geometries.as_ptr() as usize as u64,
            }
        }
        AccelerationStructureInputs::TopLevel {
            instances,
            offset,
            count,
        } => D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
            Flags: flags.bits(),
            NumDescs: count,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Descs: instances.raw.address + offset,
        },
    }
}

pub(super) fn sizes(
    device: &Device,
    structure_inputs: &AccelerationStructureInputs<'_>,
    flags: BuildFlags,
) -> AccelerationStructureSizes {
    let mut geometries = Vec::new();
    let inputs = inputs(structure_inputs, flags, &mut geometries);
    let mut info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
    let d = &device.device5;
    // SAFETY: valid interface; `inputs` points at `geometries`, alive for the call.
    unsafe {
        (d.vtbl().GetRaytracingAccelerationStructurePrebuildInfo)(d.as_raw(), &inputs, &mut info)
    };
    let align = |n: u64| n.next_multiple_of(crate::ACCELERATION_STRUCTURE_ALIGNMENT);
    AccelerationStructureSizes {
        size: align(info.ResultDataMaxSizeInBytes),
        build_scratch_size: align(info.ScratchDataSizeInBytes),
        update_scratch_size: align(info.UpdateScratchDataSizeInBytes),
    }
}
