//! Interned strings: one copy of each distinct string, referred to by a `u32` id.
//!
//! Most of an index is repeated strings (directories, extensions, type names), so tables store
//! ids into pools and each pool stores every string once.

use std::collections::HashMap;

use marlin_hash::xxh3_64_with_seed;

use crate::column::Column;

/// No string.
pub const NONE: u32 = u32::MAX;

#[derive(Default)]
pub(crate) struct StringPool {
    pub(crate) bytes: Column<u8>,
    /// `offsets[i]..offsets[i + 1]` is string `i`; one more entry than strings.
    pub(crate) offsets: Column<u32>,
    /// Hash to id, built on the first `intern` (a mapped pool is only read until then).
    lookup: Option<HashMap<u64, u32>>,
}

/// Distinct strings with equal hashes go to the next seed's slot.
fn hash(s: &str, seed: u64) -> u64 {
    xxh3_64_with_seed(s.as_bytes(), seed)
}

impl StringPool {
    pub(crate) fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// String `id`; empty for `NONE` (and for ids a damaged snapshot got wrong).
    pub(crate) fn get(&self, id: u32) -> &str {
        let i = id as usize;
        let (Some(&start), Some(&end)) = (self.offsets.get(i), self.offsets.get(i + 1)) else {
            return "";
        };
        self.bytes
            .get(start as usize..end as usize)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or_default()
    }

    fn lookup(&mut self) -> &mut HashMap<u64, u32> {
        if self.lookup.is_none() {
            let mut map = HashMap::with_capacity(self.len());
            for id in 0..self.len() as u32 {
                let s = self.get(id);
                let mut seed = 0;
                while map.contains_key(&hash(s, seed)) {
                    seed += 1;
                }
                map.insert(hash(s, seed), id);
            }
            self.lookup = Some(map);
        }
        self.lookup
            .as_mut()
            .unwrap_or_else(|| unreachable!("built above"))
    }

    pub(crate) fn intern(&mut self, s: &str) -> u32 {
        let mut seed = 0;
        loop {
            let found = self.lookup().get(&hash(s, seed)).copied();
            match found {
                Some(id) if self.get(id) == s => return id,
                Some(_) => seed += 1,
                None => break,
            }
        }
        if self.offsets.is_empty() {
            self.offsets.push(0);
        }
        let id = self.len() as u32;
        self.bytes.to_mut().extend_from_slice(s.as_bytes());
        let end = self.bytes.len() as u32;
        self.offsets.push(end);
        self.lookup().insert(hash(s, seed), id);
        id
    }

    /// Ids of every string `matches` accepts.
    pub(crate) fn matching(&self, mut matches: impl FnMut(&str) -> bool) -> Vec<bool> {
        (0..self.len() as u32)
            .map(|id| matches(self.get(id)))
            .collect()
    }
}
