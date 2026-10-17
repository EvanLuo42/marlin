//! Cooked string tables: key → message for one locale, read in place.
//!
//! # Layout
//!
//! All integers little-endian; `n` entries; the whole table is `32 + 24 n + blob` bytes.
//!
//! ```text
//! 0        magic "MSTB", version u32 (1), n u32, locale offset u32, locale length u32,
//!          blob length u32, 8 reserved bytes (0)
//! 32       key hashes      n × u64, strictly ascending: xxh3_64 of the key's UTF-8
//! 32+8n    entries         n × (key offset, key length, message offset, message length) u32
//! 32+24n   blob            UTF-8: the locale tag, keys and messages
//! ```
//!
//! Plain little-endian integers rather than `marlin_serial`'s cooked layout: marlin-serial
//! depends on marlin-reflect, which depends on this crate, and the table needs nothing but
//! offsets. Reading them with `from_le_bytes` also makes the table alignment-free and the same
//! on every platform. Lookups hash the key and binary-search the hashes; the key text is stored
//! too and compared, so a lookup never returns another key's message.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use alloc::collections::BTreeMap;
use marlin_hash::xxh3_64;

use super::{Arg, FormatError, Locale, Message, MessageError};

const MAGIC: [u8; 4] = *b"MSTB";
const VERSION: u32 = 1;
const HEADER: usize = 32;
const ENTRY: usize = 16;

/// The cooked type id of string tables: `marlin_reflect::TypeId::of_name("marlin_text::StringTable")`
/// (64-bit FNV-1a of the reflected name), the id the cooker gives `StringTable` source objects.
pub const STRING_TABLE_TYPE_ID: u64 = marlin_hash::fnv1a_64(b"marlin_text::StringTable");

/// Why bytes are not a string table.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::Error)]
pub enum TableError {
    #[error("not a string table (bad magic)")]
    Magic,
    #[error("unsupported string table version {0}")]
    Version(u32),
    #[error("string table truncated or sized wrongly")]
    Size,
    #[error("string table text is not UTF-8")]
    Utf8,
    #[error("string table entry {0} is out of range or not sorted")]
    Entry(usize),
    #[error("string table locale is not a valid tag")]
    Locale,
}

/// A cooked string table, read in place from its bytes.
#[derive(Clone, Copy, Debug)]
pub struct StringTable<'a> {
    bytes: &'a [u8],
    len: usize,
    blob: &'a str,
    locale: Locale,
}

#[inline]
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut b = [0; 4];
    b.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(b)
}

#[inline]
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}

impl<'a> StringTable<'a> {
    /// Checks and opens a table: the header, that the text is UTF-8, and that every entry lies
    /// in it on character boundaries with its key hashed and sorted as the lookup expects.
    /// Messages are not re-parsed (the builder checked them).
    pub fn new(bytes: &'a [u8]) -> Result<Self, TableError> {
        if bytes.len() < HEADER {
            return Err(TableError::Size);
        }
        if bytes[..4] != MAGIC {
            return Err(TableError::Magic);
        }
        let version = u32_at(bytes, 4);
        if version != VERSION {
            return Err(TableError::Version(version));
        }
        let len = u32_at(bytes, 8) as usize;
        let blob_len = u32_at(bytes, 20) as usize;
        let blob_at = len
            .checked_mul(8 + ENTRY)
            .and_then(|n| n.checked_add(HEADER))
            .ok_or(TableError::Size)?;
        if blob_at.checked_add(blob_len) != Some(bytes.len()) {
            return Err(TableError::Size);
        }
        let blob = core::str::from_utf8(&bytes[blob_at..]).map_err(|_| TableError::Utf8)?;
        let text = |offset: u32, len: u32| {
            let (offset, len) = (offset as usize, len as usize);
            blob.get(offset..offset.checked_add(len)?)
        };
        let locale = text(u32_at(bytes, 12), u32_at(bytes, 16)).ok_or(TableError::Locale)?;
        let locale = Locale::parse(locale).map_err(|_| TableError::Locale)?;
        let table = Self {
            bytes,
            len,
            blob,
            locale,
        };
        let mut previous = None;
        for i in 0..len {
            let hash = table.hash(i);
            let at = table.entry_at(i);
            let key = text(u32_at(bytes, at), u32_at(bytes, at + 4));
            let message = text(u32_at(bytes, at + 8), u32_at(bytes, at + 12));
            let (Some(key), Some(_)) = (key, message) else {
                return Err(TableError::Entry(i));
            };
            if xxh3_64(key.as_bytes()) != hash || previous.is_some_and(|p| p >= hash) {
                return Err(TableError::Entry(i));
            }
            previous = Some(hash);
        }
        Ok(table)
    }

    pub fn locale(&self) -> Locale {
        self.locale
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn hash(&self, i: usize) -> u64 {
        u64_at(self.bytes, HEADER + 8 * i)
    }

    #[inline]
    fn entry_at(&self, i: usize) -> usize {
        HEADER + 8 * self.len + ENTRY * i
    }

    /// Entry `i`'s text at field `field` (0 key, 1 message); checked in `new`.
    #[inline]
    fn text(&self, i: usize, field: usize) -> &'a str {
        let at = self.entry_at(i) + 8 * field;
        let offset = u32_at(self.bytes, at) as usize;
        let len = u32_at(self.bytes, at + 4) as usize;
        self.blob.get(offset..offset + len).unwrap_or("")
    }

