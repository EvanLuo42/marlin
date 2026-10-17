//! The `MARLIN_*` variables that tune test runs (registered in docs/design/0014-testing.md §8),
//! and the paths tests read data from.

use std::path::PathBuf;

/// The seed for randomized tests: `MARLIN_TEST_SEED` if set, else `default`. Prints the seed
/// when it was overridden, so the run can be reproduced; tests should also put it in their
/// failure messages.
pub fn seed(default: u64) -> u64 {
    match std::env::var("MARLIN_TEST_SEED") {
        Ok(s) => {
            let seed = s
                .parse()
                .unwrap_or_else(|_| panic!("MARLIN_TEST_SEED={s:?} is not a u64"));
            eprintln!("MARLIN_TEST_SEED={seed}");
            seed
        }
        Err(_) => default,
    }
}

/// `n` times `MARLIN_TEST_SCALE` (default 1), for iteration counts of property and stress tests.
pub fn scale(n: usize) -> usize {
    let factor = match std::env::var("MARLIN_TEST_SCALE") {
        Ok(s) => s
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("MARLIN_TEST_SCALE={s:?} is not a count")),
        Err(_) => 1,
    };
    n.saturating_mul(factor.max(1))
}

/// Whether golden files should be rewritten instead of compared (`MARLIN_BLESS=1`).
pub fn bless() -> bool {
    std::env::var_os("MARLIN_BLESS").is_some_and(|v| v != "0")
}

/// `tests/data/<rel>` of the crate being tested. Pass `env!("CARGO_MANIFEST_DIR")`.
pub fn data_path(manifest_dir: &str, rel: &str) -> PathBuf {
    PathBuf::from(manifest_dir)
        .join("tests")
        .join("data")
        .join(rel)
}

/// A UUID-v7-shaped test id: `0x0199_0000_0000_7000_8000_…` with `n` in the low bits. Callers
/// wrap it in their UUID type (`Uuid::from_u128(test_uuid(n))`).
pub const fn test_uuid(n: u64) -> u128 {
    0x0199_0000_0000_7000_8000_0000_0000_0000 | n as u128
}

/// Records a skipped test: appends `test: reason` to the file named by `MARLIN_SKIP_LOG` (set by
/// `cargo xtask test`), and prints it to stderr. Use [`skip!`](crate::skip), which also returns.
pub fn record_skip(test: &str, reason: &str) {
    eprintln!("SKIP {test}: {reason}");
    if let Some(path) = std::env::var_os("MARLIN_SKIP_LOG") {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            // One write per record: tests skip concurrently, and an append of one buffer stays
            // whole.
            let _ = file.write_all(format!("{test}: {reason}\n").as_bytes());
        }
    }
}

/// Skips the rest of the test when something it needs is missing at run time (a GPU feature,
/// an extension the OS lacks): records why (see [`record_skip`]) and returns. The Rust test
/// harness has no skipped state, so the test shows as passed; `cargo xtask test` counts skips.
#[macro_export]
macro_rules! skip {
    ($($reason:tt)+) => {{
        $crate::record_skip(
            ::std::thread::current().name().unwrap_or("<unnamed>"),
            &::std::format!($($reason)+),
        );
        return;
    }};
}
