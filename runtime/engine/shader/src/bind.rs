//! Binding a subsystem's shaders: each subsystem that runs engine shaders lists the entries it
//! uses ([`EntryDesc`]: full path, stage and the push constants it sets) next to the code that
//! sets them, and [`bind`] takes them from a cooked [`ShaderLibrary`] once each is checked
//! against the compiler's reflection. Nothing outside the subsystem knows its entries.

use core::fmt;

use crate::asset::{LibraryEntry, ShaderLibrary};
use crate::reflect::{ShaderReflection, Stage};

/// The entry point the compiler gives every module.
pub const MODULE_ENTRY: &str = "main";

/// One entry point a subsystem uses, and what it uses it as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EntryDesc {
    /// The entry's full path (`Marlin.Engine.Lighting.DirectLight`).
    pub name: &'static str,
    pub stage: Stage,
    /// Bytes of push constants the subsystem sets; `None` when it sets none.
    pub push: Option<u32>,
}

impl EntryDesc {
    /// Entry `name` as a `stage` shader that takes no push constants.
    pub const fn new(name: &'static str, stage: Stage) -> Self {
        Self {
            name,
            stage,
            push: None,
        }
    }

    /// A compute entry that takes no push constants.
    pub const fn compute(name: &'static str) -> Self {
        Self::new(name, Stage::Compute)
    }

    /// This entry, taking the push struct `T` the subsystem sets (its `#[repr(C)]` size).
    pub const fn push<T>(self) -> Self {
        self.push_bytes(size_of::<T>() as u32)
    }

    /// This entry, taking `bytes` of push constants: for a push range the struct set does not
    /// fill (a resolve's word 31).
    pub const fn push_bytes(mut self, bytes: u32) -> Self {
        self.push = Some(bytes);
        self
    }

    /// Whether `reflection` is this entry as the subsystem uses it.
    pub fn check(&self, reflection: &ShaderReflection) -> Result<(), BindError> {
        let mismatch = |problem| BindError::Mismatch {
            entry: self.name,
            problem,
        };
        if reflection.entry.stage != self.stage {
            return Err(mismatch(Problem::Stage {
                expected: self.stage,
                found: reflection.entry.stage,
            }));
        }
        let push = reflection.push.as_ref().map(|p| p.size);
        if push != self.push {
            return Err(mismatch(Problem::Push {
                expected: self.push,
                found: push,
            }));
        }
        Ok(())
    }
}

/// Why a library's entries are not what a subsystem uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindError {
    /// The library lacks an entry the subsystem uses.
    MissingEntry(&'static str),
    /// An entry is not what the subsystem uses it as.
    Mismatch {
        entry: &'static str,
        problem: Problem,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Problem {
    Stage {
        expected: Stage,
        found: Stage,
    },
    /// Bytes of push constants.
    Push {
        expected: Option<u32>,
        found: Option<u32>,
    },
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntry(entry) => write!(f, "the shader library has no entry `{entry}`"),
            Self::Mismatch {
                entry,
                problem: Problem::Stage { expected, found },
            } => write!(f, "`{entry}` is a {found:?} shader, used as {expected:?}"),
            Self::Mismatch {
                entry,
                problem: Problem::Push { expected, found },
            } => write!(
                f,
                "`{entry}` takes {} of push constants, {} set",
                bytes(*found),
                bytes(*expected)
            ),
        }
    }
}

fn bytes(push: Option<u32>) -> alloc::string::String {
    match push {
        Some(b) => alloc::format!("{b} bytes"),
        None => "none".into(),
    }
}

impl core::error::Error for BindError {}

/// The entries `entries` names, in order, each checked against its reflection.
pub fn entries<'a, const N: usize>(
    library: &'a ShaderLibrary,
    entries: &[EntryDesc; N],
) -> Result<[&'a LibraryEntry; N], BindError> {
    let mut found = [None; N];
    for (slot, desc) in found.iter_mut().zip(entries) {
        let e = library
            .entry(desc.name)
            .ok_or(BindError::MissingEntry(desc.name))?;
        desc.check(&e.reflection)?;
        *slot = Some(e);
    }
    Ok(found.map(|e| e.expect("every slot filled")))
}

#[cfg(feature = "rhi")]
pub use modules::{bind, bind_one, module};

#[cfg(feature = "rhi")]
mod modules {
    use marlin_rhi::ShaderModule;

    use super::{BindError, EntryDesc, MODULE_ENTRY, entries};
    use crate::asset::{LibraryEntry, ShaderLibrary};

    /// `entry`'s code as a module.
    pub fn module(entry: &LibraryEntry) -> ShaderModule<'_> {
        ShaderModule {
            code: &entry.code,
            entry: MODULE_ENTRY,
        }
    }

    /// The modules of the entries `descs` names, in order, each checked against its
    /// reflection.
    pub fn bind<'a, const N: usize>(
        library: &'a ShaderLibrary,
        descs: &[EntryDesc; N],
    ) -> Result<[ShaderModule<'a>; N], BindError> {
        Ok(entries(library, descs)?.map(module))
    }

    /// The module of the one entry `desc` names.
    pub fn bind_one<'a>(
        library: &'a ShaderLibrary,
        desc: &EntryDesc,
    ) -> Result<ShaderModule<'a>, BindError> {
        bind(library, core::array::from_ref(desc)).map(|[m]| m)
    }
}
