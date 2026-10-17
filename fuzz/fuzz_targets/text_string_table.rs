//! `marlin_text::loc::StringTable`: a cooked string table read in place from any bytes, then
//! every key looked up and formatted, alone and through a `Catalog`.
#![no_main]

use marlin_text::loc::{Arg, Catalog, StringTable};

fn run(data: &[u8]) {
    let Ok(table) = StringTable::new(data) else {
        return;
    };
    let _ = (table.locale(), table.len(), table.is_empty());
    let args = [Arg::new("n", 3), Arg::new("name", "x")];
    let mut out = String::new();
    for (key, message) in table.iter().take(4096) {
        let _ = table.get(key);
        out.clear();
        let _ = table.format(key, &args, &mut out);
        let _ = message.len();
    }
    let _ = table.get("");
    let _ = table.get("missing.key");
    let tables = [table];
    let catalog = Catalog::new(&tables);
    let _ = catalog.format_to_string("missing.key", &args);
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
