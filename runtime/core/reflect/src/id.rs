//! Stable type ids and type names.

use core::fmt;

use marlin_hash::stable::{FNV1A_64_INIT, fnv1a_64, fnv1a_64_extend};

// Type ids and name hashes are FNV-1a: it is `const`, so every id in a `TypeInfo` is computed at
// compile time, and streamable, so a generic type's id is hashed from its name parts without
// building the string.

/// Hash of a field or variant name, as stored in serialized data.
///
/// 32 bits suffice because names only need to be distinct within one struct or enum, and
/// `#[derive(Reflect)]` rejects collisions at compile time.
pub const fn name_hash(name: &str) -> u32 {
    let h = fnv1a_64(name.as_bytes());
    (h ^ (h >> 32)) as u32
}

/// Stable identifier of a reflected type: the FNV-1a hash of its fully qualified name.
///
/// Unlike `core::any::TypeId` it is the same across builds, compilers and platforms, so it can be
/// stored in cooked data and save games. It changes when the type's name changes; pin it with
/// `#[reflect(name = "...")]` before moving a serialized type between modules.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TypeId(pub u64);

impl TypeId {
    /// Id of the type whose full name (as printed by [`TypeName`]) is `name`.
    pub const fn of_name(name: &str) -> Self {
        Self(fnv1a_64(name.as_bytes()))
    }

    /// Id of `name`; equal to `TypeId::of_name(&name.to_string())`.
    pub const fn of_type_name(name: &TypeName) -> Self {
        Self(name.hash(FNV1A_64_INIT))
    }
}

impl fmt::Display for TypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Structured name of a type. Generic types keep their arguments as separate names so the full
/// name never has to be concatenated at compile time.
///
/// Prints as `path`, `path<A, B>`, `[T; N]`, `(A, B)` (`(A,)` for one element) or, for a const
/// generic argument, its value (the `16` of `InlineString<16>`).
#[derive(Clone, Copy, Debug)]
pub enum TypeName {
    Path {
        path: &'static str,
        /// Generic arguments, types and const values, in declaration order. Lifetimes are not
        /// part of names: reflected types are always `'static`.
        args: &'static [&'static TypeName],
    },
    Array {
        element: &'static TypeName,
        len: usize,
    },
    /// A tuple of at least one element. The unit type is `Path { path: "()" }`.
    Tuple(&'static [&'static TypeName]),
    /// The value of a const generic argument.
    Const(ConstArg),
}

/// Value of a const generic argument, as it appears in a [`TypeName`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConstArg {
    /// Signed integers, printed in decimal.
    Int(i128),
    /// Unsigned integers, printed in decimal.
    Uint(u128),
    /// Printed as `true` / `false`.
    Bool(bool),
    /// Printed quoted: `'x'`.
    Char(char),
}

impl TypeName {
    /// A non-generic name.
    pub const fn simple(path: &'static str) -> Self {
        Self::Path { path, args: &[] }
    }

    /// Hashes exactly the bytes `Display` prints.
    const fn hash(&self, state: u64) -> u64 {
        match *self {
            Self::Path { path, args } => {
                let mut state = fnv1a_64_extend(state, path.as_bytes());
                if !args.is_empty() {
                    state = fnv1a_64_extend(state, b"<");
                    state = hash_list(state, args);
                    state = fnv1a_64_extend(state, b">");
                }
                state
            }
            Self::Array { element, len } => {
                let state = element.hash(fnv1a_64_extend(state, b"["));
                let state = hash_decimal(fnv1a_64_extend(state, b"; "), len as u128);
                fnv1a_64_extend(state, b"]")
            }
            Self::Tuple(elements) => {
                let mut state = hash_list(fnv1a_64_extend(state, b"("), elements);
                if elements.len() == 1 {
                    state = fnv1a_64_extend(state, b",");
                }
                fnv1a_64_extend(state, b")")
            }
            Self::Const(ConstArg::Int(v)) => {
                let state = if v < 0 {
                    fnv1a_64_extend(state, b"-")
                } else {
                    state
                };
                hash_decimal(state, v.unsigned_abs())
            }
            Self::Const(ConstArg::Uint(v)) => hash_decimal(state, v),
            Self::Const(ConstArg::Bool(true)) => fnv1a_64_extend(state, b"true"),
            Self::Const(ConstArg::Bool(false)) => fnv1a_64_extend(state, b"false"),
            Self::Const(ConstArg::Char(c)) => {
                let mut utf8 = [0u8; 4];
                let len = c.encode_utf8(&mut utf8).len();
                let state = fnv1a_64_extend(state, b"'");
                let state = fnv1a_64_extend(state, utf8.split_at(len).0);
                fnv1a_64_extend(state, b"'")
            }
        }
    }
}

/// `a, b, c`.
const fn hash_list(mut state: u64, names: &[&TypeName]) -> u64 {
    let mut i = 0;
    while i < names.len() {
        if i > 0 {
            state = fnv1a_64_extend(state, b", ");
        }
        state = names[i].hash(state);
        i += 1;
    }
    state
}

const fn hash_decimal(state: u64, mut n: u128) -> u64 {
    let mut digits = [0u8; 39];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    fnv1a_64_extend(state, digits.split_at(start).1)
}

fn fmt_list(f: &mut fmt::Formatter<'_>, names: &[&TypeName]) -> fmt::Result {
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        fmt::Display::fmt(name, f)?;
    }
    Ok(())
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Path { path, args } => {
                f.write_str(path)?;
                if !args.is_empty() {
                    f.write_str("<")?;
                    fmt_list(f, args)?;
                    f.write_str(">")?;
                }
                Ok(())
            }
            Self::Array { element, len } => write!(f, "[{element}; {len}]"),
            Self::Tuple(elements) => {
                f.write_str("(")?;
                fmt_list(f, elements)?;
                if elements.len() == 1 {
                    f.write_str(",")?;
                }
                f.write_str(")")
            }
            Self::Const(ConstArg::Int(v)) => write!(f, "{v}"),
            Self::Const(ConstArg::Uint(v)) => write!(f, "{v}"),
            Self::Const(ConstArg::Bool(v)) => write!(f, "{v}"),
            Self::Const(ConstArg::Char(c)) => write!(f, "'{c}'"),
        }
    }
}
