//! File system operations, mapping, cancellation and watching through the safe layer.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use marlin_io::fs::{self, FileKind};
use marlin_io::watch::{WatchKind, Watcher};
use marlin_io::{ErrorKind, File, IoBuffer, IoQueue, Mmap, MmapMut, OpenOptions, PathBuf};

/// A fresh directory under the system temp directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("marlin-io-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(PathBuf::from(dir.canonicalize().unwrap().to_str().unwrap()))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn create_walk_and_remove_trees() {
    let tmp = TempDir::new("tree");
    let deep = tmp.0.join("a/b/c");
    fs::create_dir_all(&deep).unwrap();
    // Idempotent.
    fs::create_dir_all(&deep).unwrap();
    marlin_io::write(deep.join("leaf.txt"), b"leaf").unwrap();
    marlin_io::write(tmp.0.join("a/top.txt"), b"top").unwrap();
    assert!(fs::exists(deep.join("leaf.txt")));
    assert!(!fs::exists(tmp.0.join("missing")));

    let listed: BTreeSet<(String, FileKind)> = fs::read_dir(tmp.0.join("a"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_owned(), e.kind)
        })
        .collect();
    assert_eq!(
        listed,
        BTreeSet::from([
            ("b".to_owned(), FileKind::Dir),
            ("top.txt".to_owned(), FileKind::File)
        ])
    );

    let walked: BTreeSet<String> = fs::walk_dir(&tmp.0)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path;
            path.as_str()[tmp.0.as_str().len() + 1..].to_owned()
        })
        .collect();
    assert_eq!(
        walked,
        BTreeSet::from(["a", "a/b", "a/b/c", "a/b/c/leaf.txt", "a/top.txt"].map(str::to_owned))
    );

    // A file where a directory is wanted.
    let error = fs::create_dir_all(tmp.0.join("a/top.txt")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);

    fs::rename(tmp.0.join("a/top.txt"), tmp.0.join("a/b/moved.txt")).unwrap();
    assert_eq!(
        marlin_io::read(tmp.0.join("a/b/moved.txt")).unwrap(),
        b"top"
    );

    fs::remove_dir_all(tmp.0.join("a")).unwrap();
    assert!(!fs::exists(tmp.0.join("a")));
}

#[test]
fn file_sync_and_metadata() {
    let tmp = TempDir::new("sync");
    let file = File::create(tmp.0.join("f")).unwrap();
    file.write_all_at(&[1; 300], 0).unwrap();
    file.sync_all().unwrap();
    file.sync_data().unwrap();
    let meta = file.metadata().unwrap();
    assert_eq!((meta.kind, meta.len), (FileKind::File, 300));
    assert_eq!(fs::metadata(tmp.0.join("f")).unwrap(), meta);
}

#[test]
fn mmap_reads_and_writes() {
    let tmp = TempDir::new("mmap");
    let path = tmp.0.join("m.bin");
    let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 256) as u8).collect();
    marlin_io::write(&path, &data).unwrap();

    let file = File::open(&path).unwrap();
    // SAFETY: nothing else touches the file during the test.
    let all = unsafe { Mmap::map_all(&file) }.unwrap();
    assert_eq!(&all[..], &data[..]);
    // SAFETY: as above.
    let part = unsafe { Mmap::map(&file, 12_345, 1000) }.unwrap();
    assert_eq!(&part[..], &data[12_345..13_345]);
    drop((all, part));

    let rw = File::with_options(&path, &OpenOptions::read_write()).unwrap();
    // SAFETY: as above.
    let mut view = unsafe { MmapMut::map(&rw, 99_990, 10) }.unwrap();
    view.copy_from_slice(b"0123456789");
    view.flush().unwrap();
    drop(view);
    assert_eq!(&marlin_io::read(&path).unwrap()[99_990..], b"0123456789");
}

#[test]
fn cancel_completes_exactly_once() {
    let tmp = TempDir::new("cancel");
    let path = tmp.0.join("c.bin");
    marlin_io::write(&path, &[5; 4096]).unwrap();
    let file = File::open(&path).unwrap();
    let mut queue = IoQueue::new(4).unwrap();
    let id = queue.read(&file, IoBuffer::new(4096), 0).unwrap();
    assert!(queue.cancel(id).unwrap());
    let done = queue.wait().unwrap().unwrap();
    assert_eq!(done.id, id);
    match done.result {
        Ok(n) => assert_eq!(n, 4096),
        Err(e) => assert_eq!(e.kind(), ErrorKind::Cancelled),
    }
    assert!(queue.poll().unwrap().is_none());
    // No longer in flight.
    assert!(!queue.cancel(id).unwrap());
}

#[test]
fn watcher_reports_normalized_relative_paths() {
    let tmp = TempDir::new("watch");
    fs::create_dir(tmp.0.join("sub")).unwrap();
    let mut watcher = Watcher::new(&tmp.0).unwrap();
    marlin_io::write(tmp.0.join("sub/x.txt"), b"x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Some(event) = watcher.wait(Some(left)).unwrap() else {
            break;
        };
        if event.kind == WatchKind::Created && event.path.as_str() == "sub/x.txt" {
            return;
        }
        seen.push(event);
    }
    panic!("no creation event; saw {seen:?}");
}
