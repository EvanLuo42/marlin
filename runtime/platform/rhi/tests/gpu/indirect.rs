//! Indirect draws and dispatches on the hardware adapter, through each barrier path: arguments
//! (and counts) written to GPU buffers, each record's constant landing in push-constant word
//! `INDIRECT_CONSTANT_WORD`.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    Buffer, BufferBarrier, BufferState, BufferUsage, BufferViewDesc, BufferViewKind,
    ComputePipelineDesc, Device, DispatchIndirectArgs, DrawIndexedIndirectArgs, DrawIndirectArgs,
    INDIRECT_CONSTANT_WORD, IndexFormat, IndirectArgs, MAX_PUSH_CONSTANTS, MemoryLocation,
};
use support::{
    COLUMNS, ColumnTarget, buffer, bytes_of, columns_pipeline, devices, read_words, readback, run,
    shader, upload, words_to_bytes,
};

#[test]
fn indirect_constant_is_the_last_push_constant() {
    assert_eq!(INDIRECT_CONSTANT_WORD, MAX_PUSH_CONSTANTS - 1);
    assert_eq!(size_of::<DrawIndirectArgs>(), 20);
    assert_eq!(size_of::<DrawIndexedIndirectArgs>(), 24);
    assert_eq!(size_of::<DispatchIndirectArgs>(), 16);
}

/// A GPU buffer with `bytes`, copied in by `list` and left ready for indirect reads.
struct GpuBuffer {
    staging: Buffer,
    buffer: Buffer,
}

