//! The zstd encoder.

use super::bits::BitWriter;
use super::fse::{CTable, MAX_LOG, normalize, table_log, write_ncount};
use super::huf::HufEncoder;
use super::{
    LL_CODES, LL_DEFAULT, LL_DEFAULT_LOG, MAGIC, MAX_BLOCK, ML_CODES, ML_DEFAULT, ML_DEFAULT_LOG,
    OF_DEFAULT, OF_DEFAULT_LOG,
};
use crate::matcher::{Matcher, Params, Step};
use crate::{Codec, Error, ErrorKind, Level, Result};
use alloc::vec;
use alloc::vec::Vec;
use marlin_hash::xxh64;

/// Frame settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameOptions {
    /// Append the XXH64 content checksum. Off by default: packages verify chunks themselves.
    pub checksum: bool,
    /// Write the content size into the header, making the frame single-segment. On by
    /// default; off yields a window descriptor instead, like streamed `zstd` output.
    pub content_size: bool,
}

impl Default for FrameOptions {
    fn default() -> Self {
        Self {
            checksum: false,
            content_size: true,
        }
    }
}

/// The largest input the encoder accepts: match positions are 32-bit.
const MAX_INPUT: usize = u32::MAX as usize - 64;

/// Frame header (at most 18 bytes), block headers and the checksum around stored data.
pub fn max_compressed_size(len: usize) -> usize {
    len + 3 * len.div_ceil(MAX_BLOCK).max(1) + 18 + 4
}

/// Compresses `src` as one zstd frame appended to `out`.
pub fn compress(src: &[u8], level: Level, opts: &FrameOptions, out: &mut Vec<u8>) -> Result<()> {
    if src.len() > MAX_INPUT {
        return Err(Error::new(
            Codec::Zstd,
            ErrorKind::Unsupported,
            "input over 4 GiB",
        ));
    }
    out.reserve(max_compressed_size(src.len()));
    write_frame_header(src.len(), opts, out);

    let n = src.len();
    let mut matcher = Matcher::new(params(level, n), n);
    let mut enc = BlockEncoder::default();
    let mut rep = 0;
    let mut pos = 0;
    loop {
        let end = (pos + MAX_BLOCK).min(n);
        let last = end == n;
        let block = &src[pos..end];
        let header_at = out.len();
        out.extend_from_slice(&[0; 3]);
        let (kind, size) = if block.len() > 1 && block.iter().all(|&b| b == block[0]) {
            out.push(block[0]);
            // Keep the chains complete for the blocks after.
            matcher.parse(src, pos..end, end, pos, &mut rep, |_| {});
            (1, block.len())
        } else {
            let body = out.len();
            enc.sequences.clear();
            enc.literals.clear();
            matcher.parse(
                src,
                pos..end,
                end,
                end.saturating_sub(3),
                &mut rep,
                |s: Step| {
                    enc.literals
                        .extend_from_slice(&src[s.start..s.start + s.literals]);
                    if s.len != 0 {
                        enc.sequences.push(Sequence {
                            lit_len: s.literals as u32,
                            offset: s.offset as u32,
                            match_len: s.len as u32,
                        });
                    }
                },
            );
            if enc.encode(out) && out.len() - body < block.len() {
                enc.commit();
                (2, out.len() - body)
            } else {
                out.truncate(body);
                out.extend_from_slice(block);
                (0, block.len())
            }
        };
        let h = u32::from(last) | kind << 1 | (size as u32) << 3;
        out[header_at..header_at + 3].copy_from_slice(&h.to_le_bytes()[..3]);
        pos = end;
        if last {
            break;
        }
    }
    if opts.checksum {
        out.extend_from_slice(&(xxh64(src, 0) as u32).to_le_bytes());
    }
    Ok(())
}

fn write_frame_header(n: usize, opts: &FrameOptions, out: &mut Vec<u8>) {
    out.extend_from_slice(&MAGIC.to_le_bytes());
    let checksum = u8::from(opts.checksum) << 2;
    if opts.content_size {
        let (flag, bytes): (u8, &[u8]) = if n < 256 {
            (0, &(n as u8).to_le_bytes())
        } else if n < 65536 + 256 {
            (1, &((n - 256) as u16).to_le_bytes())
        } else if n <= u32::MAX as usize {
            (2, &(n as u32).to_le_bytes())
        } else {
            (3, &(n as u64).to_le_bytes())
        };
        out.push(flag << 6 | 0x20 | checksum);
        out.extend_from_slice(bytes);
    } else {
        // Window = 2^log >= n, so every match in the frame is in range.
        let log = (usize::BITS - n.saturating_sub(1).leading_zeros()).max(10);
        out.push(checksum);
        out.push(((log - 10) << 3) as u8);
    }
}

