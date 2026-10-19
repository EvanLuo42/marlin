//! A level placing a Prefab (with a nested Prefab and overrides) cooked through the pipeline:
//! the instances expanded, the overrides applied, Prefab edits recooking the level.

use std::sync::Arc;

use marlin_asset::cooked::{Encoding, UnitView};
use marlin_cook_core::Cookers;
use marlin_math::DVec3;
use marlin_reflect::{Reflect, type_info};
use marlin_world::{
    ComponentFlags, ComponentRegistry, CookedLevel, EntityGuid, EntityName, Transform,
};

use crate::support::{CookedUnit, Project, uuid};

#[derive(Clone, Copy, PartialEq, Debug, Reflect)]
#[reflect(name = "cook_level_test::Lamp", default)]
struct Lamp {
    intensity: f32,
    range: f32,
}

impl Default for Lamp {
    fn default() -> Self {
        Self {
            intensity: 100.0,
            range: 5.0,
        }
    }
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

/// A lamp: a post (the root) with a light on top.
fn lamp_prefab(intensity: f32) -> String {
    format!(
        r#"!Version 1

Prefab
{{
    !Uuid = "{}"
    !Name = "Lamp"
    Entities = [
        {{ Guid = "{}" Name = "Post" Components = [Transform] }},
        {{ Guid = "{}" Name = "Light" Parent = "{}" Components = [Transform {{ Translation = (0.0, 2.0, 0.0) }}, Lamp {{ Intensity = {intensity} }}] }},
    ]
}}
"#,
        uuid(0xa1),
        guid(1),
        guid(2),
        guid(1),
    )
}

/// A street: a root and two lamps, the second dimmer.
fn street_prefab() -> String {
    format!(
        r#"!Version 1

Prefab
{{
    !Uuid = "{}"
    !Name = "Street"
    Entities = [{{ Guid = "{}" Name = "Street" Components = [Transform] }}]
    Instances = [
        {{ Guid = "{}" Parent = "{}" Prefab = <prefabs/lamp:Lamp> Transform = {{ Translation = (-5.0, 0.0, 0.0) }} }},
        {{
            Guid = "{}" Parent = "{}" Prefab = <prefabs/lamp:Lamp> Transform = {{ Translation = (5.0, 0.0, 0.0) }}
            Overrides = [Set {{ Entity = "{}" Component = Lamp {{ Intensity = 10.0 }} Fields = ["intensity"] }}]
        }},
    ]
}}
"#,
        uuid(0xa2),
        guid(10),
        guid(11),
        guid(10),
        guid(12),
        guid(10),
        guid(2),
    )
}

/// A level with the street moved and its second lamp's range overridden from outside.
fn level() -> String {
    let nested_light = EntityGuid::derive(guid_of(12), guid_of(2)).0.to_string();
    format!(
        r#"!Version 1

Level
{{
    !Uuid = "{}"
    !Name = "Main"
    Entities = [{{ Guid = "{}" Name = "Sun" Components = [Transform] }}]
    Instances = [
        {{
            Guid = "{}" Name = "MainStreet" Prefab = <prefabs/street:Street> Transform = {{ Translation = (0.0, 0.0, 100.0) }}
            Overrides = [Set {{ Entity = "{nested_light}" Component = Lamp {{ Range = 20.0 }} Fields = ["range"] }}]
        }},
    ]
}}
"#,
        uuid(0xb0),
        guid(20),
        guid(21),
    )
}

fn guid_of(n: u64) -> EntityGuid {
    EntityGuid(uuid(0x1000 + n))
}

fn cooked(unit: &CookedUnit) -> CookedLevel {
    let view = UnitView::new(&unit.bytes).unwrap();
    let object = view.object(0).unwrap();
    assert_eq!(object.type_id, type_info::<CookedLevel>().id.0);
    assert_eq!(object.encoding, Encoding::Serial);
    marlin_serial::from_bytes(view.payload(&object)).unwrap()
}

/// The cooked level's entity `guid`: its name, parent and components.
fn find<T: Reflect + Clone>(
    level: &CookedLevel,
    guid: EntityGuid,
) -> (String, EntityGuid, Option<T>) {
    for a in &level.archetypes {
        if let Some(row) = a.entities.iter().position(|e| e.guid == guid) {
            let e = &a.entities[row];
            let c = a
                .columns
                .iter()
                .find_map(|c| c[row].downcast_ref::<T>().cloned());
            return (e.name.clone(), e.parent, c);
        }
    }
    panic!("{guid:?} is not in the level");
}

#[test]
fn levels_cook_with_prefabs_expanded() {
    let p = Project::new("street");
    p.write("prefabs/lamp.masset", lamp_prefab(100.0));
    p.write("prefabs/street.masset", street_prefab());
    p.write("levels/main.masset", level());
    let cookers = cookers();
    let units = p.cook(&cookers).unwrap();
    let paths: Vec<&str> = units.iter().map(|u| u.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/Game/levels/main",
            "/Game/prefabs/lamp",
            "/Game/prefabs/street"
        ]
    );
    let level = cooked(&units[0]);
    // The sun, the street root and two lamps of two entities each.
    assert_eq!(level.entity_count(), 6);
    assert!(level.root.is_nil());

