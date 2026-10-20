//! Packages end to end: cook, pack, mount, load through the IO queue.

use std::fs;
use std::time::Duration;

use marlin_asset::cooked::{BulkKind, Encoding, UnitView, unit_id};
use marlin_asset::package::{ConfigFile, ConfigLayer, PackageError};
use marlin_asset::{
    AssetError, AssetRef, AssetServer, Link, LoadState, Loaders, MemorySource, Package,
    SourceError, SourceFile, Uuid, Vfs,
};
use marlin_compress::Codec;
use marlin_cook_core::{
    CookContext, CookOptions, Cooked, Cooker, Cookers, Ddc, LocalDdc, Manifest, PackageOptions,
    SourceObject, cook_all, write_package,
};
use marlin_io::IoQueue;
use marlin_jobs::{Config, JobSystem};
use marlin_math::Rng;
use marlin_reflect::{Reflect, type_info};
use marlin_test_support::wait_until;

use crate::support::{Project, uuid};

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

const A: Uuid = uuid(0xa);
const B: Uuid = uuid(0xb);
const PIC: Uuid = uuid(0xaa);
const NOISE: Uuid = uuid(0xab);

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
    p.write(
        "a.masset",
        format!(
            "!Version 1\nThing {{ !Uuid = \"{A}\" !Name = \"A\" Value = 1 Other = <b:B #{B}> }}\n"
        ),
    );
    p.write(
        "b.masset",
        format!("!Version 1\nThing {{ !Uuid = \"{B}\" !Name = \"B\" Value = 2 }}\n"),
    );
    p.write(
        "img.masset",
        format!(
            "!Version 1\nImage {{ !Uuid = \"{PIC}\" !Name = \"Picture\" Source = `source:pic.bin` }}\nImage {{ !Uuid = \"{NOISE}\" !Name = \"Noise\" Source = `source:noise.bin` }}\n"
        ),
    );
    p.write("pic.bin", pattern());
    p.write("noise.bin", noise());
    let db = p.content();
    let mut cookers = Cookers::new();
    cookers.register_generic::<Thing>().unwrap();
    cookers.register::<Image>(ImageCooker).unwrap();
    let ddc = p.ddc("ddc");
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
    wait_until(Duration::from_secs(20), "loading to finish", || {
        assets.update(vfs, &mut io, Some(jobs));
        assets.is_idle()
    });
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

    let a = assets.load::<LoadedThing>(A);
    let picture = assets.load::<Pixels>(PIC);
    let noisy = assets.load::<Pixels>(NOISE);
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
        .get(manifest.unit_key(unit_id("/Game/b")).unwrap())
        .unwrap()
        .unwrap();
    let view = UnitView::new(&original).unwrap();
    let entry = view.object(0).unwrap();
    let patched = marlin_asset::cooked::write::unit(
        unit_id("/Game/b"),
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
    let a = assets.load::<LoadedThing>(A);
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
    let record = index.unit(unit_id("/Game/a")).unwrap();
    let chunk = index.chunks[record.first_chunk as usize];
    let mut bad = good.clone();
    bad[chunk.file_offset as usize + 1] ^= 0x40;
    fs::write(&out, &bad).unwrap();
    let package = Package::open(out.to_str().unwrap()).unwrap();
    assert!(matches!(
        package.read_unit(unit_id("/Game/a")),
        Err(SourceError::Package(PackageError::ChunkChecksum { .. }))
    ));
    let mut vfs = Vfs::new();
    vfs.mount("game", package);
    let mut assets = AssetServer::new(loaders());
    let a = assets.load::<LoadedThing>(A);
    let b = assets.load::<LoadedThing>(B);
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

#[test]
fn named_objects_are_found_by_name_in_the_package() {
    let p = Project::new("names");
    cooked(&p);
    let db = p.content();
    // The same project with images registered as named: their `!Name`s make the name table.
    let mut cookers = Cookers::new();
    cookers.register_generic::<Thing>().unwrap();
    cookers.register_named::<Image>(ImageCooker).unwrap();
    let ddc = p.ddc("named");
    let (report, manifest) = cook_all(&db, &cookers, &ddc, &CookOptions::default());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        manifest.names,
        [("Noise".to_string(), NOISE), ("Picture".to_string(), PIC)]
    );
    assert_eq!(
        Manifest::from_bytes(&manifest.to_bytes()).unwrap(),
        manifest
    );

    let out = p.root.join("named.mpak");
    write_package(&manifest, &ddc, &PackageOptions::default(), &out).unwrap();
    let package = Package::open(out.to_str().unwrap()).unwrap();
    assert_eq!(package.index().names.len(), 2);
    let mut vfs = Vfs::new();
    vfs.mount("named", package);
    assert_eq!(vfs.named("Picture"), Some(PIC));
    assert_eq!(vfs.named("Noise"), Some(NOISE));
    // Things are not a named type.
    assert_eq!(vfs.named("A"), None);
}

