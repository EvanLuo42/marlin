//! Paths (§4.2): contours of lines, quadratic and cubic Béziers, filled by the non-zero or the
//! even-odd rule.
//!
//! The painter flattens a path into line segments on the screen when it records it (curves to
//! within [`FLATTEN_TOLERANCE`] of a pixel). Each frame the GPU walks every segment through the
//! 16 × 16 tiles it crosses ([`walk`]): a tile gets the segments passing through it, and its
//! backdrop, the winding number at its top-left corner, is a prefix sum along its row of where
//! segments cross the tops of tiles. A pixel's coverage is its tile's backdrop plus the exact
//! signed area its tile's segments sweep to its right ([`segment_coverage`]).

use alloc::vec::Vec;

use marlin_math::Vec2;
use marlin_math::scalar::{ceil, floor, sqrt};

use crate::command::TILE;
use crate::style::Affine2;

/// How far flattened curves stray from the true curve, in screen pixels.
pub const FLATTEN_TOLERANCE: f32 = 0.05;

/// Which points a path covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FillRule {
    /// Where the winding number is not zero.
    #[default]
    NonZero,
    /// Where the winding number is odd.
    EvenOdd,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Verb {
    Move(Vec2),
    Line(Vec2),
    Quad(Vec2, Vec2),
    Cubic(Vec2, Vec2, Vec2),
    Close,
}

/// Contours in logical pixels. Every contour is closed when filled.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Path {
    verbs: Vec<Verb>,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, p: Vec2) -> &mut Self {
        self.verbs.push(Verb::Move(p));
        self
    }

    pub fn line_to(&mut self, p: Vec2) -> &mut Self {
        self.verbs.push(Verb::Line(p));
        self
    }

    pub fn quad_to(&mut self, c: Vec2, p: Vec2) -> &mut Self {
        self.verbs.push(Verb::Quad(c, p));
        self
    }

    pub fn cubic_to(&mut self, c1: Vec2, c2: Vec2, p: Vec2) -> &mut Self {
        self.verbs.push(Verb::Cubic(c1, c2, p));
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.verbs.push(Verb::Close);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }

    /// The path's segments through `to_screen`, curves flattened, every contour closed.
    pub fn flatten(&self, to_screen: Affine2) -> Vec<[f32; 4]> {
        let mut out = Vec::new();
        let (mut start, mut at) = (Vec2::ZERO, Vec2::ZERO);
        let mut open = false;
        let line = |out: &mut Vec<[f32; 4]>, a: Vec2, b: Vec2| {
            if a != b {
                out.push([a.x, a.y, b.x, b.y]);
            }
        };
        let tf = |p: Vec2| to_screen.apply(p);
        for v in &self.verbs {
            match *v {
                Verb::Move(p) => {
                    if open {
                        line(&mut out, at, start);
                    }
                    start = tf(p);
                    at = start;
                    open = true;
                }
                Verb::Line(p) => {
                    let p = tf(p);
                    line(&mut out, at, p);
                    at = p;
                    open = true;
                }
                Verb::Quad(c, p) => {
                    let (c, p) = (tf(c), tf(p));
                    let dd = (at - c * 2.0 + p).length();
                    let n = ceil(sqrt(dd / (8.0 * FLATTEN_TOLERANCE))).max(1.0) as u32;
                    let p0 = at;
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        let q = p0 * (u * u) + c * (2.0 * u * t) + p * (t * t);
                        line(&mut out, at, q);
                        at = q;
                    }
                    at = p;
                    open = true;
                }
                Verb::Cubic(c1, c2, p) => {
                    let (c1, c2, p) = (tf(c1), tf(c2), tf(p));
                    let p0 = at;
                    let dd = (p0 - c1 * 2.0 + c2)
                        .length()
                        .max((c1 - c2 * 2.0 + p).length());
                    let n = ceil(sqrt(0.75 * dd / FLATTEN_TOLERANCE)).max(1.0) as u32;
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        let q = p0 * (u * u * u)
                            + c1 * (3.0 * u * u * t)
                            + c2 * (3.0 * u * t * t)
                            + p * (t * t * t);
                        line(&mut out, at, q);
                        at = q;
                    }
                    at = p;
                    open = true;
                }
                Verb::Close => {
                    if open {
                        line(&mut out, at, start);
                    }
                    at = start;
                    open = false;
                }
            }
        }
        if open {
            line(&mut out, at, start);
        }
        out
    }
}

