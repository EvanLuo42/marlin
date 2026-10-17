//! Golden images (docs/design/0014-testing.md §7.1): a frame rendered offscreen and read back,
//! compared with a reference PNG under `<crate>/tests/data/golden/<backend>/<name>.png` (kept in
//! Git LFS) within a per-channel tolerance.
//!
//! On a mismatch the actual image and a diff image go to `target/golden/<crate>/`, and the panic
//! names them. `MARLIN_BLESS=1` rewrites the references that do not match instead.

use std::fs;
use std::path::{Path, PathBuf};

use marlin_rhi::{BufferTextureCopy, Device, Format, Texture};

use crate::{read_bytes, readback, run};

/// An 8-bit RGBA image, rows top to bottom, no padding.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

impl Image {
    pub fn new(width: u32, height: u32, rgba8: Vec<u8>) -> Self {
        assert_eq!(
            rgba8.len(),
            width as usize * height as usize * 4,
            "{width}x{height} RGBA8 pixels"
        );
        Self {
            width,
            height,
            rgba8,
        }
    }

    /// From pixels packed as little-endian `u32`s (R in the low byte), as the renderers' readback
    /// helpers return them.
    pub fn from_words(width: u32, height: u32, words: &[u32]) -> Self {
        Self::new(
            width,
            height,
            words.iter().flat_map(|w| w.to_le_bytes()).collect(),
        )
    }

    /// The RGBA bytes of pixel (x, y).
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.rgba8[i..i + 4].try_into().unwrap()
    }

    /// Reads a PNG of any color type and bit depth, converted to 8-bit RGBA.
    pub fn read_png(path: &Path) -> Result<Self, String> {
        let file = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
        let mut buf = vec![0; reader.output_buffer_size().ok_or("image too large")?];
        let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        let pixels = &buf[..info.buffer_size()];
        let rgba8 = match info.color_type {
            png::ColorType::Rgba => pixels.to_vec(),
            png::ColorType::Rgb => pixels
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            png::ColorType::GrayscaleAlpha => pixels
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            png::ColorType::Grayscale => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => return Err("palette not expanded".into()),
        };
        Ok(Self::new(info.width, info.height, rgba8))
    }

    /// Writes the image as an 8-bit RGBA PNG, creating parent directories.
    pub fn write_png(&self, path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = fs::File::create(path)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&self.rgba8).unwrap();
        writer.finish().unwrap();
    }
}

impl core::fmt::Debug for Image {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Image({}x{})", self.width, self.height)
    }
}

/// How far an image may stray from its reference.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tolerance {
    /// The largest difference in any channel for a pixel to count as matching.
    pub per_channel: u8,
    /// The fraction of pixels (0 to 1) allowed to exceed `per_channel`.
    pub max_ratio: f32,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            per_channel: 2,
            max_ratio: 0.0,
        }
    }
}

/// Reads mip 0, layer 0 of a single-sampled 2D texture with four 8-bit channels (`Rgba8*`,
/// `Bgra8*`, converted to RGBA). The texture must be in `TextureState::COPY_SRC`, with the work
/// that wrote it submitted; the copy runs on the graphics queue and is waited for.
pub fn read_texture(device: &Device, texture: &Texture) -> Image {
    let desc = texture.desc();
    let bgra = match desc.format {
        Format::Rgba8Unorm | Format::Rgba8UnormSrgb => false,
        Format::Bgra8Unorm | Format::Bgra8UnormSrgb => true,
        other => panic!("read_texture reads 8-bit RGBA or BGRA textures, not {other:?}"),
    };
    assert_eq!(
        desc.sample_count, 1,
        "read_texture reads single-sampled textures"
    );
    let copy = BufferTextureCopy::whole_mip(desc, 0, 0, 0);
    let buffer = readback(device, copy.bytes_per_row as u64 * copy.height as u64);
    run(device, |list| {
        list.copy_texture_to_buffer(texture, &buffer, &copy)
    });
    let (width, height) = (copy.width, copy.height);
    let rows = read_bytes(&buffer, 0, copy.bytes_per_row as usize * height as usize);
    let mut rgba8 = Vec::with_capacity(width as usize * height as usize * 4);
    for row in rows.chunks_exact(copy.bytes_per_row as usize) {
        rgba8.extend_from_slice(&row[..width as usize * 4]);
    }
    if bgra {
        for p in rgba8.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
    }
    Image::new(width, height, rgba8)
}

/// The backend golden references are kept under. The RHI does not name its backend yet; D3D12 is
/// the only one with hardware. Per-vendor directories (`d3d12-nvidia`) come when vendors differ.
pub fn backend() -> &'static str {
    if cfg!(any(marlin_platform = "windows", marlin_platform = "xbox")) {
        "d3d12"
    } else {
        "unknown"
    }
}

/// Asserts that `image` matches the golden reference `name` of the calling crate within a
/// [`Tolerance`] (default: [`Tolerance::default`]); see the [module docs](crate::image).
///
/// ```ignore
/// assert_image!("lit_cube", &image);
/// assert_image!("lit_cube", &image, Tolerance { per_channel: 4, max_ratio: 0.001 });
/// ```
#[macro_export]
macro_rules! assert_image {
    ($name:expr, $image:expr $(,)?) => {
        $crate::assert_image!($name, $image, $crate::image::Tolerance::default())
    };
    ($name:expr, $image:expr, $tolerance:expr $(,)?) => {
        $crate::image::assert_image_in(
            ::core::env!("CARGO_MANIFEST_DIR"),
            ::core::env!("CARGO_PKG_NAME"),
            $name,
            $image,
            $tolerance,
        )
    };
}

