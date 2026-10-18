//! Writes the seed corpora in `fuzz/corpus/<target>/`: small valid inputs for every target,
//! built with the crates' own writers, plus the compress crate's reference-codec fixtures.
//!
//! ```text
//! cargo run --release --manifest-path fuzz/Cargo.toml --bin seed-corpus
//! ```
//!
//! Rerun it when a format changes; the fuzzers' own finds stay in `target/fuzz/corpus/`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::mem::offset_of;
use std::path::{Path, PathBuf};

use marlin_asset::Uuid;
use marlin_asset::cooked::{BulkKind, Encoding, unit_id, write::UnitObject};
use marlin_asset::package::{
    self, BlobRecord, ChunkRecord, ObjectRecord, PackageHeader, PackageIndex, UnitRecord,
};
use marlin_asset::wire::{Writer, frame};
use marlin_compress::{Codec, Level, compress, gdeflate, lz4, zstd};
use marlin_fuzz::{Bounds, CookedItem, CookedRoot, Everything, Node, Shape, Tuple, Unit};
use marlin_serial::cooked::{CookedBuilder, RelPtr, RelSlice};
use marlin_serial::to_bytes;
use marlin_shaderc_backend_spirv::write_module;
use marlin_shaderc_ir::parse_module;
use marlin_text::loc::{Locale, StringTableBuilder};

fn main() {
    let fuzz = Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus = fuzz.join("corpus");
    let repo = fuzz.parent().unwrap();
    let mut out = Corpus { root: corpus };
    text(&mut out);
    serial(&mut out);
    compress_seeds(&mut out, repo);
    asset(&mut out);
    front_syntax(&mut out, repo);
    shaderc_ir(&mut out, repo);
    shaderc_opt(&mut out, repo);
    shaderc_spirv(&mut out, repo);
}

struct Corpus {
    root: PathBuf,
}

impl Corpus {
    fn put(&mut self, target: &str, name: &str, bytes: &[u8]) {
        let dir = self.root.join(target);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), bytes).unwrap();
    }
}

fn text(out: &mut Corpus) {
    let messages = [
        "Quit",
        "{name} has {count, plural, =0 {no items} one {# item} other {# items}}.",
        "{n, plural, one {# файл} few {# файла} many {# файлов} other {# файла}}",
        "{x, number, 2} of {n, number}",
        "{d, date, long} at {t, time}; {d}",
        "{{literal}} braces and {name}",
        "{n, plural, other {{count, plural, one {# inner} other {## {n} #}}}}",
    ];
    let numbers = [0i64, 1, 22, -5, 1200, i64::MIN];
    for (i, message) in messages.iter().enumerate() {
        let mut input = numbers[i % numbers.len()].to_le_bytes().to_vec();
        input.push(i as u8);
        input.extend_from_slice(message.as_bytes());
        out.put("text_message", &format!("message-{i}"), &input);
    }

    for (i, tag) in [
        "en",
        "pt-BR",
        "zh-Hans-CN",
        "zh-Hant-TW",
        "sr-Latn-RS",
        "und",
        "EN_us",
        "ar-001",
    ]
    .iter()
    .enumerate()
    {
        out.put("text_locale", &format!("tag-{i}"), tag.as_bytes());
    }

    let tables: [(&str, &[(&str, &str)]); 3] = [
        ("en", &[("quit", "Quit"), ("files", messages[1])]),
        ("ru", &[("files", messages[2]), ("when", messages[4])]),
        ("zh-Hant", &[]),
    ];
    for (locale, entries) in tables {
        let mut table = StringTableBuilder::new(Locale::parse(locale).unwrap());
        for (key, message) in entries {
            table.insert(key, message).unwrap();
        }
        out.put(
            "text_string_table",
            &format!("table-{locale}"),
            &table.build().unwrap(),
        );
    }
}