/// A path command's payload for screen segments `segs` (see `crate::command`), or `None` for a
/// path with no area.
pub(crate) fn payload(segs: &[[f32; 4]], rule: FillRule) -> Option<Vec<u32>> {
    // Horizontal segments sweep no area, but stay: where they cross a tile's left side they
    // change the winding number below them.
    if segs.iter().all(|s| s[1] == s[3]) {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut visits = 0u32;
    for s in segs {
        x0 = x0.min(s[0]).min(s[2]);
        y0 = y0.min(s[1]).min(s[3]);
        x1 = x1.max(s[0]).max(s[2]);
        y1 = y1.max(s[1]).max(s[3]);
        visits = visits.saturating_add(visit_bound(*s));
    }
    let len = SEGMENTS + 4 * segs.len();
    let mut p = Vec::with_capacity(len);
    p.extend_from_slice(&[
        match rule {
            FillRule::NonZero => 0,
            FillRule::EvenOdd => 1,
        },
        len as u32,
        segs.len() as u32,
        visits,
        0,
        x0.to_bits(),
        y0.to_bits(),
        x1.to_bits(),
        y1.to_bits(),
    ]);
    for s in segs {
        p.extend(s.iter().map(|v| v.to_bits()));
    }
    debug_assert_eq!(p.len(), len);
    Some(p)
}

/// The payload word a path's segments start at.
pub const SEGMENTS: usize = 9;

/// The most tiles segment `s` visits ([`walk`]), wherever whole pixels move it: one more than
/// the tile columns and rows it crosses.
fn visit_bound(s: [f32; 4]) -> u32 {
    let k = 1.0 / TILE as f32;
    (floor((s[2] - s[0]).abs() * k) + floor((s[3] - s[1]).abs() * k)) as u32 + 3
}

/// What [`walk`] meets along a segment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The segment passes through tile `(x, y)`; `edge`: it crosses the tile's left side there.
    Tile { x: i32, y: i32, edge: bool },
    /// The segment crosses the top of tile row `y` left of tile `x`: the winding number of
    /// every tile from `x` on in that row changes by `d` (§4.2's backdrop, before the prefix
    /// sum along the row).
    Backdrop { x: i32, y: i32, d: i32 },
}

/// The tile column (or row) holding coordinate `v`, with everything before tile `lo` in `lo - 1`
/// and everything from `hi` on in `hi`.
fn cell(v: f32, lo: i32, hi: i32) -> i32 {
    floor(v * (1.0 / TILE as f32)).clamp((lo - 1) as f32, hi as f32) as i32
}

/// Walks segment `s` through the tiles of `grid` (x0 y0 x1 y1 in tiles, half-open), in order:
/// what `UiPathTiles` does on the GPU for each segment (0012 §4.2). Columns left of the grid
/// count as one, so do rows above it, and columns and rows past it.
///
/// Which side of a corner the segment passes is decided by comparing products of differences
/// (no division), which round the same on the CPU and the GPU; and the order in which it meets
/// two grid lines does not depend on the other lines, so a smaller grid sees the same steps in
/// its tiles.
pub fn walk(s: [f32; 4], grid: [i32; 4], mut visit: impl FnMut(Step)) {
    let [x0, y0, x1, y1] = s;
    let [gx0, gy0, gx1, gy1] = grid;
    let (mut c, mut r) = (cell(x0, gx0, gx1), cell(y0, gy0, gy1));
    let (c1, r1) = (cell(x1, gx0, gx1), cell(y1, gy0, gy1));
    let (sx, sy) = ((c1 - c).signum(), (r1 - r).signum());
    let (mut nx, mut ny) = ((c1 - c).abs(), (r1 - r).abs());
    let (adx, ady) = ((x1 - x0).abs(), (y1 - y0).abs());
    let t = TILE as i32;
    let mut edge = false;
    loop {
        // The next grid line is a column's when the segment meets it first.
        let x_step = nx > 0
            && (ny == 0 || {
                let bx = (t * if sx > 0 { c + 1 } else { c }) as f32;
                let by = (t * if sy > 0 { r + 1 } else { r }) as f32;
                (bx - x0) * sx as f32 * ady < (by - y0) * sy as f32 * adx
            });
        if gx0 <= c && c < gx1 && gy0 <= r && r < gy1 {
            visit(Step::Tile {
                x: c,
                y: r,
                edge: edge || (x_step && sx < 0),
            });
        }
        if nx == 0 && ny == 0 {
            break;
        }
        if x_step {
            c += sx;
            nx -= 1;
            edge = sx > 0;
        } else {
            let row = if sy > 0 { r + 1 } else { r };
            if gy0 <= row && row < gy1 && c + 1 < gx1 {
                visit(Step::Backdrop {
                    x: c + 1,
                    y: row,
                    d: sy,
                });
            }
            r += sy;
            ny -= 1;
            edge = false;
        }
    }
}

