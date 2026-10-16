//! The fast path for indexing: what every object has in common, without building trees or
//! binding types.
//!
//! The index scans every source file in the project, so this works straight from tokens and
//! borrows from the text: per object only its type, UUID, name, links and file links. It checks
//! the structure it walks (balanced brackets, the header, `!Uuid` and `!Name`) but not values;
//! [`parse`](crate::parse) is the full check.

use std::borrow::Cow;

use marlin_asset::Uuid;

use crate::ast::Span;
use crate::error::ParseError;
use crate::lex::{Lexer, Tok};

/// The indexed part of a file.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Extract<'a> {
    pub version: u32,
    pub objects: Vec<ExtractedObject<'a>>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ExtractedObject<'a> {
    pub type_name: &'a str,
    pub uuid: Uuid,
    pub name: Cow<'a, str>,
    pub links: Vec<ExtractedLink<'a>>,
    pub files: Vec<ExtractedFile<'a>>,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ExtractedLink<'a> {
    pub path: &'a str,
    pub name: &'a str,
    pub uuid: Option<Uuid>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ExtractedFile<'a> {
    pub scheme: &'a str,
    pub path: &'a str,
}

/// Scans `text` for its objects and links.
pub fn extract(text: &str) -> Result<Extract<'_>, ParseError> {
    let mut lexer = Lexer::new(text);
    let error = |span: Span, message: &str| ParseError::new(text, span, message);

    let (tok, span) = lexer.next()?;
    if tok != Tok::Directive("Version") {
        return Err(error(span, "expected `!Version` at the start of the file"));
    }
    let version = match lexer.next()? {
        (Tok::Int(v), _) if (0..=u32::MAX as i128).contains(&v) => v as u32,
        (_, span) => return Err(error(span, "expected a format version number")),
    };

    let mut objects = Vec::new();
    loop {
        let (tok, type_span) = lexer.next()?;
        let type_name = match tok {
            Tok::Eof => break,
            Tok::Ident(name) => name,
            _ => return Err(error(type_span, "expected an object type name")),
        };
        match lexer.next()? {
            (Tok::LBrace, _) => {}
            (_, span) => return Err(error(span, "expected `{` after the object type")),
        }
        let mut object = ExtractedObject {
            type_name,
            uuid: Uuid::NIL,
            name: Cow::Borrowed(""),
            links: Vec::new(),
            files: Vec::new(),
            span: type_span,
        };
        let mut has_uuid = false;
        // Open brackets inside the object, in order, to check they match.
        let mut stack: Vec<Tok<'_>> = Vec::new();
        loop {
            let (tok, span) = lexer.next()?;
            match tok {
                Tok::Eof => return Err(error(type_span, "object is not closed")),
                Tok::LBrace => stack.push(Tok::RBrace),
                Tok::LParen => stack.push(Tok::RParen),
                Tok::LBracket => stack.push(Tok::RBracket),
                Tok::RBrace | Tok::RParen | Tok::RBracket => match stack.pop() {
                    Some(expected) if expected == tok => {}
                    None if tok == Tok::RBrace => {
                        object.span.end = span.end;
                        break;
                    }
                    _ => return Err(error(span, "mismatched bracket")),
                },
                Tok::Link { path, name, uuid } => {
                    object.links.push(ExtractedLink { path, name, uuid })
                }
                Tok::File { scheme, path } => object.files.push(ExtractedFile { scheme, path }),
                Tok::Directive(directive) if stack.is_empty() => {
                    if lexer.next()?.0 != Tok::Eq {
                        return Err(error(span, "expected `=` after the directive"));
                    }
                    let (value, value_span) = lexer.next()?;
                    match (directive, value) {
                        ("Uuid", Tok::Str(s)) => {
                            object.uuid =
                                s.parse().map_err(|_| error(value_span, "malformed UUID"))?;
                            has_uuid = true;
                        }
                        ("Name", Tok::Str(s)) => object.name = s,
                        ("Version", Tok::Int(_)) => {}
                        _ => return Err(error(span, "malformed directive")),
                    }
                }
                _ => {}
            }
        }
        if !has_uuid {
            return Err(error(type_span, "object has no `!Uuid`"));
        }
        if object.name.is_empty() {
            return Err(error(type_span, "object has no `!Name`"));
        }
        objects.push(object);
    }
    Ok(Extract { version, objects })
}
