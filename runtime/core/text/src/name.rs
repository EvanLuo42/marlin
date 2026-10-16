//! `Name` and its global interner.
//!
//! Every distinct spelling gets a 32-bit index into a global entry table. Spellings that differ
//! only in ASCII case share a *comparison* index (that of the first spelling seen), so equality
//! is one integer compare while each spelling still displays as written.
//!
//! The table is chunked (4096 entries per chunk, chunks never move or free), so reading an entry
//! is lock-free. Interning goes through one of 64 shards picked by the case-insensitive hash; each
//! shard is an `RwLock` over a hash table of indices plus a bump arena for the characters. Lookups
//! of existing names take only the read lock.

use core::cmp::Ordering;
use core::hash::{Hash, Hasher};
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering as AtomicOrdering};
use core::{fmt, slice, str};

use marlin_containers::{HashTable, SmallVec};
use marlin_hash::xxh3_64_ascii_lowercase;
use marlin_std::alloc::alloc::{Layout, alloc, alloc_zeroed, dealloc, handle_alloc_error};
use marlin_std::sync::RwLock;

/// An interned string, like Unreal's `FName`.
///
/// - 12 bytes and `Copy`. `==` and `Hash` look at two integers, never at characters.
/// - Case-insensitive for ASCII, case-preserving: `Name::new("Hero") == Name::new("hero")`, yet
///   each displays the way it was written.
/// - A trailing `_<number>` is split off and stored as an integer, so `Enemy_0` to `Enemy_9999`
///   share one interned string. Only canonical numbers split (no leading zeros, below
///   `u32::MAX`), so every name displays exactly as it was created.
/// - Interned strings are never freed; [`base_str`](Self::base_str) is `&'static str`.
///
/// Indices are per process: persist names as strings or as [`stable_hash`](Self::stable_hash),
/// never as their indices. There is no `Ord` because index order is arbitrary; use
/// [`cmp_lexical`](Self::cmp_lexical) to sort.
#[derive(Clone, Copy)]
pub struct Name {
    /// Shared by every spelling that is equal ignoring ASCII case; 0 for the empty base.
    comparison: u32,
    /// This spelling's entry; 0 for the empty base.
    display: u32,
    /// Suffix + 1; 0 means no suffix.
    number: u32,
}

impl Name {
    /// The empty name. Also `Name::default()` and `Name::new("")`.
    pub const NONE: Self = Self {
        comparison: 0,
        display: 0,
        number: 0,
    };

    /// Interns `s`, splitting off a numeric suffix (`"Enemy_12"` is base `"Enemy"`, number 12).
    pub fn new(s: &str) -> Self {
        let (base, number) = split_number(s);
        Self::from_parts(base, number)
    }

    /// `base` with suffix `number` (displays as `base_number`). Panics if `number == u32::MAX`.
    pub fn with_number(base: &str, number: u32) -> Self {
        assert!(number != u32::MAX, "Name number out of range");
        Self::from_parts(base, number + 1)
    }

    fn from_parts(base: &str, number: u32) -> Self {
        let (comparison, display) = if base.is_empty() {
            (0, 0)
        } else {
            intern(base)
        };
        Self {
            comparison,
            display,
            number,
        }
    }

    /// The existing name equal to `s`, without interning anything. If `s` was only ever interned
    /// with different casing, the result displays with that casing.
    pub fn find(s: &str) -> Option<Self> {
        let (base, number) = split_number(s);
        if base.is_empty() {
            return Some(Self {
                number,
                ..Self::NONE
            });
        }
        let hash = xxh3_64_ascii_lowercase(base.as_bytes());
        let shard = shard_for(hash).read();
        let display = match shard.table.find(hash, |&i| entry(i).as_str() == base) {
            Some(&i) => i,
            None => *shard
                .table
                .find(hash, |&i| entry(i).as_str().eq_ignore_ascii_case(base))?,
        };
        Some(Self {
            comparison: entry(display).comparison,
            display,
            number,
        })
    }

    #[inline]
    pub const fn is_none(self) -> bool {
        self.comparison == 0 && self.number == 0
    }

    /// The numeric suffix, if any.
    #[inline]
    pub const fn number(self) -> Option<u32> {
        self.number.checked_sub(1)
    }

    /// The name without its numeric suffix.
    #[inline]
    pub const fn base(self) -> Self {
        Self { number: 0, ..self }
    }

