//! Compressed blocks: the literals section, the sequences section, and their execution.

use super::bits::RevBits;
use super::fse::{DTable, MAX_LOG, read_ncount};
use super::huf::HufTable;
use super::{
    LL_CODES, LL_DEFAULT, LL_DEFAULT_LOG, MAX_BLOCK, MAX_OF_CODE, ML_CODES, ML_DEFAULT,
    ML_DEFAULT_LOG, OF_DEFAULT, OF_DEFAULT_LOG, corrupt, too_small,
};
use crate::Result;
use crate::util::{copy_match, le16};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

/// The decoding state that lives across the blocks of a frame.
pub(crate) struct Decoder {
    huf: HufTable,
    has_huf: bool,
    /// Literal length, offset and match length tables, kept for repeat mode.
    tables: [DTable; 3],
    has_table: [bool; 3],
    /// Scratch for Huffman weight decoding.
    weights_fse: DTable,
    literals: Vec<u8>,
    reps: [u64; 3],
}

const LL: usize = 0;
const OF: usize = 1;
const ML: usize = 2;

impl Decoder {
    pub fn new() -> Box<Decoder> {
        Box::new(Decoder {
            huf: HufTable::new(),
            has_huf: false,
            tables: [DTable::new(), DTable::new(), DTable::new()],
            has_table: [false; 3],
            weights_fse: DTable::new(),
            literals: vec![0; MAX_BLOCK],
            reps: [1, 4, 8],
        })
    }

    /// Without a dictionary, every frame starts from scratch.
    pub fn reset_frame(&mut self) {
        self.has_huf = false;
        self.has_table = [false; 3];
        self.reps = [1, 4, 8];
    }

    /// Decodes one compressed block into `dst[op..]`; matches may reach back to
    /// `dst[frame_start..]`. Returns the new end of output.
    pub fn decode_block(
        &mut self,
        src: &[u8],
        dst: &mut [u8],
        op: usize,
        frame_start: usize,
    ) -> Result<usize> {
        let (lit_len, used) = self.decode_literals(src)?;
        self.decode_sequences(&src[used..], lit_len, dst, op, frame_start)
    }

