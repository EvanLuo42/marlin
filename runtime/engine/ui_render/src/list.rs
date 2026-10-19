//! Recording: the [`Painter`] writes commands into a [`UiDrawList`] (0012 §2).
//!
//! A draw list is an ordered sequence of parts. [`Painter::chunk`] reserves the next place in
//! the order and returns a [`ChunkSlot`], a list of its own that another job can record into
//! (in the group and under the clip current where it was reserved); [`UiDrawList::fill`] puts
//! it back in its place. A recorded chunk can be kept as a [`RetainedChunk`] and replayed in a
//! later frame at an offset, under the clip current there: its words are copied when the list
//! is flattened, the coordinates moved. Replay offsets snap to whole physical pixels, so text
//! keeps its subpixel positions (and with them its glyph ids) and stays as crisp as it was
//! recorded.
//!
//! Groups ([`Painter::begin_group`]) nest: the painter multiplies their transforms and
//! opacities, so the GPU sees each group's transform to the screen. Commands in a group keep
//! the group's own coordinates; outside any group they are physical pixels. Text is always
//! placed on the screen: a group's translation and uniform scale move and size its glyphs
//! (they are rasterized at the scaled size); rotation and skew leave glyphs upright until text
//! can be drawn as paths (U3, §6.4).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use marlin_font::{FontView, GlyphIds, GlyphKey, SUBPIXEL_POSITIONS, TextLayout};
use marlin_math::Vec2;
use marlin_math::scalar::{floor, round, sqrt};

use crate::command::{
    CLIP_WORDS, Chunk, ClipWords, DEFAULT_SAMPLER, GROUP_AXIS_ALIGNED, GROUP_NONUNIFORM,
    GROUP_WORDS, GroupWords, IMAGE_SRGB_PREMULTIPLIED, Kind, OPAQUE, PAINT_BACKDROP,
    PAINT_EXTERNAL, PAINT_GRADIENT, PAINT_IMAGE, f,
};
use crate::path::{FillRule, Path};
use crate::style::{
    Affine2, Color, Corners, GroupDesc, ImageColor, ImageSource, ImageStyle, LineCap, Paint, Rect,
    RectStyle, ShapeStyle, StrokeStyle,
};

enum Part {
    /// Commands of the list's own chunk.
    Own(Range<usize>),
    /// A slot handed out and not filled yet: draws nothing.
    Reserved,
    Filled(alloc::boxed::Box<UiDrawList>),
    /// A retained chunk moved by whole physical pixels, under one of the list's clips.
    Replay {
        chunk: RetainedChunk,
        dx: f32,
        dy: f32,
        clip: u32,
    },
}

/// A group with a shadow or glow being recorded: its effect commands, filled at `end_group`.
struct Effects {
    group: u32,
    commands: Vec<usize>,
    approximate: f32,
    /// The members' screen box so far.
    bbox: [f32; 4],
}

#[derive(Clone, Copy)]
struct Scope {
    group: u32,
    /// From the group's space to logical screen pixels.
    transform: Affine2,
    opacity: f32,
}

/// One surface's commands for a frame, in drawing order.
pub struct UiDrawList {
    /// Physical pixels per logical pixel.
    scale: f32,
    /// Everything this list records itself; the parts say in which order.
    own: Chunk,
    parts: Vec<Part>,
    groups: Vec<Scope>,
    clips: Vec<u32>,
    effects: Vec<Effects>,
}

