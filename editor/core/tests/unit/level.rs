//! A level placing a Prefab, opened from its sources: cooked as the cook cooks it, and stale
//! once a file it was read from changes.

use std::sync::Arc;

use marlin_cook_core::Cookers;
use marlin_editor_core::{LevelDocument, SourceContent};
use marlin_reflect::Reflect;
use marlin_world::{ComponentFlags, ComponentRegistry, CookedLevel, EntityGuid};

use crate::support::{Project, uuid};

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "editor_core_test::Lamp", default)]
struct Lamp {
    intensity: f32,
}

fn cookers() -> Cookers {
    let mut r = ComponentRegistry::new();
    r.add::<Lamp>(ComponentFlags::SAVED);
    let mut cookers = Cookers::new();
    marlin_cook_level::register(&mut cookers, Arc::new(r)).unwrap();
    cookers
}

fn guid(n: u64) -> String {
    uuid(0x1000 + n).to_string()
}

fn lamp_prefab(intensity: f32) -> String {
    format!(
        r#"!Version 1

Prefab
{{
    !Uuid = "{}"
    !Name = "Lamp"
    Entities = [
        {{ Guid = "{}" Name = "Post" Components = [Transform] }},
        {{ Guid = "{}" Name = "Light" Parent = "{}" Components = [Transform, Lamp {{ Intensity = {intensity} }}] }},
    ]
}}
"#,
        uuid(0xa1),
        guid(1),
        guid(2),
        guid(1),
    )
}

/// The Sun and two lamps, the second dimmed by an override.
fn level() -> String {
    format!(
        r#"!Version 1

Level
{{
    !Uuid = "{}"
    !Name = "Main"
    Entities = [{{ Guid = "{}" Name = "Sun" Components = [Transform] }}]
    Instances = [
        {{ Guid = "{}" Name = "Left" Prefab = <prefabs/lamp:Lamp> }},
        {{
            Guid = "{}" Name = "Right" Prefab = <prefabs/lamp:Lamp>
            Overrides = [Set {{ Entity = "{}" Component = Lamp {{ Intensity = 10.0 }} Fields = ["intensity"] }}]
        }},
    ]
}}
"#,
        uuid(0xb0),
        guid(20),
        guid(21),
        guid(22),
        guid(2),
    )
}

/// The cooked level's lamp intensities, by the name of the entity above each lamp.
fn intensities(level: &CookedLevel) -> Vec<(String, f32)> {
    let entities = || level.archetypes.iter().flat_map(|a| &a.entities);
    let name_of = |guid: EntityGuid| {
        entities()
            .find(|e| e.guid == guid)
            .map_or(String::new(), |e| e.name.clone())
    };
    let mut found = Vec::new();
    for a in &level.archetypes {
        for (row, e) in a.entities.iter().enumerate() {
            if let Some(lamp) = a.columns.iter().find_map(|c| c[row].downcast_ref::<Lamp>()) {
                found.push((name_of(e.parent), lamp.intensity));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

#[test]
fn documents_cook_as_the_cook_does_and_notice_their_files_changing() {
    let project = Project::new("level-document");
    project.write("prefabs/lamp.masset", lamp_prefab(100.0));
    project.write("levels/main.masset", level());
    let mut sources = SourceContent::from_parts(project.content(), cookers());

    let document = LevelDocument::open(&sources, uuid(0xb0)).unwrap();
    assert_eq!(document.prefabs.len(), 1);
    let cooked = document.cook().unwrap();
    // The Sun, and a post and a light per lamp.
    assert_eq!(cooked.entity_count(), 5);
    assert_eq!(
        intensities(&cooked),
        [("Left".into(), 100.0), ("Right".into(), 10.0)]
    );

    // Nothing changed: not stale.
    assert!(!sources.update().unwrap());
    assert!(!document.is_stale(&sources));
    // The Prefab brightened: the document is stale, and reopened it shows the edit.
    project.write("prefabs/lamp.masset", lamp_prefab(250.0));
    assert!(sources.update().unwrap());
    assert!(document.is_stale(&sources));
    let reopened = LevelDocument::open(&sources, uuid(0xb0)).unwrap();
    assert!(!reopened.is_stale(&sources));
    assert_eq!(
        intensities(&reopened.cook().unwrap()),
        [("Left".into(), 250.0), ("Right".into(), 10.0)]
    );
}
