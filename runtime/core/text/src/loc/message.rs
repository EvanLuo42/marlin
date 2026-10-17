//! Messages: text with named placeholders and plural selection.
//!
//! # Syntax
//!
//! ```text
//! message     = { text | "{{" | "}}" | placeholder }
//! placeholder = "{" ws name ws "}"
//!             | "{" ws name ws "," ws "number" ws [ "," ws digits ws ] "}"
//!             | "{" ws name ws "," ws "date" ws [ "," ws ( "short" | "long" ) ws ] "}"
//!             | "{" ws name ws "," ws "time" ws "}"
//!             | "{" ws name ws "," ws "plural" ws "," { ws arm } ws "}"
//! arm         = selector ws "{" arm-message "}"
//! selector    = "zero" | "one" | "two" | "few" | "many" | "other" | "=" digits
//! arm-message = { text | "{{" | "##" | "#" | placeholder }
//! name        = 1*( ASCII letter | digit | "_" | "." | "-" )
//! ws          = *( " " | "\t" | "\n" | "\r" )
//! ```
//!
//! - `{name}` writes the argument by its type: a string as is, an integer or float as a
//!   number in the locale's format (floats with at most 3 fraction digits), a date in the
//!   short date format, a time in the time format.
//! - `{name, number}` requires a number; `{name, number, 2}` writes exactly 2 fraction digits
//!   (0–20). `{name, date}` / `{name, date, long}` require a date, `{name, time}` a time.
//! - `{name, plural, …}` requires a number and picks one arm: the first `=N` arm whose `N` is
//!   the (non-negative, integral) number, else the arm of the number's CLDR plural category in
//!   the locale, else `other`, which every plural must have. Each selector may appear once.
//!   Inside an arm `#` writes the number as `{name}` would; `##` is a literal `#`. Plurals nest
//!   up to 8 deep; `#` is the innermost plural's number.
//! - `{{` and `}}` are literal braces. Inside an arm a single `}` ends the arm, so `}}` there
//!   is the end of the arm and of the plural, not an escaped brace: a literal `}` cannot be
//!   written inside an arm (`{{` can).
//!
//! ```
//! use marlin_text::loc::{Arg, Locale, Message};
//!
//! let m = Message::parse("{name} has {count, plural, =0 {no items} one {# item} other {# items}}.")?;
//! let en = Locale::parse("en").unwrap();
//! let s = m.format_to_string(&en, &[Arg::new("name", "Ada"), Arg::new("count", 1200)]).unwrap();
//! assert_eq!(s, "Ada has 1,200 items.");
//! # Ok::<(), marlin_text::loc::MessageError>(())
//! ```

use alloc::string::String;
use core::fmt;

use super::{
    Date, DateFormat, DateStyle, Locale, NumberFormat, PluralCategory, PluralOperands, PluralRules,
    Time,
};
use crate::loc::number::decimal_digits;

const MAX_DEPTH: u32 = 8;
const MAX_EXACT_ARMS: usize = 16;
/// Fraction digits of floats written without a style (CLDR's `#,##0.###`).
const DEFAULT_FRACTION: u8 = 3;
const ZEROS: &str = "00000000000000000000";

/// A syntax error in a message.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::Error)]
#[error("{kind} at byte {position}")]
pub struct MessageError {
    /// Byte offset into the message.
    pub position: usize,
    pub kind: MessageErrorKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::Error)]
pub enum MessageErrorKind {
    #[error("`}}` without a matching `{{` (write `}}}}` for a literal brace)")]
    UnmatchedClose,
    #[error("`{{` is never closed")]
    Unclosed,
    #[error("expected an argument name")]
    ExpectedName,
    #[error("expected `,` or `}}`")]
    ExpectedCommaOrClose,
    #[error("expected `}}`")]
    ExpectedClose,
    #[error("expected `number`, `date`, `time` or `plural`")]
    UnknownFormat,
    #[error("bad style")]
    BadStyle,
    #[error("expected a plural selector (`zero`…`other` or `=N`)")]
    BadSelector,
    #[error("this plural selector appears twice")]
    DuplicateSelector,
    #[error("more than 16 `=N` selectors")]
    TooManyExactSelectors,
    #[error("expected `{{` after the selector")]
    ExpectedArm,
    #[error("the plural has no `other` arm")]
    MissingOther,
    #[error("plural arm is never closed")]
    UnclosedArm,
    #[error("plurals nested more than 8 deep")]
    TooDeep,
}

