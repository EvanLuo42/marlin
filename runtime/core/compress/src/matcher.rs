//! The LZ77 match finder shared by the encoders: hash chains with greedy or lazy parsing.
//!
//! One `Matcher` covers one window of the input; positions are absolute indices into the
//! slice passed to every call (`u32`, so inputs stay under 4 GiB). The parse reports literal
//! runs and matches through a callback, so each codec does its own symbol coding.

use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Params {
    /// Shortest match the format can code: 3 (Deflate, zstd) or 4 (LZ4). Also the hashed
    /// prefix length.
    pub min_match: usize,
    /// Longest match the format can code in one sequence.
    pub max_match: usize,
    /// Farthest match the format can code.
    pub max_offset: usize,
    /// Chain candidates examined per position. 1 makes it a plain hash table.
    pub depth: u32,
    /// Stop searching once a match is this long.
    pub nice: usize,
    /// Positions to look ahead for a longer match before taking one (0 = greedy, 1 or 2).
    pub lazy: u8,
    pub hash_log: u32,
}

/// A step of the parse: `literals` bytes of literals starting at `start`, then a match of
/// `len` bytes at distance `offset` (`len == 0` for the trailing literals).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Step {
    pub start: usize,
    pub literals: usize,
    pub offset: usize,
    pub len: usize,
}

pub(crate) struct Matcher {
    p: Params,
    head: Vec<u32>,
    /// Ring of chain links, indexed by `pos & mask`; at least `max_offset + 1` long so a link
    /// is never overwritten while its position is still in range.
    prev: Vec<u32>,
    mask: usize,
    /// Every position below this one is in the chains.
    inserted: usize,
}

impl Matcher {
    /// A matcher for an input of `len` bytes.
    pub fn new(p: Params, len: usize) -> Matcher {
        let ring = (p.max_offset + 1).min(len.max(1)).next_power_of_two();
        let hash_log = p
            .hash_log
            .min(len.max(64).next_power_of_two().trailing_zeros() + 1);
        Matcher {
            p: Params { hash_log, ..p },
            head: vec![NONE; 1 << hash_log],
            prev: vec![NONE; if p.depth > 1 { ring } else { 0 }],
            mask: ring - 1,
            inserted: 0,
        }
    }

    /// Forgets all positions and restarts at `pos`: for formats whose blocks are independent.
    pub fn reset(&mut self, pos: usize) {
        self.head.fill(NONE);
        self.prev.fill(NONE);
        self.inserted = pos;
    }

    #[inline]
    fn hash(&self, src: &[u8], pos: usize) -> usize {
        let v = if self.p.min_match >= 4 {
            u32::from_le_bytes([src[pos], src[pos + 1], src[pos + 2], src[pos + 3]])
                .wrapping_mul(0x9e37_79b1)
        } else {
            u32::from_le_bytes([src[pos], src[pos + 1], src[pos + 2], 0]).wrapping_mul(0x1e35_a7bd)
        };
        (v >> (32 - self.p.hash_log)) as usize
    }

    /// Adds every position in `inserted..pos` that has a full hashed prefix in `src`.
    #[inline]
    fn insert_upto(&mut self, src: &[u8], pos: usize) {
        let end = pos.min((src.len() + 1).saturating_sub(self.p.min_match.max(4)));
        while self.inserted < end {
            let i = self.inserted;
            let h = self.hash(src, i);
            if !self.prev.is_empty() {
                self.prev[i & self.mask] = self.head[h];
            }
            self.head[h] = i as u32;
            self.inserted += 1;
        }
        self.inserted = self.inserted.max(pos);
    }