fn params(level: Level, n: usize) -> Params {
    let (hash_log, depth, lazy, nice) = match level.get() {
        1 => (14, 1, 0, 4),
        2 => (16, 1, 0, 4),
        3 => (17, 4, 1, 32),
        4 => (17, 8, 1, 48),
        5 => (18, 16, 1, 64),
        6 => (18, 32, 2, 96),
        7 => (19, 64, 2, 128),
        8 => (20, 128, 2, 192),
        _ => (20, 256, 2, 256),
    };
    Params {
        min_match: 4,
        // The longest match one sequence codes: ML code 52 with all 16 extra bits set.
        max_match: 65539 + 65535,
        max_offset: n.max(1),
        depth,
        nice,
        lazy,
        hash_log,
    }
}

struct Sequence {
    lit_len: u32,
    /// The real distance; the offset value (repeat codes) is chosen at encoding.
    offset: u32,
    match_len: u32,
}

/// Per-block scratch, plus the repeat offsets, which carry from block to block.
struct BlockEncoder {
    literals: Vec<u8>,
    sequences: Vec<Sequence>,
    reps: [u32; 3],
    /// `reps` after the block being encoded, kept only if the block is used.
    next_reps: [u32; 3],
    codes: [Vec<u8>; 3],
    values: Vec<(u32, u32, u32)>,
}

impl Default for BlockEncoder {
    fn default() -> Self {
        Self {
            literals: Vec::new(),
            sequences: Vec::new(),
            reps: [1, 4, 8],
            next_reps: [1, 4, 8],
            codes: [Vec::new(), Vec::new(), Vec::new()],
            values: Vec::new(),
        }
    }
}

const LL: usize = 0;
const OF: usize = 1;
const ML: usize = 2;

impl BlockEncoder {
    /// Writes the block body (literals and sequences sections); `false` if it cannot be
    /// represented as a compressed block (the caller stores the block raw).
    fn encode(&mut self, out: &mut Vec<u8>) -> bool {
        encode_literals(&self.literals, out);
        self.encode_sequences(out)
    }

    /// The block was kept: its repeat offsets carry on.
    fn commit(&mut self) {
        self.reps = self.next_reps;
    }

    fn encode_sequences(&mut self, out: &mut Vec<u8>) -> bool {
        let n = self.sequences.len();
        match n {
            0..128 => out.push(n as u8),
            128..0x7F00 => out.extend_from_slice(&[(n >> 8) as u8 + 128, n as u8]),
            0x7F00..0x17F00 => {
                out.push(255);
                out.extend_from_slice(&((n - 0x7F00) as u16).to_le_bytes());
            }
            _ => return false,
        }
        if n == 0 {
            self.next_reps = self.reps;
            return true;
        }

        // Offset values with repeat codes, then the code of each field.
        let mut reps = self.reps;
        for c in &mut self.codes {
            c.clear();
        }
        self.values.clear();
        for s in &self.sequences {
            let value = offset_value(s.offset, s.lit_len == 0, &mut reps);
            let of = 31 - value.leading_zeros();
            let ll = LL_CODES.partition_point(|&(base, _)| base <= s.lit_len) - 1;
            let ml = ML_CODES.partition_point(|&(base, _)| base <= s.match_len) - 1;
            self.codes[LL].push(ll as u8);
            self.codes[OF].push(of as u8);
            self.codes[ML].push(ml as u8);
            self.values.push((s.lit_len, value, s.match_len));
        }
        self.next_reps = reps;

        let mode_at = out.len();
        out.push(0);
        let mut modes = 0u8;
        let kinds: [(usize, usize, &[i16], u32, u32); 3] = [
            (LL, LL_CODES.len(), &LL_DEFAULT, LL_DEFAULT_LOG, MAX_LOG),
            (OF, 32, &OF_DEFAULT, OF_DEFAULT_LOG, MAX_LOG - 1),
            (ML, ML_CODES.len(), &ML_DEFAULT, ML_DEFAULT_LOG, MAX_LOG),
        ];
        let mut coders: [Option<Coder>; 3] = [None, None, None];
        for (k, (which, alphabet, default, default_log, max_log)) in kinds.into_iter().enumerate() {
            let (mode, coder) = choose_table(
                &self.codes[which],
                alphabet,
                default,
                default_log,
                max_log,
                out,
            );
            modes |= mode << (6 - 2 * k);
            coders[which] = Some(coder);
        }
        out[mode_at] = modes;
        let [Some(ll_c), Some(of_c), Some(ml_c)] = coders else {
            unreachable!()
        };

        let mut w = BitWriter::new(out);
        let last = n - 1;
        let [llc, ofc, mlc] = [&self.codes[LL], &self.codes[OF], &self.codes[ML]];
        let mut ml_s = ml_c.init(mlc[last]);
        let mut of_s = of_c.init(ofc[last]);
        let mut ll_s = ll_c.init(llc[last]);
        put_extra(&mut w, self.values[last], llc[last], ofc[last], mlc[last]);
        for i in (0..last).rev() {
            of_c.encode(&mut of_s, ofc[i], &mut w);
            ml_c.encode(&mut ml_s, mlc[i], &mut w);
            ll_c.encode(&mut ll_s, llc[i], &mut w);
            put_extra(&mut w, self.values[i], llc[i], ofc[i], mlc[i]);
        }
        ml_c.flush(ml_s, &mut w);
        of_c.flush(of_s, &mut w);
        ll_c.flush(ll_s, &mut w);
        w.close();
        true
    }
}

