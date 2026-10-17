//! Directory watching. Events are hints the OS may coalesce, so the tests look for the expected
//! event among those that arrive within a deadline instead of asserting an exact sequence.

use std::time::{Duration, Instant};

use marlin_pal::ErrorKind;
use marlin_pal::watch::{WatchKind, Watcher};

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("marlin-pal-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        // FSEvents reports real paths; resolve /tmp -> /private/tmp style links up front.
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Waits up to two seconds for an event of `kind` on `path`, returning everything seen.
fn expect(watcher: &mut Watcher, kind: WatchKind, path: &str) -> Vec<(WatchKind, String)> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut seen = Vec::new();
    let mut buf = [0u8; 256];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Some(event) = watcher.next(&mut buf, Some(left)).unwrap() else {
            break;
        };
        let name = String::from_utf8(buf[..event.path_len].to_vec()).unwrap();
        let hit = event.kind == kind && name == path;
        seen.push((event.kind, name));
        if hit {
            return seen;
        }
    }
    panic!("no {kind:?} for {path:?}; saw {seen:?}");
}

#[test]
fn reports_changes_with_relative_paths() {
    let tmp = TempDir::new("events");
    let mut watcher = Watcher::new(tmp.0.to_str().unwrap()).unwrap();

    // Nothing has happened yet.
    assert!(
        watcher
            .next(&mut [0; 64], Some(Duration::ZERO))
            .unwrap()
            .is_none()
    );

    std::fs::write(tmp.0.join("a.txt"), b"1").unwrap();
    expect(&mut watcher, WatchKind::Created, "a.txt");

    std::fs::create_dir(tmp.0.join("sub")).unwrap();
    expect(&mut watcher, WatchKind::Created, "sub");
    // Give the Linux backend a moment to read the directory's creation and watch it.
    let _ = watcher.next(&mut [0; 64], Some(Duration::from_millis(100)));
    std::fs::write(tmp.0.join("sub/b.txt"), b"2").unwrap();
    expect(&mut watcher, WatchKind::Created, "sub/b.txt");

    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.0.join("a.txt"))
            .unwrap();
        file.write_all(b"more").unwrap();
    }
    expect(&mut watcher, WatchKind::Modified, "a.txt");

    std::fs::rename(tmp.0.join("a.txt"), tmp.0.join("c.txt")).unwrap();
    expect(&mut watcher, WatchKind::RenamedTo, "c.txt");

    std::fs::remove_file(tmp.0.join("c.txt")).unwrap();
    expect(&mut watcher, WatchKind::Removed, "c.txt");
}

#[test]
fn timeout_returns_none() {
    let tmp = TempDir::new("timeout");
    let mut watcher = Watcher::new(tmp.0.to_str().unwrap()).unwrap();
    let start = Instant::now();
    assert!(
        watcher
            .next(&mut [0; 64], Some(Duration::from_millis(100)))
            .unwrap()
            .is_none()
    );
    assert!(start.elapsed() >= Duration::from_millis(90));
}

#[test]
fn reports_full_path_length() {
    let tmp = TempDir::new("length");
    let mut watcher = Watcher::new(tmp.0.to_str().unwrap()).unwrap();
    std::fs::write(tmp.0.join("a-rather-long-name.bin"), b"").unwrap();
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
    let tmp = TempDir::new("missing");
    let missing = tmp.0.join("nope");
    let error = Watcher::new(missing.to_str().unwrap()).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::NotFound);
}
