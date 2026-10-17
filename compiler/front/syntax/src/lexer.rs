//! The lexer: C#'s lexical grammar without the preprocessor, shared by every dialect
//! (`docs/design/0015-language-frontend.md` §2).
//!
//! It never fails: every byte of the input ends up in exactly one token, and problems are
//! reported beside the token stream. C#'s reserved keywords, plus the dialect's extra ones, get
//! their keyword kind here; contextual keywords stay `IDENT` until the parser decides. Whether a
//! dialect allows a literal form (`1.0h`, `1.0m`, `'c'`) is left to validation.
//!
//! Interpolated strings (`$"a {x:F2} b"`) are split into `INTERP_START`, `INTERP_TEXT`,
//! `{` hole tokens `}`, an optional `:` + `INTERP_FORMAT`, and `INTERP_END`, so the parser sees
//! the expressions in the holes as ordinary tokens.

use marlin_lang_span::{TextRange, TextSize};

use crate::SyntaxKind::{self, *};

/// One token: its kind and length in bytes. Offsets follow from the lengths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub len: TextSize,
}

/// A problem found while lexing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub range: TextRange,
    pub message: String,
}

/// Splits `text` into tokens. The lengths sum to `text.len()`.
pub fn lex(text: &str, extra_keywords: &[SyntaxKind]) -> (Vec<Token>, Vec<LexError>) {
    let mut lexer = Lexer {
        text,
        extra_keywords,
        pos: 0,
        tokens: Vec::new(),
        errors: Vec::new(),
        modes: Vec::new(),
    };
    lexer.run();
    (lexer.tokens, lexer.errors)
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    /// Inside the text of an interpolated string.
    Text { verbatim: bool },
    /// Inside a `{...}` hole; `depth` counts unclosed brackets of any kind.
    Hole { verbatim: bool, depth: u32 },
}

struct Lexer<'a> {
    text: &'a str,
    extra_keywords: &'a [SyntaxKind],
    pos: usize,
    tokens: Vec<Token>,
    errors: Vec<LexError>,
    modes: Vec<Mode>,
}

