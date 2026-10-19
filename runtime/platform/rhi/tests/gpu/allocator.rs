//! Committed resources suballocated by the memory allocator (docs/design/0017-rhi-memory.md):
//! neighbors in a block keep their contents apart. The driver allocation counts are in
//! `isolated_gpu_memory.rs`, away from other tests' resources.

use marlin_gpu_test_support as support;

use marlin_rhi::{BufferBarrier, BufferState, BufferUsage, MemoryLocation};
use support::{buffer, read_bytes, readback, run, upload};

#[test]
fn suballocated_buffers_keep_their_contents_apart() {
    for device in support::devices() {
        // Odd sizes, so neighbors sit at unaligned ends.
        let sizes: Vec<u64> = (0..64).map(|i| 4 + i * 52 + (i % 7) * 4).collect();
        let sources: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| {
                let bytes: Vec<u8> = (0..size).map(|b| (i as u64 * 31 + b) as u8).collect();
                upload(device, &bytes, BufferUsage::COPY_SRC)
            })
            .collect();
        let gpu: Vec<_> = sizes
            .iter()
            .map(|&size| {
                buffer(
                    device,
                    size,
                    BufferUsage::COPY_DST | BufferUsage::COPY_SRC,
                    MemoryLocation::GpuOnly,
                )
            })
            .collect();
        let back: Vec<_> = sizes.iter().map(|&size| readback(device, size)).collect();
        run(device, |list| {
            for i in 0..sizes.len() {
                list.copy_buffer(&sources[i], 0, &gpu[i], 0, sizes[i]);
            }
            let barriers: Vec<_> = gpu
                .iter()
                .map(|buffer| BufferBarrier {
                    buffer,
                    before: BufferState::COPY_DST,
                    after: BufferState::COPY_SRC,
                })
                .collect();
            list.buffer_barriers(&barriers);
            for i in 0..sizes.len() {
                list.copy_buffer(&gpu[i], 0, &back[i], 0, sizes[i]);
            }
        });
        for (i, &size) in sizes.iter().enumerate() {
            let expected: Vec<u8> = (0..size).map(|b| (i as u64 * 31 + b) as u8).collect();
            assert_eq!(
                read_bytes(&back[i], 0, size as usize),
                expected,
                "buffer {i}"
            );
        }
    }
}
