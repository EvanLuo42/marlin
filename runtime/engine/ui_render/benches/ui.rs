//! The UI renderer's cost (0012 §11, §5): the CPU side (recording, flattening, hashing,
//! preparing, planning blurs) and the GPU passes of one frame, on editor-like and stress scenes.
//! `cargo xtask test --tier bench -p marlin-ui-render`, or
//! `cargo bench -p marlin-ui-render --bench ui -- [filter]`.
//!
//! GPU times are the render graph's timestamps (`FrameContext::gpu_timings`) summed over the UI
//! passes of one frame; each sample is one frame. After each GPU bench, the passes of the last
//! frame are printed. Only the Windows backend runs GPU work so far; elsewhere this is empty.

#[cfg(marlin_platform = "windows")]
#[path = "../tests/support/mod.rs"]
mod support;

#[cfg(marlin_platform = "windows")]
mod windows {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use marlin_bench::Bench;
    use marlin_font::{FontId, FontView, GlyphIds, TextLayout};
    use marlin_gpu_test_support::{hardware_device, shader};
    use marlin_math::{Rng, Vec2};
    use marlin_render_graph::{FrameContext, FrameContextDesc};
    use marlin_ui_render::damage::command_hashes;
    use marlin_ui_render::{
        Affine2, Color, Corners, FillRule, GroupDesc, Paint, Painter, Path, Rect, RectStyle,
        ShapeStyle, Stroke, StrokeStyle, UI_ENTRIES, UiDrawList, UiPrepared, UiRenderer,
        UiRendererDesc, UiShaders, UiSurface, UiSurfaceDesc, blur,
    };

    use crate::support::inter;

    const FHD: (u32, u32) = (1920, 1080);
    const UHD: (u32, u32) = (3840, 2160);

    fn text(p: &mut Painter<'_>, font: &FontView<'_>, size: f32, s: &str, at: Vec2, c: Color) {
        let layout = TextLayout::new(font, FontId(0), size, s);
        p.text(&layout, at, c);
    }

    fn fill(c: Color) -> RectStyle {
        RectStyle::fill(c)
    }