    /// The base string as this name spells it (without the numeric suffix).
    #[inline]
    pub fn base_str(self) -> &'static str {
        if self.display == 0 {
            ""
        } else {
            entry(self.display).as_str()
        }
    }

    /// Equality that also respects case: same spelling and same number.
    #[inline]
    pub const fn eq_exact(self, other: Self) -> bool {
        self.display == other.display && self.number == other.number
    }

    /// The stable, case-insensitive hash of the full name, identical across runs and platforms:
    /// [`name_hash`] of its string. O(1) unless the name has a number.
    pub fn stable_hash(self) -> u64 {
        if self.number == 0 {
            return if self.display == 0 {
                name_hash("")
            } else {
                entry(self.display).hash
            };
        }
        let mut buf: SmallVec<u8, 128> = SmallVec::new();
        buf.extend_from_slice(self.base_str().as_bytes());
        buf.push(b'_');
        buf.extend_from_slice(DecimalU32::new(self.number - 1).as_bytes());
        xxh3_64_ascii_lowercase(&buf)
    }

    /// Orders by base string (ASCII-case-insensitive), then by number, so `A_9 < A_10`.
    /// Consistent with `==`.
    pub fn cmp_lexical(self, other: Self) -> Ordering {
        if self.comparison != other.comparison {
            let ord = crate::utf8::cmp_ignore_ascii_case(self.base_str(), other.base_str());
            if ord != Ordering::Equal {
                return ord;
            }
        }
        self.number.cmp(&other.number)
    }
}

/// The stable hash of the name `s`: XXH3-64 of `s` with ASCII lowercased. Equals
/// `Name::new(s).stable_hash()`, but needs no interning and works in `const` contexts.
#[inline]
pub const fn name_hash(s: &str) -> u64 {
    xxh3_64_ascii_lowercase(s.as_bytes())
}

impl Default for Name {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

impl PartialEq for Name {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.comparison == other.comparison && self.number == other.number
    }
}

impl Eq for Name {}

impl Hash for Name {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.comparison as u64 | (self.number as u64) << 32);
    }
}

impl PartialEq<str> for Name {
    /// Compares with a string without interning it.
    fn eq(&self, other: &str) -> bool {
        let (base, number) = split_number(other);
        self.number == number && self.base_str().eq_ignore_ascii_case(base)
    }
}

impl PartialEq<&str> for Name {
    fn eq(&self, other: &&str) -> bool {
        *self == **other
    }
}

impl From<&str> for Name {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.base_str())?;
        if let Some(n) = self.number() {
            f.write_str("_")?;
            f.write_str(DecimalU32::new(n).as_str())?;
        }
        Ok(())
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Name(\"{self}\")")
    }
}

/// Splits a canonical `_<number>` suffix: returns the base and number + 1, or `(s, 0)`.
fn split_number(s: &str) -> (&str, u32) {
    let b = s.as_bytes();
    let digits = b.iter().rev().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits == b.len() || digits > 10 {
        return (s, 0);
    }
    let start = b.len() - digits;
    // "_007" would display as "_7", so leading zeros stay part of the base.
    if b[start - 1] != b'_' || (digits > 1 && b[start] == b'0') {
        return (s, 0);
    }
    let n = b[start..]
        .iter()
        .fold(0u64, |n, &c| n * 10 + (c - b'0') as u64);
    if n >= u32::MAX as u64 {
        return (s, 0);
    }
    (&s[..start - 1], n as u32 + 1)
}

/// A `u32` formatted in decimal on the stack.
struct DecimalU32 {
    buf: [u8; 10],
    start: usize,
}

impl DecimalU32 {
    fn new(mut n: u32) -> Self {
        let mut buf = [0; 10];
        let mut start = buf.len();
        loop {
            start -= 1;
            buf[start] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        Self { buf, start }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    fn as_str(&self) -> &str {
        // SAFETY: only ASCII digits were written.
        unsafe { str::from_utf8_unchecked(self.as_bytes()) }
    }
}

// --- Interner ---

const CHUNK_BITS: u32 = 12;
const CHUNK_LEN: usize = 1 << CHUNK_BITS;
/// 4096 chunks of 4096: 16M distinct spellings. The chunk pointer table is 32 KiB of zeroed
/// static memory.
const MAX_CHUNKS: usize = 4096;
const SHARD_COUNT: usize = 64;
/// Arena block per shard. Strings over a quarter of this get their own allocation.
const BLOCK_SIZE: usize = 16 << 10;

struct Entry {
    ptr: *const u8,
    len: u32,
    comparison: u32,
    /// `name_hash` of the string: the shard and hash-table key, and the stable hash.
    hash: u64,
}

impl Entry {
    #[inline]
    fn as_str(&self) -> &'static str {
        // SAFETY: `ptr..ptr + len` was copied from a `&str` into arena memory that is never
        // freed or written again.
        unsafe { str::from_utf8_unchecked(slice::from_raw_parts(self.ptr, self.len as usize)) }
    }
}

static CHUNKS: [AtomicPtr<Entry>; MAX_CHUNKS] =
    [const { AtomicPtr::new(ptr::null_mut()) }; MAX_CHUNKS];
/// Index 0 is the empty name and has no entry.
static NEXT_INDEX: AtomicU32 = AtomicU32::new(1);
static SHARDS: [RwLock<Shard>; SHARD_COUNT] = [const { RwLock::new(Shard::new()) }; SHARD_COUNT];

struct Shard {
    /// Entry indices keyed by their case-insensitive hash; all spellings of a name collide.
    table: HashTable<u32>,
    cursor: *mut u8,
    remaining: usize,
}

// SAFETY: the arena pointer is only dereferenced under the shard's write lock, and the memory
// it points into is owned by the interner forever.
unsafe impl Send for Shard {}
// SAFETY: as above; shared access (read lock) never touches the arena pointer.
unsafe impl Sync for Shard {}

impl Shard {
    const fn new() -> Self {
        Self {
            table: HashTable::new(),
            cursor: ptr::null_mut(),
            remaining: 0,
        }
    }

