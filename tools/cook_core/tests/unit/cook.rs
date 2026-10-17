//! Cooking a project: keys, caching, unit layout, errors and the manifest.

use marlin_asset::cooked::{BULK_ALIGN, BulkKind, Encoding, UnitView, unit_id};
use marlin_asset::{Link, SourceFile, Uuid};

use marlin_cook_core::{
    CookContext, CookOptions, Cooked, Cooker, Cookers, Ddc, Manifest, Platform, SourceObject,
    TextureFamily, cook_all, publish_manifest,
};
use marlin_reflect::{Reflect, type_info};
use marlin_test_support::panic_message;

use crate::support::{Project, uuid};

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Thing {
    value: u32,
    other: Link<Thing>,
    label: String,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Image {
    source: SourceFile,
    scale: f32,
}

/// What the image cooker writes as payload: the runtime form.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct CookedImage {
    bytes: u64,
    scale: f32,
}

struct ImageCooker;

impl Cooker for ImageCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let image: &Image = object.value.as_ref().downcast().ok_or("not an Image")?;
        let data = ctx.read_source(&image.source.path)?;
        let cooked = CookedImage {
            bytes: data.len() as u64,
            scale: image.scale,
        };
        ctx.add_bulk(BulkKind::Texture, data);
        Ok(Cooked {
            type_id: type_info::<CookedImage>().id,
            encoding: Encoding::Serial,
            payload: marlin_serial::to_bytes(&cooked).map_err(|e| format!("{e:?}"))?,
        })
    }
}

/// Reads a file it has no link to.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Sneaky {
    path: String,
}

struct SneakyCooker;

impl Cooker for SneakyCooker {
    fn version(&self) -> u32 {
        0
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let sneaky: &Sneaky = object.value.as_ref().downcast().ok_or("not Sneaky")?;
        ctx.read_source(&sneaky.path)?;
        unreachable!("read_source refuses undeclared files")
    }
}

fn cookers() -> Cookers {
    let mut c = Cookers::new();
    c.register_generic::<Thing>().unwrap();
    c.register::<Image>(ImageCooker).unwrap();
    c.register::<Sneaky>(SneakyCooker).unwrap();
    c
}

const A1: Uuid = uuid(0xa);
const B1: Uuid = uuid(0xb);
const C1: Uuid = uuid(0xc);
const I1: Uuid = uuid(0xaa);

/// a: A (links B in b), A2 (links A locally); b: B; img: Picture (source pic.bin).
fn standard(p: &Project) {
    p.write(
        "a.masset",
        format!(
            "!Version 1\nThing {{ !Uuid = \"{A1}\" !Name = \"A\" Value = 1 Other = <b:B #{B1}> Label = \"first\" }}\nThing {{ !Uuid = \"{C1}\" !Name = \"A2\" Other = <:A #{A1}> }}\n"
        ),
    );
    p.write(
        "b.masset",
        format!("!Version 1\nThing {{ !Uuid = \"{B1}\" !Name = \"B\" Value = 2 }}\n"),
    );
    p.write(
        "img.masset",
        format!(
            "!Version 1\nImage {{ !Uuid = \"{I1}\" !Name = \"Picture\" Source = `source:pic.bin` Scale = 2.5 }}\n"
        ),
    );
    p.write("pic.bin", "pixels!");
}

fn key_of(report: &marlin_cook_core::CookReport, path: &str) -> u128 {
    report.units.iter().find(|u| u.path == path).unwrap().key
}