impl Lexer<'_> {
    fn keyword(&self, text: &str) -> Option<SyntaxKind> {
        SyntaxKind::from_keyword(text).or_else(|| {
            SyntaxKind::from_extra_keyword(text).filter(|k| self.extra_keywords.contains(k))
        })
    }

    fn run(&mut self) {
        while self.pos < self.text.len() {
            match self.modes.last().copied() {
                Some(Mode::Text { verbatim }) => self.interp_text(verbatim),
                Some(Mode::Hole { verbatim, depth }) => self.hole_token(verbatim, depth),
                None => self.token(),
            }
        }
        if !self.modes.is_empty() {
            self.error_at(self.pos, self.pos, "unterminated interpolated string");
        }
    }

    fn rest(&self) -> &str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    fn push(&mut self, kind: SyntaxKind, start: usize) {
        debug_assert!(self.pos > start, "empty token");
        self.tokens.push(Token {
            kind,
            len: TextSize((self.pos - start) as u32),
        });
    }

    fn error_at(&mut self, start: usize, end: usize, message: impl Into<String>) {
        self.errors.push(LexError {
            range: TextRange::new(TextSize(start as u32), TextSize(end as u32)),
            message: message.into(),
        });
    }

    fn bump_while(&mut self, mut f: impl FnMut(char) -> bool) {
        while let Some(c) = self.peek() {
            if !f(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
    }

    /// One token in normal mode.
    fn token(&mut self) {
        let start = self.pos;
        let c = self.peek().unwrap();
        let kind = match c {
            c if c.is_whitespace() || c == '\u{feff}' => {
                self.bump_while(|c| c.is_whitespace() || c == '\u{feff}');
                WHITESPACE
            }
            '/' if self.peek_at(1) == Some('/') => {
                self.bump_while(|c| c != '\n');
                LINE_COMMENT
            }
            '/' if self.peek_at(1) == Some('*') => {
                self.pos += 2;
                match self.rest().find("*/") {
                    Some(end) => self.pos += end + 2,
                    None => {
                        self.pos = self.text.len();
                        self.error_at(start, start + 2, "unterminated block comment");
                    }
                }
                BLOCK_COMMENT
            }
            '$' if self.rest().starts_with("$\"") => {
                self.pos += 2;
                self.modes.push(Mode::Text { verbatim: false });
                INTERP_START
            }
            '$' if self.rest().starts_with("$@\"") => {
                self.pos += 3;
                self.modes.push(Mode::Text { verbatim: true });
                INTERP_START
            }
            '@' if self.rest().starts_with("@$\"") => {
                self.pos += 3;
                self.modes.push(Mode::Text { verbatim: true });
                INTERP_START
            }
            '@' if self.peek_at(1) == Some('"') => {
                self.pos += 2;
                self.verbatim_string(start);
                STRING
            }
            '@' if self.peek_at(1).is_some_and(is_ident_start) => {
                // Verbatim identifier: `@class` is the identifier `class`.
                self.pos += 1;
                self.bump_while(is_ident_continue);
                IDENT
            }
            '"' => {
                self.pos += 1;
                self.regular_string(start);
                STRING
            }
            '\'' => {
                self.pos += 1;
                self.char_literal(start);
                CHAR
            }
            '#' => {
                self.bump_while(|c| c != '\n');
                self.error_at(
                    start,
                    self.pos,
                    "there is no preprocessor; `#` directives are not supported",
                );
                ERROR_TOKEN
            }
            c if c.is_ascii_digit() => self.number(),
            '.' if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) => self.number(),
            c if is_ident_start(c) => {
                self.bump_while(is_ident_continue);
                self.keyword(&self.text[start..self.pos]).unwrap_or(IDENT)
            }
            _ => match self.punct() {
                Some(kind) => kind,
                None => {
                    self.pos += c.len_utf8();
                    self.error_at(start, self.pos, format!("unexpected character `{c}`"));
                    ERROR_TOKEN
                }
            },
        };
        self.push(kind, start);
    }

    fn punct(&mut self) -> Option<SyntaxKind> {
        const TABLE: &[(&str, SyntaxKind)] = &[
            ("<<=", SHL_EQ),
            ("..", DOT2),
            ("::", COLON2),
            ("==", EQ2),
            ("!=", NEQ),
            ("<=", LTEQ),
            ("<<", SHL),
            (">=", GTEQ),
            ("++", PLUS2),
            ("+=", PLUS_EQ),
            ("--", MINUS2),
            ("-=", MINUS_EQ),
            ("*=", STAR_EQ),
            ("/=", SLASH_EQ),
            ("%=", PERCENT_EQ),
            ("&&", AMP2),
            ("&=", AMP_EQ),
            ("||", PIPE2),
            ("|=", PIPE_EQ),
            ("^=", CARET_EQ),
            ("=>", FAT_ARROW),
            ("{", L_CURLY),
            ("}", R_CURLY),
            ("(", L_PAREN),
            (")", R_PAREN),
            ("[", L_BRACK),
            ("]", R_BRACK),
            (";", SEMICOLON),
            (",", COMMA),
            (".", DOT),
            (":", COLON),
            ("?", QUESTION),
            ("~", TILDE),
            ("!", BANG),
            ("=", EQ),
            ("<", LT),
            (">", GT),
            ("+", PLUS),
            ("-", MINUS),
            ("*", STAR),
            ("/", SLASH),
            ("%", PERCENT),
            ("&", AMP),
            ("|", PIPE),
            ("^", CARET),
        ];
        let rest = self.rest();
        let &(text, kind) = TABLE.iter().find(|(text, _)| rest.starts_with(text))?;
        self.pos += text.len();
        Some(kind)
    }

    /// Integer and real literals (C# §6.4.5.3–4) plus the `h` suffix for `half`.
    fn number(&mut self) -> SyntaxKind {
        let start = self.pos;
        let rest = self.rest().as_bytes();
        let radix_prefix =
            rest.len() > 1 && rest[0] == b'0' && matches!(rest[1], b'x' | b'X' | b'b' | b'B');
        if radix_prefix {
            let hex = matches!(rest[1], b'x' | b'X');
            self.pos += 2;
            let digits_start = self.pos;
            if hex {
                self.bump_while(|c| c.is_ascii_hexdigit() || c == '_');
            } else {
                self.bump_while(|c| c == '0' || c == '1' || c == '_');
            }
            if self.pos == digits_start {
                self.error_at(start, self.pos, "missing digits after the radix prefix");
            }
            self.suffix(start, false);
            return INT_NUMBER;
        }

        let mut real = false;
        self.bump_while(|c| c.is_ascii_digit() || c == '_');
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            real = true;
            self.pos += 1;
            self.bump_while(|c| c.is_ascii_digit() || c == '_');
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let sign = usize::from(matches!(self.peek_at(1), Some('+' | '-')));
            if self.peek_at(1 + sign).is_some_and(|c| c.is_ascii_digit()) {
                real = true;
                self.pos += 1 + sign;
                self.bump_while(|c| c.is_ascii_digit() || c == '_');
            }
        }
        if self.suffix(start, real) || real {
            FLOAT_NUMBER
        } else {
            INT_NUMBER
        }
    }

    /// Lexes a literal suffix; returns whether it makes the literal real.
    fn suffix(&mut self, literal_start: usize, real: bool) -> bool {
        let start = self.pos;
        self.bump_while(is_ident_continue);
        let suffix = &self.text[start..self.pos];
        let lower = suffix.to_ascii_lowercase();
        match lower.as_str() {
            "" => false,
            "u" | "l" | "ul" | "lu" if !real => false,
            // Whether a dialect has `half` or `decimal` is checked by validation.
            "f" | "d" | "h" | "m" => true,
            _ => {
                self.error_at(
                    literal_start,
                    self.pos,
                    format!("invalid suffix `{suffix}` on a number"),
                );
                real
            }
        }
    }

    /// Body of `"..."` after the opening quote.
    fn regular_string(&mut self, start: usize) {
        loop {
            match self.peek() {
                None | Some('\n') => {
                    self.error_at(start, self.pos, "unterminated string");
                    return;
                }
                Some('"') => {
                    self.pos += 1;
                    return;
                }
                Some('\\') => self.escape(),
                Some(c) => self.pos += c.len_utf8(),
            }
        }
    }

    /// Body of `@"..."` after the opening quote; `""` is a quote.
    fn verbatim_string(&mut self, start: usize) {
        loop {
            match self.peek() {
                None => {
                    self.error_at(start, self.pos, "unterminated string");
                    return;
                }
                Some('"') if self.peek_at(1) == Some('"') => self.pos += 2,
                Some('"') => {
                    self.pos += 1;
                    return;
                }
                Some(c) => self.pos += c.len_utf8(),
            }
        }
    }

    fn char_literal(&mut self, start: usize) {
        loop {
            match self.peek() {
                None | Some('\n') => {
                    self.error_at(start, self.pos, "unterminated character literal");
                    return;
                }
                Some('\'') => {
                    self.pos += 1;
                    return;
                }
                Some('\\') => self.escape(),
                Some(c) => self.pos += c.len_utf8(),
            }
        }
    }

    /// A `\` escape inside a regular string or character literal.
    fn escape(&mut self) {
        let start = self.pos;
        self.pos += 1;
        match self.peek() {
            Some('\'' | '"' | '\\' | '0' | 'a' | 'b' | 'f' | 'n' | 'r' | 't' | 'v') => {
                self.pos += 1
            }
            Some(c @ ('u' | 'U' | 'x')) => {
                self.pos += 1;
                let digits_start = self.pos;
                let max = match c {
                    'u' => 4,
                    'U' => 8,
                    _ => 4,
                };
                let mut n = 0;
                self.bump_while(|c| {
                    n += 1;
                    n <= max && c.is_ascii_hexdigit()
                });
                let count = self.pos - digits_start;
                if count == 0 || (c != 'x' && count != max) {
                    self.error_at(start, self.pos, "invalid unicode escape");
                }
            }
            Some('\n') | None => self.error_at(start, self.pos, "invalid escape"),
            Some(c) => {
                self.pos += c.len_utf8();
                self.error_at(start, self.pos, format!("unknown escape `\\{c}`"));
            }
        }
    }

    /// Literal text of an interpolated string, up to a hole, the end, or (for a regular string)
    /// the end of the line.
    fn interp_text(&mut self, verbatim: bool) {
        let start = self.pos;
        loop {
            match self.peek() {
                None => break,
                Some('\n') if !verbatim => {
                    if self.pos > start {
                        self.push(INTERP_TEXT, start);
                    }
                    self.error_at(start, self.pos, "unterminated interpolated string");
                    self.modes.pop();
                    return;
                }
                Some('{') if self.peek_at(1) == Some('{') => self.pos += 2,
                Some('}') if self.peek_at(1) == Some('}') => self.pos += 2,
                Some('"') if verbatim && self.peek_at(1) == Some('"') => self.pos += 2,
                Some('{') => {
                    if self.pos > start {
                        self.push(INTERP_TEXT, start);
                    }
                    let open = self.pos;
                    self.pos += 1;
                    self.push(L_CURLY, open);
                    *self.modes.last_mut().unwrap() = Mode::Hole { verbatim, depth: 0 };
                    return;
                }
                Some('}') => {
                    if self.pos > start {
                        self.push(INTERP_TEXT, start);
                    }
                    let brace = self.pos;
                    self.pos += 1;
                    self.error_at(
                        brace,
                        self.pos,
                        "a lone `}` in an interpolated string must be written `}}`",
                    );
                    self.push(INTERP_TEXT, brace);
                    return;
                }
                Some('"') => {
                    if self.pos > start {
                        self.push(INTERP_TEXT, start);
                    }
                    let quote = self.pos;
                    self.pos += 1;
                    self.push(INTERP_END, quote);
                    self.modes.pop();
                    return;
                }
                Some('\\') if !verbatim => self.escape(),
                Some(c) => self.pos += c.len_utf8(),
            }
        }
        if self.pos > start {
            self.push(INTERP_TEXT, start);
        }
    }

    /// One token inside an interpolation hole.
    fn hole_token(&mut self, verbatim: bool, depth: u32) {
        let start = self.pos;
        match self.peek().unwrap() {
            '\n' | '\r' if !verbatim => {
                // A hole of a regular string ends at the end of the line at the latest.
                self.error_at(start, start, "unterminated interpolated string");
                self.modes.pop();
                self.token();
            }
            '}' if depth == 0 => {
                self.pos += 1;
                self.push(R_CURLY, start);
                *self.modes.last_mut().unwrap() = Mode::Text { verbatim };
            }
            ':' if depth == 0 && self.peek_at(1) != Some(':') => {
                self.pos += 1;
                self.push(COLON, start);
                let format_start = self.pos;
                self.bump_while(|c| c != '}' && c != '"' && c != '\n');
                if self.pos > format_start {
                    self.push(INTERP_FORMAT, format_start);
                }
            }
            '"' if depth == 0 => {
                // `$"{x"`: the hole was never closed. End the string here.
                self.pos += 1;
                self.error_at(start, self.pos, "expected `}` to close the interpolation");
                self.push(INTERP_END, start);
                self.modes.pop();
            }
            _ => {
                let modes = self.modes.len();
                self.token();
                let kind = self.tokens.last().unwrap().kind;
                // A nested interpolated string pushed its own mode; leave ours alone.
                if self.modes.len() == modes {
                    let delta: i32 = match kind {
                        L_PAREN | L_BRACK | L_CURLY => 1,
                        R_PAREN | R_BRACK | R_CURLY => -1,
                        _ => 0,
                    };
                    if let Some(Mode::Hole { depth, .. }) = self.modes.last_mut() {
                        *depth = depth.saturating_add_signed(delta);
                    }
                }
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}
