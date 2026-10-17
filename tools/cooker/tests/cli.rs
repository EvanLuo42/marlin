//! `marlin-cook cook` on a small project: cook, cache, fail.

use std::io::Cursor;
use std::process::Command;

use marlin_test_support::TempDir;

fn run(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_marlin-cook"))
        .args(args)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

#[test]
fn cooks_then_hits_the_cache_then_reports_failures() {
    let root = TempDir::new("cook-cli");
    let image = image::RgbaImage::from_fn(64, 64, |x, y| {
        image::Rgba([x as u8 * 4, y as u8 * 4, 0, 255])
    });
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    root.write("tex/ramp.png", png.into_inner());
    root.write(
        "tex/ramp.masset",
        "!Version 1\nTexture { !Uuid = \"01990000-0000-7000-8000-000000000001\" !Name = \"Ramp\" Source = `source:tex/ramp.png` }\n",
    );
    let r = root.to_str().unwrap();

    let (ok, text) = run(&["cook", r]);
    assert!(ok, "{text}");
    assert!(
        text.contains("1 units (1 cooked, 0 cached), 0 failed"),
        "{text}"
    );
    assert!(root.join(".marlin/build/windows.manifest").is_file());

    let (ok, text) = run(&["cook", r, "--threads", "2"]);
    assert!(ok, "{text}");
    assert!(text.contains("(0 cooked, 1 cached)"), "{text}");

    let (ok, text) = run(&["package", r, "--bulk-codec", "gdeflate"]);
    assert!(ok, "{text}");
    assert!(text.contains("1 units"), "{text}");
    let package =
        marlin_asset::Package::open(root.join(".marlin/build/windows.mpak").to_str().unwrap())
            .unwrap();
    assert_eq!(package.index().units.len(), 1);
    let (ok, text) = run(&["package", r, "--bulk-codec", "brotli"]);
    assert!(!ok && text.contains("usage"), "{text}");

    root.write(
        "tex/broken.masset",
        "!Version 1\nTexture { !Uuid = \"01990000-0000-7000-8000-000000000002\" !Name = \"Broken\" Source = `source:tex/missing.png` }\n",
    );
    let (ok, text) = run(&["cook", r]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("error: tex/broken") && text.contains("missing.png"),
        "{text}"
    );
    assert!(text.contains("1 failed"), "{text}");

    let (ok, text) = run(&["cook", r, "--platform"]);
    assert!(!ok && text.contains("usage"), "{text}");
}
