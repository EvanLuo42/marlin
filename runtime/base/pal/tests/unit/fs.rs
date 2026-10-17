//! Path operations, metadata, directory listing and mapping.

use std::collections::BTreeMap;

use marlin_math::Rng;
use marlin_pal::ErrorKind;
use marlin_pal::file::{Disposition, File, OpenOptions};
use marlin_pal::fs::{self, FileKind, Mapping, ReadDir};
use marlin_test_support::TempDir;

/// `name` in `dir`, as the UTF-8 path the PAL takes.
fn at(dir: &TempDir, name: &str) -> String {
    dir.join(name).to_str().unwrap().to_owned()
}

fn create(path: &str, data: &[u8]) -> File {
    let file = File::open(
        path,
        &OpenOptions::read_write().disposition(Disposition::CreateOrTruncate),
    )
    .unwrap();
    assert_eq!(file.write_at(data, 0).unwrap(), data.len());
    file
}

fn list(path: &str) -> BTreeMap<String, FileKind> {
    let mut dir = ReadDir::open(path).unwrap();
    let mut entries = BTreeMap::new();
    let mut name = [0u8; 1024];
    while let Some(entry) = dir.next(&mut name).unwrap() {
        entries.insert(
            String::from_utf8(name[..entry.name_len].to_vec()).unwrap(),
            entry.kind,
        );
    }
    entries
}

#[test]
fn metadata_of_files_and_dirs() {
    let tmp = TempDir::new("pal-fs-metadata");
    let path = at(&tmp, "a.bin");
    let file = create(&path, &[7; 1000]);

    let meta = fs::metadata(&path).unwrap();
    assert_eq!(meta.kind, FileKind::File);
    assert_eq!(meta.len, 1000);
    assert!(!meta.readonly);
    assert_eq!(file.metadata().unwrap(), meta);

    let std_modified = std::fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    assert_eq!(meta.modified.0 as u64, std_modified.as_secs());
    assert_eq!(meta.modified.1, std_modified.subsec_nanos());

    let dir = fs::metadata(tmp.to_str().unwrap()).unwrap();
    assert_eq!(dir.kind, FileKind::Dir);

    let missing = fs::metadata(&at(&tmp, "missing")).unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::NotFound);
}

#[test]
fn create_remove_rename() {
    let tmp = TempDir::new("pal-fs-ops");
    let dir = at(&tmp, "sub");
    fs::create_dir(&dir).unwrap();
    assert_eq!(
        fs::create_dir(&dir).unwrap_err().kind(),
        ErrorKind::AlreadyExists
    );

    let a = at(&tmp, "sub/a.txt");
    drop(create(&a, b"first"));
    assert_eq!(
        fs::remove_dir(&dir).unwrap_err().kind(),
        ErrorKind::DirectoryNotEmpty
    );

    // Rename onto an existing file replaces it.
    let b = at(&tmp, "sub/b.txt");
    drop(create(&b, b"second, longer"));
    fs::rename(&a, &b).unwrap();
    assert_eq!(std::fs::read(&b).unwrap(), b"first");
    assert_eq!(fs::metadata(&a).unwrap_err().kind(), ErrorKind::NotFound);

    fs::remove_file(&b).unwrap();
    assert_eq!(fs::remove_file(&b).unwrap_err().kind(), ErrorKind::NotFound);
    fs::remove_dir(&dir).unwrap();
    assert_eq!(fs::metadata(&dir).unwrap_err().kind(), ErrorKind::NotFound);

    for bad in ["", "a\0b"] {
        assert_eq!(
            fs::create_dir(bad).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }
}

#[test]
fn read_dir_lists_entries() {
    let tmp = TempDir::new("pal-fs-list");
    drop(create(&at(&tmp, "one"), b"1"));
    drop(create(&at(&tmp, "a-name-longer-than-eight-bytes"), b"2"));
    drop(create(&at(&tmp, "\u{e9}t\u{e9}"), b"3"));
    fs::create_dir(&at(&tmp, "dir")).unwrap();

    let expected: BTreeMap<String, FileKind> = [
        ("one", FileKind::File),
        ("a-name-longer-than-eight-bytes", FileKind::File),
        ("\u{e9}t\u{e9}", FileKind::File),
        ("dir", FileKind::Dir),
    ]
    .into_iter()
    .map(|(n, k)| (n.to_owned(), k))
    .collect();
    assert_eq!(list(tmp.to_str().unwrap()), expected);
    assert_eq!(list(&at(&tmp, "dir")), BTreeMap::new());

    // A trailing separator is accepted.
    let with_slash = format!("{}/", tmp.to_str().unwrap());
    assert_eq!(list(&with_slash).len(), 4);

    assert_eq!(
        ReadDir::open(&at(&tmp, "one")).err().unwrap().kind(),
        ErrorKind::NotADirectory
    );
    assert_eq!(
        ReadDir::open(&at(&tmp, "missing")).err().unwrap().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn sync_flushes() {
    let tmp = TempDir::new("pal-fs-sync");
    let file = create(&at(&tmp, "f"), b"data");
    file.sync_all().unwrap();
    file.sync_data().unwrap();
}

#[test]
fn map_reads_and_writes_through() {
    let tmp = TempDir::new("pal-fs-map");
    let path = at(&tmp, "m.bin");
    let mut data = vec![0u8; 200_000];
    Rng::new(1).fill_bytes(&mut data);
    let file = create(&path, &data);

    // Unaligned offset into the middle of the file.
    let offset = 70_001;
    let len = 50_000;
    let view = Mapping::new(&file, offset, len, false).unwrap();
    assert_eq!(view.len(), len);
    // SAFETY: nothing writes the file while the view is read.
    let mapped = unsafe { std::slice::from_raw_parts(view.as_ptr(), len) };
    assert_eq!(mapped, &data[offset as usize..offset as usize + len]);
    drop(view);

    let rw = Mapping::new(&file, 4096, 10, true).unwrap();
    // SAFETY: the view is ours to write.
    unsafe {
        rw.as_ptr()
            .copy_from_nonoverlapping(b"0123456789".as_ptr(), 10)
    };
    rw.flush().unwrap();
    drop(rw);
    let mut back = [0u8; 10];
    file.read_at(&mut back, 4096).unwrap();
    assert_eq!(&back, b"0123456789");

    for (offset, len) in [(0, 0), (199_999, 2), (u64::MAX, 1)] {
        let error = Mapping::new(&file, offset, len, false).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }
}

#[test]
fn read_dir_reports_full_name_length() {
    let tmp = TempDir::new("pal-fs-truncate");
    drop(create(&at(&tmp, "a-long-file-name"), b""));
    let mut dir = ReadDir::open(tmp.to_str().unwrap()).unwrap();
    let mut small = [0u8; 4];
    let entry = dir.next(&mut small).unwrap().unwrap();
    assert_eq!(entry.name_len, "a-long-file-name".len());
    assert_eq!(&small, b"a-lo");
    assert!(dir.next(&mut small).unwrap().is_none());
}
