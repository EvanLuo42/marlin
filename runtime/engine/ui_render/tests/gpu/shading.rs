//! The GPU's pixels against the CPU reference rasterizer (same formulas, same blending order),
//! text and images included; the encode into an sRGB target; and the sample scene's golden
//! image.

use marlin_font::FontView;
use marlin_gpu_test_support::{Image, assert_image, hardware_device, run, upload};
use marlin_math::Vec2;
use marlin_render_graph::{FrameContext, FrameContextDesc, TextureDesc};
use marlin_rhi::{
    BufferTextureCopy, BufferUsage, Format, ShaderView, Texture, TextureBarrier, TextureState,
    TextureUsage, TextureViewDesc,
};
use marlin_ui_render::{
    Color, ImageColor, ImageSource, ImageStyle, Rect, RectStyle, UiDrawList, UiRenderer,
    UiRendererDesc, reference,
};

use crate::support::{CpuTexture, CpuTextures, Pages, inter, scene};
use crate::{finish, renderer};

/// The checker of [`CpuTexture::checker`] as an `Rgba8UnormSrgb` texture and its view.
pub fn checker_texture() -> (Texture, ShaderView) {
    let device = hardware_device();
    let cpu = CpuTexture::checker();
    let desc = marlin_rhi::TextureDesc::new_2d(
        4,
        4,
        Format::Rgba8UnormSrgb,
        TextureUsage::SAMPLED | TextureUsage::COPY_DST,
    );
    let texture = device.create_texture(&desc).unwrap();
    // Rows 256 bytes apart, as copies want them.
    let mut bytes = vec![0u8; 256 * 4];
    for y in 0..4 {
        bytes[256 * y..256 * y + 16].copy_from_slice(&cpu.rgba[16 * y..16 * y + 16]);
    }
    let staging = upload(device, &bytes, BufferUsage::COPY_SRC);
    run(device, |list| {
        list.texture_barriers(&[TextureBarrier::new(
            &texture,
            TextureState::UNDEFINED,
            TextureState::COPY_DST,
        )]);
        list.copy_buffer_to_texture(
            &staging,
            &texture,
            &BufferTextureCopy::whole_mip(&desc, 0, 0, 0),
        );
        list.texture_barriers(&[TextureBarrier::new(
            &texture,
            TextureState::COPY_DST,
            TextureState::SHADER_READ,
        )]);
    });
    let view = device
        .create_texture_view(&texture, &TextureViewDesc::SAMPLED)
        .unwrap();
    (texture, view)
}

