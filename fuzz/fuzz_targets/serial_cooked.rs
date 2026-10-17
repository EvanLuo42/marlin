//! `marlin_serial::cooked::CookedView`: a cooked buffer with a header, read in place, every
//! relative pointer and slice followed.
#![no_main]

use marlin_fuzz::{CookedItem, CookedRoot};
use marlin_serial::cooked::{CookedBuffer, CookedView};

fn run(data: &[u8]) {
    // Loaded the way the runtime loads cooked data: copied to an aligned buffer.
    let buffer = CookedBuffer::copy_from(data, 16);
    let headerless = CookedView::new(buffer.as_slice());
    let _ = headerless.root::<CookedItem>();
    let Ok(view) = CookedView::with_header::<CookedRoot>(buffer.as_slice()) else {
        return;
    };
    let _ = view.header();
    let Ok(root) = view.root::<CookedRoot>() else {
        return;
    };
    let _ = view.str(&root.name);
    if let Ok(items) = view.slice(&root.items) {
        for item in items.iter().take(4096) {
            let _ = view.str(&item.label);
            let _ = view.deref(&item.next);
        }
    }
    // A list that may loop: follow a bounded number of links.
    let mut next = view.deref(&root.first).ok().flatten();
    for _ in 0..1024 {
        let Some(item) = next else { break };
        let _ = view.slice(&item.label);
        next = view.deref(&item.next).ok().flatten();
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
