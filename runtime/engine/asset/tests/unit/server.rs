//! The asset server on in-memory units: links, lifetimes, layers and failures.

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, platform_id, unit_id};
use marlin_asset::{
    AssetError, AssetRef, AssetServer, Link, LoadState, Loaders, MemorySource, Read, SourceError,
    UnitBytes, UnitSource, Uuid, Vfs,
};
use marlin_reflect::{Reflect, type_info};

use crate::support::uuid;

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Part {
    value: u32,
    next: Link<Part>,
}

/// What the linking loader makes of a [`Part`]: the link resolved.
struct Linked {
    value: u32,
    next: Option<AssetRef<Linked>>,
}

fn part(id: u64, value: u32, next: Option<u64>) -> UnitObject {
    let part = Part {
        value,
        next: next.map_or(Link::NONE, |n| Link::new(uuid(n))),
    };
    UnitObject {
        uuid: uuid(id),
        type_id: type_info::<Part>().id.0,
        encoding: Encoding::Serial,
        payload: marlin_serial::to_bytes(&part).unwrap(),
        bulk: Vec::new(),
    }
}

/// A unit at `path` of `parts`, importing what they link to outside it (`(uuid, unit path)`).
fn unit_of(path: &str, parts: Vec<UnitObject>, imports: &[(u64, &str)]) -> Vec<u8> {
    let imports: Vec<(Uuid, u64)> = imports
        .iter()
        .map(|&(u, p)| (uuid(u), unit_id(p)))
        .collect();
    unit(unit_id(path), platform_id("windows"), parts, &imports)
}

fn source(units: Vec<Vec<u8>>) -> MemorySource {
    let mut s = MemorySource::new();
    for u in units {
        s.insert(u).unwrap();
    }
    s
}

fn serial() -> Loaders {
    let mut l = Loaders::new();
    l.register_serial::<Part>();
    l
}

fn linking() -> Loaders {
    let mut l = Loaders::new();
    l.register::<Linked>(type_info::<Part>().id.0, |ctx| {
        let part: Part = marlin_serial::from_bytes(ctx.payload).map_err(|e| format!("{e:?}"))?;
        let next = (!part.next.is_none()).then(|| ctx.link::<Linked>(part.next.target));
        Ok(Linked {
            value: part.value,
            next,
        })
    });
    l
}

/// a: A (1) → B; b: B (2).
fn two_units() -> Vfs {
    let mut vfs = Vfs::new();
    vfs.mount(
        "base",
        source(vec![
            unit_of("a", vec![part(1, 1, Some(2))], &[(2, "b")]),
            unit_of("b", vec![part(2, 2, None)], &[]),
        ]),
    );
    vfs
}

#[test]
fn loading_follows_links_into_other_units() {
    let vfs = two_units();
    let mut assets = AssetServer::new(linking());
    let a = assets.load::<Linked>(uuid(1));
    assert_eq!(assets.state(&a), LoadState::Loading);
    let stats = assets.update_blocking(&vfs, None);
    assert_eq!(stats.objects_loaded, 2, "A and the B it links to");
    assert_eq!(assets.state(&a), LoadState::Loaded);
    assert!(assets.is_resident(unit_id("a")) && assets.is_resident(unit_id("b")));
    let a_value = assets.get(&a).unwrap();
    assert_eq!(a_value.value, 1);
    let b = a_value.next.unwrap();
    assert_eq!(b.uuid(), uuid(2));
    assert_eq!(assets.get_ref(b).unwrap().value, 2);
    // The same object.
    let strong = assets.upgrade(b);
    assert_eq!(strong.to_ref(), b);
    assert_eq!(assets.load::<Linked>(uuid(2)), strong);
}

#[test]
fn units_unload_when_nothing_holds_them() {
    let vfs = two_units();
    let mut assets = AssetServer::new(serial());
    let a = assets.load::<Part>(uuid(1));
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&a).unwrap().next, Link::new(uuid(2)));
    assert_eq!(assets.unit_count(), 2);

    // B alone: A's unit goes, B's stays.
    let b = assets.load::<Part>(uuid(2));
    let weak = a.downgrade();
    drop(a);
    assert!(weak.upgrade().is_none());
    let stats = assets.update_blocking(&vfs, None);
    assert_eq!(stats.units_unloaded, 1);
    assert!(!assets.is_resident(unit_id("a")) && assets.is_resident(unit_id("b")));
    assert_eq!(assets.get(&b).unwrap().value, 2);

    drop(b);
    assets.update_blocking(&vfs, None);
    assert_eq!((assets.unit_count(), assets.object_count()), (0, 0));

    // And loads again.
    let a = assets.load::<Part>(uuid(1));
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.state(&a), LoadState::Loaded);
}

