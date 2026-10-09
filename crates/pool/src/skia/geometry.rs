//! Skia geometry (`SkGeometry.cpp`, `SkPoint.cpp`, `SkMatrix.cpp`):
//! points, matrix, conics and their split into quads, chopping curves at
//! extrema. All in float, in the same operation order as Chrome.

use super::fixed::saturate2int;

pub const SCALAR_NEARLY_ZERO: f32 = 1.0 / (1 << 12) as f32;
pub const SCALAR_ROOT_2_OVER_2: f32 = std::f32::consts::FRAC_1_SQRT_2;
pub const SCALAR_PI: f32 = std::f32::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }
    pub fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
    pub fn dot(a: Point, b: Point) -> f32 {
        a.x * b.x + a.y * b.y
    }
    pub fn cross(a: Point, b: Point) -> f32 {
        a.x * b.y - a.y * b.x
    }
    pub fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
    pub fn is_zero(&self) -> bool {
        self.x == 0.0 && self.y == 0.0
    }
    /// `SkPointPriv::CanNormalize`.
    pub fn can_normalize(dx: f32, dy: f32) -> bool {
        dx.is_finite() && dy.is_finite() && (dx != 0.0 || dy != 0.0)
    }
    /// `SkPointPriv::EqualsWithinTolerance(p1, p2)`.
    pub fn equals_within_tolerance(a: Point, b: Point) -> bool {
        !Point::can_normalize(a.x - b.x, a.y - b.y)
    }
    /// `SkPoint::setLength` — `set_point_length<false>`.
    pub fn set_length(&mut self, length: f32) -> bool {
        let (x, y) = (self.x, self.y);
        let mut xx = x as f64;
        let mut yy = y as f64;
        let dmag = (xx * xx + yy * yy).sqrt();
        let dscale = length as f64 / dmag;
        xx *= dscale;
        yy *= dscale;
        let nx = xx as f32;
        let ny = yy as f32;
        if !nx.is_finite() || !ny.is_finite() || (nx == 0.0 && ny == 0.0) {
            self.x = 0.0;
            self.y = 0.0;
            return false;
        }
        self.x = nx;
        self.y = ny;
        true
    }
}

// ── Matrix (affine only) ──────────────────────────────────────────────────

/// `SkMatrix` without perspective: [scaleX skewX transX skewY scaleY transY].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub sx: f32,
    pub kx: f32,
    pub tx: f32,
    pub ky: f32,
    pub sy: f32,
    pub ty: f32,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        sx: 1.0,
        kx: 0.0,
        tx: 0.0,
        ky: 0.0,
        sy: 1.0,
        ty: 0.0,
    };

    pub fn scale(sx: f32, sy: f32) -> Matrix {
        Matrix {
            sx,
            kx: 0.0,
            tx: 0.0,
            ky: 0.0,
            sy,
            ty: 0.0,
        }
    }
    pub fn is_identity(&self) -> bool {
        *self == Matrix::IDENTITY
    }
    pub fn is_scale_translate(&self) -> bool {
        self.kx == 0.0 && self.ky == 0.0
    }
    pub fn is_translate_only(&self) -> bool {
        self.is_scale_translate() && self.sx == 1.0 && self.sy == 1.0
    }
    /// `SkMatrix::rectStaysRect()` per `computeTypeMask`: compares bits,
    /// so −0 counts as non-zero.
    pub fn rect_stays_rect(&self) -> bool {
        let nz = |v: f32| v.to_bits() != 0;
        if nz(self.kx) || nz(self.ky) {
            !nz(self.sx) && !nz(self.sy) && nz(self.kx) && nz(self.ky)
        } else {
            nz(self.sx) && nz(self.sy)
        }
    }
    /// `setSinCos(sin, cos)`.
    pub fn sin_cos(s: f32, c: f32) -> Matrix {
        Matrix {
            sx: c,
            kx: -s,
            tx: 0.0,
            ky: s,
            sy: c,
            ty: 0.0,
        }
    }
    pub fn post_translate(&mut self, dx: f32, dy: f32) {
        self.tx += dx;
        self.ty += dy;
    }
    /// `preScale(sx, sy)` = setConcat(self, scale).
    pub fn pre_scale(&mut self, sx: f32, sy: f32) {
        if sx == 1.0 && sy == 1.0 {
            return;
        }
        *self = Matrix::concat(self, &Matrix::scale(sx, sy));
    }
    /// `postConcat(m)` = setConcat(m, self).
    pub fn post_concat(&mut self, m: &Matrix) {
        if !m.is_identity() {
            *self = Matrix::concat(m, self);
        }
    }
    /// `SkMatrix::setConcat(a, b)` for affine matrices.
    pub fn concat(a: &Matrix, b: &Matrix) -> Matrix {
        if a.is_identity() {
            return *b;
        }
        if b.is_identity() {
            return *a;
        }
        if a.is_scale_translate() && b.is_scale_translate() {
            return Matrix {
                sx: a.sx * b.sx,
                kx: 0.0,
                tx: a.sx * b.tx + a.tx,
                ky: 0.0,
                sy: a.sy * b.sy,
                ty: a.sy * b.ty + a.ty,
            };
        }
        // muladdmul(a, b, c, d) = a*b + c*d, computed in double and narrowed.
        let mam = |a: f32, b: f32, c: f32, d: f32| -> f32 {
            (a as f64 * b as f64 + c as f64 * d as f64) as f32
        };
        Matrix {
            sx: mam(a.sx, b.sx, a.kx, b.ky),
            kx: mam(a.sx, b.kx, a.kx, b.sy),
            tx: mam(a.sx, b.tx, a.kx, b.ty) + a.tx,
            ky: mam(a.ky, b.sx, a.sy, b.ky),
            sy: mam(a.ky, b.kx, a.sy, b.sy),
            ty: mam(a.ky, b.tx, a.sy, b.ty) + a.ty,
        }
    }
    /// `SkMatrix::mapPoints`: Skia's three procs by matrix type.
    pub fn map_points(&self, pts: &mut [Point]) {
        if self.is_identity() {
            return;
        }
        if self.is_translate_only() {
            for p in pts.iter_mut() {
                p.x += self.tx;
                p.y += self.ty;
            }
        } else if self.is_scale_translate() {
            for p in pts.iter_mut() {
                p.x = p.x * self.sx + self.tx;
                p.y = p.y * self.sy + self.ty;
            }
        } else {
            for p in pts.iter_mut() {
                let (x, y) = (p.x, p.y);
                p.x = x * self.sx + y * self.kx + self.tx;
                p.y = y * self.sy + x * self.ky + self.ty;
            }
        }
    }
    pub fn map_point(&self, p: Point) -> Point {
        let mut a = [p];
        self.map_points(&mut a);
        a[0]
    }
    pub fn map_rect(&self, r: &Rect) -> Rect {
        let mut pts = [Point::new(r.left, r.top), Point::new(r.right, r.bottom)];
        if self.is_scale_translate() {
            self.map_points(&mut pts);
            Rect::from_ltrb(pts[0].x, pts[0].y, pts[1].x, pts[1].y).sorted()
        } else {
            let mut quad = [
                Point::new(r.left, r.top),
                Point::new(r.right, r.top),
                Point::new(r.right, r.bottom),
                Point::new(r.left, r.bottom),
            ];
            self.map_points(&mut quad);
            Rect::bounds(&quad)
        }
    }
}