/// Why formatting a message failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_std::Error)]
pub enum FormatError<'a> {
    /// The message does not parse (a cooked table's messages were checked when it was built).
    #[error(transparent)]
    Message(#[from] MessageError),
    #[error("no argument `{0}`")]
    MissingArgument(&'a str),
    #[error("argument `{0}` does not have the type its placeholder needs")]
    WrongType(&'a str),
    /// The string table has no message for the key.
    #[error("no message with this key")]
    UnknownKey,
    /// The `fmt::Write` failed.
    #[error("the output refused the text")]
    Write,
}

impl From<fmt::Error> for FormatError<'_> {
    fn from(_: fmt::Error) -> Self {
        Self::Write
    }
}

/// An argument value.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Value<'a> {
    Str(&'a str),
    Int(i64),
    Float(f64),
    Date(Date),
    Time(Time),
}

impl<'a> From<&'a str> for Value<'a> {
    fn from(s: &'a str) -> Self {
        Self::Str(s)
    }
}

impl<'a> From<&'a String> for Value<'a> {
    fn from(s: &'a String) -> Self {
        Self::Str(s)
    }
}

macro_rules! int_values {
    ($($t:ty),*) => {$(
        impl From<$t> for Value<'_> {
            fn from(n: $t) -> Self {
                Self::Int(n.into())
            }
        }
    )*};
}
int_values!(i8, i16, i32, i64, u8, u16, u32);

impl From<u64> for Value<'_> {
    /// Saturates at `i64::MAX`.
    fn from(n: u64) -> Self {
        Self::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

impl From<usize> for Value<'_> {
    /// Saturates at `i64::MAX`.
    fn from(n: usize) -> Self {
        Self::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

impl From<f64> for Value<'_> {
    fn from(x: f64) -> Self {
        Self::Float(x)
    }
}

impl From<f32> for Value<'_> {
    fn from(x: f32) -> Self {
        Self::Float(x.into())
    }
}

impl From<Date> for Value<'_> {
    fn from(d: Date) -> Self {
        Self::Date(d)
    }
}

impl From<Time> for Value<'_> {
    fn from(t: Time) -> Self {
        Self::Time(t)
    }
}

/// A named argument for a message.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Arg<'a> {
    pub name: &'a str,
    pub value: Value<'a>,
}

impl<'a> Arg<'a> {
    pub fn new(name: &'a str, value: impl Into<Value<'a>>) -> Self {
        Self {
            name,
            value: value.into(),
        }
    }
}

/// A plural arm's selector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Selector {
    /// `=N`
    Exact(u64),
    Category(PluralCategory),
}

/// What [`Message::walk`] reports, in source order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part<'a> {
    /// A `{name…}` placeholder other than a plural.
    Argument(&'a str),
    /// A plural on the argument; its [`Part::Selector`]s follow, each before its arm's parts.
    Plural(&'a str),
    Selector(Selector),
}

/// A message whose syntax has been checked (see the [module docs](self)).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Message<'a> {
    source: &'a str,
}

impl<'a> Message<'a> {
    /// Checks the syntax of `source`.
    pub fn parse(source: &'a str) -> Result<Self, MessageError> {
        let mut parser = Parser::new(source);
        while parser.next_piece()?.is_some() {}
        Ok(Self { source })
    }

    /// `source` as a message without checking it: a syntax error turns up when formatting.
    pub const fn new_unchecked(source: &'a str) -> Self {
        Self { source }
    }

    pub const fn as_str(&self) -> &'a str {
        self.source
    }

    /// Reports every placeholder, plural and selector, nested ones included.
    pub fn walk(&self, mut f: impl FnMut(Part<'a>)) -> Result<(), MessageError> {
        walk(&mut Parser::new(self.source), &mut f)
    }

    /// Formats the message for `locale`: its plural rules, number and date formats.
    pub fn format(
        &self,
        locale: &Locale,
        args: &[Arg<'_>],
        out: &mut impl fmt::Write,
    ) -> Result<(), FormatError<'a>> {
        format_level(
            Parser::new(self.source),
            &Formats::for_locale(locale),
            args,
            None,
            out,
        )
    }

    /// [`format`](Self::format) into a new `String`.
    pub fn format_to_string(
        &self,
        locale: &Locale,
        args: &[Arg<'_>],
    ) -> Result<String, FormatError<'a>> {
        let mut out = String::with_capacity(self.source.len() + 16);
        self.format(locale, args, &mut out)?;
        Ok(out)
    }
}

impl fmt::Display for Message<'_> {
    /// The source text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.source)
    }
}

/// How a placeholder writes its argument.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Style {
    Default,
    Number(Option<u8>),
    Date(DateStyle),
    Time,
}

