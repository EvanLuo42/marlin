use marlin_front_hir_def::{Resolution, Resolver};

use crate::fixture::{find_item, path, show, world};

/// `(from, path, values, expected)`: resolve `path` from inside declaration `from`.
#[rustfmt::skip]
const CASES: &[(&str, &str, bool, &str)] = &[
    // Outer namespaces are searched after the current one and its usings.
    ("Marlin.Post.Blur", "Light", false, "type Marlin.Light"),
    ("Marlin.Post.Blur", "Math.Pi", true, "values [Marlin.Math.Pi]"),
    ("Marlin.Post.Blur", "Dot", true, "values [Marlin.Dot]"),
    // A using alias, and a using that imports functions (an overload set).
    ("Marlin.Post.Blur", "M.Dot", true, "values [Marlin.Math.Dot]"),
    ("Marlin.Post.Blur", "Sum", true, "values [Marlin.Wave.Sum, Marlin.Wave.Sum]"),
    // Type parameters of the enclosing declarations, innermost first.
    ("Marlin.Post.Blur.Run", "U", false, "type parameter 0 of Marlin.Post.Blur.Run"),
    ("Marlin.Post.Blur.Run", "T", false, "type parameter 0 of Marlin.Post.Blur"),
    ("Marlin.Post.Blur", "T.Zero", true, "partial type parameter 0 of Marlin.Post.Blur after 1 segment(s)"),
    // Types are told apart by their number of type arguments.
    ("Marlin.Post.Blur", "Vector<2>", false, "type Marlin.Vector"),
    ("Marlin.Post.Blur", "Vector<1>", false, "unresolved at segment 0"),
    // Members of types.
    ("Marlin.Post.Blur", "Light.Nested", false, "type Marlin.Light.Nested"),
    ("Marlin.Post.Blur", "Light.Color", true, "values [Marlin.Light.Color]"),
    ("Marlin.Post.Blur", "Light.Color", false, "partial type Marlin.Light after 1 segment(s)"),
    ("Marlin.Post.Rock", "Params", false, "type Marlin.Post.Rock.Params"),
    // Accessibility.
    ("Marlin.Post.Blur", "Light.Secret", false, "inaccessible at segment 1 [Marlin.Light.Secret]"),
    ("Marlin.Light", "Secret", false, "type Marlin.Light.Secret"),
    ("Marlin.Post.Blur", "CoreOnly", false, "inaccessible at segment 0 [Marlin.CoreOnly]"),
    // A closer namespace shadows an outer one.
    ("Marlin.Post.Inner.Light", "Light", false, "type Marlin.Post.Inner.Light"),
    ("Marlin.Post.Blur", "global::Marlin.Light", false, "type Marlin.Light"),
    // Two usings at the same level importing the same name.
    ("Game.UsesLight", "Light", false, "ambiguous at segment 0 [type Marlin.Math.Light; type Marlin.Extra.Light]"),
    // The language's implicit `using Marlin;` applies at the root level, after the file's own.
    ("Game.UsesLight", "Dot", true, "values [Marlin.Math.Dot]"),
    ("Marlin.Post.Blur", "Nope", false, "unresolved at segment 0"),
    ("Marlin.Post.Blur", "Marlin.Nope", false, "unresolved at segment 1"),
];

#[test]
fn names_resolve_with_csharp_lookup_rules() {
    let world = world();
    let snapshot = world.db.snapshot();
    let mut failures = Vec::new();
    for &(from, text, values, expected) in CASES {
        let item = find_item(&snapshot, &world, from);
        let resolver = Resolver::for_item(&snapshot, item).unwrap();
        let resolution = resolver.resolve(&snapshot, &path(text), values).unwrap();
        let actual = show(&snapshot, &resolution);
        if actual != expected {
            failures.push(format!(
                "from {from}, `{text}`: got `{actual}`, expected `{expected}`"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn types_with_different_arity_are_different_declarations() {
    let world = world();
    let snapshot = world.db.snapshot();
    let item = find_item(&snapshot, &world, "Marlin.Post.Blur");
    let resolver = Resolver::for_item(&snapshot, item).unwrap();
    let one = resolver
        .resolve(&snapshot, &path("Func<1>"), false)
        .unwrap();
    let two = resolver
        .resolve(&snapshot, &path("Func<2>"), false)
        .unwrap();
    assert!(
        matches!((&one, &two), (Resolution::Type(a), Resolution::Type(b)) if a != b),
        "{one:?} {two:?}"
    );
}
