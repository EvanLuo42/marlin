//! An open-addressing hash table from 128-bit keys to `u32` rows, stored as two columns so a
//! snapshot holds it ready to probe.
//!
//! Keys are UUIDs (objects) or 128-bit path hashes (files): already uniformly distributed, so
//! the low bits pick the slot. Key 0 marks an empty slot and `u128::MAX` a deleted one; neither
//! occurs as a real key (the nil UUID is never indexed, and a path hash of exactly 0 or MAX is
//! astronomically unlikely and rejected).

use crate::column::Column;

const EMPTY: u128 = 0;
const DELETED: u128 = u128::MAX;

#[derive(Default)]
pub(crate) struct KeyTable {
    pub(crate) keys: Column<u128>,
    pub(crate) rows: Column<u32>,
    /// Live entries.
    pub(crate) len: usize,
    /// Live plus deleted slots; drives rehashing.
    used: usize,
}

/// Spreads keys whose low bits are not random (time-ordered UUIDs keep entropy in the middle).
fn mix(key: u128) -> u64 {
    let x = (key >> 64) as u64 ^ key as u64;
    let x = (x ^ (x >> 31)).wrapping_mul(0x7fb5_d329_728e_a185);
    x ^ (x >> 27)
}

impl KeyTable {
    pub(crate) fn from_columns(keys: Column<u128>, rows: Column<u32>) -> Option<Self> {
        if keys.len() != rows.len() || (!keys.is_empty() && !keys.len().is_power_of_two()) {
            return None;
        }
        let len = keys.iter().filter(|&&k| k != EMPTY && k != DELETED).count();
        let used = keys.iter().filter(|&&k| k != EMPTY).count();
        Some(Self {
            keys,
            rows,
            len,
            used,
        })
    }

    pub(crate) fn valid_key(key: u128) -> bool {
        key != EMPTY && key != DELETED
    }

    pub(crate) fn get(&self, key: u128) -> Option<u32> {
        if self.keys.is_empty() || !Self::valid_key(key) {
            return None;
        }
        let mask = self.keys.len() - 1;
        let mut slot = mix(key) as usize & mask;
        loop {
            match self.keys[slot] {
                EMPTY => return None,
                k if k == key => return Some(self.rows[slot]),
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// Inserts or replaces.
    pub(crate) fn insert(&mut self, key: u128, row: u32) {
        debug_assert!(Self::valid_key(key));
        if (self.used + 1) * 10 > self.keys.len() * 7 {
            self.rehash((self.len + 1) * 2);
        }
        let mask = self.keys.len() - 1;
        let mut slot = mix(key) as usize & mask;
        let mut reuse = None;
        loop {
            match self.keys[slot] {
                EMPTY => break,
                DELETED => {
                    reuse.get_or_insert(slot);
                }
                k if k == key => {
                    self.rows.set(slot, row);
                    return;
                }
                _ => {}
            }
            slot = (slot + 1) & mask;
        }
        let slot = match reuse {
            Some(s) => s,
            None => {
                self.used += 1;
                slot
            }
        };
        self.keys.set(slot, key);
        self.rows.set(slot, row);
        self.len += 1;
    }

    pub(crate) fn remove(&mut self, key: u128) -> Option<u32> {
        if self.keys.is_empty() || !Self::valid_key(key) {
            return None;
        }
        let mask = self.keys.len() - 1;
        let mut slot = mix(key) as usize & mask;
        loop {
            match self.keys[slot] {
                EMPTY => return None,
                k if k == key => {
                    self.keys.set(slot, DELETED);
                    self.len -= 1;
                    return Some(self.rows[slot]);
                }
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    fn rehash(&mut self, min_capacity: usize) {
        let capacity = (min_capacity * 10 / 7 + 1).next_power_of_two().max(64);
        let old_keys = std::mem::take(&mut self.keys);
        let old_rows = std::mem::take(&mut self.rows);
        self.keys = Column::from(vec![EMPTY; capacity]);
        self.rows = Column::from(vec![0; capacity]);
        self.len = 0;
        self.used = 0;
        for (&key, &row) in old_keys.iter().zip(old_rows.iter()) {
            if Self::valid_key(key) {
                self.insert(key, row);
            }
        }
    }
}
