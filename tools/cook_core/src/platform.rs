//! Cook targets: what each platform's cooked data must look like, from the `[cook]` table of its
//! `platforms/<name>/platform.toml` (compiled in by build.rs). Cookers decide per target from
//! this description, never by matching platform names.

use marlin_compress::Codec;

/// A block-compression family of texture formats.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureFamily {
    /// BC1–BC7 (desktop and console GPUs).
    Bc,
    /// ASTC (mobile and Apple GPUs).
    Astc,
    /// ETC2 / EAC (OpenGL ES 3 and Vulkan mobile baseline).
    Etc2,
}

impl TextureFamily {
    pub const ALL: [TextureFamily; 3] =
        [TextureFamily::Bc, TextureFamily::Astc, TextureFamily::Etc2];

    /// The name `texture-compression` uses.
    pub const fn name(self) -> &'static str {
        match self {
            TextureFamily::Bc => "bc",
            TextureFamily::Astc => "astc",
            TextureFamily::Etc2 => "etc2",
        }
    }
}

/// How texture data is laid out in GPU memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureLayout {
    /// Rows of blocks, as uploaded through copies (desktop).
    Linear,
    /// Swizzled by the platform's tiler, ready to map (consoles).
    Tiled,
}

impl TextureLayout {
    pub const ALL: [TextureLayout; 2] = [TextureLayout::Linear, TextureLayout::Tiled];

    /// The name `texture-layout` uses.
    pub const fn name(self) -> &'static str {
        match self {
            TextureLayout::Linear => "linear",
            TextureLayout::Tiled => "tiled",
        }
    }
}

/// One cook target.
#[derive(Debug, PartialEq, Eq)]
pub struct Platform {
    /// As in `platforms/<name>/`.
    pub name: &'static str,
    /// Texture formats the platform's GPUs sample, preferred first.
    pub texture_compression: &'static [TextureFamily],
    pub texture_layout: TextureLayout,
    /// Default package codec of unit heads (decoded on the CPU).
    pub head_codec: Codec,
    /// Default package codec of bulk blobs (GDeflate where the GPU decompresses).
    pub bulk_codec: Codec,
}

/// No `platforms/<name>/platform.toml` was built in.
#[derive(Debug, marlin_error::Error)]
#[error(
    "unknown platform `{name}`: known are {known} (console platforms need their repository \
     cloned into platforms/ before the cook tools are built)"
)]
pub struct UnknownPlatform {
    pub name: String,
    pub known: String,
}

impl Platform {
    /// Every platform with a manifest, sorted by name.
    pub fn all() -> &'static [Platform] {
        PLATFORMS
    }

    pub fn find(name: &str) -> Result<&'static Platform, UnknownPlatform> {
        PLATFORMS
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| UnknownPlatform {
                name: name.to_owned(),
                known: PLATFORMS
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }

    /// The platform these tools were built for: the default target.
    pub fn host() -> &'static Platform {
        Self::find(HOST).expect("the host platform has a manifest")
    }

    pub fn supports(&self, family: TextureFamily) -> bool {
        self.texture_compression.contains(&family)
    }
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

// The generated table calls these in a static initializer: an unknown name fails the build.

const fn codec(name: &str) -> Codec {
    let mut i = 0;
    while i < Codec::ALL.len() {
        if same(Codec::ALL[i].name(), name) {
            return Codec::ALL[i];
        }
        i += 1;
    }
    panic!("platform.toml: unknown codec (see marlin_compress::Codec::name)")
}

const fn texture_family(name: &str) -> TextureFamily {
    let mut i = 0;
    while i < TextureFamily::ALL.len() {
        if same(TextureFamily::ALL[i].name(), name) {
            return TextureFamily::ALL[i];
        }
        i += 1;
    }
    panic!("platform.toml: unknown texture-compression family (bc, astc or etc2)")
}

const fn texture_layout(name: &str) -> TextureLayout {
    let mut i = 0;
    while i < TextureLayout::ALL.len() {
        if same(TextureLayout::ALL[i].name(), name) {
            return TextureLayout::ALL[i];
        }
        i += 1;
    }
    panic!("platform.toml: unknown texture-layout (linear or tiled)")
}

include!(concat!(env!("OUT_DIR"), "/platforms.rs"));
