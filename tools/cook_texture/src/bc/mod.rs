//! Block encoders and reference decoders, one 4×4 block at a time.
//!
//! Encoders take LDR texels scaled to 0–255 in the format's color space, or linear floats for
//! BC6H. Decoders return what a shader reads through a view of the format's linear twin: unorm
//! values in [0, 1], BC6H half floats widened to f32. Missing channels read as 0, missing alpha
//! as 1, as on the GPU.

mod bc1;
mod bc4;
mod bc6h;
mod bc7;
mod bits;
mod fit;
mod tables;

use fit::Vec4;

use crate::color::f16_to_f32;
use crate::format::{EncodeOptions, Format, Interpolation};

/// Encodes one block into `out` (`format.block_size()` bytes).
pub(crate) fn encode_block(
    format: Format,
    texels: &[Vec4; 16],
    options: &EncodeOptions,
    out: &mut [u8],
) {
    let channel = |c: usize| -> [f32; 16] { core::array::from_fn(|i| texels[i][c]) };
    let bc4 = |mask: u32| bc4::Params {
        quality: options.quality,
        mode_mask: mask,
    };
    match format {
        Format::Bc1Unorm | Format::Bc1UnormSrgb => out.copy_from_slice(&bc1::encode(
            texels,
            &bc1::Params {
                quality: options.quality,
                weights: options.channel_weights,
                four_only: false,
                alpha_cutoff: options.alpha_cutoff.map(|c| c * 255.0),
                mode_mask: options.mode_mask,
            },
        )),
        Format::Bc3Unorm | Format::Bc3UnormSrgb => {
            out[..8].copy_from_slice(&bc4::encode(&channel(3), &bc4(options.mode_mask)));
            out[8..].copy_from_slice(&bc1::encode(
                texels,
                &bc1::Params {
                    quality: options.quality,
                    weights: options.channel_weights,
                    four_only: true,
                    alpha_cutoff: None,
                    mode_mask: u32::MAX,
                },
            ));
        }
        Format::Bc4Unorm => {
            out.copy_from_slice(&bc4::encode(&channel(0), &bc4(options.mode_mask)));
        }
        Format::Bc5Unorm => {
            out[..8].copy_from_slice(&bc4::encode(&channel(0), &bc4(options.mode_mask)));
            out[8..].copy_from_slice(&bc4::encode(&channel(1), &bc4(options.mode_mask)));
        }
        Format::Bc6hUfloat | Format::Bc6hSfloat => out.copy_from_slice(&bc6h::encode(
            texels,
            &bc6h::Params {
                quality: options.quality,
                weights: options.channel_weights,
                signed: format == Format::Bc6hSfloat,
                mode_mask: options.mode_mask,
            },
        )),
        Format::Bc7Unorm | Format::Bc7UnormSrgb => out.copy_from_slice(&bc7::encode(
            texels,
            &bc7::Params {
                quality: options.quality,
                weights: options.channel_weights,
                mode_mask: options.mode_mask,
            },
        )),
    }
}

/// Decodes one block (`format.block_size()` bytes).
pub(crate) fn decode_block(
    format: Format,
    block: &[u8],
    interpolation: Interpolation,
) -> [[f32; 4]; 16] {
    match format {
        Format::Bc1Unorm | Format::Bc1UnormSrgb => bc1::decode(block, false, interpolation),
        Format::Bc3Unorm | Format::Bc3UnormSrgb => {
            let alpha = bc4::decode(&block[..8], interpolation, true);
            let mut texels = bc1::decode(&block[8..], true, interpolation);
            for (t, a) in texels.iter_mut().zip(alpha) {
                t[3] = a;
            }
            texels
        }
        Format::Bc4Unorm => bc4::decode(block, interpolation, false).map(|r| [r, 0.0, 0.0, 1.0]),
        Format::Bc5Unorm => {
            let r = bc4::decode(&block[..8], interpolation, false);
            let g = bc4::decode(&block[8..], interpolation, false);
            core::array::from_fn(|i| [r[i], g[i], 0.0, 1.0])
        }
        Format::Bc6hUfloat | Format::Bc6hSfloat => {
            let block = block.try_into().unwrap();
            bc6h::decode(block, format == Format::Bc6hSfloat)
                .map(|[r, g, b]| [f16_to_f32(r), f16_to_f32(g), f16_to_f32(b), 1.0])
        }
        Format::Bc7Unorm | Format::Bc7UnormSrgb => {
            bc7::decode(block.try_into().unwrap()).map(|t| t.map(|c| c as f32 / 255.0))
        }
    }
}

/// The mode an encoded block uses, numbered like [`EncodeOptions::mode_mask`]'s bits: BC1 0
/// four-color, 1 three-color; BC3, BC4, BC5 (their first channel block) 0 eight-value, 1
/// six-value; BC6H spec mode − 1; BC7 the mode. `None` for BC6H's and BC7's reserved modes.
pub fn block_mode(format: Format, block: &[u8]) -> Option<u32> {
    let bc4_mode = |b: &[u8]| (b[0] <= b[1]) as u32;
    match format {
        Format::Bc1Unorm | Format::Bc1UnormSrgb => {
            let c0 = u16::from_le_bytes([block[0], block[1]]);
            let c1 = u16::from_le_bytes([block[2], block[3]]);
            Some((c0 <= c1) as u32)
        }
        Format::Bc3Unorm | Format::Bc3UnormSrgb | Format::Bc4Unorm | Format::Bc5Unorm => {
            Some(bc4_mode(block))
        }
        Format::Bc6hUfloat | Format::Bc6hSfloat => {
            bc6h::block_mode(block.try_into().unwrap()).map(|m| m as u32)
        }
        Format::Bc7Unorm | Format::Bc7UnormSrgb => {
            bc7::block_mode(block.try_into().unwrap()).map(|m| m as u32)
        }
    }
}