#[derive(Clone, Copy, Debug)]
enum Piece<'a> {
    Text(&'a str),
    /// `#` in an arm.
    Hash,
    Arg {
        name: &'a str,
        style: Style,
    },
    /// `arms` is the byte offset of the first arm.
    Plural {
        name: &'a str,
        arms: usize,
    },
}

/// Walks one nesting level of a message.
#[derive(Clone)]
struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// 0 at the top level, n inside an arm of the n-th nested plural.
    depth: u32,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            depth: 0,
        }
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    #[inline]
    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + ahead).copied()
    }

    fn error(&self, kind: MessageErrorKind) -> MessageError {
        MessageError {
            position: self.pos,
            kind,
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn take_while(&mut self, f: impl Fn(u8) -> bool) -> &'a str {
        let start = self.pos;
        while self.peek().is_some_and(&f) {
            self.pos += 1;
        }
        // Only ASCII bytes were taken, so both ends are char boundaries.
        &self.src[start..self.pos]
    }

    fn expect_close(&mut self, open: usize) -> Result<(), MessageError> {
        self.skip_ws();
        match self.peek() {
            Some(b'}') => {
                self.pos += 1;
                Ok(())
            }
            None => Err(MessageError {
                position: open,
                kind: MessageErrorKind::Unclosed,
            }),
            Some(_) => Err(self.error(MessageErrorKind::ExpectedClose)),
        }
    }

    /// The next piece at this level; `None` at the end of the message, or (inside an arm) at
    /// the `}` that closes the arm, which is left unread.
    fn next_piece(&mut self) -> Result<Option<Piece<'a>>, MessageError> {
        let start = self.pos;
        let in_arm = self.depth > 0;
        while let Some(c) = self.peek() {
            match c {
                b'{' | b'}' => break,
                b'#' if in_arm => break,
                _ => self.pos += 1,
            }
        }
        if self.pos > start {
            return Ok(Some(Piece::Text(&self.src[start..self.pos])));
        }
        let doubled = |p: &Self, c| p.peek_at(1) == Some(c);
        match self.peek() {
            None if in_arm => Err(self.error(MessageErrorKind::UnclosedArm)),
            None => Ok(None),
            Some(b'#') if doubled(self, b'#') => {
                self.pos += 2;
                Ok(Some(Piece::Text("#")))
            }
            Some(b'#') => {
                self.pos += 1;
                Ok(Some(Piece::Hash))
            }
            Some(b'{') if doubled(self, b'{') => {
                self.pos += 2;
                Ok(Some(Piece::Text("{")))
            }
            Some(b'{') => self.placeholder().map(Some),
            _ if in_arm => Ok(None),
            _ if doubled(self, b'}') => {
                self.pos += 2;
                Ok(Some(Piece::Text("}")))
            }
            _ => Err(self.error(MessageErrorKind::UnmatchedClose)),
        }
    }

    fn placeholder(&mut self) -> Result<Piece<'a>, MessageError> {
        let open = self.pos;
        self.pos += 1;
        self.skip_ws();
        let name =
            self.take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        if name.is_empty() {
            return Err(self.error(MessageErrorKind::ExpectedName));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'}') => {
                self.pos += 1;
                return Ok(Piece::Arg {
                    name,
                    style: Style::Default,
                });
            }
            Some(b',') => self.pos += 1,
            None => {
                return Err(MessageError {
                    position: open,
                    kind: MessageErrorKind::Unclosed,
                });
            }
            Some(_) => return Err(self.error(MessageErrorKind::ExpectedCommaOrClose)),
        }
        self.skip_ws();
        let keyword_at = self.pos;
        let keyword = self.take_while(|b| b.is_ascii_alphabetic());
        self.skip_ws();
        let style = match keyword {
            "number" => {
                let mut digits = None;
                if self.peek() == Some(b',') {
                    self.pos += 1;
                    self.skip_ws();
                    let at = self.pos;
                    let n = self.take_while(|b| b.is_ascii_digit());
                    match n.parse::<u8>() {
                        Ok(n) if n <= 20 => digits = Some(n),
                        _ => {
                            self.pos = at;
                            return Err(self.error(MessageErrorKind::BadStyle));
                        }
                    }
                }
                Style::Number(digits)
            }
            "date" => {
                let mut style = DateStyle::Short;
                if self.peek() == Some(b',') {
                    self.pos += 1;
                    self.skip_ws();
                    let at = self.pos;
                    style = match self.take_while(|b| b.is_ascii_alphabetic()) {
                        "short" => DateStyle::Short,
                        "long" => DateStyle::Long,
                        _ => {
                            self.pos = at;
                            return Err(self.error(MessageErrorKind::BadStyle));
                        }
                    };
                }
                Style::Date(style)
            }
            "time" => Style::Time,
            "plural" => {
                if self.peek() != Some(b',') {
                    return Err(self.error(MessageErrorKind::ExpectedCommaOrClose));
                }
                self.pos += 1;
                let arms = self.pos;
                self.arms(open, |_, _| {})?;
                return Ok(Piece::Plural { name, arms });
            }
            _ => {
                self.pos = keyword_at;
                return Err(self.error(MessageErrorKind::UnknownFormat));
            }
        };
        self.expect_close(open)?;
        Ok(Piece::Arg { name, style })
    }

    /// Reads the arms of a plural (from just after `plural,` to past its closing `}`), checking
    /// each body; calls `f` with each selector and the byte offset of its body.
    fn arms(
        &mut self,
        open: usize,
        mut f: impl FnMut(Selector, usize),
    ) -> Result<(), MessageError> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error(MessageErrorKind::TooDeep));
        }
        let mut categories = 0u8;
        let mut exact = [0u64; MAX_EXACT_ARMS];
        let mut exact_count = 0;
        loop {
            self.skip_ws();
            let at = self.pos;
            let selector = match self.peek() {
                Some(b'}') if categories & 1 << PluralCategory::Other as u8 == 0 => {
                    return Err(self.error(MessageErrorKind::MissingOther));
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(());
                }
                None => {
                    return Err(MessageError {
                        position: open,
                        kind: MessageErrorKind::Unclosed,
                    });
                }
                Some(b'=') => {
                    self.pos += 1;
                    let n = self.take_while(|b| b.is_ascii_digit());
                    let n = n.parse::<u64>().map_err(|_| MessageError {
                        position: at,
                        kind: MessageErrorKind::BadSelector,
                    })?;
                    if exact[..exact_count].contains(&n) {
                        self.pos = at;
                        return Err(self.error(MessageErrorKind::DuplicateSelector));
                    }
                    if exact_count == MAX_EXACT_ARMS {
                        self.pos = at;
                        return Err(self.error(MessageErrorKind::TooManyExactSelectors));
                    }
                    exact[exact_count] = n;
                    exact_count += 1;
                    Selector::Exact(n)
                }
                Some(_) => {
                    let keyword = self.take_while(|b| b.is_ascii_alphabetic());
                    let category = PluralCategory::from_keyword(keyword).ok_or(MessageError {
                        position: at,
                        kind: MessageErrorKind::BadSelector,
                    })?;
                    let bit = 1 << category as u8;
                    if categories & bit != 0 {
                        self.pos = at;
                        return Err(self.error(MessageErrorKind::DuplicateSelector));
                    }
                    categories |= bit;
                    Selector::Category(category)
                }
            };
            self.skip_ws();
            if self.peek() != Some(b'{') {
                return Err(self.error(MessageErrorKind::ExpectedArm));
            }
            self.pos += 1;
            f(selector, self.pos);
            self.depth += 1;
            while self.next_piece()?.is_some() {}
            self.depth -= 1;
            // At the arm's closing brace.
            self.pos += 1;
        }
    }

    /// A parser for the arm body at `body`, one level deeper.
    fn arm(&self, body: usize) -> Self {
        Self {
            src: self.src,
            pos: body,
            depth: self.depth + 1,
        }
    }
}

