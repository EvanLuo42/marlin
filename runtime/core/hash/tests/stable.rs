//! Pinned vectors for the stable hashes. These values must never change.

use marlin_hash::stable::fnv1a_64_extend;
use marlin_hash::{fnv1a_32, fnv1a_64, xxh3_64, xxh3_64_with_seed, xxh3_128, xxh3_128_with_seed};

/// xorshift64 bytes; the reference values below were produced from this exact buffer.
fn data(n: usize) -> Vec<u8> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 56) as u8
        })
        .collect()
}

const SEED: u64 = 0x9e37_79b9_7f4a_7c15;

// Cross-checked against the reference implementation (via the twox-hash crate) for every
// length 0..=4200 and several seeds; these pin one length per code path and boundary.
// (len, xxh3_64, xxh3_64 seeded, xxh3_128, xxh3_128 seeded)
#[rustfmt::skip]
const VECTORS: [(usize, u64, u64, u128, u128); 24] = [
    (0, 0x2d06800538d394c2, 0x602b0e2cd6662c8b, 0x99aa06d3014798d86001c324468d497f, 0xd142977a2cca554b4ca5176998171787),
    (1, 0xb95d064157e4a8b4, 0xcea90cb7fc60d3a6, 0xdb5934bc0727b16fb95d064157e4a8b4, 0xd39787b56fbc40a7cea90cb7fc60d3a6),
    (3, 0x73ba9784590e3c21, 0x1b9b3d879cebae5a, 0xf73364c00ccfafd773ba9784590e3c21, 0xabe5a4ccc6e433f61b9b3d879cebae5a),
    (4, 0x507367f468d45e5e, 0x327499769b07fd07, 0xc5e0dbf5099d28422245f88ebd1a5c02, 0xfe6208189eecb10e0bc495df0baed045),
    (8, 0x4f2c1cb698aea121, 0x3452639c1a68d2e7, 0x0ff74717d8dedf8f1ebd29c1b4ecc4a8, 0x01b57e93bd394243377ff6aef471e494),
    (9, 0x21ad6de4d24c14f0, 0x3c556ce95069a86b, 0x4a553f9e3d645bb0c9a015f936b8f078, 0xd39054db2ff1dd55f89b97134b1ce98f),
    (16, 0x2d7026bd917f958a, 0x90c3258d554c2710, 0xf47faf37378e16041c9fb88917606e29, 0x67390a85c682f4e6fdaf379d2d5cec1c),
    (17, 0xe19561a3b4a1f86d, 0x1ade327202e06d27, 0x2ceaad3d865c411cceba6d9ce2a224fd, 0x1a524d46f09999d33832b6ae2b15d3ec),
    (32, 0x7dfbf09335b881bd, 0xce5f9b78991c3bfd, 0x8607c15b2ed7d2d7a4d210733dc7b69e, 0x3a1d619b5439d728fc3ce556d1faff4d),
    (33, 0xd7d3cf90735889a4, 0x4477957a5dad2c71, 0xc672ae02be1c8a03bdf8e15c65b8719d, 0x9615c40dff815c15767e871c329f8c76),
    (64, 0xbfde5cdfd68fc290, 0x3e5ffd744f220654, 0x264ae479def5fa544c943362e6213383, 0xf3085f74a6c817da51446e5627611d87),
    (65, 0xf59f7b1867cca406, 0xca3c09fdefbccdfc, 0xc4a798a6c58838547aeb4362ddaa2ebd, 0xa145a5c96e1ae531ccdd6b6b18e3aebc),
    (96, 0x50c4b63a422e5349, 0x1a18ae225a6007b5, 0x3c3f457253b5a5085ed4676b829cdc01, 0x7765bb37ca85a0ed43beab99f3e4000f),
    (97, 0x9cb5f2b7cf5e19dc, 0x2908282a8cf6fceb, 0xa4b6ad0f7d15ce4cb3440d19fb5863af, 0x1d018a43f251deb89061b4a813c6e380),
    (128, 0x861447540d06d95f, 0x2232eb44447eff1f, 0xa962ae7d4059b1377fba1da0732db8b3, 0xbc6181d28853498b125100656d363c93),
    (129, 0x596410e90858c8bb, 0x0d07da5991055f7b, 0x3de0d24db41eaf3ea921898f45237908, 0xca734541b110a22243b0cbea3afe4a39),
    (160, 0x735f2d6e37bb7198, 0x298797361a5e8fc9, 0xc347034ef031f3781c3f1aeab69cea85, 0x7ae6cd134ee07e2fbfc85834b66d8feb),
    (192, 0x7a1bf5fada528133, 0x6ca7c6521a068f07, 0x1908addc9b1425c557e2ddf176ab8f95, 0x87c5aa4393859802967fc4c32e519360),
    (240, 0xb3afd9aa05d8f05a, 0x2fca96e1e8d6d66d, 0xb6b6f316ffd3a8dd6a7e14cd1c58c085, 0xd57a33d83759c3c7d0be425d3aab1787),
    (241, 0xfc88e1ee2c20a14e, 0x4e98b2cb8b0a2d1d, 0xca95a365d8a765a0fc88e1ee2c20a14e, 0x0321c6bafbc9d9694e98b2cb8b0a2d1d),
    (1024, 0xd377301e0610a71b, 0xb50ff33682238a99, 0x2b354148f87eb460d377301e0610a71b, 0xcce4ae0a10378d96b50ff33682238a99),
    (1025, 0xaaa8879dc7e2a95b, 0xc06913ceddaabd0b, 0x44303a6f1b0f8ec9aaa8879dc7e2a95b, 0x8d70f54d7634277dc06913ceddaabd0b),
    (2048, 0x5e16f450f59abf88, 0x7ef055264389c59e, 0x7c3bb06daf5b6ea05e16f450f59abf88, 0x88d806d3747444cc7ef055264389c59e),
    (4096, 0x31cc379a68a931f3, 0x8d0346c09d7bc090, 0x00d572c7cc1f7a2b31cc379a68a931f3, 0x5f9d637674a75a4e8d0346c09d7bc090),
];

