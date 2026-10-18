//! Acceleration structures and inline ray queries: builds, refits, compaction, and rays cast
//! from a compute shader through the bindless table. Hardware adapter, each barrier
//! path, validation on; skipped without ray query support.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    ACCELERATION_STRUCTURE_ALIGNMENT, AccelerationStructure, AccelerationStructureAabbs,
    AccelerationStructureCopy, AccelerationStructureGeometry, AccelerationStructureInputs,
    AccelerationStructureInstance, AccelerationStructureKind, AccelerationStructureTriangles,
    Access, Buffer, BufferBarrier, BufferDesc, BufferState, BufferUsage, BufferViewDesc,
    BufferViewKind, BuildAccelerationStructure, BuildFlags, CommandList, Device, GeometryFlags,
    HeapDesc, HeapKind, IndexFormat, InstanceFlags, MemoryLocation, QueueType, Stage, VertexFormat,
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
    let pipeline = compute_pipeline(device, "TraceRays");
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
        support::require_feature!(device, ray_query);
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
        support::require_feature!(device, ray_query);
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
    support::require_feature!(device, ray_query);
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
}

/// Two bottom-level structures sub-allocated from one storage buffer placed in a heap, built
/// together and published to the top-level build and the ray queries by memory barriers alone:
/// the render graph's group of structures.
#[test]
fn sub_allocated_structures_sync_as_a_group() {
    for device in devices() {
        support::require_feature!(device, ray_query);
        let s = scene(device);
        let geometries = [triangles(&s.vertices, &s.indices)];
        let blas_sizes = device
            .acceleration_structure_sizes(
                &AccelerationStructureInputs::BottomLevel(&geometries),
                BuildFlags::PREFER_FAST_TRACE,
            )
            .unwrap();
        // Packed at the structure alignment, far below a placed resource's.
        let stride = blas_sizes
            .size
            .next_multiple_of(ACCELERATION_STRUCTURE_ALIGNMENT);
        let desc = BufferDesc {
            size: 2 * stride,
            usage: BufferUsage::ACCELERATION_STRUCTURE_STORAGE,
            memory: MemoryLocation::GpuOnly,
        };
        let req = device.buffer_requirements(&desc);
        let heap = device
            .create_heap(&HeapDesc {
                size: req.size,
                memory: MemoryLocation::GpuOnly,
                kind: HeapKind::Buffers,
            })
            .unwrap();
        let storage = device.create_placed_buffer(&heap, 0, &desc).unwrap();
        let place = |i: u64| {
            device
                .create_acceleration_structure_in(
                    &storage,
                    i * stride,
                    AccelerationStructureKind::BottomLevel,
                    blas_sizes.size,
                )
                .unwrap()
        };
        let (a, b) = (place(0), place(1));
        assert_eq!(b.instance_address(), a.instance_address() + stride);
        // Instance 7 uses the first, the moved instance 9 the second.
        let mut records = instance_records(&a);
        records[64..].copy_from_slice(&instance_records(&b)[64..]);
        // SAFETY: no submitted work reads the instance buffer.
        unsafe { s.instances.write(0, &records) };
        // One scratch range per build: the two run unordered.
        let scratch_stride = blas_sizes
            .build_scratch_size
            .next_multiple_of(ACCELERATION_STRUCTURE_ALIGNMENT);
        let scratch = buffer(
            device,
            2 * scratch_stride,
            BufferUsage::SHADER_WRITE,
            MemoryLocation::GpuOnly,
        );
        let hits = cast(device, &s.tlas, |list| {
            list.buffer_barriers(&[
                BufferBarrier {
                    buffer: &scratch,
                    before: BufferState::NONE,
                    after: BufferState::BUILD_SCRATCH,
                },
                // Activates the placed storage; covers both structures.
                BufferBarrier {
                    buffer: &storage,
                    before: BufferState::NONE,
                    after: BufferState::ACCELERATION_STRUCTURE_WRITE,
                },
                BufferBarrier {
                    buffer: s.tlas.buffer(),
                    before: BufferState::NONE,
                    after: BufferState::ACCELERATION_STRUCTURE_WRITE,
                },
            ]);
            for (i, dst) in [&a, &b].into_iter().enumerate() {
                list.build_acceleration_structure(&BuildAccelerationStructure {
                    dst,
                    inputs: AccelerationStructureInputs::BottomLevel(&geometries),
                    flags: BuildFlags::PREFER_FAST_TRACE,
                    update: None,
                    scratch: &scratch,
                    scratch_offset: i as u64 * scratch_stride,
                });
            }
            // Both bottom levels, and the scratch the top-level build reuses.
            let built = BufferState {
                stage: Stage::BUILD_ACCELERATION_STRUCTURE,
                access: Access::ACCELERATION_STRUCTURE_WRITE.union(Access::UNORDERED_ACCESS),
            };
            let read = BufferState {
                stage: Stage::BUILD_ACCELERATION_STRUCTURE,
                access: Access::ACCELERATION_STRUCTURE_READ.union(Access::UNORDERED_ACCESS),
            };
            list.memory_barrier(built, read);
            list.build_acceleration_structure(&BuildAccelerationStructure {
                dst: &s.tlas,
                inputs: AccelerationStructureInputs::TopLevel {
                    instances: &s.instances,
                    offset: 0,
                    count: 2,
                },
                flags: BuildFlags::PREFER_FAST_TRACE,
                update: None,
                scratch: &scratch,
                scratch_offset: 0,
            });
            list.memory_barrier(
                BufferState::ACCELERATION_STRUCTURE_WRITE,
                BufferState {
                    stage: Stage::COMPUTE_SHADING,
                    access: Access::ACCELERATION_STRUCTURE_READ,
                },
            );
        });
        assert_eq!(hits, expected());
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn placement_and_memory_barriers_are_checked() {
    let device = support::hardware_device();
    support::require_feature!(device, ray_query);
    let desc = BufferDesc {
        size: 8192,
        usage: BufferUsage::ACCELERATION_STRUCTURE_STORAGE,
        memory: MemoryLocation::GpuOnly,
    };
    let storage = device.create_buffer(&desc).unwrap();
    let kind = AccelerationStructureKind::BottomLevel;
    let at = |buffer: &Buffer, offset| {
        device.create_acceleration_structure_in(buffer, offset, kind, 4096)
    };
    assert!(at(&storage, 4096).is_ok());
    assert!(at(&storage, 128).is_err(), "misaligned offset");
    assert!(at(&storage, 4352).is_err(), "past the end");
    let plain = buffer(
        device,
        8192,
        BufferUsage::SHADER_WRITE,
        MemoryLocation::GpuOnly,
    );
    assert!(at(&plain, 0).is_err(), "not storage");
    let mixed = BufferDesc {
        usage: BufferUsage::ACCELERATION_STRUCTURE_STORAGE | BufferUsage::COPY_SRC,
        ..desc
    };
    assert!(device.create_buffer(&mixed).is_err(), "storage only");
    let upload = BufferDesc {
        memory: MemoryLocation::Upload,
        ..desc
    };
    assert!(device.create_buffer(&upload).is_err(), "device-local only");
    assert!(
        device
            .create_buffer_view(
                &storage,
                &BufferViewDesc {
                    kind: BufferViewKind::Raw,
                    offset: 0,
                    size: 256,
                    writable: true,
                }
            )
            .is_err(),
        "no views of storage"
    );
    let copy_access = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(device, |list| {
            list.memory_barrier(BufferState::COPY_DST, BufferState::COPY_SRC)
        });
    }));
    assert!(copy_access.is_err(), "copies need buffer barriers");
    let copy_queue = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        support::run_on(device, QueueType::Copy, |list| {
            list.memory_barrier(BufferState::COMPUTE_WRITE, BufferState::COMPUTE_WRITE)
        });
    }));
    assert!(copy_queue.is_err(), "copy queues have no unordered access");
}

