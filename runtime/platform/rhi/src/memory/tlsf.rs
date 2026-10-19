//! A two-level segregated fit (TLSF) offset allocator: O(1) allocation and free over a range of
//! `u64` offsets, with its bookkeeping outside the range (the range is GPU memory).

use alloc::vec::Vec;

/// Second-level bins per power of two: sizes are bucketed with a worst-case rounding of 1/32.
const SL_BITS: u32 = 5;
const SL_COUNT: u32 = 1 << SL_BITS;
/// Sizes below `SL_COUNT` share first-level bin 0, one byte per second-level bin; each power of
/// two from `SL_COUNT` up has a first-level bin of its own.
const FL_COUNT: usize = (64 - SL_BITS + 1) as usize;

const NIL: u32 = u32::MAX;

/// A range of the allocator, physically ordered among its neighbors.
#[derive(Clone, Copy)]
struct Node {
    offset: u64,
    size: u64,
    free: bool,
    /// Physical neighbors (`NIL` at the ends of the range).
    prev: u32,
    next: u32,
    /// The bin's free list when free; the list of unused node slots when unused.
    prev_free: u32,
    next_free: u32,
}

/// One allocation of a [`Tlsf`]: `size` bytes at `offset`. Give it back with [`Tlsf::free`].
#[derive(PartialEq, Eq, Debug)]
#[must_use = "the range stays allocated until it is freed"]
pub struct TlsfAllocation {
    pub offset: u64,
    pub size: u64,
    node: u32,
}

/// An allocator of offsets in `0..size`: GPU memory blocks, mega-buffers, any range whose
/// bookkeeping cannot live in the range itself.
///
/// Allocation and free are O(1): free ranges sit in bins by size (a power of two, split 32 ways),
/// two bitmaps find the first non-empty bin that fits, and freed ranges merge with free
/// neighbors at once. A request may be rounded up to its bin's lower bound when searching (good
/// fit, never worse than 1/32 over the request), but takes only its own size.
pub struct Tlsf {
    nodes: Vec<Node>,
    /// Head of the list of unused node slots.
    unused: u32,
    fl_bitmap: u64,
    sl_bitmaps: [u32; FL_COUNT],
    heads: [u32; FL_COUNT * SL_COUNT as usize],
    size: u64,
    used: u64,
    allocations: u32,
}

/// The bin `size` (> 0) is filed under.
fn bin(size: u64) -> (usize, u32) {
    if size < u64::from(SL_COUNT) {
        return (0, size as u32);
    }
    let f = 63 - size.leading_zeros();
    let sl = (size >> (f - SL_BITS)) as u32 - SL_COUNT;
    ((f - SL_BITS + 1) as usize, sl)
}

/// The first bin whose every range holds `size` bytes, `None` past the largest bin.
fn search_bin(size: u64) -> Option<(usize, u32)> {
    if size < u64::from(SL_COUNT) {
        return Some(bin(size));
    }
    let f = 63 - size.leading_zeros();
    let rounded = size.checked_add((1 << (f - SL_BITS)) - 1)?;
    Some(bin(rounded))
}

const fn align_up(value: u64, align: u64) -> Option<u64> {
    match value.checked_add(align - 1) {
        Some(v) => Some(v & !(align - 1)),
        None => None,
    }
}

impl Tlsf {
    /// An allocator of `size` bytes, all free.
    pub fn new(size: u64) -> Self {
        let mut t = Self {
            nodes: Vec::new(),
            unused: NIL,
            fl_bitmap: 0,
            sl_bitmaps: [0; FL_COUNT],
            heads: [NIL; FL_COUNT * SL_COUNT as usize],
            size,
            used: 0,
            allocations: 0,
        };
        if size > 0 {
            let n = t.new_node(0, size);
            t.insert_free(n);
        }
        t
    }

    /// The range's size.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Bytes allocated now (alignment padding excluded: it stays free).
    pub fn used(&self) -> u64 {
        self.used
    }

    /// Allocations not freed yet.
    pub fn allocation_count(&self) -> u32 {
        self.allocations
    }

    pub fn is_empty(&self) -> bool {
        self.allocations == 0
    }

    /// The largest free range (whether an aligned request fits also depends on where it starts).
    pub fn largest_free(&self) -> u64 {
        if self.fl_bitmap == 0 {
            return 0;
        }
        let fl = 63 - self.fl_bitmap.leading_zeros() as usize;
        let sl = 31 - self.sl_bitmaps[fl].leading_zeros();
        let mut n = self.heads[fl * SL_COUNT as usize + sl as usize];
        let mut largest = 0;
        while n != NIL {
            largest = largest.max(self.nodes[n as usize].size);
            n = self.nodes[n as usize].next_free;
        }
        largest
    }

    /// `size` bytes (0 counts as 1) at an offset aligned to `align` (a power of two), or `None`
    /// if no free range holds them.
    pub fn allocate(&mut self, size: u64, align: u64) -> Option<TlsfAllocation> {
        assert!(align.is_power_of_two(), "alignment must be a power of two");
        let size = size.max(1);
        // The first ranges of the request's own bin (or the next non-empty one) often fit, aligned
        // or not: try them first, so exact fits and naturally aligned ranges are found. Else take
        // a range that holds the request wherever it starts (good fit: bins rounded up).
        let node = self
            .find_from(bin(size))
            .and_then(|head| self.first_fitting(head, size, align))
            .or_else(|| self.find_from(search_bin(size.checked_add(align - 1)?)?))?;
        Some(self.take(node, size, align))
    }

