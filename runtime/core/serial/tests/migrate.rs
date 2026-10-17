//! Explicit migrations between data versions, on top of name-hash field matching.

use std::cell::RefCell;

use marlin_reflect::{Mut, Reflect};
use marlin_serial::{Error, Migrate, OldFields, from_bytes, to_bytes};

// Three versions of one type. v1 stored the volume in percent and a `mode` byte; v2 made the
// volume a fraction; v3 replaced `mode` with flags.

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "game::Settings", version = 1)]
struct SettingsV1 {
    volume_percent: u8,
    mode: u8,
    name: String,
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(
    name = "game::Settings",
    version = 2,
    data(Migrate::new(SettingsV2::migrate))
)]
struct SettingsV2 {
    volume: f32,
    mode: u8,
    name: String,
}

impl SettingsV2 {
    fn migrate(from: u32, value: Mut<'_>, old: &OldFields<'_>) -> Result<(), Error> {
        let this = value.downcast::<Self>().ok_or(Error::Incompatible)?;
        if from == 1 {
            this.volume = f32::from(old.get::<u8>("volume_percent")?.unwrap_or(100)) / 100.0;
        }
        Ok(())
    }
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(
    name = "game::Settings",
    version = 3,
    data(Migrate::new(SettingsV3::migrate))
)]
struct SettingsV3 {
    volume: f32,
    fullscreen: bool,
    vsync: bool,
    name: String,
}

thread_local! {
    static STEPS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

impl SettingsV3 {
    /// One step per version: 0 -> 1 (unversioned data), 1 -> 2, 2 -> 3.
    fn migrate(from: u32, value: Mut<'_>, old: &OldFields<'_>) -> Result<(), Error> {
        STEPS.with(|s| s.borrow_mut().push(from));
        let this = value.downcast::<Self>().ok_or(Error::Incompatible)?;
        match from {
            0 => this.name = format!("{} (legacy)", this.name),
            1 => {
                this.volume = f32::from(old.get::<u8>("volume_percent")?.unwrap_or(100)) / 100.0;
            }
            2 => {
                // `mode` is gone from v3, so it is an old field: 2 meant fullscreen with vsync.
                let mode = old.get::<u8>("mode")?.unwrap_or(0);
                this.fullscreen = mode >= 1;
                this.vsync = mode == 2;
            }
            _ => {}
        }
        Ok(())
    }
}

fn steps() -> Vec<u32> {
    STEPS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

#[test]
fn upgrade_chain_v1_to_v3() {
    let v1 = SettingsV1 {
        volume_percent: 40,
        mode: 2,
        name: "ana".into(),
    };
    let bytes = to_bytes(&v1).unwrap();
    steps();
    let v3: SettingsV3 = from_bytes(&bytes).unwrap();
    assert_eq!(steps(), [1, 2]);
    assert_eq!(
        v3,
        SettingsV3 {
            volume: 0.4,
            fullscreen: true,
            vsync: true,
            name: "ana".into(),
        }
    );

    // v2 data runs only the last step; v2 itself upgrades v1 data with its own migration.
    let v2: SettingsV2 = from_bytes(&bytes).unwrap();
    assert_eq!((v2.volume, v2.mode), (0.4, 2));
    let v3: SettingsV3 = from_bytes(&to_bytes(&v2).unwrap()).unwrap();
    assert_eq!(steps(), [2]);
    assert_eq!((v3.volume, v3.fullscreen, v3.vsync), (0.4, true, true));

    // Current data does not migrate; newer data read by an older type does not either.
    let again: SettingsV3 = from_bytes(&to_bytes(&v3).unwrap()).unwrap();
    assert_eq!(again, v3);
    let older: SettingsV2 = from_bytes(&to_bytes(&v3).unwrap()).unwrap();
    assert_eq!((older.volume, older.mode), (0.4, 0));
    assert!(steps().is_empty());
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "game::Settings")]
struct SettingsV0 {
    name: String,
}

#[test]
fn unversioned_data_counts_as_version_zero() {
    let bytes = to_bytes(&SettingsV0 { name: "bo".into() }).unwrap();
    steps();
    let v3: SettingsV3 = from_bytes(&bytes).unwrap();
    assert_eq!(steps(), [0, 1, 2]);
    assert_eq!(v3.name, "bo (legacy)");
    // No old fields: the defaults the migration falls back to.
    assert_eq!((v3.volume, v3.fullscreen), (1.0, false));
}

#[test]
fn versioned_types_nest() {
    #[derive(Reflect, Debug, PartialEq)]
    #[reflect(name = "game::Profile")]
    struct Profile1 {
        settings: Vec<SettingsV1>,
    }
    #[derive(Reflect, Debug, PartialEq)]
    #[reflect(name = "game::Profile")]
    struct Profile3 {
        settings: Vec<SettingsV3>,
    }
    let p = Profile1 {
        settings: vec![
            SettingsV1 {
                volume_percent: 10,
                mode: 0,
                name: "a".into(),
            },
            SettingsV1 {
                volume_percent: 50,
                mode: 1,
                name: "b".into(),
            },
        ],
    };
    let p3: Profile3 = from_bytes(&to_bytes(&p).unwrap()).unwrap();
    assert_eq!(p3.settings[0].volume, 0.1);
    assert_eq!(
        (p3.settings[1].fullscreen, p3.settings[1].vsync),
        (true, false)
    );
}

// Enums: a variant renamed with new fields. The old variant's fields reach the migration.

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "game::Shape", version = 1)]
enum Shape1 {
    Circle { r: f32 },
    Square { side: f32 },
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "game::Shape", version = 2, data(Migrate::new(Shape2::migrate)))]
enum Shape2 {
    Circle { r: f32 },
    Rect { w: f32, h: f32 },
}

