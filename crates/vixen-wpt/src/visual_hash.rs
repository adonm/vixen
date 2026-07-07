//! Perceptual hashing of rendered screenshots (docs/SPEC.md `visual-hash`
//! check, docs/PLAN.md "Snapshot tests against Firefox reference").
//!
//! This module is renderer-agnostic: callers hand it an RGBA framebuffer and
//! it returns a stable 64-bit digest. The headless/GUI offscreen plumbing is
//! still responsible for producing pixels; once it does, the WPT harness can
//! compare `visual-hash` checks without changing the manifest format.
//!
//! Pipeline: RGBA framebuffer → alpha composite over white → 8×8 average-luma
//! hash → 64-bit row-major digest, compared by Hamming distance with a
//! manifest-provided tolerance. Reference renderings live in
//! `fixtures/reftest-baseline/`.

use std::str::FromStr;

const HASH_CELLS: u32 = 8;
const HASH_BITS: usize = (HASH_CELLS * HASH_CELLS) as usize;
pub const DEFAULT_TOLERANCE_BITS: u32 = 1;

/// A perceptual hash digest with a comparison tolerance.
///
/// Two screenshots hash to the same value when their Hamming distance is
/// within `tolerance` bits — matching the "1 % pixel diff" target from
/// docs/PLAN.md at the chosen digest width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualHash {
    pub bits: u64,
    pub tolerance: u32,
}

impl VisualHash {
    pub fn new(bits: u64, tolerance: u32) -> Self {
        Self { bits, tolerance }
    }

    /// Hamming distance between two digests.
    pub fn distance(self, other: VisualHash) -> u32 {
        (self.bits ^ other.bits).count_ones()
    }

    /// Within the configured tolerance?
    pub fn matches(self, other: VisualHash) -> bool {
        self.distance(other) <= self.tolerance
    }

    /// Stable manifest spelling: 16 lowercase hex digits plus tolerance.
    pub fn to_manifest_string(self) -> String {
        format!("{:016x}@{}", self.bits, self.tolerance)
    }
}

impl std::fmt::Display for VisualHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_manifest_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseVisualHashError {
    #[error("visual-hash expected 16 hex digits, got {0:?}")]
    InvalidBits(String),
    #[error("visual-hash tolerance must be an integer in 0..=64, got {0:?}")]
    InvalidTolerance(String),
}

impl FromStr for VisualHash {
    type Err = ParseVisualHashError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        let (bits_text, tolerance_text) = match trimmed.rsplit_once('@') {
            Some((bits, tolerance)) => (bits, Some(tolerance)),
            None => (trimmed, None),
        };
        let bits_text = bits_text
            .strip_prefix("0x")
            .or_else(|| bits_text.strip_prefix("0X"))
            .unwrap_or(bits_text);
        if bits_text.len() != 16 || !bits_text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ParseVisualHashError::InvalidBits(bits_text.to_owned()));
        }
        let bits = u64::from_str_radix(bits_text, 16)
            .map_err(|_| ParseVisualHashError::InvalidBits(bits_text.to_owned()))?;
        let tolerance = match tolerance_text {
            Some(text) => text
                .parse::<u32>()
                .ok()
                .filter(|value| *value <= 64)
                .ok_or_else(|| ParseVisualHashError::InvalidTolerance(text.to_owned()))?,
            None => DEFAULT_TOLERANCE_BITS,
        };
        Ok(Self { bits, tolerance })
    }
}

/// Hash an RGBA framebuffer. Returns `None` for invalid dimensions or buffers.
pub fn hash_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<VisualHash> {
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let expected_len = width.checked_mul(height)?.checked_mul(4)?;
    if rgba.len() != expected_len {
        return None;
    }

    let mut means = [0_u64; HASH_BITS];
    for cell_y in 0..HASH_CELLS {
        let (y0, y1) = cell_bounds(cell_y, height);
        for cell_x in 0..HASH_CELLS {
            let (x0, x1) = cell_bounds(cell_x, width);
            let mut total = 0_u64;
            let mut count = 0_u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * width + x) * 4;
                    total += luma_over_white(rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]);
                    count += 1;
                }
            }
            means[(cell_y * HASH_CELLS + cell_x) as usize] = total / count;
        }
    }

    let threshold = means.iter().sum::<u64>() / HASH_BITS as u64;
    let mut bits = 0_u64;
    for (i, mean) in means.into_iter().enumerate() {
        if mean > threshold {
            bits |= 1_u64 << (HASH_BITS - 1 - i);
        }
    }

    Some(VisualHash::new(bits, DEFAULT_TOLERANCE_BITS))
}

fn cell_bounds(cell: u32, extent: usize) -> (usize, usize) {
    let extent = extent as u64;
    let start = (u64::from(cell) * extent / u64::from(HASH_CELLS)) as usize;
    let mut end = (u64::from(cell + 1) * extent / u64::from(HASH_CELLS)) as usize;
    if end <= start {
        end = (start + 1).min(extent as usize);
    }
    (start, end)
}

fn luma_over_white(r: u8, g: u8, b: u8, a: u8) -> u64 {
    let a = u64::from(a);
    let r = composite_channel_over_white(r, a);
    let g = composite_channel_over_white(g, a);
    let b = composite_channel_over_white(b, a);
    // BT.601 luma with integer weights that sum to 256.
    (77 * r + 150 * g + 29 * b + 128) / 256
}

fn composite_channel_over_white(channel: u8, alpha: u64) -> u64 {
    (u64::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_rgba(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..width * height {
            out.extend_from_slice(&rgba);
        }
        out
    }

    #[test]
    fn rejects_invalid_buffers() {
        assert_eq!(hash_rgba(0, 8, &[]), None);
        assert_eq!(hash_rgba(8, 8, &[0, 0, 0, 255]), None);
    }

    #[test]
    fn uniform_images_hash_to_zero_bits() {
        let black = solid_rgba(8, 8, [0, 0, 0, 255]);
        let white = solid_rgba(8, 8, [255, 255, 255, 255]);
        assert_eq!(hash_rgba(8, 8, &black).unwrap().bits, 0);
        assert_eq!(hash_rgba(8, 8, &white).unwrap().bits, 0);
    }

    #[test]
    fn hashes_luma_in_row_major_order() {
        let mut rgba = Vec::new();
        for _y in 0..8 {
            for x in 0..8 {
                let value = if x < 4 { 0 } else { 255 };
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let hash = hash_rgba(8, 8, &rgba).unwrap();
        assert_eq!(hash.bits, 0x0f0f_0f0f_0f0f_0f0f);
        assert_eq!(hash.to_manifest_string(), "0f0f0f0f0f0f0f0f@1");
    }

    #[test]
    fn parses_manifest_hashes() {
        assert_eq!(
            "0x0F0f0f0f0f0f0f0f@4".parse::<VisualHash>().unwrap(),
            VisualHash::new(0x0f0f_0f0f_0f0f_0f0f, 4)
        );
        assert_eq!(
            "0f0f0f0f0f0f0f0f".parse::<VisualHash>().unwrap(),
            VisualHash::new(0x0f0f_0f0f_0f0f_0f0f, DEFAULT_TOLERANCE_BITS)
        );
        assert!("not-a-hash".parse::<VisualHash>().is_err());
        assert!("0f0f0f0f0f0f0f0f@65".parse::<VisualHash>().is_err());
    }
}