fn walk<'a>(parser: &mut Parser<'a>, f: &mut impl FnMut(Part<'a>)) -> Result<(), MessageError> {
    while let Some(piece) = parser.next_piece()? {
        match piece {
            Piece::Text(_) | Piece::Hash => {}
            Piece::Arg { name, .. } => f(Part::Argument(name)),
            Piece::Plural { name, arms } => {
                f(Part::Plural(name));
                let mut bodies = parser.clone();
                bodies.pos = arms;
                let mut found = [(Selector::Exact(0), 0); 6 + MAX_EXACT_ARMS];
                let mut count = 0;
                bodies.arms(0, |selector, body| {
                    found[count] = (selector, body);
                    count += 1;
                })?;
                for &(selector, body) in &found[..count] {
                    f(Part::Selector(selector));
                    walk(&mut parser.arm(body), f)?;
                }
            }
        }
    }
    Ok(())
}

/// A locale's formatting data, looked up once per message.
struct Formats {
    plural: PluralRules,
    number: NumberFormat,
    date: DateFormat,
}

impl Formats {
    fn for_locale(locale: &Locale) -> Self {
        Self {
            plural: PluralRules::for_locale(locale),
            number: NumberFormat::for_locale(locale),
            date: DateFormat::for_locale(locale),
        }
    }
}

