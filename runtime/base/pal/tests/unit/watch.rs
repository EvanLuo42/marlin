//! Directory watching. Events are hints the OS may coalesce, so the tests look for the expected
//! event among those that arrive within a deadline instead of asserting an exact sequence.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use marlin_pal::ErrorKind;
use marlin_pal::watch::{WatchKind, Watcher};
use marlin_test_support::{TempDir, assert_duration_in, wait_until};

/// A fresh temp directory and its real path: FSEvents reports real paths, so /tmp ->
/// /private/tmp style links are resolved up front. Keep the [`TempDir`] alive while using it.
fn temp_dir(name: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new(&format!("pal-watch-{name}"));
    let path = dir.canonicalize().unwrap();
    (dir, path)
}

/// Waits up to two seconds for an event of `kind` on `path`, returning everything seen.
fn expect(watcher: &mut Watcher, kind: WatchKind, path: &str) -> Vec<(WatchKind, String)> {
    let mut seen = Vec::new();
    let mut buf = [0u8; 256];
    wait_until(
        Duration::from_secs(2),
        &format!("no {kind:?} for {path:?}"),
        || {
            let Some(event) = watcher
                .next(&mut buf, Some(Duration::from_millis(100)))
                .unwrap()
            else {
                return false;
            };
            let name = String::from_utf8(buf[..event.path_len].to_vec()).unwrap();
            let hit = event.kind == kind && name == path;
            if !hit {
                // Shown with the failure if the expected event never comes.
                eprintln!("other watch event: {:?} {name:?}", event.kind);
            }
            seen.push((event.kind, name));
            hit
        },
    );
    seen
}

#[test]
fn reports_changes_with_relative_paths() {
    let (_tmp, root) = temp_dir("events");
    let mut watcher = Watcher::new(root.to_str().unwrap()).unwrap();

    // Nothing has happened yet.
    assert!(
        watcher
            .next(&mut [0; 64], Some(Duration::ZERO))
            .unwrap()
            .is_none()
    );

    std::fs::write(root.join("a.txt"), b"1").unwrap();
    expect(&mut watcher, WatchKind::Created, "a.txt");

    std::fs::create_dir(root.join("sub")).unwrap();
    expect(&mut watcher, WatchKind::Created, "sub");
    // Give the Linux backend a moment to read the directory's creation and watch it.
    let _ = watcher.next(&mut [0; 64], Some(Duration::from_millis(100)));
    std::fs::write(root.join("sub/b.txt"), b"2").unwrap();
    expect(&mut watcher, WatchKind::Created, "sub/b.txt");

    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("a.txt"))
            .unwrap();
        file.write_all(b"more").unwrap();
    }
    expect(&mut watcher, WatchKind::Modified, "a.txt");

    std::fs::rename(root.join("a.txt"), root.join("c.txt")).unwrap();
    expect(&mut watcher, WatchKind::RenamedTo, "c.txt");

    std::fs::remove_file(root.join("c.txt")).unwrap();
    expect(&mut watcher, WatchKind::Removed, "c.txt");
}

#[test]
fn timeout_returns_none() {
    let (_tmp, root) = temp_dir("timeout");
    let mut watcher = Watcher::new(root.to_str().unwrap()).unwrap();
    let start = Instant::now();
    assert!(
        watcher
            .next(&mut [0; 64], Some(Duration::from_millis(100)))
            .unwrap()
            .is_none()
    );
    assert_duration_in(start.elapsed(), Duration::from_millis(90)..);
}

#[test]
fn reports_full_path_length() {
    let (_tmp, root) = temp_dir("length");
    let mut watcher = Watcher::new(root.to_str().unwrap()).unwrap();
    std::fs::write(root.join("a-rather-long-name.bin"), b"").unwrap();
    let mut small = [0u8; 4];
    let event = watcher
        .next(&mut small, Some(Duration::from_secs(2)))
        .unwrap()
        .unwrap();
    assert_eq!(event.path_len, "a-rather-long-name.bin".len());
    assert_eq!(&small, b"a-ra");
}

#[test]
fn missing_root_fails() {
    let (_tmp, root) = temp_dir("missing");
    let missing = root.join("nope");
    let error = Watcher::new(missing.to_str().unwrap()).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::NotFound);
}
