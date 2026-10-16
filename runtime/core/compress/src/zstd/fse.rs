//! Finite State Entropy (tANS), RFC 8878 4.1: table descriptions, decoding tables, and (for
//! the encoder) normalization and encoding tables.

use super::bits::FwdBits;
use super::corrupt;
use crate::Result;

/// Largest accuracy log any zstd FSE table uses (literal and match lengths).
pub(crate) const MAX_LOG: u32 = 9;
const MIN_LOG: u32 = 5;

/// One decoding state: emit `sym`, then the next state is `base + read(bits)`.
#[derive(Clone, Copy, Default)]
pub(crate) struct Entry {
    pub sym: u8,
    pub bits: u8,
    pub base: u16,
}

#[derive(Clone)]
pub(crate) struct DTable {
    pub log: u32,
    pub entries: [Entry; 1 << MAX_LOG],
}

impl DTable {
    pub const fn new() -> Self {
        Self {
            log: 0,
            entries: [Entry {
                sym: 0,
                bits: 0,
                base: 0,
            }; 1 << MAX_LOG],
        }
    }

    /// A table that always yields `sym` and reads no bits (RLE mode).
    pub fn set_rle(&mut self, sym: u8) {
        self.log = 0;
        self.entries[0] = Entry {
            sym,
            bits: 0,
            base: 0,
        };
    }

    /// Builds the table for normalized counts `norm` (`-1` = "less than 1"), whose absolute
    /// values sum to `1 << log`.
    pub fn build(&mut self, norm: &[i16], log: u32) -> Result<()> {
        let size = 1usize << log;
        let mut next = [0u16; 256];
        let mut high = size as isize - 1;
        for (s, &n) in norm.iter().enumerate() {
            if n == -1 {
                if high < 0 {
                    return Err(corrupt("fse distribution"));
                }
                self.entries[high as usize].sym = s as u8;
                high -= 1;
                next[s] = 1;
            } else if n > 0 {
                next[s] = n as u16;
            }
        }
        let step = (size >> 1) + (size >> 3) + 3;
        let mask = size - 1;
        let mut pos = 0usize;
        for (s, &n) in norm.iter().enumerate() {
            for _ in 0..n.max(0) {
                self.entries[pos].sym = s as u8;
                loop {
                    pos = (pos + step) & mask;
                    if pos as isize <= high {
                        break;
                    }
                }
            }
        }
        if pos != 0 {
            return Err(corrupt("fse distribution"));
        }
        for e in &mut self.entries[..size] {
            let n = next[e.sym as usize];
            next[e.sym as usize] += 1;
            let bits = log - (15 - n.leading_zeros());
            e.bits = bits as u8;
            e.base = ((n as u32) << bits).wrapping_sub(size as u32) as u16;
        }
        self.log = log;
        Ok(())
    }
}

/// Reads an FSE table description (RFC 8878 4.1.1) from the start of `src` into `norm`, which
/// must be zeroed. Returns `(bytes used, accuracy log)`.
pub(crate) fn read_ncount(
    src: &[u8],
    max_symbol: usize,
    max_log: u32,
    norm: &mut [i16; 256],
) -> Result<(usize, u32)> {
    let mut br = FwdBits::new(src);
    let log = br.peek(4) + MIN_LOG;
    br.consume(4);
    if log > max_log {
        return Err(corrupt("fse accuracy log too large"));
    }
    let mut remaining: i32 = (1 << log) + 1;
    let mut threshold: i32 = 1 << log;
    let mut nb_bits = log + 1;
    let mut sym = 0usize;
    let mut prev_zero = false;
    while remaining > 1 {
        if prev_zero {
            loop {
                let flag = br.peek(2) as usize;
                br.consume(2);
                sym += flag;
                if flag != 3 {
                    break;
                }
                if sym > max_symbol || br.overread() {
                    return Err(corrupt("fse zero run"));
                }
            }
        }
        if sym > max_symbol {
            return Err(corrupt("fse symbol out of range"));
        }
        let max = (2 * threshold - 1) - remaining;
        let low = br.peek(nb_bits - 1) as i32;
        let mut count = if low < max {
            br.consume(nb_bits - 1);
            low
        } else {
            let v = br.peek(nb_bits) as i32;
            br.consume(nb_bits);
            if v >= threshold { v - max } else { v }
        };
        count -= 1;
        remaining -= count.abs();
        if remaining < 1 {
            return Err(corrupt("fse probabilities overflow"));
        }
        norm[sym] = count as i16;
        sym += 1;
        prev_zero = count == 0;
        if remaining < threshold {
            if remaining <= 1 {
                break;
            }
            nb_bits = 32 - (remaining as u32).leading_zeros();
            threshold = 1 << (nb_bits - 1);
        }
        if br.overread() {
            return Err(corrupt("truncated fse table"));
        }
    }
    if remaining != 1 || br.overread() {
        return Err(corrupt("fse probabilities"));
    }
    Ok((br.bytes_used(), log))
}

#[cfg(feature = "encode")]
pub(crate) use encode::*;

#[cfg(feature = "encode")]
mod encode {
    use super::MIN_LOG;
    use crate::zstd::bits::BitWriter;
    use alloc::vec;
    use alloc::vec::Vec;

    #[inline]
    fn highbit(v: u32) -> u32 {
        31 - v.leading_zeros()
    }

    /// The accuracy log for `total` samples of symbols up to `max_symbol`
    /// (`FSE_optimalTableLog`).
    pub fn table_log(max_log: u32, total: u32, max_symbol: usize) -> u32 {
        let src_bits = highbit(total.max(2) - 1).saturating_sub(2);
        let min_bits = (highbit(total.max(1)) + 1).min(highbit(max_symbol.max(1) as u32) + 2);
        max_log.min(src_bits).max(min_bits).clamp(MIN_LOG, max_log)
    }

