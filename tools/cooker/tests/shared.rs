//! Two workstations sharing a store: what one cooked, the other does not cook again.

use std::io::Cursor;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use marlin_asset_db::{AssetDb, Options};
use marlin_cook_core::{CookOptions, cook_all};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_store::{Server, Store};

fn project(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("tex")).unwrap();
    for (name, shift) in [("a", 0u32), ("b", 3)] {
        let image = image::RgbaImage::from_fn(64, 64, |x, y| {
            image::Rgba([(x << shift) as u8, y as u8 * 4, 9, 255])
        });
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        std::fs::write(root.join(format!("tex/{name}.png")), png.into_inner()).unwrap();
        std::fs::write(
            root.join(format!("tex/{name}.masset")),
            format!(
                "!Version 1\nTexture {{ !Uuid = \"01990000-0000-7000-8000-00000000000{}\" !Name = \"T\" Source = `source:tex/{name}.png` }}\n",
                if name == "a" { 1 } else { 2 }
            ),
        )
        .unwrap();
    }
}

fn cook(root: &Path, store: SocketAddr) -> (usize, usize, Vec<u128>) {
    let db = AssetDb::build(
        root,
        Options {
            journal: false,
            ..Options::default()
        },
    )
    .unwrap();
    let ddc = open_ddc(root, Some(store)).unwrap();
    let (report, _) = cook_all(
        &db,
        &engine_cookers(),
        ddc.as_ref(),
        &CookOptions::default(),
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    (
        report.cooked(),
        report.cached(),
        report.units.iter().map(|u| u.key).collect(),
    )
}

#[test]
fn the_second_machine_cooks_nothing() {
    let base: PathBuf = std::env::temp_dir().join(format!("marlin-shared-{}", std::process::id()));
    let (one, two, store_dir) = (base.join("one"), base.join("two"), base.join("store"));
    project(&one);
    project(&two);

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let s = stop.clone();
    let server = std::thread::spawn(move || {
        let mut store = Store::open(&store_dir).unwrap();
        let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        tx.send(server.local_addr()).unwrap();
        server
            .run(&mut store, &s, Duration::from_millis(5))
            .unwrap();
    });
    let addr = rx.recv().unwrap();

    let (cooked, cached, _) = cook(&one, addr);
    assert_eq!((cooked, cached), (2, 0), "the first machine cooks");
    let (cooked, cached, keys) = cook(&two, addr);
    assert_eq!((cooked, cached), (0, 2), "the second finds it in the store");
    // Nothing was copied yet: the store answered `contains`. Reading pulls it into the local
    // cache, after which the store is not needed for it.
    let layered = open_ddc(&two, Some(addr)).unwrap();
    for &k in &keys {
        assert!(layered.get(k).unwrap().is_some());
    }
    drop(layered);
    let local = marlin_cook_core::LocalDdc::open(two.join(".marlin/ddc")).unwrap();
    assert_eq!(local.len(), 2);

    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(base);
}
