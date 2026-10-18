//! Index timings at scale: 50 000 files of 4 objects each. `cargo xtask test --tier bench -p
//! marlin-asset-db`, or `cargo bench -p marlin-asset-db --bench index -- [filter]`.

#[path = "../tests/support/mod.rs"]
mod support;

use marlin_asset_db::AssetDb;
use marlin_bench::{Bench, Duration};
use support::Project;

const DIRS: usize = 250;
const FILES_PER_DIR: usize = 200;
const OBJECTS: usize = 4;
const FILES: usize = DIRS * FILES_PER_DIR;

fn uuid(file: usize, object: usize) -> String {
    format!(
        "01990000-0000-7000-8000-{:012x}",
        file * OBJECTS + object + 1
    )
}

fn write_project() -> Project {
    let p = Project::new("bench");
    for d in 0..DIRS {
        for f in 0..FILES_PER_DIR {
            let file = d * FILES_PER_DIR + f;
            let mut text = String::from("!Version 1\n");
            for o in 0..OBJECTS {
                let target = (file * 7 + o * 13) % FILES;
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
    p
}

fn index() {
    let p = write_project();
    // Whole-index operations take seconds; a few samples are enough.
    let slow = |name: &str| {
        Bench::new(name)
            .throughput_items(FILES as u64, "file")
            .samples(5)
            .warmup(Duration::ZERO)
    };

    slow("build").run_with_setup(|| (), |()| AssetDb::build(&p.root, p.options()).unwrap());

    let mut db = AssetDb::build(&p.root, p.options()).unwrap();
    assert_eq!(db.object_count(), FILES * OBJECTS);
    let path = db.save().unwrap();
    let mib = std::fs::metadata(&path).unwrap().len() as f64 / (1 << 20) as f64;
    slow("save").metric("mib", mib).run(|| db.save().unwrap());
    drop(db);

    slow("open_noop_full_scan_update").run_with_setup(
        || (),
        |()| {
            let (db, report) = AssetDb::open(&p.root, p.options()).unwrap();
            assert_eq!(report.read, 0);
            db
        },
    );

    let (mut db, _) = AssetDb::open(&p.root, p.options()).unwrap();
    let hits = db.search("rock123_").len();
    Bench::new("search")
        .metric("hits", hits as f64)
        .run(|| db.search("rock123_"));

    // The first call builds the reverse table, so each sample needs a fresh index.
    Bench::new("references_to/cold")
        .samples(5)
        .warmup(Duration::ZERO)
        .run_with_setup(
            || AssetDb::open(&p.root, p.options()).unwrap().0,
            |db| {
                let refs = db.references_to(uuid(5, 0).parse().unwrap());
                (refs, db)
            },
        );
    let warm = uuid(6, 0).parse().unwrap();
    let refs = db.references_to(warm).len();
    Bench::new("references_to/warm")
        .metric("refs", refs as f64)
        .run(|| db.references_to(warm));

    let root = uuid(0, 0).parse().unwrap();
    let deps = db.dependencies(root).len();
    Bench::new("dependencies_closure")
        .metric("objects", deps as f64)
        .run(|| db.dependencies(root));

    let typed = db.objects_of_type("MeshInstance").len();
    Bench::new("objects_of_type")
        .metric("objects", typed as f64)
        .run(|| db.objects_of_type("MeshInstance"));

    let issues = db.validate().len();
    Bench::new("validate")
        .metric("issues", issues as f64)
        .run(|| db.validate());
}

marlin_bench::main!(index);
