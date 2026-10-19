//! The editor's window shell, drawn with the UI renderer: menu bar, toolbar, the outliner,
//! the details panel, the content browser, the viewport's place and the status bar. Only the
//! drawing: the widgets, layout and docking of `marlin-editor-ui-core` replace these fixed
//! rectangles when they arrive.

use marlin_font::{FontId, FontView, TextLayout};
use marlin_math::Vec2;
use marlin_ui_render::{
    Color, Corners, Dash, LineCap, Painter, Rect, RectStyle, ShapeStyle, Stroke, StrokeStyle,
};

const BACKGROUND: Color = Color::rgb(30, 31, 34);
const PANEL: Color = Color::rgb(43, 45, 49);
const HEADER: Color = Color::rgb(36, 37, 41);
const BORDER: Color = Color::rgb(58, 60, 66);
const TEXT: Color = Color::rgb(222, 224, 228);
const DIM: Color = Color::rgb(150, 154, 162);
const ACCENT: Color = Color::rgb(76, 140, 245);
const HOVER: Color = Color::rgba(255, 255, 255, 22);

const MENU_HEIGHT: f32 = 28.0;
const TOOLBAR_HEIGHT: f32 = 38.0;
const STATUS_HEIGHT: f32 = 24.0;
const LEFT_WIDTH: f32 = 260.0;
const RIGHT_WIDTH: f32 = 300.0;
const BOTTOM_HEIGHT: f32 = 210.0;
const ROW: f32 = 24.0;

/// What the shell shows that changes from frame to frame.
pub struct ShellState<'a> {
    /// The window's size in logical pixels.
    pub size: Vec2,
    /// The cursor in logical pixels, if it is over the window.
    pub cursor: Option<Vec2>,
    pub status: &'a str,
}

fn contains(r: Rect, p: Option<Vec2>) -> bool {
    p.is_some_and(|p| p.x >= r.min.x && p.x < r.max.x && p.y >= r.min.y && p.y < r.max.y)
}

struct Shell<'p, 'a> {
    p: &'p mut Painter<'a>,
    font: FontView<'p>,
    cursor: Option<Vec2>,
}

impl Shell<'_, '_> {
    fn text(&mut self, text: &str, at: Vec2, size: f32, color: Color) -> f32 {
        let layout = TextLayout::new(&self.font, FontId(0), size, text);
        self.p.text(&layout, at, color);
        layout.width
    }

    fn panel(&mut self, r: Rect, title: &str) -> Rect {
        self.p.rect(r, &RectStyle::fill(PANEL));
        let header = Rect::new(r.min.x, r.min.y, r.max.x, r.min.y + ROW + 4.0);
        self.p.rect(header, &RectStyle::fill(HEADER));
        self.text(title, Vec2::new(r.min.x + 10.0, r.min.y + 6.0), 13.0, TEXT);
        // The active tab's underline.
        self.p.line(
            Vec2::new(r.min.x + 8.0, header.max.y - 1.0),
            Vec2::new(
                r.min.x + 16.0 + title.len() as f32 * 7.0,
                header.max.y - 1.0,
            ),
            &StrokeStyle {
                width: 2.0,
                color: ACCENT,
                cap: LineCap::Round,
                dash: None,
            },
        );
        self.p.line(
            Vec2::new(r.min.x, r.min.y),
            Vec2::new(r.max.x, r.min.y),
            &StrokeStyle::new(1.0, BORDER),
        );
        Rect::new(r.min.x, header.max.y, r.max.x, r.max.y)
    }

    fn hover(&mut self, r: Rect, radius: f32) {
        if contains(r, self.cursor) {
            self.p.rect(r, &RectStyle::rounded(HOVER, radius));
        }
    }

