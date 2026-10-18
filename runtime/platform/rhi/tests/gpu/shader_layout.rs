//! Structs nested in laid-out memory, through the descriptor heap's buffer pointers: every
//! member read leaf by leaf, piece by piece and whole, from a buffer and a uniform, and written
//! back (`Test.NestedLayout`). NVIDIA's driver reads struct-typed members of such memory at
//! offset 0, so the SPIR-V backend flattens them (docs/design/0016-rhi-vulkan.md §4.2).

use marlin_gpu_test_support as support;

use marlin_rhi::{BufferUsage, BufferViewDesc, BufferViewKind, MemoryLocation};
use support::{buffer, compute_pipeline, copy_out, devices, read_words, readback, run, storage};

/// `Test.Deep`'s size in words.
const WORDS: u64 = 40;

/// Whether word `i` of a `Test.Deep` is a float (a `Pair.B`): the shader reads it as one.
fn is_float(i: u32) -> bool {
    matches!(i, 8..12 | 20..24 | 28..32 | 36..40)
}

/// A `Test.Deep` whose word `i` holds `base + i`, as a float where it is one.
fn deep(base: u32) -> Vec<u32> {
    (0..WORDS as u32)
        .map(|i| match is_float(i) {
            true => ((base + i) as f32).to_bits(),
            false => base + i,
        })
        .collect()
}

#[test]
fn nested_structs_in_buffers_and_uniforms() {
    for device in devices() {
        // The buffer's element 0 at 0 and element 1 (the one read) after it, the uniform at 512.
        let input = buffer(
            device,
            1024,
            BufferUsage::SHADER_READ | BufferUsage::UNIFORM,
            MemoryLocation::Upload,
        );
        let words = |base: u32| (0..WORDS as u32).map(|i| base + i).collect::<Vec<_>>();
        // SAFETY: no GPU work exists yet.
        unsafe {
            input.write(0, &support::words_to_bytes(&deep(900)));
            input.write(WORDS * 4, &support::words_to_bytes(&deep(100)));
            input.write(512, &support::words_to_bytes(&deep(200)));
        }
        let elements = device
            .create_buffer_view(
                &input,
                &BufferViewDesc {
                    kind: BufferViewKind::Structured {
                        stride: WORDS as u32 * 4,
                    },
                    writable: false,
                    offset: 0,
                    size: 2 * WORDS * 4,
                },
            )
            .unwrap();
        let uniform = device
            .create_buffer_view(
                &input,
                &BufferViewDesc {
                    kind: BufferViewKind::Uniform,
                    writable: false,
                    offset: 512,
                    size: WORDS * 4,
                },
            )
            .unwrap();
        let (out, out_view) = storage(device, 8 * WORDS);
        let (rw, rw_view) = storage(device, 3 * WORDS);
        let out_back = readback(device, 8 * WORDS * 4);
        let rw_back = readback(device, 3 * WORDS * 4);

        let pipeline = compute_pipeline(device, "NestedLayout");
        run(device, |list| {
            list.set_compute_pipeline(&pipeline);
            list.push_constants(
                0,
                &[
                    elements.index(),
                    uniform.index(),
                    out_view.index(),
                    rw_view.index(),
                    1,
                ],
            );
            list.dispatch(1, 1, 1);
            copy_out(list, &out, &out_back, 8 * WORDS);
            copy_out(list, &rw, &rw_back, 3 * WORDS);
        });

        let out = read_words(&out_back, 0, 8 * WORDS as usize);
        let runs = [
            "buffer leaves",
            "uniform leaves",
            "whole buffer element",
            "whole uniform",
            "buffer members whole",
            "uniform members whole",
            "nested locals",
            "written element read back",
        ];
        for (k, name) in runs.iter().enumerate() {
            let base = if k == 1 || k == 3 || k == 5 { 200 } else { 100 };
            let got = &out[k * WORDS as usize..][..WORDS as usize];
            assert_eq!(got, words(base), "run {k}: {name}");
        }
        let rw = read_words(&rw_back, 0, 3 * WORDS as usize);
        for (k, name) in ["whole", "by member", "by leaf"].iter().enumerate() {
            let got = &rw[k * WORDS as usize..][..WORDS as usize];
            assert_eq!(got, deep(100), "store {k}: {name}");
        }
        assert_eq!(device.validation_errors(), 0);
    }
}
