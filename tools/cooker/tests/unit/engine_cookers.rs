//! The engine's cooker set knows every engine asset type.

use marlin_cooker::engine_cookers;

#[test]
fn every_engine_type_has_a_cooker() {
    let cookers = engine_cookers();
    for name in [
        "Texture",
        "Mesh",
        "Skeleton",
        "AnimationClip",
        "StringTable",
        "Level",
        "ShaderPackage",
    ] {
        assert!(
            cookers.schema().object_type(name).is_some(),
            "{name} is not registered"
        );
    }
    assert!(
        cookers.is_named("ShaderPackage"),
        "shader packages are found by name"
    );
    assert!(!cookers.is_named("Texture"));
}