#[test]
fn builds_check_sizes_and_spans() {
    let device = support::hardware_device();
    support::require_feature!(device, ray_query);
    let s = scene(device);
    let geometries = [triangles(&s.vertices, &s.indices)];
    let sizes = device
        .acceleration_structure_sizes(
            &AccelerationStructureInputs::BottomLevel(&geometries),
            BLAS_FLAGS,
        )
        .unwrap();
    let build = |inputs, dst: &AccelerationStructure, scratch: &Buffer| {
        support::panic_message(|| {
            run(device, |list| {
                list.build_acceleration_structure(&BuildAccelerationStructure {
                    dst,
                    inputs,
                    flags: BLAS_FLAGS,
                    update: None,
                    scratch,
                    scratch_offset: 0,
                });
            });
        })
    };
    let blas = AccelerationStructureInputs::BottomLevel(&geometries);

    let short_scratch = buffer(
        device,
        sizes.build_scratch_size - 4,
        BufferUsage::SHADER_WRITE,
        MemoryLocation::GpuOnly,
    );
    assert_eq!(
        build(blas, &s.blas, &short_scratch),
        "scratch memory smaller than the build's scratch size"
    );

    assert!(sizes.size > ACCELERATION_STRUCTURE_ALIGNMENT, "{sizes:?}");
    let small = device
        .create_acceleration_structure(
            AccelerationStructureKind::BottomLevel,
            sizes.size - ACCELERATION_STRUCTURE_ALIGNMENT,
        )
        .unwrap();
    assert_eq!(
        build(blas, &small, &s.scratch),
        "destination smaller than the build's result"
    );

    // Spans of (count - 1) * stride past u64::MAX wrap to small ones where overflow checks are
    // off.
    let AccelerationStructureGeometry::Triangles(t) = triangles(&s.vertices, &s.indices) else {
        unreachable!()
    };
    let wide = [AccelerationStructureGeometry::Triangles(
        AccelerationStructureTriangles {
            vertex_stride: 1 << 63,
            ..t
        },
    )];
    assert_eq!(
        build(
            AccelerationStructureInputs::BottomLevel(&wide),
            &s.blas,
            &s.scratch
        ),
        "vertices past the end of the buffer"
    );
    let boxes = input(device, &[0; 64]);
    let aabbs = [AccelerationStructureGeometry::Aabbs(
        AccelerationStructureAabbs {
            buffer: &boxes,
            offset: 0,
            stride: 1 << 63,
            count: 3,
            flags: GeometryFlags::OPAQUE,
        },
    )];
    assert_eq!(
        build(
            AccelerationStructureInputs::BottomLevel(&aabbs),
            &s.blas,
            &s.scratch
        ),
        "AABBs past the end of the buffer"
    );
}