// ── Rect ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub const fn from_ltrb(left: f32, top: f32, right: f32, bottom: f32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }
    pub fn width(&self) -> f32 {
        self.right - self.left
    }
    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
    pub fn center_x(&self) -> f32 {
        // SkRect::centerX: sk_float_midpoint(fLeft, fRight)
        (self.left as f64 * 0.5 + self.right as f64 * 0.5) as f32
    }
    pub fn center_y(&self) -> f32 {
        (self.top as f64 * 0.5 + self.bottom as f64 * 0.5) as f32
    }
    pub fn is_empty(&self) -> bool {
        !(self.left < self.right && self.top < self.bottom)
    }
    pub fn sorted(&self) -> Rect {
        Rect {
            left: self.left.min(self.right),
            top: self.top.min(self.bottom),
            right: self.left.max(self.right),
            bottom: self.top.max(self.bottom),
        }
    }
    pub fn bounds(pts: &[Point]) -> Rect {
        if pts.is_empty() {
            return Rect::default();
        }
        let (mut l, mut t, mut r, mut b) = (pts[0].x, pts[0].y, pts[0].x, pts[0].y);
        for p in &pts[1..] {
            l = l.min(p.x);
            t = t.min(p.y);
            r = r.max(p.x);
            b = b.max(p.y);
        }
        Rect {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }
    pub fn is_finite(&self) -> bool {
        self.left.is_finite()
            && self.top.is_finite()
            && self.right.is_finite()
            && self.bottom.is_finite()
    }
    pub fn round_out(&self) -> IRect {
        IRect {
            left: saturate2int(self.left.floor()),
            top: saturate2int(self.top.floor()),
            right: saturate2int(self.right.ceil()),
            bottom: saturate2int(self.bottom.ceil()),
        }
    }
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let l = self.left.max(o.left);
        let t = self.top.max(o.top);
        let r = self.right.min(o.right);
        let b = self.bottom.min(o.bottom);
        if l < r && t < b {
            Some(Rect {
                left: l,
                top: t,
                right: r,
                bottom: b,
            })
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct IRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl IRect {
    pub const fn from_ltrb(left: i32, top: i32, right: i32, bottom: i32) -> IRect {
        IRect {
            left,
            top,
            right,
            bottom,
        }
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn is_empty(&self) -> bool {
        !(self.left < self.right && self.top < self.bottom)
    }
    pub fn intersect(&self, o: &IRect) -> Option<IRect> {
        let r = IRect {
            left: self.left.max(o.left),
            top: self.top.max(o.top),
            right: self.right.min(o.right),
            bottom: self.bottom.min(o.bottom),
        };
        if r.is_empty() {
            None
        } else {
            Some(r)
        }
    }
    pub fn contains(&self, o: &IRect) -> bool {
        !o.is_empty()
            && !self.is_empty()
            && self.left <= o.left
            && self.top <= o.top
            && self.right >= o.right
            && self.bottom >= o.bottom
    }
    pub fn to_rect(&self) -> Rect {
        Rect::from_ltrb(
            self.left as f32,
            self.top as f32,
            self.right as f32,
            self.bottom as f32,
        )
    }
}

// ── Quads ─────────────────────────────────────────────────────────────────

#[inline]
fn interp(v0: f32, v1: f32, t: f32) -> f32 {
    v0 + (v1 - v0) * t
}

/// `SkChopQuadAt(src, dst, t)`.
pub fn chop_quad_at(src: &[Point; 3], t: f32) -> [Point; 5] {
    let p01 = Point::new(interp(src[0].x, src[1].x, t), interp(src[0].y, src[1].y, t));
    let p12 = Point::new(interp(src[1].x, src[2].x, t), interp(src[1].y, src[2].y, t));
    [
        src[0],
        p01,
        Point::new(interp(p01.x, p12.x, t), interp(p01.y, p12.y, t)),
        p12,
        src[2],
    ]
}

/// `valid_unit_divide`: 0 < numer/denom < 1, else None.
pub fn valid_unit_divide(mut numer: f32, mut denom: f32) -> Option<f32> {
    if numer < 0.0 {
        numer = -numer;
        denom = -denom;
    }
    if denom == 0.0 || numer == 0.0 || numer >= denom {
        return None;
    }
    let r = numer / denom;
    if r.is_nan() {
        return None;
    }
    if r == 0.0 {
        return None;
    }
    Some(r)
}

/// `SkFindUnitQuadRoots`.
pub fn find_unit_quad_roots(a: f32, b: f32, c: f32) -> ([f32; 2], usize) {
    let mut roots = [0.0f32; 2];
    if a == 0.0 {
        return match valid_unit_divide(-c, b) {
            Some(r) => {
                roots[0] = r;
                (roots, 1)
            }
            None => (roots, 0),
        };
    }
    let mut dr = b as f64 * b as f64 - 4.0 * a as f64 * c as f64;
    if dr < 0.0 {
        return (roots, 0);
    }
    dr = dr.sqrt();
    let r = dr as f32;
    if !r.is_finite() {
        return (roots, 0);
    }
    let q = if b < 0.0 {
        -(b - r) / 2.0
    } else {
        -(b + r) / 2.0
    };
    let mut n = 0;
    if let Some(v) = valid_unit_divide(q, a) {
        roots[n] = v;
        n += 1;
    }
    if let Some(v) = valid_unit_divide(c, q) {
        roots[n] = v;
        n += 1;
    }
    if n == 2 {
        if roots[0] > roots[1] {
            roots.swap(0, 1);
        } else if roots[0] == roots[1] {
            n -= 1;
        }
    }
    (roots, n)
}

#[inline]
fn is_not_monotonic(a: f32, b: f32, c: f32) -> bool {
    let ab = a - b;
    let mut bc = b - c;
    if ab < 0.0 {
        bc = -bc;
    }
    ab == 0.0 || bc < 0.0
}

/// `SkChopQuadAtYExtrema`: returns the number of quads (1 or 2) in `dst`.
pub fn chop_quad_at_y_extrema(src: &[Point; 3], dst: &mut [Point; 5]) -> usize {
    let a = src[0].y;
    let mut b = src[1].y;
    let c = src[2].y;
    if is_not_monotonic(a, b, c) {
        if let Some(t) = valid_unit_divide(a - b, a - b - b + c) {
            *dst = chop_quad_at(src, t);
            // flatten_double_quad_extrema
            let m = dst[2].y;
            dst[1].y = m;
            dst[3].y = m;
            return 2;
        }
        b = if (a - b).abs() < (b - c).abs() { a } else { c };
    }
    dst[0] = Point::new(src[0].x, a);
    dst[1] = Point::new(src[1].x, b);
    dst[2] = Point::new(src[2].x, c);
    1
}

/// `SkChopQuadAtXExtrema`.
pub fn chop_quad_at_x_extrema(src: &[Point; 3], dst: &mut [Point; 5]) -> usize {
    let a = src[0].x;
    let mut b = src[1].x;
    let c = src[2].x;
    if is_not_monotonic(a, b, c) {
        if let Some(t) = valid_unit_divide(a - b, a - b - b + c) {
            *dst = chop_quad_at(src, t);
            let m = dst[2].x;
            dst[1].x = m;
            dst[3].x = m;
            return 2;
        }
        b = if (a - b).abs() < (b - c).abs() { a } else { c };
    }
    dst[0] = Point::new(a, src[0].y);
    dst[1] = Point::new(b, src[1].y);
    dst[2] = Point::new(c, src[2].y);
    1
}

// ── Cubics ────────────────────────────────────────────────────────────────

#[inline]
fn unchecked_mix(a: f32, b: f32, t: f32) -> f32 {
    (b - a) * t + a
}

/// `SkChopCubicAt(src, dst[7], t)`.
pub fn chop_cubic_at(src: &[Point; 4], t: f32) -> [Point; 7] {
    if t == 1.0 {
        return [src[0], src[1], src[2], src[3], src[3], src[3], src[3]];
    }
    let mix =
        |a: Point, b: Point| Point::new(unchecked_mix(a.x, b.x, t), unchecked_mix(a.y, b.y, t));
    let ab = mix(src[0], src[1]);
    let bc = mix(src[1], src[2]);
    let cd = mix(src[2], src[3]);
    let abc = mix(ab, bc);
    let bcd = mix(bc, cd);
    let abcd = mix(abc, bcd);
    [src[0], ab, abc, abcd, bcd, cd, src[3]]
}

/// `SkChopCubicAt(src, dst[10], t0, t1)`.
pub fn chop_cubic_at2(src: &[Point; 4], t0: f32, t1: f32) -> [Point; 10] {
    if t1 == 1.0 {
        let d = chop_cubic_at(src, t0);
        let mut out = [Point::default(); 10];
        out[..7].copy_from_slice(&d);
        out[7] = src[3];
        out[8] = src[3];
        out[9] = src[3];
        return out;
    }
    let mixp = |a: Point, b: Point, t: f32| {
        Point::new(unchecked_mix(a.x, b.x, t), unchecked_mix(a.y, b.y, t))
    };
    // Two chops "in parallel", as with float4 in Skia: lo at t0, hi at t1.
    let ab0 = mixp(src[0], src[1], t0);
    let bc0 = mixp(src[1], src[2], t0);
    let cd0 = mixp(src[2], src[3], t0);
    let abc0 = mixp(ab0, bc0, t0);
    let bcd0 = mixp(bc0, cd0, t0);
    let abcd0 = mixp(abc0, bcd0, t0);
    let ab1 = mixp(src[0], src[1], t1);
    let bc1 = mixp(src[1], src[2], t1);
    let cd1 = mixp(src[2], src[3], t1);
    let abc1 = mixp(ab1, bc1, t1);
    let bcd1 = mixp(bc1, cd1, t1);
    let abcd1 = mixp(abc1, bcd1, t1);
    // middle = mix(abc, bcd, shuffle<2,3,0,1>(T)): lo at t1, hi at t0.
    let middle_lo = mixp(abc0, bcd0, t1);
    let middle_hi = mixp(abc1, bcd1, t0);
    [
        src[0], ab0, abc0, abcd0, middle_lo, middle_hi, abcd1, bcd1, cd1, src[3],
    ]
}

/// `SkFindCubicExtrema`.
pub fn find_cubic_extrema(a: f32, b: f32, c: f32, d: f32) -> ([f32; 2], usize) {
    let aa = d - a + 3.0 * (b - c);
    let bb = 2.0 * (a - b - b + c);
    let cc = b - a;
    find_unit_quad_roots(aa, bb, cc)
}

/// `SkChopCubicAt(src, dst, tValues, roots)`: general form with 0..2 roots.
fn chop_cubic_at_roots(src: &[Point; 4], t: &[f32], dst: &mut [Point; 10]) {
    match t.len() {
        0 => dst[..4].copy_from_slice(src),
        1 => {
            let d = chop_cubic_at(src, t[0]);
            dst[..7].copy_from_slice(&d);
        }
        _ => {
            *dst = chop_cubic_at2(src, t[0], t[1]);
        }
    }
}

/// `SkChopCubicAtYExtrema`: number of cubics (1..3) in `dst`.
pub fn chop_cubic_at_y_extrema(src: &[Point; 4], dst: &mut [Point; 10]) -> usize {
    let (t, n) = find_cubic_extrema(src[0].y, src[1].y, src[2].y, src[3].y);
    chop_cubic_at_roots(src, &t[..n], dst);
    if n > 0 {
        let m = dst[3].y;
        dst[2].y = m;
        dst[4].y = m;
        if n == 2 {
            let m = dst[6].y;
            dst[5].y = m;
            dst[7].y = m;
        }
    }
    n + 1
}

/// `SkChopCubicAtXExtrema`.
pub fn chop_cubic_at_x_extrema(src: &[Point; 4], dst: &mut [Point; 10]) -> usize {
    let (t, n) = find_cubic_extrema(src[0].x, src[1].x, src[2].x, src[3].x);
    chop_cubic_at_roots(src, &t[..n], dst);
    if n > 0 {
        let m = dst[3].x;
        dst[2].x = m;
        dst[4].x = m;
        if n == 2 {
            let m = dst[6].x;
            dst[5].x = m;
            dst[7].x = m;
        }
    }
    n + 1
}

// ── Conics ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default)]
pub struct Conic {
    pub pts: [Point; 3],
    pub w: f32,
}

pub const MAX_CONIC_TO_QUAD_POW2: usize = 5;
pub const MAX_CONICS_FOR_ARC: usize = 5;

#[inline]
fn subdivide_w_value(w: f32) -> f32 {
    (0.5 + w * 0.5).sqrt()
}

#[inline]
fn between(a: f32, b: f32, c: f32) -> bool {
    (a - b) * (c - b) <= 0.0
}

impl Conic {
    pub fn new(p0: Point, p1: Point, p2: Point, w: f32) -> Conic {
        Conic {
            pts: [p0, p1, p2],
            w,
        }
    }

    /// `SkConic::chop`, the `SK_SUPPORT_LEGACY_CONIC_CHOP` variant: Chromium
    /// builds Skia with this flag (`skia/config/SkUserConfig.h`), and the float
    /// operation order differs from the new code.
    pub fn chop(&self) -> [Conic; 2] {
        let scale = 1.0 / (1.0 + self.w);
        let nw = subdivide_w_value(self.w);
        let (p0, p1, p2) = (self.pts[0], self.pts[1], self.pts[2]);
        let wp1 = Point::new(self.w * p1.x, self.w * p1.y);
        let twice = Point::new(wp1.x + wp1.x, wp1.y + wp1.y);
        let mut m = Point::new(
            ((p0.x + twice.x) + p2.x) * scale * 0.5,
            ((p0.y + twice.y) + p2.y) * scale * 0.5,
        );
        if !m.is_finite() {
            let w_d = self.w as f64;
            let w_2 = w_d * 2.0;
            let scale_half = 1.0 / (1.0 + w_d) * 0.5;
            m.x = ((p0.x as f64 + w_2 * p1.x as f64 + p2.x as f64) * scale_half) as f32;
            m.y = ((p0.y as f64 + w_2 * p1.y as f64 + p2.y as f64) * scale_half) as f32;
        }
        let c1 = Point::new((p0.x + wp1.x) * scale, (p0.y + wp1.y) * scale);
        let c2 = Point::new((wp1.x + p2.x) * scale, (wp1.y + p2.y) * scale);
        [
            Conic {
                pts: [p0, c1, m],
                w: nw,
            },
            Conic {
                pts: [m, c2, p2],
                w: nw,
            },
        ]
    }

    /// `SkConic::computeQuadPOW2(tol)`.
    pub fn compute_quad_pow2(&self, tol: f32) -> usize {
        if tol < 0.0
            || !tol.is_finite()
            || !self.pts.iter().all(|p| p.is_finite())
            || self.w < 0.0
            || !self.w.is_finite()
        {
            return 0;
        }
        let a = self.w - 1.0;
        let k = a / (4.0 * (2.0 + a));
        let x = k * (self.pts[0].x - 2.0 * self.pts[1].x + self.pts[2].x);
        let y = k * (self.pts[0].y - 2.0 * self.pts[1].y + self.pts[2].y);
        let mut error = (x * x + y * y).sqrt();
        let mut pow2 = 0;
        while pow2 < MAX_CONIC_TO_QUAD_POW2 {
            if error <= tol {
                break;
            }
            error *= 0.25;
            pow2 += 1;
        }
        pow2
    }

    fn subdivide(src: &Conic, pts: &mut Vec<Point>, level: usize) {
        if level == 0 {
            pts.push(src.pts[1]);
            pts.push(src.pts[2]);
            return;
        }
        let mut dst = src.chop();
        let start_y = src.pts[0].y;
        let end_y = src.pts[2].y;
        if between(start_y, src.pts[1].y, end_y) {
            let mid_y = dst[0].pts[2].y;
            if !between(start_y, mid_y, end_y) {
                let closer_y = if (mid_y - start_y).abs() < (mid_y - end_y).abs() {
                    start_y
                } else {
                    end_y
                };
                dst[0].pts[2].y = closer_y;
                dst[1].pts[0].y = closer_y;
            }
            if !between(start_y, dst[0].pts[1].y, dst[0].pts[2].y) {
                dst[0].pts[1].y = start_y;
            }
            if !between(dst[1].pts[0].y, dst[1].pts[1].y, end_y) {
                dst[1].pts[1].y = end_y;
            }
        }
        Conic::subdivide(&dst[0], pts, level - 1);
        Conic::subdivide(&dst[1], pts, level - 1);
    }

    /// `SkConic::chopIntoQuadsPOW2`: quad points (1 + 2·N of them), N quads.
    pub fn chop_into_quads_pow2(&self, mut pow2: usize) -> (Vec<Point>, usize) {
        if self.w < 0.0 || !self.w.is_finite() {
            pow2 = 0;
        }
        let mut pts = Vec::with_capacity(1 + 2 * (1 << pow2));
        pts.push(self.pts[0]);
        let mut done = false;
        if pow2 == MAX_CONIC_TO_QUAD_POW2 {
            let dst = self.chop();
            if Point::equals_within_tolerance(dst[0].pts[1], dst[0].pts[2])
                && Point::equals_within_tolerance(dst[1].pts[0], dst[1].pts[1])
            {
                pts.push(dst[0].pts[1]);
                pts.push(dst[0].pts[1]);
                pts.push(dst[0].pts[1]);
                pts.push(dst[1].pts[2]);
                pow2 = 1;
                done = true;
            }
        }
        if !done {
            Conic::subdivide(self, &mut pts, pow2);
        }
        let quad_count = 1 << pow2;
        let pt_count = 2 * quad_count + 1;
        debug_assert_eq!(pts.len(), pt_count);
        if pts[..pt_count].iter().any(|p| !p.is_finite()) {
            for p in pts[1..pt_count - 1].iter_mut() {
                *p = self.pts[1];
            }
        }
        (pts, quad_count)
    }

    /// `SkAutoConicToQuads::computeQuads(pts, w, tol)`.
    pub fn to_quads(&self, tol: f32) -> (Vec<Point>, usize) {
        let pow2 = self.compute_quad_pow2(tol);
        self.chop_into_quads_pow2(pow2)
    }

    /// `SkConic::BuildUnitArc`.
    pub fn build_unit_arc(
        u_start: Point,
        u_stop: Point,
        ccw: bool,
        user_matrix: Option<&Matrix>,
    ) -> Vec<Conic> {
        let x = Point::dot(u_start, u_stop);
        let mut y = Point::cross(u_start, u_stop);
        let abs_y = y.abs();
        if abs_y <= SCALAR_NEARLY_ZERO && x > 0.0 && ((y >= 0.0 && !ccw) || (y <= 0.0 && ccw)) {
            return Vec::new();
        }
        if ccw {
            y = -y;
        }
        let mut quadrant = 0usize;
        if y == 0.0 {
            quadrant = 2;
        } else if x == 0.0 {
            quadrant = if y > 0.0 { 1 } else { 3 };
        } else {
            if y < 0.0 {
                quadrant += 2;
            }
            if (x < 0.0) != (y < 0.0) {
                quadrant += 1;
            }
        }
        const QUADRANT_PTS: [Point; 8] = [
            Point::new(1.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(0.0, 1.0),
            Point::new(-1.0, 1.0),
            Point::new(-1.0, 0.0),
            Point::new(-1.0, -1.0),
            Point::new(0.0, -1.0),
            Point::new(1.0, -1.0),
        ];
        let mut out: Vec<Conic> = Vec::with_capacity(MAX_CONICS_FOR_ARC);
        for i in 0..quadrant {
            out.push(Conic::new(
                QUADRANT_PTS[i * 2],
                QUADRANT_PTS[i * 2 + 1],
                QUADRANT_PTS[(i * 2 + 2) % 8],
                SCALAR_ROOT_2_OVER_2,
            ));
        }
        let final_p = Point::new(x, y);
        let last_q = QUADRANT_PTS[quadrant * 2];
        let dot = Point::dot(last_q, final_p);
        if dot.is_nan() {
            return Vec::new();
        }
        if dot < 1.0 {
            let mut off_curve = Point::new(last_q.x + x, last_q.y + y);
            let cos_theta_over_2 = ((1.0 + dot) / 2.0).sqrt();
            off_curve.set_length(1.0 / cos_theta_over_2);
            if !Point::equals_within_tolerance(last_q, off_curve) {
                out.push(Conic::new(last_q, off_curve, final_p, cos_theta_over_2));
            }
        }
        let mut matrix = Matrix::sin_cos(u_start.y, u_start.x);
        if ccw {
            matrix.pre_scale(1.0, -1.0);
        }
        if let Some(m) = user_matrix {
            matrix.post_concat(m);
        }
        for c in out.iter_mut() {
            matrix.map_points(&mut c.pts);
        }
        out
    }
}

#[inline]
pub fn sin_snap_to_zero(radians: f32) -> f32 {
    let v = radians.sin();
    // SK_ScalarSinCosNearlyZero = SK_Scalar1 / (1 << 16)
    if v.abs() <= 1.0 / 65536.0 {
        0.0
    } else {
        v
    }
}
#[inline]
pub fn cos_snap_to_zero(radians: f32) -> f32 {
    let v = radians.cos();
    if v.abs() <= 1.0 / 65536.0 {
        0.0
    } else {
        v
    }
}
#[inline]
pub fn degrees_to_radians(d: f32) -> f32 {
    d * (SCALAR_PI / 180.0)
}
#[inline]
pub fn nearly_equal(a: f32, b: f32) -> bool {
    (a - b).abs() <= SCALAR_NEARLY_ZERO
}

// ── Matrix: invert, PolyToPoly, scale (SkMatrix.cpp) ──────────────────────

impl Matrix {
    pub fn translate(dx: f32, dy: f32) -> Matrix {
        Matrix {
            sx: 1.0,
            kx: 0.0,
            tx: dx,
            ky: 0.0,
            sy: 1.0,
            ty: dy,
        }
    }
    /// `SkMatrix::ScaleTranslate`.
    pub fn scale_translate(sx: f32, sy: f32, tx: f32, ty: f32) -> Matrix {
        Matrix {
            sx,
            kx: 0.0,
            tx,
            ky: 0.0,
            sy,
            ty,
        }
    }
    /// `postScale(sx, sy)` = setConcat(scale, self).
    pub fn post_scale(&mut self, sx: f32, sy: f32) {
        if sx == 1.0 && sy == 1.0 {
            return;
        }
        *self = Matrix::concat(&Matrix::scale(sx, sy), self);
    }
    fn is_scale_mask(&self) -> bool {
        self.sx != 1.0 || self.sy != 1.0
    }
    fn is_translate_mask(&self) -> bool {
        self.tx != 0.0 || self.ty != 0.0
    }
    fn is_affine_mask(&self) -> bool {
        self.kx != 0.0 || self.ky != 0.0
    }
    /// `SkMatrix::invert()` without perspective: a scale/translate path and a
    /// general one in double.
    pub fn invert(&self) -> Option<Matrix> {
        if !self.is_scale_mask() && !self.is_translate_mask() && !self.is_affine_mask() {
            return Some(*self);
        }
        if !self.is_affine_mask() {
            if self.is_scale_mask() {
                let inv_sx = 1.0 / self.sx;
                let inv_sy = 1.0 / self.sy;
                if !inv_sx.is_finite() || !inv_sy.is_finite() {
                    return None;
                }
                let inv_tx = -self.tx * inv_sx;
                let inv_ty = -self.ty * inv_sy;
                if !inv_tx.is_finite() || !inv_ty.is_finite() {
                    return None;
                }
                return Some(Matrix {
                    sx: inv_sx,
                    kx: 0.0,
                    tx: inv_tx,
                    ky: 0.0,
                    sy: inv_sy,
                    ty: inv_ty,
                });
            }
            if !self.tx.is_finite() || !self.ty.is_finite() {
                return None;
            }
            return Some(Matrix::translate(-self.tx, -self.ty));
        }
        // sk_inv_determinant: dcross(scaleX, scaleY, skewX, skewY) in double.
        let det = self.sx as f64 * self.sy as f64 - self.kx as f64 * self.ky as f64;
        let nz = SCALAR_NEARLY_ZERO * SCALAR_NEARLY_ZERO * SCALAR_NEARLY_ZERO;
        if (det as f32).abs() <= nz {
            return None;
        }
        let inv_det = 1.0 / det;
        let dcross_dscale = |a: f32, b: f32, c: f32, d: f32| -> f32 {
            ((a as f64 * b as f64 - c as f64 * d as f64) * inv_det) as f32
        };
        let m = Matrix {
            sx: (self.sy as f64 * inv_det) as f32,
            kx: (-self.kx as f64 * inv_det) as f32,
            tx: dcross_dscale(self.kx, self.ty, self.sy, self.tx),
            ky: (-self.ky as f64 * inv_det) as f32,
            sy: (self.sx as f64 * inv_det) as f32,
            ty: dcross_dscale(self.ky, self.tx, self.sx, self.ty),
        };
        if ![m.sx, m.kx, m.tx, m.ky, m.sy, m.ty]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        Some(m)
    }
    /// `SkMatrix::PolyToPoly` for two points: `Poly2Proc(src)`⁻¹ · `Poly2Proc(dst)`.
    pub fn poly_to_poly2(src: [Point; 2], dst: [Point; 2]) -> Option<Matrix> {
        let poly2 = |p: [Point; 2]| Matrix {
            sx: p[1].y - p[0].y,
            ky: p[0].x - p[1].x,
            kx: p[1].x - p[0].x,
            sy: p[1].y - p[0].y,
            tx: p[0].x,
            ty: p[0].y,
        };
        let temp = poly2(src);
        let inverse = temp.invert()?;
        let temp = poly2(dst);
        Some(Matrix::concat(&temp, &inverse))
    }
    /// `mapVectors` for a single vector: no translation.
    pub fn map_vector(&self, v: Point) -> Point {
        let mut t = *self;
        t.tx = 0.0;
        t.ty = 0.0;
        t.map_point(v)
    }
    /// The nine values of `get9` (row-major), as the pipeline reads them.
    pub fn get9(&self) -> [f32; 9] {
        [
            self.sx, self.kx, self.tx, self.ky, self.sy, self.ty, 0.0, 0.0, 1.0,
        ]
    }
}

// ── Cubics: chop at several t, max curvature (for hairlines) ───────────────

pub fn chop_cubic_at_ts(src: &[Point; 4], ts: &[f32]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::new();
    if ts.is_empty() {
        out.extend_from_slice(src);
        return out;
    }
    let mut cur = *src;
    let mut i = 0usize;
    let pin = |v: f32| v.clamp(0.0, 1.0);
    while i + 1 < ts.len() {
        let (mut t0, mut t1) = (ts[i], ts[i + 1]);
        if i != 0 {
            let last = ts[i - 1];
            t0 = pin((t0 - last) / (1.0 - last));
            t1 = pin((t1 - last) / (1.0 - last));
        }
        let d = chop_cubic_at2(&cur, t0, t1);
        if out.is_empty() {
            out.extend_from_slice(&d[..7]);
        } else {
            out.extend_from_slice(&d[1..7]);
        }
        cur = [d[6], d[7], d[8], d[9]];
        i += 2;
    }
    if i < ts.len() {
        let mut t = ts[i];
        if i != 0 {
            let last = ts[i - 1];
            t = pin((t - last) / (1.0 - last));
        }
        let d = chop_cubic_at(&cur, t);
        if out.is_empty() {
            out.extend_from_slice(&d);
        } else {
            out.extend_from_slice(&d[1..]);
        }
    } else {
        out.extend_from_slice(&cur[1..]);
    }
    out
}

pub(crate) fn solve_cubic_poly(coeff: [f32; 4]) -> Vec<f32> {
    if coeff[0].abs() <= SCALAR_NEARLY_ZERO {
        let (r, n) = find_unit_quad_roots(coeff[1], coeff[2], coeff[3]);
        return r[..n].to_vec();
    }
    let inva = 1.0 / coeff[0];
    let a = coeff[1] * inva;
    let b = coeff[2] * inva;
    let c = coeff[3] * inva;
    let q = (a * a - b * 3.0) / 9.0;
    let r = (2.0 * a * a * a - 9.0 * a * b + 27.0 * c) / 54.0;
    let q3 = q * q * q;
    let r2_minus_q3 = r * r - q3;
    let adiv3 = a / 3.0;
    if r2_minus_q3 < 0.0 {
        let theta = (r / q3.sqrt()).clamp(-1.0, 1.0).acos();
        let neg2_root_q = -2.0 * q.sqrt();
        let mut t = [
            (neg2_root_q * (theta / 3.0).cos() - adiv3).clamp(0.0, 1.0),
            (neg2_root_q * ((theta + 2.0 * SCALAR_PI) / 3.0).cos() - adiv3).clamp(0.0, 1.0),
            (neg2_root_q * ((theta - 2.0 * SCALAR_PI) / 3.0).cos() - adiv3).clamp(0.0, 1.0),
        ];
        // bubble_sort
        for i in 0..3 {
            for j in 0..2 - i {
                if t[j] > t[j + 1] {
                    t.swap(j, j + 1);
                }
            }
        }
        // collaps_duplicates
        let mut v = t.to_vec();
        v.dedup();
        v
    } else {
        let mut aa = r.abs() + r2_minus_q3.sqrt();
        aa = aa.powf(0.3333333);
        if r > 0.0 {
            aa = -aa;
        }
        if aa != 0.0 {
            aa += q / aa;
        }
        vec![(aa - adiv3).clamp(0.0, 1.0)]
    }
}

pub(crate) fn formulate_f1_dot_f2(s: [f32; 4]) -> [f32; 4] {
    let a = s[1] - s[0];
    let b = s[2] - 2.0 * s[1] + s[0];
    let c = s[3] + 3.0 * (s[1] - s[2]) - s[0];
    [c * c, 3.0 * b * c, 2.0 * b * b + c * a, a * b]
}

/// `SkChopCubicAtMaxCurvature`: cubics (4 points each, sharing endpoints).
pub fn chop_cubic_at_max_curvature(src: &[Point; 4]) -> Vec<Point> {
    let cx = formulate_f1_dot_f2([src[0].x, src[1].x, src[2].x, src[3].x]);
    let cy = formulate_f1_dot_f2([src[0].y, src[1].y, src[2].y, src[3].y]);
    let coeff = [cx[0] + cy[0], cx[1] + cy[1], cx[2] + cy[2], cx[3] + cy[3]];
    let roots = solve_cubic_poly(coeff);
    let ts: Vec<f32> = roots.into_iter().filter(|t| 0.0 < *t && *t < 1.0).collect();
    chop_cubic_at_ts(src, &ts)
}
