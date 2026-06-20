//! CSS Compositing 1 § 5 + § 10 — Porter-Duff compositing + the 16 blend
//! modes (pure logic). The pixel-mixing primitive the paint path's
//! `mix-blend-mode`, `background-blend-mode`, and `isolation` group
//! composition reduce to. Complements [`crate::color`] (the sRGB arithmetic
//! and premultiplied-alpha helpers) and [`crate::stacking_context`] (the
//! group-formation predicate that decides *when* blending is isolated).
//!
//! What lives here:
//! - [`LinColor`] — a linear-sRGB colour with non-premultiplied alpha, the
//!   space Compositing 1 § 4 does all arithmetic in (and the space
//!   [`crate::color::interpolate`] already blends in).
//! - [`CompositingOperator`] — the 13 Porter-Duff operators (§ 5.1) with
//!   [`composite`] evaluating the § 5.1 general formula.
//! - [`BlendMode`] — the 16 § 10 modes (normal + 11 separable + 4
//!   non-separable) with [`blend_channel`] evaluating one B(C<sub>b</sub>,
//!   C<sub>s</sub>) term and [`blend`] applying the mode to a full pixel.
//! - [`composite_blend`] — the § 5.2 combined pipeline (isolation blend of
//!   the source against the backdrop, then the Porter-Duff operator); this
//!   is what `mix-blend-mode` actually runs.
//!
//! What does *not* live here:
//! - The display-list traversal that emits the group boundaries (paint path,
//!   Phase 5; `stacking_context::forms_stacking_context` decides the
//!   isolation groups).
//! - The WebRender `mix-blend-mode` binding (paint path feeds [`LinColor`]s
//!   in; WebRender's own blend is GL-level and agrees with this model).
//! - `plus-lighter` / `plus-darker` (SVG/CSS Compositing 1 § 9.2; deferred —
//!   the Porter-Duff set covers v1.0).
//!
//! ## Why linear sRGB
//!
//! Compositing 1 § 4.4 says the alpha + blend arithmetic happens on values
//! that have *already* been transferred to the working space. Browsers do
//! the arithmetic in linear-sRGB (Web Animations § 5.4 documents the same
//! choice for interpolation). [`crate::color::Color::to_linear_f32`] is the
//! decode; [`LinColor::from_color`] reuses it so the two modules agree.
//!
//! Reference: <https://www.w3.org/TR/compositing-1/>.

#![forbid(unsafe_code)]

use crate::color::Color;

// ---------------------------------------------------------------------------
// LinColor
// ---------------------------------------------------------------------------

/// A linear-sRGB colour with non-premultiplied alpha. All four channels in
/// `[0, 1]`; `r`/`g`/`b` may exceed `a` during intermediate blend math and
/// are clamped on output ([`LinColor::to_color`]). This is the working space
/// Compositing 1 § 4 + § 10 specifies all arithmetic in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinColor {
    /// Linear-sRGB red in `[0,1]`.
    pub r: f32,
    /// Linear-sRGB green in `[0,1]`.
    pub g: f32,
    /// Linear-sRGB blue in `[0,1]`.
    pub b: f32,
    /// Alpha in `[0,1]` (non-premultiplied).
    pub a: f32,
}

impl LinColor {
    /// The transparent linear colour (`a = 0`).
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// Decode an 8-bit sRGB [`Color`] into linear sRGB + alpha. Reuses
    /// [`color::Color::to_linear_f32`] so the decode agrees with the rest of
    /// the paint path.
    pub fn from_color(c: Color) -> Self {
        let [r, g, b, a] = c.to_linear_f32();
        Self { r, g, b, a }
    }

    /// Encode a linear-sRGB colour back to the 8-bit sRGB [`Color`] the
    /// display-list builder consumes. Out-of-gamut channels clamp to `[0,1]`
    /// before the inverse sRGB transfer.
    pub fn to_color(&self) -> Color {
        Color::rgba(
            encode_channel(self.r),
            encode_channel(self.g),
            encode_channel(self.b),
            quantise_u8(self.a),
        )
    }

    /// Per-channel `min` (used by the `darken` mode and § 10.2 saturation
    /// helpers).
    pub fn min_channel(&self) -> f32 {
        self.r.min(self.g).min(self.b)
    }

    /// Per-channel `max` (used by the `lighten` mode and § 10.2 saturation).
    pub fn max_channel(&self) -> f32 {
        self.r.max(self.g).max(self.b)
    }
}

/// Encode a single linear-sRGB `[0,1]` channel back to 8-bit sRGB using the
/// inverse sRGB transfer (CSS Color 4 § 11), clamping out-of-gamut first.
fn encode_channel(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let v = if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    quantise_u8(v)
}

/// Quantise an `[0,1]` linear-space float to a `u8` (round + clamp).
fn quantise_u8(v: f32) -> u8 {
    let v = (v * 255.0).round();
    if v <= 0.0 {
        0
    } else if v >= 255.0 {
        255
    } else {
        v as u8
    }
}

// ---------------------------------------------------------------------------
// Porter-Duff compositing operators (Compositing 1 § 5.1)
// ---------------------------------------------------------------------------