fn serial(out: &mut Corpus) {
    let tree = Node {
        value: 1,
        children: vec![
            Node {
                value: 2,
                children: vec![],
            },
            Node {
                value: 3,
                children: vec![Node {
                    value: 4,
                    children: vec![],
                }],
            },
        ],
    };
    let shapes = vec![
        Shape::Empty,
        Shape::Circle(1.5),
        Shape::Rect { w: 2.0, h: 3.0 },
        Shape::Path(vec![[0.0, 1.0], [2.0, 3.0]], true),
    ];
    let everything = Everything {
        b: true,
        c: '鱼',
        u8: u8::MAX,
        u16: 0xbeef,
        u32: u32::MAX - 1,
        u64: u64::MAX,
        u128: u128::MAX / 3,
        usize: 123_456,
        i8: i8::MIN,
        i16: -2,
        i32: i32::MIN,
        i64: i64::MIN + 7,
        i128: i128::MIN,
        isize: -99,
        f32: -0.0,
        f64: f64::MAX,
        text: "hello, marlin".into(),
        list: vec![None, Some(vec![]), Some(vec![1, -1, i16::MAX])],
        grid: [[1, 2], [3, 4], [5, 6]],
        maybe: Some(Box::new(Bounds {
            min: [-1.0; 3],
            max: [f32::INFINITY, 0.5, 1e-30],
        })),
        shapes: shapes.clone(),
        unit: Unit,
        tuple: Tuple(-3, "t".into()),
        pair: (7, "pair".into()),
        map: BTreeMap::from([("a".into(), vec![1, 2]), ("b".into(), vec![])]),
        set: BTreeSet::from([-1, 0, 9]),
        queue: VecDeque::from([1.0, 2.5]),
        tree: tree.clone(),
        nothing: (),
    };
    out.put("serial_read", "everything", &to_bytes(&everything).unwrap());
    out.put("serial_read", "tree", &to_bytes(&tree).unwrap());
    out.put("serial_read", "shapes", &to_bytes(&shapes).unwrap());

    let (mut b, root) = CookedBuilder::with_header(&CookedRoot {
        name: RelSlice::EMPTY,
        items: RelSlice::EMPTY,
        first: RelPtr::NULL,
        scale: 0.5,
    });
    let name = b.push_str("root");
    let label = b.push_str("item");
    let items = b.push_slice(&[
        CookedItem {
            id: 1,
            label: RelSlice::EMPTY,
            next: RelPtr::NULL,
            weights: [1.0, 2.0, 3.0],
        },
        CookedItem {
            id: 2,
            label: RelSlice::EMPTY,
            next: RelPtr::NULL,
            weights: [0.0; 3],
        },
    ]);
    let (first, second) = (items.at(0), items.at(1));
    b.link_slice(root.field(offset_of!(CookedRoot, name), |r| &r.name), name)
        .unwrap();
    b.link_slice(
        root.field(offset_of!(CookedRoot, items), |r| &r.items),
        items,
    )
    .unwrap();
    b.link(
        root.field(offset_of!(CookedRoot, first), |r| &r.first),
        first,
    )
    .unwrap();
    b.link_slice(
        first.field(offset_of!(CookedItem, label), |i| &i.label),
        label,
    )
    .unwrap();
    b.link(
        first.field(offset_of!(CookedItem, next), |i| &i.next),
        second,
    )
    .unwrap();
    // A cycle: the second item links back to the first.
    b.link(
        second.field(offset_of!(CookedItem, next), |i| &i.next),
        first,
    )
    .unwrap();
    out.put("serial_cooked", "linked", b.finish().as_slice());
}

fn sample(len: usize, seed: u32) -> Vec<u8> {
    let words = [
        &b"marlin "[..],
        b"fish ",
        b"engine ",
        b"0123 ",
        b"\x00\x01\xff",
    ];
    let mut x = seed.wrapping_mul(0x9E37_79B9) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.extend_from_slice(words[x as usize % words.len()]);
    }
    v.truncate(len);
    v
}

/// The compress crate's reference-codec fixtures (only the smaller ones: small seeds fuzz
/// faster), and our own encodings of a few inputs.
fn compress_seeds(out: &mut Corpus, repo: &Path) {
    let crate_dir = repo.join("runtime/core/compress/tests");
    let fixtures = ["data", "fixtures", "data/fixtures"]
        .iter()
        .map(|d| crate_dir.join(d))
        .find(|d| d.join("zstd").is_dir())
        .expect("the compress fixtures (tests/data or tests/fixtures)");
    for (dir, ext, target) in [
        ("zstd", "zst", "compress_zstd"),
        ("lz4", "lz4", "compress_lz4_frame"),
        ("lz4", "lz4b", "compress_lz4_block"),
        ("gdeflate", "gdf", "compress_gdeflate"),
    ] {
        for entry in fs::read_dir(fixtures.join(dir)).unwrap() {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            if path.extension().is_none_or(|e| e != ext) || bytes.len() > 64 << 10 {
                continue;
            }
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            if target == "compress_lz4_block" {
                // Names are `<shape>-<size>-...`: the block's decoded size.
                let size: u32 = name.split('-').nth(1).unwrap().parse().unwrap();
                out.put(target, &name, &[&size.to_le_bytes()[..3], &bytes].concat());
            } else {
                out.put(target, &name, &bytes);
            }
        }
    }

    for (i, data) in [sample(3000, 1), sample(70_000, 2), vec![7; 5000], vec![]]
        .iter()
        .enumerate()
    {
        let level = Level::new(1 + 4 * (i as u8 % 3));
        let mut packed = Vec::new();
        zstd::compress(data, level, &zstd::FrameOptions::default(), &mut packed).unwrap();
        out.put("compress_zstd", &format!("own-{i}"), &packed);

        packed.clear();
        compress(Codec::Lz4, level, data, &mut packed).unwrap();
        let size = (data.len() as u32).to_le_bytes();
        out.put(
            "compress_lz4_block",
            &format!("own-{i}"),
            &[&size[..3], &packed].concat(),
        );

        packed.clear();
        let opts = lz4::FrameOptions {
            block_size: lz4::BlockSize::Max64K,
            block_checksum: i % 2 == 0,
            content_checksum: true,
            content_size: i % 2 == 1,
        };
        lz4::compress_frame(data, level, &opts, &mut packed).unwrap();
        out.put("compress_lz4_frame", &format!("own-{i}"), &packed);

        packed.clear();
        gdeflate::compress(data, level, &mut packed).unwrap();
        out.put("compress_gdeflate", &format!("own-{i}"), &packed);
    }
}

