//! `marlin_asset::package`: a whole `.mpak` in memory, read the way `marlin_asset::Package`
//! reads one from a file: header, index, then every unit and bulk blob decoded from its
//! chunks.
#![no_main]

use marlin_asset::UNIT_ALIGN;
use marlin_asset::package::{PackageIndex, read_header};
use marlin_serial::cooked::CookedBuffer;

/// Units and blobs decoded per input: bounds the work on indexes that list many.
const MAX_ENTRIES: usize = 256;

fn run(data: &[u8]) {
    let Ok(header) = read_header(data) else {
        return;
    };
    // `Package::open`: the index must be inside the file.
    let Some(index) = header
        .index_offset
        .checked_add(header.index_size)
        .filter(|&end| end <= data.len() as u64)
        .map(|end| &data[header.index_offset as usize..end as usize])
    else {
        return;
    };
    let Ok(index) = PackageIndex::parse(header, index) else {
        return;
    };
    for record in index.units.iter().take(MAX_ENTRIES) {
        let _ = index.unit(record.unit);
        // `Package::finish`: decode the unit's chunks into a buffer of its size.
        let (chunks, range) = index.unit_read(record);
        if let Some(stored) = data.get(range.start as usize..range.end as usize) {
            let mut out = CookedBuffer::zeroed(record.size as usize, UNIT_ALIGN);
            let _ = index.decode(chunks, stored, range.start, out.as_mut_slice(), 0);
        }
        // `Package::read_blob`.
        for blob in 0..record.blob_count.min(MAX_ENTRIES as u32) {
            let b = index.blobs[(record.first_blob + blob) as usize];
            let chunks = b.first_chunk as usize..(b.first_chunk + b.chunk_count) as usize;
            let range = index.file_range(chunks.clone());
            let Some(stored) = data.get(range.start as usize..range.end as usize) else {
                continue;
            };
            let mut out = CookedBuffer::zeroed(b.size as usize, UNIT_ALIGN);
            let _ = index.decode(
                chunks,
                stored,
                range.start,
                out.as_mut_slice(),
                b.unit_offset,
            );
        }
    }
    for object in index.objects.iter().take(MAX_ENTRIES) {
        let _ = index.object_unit(object.uuid);
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
