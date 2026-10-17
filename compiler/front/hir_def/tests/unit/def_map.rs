use marlin_front_hir_def::PackageDefMap;
use marlin_test_support::{data_path, golden};

use crate::fixture::world;

#[test]
fn packages_merge_their_files_into_namespaces() {
    let world = world();
    let snapshot = world.db.snapshot();
    let mut dump = String::new();
    for (name, &package) in &world.packages {
        let map = snapshot.get::<PackageDefMap>(&package).unwrap();
        dump.push_str(&format!(
            "== package {name}\n{}",
            map.dump(&snapshot).unwrap()
        ));
    }
    golden::assert_text(&data_path(env!("CARGO_MANIFEST_DIR"), "def_map.txt"), &dump);
}

#[test]
fn declaration_errors_render_with_their_spans() {
    let world = world();
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "diagnostics.txt"),
        &world.diagnostics(),
    );
}
