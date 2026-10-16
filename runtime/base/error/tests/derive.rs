use core::error::Error as _;
use core::fmt;

use marlin_error::{Error, ErrorKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug, ErrorKind)]
enum Kind {
    #[error("not found")]
    NotFound,
    #[error("device lost")]
    DeviceLost,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
#[error(code = "os error {}")]
struct OsError {
    kind: Kind,
    raw: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
#[error(code = "{:#010x}")]
struct ApiError {
    kind: Kind,
    raw: i32,
}

#[test]
fn kind_as_str_is_const() {
    const TEXT: &str = Kind::DeviceLost.as_str();
    assert_eq!(TEXT, "device lost");
    assert_eq!(Kind::NotFound.to_string(), "not found");
}

#[test]
fn code_error_shows_raw_only_when_nonzero() {
    let bare = OsError {
        kind: Kind::NotFound,
        raw: 0,
    };
    let coded = OsError {
        kind: Kind::NotFound,
        raw: 2,
    };
    assert_eq!(bare.to_string(), "not found");
    assert_eq!(coded.to_string(), "not found (os error 2)");
    assert!(coded.source().is_none());
}

#[test]
fn code_error_hex_shows_negative_codes_as_unsigned() {
    let e = ApiError {
        kind: Kind::DeviceLost,
        raw: 0x887A_0005_u32 as i32,
    };
    assert_eq!(e.to_string(), "device lost (0x887a0005)");
}

#[derive(Debug, Error)]
#[error("unit")]
struct Unit;

#[derive(Debug, Error)]
#[error("{name} at line {line:>4}")]
struct Named {
    name: &'static str,
    line: u32,
}

#[derive(Debug, Error)]
#[error("{0} of {1}, {0}")]
struct Tuple(u32, u32);

#[test]
fn struct_messages_refer_to_fields() {
    assert_eq!(Unit.to_string(), "unit");
    let named = Named {
        name: "a.toml",
        line: 7,
    };
    assert_eq!(named.to_string(), "a.toml at line    7");
    assert_eq!(Tuple(3, 9).to_string(), "3 of 9, 3");
}

const LIMIT: usize = 16;

#[derive(Debug, Error)]
enum Args {
    #[error("{} items, limit {}", count * 2, LIMIT)]
    TooMany { count: usize },
    #[error("{len} > {limit}", limit = LIMIT)]
    Named { len: usize },
    #[error("{_0:?} is {}", _1 + 1)]
    Tuple(&'static str, u32),
    #[error("{:>width$}|", "x")]
    Width { width: usize },
    #[error("{{literal}} braces, {0}}}")]
    Braces(u8),
    #[error("line\nbreak")]
    Escapes,
}

#[test]
fn extra_args_see_fields_as_bindings() {
    assert_eq!(
        Args::TooMany { count: 10 }.to_string(),
        "20 items, limit 16"
    );
    assert_eq!(Args::Named { len: 20 }.to_string(), "20 > 16");
    assert_eq!(Args::Tuple("a", 1).to_string(), "\"a\" is 2");
    assert_eq!(Args::Width { width: 3 }.to_string(), "  x|");
    assert_eq!(Args::Braces(5).to_string(), "{literal} braces, 5}");
    assert_eq!(Args::Escapes.to_string(), "line\nbreak");
}

#[derive(Debug, Error)]
enum Outer {
    #[error("io")]
    Io(#[from] OsError),
    #[error("loading {path}")]
    Load { path: &'static str, source: OsError },
    #[error("explicit")]
    Explicit {
        #[source]
        cause: ApiError,
        attempt: u32,
    },
    #[error(transparent)]
    Inner(#[from] Middle),
    #[error("plain")]
    Plain,
}

#[derive(Debug, Error)]
#[error("middle")]
struct Middle(#[source] OsError);

#[derive(Debug, Error)]
#[error(transparent)]
struct Wrapper(Middle);

const OS: OsError = OsError {
    kind: Kind::NotFound,
    raw: 2,
};

fn source_text(e: &dyn core::error::Error) -> Option<String> {
    e.source().map(|s| s.to_string())
}

#[test]
fn from_converts_and_is_the_source() {
    fn fails() -> Result<(), Outer> {
        Err(OS)?
    }
    let e = fails().unwrap_err();
    assert!(matches!(e, Outer::Io(_)));
    assert_eq!(e.to_string(), "io");
    assert_eq!(source_text(&e).as_deref(), Some("not found (os error 2)"));
}

#[test]
fn source_by_name_or_attribute() {
    let load = Outer::Load {
        path: "x",
        source: OS,
    };
    assert_eq!(load.to_string(), "loading x");
    assert_eq!(
        source_text(&load).as_deref(),
        Some("not found (os error 2)")
    );

    let explicit = Outer::Explicit {
        cause: ApiError {
            kind: Kind::DeviceLost,
            raw: 0,
        },
        attempt: 1,
    };
    assert_eq!(source_text(&explicit).as_deref(), Some("device lost"));
    assert!(Outer::Plain.source().is_none());
}

#[test]
fn transparent_forwards_display_and_source() {
    let inner = Outer::from(Middle(OS));
    assert_eq!(inner.to_string(), "middle");
    assert_eq!(
        source_text(&inner).as_deref(),
        Some("not found (os error 2)")
    );

    let wrapper = Wrapper(Middle(OS));
    assert_eq!(wrapper.to_string(), "middle");
    assert_eq!(
        source_text(&wrapper).as_deref(),
        Some("not found (os error 2)")
    );
}

#[derive(Debug, Error)]
#[error("{value}")]
struct Generic<T: fmt::Display + fmt::Debug> {
    value: T,
}

#[derive(Debug, Error)]
enum GenericSource<E: core::error::Error + 'static> {
    #[error("wrapped")]
    Wrapped(#[from] E),
}

#[test]
fn generics_pass_through() {
    assert_eq!(Generic { value: 1.5 }.to_string(), "1.5");
    let e = GenericSource::from(OS);
    assert_eq!(source_text(&e).as_deref(), Some("not found (os error 2)"));
}

#[derive(Debug, Error)]
enum Never {}

#[test]
fn empty_enum_compiles() {
    fn takes(_: Option<Never>) {}
    takes(None);
}
