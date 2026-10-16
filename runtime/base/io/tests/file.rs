mod common;

use common::{TempFile, pattern};
use marlin_io::{Disposition, ErrorKind, File, MAX_PATH_LEN, OpenOptions, PathBuf};

#[test]
fn sync_read_matches_std() {
    let data = pattern(100_003);
    let tmp = TempFile::with_data("sync", &data);
    let file = File::open(tmp.path()).unwrap();
    assert_eq!(file.alignment(), 1);
    assert_eq!(file.size().unwrap(), data.len() as u64);

    let mut buf = vec![0; 4096];
    for offset in [0, 1, 4095, 50_000, data.len() - 10] {
        let n = file.read_at(&mut buf, offset as u64).unwrap();
        let expected = &data[offset..data.len().min(offset + buf.len())];
        assert_eq!(&buf[..n], expected, "offset {offset}");
    }
    // At and past the end.
    assert_eq!(file.read_at(&mut buf, data.len() as u64).unwrap(), 0);
    assert_eq!(file.read_at(&mut buf, data.len() as u64 + 99).unwrap(), 0);

    let mut exact = vec![0; 20];
    let err = file
        .read_exact_at(&mut exact, data.len() as u64 - 10)
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnexpectedEof);

    assert_eq!(marlin_io::read(tmp.path()).unwrap(), data);
}

#[test]
fn read_helper_handles_empty_and_normalized_paths() {
    let tmp = TempFile::with_data("empty", b"");
    assert!(marlin_io::read(tmp.path()).unwrap().is_empty());

    // A normalized PathBuf with `/` separators, `.` and `..` resolved.
    let dir = tmp.0.parent().unwrap().to_str().unwrap();
    let name = tmp.0.file_name().unwrap().to_str().unwrap();
    let path = PathBuf::from(format!("{dir}/./sub/../{name}"));
    assert!(!path.as_str().contains('\\'));
    assert!(marlin_io::read(&path).unwrap().is_empty());
}

#[test]
fn relative_path() {
    let name = format!("marlin-io-relative-{}", std::process::id());
    std::fs::write(&name, b"relative").unwrap();
    let result = marlin_io::read(&name);
    std::fs::remove_file(&name).unwrap();
    assert_eq!(result.unwrap(), b"relative");
}

#[test]
fn write_then_read_back() {
    let tmp = TempFile::new("write");
    let data = pattern(70_000);
    marlin_io::write(tmp.path(), &data).unwrap();
    assert_eq!(std::fs::read(&tmp.0).unwrap(), data);

    let file = File::with_options(tmp.path(), &OpenOptions::read_write()).unwrap();
    file.write_all_at(b"hello", 10).unwrap();
    file.set_len(20).unwrap();
    assert_eq!(file.size().unwrap(), 20);
    drop(file);
    let back = std::fs::read(&tmp.0).unwrap();
    assert_eq!(&back[..10], &data[..10]);
    assert_eq!(&back[10..15], b"hello");
    assert_eq!(&back[15..], &data[15..20]);
}

#[test]
fn open_errors() {
    let missing = TempFile::new("missing");
    assert_eq!(
        File::open(missing.path()).err().unwrap().kind(),
        ErrorKind::NotFound
    );

    let tmp = TempFile::with_data("exists", b"x");
    let create_new = OpenOptions::read_write().disposition(Disposition::CreateNew);
    assert_eq!(
        File::with_options(tmp.path(), &create_new)
            .err()
            .unwrap()
            .kind(),
        ErrorKind::AlreadyExists
    );

    let truncate_read_only = OpenOptions::read().disposition(Disposition::CreateOrTruncate);
    assert_eq!(
        File::with_options(tmp.path(), &truncate_read_only)
            .err()
            .unwrap()
            .kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        File::open("").err().unwrap().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        File::open("a\0b").err().unwrap().kind(),
        ErrorKind::InvalidInput
    );
    let long = "x".repeat(MAX_PATH_LEN + 1);
    assert_eq!(
        File::open(long.as_str()).err().unwrap().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn read_only_file_rejects_writes() {
    let tmp = TempFile::with_data("readonly", b"abc");
    let file = File::open(tmp.path()).unwrap();
    assert!(file.write_at(b"z", 0).is_err());
}

/// Windows paths past the 260-character MAX_PATH limit work through the `\\?\` prefix.
#[cfg(windows)]
#[test]
fn long_path() {
    let mut dir = std::env::temp_dir();
    dir.push(format!("marlin-io-long-{}", std::process::id()));
    let root = dir.clone();
    while dir.as_os_str().len() < 300 {
        dir.push("a-rather-long-directory-name");
    }
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("file.bin");
    std::fs::write(&path, b"long").unwrap();

    let result = marlin_io::read(path.to_str().unwrap());
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(result.unwrap(), b"long");
}
