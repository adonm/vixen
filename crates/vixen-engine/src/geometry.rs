//! CSS Geometry Interfaces Level 1 — Phase 5/6 host-bindings prep (pure logic
//! called out by `docs/PLAN.md` "Testing strategy" as a Rust-unit-test
//! surface). Implements the `DOMPoint` / `DOMRect` / `DOMQuad` / `DOMMatrix`
//! value family the geometry-bearing host hooks reduce to:
//! `Element.getClientRects()` / `getBoundingClientRect()`, `MouseEvent.clientX`,
//! `IntersectionObserver` thresholds, `DOMQuad`, the CSS Transform 2 4×4
//! matrix surface, and the Web Animations § 5.7 transform-interpolation
//! decompose/compose pipeline.
//!
//! What lives here:
//! - [`DOMPoint`] / [`DOMPointInit`] — a 2D/3D/homogeneous point `(x, y, z, w)`
//!   per § 2. `w ≠ 1` carries the homogeneous-weight form the perspective divide
//!   normalises away.
//! - [`DOMRect`] — a `(x, y, width, height)` rectangle (§ 3) with the derived
//!   `top`/`right`/`bottom`/`left` accessors + the `contains` / `intersects`
//!   predicates (`getBoundingClientRect()` returns one; `IntersectionObserver`
//!   compares two).
//! - [`DOMQuad`] — four [`DOMPoint`]s describing a (possibly rotated, possibly
//!   non-axis-aligned) quadrilateral (§ 4); [`DOMQuad::bounds`] is the § 4.4
//!   axis-aligned bounding rectangle, [`DOMQuad::from_rect`] the `fromRect()`
//!   constructor.
//! - [`DOMMatrix`] — the § 6 4×4 homogeneous matrix (the 2D `matrix(a,b,c,d,e,f)`
//!   subset folds into the upper-left 2×3 + the `[0 0 1 0]`/`[0 0 0 1]` rows).
//!   Constructors for every § 6.3 transform (`translate` / `scale` /
//!   `scale_non_uniform` / `rotate` / `rotate_axis_angle` / `skew_x` / `skew_y`
//!   / `multiply` / `flip_x` / `flip_y` / `inverse`), `transform_point` (the
//!   § 6.4 homogeneous-coordinate projection), and the `is_2d` /
//!   `to_4x4` / `to_2d_array` accessors the WebIDL reflects.
//!
//! What does *not* live here:
//! - Interpolation / decomposition of 2D + 3D matrices into translate /
//!   rotate / scale / skew / perspective tuples (CSS Transforms 2 § 16 +
//!   theGraphics Gems `RecoverMatrix` recipe). That lives in the animation
//!   interpolation layer once Web Animations § 5.7 lands; this module is the
//!   arithmetic the decompose/compose pipeline reduces to.
//! - The CSS `transform` property string parser ([`crate::transform::parse_transform`]
//!   owns the 2D subset; the 3D surface lands when Stylo + WebRender add the
//!   perspective/rotateX plumbing).
//! - The host-hook JS-visible `DOMMatrixReadOnly` vs `DOMMatrix` (mutable)
//!   distinction. Rust-side the matrix is owned + mutable; the read-only view
//!   is the `&self` method set, mirroring `Vec` / `&[T]`.
//!
//! ## Composition order
//!
//! `DOMMatrix::multiply(self, rhs)` returns the matrix equivalent to "first
//! apply `rhs`, then `self`" — i.e. for `lhs.multiply(rhs)`, a point `p`
//! becomes `lhs * (rhs * p)`. This matches CSS Transforms 1 § 13.5.1
//! ("`transform: A B` applies A first") and the Geometry Interfaces § 6.3
//! `multiply(other)` wording ("the matrix `this * other`").
//!
//! Reference:
//! - Geometry Interfaces: <https://www.w3.org/TR/geometry-1/>.
//! - CSS Transforms 2 matrix math: <https://www.w3.org/TR/css-transforms-2/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// DOMPoint (§ 2)
// ---------------------------------------------------------------------------

/// A 2D / 3D / homogeneous point (Geometry Interfaces § 2). The `w` field
/// carries the homogeneous-weight form; the perspective divide normalises it
/// to `1` when projecting a transformed point ([`DOMMatrix::transform_point`]).
///
/// `DOMPointInit` in the WebIDL accepts `{x, y, z, w}` with all fields
/// optional (default `0` for `x`/`y`/`z`, `1` for `w`); see
/// [`DOMPoint::from_init`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DOMPoint {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

impl DOMPoint {
    /// `(x, y, 0, 1)` — the canonical 2D point in homogeneous coordinates.
    pub const fn new_2d(x: f64, y: f64) -> Self {
        Self {
            x,
            y,
            z: 0.0,
            w: 1.0,
        }
    }

    /// `(x, y, z, 1)` — the canonical 3D point in homogeneous coordinates.
    pub const fn new_3d(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z, w: 1.0 }
    }

    /// The full `(x, y, z, w)` form (the homogeneous-weight constructor the
    /// `new DOMPoint(x, y, z, w)` WebIDL signature maps to).
    pub const fn new(x: f64, y: f64, z: f64, w: f64) -> Self {
        Self { x, y, z, w }
    }

    /// Materialise a `DOMPointInit`-shaped record, applying the § 2 defaults
    /// (`x`/`y`/`z` default `0`, `w` defaults `1`).
    pub fn from_init(init: DOMPointInit) -> Self {
        Self {
            x: init.x,
            y: init.y,
            z: init.z,
            w: init.w,
        }
    }
}

