//! Packages end to end: cook, pack, mount, load through the IO queue.

use std::fs;

use marlin_asset::cooked::{BulkKind, Encoding, UnitView, unit_id};
use marlin_asset::package::PackageError;
use marlin_asset::{
    AssetError, AssetRef, AssetServer, Link, LoadState, Loaders, MemorySource, Package,
    SourceError, SourceFile, Uuid, Vfs,
};
use marlin_asset_db::{AssetDb, Options};
use marlin_compress::Codec;
use marlin_cook_core::{
    CookContext, CookOptions, Cooked, Cooker, Cookers, Ddc, LocalDdc, Manifest, PackageOptions,
    SourceObject, cook_all, write_package,
};
use marlin_io::IoQueue;
use marlin_jobs::{Config, JobSystem};
use marlin_math::Rng;
use marlin_reflect::{Reflect, type_info};
use marlin_test_support::TempDir;

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Thing {
    value: u32,
    other: Link<Thing>,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Image {
    source: SourceFile,
}

/// The runtime form of an image: its size, read from the payload, and its pixels (bulk).
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct ImageHeader {
    bytes: u64,
}

struct ImageCooker;

impl Cooker for ImageCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let image: &Image = object.value.as_ref().downcast().ok_or("not an Image")?;
        let data = ctx.read_source(&image.source.path)?;
        let header = ImageHeader {
            bytes: data.len() as u64,
        };
        ctx.add_bulk(BulkKind::Texture, data);
        Ok(Cooked {
            type_id: type_info::<ImageHeader>().id,
            encoding: Encoding::Serial,
            payload: marlin_serial::to_bytes(&header).map_err(|e| format!("{e:?}"))?,
        })
    }
}

/// A loaded image.
struct Pixels {
    bytes: u64,
    data: Vec<u8>,
}

/// A loaded thing, its link resolved.
struct LoadedThing {
    value: u32,
    other: Option<AssetRef<LoadedThing>>,
}

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
    l.register::<Pixels>(type_info::<ImageHeader>().id.0, |ctx| {
        let h: ImageHeader =
            marlin_serial::from_bytes(ctx.payload).map_err(|e| format!("{e:?}"))?;
        let [(BulkKind::Texture, data)] = ctx.bulk else {
            return Err("expected one texture blob".into());
        };
        Ok(Pixels {
            bytes: h.bytes,
            data: data.to_vec(),
        })
    });
    l
}

struct Project {
    root: TempDir,
}

impl Project {
    fn new(name: &str) -> Self {
        Self {
            root: TempDir::new(&format!("package-{name}")),
        }
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        self.root.write(rel, bytes);
    }
}

const A: u128 = 0x0199_0000_0000_7000_8000_0000_0000_000a;
const B: u128 = 0x0199_0000_0000_7000_8000_0000_0000_000b;
const PIC: u128 = 0x0199_0000_0000_7000_8000_0000_0000_00aa;
const NOISE: u128 = 0x0199_0000_0000_7000_8000_0000_0000_00ab;

/// The pixels: 700 KiB of a compressible pattern (several chunks).
fn pattern() -> Vec<u8> {
    (0..700 << 10)
        .map(|i: u32| (i / 64 % 251) as u8 ^ (i % 7) as u8)
        .collect()
}

/// 300 KiB of noise (stored, not compressed).
fn noise() -> Vec<u8> {
    let mut bytes = vec![0; 300 << 10];
    Rng::new(1).fill_bytes(&mut bytes);
    bytes
}

/// a: A (1) → B; b: B (2); img: Picture (pattern) and Noise.
fn cooked(p: &Project) -> (LocalDdc, Manifest) {
    let id = |n: u128| Uuid::from_u128(n);
    p.write(
        "a.masset",
        format!(
            "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"A\" Value = 1 Other = <b:B #{}> }}\n",
            id(A),
            id(B)
        )
        .as_bytes(),
    );
    p.write(
        "b.masset",
        format!(
            "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"B\" Value = 2 }}\n",
            id(B)
        )
        .as_bytes(),
    );
    p.write(
        "img.masset",
        format!(
            "!Version 1\nImage {{ !Uuid = \"{}\" !Name = \"Picture\" Source = `source:pic.bin` }}\nImage {{ !Uuid = \"{}\" !Name = \"Noise\" Source = `source:noise.bin` }}\n",
            id(PIC),
            id(NOISE)
        )
        .as_bytes(),
    );
    p.write("pic.bin", &pattern());
    p.write("noise.bin", &noise());
    let db = AssetDb::build(
        &p.root,
        Options {
            journal: false,
            ..Options::default()
        },
    )
    .unwrap();
    let mut cookers = Cookers::new();
    cookers.register_generic::<Thing>().unwrap();
    cookers.register::<Image>(ImageCooker).unwrap();
    let ddc = LocalDdc::open(p.root.join(".marlin/ddc")).unwrap();
    let (report, manifest) = cook_all(&db, &cookers, &ddc, &CookOptions::default());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    (ddc, manifest)
}

