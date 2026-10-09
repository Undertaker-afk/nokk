//! Port of `SkStroke` / `SkPathStroker` / `SkStrokerPriv` (Skia at Chrome 151):
//! stroke outline built from quads checked against the original, joins
//! miter/round/bevel, caps butt/round/square, rects on a separate path
//! (`strokeRect`). Chrome uses this for `stroke()` wider than a pixel and for
//! `strokeText` (via `SkScalerContext::internalGetPath`).

use super::geometry::{
    chop_cubic_at, find_unit_quad_roots, Conic, Matrix, Point, Rect, SCALAR_NEARLY_ZERO,
};
use super::path::{Path, PathBuilder, Verb};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Join {
    Miter,
    Round,
    Bevel,
}

/// `SkStroke` / `SkStrokeRec` for stroking: width, miter limit, caps, joins,
/// `resScale` (`SkMatrixPriv::ComputeResScaleForStroking`).
#[derive(Clone, Copy, Debug)]
pub struct StrokeParams {
    pub width: f32,
    pub miter_limit: f32,
    pub cap: Cap,
    pub join: Join,
    pub res_scale: f32,
}

/// `SkMatrixPriv::ComputeResScaleForStroking`.
pub fn res_scale_for_stroking(m: &Matrix) -> f32 {
    let len = |a: f32, b: f32| -> f32 {
        let mag2 = a * a + b * b;
        if mag2.is_finite() {
            mag2.sqrt()
        } else {
            ((a as f64 * a as f64 + b as f64 * b as f64).sqrt()) as f32
        }
    };
    let sx = len(m.sx, m.ky);
    let sy = len(m.kx, m.sy);
    if sx.is_finite() && sy.is_finite() {
        let scale = sx.max(sy);
        if scale > 0.0 {
            return scale;
        }
    }
    1.0
}

// ── point arithmetic (SkPoint) ───────────────────────────────────────────

#[inline]
fn add(a: Point, b: Point) -> Point {
    Point::new(a.x + b.x, a.y + b.y)
}
#[inline]
fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}
#[inline]
fn neg(a: Point) -> Point {
    Point::new(-a.x, -a.y)
}
#[inline]
fn mul(a: Point, s: f32) -> Point {
    Point::new(a.x * s, a.y * s)
}
#[inline]
fn dot(a: Point, b: Point) -> f32 {
    a.x * b.x + a.y * b.y
}
#[inline]
fn cross(a: Point, b: Point) -> f32 {
    a.x * b.y - a.y * b.x
}
#[inline]
fn is_zero(a: Point) -> bool {
    a.x == 0.0 && a.y == 0.0
}
#[inline]
fn can_normalize(dx: f32, dy: f32) -> bool {
    dx.is_finite() && dy.is_finite() && (dx != 0.0 || dy != 0.0)
}
#[inline]
fn degenerate_vector(v: Point) -> bool {
    !can_normalize(v.x, v.y)
}
#[inline]
fn distance_to_sqd(a: Point, b: Point) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}
#[inline]
fn length_sqd(a: Point) -> f32 {
    dot(a, a)
}
#[inline]
fn nearly_zero(x: f32) -> bool {
    x.abs() <= SCALAR_NEARLY_ZERO
}
#[inline]
fn nearly_zero_tol(x: f32, tol: f32) -> bool {
    x.abs() <= tol
}
/// `SkPointPriv::RotateCCW`.
#[inline]
fn rotate_ccw(v: Point) -> Point {
    Point::new(v.y, -v.x)
}
/// `SkPointPriv::RotateCW`.
#[inline]
fn rotate_cw(v: Point) -> Point {
    Point::new(-v.y, v.x)
}
/// `SkPoint::setLength(x, y, len)`.
#[inline]
fn set_length(x: f32, y: f32, len: f32) -> Option<Point> {
    let mut p = Point::new(x, y);
    if p.set_length(len) {
        Some(p)
    } else {
        None
    }
}

// ── curves (SkGeometry) ──────────────────────────────────────────────────

fn eval_quad_at(q: &[Point; 3], t: f32) -> Point {
    // SkQuadCoeff: C = p0, B = 2(p1−p0), A = p2 − 2p1 + p0; (A·t + B)·t + C.
    let c = q[0];
    let b = add(sub(q[1], q[0]), sub(q[1], q[0]));
    let a = add(sub(q[2], add(q[1], q[1])), q[0]);
    let ev = |a: f32, b: f32, c: f32| (a * t + b) * t + c;
    Point::new(ev(a.x, b.x, c.x), ev(a.y, b.y, c.y))
}

fn eval_quad_tangent(q: &[Point; 3], t: f32) -> Point {
    if (t == 0.0 && q[0] == q[1]) || (t == 1.0 && q[1] == q[2]) {
        return sub(q[2], q[0]);
    }
    let b = sub(q[1], q[0]);
    let a = sub(sub(q[2], q[1]), b);
    let tx = a.x * t + b.x;
    let ty = a.y * t + b.y;
    Point::new(tx + tx, ty + ty)
}

fn eval_cubic_at(c: &[Point; 4], t: f32) -> Point {
    // SkCubicCoeff: A = P3 + 3(P1−P2) − P0, B = 3(P2 − 2P1 + P0), C = 3(P1−P0), D = P0.
    let ev = |p0: f32, p1: f32, p2: f32, p3: f32| -> f32 {
        let a = p3 + 3.0 * (p1 - p2) - p0;
        let b = 3.0 * (p2 - (p1 + p1) + p0);
        let cc = 3.0 * (p1 - p0);
        ((a * t + b) * t + cc) * t + p0
    };
    Point::new(
        ev(c[0].x, c[1].x, c[2].x, c[3].x),
        ev(c[0].y, c[1].y, c[2].y, c[3].y),
    )
}

fn eval_cubic_derivative(c: &[Point; 4], t: f32) -> Point {
    let ev = |p0: f32, p1: f32, p2: f32, p3: f32| -> f32 {
        let a = p3 + 3.0 * (p1 - p2) - p0;
        let bb = p2 - (p1 + p1) + p0;
        let b = bb + bb;
        let cc = p1 - p0;
        (a * t + b) * t + cc
    };
    Point::new(
        ev(c[0].x, c[1].x, c[2].x, c[3].x),
        ev(c[0].y, c[1].y, c[2].y, c[3].y),
    )
}

/// `SkEvalCubicAt(src, t, loc, tangent, nullptr)`.
fn eval_cubic_tangent(c: &[Point; 4], t: f32) -> Point {
    if (t == 0.0 && c[0] == c[1]) || (t == 1.0 && c[2] == c[3]) {
        let mut tangent = if t == 0.0 {
            sub(c[2], c[0])
        } else {
            sub(c[3], c[1])
        };
        if tangent.x == 0.0 && tangent.y == 0.0 {
            tangent = sub(c[3], c[0]);
        }
        tangent
    } else {
        eval_cubic_derivative(c, t)
    }
}

fn conic_eval_at(conic: &Conic, t: f32) -> Point {
    let [p0, p1, p2] = conic.pts;
    let w = conic.w;
    let p1w = mul(p1, w);
    // numer: C = p0, A = p2 − 2·p1w + p0, B = 2(p1w − p0)
    let nc = p0;
    let na = add(sub(p2, add(p1w, p1w)), p0);
    let nb = add(sub(p1w, p0), sub(p1w, p0));
    // denom: C = 1, B = 2(w − 1), A = 0 − B
    let dc = 1.0f32;
    let db = (w - dc) + (w - dc);
    let da = 0.0 - db;
    let ev = |a: f32, b: f32, c: f32| (a * t + b) * t + c;
    let nx = ev(na.x, nb.x, nc.x);
    let ny = ev(na.y, nb.y, nc.y);
    let d = ev(da, db, dc);
    Point::new(nx / d, ny / d)
}

fn conic_eval_tangent(conic: &Conic, t: f32) -> Point {
    let [p0, p1, p2] = conic.pts;
    if (t == 0.0 && p0 == p1) || (t == 1.0 && p1 == p2) {
        return sub(p2, p0);
    }
    let w = conic.w;
    let p20 = sub(p2, p0);
    let p10 = sub(p1, p0);
    let c = mul(p10, w);
    let a = sub(mul(p20, w), p20);
    let b = sub(sub(p20, c), c);
    let ev = |a: f32, b: f32, c: f32| (a * t + b) * t + c;
    Point::new(ev(a.x, b.x, c.x), ev(a.y, b.y, c.y))
}

/// `SkFindQuadMaxCurvature`.
fn find_quad_max_curvature(src: &[Point; 3]) -> f32 {
    let ax = src[1].x - src[0].x;
    let ay = src[1].y - src[0].y;
    let bx = src[0].x - src[1].x - src[1].x + src[2].x;
    let by = src[0].y - src[1].y - src[1].y + src[2].y;
    let mut numer = -(ax * bx + ay * by);
    let mut denom = bx * bx + by * by;
    if denom < 0.0 {
        numer = -numer;
        denom = -denom;
    }
    if numer <= 0.0 {
        return 0.0;
    }
    if numer >= denom {
        return 1.0;
    }
    numer / denom
}

