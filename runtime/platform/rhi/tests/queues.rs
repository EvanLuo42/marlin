//! Queues on the hardware adapter, through both barrier paths: dedicated compute and copy
//! queues, GPU-side waits across queues and on the CPU's signals, typed command lists,
//! timestamps on every queue, and debug markers.
#![cfg(marlin_platform = "windows")]

mod support;

use std::time::Duration;

use marlin_rhi::{
    BufferBarrier, BufferState, BufferUsage, BufferViewDesc, BufferViewKind, CommandList,
    ComputePipelineDesc, Device, ErrorKind, MemoryLocation, QueryKind, QueryPoolDesc, QueueType,
    SubmitDesc,
};
use support::{
    Stage, buffer, devices, read_words, readback, run_on, shader, upload, words_to_bytes,
};

/// A recorded list of type `ty`, with its pool.
fn record(
    device: &Device,
    ty: QueueType,
    f: impl FnOnce(&mut CommandList),
) -> (marlin_rhi::CommandPool, CommandList) {
    let mut pool = device.create_command_pool_for(ty).unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    list.begin(&mut pool).unwrap();
    f(&mut list);
    list.end().unwrap();
    (pool, list)
}

#[test]
fn d3d12_has_dedicated_compute_and_copy_queues() {
    let device = support::hardware_device();
    for ty in [QueueType::Graphics, QueueType::Compute, QueueType::Copy] {
        let queue = device.dedicated_queue(ty).unwrap();
        assert_eq!(queue.queue_type(), ty);
        assert_eq!(device.queue_for(ty).queue_type(), ty);
        assert_eq!(device.create_command_pool_for(ty).unwrap().queue_type(), ty);
    }
}