/// The 13 Porter-Duff compositing operators (Compositing 1 § 5.1, Table 1).
/// Each selects the `Fa` / `Fb` factors for the § 5.1 general compositing
/// formula. CSS exposes these as the `1 <composite-mode>` family of
/// `isolation`-aware group operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CompositingOperator {
    /// `clear` — the backdrop is removed; nothing is drawn (Table: Fa=0,
    /// Fb=0).
    Clear,
    /// `copy` — the source replaces the backdrop (Fa=1, Fb=0).
    Copy,
    /// `destination` — the backdrop is kept; the source is discarded
    /// (Fa=0, Fb=1).
    Destination,
    /// `source-over` — the source drawn over the backdrop. The default for
    /// every element that is not a `mix-blend-mode` candidate (Fa=1,
    /// Fb=1−αs).
    #[default]
    SourceOver,
    /// `destination-over` — the backdrop drawn over the source (Fa=1−αb,
    /// Fb=1).
    DestinationOver,
    /// `source-in` — the source, masked by the backdrop's alpha (Fa=αb,
    /// Fb=0).
    SourceIn,
    /// `destination-in` — the backdrop, masked by the source's alpha (Fa=0,
    /// Fb=αs).
    DestinationIn,
    /// `source-out` — the source, masked by the inverse of the backdrop's
    /// alpha (Fa=1−αb, Fb=0).
    SourceOut,
    /// `destination-out` — the backdrop, masked by the inverse of the
    /// source's alpha (Fa=0, Fb=1−αs).
    DestinationOut,
    /// `source-atop` — the source inside the backdrop, plus the backdrop
    /// wherever the source is transparent (Fa=αb, Fb=1−αs).
    SourceAtop,
    /// `destination-atop` — the backdrop inside the source, plus the source
    /// wherever the backdrop is transparent (Fa=1−αb, Fb=αs).
    DestinationAtop,
    /// `xor` — source and backdrop, each masked by the inverse of the
    /// other's alpha (Fa=1−αb, Fb=1−αs).
    Xor,
}

impl CompositingOperator {
    /// The (Fa, Fb) factors for the § 5.1 general formula given the source
    /// and backdrop alphas. Compositing 1 § 5.1 Table 1.
    fn factors(self, alpha_s: f32, alpha_b: f32) -> (f32, f32) {
        match self {
            CompositingOperator::Clear => (0.0, 0.0),
            CompositingOperator::Copy => (1.0, 0.0),
            CompositingOperator::Destination => (0.0, 1.0),
            CompositingOperator::SourceOver => (1.0, 1.0 - alpha_s),
            CompositingOperator::DestinationOver => (1.0 - alpha_b, 1.0),
            CompositingOperator::SourceIn => (alpha_b, 0.0),
            CompositingOperator::DestinationIn => (0.0, alpha_s),
            CompositingOperator::SourceOut => (1.0 - alpha_b, 0.0),
            CompositingOperator::DestinationOut => (0.0, 1.0 - alpha_s),
            CompositingOperator::SourceAtop => (alpha_b, 1.0 - alpha_s),
            CompositingOperator::DestinationAtop => (1.0 - alpha_b, alpha_s),
            CompositingOperator::Xor => (1.0 - alpha_b, 1.0 - alpha_s),
        }
    }
}

/// Evaluate the Compositing 1 § 5.1 general compositing formula over linear
/// non-premultiplied colours: `αo = αs·Fa + αb·Fb`, `Co = (αs·Fa·Cs + αb·Fb·Cb)
/// / αo` (and `0` where `αo = 0`). The source (`Cs`) is drawn onto the
/// backdrop (`Cb`) with the chosen [`CompositingOperator`].
///
/// Operates in linear-sRGB + non-premultiplied alpha per § 4.4; the output is
/// a linear [`LinColor`] ready for sRGB re-encode via [`LinColor::to_color`].
///
/// ```
/// # use vixen_engine::blend::{composite, CompositingOperator, LinColor};
/// # use vixen_engine::color::Color;
/// let backdrop = LinColor::from_color(Color::rgb(0, 0, 0));
/// let source = LinColor::from_color(Color::rgb(255, 255, 255));
/// let out = composite(backdrop, source, CompositingOperator::SourceOver);
/// assert_eq!(out.to_color(), Color::WHITE);
/// ```
pub fn composite(backdrop: LinColor, source: LinColor, op: CompositingOperator) -> LinColor {
    let (fa, fb) = op.factors(source.a, backdrop.a);
    let alpha_o = source.a * fa + backdrop.a * fb;
    if alpha_o <= 0.0 {
        return LinColor::TRANSPARENT;
    }
    // § 5.1: co = (αs·Fa·Cs + αb·Fb·Cb) / αo  (per-channel).
    let scale = 1.0 / alpha_o;
    LinColor {
        r: (source.a * fa * source.r + backdrop.a * fb * backdrop.r) * scale,
        g: (source.a * fa * source.g + backdrop.a * fb * backdrop.g) * scale,
        b: (source.a * fa * source.b + backdrop.a * fb * backdrop.b) * scale,
        a: alpha_o,
    }
}

// ---------------------------------------------------------------------------
// Blend modes (Compositing 1 § 10)
// ---------------------------------------------------------------------------

