//! marlin-io's test helpers: temporary files as marlin paths, and position-dependent test data.

// Each test binary uses a different subset.
#![allow(dead_code)]

use marlin_math::Rng;
use marlin_test_support::{TempDir, TempFile};

/// marlin-io takes UTF-8 paths; the system temp directory is one.
pub trait Utf8 {
    fn utf8(&self) -> &str;
}

impl Utf8 for std::path::Path {
    fn utf8(&self) -> &str {
        self.to_str().unwrap()
    }
}

/// A unique temporary file holding `data`, written with std.
pub fn with_data(name: &str, data: &[u8]) -> TempFile {
    let file = TempFile::new(&format!("io-{name}"));
    std::fs::write(&file, data).unwrap();
    file
}

/// A temporary directory as a normalized marlin path, canonical so that paths the file system
/// reports back (walks, watch events) share its prefix. Keep the [`TempDir`] alive while using it.
pub fn temp_dir(name: &str) -> (TempDir, marlin_io::PathBuf) {
    let dir = TempDir::new(&format!("io-{name}"));
    let path = marlin_io::PathBuf::from(dir.canonicalize().unwrap().utf8());
    (dir, path)
}

/// Deterministic, position-dependent bytes, so misplaced reads are caught.
pub fn pattern(len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    Rng::new(1).fill_bytes(&mut bytes);
    bytes
}