    fn menu(&mut self, width: f32) {
        self.p.rect(
            Rect::new(0.0, 0.0, width, MENU_HEIGHT),
            &RectStyle::fill(HEADER),
        );
        // The logo: a marlin-blue disc with a fin.
        self.p
            .circle(Vec2::new(16.0, 14.0), 8.0, &ShapeStyle::fill(ACCENT));
        self.p.triangle(
            [
                Vec2::new(12.0, 18.0),
                Vec2::new(17.5, 6.5),
                Vec2::new(21.0, 18.0),
            ],
            &ShapeStyle::fill(Color::WHITE),
        );
        let mut x = 34.0;
        for item in ["File", "Edit", "View", "Window", "Tools", "Help"] {
            let w = TextLayout::new(&self.font, FontId(0), 13.0, item).width;
            self.hover(Rect::new(x - 6.0, 4.0, x + w + 6.0, MENU_HEIGHT - 4.0), 4.0);
            self.text(item, Vec2::new(x, 6.0), 13.0, TEXT);
            x += w + 18.0;
        }
        self.text(
            "Marlin Editor",
            Vec2::new(width * 0.5 - 40.0, 6.0),
            13.0,
            DIM,
        );
    }

    fn toolbar(&mut self, width: f32) {
        let y0 = MENU_HEIGHT;
        let bar = Rect::new(0.0, y0, width, y0 + TOOLBAR_HEIGHT);
        self.p.rect(bar, &RectStyle::fill(BACKGROUND));
        let cy = y0 + TOOLBAR_HEIGHT * 0.5;
        // Select, move, rotate, scale: a group of tool buttons.
        let mut x = 10.0;
        for (i, label) in ["Select", "Move", "Rotate", "Scale"]
            .into_iter()
            .enumerate()
        {
            let w = TextLayout::new(&self.font, FontId(0), 12.0, label).width + 20.0;
            let r = Rect::new(x, cy - 12.0, x + w, cy + 12.0);
            let style = if i == 0 {
                RectStyle {
                    fill: Color::rgba(76, 140, 245, 60),
                    corners: Corners::all(5.0),
                    stroke: Some(Stroke {
                        width: 1.0,
                        color: ACCENT,
                    }),
                }
            } else {
                RectStyle::rounded(PANEL, 5.0)
            };
            self.p.rect(r, &style);
            self.hover(r, 5.0);
            self.text(label, Vec2::new(x + 10.0, cy - 8.0), 12.0, TEXT);
            x += w + 6.0;
        }
        // Play, pause, stop in the middle.
        let mid = width * 0.5;
        let play = Rect::new(mid - 52.0, cy - 13.0, mid - 22.0, cy + 13.0);
        self.p.rect(play, &RectStyle::rounded(PANEL, 5.0));
        self.hover(play, 5.0);
        self.p.triangle(
            [
                Vec2::new(mid - 42.0, cy - 7.0),
                Vec2::new(mid - 30.0, cy),
                Vec2::new(mid - 42.0, cy + 7.0),
            ],
            &ShapeStyle::fill(Color::rgb(90, 200, 120)),
        );
        let pause = Rect::new(mid - 15.0, cy - 13.0, mid + 15.0, cy + 13.0);
        self.p.rect(pause, &RectStyle::rounded(PANEL, 5.0));
        self.hover(pause, 5.0);
        for dx in [-5.0, 2.0] {
            self.p.rect(
                Rect::new(mid + dx, cy - 6.0, mid + dx + 3.5, cy + 6.0),
                &RectStyle::rounded(TEXT, 1.0),
            );
        }
        let stop = Rect::new(mid + 22.0, cy - 13.0, mid + 52.0, cy + 13.0);
        self.p.rect(stop, &RectStyle::rounded(PANEL, 5.0));
        self.hover(stop, 5.0);
        self.p.rect(
            Rect::new(mid + 31.0, cy - 6.0, mid + 43.0, cy + 6.0),
            &RectStyle::rounded(Color::rgb(230, 90, 80), 2.0),
        );
        self.p.line(
            Vec2::new(0.0, bar.max.y),
            Vec2::new(width, bar.max.y),
            &StrokeStyle::new(1.0, BORDER),
        );
    }

