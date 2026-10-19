//! The stats overlay (`r.stats`, F3): a small panel over the frame with the frame rate, the CPU
//! and GPU frame times, a graph of the last frames' intervals and the costliest GPU passes, drawn
//! by the UI renderer and blended over the frame within the panel (`UiRenderer::overlay`).
//!
//! Nothing of it exists until it is first shown: the UI renderer and the font are made then.
//! While hidden it costs nothing: no draw list, no passes, no profiler queries.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use marlin_font::{FontId, FontView, TextLayout};
use marlin_jobs::JobSystem;
use marlin_math::Vec2;
use marlin_profile::GpuZone;
use marlin_render_graph::{FrameContext, RenderGraph, TextureHandle};
use marlin_rhi::{Device, Format};
use marlin_shader::asset::ShaderLibrary;
use marlin_time::Instant;
use marlin_ui_render::{
    Color, Rect, RectStyle, UiDrawList, UiPrepared, UiRenderer, UiRendererDesc, UiShaders,
    UiSurface, UiSurfaceDesc, UiTextureDesc,
};

/// The profiler's numbers over its history (the last frames).
pub struct Snapshot {
    pub fps: f32,
    /// The mean frame, mark to mark.
    pub frame: Duration,
    /// The mean GPU frame: its first pass's start to its last pass's end.
    pub gpu: Duration,
    /// Mean time the passes ran per queue: graphics, compute, copy.
    pub busy: [Duration; 3],
    /// The costliest CPU zones and GPU passes, per frame, most first.
    pub cpu: Vec<(&'static str, Duration)>,
    pub passes: Vec<(&'static str, Duration)>,
}

impl Snapshot {
    /// Takes the profiler's history, with `fps` measured by the caller.
    pub fn take(fps: f32) -> Self {
        marlin_profile::with_history(|h| {
            let n = h.frames().len().max(1) as u32;
            let frame = h.frames().map(|f| f.duration()).sum::<Duration>() / n;
            let (mut gpu, mut busy, mut timed) = (Duration::ZERO, [Duration::ZERO; 3], 0);
            for f in h.frames() {
                let zones = f.gpu_zones();
                let (Some(start), Some(end)) = (
                    zones.iter().map(|z| z.start).min(),
                    zones.iter().map(|z| z.end).max(),
                ) else {
                    continue;
                };
                gpu += GpuZone {
                    start,
                    end,
                    ..zones[0]
                }
                .duration();
                for z in zones {
                    busy[(z.queue as usize).min(2)] += z.duration();
                }
                timed += 1;
            }
            let timed = timed.max(1);
            let top = |stats: Vec<marlin_profile::ZoneStats>| {
                stats
                    .into_iter()
                    .take(12)
                    .map(|s| (s.name, s.per_frame.avg))
                    .collect()
            };
            Self {
                fps,
                frame,
                gpu: gpu / timed,
                busy: busy.map(|b| b / timed),
                cpu: top(h.zone_stats()),
                passes: top(h.gpu_zone_stats()),
            }
        })
    }
}

/// What the panel shows besides the profiler's numbers.
pub struct HudInfo<'a> {
    pub adapter: &'a str,
    /// The window, pixels.
    pub width: u32,
    pub height: u32,
    pub vsync: bool,
    /// The last frame's graph: passes and command lists.
    pub passes: usize,
    pub lists: usize,
}

/// Frame intervals the graph shows.
const INTERVALS: usize = 120;
/// The panel's width, its margin from the window's corner and its padding, logical pixels.
const WIDTH: f32 = 264.0;
const MARGIN: f32 = 10.0;
const PAD: f32 = 10.0;
const TEXT: f32 = 13.0;
const SMALL: f32 = 11.5;
const LINE: f32 = 17.0;
const GRAPH: f32 = 44.0;
/// GPU passes listed.
const PASSES: usize = 6;

const PANEL: Color = Color::rgba(14, 16, 20, 214);
const RULE: Color = Color::rgba(255, 255, 255, 28);
const LABEL: Color = Color::rgb(160, 166, 176);
const VALUE: Color = Color::rgb(236, 238, 241);
const GOOD: Color = Color::rgb(118, 214, 128);
const BAR: Color = Color::rgb(92, 160, 255);
const SPIKE: Color = Color::rgb(255, 150, 90);

/// How often the panel is drawn anew; the frames between blend the last drawing.
pub const REDRAW: Duration = Duration::from_millis(100);

/// The panel's texture: the window's top-left corner down to the panel's bottom-right, so its
/// texels are the window's pixels there.
struct Panel {
    surface: UiSurface,
    width: u32,
    height: u32,
}

/// The overlay's renderer, its panel and the numbers it shows.
pub struct Hud {
    ui: UiRenderer,
    panel: Option<Panel>,
    /// The last frames' intervals, seconds, oldest first.
    intervals: VecDeque<f32>,
    snapshot: Option<Snapshot>,
    /// When the panel was last drawn, and how often it is drawn anew.
    drawn: Option<Instant>,
    redraw: Duration,
}

/// One frame of the overlay: the panel's drawing when it is drawn anew this frame.
pub struct HudFrame {
    prepared: Option<UiPrepared>,
}

impl Hud {
    /// Its renderer from the engine's shaders (`Marlin.Engine.Ui`), drawing over `output`.
    pub fn new(device: &Device, library: &ShaderLibrary, output: Format) -> Result<Self, HudError> {
        let shaders = UiShaders::bind(library)?;
        let ui = UiRenderer::new(
            device,
            &shaders,
            &UiRendererDesc {
                output_format: Some(output),
                ..UiRendererDesc::default()
            },
        )?;
        Ok(Self {
            ui,
            panel: None,
            intervals: VecDeque::with_capacity(INTERVALS),
            snapshot: None,
            drawn: None,
            redraw: REDRAW,
        })
    }

