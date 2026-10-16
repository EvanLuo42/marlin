//! A game's asset server loading from a running cookd, and reloading what changes.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
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
    l
}

static NEXT: AtomicU32 = AtomicU32::new(0);

const A: u128 = 0x0199_0000_0000_7000_8000_0000_0000_000a;
const B: u128 = 0x0199_0000_0000_7000_8000_0000_0000_000b;
const PIC: u128 = 0x0199_0000_0000_7000_8000_0000_0000_00aa;
const BROKEN: u128 = 0x0199_0000_0000_7000_8000_0000_0000_00bb;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn thing_a(value: u32) -> String {
    format!(
        "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"A\" Value = {value} Other = <b:B #{}> }}\n",
        id(A),
        id(B)
    )
}

/// A cookd on a free port over a fresh project, stopped on drop.
struct Running {
    root: PathBuf,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    fn start(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "marlin-cookd-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        write(&root, "a.masset", thing_a(1).as_bytes());
        write(
            &root,
            "b.masset",
            format!(
                "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"B\" Value = 2 }}\n",
                id(B)
            )
            .as_bytes(),
        );
        write(
            &root,
            "img.masset",
            format!(
                "!Version 1\nImage {{ !Uuid = \"{}\" !Name = \"Pic\" Source = `source:pic.bin` }}\n",
                id(PIC)
            )
            .as_bytes(),
        );
        // Its own unit: a unit fails as a whole.
        write(
            &root,
            "broken.masset",
            format!(
                "!Version 1\nImage {{ !Uuid = \"{}\" !Name = \"Broken\" Source = `source:gone.bin` }}\n",
                id(BROKEN)
            )
            .as_bytes(),
        );
        write(&root, "pic.bin", b"first pixels");

        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let (r, s) = (root.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let (db, _) = AssetDb::open(&r, Options::default()).unwrap();
            let mut cookers = Cookers::new();
            cookers.register_generic::<Thing>().unwrap();
            cookers.register::<Image>(ImageCooker).unwrap();
            let ddc = Box::new(LocalDdc::open(r.join(".marlin/ddc")).unwrap());
            let options = CookdOptions {
                workers: 2,
                watch_interval: Duration::from_millis(30),
            };
            let mut cookd = Cookd::new(db, cookers, ddc, &options);
            let mut server = bind("127.0.0.1:0".parse().unwrap()).unwrap();
            tx.send(server.local_addr()).unwrap();
            cookd.serve(&mut server, &s).unwrap();
        });
        Self {
            root,
            addr: rx.recv().unwrap(),
            stop,
            thread: Some(thread),
        }
    }

    fn connect(&self) -> Vfs {
        let source = DevSource::connect(self.addr, "windows", Duration::from_secs(10)).unwrap();
        let mut vfs = Vfs::new();
        vfs.mount("dev", source);
        vfs
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Runs the asset server until `done` holds, or panics after a while.
fn until(
    assets: &mut AssetServer,
    vfs: &Vfs,
    what: &str,
    mut done: impl FnMut(&mut AssetServer) -> bool,
) {
    let start = Instant::now();
    while !done(assets) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        assets.update_blocking(vfs, None);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn loads_from_cookd_and_reloads_on_change() {
    let cookd = Running::start("reload");
    let vfs = cookd.connect();
    let mut assets = AssetServer::new(loaders());
    let a: Handle<LoadedThing> = assets.load(id(A));
    let pic: Handle<Pixels> = assets.load(id(PIC));
    assets.update_blocking(&vfs, None);
    assert_eq!(assets.state(&a), LoadState::Loaded);
    assert_eq!(assets.get(&a).unwrap().value, 1);
    let b = assets.get(&a).unwrap().other.unwrap();
    assert_eq!(assets.get_ref(b).unwrap().value, 2, "B came along");
    assert_eq!(assets.get(&pic).unwrap().0, b"first pixels");
    assert_eq!(assets.version(&a), 0);

    // The text changes: A reloads in place, the same handle.
    write(&cookd.root, "a.masset", thing_a(5).as_bytes());
    until(&mut assets, &vfs, "A to reload", |s| {
        s.get(&a).is_some_and(|t| t.value == 5)
    });
    assert_eq!(assets.version(&a), 1);
    assert!(assets.take_reloaded().contains(&id(A)));
    assert_eq!(
        assets.get(&pic).unwrap().0,
        b"first pixels",
        "other units untouched"
    );
    assert_eq!(assets.version(&pic), 0);

    // A source file changes: the image reloads.
    write(&cookd.root, "pic.bin", b"second pixels");
    until(&mut assets, &vfs, "the image to reload", |s| {
        s.get(&pic).is_some_and(|p| p.0 == b"second pixels")
    });
    assert_eq!(assets.version(&pic), 1);

    // A broken edit: the reload fails and the last good version stays.
    write(&cookd.root, "a.masset", b"!Version 1\nThing { !Uuid = ");
    until(&mut assets, &vfs, "the failed reload", |s| {
        !s.take_reload_errors().is_empty()
    });
    assert_eq!(assets.get(&a).unwrap().value, 5);
    assert_eq!(assets.state(&a), LoadState::Loaded);
    // Fixed again.
    write(&cookd.root, "a.masset", thing_a(7).as_bytes());
    until(&mut assets, &vfs, "the fix", |s| {
        s.get(&a).is_some_and(|t| t.value == 7)
    });
}

#[test]
fn failures_come_back_as_states() {
    let cookd = Running::start("failures");
    let vfs = cookd.connect();
    let mut assets = AssetServer::new(loaders());
    let broken: Handle<Pixels> = assets.load(id(BROKEN));
    let missing: Handle<Pixels> = assets.load(id(0x1234));
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
    write(&cookd.root, "gone.bin", b"here now");
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
    let ha: Handle<Pixels> = a.load(id(PIC));
    let hb: Handle<Pixels> = b.load(id(PIC));
    // Both ask before either has its answer.
    a.update(&one, &mut marlin_io::IoQueue::new(4).unwrap(), None);
    b.update(&two, &mut marlin_io::IoQueue::new(4).unwrap(), None);
    a.update_blocking(&one, None);
    b.update_blocking(&two, None);
    assert_eq!(a.get(&ha).unwrap().0, b"first pixels");
    assert_eq!(b.get(&hb).unwrap().0, b"first pixels");
}
