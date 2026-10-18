//! Reads the text format (0006 §13) back into a [`Module`]. Never panics: any text gives a
//! module or an error with a line and column.
//!
//! Besides what the printer writes, it accepts nested instructions as operands
//! (`desc.get (load (access @args, .Src))`, expanded into separate instructions before the one
//! that uses them), `//` comments, and any block labels. Values must be defined above their
//! uses; functions, structs and globals may be used before their definition.

use std::collections::HashMap;
use std::fmt;

use crate::func::{Block, FuncId, Function, Inst, Value};
use crate::infer::{result_types, step_type};
use crate::module::{
    Binding, Builtin, Const, ConstValue, Entry, GlobalData, GlobalId, Interp, Module, Stage,
};
use crate::num::f32_to_f16;
use crate::op::{
    AccessStep, AtomicOp, Axis, BinaryOp, ConvertOp, FloatCmp, IntCmp, MathOp, MemOrder, Op,
    QuadOp, ReduceOp, Scope, SwitchCase, TexArg, UnaryOp, VoteOp, WaveOp,
};
use crate::ty::{ResKind, ResourceKind, Scalar, Space, StructId, Type, TypeData};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for ParseError {}

pub fn parse_module(text: &str) -> Result<Module, ParseError> {
    let tokens = lex(text)?;
    let mut p = Parser {
        tokens,
        pos: 0,
        m: Module::new(),
        structs: HashMap::new(),
        globals: HashMap::new(),
        funcs: HashMap::new(),
        depth: 0,
    };
    p.module()?;
    Ok(p.m)
}

// ---- lexer ----

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Local(String),
    Global(String),
    /// An integer literal's text (sign included).
    Int(String),
    Float(String),
    Str(String),
    Punct(char),
    Arrow,
    Eof,
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    line: u32,
    col: u32,
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

fn lex(text: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut col) = (0, 1u32, 1u32);
    let err = |line, col, message: &str| ParseError {
        line,
        col,
        message: message.into(),
    };
    while i < chars.len() {
        let c = chars[i];
        let (start_line, start_col) = (line, col);
        let advance = |n: usize, i: &mut usize, col: &mut u32| {
            *i += n;
            *col += n as u32;
        };
        if c == '\n' {
            i += 1;
            line += 1;
            col = 1;
            continue;
        }
        if c.is_whitespace() {
            advance(1, &mut i, &mut col);
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        let tok = if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && is_name_char(chars[i]) {
                i += 1;
            }
            col += (i - start) as u32;
            Tok::Ident(chars[start..i].iter().collect())
        } else if c == '%' {
            let start = i + 1;
            i += 1;
            while i < chars.len() && is_name_char(chars[i]) {
                i += 1;
            }
            col += (i - start + 1) as u32;
            if i == start {
                return Err(err(start_line, start_col, "expected a name after `%`"));
            }
            Tok::Local(chars[start..i].iter().collect())
        } else if c == '@' {
            if chars.get(i + 1) == Some(&'"') {
                advance(1, &mut i, &mut col);
                let (s, n, lines) = string(&chars[i..])
                    .ok_or_else(|| err(start_line, start_col, "unterminated string"))?;
                i += n;
                line += lines;
                col += n as u32;
                Tok::Global(s)
            } else {
                let start = i + 1;
                i += 1;
                while i < chars.len() && is_name_char(chars[i]) {
                    i += 1;
                }
                col += (i - start + 1) as u32;
                if i == start {
                    return Err(err(start_line, start_col, "expected a name after `@`"));
                }
                Tok::Global(chars[start..i].iter().collect())
            }
        } else if c == '"' {
            let (s, n, lines) = string(&chars[i..])
                .ok_or_else(|| err(start_line, start_col, "unterminated string"))?;
            i += n;
            line += lines;
            col += n as u32;
            Tok::Str(s)
        } else if c == '-' && chars.get(i + 1) == Some(&'>') {
            advance(2, &mut i, &mut col);
            Tok::Arrow
        } else if c.is_ascii_digit()
            || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            if c == '-' {
                i += 1;
            }
            let mut float = false;
            if chars.get(i) == Some(&'0') && matches!(chars.get(i + 1), Some('x' | 'X')) {
                i += 2;
                while i < chars.len() && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(char::is_ascii_digit)
                {
                    float = true;
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if matches!(chars.get(i), Some('e' | 'E')) {
                    let mut j = i + 1;
                    if matches!(chars.get(j), Some('+' | '-')) {
                        j += 1;
                    }
                    if chars.get(j).is_some_and(char::is_ascii_digit) {
                        float = true;
                        i = j;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
            }
            col += (i - start) as u32;
            let s: String = chars[start..i].iter().collect();
            if float { Tok::Float(s) } else { Tok::Int(s) }
        } else if "()[]{}<>,:;=.".contains(c) {
            advance(1, &mut i, &mut col);
            Tok::Punct(c)
        } else {
            return Err(err(start_line, start_col, &format!("unexpected `{c}`")));
        };
        out.push(Token {
            tok,
            line: start_line,
            col: start_col,
        });
    }
    out.push(Token {
        tok: Tok::Eof,
        line,
        col,
    });
    Ok(out)
}

/// A quoted string at the start of `chars`: its value, its length in chars, and the line breaks
/// in it.
fn string(chars: &[char]) -> Option<(String, usize, u32)> {
    let mut out = String::new();
    let mut i = 1;
    let mut lines = 0;
    loop {
        match chars.get(i)? {
            '"' => return Some((out, i + 1, lines)),
            '\\' => {
                i += 1;
                match chars.get(i)? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'u' => {
                        if chars.get(i + 1)? != &'{' {
                            return None;
                        }
                        let start = i + 2;
                        let end = start + chars[start..].iter().position(|&c| c == '}')?;
                        let hex: String = chars[start..end].iter().collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                        i = end;
                    }
                    _ => return None,
                }
            }
            '\n' => {
                lines += 1;
                out.push('\n');
            }
            &c => out.push(c),
        }
        i += 1;
    }
}

// ---- parser ----

/// Nesting deeper than this (types, constants, nested instructions) is an error, not a stack
/// overflow.
const MAX_DEPTH: u32 = 128;

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    m: Module,
    structs: HashMap<String, StructId>,
    globals: HashMap<String, GlobalId>,
    funcs: HashMap<String, FuncId>,
    depth: u32,
}

/// A function header read in the first pass; its body is read in the second.
struct Pending {
    func: FuncId,
    params: Vec<(String, Type)>,
    body: usize,
}

/// The state of the function body being read.
struct Body {
    func: FuncId,
    values: HashMap<String, Value>,
    blocks: HashMap<String, Block>,
    block: Option<Block>,
}

