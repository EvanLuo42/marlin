//! Building, querying, updating and persisting the index (full-scan mode).

use crate::support::{Project, U1, U2, U3, U4, U5, object, standard};
use marlin_asset_db::{AssetDb, UpdateMode};
use marlin_masset::Links;

fn names(db: &AssetDb, objects: Vec<marlin_asset_db::ObjectId>) -> Vec<String> {
    let mut names: Vec<String> = objects
        .into_iter()
        .map(|o| db.object_info(o).name.to_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn indexes_files_objects_and_links() {
    let p = Project::new("index");
    standard(&p);
    let db = AssetDb::build(&p.root, p.options()).unwrap();

    assert_eq!(
        db.file_count(),
        4,
        "hidden and excluded directories are skipped"
    );
    assert_eq!(db.object_count(), 4);
    assert!(db.file("target/debug/thing.masset").is_none());

    let probe_file = db.file("level/probe.masset").unwrap();
    assert_eq!(db.asset_path(probe_file), "level/probe");
    let info = db.file_info(probe_file);
    assert_eq!(info.parse_error, None);
    assert_ne!(info.hash, 0);
    assert_eq!(
        names(&db, db.objects_in(probe_file).collect()),
        ["Albedo", "Probe"]
    );

    let probe = db.object(U1).unwrap();
    let probe_info = db.object_info(probe);
    assert_eq!(
        (probe_info.name, probe_info.type_name),
        ("Probe", "EnvironmentProbe")
    );
    let links: Vec<_> = db.links(probe).collect();
    assert_eq!(links.len(), 2);
    assert_eq!(
        (links[1].path, links[1].name, links[1].target),
        ("textures/rock", "Rock", Some(U3))
    );

    let broken = db.file("broken.masset").unwrap();
    assert!(
        db.file_info(broken)
            .parse_error
            .unwrap()
            .contains("not closed")
    );
    assert_eq!(db.objects_in(broken).count(), 0);

    assert_eq!(
        names(&db, db.objects_of_type("Texture")),
        ["Albedo", "Rock"]
    );
    assert_eq!(names(&db, db.search("ROC")), ["Rock"]);
    assert_eq!(
        names(&db, db.search("o")),
        ["Albedo", "Moss", "Probe", "Rock"]
    );
    assert_eq!(names(&db, db.references_to(U3)), ["Moss", "Probe"]);
    assert!(db.references_to(U1).is_empty());
    assert_eq!(names(&db, db.dependencies(U1)), ["Albedo", "Rock"]);
    assert_eq!(
        names(&db, db.users_of_source("textures/rock.png")),
        ["Rock"]
    );
    assert_eq!(db.resolve("textures/rock", "Moss"), Some(U4));
    assert_eq!(
        db.describe(U4),
        Some(("textures/rock".to_owned(), "Moss".to_owned())),
        "the index backs marlin_masset::Links"
    );
}

#[test]
fn updates_only_what_changed() {
    let p = Project::new("update");
    standard(&p);
    let mut db = AssetDb::build(&p.root, p.options()).unwrap();

    let report = db.update().unwrap();
    assert_eq!(report.mode, UpdateMode::FullScan);
    assert_eq!(
        (report.checked, report.read),
        (4, 0),
        "nothing changed, nothing read"
    );

    // Change one file, add one, remove one, move a directory.
    p.write(
        "textures/rock.masset",
        format!("!Version 1\n{}", object("Texture", U3, "Granite", "")),
    );
    p.write(
        "props/barrel.masset",
        format!(
            "!Version 1\n{}",
            object(
                "Prop",
                U5,
                "Barrel",
                &format!("    Mat = <textures/rock:Moss #{U4}>\n")
            )
        ),
    );
    p.remove("broken.masset");
    p.rename("level", "levels/one");
    let report = db.update().unwrap();
    assert_eq!(
        (report.added, report.changed, report.removed),
        (2, 1, 2),
        "{report:?}: barrel and the moved probe added; rock changed; broken and the old probe removed"
    );
    assert_eq!(report.read, 3);

    assert!(
        db.object(U4).is_none(),
        "Moss is gone from the rewritten file"
    );
    assert_eq!(db.object_info(db.object(U3).unwrap()).name, "Granite");
    assert_eq!(
        db.asset_path(db.object_info(db.object(U1).unwrap()).file),
        "levels/one/probe"
    );
    assert!(db.file("level/probe.masset").is_none());
    assert_eq!(names(&db, db.references_to(U3)), ["Probe"]);
    assert_eq!(db.file_count(), 4);
}

#[test]
fn snapshots_round_trip_and_open_without_reading() {
    let p = Project::new("snapshot");
    standard(&p);
    let mut db = AssetDb::build(&p.root, p.options()).unwrap();
    let first = db.save().unwrap();
    assert!(first.exists());
    assert_eq!(db.generation(), 1);
    drop(db);

    let (db, report) = AssetDb::open(&p.root, p.options()).unwrap();
    assert_eq!((report.checked, report.read), (4, 0), "{report:?}");
    assert_eq!(db.object_count(), 4);
    assert_eq!(names(&db, db.references_to(U3)), ["Moss", "Probe"]);
    assert_eq!(names(&db, db.search("alb")), ["Albedo"]);
    assert!(
        db.file_info(db.file("broken.masset").unwrap())
            .parse_error
            .is_some()
    );
    drop(db);

    // A change while closed is picked up by the next open; saving replaces the snapshot.
    p.write("textures/rock.png", "a different png");
    let (mut db, report) = AssetDb::open(&p.root, p.options()).unwrap();
    assert_eq!((report.changed, report.read), (1, 1), "{report:?}");
    let second = db.save().unwrap();
    assert_eq!(db.generation(), 2);
    assert!(!first.exists(), "the older snapshot is removed when unused");
    assert!(second.exists());
}

#[test]
fn a_damaged_snapshot_is_rebuilt() {
    let p = Project::new("damaged");
    standard(&p);
    let mut db = AssetDb::build(&p.root, p.options()).unwrap();
    let path = db.save().unwrap();
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let (db, report) = AssetDb::open(&p.root, p.options()).unwrap();
    assert_eq!(report.read, 4, "fell back to a full rebuild");
    assert_eq!(db.object_count(), 4);
}

#[test]
fn compaction_keeps_the_content() {
    let p = Project::new("compact");
    standard(&p);
    let mut db = AssetDb::build(&p.root, p.options()).unwrap();
    for round in 0..6 {
        p.write(
            "textures/rock.masset",
            format!(
                "!Version 1\n{}\n{}",
                object("Texture", U3, &format!("Rock{round}"), ""),
                object(
                    "Material",
                    U4,
                    "Moss",
                    &format!("    Base = <:Rock{round} #{U3}>\n")
                )
            ),
        );
        db.update().unwrap();
    }
    db.save().unwrap();
    drop(db);
    let (db, _) = AssetDb::open(&p.root, p.options()).unwrap();
    assert_eq!(db.object_count(), 4);
    assert_eq!(db.object_info(db.object(U3).unwrap()).name, "Rock5");
    assert_eq!(names(&db, db.references_to(U3)), ["Moss", "Probe"]);
    assert_eq!(db.resolve("level/probe", "Albedo"), Some(U2));
}