fn asset(out: &mut Corpus) {
    let objects = vec![
        UnitObject {
            uuid: Uuid::from_u128(1),
            type_id: 11,
            encoding: Encoding::Serial,
            payload: b"abc".to_vec(),
            bulk: vec![],
        },
        UnitObject {
            uuid: Uuid::from_u128(2),
            type_id: 22,
            encoding: Encoding::Cooked,
            payload: vec![1, 2, 3, 4],
            bulk: vec![(BulkKind::Texture, vec![9; 300]), (BulkKind::Raw, vec![])],
        },
    ];
    let unit = marlin_asset::cooked::write::unit(
        unit_id("a/b"),
        7,
        objects,
        &[(Uuid::from_u128(5), unit_id("c/d"))],
    );
    out.put("asset_unit", "two-objects", &unit);
    let empty = marlin_asset::cooked::write::unit(unit_id("empty"), 0, vec![], &[]);
    out.put("asset_unit", "empty", &empty);

    out.put(
        "asset_package",
        "one-unit",
        &package_of(&[(unit_id("a/b"), &unit)]),
    );
    out.put(
        "asset_package",
        "two-units",
        &package_of(&[(unit_id("empty"), &empty), (unit_id("a/b"), &unit)]),
    );

    let mut stream = Vec::new();
    let mut w = Writer::new();
    w.u32(7).uuid(Uuid::from_u128(3)).str("path/to/asset");
    stream.extend_from_slice(&frame(0b10_01_00, 1, &w.finish()));
    let mut w = Writer::new();
    w.bytes(b"payload").u64(99);
    stream.extend_from_slice(&frame(0b11, 2, &w.finish()));
    stream.extend_from_slice(&frame(0, 0, &[]));
    out.put(
        "asset_wire",
        "three-frames",
        &[&[0u8][..], &stream].concat(),
    );
    out.put("asset_wire", "pieces", &[&[3u8][..], &stream].concat());
}

