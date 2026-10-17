//! A throwaway asset root on disk.
#![allow(dead_code, reason = "each test file uses a subset")]

use std::fs;
use std::path::{Path, PathBuf};

use marlin_asset_db::Options;
use marlin_test_support::TempDir;

pub struct Project {
    pub root: TempDir,
}

impl Project {
    pub fn new(name: &str) -> Self {
        Self {
            root: TempDir::new(&format!("asset-db-{name}")),
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Writes `contents`, dated after every earlier write so updates always see it.
    pub fn write(&self, rel: &str, contents: &str) {
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
