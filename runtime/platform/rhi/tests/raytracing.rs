//! Acceleration structures and inline ray queries: builds, refits, compaction, and rays cast
//! from a compute shader through the bindless table. Hardware adapter, enhanced and legacy
//! barriers, validation on; skipped without ray query support.
#![cfg(marlin_platform = "windows")]

mod support;

use marlin_rhi::{
    AccelerationStructure, AccelerationStructureCopy, AccelerationStructureGeometry,
    AccelerationStructureInputs, AccelerationStructureInstance, AccelerationStructureKind,
    AccelerationStructureTriangles, Access, Buffer, BufferBarrier, BufferState, BufferUsage,
    BuildAccelerationStructure, BuildFlags, CommandList, Device, GeometryFlags, IndexFormat,
    InstanceFlags, MemoryLocation, Stage, VertexFormat,
};
use support::{
    buffer, compute_pipeline, copy_out, devices, read_bytes, read_words, readback, run, storage,
};

const RAYS: u32 = 16;
const MISS: u32 = u32::MAX;

/// Instance 9 (the triangle moved to x = -2) under rays 0..4, instance 7 under rays 8..12.
fn expected() -> Vec<u32> {
    (0..RAYS)
        .map(|i| match i {
            0..4 => 9 << 16,
            8..12 => 7 << 16,
            _ => MISS,
        })
        .collect()
}

struct Scene {
    vertices: Buffer,
    indices: Buffer,
    instances: Buffer,
    scratch: Buffer,
    blas: AccelerationStructure,
    tlas: AccelerationStructure,
}

fn input(device: &Device, bytes: &[u8]) -> Buffer {
    support::upload(device, bytes, BufferUsage::ACCELERATION_STRUCTURE_INPUT)
}

fn instance_records(blas: &AccelerationStructure) -> Vec<u8> {
    let mut moved = AccelerationStructureInstance::IDENTITY;
    moved[0][3] = -2.0;
    [(7, AccelerationStructureInstance::IDENTITY), (9, moved)]
        .iter()
        .flat_map(|&(id, transform)| {
            *AccelerationStructureInstance::new(
                transform,
                id,
                0xff,
                0,
                InstanceFlags::default(),
                blas,
            )
            .as_bytes()
        })
        .collect()
}

fn geometry(
    scene_vertices: &Buffer,
    indices: &Buffer,
) -> [AccelerationStructureGeometry<'static>; 0] {
    let _ = (scene_vertices, indices);
    []
}

fn triangles<'a>(vertices: &'a Buffer, indices: &'a Buffer) -> AccelerationStructureGeometry<'a> {
    AccelerationStructureGeometry::Triangles(AccelerationStructureTriangles {
        vertices,
        vertex_offset: 0,
        vertex_stride: 12,
        vertex_format: VertexFormat::Float3,
        vertex_count: 3,
        indices: Some((indices, 0, IndexFormat::U16)),
        triangle_count: 1,
        transform: None,
        flags: GeometryFlags::OPAQUE,
    })
}

const BLAS_FLAGS: BuildFlags = BuildFlags::ALLOW_UPDATE
    .union(BuildFlags::ALLOW_COMPACTION)
    .union(BuildFlags::PREFER_FAST_TRACE);

fn scene(device: &Device) -> Scene {
    let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let vertices = input(device, &support::bytes_of(&positions));
    let indices = input(device, &support::bytes_of(&[0u16, 1, 2, 0]));
    let blas_sizes = device
        .acceleration_structure_sizes(
            &AccelerationStructureInputs::BottomLevel(&[triangles(&vertices, &indices)]),
            BLAS_FLAGS,
        )
        .unwrap();
    let blas = device
        .create_acceleration_structure(AccelerationStructureKind::BottomLevel, blas_sizes.size)
        .unwrap();
    blas.set_name("triangle BLAS");
    let instances = input(device, &instance_records(&blas));
    let tlas_sizes = device
        .acceleration_structure_sizes(
            &AccelerationStructureInputs::TopLevel {
                instances: &instances,
                offset: 0,
                count: 2,
            },
            BuildFlags::PREFER_FAST_TRACE,
        )
        .unwrap();
    let tlas = device
        .create_acceleration_structure(AccelerationStructureKind::TopLevel, tlas_sizes.size)
        .unwrap();
    let scratch_size = [
        blas_sizes.build_scratch_size,
        blas_sizes.update_scratch_size,
        tlas_sizes.build_scratch_size,
    ]
    .into_iter()
    .max()
    .unwrap();
    let scratch = buffer(
        device,
        scratch_size,
        BufferUsage::SHADER_WRITE,
        MemoryLocation::GpuOnly,
    );
    Scene {
        vertices,
        indices,
        instances,
        scratch,
        blas,
        tlas,
    }
}

