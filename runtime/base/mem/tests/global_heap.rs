//! Runs this whole test binary, std and the test harness included, on `MarlinHeap`. The tests run
//! on parallel threads, which adds cross-test contention on top of what each test does.
#![allow(clippy::undocumented_unsafe_blocks)]

use std::alloc::{GlobalAlloc, Layout};
use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use marlin_mem::{MarlinHeap, collect_abandoned, stats};

#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Mostly small, sometimes medium, rarely huge: roughly a game's mix.
    fn size(&mut self) -> usize {
        match self.below(100) {
            0..70 => 1 + self.below(256),
            70..97 => 1 + self.below(64 << 10),
            _ => 1 + self.below(4 << 20),
        }
    }

    fn align(&mut self) -> usize {
        [1, 2, 4, 8, 8, 8, 16, 16, 32, 64, 128, 4096][self.below(12)]
    }
}

/// A raw allocation carrying a recognizable pattern.
struct Alloc {
    ptr: *mut u8,
    layout: Layout,
    seed: u8,
}

// Moved between threads to test foreign frees.
unsafe impl Send for Alloc {}

/// Offsets that get patterned: all of a small block, a sample of a big one.
fn offsets(size: usize) -> impl Iterator<Item = usize> {
    let step = if size <= 4096 { 1 } else { 4093 };
    (0..size).step_by(step).chain([size - 1])
}

impl Alloc {
    fn new(size: usize, align: usize, seed: u8) -> Self {
        let layout = Layout::from_size_align(size, align).unwrap();
        let ptr = unsafe { HEAP.alloc(layout) };
        assert!(!ptr.is_null(), "{layout:?}");
        assert_eq!(ptr.addr() % align, 0, "{layout:?}");
        let alloc = Self { ptr, layout, seed };
        alloc.fill();
        alloc
    }

    fn fill(&self) {
        for i in offsets(self.layout.size()) {
            unsafe { self.ptr.add(i).write(self.seed ^ i as u8) };
        }
    }

    fn check(&self) {
        for i in offsets(self.layout.size()) {
            let byte = unsafe { self.ptr.add(i).read() };
            assert_eq!(
                byte,
                self.seed ^ i as u8,
                "corrupted at {i} in {:?}",
                self.layout
            );
        }
    }

    fn realloc(&mut self, new_size: usize) {
        let old = self.layout.size();
        let ptr = unsafe { HEAP.realloc(self.ptr, self.layout, new_size) };
        assert!(!ptr.is_null());
        assert_eq!(ptr.addr() % self.layout.align(), 0);
        self.ptr = ptr;
        self.layout = Layout::from_size_align(new_size, self.layout.align()).unwrap();
        // Every patterned byte that still fits must have survived the move.
        for i in offsets(old).filter(|&i| i < new_size) {
            assert_eq!(unsafe { ptr.add(i).read() }, self.seed ^ i as u8);
        }
        self.fill();
    }
}

impl Drop for Alloc {
    fn drop(&mut self) {
        self.check();
        unsafe { HEAP.dealloc(self.ptr, self.layout) };
    }
}

fn churn(seed: u64, ops: usize, max_live: usize) {
    let mut rng = Rng(seed);
    let mut live: Vec<Alloc> = Vec::new();
    for _ in 0..ops {
        match rng.below(10) {
            0..5 if live.len() < max_live => {
                let (size, align) = (rng.size(), rng.align());
                live.push(Alloc::new(size, align, rng.next() as u8));
            }
            0..7 if !live.is_empty() => {
                let i = rng.below(live.len());
                let size = rng.size();
                live[i].realloc(size);
            }
            _ if !live.is_empty() => {
                let i = rng.below(live.len());
                live.swap_remove(i);
            }
            _ => {}
        }
    }
}

#[test]
fn collections() {
    let mut map: HashMap<u64, Vec<u8>> = HashMap::new();
    for i in 0..20_000u64 {
        map.insert(i, vec![i as u8; (i % 300) as usize]);
    }
    let strings: Vec<String> = (0..10_000).map(|i| format!("string number {i}")).collect();
    let tree: BTreeMap<String, usize> = strings.iter().cloned().zip(0..).collect();
    for i in 0..20_000u64 {
        let v = &map[&i];
        assert!(v.len() == (i % 300) as usize && v.iter().all(|&b| b == i as u8));
    }
    assert_eq!(tree["string number 1234"], 1234);
}

