//! Data parallelism over rows, on scoped std threads.

use std::sync::Mutex;
use std::thread;

/// Calls `f(row_index, row)` for each `row_len`-element row of `data` (the last may be short),
/// spread over every core. Rows are handed out one at a time, so uneven rows balance.
pub(crate) fn for_each_row<T: Send>(
    data: &mut [T],
    row_len: usize,
    f: impl Fn(usize, &mut [T]) + Sync,
) {
    let row_len = row_len.max(1);
    let rows = data.len().div_ceil(row_len);
    let threads = thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(rows);
    if threads <= 1 {
        for (i, row) in data.chunks_mut(row_len).enumerate() {
            f(i, row);
        }
        return;
    }
    let next = Mutex::new(data.chunks_mut(row_len).enumerate());
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let Some((i, row)) = next.lock().unwrap().next() else {
                        break;
                    };
                    f(i, row);
                }
            });
        }
    });
}