/// One tile of a path: the winding number at its top-left corner, and the segments (their
/// indices in the payload) that pass through it, each with whether it crosses its left side.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct PathTile {
    pub backdrop: i32,
    pub segments: Vec<(u32, bool)>,
}

/// Segment `i` of the path whose payload is `p`.
pub fn segment(p: &[u32], i: usize) -> [f32; 4] {
    let at = SEGMENTS + 4 * i;
    [0, 1, 2, 3].map(|k| f32::from_bits(p[at + k]))
}

/// Tile `(x, y)` of the path whose payload is `p`, as the GPU builds it (the segments in
/// payload order; the GPU's are in any order).
pub fn tile(p: &[u32], x: i32, y: i32) -> PathTile {
    let mut out = PathTile::default();
    let t = TILE as f32;
    let (top, bottom) = (y as f32 * t, (y + 1) as f32 * t);
    for i in 0..p[2] as usize {
        let s = segment(p, i);
        // Only segments reaching the row or its top line meet the tile.
        if s[1].max(s[3]) < top || s[1].min(s[3]) >= bottom {
            continue;
        }
        walk(s, [x, y, x + 1, y + 1], |step| match step {
            Step::Tile { edge, .. } => out.segments.push((i as u32, edge)),
            Step::Backdrop { d, .. } => out.backdrop += d,
        });
    }
    out
}

/// Fixed-point units of coverage the shading sums in: integers add the same in any order, so a
/// tile's pixels do not depend on the order the GPU listed its segments in.
pub const COVERAGE_UNIT: f32 = 1_048_576.0;

/// The segments tile `(x, y)` shades for segment `s`, which the walk listed there: `s` itself,
/// or when it crosses the tile's left side (`edge`), its part right of that side, and a
/// vertical segment on the side from the crossing to the tile's bottom that changes the
/// winding number below the crossing as the part left of the side did. A pixel's coverage is
/// its tile's backdrop and the signed areas these sweep to its right ([`cell_area`]).
pub fn entries(s: [f32; 4], edge: bool, x: i32, y: i32, out: &mut Vec<[f32; 4]>) {
    if !edge {
        out.push(s);
        return;
    }
    let x0 = (x * TILE as i32) as f32;
    let bottom = ((y + 1) * TILE as i32) as f32;
    let ye = s[1] + (s[3] - s[1]) * ((x0 - s[0]) / (s[2] - s[0]));
    if s[0] < x0 {
        out.push([x0, ye, s[2], s[3]]);
        out.push([x0, bottom, x0, ye]);
    } else {
        out.push([s[0], s[1], x0, ye]);
        out.push([x0, ye, x0, bottom]);
    }
}

/// What segment `s` (a tile's entry) adds to the pixel cell `(px, py)`, in [`COVERAGE_UNIT`]s.
pub fn entry_coverage(s: [f32; 4], px: f32, py: f32) -> i32 {
    floor(cell_area(s, px, py) * COVERAGE_UNIT + 0.5) as i32
}

