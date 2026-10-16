//! Updates through the NTFS change journal, on this machine's real file system.
#![cfg(windows)]

mod common;

use common::{Project, U3, U5, object, standard};
use marlin_asset_db::{AssetDb, Options, UpdateMode};

fn journal_options() -> Options {
    Options {
        journal: true,
        ..Options::default()
    }
}

#[test]
fn journal_updates_find_exactly_the_changes() {
    let p = Project::new("journal");
    standard(&p);
    let mut db = AssetDb::build(&p.root, journal_options()).unwrap();
    assert!(
        db.has_journal(),
        "the temp directory is on NTFS here; the journal must be readable without elevation"
    );
    db.save().unwrap();
    drop(db);

    // Nothing changed: the journal says so without listing directories.
    let (mut db, report) = AssetDb::open(&p.root, journal_options()).unwrap();
    assert_eq!(report.mode, UpdateMode::Journal, "{report:?}");
    assert_eq!(
        (report.read, report.added, report.removed),
        (0, 0, 0),
        "{report:?}"
    );

    // File edit, new file in a new directory, deletion, directory rename.
    p.write(
        "textures/rock.masset",
        &format!("!Version 1\n{}", object("Texture", U3, "Granite", "")),
    );
    p.write(
        "props/deep/barrel.masset",
        &format!("!Version 1\n{}", object("Prop", U5, "Barrel", "")),
    );
    p.remove("broken.masset");
    p.rename("level", "levels");
    let report = db.update().unwrap();
    assert_eq!(report.mode, UpdateMode::Journal, "{report:?}");
    assert_eq!(
        (report.added, report.changed, report.removed),
        (2, 1, 2),
        "{report:?}"
    );
    assert!(
        report.checked < 10,
        "only the touched paths are checked: {report:?}"
    );
    assert!(db.file("levels/probe.masset").is_some());
    assert!(db.file("level/probe.masset").is_none());
    assert!(db.file("props/deep/barrel.masset").is_some());
    assert!(db.file("broken.masset").is_none());
    assert_eq!(
        db.object_info(db.object(U3.parse().unwrap()).unwrap()).name,
        "Granite"
    );

    // A file under the renamed directory, after the rename: its directory FRN was learned.
    p.write("levels/extra.png", "x");
    let report = db.update().unwrap();
    assert_eq!(
        (report.mode, report.added),
        (UpdateMode::Journal, 1),
        "{report:?}"
    );
    assert!(db.file("levels/extra.png").is_some());

    // The journal result matches a full rebuild.
    let rebuilt = AssetDb::build(&p.root, p.options()).unwrap();
    let mut a: Vec<String> = db.files().map(|f| db.file_info(f).path).collect();
    let mut b: Vec<String> = rebuilt.files().map(|f| rebuilt.file_info(f).path).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

#[test]
fn changes_outside_the_tree_are_ignored() {
    let p = Project::new("outside");
    standard(&p);
    let other = Project::new("elsewhere");
    let mut db = AssetDb::build(&p.root, journal_options()).unwrap();
    if !db.has_journal() {
        panic!("no change journal on this volume");
    }
    for i in 0..50 {
        other.write(&format!("dir{i}/f.masset"), "x");
    }
    other.remove("dir0");
    let report = db.update().unwrap();
    assert_eq!(report.mode, UpdateMode::Journal, "{report:?}");
    assert_eq!((report.checked, report.read), (0, 0), "{report:?}");
}

#[test]
fn a_root_without_subdirectories_uses_the_journal() {
    let p = Project::new("flat");
    p.write(
        "a.masset",
        &format!("!Version 1\n{}", object("Thing", U3, "A", "")),
    );
    let mut db = AssetDb::build(&p.root, journal_options()).unwrap();
    assert!(db.has_journal());
    db.save().unwrap();
    p.write("b.png", "x");
    let report = db.update().unwrap();
    assert_eq!(
        (report.mode, report.added),
        (UpdateMode::Journal, 1),
        "{report:?}"
    );
}
