//! Hot reload on a source whose units change under the asset server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, UnitView, platform_id, unit_id};
use marlin_asset::{
    AssetError, AssetServer, Link, LoadState, Loaders, Read, SourceError, UNIT_ALIGN, UnitBytes,
    UnitSource, Uuid, Vfs,
};
use marlin_reflect::{Reflect, type_info};

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Part {
    value: u32,
    next: Link<Part>,
}

fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0000 | n)
}

fn part(id: u128, value: u32) -> UnitObject {
    UnitObject {
        uuid: uuid(id),
        type_id: type_info::<Part>().id.0,
        encoding: Encoding::Serial,
        payload: marlin_serial::to_bytes(&Part {
            value,
            next: Link::NONE,
        })
        .unwrap(),
        bulk: Vec::new(),
    }
}

/// Units that tests replace, invalidating them like the cook service would.
#[derive(Clone, Default)]
struct Live {
    inner: Arc<Mutex<LiveInner>>,
}

#[derive(Default)]
struct LiveInner {
    units: HashMap<u64, Vec<u8>>,
    objects: HashMap<Uuid, u64>,
    invalidated: Vec<u64>,
    /// Units that fail to read (a broken cook).
    broken: Vec<u64>,
}

impl Live {
    fn set(&self, path: &str, parts: Vec<UnitObject>) {
        let mut s = self.inner.lock().unwrap();
        let id = unit_id(path);
        s.objects.retain(|_, u| *u != id);
        for p in &parts {
            s.objects.insert(p.uuid, id);
        }
        s.units
            .insert(id, unit(id, platform_id("windows"), parts, &[]));
        s.broken.retain(|&u| u != id);
        s.invalidated.push(id);
    }

    fn break_unit(&self, path: &str) {
        let mut s = self.inner.lock().unwrap();
        s.broken.push(unit_id(path));
        s.invalidated.push(unit_id(path));
    }
}

impl UnitSource for Live {
    fn contains(&self, unit: u64) -> bool {
        self.inner.lock().unwrap().units.contains_key(&unit)
    }

    fn object_unit(&self, uuid: Uuid) -> Option<u64> {
        self.inner.lock().unwrap().objects.get(&uuid).copied()
    }

    fn begin(&self, unit: u64) -> Result<Read<'_>, SourceError> {
        let s = self.inner.lock().unwrap();
        if s.broken.contains(&unit) {
            return Err(SourceError::Remote("does not cook".into()));
        }
        let bytes = s.units.get(&unit).ok_or(SourceError::NotFound)?;
        assert!(UnitView::new(bytes).is_ok());
        Ok(Read::Ready(UnitBytes::copy_from(bytes, UNIT_ALIGN)))
    }

    fn finish(&self, _unit: u64, _read: &[u8]) -> Result<UnitBytes, SourceError> {
        Err(SourceError::NotFound)
    }

    fn take_invalidated(&self) -> Vec<u64> {
        std::mem::take(&mut self.inner.lock().unwrap().invalidated)
    }
}

fn setup() -> (Live, Vfs, AssetServer) {
    let live = Live::default();
    live.set("a", vec![part(1, 1), part(2, 2)]);
    live.set("b", vec![part(3, 3)]);
    live.take_invalidated();
    let mut vfs = Vfs::new();
    vfs.mount("live", live.clone());
    let mut loaders = Loaders::new();
    loaders.register_serial::<Part>();
    (live, vfs, AssetServer::new(loaders))
}

#[test]
fn objects_reload_in_place() {
    let (live, vfs, mut assets) = setup();
    let one = assets.load::<Part>(uuid(1));
    let two = assets.load::<Part>(uuid(2));
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&one).unwrap().value, 1);

    live.set("a", vec![part(1, 10), part(2, 20)]);
    assets.update_blocking(&vfs, None);
    assert_eq!(
        (
            assets.get(&one).unwrap().value,
            assets.get(&two).unwrap().value
        ),
        (10, 20)
    );
    assert_eq!((assets.version(&one), assets.version(&two)), (1, 1));
    let mut reloaded = assets.take_reloaded();
    reloaded.sort();
    assert_eq!(reloaded, [uuid(1), uuid(2)]);

    // Units nobody loaded are not loaded by their invalidation.
    live.set("b", vec![part(3, 30)]);
    assets.update_blocking(&vfs, None);
    assert!(!assets.is_resident(unit_id("b")));
}

#[test]
fn deleted_and_moved_objects() {
    let (live, vfs, mut assets) = setup();
    let one = assets.load::<Part>(uuid(1));
    let two = assets.load::<Part>(uuid(2));
    assets.update_blocking(&vfs, None);

    // 2 moves to b, 1 is deleted, 4 is new.
    live.set("b", vec![part(3, 3), part(2, 22)]);
    live.set("a", vec![part(4, 4)]);
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.state(&one), LoadState::Failed(AssetError::NotFound));
    assert_eq!(
        assets.get(&two).unwrap().value,
        22,
        "followed the object to its new unit"
    );
    let four = assets.load::<Part>(uuid(4));
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&four).unwrap().value, 4);
}

#[test]
fn a_failed_reload_keeps_the_last_good_version() {
    let (live, vfs, mut assets) = setup();
    let one = assets.load::<Part>(uuid(1));
    assets.update_blocking(&vfs, None);
    live.break_unit("a");
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&one).unwrap().value, 1);
    assert_eq!(assets.version(&one), 0);
    let errors = assets.take_reload_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, unit_id("a"));
    assert!(assets.is_resident(unit_id("a")));

    live.set("a", vec![part(1, 100), part(2, 2)]);
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&one).unwrap().value, 100);
}

#[test]
fn an_invalidation_during_a_load_loads_again() {
    let (live, vfs, mut assets) = setup();
    let one = assets.load::<Part>(uuid(1));
    assets.update_blocking(&vfs, None);
    // Two changes before the server looks: one reload with the latest bytes.
    live.set("a", vec![part(1, 5), part(2, 2)]);
    live.set("a", vec![part(1, 6), part(2, 2)]);
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&one).unwrap().value, 6);
    // A manual reload with nothing changed gives the same value, a new version.
    assets.reload(unit_id("a"));
    assets.update_blocking(&vfs, None);
    assert_eq!(
        (assets.get(&one).unwrap().value, assets.version(&one)),
        (6, 2)
    );
    // Dropping the handle unloads, reloads or not.
    drop(one);
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.unit_count(), 0);
}