#[test]
fn every_codec_round_trips_every_unit() {
    let p = Project::new("codecs");
    let (ddc, manifest) = cooked(&p);
    for codec in Codec::ALL {
        let out = p.root.join(format!("{codec}.mpak"));
        let options = PackageOptions {
            head_codec: codec,
            bulk_codec: codec,
            ..PackageOptions::default()
        };
        let report = write_package(&manifest, &ddc, &options, &out).unwrap();
        assert_eq!(report.units, 3);
        let package = Package::open(out.to_str().unwrap()).unwrap();
        let index = package.index();
        assert_eq!(index.header.build, manifest.key());
        assert_eq!(index.objects.len(), 4);
        for u in &manifest.units {
            let original = ddc.get(u.key).unwrap().unwrap();
            let read = package.read_unit(u.unit).unwrap();
            assert_eq!(
                read.as_slice(),
                original.as_slice(),
                "{codec}: unit {}",
                u.path
            );
            // Blobs alone, as streaming reads them.
            let view = UnitView::new(&original).unwrap();
            for b in 0..view.bulk_count() {
                let blob = package.read_blob(u.unit, b as u32).unwrap();
                assert_eq!(blob.as_slice(), view.bulk_bytes(&view.bulk(b).unwrap()));
            }
        }
        // Blobs start on 4 KiB in the file.
        for b in &index.blobs {
            assert_eq!(index.chunks[b.first_chunk as usize].file_offset % 4096, 0);
        }
        if codec != Codec::None {
            assert!(
                report.file_bytes < report.raw_bytes,
                "{codec} compresses: {report:?}"
            );
        }
        // Noise is stored as is.
        let noisy = index
            .chunks
            .iter()
            .filter(|c| c.codec == Codec::None)
            .count();
        assert!(noisy >= 2, "{codec}: incompressible chunks are stored");
    }
}

#[test]
fn packaging_is_deterministic() {
    let p = Project::new("determinism");
    let (ddc, manifest) = cooked(&p);
    let (one, two) = (p.root.join("one.mpak"), p.root.join("two.mpak"));
    write_package(
        &manifest,
        &ddc,
        &PackageOptions {
            threads: 1,
            ..PackageOptions::default()
        },
        &one,
    )
    .unwrap();
    write_package(
        &manifest,
        &ddc,
        &PackageOptions {
            threads: 8,
            ..PackageOptions::default()
        },
        &two,
    )
    .unwrap();
    assert_eq!(fs::read(one).unwrap(), fs::read(two).unwrap());
}

