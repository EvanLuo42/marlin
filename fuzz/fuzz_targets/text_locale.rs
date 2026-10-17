//! `marlin_text::loc::Locale::parse` on any tag, and everything derived from a parsed locale.
#![no_main]

use marlin_text::loc::{DateFormat, Locale, NumberFormat, PluralOperands, PluralRules};

fn run(data: &[u8]) {
    let Ok(tag) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(locale) = Locale::parse(tag) else {
        return;
    };
    let _ = locale.to_string();
    let _ = (locale.language(), locale.script(), locale.region());
    let _ = locale.without_region();
    for parent in locale.fallback_chain().take(16) {
        let _ = parent.to_string();
    }
    let _ = PluralRules::for_locale(&locale).select(&PluralOperands::from(7i64));
    let mut out = String::new();
    let _ = NumberFormat::for_locale(&locale).format_int(-1_234_567, &mut out);
    let _ = DateFormat::for_locale(&locale);
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
