//! Damaged and hostile input: decoders must return an error (or, without a checksum, possibly
//! wrong bytes), never panic, hang or touch memory out of bounds.

use crate::support::Rng;
use marlin_compress::{Codec, ErrorKind, Level, compress, gdeflate, lz4, zstd};
use marlin_test_support::{data_path, scale};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

type Decode = fn(&[u8], &mut [u8]) -> marlin_compress::Result<usize>;

/// Valid encodings to damage: our own at a few levels, plus reference fixtures.
fn samples(codec: Codec) -> Vec<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let inputs = [
        crate::support::text(20_000, 1),
        crate::support::binary(9_000, 2),
        crate::support::repetitive(30_000, 3),
        crate::support::random(3_000, 4),
        vec![7u8; 5_000],
        crate::support::text(70, 5),
    ];
    for (i, data) in inputs.iter().enumerate() {
        let mut packed = Vec::new();
        compress(codec, Level::new(1 + 4 * (i as u8 % 3)), data, &mut packed).unwrap();
        out.push((packed, data.len()));
    }
    let fixture = |name: &str, n: usize| {
        let path = data_path(env!("CARGO_MANIFEST_DIR"), name);
        (std::fs::read(path).unwrap(), n)
    };
    match codec {
        Codec::Zstd => {
            out.push(fixture("zstd/text-100000-6-l5-stream.zst", 100_000));
            out.push(fixture("zstd/binary-80000-2-l9.zst", 80_000));
        }
        Codec::Lz4 => out.push(fixture("lz4/text-100000-1-hc12.lz4b", 100_000)),
        Codec::GDeflate => out.push(fixture("gdeflate/binary-80000-2-l9.gdf", 80_000)),
        Codec::None => {}
    }
    out
}

fn mutate(r: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut v = src.to_vec();
    match r.below(6) {
        0 => {
            // A few bit flips.
            for _ in 0..1 + r.below(4) {
                if !v.is_empty() {
                    let i = r.below(v.len() as u64) as usize;
                    v[i] ^= 1 << r.below(8);
                }
            }
        }
        1 => {
            // Truncation.
            let keep = r.below(v.len() as u64 + 1) as usize;
            v.truncate(keep);
        }
        2 => {
            // A run of random bytes.
            if !v.is_empty() {
                let at = r.below(v.len() as u64) as usize;
                let len = (1 + r.below(16) as usize).min(v.len() - at);
                for b in &mut v[at..at + len] {
                    *b = r.below(256) as u8;
                }
            }
        }
        3 => {
            // Insert or delete a byte: misaligns everything after.
            let at = r.below(v.len() as u64 + 1) as usize;
            if r.below(2) == 0 || v.is_empty() || at == v.len() {
                v.insert(at, r.below(256) as u8);
            } else {
                v.remove(at);
            }
        }
        4 => {
            // Extreme values in the header region, where sizes and counts live.
            let at = r.below(v.len().min(24) as u64 + 1) as usize;
            if at < v.len() {
                v[at] = [0x00, 0xFF, 0x7F, 0x80][r.below(4) as usize];
            }
        }
        _ => {
            // Trailing garbage.
            for _ in 0..1 + r.below(64) {
                v.push(r.below(256) as u8);
            }
        }
    }
    v
}

fn hammer(codec: Codec, decode: Decode, iterations: usize) {
    let mut r = Rng::new(0xC0FFEE ^ codec.id() as u64);
    for (si, (src, n)) in samples(codec).into_iter().enumerate() {
        for it in 0..scale(iterations) {
            let bad = mutate(&mut r, &src);
            // Exact size, a bit more, and much smaller.
            for dst_len in [n, n + 33, n / 3] {
                let mut dst = vec![0u8; dst_len];
                let res = catch_unwind(AssertUnwindSafe(|| decode(&bad, &mut dst)));
                match res {
                    Ok(Ok(k)) => assert!(k <= dst_len),
                    Ok(Err(e)) => assert_eq!(e.codec(), codec),
                    Err(p) => {
                        eprintln!("{codec}: panic on sample {si}, iteration {it}, dst {dst_len}");
                        resume_unwind(p);
                    }
                }
            }
        }
    }
}

#[test]
fn lz4_block_survives_damage() {
    hammer(Codec::Lz4, lz4::decompress_block, 1500);
}

#[test]
fn zstd_survives_damage() {
    hammer(Codec::Zstd, zstd::decompress, 1500);
}

#[test]
fn gdeflate_survives_damage() {
    hammer(Codec::GDeflate, gdeflate::decompress, 1000);
}

#[test]
fn lz4_frame_survives_damage() {
    let mut r = Rng::new(77);
    let data = crate::support::text(30_000, 8);
    for flags in 0..8 {
        let opts = lz4::FrameOptions {
            block_size: lz4::BlockSize::Max64K,
            block_checksum: flags & 1 != 0,
            content_checksum: flags & 2 != 0,
            content_size: flags & 4 != 0,
        };
        let mut packed = Vec::new();
        lz4::compress_frame(&data, Level::DEFAULT, &opts, &mut packed).unwrap();
        for _ in 0..scale(400) {
            let bad = mutate(&mut r, &packed);
            let mut dst = vec![0u8; data.len()];
            if let Ok(k) = lz4::decompress_frame(&bad, &mut dst) {
                assert!(k <= dst.len());
            }
        }
    }
}