    /// One frame's interval, seconds.
    pub fn push_interval(&mut self, seconds: f32) {
        if self.intervals.len() == INTERVALS {
            self.intervals.pop_front();
        }
        self.intervals.push_back(seconds);
    }

    /// How often the panel is drawn anew ([`REDRAW`] by default); the frames between blend the
    /// last drawing.
    pub fn set_redraw_interval(&mut self, every: Duration) {
        self.redraw = every;
    }

    /// The numbers the panel shows until the next.
    pub fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.snapshot = Some(snapshot);
    }

    /// This frame of the overlay, for a window `info.width` × `info.height` at `scale` (pixels
    /// per logical pixel): the panel drawn anew in `font` when [`REDRAW`] has passed (or the
    /// window's size or scale changed its texture, which waits for `frames` to go idle).
    pub fn frame(
        &mut self,
        device: &Device,
        frames: &mut FrameContext,
        font: FontView<'_>,
        scale: f32,
        info: &HudInfo<'_>,
        jobs: Option<&JobSystem>,
    ) -> Result<HudFrame, marlin_rhi::Error> {
        let px = |v: f32| marlin_math::scalar::ceil(v * scale) as u32;
        let (width, height) = (
            px(MARGIN + WIDTH + 1.0).min(info.width).max(1),
            px(MARGIN + panel_height() + 1.0).min(info.height).max(1),
        );
        if self
            .panel
            .as_ref()
            .is_none_or(|p| (p.width, p.height) != (width, height))
        {
            if let Some(old) = self.panel.take() {
                frames.wait_idle()?;
                old.surface.forget(frames);
            }
            let surface = UiSurface::new_texture(
                device,
                &UiSurfaceDesc::new(HUD_SURFACE),
                &UiTextureDesc {
                    width,
                    height,
                    mips: false,
                },
            )?;
            self.panel = Some(Panel {
                surface,
                width,
                height,
            });
            self.drawn = None;
        }
        let now = Instant::now();
        if self
            .drawn
            .is_some_and(|t| now.duration_since(t) < self.redraw)
        {
            return Ok(HudFrame { prepared: None });
        }
        self.drawn = Some(now);
        let mut list = UiDrawList::new(scale);
        let fonts = [font];
        {
            let ids = &self.ui.glyph_cache().ids;
            let mut p = list.painter(ids).with_fonts(&fonts);
            self.draw(&mut p, &fonts[0], info);
        }
        Ok(HudFrame {
            prepared: Some(self.ui.prepare(&[&list], &[], &fonts, jobs)),
        })
    }