    /// Scales `counts` (summing to `total`) to sum to `1 << log`, keeping every used symbol at
    /// least 1.
    pub fn normalize(counts: &[u32], total: u32, log: u32, norm: &mut [i16]) {
        let size = 1i64 << log;
        let mut sum = 0i64;
        let mut largest = 0;
        for (s, &c) in counts.iter().enumerate() {
            if c == 0 {
                norm[s] = 0;
                continue;
            }
            let p = ((c as i64 * size + total as i64 / 2) / total as i64).max(1);
            norm[s] = p as i16;
            sum += p;
            if c > counts[largest] {
                largest = s;
            }
        }
        let mut diff = size - sum;
        if diff > 0 {
            norm[largest] += diff as i16;
        }
        while diff < 0 {
            // Take from the biggest share; it loses the least relative precision.
            let mut big = 0;
            for s in 0..counts.len() {
                if norm[s] > norm[big] {
                    big = s;
                }
            }
            let take = ((norm[big] - 1) as i64).min(-diff);
            norm[big] -= take as i16;
            diff += take;
        }
    }

    /// Writes an FSE table description (the inverse of [`super::read_ncount`]).
    pub fn write_ncount(norm: &[i16], log: u32, w: &mut BitWriter) {
        let size = 1i32 << log;
        w.put((log - MIN_LOG) as u64, 4);
        let mut remaining = size + 1;
        let mut threshold = size;
        let mut nb_bits = log + 1;
        let mut sym = 0;
        let mut prev_zero = false;
        while remaining > 1 {
            if prev_zero {
                let start = sym;
                while norm[sym] == 0 {
                    sym += 1;
                }
                let mut zeros = sym - start;
                while zeros >= 3 {
                    w.put(3, 2);
                    zeros -= 3;
                }
                w.put(zeros as u64, 2);
            }
            let count = norm[sym] as i32;
            sym += 1;
            let max = (2 * threshold - 1) - remaining;
            remaining -= count.abs();
            let mut v = count + 1;
            if v >= threshold {
                v += max;
            }
            w.put(v as u64, if v < max { nb_bits - 1 } else { nb_bits });
            prev_zero = v == 1;
            while remaining < threshold {
                nb_bits -= 1;
                threshold >>= 1;
            }
        }
    }

    /// An encoding table (`FSE_CTable`).
    pub struct CTable {
        pub log: u32,
        states: Vec<u16>,
        /// Per symbol: `(delta_nb_bits, delta_find_state)`.
        syms: Vec<(u32, i32)>,
    }

    impl CTable {
        pub fn new(norm: &[i16], log: u32) -> CTable {
            let size = 1usize << log;
            let mask = size - 1;
            let mut table_sym = vec![0u8; size];
            let mut cumul = vec![0u32; norm.len() + 1];
            let mut high = size - 1;
            for s in 0..norm.len() {
                if norm[s] == -1 {
                    cumul[s + 1] = cumul[s] + 1;
                    table_sym[high] = s as u8;
                    high = high.wrapping_sub(1);
                } else {
                    cumul[s + 1] = cumul[s] + norm[s].max(0) as u32;
                }
            }
            let step = (size >> 1) + (size >> 3) + 3;
            let mut pos = 0;
            for (s, &n) in norm.iter().enumerate() {
                for _ in 0..n.max(0) {
                    table_sym[pos] = s as u8;
                    loop {
                        pos = (pos + step) & mask;
                        if high == usize::MAX || pos <= high {
                            break;
                        }
                    }
                }
            }
            let mut states = vec![0u16; size];
            for (u, &s) in table_sym.iter().enumerate() {
                let c = &mut cumul[s as usize];
                states[*c as usize] = (size + u) as u16;
                *c += 1;
            }
            let mut syms = vec![(0u32, 0i32); norm.len()];
            let mut total = 0i32;
            for (s, &n) in norm.iter().enumerate() {
                syms[s] = match n {
                    0 => (((log + 1) << 16) - (1 << log), 0),
                    -1 | 1 => {
                        let e = ((log << 16) - (1 << log), total - 1);
                        total += 1;
                        e
                    }
                    _ => {
                        let max_bits_out = log - highbit(n as u32 - 1);
                        let min_state_plus = (n as u32) << max_bits_out;
                        let e = ((max_bits_out << 16) - min_state_plus, total - n as i32);
                        total += n as i32;
                        e
                    }
                };
            }
            CTable { log, states, syms }
        }

        /// The starting state for the last symbol encoded (read first), chosen to cost the
        /// fewest bits (`FSE_initCState2`).
        pub fn init_state(&self, sym: u8) -> u32 {
            let (delta_bits, delta_find) = self.syms[sym as usize];
            let nb_bits_out = (delta_bits + (1 << 15)) >> 16;
            let value = (nb_bits_out << 16).wrapping_sub(delta_bits);
            self.states[((value >> nb_bits_out) as i32 + delta_find) as usize] as u32
        }

        /// Encodes `sym` from `state`: writes the transition bits and updates the state.
        #[inline]
        pub fn encode(&self, state: &mut u32, sym: u8, w: &mut BitWriter) {
            let (delta_bits, delta_find) = self.syms[sym as usize];
            let nb_bits_out = (*state + delta_bits) >> 16;
            w.put(*state as u64, nb_bits_out);
            *state = self.states[((*state >> nb_bits_out) as i32 + delta_find) as usize] as u32;
        }

        /// Writes the final state (the decoder's initial state).
        #[inline]
        pub fn flush(&self, state: u32, w: &mut BitWriter) {
            w.put(state as u64, self.log);
        }
    }
}
