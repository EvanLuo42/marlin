//! Asset identity types.

use marlin_asset::{Link, SourceFile, Uuid};
use marlin_reflect::{Reflect, type_info};

#[test]
fn uuid_text_round_trips() {
    let text = "deb8916d-3350-78d5-bf61-c2afbe805f67";
    let uuid: Uuid = text.parse().unwrap();
    assert_eq!(uuid.as_u128(), 0xdeb8916d_3350_78d5_bf61_c2afbe805f67);
    assert_eq!(uuid.to_string(), text);
    assert_eq!(
        "DEB8916D-3350-78D5-BF61-C2AFBE805F67"
            .parse::<Uuid>()
            .unwrap(),
        uuid
    );
    assert_eq!(uuid.version(), 7);
    for bad in [
        "",
        "deb8916d3350-78d5-bf61-c2afbe805f67",
        "deb8916d-3350-78d5-bf61-c2afbe805f6",
        "deb8916d-3350-78d5-bf61-c2afbe805f6g",
        "deb8916d_3350_78d5_bf61_c2afbe805f67",
        "+eb8916d-3350-78d5-bf61-c2afbe805f67",
    ] {
        assert!(bad.parse::<Uuid>().is_err(), "{bad}");
    }
    assert_eq!(
        Uuid::NIL.to_string(),
        "00000000-0000-0000-0000-000000000000"
    );
}

#[test]
fn v7_layout_follows_rfc_9562() {
    let uuid = Uuid::v7(0x0189_abcd_ef01, u128::MAX);
    let v = uuid.as_u128();
    assert_eq!(v >> 80, 0x0189_abcd_ef01, "48-bit millisecond timestamp");
    assert_eq!(uuid.version(), 7);
    assert_eq!((v >> 62) & 0b11, 0b10, "variant");
    assert_eq!(v & ((1 << 62) - 1), (1 << 62) - 1, "rand_b");
    assert_eq!((v >> 64) & 0xfff, 0xfff, "rand_a");
    assert!(Uuid::v7(1, 0) < Uuid::v7(2, 0), "time-ordered");
}

#[test]
fn hash64_is_stable_and_spreads() {
    let a = Uuid::v7(1_700_000_000_000, 1);
    assert_eq!(a.hash64(), a.hash64());
    // Neighbouring ids must not collide or cluster in the low bits.
    let hashes: std::collections::HashSet<u64> = (0..100_000)
        .map(|i| Uuid::v7(1_700_000_000_000, i).hash64() & 0xffff)
        .collect();
    // 100k uniform draws into 65536 buckets fill about 51.3k of them.
    assert!(hashes.len() > 45_000, "{}", hashes.len());
}

#[test]
fn reflected_names_are_pinned() {
    assert_eq!(type_info::<Uuid>().name.to_string(), "marlin_asset::Uuid");
    assert_eq!(
        type_info::<SourceFile>().name.to_string(),
        "marlin_asset::SourceFile"
    );
    #[derive(Reflect)]
    #[reflect(name = "tests::Mesh")]
    struct Mesh;
    assert_eq!(
        type_info::<Link<Mesh>>().name.to_string(),
        "marlin_asset::Link<tests::Mesh>"
    );
}

#[test]
fn links_are_plain_uuids() {
    struct NotReflected;
    let link: Link<NotReflected> = Link::new(Uuid::from_u128(9));
    let copy = link;
    assert_eq!(copy, link);
    assert!(!link.is_none());
    assert!(Link::<NotReflected>::default().is_none());
    assert_eq!(size_of::<Link<NotReflected>>(), size_of::<Uuid>());
    assert_eq!(
        format!("{link:?}"),
        "Link(00000000-0000-0000-0000-000000000009)"
    );
}
