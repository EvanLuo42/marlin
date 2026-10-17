//! Encode-decode round trips across codecs, levels, data shapes and sizes around the block and
//! tile boundaries.

use marlin_compress::{
    Codec, ErrorKind, Level, compress, decompress, gdeflate, lz4, max_compressed_size, zstd,
};

const CODECS: [Codec; 4] = Codec::ALL;

/// Around LZ4's tail rules (5, 12, 13), Deflate's 258, GDeflate's 64 KiB tiles and zstd's
/// 128 KiB blocks.
const SIZES: [usize; 22] = [
    0, 1, 2, 3, 4, 5, 12, 13, 14, 100, 258, 259, 1000, 4096, 65535, 65536, 65537, 131071, 131072,
    131073, 200000, 300001,
];

fn round_trip(codec: Codec, level: Level, data: &[u8], what: &str) -> usize {
    let mut packed = vec![0xEE; 3];
    let n = compress(codec, level, data, &mut packed).unwrap();
    // Appends after what is already there.
    assert_eq!(&packed[..3], &[0xEE; 3]);
    assert_eq!(n, packed.len() - 3);
    assert!(
        n <= max_compressed_size(codec, data.len()),
        "{codec} {what}: {n} over bound {}",
        max_compressed_size(codec, data.len())
    );
    let mut out = vec![0xCD; data.len()];
    assert_eq!(
        decompress(codec, &packed[3..], &mut out),
        Ok(data.len()),
        "{codec} {what}"
    );
    assert!(out == data, "{codec} {what}: output differs");
    n
}

#[test]
fn every_codec_shape_size_and_level() {
    for &n in &SIZES {
        for (shape, data) in crate::support::shapes(n, n as u64) {
            for codec in CODECS {
                for level in [1, 5, 9] {
                    round_trip(
                        codec,
                        Level::new(level),
                        &data,
                        &format!("{shape} {n} L{level}"),
                    );
                }
            }
        }
    }
}

#[test]
fn all_levels_on_text() {
    let data = crate::support::text(150_000, 3);
    for codec in CODECS {
        for level in 0..=10 {
            round_trip(codec, Level::new(level), &data, &format!("text L{level}"));
        }
    }
}

#[test]
fn large_input() {
    // Several MiB of mixed content: many zstd blocks and GDeflate tiles, offsets far back.
    let mut data = Vec::new();
    for i in 0..6 {
        data.extend_from_slice(&crate::support::text(700_000, i));
        data.extend_from_slice(&crate::support::binary(300_000, i));
        data.extend_from_slice(&crate::support::random(50_000, i));
    }
    // Repeat a chunk from far back.
    let early = data[100_000..400_000].to_vec();
    data.extend_from_slice(&early);
    for codec in CODECS {
        let n = round_trip(codec, Level::DEFAULT, &data, "large");
        if codec != Codec::None {
            assert!(n < data.len() * 3 / 4, "{codec}: {n} of {}", data.len());
        }
    }
}

#[test]
fn compressible_data_shrinks() {
    let text = crate::support::text(100_000, 9);
    for codec in [Codec::Lz4, Codec::Zstd, Codec::GDeflate] {
        let n = round_trip(codec, Level::DEFAULT, &text, "text");
        assert!(n < text.len() / 2, "{codec}: {n}");
        let zeros = vec![0u8; 100_000];
        let n = round_trip(codec, Level::DEFAULT, &zeros, "zeros");
        assert!(n < 1000, "{codec}: {n}");
    }
}

#[test]
fn decode_into_larger_and_smaller_buffers() {
    let data = crate::support::text(70_000, 4);
    for codec in CODECS {
        let mut packed = Vec::new();
        compress(codec, Level::DEFAULT, &data, &mut packed).unwrap();
        let mut big = vec![0; data.len() + 1000];
        assert_eq!(
            decompress(codec, &packed, &mut big),
            Ok(data.len()),
            "{codec}"
        );
        assert!(big[..data.len()] == data[..]);
        for short in [0, 1, data.len() / 2, data.len() - 1] {
            let mut small = vec![0; short];
            let e = decompress(codec, &packed, &mut small).unwrap_err();
            assert_eq!(
                e.kind(),
                ErrorKind::OutputTooSmall,
                "{codec} into {short}: {e}"
            );
            assert_eq!(e.codec(), codec);
        }
    }
}

