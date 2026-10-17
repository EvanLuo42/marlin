//! Localization: locales, plural rules, number and date formatting, messages and string
//! tables.
//!
//! - [`Locale`]: BCP 47 language-script-region (`pt-BR`, `zh-Hans-CN`) with a
//!   [fallback chain](Locale::fallback_chain).
//! - [`PluralRules`]: CLDR cardinal plural categories (zero/one/two/few/many/other) for en, de,
//!   nl, sv, da, nb, fi, es, it, pt, pt-PT, fr, ru, uk, pl, cs, ar, he, ja, ko, zh, tr and hi.
//! - [`NumberFormat`] and [`DateFormat`]: separators, grouping (Indian 3;2 too), digits, and
//!   short date, long date and time patterns for the same locales.
//! - [`Message`]: `{name}` placeholders with typed [`Arg`]s and
//!   `{count, plural, one {# item} other {# items}}` selection; see [`message`] for the
//!   grammar.
//! - [`StringTable`]: a cooked key → message table read in place, built by
//!   [`StringTableBuilder`] in the cooker; [`Catalog`] looks keys up through fallback tables.
//!
//! The locale data (plural rules, symbols, patterns, month names) is hand-encoded after CLDR,
//! not generated: see each type for what it covers. Everything formats into a `fmt::Write`.
//!
//! ```
//! use marlin_text::loc::{Arg, Catalog, Locale, StringTable, StringTableBuilder};
//!
//! let mut ru = StringTableBuilder::new(Locale::parse("ru").unwrap());
//! ru.insert("files", "{n, plural, one {# файл} few {# файла} many {# файлов} other {# файла}}")?;
//! let mut en = StringTableBuilder::new(Locale::parse("en").unwrap());
//! en.insert("files", "{n, plural, one {# file} other {# files}}")?;
//! en.insert("quit", "Quit")?;
//! let (ru, en) = (ru.build()?, en.build()?);
//! let tables = [StringTable::new(&ru).unwrap(), StringTable::new(&en).unwrap()];
//! let catalog = Catalog::new(&tables);
//! assert_eq!(catalog.format_to_string("files", &[Arg::new("n", 22)]).unwrap(), "22 файла");
//! assert_eq!(catalog.format_to_string("files", &[Arg::new("n", 5)]).unwrap(), "5 файлов");
//! assert_eq!(catalog.format_to_string("quit", &[]).unwrap(), "Quit");
//! # Ok::<(), marlin_text::loc::BuildError>(())
//! ```

mod date;
mod locale;
pub mod message;
mod number;
mod plural;
mod table;

pub use date::{Date, DateFormat, DateStyle, Time};
pub use locale::{FallbackChain, Locale, LocaleError};
pub use message::{
    Arg, FormatError, Message, MessageError, MessageErrorKind, Part, Selector, Value,
};
pub use number::NumberFormat;
pub use plural::{PluralCategory, PluralOperands, PluralRules};
pub use table::{
    BuildError, Catalog, STRING_TABLE_TYPE_ID, StringTable, StringTableBuilder, TableError,
};