/// What [`assert_image!`] calls, with the crate's manifest directory and package name.
#[track_caller]
pub fn assert_image_in(
    manifest_dir: &str,
    package: &str,
    name: &str,
    image: &Image,
    tolerance: Tolerance,
) {
    let reference_path =
        marlin_test_support::data_path(manifest_dir, &format!("golden/{}/{name}.png", backend()));
    let workspace = workspace_root(Path::new(manifest_dir));
    let reference = reference_path
        .exists()
        .then(|| Image::read_png(&reference_path));
    let mismatch = match &reference {
        Some(Ok(reference)) => compare(reference, image, tolerance),
        Some(Err(e)) => Some(Mismatch::Unreadable(e.clone())),
        None => Some(Mismatch::Missing),
    };
    let Some(mismatch) = mismatch else {
        return;
    };
    if marlin_test_support::bless() {
        image.write_png(&reference_path);
        eprintln!("blessed {}", relative(&workspace, &reference_path));
        return;
    }

    let out = target_dir(&workspace).join("golden").join(package);
    let actual_path = out.join(format!("{name}.actual.png"));
    image.write_png(&actual_path);
    let mut message = format!(
        "golden image {name} ({}): {}\n  actual: {}",
        relative(&workspace, &reference_path),
        mismatch.describe(tolerance),
        relative(&workspace, &actual_path),
    );
    if let Mismatch::Pixels { diff, .. } = &mismatch {
        let diff_path = out.join(format!("{name}.diff.png"));
        diff.write_png(&diff_path);
        message += &format!("\n  diff:   {}", relative(&workspace, &diff_path));
    }
    message += "\nif the change is intended, rerun with MARLIN_BLESS=1 and commit the new reference \
                on its own, saying why";
    panic!("{message}");
}

enum Mismatch {
    Missing,
    Unreadable(String),
    Size {
        expected: (u32, u32),
    },
    Pixels {
        over: usize,
        total: usize,
        worst: u8,
        first: (u32, u32),
        diff: Image,
    },
}

impl Mismatch {
    fn describe(&self, tolerance: Tolerance) -> String {
        match self {
            Self::Missing => {
                "the reference is missing; run with MARLIN_BLESS=1 to create it".into()
            }
            Self::Unreadable(e) => format!("the reference cannot be read: {e}"),
            Self::Size { expected } => {
                format!("the reference is {}x{}", expected.0, expected.1)
            }
            Self::Pixels {
                over,
                total,
                worst,
                first,
                ..
            } => format!(
                "{over} of {total} pixels differ by more than {} (allowed: {:.4}%), \
                 by up to {worst}, first at ({}, {})",
                tolerance.per_channel,
                tolerance.max_ratio * 100.0,
                first.0,
                first.1,
            ),
        }
    }
}

/// `None` if `actual` matches `expected` within `tolerance`.
fn compare(expected: &Image, actual: &Image, tolerance: Tolerance) -> Option<Mismatch> {
    if (expected.width, expected.height) != (actual.width, actual.height) {
        return Some(Mismatch::Size {
            expected: (expected.width, expected.height),
        });
    }
    let mut over = 0;
    let mut worst = 0;
    let mut first = None;
    // The diff: the reference dimmed to grey, with pixels over the tolerance in red (brighter the
    // larger the difference).
    let mut diff = Vec::with_capacity(actual.rgba8.len());
    let pixels = expected
        .rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .zip(actual.rgba8.as_chunks::<4>().0);
    for (i, (e, a)) in pixels.enumerate() {
        let d = e.iter().zip(a).map(|(e, a)| e.abs_diff(*a)).max().unwrap();
        worst = worst.max(d);
        if d > tolerance.per_channel {
            over += 1;
            first.get_or_insert(i);
            diff.extend_from_slice(&[128 + d / 2, 0, 0, 255]);
        } else {
            let luma = ((e[0] as u32 * 3 + e[1] as u32 * 6 + e[2] as u32) / 10 / 3) as u8;
            diff.extend_from_slice(&[luma, luma, luma, 255]);
        }
    }
    let total = actual.rgba8.len() / 4;
    if over as f64 <= tolerance.max_ratio as f64 * total as f64 {
        return None;
    }
    let first = first.unwrap() as u32;
    Some(Mismatch::Pixels {
        over,
        total,
        worst,
        first: (first % actual.width, first / actual.width),
        diff: Image::new(actual.width, actual.height, diff),
    })
}

/// The workspace root: the nearest ancestor of `manifest_dir` holding `Cargo.lock`.
fn workspace_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .find(|dir| dir.join("Cargo.lock").exists())
        .unwrap_or(manifest_dir)
        .to_path_buf()
}

/// `CARGO_TARGET_DIR` if set, else the workspace's `target/`.
fn target_dir(workspace: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => workspace.join(dir),
        None => workspace.join("target"),
    }
}

/// `path` relative to the workspace, with forward slashes (or as is, if outside it).
fn relative(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