    /// Returns `(literal count, bytes used)`; the literals land in `self.literals`.
    fn decode_literals(&mut self, src: &[u8]) -> Result<(usize, usize)> {
        let Some(&b0) = src.first() else {
            return Err(corrupt("missing literals section"));
        };
        let byte = |i: usize| src.get(i).map(|&b| b as usize);
        let kind = b0 & 3;
        let format = (b0 >> 2) & 3;
        if kind < 2 {
            // Raw or RLE: the size takes 5, 12 or 20 bits.
            let (size, header) = match format {
                0 | 2 => ((b0 >> 3) as usize, 1),
                1 => {
                    let Some(b1) = byte(1) else {
                        return Err(corrupt("truncated literals header"));
                    };
                    ((b0 >> 4) as usize | b1 << 4, 2)
                }
                _ => {
                    let (Some(b1), Some(b2)) = (byte(1), byte(2)) else {
                        return Err(corrupt("truncated literals header"));
                    };
                    ((b0 >> 4) as usize | b1 << 4 | b2 << 12, 3)
                }
            };
            if size > MAX_BLOCK {
                return Err(corrupt("too many literals"));
            }
            if kind == 0 {
                let Some(data) = src.get(header..header + size) else {
                    return Err(corrupt("truncated raw literals"));
                };
                self.literals[..size].copy_from_slice(data);
                return Ok((size, header + size));
            }
            let Some(&b) = src.get(header) else {
                return Err(corrupt("truncated rle literals"));
            };
            self.literals[..size].fill(b);
            return Ok((size, header + 1));
        }

        // Huffman-compressed (kind 2) or treeless (3, reusing the previous table).
        let (four, header, bits) = match format {
            0 => (false, 3, 10),
            1 => (true, 3, 10),
            2 => (true, 4, 14),
            _ => (true, 5, 18),
        };
        let Some(h) = src.get(..header) else {
            return Err(corrupt("truncated literals header"));
        };
        let mut word = [0u8; 8];
        word[..header].copy_from_slice(h);
        let v = u64::from_le_bytes(word) >> 4;
        let mask = (1u64 << bits) - 1;
        let regen = (v & mask) as usize;
        let size = ((v >> bits) & mask) as usize;
        if regen > MAX_BLOCK {
            return Err(corrupt("too many literals"));
        }
        let Some(data) = src.get(header..header + size) else {
            return Err(corrupt("truncated compressed literals"));
        };
        let tree = if kind == 2 {
            let used = self.huf.read(data, &mut self.weights_fse)?;
            self.has_huf = true;
            used
        } else if self.has_huf {
            0
        } else {
            return Err(corrupt("treeless literals without a previous table"));
        };
        let streams = &data[tree..];
        let out = &mut self.literals[..regen];
        if !four {
            self.huf.decode_stream(streams, out)?;
        } else {
            let (Some(s1), Some(s2), Some(s3)) =
                (le16(streams, 0), le16(streams, 2), le16(streams, 4))
            else {
                return Err(corrupt("truncated jump table"));
            };
            let (s1, s2, s3) = (s1 as usize, s2 as usize, s3 as usize);
            if 6 + s1 + s2 + s3 > streams.len() {
                return Err(corrupt("jump table past end of literals"));
            }
            let seg = regen.div_ceil(4);
            if 3 * seg > regen {
                return Err(corrupt("too few literals for four streams"));
            }
            let (a, rest) = streams[6..].split_at(s1);
            let (b, rest) = rest.split_at(s2);
            let (c, d) = rest.split_at(s3);
            let (oa, orest) = out.split_at_mut(seg);
            let (ob, orest) = orest.split_at_mut(seg);
            let (oc, od) = orest.split_at_mut(seg);
            self.huf.decode_stream(a, oa)?;
            self.huf.decode_stream(b, ob)?;
            self.huf.decode_stream(c, oc)?;
            self.huf.decode_stream(d, od)?;
        }
        Ok((regen, header + size))
    }

