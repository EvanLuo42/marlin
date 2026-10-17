//! CRC-32 (IEEE) vectors, streaming and the slicing-by-8 path against a bitwise reference.

use marlin_hash::{Crc32, crc32, crc32_extend};

/// One bit at a time, straight from the definition.
fn bitwise(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn published_vectors() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(b""), 0);
    assert_eq!(crc32(b"a"), 0xe8b7_be43);
    assert_eq!(crc32(b"abc"), 0x3524_41c2);
    assert_eq!(
        crc32(b"The quick brown fox jumps over the lazy dog"),
        0x414f_a339
    );
    const CONST_EVAL: u32 = crc32(b"123456789");
    assert_eq!(CONST_EVAL, 0xcbf4_3926);
}

#[test]
fn every_length_and_split_matches_the_bitwise_definition() {
    let data: Vec<u8> = (0..300u32).map(|i| (i * 131 + 7) as u8).collect();
    for len in 0..=data.len() {
        let expected = bitwise(&data[..len]);
        assert_eq!(crc32(&data[..len]), expected, "len {len}");
        for cut in 0..=len {
            assert_eq!(
                crc32_extend(crc32(&data[..cut]), &data[cut..len]),
                expected,
                "len {len} cut {cut}"
            );
        }
    }
}

#[test]
fn streaming() {
    let data: Vec<u8> = (0..5000u32).map(|i| (i ^ (i >> 3)) as u8).collect();
    for chunk in [1, 3, 8, 13, 64, 1000] {
        let mut c = Crc32::new();
        for piece in data.chunks(chunk) {
            c.update(piece);
        }
        assert_eq!(c.finish(), crc32(&data), "chunk {chunk}");
    }
    let mut c = Crc32::with_initial(crc32(b"1234"));
    c.update(b"56789");
    assert_eq!(c.finish(), 0xcbf4_3926);
}
