//! Shared by marlin-ui-render's test binaries: the engine's Inter, glyph pages kept on the CPU,
//! a scene with every kind of command, and a CPU texture.

#![allow(dead_code)]

use std::sync::OnceLock;

use marlin_font::{FontId, FontView, GlyphCache, GlyphUpload, TextLayout};
use marlin_math::Vec2;
use marlin_ui_render::shape::Textures;
use marlin_ui_render::{
    Color, Corners, Dash, ImageColor, ImageSource, ImageStyle, LineCap, Painter, Rect, RectStyle,
    ShapeStyle, Stroke, StrokeStyle,
};

pub fn inter_payload() -> &'static [u8] {
    static PAYLOAD: OnceLock<Vec<u8>> = OnceLock::new();
    PAYLOAD.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../content/engine/fonts/Inter-Regular.ttf");
        let file = std::fs::read(path)
            .expect("content/engine/fonts/Inter-Regular.ttf (Git LFS: run `git lfs pull`)");
        marlin_cook_font::cook_font(&file, 0).unwrap()
    })
}

pub fn inter() -> FontView<'static> {
    FontView::new(inter_payload()).unwrap()
}

/// Glyph pages as the GPU holds them, from the cache's uploads.
pub struct Pages {
    pub size: u32,
    pub pages: Vec<Vec<u8>>,
}

impl Pages {
    pub fn new(size: u32) -> Self {
        Self {
            size,
            pages: Vec::new(),
        }
    }

    pub fn apply(&mut self, uploads: &[GlyphUpload]) {
        for u in uploads {
            while self.pages.len() <= u.page as usize {
                self.pages.push(vec![0; (self.size * self.size) as usize]);
            }
            let page = &mut self.pages[u.page as usize];
            for y in 0..u.rect.height {
                for x in 0..u.rect.width {
                    page[((u.rect.y + y) * self.size + u.rect.x + x) as usize] =
                        u.coverage[(y * u.rect.width + x) as usize];
                }
            }
        }
    }
}

/// A CPU texture: RGBA8 texels sampled as linear values (an sRGB view) or as stored.
pub struct CpuTexture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Decode sRGB like an `…Srgb` view.
    pub srgb: bool,
}

impl CpuTexture {
    /// A 4 × 4 checker of two colors with a transparent corner texel.
    pub fn checker() -> Self {
        let mut rgba = Vec::new();
        for y in 0..4u32 {
            for x in 0..4u32 {
                let c: [u8; 4] = if (x + y) % 2 == 0 {
                    [230, 60, 40, 255]
                } else {
                    [40, 90, 220, 255]
                };
                rgba.extend_from_slice(if x == 3 && y == 0 { &[0, 0, 0, 0] } else { &c });
            }
        }
        Self {
            width: 4,
            height: 4,
            rgba,
            srgb: true,
        }
    }

    fn texel(&self, x: i32, y: i32) -> [f32; 4] {
        let x = x.clamp(0, self.width as i32 - 1) as u32;
        let y = y.clamp(0, self.height as i32 - 1) as u32;
        let at = ((y * self.width + x) * 4) as usize;
        let mut t = [0.0; 4];
        for k in 0..4 {
            let v = self.rgba[at + k] as f32 / 255.0;
            t[k] = if self.srgb && k < 3 {
                marlin_ui_render::shape::srgb_to_linear(v)
            } else {
                v
            };
        }
        t
    }

    /// Bilinear, clamped, mip 0.
    pub fn bilinear(&self, uv: [f32; 2]) -> [f32; 4] {
        let x = uv[0] * self.width as f32 - 0.5;
        let y = uv[1] * self.height as f32 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let mut out = [0.0; 4];
        let corners = [
            (self.texel(x0, y0), (1.0 - fx) * (1.0 - fy)),
            (self.texel(x0 + 1, y0), fx * (1.0 - fy)),
            (self.texel(x0, y0 + 1), (1.0 - fx) * fy),
            (self.texel(x0 + 1, y0 + 1), fx * fy),
        ];
        for (t, w) in corners {
            for k in 0..4 {
                out[k] += t[k] * w;
            }
        }
        out
    }
}

/// Glyph pages and one texture (view [`TEXTURE_VIEW`]).
pub struct CpuTextures<'a> {
    pub pages: &'a Pages,
    pub texture: &'a CpuTexture,
}

pub const TEXTURE_VIEW: u32 = 7;

impl Textures for CpuTextures<'_> {
    fn glyph(&self, page: u32, x: u32, y: u32) -> f32 {
        self.pages.pages.get(page as usize).map_or(0.0, |p| {
            p[(y * self.pages.size + x) as usize] as f32 / 255.0
        })
    }

    fn sample(&self, view: u32, _sampler: u32, uv: [f32; 2], _lod: f32) -> [f32; 4] {
        // Every image the tests draw is the checker, whatever its view index.
        let _ = view;
        self.texture.bilinear(uv)
    }
}

