//! Index timings at scale; `cargo test -p marlin-asset-db --release --test bench -- --ignored
//! --nocapture`.

mod common;

use std::time::Instant;

use common::Project;
use marlin_asset_db::AssetDb;

#[test]
#[ignore = "benchmark"]
fn bench_index() {
    const DIRS: usize = 250;
    const FILES_PER_DIR: usize = 200;
    const OBJECTS: usize = 4;
    let p = Project::new("bench");
    let uuid = |file: usize, object: usize| {
        format!(
            "01990000-0000-7000-8000-{:012x}",
            file * OBJECTS + object + 1
        )
    };
    let t = Instant::now();
    for d in 0..DIRS {
        for f in 0..FILES_PER_DIR {
            let file = d * FILES_PER_DIR + f;
            let mut text = String::from("!Version 1\n");
            for o in 0..OBJECTS {
                let target = (file * 7 + o * 13) % (DIRS * FILES_PER_DIR);
                text.push_str(&format!(
                    "\nMeshInstance\n{{\n    !Uuid = \"{}\"\n    !Name = \"Rock{file}_{o}\"\n    Mesh = <d{}/f{}:Rock{target}_0 #{}>\n    Lightmap = `source:lightmaps/l{file}.exr`\n    Transform = {{ Translation = (1.0, 2.0, 3.0) }}\n}}\n",
                    uuid(file, o),
                    target / FILES_PER_DIR,
                    target,
                    uuid(target, 0),
                ));
            }
            std::fs::create_dir_all(p.path(&format!("d{d}"))).unwrap();
            std::fs::write(p.path(&format!("d{d}/f{file}.masset")), text).unwrap();
        }
    }
    let files = DIRS * FILES_PER_DIR;
    println!("wrote {files} files in {:.2?}", t.elapsed());

    let t = Instant::now();
    let mut db = AssetDb::build(&p.root, p.options()).unwrap();
    println!("build (read + hash + extract): {:.2?}", t.elapsed());
    assert_eq!(db.object_count(), files * OBJECTS);

    let t = Instant::now();
    let path = db.save().unwrap();
    println!(
        "save: {:.2?}, {:.1} MiB",
        t.elapsed(),
        std::fs::metadata(&path).unwrap().len() as f64 / (1 << 20) as f64
    );
    drop(db);

    let t = Instant::now();
    let (db, report) = AssetDb::open(&p.root, p.options()).unwrap();
    println!(
        "open + no-op full-scan update: {:.2?} ({report:?})",
        t.elapsed()
    );
    assert_eq!(report.read, 0);

    let t = Instant::now();
    let found = db.search("rock123_");
    println!("search: {:.2?} ({} hits)", t.elapsed(), found.len());

    let t = Instant::now();
    let refs = db.references_to(uuid(5, 0).parse().unwrap());
    println!(
        "references_to (first call builds the reverse table): {:.2?} ({})",
        t.elapsed(),
        refs.len()
    );
    let t = Instant::now();
    let refs = db.references_to(uuid(6, 0).parse().unwrap());
    println!("references_to (warm): {:.2?} ({})", t.elapsed(), refs.len());

    let t = Instant::now();
    let deps = db.dependencies(uuid(0, 0).parse().unwrap());
    println!(
        "dependencies closure: {:.2?} ({} objects)",
        t.elapsed(),
        deps.len()
    );

    let t = Instant::now();
    let typed = db.objects_of_type("MeshInstance");
    println!("objects_of_type: {:.2?} ({})", t.elapsed(), typed.len());

    let mut db = db;
    let t = Instant::now();
    let issues = db.validate();
    println!("validate: {:.2?} ({} issues)", t.elapsed(), issues.len());
}