#[test]
fn a_unit_imported_by_a_held_unit_stays() {
    let vfs = two_units();
    let mut assets = AssetServer::new(serial());
    let a = assets.load::<Part>(uuid(1));
    assets.update_blocking(&vfs, None);
    // Nothing holds B directly, but A links to it.
    for _ in 0..3 {
        assets.update_blocking(&vfs, None);
    }
    assert!(assets.is_resident(unit_id("b")));
    drop(a);
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.unit_count(), 0);
}

#[test]
fn cycles_load_together_and_unload_together() {
    let mut vfs = Vfs::new();
    vfs.mount(
        "base",
        source(vec![
            unit_of("x", vec![part(1, 10, Some(2))], &[(2, "y")]),
            unit_of("y", vec![part(2, 20, Some(1))], &[(1, "x")]),
        ]),
    );
    let mut assets = AssetServer::new(linking());
    let x = assets.load::<Linked>(uuid(1));
    assets.update_blocking(&vfs, None);
    let y = assets.get(&x).unwrap().next.unwrap();
    let back = assets.get_ref(y).unwrap().next.unwrap();
    assert_eq!(back, x.to_ref());
    assert_eq!(assets.get_ref(back).unwrap().value, 10);
    // Links do not own: dropping the one handle unloads the whole cycle.
    drop(x);
    assets.update_blocking(&vfs, None);
    assert_eq!((assets.unit_count(), assets.object_count()), (0, 0));
    assert!(
        assets.get_ref(y).is_none(),
        "a stale reference reads nothing"
    );
}

#[test]
fn later_layers_override_earlier_ones() {
    let mut vfs = Vfs::new();
    vfs.mount(
        "base",
        source(vec![unit_of("a", vec![part(1, 1, None)], &[])]),
    );
    vfs.mount(
        "patch",
        source(vec![unit_of("a", vec![part(1, 100, None)], &[])]),
    );
    assert_eq!(vfs.layer_names().collect::<Vec<_>>(), ["patch", "base"]);
    let mut assets = AssetServer::new(serial());
    let a = assets.load::<Part>(uuid(1));
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&a).unwrap().value, 100);
    drop(a);
    assets.update_blocking(&vfs, None);

    assert!(vfs.unmount("patch"));
    let a = assets.load::<Part>(uuid(1));
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&a).unwrap().value, 1);
}

#[test]
fn failures_are_states_not_panics() {
    let vfs = two_units();
    let mut assets = AssetServer::new(Loaders::new());
    let missing = assets.load::<Part>(uuid(99));
    let unloadable = assets.load::<Part>(uuid(1));
    assets.update_blocking(&vfs, None);
    assert_eq!(
        assets.state(&missing),
        LoadState::Failed(AssetError::NotFound)
    );
    assert_eq!(
        assets.state(&unloadable),
        LoadState::Failed(AssetError::NoLoader {
            type_id: type_info::<Part>().id.0
        })
    );
    assert!(assets.get(&unloadable).is_none());

    let mut assets = AssetServer::new(serial());
    let wrong = assets.load::<u32>(uuid(1));
    assets.update_blocking(&vfs, None);
    assert_eq!(
        assets.state(&wrong),
        LoadState::Failed(AssetError::WrongType)
    );
    assert!(assets.get(&wrong).is_none());
}

/// Claims a unit and serves garbage for it.
struct Garbage;

impl UnitSource for Garbage {
    fn contains(&self, unit: u64) -> bool {
        unit == unit_id("bad")
    }

    fn object_unit(&self, id: Uuid) -> Option<u64> {
        (id == uuid(7)).then(|| unit_id("bad"))
    }

    fn begin(&self, _unit: u64) -> Result<Read<'_>, SourceError> {
        Ok(Read::Ready(UnitBytes::copy_from(
            b"MCU1 but nothing else",
            16,
        )))
    }

    fn finish(&self, _unit: u64, _read: &[u8]) -> Result<UnitBytes, SourceError> {
        Err(SourceError::NotFound)
    }
}

#[test]
fn a_corrupt_unit_fails_its_objects() {
    let mut vfs = Vfs::new();
    vfs.mount("garbage", Garbage);
    let mut assets = AssetServer::new(serial());
    let h = assets.load::<Part>(uuid(7));
    assets.update_blocking(&vfs, None);
    assert!(
        matches!(assets.state(&h), LoadState::Failed(AssetError::Corrupt(_))),
        "{:?}",
        assets.state(&h)
    );
    drop(h);
    assets.update_blocking(&vfs, None);
    assert_eq!((assets.unit_count(), assets.object_count()), (0, 0));
}
