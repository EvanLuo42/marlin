//! UTF-8 paths with `/` separators.
//!
//! [`Path`] is a borrowed view that accepts `/` and `\` alike, so OS paths work as they are.
//! [`PathBuf`] is always normalized: `/` separators, no empty or `.` components, `..` resolved
//! lexically (kept at the start of relative paths, dropped at a root), no trailing `/`.
//!
//! Roots: `/`, `//` (a UNC `//server/share` path), `C:/`, and the drive-relative `C:`. Lexical
//! `..` resolution is wrong across symlinks; asset paths do not use them.

use alloc::string::String;
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::fmt;
use core::ops::Deref;

fn is_sep(c: u8) -> bool {
    c == b'/' || c == b'\\'
}

/// A borrowed path. See the [module docs](self).
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Path(str);

impl Path {
    pub fn new<S: AsRef<str> + ?Sized>(s: &S) -> &Path {
        let s: &str = s.as_ref();
        // SAFETY: `Path` is a transparent wrapper around `str`.
        unsafe { &*(s as *const str as *const Path) }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Length of the root prefix (`/`, `//`, `C:/`, `C:`), or 0.
    fn root_len(&self) -> usize {
        let b = self.0.as_bytes();
        match b {
            [a, c, ..] if is_sep(*a) && is_sep(*c) => 2,
            [a, ..] if is_sep(*a) => 1,
            [d, b':', s, ..] if d.is_ascii_alphabetic() && is_sep(*s) => 3,
            [d, b':', ..] if d.is_ascii_alphabetic() => 2,
            _ => 0,
        }
    }

    /// The root prefix in normalized form.
    fn root(&self) -> &str {
        &self.0[..self.root_len()]
    }

    /// Whether the path starts at a root (`/`, `//`, `C:/`) rather than a current directory.
    pub fn is_absolute(&self) -> bool {
        let root = self.root();
        !root.is_empty() && root.ends_with(['/', '\\'])
    }

    /// The components after the root, skipping empty and `.` ones; `..` is kept.
    pub fn components(&self) -> impl DoubleEndedIterator<Item = &str> + Clone {
        self.0[self.root_len()..]
            .split(['/', '\\'])
            .filter(|c| !c.is_empty() && *c != ".")
    }

    /// The last component, unless it is `..` or there is none.
    pub fn file_name(&self) -> Option<&str> {
        self.components().next_back().filter(|&c| c != "..")
    }

    /// The file name before its last `.`; the whole name if it has no extension.
    pub fn file_stem(&self) -> Option<&str> {
        let name = self.file_name()?;
        Some(split_extension(name).map_or(name, |(stem, _)| stem))
    }

    /// The file name after its last `.`. A leading dot (`.config`) does not start an extension.
    pub fn extension(&self) -> Option<&str> {
        split_extension(self.file_name()?).map(|(_, ext)| ext)
    }

    /// The path without its last component: `None` for an empty path or a bare root.
    pub fn parent(&self) -> Option<&Path> {
        let root = self.root_len();
        let rest = self.0[root..].trim_end_matches(['/', '\\']);
        if rest.is_empty() {
            return None;
        }
        let end = match rest.rfind(['/', '\\']) {
            Some(i) => root + rest[..i].trim_end_matches(['/', '\\']).len(),
            None => root,
        };
        Some(Path::new(&self.0[..end]))
    }

    /// `other` appended to `self`, normalized. An absolute `other` replaces `self`.
    pub fn join(&self, other: impl AsRef<Path>) -> PathBuf {
        let mut buf = self.normalize();
        buf.push(other);
        buf
    }

    pub fn normalize(&self) -> PathBuf {
        PathBuf(normalize(self))
    }

    pub fn is_normalized(&self) -> bool {
        normalize(self) == self.0
    }

    /// Whether `base`'s root and components are a prefix of this path's (whole components only).
    pub fn starts_with(&self, base: impl AsRef<Path>) -> bool {
        let base = base.as_ref();
        if !same_root(self.root(), base.root()) {
            return false;
        }
        let mut mine = self.components();
        base.components().all(|c| mine.next() == Some(c))
    }

    pub fn to_path_buf(&self) -> PathBuf {
        self.normalize()
    }
}

fn split_extension(name: &str) -> Option<(&str, &str)> {
    name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty())
}

fn same_root(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .all(|(x, y)| x == y || (is_sep(x) && is_sep(y)))
}

fn normalize(path: &Path) -> String {
    let root = path.root();
    let absolute = path.is_absolute();
    let mut parts: Vec<&str> = Vec::new();
    for c in path.components() {
        if c == ".." {
            match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                // `..` at a root stays at the root.
                _ if absolute => {}
                _ => parts.push(c),
            }
        } else {
            parts.push(c);
        }
    }

    let len = root.len() + parts.iter().map(|p| p.len() + 1).sum::<usize>();
    let mut out = String::with_capacity(len);
    out.extend(root.chars().map(|c| if c == '\\' { '/' } else { c }));
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(part);
    }
    out
}

impl AsRef<Path> for Path {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl AsRef<Path> for str {
    fn as_ref(&self) -> &Path {
        Path::new(self)
    }
}

impl AsRef<Path> for String {
    fn as_ref(&self) -> &Path {
        Path::new(self)
    }
}

impl AsRef<str> for Path {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An owned, always normalized path. See the [module docs](self).
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathBuf(String);

impl PathBuf {
    pub const fn new() -> Self {
        Self(String::new())
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// Appends `other` and normalizes. An absolute `other` replaces the path.
    pub fn push(&mut self, other: impl AsRef<Path>) {
        let other = other.as_ref();
        if other.is_absolute() {
            self.0 = normalize(other);
            return;
        }
        if !self.0.is_empty() && !self.0.ends_with('/') && !other.is_empty() {
            self.0.push('/');
        }
        self.0.push_str(other.as_str());
        self.0 = normalize(Path::new(&self.0));
    }

    /// Removes the last component; `false` if there was none.
    pub fn pop(&mut self) -> bool {
        match self.as_path().parent() {
            Some(parent) => {
                let len = parent.as_str().len();
                self.0.truncate(len);
                true
            }
            None => false,
        }
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<&str> for PathBuf {
    fn from(s: &str) -> Self {
        Path::new(s).normalize()
    }
}

impl From<String> for PathBuf {
    fn from(s: String) -> Self {
        let path = Path::new(&s);
        if path.is_normalized() {
            Self(s)
        } else {
            path.normalize()
        }
    }
}

impl From<&Path> for PathBuf {
    fn from(path: &Path) -> Self {
        path.normalize()
    }
}

impl Deref for PathBuf {
    type Target = Path;

    fn deref(&self) -> &Path {
        self.as_path()
    }
}

impl AsRef<Path> for PathBuf {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl Borrow<Path> for PathBuf {
    fn borrow(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Debug for PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
