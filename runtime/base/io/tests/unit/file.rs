use marlin_io::{Disposition, ErrorKind, File, MAX_PATH_LEN, OpenOptions, PathBuf};
use marlin_test_support::TempFile;

use crate::support::{Utf8, pattern, with_data};

#[test]
fn sync_read_matches_std() {
    let data = pattern(100_003);
    let tmp = with_data("sync", &data);
    let file = File::open(tmp.utf8()).unwrap();
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

    assert_eq!(marlin_io::read(tmp.utf8()).unwrap(), data);
}

#[test]
fn read_helper_handles_empty_and_normalized_paths() {
    let tmp = with_data("empty", b"");
    assert!(marlin_io::read(tmp.utf8()).unwrap().is_empty());

    // A normalized PathBuf with `/` separators, `.` and `..` resolved.
    let dir = tmp.parent().unwrap().to_str().unwrap();
    let name = tmp.file_name().unwrap().to_str().unwrap();
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
    let tmp = TempFile::new("io-write");
    let data = pattern(70_000);
    marlin_io::write(tmp.utf8(), &data).unwrap();
    assert_eq!(std::fs::read(&tmp).unwrap(), data);

    let file = File::with_options(tmp.utf8(), &OpenOptions::read_write()).unwrap();
    file.write_all_at(b"hello", 10).unwrap();
    file.set_len(20).unwrap();
    assert_eq!(file.size().unwrap(), 20);
    drop(file);
    let back = std::fs::read(&tmp).unwrap();
    assert_eq!(&back[..10], &data[..10]);
    assert_eq!(&back[10..15], b"hello");
    assert_eq!(&back[15..], &data[15..20]);
}

#[test]
fn open_errors() {
    let missing = TempFile::new("io-missing");
    assert_eq!(
        File::open(missing.utf8()).err().unwrap().kind(),
        ErrorKind::NotFound
    );

    let tmp = with_data("exists", b"x");
    let create_new = OpenOptions::read_write().disposition(Disposition::CreateNew);
    assert_eq!(
        File::with_options(tmp.utf8(), &create_new)
            .err()
            .unwrap()
            .kind(),
        ErrorKind::AlreadyExists
    );

    let truncate_read_only = OpenOptions::read().disposition(Disposition::CreateOrTruncate);
    assert_eq!(
        File::with_options(tmp.utf8(), &truncate_read_only)
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
    let tmp = with_data("readonly", b"abc");
    let file = File::open(tmp.utf8()).unwrap();
    assert!(file.write_at(b"z", 0).is_err());
}
