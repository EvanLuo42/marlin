//! marlin-asset-db's test helpers: the shared [`Project`] and a small standard project in it.
#![allow(dead_code, reason = "each test file uses a subset")]

mod project;

use std::path::Path;

use marlin_asset::Uuid;
pub use project::{Project, uuid};

pub fn exists(path: &Path) -> bool {
    path.exists()
}

/// An object block for test files.
pub fn object(ty: &str, uuid: Uuid, name: &str, body: &str) -> String {
    format!("{ty}\n{{\n    !Uuid = \"{uuid}\"\n    !Name = \"{name}\"\n{body}}}\n")
}

pub const U1: Uuid = uuid(1);
pub const U2: Uuid = uuid(2);
pub const U3: Uuid = uuid(3);
pub const U4: Uuid = uuid(4);
pub const U5: Uuid = uuid(5);

/// A small project:
/// - level/probe.masset: Probe (links to its own Albedo and to textures/rock:Rock), Albedo
/// - textures/rock.masset: Rock (source file textures/rock.png), Moss (links Rock)
/// - textures/rock.png
/// - broken.masset: a syntax error
pub fn standard(p: &Project) {
    p.write(
        "level/probe.masset",
        format!(
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
        format!(
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