/// The signed area segment `s` sweeps to the right inside pixel cell `(px, py)` (its top-left
/// corner): what the cell's coverage sums.
pub fn cell_area(s: [f32; 4], px: f32, py: f32) -> f32 {
    let (x0, y0, x1, y1) = (s[0], s[1], s[2], s[3]);
    if y0 == y1 {
        return 0.0;
    }
    let dir = if y1 > y0 { 1.0 } else { -1.0 };
    let ya = y0.min(y1).max(py);
    let yb = y0.max(y1).min(py + 1.0);
    if yb <= ya {
        return 0.0;
    }
    let dxdy = (x1 - x0) / (y1 - y0);
    let ta = x0 + (ya - y0) * dxdy - px;
    let tb = x0 + (yb - y0) * dxdy - px;
    // ∫ clamp(1 - t, 0, 1) dt: t for t ≤ 0, t - t²/2 between, ½ beyond 1.
    let g = |t: f32| {
        if t <= 0.0 {
            t
        } else if t >= 1.0 {
            0.5
        } else {
            t - t * t * 0.5
        }
    };
    let h = |t: f32| (1.0 - t).clamp(0.0, 1.0);
    let avg = if (tb - ta).abs() < 1e-6 {
        h(ta)
    } else {
        (g(tb) - g(ta)) / (tb - ta)
    };
    dir * avg * (yb - ya)
}

/// Words of a path's record for the GPU: its payload offset, its tile grid x0 y0 x1 y1, its
/// first tile in the frame's path tiles, and its first segment and first tile row among the
/// frame's (those of paths with tiles, in order).
pub const PATH_RECORD_WORDS: usize = 8;
/// Words of a path tile on the GPU: the backdrop (`i32`), its entries ([`entries`]), its first
/// entry, the entries written.
pub const PATH_TILE_WORDS: usize = 4;
/// The most path tile entries (four words each) a frame has room for (128 MiB).
pub const MAX_PATH_ENTRIES: u64 = 1 << 23;

/// Where a surface's frame keeps its paths' tiles (§4.2): per path a record, its tile grid the
/// tiles its bounds touch. The GPU takes the segments of every path with tiles, and their tile
/// rows, as one run each, a thread per segment or row.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct PathLayout {
    /// Per path, in command order (a path's index in its payload): [`PATH_RECORD_WORDS`] words.
    pub records: Vec<[u32; PATH_RECORD_WORDS]>,
    /// Tiles of every grid.
    pub tiles: u32,
    /// Room for every path's tile entries.
    pub entries: u32,
    /// Per segment of the run: its path.
    pub segment_paths: Vec<u32>,
    /// Per tile row of the run: its path.
    pub row_paths: Vec<u32>,
}

/// The path layout of `chunk` on a `width` × `height` surface.
pub fn layout(
    chunk: &crate::command::Chunk,
    glyphs: &[marlin_font::GlyphRecord],
    width: u32,
    height: u32,
) -> PathLayout {
    use crate::command::Kind;
    let scene = crate::shape::Scene::new(chunk, glyphs);
    let mut out = PathLayout::default();
    let mut entries = 0u64;
    let t = TILE as i32;
    for c in &chunk.commands {
        if Kind::from_word(c[0]) != Some(Kind::Path) {
            continue;
        }
        let p = &chunk.payload[c[3] as usize..];
        let b = crate::shape::bounds(c, &scene, width, height);
        let grid = if b.is_empty() {
            [0; 4]
        } else {
            [
                b.bbox[0] / t,
                b.bbox[1] / t,
                (b.bbox[2] + t - 1) / t,
                (b.bbox[3] + t - 1) / t,
            ]
        };
        let (w, h) = ((grid[2] - grid[0]) as u32, (grid[3] - grid[1]) as u32);
        out.records.push([
            c[3],
            grid[0] as u32,
            grid[1] as u32,
            grid[2] as u32,
            grid[3] as u32,
            out.tiles,
            out.segment_paths.len() as u32,
            out.row_paths.len() as u32,
        ]);
        if w * h == 0 {
            continue;
        }
        let path = out.records.len() as u32 - 1;
        out.segment_paths
            .extend(core::iter::repeat_n(path, p[2] as usize));
        out.row_paths.extend(core::iter::repeat_n(path, h as usize));
        out.tiles += w * h;
        // No segment visits more of the grid than a row and a column of it, and each visit
        // writes at most two entries.
        entries += 2 * u64::from(p[3]).min(u64::from(p[2]) * u64::from(w + h - 1));
    }
    out.entries = entries.min(MAX_PATH_ENTRIES) as u32;
    out
}