/// An asset with a small blob that loads with it and a big one that streams.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Paged {
    source: SourceFile,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct PagedHeader {
    bytes: u64,
}

struct PagedCooker;

impl Cooker for PagedCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let paged: &Paged = object.value.as_ref().downcast().ok_or("not Paged")?;
        let data = ctx.read_source(&paged.source.path)?;
        ctx.add_bulk(BulkKind::Raw, b"resident".to_vec());
        ctx.add_streamed_bulk(BulkKind::Texture, data.clone());
        Ok(Cooked {
            type_id: type_info::<PagedHeader>().id,
            encoding: Encoding::Serial,
            payload: marlin_serial::to_bytes(&PagedHeader {
                bytes: data.len() as u64,
            })
            .map_err(|e| format!("{e:?}"))?,
        })
    }
}

/// A loaded `Paged`: what it saw of its blobs, and where to stream the big one from.
struct LoadedPaged {
    resident: Vec<u8>,
    streamed: marlin_asset::BlobInfo,
    unit: u64,
}

fn paged_loaders() -> Loaders {
    let mut l = Loaders::new();
    l.register::<LoadedPaged>(type_info::<PagedHeader>().id.0, |ctx| {
        let h: PagedHeader =
            marlin_serial::from_bytes(ctx.payload).map_err(|e| format!("{e:?}"))?;
        let [(BulkKind::Raw, resident), (BulkKind::Texture, streamed)] = ctx.bulk else {
            return Err("expected a resident and a streamed blob".into());
        };
        let info = ctx.blobs[1];
        if info.size != h.bytes || info.present != !streamed.is_empty() {
            return Err("blob info disagrees".into());
        }
        Ok(LoadedPaged {
            resident: resident.to_vec(),
            streamed: info,
            unit: ctx.unit_id,
        })
    });
    l
}

const PAGED: Uuid = uuid(0xba);

