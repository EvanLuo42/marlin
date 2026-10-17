//! Golden files: expected output kept under `tests/data/`, compared byte for byte, and rewritten
//! when `MARLIN_BLESS=1`. Golden images (with tolerances) are in `marlin-gpu-test-support`.

use std::fs;
use std::path::Path;

use crate::env::bless;

/// Asserts that `actual` equals the file at `path`; with `MARLIN_BLESS=1`, writes `actual` there
/// instead (creating parent directories).
pub fn assert_bytes(path: &Path, actual: &[u8]) {
    if bless() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, actual).unwrap();
        return;
    }
    let expected = fs::read(path).unwrap_or_else(|e| {
        panic!(
            "golden file {} is missing ({e}); run with MARLIN_BLESS=1 to create it",
            path.display()
        )
    });
    if expected != actual {
        let at = expected
            .iter()
            .zip(actual)
            .position(|(a, b)| a != b)
            .unwrap_or(expected.len().min(actual.len()));
        panic!(
            "{} differs at byte {at} ({} bytes expected, {} actual); \
             if the change is intended, rerun with MARLIN_BLESS=1",
            path.display(),
            expected.len(),
            actual.len()
        );
    }
}

/// As [`assert_bytes`], for text; line endings are normalized to `\n` on both sides.
pub fn assert_text(path: &Path, actual: &str) {
    if bless() {
        assert_bytes(path, actual.as_bytes());
        return;
    }
    let expected = fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "golden file {} is missing ({e}); run with MARLIN_BLESS=1 to create it",
            path.display()
        )
    });
    let expected = expected.replace("\r\n", "\n");
    let actual = actual.replace("\r\n", "\n");
    if expected != actual {
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(expected.lines().count().min(actual.lines().count()));
        panic!(
            "{} differs at line {}:\n  expected: {:?}\n  actual:   {:?}\n\
             if the change is intended, rerun with MARLIN_BLESS=1",
            path.display(),
            line + 1,
            expected.lines().nth(line).unwrap_or("<end>"),
            actual.lines().nth(line).unwrap_or("<end>"),
        );
    }
}