    /// Copies `s` into memory that lives forever.
    fn store(&mut self, s: &str) -> *const u8 {
        let len = s.len();
        let dst = if len > BLOCK_SIZE / 4 {
            alloc_bytes(len)
        } else {
            if len > self.remaining {
                // The rest of the old block is abandoned; at most a quarter block is wasted.
                self.cursor = alloc_bytes(BLOCK_SIZE);
                self.remaining = BLOCK_SIZE;
            }
            let p = self.cursor;
            // SAFETY: len <= remaining, so this stays inside the block.
            self.cursor = unsafe { p.add(len) };
            self.remaining -= len;
            p
        };
        // SAFETY: `dst` has room for `len` bytes and cannot overlap `s`.
        unsafe { ptr::copy_nonoverlapping(s.as_ptr(), dst, len) };
        dst
    }
}

fn alloc_bytes(size: usize) -> *mut u8 {
    let layout = Layout::from_size_align(size, 1).expect("Name too long");
    // SAFETY: size > 0 (empty names are never stored).
    let p = unsafe { alloc(layout) };
    if p.is_null() {
        handle_alloc_error(layout);
    }
    p
}

#[inline]
fn shard_for(hash: u64) -> &'static RwLock<Shard> {
    // Middle bits: hashbrown uses the low bits for the bucket and the top 7 for its tags.
    &SHARDS[(hash >> 32) as usize % SHARD_COUNT]
}

#[inline]
fn entry(index: u32) -> &'static Entry {
    let chunk = CHUNKS[(index >> CHUNK_BITS) as usize].load(AtomicOrdering::Acquire);
    debug_assert!(!chunk.is_null() && index != 0);
    // SAFETY: indices only escape `intern` (into a shard table under its write lock, or a Name
    // returned to the caller) after their entry was written, and whoever obtained one did so
    // through that lock or through whatever synchronization handed them the Name. Chunks are
    // never moved or freed.
    unsafe { &*chunk.add(index as usize & (CHUNK_LEN - 1)) }
}

/// Returns `(comparison, display)` for a non-empty base string.
fn intern(s: &str) -> (u32, u32) {
    let hash = xxh3_64_ascii_lowercase(s.as_bytes());
    let lock = shard_for(hash);
    let exact = |&i: &u32| entry(i).as_str() == s;
    if let Some(&i) = lock.read().table.find(hash, exact) {
        return (entry(i).comparison, i);
    }

    let mut shard = lock.write();
    // Another thread may have interned it between the two locks.
    if let Some(&i) = shard.table.find(hash, exact) {
        return (entry(i).comparison, i);
    }
    let same_ignoring_case = shard
        .table
        .find(hash, |&i| entry(i).as_str().eq_ignore_ascii_case(s))
        .map(|&i| entry(i).comparison);

    let len = u32::try_from(s.len()).expect("Name too long");
    let index = allocate_index();
    let ptr = shard.store(s);
    let comparison = same_ignoring_case.unwrap_or(index);
    let new = Entry {
        ptr,
        len,
        comparison,
        hash,
    };
    let chunk = CHUNKS[(index >> CHUNK_BITS) as usize].load(AtomicOrdering::Acquire);
    // SAFETY: `allocate_index` made sure the chunk exists, and the index is ours alone: nothing
    // else reads or writes this slot until we publish the index below.
    unsafe { chunk.add(index as usize & (CHUNK_LEN - 1)).write(new) };
    shard.table.insert_unique(hash, index, |&i| entry(i).hash);
    (comparison, index)
}

/// Takes the next entry index and makes sure its chunk is allocated.
fn allocate_index() -> u32 {
    let index = NEXT_INDEX.fetch_add(1, AtomicOrdering::Relaxed);
    assert!(
        (index as usize) < MAX_CHUNKS * CHUNK_LEN,
        "too many distinct names"
    );
    let slot = &CHUNKS[(index >> CHUNK_BITS) as usize];
    if slot.load(AtomicOrdering::Acquire).is_null() {
        let layout = Layout::new::<[Entry; CHUNK_LEN]>();
        // SAFETY: the layout has non-zero size.
        let new = unsafe { alloc_zeroed(layout) }.cast::<Entry>();
        if new.is_null() {
            handle_alloc_error(layout);
        }
        let installed = slot.compare_exchange(
            ptr::null_mut(),
            new,
            AtomicOrdering::AcqRel,
            AtomicOrdering::Acquire,
        );
        if installed.is_err() {
            // Another thread allocated the same chunk first.
            // SAFETY: `new` was never published.
            unsafe { dealloc(new.cast(), layout) };
        }
    }
    index
}