    /// The message for `key`.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        let hash = xxh3_64(key.as_bytes());
        let (mut lo, mut hi) = (0, self.len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.hash(mid).cmp(&hash) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => {
                    return (self.text(mid, 0) == key).then(|| self.text(mid, 1));
                }
            }
        }
        None
    }

    /// The message for `key`, as a [`Message`] (checked when the table was built).
    pub fn message(&self, key: &str) -> Option<Message<'a>> {
        self.get(key).map(Message::new_unchecked)
    }

    /// Formats the message for `key` with this table's locale.
    pub fn format(
        &self,
        key: &str,
        args: &[Arg<'_>],
        out: &mut impl fmt::Write,
    ) -> Result<(), FormatError<'a>> {
        let message = self.message(key).ok_or(FormatError::UnknownKey)?;
        message.format(&self.locale, args, out)
    }

    /// Every `(key, message)`, in hash order.
    pub fn iter(&self) -> impl Iterator<Item = (&'a str, &'a str)> + '_ {
        (0..self.len).map(|i| (self.text(i, 0), self.text(i, 1)))
    }
}

/// String tables searched in order, most specific locale first (`fr-CA`, `fr`, `en`): the
/// first table with a key provides its message, which is formatted with *that table's* locale,
/// so an English fallback string gets English plural rules.
#[derive(Clone, Copy, Debug)]
pub struct Catalog<'t, 'a> {
    tables: &'t [StringTable<'a>],
}

impl<'t, 'a> Catalog<'t, 'a> {
    pub fn new(tables: &'t [StringTable<'a>]) -> Self {
        Self { tables }
    }

    pub fn tables(&self) -> &'t [StringTable<'a>] {
        self.tables
    }

    /// The message for `key` and the table it came from.
    pub fn get(&self, key: &str) -> Option<(&'a str, &'t StringTable<'a>)> {
        self.tables
            .iter()
            .find_map(|t| t.get(key).map(|message| (message, t)))
    }

    pub fn format(
        &self,
        key: &str,
        args: &[Arg<'_>],
        out: &mut impl fmt::Write,
    ) -> Result<(), FormatError<'a>> {
        let (message, table) = self.get(key).ok_or(FormatError::UnknownKey)?;
        Message::new_unchecked(message).format(&table.locale, args, out)
    }

    pub fn format_to_string(&self, key: &str, args: &[Arg<'_>]) -> Result<String, FormatError<'a>> {
        let mut out = String::new();
        self.format(key, args, &mut out)?;
        Ok(out)
    }
}

/// Why a string table could not be built.
#[derive(Clone, PartialEq, Eq, Debug, marlin_std::Error)]
pub enum BuildError {
    #[error("empty key")]
    EmptyKey,
    #[error("key `{0}` appears twice")]
    DuplicateKey(String),
    /// Two keys with the same 64-bit hash: rename one.
    #[error("keys `{0}` and `{1}` have the same hash")]
    HashCollision(String, String),
    #[error("message `{key}`: {error}")]
    Message { key: String, error: MessageError },
    #[error("the table is over 4 GiB")]
    TooLarge,
}

/// Builds the bytes of a [`StringTable`] (used by the cooker). The output depends only on the
/// locale and the set of entries, not on insertion order.
#[derive(Clone, Debug)]
pub struct StringTableBuilder {
    locale: Locale,
    /// By key hash: (key, message).
    entries: BTreeMap<u64, (String, String)>,
}

impl StringTableBuilder {
    pub fn new(locale: Locale) -> Self {
        Self {
            locale,
            entries: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds `key`, checking the message's syntax.
    pub fn insert(&mut self, key: &str, message: &str) -> Result<(), BuildError> {
        if key.is_empty() {
            return Err(BuildError::EmptyKey);
        }
        Message::parse(message).map_err(|error| BuildError::Message {
            key: key.into(),
            error,
        })?;
        match self.entries.entry(xxh3_64(key.as_bytes())) {
            alloc::collections::btree_map::Entry::Occupied(e) if e.get().0 == key => {
                Err(BuildError::DuplicateKey(key.into()))
            }
            alloc::collections::btree_map::Entry::Occupied(e) => {
                Err(BuildError::HashCollision(e.get().0.clone(), key.into()))
            }
            alloc::collections::btree_map::Entry::Vacant(e) => {
                e.insert((key.into(), message.into()));
                Ok(())
            }
        }
    }

    /// The table's bytes.
    pub fn build(&self) -> Result<Vec<u8>, BuildError> {
        let n = self.entries.len();
        let mut blob = String::new();
        let mut push = |s: &str| -> Result<(u32, u32), BuildError> {
            let offset = u32::try_from(blob.len()).map_err(|_| BuildError::TooLarge)?;
            let len = u32::try_from(s.len()).map_err(|_| BuildError::TooLarge)?;
            blob.push_str(s);
            Ok((offset, len))
        };
        let mut tag = String::new();
        // Writing to a `String` cannot fail.
        let _ = fmt::Write::write_fmt(&mut tag, format_args!("{}", self.locale));
        let locale = push(&tag)?;
        let mut entries = Vec::with_capacity(n);
        for (key, message) in self.entries.values() {
            entries.push((push(key)?, push(message)?));
        }
        let size = HEADER + (8 + ENTRY) * n + blob.len();
        let count = u32::try_from(n).map_err(|_| BuildError::TooLarge)?;
        let blob_len = u32::try_from(blob.len()).map_err(|_| BuildError::TooLarge)?;
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(&MAGIC);
        for v in [VERSION, count, locale.0, locale.1, blob_len, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for &hash in self.entries.keys() {
            out.extend_from_slice(&hash.to_le_bytes());
        }
        for ((key_at, key_len), (message_at, message_len)) in entries {
            for v in [key_at, key_len, message_at, message_len] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.extend_from_slice(blob.as_bytes());
        debug_assert_eq!(out.len(), size);
        Ok(out)
    }
}
