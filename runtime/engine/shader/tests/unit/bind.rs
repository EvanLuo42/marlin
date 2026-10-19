//! A subsystem's entries taken from a library: found by full path, in the order it lists them,
//! each checked for its stage and the push constants the subsystem sets.

use marlin_shader::asset::{LibraryEntry, ShaderLibrary};
use marlin_shader::bind::{BindError, EntryDesc, Problem, bind, bind_one, entries};
use marlin_shader::reflect::{EntryInfo, ShaderReflection, Stage, StructLayout};

#[repr(C)]
#[allow(dead_code)]
struct Args {
    a: u32,
    b: u32,
    c: f32,
}

const SET: [EntryDesc; 3] = [
    EntryDesc::compute("Test.Set.Main").push::<Args>(),
    EntryDesc::new("Test.Set.FullscreenVs", Stage::Vertex),
    EntryDesc::new("Test.Set.TonemapPs", Stage::Pixel).push_bytes(20),
];

fn entry(name: &str, stage: Stage, push: Option<u32>, code: u8) -> LibraryEntry {
    LibraryEntry {
        name: name.into(),
        code: vec![code; 4],
        reflection: ShaderReflection {
            entry: EntryInfo {
                name: name.into(),
                stage,
                group_size: [1, 1, 1],
            },
            push: push.map(|size| StructLayout {
                name: "Args".into(),
                size,
                fields: Vec::new(),
            }),
            ..ShaderReflection::default()
        },
    }
}

/// A library of `entries`, sorted by name as the cooker writes them.
fn library(mut entries: Vec<LibraryEntry>) -> ShaderLibrary {
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    ShaderLibrary {
        package: "Test".into(),
        target: "vulkan-1.4".into(),
        entries,
    }
}

fn set() -> ShaderLibrary {
    library(vec![
        entry("Test.Set.Main", Stage::Compute, Some(12), 1),
        entry("Test.Set.FullscreenVs", Stage::Vertex, None, 2),
        entry("Test.Set.TonemapPs", Stage::Pixel, Some(20), 3),
        entry("Test.Other", Stage::Compute, None, 4),
    ])
}

#[test]
fn a_push_struct_gives_its_size() {
    assert_eq!(SET[0].push, Some(12));
    assert_eq!(SET[1].push, None);
    assert_eq!(SET[2].push, Some(20));
}

#[test]
fn the_entries_come_in_the_subsystems_order() {
    let lib = set();
    let found = entries(&lib, &SET).unwrap();
    let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, SET.map(|d| d.name));

    let modules = bind(&lib, &SET).unwrap();
    assert_eq!(modules.map(|m| m.code[0]), [1, 2, 3]);
    assert!(modules.iter().all(|m| m.entry == "main"));
    assert_eq!(bind_one(&lib, &SET[2]).unwrap().code, &[3; 4]);
}

#[test]
fn a_missing_entry_is_named() {
    let mut lib = set();
    lib.entries.retain(|e| e.name != "Test.Set.TonemapPs");
    let e = bind(&lib, &SET).unwrap_err();
    assert_eq!(e, BindError::MissingEntry("Test.Set.TonemapPs"));
    assert!(e.to_string().contains("Test.Set.TonemapPs"), "{e}");
}

#[test]
fn an_entry_of_another_stage_does_not_bind() {
    let lib = library(vec![entry("Test.Set.Main", Stage::Pixel, Some(12), 1)]);
    let e = bind_one(&lib, &SET[0]).unwrap_err();
    assert_eq!(
        e,
        BindError::Mismatch {
            entry: "Test.Set.Main",
            problem: Problem::Stage {
                expected: Stage::Compute,
                found: Stage::Pixel,
            },
        }
    );
    assert!(e.to_string().contains("Pixel"), "{e}");
}

#[test]
fn push_constants_must_be_what_the_subsystem_sets() {
    let bigger = library(vec![entry("Test.Set.Main", Stage::Compute, Some(16), 1)]);
    let e = bind_one(&bigger, &SET[0]).unwrap_err();
    assert_eq!(
        e,
        BindError::Mismatch {
            entry: "Test.Set.Main",
            problem: Problem::Push {
                expected: Some(12),
                found: Some(16),
            },
        }
    );
    assert!(e.to_string().contains("16 bytes"), "{e}");

    // A push range where the subsystem sets none, and none where it sets one.
    let pushed = library(vec![entry(
        "Test.Set.FullscreenVs",
        Stage::Vertex,
        Some(4),
        2,
    )]);
    assert!(bind_one(&pushed, &SET[1]).is_err());
    let none = library(vec![entry("Test.Set.Main", Stage::Compute, None, 1)]);
    assert!(bind_one(&none, &SET[0]).is_err());
}