    /// Returns `allocation`'s range. It must come from this allocator.
    pub fn free(&mut self, allocation: TlsfAllocation) {
        let mut n = allocation.node;
        let node = self.nodes[n as usize];
        assert!(
            !node.free && node.offset == allocation.offset && node.size == allocation.size,
            "an allocation of this allocator, freed once"
        );
        self.used -= node.size;
        self.allocations -= 1;
        self.nodes[n as usize].free = true;
        let prev = node.prev;
        if prev != NIL && self.nodes[prev as usize].free {
            self.remove_free(prev);
            self.nodes[prev as usize].size += node.size;
            self.unlink(n);
            n = prev;
        }
        let next = self.nodes[n as usize].next;
        if next != NIL && self.nodes[next as usize].free {
            self.remove_free(next);
            self.nodes[n as usize].size += self.nodes[next as usize].size;
            self.unlink(next);
        }
        self.insert_free(n);
    }

    /// The head of the first non-empty bin from `(fl, sl)` on.
    fn find_from(&self, (mut fl, sl): (usize, u32)) -> Option<u32> {
        let mut sl_map = self.sl_bitmaps[fl] & (u32::MAX << sl);
        if sl_map == 0 {
            let fl_map = self.fl_bitmap & u64::MAX.checked_shl(fl as u32 + 1).unwrap_or(0);
            if fl_map == 0 {
                return None;
            }
            fl = fl_map.trailing_zeros() as usize;
            sl_map = self.sl_bitmaps[fl];
        }
        let sl = sl_map.trailing_zeros();
        Some(self.heads[fl * SL_COUNT as usize + sl as usize])
    }

    /// The first range of `head`'s bin that holds `size` bytes aligned to `align`.
    fn first_fitting(&self, mut n: u32, size: u64, align: u64) -> Option<u32> {
        // A few tries: a long bin full of misaligned ranges falls back to the padded search.
        for _ in 0..4 {
            if n == NIL {
                break;
            }
            let node = &self.nodes[n as usize];
            let start = align_up(node.offset, align)?;
            if start - node.offset + size <= node.size {
                return Some(n);
            }
            n = node.next_free;
        }
        None
    }

    /// Allocates `size` bytes aligned to `align` from free node `n` (which holds them), freeing
    /// the padding before and the rest after.
    fn take(&mut self, n: u32, size: u64, align: u64) -> TlsfAllocation {
        self.remove_free(n);
        let node = self.nodes[n as usize];
        let start = align_up(node.offset, align).expect("the node holds the aligned request");
        let pad = start - node.offset;
        if pad > 0 {
            // The node before is not free (free neighbors merge), so the padding stays apart.
            let p = self.new_node(node.offset, pad);
            self.nodes[p as usize].prev = node.prev;
            self.nodes[p as usize].next = n;
            if node.prev != NIL {
                self.nodes[node.prev as usize].next = p;
            }
            self.nodes[n as usize].prev = p;
            self.insert_free(p);
        }
        let rest = node.size - pad - size;
        if rest > 0 {
            let r = self.new_node(start + size, rest);
            self.nodes[r as usize].prev = n;
            self.nodes[r as usize].next = node.next;
            if node.next != NIL {
                self.nodes[node.next as usize].prev = r;
            }
            self.nodes[n as usize].next = r;
            self.insert_free(r);
        }
        let node = &mut self.nodes[n as usize];
        node.offset = start;
        node.size = size;
        node.free = false;
        self.used += size;
        self.allocations += 1;
        TlsfAllocation {
            offset: start,
            size,
            node: n,
        }
    }

    fn new_node(&mut self, offset: u64, size: u64) -> u32 {
        let node = Node {
            offset,
            size,
            free: true,
            prev: NIL,
            next: NIL,
            prev_free: NIL,
            next_free: NIL,
        };
        if self.unused != NIL {
            let n = self.unused;
            self.unused = self.nodes[n as usize].next_free;
            self.nodes[n as usize] = node;
            n
        } else {
            self.nodes.push(node);
            (self.nodes.len() - 1) as u32
        }
    }

    /// Takes node `n` (merged into its previous neighbor) out of the physical order and recycles
    /// its slot.
    fn unlink(&mut self, n: u32) {
        let Node { prev, next, .. } = self.nodes[n as usize];
        if prev != NIL {
            self.nodes[prev as usize].next = next;
        }
        if next != NIL {
            self.nodes[next as usize].prev = prev;
        }
        self.nodes[n as usize].next_free = self.unused;
        self.unused = n;
    }

    fn insert_free(&mut self, n: u32) {
        let (fl, sl) = bin(self.nodes[n as usize].size);
        let i = fl * SL_COUNT as usize + sl as usize;
        let head = self.heads[i];
        let node = &mut self.nodes[n as usize];
        node.free = true;
        node.prev_free = NIL;
        node.next_free = head;
        if head != NIL {
            self.nodes[head as usize].prev_free = n;
        }
        self.heads[i] = n;
        self.fl_bitmap |= 1 << fl;
        self.sl_bitmaps[fl] |= 1 << sl;
    }

    fn remove_free(&mut self, n: u32) {
        let Node {
            size,
            prev_free,
            next_free,
            ..
        } = self.nodes[n as usize];
        if prev_free != NIL {
            self.nodes[prev_free as usize].next_free = next_free;
        }
        if next_free != NIL {
            self.nodes[next_free as usize].prev_free = prev_free;
        }
        let (fl, sl) = bin(size);
        let i = fl * SL_COUNT as usize + sl as usize;
        if self.heads[i] == n {
            self.heads[i] = next_free;
            if next_free == NIL {
                self.sl_bitmaps[fl] &= !(1 << sl);
                if self.sl_bitmaps[fl] == 0 {
                    self.fl_bitmap &= !(1 << fl);
                }
            }
        }
    }
}
