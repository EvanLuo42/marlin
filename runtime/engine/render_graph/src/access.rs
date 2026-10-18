//! How passes access resources, and what that means for states and usage (0007 §3.1).

use marlin_rhi::{
    Access as RhiAccess, BufferState, BufferUsage, Layout, Stage, TextureState, TextureUsage,
};

/// A shader, copy or command access a pass declares with `read_*`/`write_*`. Render targets and
/// depth are declared with `color`/`depth` instead.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Access {
    /// Read by compute shaders.
    ComputeRead,
    /// Read by vertex shaders.
    VertexRead,
    /// Read by pixel shaders.
    PixelRead,
    /// Read by mesh and amplification shaders.
    MeshRead,
    /// Read and written by compute shaders (storage views; atomics included).
    ComputeWrite,
    /// Read and written by pixel shaders.
    PixelWrite,
    /// A buffer read through a uniform view.
    Uniform,
    /// A buffer of indirect arguments or counts.
    Indirect,
    /// A buffer of indices.
    Index,
    CopySrc,
    CopyDst,
    /// Filled (`fill_buffer`) or cleared (`clear_storage_texture`).
    Clear,
}

impl Access {
    /// Writes (read-modify-write: the previous contents stay visible to the pass).
    pub const fn is_write(self) -> bool {
        matches!(
            self,
            Self::ComputeWrite | Self::PixelWrite | Self::CopyDst | Self::Clear
        )
    }

    const fn is_copy(self) -> bool {
        matches!(self, Self::CopySrc | Self::CopyDst)
    }
}

/// Every access the graph knows: [`Access`] plus attachments.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Use {
    Shader(Access),
    Color,
    DepthWrite,
    DepthRead,
}

/// Whether an access reads the previous contents, writes, or both.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Effect {
    Read,
    /// Reads and writes: storage writes, copies into, attachments that load.
    Modify,
    /// Writes the whole range without reading it: attachments that clear or don't care.
    Overwrite,
}

impl Effect {
    pub const fn writes(self) -> bool {
        !matches!(self, Self::Read)
    }

    pub const fn reads(self) -> bool {
        !matches!(self, Self::Overwrite)
    }
}

impl Use {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Shader(a) => match a {
                Access::ComputeRead => "ComputeRead",
                Access::VertexRead => "VertexRead",
                Access::PixelRead => "PixelRead",
                Access::MeshRead => "MeshRead",
                Access::ComputeWrite => "ComputeWrite",
                Access::PixelWrite => "PixelWrite",
                Access::Uniform => "Uniform",
                Access::Indirect => "Indirect",
                Access::Index => "Index",
                Access::CopySrc => "CopySrc",
                Access::CopyDst => "CopyDst",
                Access::Clear => "Clear",
            },
            Self::Color => "color attachment",
            Self::DepthWrite => "depth attachment",
            Self::DepthRead => "read-only depth attachment",
        }
    }

    /// The texture state of this access, or `None` if textures cannot be accessed this way.
    pub fn texture_state(self) -> Option<TextureState> {
        let shader_read = |stage| TextureState {
            stage,
            access: RhiAccess::SHADER_RESOURCE,
            layout: Layout::ShaderResource,
        };
        let storage = |stage| TextureState {
            stage,
            access: RhiAccess::UNORDERED_ACCESS,
            layout: Layout::UnorderedAccess,
        };
        Some(match self {
            Self::Shader(a) => match a {
                Access::ComputeRead => shader_read(Stage::COMPUTE_SHADING),
                Access::VertexRead | Access::MeshRead => shader_read(Stage::VERTEX_SHADING),
                Access::PixelRead => shader_read(Stage::PIXEL_SHADING),
                Access::ComputeWrite => storage(Stage::COMPUTE_SHADING),
                Access::PixelWrite => storage(Stage::PIXEL_SHADING),
                Access::CopySrc => TextureState::COPY_SRC,
                Access::CopyDst => TextureState::COPY_DST,
                Access::Clear => TextureState::CLEAR_STORAGE,
                Access::Uniform | Access::Indirect | Access::Index => return None,
            },
            Self::Color => TextureState::RENDER_TARGET,
            Self::DepthWrite => TextureState::DEPTH_WRITE,
            Self::DepthRead => TextureState::DEPTH_READ,
        })
    }

    /// The buffer state of this access, or `None` if buffers cannot be accessed this way.
    pub fn buffer_state(self) -> Option<BufferState> {
        let read = |stage| BufferState {
            stage,
            access: RhiAccess::SHADER_RESOURCE,
        };
        let storage = |stage| BufferState {
            stage,
            access: RhiAccess::UNORDERED_ACCESS,
        };
        Some(match self {
            Self::Shader(a) => match a {
                Access::ComputeRead => read(Stage::COMPUTE_SHADING),
                Access::VertexRead | Access::MeshRead => read(Stage::VERTEX_SHADING),
                Access::PixelRead => read(Stage::PIXEL_SHADING),
                Access::ComputeWrite => storage(Stage::COMPUTE_SHADING),
                Access::PixelWrite => storage(Stage::PIXEL_SHADING),
                Access::Uniform => BufferState {
                    stage: Stage::ALL_SHADING,
                    access: RhiAccess::CONSTANT_BUFFER,
                },
                Access::Indirect => BufferState::INDIRECT,
                Access::Index => BufferState::INDEX,
                Access::CopySrc => BufferState::COPY_SRC,
                Access::CopyDst => BufferState::COPY_DST,
                Access::Clear => BufferState::CLEAR_STORAGE,
            },
            Self::Color | Self::DepthWrite | Self::DepthRead => return None,
        })
    }

    pub fn texture_usage(self) -> TextureUsage {
        match self {
            Self::Shader(a) => match a {
                Access::ComputeRead | Access::VertexRead | Access::PixelRead | Access::MeshRead => {
                    TextureUsage::SAMPLED
                }
                Access::ComputeWrite | Access::PixelWrite | Access::Clear => TextureUsage::STORAGE,
                Access::CopySrc => TextureUsage::COPY_SRC,
                Access::CopyDst => TextureUsage::COPY_DST,
                Access::Uniform | Access::Indirect | Access::Index => TextureUsage::default(),
            },
            Self::Color => TextureUsage::RENDER_TARGET,
            Self::DepthWrite | Self::DepthRead => TextureUsage::DEPTH_STENCIL,
        }
    }

    pub fn buffer_usage(self) -> BufferUsage {
        match self {
            Self::Shader(a) => match a {
                Access::ComputeRead | Access::VertexRead | Access::PixelRead | Access::MeshRead => {
                    BufferUsage::SHADER_READ
                }
                Access::ComputeWrite | Access::PixelWrite | Access::Clear => {
                    BufferUsage::SHADER_WRITE
                }
                Access::Uniform => BufferUsage::UNIFORM,
                Access::Indirect => BufferUsage::INDIRECT,
                Access::Index => BufferUsage::INDEX,
                Access::CopySrc => BufferUsage::COPY_SRC,
                Access::CopyDst => BufferUsage::COPY_DST,
            },
            Self::Color | Self::DepthWrite | Self::DepthRead => BufferUsage::default(),
        }
    }

    /// Whether a pass of `kind` may declare this access.
    pub fn allowed_in(self, kind: PassKind) -> bool {
        match kind {
            PassKind::Raster => true,
            PassKind::Compute => match self {
                Self::Shader(a) => !matches!(
                    a,
                    Access::VertexRead
                        | Access::PixelRead
                        | Access::MeshRead
                        | Access::PixelWrite
                        | Access::Index
                ),
                _ => false,
            },
            PassKind::Copy => match self {
                Self::Shader(a) => a.is_copy() || matches!(a, Access::Clear),
                _ => false,
            },
        }
    }
}