/// The `DOMPointInit` dictionary (Geometry Interfaces § 2.2). All fields
/// optional in the WebIDL; the defaults match [`DOMPoint::from_init`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DOMPointInit {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

impl DOMPointInit {
    /// `DOMPointInit` with every field at its WebIDL default (`0, 0, 0, 1`).
    pub const DEFAULT: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 1.0,
    };
}

impl Default for DOMPoint {
    fn default() -> Self {
        DOMPointInit::DEFAULT.into()
    }
}

impl From<DOMPointInit> for DOMPoint {
    fn from(init: DOMPointInit) -> Self {
        DOMPoint::from_init(init)
    }
}

// ---------------------------------------------------------------------------
// DOMRect (§ 3)
// ---------------------------------------------------------------------------

/// An axis-aligned rectangle (Geometry Interfaces § 3): `(x, y, width,
/// height)` with the derived `top`/`right`/`bottom`/`left` accessors.
///
/// The fields are stored verbatim; `width` / `height` may be negative (the
/// spec's `DOMRectReadOnly` admits them), in which case the derived edges
/// cross. [`DOMRect::normalized`] flips them to the canonical positive form
/// `getBoundingClientRect()` and `IntersectionObserver` already expect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DOMRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Default for DOMRect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }
}

impl DOMRect {
    /// The minimum x of the rectangle (§ 3, derived accessor `left`).
    /// Equal to `x` if `width ≥ 0`, else `x + width`.
    pub fn left(&self) -> f64 {
        if self.width >= 0.0 {
            self.x
        } else {
            self.x + self.width
        }
    }

    /// The maximum x of the rectangle (§ 3, derived accessor `right`).
    pub fn right(&self) -> f64 {
        if self.width >= 0.0 {
            self.x + self.width
        } else {
            self.x
        }
    }

    /// The minimum y of the rectangle (§ 3, derived accessor `top`).
    pub fn top(&self) -> f64 {
        if self.height >= 0.0 {
            self.y
        } else {
            self.y + self.height
        }
    }

    /// The maximum y of the rectangle (§ 3, derived accessor `bottom`).
    pub fn bottom(&self) -> f64 {
        if self.height >= 0.0 {
            self.y + self.height
        } else {
            self.y
        }
    }

    /// A canonical form with non-negative `width` / `height` (so `left ≤
    /// right` and `top ≤ bottom`). `getBoundingClientRect()` and
    /// `IntersectionObserver` consume this; the negative-`width`/`height`
    /// form is the spec's storage, not the layout caller's expectation.
    pub fn normalized(&self) -> Self {
        let (x, width) = if self.width < 0.0 {
            (self.x + self.width, -self.width)
        } else {
            (self.x, self.width)
        };
        let (y, height) = if self.height < 0.0 {
            (self.y + self.height, -self.height)
        } else {
            (self.y, self.height)
        };
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether `point` lies inside this rectangle (half-open: a point on
    /// `right`/`bottom` is *not* inside, matching `[left, right)` geometry).
    /// Negative-`width`/`height` inputs are normalised first.
    pub fn contains_point(&self, point: DOMPoint) -> bool {
        let n = self.normalized();
        point.x >= n.left() && point.x < n.right() && point.y >= n.top() && point.y < n.bottom()
    }

    /// Whether `other` overlaps this rectangle (the predicate
    /// `IntersectionObserver` evaluates between the target's `getBoundingClientRect()`
    /// and the root container). Touching edges do not count as overlap.
    pub fn intersects(&self, other: &DOMRect) -> bool {
        let a = self.normalized();
        let b = other.normalized();
        a.left() < b.right() && a.right() > b.left() && a.top() < b.bottom() && a.bottom() > b.top()
    }

    /// The smallest rectangle containing both `self` and `other`. Useful for
    /// aggregating `getClientRects()` line boxes into one `getBoundingClientRect()`.
    pub fn union(&self, other: &DOMRect) -> DOMRect {
        let a = self.normalized();
        let b = other.normalized();
        let left = a.left().min(b.left());
        let top = a.top().min(b.top());
        let right = a.right().max(b.right());
        let bottom = a.bottom().max(b.bottom());
        DOMRect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }
}

// ---------------------------------------------------------------------------
// DOMQuad (§ 4)
// ---------------------------------------------------------------------------

/// A quadrilateral (Geometry Interfaces § 4): four [`DOMPoint`] corners in
/// the spec's `p1`-`p2`-`p3`-`p4` order (clockwise from top-left for a
/// non-rotated `fromRect()` form). Captures rotation + skew that an
/// axis-aligned [`DOMRect`] cannot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DOMQuad {
    pub p1: DOMPoint,
    pub p2: DOMPoint,
    pub p3: DOMPoint,
    pub p4: DOMPoint,
}

impl DOMQuad {
    /// Construct from the four corners (the `new DOMQuad(p1, p2, p3, p4)`
    /// WebIDL signature).
    pub const fn from_points(p1: DOMPoint, p2: DOMPoint, p3: DOMPoint, p4: DOMPoint) -> Self {
        Self { p1, p2, p3, p4 }
    }

    /// The `fromRect(rect)` constructor (§ 4.3): four corners from an
    /// axis-aligned rectangle, in the canonical top-left / top-right /
    /// bottom-right / bottom-left order. Negative-`width`/`height` inputs
    /// are normalised first so the corner order stays consistent.
    pub fn from_rect(rect: DOMRect) -> Self {
        let n = rect.normalized();
        let (x, y) = (n.x, n.y);
        let (w, h) = (n.width, n.height);
        Self {
            p1: DOMPoint::new_2d(x, y),
            p2: DOMPoint::new_2d(x + w, y),
            p3: DOMPoint::new_2d(x + w, y + h),
            p4: DOMPoint::new_2d(x, y + h),
        }
    }

