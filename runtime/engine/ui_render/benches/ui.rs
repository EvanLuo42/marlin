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
        Affine2, Color, Corners, FillRule, Glow, GroupDesc, Paint, Painter, Path, Rect, RectStyle,
        Shadow, ShapeStyle, Stroke, StrokeStyle, UI_ENTRIES, UiDrawList, UiPrepared, UiRenderer,
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
                path_tiles: s(8),
                coarse_merge: s(9),
                coarse_copy: s(10),
                coarse_plan: s(11),
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

    type Scene<'s> = &'s dyn Fn(&mut Painter<'_>, f32, f32);

    /// The UI passes of a frame of `scenes` on a `size` surface (`scale` physical pixels per
    /// logical one), a sample per frame, the scenes in turn (frames that differ, for damage
    /// detection); prints the passes of the last frame. The tile pool is grown to fit first.
    fn gpu_frames(name: &str, size: (u32, u32), scale: f32, damage: bool, scenes: &[Scene<'_>]) {
        let device = hardware_device();
        let mut r = renderer(damage);
        let surface = UiSurface::new(device, &UiSurfaceDesc::new(1)).unwrap();
        let mut frames = FrameContext::new(device, &FrameContextDesc::default()).unwrap();
        let fonts = [inter()];
        let (lw, lh) = (size.0 as f32 / scale, size.1 as f32 / scale);
        let lists: Vec<UiDrawList> = scenes
            .iter()
            .map(|s| record(scale, &r.glyph_cache().ids, |p| s(p, lw, lh)))
            .collect();
        // The first frames upload the glyphs; the measured ones only draw.
        let first: Vec<UiPrepared> = lists
            .iter()
            .map(|l| r.prepare(&[l], &[], &fonts, None))
            .collect();
        let prepared: Vec<UiPrepared> = lists
            .iter()
            .map(|l| r.prepare(&[l], &[], &fonts, None))
            .collect();
        let commands = prepared[0].surfaces[0].commands().len() as u64;
        let r = &r;
        let surface = &surface;
        let frame = |frames: &mut FrameContext, prepared: &UiPrepared| {
            frames
                .execute(device, |g, _| {
                    let pages = r.begin(g, prepared);
                    let data = &prepared.surfaces[0];
                    let (w, h) = size;
                    r.add_passes(g, pages, surface, data, w, h, Color::BLACK, &[]);
                })
                .unwrap();
        };
        for p in &first {
            frame(&mut frames, p);
        }
        // Until the pool's feedback stops overflowing: a short pool drops commands.
        let mut warm = 0;
        loop {
            for p in &prepared {
                frame(&mut frames, p);
            }
            frames.wait_idle().unwrap();
            warm += 1;
            if !surface.pool_overflowed() && warm > 2 {
                break;
            }
            assert!(warm < 32, "{name}: the tile pool keeps overflowing");
        }
        let mut next = 0;
        let mut last = BTreeMap::new();
        let result = Bench::new(name)
            .throughput_items(commands, "command")
            .metric("pixels", f64::from(size.0 * size.1))
            .metric(
                "pool_mib",
                f64::from(surface.pool_blocks()) * 128.0 / (1 << 20) as f64,
            )
            .run_custom(|| {
                frame(&mut frames, &prepared[next % prepared.len()]);
                next += 1;
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
        assert!(
            !surface.pool_overflowed(),
            "{name}: the tile pool overflowed"
        );
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

    fn gpu_frame(name: &str, size: (u32, u32), scale: f32, damage: bool, scene: Scene<'_>) {
        gpu_frames(name, size, scale, damage, &[scene]);
    }

    pub fn gpu() {
        gpu_frame("gpu/editor_1080p", FHD, 1.0, false, &editor);
        // The same frame again with damage detection (and no whole-frame skip): binning and
        // the tile hashes, no tile shaded.
        gpu_frame("gpu/editor_1080p_clean", FHD, 1.0, true, &editor);
        gpu_frame("gpu/editor_4k", UHD, 2.0, false, &editor);
        gpu_frame("gpu/frosted_1080p", FHD, 1.0, false, &frosted);
        gpu_frame("gpu/paths_1080p", FHD, 1.0, false, &paths);
        gpu_frame("gpu/lines_100k_1080p", FHD, 1.0, false, &|p, w, h| {
            lines(p, w, h, 100_000)
        });
    }

    // Stress scenes, after the cases of Mast's UIPainter talk (GDC 2024) and this renderer's own
    // weak spots.

    /// The settings dialog that took ~673 commands per pixel untiled: `layers` translucent
    /// panels over the whole screen, each with a row of widgets. Nothing is opaque, so every tile
    /// lists every layer: the cost of long tile lists.
    fn overdraw(p: &mut Painter<'_>, w: f32, h: f32, layers: usize) {
        for i in 0..layers {
            let o = (i % 32) as f32 * 3.0;
            p.rect(
                Rect::new(o, o, w - o, h - o),
                &RectStyle {
                    fill: Color::rgba(40 + (i * 7 % 200) as u8, 60, 90, 6),
                    corners: Corners::all(12.0),
                    stroke: Some(Stroke {
                        width: 1.0,
                        color: Color::rgba(255, 255, 255, 8),
                    }),
                },
            );
        }
    }

    /// A cubic Bézier wire from `a` to `b` as `n` line segments.
    fn wire(p: &mut Painter<'_>, a: Vec2, b: Vec2, n: usize, style: &StrokeStyle) {
        let dx = ((b.x - a.x).abs() * 0.5).max(40.0);
        let (c0, c1) = (a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0));
        let at = |t: f32| {
            let u = 1.0 - t;
            a * (u * u * u) + c0 * (3.0 * u * u * t) + c1 * (3.0 * u * t * t) + b * (t * t * t)
        };
        let mut prev = a;
        for k in 1..=n {
            let q = at(k as f32 / n as f32);
            p.line(prev, q, style);
            prev = q;
        }
    }

    /// A node graph (the talk's "noodle graphs"): 600 nodes with shadows, titles and pins, and
    /// 1200 Bézier wires, under a pan and `zoom` (a group transform: zooming in makes wires long
    /// and thick, zooming out packs everything into few tiles).
    fn node_graph(p: &mut Painter<'_>, w: f32, h: f32, zoom: f32) {
        let font = inter();
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(30, 31, 36)));
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(w * 0.5, h * 0.5)
                * Affine2::scale(zoom, zoom)
                * Affine2::translate(-1500.0, -900.0),
            ..GroupDesc::default()
        });
        let node = |i: usize| Vec2::new((i % 30) as f32 * 100.0, (i / 30) as f32 * 90.0);
        let mut rng = Rng::new(0x0de5);
        for _ in 0..1200 {
            let (a, b) = (rng.below(600), rng.below(600));
            let style = StrokeStyle::new(2.0, Color::rgba(200, 200, 120, 200));
            wire(
                p,
                node(a) + Vec2::new(80.0, 30.0),
                node(b) + Vec2::new(0.0, 50.0),
                24,
                &style,
            );
        }
        for i in 0..600 {
            let o = node(i);
            p.begin_group(&GroupDesc {
                transform: Affine2::translate(o.x, o.y),
                shadow: Some(Shadow {
                    offset: Vec2::new(0.0, 3.0),
                    blur: 8.0,
                    color: Color::rgba(0, 0, 0, 140),
                }),
                ..GroupDesc::default()
            });
            p.rect(
                Rect::new(0.0, 0.0, 80.0, 64.0),
                &RectStyle {
                    fill: Color::rgb(52, 56, 66),
                    corners: Corners::all(6.0),
                    stroke: Some(Stroke {
                        width: 1.0,
                        color: Color::rgb(90, 96, 110),
                    }),
                },
            );
            p.rect(
                Rect::new(0.0, 0.0, 80.0, 16.0),
                &RectStyle {
                    fill: Color::rgb(70, 110, 170),
                    corners: Corners {
                        top_left: 6.0,
                        top_right: 6.0,
                        bottom_right: 0.0,
                        bottom_left: 0.0,
                    },
                    stroke: None,
                },
            );
            text(
                p,
                &font,
                10.0,
                &format!("Node {i}"),
                Vec2::new(4.0, 2.0),
                Color::WHITE,
            );
            for k in 0..3 {
                let y = 26.0 + k as f32 * 12.0;
                let pin = ShapeStyle::fill(Color::rgb(120, 200, 140));
                p.circle(Vec2::new(0.0, y), 3.5, &pin);
                p.circle(Vec2::new(80.0, y), 3.5, &pin);
            }
            p.end_group();
        }
        p.end_group();
    }

    /// Mesh previews of the talk's slowest case: a wireframe grid of `n` × `n` quads (with
    /// diagonals) under `zoom` and a turn, so most lines are diagonal; zoomed in, they are long.
    fn wireframe(p: &mut Painter<'_>, w: f32, h: f32, n: usize, zoom: f32) {
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(18, 18, 22)));
        p.begin_group(&GroupDesc {
            transform: Affine2::translate(w * 0.5, h * 0.5)
                * Affine2::rotate(0.6)
                * Affine2::scale(zoom, zoom),
            ..GroupDesc::default()
        });
        let style = StrokeStyle::new(1.0 / zoom, Color::rgba(140, 220, 255, 180));
        let s = 1000.0 / n as f32;
        let at = |i: usize, j: usize| {
            let (x, y) = (i as f32 * s - 500.0, j as f32 * s - 500.0);
            Vec2::new(x, y + 40.0 * ((x * 0.01).sin() + (y * 0.013).cos()))
        };
        for j in 0..n {
            for i in 0..n {
                p.line(at(i, j), at(i + 1, j), &style);
                p.line(at(i, j), at(i, j + 1), &style);
                p.line(at(i, j), at(i + 1, j + 1), &style);
            }
        }
        p.end_group();
    }

    /// The build tool's timeline: 400 rows of 50 bars with labels on every fifth.
    fn timeline(p: &mut Painter<'_>, w: f32, h: f32) {
        let font = inter();
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(26, 27, 31)));
        let mut rng = Rng::new(0x7111);
        let rows = 400;
        let rh = h / rows as f32;
        for r in 0..rows {
            let y = r as f32 * rh;
            let mut x = 0.0;
            for b in 0..50 {
                let len = rng.range_f32(4.0, 70.0);
                let c = if rng.below(8) == 0 {
                    Color::rgb(220, 70, 60)
                } else {
                    Color::rgb(110, 200, 90)
                };
                p.rect(
                    Rect::new(x, y + 0.5, x + len - 1.0, y + rh - 0.5),
                    &RectStyle {
                        fill: c,
                        corners: Corners::all(1.0),
                        stroke: None,
                    },
                );
                if b % 5 == 0 && rh >= 2.0 {
                    let label = format!("job {r}.{b}");
                    text(p, &font, 9.0, &label, Vec2::new(x + 1.0, y), Color::BLACK);
                }
                x += len;
            }
        }
    }

    /// The world map's rivers: 40 rivers of 300 segments each, every river one group with a
    /// glow (the talk's hover glow on a whole group), so the glow covers its members' box.
    fn rivers(p: &mut Painter<'_>, w: f32, h: f32) {
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(90, 110, 70)));
        let mut rng = Rng::new(0x51e7);
        let style = StrokeStyle::new(3.0, Color::rgb(60, 120, 200));
        for _ in 0..40 {
            p.begin_group(&GroupDesc {
                glow: Some(Glow {
                    radius: 10.0,
                    color: Color::rgba(255, 255, 200, 120),
                }),
                ..GroupDesc::default()
            });
            let mut q = Vec2::new(rng.range_f32(0.0, w), 0.0);
            let mut dir = 1.6f32;
            for _ in 0..300 {
                dir += rng.range_f32(-0.4, 0.4);
                let n = q + Vec2::new(dir.cos(), dir.sin().abs()) * 4.0;
                p.line(q, n, &style);
                q = n;
            }
            p.end_group();
        }
    }

    /// Text everywhere: the whole surface filled with 10-pixel text (a log or a big table).
    fn text_wall(p: &mut Painter<'_>, w: f32, h: f32) {
        let font = inter();
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(250, 250, 248)));
        let line = "The quick brown fox jumps over the lazy dog 0123456789 (){}[] <=> +-*/ ";
        let mut y = 0.0;
        let mut i = 0;
        while y < h {
            let s = format!("{i:05} {line}{line}{line}");
            text(
                p,
                &font,
                10.0,
                &s,
                Vec2::new(2.0, y),
                Color::rgb(20, 20, 30),
            );
            y += 12.0;
            i += 1;
        }
    }

    /// Every command behind a chain of 8 nested rounded clips (deep panels in panels).
    fn deep_clips(p: &mut Painter<'_>, w: f32, h: f32) {
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(30, 30, 30)));
        for k in 0..8 {
            let m = 10.0 + k as f32 * 12.0;
            p.push_clip(Rect::new(m, m, w - m, h - m), Corners::all(16.0));
        }
        let mut rng = Rng::new(0xc11b);
        for _ in 0..5000 {
            let a = Vec2::new(rng.range_f32(0.0, w), rng.range_f32(0.0, h));
            p.circle(
                a,
                rng.range_f32(4.0, 30.0),
                &ShapeStyle::fill(Color::rgba(200, 120, 60, 120)),
            );
        }
        for _ in 0..8 {
            p.pop_clip();
        }
    }

    /// Sixteen frosted panels overlapping in a cascade: every one starts a layer (the most a
    /// frame has), with large blur radii.
    fn blur_layers(p: &mut Painter<'_>, w: f32, h: f32) {
        editor(p, w, h);
        for i in 0..16 {
            let o = 60.0 + i as f32 * 50.0;
            let r = Rect::new(o, o * 0.5, o + 600.0, o * 0.5 + 400.0);
            p.backdrop_blur(r, 40.0, Corners::all(16.0));
            p.rect(
                r,
                &RectStyle {
                    fill: Color::rgba(255, 255, 255, 30),
                    corners: Corners::all(16.0),
                    stroke: None,
                },
            );
            text(
                p,
                &inter(),
                14.0,
                &format!("Panel {i}"),
                Vec2::new(o + 12.0, o * 0.5 + 8.0),
                Color::WHITE,
            );
        }
    }

    /// One huge path: a 4000-point star polygon over the whole screen (every 16-pixel band holds
    /// hundreds of segments), and a paragraph of text drawn from its outlines.
    fn huge_path(p: &mut Painter<'_>, w: f32, h: f32) {
        let font = inter();
        p.rect(Rect::new(0.0, 0.0, w, h), &fill(Color::rgb(245, 245, 240)));
        let c = Vec2::new(w * 0.5, h * 0.5);
        let mut star = Path::new();
        for k in 0..4000 {
            let a = k as f32 * core::f32::consts::TAU / 4000.0;
            let r = if k % 2 == 0 { h * 0.48 } else { h * 0.2 };
            let q = c + Vec2::new(a.cos(), a.sin()) * r;
            if k == 0 {
                star.move_to(q);
            } else {
                star.line_to(q);
            }
        }
        star.close();
        p.fill_path(
            &star,
            FillRule::EvenOdd,
            &Paint::Solid(Color::rgba(30, 90, 200, 200)),
        );
        p.set_text_as_paths(true);
        for (i, line) in [
            "Text drawn from its outlines while it zooms:",
            "every glyph one path, flattened on the CPU,",
            "shaded from its tiles' segments on the GPU.",
        ]
        .iter()
        .enumerate()
        {
            text(
                p,
                &font,
                40.0,
                line,
                Vec2::new(40.0, 40.0 + i as f32 * 60.0),
                Color::BLACK,
            );
        }
    }

    /// The editor with the cursor over a different outliner row each frame: damage detection
    /// shades only the two rows that changed.
    fn hover(row: usize) -> impl Fn(&mut Painter<'_>, f32, f32) {
        move |p, w, h| {
            editor(p, w, h);
            let y = 30.0 + row as f32 * 20.0;
            p.rect(
                Rect::new(0.0, y, 300.0, y + 20.0),
                &fill(Color::rgba(90, 160, 240, 60)),
            );
        }
    }

    pub fn stress() {
        gpu_frame("stress/overdraw_256_1080p", FHD, 1.0, false, &|p, w, h| {
            overdraw(p, w, h, 256)
        });
        gpu_frame("stress/node_graph_1080p", FHD, 1.0, false, &|p, w, h| {
            node_graph(p, w, h, 0.6)
        });
        gpu_frame(
            "stress/node_graph_zoom_in_1080p",
            FHD,
            1.0,
            false,
            &|p, w, h| node_graph(p, w, h, 4.0),
        );
        gpu_frame(
            "stress/node_graph_zoom_out_1080p",
            FHD,
            1.0,
            false,
            &|p, w, h| node_graph(p, w, h, 0.15),
        );
        gpu_frame(
            "stress/wireframe_300k_1080p",
            FHD,
            1.0,
            false,
            &|p, w, h| wireframe(p, w, h, 316, 1.0),
        );
        gpu_frame(
            "stress/wireframe_300k_zoom_in_1080p",
            FHD,
            1.0,
            false,
            &|p, w, h| wireframe(p, w, h, 316, 8.0),
        );
        gpu_frame("stress/timeline_20k_1080p", FHD, 1.0, false, &timeline);
        gpu_frame("stress/rivers_glow_1080p", FHD, 1.0, false, &rivers);
        gpu_frame("stress/text_wall_4k", UHD, 2.0, false, &text_wall);
        gpu_frame("stress/deep_clips_1080p", FHD, 1.0, false, &deep_clips);
        gpu_frame("stress/blur_16_layers_1080p", FHD, 1.0, false, &blur_layers);
        gpu_frame("stress/huge_path_1080p", FHD, 1.0, false, &huge_path);
        let rows: Vec<_> = (0..8).map(hover).collect();
        let scenes: Vec<Scene<'_>> = rows.iter().map(|s| s as Scene<'_>).collect();
        gpu_frames("stress/hover_damage_1080p", FHD, 1.0, true, &scenes);
    }

    /// Zooming text through the glyph cache (the talk refills its cache as text zooms): every
    /// iteration a new size of a paragraph, 128 sizes from 16 to 143 pixels, more than the
    /// cache's pages hold, so glyphs are rasterized and pages evicted every frame.
    pub fn text_zoom() {
        let mut r = renderer(true);
        let fonts = [inter()];
        let para = "Zooming text refills the glyph cache: AaBbCcDdEeFfGg 0123456789";
        let mut size = 0usize;
        Bench::new("stress/cpu/text_zoom_prepare")
            .throughput_items(para.len() as u64 * 4, "glyph")
            .run(|| {
                let mut list = UiDrawList::new(1.0);
                {
                    // No fonts: every size goes through the cache, none to paths.
                    let mut p = list.painter(&r.glyph_cache().ids);
                    let px = 16.0 + (size % 128) as f32;
                    for row in 0..4 {
                        text(
                            &mut p,
                            &fonts[0],
                            px,
                            para,
                            Vec2::new(0.0, row as f32 * px * 1.3),
                            Color::WHITE,
                        );
                    }
                }
                size += 1;
                r.prepare(&[&list], &[], &fonts, None)
            });
        let mut frames =
            FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
        r.forget(&mut frames);
    }
}

#[cfg(marlin_platform = "windows")]
marlin_bench::main!(
    windows::cpu,
    windows::gpu,
    windows::stress,
    windows::text_zoom
);

#[cfg(not(marlin_platform = "windows"))]
fn main() {}