#[test]
fn queues_wait_for_each_other_on_the_gpu() {
    for device in devices() {
        const COUNT: u32 = 128;
        let data: Vec<u32> = (0..COUNT).map(|i| i * 7 + 1).collect();
        let staging = upload(&device, &words_to_bytes(&data), BufferUsage::default());
        let copied = buffer(
            &device,
            COUNT as u64 * 4,
            BufferUsage::COPY_DST | BufferUsage::COPY_SRC,
            MemoryLocation::GpuOnly,
        );
        let filled = buffer(
            &device,
            COUNT as u64 * 4,
            BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
            MemoryLocation::GpuOnly,
        );
        let moved = buffer(
            &device,
            COUNT as u64 * 4,
            BufferUsage::COPY_DST | BufferUsage::COPY_SRC,
            MemoryLocation::GpuOnly,
        );
        let out = readback(&device, COUNT as u64 * 8);
        let view = device
            .create_buffer_view(&filled, &BufferViewDesc::whole(BufferViewKind::Raw, true))
            .unwrap();
        let fill = device
            .create_compute_pipeline(&ComputePipelineDesc {
                shader: shader("fill", Stage::Compute),
            })
            .unwrap();

        // Copy queue: upload. Compute queue: fill, and move the upload along. Graphics queue:
        // read both back, but only once the CPU says so.
        let (_copy_pool, copy_list) = record(&device, QueueType::Copy, |list| {
            list.copy_buffer(&staging, 0, &copied, 0, COUNT as u64 * 4);
        });
        let (_compute_pool, compute_list) = record(&device, QueueType::Compute, |list| {
            list.set_compute_pipeline(&fill);
            list.push_constants(0, &[view.index(), 5]);
            list.dispatch(COUNT / 64, 1, 1);
            list.copy_buffer(&copied, 0, &moved, 0, COUNT as u64 * 4);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &filled,
                before: BufferState::COMPUTE_WRITE,
                after: BufferState::COPY_SRC,
            }]);
        });
        let (_graphics_pool, graphics_list) = record(&device, QueueType::Graphics, |list| {
            list.copy_buffer(&filled, 0, &out, 0, COUNT as u64 * 4);
            list.copy_buffer(&moved, 0, &out, COUNT as u64 * 4, COUNT as u64 * 4);
        });

        let uploaded = device.create_fence(0).unwrap();
        let computed = device.create_fence(0).unwrap();
        let released = device.create_fence(0).unwrap();
        let done = device.create_fence(0).unwrap();
        // SAFETY: everything the lists use outlives the waits below. Submitted in reverse order:
        // only the GPU-side waits order the work.
        unsafe {
            device
                .queue()
                .submit_with(&SubmitDesc {
                    waits: &[(&computed, 1), (&released, 1)],
                    lists: &[&graphics_list],
                    signals: &[(&done, 1)],
                })
                .unwrap();
            device
                .queue_for(QueueType::Compute)
                .submit_with(&SubmitDesc {
                    waits: &[(&uploaded, 1)],
                    lists: &[&compute_list],
                    signals: &[(&computed, 1)],
                })
                .unwrap();
            device
                .queue_for(QueueType::Copy)
                .submit_with(&SubmitDesc {
                    waits: &[],
                    lists: &[&copy_list],
                    signals: &[(&uploaded, 1)],
                })
                .unwrap();
        }
        assert!(computed.wait(1, Some(Duration::from_secs(10))).unwrap());
        // The graphics queue is still held by the CPU's fence.
        assert!(!done.wait(2, Some(Duration::from_millis(50))).unwrap());
        assert_eq!(done.completed_value(), 0);
        released.signal(1).unwrap();
        assert!(done.wait(1, Some(Duration::from_secs(10))).unwrap());

        let words = read_words(&out, 0, COUNT as usize * 2);
        for i in 0..COUNT as usize {
            assert_eq!(words[i], 5 + 3 * i as u32, "filled word {i}");
            assert_eq!(words[COUNT as usize + i], data[i], "moved word {i}");
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn lists_run_only_on_their_queue_type() {
    let device = support::hardware_device();
    let (_pool, list) = record(&device, QueueType::Compute, |_| {});
    // SAFETY: rejected before anything is submitted.
    let err = unsafe { device.queue().submit(&[&list], None) }.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
    // A pool of another type cannot back the list.
    let mut graphics_pool = device.create_command_pool().unwrap();
    let mut compute_list = device
        .create_command_list(&device.create_command_pool_for(QueueType::Compute).unwrap())
        .unwrap();
    assert_eq!(
        compute_list.begin(&mut graphics_pool).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
#[should_panic(expected = "a dispatch needs a graphics or compute list, not a copy list")]
fn copy_lists_do_not_dispatch() {
    let device = support::hardware_device();
    let mut pool = device.create_command_pool_for(QueueType::Copy).unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    list.begin(&mut pool).unwrap();
    list.dispatch(1, 1, 1);
}

#[test]
#[should_panic(expected = "a draw needs a graphics list, not Compute")]
fn compute_lists_do_not_draw() {
    let device = support::hardware_device();
    let mut pool = device.create_command_pool_for(QueueType::Compute).unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    list.begin(&mut pool).unwrap();
    list.draw(3, 1, 0, 0);
}

#[test]
fn timestamps_on_every_queue() {
    for device in devices() {
        let fill = device
            .create_compute_pipeline(&ComputePipelineDesc {
                shader: shader("fill", Stage::Compute),
            })
            .unwrap();
        let scratch = buffer(
            &device,
            1 << 20,
            BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        let scratch2 = buffer(
            &device,
            1 << 20,
            BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        let view = device
            .create_buffer_view(&scratch, &BufferViewDesc::whole(BufferViewKind::Raw, true))
            .unwrap();
        for ty in [QueueType::Graphics, QueueType::Compute, QueueType::Copy] {
            let kind = match ty {
                QueueType::Copy => QueryKind::CopyTimestamp,
                _ => QueryKind::Timestamp,
            };
            let desc = QueryPoolDesc { kind, count: 4 };
            if ty == QueueType::Copy && !device.features().copy_queue_timestamps {
                let err = device.create_query_pool(&desc).unwrap_err();
                assert_eq!(err.kind(), ErrorKind::Unsupported);
                continue;
            }
            let pool = device.create_query_pool(&desc).unwrap();
            assert_eq!(pool.desc(), &desc);
            let out = readback(&device, 64);
            run_on(&device, ty, |list| {
                list.write_timestamp(&pool, 1);
                if ty == QueueType::Copy {
                    list.copy_buffer(&scratch, 0, &scratch2, 0, 1 << 20);
                } else {
                    list.set_compute_pipeline(&fill);
                    list.push_constants(0, &[view.index(), 0]);
                    list.dispatch((1 << 18) / 64, 1, 1);
                }
                list.write_timestamp(&pool, 2);
                // Into the middle of the buffer: 8-aligned offsets.
                list.resolve_queries(&pool, 1, 2, &out, 16);
            });
            let frequency = device.queue_for(ty).timestamp_frequency().unwrap();
            assert!(frequency > 0, "{ty:?}");
            let words = read_words(&out, 16, 4);
            let t0 = words[0] as u64 | (words[1] as u64) << 32;
            let t1 = words[2] as u64 | (words[3] as u64) << 32;
            assert!(t0 > 0 && t1 >= t0, "{ty:?}: {t0} {t1}");
            // The work took under a second.
            assert!(
                t1 - t0 < frequency,
                "{ty:?}: {} ticks at {frequency} Hz",
                t1 - t0
            );
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
#[should_panic(expected = "copy lists write CopyTimestamp queries, other lists Timestamp queries")]
fn timestamp_kind_matches_the_list() {
    let device = support::hardware_device();
    let pool = device
        .create_query_pool(&QueryPoolDesc {
            kind: QueryKind::Timestamp,
            count: 1,
        })
        .unwrap();
    let mut command_pool = device.create_command_pool_for(QueueType::Copy).unwrap();
    let mut list = device.create_command_list(&command_pool).unwrap();
    list.begin(&mut command_pool).unwrap();
    list.write_timestamp(&pool, 0);
}

#[test]
fn query_pools_hold_1_to_65536_queries() {
    let device = support::hardware_device();
    for count in [0, 65537] {
        let err = device
            .create_query_pool(&QueryPoolDesc {
                kind: QueryKind::Timestamp,
                count,
            })
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }
}

#[test]
fn debug_markers_on_every_list_type() {
    for device in devices() {
        for ty in [QueueType::Graphics, QueueType::Compute, QueueType::Copy] {
            run_on(&device, ty, |list| {
                list.begin_event("Frame");
                list.begin_event("Nested pass with a long name that PIX shows in its timeline");
                list.set_marker("halfway");
                list.end_event();
                list.set_marker("ünïcödé ✓");
                list.end_event();
            });
        }
        assert_eq!(device.validation_errors(), 0);
    }
}
