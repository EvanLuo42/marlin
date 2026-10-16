//! The GDeflate encoder: hash-chain LZ77 per tile, one Huffman (dynamic or static) or stored
//! block per tile, written through a model of the decoder's 32 streams.

use super::decode::{
    DISTANCES, LENGTHS, NUM_DIST, NUM_LITLEN, NUM_PRECODE, NUM_STREAMS, PRECODE_ORDER,
};
use super::{HEADER, ID, MAX_TILES, TILE_SIZE};
use crate::huffman::{code_lengths, deflate_codes};
use crate::matcher::{Matcher, Params, Step};
use crate::{Codec, Error, ErrorKind, Level, Result};
use alloc::vec::Vec;

/// Worst-case bytes a tile adds over its input: two stored blocks plus up to two partly used
/// 32-bit words per stream.
const TILE_OVERHEAD: usize = 512;

pub fn max_compressed_size(len: usize) -> usize {
    HEADER + len.div_ceil(TILE_SIZE) * (4 + TILE_OVERHEAD) + len
}

/// Compresses `src` as a tile stream appended to `out`.
pub fn compress(src: &[u8], level: Level, out: &mut Vec<u8>) -> Result<()> {
    let tiles = src.len().div_ceil(TILE_SIZE);
    if tiles > MAX_TILES {
        return Err(Error::new(
            Codec::GDeflate,
            ErrorKind::Unsupported,
            "input over 65535 tiles",
        ));
    }
    out.reserve(max_compressed_size(src.len()));
    let last_size = src.len() % TILE_SIZE;
    out.extend_from_slice(&[ID, ID ^ 0xFF]);
    out.extend_from_slice(&(tiles as u16).to_le_bytes());
    out.extend_from_slice(&(1 | (last_size as u32) << 2).to_le_bytes());
    let table = out.len();
    out.resize(table + 4 * tiles, 0);
    let data = out.len();
    let mut enc = TileEncoder::new(level);
    for (i, tile) in src.chunks(TILE_SIZE).enumerate() {
        let start = out.len();
        if i > 0 {
            let off = (start - data) as u32;
            out[table + 4 * i..table + 4 * i + 4].copy_from_slice(&off.to_le_bytes());
        }
        enc.compress(tile, out);
        if i + 1 == tiles {
            let size = (out.len() - start) as u32;
            out[table..table + 4].copy_from_slice(&size.to_le_bytes());
        }
    }
    Ok(())
}

/// Compresses one tile (at most 64 KiB) as a bare tile bit stream, for callers with their own
/// container.
pub fn compress_tile(src: &[u8], level: Level, out: &mut Vec<u8>) -> Result<()> {
    if src.len() > TILE_SIZE {
        return Err(Error::new(
            Codec::GDeflate,
            ErrorKind::Unsupported,
            "tile over 64 KiB",
        ));
    }
    TileEncoder::new(level).compress(src, out);
    Ok(())
}

#[derive(Clone, Copy)]
enum Item {
    Literal(u8),
    Match { len: u32, dist: u32 },
}

struct TileEncoder {
    matcher: Matcher,
    items: Vec<Item>,
}

impl TileEncoder {
    fn new(level: Level) -> Self {
        let (hash_log, depth, lazy, nice) = match level.get() {
            1 => (14, 1, 0, 8),
            2 => (15, 4, 0, 16),
            3 => (15, 8, 0, 24),
            4 => (15, 16, 1, 32),
            5 => (15, 24, 1, 48),
            6 => (15, 48, 1, 96),
            7 => (15, 100, 2, 130),
            8 => (15, 200, 2, 200),
            _ => (15, 400, 2, 258),
        };
        let p = Params {
            min_match: 3,
            // Deflate64 limits, as the reference encoder uses them.
            max_match: 65535,
            max_offset: TILE_SIZE,
            depth,
            nice,
            lazy,
            hash_log,
        };
        Self {
            matcher: Matcher::new(p, TILE_SIZE),
            items: Vec::new(),
        }
    }