#[test]
fn cooks_everything_once_then_hits_the_cache() {
    let p = Project::new("cache");
    standard(&p);
    let db = p.db();
    let cookers = cookers();
    let ddc = p.ddc("ddc");
    let options = CookOptions::default();

    let (report, manifest) = cook_all(&db, &cookers, &ddc, &options);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!((report.cooked(), report.cached()), (3, 0));

    let (again, manifest2) = cook_all(&db, &cookers, &ddc, &options);
    assert_eq!((again.cooked(), again.cached()), (0, 3));
    assert_eq!(manifest, manifest2, "same inputs, same build");

    // The unit's layout and contents.
    let bytes = ddc.get(key_of(&report, "a")).unwrap().unwrap();
    let unit = UnitView::new(&bytes).unwrap();
    assert_eq!(unit.header().unit, unit_id("a"));
    assert_eq!(unit.object_count(), 2);
    assert!(
        unit.object(0).unwrap().uuid < unit.object(1).unwrap().uuid,
        "sorted"
    );
    let (_, a) = unit.find(A1).unwrap();
    assert_eq!(a.encoding, Encoding::Serial);
    assert_eq!(a.type_id, type_info::<Thing>().id.0);
    let thing: Thing = marlin_serial::from_bytes(unit.payload(&a)).unwrap();
    assert_eq!(
        thing,
        Thing {
            value: 1,
            other: Link::new(B1),
            label: "first".into()
        }
    );
    assert_eq!(
        unit.import_count(),
        1,
        "the link to B leaves the unit; A2's link to A does not"
    );
    let import = unit.import(0).unwrap();
    assert_eq!((import.uuid, import.unit), (B1, unit_id("b")));

    let bytes = ddc.get(key_of(&report, "img")).unwrap().unwrap();
    let unit = UnitView::new(&bytes).unwrap();
    let (_, picture) = unit.find(I1).unwrap();
    let cooked: CookedImage = marlin_serial::from_bytes(unit.payload(&picture)).unwrap();
    assert_eq!(
        cooked,
        CookedImage {
            bytes: 7,
            scale: 2.5
        }
    );
    let blobs: Vec<_> = unit.object_bulk(&picture).collect();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].kind, BulkKind::Texture);
    assert_eq!(unit.bulk_bytes(&blobs[0]), b"pixels!");
    assert_eq!(unit.header().bulk_offset % BULK_ALIGN, 0);

    // The manifest knows every unit and object.
    assert_eq!(manifest.units.len(), 3);
    assert_eq!(manifest.unit_of(C1), Some(unit_id("a")));
    assert_eq!(manifest.unit_key(unit_id("b")), Some(key_of(&report, "b")));
    let key = publish_manifest(&p.root, &ddc, &manifest).unwrap();
    let stored = Manifest::from_bytes(&ddc.get(key).unwrap().unwrap()).unwrap();
    assert_eq!(stored, manifest);
    assert!(
        p.root
            .join(format!(".marlin/build/{}.manifest", Platform::host().name))
            .exists()
    );
}

#[test]
fn only_changed_inputs_cook_again() {
    let p = Project::new("incremental");
    standard(&p);
    let cookers = cookers();
    let ddc = p.ddc("ddc");
    let options = CookOptions::default();
    let mut db = p.db();
    let (first, _) = cook_all(&db, &cookers, &ddc, &options);

    // A source file changes: only the unit using it.
    p.write("pic.bin", "new pixels");
    db.update().unwrap();
    let (r, _) = cook_all(&db, &cookers, &ddc, &options);
    assert_eq!(r.cooked(), 1);
    assert_ne!(key_of(&r, "img"), key_of(&first, "img"));
    assert_eq!(key_of(&r, "a"), key_of(&first, "a"));

    // B moves to another file: a's text is unchanged, but its import table is not.
    p.write("b.masset", "!Version 1\n");
    p.write(
        "c.masset",
        format!("!Version 1\nThing {{ !Uuid = \"{B1}\" !Name = \"B\" Value = 2 }}\n"),
    );
    db.update().unwrap();
    let (r, _) = cook_all(&db, &cookers, &ddc, &options);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_ne!(key_of(&r, "a"), key_of(&first, "a"));
    let bytes = ddc.get(key_of(&r, "a")).unwrap().unwrap();
    let unit = UnitView::new(&bytes).unwrap();
    assert_eq!(unit.import(0).unwrap().unit, unit_id("c"));
    // a, b (now empty) and the new c cooked; img cached.
    assert_eq!((r.cooked(), r.cached()), (3, 1), "{:?}", r.units);
}

#[test]
fn cooking_is_deterministic() {
    let p = Project::new("determinism");
    standard(&p);
    let db = p.db();
    let cookers = cookers();
    let (one, two) = (p.ddc("one"), p.ddc("two"));
    let options = CookOptions {
        threads: 1,
        ..CookOptions::default()
    };
    let (r1, m1) = cook_all(&db, &cookers, &one, &options);
    let (r2, m2) = cook_all(
        &db,
        &cookers,
        &two,
        &CookOptions {
            threads: 8,
            ..CookOptions::default()
        },
    );
    assert_eq!(m1, m2);
    for u in &r1.units {
        assert_eq!(key_of(&r2, &u.path), u.key);
        assert_eq!(
            one.get(u.key).unwrap(),
            two.get(u.key).unwrap(),
            "{}",
            u.path
        );
    }
    // Another platform is another build, and so is another texture description of the same one.
    let host = Platform::host();
    let renamed = Platform {
        name: "other",
        ..*host
    };
    let retextured = Platform {
        texture_compression: &[TextureFamily::Astc],
        ..*host
    };
    for platform in [renamed, retextured] {
        let platform: &'static Platform = Box::leak(Box::new(platform));
        let other = cook_all(
            &db,
            &cookers,
            &one,
            &CookOptions {
                platform,
                ..CookOptions::default()
            },
        );
        assert_ne!(key_of(&other.0, "a"), key_of(&r1, "a"));
    }
}