impl GpuBuffer {
    fn new(device: &Device, bytes: &[u8], usage: BufferUsage) -> Self {
        let staging = upload(device, bytes, BufferUsage::default());
        let buffer = buffer(
            device,
            bytes.len() as u64,
            usage | BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        Self { staging, buffer }
    }

    fn copy(&self, list: &mut marlin_rhi::CommandList, after: BufferState) {
        list.copy_buffer(&self.staging, 0, &self.buffer, 0, self.buffer.desc().size);
        list.buffer_barriers(&[BufferBarrier {
            buffer: &self.buffer,
            before: BufferState::COPY_DST,
            after,
        }]);
    }
}

/// Column `c` holds `c + 1` where a draw covered it, else 0.
fn expect_columns(target: &ColumnTarget, drawn: &[u32]) {
    let columns = target.columns();
    for c in 0..COLUMNS {
        let expected = if drawn.contains(&c) { c + 1 } else { 0 };
        assert_eq!(columns[c as usize], expected, "column {c}: {columns:?}");
    }
}

#[test]
fn draws_indirect_with_and_without_a_count() {
    for device in devices() {
        let pipeline = columns_pipeline(device, None);
        let target = ColumnTarget::new(device);
        // Records for columns 1, 3, 5, 7 and 9; the count buffer runs the first 3.
        let records: Vec<DrawIndirectArgs> = [1, 3, 5, 7, 9]
            .map(|c| DrawIndirectArgs {
                constant: c,
                vertex_count: 6,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            })
            .to_vec();
        let args = GpuBuffer::new(device, &bytes_of(&records), BufferUsage::INDIRECT);
        let count = GpuBuffer::new(device, &words_to_bytes(&[3]), BufferUsage::INDIRECT);
        // Column 12 alone, from the record at byte offset 20 * 4 + 4 (a 4-aligned offset).
        let mut shifted = vec![0u8; 4];
        shifted.extend(bytes_of(&[DrawIndirectArgs {
            constant: 12,
            vertex_count: 6,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
        }]));
        let offset_args = GpuBuffer::new(device, &shifted, BufferUsage::INDIRECT);

        for with_count in [false, true] {
            run(device, |list| {
                args.copy(list, BufferState::INDIRECT);
                count.copy(list, BufferState::INDIRECT);
                offset_args.copy(list, BufferState::INDIRECT);
                target.begin(list);
                list.set_graphics_pipeline(&pipeline);
                list.push_constants(0, &[COLUMNS, 1, 0]);
                list.draw_indirect(&IndirectArgs {
                    buffer: &args.buffer,
                    offset: 0,
                    max_count: records.len() as u32,
                    count: with_count.then_some((&count.buffer, 0)),
                    constant: true,
                });
                list.draw_indirect(&IndirectArgs {
                    buffer: &offset_args.buffer,
                    offset: 4,
                    max_count: 1,
                    count: None,
                    constant: true,
                });
                target.finish(list);
            });
            match with_count {
                false => expect_columns(&target, &[1, 3, 5, 7, 9, 12]),
                true => expect_columns(&target, &[1, 3, 5, 12]),
            }
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn draws_indexed_indirect() {
    for device in devices() {
        let pipeline = columns_pipeline(device, None);
        let target = ColumnTarget::new(device);
        // Six indices (0..6) after two unused ones; vertex offsets shift the vertex id by 6 (the
        // shader takes it modulo 6, so the quad is the same).
        let indices = GpuBuffer::new(
            device,
            &words_to_bytes(&[99, 99, 0, 1, 2, 3, 4, 5]),
            BufferUsage::INDEX,
        );
        let records = [
            DrawIndexedIndirectArgs {
                constant: 2,
                index_count: 6,
                instance_count: 1,
                first_index: 2,
                vertex_offset: 0,
                first_instance: 0,
            },
            DrawIndexedIndirectArgs {
                constant: 14,
                index_count: 6,
                instance_count: 1,
                first_index: 2,
                vertex_offset: 6,
                first_instance: 0,
            },
            // No instances: draws nothing.
            DrawIndexedIndirectArgs {
                constant: 8,
                index_count: 6,
                instance_count: 0,
                first_index: 2,
                vertex_offset: 0,
                first_instance: 0,
            },
            // Past the count: not run.
            DrawIndexedIndirectArgs {
                constant: 5,
                index_count: 6,
                instance_count: 1,
                first_index: 2,
                vertex_offset: 0,
                first_instance: 0,
            },
        ];
        let args = GpuBuffer::new(device, &bytes_of(&records), BufferUsage::INDIRECT);
        let count = GpuBuffer::new(device, &words_to_bytes(&[0, 3]), BufferUsage::INDIRECT);

        run(device, |list| {
            indices.copy(list, BufferState::INDEX);
            args.copy(list, BufferState::INDIRECT);
            count.copy(list, BufferState::INDIRECT);
            target.begin(list);
            list.set_graphics_pipeline(&pipeline);
            list.push_constants(0, &[COLUMNS, 1, 0]);
            list.set_index_buffer(&indices.buffer, 0, IndexFormat::U32);
            // The count (3, at a 4-aligned offset) runs fewer records than `max_count`. A count
            // above `max_count` would be undefined (`IndirectArgs`).
            list.draw_indexed_indirect(&IndirectArgs {
                buffer: &args.buffer,
                offset: 0,
                max_count: 4,
                count: Some((&count.buffer, 4)),
                constant: true,
            });
            target.finish(list);
        });
        expect_columns(&target, &[2, 14]);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn dispatches_indirect_on_graphics_and_compute_lists() {
    for device in devices() {
        let pipeline = device
            .create_compute_pipeline(&ComputePipelineDesc {
                shader: shader("Count"),
                cache: None,
            })
            .unwrap();
        // Dispatch c counts 64 * x * y * z threads into word c.
        let records = [
            DispatchIndirectArgs {
                constant: 0,
                x: 1,
                y: 1,
                z: 1,
            },
            DispatchIndirectArgs {
                constant: 1,
                x: 2,
                y: 3,
                z: 1,
            },
            DispatchIndirectArgs {
                constant: 2,
                x: 0,
                y: 1,
                z: 1,
            },
            DispatchIndirectArgs {
                constant: 3,
                x: 1,
                y: 2,
                z: 2,
            },
        ];
        let args = GpuBuffer::new(device, &bytes_of(&records), BufferUsage::INDIRECT);
        let count = GpuBuffer::new(device, &words_to_bytes(&[2]), BufferUsage::INDIRECT);

        for ty in [
            marlin_rhi::QueueType::Graphics,
            marlin_rhi::QueueType::Compute,
        ] {
            let counters = GpuBuffer::new(
                device,
                &[0; 16],
                BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
            );
            let view = device
                .create_buffer_view(
                    &counters.buffer,
                    &BufferViewDesc::whole(BufferViewKind::Raw, true),
                )
                .unwrap();
            let out = readback(device, 32);
            support::run_on(device, ty, |list| {
                args.copy(list, BufferState::INDIRECT);
                count.copy(list, BufferState::INDIRECT);
                counters.copy(list, BufferState::COMPUTE_WRITE);
                list.set_compute_pipeline(&pipeline);
                list.push_constants(0, &[view.index()]);
                list.dispatch_indirect(&IndirectArgs {
                    buffer: &args.buffer,
                    offset: 0,
                    max_count: 4,
                    count: None,
                    constant: true,
                });
                list.buffer_barriers(&[BufferBarrier {
                    buffer: &counters.buffer,
                    before: BufferState::COMPUTE_WRITE,
                    after: BufferState::COMPUTE_WRITE,
                }]);
                // Again, counted: only the first two records.
                list.dispatch_indirect(&IndirectArgs {
                    buffer: &args.buffer,
                    offset: 0,
                    max_count: 4,
                    count: Some((&count.buffer, 0)),
                    constant: true,
                });
                list.buffer_barriers(&[BufferBarrier {
                    buffer: &counters.buffer,
                    before: BufferState::COMPUTE_WRITE,
                    after: BufferState::COPY_SRC,
                }]);
                list.copy_buffer(&counters.buffer, 0, &out, 0, 16);
            });
            assert_eq!(
                read_words(&out, 0, 4),
                [64 * 2, 64 * 6 * 2, 0, 64 * 4],
                "{ty:?}"
            );
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
#[should_panic(expected = "argument buffer without INDIRECT usage")]
fn indirect_arguments_need_indirect_usage() {
    let device = support::hardware_device();
    let args = buffer(
        device,
        64,
        BufferUsage::SHADER_READ,
        MemoryLocation::GpuOnly,
    );
    let mut pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    list.begin(&mut pool).unwrap();
    list.dispatch_indirect(&IndirectArgs {
        buffer: &args,
        offset: 0,
        max_count: 1,
        count: None,
        constant: true,
    });
}

#[test]
#[should_panic(expected = "indirect arguments past the end of the buffer")]
fn indirect_arguments_stay_in_the_buffer() {
    let device = support::hardware_device();
    let args = buffer(device, 32, BufferUsage::INDIRECT, MemoryLocation::GpuOnly);
    let mut pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    list.begin(&mut pool).unwrap();
    // Two 16-byte records from offset 4 end at 36.
    list.dispatch_indirect(&IndirectArgs {
        buffer: &args,
        offset: 4,
        max_count: 2,
        count: None,
        constant: true,
    });
}

#[test]
#[should_panic(expected = "count out of range")]
fn indirect_counts_do_not_wrap() {
    let device = support::hardware_device();
    let args = buffer(device, 64, BufferUsage::INDIRECT, MemoryLocation::GpuOnly);
    let mut pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    list.begin(&mut pool).unwrap();
    // The count's end, offset + 4, wraps to 0 where overflow checks are off.
    list.dispatch_indirect(&IndirectArgs {
        buffer: &args,
        offset: 0,
        max_count: 1,
        count: Some((&args, u64::MAX - 3)),
        constant: true,
    });
}

#[test]
fn indirect_commands_without_the_constant_run_natively() {
    for device in devices() {
        // Draws: every record draws the column pushed in the constant word.
        let pipeline = columns_pipeline(device, None);
        let target = ColumnTarget::new(device);
        let records: Vec<DrawIndirectArgs> = [1, 3, 5]
            .map(|c| DrawIndirectArgs {
                constant: c,
                vertex_count: 6,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            })
            .to_vec();
        let args = GpuBuffer::new(device, &bytes_of(&records), BufferUsage::INDIRECT);
        let count = GpuBuffer::new(device, &words_to_bytes(&[0]), BufferUsage::INDIRECT);
        for with_count in [false, true] {
            run(device, |list| {
                args.copy(list, BufferState::INDIRECT);
                count.copy(list, BufferState::INDIRECT);
                target.begin(list);
                list.set_graphics_pipeline(&pipeline);
                list.push_constants(0, &[COLUMNS, 1, 0]);
                list.push_constants(INDIRECT_CONSTANT_WORD, &[6]);
                list.draw_indirect(&IndirectArgs {
                    buffer: &args.buffer,
                    offset: 0,
                    max_count: records.len() as u32,
                    count: with_count.then_some((&count.buffer, 0)),
                    constant: false,
                });
                target.finish(list);
            });
            // A count of 0 draws nothing.
            expect_columns(&target, if with_count { &[] } else { &[6] });
        }

        // Dispatches: each record runs, counting into the word pushed in the constant word.
        let pipeline = device
            .create_compute_pipeline(&ComputePipelineDesc {
                shader: shader("Count"),
                cache: None,
            })
            .unwrap();
        let records =
            [(1, 1, 1), (2, 3, 1), (0, 1, 1), (1, 2, 2)].map(|(x, y, z)| DispatchIndirectArgs {
                constant: 3,
                x,
                y,
                z,
            });
        let args = GpuBuffer::new(device, &bytes_of(&records), BufferUsage::INDIRECT);
        let counters = GpuBuffer::new(
            device,
            &[0; 16],
            BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
        );
        let view = device
            .create_buffer_view(
                &counters.buffer,
                &BufferViewDesc::whole(BufferViewKind::Raw, true),
            )
            .unwrap();
        let out = readback(device, 16);
        run(device, |list| {
            args.copy(list, BufferState::INDIRECT);
            counters.copy(list, BufferState::COMPUTE_WRITE);
            list.set_compute_pipeline(&pipeline);
            list.push_constants(0, &[view.index()]);
            list.push_constants(INDIRECT_CONSTANT_WORD, &[1]);
            list.dispatch_indirect(&IndirectArgs {
                buffer: &args.buffer,
                offset: 0,
                max_count: 4,
                count: None,
                constant: false,
            });
            list.buffer_barriers(&[BufferBarrier {
                buffer: &counters.buffer,
                before: BufferState::COMPUTE_WRITE,
                after: BufferState::COPY_SRC,
            }]);
            list.copy_buffer(&counters.buffer, 0, &out, 0, 16);
        });
        assert_eq!(read_words(&out, 0, 4), [0, 64 * (1 + 6 + 4), 0, 0]);
        assert_eq!(device.validation_errors(), 0);
    }
}
