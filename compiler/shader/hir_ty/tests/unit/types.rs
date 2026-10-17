use std::fmt::Write;

use marlin_front_hir_def::{ItemId, ItemTreeQuery, item_path};
use marlin_lang_query::Cx;
use marlin_shaderc_hir_ty::{BuiltinKind, BuiltinsQuery, ItemTypesQuery, Scalar, display_ty};
use marlin_test_support::{data_path, golden};

use crate::support::world;

/// Every declaration of `file` with the types of its signature.
fn dump(
    cx: &Cx<'_>,
    file: marlin_front_hir_def::FileId,
    package: marlin_front_hir_def::PackageId,
) -> String {
    let builtins = cx.get::<BuiltinsQuery>(&package).unwrap();
    let show = |t: &marlin_shaderc_hir_ty::Ty| display_ty(cx, &builtins, t).unwrap();
    let tree = cx.get::<ItemTreeQuery>(&file).unwrap();
    let mut out = String::new();
    for (local, _) in tree.items.iter() {
        let item = ItemId { file, local };
        let types = cx.get::<ItemTypesQuery>(&item).unwrap();
        let _ = writeln!(out, "== {}", item_path(cx, item).unwrap());
        for g in &types.generics {
            let bounds: Vec<_> = g.bounds.iter().map(show).collect();
            let kind = match &g.const_ty {
                Some(t) => format!("const {} ", show(t)),
                None => String::new(),
            };
            let _ = writeln!(out, "  generic {kind}{} : [{}]", g.name, bounds.join(", "));
        }
        for (target, bounds) in &types.assoc_bounds {
            let bounds: Vec<_> = bounds.iter().map(show).collect();
            let _ = writeln!(out, "  where {} : [{}]", show(target), bounds.join(", "));
        }
        if let Some(r) = &types.receiver {
            let _ = writeln!(out, "  receiver {}", show(r));
        }
        for p in &types.params {
            let modifier = p.ref_kind.map(|k| format!("{k:?} ")).unwrap_or_default();
            let name = p.name.as_ref().map(|n| n.to_string()).unwrap_or_default();
            let _ = writeln!(out, "  param {modifier}{} {name}", show(&p.ty));
        }
        if let Some(t) = &types.ty {
            let _ = writeln!(out, "  type {}", show(t));
        }
        if !types.bases.is_empty() {
            let bases: Vec<_> = types.bases.iter().map(show).collect();
            let _ = writeln!(out, "  bases [{}]", bases.join(", "));
        }
        if let Some(b) = types.builtin {
            let _ = writeln!(out, "  builtin {b:?}");
        }
        if let Some(i) = &types.intrinsic {
            let _ = writeln!(out, "  intrinsic {i}");
        }
    }
    out
}

#[test]
fn the_core_library_declares_the_builtins() {
    let world = world(&["game/Game.mshader"]);
    let snapshot = world.db.snapshot();
    let builtins = snapshot.get::<BuiltinsQuery>(&world.game).unwrap();
    assert_eq!(builtins.core_package, Some(world.core));
    for scalar in [
        Scalar::Bool,
        Scalar::I32,
        Scalar::U32,
        Scalar::F16,
        Scalar::F32,
        Scalar::F64,
    ] {
        assert!(
            builtins.item(BuiltinKind::Scalar(scalar)).is_some(),
            "{scalar:?}"
        );
    }
    assert!(builtins.item(BuiltinKind::Vector).is_some());
    assert_eq!(
        builtins.items[&BuiltinKind::Func].len(),
        2,
        "Func<R> and Func<T, R>"
    );
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "core_types.txt"),
        &dump(&snapshot, world.file("core/Core.mshader"), world.core),
    );
}

#[test]
fn signatures_lower_to_types() {
    let world = world(&["game/Game.mshader"]);
    let snapshot = world.db.snapshot();
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "game_types.txt"),
        &dump(&snapshot, world.file("game/Game.mshader"), world.game),
    );
    assert_eq!(
        world.diagnostics(&["core/Core.mshader", "game/Game.mshader"]),
        "",
        "valid code has no diagnostics"
    );
}

#[test]
fn type_errors_are_reported_where_they_are() {
    let world = world(&["game/Game.mshader", "game/Errors.mshader"]);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "errors.txt"),
        &world.diagnostics(&["game/Errors.mshader"]),
    );
}