    fn outliner(&mut self, r: Rect) {
        let body = self.panel(r, "Outliner");
        let items: [(u32, &str, Color); 9] = [
            (0, "World", Color::rgb(200, 200, 210)),
            (1, "Sun", Color::rgb(250, 200, 80)),
            (1, "Sky Atmosphere", Color::rgb(110, 170, 250)),
            (1, "Ground", Color::rgb(160, 160, 160)),
            (1, "Spinning Torus", Color::rgb(240, 140, 70)),
            (1, "Cube Ring", Color::rgb(140, 220, 150)),
            (2, "Cube 0", Color::rgb(140, 220, 150)),
            (2, "Cube 1", Color::rgb(140, 220, 150)),
            (1, "Fly Camera", Color::rgb(190, 150, 240)),
        ];
        for (i, (depth, name, color)) in items.into_iter().enumerate() {
            let y = body.min.y + 4.0 + i as f32 * ROW;
            let row = Rect::new(body.min.x + 4.0, y, body.max.x - 4.0, y + ROW);
            if i == 4 {
                self.p
                    .rect(row, &RectStyle::rounded(Color::rgba(76, 140, 245, 90), 4.0));
            } else {
                self.hover(row, 4.0);
            }
            let x = body.min.x + 12.0 + depth as f32 * 16.0;
            let cy = y + ROW * 0.5;
            if i == 0 || i == 5 {
                // An expanded node's arrow.
                self.p.triangle(
                    [
                        Vec2::new(x, cy - 3.0),
                        Vec2::new(x + 8.0, cy - 3.0),
                        Vec2::new(x + 4.0, cy + 3.0),
                    ],
                    &ShapeStyle::fill(DIM),
                );
            }
            self.p
                .circle(Vec2::new(x + 18.0, cy), 5.0, &ShapeStyle::fill(color));
            self.text(name, Vec2::new(x + 30.0, y + 5.0), 13.0, TEXT);
        }
    }

    fn details(&mut self, r: Rect) {
        let body = self.panel(r, "Details");
        let label_x = body.min.x + 12.0;
        let value_x = body.min.x + 110.0;
        let mut y = body.min.y + 10.0;
        self.text("Spinning Torus", Vec2::new(label_x, y), 15.0, TEXT);
        y += 30.0;
        for (label, values) in [
            ("Location", ["0.0", "1.2", "-3.5"]),
            ("Rotation", ["0°", "45°", "0°"]),
            ("Scale", ["1.0", "1.0", "1.0"]),
        ] {
            self.text(label, Vec2::new(label_x, y + 3.0), 12.0, DIM);
            let w = (body.max.x - value_x - 12.0) / 3.0;
            for (k, v) in values.into_iter().enumerate() {
                let x = value_x + k as f32 * w;
                let field = Rect::new(x, y, x + w - 4.0, y + 20.0);
                self.p.rect(
                    field,
                    &RectStyle {
                        fill: BACKGROUND,
                        corners: Corners::all(3.0),
                        stroke: Some(Stroke {
                            width: 1.0,
                            color: BORDER,
                        }),
                    },
                );
                // The axis color on the field's left edge.
                let axis = [
                    Color::rgb(230, 80, 80),
                    Color::rgb(110, 200, 90),
                    Color::rgb(80, 130, 240),
                ][k];
                self.p.rect(
                    Rect::new(x, y, x + 3.0, y + 20.0),
                    &RectStyle {
                        fill: axis,
                        corners: Corners {
                            top_left: 3.0,
                            bottom_left: 3.0,
                            ..Corners::ZERO
                        },
                        stroke: None,
                    },
                );
                self.hover(field, 3.0);
                self.text(v, Vec2::new(x + 8.0, y + 3.0), 12.0, TEXT);
            }
            y += 28.0;
        }
        y += 6.0;
        // A color, a checkbox and a slider.
        self.text("Color", Vec2::new(label_x, y + 3.0), 12.0, DIM);
        self.p.rect(
            Rect::new(value_x, y, value_x + 60.0, y + 20.0),
            &RectStyle {
                fill: Color::rgb(240, 140, 70),
                corners: Corners::all(3.0),
                stroke: Some(Stroke {
                    width: 1.0,
                    color: BORDER,
                }),
            },
        );
        y += 28.0;
        self.text("Visible", Vec2::new(label_x, y + 3.0), 12.0, DIM);
        let check = Rect::new(value_x, y + 2.0, value_x + 16.0, y + 18.0);
        self.p.rect(check, &RectStyle::rounded(ACCENT, 3.0));
        let tick = StrokeStyle {
            width: 2.0,
            color: Color::WHITE,
            cap: LineCap::Round,
            dash: None,
        };
        self.p.line(
            Vec2::new(value_x + 4.0, y + 10.0),
            Vec2::new(value_x + 7.0, y + 13.5),
            &tick,
        );
        self.p.line(
            Vec2::new(value_x + 7.0, y + 13.5),
            Vec2::new(value_x + 12.5, y + 6.0),
            &tick,
        );
        y += 28.0;
        self.text("Spin speed", Vec2::new(label_x, y + 3.0), 12.0, DIM);
        let (s0, s1) = (value_x, body.max.x - 50.0);
        let knob = s0 + (s1 - s0) * 0.35;
        self.p.line(
            Vec2::new(s0, y + 10.0),
            Vec2::new(s1, y + 10.0),
            &StrokeStyle {
                width: 4.0,
                color: BORDER,
                cap: LineCap::Round,
                dash: None,
            },
        );
        self.p.line(
            Vec2::new(s0, y + 10.0),
            Vec2::new(knob, y + 10.0),
            &StrokeStyle {
                width: 4.0,
                color: ACCENT,
                cap: LineCap::Round,
                dash: None,
            },
        );
        self.p.circle(
            Vec2::new(knob, y + 10.0),
            7.0,
            &ShapeStyle {
                fill: Color::WHITE,
                stroke: Some(Stroke {
                    width: 1.0,
                    color: BORDER,
                }),
            },
        );
        self.text("0.35", Vec2::new(s1 + 10.0, y + 3.0), 12.0, TEXT);
    }