    /// The axis-aligned bounding rectangle (§ 4.4 `bounds()`): the smallest
    /// [`DOMRect`] containing all four corners. Pure given the four corners.
    pub fn bounds(&self) -> DOMRect {
        let xs = [self.p1.x, self.p2.x, self.p3.x, self.p4.x];
        let ys = [self.p1.y, self.p2.y, self.p3.y, self.p4.y];
        let (min_x, max_x) = min_max(&xs);
        let (min_y, max_y) = min_max(&ys);
        DOMRect {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        }
    }
}

fn min_max(v: &[f64; 4]) -> (f64, f64) {
    let mut min = v[0];
    let mut max = v[0];
    for &x in v.iter().skip(1) {
        if x < min {
            min = x;
        }
        if x > max {
            max = x;
        }
    }
    (min, max)
}

// ---------------------------------------------------------------------------
// DOMMatrix (§ 6)
// ---------------------------------------------------------------------------

/// A 4×4 homogeneous transform matrix (Geometry Interfaces § 6). Stored
/// column-major as 16 fields named after the `matrix3d(...)` argument order
/// (m11..m44, also addressable as a/b/c/d/e/f for the 2D subset). The 2D
/// `matrix(a,b,c,d,e,f)` form folds into the upper-left 2×3 + the
/// `[0 0 1 0; 0 0 0 1]` bottom rows.
///
/// All operations are pure; `DOMMatrix::identity()` is the canonical
/// no-op. The matrix is "2D" iff the z-axis and perspective rows are
/// identity-shaped (see [`DOMMatrix::is_2d`]); the WebIDL reflects the
/// same `is2D` flag for callers that want to use the cheaper 2D code path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DOMMatrix {
    /// m11 = a (2D x-scale).
    pub m11: f64,
    /// m12 = b (2D y-skew).
    pub m12: f64,
    pub m13: f64,
    pub m14: f64,
    /// m21 = c (2D x-skew).
    pub m21: f64,
    /// m22 = d (2D y-scale).
    pub m22: f64,
    pub m23: f64,
    pub m24: f64,
    /// m41 = e (2D x-translate).
    pub m41: f64,
    /// m42 = f (2D y-translate).
    pub m42: f64,
    pub m43: f64,
    pub m44: f64,
    pub m31: f64,
    pub m32: f64,
    pub m33: f64,
    pub m34: f64,
}

impl Default for DOMMatrix {
    fn default() -> Self {
        Self::identity()
    }
}

impl DOMMatrix {
    /// The identity matrix (the `DOMMatrix()` constructor's default).
    pub const fn identity() -> Self {
        Self {
            m11: 1.0,
            m12: 0.0,
            m13: 0.0,
            m14: 0.0,
            m21: 0.0,
            m22: 1.0,
            m23: 0.0,
            m24: 0.0,
            m31: 0.0,
            m32: 0.0,
            m33: 1.0,
            m34: 0.0,
            m41: 0.0,
            m42: 0.0,
            m43: 0.0,
            m44: 1.0,
        }
    }

    /// Construct from the 2D `matrix(a, b, c, d, e, f)` form (Geometry
    /// Interfaces § 6.2.2). The z-axis + perspective rows are identity.
    pub const fn from_2d(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self {
            m11: a,
            m12: b,
            m13: 0.0,
            m14: 0.0,
            m21: c,
            m22: d,
            m23: 0.0,
            m24: 0.0,
            m31: 0.0,
            m32: 0.0,
            m33: 1.0,
            m34: 0.0,
            m41: e,
            m42: f,
            m43: 0.0,
            m44: 1.0,
        }
    }

    /// Construct from the full `matrix3d(m11, m12, …, m44)` column-major form
    /// (Geometry Interfaces § 6.2.2). Argument order matches the spec.
    pub const fn from_4x4_column_major(m: [f64; 16]) -> Self {
        Self {
            m11: m[0],
            m12: m[1],
            m13: m[2],
            m14: m[3],
            m21: m[4],
            m22: m[5],
            m23: m[6],
            m24: m[7],
            m31: m[8],
            m32: m[9],
            m33: m[10],
            m34: m[11],
            m41: m[12],
            m42: m[13],
            m43: m[14],
            m44: m[15],
        }
    }

    /// The 2D aliases `a`/`b`/`c`/`d`/`e`/`f` (Geometry Interfaces § 6.4).
    /// Reflects `m11`/`m12`/`m21`/`m22`/`m41`/`m42` so the 2D call surface
    /// round-trips with the cascade-resolved transform property.
    pub fn a(&self) -> f64 {
        self.m11
    }
    pub fn b(&self) -> f64 {
        self.m12
    }
    pub fn c(&self) -> f64 {
        self.m21
    }
    pub fn d(&self) -> f64 {
        self.m22
    }
    pub fn e(&self) -> f64 {
        self.m41
    }
    pub fn f(&self) -> f64 {
        self.m42
    }

    /// Whether this matrix reduces to the 2D subset (Geometry Interfaces
    /// § 6.5 `is2D`). True iff `m13 = m14 = m23 = m24 = m31 = m32 = m34 =
    /// m43 = 0` and `m33 = m44 = 1`.
    pub fn is_2d(&self) -> bool {
        const fn is_zero(x: f64) -> bool {
            x == 0.0
        }
        is_zero(self.m13)
            && is_zero(self.m14)
            && is_zero(self.m23)
            && is_zero(self.m24)
            && is_zero(self.m31)
            && is_zero(self.m32)
            && is_zero(self.m34)
            && is_zero(self.m43)
            && self.m33 == 1.0
            && self.m44 == 1.0
    }