/// `SkFindCubicMaxCurvature`.
fn find_cubic_max_curvature(src: &[Point; 4]) -> Vec<f32> {
    let cx = super::geometry::formulate_f1_dot_f2([src[0].x, src[1].x, src[2].x, src[3].x]);
    let cy = super::geometry::formulate_f1_dot_f2([src[0].y, src[1].y, src[2].y, src[3].y]);
    let coeff = [cx[0] + cy[0], cx[1] + cy[1], cx[2] + cy[2], cx[3] + cy[3]];
    super::geometry::solve_cubic_poly(coeff)
}

/// `SkFindCubicInflections`.
fn find_cubic_inflections(src: &[Point; 4]) -> Vec<f32> {
    let ax = src[1].x - src[0].x;
    let ay = src[1].y - src[0].y;
    let bx = src[2].x - 2.0 * src[1].x + src[0].x;
    let by = src[2].y - 2.0 * src[1].y + src[0].y;
    let cx = src[3].x + 3.0 * (src[1].x - src[2].x) - src[0].x;
    let cy = src[3].y + 3.0 * (src[1].y - src[2].y) - src[0].y;
    let (r, n) = find_unit_quad_roots(bx * cy - by * cx, ax * cy - ay * cx, ax * by - ay * bx);
    r[..n].to_vec()
}

fn on_same_side(src: &[Point; 4], test_index: usize, line_index: usize) -> bool {
    let origin = src[line_index];
    let line = sub(src[line_index + 1], origin);
    let mut crosses = [0.0f32; 2];
    for (index, c) in crosses.iter_mut().enumerate() {
        let test_line = sub(src[test_index + index], origin);
        *c = cross(line, test_line);
    }
    crosses[0] * crosses[1] >= 0.0
}

fn calc_cubic_precision(src: &[Point; 4]) -> f32 {
    (distance_to_sqd(src[1], src[0])
        + distance_to_sqd(src[2], src[1])
        + distance_to_sqd(src[3], src[2]))
        * 1e-8
}

/// `SkFindCubicCusp`.
fn find_cubic_cusp(src: &[Point; 4]) -> f32 {
    if src[0] == src[1] || src[2] == src[3] {
        return -1.0;
    }
    if on_same_side(src, 0, 2) || on_same_side(src, 2, 0) {
        return -1.0;
    }
    for t in find_cubic_max_curvature(src) {
        if 0.0 >= t || t >= 1.0 {
            continue;
        }
        let d = eval_cubic_derivative(src, t);
        let mag = length_sqd(d);
        let precision = calc_cubic_precision(src);
        if mag < precision {
            return t;
        }
    }
    -1.0
}

// ── SkQuadConstruct ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
struct QuadConstruct {
    quad: [Point; 3],
    tangent_start: Point,
    tangent_end: Point,
    start_t: f32,
    mid_t: f32,
    end_t: f32,
    start_set: bool,
    end_set: bool,
    opposite_tangents: bool,
}