    fn compress(&mut self, src: &[u8], out: &mut Vec<u8>) {
        let n = src.len();
        self.items.clear();
        self.matcher.reset(0);
        let mut rep = 0;
        let items = &mut self.items;
        self.matcher
            .parse(src, 0..n, n, n.saturating_sub(2), &mut rep, |s: Step| {
                items.extend(
                    src[s.start..s.start + s.literals]
                        .iter()
                        .map(|&b| Item::Literal(b)),
                );
                if s.len != 0 {
                    items.push(Item::Match {
                        len: s.len as u32,
                        dist: s.offset as u32,
                    });
                }
            });

        let mut w = Writer::new();
        for _ in 0..NUM_STREAMS {
            w.advance();
        }
        let codes = Codes::dynamic(&self.items);
        let fixed = Codes::fixed();
        let dynamic_cost = codes.header_bits() + codes.cost(&self.items);
        let static_cost = fixed.cost(&self.items);
        let stored_cost = 8 * n as u64 + 19 * n.div_ceil(65535).max(1) as u64;
        if stored_cost <= dynamic_cost.min(static_cost) {
            write_stored(&mut w, src);
        } else if dynamic_cost < static_cost {
            write_huffman(&mut w, &codes, &self.items);
        } else {
            write_huffman(&mut w, &fixed, &self.items);
        }
        w.finish(out);
    }
}

#[inline]
/// Lengths up to 258 use the Deflate symbols (258 is 284 with all extra bits set); longer ones
/// use 285, the Deflate64 16-bit length.
fn length_symbol(len: u32) -> usize {
    if len > 258 {
        return 28;
    }
    LENGTHS[..28].partition_point(|&(base, _)| base as u32 <= len) - 1
}

#[inline]
fn dist_symbol(dist: u32) -> usize {
    DISTANCES.partition_point(|&(base, _)| base <= dist) - 1
}

/// The codes of one block: code lengths, bit-reversed codes, and for dynamic blocks the
/// precode that describes them.
struct Codes {
    dynamic: bool,
    litlen_lens: [u8; NUM_LITLEN],
    litlen: [u16; NUM_LITLEN],
    dist_lens: [u8; NUM_DIST],
    dist: [u16; NUM_DIST],
    num_litlen: usize,
    num_dist: usize,
    pre_lens: [u8; NUM_PRECODE],
    pre: [u16; NUM_PRECODE],
    num_pre: usize,
    /// `(precode symbol, extra bits value)`.
    pre_items: Vec<(u8, u8)>,
}

impl Codes {
    fn fixed() -> Self {
        let mut c = Self::empty(false);
        c.litlen_lens[..144].fill(8);
        c.litlen_lens[144..256].fill(9);
        c.litlen_lens[256..280].fill(7);
        c.litlen_lens[280..].fill(8);
        c.dist_lens.fill(5);
        deflate_codes(&c.litlen_lens, &mut c.litlen);
        deflate_codes(&c.dist_lens, &mut c.dist);
        c
    }

    fn empty(dynamic: bool) -> Self {
        Self {
            dynamic,
            litlen_lens: [0; NUM_LITLEN],
            litlen: [0; NUM_LITLEN],
            dist_lens: [0; NUM_DIST],
            dist: [0; NUM_DIST],
            num_litlen: NUM_LITLEN,
            num_dist: NUM_DIST,
            pre_lens: [0; NUM_PRECODE],
            pre: [0; NUM_PRECODE],
            num_pre: NUM_PRECODE,
            pre_items: Vec::new(),
        }
    }

    fn dynamic(items: &[Item]) -> Self {
        let mut c = Self::empty(true);
        let mut lf = [0u32; NUM_LITLEN];
        let mut df = [0u32; NUM_DIST];
        for &it in items {
            match it {
                Item::Literal(b) => lf[b as usize] += 1,
                Item::Match { len, dist } => {
                    lf[257 + length_symbol(len)] += 1;
                    df[dist_symbol(dist)] += 1;
                }
            }
        }
        lf[256] += 1;
        code_lengths(&lf[..286], 15, &mut c.litlen_lens);
        complete(&mut c.litlen_lens[..286]);
        code_lengths(&df, 15, &mut c.dist_lens);
        complete(&mut c.dist_lens);
        deflate_codes(&c.litlen_lens, &mut c.litlen);
        deflate_codes(&c.dist_lens, &mut c.dist);
        c.num_litlen = (c.litlen_lens.iter().rposition(|&l| l > 0).unwrap() + 1).max(257);
        c.num_dist = c.dist_lens.iter().rposition(|&l| l > 0).unwrap() + 1;

        let mut all = Vec::with_capacity(c.num_litlen + c.num_dist);
        all.extend_from_slice(&c.litlen_lens[..c.num_litlen]);
        all.extend_from_slice(&c.dist_lens[..c.num_dist]);
        c.pre_items = precode_items(&all);
        let mut pf = [0u32; NUM_PRECODE];
        for &(sym, _) in &c.pre_items {
            pf[sym as usize] += 1;
        }
        code_lengths(&pf, 7, &mut c.pre_lens);
        complete(&mut c.pre_lens);
        deflate_codes(&c.pre_lens, &mut c.pre);
        c.num_pre = (PRECODE_ORDER
            .iter()
            .rposition(|&s| c.pre_lens[s] > 0)
            .unwrap()
            + 1)
        .max(4);
        c
    }

