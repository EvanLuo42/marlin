//! Stack traces: capture is cheap (return addresses only); symbols are looked up when one is
//! displayed.

use core::fmt;

use marlin_pal::debug;

/// The return addresses of a thread's stack at one point, innermost first. Fixed size and
/// allocation-free, so it can be taken in a panic or crash handler.
///
/// Displaying it symbolizes every frame (slow: tens of microseconds per frame, more the first
/// time a module's symbols load): on Windows with function names, files and lines from the PDBs;
/// on POSIX with whatever `dladdr` knows (exported names, demangled) and otherwise the module and
/// offset, for offline symbolization.
#[derive(Clone, Copy)]
pub struct Backtrace {
    frames: [usize; Self::MAX_FRAMES],
    len: usize,
}

impl Backtrace {
    pub const MAX_FRAMES: usize = 64;

    /// The calling thread's stack, starting at the caller of `capture`.
    #[inline(never)]
    pub fn capture() -> Self {
        Self::capture_skip(1)
    }

    /// Like [`capture`](Self::capture), leaving out `skip` more frames (for helpers that capture
    /// on behalf of their caller).
    #[inline(never)]
    pub fn capture_skip(skip: usize) -> Self {
        let mut frames = [0; Self::MAX_FRAMES];
        // Plus one for this function.
        let len = debug::capture_stack(skip + 1, &mut frames);
        Self { frames, len }
    }

    /// An empty trace, for code that captures conditionally.
    pub const fn empty() -> Self {
        Self {
            frames: [0; Self::MAX_FRAMES],
            len: 0,
        }
    }

    pub fn frames(&self) -> &[usize] {
        &self.frames[..self.len]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl fmt::Debug for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.frames().iter().map(|a| Address(*a)))
            .finish()
    }
}

struct Address(usize);

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

/// One line per frame (two with a source location):
///
/// ```text
///    3: marlin_asset::server::AssetServer::load +0x4c
///              at runtime/engine/asset/src/server.rs:352
///    4: 0x7ff6a1b2c3d4 (game.exe +0x12c3d4)
/// ```
impl fmt::Display for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, &addr) in self.frames().iter().enumerate() {
            // Return addresses point after the call; look up the call itself.
            let lookup = addr.saturating_sub(1);
            let mut name = [0u8; 512];
            let mut file = [0u8; 512];
            write!(f, "{i:>4}: ")?;
            match debug::symbolize(lookup, &mut name, &mut file) {
                Some(symbol) if symbol.name_len <= name.len() => {
                    let text = core::str::from_utf8(&name[..symbol.name_len]).unwrap_or("?");
                    write!(f, "{} +{:#x}", Demangle(text), symbol.offset + 1)?;
                    if symbol.file_len > 0 && symbol.file_len <= file.len() {
                        let path = core::str::from_utf8(&file[..symbol.file_len]).unwrap_or("?");
                        write!(f, "\n            at {path}:{}", symbol.line)?;
                    }
                }
                _ => {
                    write!(f, "{addr:#x}")?;
                    let mut path = [0u8; 512];
                    if let Some(module) = debug::module_of(lookup, &mut path)
                        && module.path_len <= path.len()
                    {
                        let path = core::str::from_utf8(&path[..module.path_len]).unwrap_or("?");
                        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
                        write!(f, " ({name} +{:#x})", addr - module.base)?;
                    }
                }
            }
            f.write_str("\n")?;
        }
        Ok(())
    }
}

/// Displays a symbol name, demangling Rust's legacy mangling (`_ZN...17h<hash>E`, what `dladdr`
/// reports); anything else is shown as it is.
pub struct Demangle<'a>(pub &'a str);

impl fmt::Display for Demangle<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match demangle_legacy(self.0, f) {
            Some(result) => result,
            None => f.write_str(self.0),
        }
    }
}

/// Writes the demangled form, or returns `None` (writing nothing) if `name` is not a valid
/// legacy-mangled Rust symbol.
fn demangle_legacy(name: &str, f: &mut fmt::Formatter<'_>) -> Option<fmt::Result> {
    let inner = name
        .strip_prefix("_ZN")
        .or_else(|| name.strip_prefix("__ZN"))?
        .strip_suffix('E')?;
    // Validate first, so nothing is written for names that turn out not to be ours.
    let segments = || Segments(inner);
    let count = segments().try_fold(0usize, |n, s| s.map(|_| n + 1))?;
    if count == 0 {
        return None;
    }
    let is_hash = |s: &str| {
        s.len() == 17 && s.starts_with('h') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
    };
    let shown = match segments().last() {
        Some(Some(last)) if count > 1 && is_hash(last) => count - 1,
        _ => count,
    };
    Some((|| {
        for (i, segment) in segments().take(shown).enumerate() {
            if i > 0 {
                f.write_str("::")?;
            }
            write_ident(segment.unwrap_or(""), f)?;
        }
        Ok(())
    })())
}

/// Length-prefixed segments; yields `None` on malformed input.
struct Segments<'a>(&'a str);

impl<'a> Iterator for Segments<'a> {
    type Item = Option<&'a str>;

    fn next(&mut self) -> Option<Option<&'a str>> {
        if self.0.is_empty() {
            return None;
        }
        let digits = self.0.bytes().take_while(u8::is_ascii_digit).count();
        let parsed = self.0[..digits]
            .parse::<usize>()
            .ok()
            .and_then(|len| self.0.get(digits..digits + len).map(|s| (s, digits + len)));
        Some(match parsed {
            Some((segment, used)) => {
                self.0 = &self.0[used..];
                Some(segment)
            }
            None => {
                self.0 = "";
                None
            }
        })
    }
}

/// Undoes the escapes of legacy mangling: `$LT$` -> `<`, `$u20$` -> ` `, `..` -> `::`, ...
fn write_ident(mut s: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    // A leading underscore guards identifiers that start with an escape.
    if s.starts_with("_$") {
        s = &s[1..];
    }
    while !s.is_empty() {
        if let Some(rest) = s.strip_prefix("..") {
            f.write_str("::")?;
            s = rest;
        } else if s.starts_with('$')
            && let Some(end) = s[1..].find('$')
        {
            let code = &s[1..=end];
            let replacement = match code {
                "SP" => Some('@'),
                "BP" => Some('*'),
                "RF" => Some('&'),
                "LT" => Some('<'),
                "GT" => Some('>'),
                "LP" => Some('('),
                "RP" => Some(')'),
                "C" => Some(','),
                _ => code
                    .strip_prefix('u')
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .and_then(char::from_u32),
            };
            match replacement {
                Some(c) => fmt::Write::write_char(f, c)?,
                None => f.write_str(&s[..=end + 1])?,
            }
            s = &s[end + 2..];
        } else {
            let next = s[1..].find(['$', '.']).map_or(s.len(), |i| i + 1);
            f.write_str(&s[..next])?;
            s = &s[next..];
        }
    }
    Ok(())
}
