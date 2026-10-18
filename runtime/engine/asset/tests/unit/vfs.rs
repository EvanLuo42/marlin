use marlin_asset::{MemorySource, Vfs, name_hash};

use crate::support::uuid;

#[test]
fn names_resolve_through_the_topmost_layer_that_has_them() {
    let mut engine = MemorySource::new();
    engine.name("Marlin.Engine", uuid(1));
    engine.name("Marlin.Post", uuid(2));
    let mut patch = MemorySource::new();
    patch.name("Marlin.Engine", uuid(3));

    let mut vfs = Vfs::new();
    assert_eq!(vfs.named("Marlin.Engine"), None);
    vfs.mount("engine", engine);
    assert_eq!(vfs.named("Marlin.Engine"), Some(uuid(1)));
    vfs.mount("patch", patch);
    assert_eq!(vfs.named("Marlin.Engine"), Some(uuid(3)));
    assert_eq!(vfs.named("Marlin.Post"), Some(uuid(2)));
    assert_eq!(vfs.named("Marlin"), None);
    assert!(vfs.unmount("patch"));
    assert_eq!(vfs.named("Marlin.Engine"), Some(uuid(1)));
}

#[test]
fn the_name_hash_is_xxh3_of_the_name() {
    assert_eq!(
        name_hash("Marlin.Engine"),
        marlin_hash::xxh3_64(b"Marlin.Engine")
    );
    assert_ne!(name_hash("Marlin.Engine"), name_hash("Marlin.engine"));
}
