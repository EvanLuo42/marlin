//! Stable type ids and type names.

use core::fmt;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Continues a 64-bit FNV-1a hash over `bytes`.
///
/// FNV-1a is used (rather than `marlin-hash`) because it is trivially `const`, so every id in a
/// `TypeInfo` is computed at compile time; it is also streamable, which lets a generic type's id be
/// hashed from its name parts without building the string.
pub const fn fnv1a64_extend(mut state: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        state ^= bytes[i] as u64;
        state = state.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    state
}

/// 64-bit FNV-1a hash of `bytes`.
pub const fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv1a64_extend(FNV_OFFSET, bytes)
}

/// Hash of a field or variant name, as stored in serialized data.
///
/// 32 bits suffice because names only need to be distinct within one struct or enum, and
/// `#[derive(Reflect)]` rejects collisions at compile time.
pub const fn name_hash(name: &str) -> u32 {
    let h = fnv1a64(name.as_bytes());
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
        Self(fnv1a64(name.as_bytes()))
    }

    /// Id of `name`; equal to `TypeId::of_name(&name.to_string())`.
    pub const fn of_type_name(name: &TypeName) -> Self {
        Self(name.hash(FNV_OFFSET))
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
/// Prints as `path`, `path<A, B>` or `[T; N]`.
#[derive(Clone, Copy, Debug)]
pub enum TypeName {
    Path {
        path: &'static str,
        args: &'static [&'static TypeName],
    },
    Array {
        element: &'static TypeName,
        len: usize,
    },
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
                let mut state = fnv1a64_extend(state, path.as_bytes());
                if !args.is_empty() {
                    state = fnv1a64_extend(state, b"<");
                    let mut i = 0;
                    while i < args.len() {
                        if i > 0 {
                            state = fnv1a64_extend(state, b", ");
                        }
                        state = args[i].hash(state);
                        i += 1;
                    }
                    state = fnv1a64_extend(state, b">");
                }
                state
            }
            Self::Array { element, len } => {
                let state = element.hash(fnv1a64_extend(state, b"["));
                let state = hash_decimal(fnv1a64_extend(state, b"; "), len);
                fnv1a64_extend(state, b"]")
            }
        }
    }
}

const fn hash_decimal(state: u64, mut n: usize) -> u64 {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    fnv1a64_extend(state, digits.split_at(start).1)
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Path { path, args } => {
                f.write_str(path)?;
                if !args.is_empty() {
                    f.write_str("<")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        fmt::Display::fmt(arg, f)?;
                    }
                    f.write_str(">")?;
                }
                Ok(())
            }
            Self::Array { element, len } => write!(f, "[{element}; {len}]"),
        }
    }
}
