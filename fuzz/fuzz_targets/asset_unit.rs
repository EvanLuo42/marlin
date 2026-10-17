//! `marlin_asset::cooked::UnitView`: a cooked unit read in place from any bytes, every table
//! entry, payload and bulk blob visited.
#![no_main]

use marlin_asset::cooked::UnitView;

/// Entries visited per table: bounds the work on headers that claim huge counts.
const MAX_ENTRIES: usize = 4096;

fn run(data: &[u8]) {
    let Ok(view) = UnitView::new(data) else {
        return;
    };
    let _ = view.header();
    for i in 0..view.object_count().min(MAX_ENTRIES) {
        let Ok(object) = view.object(i) else {
            continue;
        };
        let _ = view.payload(&object);
        for blob in view.object_bulk(&object).take(MAX_ENTRIES) {
            let _ = view.bulk_bytes(&blob);
        }
        let _ = view.find(object.uuid);
    }
    for i in 0..view.import_count().min(MAX_ENTRIES) {
        let _ = view.import(i);
    }
    for i in 0..view.bulk_count().min(MAX_ENTRIES) {
        if let Ok(blob) = view.bulk(i) {
            let _ = view.bulk_bytes(&blob);
        }
    }
    let _ = view.find(marlin_asset::Uuid::from_u128(1));
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
