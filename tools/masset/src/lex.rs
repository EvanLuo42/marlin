//! Tokens. Zero-copy: everything but strings with escapes borrows the source text.

use std::borrow::Cow;

use marlin_asset::Uuid;

use crate::ast::Span;
use crate::error::ParseError;

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Tok<'a> {
    Ident(&'a str),
    /// `!Name`, without the `!`.
    Directive(&'a str),
    Int(i128),
    Float(f64),
    Str(Cow<'a, str>),
    Link {
        path: &'a str,
        name: &'a str,
        uuid: Option<Uuid>,
    },
    File {
        scheme: &'a str,
        path: &'a str,
    },
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Eq,
    Comma,
    Eof,
}

impl Tok<'_> {
    /// How the token reads in an error message.
    pub(crate) fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Directive(s) => format!("`!{s}`"),
            Tok::Int(_) | Tok::Float(_) => "a number".into(),
            Tok::Str(_) => "a string".into(),
            Tok::Link { .. } => "a link".into(),
            Tok::File { .. } => "a file link".into(),
            Tok::LBrace => "`{`".into(),
            Tok::RBrace => "`}`".into(),
            Tok::LParen => "`(`".into(),
            Tok::RParen => "`)`".into(),
            Tok::LBracket => "`[`".into(),
            Tok::RBracket => "`]`".into(),
            Tok::Eq => "`=`".into(),
            Tok::Comma => "`,`".into(),
            Tok::Eof => "the end of the file".into(),
        }
    }
}