    /// The longest match at `pos` ending at or before `end`, as `(len, offset)`; `len` is 0
    /// when there is none of at least `min_match` bytes. `rep` (0 = none) is tried first: a
    /// repeat of the previous offset is cheap to code in zstd.
    fn find(&mut self, src: &[u8], pos: usize, end: usize, rep: usize) -> (usize, usize) {
        self.insert_upto(src, pos);
        let limit = end.min(pos.saturating_add(self.p.max_match));
        let mut best = (0, 0);
        if limit < pos + self.p.min_match || pos + 4 > src.len() {
            return best;
        }
        if rep != 0 && rep <= pos && rep <= self.p.max_offset {
            let len = common(src, pos - rep, pos, limit);
            if len >= self.p.min_match {
                best = (len, rep);
                if len >= self.p.nice {
                    self.insert_upto(src, pos + 1);
                    return best;
                }
            }
        }
        let h = self.hash(src, pos);
        let mut cand = self.head[h];
        let mut steps = self.p.depth;
        while cand != NONE && steps > 0 {
            let c = cand as usize;
            if c >= pos || pos - c > self.p.max_offset {
                break;
            }
            // Cheap reject: a longer match must also agree at the current best length.
            if pos + best.0 < limit && src[c + best.0] == src[pos + best.0] {
                let len = common(src, c, pos, limit);
                if len > best.0 && len >= self.p.min_match {
                    best = (len, pos - c);
                    if len >= self.p.nice {
                        break;
                    }
                }
            }
            steps -= 1;
            if self.prev.is_empty() {
                break;
            }
            let next = self.prev[c & self.mask];
            if next == NONE || next as usize >= c {
                break;
            }
            cand = next;
        }
        self.insert_upto(src, pos + 1);
        best
    }

    /// Parses `src[range]` into literal runs and matches. Matches end at or before
    /// `match_end` and start before `match_start_end` (format tail rules); they may reach back
    /// before the range to anything this matcher has seen. `rep` is the offset of the last match
    /// before `from` (0 if none), and is updated.
    pub fn parse(
        &mut self,
        src: &[u8],
        range: Range<usize>,
        match_end: usize,
        match_start_end: usize,
        rep: &mut usize,
        mut emit: impl FnMut(Step),
    ) {
        let (from, to) = (range.start, range.end);
        let mut anchor = from;
        let mut pos = from;
        let match_end = match_end.min(to);
        while pos < match_start_end {
            let (mut len, mut off) = self.find(src, pos, match_end, *rep);
            if len == 0 {
                // Speed up through incompressible data in the fastest settings.
                let skip = if self.p.lazy == 0 && self.p.depth <= 4 {
                    1 + ((pos - anchor) >> 7)
                } else {
                    1
                };
                pos += skip;
                continue;
            }
            let mut ahead = 0;
            while ahead < self.p.lazy && pos + 1 < match_start_end && len < self.p.nice {
                let (l2, o2) = self.find(src, pos + 1, match_end, *rep);
                // A longer match one byte later must pay for the extra literal, and a far
                // offset costs more bits than a near one.
                if l2 > len + usize::from(o2 > 4 * off) {
                    pos += 1;
                    len = l2;
                    off = o2;
                    ahead += 1;
                } else {
                    break;
                }
            }
            emit(Step {
                start: anchor,
                literals: pos - anchor,
                offset: off,
                len,
            });
            *rep = off;
            pos += len;
            anchor = pos;
        }
        emit(Step {
            start: anchor,
            literals: to - anchor,
            offset: 0,
            len: 0,
        });
    }
}

/// Length of the common prefix of `src[a..]` and `src[b..limit]`, with `a < b`.
#[inline]
fn common(src: &[u8], a: usize, b: usize, limit: usize) -> usize {
    let max = limit - b;
    let mut n = 0;
    while n + 8 <= max {
        let x = u64::from_le_bytes(src[a + n..a + n + 8].try_into().unwrap());
        let y = u64::from_le_bytes(src[b + n..b + n + 8].try_into().unwrap());
        let d = x ^ y;
        if d != 0 {
            return n + (d.trailing_zeros() / 8) as usize;
        }
        n += 8;
    }
    while n < max && src[a + n] == src[b + n] {
        n += 1;
    }
    n
}
