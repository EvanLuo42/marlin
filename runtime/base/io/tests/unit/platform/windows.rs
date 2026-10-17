//! Windows-only behavior: long paths, junctions and unbuffered reads.

use std::collections::BTreeSet;

use marlin_io::fs::{self, FileKind};
use marlin_io::{ErrorKind, File, IoBuffer, PathBuf};
use marlin_test_support::TempDir;

use crate::queue::{queues, read_all_chunks};
use crate::support::{Utf8, pattern, temp_dir, with_data};

/// Windows paths past the 260-character MAX_PATH limit work through the `\?\` prefix.
#[test]
fn long_path() {
    let root = TempDir::new("io-long");
    let mut dir = root.to_path_buf();
    while dir.as_os_str().len() < 300 {
        dir.push("a-rather-long-directory-name");
    }
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("file.bin");
    std::fs::write(&path, b"long").unwrap();

    let result = marlin_io::read(path.utf8());
    assert_eq!(result.unwrap(), b"long");
}

/// Junctions are directory links any user can make (`mklink /J`, no privilege needed); listing
/// must report them as links so walking and removing never go through them.
#[test]
fn junctions_are_links() {
    let (_tmp, root) = temp_dir("junction");
    let target = root.join("target");
    let tree = root.join("tree");
    fs::create_dir_all(tree.join("sub")).unwrap();
    fs::create_dir(&target).unwrap();
    marlin_io::write(target.join("keep.txt"), b"keep").unwrap();
    let junction = |link: &PathBuf, to: &PathBuf| {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.as_str().replace('/', "\\"))
            .arg(to.as_str().replace('/', "\\"))
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");
    };
    junction(&tree.join("out"), &target);
    // A cycle back to the tree's root.
    junction(&tree.join("sub/up"), &tree);

    let kinds: BTreeSet<(String, FileKind)> = fs::read_dir(&tree)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_owned(), e.kind)
        })
        .collect();
    assert_eq!(
        kinds,
        BTreeSet::from([
            ("out".to_owned(), FileKind::Symlink),
            ("sub".to_owned(), FileKind::Dir)
        ])
    );
    // Metadata follows the junction.
    assert_eq!(fs::metadata(tree.join("out")).unwrap().kind, FileKind::Dir);

    // The walk ends: neither junction is entered.
    let walked: BTreeSet<String> = fs::walk_dir(&tree)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path;
            path.as_str()[tree.as_str().len() + 1..].to_owned()
        })
        .collect();
    assert_eq!(
        walked,
        BTreeSet::from(["out", "sub", "sub/up"].map(str::to_owned))
    );

    // Removing the tree removes the junctions, not what they point at.
    fs::remove_dir_all(&tree).unwrap();
    assert!(!fs::exists(&tree));
    assert_eq!(marlin_io::read(target.join("keep.txt")).unwrap(), b"keep");
}

/// Unbuffered reads need sector-aligned buffers, offsets and lengths, which `IoBuffer::for_file`
/// provides. The last chunk reads past the end and comes back short.
#[test]
fn unbuffered_aligned_reads() {
    let data = pattern((2 << 20) + 1000);
    let tmp = with_data("queue-unbuffered", &data);
    let file =
        File::with_options(tmp.utf8(), &marlin_io::OpenOptions::read().unbuffered(true)).unwrap();
    let align = file.alignment();
    assert!(align >= 512 && align.is_power_of_two(), "{align}");

    for mut queue in queues(32) {
        read_all_chunks(&mut queue, &file, &data, 64 * 1024);

        // Misaligned requests are refused up front, handing the buffer back.
        let err = queue.read(&file, IoBuffer::new(100), 0).unwrap_err();
        assert_eq!(err.error.kind(), ErrorKind::InvalidInput);
        assert_eq!(err.buffer.len(), 100);
        let err = queue
            .read(&file, IoBuffer::for_file(&file, align), 1)
            .unwrap_err();
        assert_eq!(err.error.kind(), ErrorKind::InvalidInput);
    }

    // The sync path has the same rule.
    let mut buf = IoBuffer::for_file(&file, align);
    assert_eq!(file.read_at(&mut buf, 0).unwrap(), align);
    assert_eq!(&buf[..], &data[..align]);
    assert_eq!(
        file.read_at(&mut buf[..align / 2], 0).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}
