//! Damage detection, whole-frame skips, several surfaces and external images (U2d, U2e;
//! 0012 §5.5, §7, §14).

use marlin_font::FontView;
use marlin_gpu_test_support::{Image, hardware_device};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc, PassContext, TextureDesc};
use marlin_rhi::{Format, LoadOp, StoreOp};
use marlin_ui_render::{
    Color, Corners, ExternalImageId, ImageColor, ImageSource, ImageStyle, Rect, RectStyle,
    ShapeStyle, UiDrawList, UiPrepared, UiRendererDesc, UiSurface, UiSurfaceDesc, reference,
};

use crate::support::{CpuTexture, CpuTextures, Pages, inter, scene};
use crate::{TestUi, renderer, shared_renderer};

const W: u32 = 300;
const H: u32 = 190;

/// One frame of `list`: the image and how many tiles were shaded.
fn frame(r: &mut TestUi, frames: &mut FrameContext, list: &UiDrawList) -> (Vec<u8>, usize) {
    let fonts: [FontView<'_>; 1] = [inter()];
    let data = r.prepare(list, &fonts, None);
    let (image, heads) = frames
        .execute(hardware_device(), |g, _| {
            let out = r.add_passes(g, &data, W, H, Color::BLACK);
            (g.readback_texture(out.target), g.readback(out.heads))
        })
        .unwrap();
    let image = frames.wait_readback(image).unwrap();
    let heads = frames.wait_readback(heads).unwrap();
    let shaded = heads
        .as_chunks::<16>()
        .0
        .iter()
        .filter(|h| u32::from_le_bytes([h[8], h[9], h[10], h[11]]) != 0)
        .count();
    (image, shaded)
}

/// The scene with a dot at `dot`.
fn list(r: &TestUi, dot: Vec2) -> UiDrawList {
    let mut list = UiDrawList::new(1.0);
    let mut p = list.painter(&r.glyph_cache().ids);
    scene(&mut p, W as f32, H as f32, 0);
    p.circle(dot, 6.0, &ShapeStyle::fill(Color::rgb(255, 0, 255)));
    list
}

#[test]
fn only_changed_tiles_are_shaded_and_the_pixels_stay_the_same() {
    let mut on = renderer(&UiRendererDesc::default());
    let mut off = renderer(&UiRendererDesc {
        damage: false,
        ..UiRendererDesc::default()
    });
    let mut f_on = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut f_off = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let (tx, ty) = (W.div_ceil(16) as usize, H.div_ceil(16) as usize);
    // The dot moves within one tile, stays, then jumps across the surface.
    let dots = [
        Vec2::new(40.0, 150.0),
        Vec2::new(43.0, 151.0),
        Vec2::new(43.0, 151.0),
        Vec2::new(260.0, 30.0),
    ];
    for (i, &dot) in dots.iter().enumerate() {
        let (l_on, l_off) = (list(&on, dot), list(&off, dot));
        let (a, shaded) = frame(&mut on, &mut f_on, &l_on);
        let (b, all) = frame(&mut off, &mut f_off, &l_off);
        assert_eq!(a, b, "frame {i}: damage detection changes no pixel");
        assert_eq!(all, tx * ty);
        match i {
            0 => assert_eq!(shaded, tx * ty, "the first frame draws everything"),
            // The dot's old and new tiles: its 6 px radius and the anti-aliasing reach at most
            // two tiles across and down, so four.
            1 => assert!((1..=4).contains(&shaded), "frame 1: {shaded} tiles"),
            2 => assert_eq!(shaded, 0, "nothing changed"),
            _ => assert!((2..=8).contains(&shaded), "frame 3: {shaded} tiles"),
        }
    }
    f_on.wait_idle().unwrap();
    f_off.wait_idle().unwrap();
    on.forget(&mut f_on);
    off.forget(&mut f_off);
    assert_eq!(hardware_device().validation_errors(), 0);
}

#[test]
fn unchanged_frames_need_no_redraw() {
    let mut r = renderer(&UiRendererDesc::default());
    let fonts: [FontView<'_>; 1] = [inter()];
    let a = r.prepare(&list(&r, Vec2::new(10.0, 10.0)), &fonts, None);
    let b_list = list(&r, Vec2::new(11.0, 10.0));
    let b = r.prepare(&b_list, &fonts, None);
    let data = &a.0.surfaces[0];
    let s = &r.surface;
    assert!(s.needs_redraw(data, W, H, Color::BLACK));
    assert!(!s.needs_redraw(data, W, H, Color::BLACK));
    // Another size or clear color, or another list, redraws.
    assert!(s.needs_redraw(data, W + 1, H, Color::BLACK));
    assert!(s.needs_redraw(data, W + 1, H, Color::WHITE));
    assert!(s.needs_redraw(&b.0.surfaces[0], W + 1, H, Color::WHITE));
    s.invalidate();
    assert!(s.needs_redraw(&b.0.surfaces[0], W + 1, H, Color::WHITE));
}

#[test]
fn two_surfaces_share_the_glyph_cache_in_one_graph() {
    let (_texture, view) = crate::shading::checker_texture();
    let mut ui = shared_renderer(&UiRendererDesc::default());
    let windows = [
        UiSurface::new(hardware_device(), &UiSurfaceDesc::new(10)).unwrap(),
        UiSurface::new(hardware_device(), &UiSurfaceDesc::new(11)).unwrap(),
    ];
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let font = inter();
    let fonts: [FontView<'_>; 1] = [font];
    let sizes = [(W, H), (160, 120)];
    let mut lists = Vec::new();
    for (i, &(w, h)) in sizes.iter().enumerate() {
        let mut list = UiDrawList::new(1.0);
        let mut p = list.painter(&ui.glyph_cache().ids);
        if i == 0 {
            scene(&mut p, w as f32, h as f32, view.index());
        } else {
            p.rect(
                Rect::new(0.0, 0.0, 160.0, 120.0),
                &RectStyle::fill(Color::rgb(30, 60, 90)),
            );
            let layout =
                marlin_font::TextLayout::new(&font, marlin_font::FontId(0), 14.0, "Second window");
            p.text(&layout, Vec2::new(8.0, 8.0), Color::WHITE);
        }
        lists.push(list);
    }
    let refs: Vec<&UiDrawList> = lists.iter().collect();
    let prepared: UiPrepared = ui.prepare(&refs, &[], &fonts, None);
    let tickets = frames
        .execute(hardware_device(), |g, _| {
            let pages = ui.begin(g, &prepared);
            (0..2)
                .map(|i| {
                    let (w, h) = sizes[i];
                    let out = ui.add_passes(
                        g,
                        pages,
                        &windows[i],
                        &prepared.surfaces[i],
                        w,
                        h,
                        Color::BLACK,
                        &[],
                    );
                    g.readback_texture(out.target)
                })
                .collect::<Vec<_>>()
        })
        .unwrap();
    let mut pages = Pages::new(ui.glyph_cache().config().page_size);
    pages.apply(prepared.uploads());
    let texture = CpuTexture::checker();
    let textures = CpuTextures {
        pages: &pages,
        texture: &texture,
    };
    for (i, t) in tickets.into_iter().enumerate() {
        let (w, h) = sizes[i];
        let gpu = Image::new(w, h, frames.wait_readback(t).unwrap());
        let chunk = prepared.surfaces[i].commands();
        let cpu = Image::new(
            w,
            h,
            reference::rasterize(
                chunk,
                prepared.glyph_table(),
                &textures,
                w,
                h,
                [0.0, 0.0, 0.0, 1.0],
            ),
        );
        crate::shading::close(&gpu, &cpu, 3, 0.01);
    }
    frames.wait_idle().unwrap();
    ui.forget(&mut frames);
    for w in &windows {
        w.forget(&mut frames);
    }
    assert_eq!(hardware_device().validation_errors(), 0);
}

#[test]
fn external_images_come_from_the_graph_and_their_versions_redraw() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let viewport = ExternalImageId(7);
    let fonts: [FontView<'_>; 1] = [inter()];
    let mut list = UiDrawList::new(1.0);
    {
        let mut p = list.painter(&r.glyph_cache().ids);
        p.rect(
            Rect::new(0.0, 0.0, W as f32, H as f32),
            &RectStyle::fill(Color::rgb(40, 40, 40)),
        );
        p.image(
            Rect::new(32.0, 32.0, 160.0, 128.0),
            ImageSource::External {
                id: viewport,
                width: 64,
                height: 48,
                color: ImageColor::LinearStraight,
            },
            &ImageStyle {
                corners: Corners::all(6.0),
                ..ImageStyle::default()
            },
        );
    }
    let mut run = |color: [f32; 4], version: u64| {
        let prepared = r
            .renderer
            .prepare(&[&list], &[(viewport, version)], &fonts, None);
        let (image, heads) = frames
            .execute(hardware_device(), |g, _| {
                // The viewport: a render target the graph clears, as a scene renderer would draw.
                let scene =
                    g.create_texture("Scene", TextureDesc::new_2d(64, 48, Format::Rgba8UnormSrgb));
                let scene = g.add_raster("Scene", |b| {
                    let s = b.color(scene, LoadOp::Clear(color), StoreOp::Store);
                    (s, |_: &mut PassContext<'_>| {})
                });
                let pages = r.renderer.begin(g, &prepared);
                let out = r.renderer.add_passes(
                    g,
                    pages,
                    &r.surface,
                    &prepared.surfaces[0],
                    W,
                    H,
                    Color::BLACK,
                    &[(viewport, scene)],
                );
                (g.readback_texture(out.target), g.readback(out.heads))
            })
            .unwrap();
        let image = frames.wait_readback(image).unwrap();
        let heads = frames.wait_readback(heads).unwrap();
        let shaded = heads
            .as_chunks::<16>()
            .0
            .iter()
            .filter(|h| u32::from_le_bytes([h[8], h[9], h[10], h[11]]) != 0)
            .count();
        let px = |x: u32, y: u32| {
            let at = ((y * W + x) * 4) as usize;
            [image[at], image[at + 1], image[at + 2], image[at + 3]]
        };
        (px(96, 80), px(10, 10), shaded)
    };
    // Linear 0.2 green is sRGB 124.
    let (inside, outside, _) = run([0.0, 0.2, 0.0, 1.0], 1);
    assert!(inside[1].abs_diff(124) <= 1 && inside[0] <= 1, "{inside:?}");
    assert_eq!(outside, [40, 40, 40, 255]);
    // Same version: nothing shaded (the old pixels stay, whatever the texture holds now).
    let (inside, _, shaded) = run([0.2, 0.0, 0.0, 1.0], 1);
    assert_eq!(shaded, 0);
    assert!(inside[1].abs_diff(124) <= 1, "{inside:?}");
    // A new version redraws the image's tiles only: 128 × 96 from (32, 32), and the
    // half-pixel anti-aliased edge around it, touch 10 × 8.
    let (inside, _, shaded) = run([0.2, 0.0, 0.0, 1.0], 2);
    assert!(inside[0].abs_diff(124) <= 1 && inside[1] <= 1, "{inside:?}");
    assert_eq!(shaded, 10 * 8);
    frames.wait_idle().unwrap();
    r.forget(&mut frames);
    assert_eq!(hardware_device().validation_errors(), 0);
}