impl UiDrawList {
    /// An empty list for a surface with `scale` physical pixels per logical pixel (its DPI
    /// scale).
    pub fn new(scale: f32) -> Self {
        Self {
            scale,
            own: Chunk::default(),
            parts: Vec::new(),
            groups: Vec::new(),
            clips: Vec::new(),
            effects: Vec::new(),
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// A painter appending to the list; `ids` gives glyphs their ids.
    pub fn painter<'a>(&'a mut self, ids: &'a GlyphIds) -> Painter<'a> {
        Painter {
            list: self,
            ids,
            fonts: &[],
            text_as_paths: false,
        }
    }

    /// Puts a slot's recording in the place [`Painter::chunk`] reserved for it.
    pub fn fill(&mut self, slot: ChunkSlot) {
        match self.parts.get_mut(slot.index) {
            Some(p @ Part::Reserved) => *p = Part::Filled(alloc::boxed::Box::new(slot.list)),
            _ => panic!("the slot is not this list's, or it was filled already"),
        }
    }

    /// Commands recorded, slots and replays included.
    pub fn len(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Own(r) => r.len(),
                Part::Reserved => 0,
                Part::Filled(l) => l.len(),
                Part::Replay { chunk, .. } => chunk.len(),
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every command in order, as one chunk: what the renderer uploads.
    pub fn flatten(&self) -> Chunk {
        let mut out = Chunk::default();
        self.flatten_into(&mut out);
        out
    }

    pub fn flatten_into(&self, out: &mut Chunk) {
        let own = &self.own;
        // The own chunk's groups, clips, payload and paints go in first; its commands follow in
        // their parts' places.
        let gb = out.groups.len() as u32 - 1;
        let cb = out.clips.len() as u32 - 1;
        let (pb, yb) = (out.paints.len() as u32, out.payload.len() as u32);
        let group = |g: u32| if g == 0 { 0 } else { g + gb };
        let clip = |c: u32| if c == 0 { 0 } else { c + cb };
        let first_group = out.groups.len();
        out.groups.extend_from_slice(&own.groups[1..]);
        out.clips.extend(own.clips[1..].iter().map(|c| {
            let mut c = *c;
            c[8] = group(c[8]);
            c[9] = clip(c[9]);
            c
        }));
        out.payload.extend_from_slice(&own.payload);
        let rb = out.gradients.len() as u32;
        out.paints.extend(own.paints.iter().map(|p| {
            let mut p = *p;
            if p[0] == PAINT_GRADIENT {
                p[1] += rb;
            }
            p
        }));
        out.gradients.extend_from_slice(&own.gradients);
        // Where each own range landed, for the groups' member ranges.
        let mut landed: Vec<(Range<usize>, usize)> = Vec::new();
        for p in &self.parts {
            match p {
                Part::Own(r) => {
                    landed.push((r.clone(), out.commands.len()));
                    out.commands.extend(own.commands[r.clone()].iter().map(|c| {
                        [
                            (c[0] & 0xFFFF) | group(c[0] >> 16) << 16,
                            clip(c[1] & 0xFFFF),
                            c[2] + pb,
                            c[3] + yb,
                        ]
                    }));
                }
                Part::Reserved => {}
                Part::Filled(l) => l.flatten_into(out),
                Part::Replay {
                    chunk,
                    dx,
                    dy,
                    clip: c,
                } => {
                    if *dx == 0.0 && *dy == 0.0 {
                        out.append_clipped(&chunk.chunk, clip(*c));
                    } else {
                        let mut moved = (*chunk.chunk).clone();
                        translate(&mut moved, *dx, *dy);
                        out.append_clipped(&moved, clip(*c));
                    }
                }
            }
        }
        // Member ranges: within the own range they started in.
        for g in &mut out.groups[first_group..first_group + own.groups.len() - 1] {
            if g[16] == 0 {
                continue;
            }
            let (first, count) = (g[15] as usize, g[16] as usize);
            match landed.iter().find(|(r, _)| r.contains(&first)) {
                Some((r, at)) => {
                    g[15] = (at + first - r.start) as u32;
                    g[16] = count.min(r.end - first) as u32;
                }
                None => g[16] = 0,
            }
        }
    }

    fn extend_own(&mut self) {
        let end = self.own.commands.len();
        match self.parts.last_mut() {
            Some(Part::Own(r)) if r.end + 1 == end => r.end = end,
            _ => self.parts.push(Part::Own(end - 1..end)),
        }
    }

    fn scope(&self) -> Scope {
        self.groups.last().copied().unwrap_or(Scope {
            group: 0,
            transform: Affine2::IDENTITY,
            opacity: 1.0,
        })
    }

    fn clip(&self) -> u32 {
        self.clips.last().copied().unwrap_or(0)
    }

    /// A new list starting in this one's current group and clip: copies of their records.
    fn inherit(&self) -> Self {
        let mut list = Self::new(self.scale);
        let scope = self.scope();
        // Every group the clip chain and the current scope use, copied once.
        let mut map: Vec<(u32, u32)> = Vec::new();
        let mut copy_group = |list: &mut Self, g: u32| -> u32 {
            if g == 0 {
                return 0;
            }
            if let Some(&(_, n)) = map.iter().find(|(o, _)| *o == g) {
                return n;
            }
            let mut words = self.own.groups[g as usize];
            words[16] = 0;
            list.own.groups.push(words);
            let n = list.own.groups.len() as u32 - 1;
            map.push((g, n));
            n
        };
        let mut chain = Vec::new();
        let mut c = self.clip();
        while c != 0 {
            chain.push(c);
            c = self.own.clips[c as usize][9];
        }
        let mut parent = 0;
        for &c in chain.iter().rev() {
            let mut words = self.own.clips[c as usize];
            words[8] = copy_group(&mut list, words[8]);
            words[9] = parent;
            list.own.clips.push(words);
            parent = list.own.clips.len() as u32 - 1;
        }
        if parent != 0 {
            list.clips.push(parent);
        }
        if scope.group != 0 {
            let g = copy_group(&mut list, scope.group);
            list.groups.push(Scope { group: g, ..scope });
        }
        list
    }
}

/// Moves `chunk`'s screen-space content by whole pixels: commands of group 0 and glyphs, group
/// transforms, and the clips' rectangles and screen boxes.
fn translate(chunk: &mut Chunk, dx: f32, dy: f32) {
    let mv = |w: &mut u32, d: f32| *w = f(f32::from_bits(*w) + d);
    for c in &chunk.commands {
        let p = &mut chunk.payload[c[3] as usize..];
        let kind = Kind::from_word(c[0]);
        if kind == Some(Kind::Glyph) {
            p[0] = (p[0] as i32 + dx as i32) as u32;
            p[1] = (p[1] as i32 + dy as i32) as u32;
            continue;
        }
        if c[0] >> 16 != 0 {
            continue;
        }
        match kind {
            Some(Kind::Rect | Kind::Line | Kind::Image | Kind::Blur) => {
                mv(&mut p[0], dx);
                mv(&mut p[1], dy);
                mv(&mut p[2], dx);
                mv(&mut p[3], dy);
            }
            Some(Kind::Circle) => {
                mv(&mut p[0], dx);
                mv(&mut p[1], dy);
            }
            Some(Kind::Triangle) => {
                for k in 0..3 {
                    mv(&mut p[2 * k], dx);
                    mv(&mut p[2 * k + 1], dy);
                }
            }
            Some(Kind::Path) => {
                mv(&mut p[2], dy);
                for (i, d) in [(5, dx), (6, dy), (7, dx), (8, dy)] {
                    mv(&mut p[i], d);
                }
                let (len, segs) = (p[1] as usize, p[4] as usize);
                for s in 0..segs {
                    let at = len - 4 * segs + 4 * s;
                    for (k, d) in [(0, dx), (1, dy), (2, dx), (3, dy)] {
                        mv(&mut p[at + k], d);
                    }
                }
            }
            _ => {}
        }
    }
    for g in &mut chunk.groups[1..] {
        let mut t = Affine2(core::array::from_fn(|i| f32::from_bits(g[i])));
        t.0[4] += dx;
        t.0[5] += dy;
        let inv = t.inverse();
        for i in 0..6 {
            g[i] = f(t.0[i]);
            g[6 + i] = f(inv.0[i]);
        }
    }
    for g in &mut chunk.gradients {
        mv(&mut g[1], dx);
        mv(&mut g[2], dy);
        if g[0] == 0 {
            mv(&mut g[3], dx);
            mv(&mut g[4], dy);
        }
    }
    for c in &mut chunk.clips[1..] {
        if c[8] == 0 {
            mv(&mut c[0], dx);
            mv(&mut c[1], dy);
            mv(&mut c[2], dx);
            mv(&mut c[3], dy);
        }
        for (i, d) in [
            (10, dx),
            (11, dy),
            (12, dx),
            (13, dy),
            (14, dx),
            (15, dy),
            (16, dx),
            (17, dy),
        ] {
            mv(&mut c[i], d);
        }
    }
}

/// A place in a draw list's order, recorded elsewhere (on another job) and put back with
/// [`UiDrawList::fill`].
pub struct ChunkSlot {
    index: usize,
    list: UiDrawList,
}

impl ChunkSlot {
    pub fn painter<'a>(&'a mut self, ids: &'a GlyphIds) -> Painter<'a> {
        self.list.painter(ids)
    }

    /// What has been recorded, kept for replaying in later frames.
    pub fn retain(&self) -> RetainedChunk {
        RetainedChunk {
            chunk: Arc::new(self.list.flatten()),
            scale: self.list.scale,
        }
    }
}

/// A recorded chunk kept across frames (§2.3).
#[derive(Clone)]
pub struct RetainedChunk {
    chunk: Arc<Chunk>,
    scale: f32,
}

impl RetainedChunk {
    /// Records a chunk for a surface of `scale`.
    pub fn record(scale: f32, ids: &GlyphIds, f: impl FnOnce(&mut Painter<'_>)) -> Self {
        let mut list = UiDrawList::new(scale);
        f(&mut list.painter(ids));
        Self {
            chunk: Arc::new(list.flatten()),
            scale,
        }
    }

    pub fn len(&self) -> usize {
        self.chunk.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunk.is_empty()
    }
}

/// Writes commands (§2.1). Coordinates are logical pixels; the surface's scale is applied as
/// they are written (or by the group's transform, inside a group).
pub struct Painter<'a> {
    list: &'a mut UiDrawList,
    ids: &'a GlyphIds,
    /// For text drawn as paths: `fonts[i]` is `FontId(i)`.
    fonts: &'a [FontView<'a>],
    text_as_paths: bool,
}

/// Text larger than this on the screen (pixels per em) is drawn as paths, not from the glyph
/// cache (§6.4).
pub const PATH_TEXT_SIZE: f32 = 64.0;

fn bits(v: [f32; 4]) -> [u32; 4] {
    v.map(f)
}

impl<'a> Painter<'a> {
    /// Logical units to what commands store: physical pixels outside groups, the group's own
    /// units inside one.
    fn k(&self) -> f32 {
        if self.list.groups.is_empty() {
            self.list.scale
        } else {
            1.0
        }
    }

    fn s(&self, v: f32) -> u32 {
        f(v * self.k())
    }

    fn corners(&self, c: &Corners) -> [u32; 4] {
        [c.top_left, c.top_right, c.bottom_right, c.bottom_left].map(|r| self.s(r.max(0.0)))
    }

    fn push(
        &mut self,
        kind: Kind,
        flags: u32,
        paint: impl FnOnce(&mut Chunk) -> u32,
        payload: &[u32],
    ) {
        let scope = self.list.scope();
        let clip = self.list.clip();
        // Opaque only at full opacity.
        let flags = if scope.opacity < 1.0 {
            flags & !OPAQUE
        } else {
            flags
        };
        let chunk = &mut self.list.own;
        let paint = paint(chunk);
        chunk.push(kind, flags, scope.group, clip, paint, payload);
        self.list.extend_own();
        if !self.list.effects.is_empty()
            && let Some(l) = local_box(kind, payload)
        {
            let to_screen = if scope.group == 0 {
                Affine2::IDENTITY
            } else {
                self.to_screen()
            };
            self.list.union_box(to_screen, l);
        }
    }

    /// The transform from the current group's space to physical screen pixels.
    fn to_screen(&self) -> Affine2 {
        let s = self.list.scale;
        Affine2::scale(s, s) * self.list.scope().transform
    }

    /// Starts a group (§2.2): what follows until [`end_group`](Self::end_group) is drawn in
    /// `desc`'s space, at its opacity.
    pub fn begin_group(&mut self, desc: &GroupDesc) {
        let parent = self.list.scope();
        let transform = parent.transform * desc.transform;
        let opacity = parent.opacity * desc.opacity.clamp(0.0, 1.0);
        let s = self.list.scale;
        let forward = Affine2::scale(s, s) * transform;
        let inverse = forward.inverse();
        let [ia, ib, ic, id, ..] = inverse.0;
        let mut g: GroupWords = [0; GROUP_WORDS];
        for i in 0..6 {
            g[i] = f(forward.0[i]);
            g[6 + i] = f(inverse.0[i]);
        }
        g[12] = f(opacity);
        g[13] = f(sqrt(inverse.determinant().abs()));
        g[14] = if forward.is_similarity() {
            0
        } else {
            GROUP_NONUNIFORM
        } | if forward.is_axis_aligned() {
            GROUP_AXIS_ALIGNED
        } else {
            0
        };
        if let Some(sh) = desc.shadow {
            g[17] = f(sh.offset.x * s);
            g[18] = f(sh.offset.y * s);
            g[19] = f(sh.blur.max(0.0) * s);
            g[20] = sh.color.packed();
        }
        if let Some(gl) = desc.glow {
            g[21] = f(gl.radius.max(0.0) * s);
            g[22] = gl.color.packed();
        }
        // The inverse's largest singular value: the most group units a screen pixel spans.
        let sum = ia * ia + ib * ib + ic * ic + id * id;
        let det = inverse.determinant();
        g[23] = f(sqrt(
            (sum + sqrt((sum * sum - 4.0 * det * det).max(0.0))) * 0.5,
        ));
        self.list.own.groups.push(g);
        let group = self.list.own.groups.len() as u32 - 1;
        self.list.groups.push(Scope {
            group,
            transform,
            opacity,
        });
        // The shadow, then the glow, under the members (§4.3).
        if desc.shadow.is_some() || desc.glow.is_some() {
            let clip = self.list.clip();
            let mut commands = Vec::new();
            let effects = [
                desc.shadow.map(|s| (0, s.color)),
                desc.glow.map(|g| (1, g.color)),
            ];
            for (effect, color) in effects.into_iter().flatten() {
                let own = &mut self.list.own;
                let paint = own.solid(color);
                let mut payload = [0u32; 7];
                payload[0] = effect;
                own.push(Kind::Effect, 0, group, clip, paint, &payload);
                commands.push(own.commands.len() - 1);
                self.list.extend_own();
            }
            self.list.effects.push(Effects {
                group,
                commands,
                approximate: desc.approximate.map_or(-1.0, |r| r.max(0.0) * s),
                bbox: [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
            });
        }
        let first = self.list.own.commands.len() as u32;
        self.list.own.groups[group as usize][15] = first;
    }

    pub fn end_group(&mut self) {
        let scope = self
            .list
            .groups
            .pop()
            .expect("end_group without begin_group");
        let g = &mut self.list.own.groups[scope.group as usize];
        g[16] = self.list.own.commands.len() as u32 - g[15];
        let members = g[16];
        if let Some(e) = self.list.effects.pop_if(|e| e.group == scope.group) {
            // Many members: their box, as a rounded rectangle, stands in for their shape.
            let radius = if members > MAX_EFFECT_MEMBERS {
                e.approximate.max(0.0)
            } else {
                e.approximate
            };
            let bbox = if e.bbox[0] <= e.bbox[2] {
                e.bbox
            } else {
                [0.0; 4]
            };
            for c in e.commands {
                let at = self.list.own.commands[c][3] as usize;
                let p = &mut self.list.own.payload[at..at + 7];
                p[2..6].copy_from_slice(&bits(bbox));
                p[6] = f(radius);
            }
        }
    }

    /// Clips what follows to a rounded rectangle in the current space, inside the current clip,
    /// until [`pop_clip`](Self::pop_clip).
    pub fn push_clip(&mut self, r: Rect, corners: Corners) {
        let parent = self.list.clip();
        let pw = self.list.own.clips[parent as usize];
        let pf = |i: usize| f32::from_bits(pw[i]);
        let k = self.k();
        let rect = [r.min.x * k, r.min.y * k, r.max.x * k, r.max.y * k];
        let radii = [
            corners.top_left,
            corners.top_right,
            corners.bottom_right,
            corners.bottom_left,
        ]
        .map(|v| v.max(0.0) * k);
        // Group 0's coordinates are already the screen's.
        let to_screen = if self.list.groups.is_empty() {
            Affine2::IDENTITY
        } else {
            self.to_screen()
        };
        let corners_screen = [
            Vec2::new(rect[0], rect[1]),
            Vec2::new(rect[2], rect[1]),
            Vec2::new(rect[0], rect[3]),
            Vec2::new(rect[2], rect[3]),
        ]
        .map(|p| to_screen.apply(p));
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in corners_screen {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        let aabb = [
            x0.max(pf(10)),
            y0.max(pf(11)),
            x1.min(pf(12)),
            y1.min(pf(13)),
        ];
        let inner = if to_screen.is_axis_aligned() {
            let m = radii.iter().cloned().fold(0.0f32, f32::max);
            let a = to_screen.apply(Vec2::new(rect[0] + m, rect[1] + m));
            let b = to_screen.apply(Vec2::new(rect[2] - m, rect[3] - m));
            let i = [
                a.x.min(b.x).max(pf(14)),
                a.y.min(b.y).max(pf(15)),
                a.x.max(b.x).min(pf(16)),
                a.y.max(b.y).min(pf(17)),
            ];
            if i[0] < i[2] && i[1] < i[3] {
                i
            } else {
                [0.0; 4]
            }
        } else {
            [0.0; 4]
        };
        let mut c: ClipWords = [0; CLIP_WORDS];
        c[0..4].copy_from_slice(&bits(rect));
        c[4..8].copy_from_slice(&bits(radii));
        c[8] = self.list.scope().group;
        c[9] = parent;
        c[10..14].copy_from_slice(&bits(aabb));
        c[14..18].copy_from_slice(&bits(inner));
        self.list.own.clips.push(c);
        let index = self.list.own.clips.len() as u32 - 1;
        assert!(index <= 0xFFFF, "more than 65535 clips in one draw list");
        self.list.clips.push(index);
    }

    pub fn pop_clip(&mut self) {
        self.list.clips.pop().expect("pop_clip without push_clip");
    }

    /// A rectangle: filled, rounded per corner, optionally outlined.
    pub fn rect(&mut self, r: Rect, style: &RectStyle) {
        let (sw, sc) = style
            .stroke
            .map_or((0.0, Color::TRANSPARENT), |s| (s.width.max(0.0), s.color));
        let opaque = style.fill.a == 255 && (sw == 0.0 || sc.a == 255);
        let [r0, r1, r2, r3] = self.corners(&style.corners);
        let payload = [
            self.s(r.min.x),
            self.s(r.min.y),
            self.s(r.max.x),
            self.s(r.max.y),
            r0,
            r1,
            r2,
            r3,
            self.s(sw),
            sc.packed(),
        ];
        let flags = if opaque { OPAQUE } else { 0 };
        self.push(Kind::Rect, flags, |c| c.solid(style.fill), &payload);
    }

    pub fn circle(&mut self, center: Vec2, radius: f32, style: &ShapeStyle) {
        let (sw, sc) = style
            .stroke
            .map_or((0.0, Color::TRANSPARENT), |s| (s.width.max(0.0), s.color));
        let opaque = style.fill.a == 255 && (sw == 0.0 || sc.a == 255);
        let payload = [
            self.s(center.x),
            self.s(center.y),
            self.s(radius.max(0.0)),
            self.s(sw),
            sc.packed(),
        ];
        let flags = if opaque { OPAQUE } else { 0 };
        self.push(Kind::Circle, flags, |c| c.solid(style.fill), &payload);
    }

    /// A line segment of `style.width`, with caps and dashes.
    pub fn line(&mut self, a: Vec2, b: Vec2, style: &StrokeStyle) {
        let cap = match style.cap {
            LineCap::Butt => 0,
            LineCap::Round => 1,
            LineCap::Square => 2,
        };
        let (dash, gap, phase) = style
            .dash
            .filter(|d| d.dash > 0.0 && d.gap > 0.0)
            .map_or((0.0, 0.0, 0.0), |d| (d.dash, d.gap, d.phase));
        let payload = [
            self.s(a.x),
            self.s(a.y),
            self.s(b.x),
            self.s(b.y),
            self.s(style.width.max(0.0)),
            cap,
            self.s(dash),
            self.s(gap),
            self.s(phase),
        ];
        self.push(Kind::Line, 0, |c| c.solid(style.color), &payload);
    }

    pub fn triangle(&mut self, p: [Vec2; 3], style: &ShapeStyle) {
        let (sw, sc) = style
            .stroke
            .map_or((0.0, Color::TRANSPARENT), |s| (s.width.max(0.0), s.color));
        let payload = [
            self.s(p[0].x),
            self.s(p[0].y),
            self.s(p[1].x),
            self.s(p[1].y),
            self.s(p[2].x),
            self.s(p[2].y),
            self.s(sw),
            sc.packed(),
        ];
        self.push(Kind::Triangle, 0, |c| c.solid(style.fill), &payload);
    }

    /// Laid-out text with its top-left at `origin`, rasterized at the size it has on screen
    /// (§6.2): each glyph's pen is snapped to whole pixels vertically and to quarter pixels
    /// horizontally.
    pub fn text(&mut self, layout: &TextLayout, origin: Vec2, color: Color) {
        self.text_paint(layout, origin, &Paint::Solid(color));
    }

    /// [`text`](Self::text) in any paint.
    pub fn text_paint(&mut self, layout: &TextLayout, origin: Vec2, paint: &Paint) {
        let to_screen = self.to_screen();
        let size = layout.size * sqrt(to_screen.determinant().abs());
        // Large, turned, skewed or stretched text, or text that is being zoomed, is drawn from
        // its outlines (§6.4).
        let font = self.fonts.get(layout.font.0 as usize).copied();
        if let Some(font) = font
            && (self.text_as_paths
                || size > PATH_TEXT_SIZE
                || !to_screen.is_uniform_scale_translate())
        {
            self.text_outlines(font, layout, origin, paint);
            return;
        }
        let ids = self.ids;
        let scope = self.list.scope();
        let clip = self.list.clip();
        if !self.list.effects.is_empty() {
            let b = [
                origin.x,
                origin.y,
                origin.x + layout.width,
                origin.y + layout.height,
            ];
            self.list.union_box(to_screen, b);
        }
        // Glyphs are on the screen, not in the group: they take its opacity here.
        let paint = self.paint(paint, scope.opacity);
        let list = &mut *self.list;
        ids.with(|id| {
            for g in &layout.glyphs {
                let pen = to_screen.apply(Vec2::new(origin.x + g.x, origin.y + g.y));
                let y = round(pen.y);
                let mut xi = floor(pen.x);
                let mut q = round((pen.x - xi) * SUBPIXEL_POSITIONS as f32) as u32;
                if q == SUBPIXEL_POSITIONS {
                    xi += 1.0;
                    q = 0;
                }
                let glyph = id(GlyphKey::new(layout.font, g.glyph, size), q);
                list.own.push(
                    Kind::Glyph,
                    0,
                    0,
                    clip,
                    paint,
                    &[xi as i32 as u32, y as i32 as u32, glyph.0],
                );
                list.extend_own();
            }
        });
    }

    /// Gives the painter the fonts text is laid out in (`fonts[i]` is `FontId(i)`), so text can be
    /// drawn from its outlines: past [`PATH_TEXT_SIZE`] on the screen, under a group that
    /// turns, skews or stretches it, and while [`set_text_as_paths`](Self::set_text_as_paths).
    /// Without them, text always comes from the glyph cache, upright.
    pub fn with_fonts(mut self, fonts: &'a [FontView<'a>]) -> Self {
        self.fonts = fonts;
        self
    }

    /// Draws text as paths whatever its size: for text whose scale is animating (a zooming
    /// node graph), so the glyph cache does not fill with every size it passes through (§6.4).
    pub fn set_text_as_paths(&mut self, on: bool) {
        self.text_as_paths = on;
    }

    /// One path command per glyph, from `font`'s outlines, on the screen.
    fn text_outlines(
        &mut self,
        font: FontView<'_>,
        layout: &TextLayout,
        origin: Vec2,
        paint: &Paint,
    ) {
        let to_screen = self.to_screen();
        let scope = self.list.scope();
        let clip = self.list.clip();
        if !self.list.effects.is_empty() {
            let b = [
                origin.x,
                origin.y,
                origin.x + layout.width,
                origin.y + layout.height,
            ];
            self.list.union_box(to_screen, b);
        }
        let paint = self.paint(paint, scope.opacity);
        // Font units, y up, to logical pixels, y down.
        let s = layout.size / font.units_per_em();
        for g in &layout.glyphs {
            let mut path = Path::new();
            let mut at: Option<[f32; 2]> = None;
            for seg in font.segments(g.glyph) {
                if at != Some(seg.p0) {
                    if at.is_some() {
                        path.close();
                    }
                    path.move_to(Vec2::new(seg.p0[0], seg.p0[1]));
                }
                if seg.is_line() {
                    path.line_to(Vec2::new(seg.p1[0], seg.p1[1]));
                } else {
                    path.quad_to(
                        Vec2::new(seg.c[0], seg.c[1]),
                        Vec2::new(seg.p1[0], seg.p1[1]),
                    );
                }
                at = Some(seg.p1);
            }
            if path.is_empty() {
                continue;
            }
            let place = to_screen
                * Affine2::translate(origin.x + g.x, origin.y + g.y)
                * Affine2::scale(s, -s);
            let segs = path.flatten(place);
            if let Some(payload) = crate::path::payload(&segs, FillRule::NonZero) {
                self.list.own.push(Kind::Path, 0, 0, clip, paint, &payload);
                self.list.extend_own();
            }
        }
    }

    /// The index of a new paint for `paint` at `opacity`: solid colors take it in their alpha,
    /// gradients are baked with their geometry on the screen.
    fn paint(&mut self, paint: &Paint, opacity: f32) -> u32 {
        match *paint {
            Paint::Solid(mut color) => {
                color.a = (f32::from(color.a) * opacity + 0.5) as u8;
                self.list.own.solid(color)
            }
            Paint::Gradient(g) => {
                let record = crate::gradient::bake(g, self.to_screen(), opacity);
                let own = &mut self.list.own;
                own.gradients.push(record);
                own.paints
                    .push([PAINT_GRADIENT, own.gradients.len() as u32 - 1, 0, 0]);
                own.paints.len() as u32 - 1
            }
        }
    }

    /// Blurs what is drawn under the rounded rectangle `r` (§5.6): inside it, the pixels below
    /// become their Gaussian blur of `radius` logical pixels (σ is half of it, as for shadows),
    /// at the group's opacity and clip. Draw a translucent fill over it for frosted glass.
    pub fn backdrop_blur(&mut self, r: Rect, radius: f32, corners: Corners) {
        let [r0, r1, r2, r3] = self.corners(&corners);
        let payload = [
            self.s(r.min.x),
            self.s(r.min.y),
            self.s(r.max.x),
            self.s(r.max.y),
            r0,
            r1,
            r2,
            r3,
            f(radius.max(0.0) * self.list.scale),
            0,
        ];
        let paint = |c: &mut Chunk| c.image(PAINT_BACKDROP, Color::WHITE, 0, DEFAULT_SAMPLER);
        self.push(Kind::Blur, 0, paint, &payload);
    }

    /// A rectangle in any paint, rounded per corner (§2.1): in the current group, at its
    /// opacity.
    pub fn fill_rect(&mut self, r: Rect, corners: Corners, paint: &Paint) {
        let opaque = match paint {
            Paint::Solid(c) => c.a == 255,
            Paint::Gradient(g) => g.stops.iter().all(|(_, c)| c.a == 255),
        };
        let [r0, r1, r2, r3] = self.corners(&corners);
        let payload = [
            self.s(r.min.x),
            self.s(r.min.y),
            self.s(r.max.x),
            self.s(r.max.y),
            r0,
            r1,
            r2,
            r3,
            0,
            0,
        ];
        let index = self.paint(paint, 1.0);
        let flags = if opaque { OPAQUE } else { 0 };
        self.push(Kind::Rect, flags, |_| index, &payload);
    }

    /// Fills `path` (logical pixels in the current space) by `rule` (§4.2). It is flattened onto
    /// the screen here, so it keeps the group's opacity and clip but not the group itself.
    pub fn fill_path(&mut self, path: &Path, rule: FillRule, paint: &Paint) {
        let segs = path.flatten(self.to_screen());
        let Some(payload) = crate::path::payload(&segs, rule) else {
            return;
        };
        let scope = self.list.scope();
        let clip = self.list.clip();
        if !self.list.effects.is_empty() {
            let b = |i: usize| f32::from_bits(payload[i]);
            self.list
                .union_box(Affine2::IDENTITY, [b(5), b(6), b(7), b(8)]);
        }
        let paint = self.paint(paint, scope.opacity);
        self.list.own.push(Kind::Path, 0, 0, clip, paint, &payload);
        self.list.extend_own();
    }

    /// A texture in `r`, through `style`'s UV rectangle, tint and corners.
    pub fn image(&mut self, r: Rect, src: ImageSource, style: &ImageStyle) {
        let (kind, view, width, height, color) = match src {
            ImageSource::Texture {
                view,
                width,
                height,
                color,
            } => (PAINT_IMAGE, view, width, height, color),
            ImageSource::External {
                id,
                width,
                height,
                color,
            } => (PAINT_EXTERNAL, id.0, width, height, color),
        };
        let flags = match color {
            ImageColor::LinearStraight => 0,
            ImageColor::SrgbPremultiplied => IMAGE_SRGB_PREMULTIPLIED,
        };
        let [r0, r1, r2, r3] = self.corners(&style.corners);
        let payload = [
            self.s(r.min.x),
            self.s(r.min.y),
            self.s(r.max.x),
            self.s(r.max.y),
            f(style.uv.min.x),
            f(style.uv.min.y),
            f(style.uv.max.x),
            f(style.uv.max.y),
            width.min(0xFFFF) | height.min(0xFFFF) << 16,
            r0,
            r1,
            r2,
            r3,
        ];
        let sampler = style.sampler.unwrap_or(DEFAULT_SAMPLER);
        self.push(
            Kind::Image,
            flags,
            |c| c.image(kind, style.tint, view, sampler),
            &payload,
        );
    }

    /// Reserves the next place in the order for a chunk recorded elsewhere, in the current
    /// group and under the current clip.
    pub fn chunk(&mut self) -> ChunkSlot {
        let index = self.list.parts.len();
        self.list.parts.push(Part::Reserved);
        ChunkSlot {
            index,
            list: self.list.inherit(),
        }
    }

    /// Inserts a retained chunk moved by `offset` (logical pixels, snapped to whole physical
    /// pixels), under the current clip. The chunk keeps its own groups: the current group does
    /// not apply to it.
    pub fn replay(&mut self, chunk: &RetainedChunk, offset: Vec2) {
        debug_assert_eq!(chunk.scale, self.list.scale, "replayed at another scale");
        let dx = round(offset.x * self.list.scale);
        let dy = round(offset.y * self.list.scale);
        let clip = self.list.clip();
        self.list.parts.push(Part::Replay {
            chunk: chunk.clone(),
            dx,
            dy,
            clip,
        });
    }
}

/// Members a group's shadow and glow follow exactly; beyond this, the members' box does.
pub const MAX_EFFECT_MEMBERS: u32 = 16;

/// The box a command's ink stays in, in its own units (strokes and line reach included); none
/// for glyphs, which [`Painter::text`] boxes itself, and effects.
fn local_box(kind: Kind, p: &[u32]) -> Option<[f32; 4]> {
    let w = |i: usize| f32::from_bits(p[i]);
    Some(match kind {
        Kind::Rect => {
            let e = w(8) * 0.5;
            [w(0) - e, w(1) - e, w(2) + e, w(3) + e]
        }
        Kind::Circle => {
            let e = w(2) + w(3) * 0.5;
            [w(0) - e, w(1) - e, w(0) + e, w(1) + e]
        }
        Kind::Line => {
            let e =
                w(4) * if p[5] == 2 {
                    core::f32::consts::SQRT_2
                } else {
                    1.0
                } * 0.5;
            [
                w(0).min(w(2)) - e,
                w(1).min(w(3)) - e,
                w(0).max(w(2)) + e,
                w(1).max(w(3)) + e,
            ]
        }
        Kind::Triangle => {
            let e = w(6) * 0.5;
            [
                w(0).min(w(2)).min(w(4)) - e,
                w(1).min(w(3)).min(w(5)) - e,
                w(0).max(w(2)).max(w(4)) + e,
                w(1).max(w(3)).max(w(5)) + e,
            ]
        }
        Kind::Image | Kind::Blur => [w(0), w(1), w(2), w(3)],
        Kind::Glyph | Kind::Effect | Kind::Path => return None,
    })
}

impl UiDrawList {
    /// Adds the local box `l`, mapped by `to_screen`, to every open group effect's members.
    fn union_box(&mut self, to_screen: Affine2, l: [f32; 4]) {
        let corners = [
            Vec2::new(l[0], l[1]),
            Vec2::new(l[2], l[1]),
            Vec2::new(l[0], l[3]),
            Vec2::new(l[2], l[3]),
        ]
        .map(|p| to_screen.apply(p));
        for e in &mut self.effects {
            for c in corners {
                e.bbox[0] = e.bbox[0].min(c.x);
                e.bbox[1] = e.bbox[1].min(c.y);
                e.bbox[2] = e.bbox[2].max(c.x);
                e.bbox[3] = e.bbox[3].max(c.y);
            }
        }
    }
}
