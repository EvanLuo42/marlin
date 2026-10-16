//! Huffman coding of zstd literals, RFC 8878 4.2.

use super::bits::RevBits;
use super::corrupt;
use super::fse::{DTable, read_ncount};
use crate::Result;

/// `Max_Number_of_Bits`.
pub(crate) const MAX_BITS: u32 = 11;

/// A decoding table indexed by the next `max_bits` bits of the stream.
pub(crate) struct HufTable {
    pub max_bits: u32,
    /// `(symbol, code length)`.
    pub entries: [(u8, u8); 1 << MAX_BITS],
}

impl HufTable {
    pub const fn new() -> Self {
        Self {
            max_bits: 0,
            entries: [(0, 0); 1 << MAX_BITS],
        }
    }

    /// Reads a Huffman tree description (4.2.1) from the start of `src`; returns the bytes it
    /// used.
    pub fn read(&mut self, src: &[u8], fse: &mut DTable) -> Result<usize> {
        let Some(&header) = src.first() else {
            return Err(corrupt("truncated huffman tree"));
        };
        let mut weights = [0u8; 256];
        let (count, used) = if header >= 128 {
            // Direct: 4 bits per weight, high nibble first.
            let count = header as usize - 127;
            let bytes = count.div_ceil(2);
            let Some(data) = src.get(1..1 + bytes) else {
                return Err(corrupt("truncated huffman weights"));
            };
            for (i, w) in weights[..count].iter_mut().enumerate() {
                let b = data[i / 2];
                *w = if i % 2 == 0 { b >> 4 } else { b & 15 };
            }
            (count, 1 + bytes)
        } else {
            let size = header as usize;
            let Some(data) = src.get(1..1 + size) else {
                return Err(corrupt("truncated huffman weights"));
            };
            (read_fse_weights(data, &mut weights, fse)?, 1 + size)
        };
        self.build(&mut weights, count)?;
        Ok(used)
    }

    /// Completes the weights (the last one is implied) and fills the table.
    fn build(&mut self, weights: &mut [u8; 256], count: usize) -> Result<()> {
        if count == 0 || count > 255 {
            return Err(corrupt("huffman weight count"));
        }
        let mut sum = 0u32;
        for &w in &weights[..count] {
            if w as u32 > MAX_BITS {
                return Err(corrupt("huffman weight too large"));
            }
            if w > 0 {
                sum += 1 << (w - 1);
            }
        }
        if sum == 0 {
            return Err(corrupt("huffman weights all zero"));
        }
        let max_bits = 32 - sum.leading_zeros();
        if max_bits > MAX_BITS {
            return Err(corrupt("huffman code too long"));
        }
        let rest = (1u32 << max_bits) - sum;
        if !rest.is_power_of_two() {
            return Err(corrupt("huffman weights incomplete"));
        }
        weights[count] = (rest.trailing_zeros() + 1) as u8;
        let symbols = count + 1;

        // Lowest weight (longest code) first, then by symbol.
        let mut start = [0u32; MAX_BITS as usize + 2];
        for &w in &weights[..symbols] {
            if w > 0 {
                start[w as usize + 1] += 1 << (w - 1);
            }
        }
        for w in 1..start.len() {
            start[w] += start[w - 1];
        }
        for (s, &w) in weights[..symbols].iter().enumerate() {
            if w == 0 {
                continue;
            }
            let len = 1usize << (w - 1);
            let at = start[w as usize] as usize;
            start[w as usize] += len as u32;
            self.entries[at..at + len].fill((s as u8, (max_bits + 1 - w as u32) as u8));
        }
        self.max_bits = max_bits;
        Ok(())
    }

    /// Decodes one stream of exactly `out.len()` symbols that must use every bit of `src`.
    pub fn decode_stream(&self, src: &[u8], out: &mut [u8]) -> Result<()> {
        let mut br = RevBits::new(src)?;
        let mb = self.max_bits;
        // 4 symbols of at most 11 bits fit one reload.
        let (chunks, rest) = out.as_chunks_mut::<4>();
        for chunk in chunks {
            br.reload();
            for o in chunk {
                let (sym, bits) = self.entries[br.peek(mb) as usize];
                *o = sym;
                br.consume(bits as u32);
            }
        }
        br.reload();
        for o in rest {
            let (sym, bits) = self.entries[br.peek(mb) as usize];
            *o = sym;
            br.consume(bits as u32);
        }
        if br.remaining() != 0 {
            return Err(corrupt("huffman stream size"));
        }
        Ok(())
    }
}

/// FSE-compressed weights (4.2.1.2): two interleaved states over one backward stream, ending
/// when a state update runs out of bits.
fn read_fse_weights(src: &[u8], weights: &mut [u8; 256], fse: &mut DTable) -> Result<usize> {
    let mut norm = [0i16; 256];
    let (used, log) = read_ncount(src, 255, 6, &mut norm)?;
    fse.build(&norm, log)?;
    let mut br = RevBits::new(&src[used..])?;
    let t = &fse.entries;
    let mut s1 = br.read(log) as usize;
    let mut s2 = br.read(log) as usize;
    let mut n = 0;
    loop {
        if n + 2 > 255 {
            return Err(corrupt("too many huffman weights"));
        }
        br.reload();
        weights[n] = t[s1].sym;
        n += 1;
        s1 = t[s1].base as usize + br.read(t[s1].bits as u32) as usize;
        if br.remaining() < 0 {
            weights[n] = t[s2].sym;
            n += 1;
            break;
        }
        if n + 2 > 255 {
            return Err(corrupt("too many huffman weights"));
        }
        weights[n] = t[s2].sym;
        n += 1;
        s2 = t[s2].base as usize + br.read(t[s2].bits as u32) as usize;
        if br.remaining() < 0 {
            weights[n] = t[s1].sym;
            n += 1;
            break;
        }
    }
    Ok(n)
}

