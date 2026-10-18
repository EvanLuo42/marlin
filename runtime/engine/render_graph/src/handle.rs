//! Handles: a resource slot and a version, valid for one frame's graph (0007 §2.4).

use core::marker::PhantomData;

/// Ties a handle to one graph's lifetime `'g`, invariantly, so handles cannot outlive the frame
/// or move between graphs of different frames.
pub(crate) type Frame<'g> = PhantomData<fn(&'g ()) -> &'g ()>;

/// Mips and layers of a texture a handle covers. `u32::MAX` counts run to the last one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Range {
    pub base_mip: u32,
    pub mip_count: u32,
    pub base_layer: u32,
    pub layer_count: u32,
}

impl Range {
    pub const ALL: Self = Self {
        base_mip: 0,
        mip_count: u32::MAX,
        base_layer: 0,
        layer_count: u32::MAX,
    };

    /// The range clamped to a texture of `mips` × `layers`, with explicit counts. Panics when it
    /// starts past the end.
    pub fn resolve(self, mips: u32, layers: u32) -> Self {
        assert!(
            self.base_mip < mips && self.base_layer < layers,
            "subresource range starts past the texture's {mips} mips × {layers} layers"
        );
        let mip_count = self.mip_count.min(mips - self.base_mip);
        let layer_count = self.layer_count.min(layers - self.base_layer);
        Self {
            mip_count,
            layer_count,
            ..self
        }
    }

    pub const fn to_rhi(self) -> marlin_rhi::SubresourceRange {
        marlin_rhi::SubresourceRange {
            base_mip: self.base_mip,
            mip_count: self.mip_count,
            base_layer: self.base_layer,
            layer_count: self.layer_count,
        }
    }

    /// `count` items starting `offset` into the run `base..base + len` (`len` may be `u32::MAX`).
    fn narrow(base: u32, len: u32, offset: u32, count: u32) -> (u32, u32) {
        assert!(
            len == u32::MAX || (offset < len && count <= len - offset),
            "subresource {offset}..+{count} outside a handle covering {len}"
        );
        (base + offset, count)
    }
}

/// A texture in this frame's graph, at one version. `Copy`; every write returns the next
/// version, and using an older one is a bug Development builds catch.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextureHandle<'g> {
    pub(crate) index: u32,
    pub(crate) version: u32,
    pub(crate) range: Range,
    pub(crate) _frame: Frame<'g>,
}

impl<'g> TextureHandle<'g> {
    pub(crate) const fn new(index: u32, version: u32) -> Self {
        Self {
            index,
            version,
            range: Range::ALL,
            _frame: PhantomData,
        }
    }

    /// Mip `i` of what this handle covers.
    pub fn mip(self, i: u32) -> Self {
        self.mips(i, 1)
    }

    /// `count` mips from mip `first` of what this handle covers.
    pub fn mips(self, first: u32, count: u32) -> Self {
        let (base_mip, mip_count) =
            Range::narrow(self.range.base_mip, self.range.mip_count, first, count);
        Self {
            range: Range {
                base_mip,
                mip_count,
                ..self.range
            },
            ..self
        }
    }

    /// Layer `i` of what this handle covers.
    pub fn layer(self, i: u32) -> Self {
        self.layers(i, 1)
    }

    /// `count` layers from layer `first` of what this handle covers.
    pub fn layers(self, first: u32, count: u32) -> Self {
        let (base_layer, layer_count) =
            Range::narrow(self.range.base_layer, self.range.layer_count, first, count);
        Self {
            range: Range {
                base_layer,
                layer_count,
                ..self.range
            },
            ..self
        }
    }

    /// The resource's slot in the graph: stable for the frame, shared by all versions.
    pub const fn index(self) -> u32 {
        self.index
    }

    pub const fn version(self) -> u32 {
        self.version
    }
}

/// A buffer in this frame's graph, at one version; see [`TextureHandle`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BufferHandle<'g> {
    pub(crate) index: u32,
    pub(crate) version: u32,
    /// Bytes from the start; the view a pass gets covers `offset..offset + size`.
    pub(crate) offset: u64,
    /// `u64::MAX`: to the end.
    pub(crate) size: u64,
    pub(crate) _frame: Frame<'g>,
}

impl<'g> BufferHandle<'g> {
    pub(crate) const fn new(index: u32, version: u32) -> Self {
        Self {
            index,
            version,
            offset: 0,
            size: u64::MAX,
            _frame: PhantomData,
        }
    }

    /// `size` bytes from `offset` into what this handle covers. Views a pass takes cover the
    /// range; barriers still cover the whole buffer (the RHI's buffer barriers have no range).
    pub fn range(self, offset: u64, size: u64) -> Self {
        assert!(
            self.size == u64::MAX || (offset <= self.size && size <= self.size - offset),
            "buffer range {offset}..+{size} outside a handle covering {} bytes",
            self.size
        );
        Self {
            offset: self.offset + offset,
            size,
            ..self
        }
    }

    pub const fn index(self) -> u32 {
        self.index
    }

    pub const fn version(self) -> u32 {
        self.version
    }
}
