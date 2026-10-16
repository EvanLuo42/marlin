//! The tile decoder: Deflate over 32 round-robin bit streams with deferred match copies.

use super::corrupt;
use crate::util::{copy_match, le32};
use crate::{Codec, Error, ErrorKind, Result};

pub(crate) const NUM_STREAMS: usize = 32;
const MAX_CODE_LEN: usize = 15;
const TABLE_BITS: u32 = 10;

pub(crate) const NUM_LITLEN: usize = 288;
pub(crate) const NUM_DIST: usize = 32;
pub(crate) const NUM_PRECODE: usize = 19;

/// The order precode lengths are stored in.
pub(crate) const PRECODE_ORDER: [usize; NUM_PRECODE] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// `(base, extra bits)` of length symbols 257..=287. GDeflate is Deflate64-based: 285 is any
/// length 3..=65538 in 16 extra bits (not 258), and the reference decodes 286 and 287 the same.
#[rustfmt::skip]
pub(crate) const LENGTHS: [(u16, u8); 31] = [
    (3, 0), (4, 0), (5, 0), (6, 0), (7, 0), (8, 0), (9, 0), (10, 0),
    (11, 1), (13, 1), (15, 1), (17, 1), (19, 2), (23, 2), (27, 2), (31, 2),
    (35, 3), (43, 3), (51, 3), (59, 3), (67, 4), (83, 4), (99, 4), (115, 4),
    (131, 5), (163, 5), (195, 5), (227, 5), (3, 16), (3, 16), (3, 16),
];

/// `(base, extra bits)` of distance symbols. Deflate64 adds 30 and 31, reaching 64 KiB back:
/// a whole tile.
#[rustfmt::skip]
pub(crate) const DISTANCES: [(u32, u8); NUM_DIST] = [
    (1, 0), (2, 0), (3, 0), (4, 0), (5, 1), (7, 1), (9, 2), (13, 2),
    (17, 3), (25, 3), (33, 4), (49, 4), (65, 5), (97, 5), (129, 6), (193, 6),
    (257, 7), (385, 7), (513, 8), (769, 8), (1025, 9), (1537, 9), (2049, 10), (3073, 10),
    (4097, 11), (6145, 11), (8193, 12), (12289, 12), (16385, 13), (24577, 13),
    (32769, 14), (49153, 14),
];

/// A canonical Huffman decoder: a direct table for codes up to `TABLE_BITS`, and the
/// count/symbol arrays of the classic canonical decoder for longer ones.
struct Huffman<const N: usize> {
    /// `symbol | length << 12`; 0 for entries a longer code (or no code) owns.
    table: [u16; 1 << TABLE_BITS],
    count: [u16; MAX_CODE_LEN + 1],
    /// Symbols by code length, then value.
    sorted: [u16; N],
}

impl<const N: usize> Huffman<N> {
    const fn new() -> Self {
        Self {
            table: [0; 1 << TABLE_BITS],
            count: [0; MAX_CODE_LEN + 1],
            sorted: [0; N],
        }
    }

    /// Builds from code lengths. Accepts complete codes, the empty code, and a single code of
    /// length 1, as the reference does.
    fn build(&mut self, lens: &[u8]) -> Result<()> {
        self.count = [0; MAX_CODE_LEN + 1];
        for &l in lens {
            self.count[l as usize] += 1;
        }
        self.count[0] = 0;
        let mut left: i32 = 1;
        for &count in &self.count[1..] {
            left = (left << 1) - count as i32;
            if left < 0 {
                return Err(corrupt("oversubscribed huffman code"));
            }
        }
        let used: u16 = self.count.iter().sum();
        if left != 0 && !(used == 0 || (used == 1 && self.count[1] == 1)) {
            return Err(corrupt("incomplete huffman code"));
        }
        let mut offs = [0u16; MAX_CODE_LEN + 2];
        for len in 1..=MAX_CODE_LEN {
            offs[len + 1] = offs[len] + self.count[len];
        }
        let mut next_code = [0u32; MAX_CODE_LEN + 1];
        let mut code = 0u32;
        for len in 1..=MAX_CODE_LEN {
            code = (code + self.count[len - 1] as u32) << 1;
            next_code[len] = code;
        }
        self.table.fill(0);
        for (sym, &l) in lens.iter().enumerate() {
            let l = l as usize;
            if l == 0 {
                continue;
            }
            self.sorted[offs[l] as usize] = sym as u16;
            offs[l] += 1;
            let c = next_code[l];
            next_code[l] += 1;
            if l as u32 <= TABLE_BITS {
                // Codes go into the stream MSB first, so the table is indexed by the reversed
                // code; every index sharing those low bits decodes to this symbol.
                let rev = (c.reverse_bits() >> (32 - l)) as usize;
                let entry = sym as u16 | (l as u16) << 12;
                let mut i = rev;
                while i < self.table.len() {
                    self.table[i] = entry;
                    i += 1 << l;
                }
            }
        }
        Ok(())
    }