    /// Column-major `[m11, m12, …, m44]` view (the WebIDL `toFloat64Array`
    /// accessor's 16-element form). Complements [`Self::from_4x4_column_major`].
    pub fn to_4x4_column_major(&self) -> [f64; 16] {
        [
            self.m11, self.m12, self.m13, self.m14, self.m21, self.m22, self.m23, self.m24,
            self.m31, self.m32, self.m33, self.m34, self.m41, self.m42, self.m43, self.m44,
        ]
    }

    /// `translate(tx, ty, tz)` per § 6.3. Returns a fresh matrix equivalent to
    /// `self * TranslateMatrix(tx, ty, tz)`.
    pub fn translate(&self, tx: f64, ty: f64, tz: f64) -> Self {
        let t = DOMMatrix::identity();
        let t = DOMMatrix {
            m41: tx,
            m42: ty,
            m43: tz,
            ..t
        };
        self.multiply(&t)
    }

    /// `scale(sx, sy, sz)` per § 6.3. Uniform when called with one argument;
    /// the WebIDL signature defaults `sy = sx` and `sz = 1`. The optional
    /// `(sx, sy, sz, fx, fy, fz)` "scale origin" form folds to
    /// `translate(fx,fy,fz) * scale * translate(-fx,-fy,-fz)` at the call
    /// site (the spec records the formula in § 6.3 step 5).
    pub fn scale(&self, sx: f64, sy: f64, sz: f64) -> Self {
        let s = DOMMatrix {
            m11: sx,
            m22: sy,
            m33: sz,
            ..DOMMatrix::identity()
        };
        self.multiply(&s)
    }

    /// `scaleNonUniform(sx, sy)` — the 2D form (`sz = 1`).
    pub fn scale_non_uniform(&self, sx: f64, sy: f64) -> Self {
        self.scale(sx, sy, 1.0)
    }

    /// `rotate(angle_in_degrees)` per § 6.3 — rotation about the z axis (the
    /// 2D rotation). Positive angles rotate clockwise (CSS y-axis points down).
    pub fn rotate(&self, angle_degrees: f64) -> Self {
        let theta = angle_degrees.to_radians();
        let (c, s) = (theta.cos(), theta.sin());
        let r = DOMMatrix::from_2d(c, s, -s, c, 0.0, 0.0);
        self.multiply(&r)
    }

    /// `rotateAxisAngle(x, y, z, angle_degrees)` per § 6.3 — rotation about an
    /// arbitrary axis through the origin. Implements the standard
    /// axis-angle → rotation-matrix formula (Rodrigues' rotation). The axis
    /// need not be normalised; a zero axis degrades to identity (no rotation).
    pub fn rotate_axis_angle(&self, x: f64, y: f64, z: f64, angle_degrees: f64) -> Self {
        let len2 = x * x + y * y + z * z;
        if len2 == 0.0 {
            return *self;
        }
        let inv = 1.0 / len2.sqrt();
        let (x, y, z) = (x * inv, y * inv, z * inv);
        let theta = angle_degrees.to_radians();
        let (c, s) = (theta.cos(), theta.sin());
        let t = 1.0 - c;
        let m11 = t * x * x + c;
        let m12 = t * x * y + s * z;
        let m13 = t * x * z - s * y;
        let m21 = t * x * y - s * z;
        let m22 = t * y * y + c;
        let m23 = t * y * z + s * x;
        let m31 = t * x * z + s * y;
        let m32 = t * y * z - s * x;
        let m33 = t * z * z + c;
        let r = DOMMatrix {
            m11,
            m12,
            m13,
            m14: 0.0,
            m21,
            m22,
            m23,
            m24: 0.0,
            m31,
            m32,
            m33,
            m34: 0.0,
            m41: 0.0,
            m42: 0.0,
            m43: 0.0,
            m44: 1.0,
        };
        self.multiply(&r)
    }

    /// `skewX(angle_in_degrees)` per § 6.3 — x-axis skew.
    pub fn skew_x(&self, angle_degrees: f64) -> Self {
        let t = angle_degrees.to_radians().tan();
        let skew = DOMMatrix::from_2d(1.0, 0.0, t, 1.0, 0.0, 0.0);
        self.multiply(&skew)
    }

    /// `skewY(angle_in_degrees)` per § 6.3 — y-axis skew.
    pub fn skew_y(&self, angle_degrees: f64) -> Self {
        let t = angle_degrees.to_radians().tan();
        let skew = DOMMatrix::from_2d(1.0, t, 0.0, 1.0, 0.0, 0.0);
        self.multiply(&skew)
    }