#[test]
fn xxh3_vectors() {
    let d = data(4096);
    for &(len, h64, h64s, h128, h128s) in &VECTORS {
        let s = &d[..len];
        assert_eq!(xxh3_64(s), h64, "xxh3_64 len {len}");
        assert_eq!(xxh3_64_with_seed(s, SEED), h64s, "xxh3_64 seeded len {len}");
        assert_eq!(xxh3_128(s), h128, "xxh3_128 len {len}");
        assert_eq!(
            xxh3_128_with_seed(s, SEED),
            h128s,
            "xxh3_128 seeded len {len}"
        );
    }
}

#[test]
fn string_vectors() {
    #[rustfmt::skip]
    let vectors: [(&str, u64, u128, u64, u32); 6] = [
        ("", 0x2d06800538d394c2, 0x99aa06d3014798d86001c324468d497f, 0xcbf29ce484222325, 0x811c9dc5),
        ("a", 0xe6c632b61e964e1f, 0xa96faf705af16834e6c632b61e964e1f, 0xaf63dc4c8601ec8c, 0xe40c292c),
        ("abc", 0x78af5f94892f3950, 0x06b05ab6733a618578af5f94892f3950, 0xe71fa2190541574b, 0x1a47e90b),
        ("Marlin", 0x9ef172ebf41fce7a, 0x5090d5dbc3ce31fad534fcad489d3976, 0x13c88b3f129af11e, 0x81a70b9e),
        ("Player_12", 0x57cc36e853aa5650, 0x511fed5ac75e61e3efb23b23ef074fc5, 0x947609aa36dfecb2, 0x7d96c192),
        ("Engine/Content/Characters/Hero/Hero.mesh", 0xc89940a17d1d2e6b, 0x58e7c1cae51c0f69def8f524d867ae28, 0x51be7e462c5d95f7, 0x24f50b37),
    ];
    for (s, h64, h128, f64, f32) in vectors {
        assert_eq!(xxh3_64(s.as_bytes()), h64, "{s:?}");
        assert_eq!(xxh3_128(s.as_bytes()), h128, "{s:?}");
        assert_eq!(fnv1a_64(s.as_bytes()), f64, "{s:?}");
        assert_eq!(fnv1a_32(s.as_bytes()), f32, "{s:?}");
    }
}

#[test]
fn const_evaluation_matches_runtime() {
    // Every path, including the long one with a derived secret, evaluated by the compiler.
    const LONG: [u8; 1500] = {
        let mut a = [0u8; 1500];
        let mut i = 0;
        while i < a.len() {
            a[i] = (i * 31 + 7) as u8;
            i += 1;
        }
        a
    };
    const H64: u64 = xxh3_64_with_seed(&LONG, 42);
    const H128: u128 = xxh3_128_with_seed(&LONG, 42);
    const SHORT: u64 = xxh3_64(b"Marlin");
    const FNV: u64 = fnv1a_64(b"Marlin");
    let long = LONG;
    assert_eq!(H64, xxh3_64_with_seed(&long, 42));
    assert_eq!(H128, xxh3_128_with_seed(&long, 42));
    assert_eq!(SHORT, 0x9ef172ebf41fce7a);
    assert_eq!(FNV, 0x13c88b3f129af11e);
}

#[test]
fn fnv_extend_is_concatenation() {
    let whole = fnv1a_64(b"Engine/Content");
    assert_eq!(fnv1a_64_extend(fnv1a_64(b"Engine/"), b"Content"), whole);
}

#[test]
fn ascii_lowercase_variant_equals_hashing_the_lowered_bytes() {
    // Every length through every code path, with upper, lower, digits and non-ASCII bytes.
    let alphabet = b"AZaz09_@[`{\x7f\x80\xc3\xa9QWERTYuiop";
    let mut x: u64 = 1;
    let bytes: Vec<u8> = (0..1500)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            alphabet[(x >> 33) as usize % alphabet.len()]
        })
        .collect();
    for len in 0..=bytes.len() {
        let s = &bytes[..len];
        assert_eq!(
            marlin_hash::xxh3_64_ascii_lowercase(s),
            xxh3_64(&s.to_ascii_lowercase()),
            "len {len}"
        );
    }
    // Every single byte value.
    for b in 0..=255u8 {
        for len in [1usize, 4, 8, 17] {
            let s = vec![b; len];
            assert_eq!(
                marlin_hash::xxh3_64_ascii_lowercase(&s),
                xxh3_64(&s.to_ascii_lowercase()),
                "byte {b:#x} len {len}"
            );
        }
    }
    const H: u64 = marlin_hash::xxh3_64_ascii_lowercase(b"Player_12");
    assert_eq!(H, xxh3_64(b"player_12"));
}
