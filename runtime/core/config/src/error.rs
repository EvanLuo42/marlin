use alloc::string::String;
use core::fmt;

/// Why a cvar could not be registered or set.
#[derive(Clone, Copy, Debug, PartialEq, marlin_std::Error)]
pub enum Error {
    #[error("unknown cvar")]
    Unknown,
    #[error("`{0}` is already registered")]
    Duplicate(&'static str),
    #[error("read-only")]
    ReadOnly,
    #[error("cheat-protected and cheats are disabled")]
    Cheat,
    #[error("expected {0}")]
    Invalid(Expected),
    #[error("out of range {0}")]
    OutOfRange(Bounds),
    #[error("conflicts with `{0}`: a TOML key cannot be both a value and a table")]
    Conflict(&'static str),
}

/// What a cvar accepts, for [`Error::Invalid`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    Bool,
    Int,
    Float,
    String,
    Enum(&'static [&'static str]),
}

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("true or false"),
            Self::Int => f.write_str("an integer"),
            Self::Float => f.write_str("a finite number"),
            Self::String => f.write_str("a string"),
            Self::Enum(names) => {
                f.write_str("one of ")?;
                for (i, name) in names.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(name)?;
                }
                Ok(())
            }
        }
    }
}

/// A cvar's inclusive range, for [`Error::OutOfRange`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bounds {
    Int(i64, i64),
    Float(f64, f64),
}

impl fmt::Display for Bounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(min, max) => write!(f, "[{min}, {max}]"),
            Self::Float(min, max) => write!(f, "[{min}, {max}]"),
        }
    }
}

/// Where a config file or command-line entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// 1-based line of a config file.
    Line(u32),
    /// Index of the `+name` argument on the command line.
    Arg(u32),
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Line(n) => write!(f, "line {n}"),
            Self::Arg(n) => write!(f, "argument {n}"),
        }
    }
}

/// An entry of a config file or the command line that was not applied.
#[derive(Clone, Debug, PartialEq, marlin_std::Error)]
#[error("{location}: `{key}`")]
pub struct ApplyError {
    pub location: Location,
    /// The cvar name (dotted, for TOML tables); empty for a syntax error with no key.
    pub key: String,
    pub source: Cause,
}

/// Why an entry was not applied.
#[derive(Clone, Debug, PartialEq, marlin_std::Error)]
pub enum Cause {
    #[error(transparent)]
    Set(#[from] Error),
    /// Malformed TOML or command line, with the parser's message.
    #[error("syntax error: {0}")]
    Syntax(String),
}
