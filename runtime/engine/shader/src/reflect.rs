//! What the shader compiler says about one compiled entry point (`docs/design/0005-shader-compiler.md`
//! §6): plain data, registered with marlin-reflect, cooked next to the code like any other
//! object (0003). The compiler's `marlin-shaderc-reflect` builds it; the engine reads it to
//! dispatch, to check its push structs against the shader's (0005 §9) and to pick permutations.
//!
//! Names are the source's: `Game.Rock.UseMoss`, `Topology`. Layouts are the compiler's
//! (scalar layout, 0004 §16): offsets and sizes in bytes.

use alloc::string::String;
use alloc::vec::Vec;

use marlin_reflect::Reflect;

/// One entry point's reflection.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct ShaderReflection {
    pub entry: EntryInfo,
    /// The entry's `[Push]` parameter; `None` when it takes none.
    pub push: Option<StructLayout>,
    /// The stage inputs and outputs (0004 §11.2): system values, locations in declaration
    /// order, render targets.
    pub inputs: Vec<StageVariable>,
    pub outputs: Vec<StageVariable>,
    /// The static parameters the code uses (`const` fields of structs, 0004 §13.3), with
    /// their values for this compile, by path.
    pub static_params: Vec<NamedValue>,
    /// The entry's `[Export]` constants (0004 §13.4), in source order.
    pub exports: Vec<NamedValue>,
    /// The optional GPU features the code needs (0004 §12.3), by name, sorted.
    pub capabilities: Vec<String>,
    /// The `Target.*` constants the code reads, sorted: the permutation dimensions a target
    /// decides.
    pub target_constants: Vec<String>,
    /// The source files the code was compiled from, with their contents' hashes (0005 §8).
    pub sources: Vec<SourceHash>,
    pub stats: Stats,
    /// What only one backend has to say, by name (0005 §6.1).
    pub extensions: Vec<Extension>,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct EntryInfo {
    /// The entry function's path, with its type arguments (`Game.Resolve<Game.Rock>`).
    pub name: String,
    pub stage: Stage,
    /// Threads per group, for compute, mesh and amplification entries (1s otherwise).
    pub group_size: [u32; 3],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Reflect)]
pub enum Stage {
    #[default]
    Compute,
    Vertex,
    Pixel,
    Mesh,
    Amplification,
}

/// One stage input or output.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct StageVariable {
    /// The parameter, or the stage I/O struct's field (`Uv`).
    pub name: String,
    /// The compiler's type (`f32x2`).
    pub ty: String,
    pub binding: StageBinding,
}

/// How a stage variable connects to the pipeline.
#[derive(Clone, PartialEq, Eq, Debug, Reflect)]
pub enum StageBinding {
    /// A system value, by the attribute's name (`Position`).
    SystemValue(String),
    Location(u32),
    /// A render target, and which dual-source blending source it is.
    Target {
        index: u32,
        source: u32,
    },
}

impl Default for StageBinding {
    fn default() -> Self {
        StageBinding::Location(0)
    }
}

/// A struct as the shader lays it out.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct StructLayout {
    /// The struct's path (`Game.Args`).
    pub name: String,
    pub size: u32,
    pub fields: Vec<FieldLayout>,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct FieldLayout {
    /// As the source writes it (`RoughnessScale`).
    pub name: String,
    /// The type as the source writes it (`float3`, `Buffer<Game.Particle>`).
    pub ty: String,
    pub offset: u32,
    pub size: u32,
    /// The user attributes on the field, as written (0004 §14).
    pub attributes: Vec<Attribute>,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct Attribute {
    /// The attribute's path, without the `Attribute` suffix (`Range`).
    pub name: String,
    /// Each argument's source text (`0`, `2`, `max: 4`).
    pub args: Vec<String>,
}

/// A compile-time value with its name and type.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct NamedValue {
    pub name: String,
    /// As the source writes it (`uint`, `Game.Topology`).
    pub ty: String,
    pub value: ConstValue,
}

/// A value computed at compile time.
#[derive(Clone, PartialEq, Debug, Reflect)]
pub enum ConstValue {
    Bool(bool),
    /// Signed integers, and enums with a signed underlying type.
    Int(i64),
    /// Unsigned integers, and enums with an unsigned underlying type.
    UInt(u64),
    Float(f64),
    /// Vectors, matrices (by column), arrays and tuples, in order.
    List(Vec<ConstValue>),
    /// A struct's fields, in declaration order.
    Struct(Vec<FieldValue>),
}

impl Default for ConstValue {
    fn default() -> Self {
        ConstValue::Bool(false)
    }
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct FieldValue {
    pub name: String,
    pub value: ConstValue,
}

/// A source file: its package, its path in the package, and the xxh3-128 hash of its text.
#[derive(Clone, PartialEq, Eq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct SourceHash {
    pub package: String,
    pub path: String,
    pub hash: u128,
}

/// Numbers for the cook report (0005 §7).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct Stats {
    /// Instructions of the optimized IR.
    pub instructions: u32,
    /// Bytes of groupshared memory.
    pub groupshared_bytes: u32,
    /// Bytes of the backend's code.
    pub code_bytes: u32,
}

/// A backend's own data.
#[derive(Clone, PartialEq, Eq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct Extension {
    pub name: String,
    pub data: Vec<u8>,
}

/// Registers the reflection types with marlin-reflect, for loading cooked reflection by type.
pub fn register() -> Result<(), marlin_reflect::IdCollision> {
    marlin_reflect::register::<ShaderReflection>()
}
