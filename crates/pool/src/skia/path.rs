//! Path as in `SkPathBuilder`/`SkPathRaw` (Skia at Chrome 151) and the canvas
//! path as Blink builds it (`canvas_path.cc`, `path_builder.cc`): arcs as
//! conics via `arcTo`, a full circle as an oval, angles in float, convexity
//! computed as Skia does before picking the scan converter.

use super::geometry::{
    cos_snap_to_zero, degrees_to_radians, nearly_equal, sin_snap_to_zero, Conic, Matrix, Point,
    Rect, SCALAR_ROOT_2_OVER_2,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Move,
    Line,
    Quad,
    Conic,
    Cubic,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillType {
    Winding,
    EvenOdd,
}

/// `SkPathConvexity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Convexity {
    Unknown,
    ConvexCw,
    ConvexCcw,
    ConvexDegenerate,
    Concave,
}

impl Convexity {
    pub fn is_convex(self) -> bool {
        matches!(
            self,
            Convexity::ConvexCw | Convexity::ConvexCcw | Convexity::ConvexDegenerate
        )
    }
    fn from_dir(cw: bool) -> Convexity {
        if cw {
            Convexity::ConvexCw
        } else {
            Convexity::ConvexCcw
        }
    }
    fn opposite(self) -> Convexity {
        match self {
            Convexity::ConvexCw => Convexity::ConvexCcw,
            Convexity::ConvexCcw => Convexity::ConvexCw,
            o => o,
        }
    }
}

pub const SEG_LINE: u8 = 1;
pub const SEG_QUAD: u8 = 2;
pub const SEG_CONIC: u8 = 4;
pub const SEG_CUBIC: u8 = 8;

#[derive(Clone, Debug)]
pub struct Path {
    pub verbs: Vec<Verb>,
    pub pts: Vec<Point>,
    pub conics: Vec<f32>,
    pub fill_type: FillType,
    pub convexity: Convexity,
    pub segment_mask: u8,
}

impl Path {
    pub fn bounds(&self) -> Rect {
        Rect::bounds(&self.pts)
    }
    pub fn is_finite(&self) -> bool {
        self.pts.iter().all(|p| p.is_finite())
    }
    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }
    /// `SkPathData::MakeTransform` + `TransformConvexity`.
    pub fn transform(&self, m: &Matrix) -> Path {
        if m.is_identity() {
            return self.clone();
        }
        let mut out = self.clone();
        m.map_points(&mut out.pts);
        out.convexity = transform_convexity(m, &self.pts, self.convexity);
        out
    }
    /// Convexity as `SkPathRaw` finds it with `SkResolveConvexity::kYes`.
    pub fn resolve_convexity(&mut self) {
        if self.convexity == Convexity::Unknown {
            self.convexity = compute_convexity(&self.pts, &self.verbs);
        }
    }
    /// Simple "path is a rect" check for four lines. Skia
    /// (`SkPathRaw::isRect`) handles more, but canvas only draws rects
    /// via `rect()`, which yields exactly this shape.
    pub fn as_rect(&self) -> Option<Rect> {
        let v = &self.verbs;
        let ok = match v.len() {
            5 => {
                v[0] == Verb::Move
                    && v[1..4].iter().all(|x| *x == Verb::Line)
                    && v[4] == Verb::Close
            }
            6 => {
                v[0] == Verb::Move
                    && v[1..5].iter().all(|x| *x == Verb::Line)
                    && v[5] == Verb::Close
            }
            _ => false,
        };
        if !ok {
            return None;
        }
        let p = &self.pts;
        if v.len() == 6 && p[4] != p[0] {
            return None;
        }
        let axis = |a: Point, b: Point| a.x == b.x || a.y == b.y;
        for i in 0..4 {
            if !axis(p[i], p[(i + 1) % 4]) {
                return None;
            }
        }
        // Opposite sides must run along different axes.
        let horiz0 = p[0].y == p[1].y && p[0].x != p[1].x;
        let vert1 = p[1].x == p[2].x && p[1].y != p[2].y;
        let horiz2 = p[2].y == p[3].y && p[2].x != p[3].x;
        let vert3 = p[3].x == p[0].x && p[3].y != p[0].y;
        let vert0 = p[0].x == p[1].x && p[0].y != p[1].y;
        let horiz1 = p[1].y == p[2].y && p[1].x != p[2].x;
        let vert2 = p[2].x == p[3].x && p[2].y != p[3].y;
        let horiz3 = p[3].y == p[0].y && p[3].x != p[0].x;
        if (horiz0 && vert1 && horiz2 && vert3) || (vert0 && horiz1 && vert2 && horiz3) {
            let r = Rect::bounds(&p[..4]);
            if !r.is_empty() {
                return Some(r);
            }
        }
        None
    }
}

