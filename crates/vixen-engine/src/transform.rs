//! 2D CSS transform matrix algebra — Phase 5 paint prep (pure logic called
//! out by `docs/PLAN.md` "Testing strategy" as a Rust-unit-test surface).
//! Implements the 2D affine transforms CSS Transforms Level 1 § 13 defines
//! for the `transform` property (`translate()` / `scale()` / `rotate()` /
//! `skew()` / `matrix()`), so the paint path can transform the display-list
//! geometry without waiting for the full WebRender transform plumbing.
//!
//! What lives here:
//! - [`Transform2D`] — a 2×3 affine matrix `[a c e; b d f; 0 0 1]` (CSS
//!   Transforms 1 § 13.1), stored as six fields named to match the spec's
//!   `matrix(a,b,c,d,e,f)` argument order.
//! - Constructors for every 2D transform function ([`Transform2D::translate`],
//!   [`Transform2D::scale`], [`Transform2D::rotate`], [`Transform2D::skew`],
//!   [`Transform2D::matrix`]) and [`Transform2D::multiply`] for composition.
//! - [`Transform2D::apply_point`] / [`Transform2D::apply_rect`] — point/rect
//!   transformation; [`Transform2D::inverse`] + [`Transform2D::determinant`].
//!
//! What does *not* live here:
//! - 3D transforms (`rotateX`, `perspective`, matrix3d — post-v1.0 / WR).
//! - `transform-origin` resolution (caller: translate origin to (0,0),
//!   multiply, translate back — the same pre/post-multiply Stylo does).
//! - The full `transform` *property* parser (Stylo's job; this module is the
//!   arithmetic the cascade-resolved values reduce to). [`parse_transform`]
//!   handles the common 2D-function grammar as a host-hook + reftest helper.
//!
//! Composition order: `a.multiply(b)` produces the matrix that applies `a`
//! *after* `b` — i.e. for `a * b`, point `p` becomes `a * (b * p)`, matching
//! CSS Transforms 1 § 13.5.1 ("`transform: A B` applies A first"). This is the
//! convention Firefox/Servo use, so Vixen stays interoperable with the
//! upstream computed-value surface.
//!
//! Reference: <https://www.w3.org/TR/css-transforms-1/> (§ 13 matrix math),
//! § 16.2 "Interpolation of 2D matrices" (for the future `interpolate` slice).

#![forbid(unsafe_code)]

use crate::angle::Angle;

