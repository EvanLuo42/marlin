//! IR types (0006 §3), interned per module.

use marlin_lang_arena::Idx;

use crate::module::StructData;

pub type Type = Idx<TypeData>;
pub type StructId = Idx<StructData>;

/// Integers have no sign; operations say how they read the bits (`sdiv`, `ult`). `I1` is bool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scalar {
    I1,
    I16,
    I32,
    I64,
    F16,
    F32,
    F64,
}

impl Scalar {
    pub const ALL: [Scalar; 7] = [
        Scalar::I1,
        Scalar::I16,
        Scalar::I32,
        Scalar::I64,
        Scalar::F16,
        Scalar::F32,
        Scalar::F64,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scalar::I1 => "i1",
            Scalar::I16 => "i16",
            Scalar::I32 => "i32",
            Scalar::I64 => "i64",
            Scalar::F16 => "f16",
            Scalar::F32 => "f32",
            Scalar::F64 => "f64",
        }
    }

    pub fn from_name(name: &str) -> Option<Scalar> {
        Scalar::ALL.into_iter().find(|s| s.name() == name)
    }

    pub fn bits(self) -> u32 {
        match self {
            Scalar::I1 => 1,
            Scalar::I16 | Scalar::F16 => 16,
            Scalar::I32 | Scalar::F32 => 32,
            Scalar::I64 | Scalar::F64 => 64,
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, Scalar::F16 | Scalar::F32 | Scalar::F64)
    }

    pub fn is_int(self) -> bool {
        !self.is_float()
    }
}

/// Address spaces (0006 §6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Space {
    Function,
    GroupShared,
    Buffer,
    Uniform,
    Constant,
    Push,
    Input,
    Output,
    MeshVertex,
    MeshPrimitive,
    MeshIndex,
    Payload,
}

impl Space {
    pub const ALL: [Space; 12] = [
        Space::Function,
        Space::GroupShared,
        Space::Buffer,
        Space::Uniform,
        Space::Constant,
        Space::Push,
        Space::Input,
        Space::Output,
        Space::MeshVertex,
        Space::MeshPrimitive,
        Space::MeshIndex,
        Space::Payload,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Space::Function => "function",
            Space::GroupShared => "groupshared",
            Space::Buffer => "buffer",
            Space::Uniform => "uniform",
            Space::Constant => "constant",
            Space::Push => "push",
            Space::Input => "input",
            Space::Output => "output",
            Space::MeshVertex => "mesh_vertex",
            Space::MeshPrimitive => "mesh_primitive",
            Space::MeshIndex => "mesh_index",
            Space::Payload => "payload",
        }
    }

    pub fn from_name(name: &str) -> Option<Space> {
        Space::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Spaces that cannot be written whatever the pointer says.
    pub fn is_read_only(self) -> bool {
        matches!(
            self,
            Space::Uniform | Space::Constant | Space::Push | Space::Input
        )
    }
}

/// Resource kinds (0004 §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceKind {
    Uniform,
    Buffer,
    ReadOnlyBuffer,
    ByteBuffer,
    ReadOnlyByteBuffer,
    Texture2D,
    Texture2DArray,
    Texture3D,
    TextureCube,
    TextureCubeArray,
    Texture2DMS,
    Texture2DMSArray,
    StorageTexture2D,
    StorageTexture2DArray,
    StorageTexture3D,
    Sampler,
    ComparisonSampler,
    AccelerationStructure,
}

