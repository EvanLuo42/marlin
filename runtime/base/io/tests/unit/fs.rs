//! File system operations, mapping, cancellation and watching through the safe layer.

use std::collections::BTreeSet;
use std::time::Duration;

use marlin_io::fs::{self, FileKind};
use marlin_io::watch::{WatchKind, Watcher};
use marlin_io::{ErrorKind, File, IoBuffer, IoQueue, Mmap, MmapMut, OpenOptions};
use marlin_test_support::wait_until;

use crate::support::{pattern, temp_dir};

#[test]
fn create_walk_and_remove_trees() {
    let (_tmp, root) = temp_dir("tree");
    let deep = root.join("a/b/c");
    fs::create_dir_all(&deep).unwrap();
    // Idempotent.
    fs::create_dir_all(&deep).unwrap();
    marlin_io::write(deep.join("leaf.txt"), b"leaf").unwrap();
    marlin_io::write(root.join("a/top.txt"), b"top").unwrap();
    assert!(fs::exists(deep.join("leaf.txt")));
    assert!(!fs::exists(root.join("missing")));

    let listed: BTreeSet<(String, FileKind)> = fs::read_dir(root.join("a"))
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

    let walked: BTreeSet<String> = fs::walk_dir(&root)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path;
            path.as_str()[root.as_str().len() + 1..].to_owned()
        })
        .collect();
    assert_eq!(
        walked,
        BTreeSet::from(["a", "a/b", "a/b/c", "a/b/c/leaf.txt", "a/top.txt"].map(str::to_owned))
    );

    // A file where a directory is wanted.
    let error = fs::create_dir_all(root.join("a/top.txt")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);

    fs::rename(root.join("a/top.txt"), root.join("a/b/moved.txt")).unwrap();
    assert_eq!(marlin_io::read(root.join("a/b/moved.txt")).unwrap(), b"top");

    fs::remove_dir_all(root.join("a")).unwrap();
    assert!(!fs::exists(root.join("a")));
}

#[test]
fn long_names_are_listed() {
    let (_tmp, root) = temp_dir("long-names");
    // The longest names the file system takes: 255 bytes on Linux, 255 UTF-16 units elsewhere
    // (765 UTF-8 bytes of CJK), both longer than a small first guess at the buffer size.
    let count = if cfg!(marlin_platform = "linux") {
        85
    } else {
        255
    };
    let long: String = core::iter::repeat_n('名', count).collect();
    for name in [&*long, "short"] {
        marlin_io::write(root.join(name), b"x").unwrap();
    }
    let listed: BTreeSet<String> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_owned())
        .collect();
    assert_eq!(listed, BTreeSet::from([long, "short".to_owned()]));
}

#[test]
fn file_sync_and_metadata() {
    let (_tmp, root) = temp_dir("sync");
    let file = File::create(root.join("f")).unwrap();
    file.write_all_at(&[1; 300], 0).unwrap();
    file.sync_all().unwrap();
    file.sync_data().unwrap();
    let meta = file.metadata().unwrap();
    assert_eq!((meta.kind, meta.len), (FileKind::File, 300));
    assert_eq!(fs::metadata(root.join("f")).unwrap(), meta);
}

#[test]
fn mmap_reads_and_writes() {
    let (_tmp, root) = temp_dir("mmap");
    let path = root.join("m.bin");
    let data = pattern(100_000);
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
    let (_tmp, root) = temp_dir("cancel");
    let path = root.join("c.bin");
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
    let (_tmp, root) = temp_dir("watch");
    fs::create_dir(root.join("sub")).unwrap();
    let mut watcher = Watcher::new(&root).unwrap();
    marlin_io::write(root.join("sub/x.txt"), b"x").unwrap();
    wait_until(
        Duration::from_secs(2),
        "a creation event for sub/x.txt",
        || {
            let Some(event) = watcher.wait(Some(Duration::from_millis(100))).unwrap() else {
                return false;
            };
            let hit = event.kind == WatchKind::Created && event.path.as_str() == "sub/x.txt";
            if !hit {
                // Shown with the failure if the expected event never comes.
                eprintln!("other watch event: {event:?}");
            }
            hit
        },
    );
}