/// The 16 Compositing 1 § 10 blend modes: `normal` + 11 separable (§ 10.1) +
/// 4 non-separable (§ 10.2). CSS exposes these as `mix-blend-mode` and
/// `background-blend-mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    /// § 10.1 — the source replaces the backdrop colour (B = Cs; the blend
    /// step is a no-op, leaving only the Porter-Duff operator).
    #[default]
    Normal,
    /// § 10.1 — `B = Cb·Cs`. Darkens; white backdrop is inert.
    Multiply,
    /// § 10.1 — `B = Cb + Cs − Cb·Cs`. Brightens; black backdrop is inert.
    Screen,
    /// § 10.1 — `overlay = hardLight(Cs, Cb)` (hard-light with the operands
    /// swapped).
    Overlay,
    /// § 10.1 — `B = min(Cb, Cs)`.
    Darken,
    /// § 10.1 — `B = max(Cb, Cs)`.
    Lighten,
    /// § 10.1 — `B = 0` if `Cb=0`, `1` if `Cs=1`, else `min(1, Cb/(1−Cs))`.
    ColorDodge,
    /// § 10.1 — `B = 1` if `Cb=1`, `0` if `Cs=0`, else `1 − min(1,
    /// (1−Cb)/Cs)`.
    ColorBurn,
    /// § 10.1 — `multiply(Cb, 2·Cs)` if `Cs ≤ 0.5`, else `screen(Cb,
    /// 2·Cs−1)`.
    HardLight,
    /// § 10.1 — the `soft-light` smudge (with the § 10.1 `D(x)` piecewise
    /// helper).
    SoftLight,
    /// § 10.1 — `B = |Cb − Cs|`.
    Difference,
    /// § 10.1 — `B = Cb + Cs − 2·Cb·Cs`.
    Exclusion,
    /// § 10.2 — `SetLum(SetSat(Cs, Sat(Cb)), Lum(Cb))`.
    Hue,
    /// § 10.2 — `SetLum(SetSat(Cb, Sat(Cs)), Lum(Cb))`.
    Saturation,
    /// § 10.2 — `SetLum(Cs, Lum(Cb))`.
    Color,
    /// § 10.2 — `SetLum(Cb, Lum(Cs))`.
    Luminosity,
}

impl BlendMode {
    /// Parse a CSS `mix-blend-mode` / `background-blend-mode` keyword
    /// (case-insensitive). Returns `None` for unknown keywords so the host
    /// hook fails closed.
    pub fn parse(keyword: &str) -> Option<Self> {
        match keyword.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(BlendMode::Normal),
            "multiply" => Some(BlendMode::Multiply),
            "screen" => Some(BlendMode::Screen),
            "overlay" => Some(BlendMode::Overlay),
            "darken" => Some(BlendMode::Darken),
            "lighten" => Some(BlendMode::Lighten),
            "color-dodge" => Some(BlendMode::ColorDodge),
            "color-burn" => Some(BlendMode::ColorBurn),
            "hard-light" => Some(BlendMode::HardLight),
            "soft-light" => Some(BlendMode::SoftLight),
            "difference" => Some(BlendMode::Difference),
            "exclusion" => Some(BlendMode::Exclusion),
            "hue" => Some(BlendMode::Hue),
            "saturation" => Some(BlendMode::Saturation),
            "color" => Some(BlendMode::Color),
            "luminosity" => Some(BlendMode::Luminosity),
            _ => None,
        }
    }

    /// The CSS keyword for this mode (canonical lowercase form).
    pub fn keyword(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::ColorDodge => "color-dodge",
            BlendMode::ColorBurn => "color-burn",
            BlendMode::HardLight => "hard-light",
            BlendMode::SoftLight => "soft-light",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
            BlendMode::Hue => "hue",
            BlendMode::Saturation => "saturation",
            BlendMode::Color => "color",
            BlendMode::Luminosity => "luminosity",
        }
    }
}

/// Evaluate one separable blend term `B(Cb, Cs)` per Compositing 1 § 10.1
/// (the per-channel primitive the separable modes reduce to). For
/// non-separable modes this is meaningless — use [`blend`] instead.
fn separable_b(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        // overlay = hardLight with operands swapped (§ 10.1).
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::ColorDodge => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                1.0_f32.min(cb / (1.0 - cs))
            }
        }
        BlendMode::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - 1.0_f32.min((1.0 - cb) / cs)
            }
        }
        BlendMode::SoftLight => soft_light(cb, cs),
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        // Non-separable modes are handled elsewhere; reaching here is a bug.
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            non_separable_b(
                mode,
                LinColor::from_channels(cb),
                LinColor::from_channels(cs),
            )
            .r
        }
    }
}

/// `hard-light(Cb, Cs)` per § 10.1: multiply if `Cs ≤ 0.5`, screen otherwise
/// (over the doubled Cs).
fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb * (2.0 * cs)
    } else {
        cb + (2.0 * cs - 1.0) - cb * (2.0 * cs - 1.0)
    }
}

/// `soft-light(Cb, Cs)` per § 10.1, with the `D(x)` piecewise helper. The
/// formula is the spec's documented PDF-1.7 smudge (the "Photoshop"
/// soft-light, which is what browsers implement).
fn soft_light(cb: f32, cs: f32) -> f32 {
    fn d(x: f32) -> f32 {
        if x <= 0.25 {
            ((16.0 * x - 12.0) * x + 4.0) * x
        } else {
            x.sqrt()
        }
    }
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        cb + (2.0 * cs - 1.0) * (d(cb) - cb)
    }
}