impl ResourceKind {
    pub const ALL: [ResourceKind; 18] = [
        ResourceKind::Uniform,
        ResourceKind::Buffer,
        ResourceKind::ReadOnlyBuffer,
        ResourceKind::ByteBuffer,
        ResourceKind::ReadOnlyByteBuffer,
        ResourceKind::Texture2D,
        ResourceKind::Texture2DArray,
        ResourceKind::Texture3D,
        ResourceKind::TextureCube,
        ResourceKind::TextureCubeArray,
        ResourceKind::Texture2DMS,
        ResourceKind::Texture2DMSArray,
        ResourceKind::StorageTexture2D,
        ResourceKind::StorageTexture2DArray,
        ResourceKind::StorageTexture3D,
        ResourceKind::Sampler,
        ResourceKind::ComparisonSampler,
        ResourceKind::AccelerationStructure,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ResourceKind::Uniform => "Uniform",
            ResourceKind::Buffer => "Buffer",
            ResourceKind::ReadOnlyBuffer => "ReadOnlyBuffer",
            ResourceKind::ByteBuffer => "ByteBuffer",
            ResourceKind::ReadOnlyByteBuffer => "ReadOnlyByteBuffer",
            ResourceKind::Texture2D => "Texture2D",
            ResourceKind::Texture2DArray => "Texture2DArray",
            ResourceKind::Texture3D => "Texture3D",
            ResourceKind::TextureCube => "TextureCube",
            ResourceKind::TextureCubeArray => "TextureCubeArray",
            ResourceKind::Texture2DMS => "Texture2DMS",
            ResourceKind::Texture2DMSArray => "Texture2DMSArray",
            ResourceKind::StorageTexture2D => "StorageTexture2D",
            ResourceKind::StorageTexture2DArray => "StorageTexture2DArray",
            ResourceKind::StorageTexture3D => "StorageTexture3D",
            ResourceKind::Sampler => "Sampler",
            ResourceKind::ComparisonSampler => "ComparisonSampler",
            ResourceKind::AccelerationStructure => "AccelerationStructure",
        }
    }

    pub fn from_name(name: &str) -> Option<ResourceKind> {
        ResourceKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// Whether the kind takes an element type (`Buffer<T>`, `Texture2D<T>`).
    pub fn has_element(self) -> bool {
        !matches!(
            self,
            ResourceKind::ByteBuffer
                | ResourceKind::ReadOnlyByteBuffer
                | ResourceKind::Sampler
                | ResourceKind::ComparisonSampler
                | ResourceKind::AccelerationStructure
        )
    }
}

/// A resource kind with its element type: `Texture2D<f32x4>`, `Buffer<@Light>`, `Sampler`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResKind {
    pub kind: ResourceKind,
    pub element: Option<Type>,
    /// A texture of signed integers (`Texture2D<signed i32>`): the IR's integers are
    /// signless, image types are not.
    pub signed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeData {
    Scalar(Scalar),
    /// 2 to 4 elements.
    Vector(Scalar, u8),
    /// Rows × columns, column-major.
    Matrix {
        element: Scalar,
        rows: u8,
        cols: u8,
    },
    Array(Type, u32),
    Struct(StructId),
    /// A logical pointer (0006 §6); `readonly` for pointers derived from read-only views.
    Ptr {
        space: Space,
        readonly: bool,
        pointee: Type,
    },
    /// A resource index (0006 §7), which moves freely.
    Res(ResKind),
    /// A materialized descriptor, which stays where it is made.
    Handle(ResKind),
    RayQuery,
}

/// What a texture kind is like, for texture operations (0006 §8.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureShape {
    /// Components of a coordinate: 2 (2D), 3 (3D, a cube's direction).
    pub dims: u8,
    pub cube: bool,
    pub array: bool,
    pub multisampled: bool,
    /// Written through an indexer, not sampled.
    pub storage: bool,
}

impl ResourceKind {
    /// The shape of a texture kind; `None` for the others.
    pub fn texture(self) -> Option<TextureShape> {
        let t = |dims, cube, array, multisampled, storage| {
            Some(TextureShape {
                dims,
                cube,
                array,
                multisampled,
                storage,
            })
        };
        match self {
            ResourceKind::Texture2D => t(2, false, false, false, false),
            ResourceKind::Texture2DArray => t(2, false, true, false, false),
            ResourceKind::Texture3D => t(3, false, false, false, false),
            ResourceKind::TextureCube => t(3, true, false, false, false),
            ResourceKind::TextureCubeArray => t(3, true, true, false, false),
            ResourceKind::Texture2DMS => t(2, false, false, true, false),
            ResourceKind::Texture2DMSArray => t(2, false, true, true, false),
            ResourceKind::StorageTexture2D => t(2, false, false, false, true),
            ResourceKind::StorageTexture2DArray => t(2, false, true, false, true),
            ResourceKind::StorageTexture3D => t(3, false, false, false, true),
            _ => None,
        }
    }
}