#[cfg(feature = "encode")]
pub(crate) use encode::HufEncoder;

#[cfg(feature = "encode")]
mod encode {
    use super::MAX_BITS;
    use crate::huffman::code_lengths;
    use crate::zstd::bits::BitWriter;
    use crate::zstd::fse::{CTable, normalize, table_log, write_ncount};
    use alloc::vec::Vec;

    pub struct HufEncoder {
        codes: [u16; 256],
        bits: [u8; 256],
        max_bits: u32,
        /// The last symbol with a code; its weight is implied, not written.
        last: usize,
    }

    impl HufEncoder {
        /// A code for `counts`, or `None` when fewer than two symbols occur.
        pub fn new(counts: &[u32; 256]) -> Option<HufEncoder> {
            let mut lens = [0u8; 256];
            code_lengths(counts, MAX_BITS, &mut lens);
            if lens.iter().filter(|&&l| l > 0).count() < 2 {
                return None;
            }
            let max_bits = *lens.iter().max().unwrap() as u32;
            let last = lens.iter().rposition(|&l| l > 0).unwrap();
            // Codes as the decoder assigns them: by weight, then symbol, counting up.
            let mut start = [0u32; MAX_BITS as usize + 2];
            for &l in &lens {
                if l > 0 {
                    let w = max_bits + 1 - l as u32;
                    start[w as usize + 1] += 1 << (w - 1);
                }
            }
            for w in 1..start.len() {
                start[w] += start[w - 1];
            }
            let mut codes = [0u16; 256];
            for (s, &l) in lens.iter().enumerate() {
                if l > 0 {
                    let w = max_bits + 1 - l as u32;
                    codes[s] = (start[w as usize] >> (w - 1)) as u16;
                    start[w as usize] += 1 << (w - 1);
                }
            }
            Some(HufEncoder {
                codes,
                bits: lens,
                max_bits,
                last,
            })
        }

        fn weight(&self, s: usize) -> u8 {
            match self.bits[s] {
                0 => 0,
                l => (self.max_bits + 1 - l as u32) as u8,
            }
        }

        /// Estimated encoded size in bytes of `counts` with this code.
        pub fn cost(&self, counts: &[u32; 256]) -> usize {
            let bits: u64 = counts
                .iter()
                .zip(&self.bits)
                .map(|(&c, &b)| c as u64 * b as u64)
                .sum();
            (bits / 8) as usize + 1
        }

        /// Appends the tree description; `None` if it cannot be written (more than 128
        /// weights that FSE cannot compress).
        pub fn write_description(&self, out: &mut Vec<u8>) -> Option<()> {
            let weights: Vec<u8> = (0..self.last).map(|s| self.weight(s)).collect();
            let n = weights.len();
            if let Some(fse) = fse_weights(&weights)
                && (fse.len() < n.div_ceil(2) || n > 128)
            {
                out.push(fse.len() as u8);
                out.extend_from_slice(&fse);
                return Some(());
            }
            if n > 128 {
                return None;
            }
            out.push(127 + n as u8);
            for pair in weights.chunks(2) {
                out.push((pair[0] << 4) | pair.get(1).copied().unwrap_or(0));
            }
            Some(())
        }

        /// One stream: symbols in reverse so the decoder, reading backward, sees them in order.
        pub fn encode_stream(&self, src: &[u8], out: &mut Vec<u8>) {
            let mut w = BitWriter::new(out);
            for &b in src.iter().rev() {
                w.put(self.codes[b as usize] as u64, self.bits[b as usize] as u32);
            }
            w.close();
        }
    }

    /// FSE-compressed weights, if they fit the 127-byte limit and FSE applies (at least two
    /// distinct weights).
    fn fse_weights(weights: &[u8]) -> Option<Vec<u8>> {
        let n = weights.len();
        let mut counts = [0u32; MAX_BITS as usize + 1];
        for &w in weights {
            counts[w as usize] += 1;
        }
        if n < 2 || counts.iter().filter(|&&c| c > 0).count() < 2 {
            return None;
        }
        let max_symbol = counts.iter().rposition(|&c| c > 0).unwrap();
        let counts = &counts[..=max_symbol];
        let log = table_log(6, n as u32, max_symbol);
        let mut norm = [0i16; MAX_BITS as usize + 1];
        let norm = &mut norm[..=max_symbol];
        normalize(counts, n as u32, log, norm);
        let mut out = Vec::new();
        let mut w = BitWriter::new(&mut out);
        write_ncount(norm, log, &mut w);
        w.finish();
        let ct = CTable::new(norm, log);
        let mut w = BitWriter::new(&mut out);
        // Even indices belong to the state read first, odd ones to the second; the last two
        // weights start the states, chosen so the final update runs out of bits.
        let mut state = [0u32; 2];
        state[(n - 1) & 1] = ct.init_state(weights[n - 1]);
        state[(n - 2) & 1] = ct.init_state(weights[n - 2]);
        for i in (0..n - 2).rev() {
            ct.encode(&mut state[i & 1], weights[i], &mut w);
        }
        ct.flush(state[1], &mut w);
        ct.flush(state[0], &mut w);
        w.close();
        (out.len() < 128).then_some(out)
    }
}