    fn header_bits(&self) -> u64 {
        let items: u64 = self
            .pre_items
            .iter()
            .map(|&(sym, _)| self.pre_lens[sym as usize] as u64 + extra_pre_bits(sym) as u64)
            .sum();
        14 + 3 * self.num_pre as u64 + items
    }

    fn cost(&self, items: &[Item]) -> u64 {
        let mut bits = self.litlen_lens[256] as u64;
        for &it in items {
            bits += match it {
                Item::Literal(b) => self.litlen_lens[b as usize] as u64,
                Item::Match { len, dist } => {
                    let l = length_symbol(len);
                    let d = dist_symbol(dist);
                    (self.litlen_lens[257 + l] + LENGTHS[l].1 + self.dist_lens[d] + DISTANCES[d].1)
                        as u64
                }
            };
        }
        bits
    }
}

/// Makes a code with fewer than two used symbols complete, so every decoder accepts it.
fn complete(lens: &mut [u8]) {
    let used = lens.iter().filter(|&&l| l > 0).count();
    if used < 2 {
        // Give the used symbol (or symbol 0) and one neighbour length 1.
        let s = lens.iter().position(|&l| l > 0).unwrap_or(0);
        lens.fill(0);
        lens[s] = 1;
        lens[if s == 0 { 1 } else { 0 }] = 1;
    }
}

fn extra_pre_bits(sym: u8) -> u32 {
    match sym {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    }
}

/// Run-length codes the code lengths with precode symbols 16 (repeat previous 3-6 times), 17
/// (3-10 zeros) and 18 (11-138 zeros).
fn precode_items(lens: &[u8]) -> Vec<(u8, u8)> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < lens.len() {
        let v = lens[i];
        let mut run = lens[i..].iter().take_while(|&&l| l == v).count();
        i += run;
        if v == 0 {
            while run >= 11 {
                let k = run.min(138);
                items.push((18, (k - 11) as u8));
                run -= k;
            }
            if run >= 3 {
                items.push((17, (run - 3) as u8));
                run = 0;
            }
        } else {
            items.push((v, 0));
            run -= 1;
            while run >= 3 {
                let k = run.min(6);
                items.push((16, (k - 3) as u8));
                run -= k;
            }
        }
        items.extend(core::iter::repeat_n((v, 0), run));
    }
    items
}

fn write_header(w: &mut Writer, last: bool, kind: u32) {
    w.idx = 0;
    w.put(last as u32, 1);
    w.put(kind, 2);
    w.ensure();
}

/// Stored blocks: a 16-bit length on stream 0, then one byte per stream turn.
fn write_stored(w: &mut Writer, src: &[u8]) {
    let mut chunks = src.chunks(65535).peekable();
    if src.is_empty() {
        write_header(w, true, 0);
        w.put(0, 16);
        end_block(w, &mut [None; NUM_STREAMS]);
        return;
    }
    while let Some(chunk) = chunks.next() {
        write_header(w, chunks.peek().is_none(), 0);
        w.put(chunk.len() as u32, 16);
        for &b in chunk {
            w.put(b as u32, 8);
            w.advance();
        }
        end_block(w, &mut [None; NUM_STREAMS]);
    }
}