    fn decode_sequences(
        &mut self,
        src: &[u8],
        lit_len: usize,
        dst: &mut [u8],
        mut op: usize,
        frame_start: usize,
    ) -> Result<usize> {
        let Some(&b0) = src.first() else {
            return Err(corrupt("missing sequences section"));
        };
        let byte = |i: usize| src.get(i).map(|&b| b as usize);
        let (count, mut ip) = match b0 {
            0..128 => (b0 as usize, 1),
            128..255 => {
                let Some(b1) = byte(1) else {
                    return Err(corrupt("truncated sequence count"));
                };
                (((b0 as usize - 128) << 8) + b1, 2)
            }
            255 => {
                let (Some(b1), Some(b2)) = (byte(1), byte(2)) else {
                    return Err(corrupt("truncated sequence count"));
                };
                (b1 + (b2 << 8) + 0x7F00, 3)
            }
        };
        if count == 0 {
            if ip != src.len() {
                return Err(corrupt("data after empty sequences section"));
            }
            return self.copy_literals(0, lit_len, dst, op);
        }

        let Some(&modes) = src.get(ip) else {
            return Err(corrupt("truncated compression modes"));
        };
        ip += 1;
        if modes & 3 != 0 {
            return Err(corrupt("reserved compression mode bits"));
        }
        const KINDS: [(usize, usize, u32, &[i16], u32); 3] = [
            (LL, LL_CODES.len() - 1, MAX_LOG, &LL_DEFAULT, LL_DEFAULT_LOG),
            (OF, MAX_OF_CODE, MAX_LOG - 1, &OF_DEFAULT, OF_DEFAULT_LOG),
            (ML, ML_CODES.len() - 1, MAX_LOG, &ML_DEFAULT, ML_DEFAULT_LOG),
        ];
        for (k, (which, max_sym, max_log, default, default_log)) in KINDS.into_iter().enumerate() {
            let table = &mut self.tables[which];
            match (modes >> (6 - 2 * k)) & 3 {
                0 => table.build(default, default_log)?,
                1 => {
                    let Some(&sym) = src.get(ip) else {
                        return Err(corrupt("truncated rle sequence table"));
                    };
                    ip += 1;
                    if sym as usize > max_sym {
                        return Err(corrupt("rle sequence symbol out of range"));
                    }
                    table.set_rle(sym);
                }
                2 => {
                    let mut norm = [0i16; 256];
                    let (used, log) = read_ncount(&src[ip..], max_sym, max_log, &mut norm)?;
                    ip += used;
                    table.build(&norm, log)?;
                }
                _ => {
                    if !self.has_table[which] {
                        return Err(corrupt("repeat mode without a previous table"));
                    }
                }
            }
            self.has_table[which] = true;
        }

        let mut br = RevBits::new(&src[ip..])?;
        let [ll_t, of_t, ml_t] = &self.tables;
        let mut ll_s = br.read(ll_t.log) as usize;
        let mut of_s = br.read(of_t.log) as usize;
        br.reload();
        let mut ml_s = br.read(ml_t.log) as usize;
        let mut lit_pos = 0;
        let mut reps = self.reps;
        for i in 0..count {
            let ll_e = ll_t.entries[ll_s];
            let of_e = of_t.entries[of_s];
            let ml_e = ml_t.entries[ml_s];

            br.reload();
            let of_code = of_e.sym as u32;
            let of_value = (1u64 << of_code) + br.read(of_code);
            br.reload();
            let (ml_base, ml_bits) = ML_CODES[ml_e.sym as usize];
            let ml = ml_base as usize + br.read(ml_bits as u32) as usize;
            let (ll_base, ll_bits) = LL_CODES[ll_e.sym as usize];
            let ll = ll_base as usize + br.read(ll_bits as u32) as usize;

            let offset = if of_value > 3 {
                let offset = of_value - 3;
                reps = [offset, reps[0], reps[1]];
                offset
            } else {
                match of_value as usize - 1 + usize::from(ll == 0) {
                    0 => reps[0],
                    1 => {
                        reps = [reps[1], reps[0], reps[2]];
                        reps[0]
                    }
                    2 => {
                        reps = [reps[2], reps[0], reps[1]];
                        reps[0]
                    }
                    _ => {
                        let offset = reps[0] - 1;
                        if offset == 0 {
                            return Err(corrupt("repeat offset of zero"));
                        }
                        reps = [offset, reps[0], reps[1]];
                        offset
                    }
                }
            };

            if i + 1 < count {
                br.reload();
                ll_s = ll_e.base as usize + br.read(ll_e.bits as u32) as usize;
                ml_s = ml_e.base as usize + br.read(ml_e.bits as u32) as usize;
                of_s = of_e.base as usize + br.read(of_e.bits as u32) as usize;
            }

            // Execute: literals, then the match.
            if ll > lit_len - lit_pos {
                return Err(corrupt("sequences use more literals than decoded"));
            }
            op = self.copy_literals(lit_pos, lit_pos + ll, dst, op)?;
            lit_pos += ll;
            if offset > (op - frame_start) as u64 {
                return Err(corrupt("offset before start of frame"));
            }
            if ml > dst.len() - op {
                return Err(too_small());
            }
            copy_match(dst, op, offset as usize, ml);
            op += ml;
        }
        if br.remaining() != 0 {
            return Err(corrupt("sequence bit stream size"));
        }
        self.reps = reps;
        self.copy_literals(lit_pos, lit_len, dst, op)
    }

    #[inline]
    fn copy_literals(&self, from: usize, to: usize, dst: &mut [u8], op: usize) -> Result<usize> {
        let n = to - from;
        let Some(out) = dst.get_mut(op..op + n) else {
            return Err(too_small());
        };
        out.copy_from_slice(&self.literals[from..to]);
        Ok(op + n)
    }
}