#[test]
fn lz4_frame_options() {
    let data = crate::support::text(300_000, 5);
    for block_size in [
        lz4::BlockSize::Max64K,
        lz4::BlockSize::Max256K,
        lz4::BlockSize::Max1M,
        lz4::BlockSize::Max4M,
    ] {
        for flags in 0..8 {
            let opts = lz4::FrameOptions {
                block_size,
                block_checksum: flags & 1 != 0,
                content_checksum: flags & 2 != 0,
                content_size: flags & 4 != 0,
            };
            let mut packed = Vec::new();
            lz4::compress_frame(&data, Level::DEFAULT, &opts, &mut packed).unwrap();
            let mut out = vec![0; data.len()];
            assert_eq!(
                lz4::decompress_frame(&packed, &mut out),
                Ok(data.len()),
                "{opts:?}"
            );
            assert!(out == data);
        }
    }
    // Incompressible blocks are stored; concatenated frames decode as one.
    let random = crate::support::random(100_000, 6);
    let mut packed = Vec::new();
    lz4::compress_frame(&random, Level::DEFAULT, &Default::default(), &mut packed).unwrap();
    lz4::compress_frame(&data, Level::DEFAULT, &Default::default(), &mut packed).unwrap();
    let mut out = vec![0; random.len() + data.len()];
    assert_eq!(lz4::decompress_frame(&packed, &mut out), Ok(out.len()));
    assert!(out[..random.len()] == random[..] && out[random.len()..] == data[..]);
}

#[test]
fn zstd_frame_options() {
    for n in [0, 1, 255, 256, 65791, 65792, 300_000] {
        let data = crate::support::text(n, 7);
        for checksum in [false, true] {
            for content_size in [false, true] {
                let opts = zstd::FrameOptions {
                    checksum,
                    content_size,
                };
                let mut packed = Vec::new();
                zstd::compress(&data, Level::DEFAULT, &opts, &mut packed).unwrap();
                let mut out = vec![0; n];
                assert_eq!(zstd::decompress(&packed, &mut out), Ok(n), "{n} {opts:?}");
                assert!(out == data);
                assert_eq!(
                    zstd::frame_content_size(&packed),
                    content_size.then_some(n as u64),
                    "{n} {opts:?}"
                );
            }
        }
    }
}

#[test]
fn gdeflate_tiles() {
    let data = crate::support::text(200_000, 8);
    let mut packed = Vec::new();
    gdeflate::compress(&data, Level::DEFAULT, &mut packed).unwrap();
    let tiles = gdeflate::Tiles::parse(&packed).unwrap();
    assert_eq!(tiles.len(), 4);
    assert_eq!(tiles.uncompressed_size(), data.len());
    let mut out = vec![0; data.len()];
    // Any order: tiles are independent.
    for i in (0..tiles.len()).rev() {
        let (src, range) = tiles.get(i);
        let len = range.len();
        assert_eq!(gdeflate::decompress_tile(src, &mut out[range]), Ok(len));
    }
    assert!(out == data);

    let mut tile = Vec::new();
    gdeflate::compress_tile(&data[..1000], Level::DEFAULT, &mut tile).unwrap();
    let mut out = vec![0; 1000];
    assert_eq!(gdeflate::decompress_tile(&tile, &mut out), Ok(1000));
    assert!(out == data[..1000]);
    let e = gdeflate::compress_tile(&data[..70_000], Level::DEFAULT, &mut tile).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Unsupported);
}

#[test]
fn codec_ids_are_stable() {
    for codec in Codec::ALL {
        assert_eq!(Codec::from_id(codec.id()), Some(codec));
    }
    assert_eq!(
        Codec::ALL.map(Codec::id),
        [0, 1, 2, 3],
        "on-disk IDs must never change"
    );
    assert_eq!(Codec::from_id(4), None);
    assert_eq!(Codec::Zstd.to_string(), "zstd");
}

#[test]
fn levels_clamp() {
    assert_eq!(Level::new(0), Level::FASTEST);
    assert_eq!(Level::new(200), Level::BEST);
    assert_eq!(Level::default(), Level::DEFAULT);
    assert_eq!(Level::new(7).get(), 7);
}