fn write_huffman(w: &mut Writer, c: &Codes, items: &[Item]) {
    write_header(w, true, if c.dynamic { 2 } else { 1 });
    if c.dynamic {
        w.put((c.num_litlen - 257) as u32, 5);
        w.put((c.num_dist - 1) as u32, 5);
        w.put((c.num_pre - 4) as u32, 4);
        w.ensure();
        for &s in &PRECODE_ORDER[..c.num_pre] {
            w.put(c.pre_lens[s] as u32, 3);
            w.advance();
        }
        w.idx = 0;
        for &(sym, extra) in &c.pre_items {
            w.put(c.pre[sym as usize] as u32, c.pre_lens[sym as usize] as u32);
            w.put(extra as u32, extra_pre_bits(sym));
            w.advance();
        }
    }
    w.idx = 0;
    // Each match's offset waits for its stream's next turn: `(bits, count)`.
    let mut pending: [Option<(u32, u32)>; NUM_STREAMS] = [None; NUM_STREAMS];
    let mut items = items.iter();
    loop {
        let i = w.idx;
        if let Some((bits, n)) = pending[i].take() {
            w.put(bits, n);
        } else {
            match items.next() {
                Some(&Item::Literal(b)) => {
                    w.put(
                        c.litlen[b as usize] as u32,
                        c.litlen_lens[b as usize] as u32,
                    );
                }
                Some(&Item::Match { len, dist }) => {
                    let l = length_symbol(len);
                    let (base, extra) = LENGTHS[l];
                    w.put(c.litlen[257 + l] as u32, c.litlen_lens[257 + l] as u32);
                    w.put(len - base as u32, extra as u32);
                    let d = dist_symbol(dist);
                    let (base, extra) = DISTANCES[d];
                    let code_len = c.dist_lens[d] as u32;
                    pending[i] = Some((
                        c.dist[d] as u32 | (dist - base) << code_len,
                        code_len + extra as u32,
                    ));
                }
                None => {
                    w.put(c.litlen[256] as u32, c.litlen_lens[256] as u32);
                    break;
                }
            }
        }
        w.advance();
    }
    end_block(w, &mut pending);
}

/// The decoder's end of block: a full round from the current stream, finishing deferred
/// copies.
fn end_block(w: &mut Writer, pending: &mut [Option<(u32, u32)>; NUM_STREAMS]) {
    for _ in 0..NUM_STREAMS {
        if let Some((bits, n)) = pending[w.idx].take() {
            w.put(bits, n);
        }
        w.advance();
    }
}

/// The 32 output streams, plus a model of when the decoder refills each one: that order is
/// the order of the 32-bit words in the output.
struct Writer {
    words: [Vec<u32>; NUM_STREAMS],
    acc: [u64; NUM_STREAMS],
    nacc: [u32; NUM_STREAMS],
    /// Bits written (= bits the decoder consumes) per stream.
    bits: [u32; NUM_STREAMS],
    /// Words the decoder has loaded per stream.
    loaded: [u32; NUM_STREAMS],
    order: Vec<u8>,
    idx: usize,
}

impl Writer {
    fn new() -> Self {
        Self {
            words: core::array::from_fn(|_| Vec::new()),
            acc: [0; NUM_STREAMS],
            nacc: [0; NUM_STREAMS],
            bits: [0; NUM_STREAMS],
            loaded: [0; NUM_STREAMS],
            order: Vec::new(),
            idx: 0,
        }
    }

    #[inline]
    fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        let i = self.idx;
        self.acc[i] |= (v as u64 & ((1u64 << n) - 1)) << self.nacc[i];
        self.nacc[i] += n;
        self.bits[i] += n;
        debug_assert!(self.bits[i] <= 32 * self.loaded[i], "decoder would run dry");
        if self.nacc[i] >= 32 {
            self.words[i].push(self.acc[i] as u32);
            self.acc[i] >>= 32;
            self.nacc[i] -= 32;
        }
    }

    /// The decoder's `ENSURE_BITS(32)` on the current stream.
    #[inline]
    fn ensure(&mut self) {
        let i = self.idx;
        if 32 * self.loaded[i] - self.bits[i] < 32 {
            self.order.push(i as u8);
            self.loaded[i] += 1;
        }
    }

    #[inline]
    fn advance(&mut self) {
        self.ensure();
        self.idx = (self.idx + 1) % NUM_STREAMS;
    }

    fn finish(mut self, out: &mut Vec<u8>) {
        for i in 0..NUM_STREAMS {
            if self.nacc[i] > 0 {
                self.words[i].push(self.acc[i] as u32);
            }
        }
        let mut next = [0usize; NUM_STREAMS];
        out.reserve(4 * self.order.len());
        for &s in &self.order {
            let s = s as usize;
            let word = self.words[s].get(next[s]).copied().unwrap_or(0);
            next[s] += 1;
            out.extend_from_slice(&word.to_le_bytes());
        }
    }
}