type R<T> = Result<T, ParseError>;

impl Parser {
    // ---- tokens ----

    fn peek(&self) -> &Tok {
        &self.tokens[self.pos.min(self.tokens.len() - 1)].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)].tok
    }

    fn token(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn line(&self) -> u32 {
        self.token().line
    }

    fn bump(&mut self) -> Tok {
        let t = self.peek().clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn error<T>(&self, message: impl Into<String>) -> R<T> {
        let t = self.token();
        Err(ParseError {
            line: t.line,
            col: t.col,
            message: message.into(),
        })
    }

    fn at_punct(&self, c: char) -> bool {
        *self.peek() == Tok::Punct(c)
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.at_punct(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, c: char) -> R<()> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            self.error(format!("expected `{c}`"))
        }
    }

    fn at_word(&self, w: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s == w)
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.at_word(w) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_word(&mut self, w: &str) -> R<()> {
        if self.eat_word(w) {
            Ok(())
        } else {
            self.error(format!("expected `{w}`"))
        }
    }

    fn ident(&mut self) -> R<String> {
        match self.bump() {
            Tok::Ident(s) => Ok(s),
            _ => {
                self.pos = self.pos.saturating_sub(1);
                self.error("expected a word")
            }
        }
    }

    fn global_name(&mut self) -> R<String> {
        match self.bump() {
            Tok::Global(s) => Ok(s),
            _ => {
                self.pos = self.pos.saturating_sub(1);
                self.error("expected an `@` name")
            }
        }
    }

    /// A field name after `.`: a word or a quoted string.
    fn field_name(&mut self) -> R<String> {
        match self.bump() {
            Tok::Ident(s) | Tok::Str(s) => Ok(s),
            _ => {
                self.pos = self.pos.saturating_sub(1);
                self.error("expected a field name")
            }
        }
    }

    fn int(&mut self) -> R<i128> {
        match self.peek().clone() {
            Tok::Int(s) => {
                let v = parse_int(&s);
                match v {
                    Some(v) => {
                        self.bump();
                        Ok(v)
                    }
                    None => self.error("integer out of range"),
                }
            }
            _ => self.error("expected an integer"),
        }
    }

    fn u32(&mut self) -> R<u32> {
        let v = self.int()?;
        match u32::try_from(v) {
            Ok(v) => Ok(v),
            Err(_) => {
                self.pos = self.pos.saturating_sub(1);
                self.error("expected a small non-negative integer")
            }
        }
    }

    /// `word(x)`
    fn paren_word(&mut self, word: &str) -> R<String> {
        self.expect_word(word)?;
        self.expect_punct('(')?;
        let w = self.ident()?;
        self.expect_punct(')')?;
        Ok(w)
    }

    fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.error("nested too deeply");
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // ---- module ----

    fn module(&mut self) -> R<()> {
        // Pass 0: struct names, so types can name structs defined further down.
        let mut depth = 0i64;
        for i in 0..self.tokens.len() {
            match &self.tokens[i].tok {
                Tok::Punct('{') => depth += 1,
                Tok::Punct('}') => depth -= 1,
                Tok::Ident(w) if w == "struct" && depth == 0 => {
                    if let Some(Token {
                        tok: Tok::Global(name),
                        ..
                    }) = self.tokens.get(i + 1)
                    {
                        if self.structs.contains_key(name) {
                            self.pos = i + 1;
                            return self.error(format!("struct `{name}` is defined twice"));
                        }
                        let id = self.m.add_struct(name, Vec::new());
                        self.structs.insert(name.clone(), id);
                    }
                }
                _ => {}
            }
        }

        // Pass 1: everything but function bodies.
        let mut pending = Vec::new();
        let mut formats: Vec<Option<Box<str>>> = Vec::new();
        loop {
            match self.peek().clone() {
                Tok::Eof => break,
                Tok::Ident(w) => match w.as_str() {
                    "format" => {
                        self.bump();
                        let index = self.u32()? as usize;
                        self.expect_punct('=')?;
                        let Tok::Str(s) = self.bump() else {
                            self.pos = self.pos.saturating_sub(1);
                            return self.error("expected a string");
                        };
                        if index > 1 << 16 {
                            return self.error("format index too large");
                        }
                        if formats.len() <= index {
                            formats.resize(index + 1, None);
                        }
                        if formats[index].is_some() {
                            return self.error(format!("format {index} is defined twice"));
                        }
                        formats[index] = Some(s.into());
                    }
                    "struct" => self.struct_def()?,
                    "global" => self.global_def()?,
                    "func" | "entry" => pending.push(self.func_header()?),
                    _ => return self.error(format!("unexpected `{w}`")),
                },
                _ => return self.error("expected `struct`, `global`, `func`, `entry` or `format`"),
            }
        }
        for (i, f) in formats.into_iter().enumerate() {
            match f {
                Some(f) => self.m.formats.push(f),
                None => {
                    return Err(ParseError {
                        line: 1,
                        col: 1,
                        message: format!("format {i} is missing"),
                    });
                }
            }
        }

        // Pass 2: function bodies.
        for p in pending {
            self.pos = p.body;
            self.body(p)?;
        }
        Ok(())
    }

    fn struct_def(&mut self) -> R<()> {
        self.expect_word("struct")?;
        let name = self.global_name()?;
        let Some(&id) = self.structs.get(&name) else {
            return self.error(format!("struct `{name}` is not at the top level"));
        };
        self.expect_punct('{')?;
        let mut fields = Vec::new();
        while !self.at_punct('}') {
            let field = self.field_name()?;
            self.expect_punct(':')?;
            let ty = self.ty()?;
            fields.push((field.into(), ty));
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct('}')?;
        self.m.structs[id].fields = fields;
        Ok(())
    }

    fn global_def(&mut self) -> R<()> {
        self.expect_word("global")?;
        let name = self.global_name()?;
        if self.globals.contains_key(&name) {
            return self.error(format!("global `{name}` is defined twice"));
        }
        self.expect_punct(':')?;
        let type_start = self.pos;
        let ty = self.ty()?;
        let TypeData::Ptr { pointee, .. } = self.m.data(ty).clone() else {
            self.pos = type_start;
            return self.error("a global's type is a pointer");
        };
        let mut global = GlobalData {
            name: name.as_str().into(),
            ty,
            binding: Binding::None,
            interp: None,
            init: None,
        };
        loop {
            if self.at_word("builtin") {
                let w = self.paren_word("builtin")?;
                match Builtin::from_name(&w) {
                    Some(b) => global.binding = Binding::Builtin(b),
                    None => return self.error(format!("unknown builtin `{w}`")),
                }
            } else if self.eat_word("location") {
                self.expect_punct('(')?;
                global.binding = Binding::Location(self.u32()?);
                self.expect_punct(')')?;
            } else if self.eat_word("target") {
                self.expect_punct('(')?;
                let index = self.u32()?;
                let mut source = 0;
                if self.eat_punct(',') {
                    if !self.eat_word("source") {
                        return self.error("expected `source` in `target(i, source s)`");
                    }
                    source = self.u32()?;
                }
                global.binding = Binding::Target(index, source);
                self.expect_punct(')')?;
            } else if self.at_word("interp") {
                let w = self.paren_word("interp")?;
                match Interp::from_name(&w) {
                    Some(i) => global.interp = Some(i),
                    None => return self.error(format!("unknown interpolation `{w}`")),
                }
            } else if self.eat_punct('=') {
                global.init = Some(self.lit(pointee)?);
            } else {
                break;
            }
        }
        let id = self.m.add_global(global);
        self.globals.insert(name, id);
        Ok(())
    }

    fn func_header(&mut self) -> R<Pending> {
        let is_entry = self.eat_word("entry");
        if !is_entry {
            self.expect_word("func")?;
        }
        let mut stage = None;
        let mut uses = Vec::new();
        let mut params = Vec::new();
        let mut results = Vec::new();
        if is_entry {
            let word = self.ident()?;
            let size = |p: &mut Self| -> R<[u32; 3]> {
                p.expect_punct('(')?;
                let x = p.u32()?;
                p.expect_punct(',')?;
                let y = p.u32()?;
                p.expect_punct(',')?;
                let z = p.u32()?;
                p.expect_punct(')')?;
                Ok([x, y, z])
            };
            stage = Some(match word.as_str() {
                "compute" => Stage::Compute(size(self)?),
                "mesh" => Stage::Mesh(size(self)?),
                "amplification" => Stage::Amplification(size(self)?),
                "vertex" => Stage::Vertex,
                "pixel" => Stage::Pixel,
                _ => return self.error(format!("unknown stage `{word}`")),
            });
        }
        let name = self.global_name()?;
        if self.funcs.contains_key(&name) {
            return self.error(format!("function `{name}` is defined twice"));
        }
        if is_entry {
            self.expect_word("uses")?;
            self.expect_punct('(')?;
            while !self.at_punct(')') {
                let g = self.global_name()?;
                match self.globals.get(&g) {
                    Some(&id) => uses.push(id),
                    None => return self.error(format!("no global `{g}` (define it above)")),
                }
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')')?;
        } else {
            self.expect_punct('(')?;
            while !self.at_punct(')') {
                let Tok::Local(p) = self.bump() else {
                    self.pos = self.pos.saturating_sub(1);
                    return self.error("expected a parameter `%name: type`");
                };
                self.expect_punct(':')?;
                let ty = self.ty()?;
                params.push((p, ty));
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')')?;
            if *self.peek() == Tok::Arrow {
                self.bump();
                if self.eat_punct('(') {
                    while !self.at_punct(')') {
                        results.push(self.ty()?);
                        if !self.eat_punct(',') {
                            break;
                        }
                    }
                    self.expect_punct(')')?;
                } else {
                    results.push(self.ty()?);
                }
            }
        }
        let func = self.m.add_function(Function::new(&name, results));
        self.funcs.insert(name, func);
        if let Some(stage) = stage {
            self.m.entries.push(Entry { func, stage, uses });
        }
        // Skip the body; pass 2 reads it.
        if !self.at_punct('{') {
            return self.error("expected `{`");
        }
        let body = self.pos;
        let mut depth = 0;
        loop {
            match self.bump() {
                Tok::Punct('{') => depth += 1,
                Tok::Punct('}') => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Eof => return self.error("unterminated function body"),
                _ => {}
            }
        }
        Ok(Pending { func, params, body })
    }

    // ---- types and constants ----

    fn ty(&mut self) -> R<Type> {
        self.enter()?;
        let t = self.ty_inner();
        self.leave();
        t
    }

    fn ty_inner(&mut self) -> R<Type> {
        match self.peek().clone() {
            Tok::Global(name) => {
                self.bump();
                match self.structs.get(&name) {
                    Some(&s) => Ok(self.m.struct_ty(s)),
                    None => self.error(format!("no struct `{name}`")),
                }
            }
            Tok::Punct('[') => {
                self.bump();
                let e = self.ty()?;
                self.expect_punct(';')?;
                let n = self.u32()?;
                self.expect_punct(']')?;
                Ok(self.m.ty(TypeData::Array(e, n)))
            }
            Tok::Ident(w) => {
                self.bump();
                match w.as_str() {
                    "ptr" => {
                        self.expect_punct('<')?;
                        let space = self.ident()?;
                        let Some(space) = Space::from_name(&space) else {
                            return self.error(format!("unknown address space `{space}`"));
                        };
                        let readonly = self.eat_word("ro");
                        self.expect_punct(',')?;
                        let pointee = self.ty()?;
                        self.expect_punct('>')?;
                        Ok(self.m.ptr(space, readonly, pointee))
                    }
                    "res" | "handle" => {
                        self.expect_punct('<')?;
                        let kind = self.ident()?;
                        let Some(kind) = ResourceKind::from_name(&kind) else {
                            return self.error(format!("unknown resource kind `{kind}`"));
                        };
                        let element = if kind.has_element() {
                            self.expect_punct('<')?;
                            let e = self.ty()?;
                            self.expect_punct('>')?;
                            Some(e)
                        } else {
                            None
                        };
                        self.expect_punct('>')?;
                        let k = ResKind { kind, element };
                        Ok(self.m.ty(if w == "res" {
                            TypeData::Res(k)
                        } else {
                            TypeData::Handle(k)
                        }))
                    }
                    "rayquery" => Ok(self.m.ty(TypeData::RayQuery)),
                    _ => match scalar_shape(&w) {
                        Some((s, None)) => Ok(self.m.scalar(s)),
                        Some((s, Some((n, None)))) => Ok(self.m.ty(TypeData::Vector(s, n))),
                        Some((s, Some((rows, Some(cols))))) => Ok(self.m.ty(TypeData::Matrix {
                            element: s,
                            rows,
                            cols,
                        })),
                        None => {
                            self.pos = self.pos.saturating_sub(1);
                            self.error(format!("unknown type `{w}`"))
                        }
                    },
                }
            }
            _ => self.error("expected a type"),
        }
    }

    /// A constant literal of type `ty`.
    fn lit(&mut self, ty: Type) -> R<Const> {
        self.enter()?;
        let c = self.lit_inner(ty);
        self.leave();
        c
    }

    fn lit_inner(&mut self, ty: Type) -> R<Const> {
        if self.eat_word("zero") {
            return Ok(self.m.constant(ty, ConstValue::Zero));
        }
        if self.eat_word("any") {
            return Ok(self.m.constant(ty, ConstValue::Any));
        }
        let data = self.m.data(ty).clone();
        if self.eat_punct('(') {
            let parts: Vec<Type> = match &data {
                TypeData::Vector(s, n) => {
                    let e = self.m.scalar(*s);
                    vec![e; *n as usize]
                }
                TypeData::Matrix {
                    element,
                    rows,
                    cols,
                } => {
                    let column = self.m.vector(*element, *rows);
                    vec![column; *cols as usize]
                }
                TypeData::Array(e, n) => {
                    if *n > 1 << 16 {
                        return self.error("array constant too long");
                    }
                    vec![*e; *n as usize]
                }
                TypeData::Struct(s) => self.m.structs[*s].fields.iter().map(|f| f.1).collect(),
                _ => return self.error("this type has no composite constants"),
            };
            let mut items = Vec::new();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    self.expect_punct(',')?;
                }
                items.push(self.lit(*part)?);
            }
            self.expect_punct(')')?;
            return Ok(self.m.constant(ty, ConstValue::Composite(items)));
        }
        let TypeData::Scalar(s) = data else {
            return self.error("expected `zero`, `any` or `(...)` for this type");
        };
        let value = match (s, self.peek().clone()) {
            (Scalar::I1, Tok::Ident(w)) if w == "true" || w == "false" => {
                self.bump();
                ConstValue::Int(u64::from(w == "true"))
            }
            (s, Tok::Int(text)) if s.is_int() => {
                let Some(v) = parse_int(&text) else {
                    return self.error("integer out of range");
                };
                self.bump();
                let bits = s.bits();
                let mask = if bits == 64 {
                    u64::MAX
                } else {
                    (1u64 << bits) - 1
                };
                ConstValue::Int((v as u64) & mask)
            }
            (s, Tok::Int(text)) if is_hex(&text) => {
                // Raw bits of a float.
                let Some(v) = parse_int(&text) else {
                    return self.error("integer out of range");
                };
                self.bump();
                let mask = if s.bits() == 64 {
                    u64::MAX
                } else {
                    (1u64 << s.bits()) - 1
                };
                ConstValue::Float((v as u64) & mask)
            }
            (s, Tok::Int(text) | Tok::Float(text)) => {
                let bits = match s {
                    Scalar::F16 => text.parse::<f32>().ok().map(|x| u64::from(f32_to_f16(x))),
                    Scalar::F32 => text.parse::<f32>().ok().map(|x| u64::from(x.to_bits())),
                    _ => text.parse::<f64>().ok().map(f64::to_bits),
                };
                let Some(bits) = bits else {
                    return self.error("bad number");
                };
                self.bump();
                ConstValue::Float(bits)
            }
            _ => return self.error(format!("expected a `{}` literal", s.name())),
        };
        Ok(self.m.constant(ty, value))
    }

    // ---- function bodies ----

    fn body(&mut self, p: Pending) -> R<()> {
        self.expect_punct('{')?;
        let mut b = Body {
            func: p.func,
            values: HashMap::new(),
            blocks: HashMap::new(),
            block: None,
        };
        // Labels first, so jumps can go forward.
        let start = self.pos;
        let mut first = true;
        while *self.peek() != Tok::Punct('}') && *self.peek() != Tok::Eof {
            let at_label = self.at_label();
            if at_label {
                let Tok::Ident(name) = self.bump() else {
                    unreachable!()
                };
                if b.blocks.contains_key(&name) {
                    return self.error(format!("block `{name}` is defined twice"));
                }
                let block = self.m.funcs[p.func].add_block();
                b.blocks.insert(name, block);
                if first {
                    for (name, ty) in &p.params {
                        let hint = numeric_free(name);
                        let v = self.m.funcs[p.func].add_param(block, *ty, hint);
                        if b.values.insert(name.clone(), v).is_some() {
                            return self.error(format!("`%{name}` is defined twice"));
                        }
                    }
                    first = false;
                }
            } else {
                self.bump();
            }
        }
        if first && !p.params.is_empty() {
            return self.error("a function with parameters needs an entry block");
        }
        self.pos = start;
        while !self.at_punct('}') {
            if *self.peek() == Tok::Eof {
                return self.error("unterminated function body");
            }
            let at_label = self.at_label();
            if at_label {
                self.label(&mut b)?;
            } else {
                if b.block.is_none() {
                    return self.error("an instruction before the first block label");
                }
                self.statement(&mut b)?;
            }
        }
        self.bump();
        Ok(())
    }

    /// Whether a block label starts here: at the start of a line, `name:` or `name(...):`
    /// (an instruction such as `store (...), %v` has no `:` after its parenthesis).
    fn at_label(&self) -> bool {
        if !matches!(self.peek(), Tok::Ident(_)) || !self.statement_start() {
            return false;
        }
        match self.peek_at(1) {
            Tok::Punct(':') => true,
            Tok::Punct('(') => {
                // The matching `)` on this line, then `:`.
                let line = self.token().line;
                let mut depth = 0;
                for (i, t) in self.tokens[self.pos + 1..].iter().enumerate() {
                    if t.line != line {
                        return false;
                    }
                    match t.tok {
                        Tok::Punct('(') => depth += 1,
                        Tok::Punct(')') => {
                            depth -= 1;
                            if depth == 0 {
                                return matches!(
                                    self.tokens.get(self.pos + 2 + i).map(|t| &t.tok),
                                    Some(Tok::Punct(':'))
                                );
                            }
                        }
                        _ => {}
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Whether the current token starts a line (labels and instructions start lines).
    fn statement_start(&self) -> bool {
        self.pos == 0 || self.tokens[self.pos - 1].line != self.tokens[self.pos].line
    }

    fn label(&mut self, b: &mut Body) -> R<()> {
        let name = self.ident()?;
        let Some(&block) = b.blocks.get(&name) else {
            return self.error(format!("no block `{name}`"));
        };
        let is_entry = self.m.funcs[b.func].entry() == Some(block);
        if self.eat_punct('(') {
            if is_entry {
                return self.error("the entry block's parameters are the function's");
            }
            while !self.at_punct(')') {
                let Tok::Local(v) = self.bump() else {
                    self.pos = self.pos.saturating_sub(1);
                    return self.error("expected `%name: type`");
                };
                self.expect_punct(':')?;
                let ty = self.ty()?;
                let value = self.m.funcs[b.func].add_param(block, ty, numeric_free(&v));
                self.define(b, v, value)?;
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct(')')?;
        }
        self.expect_punct(':')?;
        b.block = Some(block);
        Ok(())
    }

    fn define(&self, b: &mut Body, name: String, value: Value) -> R<()> {
        if b.values.insert(name.clone(), value).is_some() {
            return self.error(format!("`%{name}` is defined twice"));
        }
        Ok(())
    }

    fn statement(&mut self, b: &mut Body) -> R<()> {
        let line = self.line();
        let start = self.pos;
        let mut names = Vec::new();
        if matches!(self.peek(), Tok::Local(_)) {
            loop {
                let Tok::Local(n) = self.bump() else {
                    self.pos = self.pos.saturating_sub(1);
                    return self.error("expected `%name`");
                };
                names.push(n);
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.expect_punct('=')?;
        }
        let inst = self.instruction(b, line)?;
        let results = self.m.funcs[b.func].insts[inst].results.clone();
        if results.len() != names.len() {
            self.pos = start;
            return self.error(format!(
                "this instruction has {} result(s), {} named",
                results.len(),
                names.len()
            ));
        }
        for (name, value) in names.into_iter().zip(results) {
            self.m.funcs[b.func].set_name(value, numeric_free(&name));
            self.define(b, name, value)?;
        }
        if self.line() == line && !self.at_punct('}') && *self.peek() != Tok::Eof {
            return self.error("expected the end of the line");
        }
        Ok(())
    }

    /// Whether the statement that started on `line` has more on its line.
    fn more_on_line(&self, line: u32) -> bool {
        self.line() == line
            && !self.at_punct('}')
            && !self.at_punct(')')
            && *self.peek() != Tok::Eof
    }

    fn operand(&mut self, b: &mut Body, line: u32) -> R<Value> {
        self.enter()?;
        let v = self.operand_inner(b, line);
        self.leave();
        v
    }

    fn operand_inner(&mut self, b: &mut Body, line: u32) -> R<Value> {
        match self.peek().clone() {
            Tok::Local(name) => {
                self.bump();
                match b.values.get(&name) {
                    Some(&v) => Ok(v),
                    None => {
                        self.pos = self.pos.saturating_sub(1);
                        self.error(format!("`%{name}` is not defined above"))
                    }
                }
            }
            Tok::Global(name)
                if !matches!(self.peek_at(1), Tok::Punct('(')) && !self.at_lit_word(1) =>
            {
                self.bump();
                match self.globals.get(&name) {
                    Some(&g) => {
                        let ty = self.m.globals[g].ty;
                        Ok(self.m.funcs[b.func].global_value(g, ty))
                    }
                    None => {
                        self.pos = self.pos.saturating_sub(1);
                        self.error(format!("no global `{name}`"))
                    }
                }
            }
            Tok::Punct('(') => {
                // A nested instruction, placed before the one that uses it.
                self.bump();
                let inst = self.instruction(b, line)?;
                self.expect_punct(')')?;
                let results = &self.m.funcs[b.func].insts[inst].results;
                if results.len() != 1 {
                    return self.error("a nested instruction must have one result");
                }
                Ok(results[0])
            }
            _ => {
                let ty = self.ty()?;
                let c = self.lit(ty)?;
                Ok(self.m.funcs[b.func].const_value(c, ty))
            }
        }
    }

    fn at_lit_word(&self, n: usize) -> bool {
        matches!(self.peek_at(n), Tok::Ident(w) if w == "zero" || w == "any")
    }

    /// Operands separated by commas, up to the end of the line (or a closing parenthesis).
    fn operands_to_end(&mut self, b: &mut Body, line: u32) -> R<Vec<Value>> {
        let mut out = Vec::new();
        if !self.more_on_line(line) {
            return Ok(out);
        }
        loop {
            out.push(self.operand(b, line)?);
            if !self.eat_punct(',') {
                return Ok(out);
            }
        }
    }

    /// Exactly `n` operands separated by commas.
    fn operands(&mut self, b: &mut Body, line: u32, n: usize) -> R<Vec<Value>> {
        let mut out = Vec::new();
        for i in 0..n {
            if i > 0 {
                self.expect_punct(',')?;
            }
            out.push(self.operand(b, line)?);
        }
        Ok(out)
    }

    /// `( operands )`
    fn paren_operands(&mut self, b: &mut Body, line: u32) -> R<Vec<Value>> {
        self.expect_punct('(')?;
        let mut out = Vec::new();
        while !self.at_punct(')') {
            out.push(self.operand(b, line)?);
            if !self.eat_punct(',') {
                break;
            }
        }
        self.expect_punct(')')?;
        Ok(out)
    }

    /// `label` or `label(args)`.
    fn target(&mut self, b: &mut Body, line: u32) -> R<(Block, Vec<Value>)> {
        let block = self.block_ref(b)?;
        let args = if self.at_punct('(') {
            self.paren_operands(b, line)?
        } else {
            Vec::new()
        };
        Ok((block, args))
    }

    fn block_ref(&mut self, b: &Body) -> R<Block> {
        let name = self.ident()?;
        match b.blocks.get(&name) {
            Some(&block) => Ok(block),
            None => {
                self.pos = self.pos.saturating_sub(1);
                self.error(format!("no block `{name}`"))
            }
        }
    }

    fn emit(
        &mut self,
        b: &Body,
        start: usize,
        op: Op,
        operands: Vec<Value>,
        explicit: Option<Type>,
    ) -> R<Inst> {
        let block = b.block.expect("checked by the caller");
        let types: Vec<Type> = operands
            .iter()
            .map(|&v| self.m.funcs[b.func].ty(v))
            .collect();
        let results = match result_types(&mut self.m, &op, &types, explicit) {
            Ok(r) => r,
            Err(e) => {
                self.pos = start;
                return self.error(e);
            }
        };
        let f = &mut self.m.funcs[b.func];
        let inst = f.create_inst(op, operands, &results);
        f.append_inst(block, inst);
        Ok(inst)
    }

    fn instruction(&mut self, b: &mut Body, line: u32) -> R<Inst> {
        self.enter()?;
        let inst = self.instruction_inner(b, line);
        self.leave();
        inst
    }

    fn instruction_inner(&mut self, b: &mut Body, line: u32) -> R<Inst> {
        let start = self.pos;
        let word = self.ident()?;
        let precise = |p: &mut Self| p.eat_word("precise");
        let mut is_precise = false;
        let (op, operands, explicit) = if let Some(op) = BinaryOp::from_name(&word) {
            is_precise = precise(self);
            (Op::Binary(op), self.operands(b, line, 2)?, None)
        } else if let Some(op) = UnaryOp::from_name(&word) {
            is_precise = precise(self);
            (Op::Unary(op), self.operands(b, line, 1)?, None)
        } else if let Some(op) = ConvertOp::from_name(&word) {
            is_precise = precise(self);
            let operands = self.operands(b, line, 1)?;
            self.expect_punct(':')?;
            let ty = self.ty()?;
            (Op::Convert(op), operands, Some(ty))
        } else if let Some(op) = MathOp::from_name(&word) {
            is_precise = precise(self);
            (Op::Math(op), self.operands(b, line, op.arity())?, None)
        } else {
            match word.as_str() {
                "icmp" => {
                    let cc = self.ident()?;
                    let Some(cc) = IntCmp::from_name(&cc) else {
                        return self.error(format!("unknown integer comparison `{cc}`"));
                    };
                    (Op::Icmp(cc), self.operands(b, line, 2)?, None)
                }
                "fcmp" => {
                    is_precise = precise(self);
                    let cc = self.ident()?;
                    let Some(cc) = FloatCmp::from_name(&cc) else {
                        return self.error(format!("unknown float comparison `{cc}`"));
                    };
                    (Op::Fcmp(cc), self.operands(b, line, 2)?, None)
                }
                "select" => (Op::Select, self.operands(b, line, 3)?, None),
                "splat" => {
                    let operands = self.operands(b, line, 1)?;
                    self.expect_punct(':')?;
                    let ty = self.ty()?;
                    (Op::Splat, operands, Some(ty))
                }
                "extract" => {
                    let operands = self.operands(b, line, 1)?;
                    self.expect_punct(',')?;
                    (Op::Extract(self.u32()?), operands, None)
                }
                "insert" => {
                    let mut operands = self.operands(b, line, 1)?;
                    self.expect_punct(',')?;
                    let i = self.u32()?;
                    self.expect_punct(',')?;
                    operands.push(self.operand(b, line)?);
                    (Op::Insert(i), operands, None)
                }
                "shuffle" => {
                    let operands = self.operands(b, line, 2)?;
                    self.expect_punct(',')?;
                    self.expect_punct('[')?;
                    let mut lanes = Vec::new();
                    while !self.at_punct(']') {
                        lanes.push(self.u32()?);
                        if !self.eat_punct(',') {
                            break;
                        }
                    }
                    self.expect_punct(']')?;
                    (Op::Shuffle(lanes), operands, None)
                }
                "extract.dyn" => (Op::ExtractDyn, self.operands(b, line, 2)?, None),
                "insert.dyn" => (Op::InsertDyn, self.operands(b, line, 3)?, None),
                "matmul" => {
                    is_precise = precise(self);
                    (Op::MatMul, self.operands(b, line, 2)?, None)
                }
                "transpose" => (Op::Transpose, self.operands(b, line, 1)?, None),
                "assume.uniform" => (Op::AssumeUniform, self.operands(b, line, 1)?, None),
                "assume.nonuniform" => (Op::AssumeNonUniform, self.operands(b, line, 1)?, None),
                "mat.column" | "mat.row" => {
                    let operands = self.operands(b, line, 1)?;
                    self.expect_punct(',')?;
                    let i = self.u32()?;
                    let op = if word == "mat.column" {
                        Op::MatColumn(i)
                    } else {
                        Op::MatRow(i)
                    };
                    (op, operands, None)
                }
                "mat.extract" => {
                    let operands = self.operands(b, line, 1)?;
                    self.expect_punct(',')?;
                    let r = self.u32()?;
                    self.expect_punct(',')?;
                    let c = self.u32()?;
                    (Op::MatExtract(r, c), operands, None)
                }
                "construct" => {
                    let ty = self.ty()?;
                    let operands = self.paren_operands(b, line)?;
                    (Op::Construct, operands, Some(ty))
                }
                "extract.field" | "insert.field" => {
                    let mut operands = self.operands(b, line, 1)?;
                    self.expect_punct(',')?;
                    self.expect_punct('.')?;
                    let ty = self.m.funcs[b.func].ty(operands[0]);
                    let i = self.field_index(ty)?;
                    let op = if word == "extract.field" {
                        Op::ExtractField(i)
                    } else {
                        self.expect_punct(',')?;
                        operands.push(self.operand(b, line)?);
                        Op::InsertField(i)
                    };
                    (op, operands, None)
                }
                "extract.elem" | "insert.elem" => {
                    let mut operands = self.operands(b, line, 1)?;
                    self.expect_punct(',')?;
                    let i = self.u32()?;
                    let op = if word == "extract.elem" {
                        Op::ExtractElem(i)
                    } else {
                        self.expect_punct(',')?;
                        operands.push(self.operand(b, line)?);
                        Op::InsertElem(i)
                    };
                    (op, operands, None)
                }
                "var" => {
                    self.expect_word("function")?;
                    self.expect_punct(':')?;
                    let pointee = self.ty()?;
                    let ty = self.m.ptr(Space::Function, false, pointee);
                    (Op::Var, Vec::new(), Some(ty))
                }
                "load" => {
                    let operands = self.operands(b, line, 1)?;
                    let aligned = self.aligned()?;
                    (Op::Load { aligned }, operands, None)
                }
                "store" => {
                    let operands = self.operands(b, line, 2)?;
                    let aligned = self.aligned()?;
                    (Op::Store { aligned }, operands, None)
                }
                "access" => self.access(b, line)?,
                "buffer.elem" => (Op::BufferElem, self.operands(b, line, 2)?, None),
                "tex.sample" => self.tex(b, line, 3, Op::TexSample)?,
                "tex.load" => self.tex(b, line, 2, Op::TexLoad)?,
                "tex.store" => self.tex(b, line, 3, Op::TexStore)?,
                "buffer.byte" => (Op::BufferByte, self.operands(b, line, 2)?, None),
                "uniform.ptr" => (Op::UniformPtr, self.operands(b, line, 1)?, None),
                "buffer.length" => (Op::BufferLength, self.operands(b, line, 1)?, None),
                "desc.get" => {
                    let operands = self.operands(b, line, 1)?;
                    let nonuniform = self.eat_word("nonuniform");
                    (Op::DescGet { nonuniform }, operands, None)
                }
                "barrier" => {
                    let exec = self.paren_word("exec")?;
                    let exec = match exec.as_str() {
                        "none" => None,
                        w => match Scope::from_name(w) {
                            Some(s) => Some(s),
                            None => return self.error(format!("unknown scope `{w}`")),
                        },
                    };
                    let mem = self.paren_word("mem")?;
                    let Some(mem) = Scope::from_name(&mem) else {
                        return self.error(format!("unknown scope `{mem}`"));
                    };
                    self.expect_word("spaces")?;
                    self.expect_punct('(')?;
                    let mut spaces = Vec::new();
                    while !self.at_punct(')') {
                        let s = self.ident()?;
                        match Space::from_name(&s) {
                            Some(s) => spaces.push(s),
                            None => return self.error(format!("unknown address space `{s}`")),
                        }
                        if !self.eat_punct(',') {
                            break;
                        }
                    }
                    self.expect_punct(')')?;
                    (Op::Barrier { exec, mem, spaces }, Vec::new(), None)
                }
                "discard" => (Op::Discard, Vec::new(), None),
                "is_helper" => (Op::IsHelper, Vec::new(), None),
                "deriv.ddx" | "deriv.ddy" => {
                    let axis = if word == "deriv.ddx" {
                        Axis::X
                    } else {
                        Axis::Y
                    };
                    let fine = match self.ident()?.as_str() {
                        "fine" => true,
                        "coarse" => false,
                        _ => return self.error("expected `coarse` or `fine`"),
                    };
                    (Op::Deriv { axis, fine }, self.operands(b, line, 1)?, None)
                }
                "call" => {
                    let name = self.global_name()?;
                    let Some(&f) = self.funcs.get(&name) else {
                        return self.error(format!("no function `{name}`"));
                    };
                    (Op::Call(f), self.paren_operands(b, line)?, None)
                }
                "debug.print" => {
                    let fmt = self.u32()?;
                    self.expect_punct(',')?;
                    (Op::DebugPrint(fmt), self.paren_operands(b, line)?, None)
                }
                "debug.assert" => {
                    let mut operands = self.operands(b, line, 1)?;
                    self.expect_punct(',')?;
                    let fmt = self.u32()?;
                    self.expect_punct(',')?;
                    operands.extend(self.paren_operands(b, line)?);
                    (Op::DebugAssert(fmt), operands, None)
                }
                "br" => {
                    let (block, args) = self.target(b, line)?;
                    (Op::Br(block), args, None)
                }
                "cond_br" => {
                    let mut operands = self.operands(b, line, 1)?;
                    self.expect_punct(',')?;
                    let (then_block, then_args) = self.target(b, line)?;
                    self.expect_punct(',')?;
                    let (else_block, else_args) = self.target(b, line)?;
                    self.expect_word("merge")?;
                    let merge = self.block_ref(b)?;
                    let n = then_args.len() as u32;
                    operands.extend(then_args);
                    operands.extend(else_args);
                    let op = Op::CondBr {
                        then_block,
                        then_args: n,
                        else_block,
                        merge,
                    };
                    (op, operands, None)
                }
                "switch" => {
                    let mut operands = self.operands(b, line, 1)?;
                    let width = {
                        let ty = self.m.funcs[b.func].ty(operands[0]);
                        self.m.element_scalar(ty).map_or(64, |s| s.bits())
                    };
                    self.expect_punct('[')?;
                    let mut cases = Vec::new();
                    while !self.at_punct(']') {
                        let value = self.int()?;
                        self.expect_punct(':')?;
                        let (block, args) = self.target(b, line)?;
                        let mask = if width >= 64 {
                            u64::MAX
                        } else {
                            (1u64 << width) - 1
                        };
                        cases.push(SwitchCase {
                            value: (value as u64) & mask,
                            block,
                            args: args.len() as u32,
                        });
                        operands.extend(args);
                        if !self.eat_punct(',') {
                            break;
                        }
                    }
                    self.expect_punct(']')?;
                    self.expect_word("default")?;
                    let (default, args) = self.target(b, line)?;
                    let default_args = args.len() as u32;
                    operands.extend(args);
                    self.expect_word("merge")?;
                    let merge = self.block_ref(b)?;
                    let op = Op::Switch {
                        cases,
                        default,
                        default_args,
                        merge,
                    };
                    (op, operands, None)
                }
                "loop" => {
                    self.expect_word("header")?;
                    let (header, args) = self.target(b, line)?;
                    self.expect_word("continue")?;
                    let continuing = self.block_ref(b)?;
                    self.expect_word("merge")?;
                    let merge = self.block_ref(b)?;
                    let op = Op::Loop {
                        header,
                        continuing,
                        merge,
                    };
                    (op, args, None)
                }
                "return" => (Op::Return, self.operands_to_end(b, line)?, None),
                "unreachable" => (Op::Unreachable, Vec::new(), None),
                w if w.starts_with("atomic.") => {
                    let Some(op) = AtomicOp::from_name(&w["atomic.".len()..]) else {
                        return self.error(format!("unknown atomic `{w}`"));
                    };
                    let n = match op {
                        AtomicOp::Load => 1,
                        AtomicOp::CmpXchg => 3,
                        _ => 2,
                    };
                    let operands = self.operands(b, line, n)?;
                    let scope = self.paren_word("scope")?;
                    let Some(scope) = Scope::from_name(&scope) else {
                        return self.error(format!("unknown scope `{scope}`"));
                    };
                    let order = self.paren_word("order")?;
                    let Some(order) = MemOrder::from_name(&order) else {
                        return self.error(format!("unknown ordering `{order}`"));
                    };
                    (Op::Atomic { op, scope, order }, operands, None)
                }
                w if w.starts_with("wave.") => self.wave(b, line, &w["wave.".len()..])?,
                _ => {
                    self.pos = self.pos.saturating_sub(1);
                    return self.error(format!("unknown operation `{word}`"));
                }
            }
        };
        if b.block.is_none() {
            return self.error("an instruction before the first block label");
        }
        let inst = self.emit(b, start, op, operands, explicit)?;
        self.m.funcs[b.func].insts[inst].precise = is_precise;
        Ok(inst)
    }

    fn aligned(&mut self) -> R<Option<u32>> {
        if self.eat_word("aligned") {
            self.expect_punct('(')?;
            let n = self.u32()?;
            self.expect_punct(')')?;
            Ok(Some(n))
        } else {
            Ok(None)
        }
    }

    /// The index of the field named next in struct `ty` (or the struct `ty` points to).
    fn field_index(&mut self, ty: Type) -> R<u32> {
        let ty = match self.m.data(ty) {
            TypeData::Ptr { pointee, .. } => *pointee,
            _ => ty,
        };
        let TypeData::Struct(s) = self.m.data(ty).clone() else {
            return self.error("`.field` needs a struct");
        };
        let name = self.field_name()?;
        match self.m.structs[s].fields.iter().position(|f| *f.0 == *name) {
            Some(i) => Ok(i as u32),
            None => {
                self.pos = self.pos.saturating_sub(1);
                let s = self.m.structs[s].name.clone();
                self.error(format!("struct `{s}` has no field `{name}`"))
            }
        }
    }

    /// `tex.* operands { arg: v, grad: (dx, dy) }`: `n` operands, then the arguments, in the
    /// order `TexArg` lists them.
    fn tex(
        &mut self,
        b: &mut Body,
        line: u32,
        n: usize,
        make: fn(Vec<TexArg>) -> Op,
    ) -> R<(Op, Vec<Value>, Option<Type>)> {
        let mut operands = self.operands(b, line, n)?;
        let mut args = Vec::new();
        if self.eat_punct('{') {
            while !self.eat_punct('}') {
                let w = self.ident()?;
                let Some(arg) = TexArg::from_name(&w) else {
                    return self.error(format!("unknown texture argument `{w}`"));
                };
                let rank = |a: TexArg| TexArg::ALL.iter().position(|&x| x == a);
                if args.last().is_some_and(|&last| rank(last) >= rank(arg)) {
                    return self.error("texture arguments come once each, in their order");
                }
                self.expect_punct(':')?;
                if arg == TexArg::Grad {
                    self.expect_punct('(')?;
                    operands.push(self.operand(b, line)?);
                    self.expect_punct(',')?;
                    operands.push(self.operand(b, line)?);
                    self.expect_punct(')')?;
                } else {
                    operands.push(self.operand(b, line)?);
                }
                args.push(arg);
                if !self.eat_punct(',') {
                    self.expect_punct('}')?;
                    break;
                }
            }
        }
        Ok((make(args), operands, None))
    }

    fn access(&mut self, b: &mut Body, line: u32) -> R<(Op, Vec<Value>, Option<Type>)> {
        let base = self.operand(b, line)?;
        let mut operands = vec![base];
        let mut steps = Vec::new();
        let mut ty = match self.m.data(self.m.funcs[b.func].ty(base)) {
            TypeData::Ptr { pointee, .. } => *pointee,
            _ => return self.error("`access` needs a pointer"),
        };
        while self.eat_punct(',') {
            let step = if self.eat_punct('.') {
                AccessStep::Field(self.field_index(ty)?)
            } else {
                self.expect_punct('[')?;
                let index = if let Tok::Int(_) = self.peek() {
                    let v = self.int()?;
                    let Ok(v) = i64::try_from(v) else {
                        return self.error("index out of range");
                    };
                    let i32_ty = self.m.scalar(Scalar::I32);
                    let c = self.m.int(i32_ty, v);
                    self.m.funcs[b.func].const_value(c, i32_ty)
                } else {
                    self.operand(b, line)?
                };
                self.expect_punct(']')?;
                operands.push(index);
                AccessStep::Index
            };
            ty = match step_type(&mut self.m, ty, step) {
                Ok(t) => t,
                Err(e) => return self.error(e),
            };
            steps.push(step);
        }
        Ok((Op::Access(steps), operands, None))
    }

    fn wave(&mut self, b: &mut Body, line: u32, name: &str) -> R<(Op, Vec<Value>, Option<Type>)> {
        let reduce = |p: &mut Self| -> R<ReduceOp> {
            let w = p.ident()?;
            match ReduceOp::from_name(&w) {
                Some(r) => Ok(r),
                None => p.error(format!("unknown reduction `{w}`")),
            }
        };
        let (op, operands) = match name {
            "reduce" => {
                let r = reduce(self)?;
                (WaveOp::Reduce(r), self.operands(b, line, 1)?)
            }
            "scan" => {
                let r = reduce(self)?;
                let exclusive = match self.ident()?.as_str() {
                    "exclusive" => true,
                    "inclusive" => false,
                    _ => return self.error("expected `inclusive` or `exclusive`"),
                };
                (
                    WaveOp::Scan { op: r, exclusive },
                    self.operands(b, line, 1)?,
                )
            }
            "ballot" => (WaveOp::Ballot, self.operands(b, line, 1)?),
            "broadcast" => (WaveOp::Broadcast, self.operands(b, line, 2)?),
            "shuffle" => (WaveOp::Shuffle, self.operands(b, line, 2)?),
            "quad" => {
                let w = self.ident()?;
                let Some(q) = QuadOp::from_name(&w) else {
                    return self.error(format!("unknown quad operation `{w}`"));
                };
                let n = if q == QuadOp::ReadLane { 2 } else { 1 };
                (WaveOp::Quad(q), self.operands(b, line, n)?)
            }
            "vote" => {
                let w = self.ident()?;
                let Some(v) = VoteOp::from_name(&w) else {
                    return self.error(format!("unknown vote `{w}`"));
                };
                (WaveOp::Vote(v), self.operands(b, line, 1)?)
            }
            "match" => (WaveOp::Match, self.operands(b, line, 1)?),
            "lane_index" => (WaveOp::LaneIndex, Vec::new()),
            "lane_count" => (WaveOp::LaneCount, Vec::new()),
            "is_first" => (WaveOp::IsFirst, Vec::new()),
            _ => return self.error(format!("unknown wave operation `wave.{name}`")),
        };
        let cluster = if self.eat_word("cluster") {
            self.expect_punct('(')?;
            let n = self.u32()?;
            self.expect_punct(')')?;
            Some(n)
        } else {
            None
        };
        Ok((Op::Wave { op, cluster }, operands, None))
    }
}

/// A value name as a hint: `None` for the numbers the printer makes up.
fn numeric_free(name: &str) -> Option<&str> {
    if name.chars().all(|c| c.is_ascii_digit()) {
        None
    } else {
        Some(name)
    }
}

fn is_hex(text: &str) -> bool {
    let t = text.strip_prefix('-').unwrap_or(text);
    t.starts_with("0x") || t.starts_with("0X")
}

/// A decimal or hexadecimal integer, with an optional minus sign.
fn parse_int(text: &str) -> Option<i128> {
    let (neg, t) = match text.strip_prefix('-') {
        Some(t) => (true, t),
        None => (false, text),
    };
    let v = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => i128::from_str_radix(hex, 16).ok()?,
        None => t.parse::<i128>().ok()?,
    };
    if v > u64::MAX as i128 {
        return None;
    }
    Some(if neg { -v } else { v })
}

/// `f32` → (F32, None); `f32x4` → (F32, Some((4, None))); `f32x4x3` → (F32, Some((4, Some(3)))).
#[allow(clippy::type_complexity)]
fn scalar_shape(word: &str) -> Option<(Scalar, Option<(u8, Option<u8>)>)> {
    let mut parts = word.split('x');
    let s = Scalar::from_name(parts.next()?)?;
    let dim = |p: &str| -> Option<u8> {
        let n: u8 = p.parse().ok()?;
        (2..=4).contains(&n).then_some(n)
    };
    match (parts.next(), parts.next(), parts.next()) {
        (None, _, _) => Some((s, None)),
        (Some(n), None, _) => Some((s, Some((dim(n)?, None)))),
        (Some(r), Some(c), None) => Some((s, Some((dim(r)?, Some(dim(c)?))))),
        _ => None,
    }
}