/// A number being pluralized: `#` writes it.
#[derive(Clone, Copy)]
enum Count {
    Int(i64),
    Float(f64),
}

impl Count {
    fn write(self, number: &NumberFormat, out: &mut dyn fmt::Write) -> fmt::Result {
        match self {
            Self::Int(n) => number.format_int(n, out),
            Self::Float(x) => number.format_float(x, 0, DEFAULT_FRACTION, out),
        }
    }
}

fn format_level<'a>(
    mut parser: Parser<'a>,
    formats: &Formats,
    args: &[Arg<'_>],
    count: Option<Count>,
    out: &mut dyn fmt::Write,
) -> Result<(), FormatError<'a>> {
    while let Some(piece) = parser.next_piece()? {
        match piece {
            Piece::Text(text) => out.write_str(text)?,
            Piece::Hash => {
                if let Some(count) = count {
                    count.write(&formats.number, out)?;
                }
            }
            Piece::Arg { name, style } => {
                let value = find(args, name)?;
                write_value(name, value, style, formats, out)?;
            }
            Piece::Plural { name, arms } => {
                let (count, negative, operands) = match find(args, name)? {
                    Value::Int(n) => (Count::Int(n), n < 0, PluralOperands::from_int(n)),
                    Value::Float(x) if x.is_finite() => {
                        let mut buf = [0u8; 340];
                        let digits = decimal_digits(x, 0, DEFAULT_FRACTION, &mut buf);
                        let operands = PluralOperands::from_decimal(digits).unwrap_or_default();
                        (Count::Float(x), x < 0.0, operands)
                    }
                    Value::Float(x) => (Count::Float(x), x < 0.0, PluralOperands::default()),
                    _ => return Err(FormatError::WrongType(name)),
                };
                let category = formats.plural.select(&operands);
                let exact = (!negative && operands.v == 0).then_some(operands.i);
                // The first matching `=N`, else the category's arm, else `other`.
                let (mut by_exact, mut by_category, mut other) = (None, None, None);
                let mut bodies = parser.clone();
                bodies.pos = arms;
                bodies.arms(0, |selector, body| match selector {
                    Selector::Exact(n) if Some(n) == exact && by_exact.is_none() => {
                        by_exact = Some(body);
                    }
                    Selector::Category(c) if c == category => by_category = Some(body),
                    Selector::Category(PluralCategory::Other) => other = Some(body),
                    _ => {}
                })?;
                if let Some(body) = by_exact.or(by_category).or(other) {
                    format_level(parser.arm(body), formats, args, Some(count), out)?;
                }
            }
        }
    }
    Ok(())
}

fn find<'v, 'a>(args: &[Arg<'v>], name: &'a str) -> Result<Value<'v>, FormatError<'a>> {
    args.iter()
        .find(|a| a.name == name)
        .map(|a| a.value)
        .ok_or(FormatError::MissingArgument(name))
}

fn write_value<'a>(
    name: &'a str,
    value: Value<'_>,
    style: Style,
    formats: &Formats,
    out: &mut dyn fmt::Write,
) -> Result<(), FormatError<'a>> {
    let number = &formats.number;
    match (value, style) {
        (Value::Str(s), Style::Default) => out.write_str(s)?,
        (Value::Int(n), Style::Default | Style::Number(None | Some(0))) => {
            number.format_int(n, out)?
        }
        (Value::Int(n), Style::Number(Some(digits))) => {
            let mut buf = [0u8; 20];
            let int = super::number::ascii_digits(n.unsigned_abs(), &mut buf);
            number.write_parts(n < 0, int, &ZEROS[..usize::from(digits)], out)?;
        }
        (Value::Float(x), Style::Default | Style::Number(None)) => {
            number.format_float(x, 0, DEFAULT_FRACTION, out)?;
        }
        (Value::Float(x), Style::Number(Some(digits))) => {
            number.format_float(x, digits, digits, out)?;
        }
        (Value::Date(d), Style::Default) => formats.date.format_date(d, DateStyle::Short, out)?,
        (Value::Date(d), Style::Date(style)) => formats.date.format_date(d, style, out)?,
        (Value::Time(t), Style::Default | Style::Time) => formats.date.format_time(t, out)?,
        _ => return Err(FormatError::WrongType(name)),
    }
    Ok(())
}