// ── SkPathBuilder ──────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct PathBuilder {
    pub verbs: Vec<Verb>,
    pub pts: Vec<Point>,
    pub conics: Vec<f32>,
    pub fill_type: FillType,
    convexity: Convexity,
    segment_mask: u8,
    last_move_point: Point,
    needs_move_verb: bool,
}

impl Default for Path {
    fn default() -> Self {
        Path {
            verbs: Vec::new(),
            pts: Vec::new(),
            conics: Vec::new(),
            fill_type: FillType::Winding,
            convexity: Convexity::Unknown,
            segment_mask: 0,
        }
    }
}

impl Default for PathBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PathBuilder {
    pub fn new() -> PathBuilder {
        PathBuilder {
            verbs: Vec::new(),
            pts: Vec::new(),
            conics: Vec::new(),
            fill_type: FillType::Winding,
            convexity: Convexity::Unknown,
            segment_mask: 0,
            last_move_point: Point::new(0.0, 0.0),
            needs_move_verb: true,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }
    pub fn reset(&mut self) {
        *self = PathBuilder::new();
    }
    pub fn last_pt(&self) -> Option<Point> {
        self.pts.last().copied()
    }
    /// After appending verbs/points directly (addPath): recompute the segment
    /// mask and the "needs Move" flag.
    pub fn note_appended(&mut self) {
        let mut mask = 0u8;
        for v in &self.verbs {
            mask |= match v {
                Verb::Line => SEG_LINE,
                Verb::Quad => SEG_QUAD,
                Verb::Conic => SEG_CONIC,
                Verb::Cubic => SEG_CUBIC,
                _ => 0,
            };
        }
        self.segment_mask = mask;
        self.convexity = Convexity::Unknown;
        self.needs_move_verb = self.verbs.last() == Some(&Verb::Close);
        // Last Move point, for ensure_move after Close.
        let mut pi = 0usize;
        for v in &self.verbs {
            match v {
                Verb::Move => {
                    self.last_move_point = self.pts[pi];
                    pi += 1;
                }
                Verb::Line => pi += 1,
                Verb::Quad | Verb::Conic => pi += 2,
                Verb::Cubic => pi += 3,
                Verb::Close => {}
            }
        }
    }
    pub fn bounds(&self) -> Rect {
        Rect::bounds(&self.pts)
    }

    fn ensure_move(&mut self) {
        if self.needs_move_verb {
            let p = self.last_move_point;
            self.move_to(p);
        }
    }

    pub fn move_to(&mut self, pt: Point) {
        if self.verbs.last() == Some(&Verb::Move) {
            *self.pts.last_mut().unwrap() = pt;
        } else {
            self.pts.push(pt);
            self.verbs.push(Verb::Move);
            self.convexity = Convexity::Unknown;
        }
        self.last_move_point = pt;
        self.needs_move_verb = false;
    }
    pub fn line_to(&mut self, pt: Point) {
        self.ensure_move();
        self.pts.push(pt);
        self.verbs.push(Verb::Line);
        self.segment_mask |= SEG_LINE;
    }
    pub fn quad_to(&mut self, p1: Point, p2: Point) {
        self.ensure_move();
        self.pts.push(p1);
        self.pts.push(p2);
        self.verbs.push(Verb::Quad);
        self.segment_mask |= SEG_QUAD;
    }
    pub fn conic_to(&mut self, p1: Point, p2: Point, w: f32) {
        self.ensure_move();
        if w <= 0.0 {
            self.line_to(p2);
            return;
        }
        self.pts.push(p1);
        self.pts.push(p2);
        if w == 1.0 {
            self.verbs.push(Verb::Quad);
            self.segment_mask |= SEG_QUAD;
        } else if w.is_finite() {
            self.verbs.push(Verb::Conic);
            self.conics.push(w);
            self.segment_mask |= SEG_CONIC;
        } else {
            self.verbs.push(Verb::Line);
            self.verbs.push(Verb::Line);
            self.segment_mask |= SEG_LINE;
        }
    }
    pub fn cubic_to(&mut self, p1: Point, p2: Point, p3: Point) {
        self.ensure_move();
        self.pts.push(p1);
        self.pts.push(p2);
        self.pts.push(p3);
        self.verbs.push(Verb::Cubic);
        self.segment_mask |= SEG_CUBIC;
    }
    pub fn close(&mut self) {
        if !self.verbs.is_empty() && self.verbs.last() != Some(&Verb::Close) {
            self.ensure_move();
            self.verbs.push(Verb::Close);
            self.needs_move_verb = true;
        }
    }

    /// `SkPathBuilder::addRect(rect, dir, index)`; Blink uses CW, index 0.
    pub fn add_rect(&mut self, r: &Rect, cw: bool, index: usize) {
        let was_empty = self.segment_mask == 0;
        let pts = [
            Point::new(r.left, r.top),
            Point::new(r.right, r.top),
            Point::new(r.right, r.bottom),
            Point::new(r.left, r.bottom),
        ];
        let mut it = PointIter::new(4, cw, index);
        self.move_to(pts[it.current()]);
        for _ in 0..3 {
            self.line_to(pts[it.next()]);
        }
        self.close();
        if was_empty {
            self.convexity = Convexity::from_dir(cw);
        }
    }

    /// `SkPathBuilder::addOval(oval, dir, index)`: four conics, w=√2/2.
    pub fn add_oval(&mut self, oval: &Rect, cw: bool, index: usize) {
        let was_empty = self.segment_mask == 0;
        let cx = oval.center_x();
        let cy = oval.center_y();
        let oval_pts = [
            Point::new(cx, oval.top),
            Point::new(oval.right, cy),
            Point::new(cx, oval.bottom),
            Point::new(oval.left, cy),
        ];
        let rect_pts = [
            Point::new(oval.left, oval.top),
            Point::new(oval.right, oval.top),
            Point::new(oval.right, oval.bottom),
            Point::new(oval.left, oval.bottom),
        ];
        let mut oval_iter = PointIter::new(4, cw, index);
        let mut rect_iter = PointIter::new(4, cw, index + if cw { 0 } else { 1 });
        self.move_to(oval_pts[oval_iter.current()]);
        for _ in 0..4 {
            let c = rect_pts[rect_iter.next()];
            let p = oval_pts[oval_iter.next()];
            self.conic_to(c, p, SCALAR_ROOT_2_OVER_2);
        }
        self.close();
        if was_empty {
            self.convexity = Convexity::from_dir(cw);
        }
    }

    /// `SkPathBuilder::arcTo(oval, startAngle, sweepAngle, forceMoveTo)`.
    pub fn arc_to(&mut self, oval: &Rect, start_angle: f32, sweep_angle: f32, force_move_to: bool) {
        if oval.width() < 0.0 || oval.height() < 0.0 {
            return;
        }
        let start_angle = start_angle % 360.0;
        let force_move_to = force_move_to || self.verbs.is_empty();

        // arc_is_lone_point
        if sweep_angle == 0.0 && (start_angle == 0.0 || start_angle == 360.0) {
            let pt = Point::new(oval.right, oval.center_y());
            return self.add_pt(pt, force_move_to);
        } else if oval.width() == 0.0 && oval.height() == 0.0 {
            let pt = Point::new(oval.right, oval.top);
            return self.add_pt(pt, force_move_to);
        }

        // angles_to_unit_vectors
        let start_rad = degrees_to_radians(start_angle);
        let mut stop_rad = degrees_to_radians(start_angle + sweep_angle);
        let start_v = Point::new(cos_snap_to_zero(start_rad), sin_snap_to_zero(start_rad));
        let mut stop_v = Point::new(cos_snap_to_zero(stop_rad), sin_snap_to_zero(stop_rad));
        if start_v == stop_v {
            let sw = sweep_angle.abs();
            if sw < 360.0 && sw > 359.0 {
                let delta_rad = (1.0f32 / 512.0).copysign(sweep_angle);
                loop {
                    stop_rad -= delta_rad;
                    stop_v = Point::new(cos_snap_to_zero(stop_rad), sin_snap_to_zero(stop_rad));
                    if start_v != stop_v {
                        break;
                    }
                }
            }
        }
        let ccw = !(sweep_angle > 0.0);

        if start_v == stop_v {
            let end_angle = degrees_to_radians(start_angle + sweep_angle);
            let radius_x = oval.width() / 2.0;
            let radius_y = oval.height() / 2.0;
            let single = Point::new(
                oval.center_x() + radius_x * end_angle.cos(),
                oval.center_y() + radius_y * end_angle.sin(),
            );
            return self.add_pt(single, force_move_to);
        }

        // build_arc_conics
        let mut matrix = Matrix::scale(oval.width() / 2.0, oval.height() / 2.0);
        matrix.post_translate(oval.center_x(), oval.center_y());
        let conics = Conic::build_unit_arc(start_v, stop_v, ccw, Some(&matrix));
        if conics.is_empty() {
            let single = matrix.map_point(stop_v);
            return self.add_pt(single, force_move_to);
        }
        self.add_pt(conics[0].pts[0], force_move_to);
        for c in &conics {
            self.conic_to(c.pts[1], c.pts[2], c.w);
        }
    }

    fn add_pt(&mut self, pt: Point, force_move_to: bool) {
        if force_move_to {
            self.move_to(pt);
        } else {
            let last = self.pts.last().copied().unwrap_or(Point::new(0.0, 0.0));
            if !(nearly_equal(last.x, pt.x) && nearly_equal(last.y, pt.y)) {
                self.line_to(pt);
            }
        }
    }

    /// `SkPathBuilder::transform`: points through the matrix, convexity reset.
    pub fn transform(&mut self, m: &Matrix) {
        m.map_points(&mut self.pts);
        self.last_move_point = m.map_point(self.last_move_point);
        self.convexity = Convexity::Unknown;
    }

    pub fn set_convexity(&mut self, c: Convexity) {
        self.convexity = c;
    }

    pub fn detach(self) -> Path {
        Path {
            verbs: self.verbs,
            pts: self.pts,
            conics: self.conics,
            fill_type: self.fill_type,
            convexity: self.convexity,
            segment_mask: self.segment_mask,
        }
    }
}

struct PointIter {
    n: usize,
    current: usize,
    advance: usize,
}
impl PointIter {
    fn new(n: usize, cw: bool, start: usize) -> PointIter {
        PointIter {
            n,
            current: start % n,
            advance: if cw { 1 } else { n - 1 },
        }
    }
    fn current(&self) -> usize {
        self.current
    }
    fn next(&mut self) -> usize {
        self.current = (self.current + self.advance) % self.n;
        self.current
    }
}

// ── Convexity (SkPathPriv::ComputeConvexity) ──────────────────────────────

fn is_axis_aligned(pts: &[Point]) -> bool {
    for i in 1..pts.len() {
        if pts[i - 1].x != pts[i].x && pts[i - 1].y != pts[i].y {
            return false;
        }
    }
    true
}

pub fn transform_convexity(m: &Matrix, pts: &[Point], convexity: Convexity) -> Convexity {
    if m.is_identity() || pts.is_empty() {
        return convexity;
    }
    if convexity.is_convex() {
        if !m.is_scale_translate() || !is_axis_aligned(pts) {
            return Convexity::Unknown;
        }
        let det = m.sx * m.sy - m.kx * m.ky;
        if det < 0.0 {
            return convexity.opposite();
        } else if det > 0.0 {
            return convexity;
        }
        return Convexity::ConvexDegenerate;
    }
    convexity
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirChange {
    Unknown,
    Left,
    Right,
    Straight,
    Backwards,
    Invalid,
}

struct Convexicator {
    first_pt: Point,
    first_vec: Point,
    last_pt: Point,
    last_vec: Point,
    expected_dir: DirChange,
    first_direction: Option<bool>, // Some(cw)
    reversals: i32,
    is_finite: bool,
}

impl Convexicator {
    fn new() -> Self {
        Convexicator {
            first_pt: Point::default(),
            first_vec: Point::default(),
            last_pt: Point::default(),
            last_vec: Point::default(),
            expected_dir: DirChange::Invalid,
            first_direction: None,
            reversals: 0,
            is_finite: true,
        }
    }
    fn set_move_pt(&mut self, pt: Point) {
        self.first_pt = pt;
        self.last_pt = pt;
        self.expected_dir = DirChange::Invalid;
    }
    fn add_pt(&mut self, pt: Point) -> bool {
        if self.last_pt == pt {
            return true;
        }
        if self.first_pt == self.last_pt
            && self.expected_dir == DirChange::Invalid
            && self.last_vec.is_zero()
        {
            self.last_vec = pt.sub(self.last_pt);
            self.first_vec = self.last_vec;
        } else if !self.add_vec(pt.sub(self.last_pt)) {
            return false;
        }
        self.last_pt = pt;
        true
    }
    fn is_concave_by_sign(points: &[Point]) -> bool {
        let count = points.len();
        if count <= 3 {
            return false;
        }
        let sign = |x: f32| -> i32 { (x < 0.0) as i32 };
        let mut curr = points[0];
        let first = curr;
        let mut dxes = 0;
        let mut dyes = 0;
        let mut last_sx = 2;
        let mut last_sy = 2;
        let mut i = 1;
        for outer in 0..2 {
            loop {
                let p = if outer == 0 {
                    if i >= count {
                        break;
                    }
                    let p = points[i];
                    i += 1;
                    p
                } else {
                    first
                };
                let vec = p.sub(curr);
                if !vec.is_zero() {
                    if !vec.is_finite() {
                        return true;
                    }
                    let sx = sign(vec.x);
                    let sy = sign(vec.y);
                    dxes += (sx != last_sx) as i32;
                    dyes += (sy != last_sy) as i32;
                    if dxes > 3 || dyes > 3 {
                        return true;
                    }
                    last_sx = sx;
                    last_sy = sy;
                }
                curr = p;
                if outer == 1 {
                    break;
                }
            }
        }
        false
    }
    fn close(&mut self) -> bool {
        let fp = self.first_pt;
        let fv = self.first_vec;
        self.add_pt(fp) && self.add_vec(fv)
    }
    fn direction_change(&self, cur: Point) -> DirChange {
        let cross = Point::cross(self.last_vec, cur);
        if !cross.is_finite() {
            return DirChange::Unknown;
        }
        if cross == 0.0 {
            return if Point::dot(self.last_vec, cur) < 0.0 {
                DirChange::Backwards
            } else {
                DirChange::Straight
            };
        }
        if cross > 0.0 {
            DirChange::Right
        } else {
            DirChange::Left
        }
    }
    fn add_vec(&mut self, cur: Point) -> bool {
        let dir = self.direction_change(cur);
        match dir {
            DirChange::Left | DirChange::Right => {
                if self.expected_dir == DirChange::Invalid {
                    self.expected_dir = dir;
                    self.first_direction = Some(dir == DirChange::Right);
                } else if dir != self.expected_dir {
                    self.first_direction = None;
                    return false;
                }
                self.last_vec = cur;
            }
            DirChange::Straight => {}
            DirChange::Backwards => {
                self.last_vec = cur;
                self.reversals += 1;
                return self.reversals < 3;
            }
            DirChange::Unknown => {
                self.is_finite = false;
                return false;
            }
            DirChange::Invalid => unreachable!(),
        }
        true
    }
}

fn pts_in_verb(v: Verb) -> usize {
    match v {
        Verb::Move => 1,
        Verb::Line => 1,
        Verb::Quad | Verb::Conic => 2,
        Verb::Cubic => 3,
        Verb::Close => 0,
    }
}

pub fn compute_convexity(points: &[Point], verbs: &[Verb]) -> Convexity {
    let mut vb_count = verbs.len();
    while vb_count > 0 && verbs[vb_count - 1] == Verb::Move {
        vb_count -= 1;
    }
    let delta = verbs.len() - vb_count;
    let points = &points[..points.len() - delta];
    let verbs = &verbs[..vb_count];
    if verbs.is_empty() {
        return Convexity::ConvexDegenerate;
    }
    if Convexicator::is_concave_by_sign(points) {
        return Convexity::Concave;
    }
    let mut contour_count = 0;
    let mut needs_close = false;
    let mut state = Convexicator::new();
    let mut pi = 0usize; // index of the current verb's first point (the point itself for Move)
    for &verb in verbs {
        let n = pts_in_verb(verb);
        if contour_count == 0 {
            if verb == Verb::Move {
                state.set_move_pt(points[pi]);
            } else {
                contour_count += 1;
                needs_close = true;
            }
        }
        if contour_count == 1 {
            if verb == Verb::Close || verb == Verb::Move {
                if !state.close() {
                    return Convexity::Concave;
                }
                needs_close = false;
                contour_count += 1;
            } else {
                for i in 0..n {
                    if !state.add_pt(points[pi + i]) {
                        return Convexity::Concave;
                    }
                }
            }
        } else if contour_count >= 2 && verb != Verb::Move {
            return Convexity::Concave;
        }
        pi += n;
    }
    if needs_close && !state.close() {
        return Convexity::Concave;
    }
    match state.first_direction {
        Some(cw) => Convexity::from_dir(cw),
        None => {
            if state.reversals >= 3 {
                Convexity::Concave
            } else {
                Convexity::ConvexDegenerate
            }
        }
    }
}

// ── Blink: canvas path ────────────────────────────────────────────────────

const TWO_PI_F: f32 = std::f32::consts::PI * 2.0;
const PI_F: f32 = std::f32::consts::PI;
const PI_OVER_TWO_F: f32 = std::f32::consts::FRAC_PI_2;

/// What actually gets drawn, as in Blink's `CanvasPath` with its fast paths.
#[derive(Clone, Debug)]
pub enum Drawable {
    Empty,
    /// A line segment: filling it is a no-op.
    Line(Point, Point),
    /// A single arc: `PaintCanvas::drawArc`.
    Arc {
        oval: Rect,
        start_deg: f32,
        sweep_deg: f32,
        closed: bool,
    },
    Path(Path),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineState {
    Empty,
    StartingPoint,
    Line,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArcState {
    Empty,
    Arc,
    Closed,
}

/// `CanvasPath` state: three builders, as in Blink.
pub struct CanvasPath {
    line_state: LineState,
    line_start: Point,
    line_end: Point,
    arc_state: ArcState,
    arc: (f32, f32, f32, f32, f32), // x, y, r, start, sweep
    pub builder: PathBuilder,
}

fn fmodf(a: f32, b: f32) -> f32 {
    a % b
}

fn adjust_end_angle(start: f32, end: f32, anticlockwise: bool) -> f32 {
    let mut new_end = end;
    if !anticlockwise && end - start >= TWO_PI_F {
        new_end = start + TWO_PI_F;
    } else if anticlockwise && start - end >= TWO_PI_F {
        new_end = start - TWO_PI_F;
    } else if !anticlockwise && start > end {
        new_end = start + (TWO_PI_F - fmodf(start - end, TWO_PI_F));
    } else if anticlockwise && start < end {
        new_end = start - (TWO_PI_F - fmodf(end - start, TWO_PI_F));
    }
    new_end
}

fn canonicalize_angle(start: &mut f32, end: &mut f32) {
    let mut new_start = fmodf(*start, TWO_PI_F);
    if new_start < 0.0 {
        new_start += TWO_PI_F;
        if new_start >= TWO_PI_F {
            new_start -= TWO_PI_F;
        }
    }
    let delta = new_start - *start;
    *start = new_start;
    *end += delta;
}

fn rad2deg(r: f32) -> f32 {
    // blink: Rad2deg(x) = x * 180 / kPiFloat
    r * 180.0 / PI_F
}

impl Default for CanvasPath {
    fn default() -> Self {
        Self::new()
    }
}

impl CanvasPath {
    pub fn new() -> CanvasPath {
        CanvasPath {
            line_state: LineState::Empty,
            line_start: Point::default(),
            line_end: Point::default(),
            arc_state: ArcState::Empty,
            arc: (0.0, 0.0, 0.0, 0.0, 0.0),
            builder: PathBuilder::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.line_state == LineState::Empty
            && self.arc_state == ArcState::Empty
            && self.builder.is_empty()
    }
    fn is_line(&self) -> bool {
        self.line_state == LineState::Line
    }
    fn is_arc(&self) -> bool {
        self.arc_state != ArcState::Empty
    }

    fn update_path_from_line_or_arc(&mut self) {
        let needs = (self.line_state != LineState::Empty || self.arc_state != ArcState::Empty)
            && self.builder.is_empty();
        if !needs {
            return;
        }
        if self.line_state != LineState::Empty {
            self.builder.move_to(self.line_start);
            if self.line_state == LineState::Line {
                self.builder.line_to(self.line_end);
            }
        } else {
            let (x, y, r, start, sweep) = self.arc;
            add_ellipse(
                &mut self.builder,
                Point::new(x, y),
                r,
                r,
                start,
                start + sweep,
            );
            if self.arc_state == ArcState::Closed {
                self.builder.close();
            }
        }
    }
    fn update_for_mutation(&mut self) {
        self.update_path_from_line_or_arc();
        self.line_state = LineState::Empty;
        self.arc_state = ArcState::Empty;
    }

    pub fn close_path(&mut self) {
        if self.is_empty() {
            return;
        }
        let pb = self.builder.bounds();
        let lb_zero = match self.line_state {
            LineState::StartingPoint => true,
            LineState::Line => {
                self.line_start.x == self.line_end.x && self.line_start.y == self.line_end.y
            }
            LineState::Empty => false,
        };
        if pb.width() == 0.0 && pb.height() == 0.0 && self.is_line() && lb_zero {
            let p = self.builder.last_pt();
            *self = CanvasPath::new();
            if let Some(p) = p {
                self.move_to(p.x, p.y);
            }
            return;
        }
        if self.is_arc() {
            if self.arc_state != ArcState::Closed {
                self.builder.reset();
                self.arc_state = ArcState::Closed;
            }
        } else {
            self.update_for_mutation();
            self.builder.close();
        }
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let p = Point::new(x, y);
        if self.is_empty() {
            self.line_state = LineState::StartingPoint;
            self.line_start = p;
        } else {
            self.update_for_mutation();
            self.builder.move_to(p);
        }
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let p = Point::new(x, y);
        if self.is_empty() {
            self.line_state = LineState::StartingPoint;
            self.line_start = p;
        }
        if self.line_state == LineState::StartingPoint {
            self.builder.reset();
            self.line_state = LineState::Line;
            self.line_end = p;
            return;
        }
        self.update_for_mutation();
        self.builder.line_to(p);
    }

    pub fn quadratic_curve_to(&mut self, cpx: f32, cpy: f32, x: f32, y: f32) {
        if ![cpx, cpy, x, y].iter().all(|v| v.is_finite()) {
            return;
        }
        self.update_for_mutation();
        if self.builder.last_pt().is_none() {
            self.builder.move_to(Point::new(cpx, cpy));
        }
        self.builder.quad_to(Point::new(cpx, cpy), Point::new(x, y));
    }

    pub fn bezier_curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        if ![c1x, c1y, c2x, c2y, x, y].iter().all(|v| v.is_finite()) {
            return;
        }
        self.update_for_mutation();
        if self.builder.last_pt().is_none() {
            self.builder.move_to(Point::new(c1x, c1y));
        }
        self.builder
            .cubic_to(Point::new(c1x, c1y), Point::new(c2x, c2y), Point::new(x, y));
    }

    pub fn arc(&mut self, x: f32, y: f32, radius: f32, start: f32, end: f32, anticlockwise: bool) {
        if ![x, y, radius, start, end].iter().all(|v| v.is_finite()) || radius < 0.0 {
            return;
        }
        self.update_for_mutation();
        if radius == 0.0 || start == end {
            self.line_to(x + radius * start.cos(), y + radius * start.sin());
            return;
        }
        let mut start = start;
        let mut end = end;
        canonicalize_angle(&mut start, &mut end);
        let end = adjust_end_angle(start, end, anticlockwise);
        if self.is_empty() && radius >= 1.0 {
            self.arc_state = ArcState::Arc;
            self.arc = (x, y, radius, start, end - start);
            return;
        }
        add_ellipse(
            &mut self.builder,
            Point::new(x, y),
            radius,
            radius,
            start,
            end,
        );
    }

    pub fn ellipse(
        &mut self,
        x: f32,
        y: f32,
        rx: f32,
        ry: f32,
        rotation: f32,
        start: f32,
        end: f32,
        anticlockwise: bool,
    ) {
        if ![x, y, rx, ry, rotation, start, end]
            .iter()
            .all(|v| v.is_finite())
            || rx < 0.0
            || ry < 0.0
        {
            return;
        }
        self.update_for_mutation();
        let mut start = start;
        let mut end = end;
        canonicalize_angle(&mut start, &mut end);
        let adjusted_end = adjust_end_angle(start, end, anticlockwise);
        if rx == 0.0 || ry == 0.0 || start == adjusted_end {
            self.degenerate_ellipse(x, y, rx, ry, rotation, start, adjusted_end, anticlockwise);
            return;
        }
        add_ellipse_rotated(
            &mut self.builder,
            Point::new(x, y),
            rx,
            ry,
            rotation,
            start,
            adjusted_end,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn degenerate_ellipse(
        &mut self,
        x: f32,
        y: f32,
        rx: f32,
        ry: f32,
        rotation: f32,
        start: f32,
        end: f32,
        anticlockwise: bool,
    ) {
        let center = Point::new(x, y);
        let rot = rotation_matrix(rotation);
        let pt = |theta: f32| -> Point {
            let p = rot.map_point(Point::new(rx * theta.cos(), ry * theta.sin()));
            Point::new(center.x + p.x, center.y + p.y)
        };
        let p = pt(start);
        self.line_to(p.x, p.y);
        if (rx == 0.0 && ry == 0.0) || start == end {
            return;
        }
        if !anticlockwise {
            let mut angle = start - fmodf(start, PI_OVER_TWO_F) + PI_OVER_TWO_F;
            while angle < end {
                let p = pt(angle);
                self.line_to(p.x, p.y);
                angle += PI_OVER_TWO_F;
            }
        } else {
            let mut angle = start - fmodf(start, PI_OVER_TWO_F);
            while angle > end {
                let p = pt(angle);
                self.line_to(p.x, p.y);
                angle -= PI_OVER_TWO_F;
            }
        }
        let p = pt(end);
        self.line_to(p.x, p.y);
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) {
            return;
        }
        if w == 0.0 && h == 0.0 {
            self.move_to(x, y);
            return;
        }
        self.update_for_mutation();
        self.builder
            .add_rect(&Rect::from_ltrb(x, y, x + w, y + h), true, 0);
    }

    /// Result for fill/stroke.
    pub fn drawable(&self) -> Drawable {
        if self.is_empty() {
            return Drawable::Empty;
        }
        if self.is_line() {
            return Drawable::Line(self.line_start, self.line_end);
        }
        if self.is_arc() {
            let (x, y, r, start, sweep) = self.arc;
            let clamp = |v: f32| if v.is_finite() { v } else { 0.0 };
            return Drawable::Arc {
                oval: Rect::from_ltrb(x - r, y - r, x + r, y + r),
                start_deg: clamp(start * 180.0 / PI_F),
                sweep_deg: clamp(sweep * 180.0 / PI_F),
                closed: self.arc_state == ArcState::Closed,
            };
        }
        let mut cp = CanvasPath {
            line_state: self.line_state,
            line_start: self.line_start,
            line_end: self.line_end,
            arc_state: self.arc_state,
            arc: self.arc,
            builder: self.builder.clone(),
        };
        cp.update_path_from_line_or_arc();
        Drawable::Path(cp.builder.detach())
    }
}

fn rotation_matrix(radians: f32) -> Matrix {
    // AffineTransform::RotateRadians: cos/sin in double, then a double matrix;
    // MapPoint computes in double and narrows. Same precision here.
    let c = (radians as f64).cos();
    let s = (radians as f64).sin();
    Matrix {
        sx: c as f32,
        kx: (-s) as f32,
        tx: 0.0,
        ky: s as f32,
        sy: c as f32,
        ty: 0.0,
    }
}

/// Blink's `PathBuilder::AddEllipse(p, rx, ry, start, end)`.
pub fn add_ellipse(b: &mut PathBuilder, p: Point, rx: f32, ry: f32, start: f32, end: f32) {
    let oval = Rect::from_ltrb(p.x - rx, p.y - ry, p.x + rx, p.y + ry);
    let start_deg = rad2deg(start);
    let sweep_deg = rad2deg(end - start);
    if nearly_equal_web(sweep_deg.abs(), 360.0) {
        let sweep180 = 180.0f32.copysign(sweep_deg);
        b.arc_to(&oval, start_deg, sweep180, false);
        b.arc_to(&oval, start_deg + sweep180, sweep180, false);
    } else {
        b.arc_to(&oval, start_deg, sweep_deg, false);
    }
}

/// `WebCoreFloatNearlyEqual(a, b)` = `SkScalarNearlyEqual` after
/// `ClampNonFiniteToZero` (tolerance 1/4096).
fn nearly_equal_web(a: f32, b: f32) -> bool {
    let c = |v: f32| if v.is_finite() { v } else { 0.0 };
    nearly_equal(c(a), c(b))
}

/// `PathBuilder::AddEllipse` with rotation: the path is moved into the
/// ellipse's frame, the arc added, then the path moved back.
pub fn add_ellipse_rotated(
    b: &mut PathBuilder,
    p: Point,
    rx: f32,
    ry: f32,
    rotation: f32,
    start: f32,
    end: f32,
) {
    if rotation == 0.0 {
        add_ellipse(b, p, rx, ry, start, end);
        return;
    }
    // AffineTransform::Translation(p).RotateRadians(rotation), in double.
    let c = (rotation as f64).cos();
    let s = (rotation as f64).sin();
    let fwd = Matrix {
        sx: c as f32,
        kx: (-s) as f32,
        tx: p.x,
        ky: s as f32,
        sy: c as f32,
        ty: p.y,
    };
    // Inverse: rotate by −rotation and translate by −p (double, like Inverse()).
    let inv = {
        let det = c * c + s * s;
        let (a, bb, cc, d) = (c / det, s / det, -s / det, c / det);
        let e = -(a * p.x as f64 + cc * p.y as f64);
        let f = -(bb * p.x as f64 + d * p.y as f64);
        Matrix {
            sx: a as f32,
            kx: cc as f32,
            tx: e as f32,
            ky: bb as f32,
            sy: d as f32,
            ty: f as f32,
        }
    };
    b.transform(&inv);
    add_ellipse(b, Point::new(0.0, 0.0), rx, ry, start, end);
    b.transform(&fwd);
}

/// `SkPathPriv::CreateDrawArcPath(arc, isFillNoPathEffect)` for arcs without
/// a centre (the kind canvas draws).
pub fn create_draw_arc_path(
    oval: &Rect,
    start_angle: f32,
    sweep_angle: f32,
    is_fill: bool,
) -> Path {
    let mut start = start_angle;
    let mut sweep = sweep_angle;
    if sweep.abs() > 3600.0 {
        sweep = 3600.0f32.copysign(sweep) + sweep % 360.0;
    }
    let mut b = PathBuilder::new();
    if is_fill && sweep.abs() >= 360.0 {
        b.add_oval(oval, true, 1);
        return b.detach();
    }
    let convex = sweep.abs() <= 360.0;
    let first_cw = sweep > 0.0;
    let mut force_move_to = true;
    while sweep <= -360.0 {
        b.arc_to(oval, start, -180.0, force_move_to);
        start -= 180.0;
        b.arc_to(oval, start, -180.0, false);
        start -= 180.0;
        force_move_to = false;
        sweep += 360.0;
    }
    while sweep >= 360.0 {
        b.arc_to(oval, start, 180.0, force_move_to);
        start += 180.0;
        b.arc_to(oval, start, 180.0, false);
        start += 180.0;
        force_move_to = false;
        sweep -= 360.0;
    }
    b.arc_to(oval, start, sweep, force_move_to);
    b.set_convexity(if convex {
        Convexity::from_dir(first_cw)
    } else {
        Convexity::Concave
    });
    b.detach()
}

/// `SkPath::Oval(oval)`, as `drawOval` draws it.
pub fn oval_path(oval: &Rect) -> Path {
    let mut b = PathBuilder::new();
    b.add_oval(oval, true, 1);
    b.detach()
}