#[test]
fn random_single_thread() {
    churn(0x9E37_79B9_7F4A_7C15, 50_000, 2_000);
}

#[test]
fn random_many_threads() {
    let threads: Vec<_> = (0..8)
        .map(|t| thread::spawn(move || churn(0xDEAD_BEEF + t, 20_000, 500)))
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn frees_from_other_threads() {
    let (tx, rx) = mpsc::sync_channel::<Vec<Alloc>>(16);
    let producers: Vec<_> = (0..4u64)
        .map(|t| {
            let tx = tx.clone();
            thread::spawn(move || {
                let mut rng = Rng(0x1234_5678 + t);
                for _ in 0..200 {
                    let batch = (0..100)
                        .map(|_| Alloc::new(rng.size().min(8192), rng.align(), rng.next() as u8))
                        .collect();
                    tx.send(batch).unwrap();
                }
            })
        })
        .collect();
    drop(tx);
    // Freed here, on another thread than the one that allocated.
    let consumer = thread::spawn(move || rx.iter().map(|batch| batch.len()).sum::<usize>());
    for producer in producers {
        producer.join().unwrap();
    }
    assert_eq!(consumer.join().unwrap(), 4 * 200 * 100);
}

#[test]
fn segments_of_exited_threads_are_reused() {
    for round in 0..4u64 {
        // The thread exits with its blocks still alive, so its segments are abandoned.
        let survivors = thread::spawn(move || {
            let mut rng = Rng(0xABCD + round);
            (0..20_000)
                .map(|_| Alloc::new(1 + rng.below(2048), 8, rng.next() as u8))
                .collect::<Vec<_>>()
        })
        .join()
        .unwrap();
        // Half is freed by this thread (a foreign free into abandoned segments) ...
        let (keep, free): (Vec<_>, Vec<_>) = survivors
            .into_iter()
            .enumerate()
            .partition(|(i, _)| i % 2 == 0);
        drop(free);
        // ... and a new thread allocating the same sizes reclaims the segments.
        thread::spawn(move || churn(0x5555 + round, 20_000, 1_000))
            .join()
            .unwrap();
        drop(keep);
    }
    let before = stats();
    collect_abandoned();
    println!("before collect: {before:?}\nafter collect:  {:?}", stats());
}

#[test]
fn huge_allocations_grow_in_place() {
    let mut v: Vec<u8> = Vec::new();
    for i in 0..(48 << 20) {
        v.push(i as u8);
    }
    assert!(v.iter().enumerate().all(|(i, &b)| b == i as u8));

    // Larger than a segment, over-aligned.
    let a = Alloc::new(9 << 20, 1 << 21, 7);
    drop(a);
}

#[test]
fn zeroed_after_reuse() {
    for size in [16, 200, 5000, 40_000, 100_000] {
        let layout = Layout::from_size_align(size, 8).unwrap();
        for _ in 0..50 {
            unsafe {
                let p = HEAP.alloc(layout);
                p.write_bytes(0xFF, size);
                HEAP.dealloc(p, layout);
                let z = HEAP.alloc_zeroed(layout);
                assert!((0..size).all(|i| z.add(i).read() == 0), "size {size}");
                HEAP.dealloc(z, layout);
            }
        }
    }
}

#[test]
fn over_aligned_blocks_freed_elsewhere() {
    let blocks: Vec<Alloc> = (0..5000)
        .map(|i| Alloc::new(1 + i % 500, [32, 64, 256, 1024][i % 4], i as u8))
        .collect();
    thread::spawn(move || drop(blocks)).join().unwrap();
}

/// `cargo test --release -p marlin-mem --test global_heap -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_against_system() {
    fn run<A: GlobalAlloc>(name: &str, a: &A) {
        let layouts: Vec<Layout> = (0..1024)
            .map(|i| Layout::from_size_align(16 + (i * 37) % 512, 8).unwrap())
            .collect();
        let mut ptrs = vec![std::ptr::null_mut(); 1024];
        let start = Instant::now();
        for _ in 0..2_000 {
            for (p, &l) in ptrs.iter_mut().zip(&layouts) {
                *p = unsafe { a.alloc(l) };
            }
            for (&p, &l) in ptrs.iter().zip(&layouts) {
                unsafe { a.dealloc(p, l) };
            }
        }
        let ns = start.elapsed().as_nanos() as f64 / (2_000.0 * 1024.0);
        println!("{name:>8}: {ns:.1} ns per alloc+free");
    }
    run("system", &std::alloc::System);
    run("marlin", &HEAP);
}