    /// Declares `frame` over `target` (the window's image): the panel's passes when it is
    /// drawn anew, then the panel blended over the window's corner. Returns `target` after it.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        frame: &'g HudFrame,
        target: TextureHandle<'g>,
    ) -> TextureHandle<'g> {
        let Some(panel) = &self.panel else {
            return target;
        };
        let texture = panel.surface.texture().expect("a texture surface");
        let drawn = match &frame.prepared {
            Some(prepared) => {
                let pages = self.ui.begin(g, prepared);
                self.ui
                    .add_passes(
                        g,
                        pages,
                        &panel.surface,
                        &prepared.surfaces[0],
                        panel.width,
                        panel.height,
                        Color::TRANSPARENT,
                        &[],
                    )
                    .target
            }
            None => g.import_texture("HudPanel", texture),
        };
        let rect = marlin_rhi::Rect {
            x: 0,
            y: 0,
            width: panel.width,
            height: panel.height,
        };
        self.ui.overlay(g, drawn, target, rect)
    }

    /// Makes `frames` forget the overlay's resources; call before dropping it.
    pub fn forget(&self, frames: &mut FrameContext) {
        self.ui.forget(frames);
        if let Some(p) = &self.panel {
            p.surface.forget(frames);
        }
    }

    /// Paints the panel.
    fn draw(&self, p: &mut marlin_ui_render::Painter<'_>, font: &FontView<'_>, info: &HudInfo<'_>) {
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let s = self.snapshot.as_ref();
        let mut rows: Vec<Row> = Vec::new();
        rows.push(Row::Title(String::from(info.adapter)));
        rows.push(Row::Note(format!(
            "Vulkan  ·  {}×{}  ·  {}",
            info.width,
            info.height,
            if info.vsync { "vsync" } else { "no vsync" }
        )));
        rows.push(Row::Rule);
        let value = |v: Option<String>| v.unwrap_or_else(|| String::from("–"));
        rows.push(Row::Value(
            "FPS",
            value(s.map(|s| format!("{:.0}", s.fps))),
            GOOD,
        ));
        rows.push(Row::Value(
            "Frame",
            value(s.map(|s| format!("{:.2} ms", ms(s.frame)))),
            VALUE,
        ));
        rows.push(Row::Value(
            "GPU",
            value(s.map(|s| format!("{:.2} ms", ms(s.gpu)))),
            VALUE,
        ));
        rows.push(Row::Value(
            "GPU busy",
            value(s.map(|s| {
                let mut text = format!("{:.2} ms", ms(s.busy[0]));
                if !s.busy[1].is_zero() {
                    text += &format!("  + {:.2} async", ms(s.busy[1]));
                }
                text
            })),
            VALUE,
        ));
        for (label, zone) in [("CPU render", "Render"), ("CPU simulate", "Simulate")] {
            rows.push(Row::Value(
                label,
                value(s.and_then(|s| {
                    s.cpu
                        .iter()
                        .find(|(n, _)| *n == zone)
                        .map(|(_, d)| format!("{:.2} ms", ms(*d)))
                })),
                VALUE,
            ));
        }
        rows.push(Row::Graph);
        rows.push(Row::Rule);
        rows.push(Row::Note(format!(
            "{} passes  ·  {} command lists",
            info.passes, info.lists
        )));
        // Always as many rows, so the panel keeps its size.
        let passes = s.map_or(&[][..], |s| &s.passes[..]);
        for i in 0..PASSES {
            rows.push(match passes.get(i) {
                Some((name, d)) => Row::Value(name, format!("{:.3} ms", ms(*d)), LABEL),
                None => Row::Value("", String::new(), LABEL),
            });
        }
        debug_assert_eq!(rows.iter().map(Row::height).sum::<f32>(), ROWS);
        let height = panel_height();
        let (x0, y0) = (MARGIN, MARGIN);
        let (left, right) = (x0 + PAD, x0 + WIDTH - PAD);
        p.rect(
            Rect::new(x0, y0, x0 + WIDTH, y0 + height),
            &RectStyle::rounded(PANEL, 6.0),
        );
        let text = |t: &str, size: f32| TextLayout::new(font, FontId(0), size, t);
        let mut y = y0 + PAD;
        for row in &rows {
            match row {
                Row::Title(t) => {
                    let l = text(t, TEXT);
                    p.text(&l, Vec2::new(left, y + 1.0), VALUE);
                }
                Row::Note(t) => {
                    let l = text(t, SMALL);
                    p.text(&l, Vec2::new(left, y + 1.0), LABEL);
                }
                Row::Rule => {
                    let r = y + row.height() * 0.5;
                    p.rect(Rect::new(left, r, right, r + 1.0), &RectStyle::fill(RULE));
                }
                Row::Value(label, v, color) => {
                    let l = text(label, TEXT);
                    p.text(&l, Vec2::new(left, y), LABEL);
                    let v = text(v, TEXT);
                    p.text(&v, Vec2::new(right - v.width, y), *color);
                }
                Row::Graph => {
                    let bars = Rect::new(left, y + LINE, right, y + LINE + GRAPH);
                    self.graph(p, font, bars);
                }
            }
            y += row.height();
        }
    }

    /// The frame intervals as bars in `r`, newest on the right, against a scale that fits them.
    fn graph(&self, p: &mut marlin_ui_render::Painter<'_>, font: &FontView<'_>, r: Rect) {
        p.rect(r, &RectStyle::fill(Color::rgba(255, 255, 255, 10)));
        let worst = self.intervals.iter().copied().fold(0.0f32, f32::max) * 1e3;
        let scale = [1.0f32, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0]
            .into_iter()
            .find(|&s| s >= worst)
            .unwrap_or(100.0);
        let mean = self.intervals.iter().sum::<f32>() * 1e3 / self.intervals.len().max(1) as f32;
        let w = (r.max.x - r.min.x) / INTERVALS as f32;
        let first = INTERVALS - self.intervals.len();
        for (i, &t) in self.intervals.iter().enumerate() {
            let ms = t * 1e3;
            let h = (ms / scale).min(1.0) * (r.max.y - r.min.y);
            let x = r.min.x + (first + i) as f32 * w;
            // Twice the mean is a hitch.
            let color = if ms > 2.0 * mean { SPIKE } else { BAR };
            p.rect(
                Rect::new(x, r.max.y - h.max(1.0), x + (w - 0.5).max(0.5), r.max.y),
                &RectStyle::fill(color),
            );
        }
        // The header above: what the bars are, and the scale.
        let title = TextLayout::new(font, FontId(0), SMALL, "Frame interval");
        p.text(&title, Vec2::new(r.min.x, r.min.y - LINE + 2.0), LABEL);
        let label = TextLayout::new(font, FontId(0), SMALL, &format!("{scale:.0} ms"));
        p.text(
            &label,
            Vec2::new(r.max.x - label.width, r.min.y - LINE + 2.0),
            LABEL,
        );
    }
}

/// One row of the panel.
enum Row {
    Title(String),
    Note(String),
    Rule,
    /// A label, its value right-aligned, the value's color.
    Value(&'static str, String, Color),
    Graph,
}

impl Row {
    fn height(&self) -> f32 {
        match self {
            Self::Rule => 9.0,
            Self::Graph => LINE + GRAPH + 6.0,
            _ => LINE,
        }
    }
}

/// The rows' heights: two lines of header, a rule, six values, the graph, a rule, a line and the
/// passes.
const ROWS: f32 = (3.0 + 6.0 + PASSES as f32) * LINE + 2.0 * 9.0 + (LINE + GRAPH + 6.0);

/// The panel's height, logical pixels.
const fn panel_height() -> f32 {
    ROWS + 2.0 * PAD
}

/// The overlay surface's key among the frame's UI surfaces.
const HUD_SURFACE: u64 = 0x4855_445f_5354_4154;

/// Why the overlay could not be made.
#[derive(Debug, marlin_error::Error)]
pub enum HudError {
    #[error("UI shaders")]
    Shaders(#[from] marlin_shader::bind::BindError),
    #[error("gpu")]
    Gpu(#[from] marlin_rhi::Error),
}