fn wait_loaded(assets: &mut AssetServer, vfs: &Vfs, jobs: &JobSystem) {
    let mut io = IoQueue::new(16).unwrap();
    for _ in 0..10_000 {
        assets.update(vfs, &mut io, Some(jobs));
        if assets.is_idle() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("loading never finished");
}

#[test]
fn a_mounted_package_loads_through_the_io_queue() {
    let p = Project::new("load");
    let (ddc, manifest) = cooked(&p);
    let out = p.root.join("game.mpak");
    let options = PackageOptions {
        bulk_codec: Codec::GDeflate,
        ..PackageOptions::default()
    };
    write_package(&manifest, &ddc, &options, &out).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount("game", Package::open(out.to_str().unwrap()).unwrap());
    let jobs = JobSystem::new(Config::default()).unwrap();
    let mut assets = AssetServer::new(loaders());

    let a = assets.load::<LoadedThing>(Uuid::from_u128(A));
    let picture = assets.load::<Pixels>(Uuid::from_u128(PIC));
    let noisy = assets.load::<Pixels>(Uuid::from_u128(NOISE));
    wait_loaded(&mut assets, &vfs, &jobs);
    assert_eq!(assets.state(&a), LoadState::Loaded);
    let loaded = assets.get(&a).unwrap();
    assert_eq!(loaded.value, 1);
    let b = loaded.other.unwrap();
    assert_eq!(
        assets.get_ref(b).unwrap().value,
        2,
        "B came with A's imports"
    );
    let pixels = assets.get(&picture).unwrap();
    assert_eq!(pixels.bytes, 700 << 10);
    assert_eq!(pixels.data, pattern());
    assert_eq!(assets.get(&noisy).unwrap().data, noise());

    drop((a, picture, noisy));
    wait_loaded(&mut assets, &vfs, &jobs);
    assert_eq!(assets.unit_count(), 0);
}

#[test]
fn a_patch_layer_overrides_the_package() {
    let p = Project::new("patch");
    let (ddc, manifest) = cooked(&p);
    let out = p.root.join("game.mpak");
    write_package(&manifest, &ddc, &PackageOptions::default(), &out).unwrap();
    // The patch: unit b again, with B = 20.
    let original = ddc
        .get(manifest.unit_key(unit_id("b")).unwrap())
        .unwrap()
        .unwrap();
    let view = UnitView::new(&original).unwrap();
    let entry = view.object(0).unwrap();
    let patched = marlin_asset::cooked::write::unit(
        unit_id("b"),
        view.header().platform,
        vec![marlin_asset::cooked::write::UnitObject {
            uuid: entry.uuid,
            type_id: entry.type_id,
            encoding: entry.encoding,
            payload: marlin_serial::to_bytes(&Thing {
                value: 20,
                other: Link::NONE,
            })
            .unwrap(),
            bulk: Vec::new(),
        }],
        &[],
    );
    let mut patch = MemorySource::new();
    patch.insert(patched).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount("game", Package::open(out.to_str().unwrap()).unwrap());
    vfs.mount("patch", patch);
    let mut assets = AssetServer::new(loaders());
    let a = assets.load::<LoadedThing>(Uuid::from_u128(A));
    assets.update_blocking(&vfs, None);
    let b = assets.get(&a).unwrap().other.unwrap();
    assert_eq!(assets.get_ref(b).unwrap().value, 20);
}

#[test]
fn damage_is_detected() {
    let p = Project::new("damage");
    let (ddc, manifest) = cooked(&p);
    let out = p.root.join("game.mpak");
    write_package(&manifest, &ddc, &PackageOptions::default(), &out).unwrap();
    let good = fs::read(&out).unwrap();
    let package = Package::open(out.to_str().unwrap()).unwrap();
    let index = package.index().clone();
    drop(package);

    // A flipped byte in unit a's data: that unit fails, the rest loads.
    let record = index.unit(unit_id("a")).unwrap();
    let chunk = index.chunks[record.first_chunk as usize];
    let mut bad = good.clone();
    bad[chunk.file_offset as usize + 1] ^= 0x40;
    fs::write(&out, &bad).unwrap();
    let package = Package::open(out.to_str().unwrap()).unwrap();
    assert!(matches!(
        package.read_unit(unit_id("a")),
        Err(SourceError::Package(PackageError::ChunkChecksum { .. }))
    ));
    let mut vfs = Vfs::new();
    vfs.mount("game", package);
    let mut assets = AssetServer::new(loaders());
    let a = assets.load::<LoadedThing>(Uuid::from_u128(A));
    let b = assets.load::<LoadedThing>(Uuid::from_u128(B));
    assets.update_blocking(&vfs, None);
    assert!(
        matches!(assets.state(&a), LoadState::Failed(AssetError::Source(_))),
        "{:?}",
        assets.state(&a)
    );
    assert_eq!(assets.state(&b), LoadState::Loaded);

    // A flipped byte in the index: the package does not open.
    let mut bad = good.clone();
    let at = bad.len() - 20;
    bad[at] ^= 1;
    fs::write(&out, &bad).unwrap();
    assert!(matches!(
        Package::open(out.to_str().unwrap()),
        Err(SourceError::Package(PackageError::IndexChecksum))
    ));
    // Truncated.
    fs::write(&out, &good[..good.len() / 2]).unwrap();
    assert!(Package::open(out.to_str().unwrap()).is_err());
}
