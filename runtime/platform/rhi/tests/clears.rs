//! Buffer fills, storage texture clears and discards from a real layout, on the hardware
//! adapter through enhanced and legacy barriers, with validation on.
#![cfg(marlin_platform = "windows")]

mod support;

use marlin_rhi::{
    BufferBarrier, BufferState, BufferTextureCopy, BufferUsage, ClearColor, Format, MemoryLocation,
    QueueType, SubresourceRange, TextureBarrier, TextureDesc, TextureState, TextureUsage,
};
use support::{buffer, devices, read_words, readback, run, run_on};

#[test]
fn fills_buffer_ranges_on_graphics_and_compute_lists() {
    for device in devices() {
        for ty in [QueueType::Graphics, QueueType::Compute] {
            let b = buffer(
                device,
                1024,
                BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
                MemoryLocation::GpuOnly,
            );
            let out = readback(device, 1024);
            run_on(device, ty, |list| {
                list.buffer_barriers(&[BufferBarrier {
                    buffer: &b,
                    before: BufferState::NONE,
                    after: BufferState::CLEAR_STORAGE,
                }]);
                list.fill_buffer(&b, 0, 1024, 0x1111_1111);
                list.buffer_barriers(&[BufferBarrier {
                    buffer: &b,
                    before: BufferState::CLEAR_STORAGE,
                    after: BufferState::CLEAR_STORAGE,
                }]);
                // A second fill over the middle, ordered after the first.
                list.fill_buffer(&b, 256, 512, 0xdead_beef);
                list.buffer_barriers(&[BufferBarrier {
                    buffer: &b,
                    before: BufferState::CLEAR_STORAGE,
                    after: BufferState::COPY_SRC,
                }]);
                list.copy_buffer(&b, 0, &out, 0, 1024);
            });
            let words = read_words(&out, 0, 256);
            assert!(words[..64].iter().all(|&w| w == 0x1111_1111), "{ty:?}");
            assert!(words[64..192].iter().all(|&w| w == 0xdead_beef), "{ty:?}");
            assert!(words[192..].iter().all(|&w| w == 0x1111_1111), "{ty:?}");
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
#[should_panic(expected = "fills cover whole 32-bit words")]
fn fills_are_word_aligned() {
    let device = support::hardware_device();
    let b = buffer(
        device,
        64,
        BufferUsage::SHADER_WRITE,
        MemoryLocation::GpuOnly,
    );
    run(device, |list| list.fill_buffer(&b, 2, 8, 0));
}

#[test]
fn clears_storage_textures_by_layer() {
    for device in devices() {
        for (format, value, expect) in [
            (
                Format::Rgba8Unorm,
                ClearColor::Float([1.0, 0.0, 0.2, 1.0]),
                0xff33_00ff_u32,
            ),
            (Format::R32Uint, ClearColor::Uint([77, 0, 0, 0]), 77),
            (
                Format::R32Sint,
                ClearColor::Sint([-2, 0, 0, 0]),
                (-2i32) as u32,
            ),
        ] {
            let desc = TextureDesc {
                array_layers: 3,
                ..TextureDesc::new_2d(
                    16,
                    16,
                    format,
                    TextureUsage::STORAGE | TextureUsage::COPY_SRC,
                )
            };
            let texture = device.create_texture(&desc).unwrap();
            let out = readback(device, 3 * 16 * 256);
            run(device, |list| {
                list.texture_barriers(&[TextureBarrier::new(
                    &texture,
                    TextureState::UNDEFINED,
                    TextureState::CLEAR_STORAGE,
                )]);
                // Layers 1 and 2 only; layer 0 stays as cleared below.
                list.clear_storage_texture(&texture, &SubresourceRange::ALL, zero_of(value));
                list.texture_barriers(&[TextureBarrier::new(
                    &texture,
                    TextureState::CLEAR_STORAGE,
                    TextureState::CLEAR_STORAGE,
                )]);
                list.clear_storage_texture(
                    &texture,
                    &SubresourceRange {
                        base_mip: 0,
                        mip_count: 1,
                        base_layer: 1,
                        layer_count: 2,
                    },
                    value,
                );
                list.texture_barriers(&[TextureBarrier::new(
                    &texture,
                    TextureState::CLEAR_STORAGE,
                    TextureState::COPY_SRC,
                )]);
                for layer in 0..3 {
                    list.copy_texture_to_buffer(
                        &texture,
                        &out,
                        &BufferTextureCopy::whole_mip(&desc, 0, layer, layer as u64 * 16 * 256),
                    );
                }
            });
            let layer = |l: u64| read_words(&out, l * 16 * 256 + 5 * 256, 16);
            assert!(layer(0).iter().all(|&w| w == 0), "{format:?}");
            assert!(
                layer(1).iter().all(|&w| w == expect),
                "{format:?} {:x?}",
                layer(1)
            );
            assert!(layer(2).iter().all(|&w| w == expect), "{format:?}");
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

/// The zero of `value`'s representation, for a first clear to zero.
fn zero_of(value: ClearColor) -> ClearColor {
    match value {
        ClearColor::Float(_) => ClearColor::Float([0.0; 4]),
        ClearColor::Uint(_) => ClearColor::Uint([0; 4]),
        ClearColor::Sint(_) => ClearColor::Sint([0; 4]),
    }
}

#[test]
#[should_panic(expected = "clear integer formats with integers")]
fn clears_match_the_format_kind() {
    let device = support::hardware_device();
    let texture = device
        .create_texture(&TextureDesc::new_2d(
            4,
            4,
            Format::R32Uint,
            TextureUsage::STORAGE,
        ))
        .unwrap();
    run(device, |list| {
        list.clear_storage_texture(
            &texture,
            &SubresourceRange::ALL,
            ClearColor::Float([0.0; 4]),
        );
    });
}

/// A texture reused frame after frame, discarded each time from the layout the last frame left
/// it in: the caller (the render graph) names that layout, recording order does not.
#[test]
fn discards_from_the_named_layout() {
    for device in devices() {
        let desc = TextureDesc::new_2d(
            8,
            8,
            Format::R32Uint,
            TextureUsage::STORAGE | TextureUsage::COPY_SRC | TextureUsage::SAMPLED,
        );
        let texture = device.create_texture(&desc).unwrap();
        let out = readback(device, 8 * 256);
        // Two lists recorded out of execution order: the second records first.
        let mut pool_a = device.create_command_pool().unwrap();
        let mut pool_b = device.create_command_pool().unwrap();
        let mut a = device.create_command_list(&pool_a).unwrap();
        let mut b = device.create_command_list(&pool_b).unwrap();
        b.begin(&mut pool_b).unwrap();
        b.texture_barriers(&[TextureBarrier::discard(
            &texture,
            TextureState::SHADER_READ,
            TextureState::CLEAR_STORAGE,
        )]);
        b.clear_storage_texture(&texture, &SubresourceRange::ALL, ClearColor::Uint([9; 4]));
        b.texture_barriers(&[TextureBarrier::new(
            &texture,
            TextureState::CLEAR_STORAGE,
            TextureState::COPY_SRC,
        )]);
        b.copy_texture_to_buffer(
            &texture,
            &out,
            &BufferTextureCopy::whole_mip(&desc, 0, 0, 0),
        );
        b.end().unwrap();
        a.begin(&mut pool_a).unwrap();
        a.texture_barriers(&[TextureBarrier::new(
            &texture,
            TextureState::UNDEFINED,
            TextureState::CLEAR_STORAGE,
        )]);
        a.clear_storage_texture(&texture, &SubresourceRange::ALL, ClearColor::Uint([1; 4]));
        a.texture_barriers(&[TextureBarrier::new(
            &texture,
            TextureState::CLEAR_STORAGE,
            TextureState::SHADER_READ,
        )]);
        a.end().unwrap();
        let fence = device.create_fence(0).unwrap();
        // SAFETY: everything outlives the wait below.
        unsafe { device.queue().submit(&[&a, &b], Some((&fence, 1))).unwrap() };
        fence.wait(1, None).unwrap();
        assert!(read_words(&out, 3 * 256, 8).iter().all(|&w| w == 9));
        assert_eq!(device.validation_errors(), 0);
    }
}