/// A package of `units` (sorted by id), each split into chunks: stored, zstd and LZ4 in turn.
/// The bulk region of each unit is its own blob, as the packager writes it.
fn package_of(units: &[(u64, &Vec<u8>)]) -> Vec<u8> {
    const CHUNK: usize = 256;
    let mut units = units.to_vec();
    units.sort_by_key(|u| u.0);
    let mut file = vec![0u8; PackageHeader::SIZE];
    let (mut unit_records, mut blob_records, mut chunks, mut objects) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for &(id, bytes) in &units {
        let view = marlin_asset::cooked::UnitView::new(bytes).unwrap();
        let first_chunk = chunks.len() as u32;
        let first_blob = blob_records.len() as u32;
        let bulk_offset = view.header().bulk_offset as usize;
        let mut at = 0;
        while at < bytes.len() {
            // Chunks never straddle the start of the bulk region, so it can be a blob.
            let limit = if at < bulk_offset && bulk_offset < bytes.len() {
                bulk_offset
            } else {
                bytes.len()
            };
            let end = (at + CHUNK).min(limit);
            let raw = &bytes[at..end];
            let codec = [Codec::None, Codec::Zstd, Codec::Lz4][chunks.len() % 3];
            let mut stored = Vec::new();
            match codec {
                Codec::None => stored.extend_from_slice(raw),
                c => {
                    compress(c, Level::DEFAULT, raw, &mut stored).unwrap();
                }
            }
            let offset = file
                .len()
                .next_multiple_of(package::CHUNK_FILE_ALIGN as usize);
            file.resize(offset, 0);
            file.extend_from_slice(&stored);
            chunks.push(ChunkRecord {
                file_offset: offset as u64,
                unit_offset: at as u64,
                stored_size: stored.len() as u32,
                raw_size: raw.len() as u32,
                codec,
                checksum: marlin_hash::xxh3_64(&stored) as u32,
            });
            at = end;
        }
        if bulk_offset < bytes.len() {
            let first = chunks
                .iter()
                .skip(first_chunk as usize)
                .position(|c| c.unit_offset == bulk_offset as u64)
                .unwrap()
                + first_chunk as usize;
            blob_records.push(BlobRecord {
                unit_offset: bulk_offset as u64,
                size: (bytes.len() - bulk_offset) as u64,
                first_chunk: first as u32,
                chunk_count: (chunks.len() - first) as u32,
            });
        }
        unit_records.push(UnitRecord {
            unit: id,
            size: bytes.len() as u64,
            first_chunk,
            chunk_count: chunks.len() as u32 - first_chunk,
            first_blob,
            blob_count: blob_records.len() as u32 - first_blob,
        });
        for i in 0..view.object_count() {
            objects.push(ObjectRecord {
                uuid: view.object(i).unwrap().uuid,
                unit: id,
            });
        }
    }
    objects.sort_by_key(|o| o.uuid);
    let index = package::write::index(&unit_records, &blob_records, &chunks, &objects);
    let index_offset = file.len().next_multiple_of(16);
    file.resize(index_offset, 0);
    file.extend_from_slice(&index);
    let header = PackageHeader {
        platform: 7,
        unit_count: unit_records.len() as u32,
        blob_count: blob_records.len() as u32,
        chunk_count: chunks.len() as u32,
        object_count: objects.len() as u32,
        build: 0x5eed,
        index_offset: index_offset as u64,
        index_size: index.len() as u64,
    };
    file[..PackageHeader::SIZE].copy_from_slice(&package::write::header(&header));

    // The seed must be a valid package, or it seeds nothing past the checks.
    let parsed = PackageIndex::parse(header, &file[index_offset..]).unwrap();
    for (record, &(_, bytes)) in parsed.units.iter().zip(&units) {
        let (range, file_range) = parsed.unit_read(record);
        let mut decoded = vec![0u8; record.size as usize];
        parsed
            .decode(
                range,
                &file[file_range.start as usize..file_range.end as usize],
                file_range.start,
                &mut decoded,
                0,
            )
            .unwrap();
        assert_eq!(&decoded, bytes);
    }
    file
}

/// `front_syntax_parse`: the parser's snapshot sources and the shader syntax tour, each under
/// both dialect selectors.
fn front_syntax(out: &mut Corpus, repo: &Path) {
    let mut sources: Vec<PathBuf> = Vec::new();
    for dir in [
        "compiler/front/syntax/tests/data/parser/ok",
        "compiler/front/syntax/tests/data/parser/err",
    ] {
        for entry in fs::read_dir(repo.join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "src") {
                sources.push(path);
            }
        }
    }
    sources.push(repo.join("compiler/shader/syntax/tests/data/tour.mshader"));
    sources.sort();
    for path in sources {
        let text = fs::read(&path).unwrap();
        let stem = path.file_stem().unwrap().to_string_lossy();
        for selector in [0u8, 1] {
            let mut input = vec![selector];
            input.extend_from_slice(&text);
            out.put("front_syntax_parse", &format!("{stem}-{selector}"), &input);
        }
    }
}

fn shaderc_ir(out: &mut Corpus, repo: &Path) {
    let dir = repo.join("compiler/shader/ir/tests/data/text");
    let mut sources: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ir"))
        .collect();
    sources.sort();
    for path in sources {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        out.put("shaderc_ir_parse", &stem, &fs::read(&path).unwrap());
    }
}

fn shaderc_opt(out: &mut Corpus, repo: &Path) {
    let mut sources: Vec<PathBuf> = Vec::new();
    for dir in [
        "compiler/shader/ir/tests/data/text",
        "compiler/shader/opt/tests/data/passes",
    ] {
        sources.extend(
            fs::read_dir(repo.join(dir))
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|e| e == "ir")),
        );
    }
    sources.sort();
    for path in sources {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        out.put("shaderc_opt_pipeline", &stem, &fs::read(&path).unwrap());
    }
}

/// The backend's compute fixtures written as SPIR-V.
fn shaderc_spirv(out: &mut Corpus, repo: &Path) {
    let dir = repo.join("compiler/shader/backend_spirv/tests/data/compute");
    let mut sources: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ir"))
        .collect();
    sources.sort();
    for path in sources {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let text = fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
        let m = parse_module(&text).unwrap();
        let words = write_module(&m, m.entries[0].func).unwrap();
        out.put(
            "shaderc_backend_spirv_validate",
            &stem,
            &marlin_shaderc_backend_spirv::to_bytes(&words),
        );
    }
}
