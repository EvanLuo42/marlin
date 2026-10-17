//! Parser infrastructure: token input, markers, events, and building the green tree.
//!
//! The grammar functions (`grammar/`) never build nodes directly. They push events (start a
//! node, add a token, finish, report an error); [`build_tree`] then turns the events and the
//! lexer's tokens into a green tree, putting trivia back in between. This keeps the grammar free
//! of trivia handling and lets a node be wrapped in a parent after the fact
//! ([`CompletedMarker::precede`]), which is how left-recursive constructs such as `a.b(c)` are
//! parsed.

use std::cell::Cell;

use marlin_lang_span::{TextRange, TextSize};

use crate::SyntaxKind::{self, *};
use crate::lexer::Token;
use crate::tree::{GreenBuilder, GreenNode};
use crate::{SyntaxError, SyntaxErrorKind, T};
use std::sync::Arc;

// Token sets index kinds by bit; every token kind must fit.
const _: () = assert!((EOF as usize) < 256);

/// A set of token kinds.
#[derive(Clone, Copy)]
pub(crate) struct TokenSet([u64; 4]);

impl TokenSet {
    pub(crate) const fn new(kinds: &[SyntaxKind]) -> TokenSet {
        let mut bits = [0u64; 4];
        let mut i = 0;
        while i < kinds.len() {
            let k = kinds[i] as usize;
            bits[k / 64] |= 1 << (k % 64);
            i += 1;
        }
        TokenSet(bits)
    }

    pub(crate) const fn contains(&self, kind: SyntaxKind) -> bool {
        let k = kind as usize;
        k < 256 && self.0[k / 64] & (1 << (k % 64)) != 0
    }
}

/// The non-trivia tokens the grammar sees.
pub(crate) struct Input {
    kinds: Vec<SyntaxKind>,
    ranges: Vec<TextRange>,
}

impl Input {
    pub(crate) fn new(tokens: &[Token]) -> Self {
        let mut kinds = Vec::new();
        let mut ranges = Vec::new();
        let mut offset = TextSize(0);
        for token in tokens {
            let range = TextRange::at(offset, token.len);
            offset = offset + token.len;
            if !token.kind.is_trivia() {
                kinds.push(token.kind);
                ranges.push(range);
            }
        }
        Self { kinds, ranges }
    }
}

pub(crate) enum Event {
    /// Start a node. `forward_parent` is set when a later `precede` wrapped this node: the
    /// event at that index starts the parent, which must be opened first.
    Start {
        kind: SyntaxKind,
        forward_parent: Option<u32>,
    },
    Finish,
    /// One tree token made of `n_raw` adjacent input tokens (more than one for `>>` and kin).
    Token {
        kind: SyntaxKind,
        n_raw: u8,
    },
    Error(SyntaxError),
    /// A `Start` that was abandoned or already emitted through a forward parent.
    Tombstone,
}

/// Nesting deeper than this is reported instead of parsed, so that adversarial input cannot
/// overflow the stack, including the 1 MiB main thread of a Windows build script.
const MAX_DEPTH: u32 = 128;

/// Lookaheads allowed without consuming a token; more means a grammar bug (a loop that does
/// not advance).
const STEP_LIMIT: u32 = 100_000;

pub(crate) struct Parser<'t> {
    text: &'t str,
    input: &'t Input,
    pos: usize,
    events: Vec<Event>,
    steps: Cell<u32>,
    depth: u32,
    /// Inside a generic argument list's constant expression: `>` closes the list instead of
    /// comparing.
    pub(crate) no_gt: bool,
}

impl<'t> Parser<'t> {
    pub(crate) fn new(text: &'t str, input: &'t Input) -> Self {
        Self {
            text,
            input,
            pos: 0,
            events: Vec::new(),
            steps: Cell::new(0),
            depth: 0,
            no_gt: false,
        }
    }