    /// Decodes one symbol from the low bits of `bits` (at least 15 valid bits); returns
    /// `(symbol, length)`.
    #[inline]
    fn decode(&self, bits: u32) -> Result<(usize, u32)> {
        let e = self.table[(bits & ((1 << TABLE_BITS) - 1)) as usize];
        if e != 0 {
            return Ok(((e & 0xFFF) as usize, (e >> 12) as u32));
        }
        // Long code: walk the canonical code one bit at a time.
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=MAX_CODE_LEN {
            code |= ((bits >> (len - 1)) & 1) as i32;
            let count = self.count[len] as i32;
            if code - first < count {
                return Ok((
                    self.sorted[(index + code - first) as usize] as usize,
                    len as u32,
                ));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(corrupt("invalid huffman code"))
    }
}

/// The 32 bit streams over one tile's input.
struct Streams<'a> {
    src: &'a [u8],
    pos: usize,
    buf: [u64; NUM_STREAMS],
    left: [u32; NUM_STREAMS],
    idx: usize,
}

impl Streams<'_> {
    /// Tops the current stream up to at least 32 bits from the shared input.
    #[inline]
    fn ensure(&mut self) -> Result<()> {
        let i = self.idx;
        if self.left[i] < 32 {
            let Some(word) = le32(self.src, self.pos) else {
                return Err(corrupt("truncated tile"));
            };
            self.pos += 4;
            self.buf[i] |= (word as u64) << self.left[i];
            self.left[i] += 32;
        }
        Ok(())
    }

    /// Refills the current stream, then moves to the next.
    #[inline]
    fn advance(&mut self) -> Result<()> {
        self.ensure()?;
        self.idx = (self.idx + 1) % NUM_STREAMS;
        Ok(())
    }

    #[inline]
    fn peek(&self) -> u32 {
        self.buf[self.idx] as u32
    }

    #[inline]
    fn consume(&mut self, n: u32) {
        let i = self.idx;
        // Every turn reads at most 29 bits after a refill to 32, so this never runs dry on
        // input that passed `ensure`.
        self.buf[i] >>= n;
        self.left[i] = self.left[i].saturating_sub(n);
    }

    #[inline]
    fn pop(&mut self, n: u32) -> u32 {
        let v = self.peek() & ((1u32 << n) - 1);
        self.consume(n);
        v
    }

    #[inline]
    fn decode<const N: usize>(&mut self, h: &Huffman<N>) -> Result<usize> {
        let (sym, len) = h.decode(self.peek())?;
        self.consume(len);
        Ok(sym)
    }
}

struct Tables {
    litlen: Huffman<NUM_LITLEN>,
    dist: Huffman<NUM_DIST>,
    precode: Huffman<NUM_PRECODE>,
    /// What `litlen`/`dist` hold: the static codes are reused across blocks.
    static_loaded: bool,
}

