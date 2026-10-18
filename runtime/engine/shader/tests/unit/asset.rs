use marlin_shader::asset::{LibraryEntry, ShaderLibrary};
use marlin_shader::reflect::{EntryInfo, ShaderReflection, Stage};

fn entry(name: &str, stage: Stage) -> LibraryEntry {
    LibraryEntry {
        name: name.into(),
        code: vec![3, 2, 35, 7],
        reflection: ShaderReflection {
            entry: EntryInfo {
                name: name.into(),
                stage,
                group_size: [1; 3],
            },
            ..Default::default()
        },
    }
}

fn library() -> ShaderLibrary {
    ShaderLibrary {
        package: "Marlin.Engine".into(),
        target: "vulkan-1.4".into(),
        entries: vec![
            entry("Marlin.Engine.ForwardPs", Stage::Pixel),
            entry("Marlin.Engine.ForwardVs", Stage::Vertex),
        ],
    }
}

#[test]
fn entries_are_found_by_full_name() {
    let lib = library();
    let vs = lib.entry("Marlin.Engine.ForwardVs").unwrap();
    assert_eq!(vs.reflection.entry.stage, Stage::Vertex);
    assert_eq!(
        lib.entry("Marlin.Engine.ForwardPs")
            .unwrap()
            .reflection
            .entry
            .stage,
        Stage::Pixel
    );
    assert!(lib.entry("Marlin.Engine.Forward").is_none());
    assert!(lib.entry("ForwardVs").is_none());
}

#[test]
fn a_library_round_trips_through_marlin_serial() {
    let lib = library();
    let bytes = marlin_serial::to_bytes(&lib).unwrap();
    assert_eq!(
        marlin_serial::from_bytes::<ShaderLibrary>(&bytes).unwrap(),
        lib
    );
}