/// A 2D affine transform in the CSS Transforms 1 § 13.1 form:
///
/// ```text
/// | a c e |
/// | b d f |
/// | 0 0 1 |
/// ```
///
/// Field names match the `matrix(a,b,c,d,e,f)` argument order so the values
/// round-trip to/from the spec text verbatim.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Transform2D {
    /// The identity transform (no-op).
    pub const IDENTITY: Transform2D = Transform2D {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// `matrix(a,b,c,d,e,f)` per CSS Transforms 1 § 13.1.
    pub const fn matrix(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// `translate(tx, ty)` per CSS Transforms 1 § 13.2.
    pub const fn translate(tx: f32, ty: f32) -> Self {
        Self {
            e: tx,
            f: ty,
            ..Self::IDENTITY
        }
    }

    /// `scale(sx, sy)` per CSS Transforms 1 § 13.3. `scale(s)` is `scale(s,s)`.
    pub const fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    /// `rotate(angle)` per CSS Transforms 1 § 13.4. Positive angles rotate
    /// clockwise (CSS y-axis points down). Consumes [`Angle`] directly so the
    /// unit grammar (`deg`/`rad`/`grad`/`turn`) is shared with [`crate::angle`].
    pub fn rotate(angle: Angle) -> Self {
        let (c, s) = angle.cos_sin();
        let (c, s) = (c as f32, s as f32);
        Self {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: 0.0,
            f: 0.0,
        }
    }

    /// `skew(ax, ay)` per CSS Transforms 1 § 13.5.
    pub fn skew(ax: Angle, ay: Angle) -> Self {
        // tan(x) = sin/cos; fall back to 0 at the ±90° asymptote (CSS clamps
        // there rather than producing an infinite matrix).
        let tan = |a: Angle| {
            let (c, s) = a.cos_sin();
            if c.abs() < f64::EPSILON {
                0.0
            } else {
                (s / c) as f32
            }
        };
        Self {
            c: tan(ax),
            b: tan(ay),
            ..Self::IDENTITY
        }
    }

    /// `self * other` — the result applies `self` *after* `other`, matching
    /// CSS Transforms 1 § 13.5.1 (`transform: A B` ⇒ apply A first).
    pub fn multiply(self, other: Transform2D) -> Transform2D {
        Transform2D {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    /// Determinant of the linear part (`a*d - b*c`); 0 ⇒ singular.
    pub fn determinant(self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    /// `true` for the identity matrix.
    pub fn is_identity(self) -> bool {
        self == Self::IDENTITY
    }

    /// The inverse, or `None` if singular (determinant ≈ 0). Used to map
    /// pointer events from screen space back into element space.
    pub fn inverse(self) -> Option<Transform2D> {
        let det = self.determinant();
        if det.abs() < 1e-9 {
            return None;
        }
        let inv_det = 1.0 / det;
        Some(Transform2D {
            a: self.d * inv_det,
            b: -self.b * inv_det,
            c: -self.c * inv_det,
            d: self.a * inv_det,
            e: (self.c * self.f - self.d * self.e) * inv_det,
            f: (self.b * self.e - self.a * self.f) * inv_det,
        })
    }

    /// Apply the transform to a point.
    pub fn apply_point(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Apply the transform to all four corners of a rectangle and return the
    /// axis-aligned bounding box of the result (the form paint needs — a
    /// rotated rectangle's paint region is its AABB).
    pub fn apply_rect(self, r: crate::display_list::Rect) -> crate::display_list::Rect {
        let (x0, y0) = self.apply_point(r.x, r.y);
        let (x1, y1) = self.apply_point(r.x + r.w, r.y);
        let (x2, y2) = self.apply_point(r.x, r.y + r.h);
        let (x3, y3) = self.apply_point(r.x + r.w, r.y + r.h);
        let min_x = x0.min(x1).min(x2).min(x3);
        let max_x = x0.max(x1).max(x2).max(x3);
        let min_y = y0.min(y1).min(y2).min(y3);
        let max_y = y0.max(y1).max(y2).max(y3);
        crate::display_list::Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

// ---------------------------------------------------------------------------
// Small transform-list parser (reftest + host-hook helper)
// ---------------------------------------------------------------------------

/// Parse a simple 2D transform *list* (`translate(10px, 0) rotate(45deg)`).
///
/// Handles the common productions Stylo hands the cascade and that the
/// `--computed-style` projection re-derives; exotic forms (`matrix()`, full
/// unit lists) are best left to Stylo. Whitespace tolerant; comma-separated
/// args. Unknown functions fail closed with [`TransformParseError::UnknownFunction`].
///
/// Functions are applied left-to-right (first listed is applied first), per
/// CSS Transforms 1 § 13.5.1.
pub fn parse_transform(input: &str) -> Result<Transform2D, TransformParseError> {
    // CSS Transforms 1 § 13.5.1: start from identity, *post-multiply* each
    // function's matrix. The net result `T1·T2·…·Tn` applies Tn to the point
    // first (rightmost-first), matching Firefox/Servo.
    let mut acc = Transform2D::IDENTITY;
    for func in split_functions(input) {
        acc = acc.multiply(func.build_matrix()?);
    }
    Ok(acc)
}

struct FuncToken<'a> {
    name: &'a str,
    args: &'a str,
}

impl FuncToken<'_> {
    fn build_matrix(self) -> Result<Transform2D, TransformParseError> {
        match self.name {
            // translate: single arg ⇒ ty = 0.
            "translate" => {
                let (tx, ty) = one_or_two_args(self.args, 0.0)?;
                Ok(Transform2D::translate(tx, ty))
            }
            // scale: single arg ⇒ sy = sx (CSS Transforms 1 § 13.3).
            "scale" => {
                let parts = split_args(self.args);
                match parts.as_slice() {
                    [a] => Ok(Transform2D::scale(parse_len(a)?, parse_len(a)?)),
                    [a, b] => Ok(Transform2D::scale(parse_len(a)?, parse_len(b)?)),
                    _ => Err(TransformParseError::BadArity),
                }
            }
            "rotate" => {
                let a = parse_angle_arg(self.args)?;
                Ok(Transform2D::rotate(a))
            }
            "skew" => {
                let (ax, ay) = one_or_two_angle_args(self.args)?;
                Ok(Transform2D::skew(ax, ay))
            }
            other => Err(TransformParseError::UnknownFunction(other.to_owned())),
        }
    }
}

/// Split `translate(1px,2px) rotate(45deg)` into its function tokens. Tolerant
/// of inter-function whitespace. Returns tokens in document order.
fn split_functions(input: &str) -> Vec<FuncToken<'_>> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip leading whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Read name (ASCII letters).
        let name_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let name = &input[name_start..i];
        if name.is_empty() {
            // Stray punctuation; skip one byte to make progress.
            i += 1;
            continue;
        }
        // Expect '('.
        if i >= bytes.len() || bytes[i] != b'(' {
            break;
        }
        i += 1;
        // Read until matching ')'.
        let args_start = i;
        let mut depth = 1;
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                i += 1;
            }
        }
        let args = &input[args_start..i];
        if i < bytes.len() {
            i += 1; // consume ')'
        }
        out.push(FuncToken { name, args });
    }
    out
}

/// Split comma-separated args, trimmed. `split_args("10px, 20px")` ⇒ `["10px", "20px"]`.
fn split_args(args: &str) -> Vec<&str> {
    args.split(',').map(|p| p.trim()).collect()
}

/// Parse two length args; one arg ⇒ the second takes `default_second` (translate
/// uses `0.0`; scale special-cases `sy = sx` at the call site). Length args
/// carry a `px` suffix (stripped).
fn one_or_two_args(args: &str, default_second: f32) -> Result<(f32, f32), TransformParseError> {
    match split_args(args).as_slice() {
        [a] => Ok((parse_len(a)?, default_second)),
        [a, b] => Ok((parse_len(a)?, parse_len(b)?)),
        _ => Err(TransformParseError::BadArity),
    }
}

/// Like [`one_or_two_args`] but for angle-bearing functions (skew); the
/// second arg defaults to `0deg`.
fn one_or_two_angle_args(args: &str) -> Result<(Angle, Angle), TransformParseError> {
    match split_args(args).as_slice() {
        [a] => Ok((parse_angle_arg(a)?, Angle::deg(0.0))),
        [a, b] => Ok((parse_angle_arg(a)?, parse_angle_arg(b)?)),
        _ => Err(TransformParseError::BadArity),
    }
}

fn parse_angle_arg(args: &str) -> Result<Angle, TransformParseError> {
    let s = args.trim();
    if s.is_empty() {
        // rotate() with no arg ⇒ rotate(0) per the grammar.
        return Ok(Angle::deg(0.0));
    }
    Angle::parse(s).map_err(|_| TransformParseError::BadAngle(s.to_owned()))
}

fn parse_len(s: &str) -> Result<f32, TransformParseError> {
    let s = s.trim();
    let stripped = s.strip_suffix("px").unwrap_or(s).trim();
    stripped
        .parse::<f32>()
        .map_err(|_| TransformParseError::BadLength(s.to_owned()))
}

/// Parse error for [`parse_transform`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransformParseError {
    #[error("unknown transform function: {0}")]
    UnknownFunction(String),
    #[error("wrong number of arguments")]
    BadArity,
    #[error("invalid angle argument: {0}")]
    BadAngle(String),
    #[error("invalid length argument: {0}")]
    BadLength(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::angle::AngleUnit;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    // --- Constructors + identity ---------------------------------------

    #[test]
    fn identity_is_a_noop_on_point() {
        let (x, y) = Transform2D::IDENTITY.apply_point(5.0, -3.0);
        assert!(approx(x, 5.0) && approx(y, -3.0));
        assert!(Transform2D::IDENTITY.is_identity());
    }

    #[test]
    fn translate_moves_a_point() {
        let (x, y) = Transform2D::translate(10.0, 20.0).apply_point(0.0, 0.0);
        assert!(approx(x, 10.0) && approx(y, 20.0));
    }

    #[test]
    fn scale_multiplies_a_point() {
        let (x, y) = Transform2D::scale(2.0, 3.0).apply_point(5.0, 4.0);
        assert!(approx(x, 10.0) && approx(y, 12.0));
    }

    #[test]
    fn rotate_90_maps_axes() {
        // rotate(90deg) maps (1,0) → (0,1) (clockwise, y-down).
        let (x, y) = Transform2D::rotate(Angle::deg(90.0)).apply_point(1.0, 0.0);
        assert!(approx(x, 0.0), "x={x}");
        assert!(approx(y, 1.0), "y={y}");
    }

    #[test]
    fn rotate_accepts_any_angle_unit() {
        // 100grad == 90deg, 0.25turn == 90deg.
        for a in [
            Angle::new(100.0, AngleUnit::Grad),
            Angle::new(0.25, AngleUnit::Turn),
        ] {
            let (x, _y) = Transform2D::rotate(a).apply_point(1.0, 0.0);
            assert!(approx(x, 0.0), "x for {a:?}");
        }
    }

    // --- Composition ---------------------------------------------------

    #[test]
    fn multiply_composes_left_first() {
        // transform: translate(10,0) rotate(45deg) ⇒ first rotate, then translate.
        // Per CSS, the leftmost is applied first to the point. So a unit x vector
        // is rotated then shifted: (10 + cos45, sin45).
        let t = parse_transform("translate(10px, 0) rotate(45deg)").unwrap();
        let (x, y) = t.apply_point(1.0, 0.0);
        let s2 = std::f32::consts::FRAC_1_SQRT_2;
        assert!(approx(x, 10.0 + s2), "x={x}");
        assert!(approx(y, s2), "y={y}");
    }

    #[test]
    fn multiply_identity_is_a_noop() {
        let t = Transform2D::scale(2.0, 2.0);
        assert_eq!(t.multiply(Transform2D::IDENTITY), t);
        assert_eq!(Transform2D::IDENTITY.multiply(t), t);
    }

    #[test]
    fn multiply_associates_and_composes() {
        // (S * R) * T == S * (R * T).
        let s = Transform2D::scale(2.0, 2.0);
        let r = Transform2D::rotate(Angle::deg(30.0));
        let t = Transform2D::translate(3.0, 4.0);
        let lhs = s.multiply(r).multiply(t);
        let rhs = s.multiply(r.multiply(t));
        assert_eq!(lhs, rhs);
    }

    // --- Determinant + inverse ----------------------------------------

    #[test]
    fn determinant_of_scale_is_area_scale() {
        // scale(2,3) doubles area by 6.
        assert!(approx(Transform2D::scale(2.0, 3.0).determinant(), 6.0));
        // rotation preserves area (det = 1).
        assert!(approx(
            Transform2D::rotate(Angle::deg(37.0)).determinant(),
            1.0
        ));
    }

    #[test]
    fn inverse_round_trips() {
        let m = Transform2D::translate(10.0, 20.0).multiply(Transform2D::scale(2.0, 3.0));
        let inv = m.inverse().expect("non-singular");
        let id = m.multiply(inv);
        assert!(id.is_identity(), "{:?}", id);
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        // scale(0,1) collapses the x axis → det = 0.
        assert!(Transform2D::scale(0.0, 1.0).inverse().is_none());
    }

    // --- Rect ----------------------------------------------------------

    #[test]
    fn apply_rect_returns_aabb() {
        // rotate(90) of a 10x20 box at origin → 20x10 box.
        let r = crate::display_list::Rect::new(0.0, 0.0, 10.0, 20.0);
        let out = Transform2D::rotate(Angle::deg(90.0)).apply_rect(r);
        assert!(approx(out.w, 20.0), "w={}", out.w);
        assert!(approx(out.h, 10.0), "h={}", out.h);
    }

    // --- Parser --------------------------------------------------------

    #[test]
    fn parse_translate_one_arg_defaults_y() {
        let t = parse_transform("translate(10px)").unwrap();
        let (x, y) = t.apply_point(0.0, 0.0);
        assert!(approx(x, 10.0));
        assert!(approx(y, 0.0));
    }

    #[test]
    fn parse_scale_one_arg_is_uniform() {
        let t = parse_transform("scale(2)").unwrap();
        let (x, y) = t.apply_point(3.0, 4.0);
        assert!(approx(x, 6.0));
        assert!(approx(y, 8.0));
    }

    #[test]
    fn parse_list_with_whitespace() {
        // List `rotate scale` ⇒ net `rotate · scale`, so scale applies first
        // (rightmost-first). p=(1,0): scale(2,2) → (2,0); rotate(90deg)
        // (clockwise, y-down) → (0,2).
        let t = parse_transform("  rotate( 90deg )   scale( 2 , 2 )  ").unwrap();
        let (x, y) = t.apply_point(1.0, 0.0);
        assert!(approx(x, 0.0), "x={x}");
        assert!(approx(y, 2.0), "y={y}");
    }

    #[test]
    fn parse_skew_two_angle_args() {
        let t = parse_transform("skew(0deg, 0deg)").unwrap();
        assert!(t.is_identity());
    }

    #[test]
    fn parse_errors() {
        assert!(matches!(
            parse_transform("frobnicate(1px)"),
            Err(TransformParseError::UnknownFunction(_))
        ));
        assert!(parse_transform("translate(a, b)").is_err());
        assert!(parse_transform("rotate(45garbage)").is_err());
    }
}
