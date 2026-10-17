//! Clear-and-present on a real (hidden) window, with validation on, through both barrier paths.
//! Desktop tier (docs/design/0014-testing.md §5): it needs a desktop session. Its own binary, as
//! it owns the window and swapchain. Run alone with
//! `cargo test -p marlin-rhi --test isolated_desktop_present`.
#![cfg(marlin_platform = "windows")]

use marlin_gpu_test_support as support;

use std::time::Duration;

use marlin_app::{App, Surface, WindowDesc};
use marlin_rhi::{
    ColorAttachment, ColorSpace, Device, ErrorKind, Format, FramesInFlight, HdrMetadata, LoadOp,
    RenderPassDesc, StoreOp, SurfaceHandle, SwapchainDesc, TextureBarrier, TextureState,
};
use support::devices;

fn clear_and_present(device: &Device) {
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
    let mut frames = FramesInFlight::new(device, 2).unwrap();

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
        // SAFETY: the list only uses the swapchain image, which the swapchain keeps alive.
        unsafe { frames.submit(device.queue()).unwrap() };
        swapchain.present().unwrap();
    }
    frames.wait_idle().unwrap();

    // The display the window is on, and what it can do.
    let output = swapchain.output().unwrap();
    assert!(!output.name().is_empty(), "{output:?}");
    // Statistics exist once presents reached the display (a hidden window may report none).
    if let Some(stats) = swapchain.frame_statistics().unwrap() {
        assert!(stats.present_count > 0, "{stats:?}");
    }
    // An 8-bit swapchain takes neither HDR color space.
    assert_eq!(swapchain.color_space(), ColorSpace::Srgb);
    assert!(!swapchain.supports_color_space(ColorSpace::Hdr10));
    assert_eq!(
        swapchain
            .set_color_space(ColorSpace::ScRgb)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    drop(frames);
    drop(swapchain);

    // A 10-bit swapchain: HDR10 when the display does it, metadata either way.
    // SAFETY: the window outlives the swapchain (dropped first, below).
    let mut hdr = unsafe {
        device.create_swapchain(
            SurfaceHandle::Win32 { hwnd, hinstance },
            &SwapchainDesc {
                width: 64,
                height: 64,
                format: Format::Rgb10a2Unorm,
                ..SwapchainDesc::default()
            },
        )
    }
    .unwrap();
    hdr.set_hdr_metadata(Some(&HdrMetadata::rec2020(0.01, 1000.0)))
        .unwrap();
    if hdr.supports_color_space(ColorSpace::Hdr10) {
        hdr.set_color_space(ColorSpace::Hdr10).unwrap();
        assert_eq!(hdr.color_space(), ColorSpace::Hdr10);
        hdr.set_color_space(ColorSpace::Srgb).unwrap();
    }
    hdr.set_hdr_metadata(None).unwrap();
    drop(hdr);
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn enhanced_barriers() {
    clear_and_present(&devices()[0]);
}

#[test]
fn legacy_barriers() {
    clear_and_present(&devices()[1]);
}
