//! Opens a window and clears it to an animated color until it is closed: the first slice through
//! marlin-app, marlin-input and marlin-rhi, as a no_std game binary.
//!
//! Esc or gamepad Start: quit. F11 or Alt+Enter: borderless fullscreen. V: toggle vsync.
//! Space or gamepad South: pause the animation. Mouse motion, the wheel and the left stick shift
//! the hue. Typed text goes to the log.
#![no_std]
#![no_main]

use core::fmt::Write;
use core::time::Duration;

use marlin_app::{App, Event, Surface, WindowDesc};
use marlin_diag::{FixedBuf, chain, info, warn};
use marlin_input::{GamepadButton, Input, Key};
use marlin_math::scalar::sin;
use marlin_rhi::{
    AdapterPreference, ColorAttachment, DeviceDesc, FramesInFlight, Instance, InstanceDesc, LoadOp,
    PresentMode, PresentStatus, RenderPassDesc, StoreOp, SurfaceHandle, SwapchainDesc,
    TextureBarrier, TextureState,
};
use marlin_time::Instant;

marlin_rt::game_entry!(demo_main);

fn demo_main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(error) => {
            marlin_diag::error!("demo_clear: {}", chain(&error));
            1
        }
    }
}

// Each message names only the failing layer; `chain` appends the cause.
#[derive(Debug, marlin_error::Error)]
enum Failure {
    #[error("platform")]
    Platform(#[from] marlin_app::Error),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
}

fn run() -> Result<(), Failure> {
    let mut app = App::new()?;
    let window = app.create_window(&WindowDesc {
        title: "Marlin clear",
        ..WindowDesc::default()
    })?;
    let mut input = Input::new(&app)?;

    let instance = Instance::new(&InstanceDesc::default())?;
    let device = match instance.create_device(&DeviceDesc::default()) {
        Ok(device) => device,
        Err(e) => {
            warn!("no hardware adapter ({e}); using the software rasterizer");
            instance.create_device(&DeviceDesc {
                adapter: AdapterPreference::Software,
                ..DeviceDesc::default()
            })?
        }
    };
    info!(
        "adapter: {} ({:?})",
        device.adapter_info().name(),
        device.features()
    );

    let Surface::Win32 { hwnd, hinstance } = window.surface() else {
        unreachable!("only Win32 windows exist on this platform")
    };
    let size = window.size();
    // SAFETY: the window outlives the swapchain: locals drop in reverse order.
    let mut swapchain = unsafe {
        device.create_swapchain(
            SurfaceHandle::Win32 { hwnd, hinstance },
            &SwapchainDesc {
                width: size.width,
                height: size.height,
                ..SwapchainDesc::default()
            },
        )
    }?;
    let mut frames = FramesInFlight::new(&device, 2)?;

    let mut vsync = true;
    let mut paused = false;
    let mut minimized = false;
    let mut time = 0.0f32;
    let mut hue = 0.0f32;
    let mut last = Instant::now();
    let mut fps_window = (Instant::now(), 0u32);

    'main: loop {
        // Sleep in the pump while minimized instead of spinning.
        let timeout = if minimized {
            Some(Duration::from_millis(100))
        } else {
            Some(Duration::ZERO)
        };
        for &event in app.pump_timeout(timeout) {
            match event {
                Event::CloseRequested(_) | Event::Quit => break 'main,
                Event::Resized { size, .. } => swapchain.resize(size.width, size.height)?,
                Event::Minimized(_) => minimized = true,
                Event::Restored(_) => minimized = false,
                Event::Suspended => info!("suspended"),
                Event::Resumed => info!("resumed"),
                _ => {}
            }
        }

        input.update();
        let state = input.state();
        let pad = state.gamepad(0);
        if state.key_pressed(Key::ESCAPE) || pad.pressed(GamepadButton::Start) {
            break;
        }
        if state.key_pressed(Key::F11) || (state.alt() && state.key_pressed(Key::ENTER)) {
            window.toggle_fullscreen()?;
        }
        if state.key_pressed(Key::V) {
            vsync = !vsync;
            swapchain.set_present_mode(if vsync {
                PresentMode::Fifo
            } else {
                PresentMode::Immediate
            });
            info!("vsync {}", if vsync { "on" } else { "off" });
        }
        if state.key_pressed(Key::SPACE) || pad.pressed(GamepadButton::South) {
            paused = !paused;
        }
        if !state.text().is_empty() {
            info!("typed {:?}", state.text());
        }

        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32().min(0.1);
        last = now;
        if !paused {
            time += dt;
        }
        let [dx, _] = state.mouse().delta();
        hue += dx * 0.002 + state.mouse().wheel()[1] * 0.25 + pad.left_stick()[0] * dt * 2.0;

        if minimized {
            continue;
        }

        let image = swapchain.acquire(None)?;
        let texture = swapchain.texture(image);
        let list = frames.begin()?;
        list.texture_barriers(&[TextureBarrier::new(
            texture,
            TextureState::PRESENT,
            TextureState::RENDER_TARGET,
        )]);
        list.begin_render_pass(&RenderPassDesc {
            color: &[ColorAttachment {
                view: swapchain.view(image),
                load: LoadOp::Clear(color(time + hue)),
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
        frames.submit(device.queue())?;
        if swapchain.present()? == PresentStatus::Occluded {
            // Fully covered: nothing is shown, so don't burn power on more frames.
            app.pump_timeout(Some(Duration::from_millis(16)));
        }

        fps_window.1 += 1;
        let elapsed = now.duration_since(fps_window.0);
        if elapsed >= Duration::from_secs(1) {
            let mut title = FixedBuf::<64>::new();
            let fps = fps_window.1 as f32 / elapsed.as_secs_f32();
            let _ = write!(title, "Marlin clear  {fps:.0} fps");
            window.set_title(title.as_str());
            fps_window = (now, 0);
        }
    }

    frames.wait_idle()?;
    Ok(())
}

/// A slowly cycling, fully saturated color: three sines a third of a turn apart.
fn color(t: f32) -> [f32; 4] {
    const TAU: f32 = core::f32::consts::TAU;
    let channel = |phase: f32| 0.5 + 0.5 * sin(t * 0.7 + phase * TAU / 3.0);
    [channel(0.0), channel(1.0), channel(2.0), 1.0]
}
