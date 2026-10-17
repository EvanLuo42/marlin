use std::fmt::Write;

use marlin_front_hir_def::{BodyWithSourceMap, ItemId, ItemTreeQuery, body_owners, item_path};
use marlin_shaderc_hir_ty::{BuiltinsQuery, InferQuery, display_ty};
use marlin_test_support::{data_path, golden};

use crate::support::world;

/// The type of every local of every body in `file`.
fn locals(world: &crate::support::World, file: &str) -> String {
    let snapshot = world.db.snapshot();
    let file = world.file(file);
    let builtins = snapshot.get::<BuiltinsQuery>(&world.game).unwrap();
    let tree = snapshot.get::<ItemTreeQuery>(&file).unwrap();
    let mut out = String::new();
    for (local, _) in tree.items.iter() {
        let item = ItemId { file, local };
        for owner in body_owners(&snapshot, item).unwrap() {
            let (body, _) = snapshot.get::<BodyWithSourceMap>(&owner).unwrap();
            let infer = snapshot.get::<InferQuery>(&owner).unwrap();
            let _ = writeln!(
                out,
                "== {} {:?}",
                item_path(&snapshot, item).unwrap(),
                owner.part
            );
            for (binding, data) in body.store.bindings.iter() {
                let ty = infer
                    .binding_types
                    .get(binding)
                    .map(|t| display_ty(&snapshot, &builtins, t).unwrap())
                    .unwrap_or_else(|| "<untyped>".into());
                let _ = writeln!(out, "  {} : {ty}", data.name);
            }
        }
    }
    out
}

#[test]
fn bodies_are_typed_by_csharp_rules() {
    let world = world(&["game/Game.mshader", "game/Bodies.mshader"]);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "body_types.txt"),
        &locals(&world, "game/Bodies.mshader"),
    );
    assert_eq!(
        world.diagnostics(&["core/Core.mshader", "game/Bodies.mshader"]),
        "",
        "well-typed bodies have no diagnostics"
    );
}

#[test]
fn type_errors_in_bodies_are_reported() {
    let world = world(&["game/Game.mshader", "game/BodyErrors.mshader"]);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "body_errors.txt"),
        &world.diagnostics(&["game/BodyErrors.mshader"]),
    );
}
