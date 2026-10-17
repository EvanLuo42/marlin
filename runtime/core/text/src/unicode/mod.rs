//! Unicode algorithms on `str`, from tables generated out of the Unicode Character Database by
//! `cargo xtask gen-unicode` (version [`UNICODE_VERSION`], about 60 KB of tables).
//!
//! - **Case** ([`case`]): full and simple case folding (CaseFolding.txt, statuses C+F and
//!   C+S), caseless comparison ([`eq_ignore_case`], [`cmp_ignore_case`], [`Caseless`]) and the
//!   simple lowercase and uppercase mappings of UnicodeData.txt.
//! - **Normalization** ([`normalize`]): NFC, NFD, NFKC and NFKD (UAX #15) as iterators, into a
//!   `fmt::Write` or a `String`, plus the quick check and [`is_nfc`] and friends.
//! - **Segmentation** ([`segment`]): extended grapheme clusters and word boundaries (UAX #29).
//!
//! Everything works on `&str` (or any `char` iterator) and allocates nothing, except the
//! functions that return a `String` and normalizing a run of more than 16 combining marks.
//!
//! Not covered:
//!
//! - SpecialCasing.txt: no full, conditional or language-sensitive case mapping. `ß` uppercases
//!   to itself, a final sigma lowercases to `σ`, and Turkish/Azeri and Lithuanian rules are not
//!   applied. Caseless matching uses full folding, so `"STRASSE"` still matches `"straße"`.
//! - The Turkic folding entries (CaseFolding.txt status T).
//! - The grapheme rule GB9c (no break inside Indic conjuncts such as क्त): it needs the
//!   Indic_Conjunct_Break property of DerivedCoreProperties.txt, which the generator does not
//!   read. Those clusters are split after the virama.
//! - Tailorings: the segmentation is the default (untailored) one.
//!
//! The ASCII-only helpers in [`utf8`](crate::utf8) remain the fast choice for identifiers,
//! paths and config keys.

pub mod case;
pub mod normalize;
pub mod segment;
#[rustfmt::skip]
mod tables;

pub use case::{
    CaseFold, Caseless, cmp_ignore_case, eq_ignore_case, eq_ignore_case_canonical, fold,
    fold_simple, simple_fold, to_lower, to_lowercase, to_upper, to_uppercase,
};
pub use normalize::{
    Decompositions, Form, IsNormalized, Recompositions, combining_class, is_nfc, is_nfd, is_nfkc,
    is_nfkd, is_normalized, nfc, nfd, nfkc, nfkd, normalize, normalize_to, quick_check,
};
pub use segment::{
    GraphemeIndices, Graphemes, WordBoundIndices, WordBounds, grapheme_indices, graphemes,
    word_bound_indices, word_bounds,
};
pub use tables::UNICODE_VERSION;

/// Range tables: sorted entries `start << 11 | (len - 1)` with a parallel value array.
const RANGE_LEN_BITS: u32 = 11;
const RANGE_LEN_MASK: u32 = (1 << RANGE_LEN_BITS) - 1;

/// The value of `cp` in a range table; 0 outside every range.
#[inline]
fn range_value(keys: &[u32], values: &[u8], cp: u32) -> u8 {
    let i = keys.partition_point(|&k| k >> RANGE_LEN_BITS <= cp);
    match i.checked_sub(1) {
        Some(i) if cp - (keys[i] >> RANGE_LEN_BITS) <= keys[i] & RANGE_LEN_MASK => values[i],
        _ => 0,
    }
}

/// A delta table: entries `start << 11 | (count - 1) << 1 | stride_is_2`, each mapping `count`
/// code points (consecutive or every other one) by the same offset.
#[inline]
fn delta_map(keys: &[u32], deltas: &[i32], c: char) -> Option<char> {
    let cp = c as u32;
    let i = keys.partition_point(|&k| k >> RANGE_LEN_BITS <= cp);
    let i = i.checked_sub(1)?;
    let k = keys[i];
    let offset = cp - (k >> RANGE_LEN_BITS);
    let stride = 1 + (k & 1);
    let count = (k >> 1 & 0x3ff) + 1;
    if !offset.is_multiple_of(stride) || offset / stride >= count {
        return None;
    }
    char::from_u32(cp.wrapping_add_signed(deltas[i]))
}

/// A code point → string table (see `string_map` in the generator).
struct StrMap {
    keys: &'static [u32],
    index: &'static [u16],
    offsets: &'static [u16],
    pool: &'static str,
}

impl StrMap {
    #[inline]
    fn get(&self, cp: u32) -> Option<&'static str> {
        let i = self.keys.partition_point(|&k| k >> RANGE_LEN_BITS <= cp);
        let i = i.checked_sub(1)?;
        let offset = cp - (self.keys[i] >> RANGE_LEN_BITS);
        if offset > self.keys[i] & RANGE_LEN_MASK {
            return None;
        }
        let entry = self.index[i] as usize + offset as usize;
        self.pool
            .get(self.offsets[entry] as usize..self.offsets[entry + 1] as usize)
    }
}