    /// `multiply(other)` per § 6.3 — the matrix product `self * other` (so a
    /// point becomes `self * (other * p)`, i.e. `other` applies first). See
    /// the module-level "Composition order" note.
    pub fn multiply(&self, other: &DOMMatrix) -> DOMMatrix {
        // 4×4 matrix product. Both stored column-major: m_ij is the element
        // at row i, column j. (self * other)[i, j] = sum_k self[i, k] * other[k, j].
        // Indexing helper for clarity.
        let a = |i: usize, j: usize| -> f64 { self.cell(i, j) };
        let b = |i: usize, j: usize| -> f64 { other.cell(i, j) };
        let mut out = [0.0f64; 16];
        for j in 0..4 {
            for i in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += a(i, k) * b(k, j);
                }
                out[j * 4 + i] = sum;
            }
        }
        DOMMatrix::from_4x4_column_major(out)
    }

    /// `flipX()` per § 6.3 — post-multiply by a y-axis mirror (`scale(-1, 1, 1)`).
    pub fn flip_x(&self) -> Self {
        let flip = DOMMatrix {
            m11: -1.0,
            ..DOMMatrix::identity()
        };
        self.multiply(&flip)
    }

    /// `flipY()` per § 6.3 — post-multiply by an x-axis mirror (`scale(1, -1, 1)`).
    pub fn flip_y(&self) -> Self {
        let flip = DOMMatrix {
            m22: -1.0,
            ..DOMMatrix::identity()
        };
        self.multiply(&flip)
    }

    /// The cell at row `i`, column `j` (0-indexed, both in `0..4`). Helper
    /// for the matrix-product arithmetic.
    fn cell(&self, i: usize, j: usize) -> f64 {
        debug_assert!(i < 4 && j < 4);
        // Stored column-major: column j is (m1(j+1), m2(j+1), m3(j+1), m4(j+1)).
        // Row i within that column.
        match (i, j) {
            (0, 0) => self.m11,
            (1, 0) => self.m12,
            (2, 0) => self.m13,
            (3, 0) => self.m14,
            (0, 1) => self.m21,
            (1, 1) => self.m22,
            (2, 1) => self.m23,
            (3, 1) => self.m24,
            (0, 2) => self.m31,
            (1, 2) => self.m32,
            (2, 2) => self.m33,
            (3, 2) => self.m34,
            (0, 3) => self.m41,
            (1, 3) => self.m42,
            (2, 3) => self.m43,
            (3, 3) => self.m44,
            _ => unreachable!(),
        }
    }

    /// Compute the determinant (Geometry Interfaces § 6.4 determinant; needed
    /// for `inverse()` and used by the `premultiply`/`postmultiply` no-op
    /// detection). Returns `0.0` for singular matrices.
    pub fn determinant(&self) -> f64 {
        // 4×4 determinant via Laplace expansion along the first row.
        let m = self.to_4x4_column_major();
        // Index helpers (column-major): m[j*4 + i].
        let c = |i: usize, j: usize| -> f64 { m[j * 4 + i] };
        // Compute the 3×3 minor obtained by dropping row r0 and column c0.
        let minor3 = |r0: usize, c0: usize| -> f64 {
            // Collect the remaining (row, col) entries into a 3×3.
            let rows: [usize; 3] = {
                let mut r = [0, 0, 0];
                let mut k = 0;
                for i in 0..4 {
                    if i != r0 {
                        r[k] = i;
                        k += 1;
                    }
                }
                r
            };
            let cols: [usize; 3] = {
                let mut cc = [0, 0, 0];
                let mut k = 0;
                for j in 0..4 {
                    if j != c0 {
                        cc[k] = j;
                        k += 1;
                    }
                }
                cc
            };
            let mut a3 = [0.0f64; 9];
            for (ri, &r) in rows.iter().enumerate() {
                for (cj, &col) in cols.iter().enumerate() {
                    a3[cj * 3 + ri] = c(r, col);
                }
            }
            // 3×3 determinant via the rule of Sarrus.
            a3[0] * (a3[4] * a3[8] - a3[5] * a3[7]) - a3[3] * (a3[1] * a3[8] - a3[2] * a3[7])
                + a3[6] * (a3[1] * a3[5] - a3[2] * a3[4])
        };
        // Laplace expansion along the first row (row 0).
        let mut det = 0.0;
        for j in 0..4 {
            let sign = if j % 2 == 0 { 1.0 } else { -1.0 };
            det += sign * c(0, j) * minor3(0, j);
        }
        det
    }

    /// `inverse()` per § 6.3. Returns `None` for a singular matrix
    /// (determinant zero); the WebIDL throws in that case, the Rust surface
    /// stays total.
    pub fn inverse(&self) -> Option<DOMMatrix> {
        let det = self.determinant();
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        // Adjugate / determinant: the (i, j) entry of the inverse is
        // cofactor(j, i) / det. Compute cofactors via the 3×3 minors.
        let m = self.to_4x4_column_major();
        let c = |i: usize, j: usize| -> f64 { m[j * 4 + i] };
        let minor3 = |r0: usize, c0: usize| -> f64 {
            let rows: [usize; 3] = {
                let mut r = [0, 0, 0];
                let mut k = 0;
                for i in 0..4 {
                    if i != r0 {
                        r[k] = i;
                        k += 1;
                    }
                }
                r
            };
            let cols: [usize; 3] = {
                let mut cc = [0, 0, 0];
                let mut k = 0;
                for j in 0..4 {
                    if j != c0 {
                        cc[k] = j;
                        k += 1;
                    }
                }
                cc
            };
            let mut a3 = [0.0f64; 9];
            for (ri, &r) in rows.iter().enumerate() {
                for (cj, &col) in cols.iter().enumerate() {
                    a3[cj * 3 + ri] = c(r, col);
                }
            }
            a3[0] * (a3[4] * a3[8] - a3[5] * a3[7]) - a3[3] * (a3[1] * a3[8] - a3[2] * a3[7])
                + a3[6] * (a3[1] * a3[5] - a3[2] * a3[4])
        };
        let mut out = [0.0f64; 16];
        for i in 0..4 {
            for j in 0..4 {
                let sign = if (i + j) % 2 == 0 { 1.0 } else { -1.0 };
                // inv[i, j] = adjugate[i, j] / det = cofactor(j, i) / det,
                // i.e. the (j, i) cofactor (the transpose step that turns the
                // cofactor matrix into the adjugate). Stored column-major at
                // out[j*4 + i].
                out[j * 4 + i] = sign * minor3(j, i) / det;
            }
        }
        Some(DOMMatrix::from_4x4_column_major(out))
    }

    /// `transformPoint(point)` per § 6.4. The § 6.4 step 5 perspective
    /// divide normalises the homogeneous result so the returned point's `w`
    /// is `1` (or the original `w` if `0`, matching the spec's "if w is 0,
    /// the point is at infinity and is returned unchanged" rule).
    pub fn transform_point(&self, point: DOMPoint) -> DOMPoint {
        let p = [point.x, point.y, point.z, point.w];
        let out: [f64; 4] = core::array::from_fn(|i| {
            let mut sum = 0.0;
            for (k, pk) in p.iter().enumerate() {
                sum += self.cell(i, k) * pk;
            }
            sum
        });
        let (x, y, z, w) = (out[0], out[1], out[2], out[3]);
        if w == 0.0 || !w.is_finite() {
            // Perspective divide is undefined — return the raw homogeneous
            // form (the spec leaves this case implementation-defined).
            return DOMPoint::new(x, y, z, w);
        }
        DOMPoint::new(x / w, y / w, z / w, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn point_approx(a: DOMPoint, b: DOMPoint) -> bool {
        approx(a.x, b.x) && approx(a.y, b.y) && approx(a.z, b.z) && approx(a.w, b.w)
    }

    fn matrix_approx(a: DOMMatrix, b: DOMMatrix) -> bool {
        let av = a.to_4x4_column_major();
        let bv = b.to_4x4_column_major();
        av.iter().zip(bv.iter()).all(|(x, y)| approx(*x, *y))
    }

    // --- DOMPoint ------------------------------------------------------

    #[test]
    fn point_2d_constructor() {
        let p = DOMPoint::new_2d(3.0, 4.0);
        assert_eq!(p.x, 3.0);
        assert_eq!(p.y, 4.0);
        assert_eq!(p.z, 0.0);
        assert_eq!(p.w, 1.0);
    }

    #[test]
    fn point_init_defaults() {
        let p = DOMPoint::from_init(DOMPointInit::DEFAULT);
        assert_eq!(p.x, 0.0);
        assert_eq!(p.y, 0.0);
        assert_eq!(p.z, 0.0);
        assert_eq!(p.w, 1.0);
    }

    // --- DOMRect -------------------------------------------------------

    #[test]
    fn rect_derived_edges_positive() {
        let r = DOMRect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        assert_eq!(r.left(), 10.0);
        assert_eq!(r.top(), 20.0);
        assert_eq!(r.right(), 110.0);
        assert_eq!(r.bottom(), 70.0);
    }

    #[test]
    fn rect_derived_edges_negative_dimensions() {
        // Negative width/height: spec stores them; derived edges cross.
        let r = DOMRect {
            x: 110.0,
            y: 70.0,
            width: -100.0,
            height: -50.0,
        };
        assert_eq!(r.left(), 10.0);
        assert_eq!(r.top(), 20.0);
        assert_eq!(r.right(), 110.0);
        assert_eq!(r.bottom(), 70.0);
    }

    #[test]
    fn rect_normalized_flips_negative_dims() {
        let r = DOMRect {
            x: 110.0,
            y: 70.0,
            width: -100.0,
            height: -50.0,
        };
        let n = r.normalized();
        assert_eq!(n.x, 10.0);
        assert_eq!(n.y, 20.0);
        assert_eq!(n.width, 100.0);
        assert_eq!(n.height, 50.0);
    }

    #[test]
    fn rect_contains_point_half_open() {
        let r = DOMRect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        assert!(r.contains_point(DOMPoint::new_2d(0.0, 0.0)));
        assert!(r.contains_point(DOMPoint::new_2d(9.9, 9.9)));
        // Right/bottom edges are NOT contained (half-open).
        assert!(!r.contains_point(DOMPoint::new_2d(10.0, 5.0)));
        assert!(!r.contains_point(DOMPoint::new_2d(5.0, 10.0)));
    }

    #[test]
    fn rect_intersects_and_disjoint() {
        let a = DOMRect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b = DOMRect {
            x: 5.0,
            y: 5.0,
            width: 10.0,
            height: 10.0,
        };
        assert!(a.intersects(&b));
        let c = DOMRect {
            x: 20.0,
            y: 20.0,
            width: 5.0,
            height: 5.0,
        };
        assert!(!a.intersects(&c));
        // Touching edges do not count.
        let touch = DOMRect {
            x: 10.0,
            y: 0.0,
            width: 5.0,
            height: 5.0,
        };
        assert!(!a.intersects(&touch));
    }

    #[test]
    fn rect_union_contains_both() {
        let a = DOMRect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b = DOMRect {
            x: 20.0,
            y: 20.0,
            width: 5.0,
            height: 5.0,
        };
        let u = a.union(&b);
        assert_eq!(u.left(), 0.0);
        assert_eq!(u.top(), 0.0);
        assert_eq!(u.right(), 25.0);
        assert_eq!(u.bottom(), 25.0);
    }

    // --- DOMQuad -------------------------------------------------------

    #[test]
    fn quad_from_rect_canonical_corner_order() {
        let q = DOMQuad::from_rect(DOMRect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        });
        // Top-left, top-right, bottom-right, bottom-left.
        assert!(point_approx(q.p1, DOMPoint::new_2d(10.0, 20.0)));
        assert!(point_approx(q.p2, DOMPoint::new_2d(110.0, 20.0)));
        assert!(point_approx(q.p3, DOMPoint::new_2d(110.0, 70.0)));
        assert!(point_approx(q.p4, DOMPoint::new_2d(10.0, 70.0)));
    }

    #[test]
    fn quad_from_negative_rect_normalises() {
        let q = DOMQuad::from_rect(DOMRect {
            x: 110.0,
            y: 70.0,
            width: -100.0,
            height: -50.0,
        });
        // Same corners as the positive case after normalisation.
        assert!(point_approx(q.p1, DOMPoint::new_2d(10.0, 20.0)));
    }

    #[test]
    fn quad_bounds_axis_aligned() {
        let q = DOMQuad::from_rect(DOMRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        });
        let b = q.bounds();
        assert_eq!(b.x, 1.0);
        assert_eq!(b.y, 2.0);
        assert_eq!(b.width, 3.0);
        assert_eq!(b.height, 4.0);
    }

    #[test]
    fn quad_bounds_rotated_uses_corner_extents() {
        // A diamond: rotated square with corners N/E/S/W.
        let q = DOMQuad::from_points(
            DOMPoint::new_2d(0.0, -1.0),
            DOMPoint::new_2d(1.0, 0.0),
            DOMPoint::new_2d(0.0, 1.0),
            DOMPoint::new_2d(-1.0, 0.0),
        );
        let b = q.bounds();
        assert!(approx(b.x, -1.0));
        assert!(approx(b.y, -1.0));
        assert!(approx(b.width, 2.0));
        assert!(approx(b.height, 2.0));
    }

    // --- DOMMatrix: identity + 2D detection ----------------------------

    #[test]
    fn identity_is_2d() {
        let m = DOMMatrix::identity();
        assert!(m.is_2d());
        let arr = m.to_4x4_column_major();
        // Identity has 1s on the diagonal.
        for i in 0..4 {
            assert!(approx(arr[i * 4 + i], 1.0));
        }
    }

    #[test]
    fn from_2d_is_2d() {
        let m = DOMMatrix::from_2d(1.0, 0.0, 0.0, 1.0, 5.0, 6.0);
        assert!(m.is_2d());
        assert_eq!(m.a(), 1.0);
        assert_eq!(m.b(), 0.0);
        assert_eq!(m.c(), 0.0);
        assert_eq!(m.d(), 1.0);
        assert_eq!(m.e(), 5.0);
        assert_eq!(m.f(), 6.0);
    }

    #[test]
    fn translate_with_z_is_not_2d() {
        let m = DOMMatrix::identity().translate(0.0, 0.0, 1.0);
        assert!(!m.is_2d());
    }

    // --- DOMMatrix: transforms -----------------------------------------

    #[test]
    fn translate_2d() {
        let m = DOMMatrix::identity().translate(10.0, 20.0, 0.0);
        let p = m.transform_point(DOMPoint::new_2d(1.0, 2.0));
        assert!(point_approx(p, DOMPoint::new_2d(11.0, 22.0)));
    }

    #[test]
    fn scale_2d() {
        let m = DOMMatrix::identity().scale(2.0, 3.0, 1.0);
        let p = m.transform_point(DOMPoint::new_2d(5.0, 5.0));
        assert!(point_approx(p, DOMPoint::new_2d(10.0, 15.0)));
    }

    #[test]
    fn rotate_90_clockwise_about_origin() {
        // CSS y-axis points down, so positive rotation is clockwise.
        let m = DOMMatrix::identity().rotate(90.0);
        let p = m.transform_point(DOMPoint::new_2d(1.0, 0.0));
        assert!(point_approx(p, DOMPoint::new_2d(0.0, 1.0)));
    }

    #[test]
    fn skew_x_simple() {
        // skewX(45°): (x, y) → (x + y*tan(45°), y) = (x + y, y).
        let m = DOMMatrix::identity().skew_x(45.0);
        let p = m.transform_point(DOMPoint::new_2d(0.0, 1.0));
        assert!(point_approx(p, DOMPoint::new_2d(1.0, 1.0)));
    }

    #[test]
    fn skew_y_simple() {
        // skewY(45°): (x, y) → (x, y + x*tan(45°)).
        let m = DOMMatrix::identity().skew_y(45.0);
        let p = m.transform_point(DOMPoint::new_2d(1.0, 0.0));
        assert!(point_approx(p, DOMPoint::new_2d(1.0, 1.0)));
    }

    #[test]
    fn rotate_axis_angle_90_about_z() {
        // 90° rotation about the z axis: x → y (CSS-clockwise).
        let m = DOMMatrix::identity().rotate_axis_angle(0.0, 0.0, 1.0, 90.0);
        let p = m.transform_point(DOMPoint::new_3d(1.0, 0.0, 0.0));
        assert!(point_approx(p, DOMPoint::new_3d(0.0, 1.0, 0.0)));
    }

    #[test]
    fn rotate_axis_angle_zero_axis_is_identity() {
        // A zero-length axis is a no-op (caller bug; stay total).
        let m = DOMMatrix::identity().rotate_axis_angle(0.0, 0.0, 0.0, 45.0);
        assert!(matrix_approx(m, DOMMatrix::identity()));
    }

    #[test]
    fn flip_x_inverts_x_axis() {
        let m = DOMMatrix::identity().flip_x();
        let p = m.transform_point(DOMPoint::new_2d(1.0, 2.0));
        assert!(point_approx(p, DOMPoint::new_2d(-1.0, 2.0)));
    }

    #[test]
    fn flip_y_inverts_y_axis() {
        let m = DOMMatrix::identity().flip_y();
        let p = m.transform_point(DOMPoint::new_2d(1.0, 2.0));
        assert!(point_approx(p, DOMPoint::new_2d(1.0, -2.0)));
    }

    // --- DOMMatrix: composition order ----------------------------------

    #[test]
    fn multiply_applies_rhs_first() {
        // `translate(10,0).multiply(scale(2,2))` ⇒ scale first, then translate.
        // Point (5, 0) becomes scale(2,2) ⇒ (10, 0) ⇒ translate(10,0) ⇒ (20, 0).
        let t = DOMMatrix::identity().translate(10.0, 0.0, 0.0);
        let s = DOMMatrix::identity().scale(2.0, 2.0, 1.0);
        let m = t.multiply(&s);
        let p = m.transform_point(DOMPoint::new_2d(5.0, 0.0));
        assert!(point_approx(p, DOMPoint::new_2d(20.0, 0.0)));
    }

    #[test]
    fn transform_chain_consistency() {
        // Verify a translate+rotate chain matches Firefox's interpretation
        // (rotate applies first when chained as translate.rotate).
        let m = DOMMatrix::identity().translate(10.0, 0.0, 0.0).rotate(90.0);
        // Point (1, 0) → rotate ⇒ (0, 1) → translate ⇒ (10, 1).
        let p = m.transform_point(DOMPoint::new_2d(1.0, 0.0));
        assert!(point_approx(p, DOMPoint::new_2d(10.0, 1.0)));
    }

    // --- DOMMatrix: determinant + inverse ------------------------------

    #[test]
    fn determinant_of_identity_is_one() {
        assert!(approx(DOMMatrix::identity().determinant(), 1.0));
    }

    #[test]
    fn determinant_of_scale_is_product() {
        let m = DOMMatrix::identity().scale(2.0, 3.0, 4.0);
        assert!(approx(m.determinant(), 24.0));
    }

    #[test]
    fn determinant_of_singular_matrix_is_zero() {
        // A projection onto the xy plane (m33 = 0, m44 = 0) is singular.
        let m = DOMMatrix {
            m33: 0.0,
            m44: 0.0,
            ..DOMMatrix::identity()
        };
        assert!(approx(m.determinant(), 0.0));
    }

    #[test]
    fn inverse_of_identity_is_identity() {
        let inv = DOMMatrix::identity().inverse().unwrap();
        assert!(matrix_approx(inv, DOMMatrix::identity()));
    }

    #[test]
    fn inverse_of_translate_negates_offset() {
        let m = DOMMatrix::identity().translate(10.0, 20.0, 0.0);
        let inv = m.inverse().unwrap();
        let p = inv.transform_point(DOMPoint::new_2d(10.0, 20.0));
        assert!(point_approx(p, DOMPoint::new_2d(0.0, 0.0)));
    }

    #[test]
    fn inverse_of_scale_negates_factor() {
        let m = DOMMatrix::identity().scale(2.0, 4.0, 1.0);
        let inv = m.inverse().unwrap();
        let p = inv.transform_point(DOMPoint::new_2d(2.0, 4.0));
        assert!(point_approx(p, DOMPoint::new_2d(1.0, 1.0)));
    }

    #[test]
    fn inverse_of_singular_returns_none() {
        let m = DOMMatrix {
            m33: 0.0,
            m44: 0.0,
            ..DOMMatrix::identity()
        };
        assert!(m.inverse().is_none());
    }

    #[test]
    fn multiply_by_inverse_is_identity() {
        let m = DOMMatrix::identity()
            .translate(5.0, 6.0, 0.0)
            .rotate(33.0)
            .scale(2.0, 3.0, 1.0)
            .skew_x(10.0);
        let inv = m.inverse().unwrap();
        let prod = m.multiply(&inv);
        assert!(matrix_approx(prod, DOMMatrix::identity()));
    }

    // --- DOMMatrix: perspective divide ---------------------------------

    #[test]
    fn transform_point_applies_perspective_divide() {
        // Perspective-shaped matrix: m34 = 1, m44 = 0 means transformed
        // w = original z (in spec naming mRC = math row C, math col R, so
        // m34 lives at math row 4, col 3 — the bottom row's z entry).
        let m = DOMMatrix {
            m14: 0.0,
            m24: 0.0,
            m34: 1.0,
            m44: 0.0,
            ..DOMMatrix::identity()
        };
        let p = m.transform_point(DOMPoint::new_3d(10.0, 2.0, 2.0));
        // Transformed point before divide: (x, y, z, w) = (10, 2, 2, 2).
        // After divide by w = 2: (5, 1, 1, 1).
        assert!(point_approx(p, DOMPoint::new_3d(5.0, 1.0, 1.0)));
    }

    // --- DOMMatrix: round-trip (storage ↔ array) -----------------------

    #[test]
    fn four_by_four_array_round_trips() {
        let mut m = DOMMatrix::identity();
        m.m11 = 1.0;
        m.m12 = 2.0;
        m.m13 = 3.0;
        m.m14 = 4.0;
        m.m21 = 5.0;
        m.m22 = 6.0;
        m.m33 = 9.0;
        m.m44 = 16.0;
        let arr = m.to_4x4_column_major();
        let restored = DOMMatrix::from_4x4_column_major(arr);
        assert!(matrix_approx(m, restored));
    }
}
