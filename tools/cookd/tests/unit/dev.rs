//! A game's asset server loading from a running cookd, and reloading what changes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use marlin_asset::cooked::{BulkKind, Encoding};
use marlin_asset::{
    AssetError, AssetRef, AssetServer, DevSource, Handle, Link, LoadState, Loaders, SourceFile,
    Uuid, Vfs,
};
use marlin_asset_db::{AssetDb, Options};
use marlin_cook_core::{CookContext, Cooked, Cooker, Cookers, LocalDdc, SourceObject};
use marlin_cookd::{Cookd, CookdOptions, bind};
use marlin_reflect::{Reflect, type_info};
use marlin_test_support::{BackgroundService, TempDir, panic_message, test_uuid, wait_until};

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Thing {
    value: u32,
    other: Link<Thing>,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Image {
    source: SourceFile,
}

/// Cooks an image into its bytes, as one blob.
struct ImageCooker;

impl Cooker for ImageCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let image: &Image = object.value.as_ref().downcast().ok_or("not an Image")?;
        let data = ctx.read_source(&image.source.path)?;
        ctx.add_bulk(BulkKind::Texture, data);
        Ok(Cooked {
            type_id: type_info::<Image>().id,
            encoding: Encoding::Cooked,
            payload: Vec::new(),
        })
    }
}

struct LoadedThing {
    value: u32,
    other: Option<AssetRef<LoadedThing>>,
}

struct Pixels(Vec<u8>);

fn loaders() -> Loaders {
    let mut l = Loaders::new();
    l.register::<LoadedThing>(type_info::<Thing>().id.0, |ctx| {
        let t: Thing = marlin_serial::from_bytes(ctx.payload).map_err(|e| format!("{e:?}"))?;
        let other = (!t.other.is_none()).then(|| ctx.link(t.other.target));
        Ok(LoadedThing {
            value: t.value,
            other,
        })
    });
    l.register::<Pixels>(type_info::<Image>().id.0, |ctx| match ctx.bulk {
        [(_, data)] => Ok(Pixels(data.to_vec())),
        _ => Err("expected one blob".into()),
    });
    l.register::<LoadedSlow>(type_info::<Slow>().id.0, |ctx| {
        let s: Slow = marlin_serial::from_bytes(ctx.payload).map_err(|e| format!("{e:?}"))?;
        Ok(LoadedSlow(s.value))
    });
    l
}

struct LoadedSlow(u32);

const A: Uuid = Uuid::from_u128(test_uuid(0xa));
const B: Uuid = Uuid::from_u128(test_uuid(0xb));
const PIC: Uuid = Uuid::from_u128(test_uuid(0xaa));
const BROKEN: Uuid = Uuid::from_u128(test_uuid(0xbb));

fn thing_a(value: u32) -> String {
    format!(
        "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"A\" Value = {value} Other = <b:B #{}> }}\n",
        A, B
    )
}

/// Cooks slowly: until [`GATE_OPEN`] is set (or a while passes, so a failing test cannot hang).
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Slow {
    value: u32,
}

static GATE_ENTERED: AtomicBool = AtomicBool::new(false);
static GATE_OPEN: AtomicBool = AtomicBool::new(false);

struct SlowCooker;

impl Cooker for SlowCooker {
    fn version(&self) -> u32 {
        0
    }

    fn cook(&self, object: &SourceObject<'_>, _: &mut CookContext<'_>) -> Result<Cooked, String> {
        GATE_ENTERED.store(true, Ordering::Relaxed);
        let start = Instant::now();
        while !GATE_OPEN.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut payload = Vec::new();
        marlin_serial::write(object.value.as_ref(), &mut payload).map_err(|e| format!("{e:?}"))?;
        Ok(Cooked {
            type_id: type_info::<Slow>().id,
            encoding: Encoding::Serial,
            payload,
        })
    }
}

/// Panics: a bug in a cooker.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Bomb {
    value: u32,
}

struct BombCooker;

impl Cooker for BombCooker {
    fn version(&self) -> u32 {
        0
    }

    fn cook(&self, _: &SourceObject<'_>, _: &mut CookContext<'_>) -> Result<Cooked, String> {
        panic!("cooker bug")
    }
}

const SLOW: Uuid = Uuid::from_u128(test_uuid(0xcc));
const BOMB: Uuid = Uuid::from_u128(test_uuid(0xdd));

/// A cookd on a free port over a fresh project, stopped on drop.
struct Running {
    // First: stopped and joined before the project directory is removed.
    service: BackgroundService,
    root: TempDir,
}