/// The extra bits of one sequence, in the order the decoder reads them backward: offset,
/// match length, literal length.
fn put_extra(w: &mut BitWriter, (ll, of, ml): (u32, u32, u32), llc: u8, ofc: u8, mlc: u8) {
    let (ll_base, ll_bits) = LL_CODES[llc as usize];
    let (ml_base, ml_bits) = ML_CODES[mlc as usize];
    w.put((ll - ll_base) as u64, ll_bits as u32);
    w.put((ml - ml_base) as u64, ml_bits as u32);
    w.put((of - (1 << ofc)) as u64, ofc as u32);
}

/// The offset value for `offset` (RFC 8878 3.1.2.5), updating the repeat offsets as the
/// decoder will.
fn offset_value(offset: u32, ll_zero: bool, reps: &mut [u32; 3]) -> u32 {
    let r = *reps;
    let (value, new) = if !ll_zero {
        if offset == r[0] {
            (1, r)
        } else if offset == r[1] {
            (2, [r[1], r[0], r[2]])
        } else if offset == r[2] {
            (3, [r[2], r[0], r[1]])
        } else {
            (offset + 3, [offset, r[0], r[1]])
        }
    } else if offset == r[1] {
        (1, [r[1], r[0], r[2]])
    } else if offset == r[2] {
        (2, [r[2], r[0], r[1]])
    } else if offset == r[0] - 1 {
        (3, [offset, r[0], r[1]])
    } else {
        (offset + 3, [offset, r[0], r[1]])
    };
    *reps = new;
    value
}

/// An FSE coder for one sequence field, or RLE (one symbol, no bits).
enum Coder {
    Rle,
    Fse(CTable),
}

impl Coder {
    fn init(&self, sym: u8) -> u32 {
        match self {
            Coder::Rle => 0,
            Coder::Fse(t) => t.init_state(sym),
        }
    }

    fn encode(&self, state: &mut u32, sym: u8, w: &mut BitWriter) {
        if let Coder::Fse(t) = self {
            t.encode(state, sym, w);
        }
    }

    fn flush(&self, state: u32, w: &mut BitWriter) {
        if let Coder::Fse(t) = self {
            t.flush(state, w);
        }
    }
}

/// Picks predefined, RLE or compressed mode for one field; writes the table description if
/// any. Returns the mode bits and the coder.
fn choose_table(
    codes: &[u8],
    alphabet: usize,
    default: &[i16],
    default_log: u32,
    max_log: u32,
    out: &mut Vec<u8>,
) -> (u8, Coder) {
    let mut counts = vec![0u32; alphabet];
    for &c in codes {
        counts[c as usize] += 1;
    }
    let max_symbol = counts.iter().rposition(|&c| c > 0).unwrap_or(0);
    let distinct = counts.iter().filter(|&&c| c > 0).count();
    if distinct == 1 {
        out.push(max_symbol as u8);
        return (1, Coder::Rle);
    }

    let counts = &counts[..=max_symbol];
    let total = codes.len() as u32;
    let log = table_log(max_log, total, max_symbol);
    let mut norm = vec![0i16; max_symbol + 1];
    normalize(counts, total, log, &mut norm);
    let mut header = Vec::new();
    let mut w = BitWriter::new(&mut header);
    write_ncount(&norm, log, &mut w);
    w.finish();
    let custom_bits = header.len() as u64 * 8 * 256 + cost(counts, &norm, log);

    if max_symbol < default.len() {
        let default_bits = cost(counts, default, default_log);
        if default_bits <= custom_bits {
            return (0, Coder::Fse(CTable::new(default, default_log)));
        }
    }
    out.extend_from_slice(&header);
    (2, Coder::Fse(CTable::new(&norm, log)))
}