impl Shape2 {
    fn migrate(from: u32, value: Mut<'_>, old: &OldFields<'_>) -> Result<(), Error> {
        if from == 1 && old.variant_is("Square") {
            let side = old.get::<f32>("side")?.unwrap_or(0.0);
            *value.downcast::<Self>().ok_or(Error::Incompatible)? =
                Shape2::Rect { w: side, h: side };
        }
        Ok(())
    }
}

#[test]
fn enum_migration_sees_removed_variants() {
    let square = to_bytes(&Shape1::Square { side: 3.0 }).unwrap();
    assert_eq!(
        from_bytes::<Shape2>(&square),
        Ok(Shape2::Rect { w: 3.0, h: 3.0 })
    );
    let circle = to_bytes(&Shape1::Circle { r: 1.0 }).unwrap();
    assert_eq!(from_bytes::<Shape2>(&circle), Ok(Shape2::Circle { r: 1.0 }));
}

// A migration attached at run time, and errors from a migration.

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "game::Counter", version = 1)]
struct Counter1 {
    count: u32,
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "game::Counter", version = 2)]
struct Counter2 {
    total: u64,
}

fn counter_migrate(from: u32, value: Mut<'_>, old: &OldFields<'_>) -> Result<(), Error> {
    assert_eq!(from, 1);
    assert!(old.contains("count") && !old.contains("total") && old.len() == 1);
    let count = old.get::<u32>("count")?.ok_or(Error::Incompatible)?;
    if count == 13 {
        return Err(Error::Incompatible);
    }
    value
        .downcast::<Counter2>()
        .ok_or(Error::Incompatible)?
        .total = u64::from(count) * 10;
    Ok(())
}

#[test]
fn registered_migration_and_errors() {
    let bytes = to_bytes(&Counter1 { count: 4 }).unwrap();
    // Without a migration, the fields are matched by name only.
    assert_eq!(from_bytes::<Counter2>(&bytes), Ok(Counter2 { total: 0 }));
    marlin_serial::register_migration::<Counter2>(counter_migrate).unwrap();
    assert_eq!(from_bytes::<Counter2>(&bytes), Ok(Counter2 { total: 40 }));
    // An error from the migration fails the read (the root has nothing to fall back to).
    let bad = to_bytes(&Counter1 { count: 13 }).unwrap();
    assert_eq!(from_bytes::<Counter2>(&bad), Err(Error::Incompatible));
}
