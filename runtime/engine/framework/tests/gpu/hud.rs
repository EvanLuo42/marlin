//! The stats overlay over a frame, with the engine's cooked UI shaders and font: it draws its
//! panel and leaves the rest of the frame alone, and the frames between drawings blend the last
//! one unchanged.

use std::time::Duration;

use marlin_asset::AssetServer;
use marlin_framework::hud::{Hud, HudInfo, Snapshot};
use marlin_framework::{ContentSettings, mount};
use marlin_gpu_test_support::hardware_device;
use marlin_render_graph::{FrameContext, FrameContextDesc, PassContext, TextureDesc};
use marlin_renderer::load_engine_shaders;
use marlin_rhi::{Format, LoadOp, StoreOp};
use marlin_ui_render::load_ui_font;
use marlin_world::{Modules, Resources};

use crate::end_to_end::{LEVEL, package, sample};

const WIDTH: u32 = 400;
const HEIGHT: u32 = 520;
/// The frame under the overlay.
const CLEAR: [u8; 4] = [0, 128, 0, 255];

#[test]
fn the_stats_overlay_draws_its_panel_and_leaves_the_frame_alone() {
    let root = sample("hud");
    let boot = mount(&ContentSettings {
        packages: vec![package(&root)],
        cookd: None,
        platform: marlin_rt::PLATFORM.into(),
        level: LEVEL.parse().unwrap(),
    })
    .unwrap();
    let device = hardware_device();
    let modules = Modules::new(marlin::game_modules()).unwrap();
    let mut assets = AssetServer::new(modules.table().loaders(&Resources::new()));
    let library = load_engine_shaders(&mut assets, &boot.vfs, None).unwrap();
    let font = load_ui_font(&mut assets, &boot.vfs, None).unwrap();
    let library = assets.get(&library).unwrap();
    let font = assets.get(&font).unwrap().view();

    let mut hud = Hud::new(device, library, Format::Rgba8Unorm).unwrap();
    // Drawn once, then blended from the last drawing.
    hud.set_redraw_interval(Duration::from_secs(3600));
    for i in 0..60 {
        hud.push_interval(if i == 40 { 0.012 } else { 0.004 });
    }
    hud.set_snapshot(Snapshot {
        fps: 250.0,
        frame: Duration::from_micros(4000),
        gpu: Duration::from_micros(3100),
        busy: [
            Duration::from_micros(2500),
            Duration::from_micros(200),
            Duration::ZERO,
        ],
        cpu: vec![("Render", Duration::from_micros(900))],
        passes: vec![("Lighting", Duration::from_micros(700))],
    });
    let info = HudInfo {
        adapter: "Test adapter",
        width: WIDTH,
        height: HEIGHT,
        vsync: false,
        passes: 40,
        lists: 3,
    };
    let mut frames = FrameContext::new(
        device,
        &FrameContextDesc {
            overlap: true,
            ..FrameContextDesc::default()
        },
    )
    .unwrap();
    let mut shoot = |hud: &mut Hud| -> Vec<u8> {
        let frame = hud
            .frame(device, &mut frames, font, 1.0, &info, None)
            .unwrap();
        let ticket = frames
            .execute(device, |g, _| {
                let t = g.create_texture(
                    "Frame",
                    TextureDesc::new_2d(WIDTH, HEIGHT, Format::Rgba8Unorm),
                );
                let clear = CLEAR.map(|c| f32::from(c) / 255.0);
                let t = g.add_raster("Clear", |b| {
                    let t = b.color(t, LoadOp::Clear(clear), StoreOp::Store);
                    (t, |_: &mut PassContext<'_>| {})
                });
                let t = hud.add_passes(g, &frame, t);
                g.readback_texture(t)
            })
            .unwrap();
        frames.wait_readback(ticket).unwrap()
    };
    let drawn = shoot(&mut hud);
    let blended = shoot(&mut hud);
    assert!(
        drawn == blended,
        "a frame blending the last drawing shows the same"
    );

    let at = |x: u32, y: u32| {
        let i = ((y * WIDTH + x) * 4) as usize;
        [drawn[i], drawn[i + 1], drawn[i + 2], drawn[i + 3]]
    };
    // The panel: its 264-pixel width from a 10-pixel margin, a pixel more for its edges; and
    // its height, a few hundred pixels.
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if x >= 276 || y >= 400 {
                assert_eq!(at(x, y), CLEAR, "({x}, {y}) is outside the panel");
            }
        }
    }
    // Inside: the dark panel, and text brighter than it.
    let inside = || (12..368).flat_map(|y| (12..272).map(move |x| (x, y)));
    let dark = inside().filter(|&(x, y)| at(x, y)[1] < 64).count();
    assert!(
        dark * 10 > inside().count() * 7,
        "the panel's background darkens the frame: {dark} of {} pixels",
        inside().count()
    );
    let bright = (0..400)
        .flat_map(|y| (0..276).map(move |x| (x, y)))
        .filter(|&(x, y)| at(x, y)[0] > 180)
        .count();
    assert!(bright > 200, "the panel's text: {bright} bright pixels");
}
