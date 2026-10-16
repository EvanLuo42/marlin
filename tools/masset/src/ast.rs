//! The syntax tree of a `.masset` file.
//!
//! Untyped: it records what the text says, with byte spans for diagnostics. [`Schema`] binds it
//! to reflected Rust types; tools that only need structure (the index, link fix-ups) work on it
//! directly.
//!
//! [`Schema`]: crate::Schema

use marlin_asset::Uuid;

/// A byte range in the source text.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    /// 1-based line and column (in chars) of the start, in `text`.
    pub fn location(&self, text: &str) -> Location {
        Location::of(text, self.start as usize)
    }
}

/// A 1-based line and column.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Location {
    pub line: u32,
    pub column: u32,
}

impl Location {
    pub fn of(text: &str, offset: usize) -> Self {
        let before = &text[..offset.min(text.len())];
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        Self {
            line: before.matches('\n').count() as u32 + 1,
            column: before[line_start..].chars().count() as u32 + 1,
        }
    }
}

impl core::fmt::Display for Location {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// The current format version, written as `!Version` at the top of every file.
pub const FORMAT_VERSION: u32 = 1;

/// A whole file: objects in file order.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Document {
    /// The file's `!Version` (format version).
    pub version: u32,
    pub objects: Vec<Object>,
}

impl Document {
    pub fn object(&self, name: &str) -> Option<&Object> {
        self.objects.iter().find(|o| o.name == name)
    }

    pub fn object_by_uuid(&self, uuid: Uuid) -> Option<&Object> {
        self.objects.iter().find(|o| o.uuid == uuid)
    }
}

/// One object block: `TypeName { !Uuid = … !Name = … attributes }`. Equality ignores spans.
#[derive(Clone, Debug)]
pub struct Object {
    /// Short reflected type name, PascalCase.
    pub type_name: String,
    pub uuid: Uuid,
    /// Unique within its file.
    pub name: String,
    /// The type's schema version (`!Version` inside the object); 0 when absent.
    pub type_version: u32,
    pub attrs: Vec<Attr>,
    pub span: Span,
}

impl PartialEq for Object {
    fn eq(&self, other: &Self) -> bool {
        self.type_name == other.type_name
            && self.uuid == other.uuid
            && self.name == other.name
            && self.type_version == other.type_version
            && self.attrs == other.attrs
    }
}

/// `Key = value`. Equality ignores spans.
#[derive(Clone, Debug)]
pub struct Attr {
    /// PascalCase field name.
    pub key: String,
    pub value: Node,
    pub span: Span,
}

impl PartialEq for Attr {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.value == other.value
    }
}

/// A value with its source span. Equality ignores spans.
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub span: Span,
}

impl Node {
    /// A node with no source position (built by tools rather than parsed).
    pub const fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            span: Span { start: 0, end: 0 },
        }
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl From<NodeKind> for Node {
    fn from(kind: NodeKind) -> Self {
        Self::new(kind)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum NodeKind {
    Bool(bool),
    /// An integer literal. Wide enough for every reflected integer type but `u128` above
    /// `i128::MAX`.
    Int(i128),
    Float(f64),
    Str(String),
    /// A bare word: a unit enum variant, a unit struct, or `None`.
    Ident(String),
    /// `Variant(a, b)`: a tuple enum variant.
    Call(String, Vec<Node>),
    /// `{ A = … }` (a struct) or `Variant { A = … }` (a struct enum variant).
    Record(Option<String>, Vec<Attr>),
    /// `(a, b)`: a tuple struct or a fixed-size array.
    Tuple(Vec<Node>),
    /// `[a, b]`: a list.
    List(Vec<Node>),
    /// `<path:Name #uuid>`.
    Link(LinkRef),
    /// `` `scheme:path` ``.
    File(FileRef),
}

/// An object link as written: `<path:Name #uuid>`. The UUID is authoritative; path and name keep
/// the text readable and are fixed up by tools when they go stale.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct LinkRef {
    /// Path of the target's file relative to the asset root, without extension; empty for an
    /// object in the same file.
    pub path: String,
    /// The target's `!Name`.
    pub name: String,
    /// `None` when written by hand without one; resolved by path and name, then filled in.
    pub uuid: Option<Uuid>,
}

impl LinkRef {
    pub fn is_local(&self) -> bool {
        self.path.is_empty()
    }
}

/// A file link as written: `` `source:level/rock.png` ``.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FileRef {
    pub scheme: String,
    pub path: String,
}

/// The scheme of links to source files (`marlin_asset::SourceFile`).
pub const SOURCE_SCHEME: &str = "source";
