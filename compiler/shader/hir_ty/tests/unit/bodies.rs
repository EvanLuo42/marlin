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

#[test]
fn csharp_flow_patterns_pass_the_checks() {
    let world = world(&["game/ChecksOk.mshader"]);
    assert_eq!(
        world.diagnostics(&["core/Core.mshader", "game/ChecksOk.mshader"]),
        "",
        "definitely assigned, returning and read-only-safe code has no diagnostics"
    );
}

#[test]
fn flow_readonly_and_exhaustiveness_errors_are_reported() {
    let world = world(&["game/Checks.mshader"]);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "check_errors.txt"),
        &world.diagnostics(&["game/Checks.mshader"]),
    );
}

#[test]
fn valid_declarations_and_entry_points_pass_the_checks() {
    let world = world(&["game/DeclsOk.mshader"]);
    assert_eq!(
        world.diagnostics(&["core/Core.mshader", "game/DeclsOk.mshader"]),
        "",
        "implemented interfaces, entry points, push constants and contracts are fine"
    );
}

#[test]
fn declaration_errors_are_reported() {
    let world = world(&["game/Decls.mshader"]);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "decl_errors.txt"),
        &world.diagnostics(&["game/Decls.mshader"]),
    );
}

/// The item of `file` whose path ends in `name` (`Game.Main`).
fn find(world: &crate::support::World, file: &str, name: &str) -> ItemId {
    let snapshot = world.db.snapshot();
    let file = world.file(file);
    let tree = snapshot.get::<ItemTreeQuery>(&file).unwrap();
    tree.items
        .iter()
        .map(|(local, _)| ItemId { file, local })
        .find(|&item| item_path(&snapshot, item).unwrap() == name)
        .unwrap_or_else(|| panic!("no item {name}"))
}

#[test]
fn capabilities_are_inferred_and_const_if_branches_kept_apart() {
    use marlin_shaderc_hir_ty::CapabilitiesQuery;
    let world = world(&["game/DeclsOk.mshader"]);
    let snapshot = world.db.snapshot();
    let caps = |name: &str| {
        let caps = snapshot
            .get::<CapabilitiesQuery>(&find(&world, "game/DeclsOk.mshader", name))
            .unwrap();
        let keys = |m: &std::collections::BTreeMap<Box<str>, String>| {
            m.iter()
                .map(|(k, why)| format!("{k} ({why})"))
                .collect::<Vec<_>>()
        };
        (keys(&caps.required), keys(&caps.conditional))
    };
    assert_eq!(caps("Game.Main"), (vec![], vec![]));
    assert_eq!(
        caps("Game.Precise"),
        (vec!["Float64 (uses `double`)".to_string()], vec![])
    );
    assert_eq!(
        caps("Game.Guarded"),
        (vec![], vec!["WaveMatrix (calls `MatMul16`)".to_string()])
    );
}

#[test]
fn structs_have_scalar_layout() {
    use marlin_shaderc_hir_ty::{Layout, Ty, layout_of};
    let world = world(&["game/DeclsOk.mshader", "game/Game.mshader"]);
    let snapshot = world.db.snapshot();
    let builtins = snapshot.get::<BuiltinsQuery>(&world.game).unwrap();
    let layout = |file: &str, name: &str| {
        let ty = Ty::adt(find(&world, file, name));
        layout_of(&snapshot, &builtins, &ty).unwrap()
    };
    // Resource 4 + float 4 + uint 4.
    let args = layout("game/DeclsOk.mshader", "Game.Args");
    assert_eq!(args, Some(Layout { size: 12, align: 4 }));
    // float4 16 + float3 12 + float 4.
    let lighting = layout("game/DeclsOk.mshader", "Game.Lighting");
    assert_eq!(lighting, Some(Layout { size: 32, align: 4 }));
    // float3 12, float 4, Texture2D 4, float[4] 16, float4 16 (Vector<float, 2 * 2>), float4x4 64.
    let light = layout("game/Game.mshader", "Game.Light");
    assert_eq!(
        light,
        Some(Layout {
            size: 116,
            align: 4
        })
    );
    // `Grid<N>` contains `Grid<N - 1>`, which has no layout until `N` is known.
    assert_eq!(layout("game/Game.mshader", "Game.Grid"), None);
}

#[test]
fn fields_of_array_elements_in_buffer_elements_are_assignable() {
    let world = world(&["game/BufferPlaces.mshader"]);
    let shown = world.diagnostics(&["game/BufferPlaces.mshader"]);
    // Only the element of an array returned by value is not a variable.
    assert_eq!(shown.matches("error[").count(), 1, "{shown}");
    assert!(
        shown.contains("error[E1107]") && shown.contains("TwoLinks()[1].A"),
        "{shown}"
    );
}