/// One frame of `list` on a `width` × `height` surface: the GPU's UI target and the reference's
/// image. `pages` keeps the glyph pages across frames, as the GPU does.
pub fn draw(
    r: &mut UiRenderer,
    frames: &mut FrameContext,
    pages: &mut Pages,
    list: &UiDrawList,
    width: u32,
    height: u32,
    clear: Color,
) -> (Image, Image) {
    let fonts: [FontView<'_>; 1] = [inter()];
    let data = r.prepare(list, &fonts, None);
    let ticket = frames
        .execute(hardware_device(), |g, _| {
            let out = r.add_passes(g, &data, width, height, clear);
            g.readback_texture(out.target)
        })
        .unwrap();
    let gpu = frames.wait_readback(ticket).unwrap();
    pages.apply(data.uploads());
    let texture = CpuTexture::checker();
    let textures = CpuTextures {
        pages,
        texture: &texture,
    };
    let clear = marlin_ui_render::shape::premultiplied(clear.packed());
    let cpu = reference::rasterize(
        data.commands(),
        data.glyph_table(),
        &textures,
        width,
        height,
        clear,
    );
    (
        Image::new(width, height, gpu),
        Image::new(width, height, cpu),
    )
}

/// Every channel within `limit`, and at most `ratio` of pixels off by more than 1.
pub fn close(gpu: &Image, cpu: &Image, limit: u8, ratio: f32) {
    let (mut worst, mut off) = (0u8, 0usize);
    let mut at = (0, 0);
    for y in 0..gpu.height {
        for x in 0..gpu.width {
            let (g, c) = (gpu.pixel(x, y), cpu.pixel(x, y));
            let d = (0..4).map(|k| g[k].abs_diff(c[k])).max().unwrap();
            if d > worst {
                worst = d;
                at = (x, y);
            }
            off += usize::from(d > 1);
        }
    }
    let n = (gpu.width * gpu.height) as usize;
    assert!(
        worst <= limit,
        "worst difference {worst} at {at:?}: GPU {:?}, reference {:?}",
        gpu.pixel(at.0, at.1),
        cpu.pixel(at.0, at.1)
    );
    assert!(
        (off as f32) <= ratio * n as f32,
        "{off} of {n} pixels off by more than 1"
    );
}

#[test]
fn the_scene_shades_like_the_reference() {
    let (_texture, view) = checker_texture();
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let (w, h) = (300, 190);
    let mut list = UiDrawList::new(1.0);
    scene(
        &mut list.painter(&r.glyph_cache().ids),
        w as f32,
        h as f32,
        view.index(),
    );
    let (gpu, cpu) = draw(&mut r, &mut frames, &mut pages, &list, w, h, Color::BLACK);
    // Rounding in the distance math and the hardware's bilinear weights: a level or two at
    // edges and in the image.
    close(&gpu, &cpu, 3, 0.01);
    assert_image!("ui_scene", &gpu);
    finish(&mut frames, &r);
}

#[test]
fn text_matches_its_rasterized_glyphs() {
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let font = inter();
    let (w, h) = (260, 120);
    // Several sizes, scales and subpixel positions; the second frame reuses resident glyphs and
    // adds new ones.
    for (frame, scale) in [(0, 1.0), (1, 1.25)] {
        let mut list = UiDrawList::new(scale);
        let mut p = list.painter(&r.glyph_cache().ids);
        p.rect(
            Rect::new(0.0, 0.0, 400.0, 400.0),
            &RectStyle::fill(Color::rgb(250, 250, 248)),
        );
        for (i, size) in [9.0, 12.0, 16.5, 24.0].into_iter().enumerate() {
            let text = if frame == 0 {
                "Hamburgefonstiv 0123"
            } else {
                "The quick brown fox €→"
            };
            let layout = marlin_font::TextLayout::new(&font, marlin_font::FontId(0), size, text);
            p.text(
                &layout,
                Vec2::new(3.3 + i as f32 * 0.25, 4.0 + i as f32 * 24.0),
                Color::rgb(20, 20, 30),
            );
        }
        let (gpu, cpu) = draw(&mut r, &mut frames, &mut pages, &list, w, h, Color::WHITE);
        // Glyph texels are loaded, not filtered: the same coverage, blended the same way.
        close(&gpu, &cpu, 1, 0.0);
    }
    finish(&mut frames, &r);
}

#[test]
fn images_sample_with_tint_corners_and_magnification() {
    let (_texture, view) = checker_texture();
    let mut r = renderer(&UiRendererDesc::default());
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let mut pages = Pages::new(r.glyph_cache().config().page_size);
    let src = ImageSource::Texture {
        view: view.index(),
        width: 4,
        height: 4,
        color: ImageColor::LinearStraight,
    };
    let mut list = UiDrawList::new(1.0);
    let mut p = list.painter(&r.glyph_cache().ids);
    p.rect(
        Rect::new(0.0, 0.0, 200.0, 100.0),
        &RectStyle::fill(Color::rgb(90, 90, 90)),
    );
    p.image(Rect::new(4.0, 4.0, 68.0, 68.0), src, &ImageStyle::default());
    p.image(
        Rect::new(72.5, 6.25, 140.0, 90.0),
        src,
        &ImageStyle {
            uv: Rect::new(0.25, 0.0, 0.75, 1.0),
            tint: Color::rgba(255, 200, 120, 160),
            corners: marlin_ui_render::Corners::all(14.0),
            sampler: None,
        },
    );
    // Smaller than the texture: mip 0 is all it has, sampled at lod > 0.
    p.image(
        Rect::new(150.0, 10.0, 152.0, 12.0),
        src,
        &ImageStyle::default(),
    );
    let (gpu, cpu) = draw(
        &mut r,
        &mut frames,
        &mut pages,
        &list,
        200,
        100,
        Color::BLACK,
    );
    // The hardware's bilinear weights have 8 fractional bits.
    close(&gpu, &cpu, 3, 0.02);
    finish(&mut frames, &r);
}

#[test]
fn encoding_into_an_srgb_target_keeps_the_pixels() {
    let mut r = renderer(&UiRendererDesc {
        output_format: Some(Format::Rgba8UnormSrgb),
        ..UiRendererDesc::default()
    });
    let mut frames = FrameContext::new(hardware_device(), &FrameContextDesc::default()).unwrap();
    let (w, h) = (300, 190);
    let mut list = UiDrawList::new(1.0);
    scene(
        &mut list.painter(&r.glyph_cache().ids),
        w as f32,
        h as f32,
        0,
    );
    let fonts: [FontView<'_>; 1] = [inter()];
    let data = r.prepare(&list, &fonts, None);
    let (ui, encoded) = frames
        .execute(hardware_device(), |g, _| {
            let out = r.add_passes(g, &data, w, h, Color::BLACK);
            let target =
                g.create_texture("Window", TextureDesc::new_2d(w, h, Format::Rgba8UnormSrgb));
            let target = r.encode(g, out.target, target);
            (g.readback_texture(out.target), g.readback_texture(target))
        })
        .unwrap();
    let ui = frames.wait_readback(ui).unwrap();
    let encoded = frames.wait_readback(encoded).unwrap();
    // Decoded to linear, then encoded by the target on write: the same sRGB values (the image
    // is opaque, so premultiplied or not makes no difference).
    let worst = ui
        .iter()
        .zip(&encoded)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(worst <= 1, "{worst}");
    finish(&mut frames, &r);
}