const SCRATCH_AGAIN: BufferState = BufferState::BUILD_SCRATCH;

fn build_blas(list: &mut CommandList, s: &Scene, update: bool) {
    list.buffer_barriers(&[
        BufferBarrier {
            buffer: &s.scratch,
            before: BufferState::NONE,
            after: BufferState::BUILD_SCRATCH,
        },
        BufferBarrier {
            buffer: s.blas.buffer(),
            before: match update {
                true => BufferState::ACCELERATION_STRUCTURE_READ,
                false => BufferState::NONE,
            },
            after: BufferState::ACCELERATION_STRUCTURE_WRITE,
        },
    ]);
    let geometries = [triangles(&s.vertices, &s.indices)];
    list.build_acceleration_structure(&BuildAccelerationStructure {
        dst: &s.blas,
        inputs: AccelerationStructureInputs::BottomLevel(&geometries),
        flags: BLAS_FLAGS,
        update: update.then_some(&s.blas),
        scratch: &s.scratch,
        scratch_offset: 0,
    });
    list.buffer_barriers(&[
        BufferBarrier {
            buffer: s.blas.buffer(),
            before: BufferState::ACCELERATION_STRUCTURE_WRITE,
            after: BufferState::ACCELERATION_STRUCTURE_READ,
        },
        BufferBarrier {
            buffer: &s.scratch,
            before: BufferState::BUILD_SCRATCH,
            after: SCRATCH_AGAIN,
        },
    ]);
}

fn build_tlas(list: &mut CommandList, s: &Scene, tlas: &AccelerationStructure) {
    list.buffer_barriers(&[BufferBarrier {
        buffer: tlas.buffer(),
        before: BufferState::NONE,
        after: BufferState::ACCELERATION_STRUCTURE_WRITE,
    }]);
    list.build_acceleration_structure(&BuildAccelerationStructure {
        dst: tlas,
        inputs: AccelerationStructureInputs::TopLevel {
            instances: &s.instances,
            offset: 0,
            count: 2,
        },
        flags: BuildFlags::PREFER_FAST_TRACE,
        update: None,
        scratch: &s.scratch,
        scratch_offset: 0,
    });
    list.buffer_barriers(&[
        BufferBarrier {
            buffer: tlas.buffer(),
            before: BufferState::ACCELERATION_STRUCTURE_WRITE,
            after: BufferState::ACCELERATION_STRUCTURE_READ,
        },
        BufferBarrier {
            buffer: &s.scratch,
            before: BufferState::BUILD_SCRATCH,
            after: SCRATCH_AGAIN,
        },
    ]);
}

/// Casts the test's rays against `tlas` and returns what each hit.
fn cast(
    device: &Device,
    tlas: &AccelerationStructure,
    record: impl FnOnce(&mut CommandList),
) -> Vec<u32> {
    let pipeline = compute_pipeline(device, "rayquery");
    let (out, view) = storage(device, RAYS as u64);
    let rb = readback(device, RAYS as u64 * 4);
    run(device, |list| {
        record(list);
        list.set_compute_pipeline(&pipeline);
        list.push_constants(0, &[tlas.index(), view.index(), RAYS]);
        list.dispatch(1, 1, 1);
        copy_out(list, &out, &rb, RAYS as u64);
    });
    read_words(&rb, 0, RAYS as usize)
}