#[test]
fn checksums_catch_damage() {
    // With checksums on, any damage the decoder does not reject outright must not yield
    // success with wrong bytes.
    let data = crate::support::text(40_000, 9);
    let mut z = Vec::new();
    let opts = zstd::FrameOptions {
        checksum: true,
        content_size: true,
    };
    zstd::compress(&data, Level::DEFAULT, &opts, &mut z).unwrap();
    let mut l = Vec::new();
    let lopts = lz4::FrameOptions {
        block_checksum: true,
        content_checksum: true,
        content_size: true,
        ..Default::default()
    };
    lz4::compress_frame(&data, Level::DEFAULT, &lopts, &mut l).unwrap();
    let mut r = Rng::new(5);
    for _ in 0..scale(1000) {
        let mut dst = vec![0u8; data.len()];
        let bad = mutate(&mut r, &z);
        if zstd::decompress(&bad, &mut dst).is_ok() && bad != z {
            assert!(dst == data, "zstd accepted damaged data with a checksum");
        }
        let bad = mutate(&mut r, &l);
        if lz4::decompress_frame(&bad, &mut dst).is_ok() && bad != l {
            assert!(dst == data, "lz4 accepted damaged data with a checksum");
        }
    }
}

#[test]
fn random_garbage() {
    let mut r = Rng::new(99);
    let decoders: [(Codec, Decode); 4] = [
        (Codec::Lz4, lz4::decompress_block),
        (Codec::Lz4, lz4::decompress_frame),
        (Codec::Zstd, zstd::decompress),
        (Codec::GDeflate, gdeflate::decompress),
    ];
    let prefixes: [&[u8]; 4] = [
        &[],
        &[0x04, 0x22, 0x4D, 0x18],
        &[0x28, 0xB5, 0x2F, 0xFD],
        &[4, 0xFB],
    ];
    for i in 0..scale(3000) {
        let (codec, decode) = decoders[i % 4];
        let mut src = prefixes[i % 4].to_vec();
        let len = r.below(600) as usize;
        src.extend((0..len).map(|_| r.below(256) as u8));
        let mut dst = vec![0u8; r.below(70_000) as usize];
        if let Err(e) = decode(&src, &mut dst) {
            assert_eq!(e.codec(), codec);
        }
    }
}

#[test]
fn hostile_headers() {
    let mut dst = vec![0u8; 1000];
    // zstd: a dictionary ID is valid but unsupported.
    let dict = [0x28, 0xB5, 0x2F, 0xFD, 0x01, 0x2A, 0x01, 0x00, 0x00];
    let e = zstd::decompress(&dict, &mut dst).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Unsupported);
    // zstd: a declared size of 2^64 - 1 with no data.
    let mut huge = vec![0x28, 0xB5, 0x2F, 0xFD, 0xE0];
    huge.extend_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        zstd::decompress(&huge, &mut dst).unwrap_err().kind(),
        ErrorKind::Corrupt
    );
    // zstd: an RLE block of 2 MiB (over the block maximum).
    let rle = [0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x58, 0x03, 0x00, 0x80, 0xAA];
    assert!(zstd::decompress(&rle, &mut dst).is_err());
    // zstd: an empty skippable frame alone decodes to nothing.
    let skip = [0x50, 0x2A, 0x4D, 0x18, 0, 0, 0, 0];
    assert_eq!(zstd::decompress(&skip, &mut dst), Ok(0));
    // LZ4: legacy frames are not supported.
    let legacy = [0x02, 0x21, 0x4C, 0x18, 0, 0, 0, 0];
    assert_eq!(
        lz4::decompress_frame(&legacy, &mut dst).unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    // LZ4: an offset of zero, and one before the start of the output.
    assert!(lz4::decompress_block(&[0x14, b'a', 0, 0, 0], &mut dst).is_err());
    assert!(lz4::decompress_block(&[0x14, b'a', 2, 0, 0], &mut dst).is_err());
    // LZ4: a literal length that runs on forever.
    let mut endless = vec![0xF0];
    endless.extend(std::iter::repeat_n(0xFF, 10_000));
    assert!(lz4::decompress_block(&endless, &mut dst).is_err());
    // GDeflate: a stream claiming 65535 tiles with no table.
    let tiles = [4, 0xFB, 0xFF, 0xFF, 1, 0, 0, 0];
    assert!(gdeflate::decompress(&tiles, &mut dst).is_err());
    // GDeflate: an empty stream.
    assert_eq!(
        gdeflate::decompress(&[4, 0xFB, 0, 0, 1, 0, 0, 0], &mut dst),
        Ok(0)
    );
    // Empty input is never a valid frame.
    for codec in [Codec::Lz4, Codec::Zstd, Codec::GDeflate] {
        let e = marlin_compress::decompress(codec, &[], &mut dst).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::Corrupt, "{codec}");
    }
}
