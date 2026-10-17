//! `marlin_text::loc::Message`: parsing, walking and formatting any message text.
//!
//! Input: an 8-byte little-endian `i64` (the numeric arguments), a locale byte, then the
//! message source as UTF-8. Formatting runs on unchecked messages too: a cooked string table's
//! messages reach `format` without a fresh parse.
#![no_main]

use marlin_text::loc::{Arg, Date, Locale, Message, Time};

const LOCALES: [&str; 8] = ["en", "ru", "ar", "pl", "fr", "ja", "hi", "pt-PT"];

fn run(data: &[u8]) {
    let Some(([n0, n1, n2, n3, n4, n5, n6, n7, locale], rest)) = marlin_fuzz::split::<9>(data)
    else {
        return;
    };
    let Ok(source) = std::str::from_utf8(rest) else {
        return;
    };
    let n = i64::from_le_bytes([n0, n1, n2, n3, n4, n5, n6, n7]);
    let locale = Locale::parse(LOCALES[locale as usize % LOCALES.len()]).unwrap();
    let date = Date::new(2026, 10, 17).unwrap();
    let time = Time::new(23, 59, 1).unwrap();
    let args = [
        Arg::new("n", n),
        Arg::new("count", n as u64),
        Arg::new("x", n as f64 / 7.0),
        Arg::new("f", f32::from_bits(n as u32)),
        Arg::new("name", source),
        Arg::new("d", date),
        Arg::new("t", time),
    ];
    let checked = Message::parse(source);
    if let Ok(message) = checked {
        let _ = message.walk(|_| {});
        let _ = message.format_to_string(&locale, &args);
        let _ = message.format_to_string(&locale, &[]);
        let _ = message.to_string();
    }
    let _ = Message::new_unchecked(source).format_to_string(&locale, &args);
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
