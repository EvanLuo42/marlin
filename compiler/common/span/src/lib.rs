//! Source positions for the Marlin language toolchain (`docs/design/0015-language-frontend.md` §1).
//!
//! Offsets are byte offsets into a file's UTF-8 text. A [`Span`] names a range in one file; the
//! file itself is a [`FileId`] that the session maps to a [`SourcePath`] (package + path inside
//! it), so nothing in the compiler holds a machine path. [`LineIndex`] turns offsets into
//! 0-based line and column numbers for diagnostics and tools.

use std::fmt;
use std::ops::{Add, Range};
use std::sync::Arc;

/// A byte offset or length in a source file. Files are limited to 4 GiB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextSize(pub u32);

impl TextSize {
    /// The length of `text` in bytes.
    ///
    /// # Panics
    /// If `text` is 4 GiB or longer.
    pub fn of(text: &str) -> Self {
        Self(u32::try_from(text.len()).expect("source text larger than 4 GiB"))
    }

    pub const fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl Add for TextSize {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl From<u32> for TextSize {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for TextSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A half-open byte range `start..end` in one file.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextRange {
    start: TextSize,
    end: TextSize,
}

impl TextRange {
    /// # Panics
    /// If `start > end`.
    pub fn new(start: TextSize, end: TextSize) -> Self {
        assert!(start <= end, "inverted text range {start}..{end}");
        Self { start, end }
    }

    pub fn at(start: TextSize, len: TextSize) -> Self {
        Self::new(start, start + len)
    }

    pub fn empty(at: TextSize) -> Self {
        Self { start: at, end: at }
    }

    pub const fn start(self) -> TextSize {
        self.start
    }

    pub const fn end(self) -> TextSize {
        self.end
    }

    pub const fn len(self) -> TextSize {
        TextSize(self.end.0 - self.start.0)
    }

    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    /// Whether `offset` lies in `start..end`.
    pub fn contains(self, offset: TextSize) -> bool {
        self.start <= offset && offset < self.end
    }

    /// Whether `other` lies entirely inside this range.
    pub fn contains_range(self, other: TextRange) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// The smallest range covering both.
    pub fn cover(self, other: TextRange) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn to_range(self) -> Range<usize> {
        self.start.to_usize()..self.end.to_usize()
    }
}

impl fmt::Debug for TextRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// A file known to a compiler session. The session owns the mapping to its [`SourcePath`] and
/// text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(pub u32);

/// A range in a specific file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub file: FileId,
    pub range: TextRange,
}

impl Span {
    pub fn new(file: FileId, range: TextRange) -> Self {
        Self { file, range }
    }
}

/// Where a source file lives: a package name plus a `/`-separated path relative to the package
/// root (0005 §5.2). Never a machine path.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourcePath {
    package: Arc<str>,
    path: Arc<str>,
}

impl SourcePath {
    pub fn new(package: impl Into<Arc<str>>, path: impl Into<Arc<str>>) -> Self {
        Self {
            package: package.into(),
            path: path.into(),
        }
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for SourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.package, self.path)
    }
}

impl fmt::Debug for SourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SourcePath({self})")
    }
}

/// A 0-based line and column. Columns count Unicode scalar values, which is what a terminal
/// or editor shows for the text the compiler accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

/// Line starts of one file, for offset → line/column lookups.
///
/// Lines end at `\n`; a `\r` before it belongs to the line it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineIndex {
    starts: Vec<TextSize>,
    len: TextSize,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![TextSize(0)];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|&(_, b)| b == b'\n')
                .map(|(i, _)| TextSize(i as u32 + 1)),
        );
        Self {
            starts,
            len: TextSize::of(text),
        }
    }

    pub fn line_count(&self) -> u32 {
        self.starts.len() as u32
    }

    /// The 0-based line containing `offset`. Offsets past the end clamp to the last line.
    pub fn line(&self, offset: TextSize) -> u32 {
        let offset = offset.min(self.len);
        (self.starts.partition_point(|&s| s <= offset) - 1) as u32
    }

    /// The byte range of `line`, without its line terminator.
    ///
    /// # Panics
    /// If `line` is out of range.
    pub fn line_range(&self, line: u32, text: &str) -> TextRange {
        let start = self.starts[line as usize];
        let mut end = self
            .starts
            .get(line as usize + 1)
            .map_or(self.len, |&next| TextSize(next.0 - 1));
        if end > start && text.as_bytes()[end.to_usize() - 1] == b'\r' {
            end = TextSize(end.0 - 1);
        }
        TextRange::new(start, end)
    }

    /// The line and column of `offset` in `text`, which must be the text this index was built
    /// from. An offset inside a multi-byte character maps to that character.
    pub fn line_col(&self, offset: TextSize, text: &str) -> LineCol {
        let offset = offset.min(self.len);
        let line = self.line(offset);
        let start = self.starts[line as usize].to_usize();
        let mut end = offset.to_usize();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        LineCol {
            line,
            col: text[start..end].chars().count() as u32,
        }
    }
}