/// What a pass records.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PassKind {
    /// Draws into color and depth attachments.
    Raster,
    Compute,
    /// Copies and clears.
    Copy,
}

/// Whether two read states of a texture can be one state: they share a layout.
pub(crate) fn texture_reads_merge(a: TextureState, b: TextureState) -> bool {
    a.layout == b.layout
}

/// The stages `stage` stands for: the umbrella stages expanded into the shader stages they
/// cover.
const fn expand(stage: Stage) -> Stage {
    let shading = Stage::VERTEX_SHADING
        .union(Stage::PIXEL_SHADING)
        .union(Stage::COMPUTE_SHADING)
        .union(Stage::RAYTRACING);
    let mut s = stage;
    if stage.contains(Stage::ALL_SHADING) {
        s = s.union(shading);
    }
    if stage.contains(Stage::NON_PIXEL_SHADING) {
        s = s
            .union(Stage::VERTEX_SHADING)
            .union(Stage::COMPUTE_SHADING)
            .union(Stage::RAYTRACING);
    }
    s
}

/// Whether work in `have`'s stages already sees what `want`'s stages need: `have` covers them.
pub(crate) const fn stages_cover(have: Stage, want: Stage) -> bool {
    have.contains(Stage::ALL) || expand(have).contains(expand(want))
}

pub(crate) const fn texture_covers(have: TextureState, want: TextureState) -> bool {
    stages_cover(have.stage, want.stage) && have.access.contains(want.access)
}

pub(crate) const fn buffer_covers(have: BufferState, want: BufferState) -> bool {
    stages_cover(have.stage, want.stage) && have.access.contains(want.access)
}

pub(crate) const fn union_texture(a: TextureState, b: TextureState) -> TextureState {
    TextureState {
        stage: a.stage.union(b.stage),
        access: a.access.union(b.access),
        layout: a.layout,
    }
}

pub(crate) const fn union_buffer(a: BufferState, b: BufferState) -> BufferState {
    BufferState {
        stage: a.stage.union(b.stage),
        access: a.access.union(b.access),
    }
}

/// Accesses that only read.
pub(crate) const READ_ACCESS: RhiAccess = RhiAccess::VERTEX_BUFFER
    .union(RhiAccess::CONSTANT_BUFFER)
    .union(RhiAccess::INDEX_BUFFER)
    .union(RhiAccess::DEPTH_STENCIL_READ)
    .union(RhiAccess::SHADER_RESOURCE)
    .union(RhiAccess::INDIRECT_ARGUMENT)
    .union(RhiAccess::COPY_SRC)
    .union(RhiAccess::RESOLVE_SRC)
    .union(RhiAccess::ACCELERATION_STRUCTURE_READ)
    .union(RhiAccess::SHADING_RATE_SOURCE);

pub(crate) const fn is_read_only(access: RhiAccess) -> bool {
    access.bits() & !READ_ACCESS.bits() == 0
}
