//! Damage detection (0012 §5.5): every command's content hash, which UiCoarse folds into a hash
//! per tile and compares with the tile's last one, and the hash of a whole frame, which the CPU
//! compares to skip a frame that would draw the same.
//!
//! A command's hash covers everything that decides its pixels and nothing that is only a
//! position in this frame's buffers: its kind and flags, payload, paint (a gradient's record,
//! an external image's version), group and clip chain, a glyph's place in the glyph cache, and
//! for a group effect, its members' hashes.

use alloc::vec::Vec;

use marlin_font::GlyphRecord;
use marlin_hash::xxh3_64;

use crate::command::{Chunk, Kind, PAINT_EXTERNAL, PAINT_GRADIENT, clip_of, group_of};
use crate::style::ExternalImageId;

fn hash_words(words: &[u32]) -> u64 {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    xxh3_64(&bytes)
}

/// Every command's hash (32 bits each, never 0), in order. `externals` give the version of each
/// external image the frame draws.
pub fn command_hashes(
    chunk: &Chunk,
    glyphs: &[GlyphRecord],
    externals: &[(ExternalImageId, u64)],
) -> Vec<u32> {
    let mut words: Vec<u32> = Vec::new();
    let mut hashes = Vec::with_capacity(chunk.commands.len());
    for c in &chunk.commands {
        words.clear();
        words.push(c[0] & 0xFFFF);
        let at = c[3] as usize;
        let kind = Kind::from_word(c[0]);
        let len = kind.map_or(0, |k| k.payload_len(&chunk.payload[at..]));
        words.extend_from_slice(&chunk.payload[at..at + len]);
        let paint = chunk.paints[c[2] as usize];
        words.extend_from_slice(&paint);
        match paint[0] {
            PAINT_GRADIENT => words.extend_from_slice(&chunk.gradients[paint[1] as usize]),
            PAINT_EXTERNAL => {
                let version = externals
                    .iter()
                    .find(|(id, _)| id.0 == paint[2])
                    .map_or(0, |&(_, v)| v);
                words.extend_from_slice(&[version as u32, (version >> 32) as u32]);
            }
            _ => {}
        }
        let g = group_of(c) as usize;
        if g != 0 {
            words.extend_from_slice(&chunk.groups[g][..15]);
            words.extend_from_slice(&chunk.groups[g][17..]);
        }
        let mut k = clip_of(c);
        while k != 0 {
            let r = &chunk.clips[k as usize];
            words.extend_from_slice(&r[..8]);
            words.extend_from_slice(&chunk.groups[r[8] as usize][..15]);
            k = r[9];
        }
        if kind == Some(Kind::Glyph) {
            let id = chunk.payload[at + 2] as usize;
            words.extend_from_slice(&glyphs.get(id).copied().unwrap_or_default());
        }
        hashes.push((hash_words(&words) as u32).max(1));
    }
    // Effects follow their members' content.
    for (i, c) in chunk.commands.iter().enumerate() {
        if Kind::from_word(c[0]) == Some(Kind::Effect) {
            let g = &chunk.groups[group_of(c) as usize];
            let members = &hashes[g[15] as usize..(g[15] + g[16]) as usize];
            let mut all = alloc::vec![hashes[i]];
            all.extend_from_slice(members);
            hashes[i] = (hash_words(&all) as u32).max(1);
        }
    }
    hashes
}

/// The hash of a whole frame: its commands' hashes, size and clear color.
pub fn frame_hash(hashes: &[u32], width: u32, height: u32, clear: u32) -> u64 {
    let mut words = Vec::with_capacity(hashes.len() + 3);
    words.extend_from_slice(&[width, height, clear]);
    words.extend_from_slice(hashes);
    hash_words(&words)
}
