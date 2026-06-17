//! Perceptual hashing of rendered screenshots (docs/SPEC.md `visual-hash`
//! check, docs/PLAN.md "Snapshot tests against Firefox reference").
//!
//! Stub at Phase 3: a real perceptual hash needs pixels, which need the
//! offscreen renderer (Phase 5, `vixen-headless::SurfacelessSurface` →
//! `glReadPixels`). The contract is captured here so the harness and the
//! `ref-equivalent`/`visual-hash` checks can target it.
//!
//! Planned pipeline (Phase 5+): RGBA framebuffer → downscale to 32×32
//! grayscale → DCT-like hash (pHash) → 64-bit digest, compared by Hamming
//! distance with a tolerance. Reference renderings live in
//! `fixtures/reftest-baseline/`.

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
    /// Hamming distance between two digests.
    pub fn distance(self, other: VisualHash) -> u32 {
        (self.bits ^ other.bits).count_ones()
    }

    /// Within the configured tolerance?
    pub fn matches(self, other: VisualHash) -> bool {
        self.distance(other) <= self.tolerance
    }
}

/// Not implemented until the offscreen renderer (Phase 5). Renderers produce
/// an RGBA buffer; this hashes it.
#[allow(dead_code)]
pub fn hash_rgba(_width: u32, _height: u32, _rgba: &[u8]) -> Option<VisualHash> {
    // TODO(Phase 5): downscale + phash once vixen-headless can read pixels.
    None
}