impl QuadConstruct {
    fn init(&mut self, start: f32, end: f32) -> bool {
        self.start_t = start;
        self.mid_t = (start + end) * 0.5;
        self.end_t = end;
        self.start_set = false;
        self.end_set = false;
        self.start_t < self.mid_t && self.mid_t < self.end_t
    }
    fn init_with_start(&mut self, parent: &QuadConstruct) -> bool {
        if !self.init(parent.start_t, parent.mid_t) {
            return false;
        }
        self.quad[0] = parent.quad[0];
        self.tangent_start = parent.tangent_start;
        self.start_set = true;
        true
    }
    fn init_with_end(&mut self, parent: &QuadConstruct) -> bool {
        if !self.init(parent.mid_t, parent.end_t) {
            return false;
        }
        self.quad[2] = parent.quad[2];
        self.tangent_end = parent.tangent_end;
        self.end_set = true;
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultType {
    Split,
    Degenerate,
    Quad,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ReductionType {
    Point,
    Line,
    Quad,
    Degenerate,
    Degenerate2,
    Degenerate3,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StrokeType {
    Outer,
    Inner,
}

const RECURSIVE_LIMITS: [i32; 4] = [5 * 3, 24, 11 * 3, 11 * 3];
const TANGENT_LIMIT: usize = 0;
const CUBIC_LIMIT: usize = 1;
const CONIC_LIMIT: usize = 2;
const QUAD_LIMIT: usize = 3;

// ── SkPath::Iter (forceClose = false) ─────────────────────────────────────

#[derive(Clone, Copy)]
enum Seg {
    Move(Point),
    Line([Point; 2]),
    Quad([Point; 3]),
    Conic([Point; 3], f32),
    Cubic([Point; 4]),
    Close,
}

#[derive(Clone)]
struct PathIter<'a> {
    path: &'a Path,
    vi: usize,
    pi: usize,
    ci: usize,
    move_to: Point,
    last_pt: Point,
    need_close: bool,
}

impl<'a> PathIter<'a> {
    fn new(path: &'a Path) -> Self {
        PathIter {
            path,
            vi: 0,
            pi: 0,
            ci: 0,
            move_to: Point::default(),
            last_pt: Point::default(),
            need_close: false,
        }
    }
    /// `autoClose`: line to the contour start, or the Close itself.
    fn auto_close(&mut self) -> Seg {
        if self.last_pt != self.move_to {
            if self.last_pt.x.is_nan()
                || self.last_pt.y.is_nan()
                || self.move_to.x.is_nan()
                || self.move_to.y.is_nan()
            {
                return Seg::Close;
            }
            let seg = Seg::Line([self.last_pt, self.move_to]);
            self.last_pt = self.move_to;
            return seg;
        }
        Seg::Close
    }
    fn next(&mut self) -> Option<Seg> {
        let verbs = &self.path.verbs;
        let pts = &self.path.pts;
        if self.vi >= verbs.len() {
            if self.need_close {
                let seg = self.auto_close();
                if matches!(seg, Seg::Line(_)) {
                    return Some(seg);
                }
                self.need_close = false;
                return Some(Seg::Close);
            }
            return None;
        }
        let verb = verbs[self.vi];
        self.vi += 1;
        match verb {
            Verb::Move => {
                if self.need_close {
                    self.vi -= 1;
                    let seg = self.auto_close();
                    if matches!(seg, Seg::Close) {
                        self.need_close = false;
                    }
                    return Some(seg);
                }
                if self.vi >= verbs.len() {
                    return None;
                }
                self.move_to = pts[self.pi];
                self.pi += 1;
                self.last_pt = self.move_to;
                Some(Seg::Move(self.move_to))
            }
            Verb::Line => {
                let p = pts[self.pi];
                self.pi += 1;
                let seg = Seg::Line([self.last_pt, p]);
                self.last_pt = p;
                Some(seg)
            }
            Verb::Quad => {
                let (a, b) = (pts[self.pi], pts[self.pi + 1]);
                self.pi += 2;
                let seg = Seg::Quad([self.last_pt, a, b]);
                self.last_pt = b;
                Some(seg)
            }
            Verb::Conic => {
                let (a, b) = (pts[self.pi], pts[self.pi + 1]);
                let w = self.path.conics.get(self.ci).copied().unwrap_or(1.0);
                self.ci += 1;
                self.pi += 2;
                let seg = Seg::Conic([self.last_pt, a, b], w);
                self.last_pt = b;
                Some(seg)
            }
            Verb::Cubic => {
                let (a, b, c) = (pts[self.pi], pts[self.pi + 1], pts[self.pi + 2]);
                self.pi += 3;
                let seg = Seg::Cubic([self.last_pt, a, b, c]);
                self.last_pt = c;
                Some(seg)
            }
            Verb::Close => {
                let seg = self.auto_close();
                if matches!(seg, Seg::Line(_)) {
                    self.vi -= 1;
                } else {
                    self.need_close = false;
                }
                self.last_pt = self.move_to;
                Some(seg)
            }
        }
    }
}

fn has_valid_tangent(iter: &PathIter) -> bool {
    let mut copy = iter.clone();
    while let Some(seg) = copy.next() {
        match seg {
            Seg::Move(_) => return false,
            Seg::Line(p) => {
                if p[0] == p[1] {
                    continue;
                }
                return true;
            }
            Seg::Quad(p) | Seg::Conic(p, _) => {
                if p[0] == p[1] && p[0] == p[2] {
                    continue;
                }
                return true;
            }
            Seg::Cubic(p) => {
                if p[0] == p[1] && p[0] == p[2] && p[0] == p[3] {
                    continue;
                }
                return true;
            }
            Seg::Close => return false,
        }
    }
    false
}

// ── SkStrokerPriv: caps and joins ────────────────────────────────────────

fn set_last_point(b: &mut PathBuilder, p: Point) {
    if let Some(l) = b.pts.last_mut() {
        *l = p;
    }
}

fn butt_capper(sink: &mut PathBuilder, _pivot: Point, _normal: Point, stop: Point, _extend: bool) {
    sink.line_to(stop);
}

fn round_capper(sink: &mut PathBuilder, pivot: Point, normal: Point, stop: Point, _extend: bool) {
    let parallel = rotate_cw(normal);
    let projected_center = add(pivot, parallel);
    let root2 = std::f32::consts::FRAC_1_SQRT_2;
    sink.conic_to(add(projected_center, normal), projected_center, root2);
    sink.conic_to(sub(projected_center, normal), stop, root2);
}

fn square_capper(
    sink: &mut PathBuilder,
    pivot: Point,
    normal: Point,
    stop: Point,
    extend_last_pt: bool,
) {
    let parallel = rotate_cw(normal);
    if extend_last_pt {
        set_last_point(sink, add(add(pivot, normal), parallel));
        sink.line_to(add(sub(pivot, normal), parallel));
    } else {
        sink.line_to(add(add(pivot, normal), parallel));
        sink.line_to(add(sub(pivot, normal), parallel));
        sink.line_to(stop);
    }
}

fn cap(cap: Cap, sink: &mut PathBuilder, pivot: Point, normal: Point, stop: Point, extend: bool) {
    match cap {
        Cap::Butt => butt_capper(sink, pivot, normal, stop, extend),
        Cap::Round => round_capper(sink, pivot, normal, stop, extend),
        Cap::Square => square_capper(sink, pivot, normal, stop, extend),
    }
}

fn is_clockwise(before: Point, after: Point) -> bool {
    before.x * after.y > before.y * after.x
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AngleType {
    Nearly180,
    Sharp,
    Shallow,
    NearlyLine,
}

fn dot2angle(dot: f32) -> AngleType {
    if dot >= 0.0 {
        if nearly_zero(1.0 - dot) {
            AngleType::NearlyLine
        } else {
            AngleType::Shallow
        }
    } else if nearly_zero(1.0 + dot) {
        AngleType::Nearly180
    } else {
        AngleType::Sharp
    }
}

fn handle_inner_join(inner: &mut PathBuilder, pivot: Point, after: Point) {
    inner.line_to(pivot);
    inner.line_to(Point::new(pivot.x - after.x, pivot.y - after.y));
}

#[allow(clippy::too_many_arguments)]
fn blunt_joiner(
    outer: &mut PathBuilder,
    inner: &mut PathBuilder,
    before_unit: Point,
    pivot: Point,
    after_unit: Point,
    radius: f32,
    _inv_miter: f32,
    _prev_is_line: bool,
    _curr_is_line: bool,
) {
    let mut after = mul(after_unit, radius);
    let (o, i): (&mut PathBuilder, &mut PathBuilder) = if is_clockwise(before_unit, after_unit) {
        (outer, inner)
    } else {
        after = neg(after);
        (inner, outer)
    };
    o.line_to(Point::new(pivot.x + after.x, pivot.y + after.y));
    handle_inner_join(i, pivot, after);
}

#[allow(clippy::too_many_arguments)]
fn round_joiner(
    outer: &mut PathBuilder,
    inner: &mut PathBuilder,
    before_unit: Point,
    pivot: Point,
    after_unit: Point,
    radius: f32,
    _inv_miter: f32,
    _prev_is_line: bool,
    _curr_is_line: bool,
) {
    let dot_prod = dot(before_unit, after_unit);
    if dot2angle(dot_prod) == AngleType::NearlyLine {
        return;
    }
    let mut before = before_unit;
    let mut after = after_unit;
    let mut ccw = false;
    let (o, i): (&mut PathBuilder, &mut PathBuilder) = if is_clockwise(before, after) {
        (outer, inner)
    } else {
        before = neg(before);
        after = neg(after);
        ccw = true;
        (inner, outer)
    };
    let mut matrix = Matrix::scale(radius, radius);
    matrix.post_translate(pivot.x, pivot.y);
    let conics = Conic::build_unit_arc(before, after, ccw, Some(&matrix));
    if !conics.is_empty() {
        for c in &conics {
            o.conic_to(c.pts[1], c.pts[2], c.w);
        }
        let after_r = mul(after, radius);
        handle_inner_join(i, pivot, after_r);
    }
}

const ONE_OVER_SQRT2: f32 = std::f32::consts::FRAC_1_SQRT_2;

#[allow(clippy::too_many_arguments)]
fn miter_joiner(
    outer: &mut PathBuilder,
    inner: &mut PathBuilder,
    before_unit: Point,
    pivot: Point,
    after_unit: Point,
    radius: f32,
    inv_miter_limit: f32,
    prev_is_line: bool,
    curr_is_line: bool,
) {
    let dot_prod = dot(before_unit, after_unit);
    let angle_type = dot2angle(dot_prod);
    let mut before = before_unit;
    let mut after = after_unit;
    let mut curr_is_line = curr_is_line;
    if angle_type == AngleType::NearlyLine {
        return;
    }
    // Mirrors the original's goto branches: DO_MITER → DO_BLUNT.
    let mut do_miter: Option<Point> = None;
    let (o, i): (&mut PathBuilder, &mut PathBuilder);
    if angle_type == AngleType::Nearly180 {
        curr_is_line = false;
        o = outer;
        i = inner;
    } else {
        let ccw = !is_clockwise(before, after);
        if ccw {
            before = neg(before);
            after = neg(after);
            o = inner;
            i = outer;
        } else {
            o = outer;
            i = inner;
        }
        if dot_prod == 0.0 && inv_miter_limit <= ONE_OVER_SQRT2 {
            do_miter = Some(mul(add(before, after), radius));
        } else {
            let sin_half_angle = ((1.0 + dot_prod) / 2.0).sqrt();
            if sin_half_angle < inv_miter_limit {
                curr_is_line = false;
            } else {
                let mut mid = if angle_type == AngleType::Sharp {
                    let mut m = Point::new(after.y - before.y, before.x - after.x);
                    if ccw {
                        m = neg(m);
                    }
                    m
                } else {
                    add(before, after)
                };
                mid.set_length(radius / sin_half_angle);
                do_miter = Some(mid);
            }
        }
    }
    if let Some(mid) = do_miter {
        if prev_is_line {
            set_last_point(o, add(pivot, mid));
        } else {
            o.line_to(add(pivot, mid));
        }
    }
    let after_r = mul(after, radius);
    if !curr_is_line {
        o.line_to(Point::new(pivot.x + after_r.x, pivot.y + after_r.y));
    }
    handle_inner_join(i, pivot, after_r);
}

#[allow(clippy::too_many_arguments)]
fn join(
    join: Join,
    outer: &mut PathBuilder,
    inner: &mut PathBuilder,
    before_unit: Point,
    pivot: Point,
    after_unit: Point,
    radius: f32,
    inv_miter: f32,
    prev_is_line: bool,
    curr_is_line: bool,
) {
    match join {
        Join::Miter => miter_joiner(
            outer,
            inner,
            before_unit,
            pivot,
            after_unit,
            radius,
            inv_miter,
            prev_is_line,
            curr_is_line,
        ),
        Join::Round => round_joiner(
            outer,
            inner,
            before_unit,
            pivot,
            after_unit,
            radius,
            inv_miter,
            prev_is_line,
            curr_is_line,
        ),
        Join::Bevel => blunt_joiner(
            outer,
            inner,
            before_unit,
            pivot,
            after_unit,
            radius,
            inv_miter,
            prev_is_line,
            curr_is_line,
        ),
    }
}

// ── SkPathStroker ─────────────────────────────────────────────────────────

struct PathStroker {
    radius: f32,
    inv_miter_limit: f32,
    res_scale: f32,
    inv_res_scale: f32,
    inv_res_scale_squared: f32,
    first_normal: Point,
    prev_normal: Point,
    first_unit_normal: Point,
    prev_unit_normal: Point,
    first_pt: Point,
    prev_pt: Point,
    first_outer_pt: Point,
    first_outer_pt_index_in_contour: usize,
    segment_count: i32,
    prev_is_line: bool,
    can_ignore_center: bool,
    capper: Cap,
    joiner: Join,
    inner: PathBuilder,
    outer: PathBuilder,
    cusper: PathBuilder,
    stroke_type: StrokeType,
    recursion_depth: i32,
    found_tangents: bool,
    join_completed: bool,
}

fn set_normal_unitnormal(
    before: Point,
    after: Point,
    scale: f32,
    radius: f32,
) -> Option<(Point, Point)> {
    let unit = set_length(
        (after.x - before.x) * scale,
        (after.y - before.y) * scale,
        1.0,
    )?;
    let unit = rotate_ccw(unit);
    Some((mul(unit, radius), unit))
}

fn set_normal_unitnormal_vec(vec: Point, radius: f32) -> Option<(Point, Point)> {
    let unit = set_length(vec.x, vec.y, 1.0)?;
    let unit = rotate_ccw(unit);
    Some((mul(unit, radius), unit))
}

fn is_zero_length_since_point(pts: &[Point], start: usize) -> bool {
    if pts.len() < start + 2 {
        return true;
    }
    let first = pts[start];
    pts[start + 1..].iter().all(|p| *p == first)
}

fn pt_to_line(pt: Point, line_start: Point, line_end: Point) -> f32 {
    let dxy = sub(line_end, line_start);
    let ab0 = sub(pt, line_start);
    let numer = dot(dxy, ab0);
    let denom = dot(dxy, dxy);
    let t = numer / denom;
    if t >= 0.0 && t <= 1.0 {
        let hit = add(mul(line_start, 1.0 - t), mul(line_end, t));
        distance_to_sqd(hit, pt)
    } else {
        distance_to_sqd(pt, line_start)
    }
}

fn pt_to_tangent_line(pt: Point, line_start: Point, tangent: Point) -> f32 {
    let dxy = tangent;
    let ab0 = sub(pt, line_start);
    let numer = dot(dxy, ab0);
    let denom = dot(dxy, dxy);
    let t = numer / denom;
    if t >= 0.0 && t <= 1.0 {
        let hit = add(line_start, mul(tangent, t));
        distance_to_sqd(hit, pt)
    } else {
        distance_to_sqd(pt, line_start)
    }
}

fn cubic_in_line(cubic: &[Point; 4]) -> bool {
    let mut pt_max = -1.0f32;
    let mut outer1 = 0usize;
    let mut outer2 = 0usize;
    for index in 0..3 {
        for inner in index + 1..4 {
            let d = sub(cubic[inner], cubic[index]);
            let test_max = d.x.abs().max(d.y.abs());
            if pt_max < test_max {
                outer1 = index;
                outer2 = inner;
                pt_max = test_max;
            }
        }
    }
    let mid1 = (1 + (2 >> outer2)) >> outer1;
    let mid2 = outer1 ^ outer2 ^ mid1;
    let line_slop = pt_max * pt_max * 0.00001;
    pt_to_line(cubic[mid1], cubic[outer1], cubic[outer2]) <= line_slop
        && pt_to_line(cubic[mid2], cubic[outer1], cubic[outer2]) <= line_slop
}

fn quad_in_line(quad: &[Point; 3]) -> bool {
    let mut pt_max = -1.0f32;
    let mut outer1 = 0usize;
    let mut outer2 = 0usize;
    for index in 0..2 {
        for inner in index + 1..3 {
            let d = sub(quad[inner], quad[index]);
            let test_max = d.x.abs().max(d.y.abs());
            if pt_max < test_max {
                outer1 = index;
                outer2 = inner;
                pt_max = test_max;
            }
        }
    }
    let mid = outer1 ^ outer2 ^ 3;
    let line_slop = pt_max * pt_max * 0.000005;
    pt_to_line(quad[mid], quad[outer1], quad[outer2]) <= line_slop
}

fn check_cubic_linear(cubic: &[Point; 4]) -> (ReductionType, [Point; 3], usize) {
    let degenerate_ab = degenerate_vector(sub(cubic[1], cubic[0]));
    let degenerate_bc = degenerate_vector(sub(cubic[2], cubic[1]));
    let degenerate_cd = degenerate_vector(sub(cubic[3], cubic[2]));
    let mut reduction = [Point::default(); 3];
    if degenerate_ab && degenerate_bc && degenerate_cd {
        return (ReductionType::Point, reduction, 1);
    }
    if degenerate_ab as u8 + degenerate_bc as u8 + degenerate_cd as u8 == 2 {
        return (ReductionType::Line, reduction, 1);
    }
    if !cubic_in_line(cubic) {
        return (
            ReductionType::Quad,
            reduction,
            if degenerate_ab { 2 } else { 1 },
        );
    }
    let mut r_count = 0usize;
    for t in find_cubic_max_curvature(cubic) {
        if 0.0 >= t || t >= 1.0 {
            continue;
        }
        let p = eval_cubic_at(cubic, t);
        if p != cubic[0] && p != cubic[3] {
            reduction[r_count] = p;
            r_count += 1;
        }
    }
    if r_count == 0 {
        return (ReductionType::Line, reduction, 1);
    }
    let kind = match r_count {
        1 => ReductionType::Degenerate,
        2 => ReductionType::Degenerate2,
        _ => ReductionType::Degenerate3,
    };
    (kind, reduction, 1)
}

fn check_conic_linear(conic: &Conic) -> (ReductionType, Point) {
    let degenerate_ab = degenerate_vector(sub(conic.pts[1], conic.pts[0]));
    let degenerate_bc = degenerate_vector(sub(conic.pts[2], conic.pts[1]));
    if degenerate_ab && degenerate_bc {
        return (ReductionType::Point, Point::default());
    }
    if degenerate_ab || degenerate_bc {
        return (ReductionType::Line, Point::default());
    }
    if !quad_in_line(&conic.pts) {
        return (ReductionType::Quad, Point::default());
    }
    let t = find_quad_max_curvature(&conic.pts);
    if t == 0.0 || t.is_nan() {
        return (ReductionType::Line, Point::default());
    }
    (ReductionType::Degenerate, conic_eval_at(conic, t))
}

fn check_quad_linear(quad: &[Point; 3]) -> (ReductionType, Point) {
    let degenerate_ab = degenerate_vector(sub(quad[1], quad[0]));
    let degenerate_bc = degenerate_vector(sub(quad[2], quad[1]));
    if degenerate_ab && degenerate_bc {
        return (ReductionType::Point, Point::default());
    }
    if degenerate_ab || degenerate_bc {
        return (ReductionType::Line, Point::default());
    }
    if !quad_in_line(quad) {
        return (ReductionType::Quad, Point::default());
    }
    let t = find_quad_max_curvature(quad);
    if t == 0.0 || t == 1.0 {
        return (ReductionType::Line, Point::default());
    }
    (ReductionType::Degenerate, eval_quad_at(quad, t))
}

fn intersect_quad_ray(line: &[Point; 2], quad: &[Point; 3]) -> ([f32; 2], usize) {
    let vec = sub(line[1], line[0]);
    let mut r = [0.0f32; 3];
    for n in 0..3 {
        r[n] = cross(vec, sub(quad[n], line[0]));
    }
    let mut a = r[2];
    let mut b = r[1];
    let c = r[0];
    a += c - 2.0 * b;
    b -= c;
    find_unit_quad_roots(a, 2.0 * b, c)
}

fn points_within_dist(near: Point, far: Point, limit: f32) -> bool {
    distance_to_sqd(near, far) <= limit * limit
}

fn sharp_angle(quad: &[Point; 3]) -> bool {
    let mut smaller = sub(quad[1], quad[0]);
    let mut larger = sub(quad[1], quad[2]);
    let smaller_len = length_sqd(smaller);
    let mut larger_len = length_sqd(larger);
    if smaller_len > larger_len {
        std::mem::swap(&mut smaller, &mut larger);
        larger_len = smaller_len;
    }
    if !smaller.set_length(larger_len) {
        return false;
    }
    dot(smaller, larger) > 0.0
}

impl PathStroker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        radius: f32,
        miter_limit: f32,
        cap: Cap,
        join: Join,
        res_scale: f32,
        can_ignore_center: bool,
    ) -> PathStroker {
        let mut join = join;
        let mut inv_miter_limit = 0.0;
        if join == Join::Miter {
            if miter_limit <= 1.0 {
                join = Join::Bevel;
            } else {
                inv_miter_limit = 1.0 / miter_limit;
            }
        }
        let inv_res_scale = 1.0 / (res_scale * 4.0);
        PathStroker {
            radius,
            inv_miter_limit,
            res_scale,
            inv_res_scale,
            inv_res_scale_squared: inv_res_scale * inv_res_scale,
            first_normal: Point::default(),
            prev_normal: Point::default(),
            first_unit_normal: Point::default(),
            prev_unit_normal: Point::default(),
            first_pt: Point::default(),
            prev_pt: Point::default(),
            first_outer_pt: Point::default(),
            first_outer_pt_index_in_contour: 0,
            segment_count: -1,
            prev_is_line: false,
            can_ignore_center,
            capper: cap,
            joiner: join,
            inner: PathBuilder::new(),
            outer: PathBuilder::new(),
            cusper: PathBuilder::new(),
            stroke_type: StrokeType::Outer,
            recursion_depth: 0,
            found_tangents: false,
            join_completed: false,
        }
    }

    fn has_only_move_to(&self) -> bool {
        self.segment_count == 0
    }
    fn move_to_pt(&self) -> Point {
        self.first_pt
    }
    fn is_current_contour_empty(&self) -> bool {
        is_zero_length_since_point(&self.inner.pts, 0)
            && is_zero_length_since_point(&self.outer.pts, self.first_outer_pt_index_in_contour)
    }

    fn pre_join_to(&mut self, curr_pt: Point, curr_is_line: bool) -> Option<(Point, Point)> {
        let (normal, unit_normal) =
            match set_normal_unitnormal(self.prev_pt, curr_pt, self.res_scale, self.radius) {
                Some(v) => v,
                None => {
                    if self.capper == Cap::Butt {
                        return None;
                    }
                    (Point::new(self.radius, 0.0), Point::new(1.0, 0.0))
                }
            };
        if self.segment_count == 0 {
            self.first_normal = normal;
            self.first_unit_normal = unit_normal;
            self.first_outer_pt = add(self.prev_pt, normal);
            self.outer.move_to(self.first_outer_pt);
            self.inner.move_to(sub(self.prev_pt, normal));
        } else {
            join(
                self.joiner,
                &mut self.outer,
                &mut self.inner,
                self.prev_unit_normal,
                self.prev_pt,
                unit_normal,
                self.radius,
                self.inv_miter_limit,
                self.prev_is_line,
                curr_is_line,
            );
        }
        self.prev_is_line = curr_is_line;
        Some((normal, unit_normal))
    }

    fn post_join_to(&mut self, curr_pt: Point, normal: Point, unit_normal: Point) {
        self.join_completed = true;
        self.prev_pt = curr_pt;
        self.prev_unit_normal = unit_normal;
        self.prev_normal = normal;
        self.segment_count += 1;
    }

    fn finish_contour(&mut self, close: bool, curr_is_line: bool) {
        if self.segment_count > 0 {
            if close {
                join(
                    self.joiner,
                    &mut self.outer,
                    &mut self.inner,
                    self.prev_unit_normal,
                    self.prev_pt,
                    self.first_unit_normal,
                    self.radius,
                    self.inv_miter_limit,
                    self.prev_is_line,
                    curr_is_line,
                );
                self.outer.close();
                if self.can_ignore_center {
                    if rect_contains(&self.inner.bounds(), &self.outer.bounds()) {
                        self.outer = std::mem::take(&mut self.inner);
                    }
                } else if let Some(pt) = self.inner.last_pt() {
                    self.outer.move_to(pt);
                    let inner = std::mem::take(&mut self.inner).detach();
                    reverse_path_to(&mut self.outer, &inner);
                    self.outer.close();
                }
            } else if let Some(pt) = self.inner.last_pt() {
                cap(
                    self.capper,
                    &mut self.outer,
                    self.prev_pt,
                    self.prev_normal,
                    pt,
                    curr_is_line,
                );
                let inner = std::mem::take(&mut self.inner).detach();
                reverse_path_to(&mut self.outer, &inner);
                cap(
                    self.capper,
                    &mut self.outer,
                    self.first_pt,
                    neg(self.first_normal),
                    self.first_outer_pt,
                    self.prev_is_line,
                );
                self.outer.close();
            }
            if !self.cusper.is_empty() {
                let c = std::mem::take(&mut self.cusper).detach();
                append_path(&mut self.outer, &c);
            }
        }
        self.inner.reset();
        self.segment_count = -1;
        self.first_outer_pt_index_in_contour = self.outer.pts.len();
    }

    fn move_to(&mut self, pt: Point) {
        if self.segment_count > 0 {
            self.finish_contour(false, false);
        }
        self.segment_count = 0;
        self.first_pt = pt;
        self.prev_pt = pt;
        self.join_completed = false;
    }

    fn line_to_raw(&mut self, curr_pt: Point, normal: Point) {
        self.outer.line_to(add(curr_pt, normal));
        self.inner.line_to(sub(curr_pt, normal));
    }

    fn line_to(&mut self, curr_pt: Point, iter: Option<&PathIter>) {
        let tol = SCALAR_NEARLY_ZERO * self.inv_res_scale;
        let teeny_line = nearly_zero_tol(self.prev_pt.x - curr_pt.x, tol)
            && nearly_zero_tol(self.prev_pt.y - curr_pt.y, tol);
        if self.capper == Cap::Butt && teeny_line {
            return;
        }
        if teeny_line && (self.join_completed || iter.is_some_and(has_valid_tangent)) {
            return;
        }
        let Some((normal, unit_normal)) = self.pre_join_to(curr_pt, true) else {
            return;
        };
        self.line_to_raw(curr_pt, normal);
        self.post_join_to(curr_pt, normal, unit_normal);
    }

    fn set_quad_end_normal(
        &self,
        quad: &[Point; 3],
        normal_ab: Point,
        unit_ab: Point,
    ) -> (Point, Point) {
        set_normal_unitnormal(quad[1], quad[2], self.res_scale, self.radius)
            .unwrap_or((normal_ab, unit_ab))
    }

    fn set_cubic_end_normal(
        &self,
        cubic: &[Point; 4],
        normal_ab: Point,
        unit_ab: Point,
    ) -> (Point, Point) {
        let mut ab = sub(cubic[1], cubic[0]);
        let mut cd = sub(cubic[3], cubic[2]);
        let mut degenerate_ab = degenerate_vector(ab);
        let mut degenerate_cd = degenerate_vector(cd);
        if degenerate_ab && degenerate_cd {
            return (normal_ab, unit_ab);
        }
        if degenerate_ab {
            ab = sub(cubic[2], cubic[0]);
            degenerate_ab = degenerate_vector(ab);
        }
        if degenerate_cd {
            cd = sub(cubic[3], cubic[1]);
            degenerate_cd = degenerate_vector(cd);
        }
        if degenerate_ab || degenerate_cd {
            return (normal_ab, unit_ab);
        }
        set_normal_unitnormal_vec(cd, self.radius).unwrap_or((normal_ab, unit_ab))
    }

    fn init(
        &mut self,
        stroke_type: StrokeType,
        quad_pts: &mut QuadConstruct,
        t_start: f32,
        t_end: f32,
    ) {
        self.stroke_type = stroke_type;
        self.found_tangents = false;
        quad_pts.init(t_start, t_end);
    }

    fn set_ray_pts(&self, t_pt: Point, dxy: Point) -> (Point, Point) {
        let mut d = dxy;
        if !d.set_length(self.radius) {
            d = Point::new(self.radius, 0.0);
        }
        let axis_flip = if self.stroke_type == StrokeType::Outer {
            1.0f32
        } else {
            -1.0
        };
        let on_pt = Point::new(t_pt.x + axis_flip * d.y, t_pt.y - axis_flip * d.x);
        (on_pt, d)
    }

    fn conic_perp_ray(&self, conic: &Conic, t: f32) -> (Point, Point, Point) {
        let t_pt = conic_eval_at(conic, t);
        let mut dxy = conic_eval_tangent(conic, t);
        if is_zero(dxy) {
            dxy = sub(conic.pts[2], conic.pts[0]);
        }
        let (on_pt, tangent) = self.set_ray_pts(t_pt, dxy);
        (t_pt, on_pt, tangent)
    }

    fn conic_quad_ends(&self, conic: &Conic, q: &mut QuadConstruct) {
        if !q.start_set {
            let (_, on, tan) = self.conic_perp_ray(conic, q.start_t);
            q.quad[0] = on;
            q.tangent_start = tan;
            q.start_set = true;
        }
        if !q.end_set {
            let (_, on, tan) = self.conic_perp_ray(conic, q.end_t);
            q.quad[2] = on;
            q.tangent_end = tan;
            q.end_set = true;
        }
    }

    fn cubic_perp_ray(&self, cubic: &[Point; 4], t: f32) -> (Point, Point, Point) {
        let t_pt = eval_cubic_at(cubic, t);
        let mut dxy = eval_cubic_tangent(cubic, t);
        if is_zero(dxy) {
            let mut c_pts: [Point; 4] = *cubic;
            if nearly_zero(t) {
                dxy = sub(cubic[2], cubic[0]);
            } else if nearly_zero(1.0 - t) {
                dxy = sub(cubic[3], cubic[1]);
            } else {
                let chopped = chop_cubic_at(cubic, t);
                dxy = sub(chopped[3], chopped[2]);
                if is_zero(dxy) {
                    dxy = sub(chopped[3], chopped[1]);
                    c_pts = [chopped[0], chopped[1], chopped[2], chopped[3]];
                }
            }
            if is_zero(dxy) {
                dxy = sub(c_pts[3], c_pts[0]);
            }
        }
        let (on_pt, tangent) = self.set_ray_pts(t_pt, dxy);
        (t_pt, on_pt, tangent)
    }

    fn cubic_quad_ends(&self, cubic: &[Point; 4], q: &mut QuadConstruct) {
        if !q.start_set {
            let (_, on, tan) = self.cubic_perp_ray(cubic, q.start_t);
            q.quad[0] = on;
            q.tangent_start = tan;
            q.start_set = true;
        }
        if !q.end_set {
            let (_, on, tan) = self.cubic_perp_ray(cubic, q.end_t);
            q.quad[2] = on;
            q.tangent_end = tan;
            q.end_set = true;
        }
    }

    fn cubic_quad_mid(&self, cubic: &[Point; 4], q: &QuadConstruct) -> Point {
        self.cubic_perp_ray(cubic, q.mid_t).1
    }

    fn quad_perp_ray(&self, quad: &[Point; 3], t: f32) -> (Point, Point, Point) {
        let t_pt = eval_quad_at(quad, t);
        let mut dxy = eval_quad_tangent(quad, t);
        if is_zero(dxy) {
            dxy = sub(quad[2], quad[0]);
        }
        let (on_pt, tangent) = self.set_ray_pts(t_pt, dxy);
        (t_pt, on_pt, tangent)
    }

    fn intersect_ray(&self, q: &mut QuadConstruct, ctrl_pt: bool) -> ResultType {
        let start = q.quad[0];
        let end = q.quad[2];
        let a_len = q.tangent_start;
        let b_len = q.tangent_end;
        let denom = cross(a_len, b_len);
        if denom == 0.0 || !denom.is_finite() {
            q.opposite_tangents = dot(a_len, b_len) < 0.0;
            return ResultType::Degenerate;
        }
        q.opposite_tangents = false;
        let ab0 = sub(start, end);
        let mut numer_a = cross(b_len, ab0);
        let numer_b = cross(a_len, ab0);
        if (numer_a >= 0.0) == (numer_b >= 0.0) {
            let dist1 = pt_to_tangent_line(start, end, q.tangent_end);
            let dist2 = pt_to_tangent_line(end, start, q.tangent_start);
            if dist1.max(dist2) <= self.inv_res_scale_squared {
                return ResultType::Degenerate;
            }
            return ResultType::Split;
        }
        numer_a /= denom;
        let valid_divide = numer_a > numer_a - 1.0;
        if valid_divide {
            if ctrl_pt {
                q.quad[1] = add(start, mul(q.tangent_start, numer_a));
            }
            return ResultType::Quad;
        }
        q.opposite_tangents = dot(a_len, b_len) < 0.0;
        ResultType::Degenerate
    }

    fn tangents_meet(&self, cubic: &[Point; 4], q: &mut QuadConstruct) -> ResultType {
        self.cubic_quad_ends(cubic, q);
        self.intersect_ray(q, false)
    }

    fn pt_in_quad_bounds(&self, quad: &[Point; 3], pt: Point) -> bool {
        let x_min = quad[0].x.min(quad[1].x).min(quad[2].x);
        if pt.x + self.inv_res_scale < x_min {
            return false;
        }
        let x_max = quad[0].x.max(quad[1].x).max(quad[2].x);
        if pt.x - self.inv_res_scale > x_max {
            return false;
        }
        let y_min = quad[0].y.min(quad[1].y).min(quad[2].y);
        if pt.y + self.inv_res_scale < y_min {
            return false;
        }
        let y_max = quad[0].y.max(quad[1].y).max(quad[2].y);
        if pt.y - self.inv_res_scale > y_max {
            return false;
        }
        true
    }

    fn stroke_close_enough(
        &self,
        stroke: &[Point; 3],
        ray: &[Point; 2],
        q: &QuadConstruct,
    ) -> ResultType {
        let stroke_mid = eval_quad_at(stroke, 0.5);
        if points_within_dist(ray[0], stroke_mid, self.inv_res_scale) {
            if sharp_angle(&q.quad) {
                return ResultType::Split;
            }
            return ResultType::Quad;
        }
        if !self.pt_in_quad_bounds(stroke, ray[0]) {
            return ResultType::Split;
        }
        let (roots, root_count) = intersect_quad_ray(ray, stroke);
        if root_count != 1 {
            return ResultType::Split;
        }
        let quad_pt = eval_quad_at(stroke, roots[0]);
        let error = self.inv_res_scale * (1.0 - (roots[0] - 0.5).abs() * 2.0);
        if points_within_dist(ray[0], quad_pt, error) {
            if sharp_angle(&q.quad) {
                return ResultType::Split;
            }
            return ResultType::Quad;
        }
        ResultType::Split
    }

    fn compare_quad_cubic(&self, cubic: &[Point; 4], q: &mut QuadConstruct) -> ResultType {
        self.cubic_quad_ends(cubic, q);
        let r = self.intersect_ray(q, true);
        if r != ResultType::Quad {
            return r;
        }
        let (t_pt, on_pt, _) = self.cubic_perp_ray(cubic, q.mid_t);
        let ray = [on_pt, t_pt];
        self.stroke_close_enough(&q.quad.clone(), &ray, q)
    }

    fn compare_quad_conic(&self, conic: &Conic, q: &mut QuadConstruct) -> ResultType {
        self.conic_quad_ends(conic, q);
        let r = self.intersect_ray(q, true);
        if r != ResultType::Quad {
            return r;
        }
        let (t_pt, on_pt, _) = self.conic_perp_ray(conic, q.mid_t);
        let ray = [on_pt, t_pt];
        self.stroke_close_enough(&q.quad.clone(), &ray, q)
    }

    fn compare_quad_quad(&self, quad: &[Point; 3], q: &mut QuadConstruct) -> ResultType {
        if !q.start_set {
            let (_, on, tan) = self.quad_perp_ray(quad, q.start_t);
            q.quad[0] = on;
            q.tangent_start = tan;
            q.start_set = true;
        }
        if !q.end_set {
            let (_, on, tan) = self.quad_perp_ray(quad, q.end_t);
            q.quad[2] = on;
            q.tangent_end = tan;
            q.end_set = true;
        }
        let r = self.intersect_ray(q, true);
        if r != ResultType::Quad {
            return r;
        }
        let (t_pt, on_pt, _) = self.quad_perp_ray(quad, q.mid_t);
        let ray = [on_pt, t_pt];
        self.stroke_close_enough(&q.quad.clone(), &ray, q)
    }

    fn sink(&mut self) -> &mut PathBuilder {
        if self.stroke_type == StrokeType::Outer {
            &mut self.outer
        } else {
            &mut self.inner
        }
    }

    fn add_degenerate_line(&mut self, q: &QuadConstruct) {
        let p = q.quad[2];
        self.sink().line_to(p);
    }

    fn cubic_mid_on_line(&self, cubic: &[Point; 4], q: &QuadConstruct) -> bool {
        let stroke_mid = self.cubic_quad_mid(cubic, q);
        let dist = pt_to_line(stroke_mid, q.quad[0], q.quad[2]);
        dist < self.inv_res_scale_squared
    }

    fn cubic_stroke(&mut self, cubic: &[Point; 4], q: &mut QuadConstruct) -> bool {
        if !self.found_tangents {
            let r = self.tangents_meet(cubic, q);
            if r != ResultType::Quad {
                if (r == ResultType::Degenerate
                    || points_within_dist(q.quad[0], q.quad[2], self.inv_res_scale))
                    && self.cubic_mid_on_line(cubic, q)
                {
                    self.add_degenerate_line(q);
                    return true;
                }
            } else {
                self.found_tangents = true;
            }
        }
        if self.found_tangents {
            let r = self.compare_quad_cubic(cubic, q);
            if r == ResultType::Quad {
                let s = q.quad;
                self.sink().quad_to(s[1], s[2]);
                return true;
            }
            if r == ResultType::Degenerate && !q.opposite_tangents {
                self.add_degenerate_line(q);
                return true;
            }
        }
        if !q.quad[2].is_finite() {
            return false;
        }
        self.recursion_depth += 1;
        if self.recursion_depth
            > RECURSIVE_LIMITS[if self.found_tangents {
                CUBIC_LIMIT
            } else {
                TANGENT_LIMIT
            }]
        {
            self.add_degenerate_line(q);
            return true;
        }
        let mut half = QuadConstruct::default();
        if !half.init_with_start(q) {
            self.add_degenerate_line(q);
            self.recursion_depth -= 1;
            return true;
        }
        if !self.cubic_stroke(cubic, &mut half) {
            return false;
        }
        if !half.init_with_end(q) {
            self.add_degenerate_line(q);
            self.recursion_depth -= 1;
            return true;
        }
        if !self.cubic_stroke(cubic, &mut half) {
            return false;
        }
        self.recursion_depth -= 1;
        true
    }

    fn conic_stroke(&mut self, conic: &Conic, q: &mut QuadConstruct) -> bool {
        let r = self.compare_quad_conic(conic, q);
        if r == ResultType::Quad {
            let s = q.quad;
            self.sink().quad_to(s[1], s[2]);
            return true;
        }
        if r == ResultType::Degenerate {
            self.add_degenerate_line(q);
            return true;
        }
        self.recursion_depth += 1;
        if self.recursion_depth > RECURSIVE_LIMITS[CONIC_LIMIT] {
            self.add_degenerate_line(q);
            return true;
        }
        let mut half = QuadConstruct::default();
        let _ = half.init_with_start(q);
        if !self.conic_stroke(conic, &mut half) {
            return false;
        }
        let _ = half.init_with_end(q);
        if !self.conic_stroke(conic, &mut half) {
            return false;
        }
        self.recursion_depth -= 1;
        true
    }

    fn quad_stroke(&mut self, quad: &[Point; 3], q: &mut QuadConstruct) -> bool {
        let r = self.compare_quad_quad(quad, q);
        if r == ResultType::Quad {
            let s = q.quad;
            self.sink().quad_to(s[1], s[2]);
            return true;
        }
        if r == ResultType::Degenerate {
            self.add_degenerate_line(q);
            return true;
        }
        self.recursion_depth += 1;
        if self.recursion_depth > RECURSIVE_LIMITS[QUAD_LIMIT] {
            self.add_degenerate_line(q);
            return true;
        }
        let mut half = QuadConstruct::default();
        let _ = half.init_with_start(q);
        if !self.quad_stroke(quad, &mut half) {
            return false;
        }
        let _ = half.init_with_end(q);
        if !self.quad_stroke(quad, &mut half) {
            return false;
        }
        self.recursion_depth -= 1;
        true
    }

    fn conic_to(&mut self, pt1: Point, pt2: Point, weight: f32) {
        let conic = Conic::new(self.prev_pt, pt1, pt2, weight);
        let (kind, reduction) = check_conic_linear(&conic);
        match kind {
            ReductionType::Point | ReductionType::Line => {
                self.line_to(pt2, None);
                return;
            }
            ReductionType::Degenerate => {
                self.line_to(reduction, None);
                let save = self.joiner;
                self.joiner = Join::Round;
                self.line_to(pt2, None);
                self.joiner = save;
                return;
            }
            _ => {}
        }
        let Some((normal_ab, unit_ab)) = self.pre_join_to(pt1, false) else {
            self.line_to(pt2, None);
            return;
        };
        let mut q = QuadConstruct::default();
        self.init(StrokeType::Outer, &mut q, 0.0, 1.0);
        let _ = self.conic_stroke(&conic, &mut q);
        self.init(StrokeType::Inner, &mut q, 0.0, 1.0);
        let _ = self.conic_stroke(&conic, &mut q);
        let (normal_bc, unit_bc) = self.set_quad_end_normal(&conic.pts, normal_ab, unit_ab);
        self.post_join_to(pt2, normal_bc, unit_bc);
    }

    fn quad_to(&mut self, pt1: Point, pt2: Point) {
        let quad = [self.prev_pt, pt1, pt2];
        let (kind, reduction) = check_quad_linear(&quad);
        match kind {
            ReductionType::Point | ReductionType::Line => {
                self.line_to(pt2, None);
                return;
            }
            ReductionType::Degenerate => {
                self.line_to(reduction, None);
                let save = self.joiner;
                self.joiner = Join::Round;
                self.line_to(pt2, None);
                self.joiner = save;
                return;
            }
            _ => {}
        }
        let Some((normal_ab, unit_ab)) = self.pre_join_to(pt1, false) else {
            self.line_to(pt2, None);
            return;
        };
        let mut q = QuadConstruct::default();
        self.init(StrokeType::Outer, &mut q, 0.0, 1.0);
        let _ = self.quad_stroke(&quad, &mut q);
        self.init(StrokeType::Inner, &mut q, 0.0, 1.0);
        let _ = self.quad_stroke(&quad, &mut q);
        let (normal_bc, unit_bc) = self.set_quad_end_normal(&quad, normal_ab, unit_ab);
        self.post_join_to(pt2, normal_bc, unit_bc);
    }

    fn cubic_to(&mut self, pt1: Point, pt2: Point, pt3: Point) {
        let cubic = [self.prev_pt, pt1, pt2, pt3];
        let (kind, reduction, tangent_index) = check_cubic_linear(&cubic);
        match kind {
            ReductionType::Point | ReductionType::Line => {
                self.line_to(pt3, None);
                return;
            }
            ReductionType::Degenerate | ReductionType::Degenerate2 | ReductionType::Degenerate3 => {
                self.line_to(reduction[0], None);
                let save = self.joiner;
                self.joiner = Join::Round;
                if kind >= ReductionType::Degenerate2 {
                    self.line_to(reduction[1], None);
                }
                if kind == ReductionType::Degenerate3 {
                    self.line_to(reduction[2], None);
                }
                self.line_to(pt3, None);
                self.joiner = save;
                return;
            }
            ReductionType::Quad => {}
        }
        let tangent_pt = cubic[tangent_index];
        let Some((normal_ab, unit_ab)) = self.pre_join_to(tangent_pt, false) else {
            self.line_to(pt3, None);
            return;
        };
        let t_values = find_cubic_inflections(&cubic);
        let mut last_t = 0.0f32;
        for index in 0..=t_values.len() {
            let next_t = if index < t_values.len() {
                t_values[index]
            } else {
                1.0
            };
            let mut q = QuadConstruct::default();
            self.init(StrokeType::Outer, &mut q, last_t, next_t);
            let _ = self.cubic_stroke(&cubic, &mut q);
            self.init(StrokeType::Inner, &mut q, last_t, next_t);
            let _ = self.cubic_stroke(&cubic, &mut q);
            last_t = next_t;
        }
        let cusp = find_cubic_cusp(&cubic);
        if cusp > 0.0 {
            let loc = eval_cubic_at(&cubic, cusp);
            let r = self.radius;
            if r >= 0.0 {
                self.cusper.add_oval(
                    &Rect::from_ltrb(loc.x - r, loc.y - r, loc.x + r, loc.y + r),
                    true,
                    1,
                );
            }
        }
        let (normal_cd, unit_cd) = self.set_cubic_end_normal(&cubic, normal_ab, unit_ab);
        self.post_join_to(pt3, normal_cd, unit_cd);
    }

    fn close(&mut self, is_line: bool) {
        self.finish_contour(true, is_line);
    }

    fn done(mut self, is_line: bool) -> Path {
        self.finish_contour(false, is_line);
        self.outer.detach()
    }
}

/// `SkRect::contains(const SkRect&)`.
fn rect_contains(outer: &Rect, inner: &Rect) -> bool {
    !(inner.left >= inner.right || inner.top >= inner.bottom)
        && !(outer.left >= outer.right || outer.top >= outer.bottom)
        && outer.left <= inner.left
        && outer.top <= inner.top
        && outer.right >= inner.right
        && outer.bottom >= inner.bottom
}

fn pts_in_verb(v: Verb) -> usize {
    match v {
        Verb::Move | Verb::Line => 1,
        Verb::Quad | Verb::Conic => 2,
        Verb::Cubic => 3,
        Verb::Close => 0,
    }
}

/// `SkPathBuilder::privateReversePathTo`: path segments in reverse up to
/// its first Move (the Move itself is not added).
fn reverse_path_to(b: &mut PathBuilder, path: &Path) {
    if path.verbs.is_empty() {
        return;
    }
    let mut vi = path.verbs.len();
    let mut pi = path.pts.len() as isize - 1;
    let mut ci = path.conics.len();
    while vi > 0 {
        vi -= 1;
        let v = path.verbs[vi];
        pi -= pts_in_verb(v) as isize;
        let p = |k: isize| path.pts[(pi + k) as usize];
        match v {
            Verb::Move => return,
            Verb::Line => b.line_to(p(0)),
            Verb::Quad => b.quad_to(p(1), p(0)),
            Verb::Conic => {
                ci -= 1;
                b.conic_to(p(1), p(0), path.conics[ci]);
            }
            Verb::Cubic => b.cubic_to(p(2), p(1), p(0)),
            Verb::Close => {}
        }
    }
}

/// `SkPathBuilder::addPath(src)` (append): verbs and points as is; a
/// trailing lone Move in the destination is dropped.
fn append_path(b: &mut PathBuilder, src: &Path) {
    if src.verbs.is_empty() {
        return;
    }
    if b.verbs.last() == Some(&Verb::Move) {
        b.verbs.pop();
        b.pts.pop();
    }
    b.verbs.extend_from_slice(&src.verbs);
    b.pts.extend_from_slice(&src.pts);
    b.conics.extend_from_slice(&src.conics);
    b.note_appended();
}

// ── rect (SkPathPriv::IsRectContour, SkStroke::strokeRect) ───────────────

struct RectContour {
    rect: Rect,
    is_closed: bool,
    cw: bool,
}

fn rect_make_dir(dx: f32, dy: f32) -> i32 {
    ((dx != 0.0) as i32) | (((dx > 0.0 || dy > 0.0) as i32) << 1)
}

fn trivial_rect(path: &Path) -> Option<RectContour> {
    if path.pts.len() != 4 || path.verbs.len() != 5 {
        return None;
    }
    if path.verbs != [Verb::Move, Verb::Line, Verb::Line, Verb::Line, Verb::Close] {
        return None;
    }
    let p = &path.pts;
    let v0 = sub(p[1], p[0]);
    let v1 = sub(p[2], p[1]);
    let v2 = sub(p[3], p[2]);
    let v3 = sub(p[0], p[3]);
    let ortho = |a: Point, b: Point| ((a.x == 0.0) ^ (b.x == 0.0)) & ((a.y == 0.0) ^ (b.y == 0.0));
    if !(((v0.x == 0.0) ^ (v0.y == 0.0)) & ortho(v0, v1) & ortho(v1, v2) & ortho(v2, v3)) {
        return None;
    }
    let rect = Rect::from_ltrb(p[0].x, p[0].y, p[2].x, p[2].y).sorted();
    Some(RectContour {
        rect,
        is_closed: true,
        cw: cross(v0, v1) > 0.0,
    })
}

/// `SkPathPriv::IsRectContour(pts, verbs, mask, allowPartial=false)`.
fn is_rect_contour(path: &Path) -> Option<RectContour> {
    if path.segment_mask != super::path::SEG_LINE || path.pts.len() < 4 || path.verbs.len() < 4 {
        return None;
    }
    if let Some(rc) = trivial_rect(path) {
        return Some(rc);
    }
    let verbs = &path.verbs;
    let pts = &path.pts;
    let mut corners = 0i32;
    let mut line_start = Point::new(0.0, 0.0);
    let mut first_pt: Option<usize> = None;
    let mut last_pt: Option<usize> = None;
    let mut first_corner = Point::default();
    let mut third_corner = Point::default();
    let mut pi = 0usize;
    let mut directions = [-1i32; 5];
    let mut closed_or_moved = false;
    let mut auto_close = false;
    for &verb in verbs.iter() {
        match verb {
            Verb::Close | Verb::Line => {
                let is_close = verb == Verb::Close;
                if is_close {
                    auto_close = true;
                } else {
                    last_pt = Some(pi);
                }
                let line_end = if is_close {
                    pts[first_pt?]
                } else {
                    let p = pts[pi];
                    pi += 1;
                    p
                };
                let line_delta = sub(line_end, line_start);
                if line_delta.x != 0.0 && line_delta.y != 0.0 {
                    return None;
                }
                if !line_delta.is_finite() {
                    return None;
                }
                if line_start == line_end {
                    continue;
                }
                let next_direction = rect_make_dir(line_delta.x, line_delta.y);
                if corners == 0 {
                    directions[0] = next_direction;
                    corners = 1;
                    closed_or_moved = false;
                    line_start = line_end;
                    continue;
                }
                if closed_or_moved {
                    return None;
                }
                if auto_close && next_direction == directions[0] {
                    continue;
                }
                closed_or_moved = auto_close;
                if directions[(corners - 1) as usize] == next_direction {
                    if corners == 3 && !is_close {
                        third_corner = line_end;
                    }
                    line_start = line_end;
                    continue;
                }
                directions[corners as usize] = next_direction;
                corners += 1;
                match corners {
                    2 => first_corner = line_start,
                    3 => {
                        if (directions[0] ^ directions[2]) != 2 {
                            return None;
                        }
                        third_corner = line_end;
                    }
                    4 => {
                        if (directions[1] ^ directions[3]) != 2 {
                            return None;
                        }
                    }
                    _ => return None,
                }
                line_start = line_end;
            }
            Verb::Quad | Verb::Conic | Verb::Cubic => return None,
            Verb::Move => {
                if corners == 0 {
                    first_pt = Some(pi);
                } else {
                    let close_xy = sub(pts[first_pt?], pts[last_pt?]);
                    if close_xy.x != 0.0 && close_xy.y != 0.0 {
                        return None;
                    }
                }
                line_start = pts[pi];
                pi += 1;
                closed_or_moved = true;
            }
        }
    }
    if !(3..=4).contains(&corners) {
        return None;
    }
    let close_xy = sub(pts[first_pt?], pts[last_pt?]);
    if close_xy.x != 0.0 && close_xy.y != 0.0 {
        return None;
    }
    let rect = Rect::from_ltrb(
        first_corner.x.min(third_corner.x),
        first_corner.y.min(third_corner.y),
        first_corner.x.max(third_corner.x),
        first_corner.y.max(third_corner.y),
    );
    Some(RectContour {
        rect,
        is_closed: auto_close,
        cw: directions[0] == ((directions[1] + 1) & 3),
    })
}

fn add_bevel(b: &mut PathBuilder, r: &Rect, outer: &Rect, cw: bool) {
    let mut pts = [Point::default(); 8];
    if cw {
        pts[0] = Point::new(r.left, outer.top);
        pts[1] = Point::new(r.right, outer.top);
        pts[2] = Point::new(outer.right, r.top);
        pts[3] = Point::new(outer.right, r.bottom);
        pts[4] = Point::new(r.right, outer.bottom);
        pts[5] = Point::new(r.left, outer.bottom);
        pts[6] = Point::new(outer.left, r.bottom);
        pts[7] = Point::new(outer.left, r.top);
    } else {
        pts[7] = Point::new(r.left, outer.top);
        pts[6] = Point::new(r.right, outer.top);
        pts[5] = Point::new(outer.right, r.top);
        pts[4] = Point::new(outer.right, r.bottom);
        pts[3] = Point::new(r.right, outer.bottom);
        pts[2] = Point::new(r.left, outer.bottom);
        pts[1] = Point::new(outer.left, r.bottom);
        pts[0] = Point::new(outer.left, r.top);
    }
    b.move_to(pts[0]);
    for p in &pts[1..] {
        b.line_to(*p);
    }
    b.close();
}

/// `SkStroke::strokeRect`; None for a round join (RRect not ported yet).
fn stroke_rect(orig: &Rect, p: &StrokeParams, cw: bool) -> Option<Path> {
    let radius = p.width / 2.0;
    if radius <= 0.0 {
        return Some(Path::default());
    }
    let mut cw = cw;
    let rw = orig.right - orig.left;
    let rh = orig.bottom - orig.top;
    if (rw < 0.0) ^ (rh < 0.0) {
        cw = !cw;
    }
    let rect = orig.sorted();
    let rw = rect.right - rect.left;
    let rh = rect.bottom - rect.top;
    let r = Rect::from_ltrb(
        rect.left - radius,
        rect.top - radius,
        rect.right + radius,
        rect.bottom + radius,
    );
    let mut join = p.join;
    if join == Join::Miter && p.miter_limit < std::f32::consts::SQRT_2 {
        join = Join::Bevel;
    }
    let mut b = PathBuilder::new();
    match join {
        Join::Miter => b.add_rect(&r, cw, 0),
        Join::Bevel => add_bevel(&mut b, &rect, &r, cw),
        Join::Round => return None,
    }
    if p.width < rw.min(rh) {
        let inner = Rect::from_ltrb(
            rect.left + radius,
            rect.top + radius,
            rect.right - radius,
            rect.bottom - radius,
        );
        b.add_rect(&inner, !cw, 0);
    }
    Some(b.detach())
}

/// `SkStroke::strokePath` (no stroke-and-fill). None for a case we lack
/// (round join on a rect); an empty path for zero width.
pub fn stroke_path(src: &Path, p: &StrokeParams) -> Option<Path> {
    let radius = p.width / 2.0;
    if radius <= 0.0 {
        return Some(Path::default());
    }
    if let Some(rc) = is_rect_contour(src) {
        if rc.is_closed {
            return stroke_rect(&rc.rect, p, rc.cw);
        }
    }
    let mut stroker = PathStroker::new(radius, p.miter_limit, p.cap, p.join, p.res_scale, false);
    let mut iter = PathIter::new(src);
    let mut last_segment_is_line = false;
    while let Some(seg) = iter.next() {
        match seg {
            Seg::Move(pt) => stroker.move_to(pt),
            Seg::Line(pts) => {
                stroker.line_to(pts[1], Some(&iter));
                last_segment_is_line = true;
            }
            Seg::Quad(pts) => {
                stroker.quad_to(pts[1], pts[2]);
                last_segment_is_line = false;
            }
            Seg::Conic(pts, w) => {
                stroker.conic_to(pts[1], pts[2], w);
                last_segment_is_line = false;
            }
            Seg::Cubic(pts) => {
                stroker.cubic_to(pts[1], pts[2], pts[3]);
                last_segment_is_line = false;
            }
            Seg::Close => {
                if p.cap != Cap::Butt {
                    if stroker.has_only_move_to() {
                        let mv = stroker.move_to_pt();
                        stroker.line_to(mv, None);
                        last_segment_is_line = true;
                        continue;
                    }
                    if stroker.is_current_contour_empty() {
                        last_segment_is_line = true;
                        continue;
                    }
                }
                stroker.close(last_segment_is_line);
            }
        }
    }
    Some(stroker.done(last_segment_is_line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_a_line_into_a_closed_quad() {
        let mut b = PathBuilder::new();
        b.move_to(Point::new(10.0, 10.0));
        b.line_to(Point::new(30.0, 10.0));
        let path = b.detach();
        let out = stroke_path(
            &path,
            &StrokeParams {
                width: 4.0,
                miter_limit: 10.0,
                cap: Cap::Butt,
                join: Join::Miter,
                res_scale: 1.0,
            },
        )
        .unwrap();
        assert_eq!(out.verbs.first(), Some(&Verb::Move));
        assert_eq!(out.verbs.last(), Some(&Verb::Close));
        let bounds = out.bounds();
        assert_eq!(
            (bounds.left, bounds.top, bounds.right, bounds.bottom),
            (10.0, 8.0, 30.0, 12.0)
        );
    }

    #[test]
    fn rect_stroke_is_two_rects() {
        let mut b = PathBuilder::new();
        b.add_rect(&Rect::from_ltrb(10.0, 10.0, 30.0, 20.0), true, 0);
        let path = b.detach();
        let out = stroke_path(
            &path,
            &StrokeParams {
                width: 2.0,
                miter_limit: 10.0,
                cap: Cap::Butt,
                join: Join::Miter,
                res_scale: 1.0,
            },
        )
        .unwrap();
        assert_eq!(out.verbs.iter().filter(|v| **v == Verb::Move).count(), 2);
        let bounds = out.bounds();
        assert_eq!(
            (bounds.left, bounds.top, bounds.right, bounds.bottom),
            (9.0, 9.0, 31.0, 21.0)
        );
    }
}