pub(crate) struct Lexer<'a> {
    text: &'a str,
    pos: usize,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        // A UTF-8 byte order mark is not content.
        let pos = if text.starts_with('\u{feff}') { 3 } else { 0 };
        Self { text, pos }
    }

    pub(crate) fn text(&self) -> &'a str {
        self.text
    }

    fn error(&self, start: usize, message: impl Into<String>) -> ParseError {
        ParseError::new(self.text, Span::new(start, self.pos.max(start)), message)
    }

    fn peek_byte(&self, ahead: usize) -> Option<u8> {
        self.text.as_bytes().get(self.pos + ahead).copied()
    }

    fn skip_trivia(&mut self) -> Result<(), ParseError> {
        let bytes = self.text.as_bytes();
        while self.pos < bytes.len() {
            match bytes[self.pos] {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                b'/' if self.peek_byte(1) == Some(b'/') => {
                    self.pos = self.text[self.pos..]
                        .find('\n')
                        .map_or(bytes.len(), |i| self.pos + i);
                }
                b'/' if self.peek_byte(1) == Some(b'*') => {
                    let start = self.pos;
                    match self.text[self.pos + 2..].find("*/") {
                        Some(i) => self.pos += i + 4,
                        None => {
                            self.pos = bytes.len();
                            return Err(self.error(start, "unterminated `/*` comment"));
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(())
    }

    pub(crate) fn next(&mut self) -> Result<(Tok<'a>, Span), ParseError> {
        self.skip_trivia()?;
        let start = self.pos;
        let Some(b) = self.peek_byte(0) else {
            return Ok((Tok::Eof, Span::new(start, start)));
        };
        let tok = match b {
            b'{' => self.single(Tok::LBrace),
            b'}' => self.single(Tok::RBrace),
            b'(' => self.single(Tok::LParen),
            b')' => self.single(Tok::RParen),
            b'[' => self.single(Tok::LBracket),
            b']' => self.single(Tok::RBracket),
            b'=' => self.single(Tok::Eq),
            b',' => self.single(Tok::Comma),
            b'"' => self.string()?,
            b'<' => self.link()?,
            b'`' => self.file()?,
            b'!' => {
                self.pos += 1;
                if !self.peek_byte(0).is_some_and(is_ident_start) {
                    return Err(self.error(start, "expected a directive name after `!`"));
                }
                Tok::Directive(self.ident())
            }
            b'-' | b'0'..=b'9' => self.number()?,
            b if is_ident_start(b) => Tok::Ident(self.ident()),
            _ => {
                let c = self.text[start..].chars().next().unwrap_or('?');
                self.pos += c.len_utf8();
                return Err(self.error(start, format!("unexpected character `{c}`")));
            }
        };
        Ok((tok, Span::new(start, self.pos)))
    }

    fn single(&mut self, tok: Tok<'a>) -> Tok<'a> {
        self.pos += 1;
        tok
    }

    fn ident(&mut self) -> &'a str {
        let start = self.pos;
        let bytes = self.text.as_bytes();
        while self.pos < bytes.len() && is_ident_char(bytes[self.pos]) {
            self.pos += 1;
        }
        &self.text[start..self.pos]
    }

    fn number(&mut self) -> Result<Tok<'a>, ParseError> {
        let start = self.pos;
        let negative = self.peek_byte(0) == Some(b'-');
        if negative {
            self.pos += 1;
            if self.text[self.pos..].starts_with("inf") {
                self.pos += 3;
                if self.peek_byte(0).is_some_and(is_ident_char) {
                    return Err(self.error(start, "malformed number"));
                }
                return Ok(Tok::Float(f64::NEG_INFINITY));
            }
            if !self.peek_byte(0).is_some_and(|b| b.is_ascii_digit()) {
                return Err(self.error(start, "expected a digit after `-`"));
            }
        }
        let bytes = self.text.as_bytes();
        let digits_start = self.pos;
        if self.peek_byte(0) == Some(b'0') && matches!(self.peek_byte(1), Some(b'x' | b'X')) {
            self.pos += 2;
            let hex_start = self.pos;
            while self.pos < bytes.len()
                && (bytes[self.pos].is_ascii_hexdigit() || bytes[self.pos] == b'_')
            {
                self.pos += 1;
            }
            let digits: String = self.text[hex_start..self.pos]
                .chars()
                .filter(|&c| c != '_')
                .collect();
            let value = i128::from_str_radix(&digits, 16)
                .ok()
                .filter(|_| !digits.is_empty())
                .ok_or_else(|| self.error(start, "malformed hexadecimal number"))?;
            return self.end_number(start, Tok::Int(if negative { -value } else { value }));
        }

        let mut float = false;
        let scan_digits = |pos: &mut usize| {
            while *pos < bytes.len() && (bytes[*pos].is_ascii_digit() || bytes[*pos] == b'_') {
                *pos += 1;
            }
        };
        scan_digits(&mut self.pos);
        if self.peek_byte(0) == Some(b'.') && self.peek_byte(1).is_some_and(|b| b.is_ascii_digit())
        {
            float = true;
            self.pos += 1;
            scan_digits(&mut self.pos);
        }
        if matches!(self.peek_byte(0), Some(b'e' | b'E')) {
            float = true;
            self.pos += 1;
            if matches!(self.peek_byte(0), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !self.peek_byte(0).is_some_and(|b| b.is_ascii_digit()) {
                return Err(self.error(start, "malformed exponent"));
            }
            scan_digits(&mut self.pos);
        }
        let text: String = self.text[digits_start..self.pos]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        let tok = if float {
            let value: f64 = text
                .parse()
                .map_err(|_| self.error(start, "malformed number"))?;
            Tok::Float(if negative { -value } else { value })
        } else {
            let value: i128 = text
                .parse()
                .map_err(|_| self.error(start, "integer out of range"))?;
            Tok::Int(if negative { -value } else { value })
        };
        self.end_number(start, tok)
    }

    /// A number must not run into an identifier (`12px`).
    fn end_number(&self, start: usize, tok: Tok<'a>) -> Result<Tok<'a>, ParseError> {
        if self
            .peek_byte(0)
            .is_some_and(|b| is_ident_char(b) || b == b'.')
        {
            return Err(self.error(start, "malformed number"));
        }
        Ok(tok)
    }

    fn string(&mut self) -> Result<Tok<'a>, ParseError> {
        let start = self.pos;
        self.pos += 1;
        let body_start = self.pos;
        let mut owned: Option<String> = None;
        loop {
            let Some(c) = self.text[self.pos..].chars().next() else {
                return Err(self.error(start, "unterminated string"));
            };
            match c {
                '"' => {
                    let borrowed = &self.text[body_start..self.pos];
                    self.pos += 1;
                    return Ok(Tok::Str(match owned {
                        Some(s) => Cow::Owned(s),
                        None => Cow::Borrowed(borrowed),
                    }));
                }
                '\n' => return Err(self.error(start, "unterminated string")),
                '\\' => {
                    if owned.is_none() {
                        owned = Some(self.text[body_start..self.pos].to_owned());
                    }
                    let escape_start = self.pos;
                    self.pos += 1;
                    let e = self.text[self.pos..].chars().next();
                    self.pos += e.map_or(0, char::len_utf8);
                    let decoded = match e {
                        Some('n') => '\n',
                        Some('r') => '\r',
                        Some('t') => '\t',
                        Some('0') => '\0',
                        Some('\\') => '\\',
                        Some('"') => '"',
                        Some('\'') => '\'',
                        Some('u') => {
                            let rest = &self.text[self.pos..];
                            let close = rest
                                .strip_prefix('{')
                                .and_then(|r| r.find('}'))
                                .ok_or_else(|| {
                                    self.error(escape_start, "malformed `\\u{…}` escape")
                                })?;
                            let hex = &rest[1..close + 1];
                            self.pos += close + 2;
                            u32::from_str_radix(hex, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .ok_or_else(|| {
                                    self.error(escape_start, "invalid `\\u{…}` escape")
                                })?
                        }
                        _ => return Err(self.error(escape_start, "unknown escape")),
                    };
                    if let Some(s) = owned.as_mut() {
                        s.push(decoded);
                    }
                }
                c => {
                    if let Some(s) = owned.as_mut() {
                        s.push(c);
                    }
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn link(&mut self) -> Result<Tok<'a>, ParseError> {
        let start = self.pos;
        let close = self.text[self.pos..]
            .find('>')
            .ok_or_else(|| self.error(start, "unterminated link: expected `>`"))?;
        let body = &self.text[self.pos + 1..self.pos + close];
        self.pos += close + 1;
        if body.contains('\n') {
            return Err(self.error(start, "a link must be on one line"));
        }
        let (target, uuid) = match body.split_once('#') {
            Some((target, uuid)) => {
                let uuid = uuid
                    .trim()
                    .parse::<Uuid>()
                    .map_err(|_| self.error(start, "malformed UUID in link"))?;
                (target.trim_end(), Some(uuid))
            }
            None => (body.trim_end(), None),
        };
        let (path, name) = target
            .rsplit_once(':')
            .ok_or_else(|| self.error(start, "a link is `<path:Name #uuid>`"))?;
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(self.error(start, "a link needs the target object's name after `:`"));
        }
        if path.contains(char::is_whitespace) {
            return Err(self.error(start, "link paths cannot contain spaces"));
        }
        Ok(Tok::Link { path, name, uuid })
    }

    fn file(&mut self) -> Result<Tok<'a>, ParseError> {
        let start = self.pos;
        let close = self.text[self.pos + 1..]
            .find('`')
            .ok_or_else(|| self.error(start, "unterminated file link: expected `"))?;
        let body = &self.text[self.pos + 1..self.pos + 1 + close];
        self.pos += close + 2;
        let (scheme, path) = body
            .split_once(':')
            .ok_or_else(|| self.error(start, "a file link is `scheme:path`"))?;
        if scheme.is_empty() || path.is_empty() || body.contains('\n') {
            return Err(self.error(start, "a file link is `scheme:path`"));
        }
        Ok(Tok::File { scheme, path })
    }
}
