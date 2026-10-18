//! A throwaway asset project on disk, shared by the tests of marlin-asset-db and of the cook
//! crates (docs/design/0014-testing.md §6.3). The cook crates include this file by path from
//! their own `tests/support/mod.rs`, with `cook.rs` from marlin-cook-core's next to it; the
//! including crate needs marlin-asset, marlin-asset-db and marlin-test-support.
#![allow(dead_code, reason = "each test binary uses a subset")]

use std::fs;
use std::path::PathBuf;

use marlin_asset::Uuid;
use marlin_asset_db::{AssetDb, Content, GAME_MOUNT, Mount, Options};
use marlin_test_support::{TempDir, test_uuid};

/// Test object id `n`: `01990000-0000-7000-8000-<n as 12 hex digits>` in source text.
pub const fn uuid(n: u64) -> Uuid {
    Uuid::from_u128(test_uuid(n))
}

pub struct Project {
    pub root: TempDir,
}

impl Project {
    pub fn new(name: &str) -> Self {
        let package = env!("CARGO_PKG_NAME").trim_start_matches("marlin-");
        Self {
            root: TempDir::new(&format!("{package}-{name}")),
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Writes `contents`, dated after every earlier write so updates always see it.
    pub fn write(&self, rel: &str, contents: impl AsRef<[u8]>) {
        self.root.write_newer(rel, contents);
    }

    pub fn remove(&self, rel: &str) {
        let path = self.path(rel);
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }

    pub fn rename(&self, from: &str, to: &str) {
        let to = self.path(to);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::rename(self.path(from), to).unwrap();
    }

    /// Options for a full-scan index (the journal is tested on its own).
    pub fn options(&self) -> Options {
        Options {
            journal: false,
            ..Options::default()
        }
    }

    /// A full-scan index of the project as it is now.
    pub fn db(&self) -> AssetDb {
        AssetDb::build(&self.root, self.options()).unwrap()
    }

    /// The project as the only mount of some content, `Game`.
    pub fn content(&self) -> Content {
        self.content_as(GAME_MOUNT)
    }

    /// The project as the only mount of some content, named `mount`.
    pub fn content_as(&self, mount: &str) -> Content {
        Content::new(vec![Mount {
            name: mount.into(),
            db: self.db(),
        }])
    }
}