#[test]
fn ray_queries_hit_built_instances() {
    for device in devices() {
        if !device.features().ray_query {
            return;
        }
        let s = scene(device);
        let hits = cast(device, &s.tlas, |list| {
            build_blas(list, &s, false);
            build_tlas(list, &s, &s.tlas);
        });
        assert_eq!(hits, expected());
        // A refit of the bottom level (same triangle) keeps every hit.
        let hits = cast(device, &s.tlas, |list| {
            build_blas(list, &s, true);
            build_tlas(list, &s, &s.tlas);
        });
        assert_eq!(hits, expected());
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn compacted_structures_trace_the_same() {
    for device in devices() {
        if !device.features().ray_query {
            return;
        }
        let s = scene(device);
        let sizes = buffer(
            device,
            8,
            BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
            MemoryLocation::GpuOnly,
        );
        let sizes_rb = readback(device, 8);
        let emitted = BufferState {
            stage: Stage::EMIT_ACCELERATION_STRUCTURE_INFO,
            access: Access::UNORDERED_ACCESS,
        };
        run(device, |list| {
            build_blas(list, &s, false);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &sizes,
                before: BufferState::NONE,
                after: emitted,
            }]);
            list.write_compacted_sizes(&[&s.blas], &sizes, 0);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &sizes,
                before: emitted,
                after: BufferState::COPY_SRC,
            }]);
            list.copy_buffer(&sizes, 0, &sizes_rb, 0, 8);
        });
        let compacted_size = u64::from_le_bytes(read_bytes(&sizes_rb, 0, 8).try_into().unwrap());
        assert!(
            compacted_size > 0 && compacted_size <= s.blas.size(),
            "{compacted_size} of {}",
            s.blas.size()
        );
        let compact = device
            .create_acceleration_structure(AccelerationStructureKind::BottomLevel, compacted_size)
            .unwrap();
        // Point the instances at the compacted copy (the GPU is done with them).
        // SAFETY: no submitted work reads the instance buffer any more.
        unsafe { s.instances.write(0, &instance_records(&compact)) };
        let hits = cast(device, &s.tlas, |list| {
            list.buffer_barriers(&[BufferBarrier {
                buffer: compact.buffer(),
                before: BufferState::NONE,
                after: BufferState::ACCELERATION_STRUCTURE_WRITE,
            }]);
            list.copy_acceleration_structure(&s.blas, &compact, AccelerationStructureCopy::Compact);
            list.buffer_barriers(&[BufferBarrier {
                buffer: compact.buffer(),
                before: BufferState::ACCELERATION_STRUCTURE_WRITE,
                after: BufferState::ACCELERATION_STRUCTURE_READ,
            }]);
            build_tlas(list, &s, &s.tlas);
        });
        assert_eq!(hits, expected());
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn builds_check_their_inputs() {
    let device = support::hardware_device();
    if !device.features().ray_query {
        return;
    }
    let s = scene(device);
    let plain = buffer(device, 64, BufferUsage::COPY_DST, MemoryLocation::GpuOnly);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(device, |list| {
            let geometries = [triangles(&plain, &s.indices)];
            list.build_acceleration_structure(&BuildAccelerationStructure {
                dst: &s.blas,
                inputs: AccelerationStructureInputs::BottomLevel(&geometries),
                flags: BLAS_FLAGS,
                update: None,
                scratch: &s.scratch,
                scratch_offset: 0,
            });
        });
    }));
    assert!(
        result.is_err(),
        "vertices without ACCELERATION_STRUCTURE_INPUT usage"
    );
    // Top-level structures built from bottom-level inputs are refused too.
    let wrong = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(device, |list| {
            let geometries = [triangles(&s.vertices, &s.indices)];
            list.build_acceleration_structure(&BuildAccelerationStructure {
                dst: &s.tlas,
                inputs: AccelerationStructureInputs::BottomLevel(&geometries),
                flags: BLAS_FLAGS,
                update: None,
                scratch: &s.scratch,
                scratch_offset: 0,
            });
        });
    }));
    assert!(wrong.is_err());
    let _ = geometry(&s.vertices, &s.indices);
}