    fn content_browser(&mut self, r: Rect) {
        let body = self.panel(r, "Content Browser");
        let assets: [(&str, Color); 7] = [
            ("checker", Color::rgb(200, 200, 200)),
            ("cube", Color::rgb(140, 220, 150)),
            ("sphere", Color::rgb(240, 140, 70)),
            ("torus", Color::rgb(240, 180, 90)),
            ("main", Color::rgb(110, 170, 250)),
            ("Inter", Color::rgb(190, 150, 240)),
            ("Ui", Color::rgb(76, 140, 245)),
        ];
        let tile = 96.0;
        for (i, (name, color)) in assets.into_iter().enumerate() {
            let x = body.min.x + 12.0 + i as f32 * (tile + 12.0);
            if x + tile > body.max.x {
                break;
            }
            let y = body.min.y + 12.0;
            let card = Rect::new(x, y, x + tile, y + tile + 26.0);
            self.p.rect(
                card,
                &RectStyle {
                    fill: BACKGROUND,
                    corners: Corners::all(6.0),
                    stroke: Some(Stroke {
                        width: 1.0,
                        color: BORDER,
                    }),
                },
            );
            self.hover(card, 6.0);
            let thumb = Rect::new(x + 8.0, y + 8.0, x + tile - 8.0, y + tile - 8.0);
            self.p
                .rect(thumb, &RectStyle::rounded(Color::rgb(24, 25, 28), 4.0));
            let c = Vec2::new(
                (thumb.min.x + thumb.max.x) * 0.5,
                (thumb.min.y + thumb.max.y) * 0.5,
            );
            match i % 3 {
                0 => self.p.circle(c, 26.0, &ShapeStyle::fill(color)),
                1 => self.p.rect(
                    Rect::new(c.x - 24.0, c.y - 24.0, c.x + 24.0, c.y + 24.0),
                    &RectStyle::rounded(color, 6.0),
                ),
                _ => self.p.triangle(
                    [
                        Vec2::new(c.x, c.y - 28.0),
                        Vec2::new(c.x + 28.0, c.y + 22.0),
                        Vec2::new(c.x - 28.0, c.y + 22.0),
                    ],
                    &ShapeStyle::fill(color),
                ),
            }
            self.p.rect(
                Rect::new(x, y + tile - 2.0, x + tile, y + tile),
                &RectStyle::fill(color),
            );
            self.text(name, Vec2::new(x + 8.0, y + tile + 4.0), 12.0, TEXT);
        }
    }