impl LinColor {
    /// Build a grey [`LinColor`] from one channel value (testing helper for
    /// the non-separable modes + per-channel sanity).
    fn from_channels(c: f32) -> Self {
        Self {
            r: c,
            g: c,
            b: c,
            a: 1.0,
        }
    }
}

// --- Non-separable helpers (Compositing 1 § 10.2) ------------------------

/// `Lum(C)` per § 10.2 — the luminance of a colour. The coefficients are the
/// spec's `0.3 / 0.59 / 0.11` (Rec. 601 luminance; Compositing 1 § 10.2).
fn lum(c: LinColor) -> f32 {
    0.3 * c.r + 0.59 * c.g + 0.11 * c.b
}

/// `Sat(C)` per § 10.2 — `max - min` of the three colour channels.
fn sat(c: LinColor) -> f32 {
    c.max_channel() - c.min_channel()
}

/// Clip a colour so its channels stay within `[0,1]` while preserving
/// luminance. § 10.2 `ClipColor`. `c`'s channels may exceed gamut coming out
/// of `set_lum`; this scales them back in.
fn clip_color(c: LinColor) -> LinColor {
    let l = lum(c);
    let n = c.min_channel();
    let mut out = c;
    if n < 0.0 {
        // Scale toward luminance: `C = L + (C - L) * L / (L - n)`.
        let f = l / (l - n);
        out.r = l + (c.r - l) * f;
        out.g = l + (c.g - l) * f;
        out.b = l + (c.b - l) * f;
    }
    let x = out.max_channel();
    if x > 1.0 {
        // Scale toward luminance: `C = L + (C - L) * (1 - L) / (x - L)`.
        let f = (1.0 - l) / (x - l);
        out.r = l + (out.r - l) * f;
        out.g = l + (out.g - l) * f;
        out.b = l + (out.b - l) * f;
    }
    out.r = out.r.clamp(0.0, 1.0);
    out.g = out.g.clamp(0.0, 1.0);
    out.b = out.b.clamp(0.0, 1.0);
    out
}

/// `SetLum(C, l)` per § 10.2 — shift the colour's luminance to `l`,
/// clipping to gamut.
fn set_lum(c: LinColor, l: f32) -> LinColor {
    let d = l - lum(c);
    clip_color(LinColor {
        r: c.r + d,
        g: c.g + d,
        b: c.b + d,
        a: c.a,
    })
}

/// `SetSat(C, s)` per § 10.2 — set the colour's saturation to `s` while
/// preserving the channel ordering. The three branches cover the six
/// possible channel orderings via the min/mid/max identification.
fn set_sat(c: LinColor, s: f32) -> LinColor {
    // Identify min/mid/max channels by *index* so we can rewrite them in
    // place. § 10.2 SetSat: "Set the saturation by scaling the channels so
    // that max-min = s, with min → 0 and max → s, mid scaled proportionally."
    let mut idx = [0usize, 1, 2];
    let chans = [c.r, c.g, c.b];
    // Sort indices by the channel value (stable enough for ties — the spec
    // does not distinguish equal channels).
    idx.sort_by(|&a, &b| {
        chans[a]
            .partial_cmp(&chans[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let [min_i, mid_i, max_i] = [idx[0], idx[1], idx[2]];
    let mut out = [c.r, c.g, c.b];
    let (cmin, cmid, cmax) = (chans[min_i], chans[mid_i], chans[max_i]);
    if cmax > cmin {
        // mid = s * (cmid - cmin) / (cmax - cmin).
        out[mid_i] = s * (cmid - cmin) / (cmax - cmin);
    } else {
        // Degenerate: all channels equal. Saturation is 0; the spec keeps the
        // colour as-is (s · 0 / 0 → 0).
        out[mid_i] = 0.0;
    }
    out[min_i] = 0.0;
    out[max_i] = s;
    LinColor {
        r: out[0],
        g: out[1],
        b: out[2],
        a: c.a,
    }
}

/// Evaluate the non-separable blend `B(Cb, Cs)` per § 10.2. Returns the
/// blended colour (alpha ignored — the caller applies the Porter-Duff step).
fn non_separable_b(mode: BlendMode, cb: LinColor, cs: LinColor) -> LinColor {
    match mode {
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Color => set_lum(cs, lum(cb)),
        BlendMode::Luminosity => set_lum(cb, lum(cs)),
        // Reaching here for a separable mode is a bug — separable_b handles it.
        _ => unreachable!("non_separable_b called on {mode:?}"),
    }
}

/// Apply a blend mode to a full pixel, returning `B(Cb, Cs)` per channel
/// (the colour result; alpha is left at `cs.a` so the caller's Porter-Duff
/// step is the alpha authority, per § 5.2). For separable modes each channel
/// is independent; for the non-separable modes the pixel is transformed as a
/// whole. `Normal` returns `Cs` unchanged.
pub fn blend(backdrop: LinColor, source: LinColor, mode: BlendMode) -> LinColor {
    let out_a = source.a;
    match mode {
        BlendMode::Normal => LinColor { a: out_a, ..source },
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            let mut out = non_separable_b(mode, backdrop, source);
            out.a = out_a;
            out
        }
        _ => LinColor {
            r: separable_b(mode, backdrop.r, source.r),
            g: separable_b(mode, backdrop.g, source.g),
            b: separable_b(mode, backdrop.b, source.b),
            a: out_a,
        },
    }
}

/// A single separable channel's blend result (exposed for tests + callers
/// that want to drive the channel primitive directly).
pub fn blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            // Non-separable modes don't have a per-channel primitive.
            non_separable_b(
                mode,
                LinColor::from_channels(cb),
                LinColor::from_channels(cs),
            )
            .r
        }
        _ => separable_b(mode, cb, cs),
    }
}