/// Decodes one tile's bit stream (`src` is exactly the tile's compressed bytes) into `dst`;
/// returns the decoded size, at most 64 KiB.
pub fn decompress_tile(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    let mut t = Tables {
        litlen: Huffman::new(),
        dist: Huffman::new(),
        precode: Huffman::new(),
        static_loaded: false,
    };
    let mut s = Streams {
        src,
        pos: 0,
        buf: [0; NUM_STREAMS],
        left: [0; NUM_STREAMS],
        idx: 0,
    };
    for _ in 0..NUM_STREAMS {
        s.advance()?;
    }
    // Deferred copies: one per stream, `(length, destination)`, live when the bit is set.
    let mut copies = [(0u32, 0usize); NUM_STREAMS];
    let mut pending = 0u32;
    let mut out = 0usize;
    loop {
        s.idx = 0;
        let last = s.pop(1) != 0;
        let kind = s.pop(2);
        s.ensure()?;
        match kind {
            0 => {
                let len = s.pop(16) as usize;
                if len > dst.len() - out {
                    return Err(too_small());
                }
                for _ in 0..len {
                    dst[out] = s.pop(8) as u8;
                    out += 1;
                    s.advance()?;
                }
            }
            1 | 2 => {
                if kind == 2 {
                    read_dynamic_header(&mut s, &mut t)?;
                } else if !t.static_loaded {
                    let mut lens = [0u8; NUM_LITLEN + NUM_DIST];
                    lens[..144].fill(8);
                    lens[144..256].fill(9);
                    lens[256..280].fill(7);
                    lens[280..NUM_LITLEN].fill(8);
                    lens[NUM_LITLEN..].fill(5);
                    t.litlen.build(&lens[..NUM_LITLEN])?;
                    t.dist.build(&lens[NUM_LITLEN..])?;
                    t.static_loaded = true;
                }
                s.idx = 0;
                loop {
                    let i = s.idx;
                    if pending & (1 << i) != 0 {
                        run_copy(&mut s, &t.dist, copies[i], dst)?;
                        pending &= !(1 << i);
                    } else {
                        let sym = s.decode(&t.litlen)?;
                        if sym < 256 {
                            let Some(o) = dst.get_mut(out) else {
                                return Err(too_small());
                            };
                            *o = sym as u8;
                            out += 1;
                        } else if sym == 256 {
                            break;
                        } else {
                            let (base, extra) = LENGTHS[sym - 257];
                            let len = base as u32 + s.pop(extra as u32);
                            if len as usize > dst.len() - out {
                                return Err(too_small());
                            }
                            copies[i] = (len, out);
                            pending |= 1 << i;
                            out += len as usize;
                        }
                    }
                    s.advance()?;
                }
            }
            _ => return Err(corrupt("reserved block type")),
        }
        // End of block: every stream finishes its deferred copy, starting with the one that
        // read the end-of-block symbol.
        for _ in 0..NUM_STREAMS {
            let i = s.idx;
            if pending & (1 << i) != 0 {
                run_copy(&mut s, &t.dist, copies[i], dst)?;
                pending &= !(1 << i);
            }
            s.advance()?;
        }
        if last {
            return Ok(out);
        }
    }
}

#[inline]
fn too_small() -> Error {
    Error::new(Codec::GDeflate, ErrorKind::OutputTooSmall, "tile")
}

/// A deferred copy: its offset is read now, from the stream that read its length.
#[inline]
fn run_copy(
    s: &mut Streams,
    dist: &Huffman<NUM_DIST>,
    (len, at): (u32, usize),
    dst: &mut [u8],
) -> Result<()> {
    let sym = s.decode(dist)?;
    let (base, extra) = DISTANCES[sym];
    let offset = (base + s.pop(extra as u32)) as usize;
    if offset > at {
        return Err(corrupt("offset before start of tile"));
    }
    copy_match(dst, at, offset, len as usize);
    Ok(())
}

/// The code lengths of a dynamic block: counts on stream 0, precode lengths one per stream,
/// then the run-length coded lengths one item per stream.
fn read_dynamic_header(s: &mut Streams, t: &mut Tables) -> Result<()> {
    let num_litlen = s.pop(5) as usize + 257;
    let num_dist = s.pop(5) as usize + 1;
    let num_precode = s.pop(4) as usize + 4;
    s.ensure()?;
    t.static_loaded = false;

    let mut pre = [0u8; NUM_PRECODE];
    for &sym in &PRECODE_ORDER[..num_precode] {
        pre[sym] = s.pop(3) as u8;
        s.advance()?;
    }
    t.precode.build(&pre)?;

    s.idx = 0;
    // Room for a run of 138 zeros past the end, which the reference tolerates.
    let mut lens = [0u8; NUM_LITLEN + NUM_DIST + 138];
    let total = num_litlen + num_dist;
    let mut i = 0;
    while i < total {
        let sym = s.decode(&t.precode)?;
        match sym {
            0..16 => {
                lens[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err(corrupt("length repeat with no previous length"));
                }
                let n = 3 + s.pop(2) as usize;
                let v = lens[i - 1];
                lens[i..i + n].fill(v);
                i += n;
            }
            17 => i += 3 + s.pop(3) as usize,
            _ => i += 11 + s.pop(7) as usize,
        }
        s.advance()?;
    }
    let mut litlen = [0u8; NUM_LITLEN];
    litlen[..num_litlen].copy_from_slice(&lens[..num_litlen]);
    t.litlen.build(&litlen)?;
    let mut dist = [0u8; NUM_DIST];
    dist[..num_dist].copy_from_slice(&lens[num_litlen..total]);
    t.dist.build(&dist)
}
