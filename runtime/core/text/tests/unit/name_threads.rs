//! Interning from many threads at once. The binary runs on `MarlinHeap`, so the interner's arena
//! and chunk allocations also go through the engine allocator from many threads.

use std::collections::HashMap;
use std::sync::Barrier;
use std::thread;

use marlin_text::Name;

const THREADS: usize = 16;
const WORDS: usize = 4_000;

fn word(i: usize) -> String {
    format!("threaded_word_{i}")
}

/// Thread `t`'s spelling of word `i`: one of three casings, so threads race to create the first
/// spelling (which owns the comparison index) and to add the others.
fn spelling(t: usize, i: usize) -> String {
    let w = word(i);
    match (t + i) % 3 {
        0 => w,
        1 => w.to_ascii_uppercase(),
        _ => w[..1].to_ascii_uppercase() + &w[1..],
    }
}

#[test]
fn concurrent_interning_agrees() {
    let barrier = Barrier::new(THREADS);
    let results: Vec<Vec<(String, Name)>> = thread::scope(|s| {
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let barrier = &barrier;
                s.spawn(move || {
                    barrier.wait();
                    // Each thread walks the words in a different order.
                    (0..WORDS)
                        .map(|k| {
                            let i = (k * (2 * t + 1) + t * 97) % WORDS;
                            let text = if k % 5 == 0 {
                                format!("{}_{}", spelling(t, i), k % 7)
                            } else {
                                spelling(t, i)
                            };
                            let name = Name::new(&text);
                            // Readers race with writers on the same shards.
                            assert_eq!(Name::find(&text), Some(name));
                            (text, name)
                        })
                        .collect()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut by_lowercase: HashMap<String, Name> = HashMap::new();
    let mut by_spelling: HashMap<String, Name> = HashMap::new();
    for (text, name) in results.into_iter().flatten() {
        assert_eq!(name.to_string(), text, "displays as interned");
        let first = *by_lowercase
            .entry(text.to_ascii_lowercase())
            .or_insert(name);
        assert_eq!(name, first, "{text}: all casings are one name");
        let same = *by_spelling.entry(text.clone()).or_insert(name);
        assert!(name.eq_exact(same), "{text}: one entry per spelling");
    }
    // Distinct lowercase strings are distinct names.
    let mut distinct: Vec<Name> = by_lowercase.values().copied().collect();
    distinct.sort_by(|a, b| a.cmp_lexical(*b));
    distinct.dedup();
    assert_eq!(distinct.len(), by_lowercase.len());
}

#[test]
fn names_cross_threads() {
    let names: Vec<Name> = (0..1000)
        .map(|i| Name::new(&format!("sent_{i}_x")))
        .collect();
    let back = thread::spawn(move || {
        names
            .iter()
            .map(|n| (n.to_string(), *n))
            .collect::<Vec<_>>()
    })
    .join()
    .unwrap();
    for (i, (s, n)) in back.into_iter().enumerate() {
        assert_eq!(s, format!("sent_{i}_x"));
        assert_eq!(Name::new(&s.to_uppercase()), n);
    }
}
