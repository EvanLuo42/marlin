//! Deliberate API misuse, in its own test binary with a single test: validation messages are not
//! tied to one device (D3D12 hands out one device per adapter per process; a Vulkan debug
//! messenger belongs to the instance), so they would otherwise reach other tests' devices.
#![cfg(marlin_platform = "windows")]

use marlin_rhi::{
    AdapterPreference, DeviceDesc, ErrorKind, Format, GRAPHICS_API, GraphicsApi, Instance,
    InstanceDesc, TextureBarrier, TextureDesc, TextureState, TextureUsage,
};

#[test]
fn misuse_is_refused_or_reported() {
    let instance = Instance::new(&InstanceDesc {
        validation: true,
        ..InstanceDesc::default()
    })
    .unwrap();
    let device = instance
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::HighPerformance,
            ..DeviceDesc::default()
        })
        .unwrap();
    let mut pool = device.create_command_pool().unwrap();
    let mut list = device.create_command_list(&pool).unwrap();

    // Lists start closed: ending one is refused before the API sees it.
    assert_eq!(list.end().unwrap_err().kind(), ErrorKind::InvalidInput);

    // A barrier from the wrong layout, which only the validation layer can see: a new texture is
    // in `Layout::Common`, not the copy source layout. Vulkan's validation layer tracks layouts
    // across submissions and reports it at submit.
    if GRAPHICS_API != GraphicsApi::Vulkan {
        return;
    }
    let texture = device
        .create_texture(&TextureDesc::new_2d(
            4,
            4,
            Format::Rgba8Unorm,
            TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
        ))
        .unwrap();
    list.begin(&mut pool).unwrap();
    list.texture_barriers(&[TextureBarrier::new(
        &texture,
        TextureState::COPY_SRC,
        TextureState::COPY_DST,
    )]);
    list.end().unwrap();
    let fence = device.create_fence(0).unwrap();
    // SAFETY: everything the list uses outlives the wait below.
    unsafe { device.queue().submit(&[&list], Some((&fence, 1))).unwrap() };
    fence.wait(1, None).unwrap();
    assert!(device.validation_errors() > 0);
}
