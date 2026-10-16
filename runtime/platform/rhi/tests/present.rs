//! Clear-and-present on a real (hidden) window, with validation on, through both barrier paths.
//! Ignored by default because it needs a desktop session:
//! `cargo test -p marlin-rhi --test present -- --ignored`.
#![cfg(marlin_platform = "windows")]

use std::time::Duration;

use marlin_app::{App, Surface, WindowDesc};
use marlin_rhi::{
    AdapterPreference, ColorAttachment, DeviceDesc, FramesInFlight, Instance, InstanceDesc, LoadOp,
    RenderPassDesc, StoreOp, SurfaceHandle, SwapchainDesc, TextureBarrier, TextureState,
};

fn clear_and_present(disable_enhanced_barriers: bool) {
    let app = App::new().unwrap();
    let window = app
        .create_window(&WindowDesc {
            title: "marlin-rhi present test",
            width: 320,
            height: 240,
            visible: false,
            ..WindowDesc::default()
        })
        .unwrap();
    let Surface::Win32 { hwnd, hinstance } = window.surface() else {
        unreachable!("Win32 window")
    };

    let instance = Instance::new(&InstanceDesc { validation: true }).unwrap();
    let device = instance
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::HighPerformance,
            disable_enhanced_barriers,
        })
        .unwrap();
    let size = window.size();
    // SAFETY: the window outlives the swapchain (dropped first, below).
    let mut swapchain = unsafe {
        device.create_swapchain(
            SurfaceHandle::Win32 { hwnd, hinstance },
            &SwapchainDesc {
                width: size.width,
                height: size.height,
                ..SwapchainDesc::default()
            },
        )
    }
    .unwrap();
    assert_eq!(swapchain.image_count(), 3);
    let mut frames = FramesInFlight::new(&device, 2).unwrap();

    for frame in 0..8 {
        if frame == 4 {
            frames.wait_idle().unwrap();
            swapchain.resize(200, 100).unwrap();
            assert_eq!(swapchain.size(), (200, 100));
        }
        let image = swapchain.acquire(Some(Duration::from_secs(10))).unwrap();
        let list = frames.begin().unwrap();
        let texture = swapchain.texture(image);
        list.texture_barriers(&[TextureBarrier::new(
            texture,
            TextureState::PRESENT,
            TextureState::RENDER_TARGET,
        )]);
        list.begin_render_pass(&RenderPassDesc {
            color: &[ColorAttachment {
                view: swapchain.view(image),
                load: LoadOp::Clear([frame as f32 / 8.0, 0.25, 0.5, 1.0]),
                store: StoreOp::Store,
            }],
            depth: None,
        });
        list.end_render_pass();
        list.texture_barriers(&[TextureBarrier::new(
            texture,
            TextureState::RENDER_TARGET,
            TextureState::PRESENT,
        )]);
        frames.submit(device.queue()).unwrap();
        swapchain.present().unwrap();
    }
    frames.wait_idle().unwrap();
    drop(frames);
    drop(swapchain);
    assert_eq!(device.validation_errors(), 0);
}

#[test]
#[ignore = "needs a desktop session"]
fn enhanced_barriers() {
    clear_and_present(false);
}

#[test]
#[ignore = "needs a desktop session"]
fn legacy_barriers() {
    clear_and_present(true);
}
