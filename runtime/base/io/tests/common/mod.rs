#![allow(dead_code)]

use std::path::PathBuf;

use marlin_math::Rng;
use marlin_test_support::TempDir;

/// A file in a fresh temp directory, deleted (with the directory) on drop.
pub struct TempFile(pub PathBuf, TempDir);

impl TempFile {
    /// A unique path; the file is not created.
    pub fn new(name: &str) -> Self {
        let dir = TempDir::new(&format!("io-{name}"));
        Self(dir.join(name), dir)
    }

    /// A unique file holding `data`, written with std.
    pub fn with_data(name: &str, data: &[u8]) -> Self {
        let file = Self::new(name);
        std::fs::write(&file.0, data).unwrap();
        file
    }

    pub fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

/// Deterministic, position-dependent bytes, so misplaced reads are caught.
pub fn pattern(len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    Rng::new(1).fill_bytes(&mut bytes);
    bytes
}
