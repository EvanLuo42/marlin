//! Shader assets (`docs/design/0003-asset-pipeline.md` §4.4): a [`ShaderPackage`] in the
//! source tree, cooked into a [`ShaderLibrary`] that holds every entry point of the package,
//! compiled for the platform's target, by full name.
//!
//! The package's name is the object's `!Name`, and the build's name table finds a package by
//! it, so the engine asks for `Marlin.Engine` and then for `Marlin.Engine.ForwardVs`; nothing
//! names a shader by path or UUID in code.
//!
//! A [`Shader`] is one permutation (type arguments, static parameters) that a material links
//! to; it cooks into a library of its one entry.

use alloc::string::String;
use alloc::vec::Vec;

use marlin_asset::{Link, SourceFile};
use marlin_reflect::Reflect;

use crate::reflect::{ConstValue, ShaderReflection};

/// A Marlin shader package (0004 §7.2): its source files and the packages it uses. The core
/// library (`Marlin`) comes with the compiler and is always a dependency.
///
/// ```text
/// ShaderPackage
/// {
///     !Uuid = "…"
///     !Name = "Marlin.Engine"
///     Sources = [`source:shaders/Forward.mshader`]
/// }
/// ```
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct ShaderPackage {
    /// Every `.mshader` file of the package (a cooker reads only the files an object lists).
    pub sources: Vec<SourceFile>,
    /// Packages whose public declarations this one uses, by link.
    pub dependencies: Vec<Link<ShaderPackage>>,
    /// Only types and functions for other packages (`Marlin.Substrate`): no entry points are
    /// expected, so the cooker does not warn that it has none.
    pub library: bool,
}

/// One permutation of an entry point (0003 §4.4), for what a package's library cannot hold:
/// a generic entry with its type arguments, or static parameters (0004 §13.3) set away from
/// their defaults. Materials link to these.
///
/// `package` is the compile's context: the session holds it and its dependencies, and `entry`
/// and the types in `type_args` are full paths into any of them, so a game package's material
/// type can instantiate an engine entry point.
///
/// ```text
/// Shader
/// {
///     !Uuid = "…"
///     !Name = "RockPixel"
///     Package = <shaders/package:Game #…>
///     Entry = "Marlin.Material.ResolvePixel"
///     TypeArgs = ["Game.Rock"]
///     Consts = [("Game.Rock.UseMoss", Bool(true))]
/// }
/// ```
///
/// It cooks into a [`ShaderLibrary`] with the one entry. The link to the package is a build
/// dependency, not an import: loading the permutation does not load the package's library.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct Shader {
    pub package: Link<ShaderPackage>,
    /// The entry function's full path.
    pub entry: String,
    /// Full paths of the types for the entry's generic parameters, in order.
    pub type_args: Vec<String>,
    /// Static parameters by full path (`Game.Rock.UseMoss`), with their values.
    pub consts: Vec<(String, ConstValue)>,
}

/// A cooked [`ShaderPackage`]: each entry point that compiles without type arguments (public,
/// with a stage attribute, not generic), sorted by name. A cooked [`Shader`] is a library of
/// its one entry.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct ShaderLibrary {
    /// The package's name (`Marlin.Engine`).
    pub package: String,
    /// The compiler target the code is for (`vulkan-1.4`), from the platform description.
    pub target: String,
    pub entries: Vec<LibraryEntry>,
}

/// One compiled entry point.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct LibraryEntry {
    /// The entry's full path (`Marlin.Engine.ForwardVs`).
    pub name: String,
    /// The code for the target's graphics API (SPIR-V for Vulkan); the module's entry point is
    /// `main`.
    pub code: Vec<u8>,
    pub reflection: ShaderReflection,
}

impl ShaderLibrary {
    /// The entry point named `name` (its full path).
    pub fn entry(&self, name: &str) -> Option<&LibraryEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.entries[i])
    }
}