    /// How many input tokens have been consumed, for loops that must check progress.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn finish(self) -> Vec<Event> {
        self.events
    }

    // ---- lookahead ----

    /// The kind of the token `n` ahead, or `EOF`.
    pub(crate) fn nth(&self, n: usize) -> SyntaxKind {
        let steps = self.steps.get();
        assert!(steps <= STEP_LIMIT, "the parser is stuck");
        self.steps.set(steps + 1);
        self.raw(n)
    }

    /// Like [`nth`](Self::nth) but without counting a step, for bounded speculative scans.
    pub(crate) fn raw(&self, n: usize) -> SyntaxKind {
        self.input.kinds.get(self.pos + n).copied().unwrap_or(EOF)
    }

    pub(crate) fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    pub(crate) fn raw_text(&self, n: usize) -> &str {
        match self.input.ranges.get(self.pos + n) {
            Some(range) => &self.text[range.to_range()],
            None => "",
        }
    }

    /// Whether a line break separates the current token from the previous one.
    pub(crate) fn newline_before(&self) -> bool {
        let start = self.nth_range(0).start().to_usize();
        let prev = match self.pos.checked_sub(1) {
            Some(i) => self.input.ranges[i].end().to_usize(),
            None => 0,
        };
        self.text[prev..start].contains('\n')
    }

    /// Whether the token `n` ahead ends exactly where the next one starts.
    fn raw_joint(&self, n: usize) -> bool {
        match (
            self.input.ranges.get(self.pos + n),
            self.input.ranges.get(self.pos + n + 1),
        ) {
            (Some(a), Some(b)) => a.end() == b.start(),
            _ => false,
        }
    }

    /// Whether the composite token `kind` starts `n` ahead, and how many input tokens it spans.
    fn composite_at(&self, n: usize, kind: SyntaxKind) -> Option<u8> {
        let gt = |i: usize| self.raw(n + i) == GT;
        let joint = |i: usize| self.raw_joint(n + i);
        match kind {
            SHR => {
                (gt(0) && joint(0) && gt(1) && !(joint(1) && matches!(self.raw(n + 2), GT | GTEQ)))
                    .then_some(2)
            }
            SHR_EQ => (gt(0) && joint(0) && self.raw(n + 1) == GTEQ).then_some(2),
            USHR => (gt(0) && joint(0) && gt(1) && joint(1) && gt(2)).then_some(3),
            USHR_EQ => {
                (gt(0) && joint(0) && gt(1) && joint(1) && self.raw(n + 2) == GTEQ).then_some(3)
            }
            _ => None,
        }
    }

    pub(crate) fn nth_at(&self, n: usize, kind: SyntaxKind) -> bool {
        match kind {
            SHR | SHR_EQ | USHR | USHR_EQ => {
                self.nth(n);
                self.composite_at(n, kind).is_some()
            }
            GT => {
                // A `>` that starts `>>` is still a `>` (generic lists close one at a time).
                self.nth(n) == GT
            }
            k if k.is_contextual_keyword() => {
                self.nth(n) == IDENT
                    && SyntaxKind::from_contextual_keyword(self.raw_text(n)) == Some(k)
            }
            k => self.nth(n) == k,
        }
    }

    pub(crate) fn at(&self, kind: SyntaxKind) -> bool {
        self.nth_at(0, kind)
    }

    /// The current token's kind, with contextual keywords resolved (`get` → `GET_KW`).
    pub(crate) fn current_contextual(&self) -> SyntaxKind {
        match self.current() {
            IDENT => SyntaxKind::from_contextual_keyword(self.raw_text(0)).unwrap_or(IDENT),
            kind => kind,
        }
    }

    pub(crate) fn at_ts(&self, set: TokenSet) -> bool {
        set.contains(self.current())
    }

    /// The range of the token `n` ahead (empty at the end of input for `EOF`).
    pub(crate) fn nth_range(&self, n: usize) -> TextRange {
        match self.input.ranges.get(self.pos + n) {
            Some(&range) => range,
            None => TextRange::empty(TextSize::of(self.text)),
        }
    }

    // ---- consuming ----

    /// Consumes the current token as `kind`, which must be at the current position. Contextual
    /// keywords and composite tokens get their own kind.
    pub(crate) fn bump(&mut self, kind: SyntaxKind) {
        assert!(self.at(kind), "bump({kind:?}) at {:?}", self.current());
        let n_raw = self.composite_at(0, kind).unwrap_or(1);
        self.do_bump(kind, n_raw);
    }

    /// Consumes the current token with its own kind.
    pub(crate) fn bump_any(&mut self) {
        let kind = self.current();
        if kind == EOF {
            return;
        }
        self.do_bump(kind, 1);
    }

    fn do_bump(&mut self, kind: SyntaxKind, n_raw: u8) {
        self.pos += n_raw as usize;
        self.steps.set(0);
        self.events.push(Event::Token { kind, n_raw });
    }

    pub(crate) fn eat(&mut self, kind: SyntaxKind) -> bool {
        if !self.at(kind) {
            return false;
        }
        self.bump(kind);
        true
    }

    /// Consumes `kind` or reports that it is missing.
    pub(crate) fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        self.error_expected(&kind.describe());
        false
    }

    // ---- errors ----

    /// Reports `message` at the current token.
    pub(crate) fn error(&mut self, message: impl Into<String>) {
        let range = self.nth_range(0);
        self.push_error(range, message);
    }

    pub(crate) fn push_error(&mut self, range: TextRange, message: impl Into<String>) {
        self.events.push(Event::Error(SyntaxError::new(
            SyntaxErrorKind::Parse,
            range,
            message,
        )));
    }

    /// Reports `expected X`, pointing just after the previous token.
    pub(crate) fn error_expected(&mut self, what: &str) {
        let at = if self.pos == 0 {
            self.nth_range(0).start()
        } else {
            self.input.ranges[self.pos - 1].end()
        };
        let found = match self.current() {
            EOF => "end of file".to_string(),
            _ => format!("`{}`", self.raw_text(0)),
        };
        self.push_error(
            TextRange::empty(at),
            format!("expected {what}, found {found}"),
        );
    }

    /// Reports `message` and wraps the current token in an `ERROR` node.
    pub(crate) fn err_and_bump(&mut self, message: impl Into<String>) {
        self.error(message);
        if self.current() != EOF {
            let m = self.start();
            self.bump_any();
            m.complete(self, ERROR);
        }
    }

    /// Like [`err_and_bump`](Self::err_and_bump), but leaves tokens of `recovery` (and braces,
    /// which delimit everything) for the caller.
    pub(crate) fn err_recover(&mut self, message: impl Into<String>, recovery: TokenSet) {
        if self.at_ts(recovery) || matches!(self.current(), L_CURLY | R_CURLY | EOF) {
            self.error(message);
        } else {
            self.err_and_bump(message);
        }
    }

    // ---- nesting ----

    /// Enters one level of nesting; on `false` the caller must return without parsing (an
    /// error has been reported and a token consumed).
    pub(crate) fn enter(&mut self) -> bool {
        if self.depth >= MAX_DEPTH {
            self.err_and_bump("too deeply nested");
            return false;
        }
        self.depth += 1;
        true
    }

    pub(crate) fn leave(&mut self) {
        self.depth -= 1;
    }

    // ---- markers ----

    pub(crate) fn start(&mut self) -> Marker {
        let pos = self.events.len() as u32;
        self.events.push(Event::Tombstone);
        Marker { pos, done: false }
    }
}

