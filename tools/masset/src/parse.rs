//! Text to [`Document`].
//!
//! ```text
//! document = "!Version" int object*
//! object   = Ident "{" entry* "}"
//! entry    = Directive "=" value | Ident "=" value          separated by optional ","
//! value    = bool | int | float | string | link | file
//!          | Ident [ "(" value* ")" | "{" entry* "}" ]       unit / tuple / struct variant
//!          | "(" value* ")" | "[" value* "]" | "{" entry* "}"
//! ```
//!
//! Commas between elements are optional, so `(1 0 0)` and `(1, 0, 0)` are the same tuple.

use std::collections::HashSet;

use marlin_asset::Uuid;

use crate::ast::{Attr, Document, FileRef, LinkRef, Node, NodeKind, Object, Span};
use crate::error::ParseError;
use crate::lex::{Lexer, Tok};

/// Deeper nesting is rejected rather than risk the stack on hostile input.
const MAX_DEPTH: usize = 128;

/// Parses a whole file.
pub fn parse(text: &str) -> Result<Document, ParseError> {
    let mut p = Parser::new(text)?;
    let version = match p.tok {
        Tok::Directive("Version") => {
            p.bump()?;
            match p.tok {
                Tok::Int(v) if (0..=u32::MAX as i128).contains(&v) => {
                    p.bump()?;
                    v as u32
                }
                _ => return Err(p.unexpected("a format version number")),
            }
        }
        _ => return Err(p.unexpected("`!Version` at the start of the file")),
    };
    let mut objects = Vec::new();
    let mut names = HashSet::new();
    let mut uuids = HashSet::new();
    while p.tok != Tok::Eof {
        let object = p.object()?;
        if !names.insert(object.name.clone()) {
            return Err(p.error(
                object.span,
                format!("two objects are named `{}`", object.name),
            ));
        }
        if !uuids.insert(object.uuid) {
            return Err(p.error(
                object.span,
                format!("two objects have UUID {}", object.uuid),
            ));
        }
        objects.push(object);
    }
    Ok(Document { version, objects })
}