#[test]
fn streamed_blobs_load_on_their_own() {
    let p = Project::new("streamed");
    p.write(
        "paged.masset",
        format!("!Version 1\nPaged {{ !Uuid = \"{PAGED}\" !Name = \"Paged\" Source = `source:pic.bin` }}\n"),
    );
    p.write("pic.bin", pattern());
    let db = p.content();
    let mut cookers = Cookers::new();
    cookers.register::<Paged>(PagedCooker).unwrap();
    let ddc = p.ddc("ddc");
    let (report, manifest) = cook_all(&db, &cookers, &ddc, &CookOptions::default());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let out = p.root.join("game.mpak");
    write_package(&manifest, &ddc, &PackageOptions::default(), &out).unwrap();
    let unit = unit_id("/Game/paged");

    // The package reads the unit without the streamed blob: a shorter range, a shorter unit.
    let package = Package::open(out.to_str().unwrap()).unwrap();
    let record = *package.index().unit(unit).unwrap();
    let (_, whole) = package.index().unit_read(&record);
    let (_, resident, size) = package.index().unit_resident_read(&record);
    assert!(resident.end - resident.start < (whole.end - whole.start) / 4);
    assert!(size < record.size / 4, "{size} of {}", record.size);
    assert_eq!(
        package.index().blobs[record.first_blob as usize + 1].flags,
        marlin_asset::cooked::BULK_STREAMED
    );

    // Loaded through the IO queue: the resident blob is there, the streamed one is not.
    let mut vfs = Vfs::new();
    vfs.mount("game", package);
    let jobs = JobSystem::new(Config::default()).unwrap();
    let mut assets = AssetServer::new(paged_loaders());
    let handle = assets.load::<LoadedPaged>(PAGED);
    wait_loaded(&mut assets, &vfs, &jobs);
    let loaded = assets.get(&handle).unwrap();
    assert_eq!(loaded.resident, b"resident");
    assert!(loaded.streamed.streamed && !loaded.streamed.present);
    assert_eq!(loaded.unit, unit);

    // The streamer reads it on request, through its own queue.
    let streamer = marlin_asset::BlobStreamer::new();
    let request = streamer.request(unit, loaded.streamed.index);
    assert!(request.take().is_none());
    let mut io = IoQueue::new(8).unwrap();
    let mut started = 0;
    let mut bytes = None;
    wait_until(Duration::from_secs(10), "the streamed blob", || {
        started += streamer.update(&vfs, &mut io).reads_started;
        bytes = request.take();
        bytes.is_some()
    });
    assert_eq!(bytes.unwrap().unwrap().as_slice(), pattern());
    assert_eq!(started, 1);
    assert_eq!(streamer.in_flight(), 0);
    // A request dropped before its read starts reads nothing.
    drop(streamer.request(unit, loaded.streamed.index));
    assert_eq!(streamer.update(&vfs, &mut io).reads_started, 0);
    // Blocking, for tools.
    let request = streamer.request(unit, loaded.streamed.index);
    streamer.update_blocking(&vfs);
    assert_eq!(request.take().unwrap().unwrap().as_slice(), pattern());

    // A memory source can serve the same unit the same way.
    let bytes = ddc.get(manifest.unit_key(unit).unwrap()).unwrap().unwrap();
    let mut memory = MemorySource::new();
    memory.insert_streamed(bytes.to_vec()).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount("memory", memory);
    let mut assets = AssetServer::new(paged_loaders());
    let handle = assets.load::<LoadedPaged>(PAGED);
    assets.update_blocking(&vfs, None);
    let loaded = assets.get(&handle).unwrap();
    assert!(!loaded.streamed.present);
    let request = streamer.request(unit, loaded.streamed.index);
    streamer.update_blocking(&vfs);
    assert_eq!(request.take().unwrap().unwrap().as_slice(), pattern());
}

#[test]
fn config_files_travel_with_the_build() {
    let p = Project::new("config");
    let (ddc, mut manifest) = cooked(&p);
    assert!(manifest.config.is_empty(), "the caller adds the config");
    manifest.config = vec![
        ConfigFile {
            layer: ConfigLayer::Engine,
            name: "Engine/config/engine.toml".into(),
            text: "[r]\nvsync = true\n".into(),
        },
        ConfigFile {
            layer: ConfigLayer::Project,
            name: "Game/config/game.toml".into(),
            text: "[game]\ndefault_level = \"</Game/levels/main:Main #…>\"\n".into(),
        },
    ];
    // The config is part of the build: it changes the build's key.
    let bare = Manifest {
        config: Vec::new(),
        ..manifest.clone()
    };
    assert_ne!(manifest.key(), bare.key());
    assert_eq!(
        Manifest::from_bytes(&manifest.to_bytes()).unwrap(),
        manifest
    );

    let out = p.root.join("config.mpak");
    write_package(&manifest, &ddc, &PackageOptions::default(), &out).unwrap();
    let package = Package::open(out.to_str().unwrap()).unwrap();
    assert_eq!(package.index().config, manifest.config);
    assert_eq!(package.index().header.build, manifest.key());

    // A damaged config section is caught by the index checksum like any table.
    let mut bytes = fs::read(&out).unwrap();
    let at = bytes
        .windows(b"default_level".len())
        .position(|w| w == b"default_level")
        .unwrap();
    bytes[at] ^= 1;
    fs::write(&out, &bytes).unwrap();
    assert!(Package::open(out.to_str().unwrap()).is_err());
}