impl Running {
    fn start(name: &str) -> Self {
        Self::start_with(name, &[])
    }

    /// With `extra` (path, text) files besides the standard ones.
    fn start_with(name: &str, extra: &[(&str, String)]) -> Self {
        let root = TempDir::new(&format!("cookd-{name}"));
        for (path, text) in extra {
            root.write(path, text.as_bytes());
        }
        root.write("a.masset", thing_a(1).as_bytes());
        root.write(
            "b.masset",
            format!(
                "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"B\" Value = 2 }}\n",
                B
            )
            .as_bytes(),
        );
        root.write(
            "img.masset",
            format!(
                "!Version 1\nImage {{ !Uuid = \"{}\" !Name = \"Pic\" Source = `source:pic.bin` }}\n",
                PIC
            )
            .as_bytes(),
        );
        // Its own unit: a unit fails as a whole.
        root.write(
            "broken.masset",
            format!(
                "!Version 1\nImage {{ !Uuid = \"{}\" !Name = \"Broken\" Source = `source:gone.bin` }}\n",
                BROKEN
            )
            .as_bytes(),
        );
        root.write("pic.bin", b"first pixels");

        let r = root.to_path_buf();
        let service = BackgroundService::spawn(move |ready, stop| {
            let (db, _) = AssetDb::open(&r, Options::default()).unwrap();
            let mut cookers = Cookers::new();
            cookers.register_generic::<Thing>().unwrap();
            cookers.register::<Image>(ImageCooker).unwrap();
            cookers.register::<Slow>(SlowCooker).unwrap();
            cookers.register::<Bomb>(BombCooker).unwrap();
            let ddc = Box::new(LocalDdc::open(r.join(".marlin/ddc")).unwrap());
            let options = CookdOptions {
                workers: 2,
                watch_interval: Duration::from_millis(30),
            };
            let mut cookd = Cookd::new(db, cookers, ddc, &options);
            let mut server = bind("127.0.0.1:0".parse().unwrap()).unwrap();
            ready.listening(server.local_addr());
            cookd.serve(&mut server, stop).unwrap();
        });
        Self { service, root }
    }

    fn connect(&self) -> Vfs {
        let source =
            DevSource::connect(self.service.addr(), "windows", Duration::from_secs(10)).unwrap();
        let mut vfs = Vfs::new();
        vfs.mount("dev", source);
        vfs
    }
}

/// Runs the asset server until `done` holds, or panics after a while.
fn until(
    assets: &mut AssetServer,
    vfs: &Vfs,
    what: &str,
    mut done: impl FnMut(&mut AssetServer) -> bool,
) {
    wait_until(Duration::from_secs(20), what, || {
        done(assets) || {
            assets.update_blocking(vfs, None);
            false
        }
    });
}

#[test]
fn loads_from_cookd_and_reloads_on_change() {
    let cookd = Running::start("reload");
    let vfs = cookd.connect();
    let mut assets = AssetServer::new(loaders());
    let a: Handle<LoadedThing> = assets.load(A);
    let pic: Handle<Pixels> = assets.load(PIC);
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.state(&a), LoadState::Loaded);
    assert_eq!(assets.get(&a).unwrap().value, 1);
    let b = assets.get(&a).unwrap().other.unwrap();
    assert_eq!(assets.get_ref(b).unwrap().value, 2, "B came along");
    assert_eq!(assets.get(&pic).unwrap().0, b"first pixels");
    assert_eq!(assets.version(&a), 0);

    // The text changes: A reloads in place, the same handle.
    cookd.root.write("a.masset", thing_a(5).as_bytes());
    until(&mut assets, &vfs, "A to reload", |s| {
        s.get(&a).is_some_and(|t| t.value == 5)
    });
    assert_eq!(assets.version(&a), 1);
    assert!(assets.take_reloaded().contains(&A));
    assert_eq!(
        assets.get(&pic).unwrap().0,
        b"first pixels",
        "other units untouched"
    );
    assert_eq!(assets.version(&pic), 0);

    // A source file changes: the image reloads.
    cookd.root.write("pic.bin", b"second pixels");
    until(&mut assets, &vfs, "the image to reload", |s| {
        s.get(&pic).is_some_and(|p| p.0 == b"second pixels")
    });
    assert_eq!(assets.version(&pic), 1);

    // A broken edit: the reload fails and the last good version stays.
    cookd
        .root
        .write("a.masset", b"!Version 1\nThing { !Uuid = ");
    until(&mut assets, &vfs, "the failed reload", |s| {
        !s.take_reload_errors().is_empty()
    });
    assert_eq!(assets.get(&a).unwrap().value, 5);
    assert_eq!(assets.state(&a), LoadState::Loaded);
    // Fixed again.
    cookd.root.write("a.masset", thing_a(7).as_bytes());
    until(&mut assets, &vfs, "the fix", |s| {
        s.get(&a).is_some_and(|t| t.value == 7)
    });
}

