#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// A file in the temp directory, deleted on drop.
pub struct TempFile(pub PathBuf);

impl TempFile {
    /// A unique path; the file is not created.
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        Self(std::env::temp_dir().join(format!("marlin-io-{pid}-{n}-{name}")))
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

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Deterministic, position-dependent bytes, so misplaced reads are caught.
pub fn pattern(len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9u32;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}