/// An open node. It must be completed or abandoned.
pub(crate) struct Marker {
    pos: u32,
    done: bool,
}

impl Marker {
    pub(crate) fn complete(mut self, p: &mut Parser<'_>, kind: SyntaxKind) -> CompletedMarker {
        self.done = true;
        p.events[self.pos as usize] = Event::Start {
            kind,
            forward_parent: None,
        };
        p.events.push(Event::Finish);
        CompletedMarker {
            pos: self.pos,
            kind,
        }
    }

    /// Drops the node; its children become children of the enclosing node.
    pub(crate) fn abandon(mut self, p: &mut Parser<'_>) {
        self.done = true;
        if self.pos as usize == p.events.len() - 1 {
            p.events.pop();
        }
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        if !self.done && !std::thread::panicking() {
            panic!("marker dropped without complete or abandon");
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CompletedMarker {
    pos: u32,
    kind: SyntaxKind,
}

impl CompletedMarker {
    pub(crate) fn kind(&self) -> SyntaxKind {
        self.kind
    }

    /// Starts a new node that will contain this one.
    pub(crate) fn precede(self, p: &mut Parser<'_>) -> Marker {
        let m = p.start();
        if let Event::Start { forward_parent, .. } = &mut p.events[self.pos as usize] {
            *forward_parent = Some(m.pos - self.pos);
        }
        m
    }
}

/// Turns the events into a green tree, putting trivia back between tokens.
pub(crate) fn build_tree(
    text: &str,
    tokens: &[Token],
    mut events: Vec<Event>,
) -> (Arc<GreenNode>, Vec<SyntaxError>) {
    let mut builder = GreenBuilder::new();
    let mut errors = Vec::new();
    let mut raw = 0usize;
    let mut offset = 0usize;
    let mut depth = 0usize;

    let flush_trivia = |builder: &mut GreenBuilder, raw: &mut usize, offset: &mut usize| {
        while let Some(token) = tokens.get(*raw) {
            if !token.kind.is_trivia() {
                break;
            }
            let len = token.len.to_usize();
            builder.token(token.kind, &text[*offset..*offset + len]);
            *offset += len;
            *raw += 1;
        }
    };

    let mut forward = Vec::new();
    for i in 0..events.len() {
        match std::mem::replace(&mut events[i], Event::Tombstone) {
            Event::Start {
                kind,
                forward_parent,
            } => {
                // Collect the chain of nodes that later `precede` calls put around this one,
                // and open them outermost first.
                forward.clear();
                forward.push(kind);
                let mut idx = i;
                let mut fp = forward_parent;
                while let Some(delta) = fp {
                    idx += delta as usize;
                    fp = match std::mem::replace(&mut events[idx], Event::Tombstone) {
                        Event::Start {
                            kind,
                            forward_parent,
                        } => {
                            forward.push(kind);
                            forward_parent
                        }
                        _ => unreachable!("forward parent is not a start event"),
                    };
                }
                for &kind in forward.iter().rev() {
                    // Whitespace before a node belongs to the enclosing node.
                    if depth > 0 {
                        flush_trivia(&mut builder, &mut raw, &mut offset);
                    }
                    builder.start_node(kind);
                    depth += 1;
                }
            }
            Event::Finish => {
                depth -= 1;
                if depth == 0 {
                    flush_trivia(&mut builder, &mut raw, &mut offset);
                }
                builder.finish_node();
            }
            Event::Token { kind, n_raw } => {
                flush_trivia(&mut builder, &mut raw, &mut offset);
                let len: usize = tokens[raw..raw + n_raw as usize]
                    .iter()
                    .map(|t| t.len.to_usize())
                    .sum();
                builder.token(kind, &text[offset..offset + len]);
                offset += len;
                raw += n_raw as usize;
            }
            Event::Error(error) => errors.push(error),
            Event::Tombstone => {}
        }
    }
    debug_assert_eq!(offset, text.len(), "tokens left over after the root node");
    (builder.finish(), errors)
}

/// Recovery set for "expected a member / statement" situations: tokens that start the next
/// declaration.
pub(crate) const ITEM_RECOVERY: TokenSet = TokenSet::new(&[
    T![;],
    T!['['],
    PUBLIC_KW,
    INTERNAL_KW,
    PRIVATE_KW,
    PROTECTED_KW,
    STATIC_KW,
    READONLY_KW,
    CONST_KW,
    STRUCT_KW,
    CLASS_KW,
    INTERFACE_KW,
    ENUM_KW,
    NAMESPACE_KW,
    USING_KW,
]);