// ---------------------------------------------------------------------------
// Combined compositing + blending (Compositing 1 § 5.2)
// ---------------------------------------------------------------------------

/// The Compositing 1 § 5.2 combined pipeline: the source is blended against
/// the backdrop with `mode`, the blended colour is isolated through the
/// backdrop alpha (`Cs' = (1 − αb)·Cs + αb·B(Cb, Cs)`), and the result is
/// composited with `op` (usually [`CompositingOperator::SourceOver`]). This
/// is the operation `mix-blend-mode` actually performs.
///
/// For [`BlendMode::Normal`] this reduces to [`composite`] — the blend step
/// is the identity, so only the Porter-Duff operator applies.
///
/// ```
/// # use vixen_engine::blend::{composite_blend, BlendMode, CompositingOperator, LinColor};
/// # use vixen_engine::color::Color;
/// let backdrop = LinColor::from_color(Color::rgb(80, 80, 80));
/// let source = LinColor::from_color(Color::rgb(200, 200, 200));
/// let out = composite_blend(
///     backdrop,
///     source,
///     BlendMode::Multiply,
///     CompositingOperator::SourceOver,
/// );
/// // Multiply darkens; the output is between the two greys.
/// let c = out.to_color();
/// assert!(c.r > 30 && c.r < 150, "got {}", c.r);
/// ```
pub fn composite_blend(
    backdrop: LinColor,
    source: LinColor,
    mode: BlendMode,
    op: CompositingOperator,
) -> LinColor {
    let alpha_b = backdrop.a;
    // § 5.2 isolation: Cs' = (1 - αb)·Cs + αb·B(Cb, Cs).
    let blended = blend(backdrop, source, mode);
    let isolated = LinColor {
        r: (1.0 - alpha_b) * source.r + alpha_b * blended.r,
        g: (1.0 - alpha_b) * source.g + alpha_b * blended.g,
        b: (1.0 - alpha_b) * source.b + alpha_b * blended.b,
        a: source.a,
    };
    composite(backdrop, isolated, op)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn approx_color(c: LinColor, r: f32, g: f32, b: f32, a: f32, tol: f32) -> bool {
        approx(c.r, r, tol) && approx(c.g, g, tol) && approx(c.b, b, tol) && approx(c.a, a, tol)
    }

    const TOL: f32 = 0.02;

    // --- LinColor round-trip -------------------------------------------

    #[test]
    fn lin_color_round_trip_opaque() {
        for c in [
            Color::BLACK,
            Color::WHITE,
            Color::rgb(255, 0, 0),
            Color::rgb(123, 45, 67),
        ] {
            let rt = LinColor::from_color(c).to_color();
            assert!(
                (rt.r as i16 - c.r as i16).unsigned_abs() <= 1
                    && (rt.g as i16 - c.g as i16).unsigned_abs() <= 1
                    && (rt.b as i16 - c.b as i16).unsigned_abs() <= 1
                    && rt.a == c.a,
                "{c:?} -> {rt:?}"
            );
        }
    }

    #[test]
    fn transparent_is_zero() {
        assert_eq!(
            LinColor::TRANSPARENT,
            LinColor::from_color(Color::TRANSPARENT)
        );
    }

    // --- Porter-Duff operators -----------------------------------------

    #[test]
    fn clear_returns_transparent() {
        let b = LinColor::from_color(Color::WHITE);
        let s = LinColor::from_color(Color::BLACK);
        assert_eq!(
            composite(b, s, CompositingOperator::Clear),
            LinColor::TRANSPARENT
        );
    }

    #[test]
    fn copy_keeps_source_drops_backdrop() {
        let b = LinColor::from_color(Color::WHITE);
        let s = LinColor::from_color(Color::rgb(10, 20, 30));
        let out = composite(b, s, CompositingOperator::Copy);
        assert_eq!(out, s);
    }

    #[test]
    fn destination_keeps_backdrop() {
        let b = LinColor::from_color(Color::rgb(10, 20, 30));
        let s = LinColor::from_color(Color::WHITE);
        let out = composite(b, s, CompositingOperator::Destination);
        assert_eq!(out, b);
    }

    #[test]
    fn source_over_opaque_source_replaces_backdrop() {
        let b = LinColor::from_color(Color::WHITE);
        let s = LinColor::from_color(Color::rgb(10, 20, 30));
        let out = composite(b, s, CompositingOperator::SourceOver);
        assert_eq!(out, s);
    }

    #[test]
    fn source_over_transparent_source_keeps_backdrop() {
        let b = LinColor::from_color(Color::rgb(10, 20, 30));
        let s = LinColor::TRANSPARENT;
        let out = composite(b, s, CompositingOperator::SourceOver);
        assert_eq!(out, b);
    }

    #[test]
    fn source_over_half_alpha_blends() {
        let b = LinColor::from_color(Color::rgb(0, 0, 0)); // black backdrop
        let mut s = LinColor::from_color(Color::rgb(255, 255, 255));
        s.a = 0.5;
        let out = composite(b, s, CompositingOperator::SourceOver);
        // White at 50% over black ≈ 0.5 linear ≈ 188 sRGB.
        assert!(approx(out.a, 1.0, TOL));
        let c = out.to_color();
        assert!((c.r as i16 - 188).unsigned_abs() <= 3, "got {}", c.r);
    }

    #[test]
    fn source_in_masks_by_backdrop_alpha() {
        let mut b = LinColor::from_color(Color::WHITE);
        b.a = 0.25;
        let s = LinColor::from_color(Color::rgb(255, 0, 0));
        let out = composite(b, s, CompositingOperator::SourceIn);
        // αo = αs·αb = 0.25; colour unchanged (red) but alpha is 0.25.
        assert!(approx(out.a, 0.25, TOL));
        assert!(approx(out.r, 1.0, TOL));
    }

    #[test]
    fn destination_in_masks_by_source_alpha() {
        let b = LinColor::from_color(Color::rgb(255, 0, 0));
        let mut s = LinColor::from_color(Color::WHITE);
        s.a = 0.5;
        let out = composite(b, s, CompositingOperator::DestinationIn);
        assert!(approx(out.a, 0.5, TOL));
        assert!(approx(out.r, 1.0, TOL));
    }

    #[test]
    fn xor_blends_two_half_opaques() {
        let mut a = LinColor::from_color(Color::rgb(255, 0, 0));
        a.a = 0.5;
        let mut b = LinColor::from_color(Color::rgb(0, 0, 255));
        b.a = 0.5;
        let out = composite(a, b, CompositingOperator::Xor);
        // αo = αb·(1-αs) + αs·(1-αb) = 0.5·0.5 + 0.5·0.5 = 0.5.
        assert!(approx(out.a, 0.5, TOL));
    }

    #[test]
    fn destination_over_draws_backdrop_on_top() {
        let mut b = LinColor::from_color(Color::rgb(255, 0, 0));
        b.a = 0.5;
        let s = LinColor::from_color(Color::rgb(0, 255, 0));
        let out = composite(b, s, CompositingOperator::DestinationOver);
        // Opaque source "under" 50% backdrop: output alpha is 1.0.
        assert!(approx(out.a, 1.0, TOL));
    }

    // --- Blend mode parse + keyword ------------------------------------

    #[test]
    fn blend_mode_keyword_round_trip() {
        for m in [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::HardLight,
            BlendMode::SoftLight,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ] {
            assert_eq!(BlendMode::parse(m.keyword()), Some(m));
        }
    }

    #[test]
    fn blend_mode_parse_case_insensitive() {
        assert_eq!(BlendMode::parse("MULTIPLY"), Some(BlendMode::Multiply));
        assert_eq!(
            BlendMode::parse("  Color-Dodge  "),
            Some(BlendMode::ColorDodge)
        );
    }

    #[test]
    fn blend_mode_parse_unknown_fails_closed() {
        assert_eq!(BlendMode::parse("burn"), None);
        assert_eq!(BlendMode::parse(""), None);
        assert_eq!(BlendMode::parse("vivid-light"), None);
    }

    // --- Separable blend maths (spec § 10.1) ---------------------------

    #[test]
    fn normal_returns_source() {
        let cb = 0.3;
        let cs = 0.7;
        assert!(approx(blend_channel(BlendMode::Normal, cb, cs), 0.7, TOL));
    }

    #[test]
    fn multiply_darkens() {
        assert!(approx(
            blend_channel(BlendMode::Multiply, 0.5, 0.5),
            0.25,
            TOL
        ));
        // White backdrop is inert.
        assert!(approx(
            blend_channel(BlendMode::Multiply, 1.0, 0.5),
            0.5,
            TOL
        ));
    }

    #[test]
    fn screen_brightens() {
        // screen(0.5, 0.5) = 0.5 + 0.5 - 0.25 = 0.75.
        assert!(approx(
            blend_channel(BlendMode::Screen, 0.5, 0.5),
            0.75,
            TOL
        ));
        // Black backdrop is inert.
        assert!(approx(blend_channel(BlendMode::Screen, 0.0, 0.5), 0.5, TOL));
    }

    #[test]
    fn darken_lighten_are_min_max() {
        assert!(approx(blend_channel(BlendMode::Darken, 0.3, 0.7), 0.3, TOL));
        assert!(approx(
            blend_channel(BlendMode::Lighten, 0.3, 0.7),
            0.7,
            TOL
        ));
    }

    #[test]
    fn color_dodge_brightens_to_white() {
        // Cs=1 → 1.
        assert!(approx(
            blend_channel(BlendMode::ColorDodge, 0.5, 1.0),
            1.0,
            TOL
        ));
        // Cb=0 → 0.
        assert!(approx(
            blend_channel(BlendMode::ColorDodge, 0.0, 0.5),
            0.0,
            TOL
        ));
        // 0.5 / (1 - 0.25) = 2/3.
        assert!(approx(
            blend_channel(BlendMode::ColorDodge, 0.5, 0.25),
            0.6667,
            TOL
        ));
    }

    #[test]
    fn color_burn_darkens_to_black() {
        // Cs=0 → 0.
        assert!(approx(
            blend_channel(BlendMode::ColorBurn, 0.5, 0.0),
            0.0,
            TOL
        ));
        // Cb=1 → 1.
        assert!(approx(
            blend_channel(BlendMode::ColorBurn, 1.0, 0.5),
            1.0,
            TOL
        ));
        // 1 - min(1, 0.5/0.5) = 0.
        assert!(approx(
            blend_channel(BlendMode::ColorBurn, 0.5, 0.5),
            0.0,
            TOL
        ));
    }

    #[test]
    fn difference_is_abs_difference() {
        assert!(approx(
            blend_channel(BlendMode::Difference, 0.3, 0.7),
            0.4,
            TOL
        ));
        assert!(approx(
            blend_channel(BlendMode::Difference, 0.7, 0.3),
            0.4,
            TOL
        ));
    }

    #[test]
    fn exclusion_midpoint() {
        // exclusion(0.5, 0.5) = 0.5.
        assert!(approx(
            blend_channel(BlendMode::Exclusion, 0.5, 0.5),
            0.5,
            TOL
        ));
        // exclusion(0, x) = x.
        assert!(approx(
            blend_channel(BlendMode::Exclusion, 0.0, 0.3),
            0.3,
            TOL
        ));
    }

    #[test]
    fn hard_light_branches() {
        // Cs ≤ 0.5 → multiply(Cb, 2·Cs) = 0.5 * 0.6 = 0.3.
        assert!(approx(
            blend_channel(BlendMode::HardLight, 0.5, 0.3),
            0.3,
            TOL
        ));
        // Cs > 0.5 → screen(Cb, 2·Cs-1) = 0.5 + 0.6 - 0.3 = 0.8.
        assert!(approx(
            blend_channel(BlendMode::HardLight, 0.5, 0.8),
            0.8,
            TOL
        ));
    }

    #[test]
    fn overlay_is_hard_light_swapped() {
        // overlay(Cb, Cs) == hardLight(Cs, Cb).
        let cb = 0.4;
        let cs = 0.7;
        let overlay = blend_channel(BlendMode::Overlay, cb, cs);
        let hardlight_swapped = hard_light(cs, cb);
        assert!(approx(overlay, hardlight_swapped, TOL));
    }

    #[test]
    fn soft_light_endpoint_is_backdrop() {
        // soft-light(Cb, Cs) = Cb - (1 - 2·Cs)·Cb·(1 - Cb) for Cs ≤ 0.5.
        // At Cs = 0 that collapses to Cb - Cb(1 - Cb) = Cb²  → darkens.
        assert!(approx(
            blend_channel(BlendMode::SoftLight, 0.5, 0.0),
            0.25,
            TOL
        ));
        // At Cs = 0.5 the (1 - 2·Cs) factor is zero, so B = Cb unchanged.
        assert!(approx(
            blend_channel(BlendMode::SoftLight, 0.5, 0.5),
            0.5,
            TOL
        ));
        // The Cs > 0.5 branch stays in gamut.
        let out = blend_channel(BlendMode::SoftLight, 0.5, 1.0);
        assert!((0.0..=1.0).contains(&out), "got {out}");
    }

    // --- Non-separable modes (§ 10.2) ----------------------------------

    #[test]
    fn lum_luminance_coefficients() {
        // White → 1.0, black → 0.0.
        assert!(approx(lum(LinColor::from_channels(1.0)), 1.0, TOL));
        assert!(approx(lum(LinColor::from_channels(0.0)), 0.0, TOL));
        // Pure red → 0.3.
        assert!(approx(
            lum(LinColor {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0
            }),
            0.3,
            TOL
        ));
    }

    #[test]
    fn set_lum_preserves_target_luminance() {
        let c = LinColor {
            r: 0.8,
            g: 0.2,
            b: 0.4,
            a: 1.0,
        };
        let out = set_lum(c, 0.5);
        assert!(approx(lum(out), 0.5, TOL));
        assert!(out.r >= 0.0 && out.r <= 1.0);
    }

    #[test]
    fn set_sat_zeroes_saturation() {
        let c = LinColor {
            r: 0.8,
            g: 0.2,
            b: 0.4,
            a: 1.0,
        };
        let out = set_sat(c, 0.0);
        // Saturation 0 → all channels equal.
        assert!(approx(sat(out), 0.0, TOL));
    }

    #[test]
    fn color_mode_takes_source_hue_backdrop_lum() {
        // B(Cb, Cs) = SetLum(Cs, Lum(Cb)).
        let cb = LinColor {
            r: 0.1,
            g: 0.1,
            b: 0.1,
            a: 1.0,
        };
        let cs = LinColor {
            r: 0.9,
            g: 0.1,
            b: 0.1,
            a: 1.0,
        };
        let out = blend(cb, cs, BlendMode::Color);
        // Lum(cb) = 0.1; SetLum(cs, 0.1) keeps the hue of cs at cb's luminance.
        assert!(approx(lum(out), lum(cb), TOL));
    }

    #[test]
    fn luminosity_mode_takes_source_lum() {
        let cb = LinColor {
            r: 0.2,
            g: 0.4,
            b: 0.6,
            a: 1.0,
        };
        let cs = LinColor {
            r: 0.8,
            g: 0.1,
            b: 0.1,
            a: 1.0,
        };
        let out = blend(cb, cs, BlendMode::Luminosity);
        // B = SetLum(Cb, Lum(Cs)).
        assert!(approx(lum(out), lum(cs), TOL));
    }

    #[test]
    fn hue_mode_combines_sat_and_lum() {
        // B = SetLum(SetSat(Cs, Sat(Cb)), Lum(Cb)).
        let cb = LinColor {
            r: 0.6,
            g: 0.3,
            b: 0.1,
            a: 1.0,
        };
        let cs = LinColor {
            r: 0.1,
            g: 0.8,
            b: 0.2,
            a: 1.0,
        };
        let out = blend(cb, cs, BlendMode::Hue);
        // Output luminance should match the backdrop's.
        assert!(approx(lum(out), lum(cb), 0.05));
    }

    // --- blend() full pixel --------------------------------------------

    #[test]
    fn blend_normal_is_source_colour() {
        let b = LinColor::from_color(Color::rgb(40, 40, 40));
        let s = LinColor::from_color(Color::rgb(200, 100, 50));
        let out = blend(b, s, BlendMode::Normal);
        assert!(approx_color(out, s.r, s.g, s.b, s.a, TOL));
    }

    #[test]
    fn blend_multiply_darkens_per_channel() {
        let b = LinColor::from_color(Color::rgb(128, 128, 128));
        let s = LinColor::from_color(Color::rgb(128, 128, 128));
        let out = blend(b, s, BlendMode::Multiply);
        // mid-grey × mid-grey in linear space: 128/255 ≈ 0.502 sRGB →
        // ≈ 0.216 linear → squared ≈ 0.0466 → re-encoded ≈ 61 sRGB.
        let c = out.to_color();
        assert!((c.r as i16 - 61).unsigned_abs() <= 3, "got {}", c.r);
    }

    #[test]
    fn blend_keeps_source_alpha() {
        let b = LinColor::from_color(Color::WHITE);
        let mut s = LinColor::from_color(Color::rgb(255, 0, 0));
        s.a = 0.5;
        let out = blend(b, s, BlendMode::Multiply);
        assert!(approx(out.a, 0.5, TOL));
    }

    // --- composite_blend (the full § 5.2 pipeline) ---------------------

    #[test]
    fn composite_blend_normal_equals_source_over() {
        let b = LinColor::from_color(Color::rgb(40, 80, 120));
        let s = LinColor::from_color(Color::rgb(200, 100, 50));
        let direct = composite(b, s, CompositingOperator::SourceOver);
        let via_blend = composite_blend(b, s, BlendMode::Normal, CompositingOperator::SourceOver);
        assert!(approx_color(
            via_blend, direct.r, direct.g, direct.b, direct.a, TOL
        ));
    }

    #[test]
    fn composite_blend_transparent_backdrop_is_pure_source() {
        // αb = 0 → isolation is a no-op → result is just Porter-Duff(source).
        let b = LinColor::TRANSPARENT;
        let s = LinColor::from_color(Color::rgb(200, 100, 50));
        let out = composite_blend(b, s, BlendMode::Multiply, CompositingOperator::SourceOver);
        // Source over transparent backdrop = source unchanged.
        assert!(approx_color(out, s.r, s.g, s.b, s.a, TOL));
    }

    #[test]
    fn composite_blend_multiply_darkens_opaque() {
        let b = LinColor::from_color(Color::rgb(180, 180, 180));
        let s = LinColor::from_color(Color::rgb(180, 180, 180));
        let out = composite_blend(b, s, BlendMode::Multiply, CompositingOperator::SourceOver);
        // Both opaque → isolation fully blends → darker than either input.
        let c = out.to_color();
        assert!(c.r < 180, "got {}", c.r);
        assert!(approx(out.a, 1.0, TOL));
    }

    #[test]
    fn composite_blend_difference_white_inverts() {
        let b = LinColor::from_color(Color::rgb(80, 80, 80));
        let s = LinColor::from_color(Color::WHITE);
        let out = composite_blend(b, s, BlendMode::Difference, CompositingOperator::SourceOver);
        // |Cb - 1| = 1 - Cb → inverted backdrop.
        let expected = 1.0 - b.r;
        assert!(approx(out.r, expected, TOL));
    }

    // --- edge cases -----------------------------------------------------

    #[test]
    fn composite_zero_alpha_yields_transparent() {
        let b = LinColor::TRANSPARENT;
        let s = LinColor::TRANSPARENT;
        assert_eq!(
            composite(b, s, CompositingOperator::SourceOver),
            LinColor::TRANSPARENT
        );
    }

    #[test]
    fn clip_color_gamut_clamps() {
        // An out-of-gamut colour (R > 1) is clipped back toward the luminance.
        let c = LinColor {
            r: 1.5,
            g: 0.5,
            b: 0.5,
            a: 1.0,
        };
        let out = clip_color(c);
        assert!(out.r <= 1.0 && out.r >= 0.0, "r {}", out.r);
        assert!(out.g <= 1.0 && out.g >= 0.0);
        assert!(out.b <= 1.0 && out.b >= 0.0);
    }

    #[test]
    fn default_compositing_operator_is_source_over() {
        assert_eq!(
            CompositingOperator::default(),
            CompositingOperator::SourceOver
        );
    }

    #[test]
    fn default_blend_mode_is_normal() {
        assert_eq!(BlendMode::default(), BlendMode::Normal);
    }
}