pub(crate) struct Parser<'a> {
    lexer: Lexer<'a>,
    tok: Tok<'a>,
    span: Span,
    depth: usize,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(text: &'a str) -> Result<Self, ParseError> {
        let mut lexer = Lexer::new(text);
        let (tok, span) = lexer.next()?;
        Ok(Self {
            lexer,
            tok,
            span,
            depth: 0,
        })
    }

    fn bump(&mut self) -> Result<(), ParseError> {
        let (tok, span) = self.lexer.next()?;
        self.tok = tok;
        self.span = span;
        Ok(())
    }

    fn error(&self, span: Span, message: impl Into<String>) -> ParseError {
        ParseError::new(self.lexer.text(), span, message)
    }

    fn unexpected(&self, expected: &str) -> ParseError {
        self.error(
            self.span,
            format!("expected {expected}, found {}", self.tok.describe()),
        )
    }

    fn expect(&mut self, tok: Tok<'static>, what: &str) -> Result<(), ParseError> {
        if self.tok != tok {
            return Err(self.unexpected(what));
        }
        self.bump()
    }

    fn eat_comma(&mut self) -> Result<(), ParseError> {
        if self.tok == Tok::Comma {
            self.bump()?;
        }
        Ok(())
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error(self.span, "nested too deeply"));
        }
        Ok(())
    }

    fn object(&mut self) -> Result<Object, ParseError> {
        let start = self.span.start as usize;
        let Tok::Ident(type_name) = self.tok else {
            return Err(self.unexpected("an object type name"));
        };
        let type_span = self.span;
        self.bump()?;
        self.expect(Tok::LBrace, "`{` after the object type")?;

        let mut uuid = None;
        let mut name = None;
        let mut type_version = 0;
        let mut attrs = Vec::new();
        let mut keys = HashSet::new();
        loop {
            match self.tok {
                Tok::RBrace => break,
                Tok::Directive(directive) => {
                    let span = self.span;
                    self.bump()?;
                    self.expect(Tok::Eq, "`=`")?;
                    let value = self.value()?;
                    match (directive, value.kind) {
                        ("Uuid", NodeKind::Str(s)) => {
                            let parsed = s
                                .parse::<Uuid>()
                                .map_err(|_| self.error(value.span, "malformed UUID"))?;
                            if uuid.replace(parsed).is_some() {
                                return Err(self.error(span, "`!Uuid` given twice"));
                            }
                        }
                        ("Name", NodeKind::Str(s)) => {
                            if s.is_empty() || s.contains(char::is_whitespace) {
                                return Err(self.error(
                                    value.span,
                                    "object names cannot be empty or contain spaces",
                                ));
                            }
                            if name.replace(s).is_some() {
                                return Err(self.error(span, "`!Name` given twice"));
                            }
                        }
                        ("Version", NodeKind::Int(v)) if (0..=u32::MAX as i128).contains(&v) => {
                            type_version = v as u32;
                        }
                        ("Uuid" | "Name", _) => {
                            return Err(
                                self.error(value.span, format!("`!{directive}` takes a string"))
                            );
                        }
                        ("Version", _) => {
                            return Err(self.error(value.span, "`!Version` takes a version number"));
                        }
                        _ => {
                            return Err(
                                self.error(span, format!("unknown directive `!{directive}`"))
                            );
                        }
                    }
                }
                Tok::Ident(_) => {
                    let attr = self.attr()?;
                    if !keys.insert(attr.key.clone()) {
                        return Err(self.error(attr.span, format!("`{}` is given twice", attr.key)));
                    }
                    attrs.push(attr);
                }
                _ => return Err(self.unexpected("an attribute or `}`")),
            }
            self.eat_comma()?;
        }
        let end = self.span.end as usize;
        self.bump()?;
        let span = Span::new(start, end);
        let uuid = uuid.ok_or_else(|| self.error(type_span, "object has no `!Uuid`"))?;
        let name = name.ok_or_else(|| self.error(type_span, "object has no `!Name`"))?;
        Ok(Object {
            type_name: type_name.to_owned(),
            uuid,
            name,
            type_version,
            attrs,
            span,
        })
    }

    fn attr(&mut self) -> Result<Attr, ParseError> {
        let Tok::Ident(key) = self.tok else {
            return Err(self.unexpected("an attribute name"));
        };
        let start = self.span.start as usize;
        self.bump()?;
        self.expect(Tok::Eq, "`=` after the attribute name")?;
        let value = self.value()?;
        Ok(Attr {
            key: key.to_owned(),
            span: Span::new(start, value.span.end as usize),
            value,
        })
    }

    /// `{ entries }` after the `{` has been seen; consumes the `}`.
    fn record_body(&mut self) -> Result<(Vec<Attr>, usize), ParseError> {
        self.bump()?;
        let mut attrs: Vec<Attr> = Vec::new();
        while self.tok != Tok::RBrace {
            let attr = self.attr()?;
            if attrs.iter().any(|a| a.key == attr.key) {
                return Err(self.error(attr.span, format!("`{}` is given twice", attr.key)));
            }
            attrs.push(attr);
            self.eat_comma()?;
        }
        let end = self.span.end as usize;
        self.bump()?;
        Ok((attrs, end))
    }

    /// Values up to `close`, after the opening bracket has been seen; consumes `close`.
    fn sequence(&mut self, close: Tok<'static>) -> Result<(Vec<Node>, usize), ParseError> {
        self.bump()?;
        let mut items = Vec::new();
        while self.tok != close {
            if matches!(
                self.tok,
                Tok::Eof | Tok::RBrace | Tok::RParen | Tok::RBracket
            ) {
                return Err(self.unexpected(&close.describe()));
            }
            items.push(self.value()?);
            self.eat_comma()?;
        }
        let end = self.span.end as usize;
        self.bump()?;
        Ok((items, end))
    }

    pub(crate) fn value(&mut self) -> Result<Node, ParseError> {
        self.enter()?;
        let node = self.value_inner();
        self.depth -= 1;
        node
    }

    fn value_inner(&mut self) -> Result<Node, ParseError> {
        let start = self.span.start as usize;
        let leaf = |kind, span: Span| Node { kind, span };
        let kind = match &self.tok {
            Tok::Int(v) => NodeKind::Int(*v),
            Tok::Float(v) => NodeKind::Float(*v),
            Tok::Str(s) => NodeKind::Str(s.clone().into_owned()),
            Tok::Link { path, name, uuid } => NodeKind::Link(LinkRef {
                path: (*path).to_owned(),
                name: (*name).to_owned(),
                uuid: *uuid,
            }),
            Tok::File { scheme, path } => NodeKind::File(FileRef {
                scheme: (*scheme).to_owned(),
                path: (*path).to_owned(),
            }),
            Tok::Ident(word) => {
                let word = *word;
                let span = self.span;
                self.bump()?;
                return Ok(match self.tok {
                    Tok::LParen => {
                        let (items, end) = self.sequence(Tok::RParen)?;
                        leaf(
                            NodeKind::Call(word.to_owned(), items),
                            Span::new(start, end),
                        )
                    }
                    Tok::LBrace => {
                        let (attrs, end) = self.record_body()?;
                        leaf(
                            NodeKind::Record(Some(word.to_owned()), attrs),
                            Span::new(start, end),
                        )
                    }
                    _ => leaf(
                        match word {
                            "true" => NodeKind::Bool(true),
                            "false" => NodeKind::Bool(false),
                            "inf" => NodeKind::Float(f64::INFINITY),
                            "nan" => NodeKind::Float(f64::NAN),
                            _ => NodeKind::Ident(word.to_owned()),
                        },
                        span,
                    ),
                });
            }
            Tok::LParen => {
                let (items, end) = self.sequence(Tok::RParen)?;
                return Ok(leaf(NodeKind::Tuple(items), Span::new(start, end)));
            }
            Tok::LBracket => {
                let (items, end) = self.sequence(Tok::RBracket)?;
                return Ok(leaf(NodeKind::List(items), Span::new(start, end)));
            }
            Tok::LBrace => {
                let (attrs, end) = self.record_body()?;
                return Ok(leaf(NodeKind::Record(None, attrs), Span::new(start, end)));
            }
            _ => return Err(self.unexpected("a value")),
        };
        let span = self.span;
        self.bump()?;
        Ok(leaf(kind, span))
    }
}

/// Parses a single value, as written after `=` (for tools and tests).
pub fn parse_value(text: &str) -> Result<Node, ParseError> {
    let mut p = Parser::new(text)?;
    let node = p.value()?;
    if p.tok != Tok::Eof {
        return Err(p.unexpected("the end of the value"));
    }
    Ok(node)
}