#[test]
fn errors_stay_with_their_unit() {
    let p = Project::new("errors");
    standard(&p);
    p.write(
        "unknown.masset",
        format!(
            "!Version 1\nMystery {{ !Uuid = \"{}\" !Name = \"M\" }}\n",
            uuid(0x100)
        ),
    );
    p.write(
        "badvalue.masset",
        format!(
            "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"T\" Value = \"nope\" }}\n",
            uuid(0x101)
        ),
    );
    p.write(
        "dangling.masset",
        format!(
            "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"D\" Other = <x:Gone #{}> }}\n",
            uuid(0x102),
            uuid(0x999)
        ),
    );
    p.write(
        "sneaky.masset",
        format!(
            "!Version 1\nSneaky {{ !Uuid = \"{}\" !Name = \"S\" Path = \"pic.bin\" }}\n",
            uuid(0x103)
        ),
    );
    p.write(
        "nosource.masset",
        format!(
            "!Version 1\nImage {{ !Uuid = \"{}\" !Name = \"N\" Source = `source:missing.bin` }}\n",
            uuid(0x104)
        ),
    );
    let db = p.db();
    let ddc = p.ddc("ddc");
    let (report, manifest) = cook_all(&db, &cookers(), &ddc, &CookOptions::default());
    let messages: Vec<String> = report.errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(report.errors.len(), 5, "{messages:#?}");
    let has = |text: &str| messages.iter().any(|m| m.contains(text));
    assert!(
        has("unknown.masset: M: no cooker for type `Mystery`"),
        "{messages:#?}"
    );
    assert!(
        has("badvalue.masset: T: ") && has("expected an integer"),
        "{messages:#?}"
    );
    assert!(has("dangling.masset: D: links to"), "{messages:#?}");
    assert!(
        has("sneaky.masset: S: `pic.bin` is not a source file this object links to"),
        "{messages:#?}"
    );
    assert!(
        has("nosource.masset: N: source file `missing.bin` does not exist"),
        "{messages:#?}"
    );
    assert_eq!(report.cooked(), 3, "the good units still cooked");
    assert_eq!(
        manifest.units.len(),
        3,
        "the manifest holds only what cooked"
    );
}

#[test]
fn a_source_saved_after_indexing_is_not_cached_under_the_old_key() {
    let p = Project::new("stale-source");
    standard(&p);
    let db = p.db();
    // Saved after the index read it: the key covers the old bytes, the cooker would read new.
    p.write("pic.bin", "new pixels");
    let ddc = p.ddc("ddc");
    let (report, manifest) = cook_all(&db, &cookers(), &ddc, &CookOptions::default());
    let messages: Vec<String> = report.errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(messages.len(), 1, "{messages:#?}");
    assert!(
        messages[0].contains("img.masset: Picture: `pic.bin` changed since it was indexed"),
        "{messages:#?}"
    );
    assert!(report.units.iter().all(|u| u.path != "img"));
    assert_eq!(ddc.len(), 2, "nothing stored for the image");
    assert_eq!(manifest.units.len(), 2);
}

/// Panics: a bug in a cooker.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Bomb {
    value: u32,
}

struct BombCooker;

impl Cooker for BombCooker {
    fn version(&self) -> u32 {
        0
    }

    fn cook(&self, _: &SourceObject<'_>, _: &mut CookContext<'_>) -> Result<Cooked, String> {
        panic!("cooker bug")
    }
}

#[test]
fn a_cooker_panic_is_not_swallowed() {
    let p = Project::new("panic");
    standard(&p);
    p.write(
        "bomb.masset",
        format!(
            "!Version 1\nBomb {{ !Uuid = \"{}\" !Name = \"B\" }}\n",
            uuid(0x200)
        ),
    );
    let db = p.db();
    let mut cookers = cookers();
    cookers.register::<Bomb>(BombCooker).unwrap();
    let ddc = p.ddc("ddc");
    let options = CookOptions {
        threads: 2,
        ..CookOptions::default()
    };
    // The panic reaches the caller.
    let message = panic_message(|| {
        cook_all(&db, &cookers, &ddc, &options);
    });
    assert_eq!(message, "cooker bug");
}