    /// An editor window of `w` × `h` logical pixels: menu, outliner, details, a viewport with a
    /// grid, a content browser and a log, text-heavy like the real one.
    fn editor(p: &mut Painter<'_>, w: f32, h: f32) {
        let font = inter();
        let f = &font;
        let ink = Color::rgb(220, 222, 228);
        let dim = Color::rgb(150, 154, 162);
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(28, 30, 34)));
        p.rect(Rect::new(0.0, 0.0, w, 26.0), &fill(Color::rgb(38, 40, 46)));
        let menu = [
            "File", "Edit", "View", "Level", "Build", "Tools", "Window", "Help",
        ];
        for (i, m) in menu.iter().enumerate() {
            text(p, f, 13.0, m, Vec2::new(10.0 + i as f32 * 56.0, 5.0), ink);
        }
        let (left, right, bottom) = (300.0, w - 360.0, h - 280.0);

        // Outliner.
        p.push_clip(Rect::new(0.0, 26.0, left, bottom), Corners::ZERO);
        let mut y = 30.0;
        let mut i = 0;
        while y < bottom {
            if i % 2 == 0 {
                p.rect(
                    Rect::new(0.0, y, left, y + 20.0),
                    &fill(Color::rgb(33, 35, 40)),
                );
            }
            let depth = (i % 5) as f32 * 14.0;
            p.circle(
                Vec2::new(14.0 + depth, y + 10.0),
                5.0,
                &ShapeStyle::fill(Color::rgb(90, 160, 240)),
            );
            let name = format!("Entity_{i:04} StaticMeshActor");
            text(p, f, 12.0, &name, Vec2::new(26.0 + depth, y + 3.0), ink);
            y += 20.0;
            i += 1;
        }
        p.pop_clip();

        // Details.
        p.push_clip(Rect::new(right, 26.0, w, bottom), Corners::ZERO);
        let mut y = 32.0;
        let mut i = 0;
        while y < bottom {
            let label = format!("Property {i}");
            text(p, f, 12.0, &label, Vec2::new(right + 10.0, y + 3.0), dim);
            p.rect(
                Rect::new(right + 150.0, y, w - 10.0, y + 20.0),
                &RectStyle {
                    fill: Color::rgb(24, 25, 29),
                    corners: Corners::all(3.0),
                    stroke: Some(Stroke {
                        width: 1.0,
                        color: Color::rgb(60, 63, 72),
                    }),
                },
            );
            let value = format!("{:.3}, {:.3}, {:.3}", i as f32 * 0.25, 1.5, -2.0);
            text(p, f, 12.0, &value, Vec2::new(right + 156.0, y + 3.0), ink);
            y += 24.0;
            i += 1;
        }
        p.pop_clip();

        // Viewport: a grid.
        p.push_clip(Rect::new(left, 26.0, right, bottom), Corners::ZERO);
        p.rect(
            Rect::new(left, 26.0, right, bottom),
            &fill(Color::rgb(60, 66, 76)),
        );
        let grid = StrokeStyle::new(1.0, Color::rgba(255, 255, 255, 40));
        let mut x = left;
        while x < right {
            p.line(Vec2::new(x, 26.0), Vec2::new(x, bottom), &grid);
            x += 32.0;
        }
        let mut y = 26.0;
        while y < bottom {
            p.line(Vec2::new(left, y), Vec2::new(right, y), &grid);
            y += 32.0;
        }
        p.pop_clip();

        // Content browser.
        p.push_clip(Rect::new(0.0, bottom, w * 0.6, h), Corners::ZERO);
        let mut i = 0;
        for row in 0..((h - bottom) / 110.0) as i32 {
            for col in 0..(w * 0.6 / 96.0) as i32 {
                let (x, y) = (8.0 + col as f32 * 96.0, bottom + 8.0 + row as f32 * 110.0);
                p.rect(
                    Rect::new(x, y, x + 88.0, y + 102.0),
                    &RectStyle {
                        fill: Color::rgb(40, 43, 50),
                        corners: Corners::all(6.0),
                        stroke: None,
                    },
                );
                let thumb = Color::rgb(70 + (i * 13 % 100) as u8, 90, 120);
                p.rect(
                    Rect::new(x + 8.0, y + 8.0, x + 80.0, y + 72.0),
                    &fill(thumb),
                );
                let name = format!("M_Asset{i:03}");
                text(p, f, 11.0, &name, Vec2::new(x + 6.0, y + 80.0), ink);
                i += 1;
            }
        }
        p.pop_clip();

        // Log.
        p.push_clip(Rect::new(w * 0.6, bottom, w, h), Corners::ZERO);
        let mut y = bottom + 4.0;
        let mut i = 0;
        while y < h {
            let line = format!("[{i:05}] LogCook: cooked Marlin.Engine.Shaders.Ui in 0.{i:03} s");
            text(p, f, 12.0, &line, Vec2::new(w * 0.6 + 8.0, y), dim);
            y += 16.0;
            i += 1;
        }
        p.pop_clip();
    }

    /// The editor under three frosted panels, the third over the first two (two layers).
    fn frosted(p: &mut Painter<'_>, w: f32, h: f32) {
        editor(p, w, h);
        let panels = [
            Rect::new(400.0, 120.0, 900.0, 520.0),
            Rect::new(1000.0, 120.0, 1500.0, 520.0),
            Rect::new(700.0, 400.0, 1200.0, 800.0),
        ];
        for (i, r) in panels.into_iter().enumerate() {
            p.backdrop_blur(r, 16.0 + i as f32 * 8.0, Corners::all(14.0));
            p.rect(
                r,
                &RectStyle {
                    fill: Color::rgba(255, 255, 255, 40),
                    corners: Corners::all(14.0),
                    stroke: None,
                },
            );
        }
    }

    /// Decima's wireframe case: `n` short thin lines all over the screen.
    fn lines(p: &mut Painter<'_>, w: f32, h: f32, n: usize) {
        let mut rng = Rng::new(0x11e5);
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(20, 20, 24)));
        let style = StrokeStyle::new(1.0, Color::rgba(120, 220, 140, 160));
        for _ in 0..n {
            let a = Vec2::new(rng.range_f32(0.0, w), rng.range_f32(0.0, h));
            let d = Vec2::new(rng.range_f32(-40.0, 40.0), rng.range_f32(-40.0, 40.0));
            p.line(a, a + d, &style);
        }
    }

    /// Filled paths and large text: 400 stars, and titles drawn from their outlines, turned.
    fn paths(p: &mut Painter<'_>, w: f32, h: f32) {
        let font = inter();
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(240, 240, 236)));
        let red = Paint::Solid(Color::rgb(200, 60, 80));
        for i in 0..400 {
            let c = Vec2::new(40.0 + (i % 25) as f32 * 74.0, 40.0 + (i / 25) as f32 * 64.0);
            let mut star = Path::new();
            for k in 0..5 {
                let a =
                    -core::f32::consts::FRAC_PI_2 + k as f32 * 4.0 * core::f32::consts::PI / 5.0;
                let q = c + Vec2::new(a.cos(), a.sin()) * 26.0;
                if k == 0 {
                    star.move_to(q);
                } else {
                    star.line_to(q);
                }
            }
            star.close();
            p.fill_path(&star, FillRule::NonZero, &red);
        }
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(200.0, 300.0) * Affine2::rotate(0.2),
            ..GroupDesc::default()
        });
        for (i, t) in ["Marlin", "UI renderer", "Paths"].iter().enumerate() {
            let layout = TextLayout::new(&font, FontId(0), 120.0, t);
            p.text(
                &layout,
                Vec2::new(0.0, i as f32 * 150.0),
                Color::rgb(20, 30, 60),
            );
        }
        p.end_group();
    }

    fn record(scale: f32, ids: &GlyphIds, f: impl FnOnce(&mut Painter<'_>)) -> UiDrawList {
        let fonts = [inter()];
        let mut list = UiDrawList::new(scale);
        f(&mut list.painter(ids).with_fonts(&fonts));
        list
    }

    fn renderer(damage: bool) -> UiRenderer {
        let s = |i: usize| shader(UI_ENTRIES[i]);
        UiRenderer::new(
            hardware_device(),
            &UiShaders {
                glyph_upload: s(0),
                bounds: s(1),
                bin: s(2),
                coarse: s(3),
                fine: s(4),
                encode_vs: s(5),
                encode_ps: s(6),
                blur: s(7),
            },
            &UiRendererDesc {
                damage,
                ..UiRendererDesc::default()
            },
        )
        .unwrap()
    }

    pub fn cpu() {
        let ids = GlyphIds::new();
        let (w, h) = (FHD.0 as f32, FHD.1 as f32);
        let list = record(1.0, &ids, |p| editor(p, w, h));
        let chunk = list.flatten();
        let n = chunk.len() as u64;

        Bench::new("cpu/record/editor_1080p")
            .throughput_items(n, "command")
            .run(|| record(1.0, &ids, |p| editor(p, w, h)));
        Bench::new("cpu/flatten/editor_1080p")
            .throughput_items(n, "command")
            .run(|| list.flatten());
        Bench::new("cpu/hash/editor_1080p")
            .throughput_items(n, "command")
            .run(|| command_hashes(&chunk, &[], &[]));

        let mut r = renderer(true);
        let fonts = [inter()];
        let list = record(1.0, &r.glyph_cache().ids, |p| editor(p, w, h));
        // The first prepare rasterizes; the rest find every glyph resident.
        r.prepare(&[&list], &[], &fonts, None);
        Bench::new("cpu/prepare/editor_1080p")
            .throughput_items(n, "command")
            .run(|| r.prepare(&[&list], &[], &fonts, None));
        let mut frames =
            FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
        r.forget(&mut frames);

        let frosted = record(1.0, &ids, |p| frosted(p, w, h)).flatten();
        Bench::new("cpu/blur_plan/frosted_1080p")
            .throughput_items(frosted.len() as u64, "command")
            .run(|| blur::plan(&frosted, &[], FHD.0, FHD.1));

        let n = record(1.0, &ids, |p| paths(p, w, h)).flatten().len() as u64;
        Bench::new("cpu/record/paths_1080p")
            .throughput_items(n, "command")
            .run(|| record(1.0, &ids, |p| paths(p, w, h)));
    }

    /// The UI passes of one frame of `scene` on a `size` surface (`scale` physical pixels per
    /// logical one), a sample per frame; prints the passes of the last frame.
    fn gpu_frame(
        name: &str,
        size: (u32, u32),
        scale: f32,
        damage: bool,
        scene: impl Fn(&mut Painter<'_>, f32, f32),
    ) {
        let device = hardware_device();
        let mut r = renderer(damage);
        let surface = UiSurface::new(device, &UiSurfaceDesc::new(1)).unwrap();
        let mut frames = FrameContext::new(device, &FrameContextDesc::default()).unwrap();
        let fonts = [inter()];
        let (lw, lh) = (size.0 as f32 / scale, size.1 as f32 / scale);
        let list = record(scale, &r.glyph_cache().ids, |p| scene(p, lw, lh));
        // The first frame uploads the glyphs; the measured ones only draw.
        let first = r.prepare(&[&list], &[], &fonts, None);
        let prepared = r.prepare(&[&list], &[], &fonts, None);
        let commands = prepared.surfaces[0].commands().len() as u64;
        let r = &r;
        let frame = |frames: &mut FrameContext, prepared: &UiPrepared| {
            frames
                .execute(device, |g, _| {
                    let pages = r.begin(g, prepared);
                    let data = &prepared.surfaces[0];
                    let (w, h) = size;
                    r.add_passes(g, pages, &surface, data, w, h, Color::BLACK, &[]);
                })
                .unwrap();
        };
        frame(&mut frames, &first);
        let mut last = BTreeMap::new();
        let result = Bench::new(name)
            .throughput_items(commands, "command")
            .metric("pixels", f64::from(size.0 * size.1))
            .run_custom(|| {
                frame(&mut frames, &prepared);
                last.clear();
                let mut total = 0;
                for t in frames
                    .gpu_timings()
                    .iter()
                    .filter(|t| t.name.starts_with("Ui"))
                {
                    *last.entry(t.name).or_insert(0u64) += t.nanoseconds;
                    total += t.nanoseconds;
                }
                (Duration::from_nanos(total), 1)
            });
        if result.is_some() {
            let parts: Vec<String> = last
                .iter()
                .map(|(n, ns)| format!("{n} {:.1} µs", *ns as f64 / 1000.0))
                .collect();
            println!("    {}", parts.join(", "));
        }
        frames.wait_idle().unwrap();
        surface.forget(&mut frames);
        r.forget(&mut frames);
    }

    pub fn gpu() {
        gpu_frame("gpu/editor_1080p", FHD, 1.0, false, editor);
        // The same frame again with damage detection (and no whole-frame skip): binning and
        // the tile hashes, no tile shaded.
        gpu_frame("gpu/editor_1080p_clean", FHD, 1.0, true, editor);
        gpu_frame("gpu/editor_4k", UHD, 2.0, false, editor);
        gpu_frame("gpu/frosted_1080p", FHD, 1.0, false, frosted);
        gpu_frame("gpu/paths_1080p", FHD, 1.0, false, paths);
        gpu_frame("gpu/lines_100k_1080p", FHD, 1.0, false, |p, w, h| {
            lines(p, w, h, 100_000)
        });
    }
}

#[cfg(marlin_platform = "windows")]
marlin_bench::main!(windows::cpu, windows::gpu);

#[cfg(not(marlin_platform = "windows"))]
fn main() {}
