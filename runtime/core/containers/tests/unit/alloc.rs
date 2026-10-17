//! Containers in an arena instead of the global heap.

use std::alloc::{AllocError, Allocator, Global, Layout};
use std::cell::Cell;
use std::ptr::NonNull;

use marlin_containers::{
    BitSet, Deque, HashMap, HashMapInExt, HashSet, HashSetInExt, IndexMap, IndexSet, SecondaryMap,
    SlotMap, SmallVec, SparseSet,
};
use marlin_std::mem::Arena;
use marlin_test_support::Tracker;

/// Counts live blocks, to prove every allocation goes through it and is returned.
#[derive(Clone, Copy)]
struct Counting<'a>(&'a Cell<isize>);

// SAFETY: forwards to `Global`, which upholds the contract.
unsafe impl Allocator for Counting<'_> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.0.set(self.0.get() + 1);
        Global.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.0.set(self.0.get() - 1);
        // SAFETY: forwarded from the caller; the block came from `Global`.
        unsafe { Global.deallocate(ptr, layout) }
    }
}

#[test]
fn everything_in_an_arena() {
    let arena = Arena::new(16 << 20).unwrap();
    let a = &arena;

    let mut v: SmallVec<u64, 4, &Arena> = SmallVec::new_in(a);
    v.extend(0..100);
    assert!(v.spilled());
    assert_eq!(v.iter().sum::<u64>(), 4950);
    v.truncate(3);
    v.shrink_to_fit();
    assert!(!v.spilled());
    let back = SmallVec::<u64, 2, &Arena>::from(v.into_vec());
    assert_eq!(back.as_slice(), &[0, 1, 2]);

    let mut slots: SlotMap<String, &Arena> = SlotMap::new_in(a);
    let h = slots.insert("hero".into());
    let mut extra: SecondaryMap<String, u32, &Arena> = SecondaryMap::new_in(a);
    extra.insert(h, 7);
    assert_eq!((slots[h].as_str(), extra[h]), ("hero", 7));

    let mut sparse: SparseSet<u32, &Arena> = SparseSet::new_in(a);
    for k in (0..5000).step_by(7) {
        sparse.insert(k, k * 2);
    }
    assert_eq!(sparse.get(4998), Some(&9996));

    let mut map: HashMap<u32, u32, _, &Arena> = HashMap::new_in(a);
    let mut set: HashSet<u32, _, &Arena> = HashSet::with_capacity_in(10, a);
    for i in 0..1000 {
        map.insert(i, i + 1);
        set.insert(i * 3);
    }
    assert_eq!(map[&500], 501);
    assert!(set.contains(&2997));

    let mut ordered: IndexMap<&str, u32, _, &Arena> = IndexMap::new_in(a);
    ordered.insert("b", 1);
    ordered.insert("a", 2);
    assert_eq!(ordered.keys().copied().collect::<Vec<_>>(), ["b", "a"]);
    let mut names: IndexSet<u32, _, &Arena> = IndexSet::new_in(a);
    names.extend([5, 3, 5, 1]);
    assert_eq!(names.iter().copied().collect::<Vec<_>>(), [5, 3, 1]);

    let mut bits: BitSet<&Arena> = BitSet::new_in(a);
    bits.insert(10_000);
    assert!(bits.contains(10_000));

    let mut ring: Deque<u32, &Arena> = Deque::new_in(a);
    ring.push_front(1);
    ring.push_back(2);
    assert_eq!(ring.iter().copied().collect::<Vec<_>>(), [1, 2]);

    // Every container above took its memory from the arena.
    assert!(arena.used() > 64 << 10, "used {}", arena.used());
}

#[test]
fn every_block_goes_back_to_its_allocator() {
    let live = Cell::new(0);
    let t = Tracker::new();
    {
        let alloc = Counting(&live);
        let mut v: SmallVec<_, 2, _> = SmallVec::new_in(alloc);
        for i in 0..50 {
            v.push(t.make(i));
        }
        v.truncate(10);
        v.shrink_to_fit();
        let c = v.clone();
        let mut w = c.into_vec();
        w.push(t.make(99));
        let w: SmallVec<_, 2, _> = SmallVec::from(w);
        drop(w.into_iter().skip(3));

        let mut m: SlotMap<_, _> = SlotMap::new_in(alloc);
        let mut s: SparseSet<_, _> = SparseSet::new_in(alloc);
        let mut i: IndexMap<u32, _, _, _> = IndexMap::new_in(alloc);
        let mut b = BitSet::new_in(alloc);
        let mut x: SecondaryMap<_, _, _> = SecondaryMap::new_in(alloc);
        for n in 0..100 {
            let h = m.insert(t.make(n));
            x.insert(h, t.make(n));
            s.insert(n * 1000, t.make(n));
            i.insert(n, t.make(n));
            b.insert(n as usize * 64);
        }
        i.shift_remove(&3);
        let cloned = (m.clone(), s.clone(), i.clone(), x.clone());
        drop(cloned);
        let _ = i.into_iter().take(5).count();
        assert!(live.get() > 0);
    }
    assert_eq!(live.get(), 0, "blocks leaked");
    t.assert_all_dropped();
}
