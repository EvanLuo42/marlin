//! Decoding frames written by the reference zstd, LZ4 and GDeflate implementations; see
//! `fixtures/README.md`.

mod common;

use marlin_compress::{Codec, gdeflate, lz4, zstd};

/// The input a fixture name describes: `<shape>-<size>-<seed>-...`.
fn input_for(name: &str) -> Vec<u8> {
    let mut parts = name.split('-');
    let shape = parts.next().unwrap();
    let n: usize = parts.next().unwrap().parse().unwrap();
    let seed: u64 = parts.next().unwrap().parse().unwrap();
    match shape {
        "zeros" => vec![0; n],
        "random" => common::random(n, seed),
        "text" => common::text(n, seed),
        "repetitive" => common::repetitive(n, seed),
        "binary" => common::binary(n, seed),
        _ => panic!("unknown shape in {name}"),
    }
}

fn fixtures(dir: &str) -> Vec<(String, Vec<u8>)> {
    let path = format!("{}/tests/fixtures/{dir}", env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            let name = p.file_name().unwrap().to_str().unwrap().to_owned();
            (name, std::fs::read(&p).unwrap())
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {dir}");
    files
}

fn check(
    name: &str,
    expected: &[u8],
    decode: impl Fn(&mut [u8]) -> marlin_compress::Result<usize>,
) {
    // Exact size, as package chunks are decoded.
    let mut out = vec![0xCD; expected.len()];
    assert_eq!(decode(&mut out), Ok(expected.len()), "{name}");
    assert!(out == expected, "{name}: wrong output");
    // A larger destination reports the real size.
    let mut big = vec![0xCD; expected.len() + 100];
    assert_eq!(decode(&mut big), Ok(expected.len()), "{name} (larger dst)");
    assert!(
        big[..expected.len()] == *expected,
        "{name}: wrong output (larger dst)"
    );
    // One byte short fails cleanly.
    if !expected.is_empty() {
        let mut small = vec![0; expected.len() - 1];
        let e = decode(&mut small).expect_err(name);
        assert_eq!(
            e.kind(),
            marlin_compress::ErrorKind::OutputTooSmall,
            "{name}: {e}"
        );
    }
}

#[test]
fn zstd_reference_frames() {
    for (name, data) in fixtures("zstd") {
        let expected = if name == "multi-frame.zst" {
            let mut v = common::text(50000, 7);
            v.extend_from_slice(&common::binary(30000, 8));
            v
        } else {
            input_for(&name)
        };
        check(&name, &expected, |dst| zstd::decompress(&data, dst));
        check(&name, &expected, |dst| {
            marlin_compress::decompress(Codec::Zstd, &data, dst)
        });
        if !name.ends_with("-stream.zst") && name != "multi-frame.zst" {
            assert_eq!(
                zstd::frame_content_size(&data),
                Some(expected.len() as u64),
                "{name}"
            );
        }
    }
}

#[test]
fn lz4_reference_frames_and_blocks() {
    for (name, data) in fixtures("lz4") {
        let expected = input_for(&name);
        if name.ends_with(".lz4b") {
            check(&name, &expected, |dst| lz4::decompress_block(&data, dst));
            check(&name, &expected, |dst| {
                marlin_compress::decompress(Codec::Lz4, &data, dst)
            });
        } else {
            check(&name, &expected, |dst| lz4::decompress_frame(&data, dst));
        }
    }
}

#[test]
fn gdeflate_reference_tile_streams() {
    for (name, data) in fixtures("gdeflate") {
        let expected = input_for(&name);
        check(&name, &expected, |dst| gdeflate::decompress(&data, dst));
        check(&name, &expected, |dst| {
            marlin_compress::decompress(Codec::GDeflate, &data, dst)
        });
        // Tile by tile, as a parallel decoder would.
        let tiles = gdeflate::Tiles::parse(&data).unwrap();
        assert_eq!(tiles.uncompressed_size(), expected.len(), "{name}");
        let mut out = vec![0; expected.len()];
        for i in 0..tiles.len() {
            let (src, range) = tiles.get(i);
            let len = range.len();
            assert_eq!(
                gdeflate::decompress_tile(src, &mut out[range]),
                Ok(len),
                "{name} tile {i}"
            );
        }
        assert!(out == expected, "{name}: tile-wise output");
    }
}
