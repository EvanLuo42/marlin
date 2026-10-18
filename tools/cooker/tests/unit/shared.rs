//! Two workstations sharing a store: what one cooked, the other does not cook again.

use std::io::Cursor;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use marlin_asset::Uuid;
use marlin_asset_db::{AssetDb, Content, Options};
use marlin_cook_core::{CookOptions, cook_all};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_store::{Server, Store};
use marlin_test_support::{BackgroundService, TempDir, test_uuid};

fn project(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("tex")).unwrap();
    for (name, shift, n) in [("a", 0u32, 1), ("b", 3, 2)] {
        let image = image::RgbaImage::from_fn(64, 64, |x, y| {
            image::Rgba([(x << shift) as u8, y as u8 * 4, 9, 255])
        });
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        std::fs::write(root.join(format!("tex/{name}.png")), png.into_inner()).unwrap();
        std::fs::write(
            root.join(format!("tex/{name}.masset")),
            format!(
                "!Version 1\nTexture {{ !Uuid = \"{}\" !Name = \"T\" Source = `source:tex/{name}.png` }}\n",
                Uuid::from_u128(test_uuid(n))
            ),
        )
        .unwrap();
    }
}

fn cook(root: &Path, store: SocketAddr) -> (usize, usize, Vec<u128>) {
    let db = Content::single(
        AssetDb::build(
            root,
            Options {
                journal: false,
                ..Options::default()
            },
        )
        .unwrap(),
    );
    let ddc = open_ddc(&root.join(".marlin/ddc"), Some(store)).unwrap();
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
    let base = TempDir::new("shared");
    let (one, two, store_dir) = (base.join("one"), base.join("two"), base.join("store"));
    project(&one);
    project(&two);

    let server = BackgroundService::spawn(move |ready, stop| {
        let mut store = Store::open(&store_dir).unwrap();
        let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        ready.listening(server.local_addr());
        server
            .run(&mut store, stop, Duration::from_millis(5))
            .unwrap();
    });
    let addr = server.addr();

    let (cooked, cached, _) = cook(&one, addr);
    assert_eq!((cooked, cached), (2, 0), "the first machine cooks");
    let (cooked, cached, keys) = cook(&two, addr);
    assert_eq!((cooked, cached), (0, 2), "the second finds it in the store");
    // Nothing was copied yet: the store answered `contains`. Reading pulls it into the local
    // cache, after which the store is not needed for it.
    let layered = open_ddc(&two.join(".marlin/ddc"), Some(addr)).unwrap();
    for &k in &keys {
        assert!(layered.get(k).unwrap().is_some());
    }
    drop(layered);
    let local = marlin_cook_core::LocalDdc::open(two.join(".marlin/ddc")).unwrap();
    assert_eq!(local.len(), 2);

    drop(server);
}