#[test]
fn failures_come_back_as_states() {
    let cookd = Running::start("failures");
    let vfs = cookd.connect();
    let mut assets = AssetServer::new(loaders());
    let broken: Handle<Pixels> = assets.load(BROKEN);
    let missing: Handle<Pixels> = assets.load(Uuid::from_u128(0x1234));
    assets.update_blocking(&vfs, None);
    match assets.state(&broken) {
        LoadState::Failed(AssetError::Source(message)) => {
            assert!(message.contains("gone.bin"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        assets.state(&missing),
        LoadState::Failed(AssetError::NotFound)
    );

    // Adding the source fixes the unit, and the held handle loads.
    cookd.root.write("gone.bin", b"here now");
    until(&mut assets, &vfs, "the fixed unit", |s| {
        s.get(&broken).is_some()
    });
    assert_eq!(assets.get(&broken).unwrap().0, b"here now");
}

#[test]
fn clients_share_the_cook() {
    let cookd = Running::start("shared");
    let (one, two) = (cookd.connect(), cookd.connect());
    let mut a = AssetServer::new(loaders());
    let mut b = AssetServer::new(loaders());
    let ha: Handle<Pixels> = a.load(PIC);
    let hb: Handle<Pixels> = b.load(PIC);
    // Both ask before either has its answer.
    a.update(&one, &mut marlin_io::IoQueue::new(4).unwrap(), None);
    b.update(&two, &mut marlin_io::IoQueue::new(4).unwrap(), None);
    a.update_blocking(&one, None);
    b.update_blocking(&two, None);
    assert_eq!(a.get(&ha).unwrap().0, b"first pixels");
    assert_eq!(b.get(&hb).unwrap().0, b"first pixels");
}

#[test]
fn a_long_cook_does_not_stall_the_service() {
    let cookd = Running::start_with(
        "stall",
        &[(
            "slow.masset",
            format!(
                "!Version 1\nSlow {{ !Uuid = \"{}\" !Name = \"Slow\" Value = 3 }}\n",
                SLOW
            ),
        )],
    );
    let vfs = cookd.connect();
    let mut assets = AssetServer::new(loaders());
    let slow: Handle<LoadedSlow> = assets.load(SLOW);
    assets.update(&vfs, &mut marlin_io::IoQueue::new(4).unwrap(), None);
    wait_until(Duration::from_secs(20), "the cook to start", || {
        GATE_ENTERED.load(Ordering::Relaxed)
    });
    // A worker holds the project for the whole cook, and the watch runs every 30 ms meanwhile:
    // with nothing changed it must not wait for the project, so a new client is still served.
    std::thread::sleep(Duration::from_millis(100));
    let connected = DevSource::connect(cookd.service.addr(), "windows", Duration::from_secs(3));
    GATE_OPEN.store(true, Ordering::Relaxed);
    assert!(
        connected.is_ok(),
        "a new client was not served during the cook"
    );
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.get(&slow).unwrap().0, 3);
}

#[test]
fn a_cooker_panic_takes_the_service_down() {
    let cookd = Running::start_with(
        "panic",
        &[(
            "bomb.masset",
            format!(
                "!Version 1\nBomb {{ !Uuid = \"{}\" !Name = \"Bomb\" }}\n",
                BOMB
            ),
        )],
    );
    let vfs = cookd.connect();
    let mut assets = AssetServer::new(loaders());
    let _bomb: Handle<LoadedSlow> = assets.load(BOMB);
    assets.update(&vfs, &mut marlin_io::IoQueue::new(4).unwrap(), None);
    // Not a request left waiting forever: the panic reaches the thread serving.
    wait_until(Duration::from_secs(20), "the service to stop", || {
        cookd.service.is_finished()
    });
    let panic = cookd
        .service
        .finish()
        .expect_err("the service thread panicked");
    assert_eq!(
        panic_message(|| std::panic::resume_unwind(panic)),
        "cooker bug"
    );
}