/// A scene with every kind of command on a `width` × `height` surface: an opaque background,
/// panels, rounded and outlined rectangles, circles, lines of every cap and a dashed one,
/// triangles, text and an image.
pub fn scene(p: &mut Painter<'_>, width: f32, height: f32, texture_view: u32) {
    let font = inter();
    p.rect(
        Rect::new(0.0, 0.0, width, height),
        &RectStyle::fill(Color::rgb(28, 30, 34)),
    );
    p.rect(
        Rect::new(8.0, 8.0, width * 0.5 - 4.0, height - 8.0),
        &RectStyle {
            fill: Color::rgb(44, 47, 54),
            corners: Corners::all(6.0),
            stroke: Some(Stroke {
                width: 1.0,
                color: Color::rgb(70, 74, 84),
            }),
        },
    );
    p.rect(
        Rect::new(20.5, 20.25, 90.0, 52.0),
        &RectStyle {
            fill: Color::rgba(90, 160, 250, 200),
            corners: Corners {
                top_left: 0.0,
                top_right: 10.0,
                bottom_right: 3.0,
                bottom_left: 16.0,
            },
            stroke: Some(Stroke {
                width: 2.5,
                color: Color::rgba(255, 255, 255, 180),
            }),
        },
    );
    p.circle(
        Vec2::new(60.0, 90.0),
        22.3,
        &ShapeStyle::fill(Color::rgb(240, 180, 60)),
    );
    p.circle(
        Vec2::new(75.0, 100.0),
        14.0,
        &ShapeStyle {
            fill: Color::rgba(200, 60, 120, 120),
            stroke: Some(Stroke {
                width: 3.0,
                color: Color::rgb(250, 250, 250),
            }),
        },
    );
    for (i, cap) in [LineCap::Butt, LineCap::Round, LineCap::Square]
        .into_iter()
        .enumerate()
    {
        let y = 130.0 + i as f32 * 9.0;
        p.line(
            Vec2::new(20.0, y),
            Vec2::new(100.0, y + 3.5),
            &StrokeStyle {
                width: 4.0,
                color: Color::rgb(120, 220, 140),
                cap,
                dash: None,
            },
        );
    }
    p.line(
        Vec2::new(12.0, 175.0),
        Vec2::new(width - 12.0, 160.0),
        &StrokeStyle {
            width: 2.0,
            color: Color::rgba(255, 120, 80, 230),
            cap: LineCap::Round,
            dash: Some(Dash {
                dash: 9.0,
                gap: 5.0,
                phase: 2.0,
            }),
        },
    );
    // A long thin diagonal: binned only where it passes.
    p.line(
        Vec2::new(0.0, 0.0),
        Vec2::new(width, height),
        &StrokeStyle::new(1.0, Color::rgba(255, 255, 255, 90)),
    );
    p.triangle(
        [
            Vec2::new(130.0, 30.0),
            Vec2::new(180.0, 50.0),
            Vec2::new(140.0, 95.0),
        ],
        &ShapeStyle {
            fill: Color::rgb(100, 200, 230),
            stroke: Some(Stroke {
                width: 1.5,
                color: Color::rgb(20, 40, 60),
            }),
        },
    );
    p.triangle(
        [
            Vec2::new(200.0, 110.0),
            Vec2::new(150.0, 100.0),
            Vec2::new(170.0, 140.0),
        ],
        &ShapeStyle::fill(Color::rgba(250, 250, 120, 160)),
    );
    let title = TextLayout::new(
        &font,
        FontId(0),
        15.0,
        "Marlin UI — glyphs\nquick brown fox",
    );
    p.text(
        &title,
        Vec2::new(width * 0.5 + 6.0, 14.0),
        Color::rgb(235, 235, 240),
    );
    let small = TextLayout::new(&font, FontId(0), 11.0, "0123456789 ,.;:!?");
    p.text(
        &small,
        Vec2::new(width * 0.5 + 6.3, 60.0),
        Color::rgba(200, 210, 255, 220),
    );
    p.image(
        Rect::new(width * 0.5 + 10.0, 90.0, width * 0.5 + 74.0, 154.0),
        ImageSource::Texture {
            view: texture_view,
            width: 4,
            height: 4,
            color: ImageColor::LinearStraight,
        },
        &ImageStyle {
            corners: Corners::all(8.0),
            tint: Color::rgba(255, 255, 255, 230),
            ..ImageStyle::default()
        },
    );
}

/// Every glyph id `chunk`'s commands draw.
pub fn glyph_ids(chunk: &marlin_ui_render::command::Chunk) -> Vec<marlin_font::GlyphId> {
    use marlin_ui_render::command::Kind;
    chunk
        .commands
        .iter()
        .filter(|c| Kind::from_word(c[0]) == Some(Kind::Glyph))
        .map(|c| marlin_font::GlyphId(chunk.payload[c[3] as usize + 2]))
        .collect()
}

/// A cache with `chunk`'s glyphs resident, and their pages.
pub fn prepared(
    cache: &mut GlyphCache,
    chunk: &marlin_ui_render::command::Chunk,
    pages: &mut Pages,
) {
    cache.prepare(&[inter()], &glyph_ids(chunk), None);
    pages.apply(&cache.take_uploads());
}