    fn viewport(&mut self, r: Rect) {
        self.p.rect(r, &RectStyle::fill(Color::rgb(22, 23, 26)));
        // A ground grid in perspective, with the horizon.
        let horizon = r.min.y + (r.max.y - r.min.y) * 0.42;
        let cx = (r.min.x + r.max.x) * 0.5;
        let grid = StrokeStyle::new(1.0, Color::rgba(255, 255, 255, 26));
        for k in -12..=12 {
            let x = cx + k as f32 * 70.0;
            self.p.line(
                Vec2::new(cx + k as f32 * 6.0, horizon),
                Vec2::new(x, r.max.y),
                &grid,
            );
        }
        let mut y = horizon + 6.0;
        let mut step = 4.0;
        while y < r.max.y {
            self.p
                .line(Vec2::new(r.min.x, y), Vec2::new(r.max.x, y), &grid);
            y += step;
            step *= 1.35;
        }
        self.p.line(
            Vec2::new(r.min.x, horizon),
            Vec2::new(r.max.x, horizon),
            &StrokeStyle {
                width: 1.0,
                color: Color::rgba(76, 140, 245, 120),
                cap: LineCap::Butt,
                dash: Some(Dash {
                    dash: 10.0,
                    gap: 6.0,
                    phase: 0.0,
                }),
            },
        );
        // The viewport's own toolbar, floating over it.
        let bar = Rect::new(
            r.min.x + 10.0,
            r.min.y + 10.0,
            r.min.x + 230.0,
            r.min.y + 36.0,
        );
        self.p
            .rect(bar, &RectStyle::rounded(Color::rgba(43, 45, 49, 220), 6.0));
        self.text(
            "Perspective · Lit · Show",
            Vec2::new(bar.min.x + 10.0, bar.min.y + 6.0),
            12.0,
            TEXT,
        );
        let msg = "The scene view lands here with U2";
        let w = TextLayout::new(&self.font, FontId(0), 14.0, msg).width;
        self.text(msg, Vec2::new(cx - w * 0.5, horizon - 30.0), 14.0, DIM);
        // The axis gizmo in the corner.
        let o = Vec2::new(r.min.x + 40.0, r.max.y - 40.0);
        for (d, color) in [
            (Vec2::new(26.0, 6.0), Color::rgb(230, 80, 80)),
            (Vec2::new(0.0, -28.0), Color::rgb(110, 200, 90)),
            (Vec2::new(-14.0, 14.0), Color::rgb(80, 130, 240)),
        ] {
            self.p.line(
                o,
                o + d,
                &StrokeStyle {
                    width: 2.0,
                    color,
                    cap: LineCap::Round,
                    dash: None,
                },
            );
            self.p.circle(o + d, 3.5, &ShapeStyle::fill(color));
        }
    }
}

/// Draws the shell into `p` with `font`.
pub fn draw(p: &mut Painter<'_>, font: FontView<'_>, state: &ShellState<'_>) {
    let (w, h) = (state.size.x, state.size.y);
    p.rect(Rect::new(0.0, 0.0, w, h), &RectStyle::fill(BACKGROUND));
    let mut s = Shell {
        p,
        font,
        cursor: state.cursor,
    };
    s.menu(w);
    s.toolbar(w);
    let top = MENU_HEIGHT + TOOLBAR_HEIGHT + 1.0;
    let bottom = h - STATUS_HEIGHT;
    let left = LEFT_WIDTH.min(w * 0.3);
    let right = w - RIGHT_WIDTH.min(w * 0.3);
    let split = (bottom - BOTTOM_HEIGHT).max(top + 80.0);
    // The viewport first: its grid runs past its sides, and with no clipping before U2 the
    // opaque panels drawn after it cover that (and drop it from their tiles).
    s.viewport(Rect::new(left + 1.0, top, right - 1.0, split - 1.0));
    s.outliner(Rect::new(0.0, top, left, bottom));
    s.details(Rect::new(right, top, w, bottom));
    s.content_browser(Rect::new(left + 1.0, split, right - 1.0, bottom));
    // Splitters between the panels.
    let line = StrokeStyle::new(1.0, BORDER);
    s.p.line(
        Vec2::new(left + 0.5, top),
        Vec2::new(left + 0.5, bottom),
        &line,
    );
    s.p.line(
        Vec2::new(right - 0.5, top),
        Vec2::new(right - 0.5, bottom),
        &line,
    );
    s.p.line(
        Vec2::new(left, split - 0.5),
        Vec2::new(right, split - 0.5),
        &line,
    );
    // The status bar.
    let bar = Rect::new(0.0, bottom, w, h);
    s.p.rect(bar, &RectStyle::fill(ACCENT));
    s.text(
        state.status,
        Vec2::new(10.0, bottom + 4.0),
        12.0,
        Color::WHITE,
    );
}
