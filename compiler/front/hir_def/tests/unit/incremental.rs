use marlin_front_hir_def::{FileImports, FileText, Name, NamespacePath, PackageDefMap};

use crate::fixture::{Fixture, world};

const BLUR: &str = "post/Blur.mshader";

fn def_maps_and_imports(world: &Fixture) {
    let snapshot = world.db.snapshot();
    for &package in world.packages.values() {
        snapshot.get::<PackageDefMap>(&package).unwrap();
    }
    for file in world.files.values() {
        snapshot.get::<FileImports>(file).unwrap();
    }
}

fn text(world: &Fixture, path: &str) -> String {
    world
        .db
        .snapshot()
        .input::<FileText>(&world.file(path))
        .unwrap()
        .to_string()
}

#[test]
fn edits_inside_bodies_do_not_recompute_declarations() {
    let world = world();
    def_maps_and_imports(&world);
    let original = text(&world, BLUR);
    // A different body and extra blank lines: declarations and their order are unchanged.
    let edited = original
        .replace("=> Dot(c, c);", "=> Dot(c, c) * 2;\n\n")
        .replace("public T Run", "\n    public T Run");
    assert_ne!(original, edited);
    world.set_text(BLUR, &edited);

    let before = world.db.stats();
    def_maps_and_imports(&world);
    assert_eq!(
        world.db.stats().executions - before.executions,
        3,
        "only ParseFile, AstIdMap and ItemTreeQuery of the edited file run again"
    );
}

#[test]
fn adding_a_declaration_updates_the_def_map() {
    let world = world();
    def_maps_and_imports(&world);
    let original = text(&world, BLUR);
    world.set_text(BLUR, &format!("{original}public struct Added {{ }}\n"));
    let snapshot = world.db.snapshot();
    let map = snapshot
        .get::<PackageDefMap>(&world.package("Post"))
        .unwrap();
    let post = map
        .namespace(&NamespacePath::new(["Marlin", "Post"].map(Name::new)))
        .unwrap();
    assert!(
        post.types.keys().any(|(name, _)| name.as_str() == "Added"),
        "{:?}",
        post.types.keys().collect::<Vec<_>>()
    );
}
