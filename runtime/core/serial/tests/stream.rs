//! Reading serialized data incrementally from a source.

use std::cell::Cell;

use marlin_reflect::{Mut, Reflect};
use marlin_serial::stream::{FileSource, Source, StreamReader, read_from};
use marlin_serial::{Error, ReadOptions, to_bytes};
use marlin_std::io::File;

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Record {
    id: u32,
    name: String,
    samples: Vec<f32>,
    tag: Option<(u8, bool)>,
}

fn records(n: u32) -> Vec<Record> {
    (0..n)
        .map(|i| Record {
            id: i,
            name: format!("record {i}"),
            samples: (0..i % 7).map(|s| s as f32).collect(),
            tag: (i % 3 == 0).then_some((i as u8, i % 2 == 0)),
        })
        .collect()
}

/// A source that hands out at most `step` bytes per read and tracks how far it got, to check
/// reads stay incremental.
struct Trickle<'a> {
    bytes: &'a [u8],
    step: usize,
    taken: &'a Cell<usize>,
}

impl Source for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let n = buf.len().min(self.step).min(self.bytes.len());
        buf[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        self.taken.set(self.taken.get() + n);
        Ok(n)
    }
}

#[test]
fn reads_a_list_one_record_at_a_time() {
    let all = records(200);
    let bytes = to_bytes(&all).unwrap();
    let taken = Cell::new(0);
    let source = Trickle {
        bytes: &bytes,
        step: 3,
        taken: &taken,
    };
    let mut reader = StreamReader::new(source).unwrap();
    assert_eq!(reader.remaining(), 200);
    assert_eq!(
        reader.header().root,
        marlin_reflect::type_info::<Vec<Record>>().id
    );
    let first: Record = reader.read_next().unwrap().unwrap();
    assert_eq!(first, all[0]);
    // Only the header and the first record have been read.
    assert!(taken.get() < bytes.len() / 50, "read {} bytes", taken.get());

    let mut target = Record {
        id: 0,
        name: String::new(),
        samples: vec![],
        tag: None,
    };
    for expected in &all[1..] {
        assert!(reader.read_into(Mut::new(&mut target)).unwrap());
        assert_eq!(&target, expected);
    }
    assert_eq!(reader.read_next::<Record>(), Ok(None));
    assert!(!reader.read_into(Mut::new(&mut target)).unwrap());
    reader.finish().unwrap();
    assert_eq!(taken.get(), bytes.len());
}

#[test]
fn arrays_sets_and_other_roots() {
    let bytes = to_bytes(&[1u16, 2, 3]).unwrap();
    let mut reader = StreamReader::new(&bytes[..]).unwrap();
    let mut got = vec![];
    while let Some(v) = reader.read_next::<u64>().unwrap() {
        got.push(v);
    }
    assert_eq!(got, [1, 2, 3]);

    let set = std::collections::BTreeSet::from([5i8, -5]);
    let bytes = to_bytes(&set).unwrap();
    let mut reader = StreamReader::new(&bytes[..]).unwrap();
    assert_eq!(reader.read_next::<i8>(), Ok(Some(-5)));

    // A root that is not a sequence.
    let bytes = to_bytes(&records(1)[0]).unwrap();
    assert!(matches!(
        StreamReader::new(&bytes[..]),
        Err(Error::Incompatible)
    ));
    // ... but reads whole with `read_from`.
    assert_eq!(
        read_from::<Record, _>(&bytes[..]),
        Ok(records(1)[0].clone())
    );
}

#[test]
fn finish_skips_and_checks_trailing_bytes() {
    let bytes = to_bytes(&records(10)).unwrap();
    let mut reader = StreamReader::new(&bytes[..]).unwrap();
    reader.read_next::<Record>().unwrap();
    let rest = reader.finish().unwrap();
    assert!(rest.is_empty());

    let mut longer = bytes.clone();
    longer.push(0);
    let reader = StreamReader::new(&longer[..]).unwrap();
    assert_eq!(reader.finish().err(), Some(Error::TrailingBytes));
}

#[test]
fn corrupt_streams_are_errors() {
    let bytes = to_bytes(&records(20)).unwrap();
    // Truncated anywhere: an error, never a panic, and never a record past the cut.
    for len in 0..bytes.len() {
        let result = (|| {
            let mut reader = StreamReader::new(&bytes[..len])?;
            while reader.read_next::<Record>()?.is_some() {}
            reader.finish().map(drop)
        })();
        assert!(result.is_err(), "len {len}");
    }
    for i in 0..bytes.len() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut corrupt = bytes.clone();
            corrupt[i] ^= flip;
            if let Ok(mut reader) = StreamReader::new(&corrupt[..]) {
                while let Ok(Some(_)) = reader.read_next::<Record>() {}
            }
        }
    }
    // A record claiming a 4 GiB string fails when the source ends, without allocating for it.
    let mut huge = to_bytes(&vec![String::from("x")]).unwrap();
    let len_at = huge.len() - 5;
    huge[len_at..len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut reader = StreamReader::new(&huge[..]).unwrap();
    assert_eq!(reader.read_next::<String>(), Err(Error::Truncated));
    // Nesting limits apply to the stream's signature and records.
    let deep = to_bytes(&vec![vec![vec![1u8]]]).unwrap();
    let options = ReadOptions { max_depth: 2 };
    assert!(StreamReader::with_options(&deep[..], &options).is_err());
    assert!(matches!(
        StreamReader::new(&b"MRLS"[..]),
        Err(Error::BadMagic)
    ));
}

/// A temporary file removed on drop.
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn reads_from_a_file() {
    let all = records(500);
    let bytes = to_bytes(&all).unwrap();
    let tmp = TempFile(
        std::env::temp_dir().join(format!("marlin-serial-stream-{}.bin", std::process::id())),
    );
    std::fs::write(&tmp.0, &bytes).unwrap();
    let file = File::open(tmp.0.to_str().unwrap()).unwrap();
    let mut reader = StreamReader::new(FileSource::new(&file)).unwrap();
    let mut count = 0;
    while let Some(record) = reader.read_next::<Record>().unwrap() {
        assert_eq!(record, all[count]);
        count += 1;
    }
    assert_eq!(count, all.len());
    let source = reader.finish().unwrap();
    assert_eq!(source.offset(), bytes.len() as u64);
}
