//! A throwaway asset root on disk.
#![allow(dead_code, reason = "each test file uses a subset")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use marlin_asset_db::Options;

pub struct Project {
    pub root: PathBuf,
}

static NEXT: AtomicU32 = AtomicU32::new(0);

impl Project {
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "marlin-asset-db-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn write(&self, rel: &str, contents: &str) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        // Modification times can be coarse; push each write forward so updates always see it.
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        let bump = NEXT.fetch_add(1, Ordering::Relaxed) as u64;
        file.set_modified(SystemTime::now() + Duration::from_secs(bump + 1))
            .unwrap();
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
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}

/// An object block for test files.
pub fn object(ty: &str, uuid: &str, name: &str, body: &str) -> String {
    format!("{ty}\n{{\n    !Uuid = \"{uuid}\"\n    !Name = \"{name}\"\n{body}}}\n")
}

pub const U1: &str = "01990000-0000-7000-8000-000000000001";
pub const U2: &str = "01990000-0000-7000-8000-000000000002";
pub const U3: &str = "01990000-0000-7000-8000-000000000003";
pub const U4: &str = "01990000-0000-7000-8000-000000000004";
pub const U5: &str = "01990000-0000-7000-8000-000000000005";

/// A small project:
/// - level/probe.masset: Probe (links to its own Albedo and to textures/rock:Rock), Albedo
/// - textures/rock.masset: Rock (source file textures/rock.png), Moss (links Rock)
/// - textures/rock.png
/// - broken.masset: a syntax error
pub fn standard(p: &Project) {
    p.write(
        "level/probe.masset",
        &format!(
            "!Version 1\n{}\n{}",
            object(
                "EnvironmentProbe",
                U1,
                "Probe",
                &format!("    Albedo = <:Albedo #{U2}>\n    Rock = <textures/rock:Rock #{U3}>\n")
            ),
            object(
                "Texture",
                U2,
                "Albedo",
                "    Source = `source:level/albedo.png`\n"
            ),
        ),
    );
    p.write(
        "textures/rock.masset",
        &format!(
            "!Version 1\n{}\n{}",
            object(
                "Texture",
                U3,
                "Rock",
                "    Source = `source:textures/rock.png`\n"
            ),
            object(
                "Material",
                U4,
                "Moss",
                &format!("    Base = <:Rock #{U3}>\n")
            ),
        ),
    );
    p.write("textures/rock.png", "not really a png");
    p.write("broken.masset", "!Version 1\nThing {");
    // Never indexed.
    p.write(".git/config", "x");
    p.write("target/debug/thing.masset", "x");
}