/// Approximate cost in 1/256 bits of coding `counts` with `norm`: each symbol costs
/// `log - log2(norm)`.
fn cost(counts: &[u32], norm: &[i16], log: u32) -> u64 {
    counts
        .iter()
        .zip(norm)
        .filter(|(c, _)| **c > 0)
        .map(|(&c, &p)| {
            let p = p.max(1) as u32;
            c as u64 * ((log << 8) - log2_fixed(p)) as u64
        })
        .sum()
}

/// `log2(v)` in 1/256 units, linearly interpolated between powers of two.
fn log2_fixed(v: u32) -> u32 {
    let hb = 31 - v.leading_zeros();
    let frac = ((v - (1 << hb)) << 8) >> hb;
    (hb << 8) + frac
}

/// The literals section: Huffman-compressed when that pays, else RLE or raw.
fn encode_literals(lits: &[u8], out: &mut Vec<u8>) {
    let n = lits.len();
    if n > 1 && lits.iter().all(|&b| b == lits[0]) {
        write_raw_header(1, n, out);
        out.push(lits[0]);
        return;
    }
    if n >= 64 && try_huffman(lits, out) {
        return;
    }
    write_raw_header(0, n, out);
    out.extend_from_slice(lits);
}

fn write_raw_header(kind: u8, n: usize, out: &mut Vec<u8>) {
    if n < 32 {
        out.push(kind | (n as u8) << 3);
    } else if n < 4096 {
        out.extend_from_slice(&[kind | 1 << 2 | (n as u8 & 15) << 4, (n >> 4) as u8]);
    } else {
        out.extend_from_slice(&[
            kind | 3 << 2 | (n as u8 & 15) << 4,
            (n >> 4) as u8,
            (n >> 12) as u8,
        ]);
    }
}

fn try_huffman(lits: &[u8], out: &mut Vec<u8>) -> bool {
    let n = lits.len();
    let mut counts = [0u32; 256];
    for &b in lits {
        counts[b as usize] += 1;
    }
    let Some(huf) = HufEncoder::new(&counts) else {
        return false;
    };
    // Not worth it unless it saves a few percent.
    if huf.cost(&counts) + 64 >= n - n / 32 {
        return false;
    }
    let start = out.len();
    let four = n >= 256;
    // One stream only below 256 literals, so the 3-byte header holds both sizes.
    let header = if n < 1024 {
        3
    } else if n < 16384 {
        4
    } else {
        5
    };
    out.resize(start + header, 0);
    let body = out.len();
    if huf.write_description(out).is_none() {
        out.truncate(start);
        return false;
    }
    if !four {
        huf.encode_stream(lits, out);
    } else {
        let seg = n.div_ceil(4);
        let jump = out.len();
        out.extend_from_slice(&[0; 6]);
        for (i, part) in lits.chunks(seg).enumerate() {
            let at = out.len();
            huf.encode_stream(part, out);
            let size = out.len() - at;
            if i < 3 {
                if size > u16::MAX as usize {
                    out.truncate(start);
                    return false;
                }
                out[jump + 2 * i..jump + 2 * i + 2].copy_from_slice(&(size as u16).to_le_bytes());
            }
        }
    }
    let size = out.len() - body;
    let (format, bits) = match header {
        3 if !four => (0u64, 10),
        3 => (1, 10),
        4 => (2, 14),
        _ => (3, 18),
    };
    if size >= 1 << bits || size >= n {
        out.truncate(start);
        return false;
    }
    let v = 2 | format << 2 | (n as u64) << 4 | (size as u64) << (4 + bits);
    out[start..start + header].copy_from_slice(&v.to_le_bytes()[..header]);
    true
}
