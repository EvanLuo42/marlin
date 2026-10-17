//! The cook targets compiled in from platforms/*/platform.toml.

use std::path::Path;

use marlin_cook_core::{CookOptions, PackageOptions, Platform};

#[test]
fn every_manifest_is_a_target() {
    let platforms = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms");
    let mut names: Vec<String> = std::fs::read_dir(platforms)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|dir| dir.join("platform.toml").is_file())
        .map(|dir| dir.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let built: Vec<&str> = Platform::all().iter().map(|p| p.name).collect();
    assert_eq!(built, names);
}

#[test]
fn the_host_is_the_default_target() {
    let host = Platform::host();
    assert_eq!(CookOptions::default().platform, host);
    assert_eq!(Platform::find(host.name).unwrap(), host);
    let package = PackageOptions::default();
    assert_eq!(package.head_codec, host.head_codec);
    assert_eq!(package.bulk_codec, host.bulk_codec);
}

#[test]
fn unknown_names_are_errors_that_list_the_known_ones() {
    let message = Platform::find("Windows").unwrap_err().to_string();
    assert!(message.contains("unknown platform `Windows`"), "{message}");
    for p in Platform::all() {
        assert!(message.contains(p.name), "{message}");
    }
}