    let street = guid_of(21);
    let (name, parent, transform) = find::<Transform>(&level, street);
    assert_eq!((name.as_str(), parent), ("MainStreet", EntityGuid::NIL));
    assert_eq!(transform.unwrap().translation, DVec3::new(0.0, 0.0, 100.0));

    let lamp = |n: u64| EntityGuid::derive(street, EntityGuid::derive(guid_of(n), guid_of(2)));
    let (_, parent, first) = find::<Lamp>(&level, lamp(11));
    assert_eq!(parent, EntityGuid::derive(street, guid_of(11)));
    assert_eq!(
        first,
        Some(Lamp::default()),
        "the first lamp as the Prefab has it"
    );
    let (name, _, second) = find::<Lamp>(&level, lamp(12));
    assert_eq!(name, "Light");
    assert_eq!(
        second,
        Some(Lamp {
            intensity: 10.0,
            range: 20.0
        }),
        "the street's override and the level's, each on its field"
    );

    // The lamp Prefab cooks on its own too, for spawning: rooted at its post.
    let prefab = cooked(&units[1]);
    assert_eq!(prefab.root, guid_of(1));
    assert_eq!(prefab.entity_count(), 2);

    // Editing the lamp recooks the level through the street.
    p.write("prefabs/lamp.masset", lamp_prefab(400.0));
    let units = p.cook(&cookers).unwrap();
    let level = cooked(&units[0]);
    let (_, _, first) = find::<Lamp>(&level, lamp(11));
    assert_eq!(first.unwrap().intensity, 400.0);
    let (_, _, second) = find::<Lamp>(&level, lamp(12));
    assert_eq!(second.unwrap().intensity, 10.0, "still overridden");
    let (_, _, name) = find::<EntityName>(&level, street);
    assert!(name.is_none(), "names are the entity's, not a component");
}

#[test]
fn bad_prefabs_fail_the_cook() {
    let p = Project::new("bad");
    let two_roots = lamp_prefab(1.0).replace(&format!("Parent = \"{}\" ", guid(1)), "");
    p.write("prefabs/lamp.masset", two_roots);
    p.write("prefabs/street.masset", street_prefab());
    p.write("levels/main.masset", level());
    let errors = p.cook(&cookers()).unwrap_err().join("\n");
    assert!(errors.contains("exactly one root"), "{errors}");
    assert!(errors.contains("has 2 root entities"), "{errors}");

    p.write(
        "prefabs/lamp.masset",
        lamp_prefab(1.0).replace("Lamp {", "Nope {"),
    );
    let errors = p.cook(&cookers()).unwrap_err().join("\n");
    assert!(errors.contains("`Nope` is not a type"), "{errors}");
}
