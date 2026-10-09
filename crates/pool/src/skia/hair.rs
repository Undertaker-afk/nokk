//! Anti-aliased hairlines: port of `SkScan_Antihair.cpp` and
//! `SkScan_Hairline.cpp` (butt cap). Skia uses this for canvas strokes
//! at most one device pixel wide.

use super::blit::Blitter;
use super::fixed::*;
use super::geometry::{chop_cubic_at_max_curvature, Conic, IRect, Point, Rect, SCALAR_NEARLY_ZERO};
use super::path::{Path, Verb};

const HLINE_STACK_BUFFER: usize = 100;

#[inline]
fn scale_alpha_by_coverage(value: u32, coverage: FDot6) -> u8 {
    ((value * coverage as u32) >> 6) as u8
}
#[inline]
fn fixed_to_alpha(f: Fixed) -> u32 {
    ((f >> 8) & 0xFF) as u32
}

fn call_hline_blitter(b: &mut dyn Blitter, mut x: i32, y: i32, mut count: i32, alpha: u8) {
    let mut runs = [0i16; HLINE_STACK_BUFFER + 1];
    let mut aa = [0u8; HLINE_STACK_BUFFER];
    while count > 0 {
        aa[0] = alpha;
        let n = count.min(HLINE_STACK_BUFFER as i32);
        runs[0] = n as i16;
        runs[n as usize] = 0;
        b.blit_anti_h(x, y, &aa, &runs);
        x += n;
        count -= n;
    }
}

enum Kind {
    HLine,
    Horish,
    VLine,
    Vertish,
}

fn draw_cap(
    kind: &Kind,
    b: &mut dyn Blitter,
    x: i32,
    fy: Fixed,
    slope: Fixed,
    coverage: FDot6,
) -> Fixed {
    match kind {
        Kind::HLine => {
            let fy = fy.wrapping_add(FIXED_HALF);
            let y = fixed_floor_to_int(fy);
            let a = fixed_to_alpha(fy);
            let ma = scale_alpha_by_coverage(a, coverage);
            if ma != 0 {
                call_hline_blitter(b, x, y, 1, ma);
            }
            let ma = scale_alpha_by_coverage(255 - a, coverage);
            if ma != 0 {
                call_hline_blitter(b, x, y - 1, 1, ma);
            }
            fy - FIXED_HALF
        }
        Kind::Horish => {
            let fy = fy.wrapping_add(FIXED_HALF);
            let lower_y = fixed_floor_to_int(fy);
            let a = fixed_to_alpha(fy);
            let a0 = scale_alpha_by_coverage(255 - a, coverage);
            let a1 = scale_alpha_by_coverage(a, coverage);
            b.blit_anti_v2(x, lower_y - 1, a0, a1);
            fy.wrapping_add(slope) - FIXED_HALF
        }
        Kind::VLine => {
            let fx = fy.wrapping_add(FIXED_HALF);
            let xx = fixed_floor_to_int(fx);
            let a = fixed_to_alpha(fx);
            let ma = scale_alpha_by_coverage(a, coverage);
            if ma != 0 {
                b.blit_v(xx, x, 1, ma);
            }
            let ma = scale_alpha_by_coverage(255 - a, coverage);
            if ma != 0 {
                b.blit_v(xx - 1, x, 1, ma);
            }
            fx - FIXED_HALF
        }
        Kind::Vertish => {
            let fx = fy.wrapping_add(FIXED_HALF);
            let xx = fixed_floor_to_int(fx);
            let a = fixed_to_alpha(fx);
            b.blit_anti_h2(
                xx - 1,
                x,
                scale_alpha_by_coverage(255 - a, coverage),
                scale_alpha_by_coverage(a, coverage),
            );
            fx.wrapping_add(slope) - FIXED_HALF
        }
    }
}

fn draw_line(
    kind: &Kind,
    b: &mut dyn Blitter,
    x: i32,
    stopx: i32,
    fy: Fixed,
    slope: Fixed,
) -> Fixed {
    match kind {
        Kind::HLine => {
            let count = stopx - x;
            let fy = fy.wrapping_add(FIXED_HALF);
            let y = fixed_floor_to_int(fy);
            let a = fixed_to_alpha(fy);
            if a != 0 {
                call_hline_blitter(b, x, y, count, a as u8);
            }
            let a = 255 - a;
            if a != 0 {
                call_hline_blitter(b, x, y - 1, count, a as u8);
            }
            fy - FIXED_HALF
        }
        Kind::Horish => {
            let mut fy = fy.wrapping_add(FIXED_HALF);
            let mut xx = x;
            loop {
                let lower_y = fixed_floor_to_int(fy);
                let a = fixed_to_alpha(fy);
                b.blit_anti_v2(xx, lower_y - 1, (255 - a) as u8, a as u8);
                fy = fy.wrapping_add(slope);
                xx += 1;
                if xx >= stopx {
                    break;
                }
            }
            fy - FIXED_HALF
        }
        Kind::VLine => {
            let fx = fy.wrapping_add(FIXED_HALF);
            let xx = fixed_floor_to_int(fx);
            let a = fixed_to_alpha(fx);
            if a != 0 {
                b.blit_v(xx, x, stopx - x, a as u8);
            }
            let a = 255 - a;
            if a != 0 {
                b.blit_v(xx - 1, x, stopx - x, a as u8);
            }
            fx - FIXED_HALF
        }
        Kind::Vertish => {
            let mut fx = fy.wrapping_add(FIXED_HALF);
            let mut yy = x;
            loop {
                let xx = fixed_floor_to_int(fx);
                let a = fixed_to_alpha(fx);
                b.blit_anti_h2(xx - 1, yy, (255 - a) as u8, a as u8);
                fx = fx.wrapping_add(slope);
                yy += 1;
                if yy >= stopx {
                    break;
                }
            }
            fx - FIXED_HALF
        }
    }
}

#[inline]
fn fastfixdiv(a: FDot6, b: FDot6) -> Fixed {
    left_shift(a, 16) / b
}
#[inline]
fn bad_int(x: i32) -> i32 {
    x & x.wrapping_neg()
}
#[inline]
fn any_bad_ints(a: i32, b: i32, c: i32, d: i32) -> bool {
    ((bad_int(a) | bad_int(b) | bad_int(c) | bad_int(d)) >> 31) != 0
}
#[inline]
fn fd6_frac(x: FDot6) -> FDot6 {
    x & (FDOT6_ONE - 1)
}
#[inline]
fn fd6_floor(x: FDot6) -> i32 {
    x >> 6
}
#[inline]
fn fd6_ceil(x: FDot6) -> i32 {
    (x + 63) >> 6
}
#[inline]
fn partial_pixel_coverage(pos: FDot6) -> FDot6 {
    fd6_frac(pos - 1) + 1
}

fn do_anti_hairline(
    mut x0: FDot6,
    mut y0: FDot6,
    mut x1: FDot6,
    mut y1: FDot6,
    clip: Option<&IRect>,
    b: &mut dyn Blitter,
) {
    if any_bad_ints(x0, y0, x1, y1) {
        return;
    }
    if abs32(x1 - x0) > (511 << 6) || abs32(y1 - y0) > (511 << 6) {
        let hx = (x0 >> 1) + (x1 >> 1);
        let hy = (y0 >> 1) + (y1 >> 1);
        do_anti_hairline(x0, y0, hx, hy, clip, b);
        do_anti_hairline(hx, hy, x1, y1, clip, b);
        return;
    }
    let (mut start_coverage, mut stop_coverage);
    let (mut istart, mut istop);
    let (mut fstart, slope);
    let kind;
    let mut clip = clip;
    if abs32(x1 - x0) > abs32(y1 - y0) {
        if x0 > x1 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
        }
        istart = fd6_floor(x0);
        istop = fd6_ceil(x1);
        if y0 == y1 {
            slope = 0;
            kind = Kind::HLine;
            fstart = fdot6_to_fixed(y0);
        } else {
            slope = fastfixdiv(y1 - y0, x1 - x0);
            let dx_to_center = FDOT6_HALF - fd6_frac(x0);
            fstart = fdot6_to_fixed(y0) + ((slope.wrapping_mul(dx_to_center) + FDOT6_HALF) >> 6);
            kind = Kind::Horish;
        }
        if istop - istart == 1 {
            start_coverage = x1 - x0;
            stop_coverage = 0;
        } else {
            start_coverage = FDOT6_ONE - fd6_frac(x0);
            stop_coverage = fd6_frac(x1);
        }
        if let Some(c) = clip {
            if istart >= c.right || istop <= c.left {
                return;
            }
            if istart < c.left {
                fstart = fstart.wrapping_add(slope.wrapping_mul(c.left - istart));
                istart = c.left;
                start_coverage = FDOT6_ONE;
                if istop - istart == 1 {
                    start_coverage = partial_pixel_coverage(x1);
                    stop_coverage = 0;
                }
            }
            if istop > c.right {
                istop = c.right;
                stop_coverage = 0;
            }
            if istart == istop {
                return;
            }
            let (mut top, mut bottom);
            if slope >= 0 {
                top = fixed_floor_to_int(fstart - FIXED_HALF);
                bottom = fixed_ceil_to_int(
                    fstart
                        .wrapping_add((istop - istart - 1).wrapping_mul(slope))
                        .wrapping_add(FIXED_HALF),
                );
            } else {
                bottom = fixed_ceil_to_int(fstart + FIXED_HALF);
                top = fixed_floor_to_int(
                    fstart.wrapping_add((istop - istart - 1).wrapping_mul(slope)) - FIXED_HALF,
                );
            }
            top -= 1;
            bottom += 1;
            if top >= c.bottom || bottom <= c.top {
                return;
            }
            if c.top <= top && c.bottom >= bottom {
                clip = None;
            }
        }
    } else {
        if y0 > y1 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
        }
        istart = fd6_floor(y0);
        istop = fd6_ceil(y1);
        if x0 == x1 {
            if y0 == y1 {
                return;
            }
            slope = 0;
            kind = Kind::VLine;
            fstart = fdot6_to_fixed(x0);
        } else {
            slope = fastfixdiv(x1 - x0, y1 - y0);
            let dy_to_center = FDOT6_HALF - fd6_frac(y0);
            fstart = fdot6_to_fixed(x0) + ((slope.wrapping_mul(dy_to_center) + FDOT6_HALF) >> 6);
            kind = Kind::Vertish;
        }
        if istop - istart == 1 {
            start_coverage = y1 - y0;
            stop_coverage = 0;
        } else {
            start_coverage = FDOT6_ONE - fd6_frac(y0);
            stop_coverage = fd6_frac(y1);
        }
        if let Some(c) = clip {
            if istart >= c.bottom || istop <= c.top {
                return;
            }
            if istart < c.top {
                fstart = fstart.wrapping_add(slope.wrapping_mul(c.top - istart));
                istart = c.top;
                start_coverage = FDOT6_ONE;
                if istop - istart == 1 {
                    start_coverage = partial_pixel_coverage(y1);
                    stop_coverage = 0;
                }
            }
            if istop > c.bottom {
                istop = c.bottom;
                stop_coverage = 0;
            }
            if istart == istop {
                return;
            }
            let (mut left, mut right);
            if slope >= 0 {
                left = fixed_floor_to_int(fstart - FIXED_HALF);
                right = fixed_ceil_to_int(
                    fstart
                        .wrapping_add((istop - istart - 1).wrapping_mul(slope))
                        .wrapping_add(FIXED_HALF),
                );
            } else {
                right = fixed_ceil_to_int(fstart + FIXED_HALF);
                left = fixed_floor_to_int(
                    fstart.wrapping_add((istop - istart - 1).wrapping_mul(slope)) - FIXED_HALF,
                );
            }
            left -= 1;
            right += 1;
            if left >= c.right || right <= c.left {
                return;
            }
            if c.left <= left && c.right >= right {
                clip = None;
            }
        }
    }
    // SkRectClipBlitter changes the arithmetic too, not just the clip
    // (blitAntiH2/V2 go through blitAntiH with runs), see blit::RectClipBlitter.
    let mut wrapped;
    let b: &mut dyn Blitter = match clip {
        Some(c) => {
            wrapped = super::blit::RectClipBlitter::new(b, *c);
            &mut wrapped
        }
        None => b,
    };
    fstart = draw_cap(&kind, b, istart, fstart, slope, start_coverage);
    istart += 1;
    let full_spans = istop - istart - (stop_coverage > 0) as i32;
    if full_spans > 0 {
        fstart = draw_line(&kind, b, istart, istart + full_spans, fstart, slope);
    }
    if stop_coverage > 0 {
        draw_cap(&kind, b, istop - 1, fstart, slope, stop_coverage);
    }
}

// ── SkLineClipper::IntersectLine ──────────────────────────────────────────

fn nested_lt(a: f32, b: f32, dim: f32) -> bool {
    a <= b && (a < b || dim > 0.0)
}
fn contains_no_empty_check(outer: &Rect, inner: &Rect) -> bool {
    outer.left <= inner.left
        && outer.top <= inner.top
        && outer.right >= inner.right
        && outer.bottom >= inner.bottom
}
fn sect_with_horizontal(src: &[Point; 2], y: f32) -> f32 {
    let dy = src[1].y - src[0].y;
    if dy.abs() <= SCALAR_NEARLY_ZERO {
        (0.5 * (src[0].x as f64 + src[1].x as f64)) as f32
    } else {
        let (x0, y0, x1, y1) = (
            src[0].x as f64,
            src[0].y as f64,
            src[1].x as f64,
            src[1].y as f64,
        );
        let result = x0 + (y as f64 - y0) * (x1 - x0) / (y1 - y0);
        let (lo, hi) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
        result.clamp(lo, hi) as f32
    }
}
fn sect_with_vertical(src: &[Point; 2], x: f32) -> f32 {
    let dx = src[1].x - src[0].x;
    if dx.abs() <= SCALAR_NEARLY_ZERO {
        (0.5 * (src[0].y as f64 + src[1].y as f64)) as f32
    } else {
        let (x0, y0, x1, y1) = (
            src[0].x as f64,
            src[0].y as f64,
            src[1].x as f64,
            src[1].y as f64,
        );
        (y0 + (x as f64 - x0) * (y1 - y0) / (x1 - x0)) as f32
    }
}

pub fn intersect_line(src: &[Point; 2], clip: &Rect) -> Option<[Point; 2]> {
    let bounds = Rect::bounds(src);
    if contains_no_empty_check(clip, &bounds) {
        return Some(*src);
    }
    if nested_lt(bounds.right, clip.left, bounds.width())
        || nested_lt(clip.right, bounds.left, bounds.width())
        || nested_lt(bounds.bottom, clip.top, bounds.height())
        || nested_lt(clip.bottom, bounds.top, bounds.height())
    {
        return None;
    }
    let (index0, index1) = if src[0].y < src[1].y { (0, 1) } else { (1, 0) };
    let mut tmp = *src;
    if tmp[index0].y < clip.top {
        tmp[index0] = Point::new(sect_with_horizontal(src, clip.top), clip.top);
    }
    if tmp[index1].y > clip.bottom {
        tmp[index1] = Point::new(sect_with_horizontal(src, clip.bottom), clip.bottom);
    }
    let (index0, index1) = if tmp[0].x < tmp[1].x { (0, 1) } else { (1, 0) };
    if tmp[index1].x <= clip.left || tmp[index0].x >= clip.right {
        if tmp[0].x != tmp[1].x || tmp[0].x < clip.left || tmp[0].x > clip.right {
            return None;
        }
    }
    if tmp[index0].x < clip.left {
        let y = sect_with_vertical(&tmp, clip.left);
        tmp[index0] = Point::new(clip.left, y);
    }
    if tmp[index1].x > clip.right {
        let y = sect_with_vertical(&tmp, clip.right);
        tmp[index1] = Point::new(clip.right, y);
    }
    Some(tmp)
}

/// `SkScan::AntiHairLineRgn`: polyline; `clip` is the window (None if unneeded).
fn anti_hair_line_rgn(src: &[Point], clip: Option<&IRect>, b: &mut dyn Blitter) {
    if src.is_empty() {
        return;
    }
    let max = 32767.0f32;
    let fixed_bounds = Rect::from_ltrb(-max, -max, max, max);
    let clip_bounds = clip.map(|c| {
        Rect::from_ltrb(
            c.left as f32 - 1.0,
            c.top as f32 - 1.0,
            c.right as f32 + 1.0,
            c.bottom as f32 + 1.0,
        )
    });
    for i in 0..src.len() - 1 {
        let seg = [src[i], src[i + 1]];
        let Some(mut pts) = intersect_line(&seg, &fixed_bounds) else {
            continue;
        };
        if let Some(cb) = &clip_bounds {
            match intersect_line(&pts, cb) {
                Some(p) => pts = p,
                None => continue,
            }
        }
        let x0 = scalar_to_fdot6(pts[0].x);
        let y0 = scalar_to_fdot6(pts[0].y);
        let x1 = scalar_to_fdot6(pts[1].x);
        let y1 = scalar_to_fdot6(pts[1].y);
        if let Some(c) = clip {
            let left = x0.min(x1);
            let top = y0.min(y1);
            let right = x0.max(x1);
            let bottom = y0.max(y1);
            let ir = IRect::from_ltrb(
                fd6_floor(left) - 1,
                fd6_floor(top) - 1,
                fd6_ceil(right) + 1,
                fd6_ceil(bottom) + 1,
            );
            if ir.intersect(c).is_none() {
                continue;
            }
            if !c.contains(&ir) {
                do_anti_hairline(x0, y0, x1, y1, Some(c), b);
                continue;
            }
        }
        do_anti_hairline(x0, y0, x1, y1, None, b);
    }
}

// ── SkScan_Hairline: curves to polylines ─────────────────────────────────

const MAX_CUBIC_SUBDIVIDE_LEVEL: usize = 9;
const MAX_QUAD_SUBDIVIDE_LEVEL: usize = 5;

fn compute_int_quad_dist(pts: &[Point; 3]) -> u32 {
    let dx = ((pts[0].x + pts[2].x) / 2.0 - pts[1].x).abs();
    let dy = ((pts[0].y + pts[2].y) / 2.0 - pts[1].y).abs();
    let idx = ceil2int(dx) as u32;
    let idy = ceil2int(dy) as u32;
    if idx > idy {
        idx + (idy >> 1)
    } else {
        idy + (idx >> 1)
    }
}

fn compute_quad_level(pts: &[Point; 3]) -> usize {
    let d = compute_int_quad_dist(pts);
    let level = ((33 - clz(d)) >> 1) as usize;
    level.min(MAX_QUAD_SUBDIVIDE_LEVEL)
}

fn hair_quad(pts: &[Point; 3], clip: Option<&IRect>, b: &mut dyn Blitter, level: usize) {
    // SkQuadCoeff
    let c = pts[0];
    let p1 = pts[1];
    let p2 = pts[2];
    let bx = 2.0 * (p1.x - c.x);
    let by = 2.0 * (p1.y - c.y);
    let ax = p2.x - 2.0 * p1.x + c.x;
    let ay = p2.y - 2.0 * p1.y + c.y;
    let lines = 1usize << level;
    let dt = 1.0f32 / lines as f32;
    let mut t = 0.0f32;
    let mut tmp: Vec<Point> = Vec::with_capacity(lines + 1);
    tmp.push(pts[0]);
    let mut finite = true;
    for _ in 1..lines {
        t += dt;
        let px = (ax * t + bx) * t + c.x;
        let py = (ay * t + by) * t + c.y;
        finite &= px.is_finite() && py.is_finite();
        tmp.push(Point::new(px, py));
    }
    if finite {
        tmp.push(pts[2]);
        anti_hair_line_rgn(&tmp, clip, b);
    }
}

fn compute_cubic_segs(pts: &[Point; 4]) -> usize {
    let one_third = 1.0f32 / 3.0;
    let two_third = 2.0f32 / 3.0;
    let p13 = Point::new(
        one_third * pts[3].x + two_third * pts[0].x,
        one_third * pts[3].y + two_third * pts[0].y,
    );
    let p23 = Point::new(
        one_third * pts[0].x + two_third * pts[3].x,
        one_third * pts[0].y + two_third * pts[3].y,
    );
    let d1 = Point::new((pts[1].x - p13.x).abs(), (pts[1].y - p13.y).abs());
    let d2 = Point::new((pts[2].x - p23.x).abs(), (pts[2].y - p23.y).abs());
    let diff = d1.x.max(d2.x).max(d1.y.max(d2.y));
    let mut tol = 1.0f32 / 8.0;
    for i in 0..MAX_CUBIC_SUBDIVIDE_LEVEL {
        if diff < tol {
            return 1 << i;
        }
        tol *= 4.0;
    }
    1 << MAX_CUBIC_SUBDIVIDE_LEVEL
}

fn lt_90(p0: Point, pivot: Point, p2: Point) -> bool {
    Point::dot(p0.sub(pivot), p2.sub(pivot)) >= 0.0
}
fn quick_cubic_niceness_check(pts: &[Point; 4]) -> bool {
    lt_90(pts[1], pts[0], pts[3])
        && lt_90(pts[2], pts[0], pts[3])
        && lt_90(pts[1], pts[3], pts[0])
        && lt_90(pts[2], pts[3], pts[0])
}

fn hair_cubic(pts: &[Point; 4], clip: Option<&IRect>, b: &mut dyn Blitter) {
    let lines = compute_cubic_segs(pts);
    if lines == 1 {
        anti_hair_line_rgn(&[pts[0], pts[3]], clip, b);
        return;
    }
    // SkCubicCoeff
    let (p0, p1, p2, p3) = (pts[0], pts[1], pts[2], pts[3]);
    let ax = p3.x + 3.0 * (p1.x - p2.x) - p0.x;
    let ay = p3.y + 3.0 * (p1.y - p2.y) - p0.y;
    let bx = 3.0 * (p2.x - 2.0 * p1.x + p0.x);
    let by = 3.0 * (p2.y - 2.0 * p1.y + p0.y);
    let cx = 3.0 * (p1.x - p0.x);
    let cy = 3.0 * (p1.y - p0.y);
    let dt = 1.0f32 / lines as f32;
    let mut t = 0.0f32;
    let mut tmp: Vec<Point> = Vec::with_capacity(lines + 1);
    tmp.push(pts[0]);
    let mut finite = true;
    for _ in 1..lines {
        t += dt;
        let px = ((ax * t + bx) * t + cx) * t + p0.x;
        let py = ((ay * t + by) * t + cy) * t + p0.y;
        finite &= px.is_finite() && py.is_finite();
        tmp.push(Point::new(px, py));
    }
    if finite {
        tmp.push(pts[3]);
        anti_hair_line_rgn(&tmp, clip, b);
    }
}

fn geometric_overlap(a: &Rect, b: &Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}
fn geometric_contains(outer: &Rect, inner: &Rect) -> bool {
    inner.right <= outer.right
        && inner.left >= outer.left
        && inner.bottom <= outer.bottom
        && inner.top >= outer.top
}

struct Params<'c> {
    clip: Option<&'c IRect>,
    inset: Option<Rect>,
    outset: Option<Rect>,
}

fn cull<'c>(bounds: &Rect, p: &Params<'c>) -> Option<Option<&'c IRect>> {
    if let (Some(inset), Some(outset)) = (&p.inset, &p.outset) {
        if !geometric_overlap(outset, bounds) {
            return None;
        } else if geometric_contains(inset, bounds) {
            return Some(None);
        }
    }
    Some(p.clip)
}

fn hairquad(pts: &[Point; 3], p: &Params, b: &mut dyn Blitter, level: usize) {
    let Some(clip) = cull(&Rect::bounds(pts), p) else {
        return;
    };
    hair_quad(pts, clip, b, level);
}

fn haircubic(pts: &[Point; 4], p: &Params, b: &mut dyn Blitter) {
    let Some(clip) = cull(&Rect::bounds(pts), p) else {
        return;
    };
    if quick_cubic_niceness_check(pts) {
        hair_cubic(pts, clip, b);
    } else {
        let chopped = chop_cubic_at_max_curvature(pts);
        let n = (chopped.len() - 1) / 3;
        for i in 0..n {
            let c = [
                chopped[i * 3],
                chopped[i * 3 + 1],
                chopped[i * 3 + 2],
                chopped[i * 3 + 3],
            ];
            hair_cubic(&c, clip, b);
        }
    }
}

fn hairconic(pts: &[Point; 3], w: f32, p: &Params, b: &mut dyn Blitter) {
    let (q, n) = Conic::new(pts[0], pts[1], pts[2], w).to_quads(0.25);
    for i in 0..n {
        let qq = [q[i * 2], q[i * 2 + 1], q[i * 2 + 2]];
        let level = compute_quad_level(&qq);
        hairquad(&qq, p, b, level);
    }
}

/// `SkScan::AntiHairPath` (butt cap): path in device coordinates,
/// `rclip` is the window.
pub fn anti_hair_path(path: &Path, rclip: &IRect, b: &mut dyn Blitter) {
    if path.is_empty() {
        return;
    }
    let ib = path.bounds().round_out();
    let ibounds = IRect::from_ltrb(ib.left - 1, ib.top - 1, ib.right + 1, ib.bottom + 1);
    if ibounds.intersect(rclip).is_none() {
        return;
    }
    let mut params = Params {
        clip: None,
        inset: None,
        outset: None,
    };
    if !rclip.contains(&ibounds) {
        params.clip = Some(rclip);
        let r = rclip.to_rect();
        params.outset = Some(Rect::from_ltrb(
            r.left - 1.0,
            r.top - 1.0,
            r.right + 1.0,
            r.bottom + 1.0,
        ));
        let inset = Rect::from_ltrb(r.left + 1.0, r.top + 1.0, r.right - 1.0, r.bottom - 1.0);
        params.inset = Some(if inset.left > inset.right || inset.top > inset.bottom {
            Rect::default()
        } else {
            inset
        });
    }
    let mut first_pt = Point::default();
    let mut last_pt = Point::default();
    let mut pi = 0usize;
    let mut ci = 0usize;
    for &verb in &path.verbs {
        match verb {
            Verb::Move => {
                first_pt = path.pts[pi];
                last_pt = first_pt;
                pi += 1;
            }
            Verb::Line => {
                let pts = [path.pts[pi - 1], path.pts[pi]];
                anti_hair_line_rgn(&pts, params.clip, b);
                last_pt = pts[1];
                pi += 1;
            }
            Verb::Quad => {
                let pts = [path.pts[pi - 1], path.pts[pi], path.pts[pi + 1]];
                let level = compute_quad_level(&pts);
                hairquad(&pts, &params, b, level);
                last_pt = pts[2];
                pi += 2;
            }
            Verb::Conic => {
                let pts = [path.pts[pi - 1], path.pts[pi], path.pts[pi + 1]];
                let w = path.conics[ci];
                ci += 1;
                hairconic(&pts, w, &params, b);
                last_pt = pts[2];
                pi += 2;
            }
            Verb::Cubic => {
                let pts = [
                    path.pts[pi - 1],
                    path.pts[pi],
                    path.pts[pi + 1],
                    path.pts[pi + 2],
                ];
                haircubic(&pts, &params, b);
                last_pt = pts[3];
                pi += 3;
            }
            Verb::Close => {
                let pts = [last_pt, first_pt];
                anti_hair_line_rgn(&pts, params.clip, b);
            }
        }
    }
}

// ── SkScan::AntiFillRect ──────────────────────────────────────────────────

type FDot8 = i32;

fn fixed_to_fdot8(x: Fixed) -> FDot8 {
    x.wrapping_add(0x80) >> 8
}

/// `SkAlphaMul(value, alpha256)`.
fn alpha_mul(v: i32, a256: i32) -> u8 {
    ((v * a256) >> 8) as u8
}

fn do_scanline(l: FDot8, top: i32, r: FDot8, alpha: i32, b: &mut dyn Blitter) {
    if (l >> 8) == ((r - 1) >> 8) {
        b.blit_v(l >> 8, top, 1, alpha_mul(alpha, r - l));
        return;
    }
    let mut left = l >> 8;
    if l & 0xFF != 0 {
        b.blit_v(left, top, 1, alpha_mul(alpha, 256 - (l & 0xFF)));
        left += 1;
    }
    let rite = r >> 8;
    let width = rite - left;
    if width > 0 {
        call_hline_blitter(b, left, top, width, alpha as u8);
    }
    if r & 0xFF != 0 {
        b.blit_v(rite, top, 1, alpha_mul(alpha, r & 0xFF));
    }
}

fn antifilldot8(l: FDot8, t: FDot8, r: FDot8, bt: FDot8, b: &mut dyn Blitter, fill_inner: bool) {
    if l >= r || t >= bt {
        return;
    }
    let mut top = t >> 8;
    if top == ((bt - 1) >> 8) {
        do_scanline(l, top, r, bt - t - 1, b);
        return;
    }
    if t & 0xFF != 0 {
        do_scanline(l, top, r, 256 - (t & 0xFF), b);
        top += 1;
    }
    let bot = bt >> 8;
    let height = bot - top;
    if height > 0 {
        let mut left = l >> 8;
        if left == ((r - 1) >> 8) {
            b.blit_v(left, top, height, (r - l - 1) as u8);
        } else {
            if l & 0xFF != 0 {
                b.blit_v(left, top, height, (256 - (l & 0xFF)) as u8);
                left += 1;
            }
            let rite = r >> 8;
            let width = rite - left;
            if width > 0 && fill_inner {
                b.blit_rect(left, top, width, height);
            }
            if r & 0xFF != 0 {
                b.blit_v(rite, top, height, (r & 0xFF) as u8);
            }
        }
    }
    if bt & 0xFF != 0 {
        do_scanline(l, bot, r, bt & 0xFF, b);
    }
}

/// `SkScan::AntiFillRect(rect, clip, blitter)` for a rect clip: the
/// `drawRect` route (no quarter-pixel edge snapping, unlike paths).
pub fn anti_fill_rect(orig: &Rect, clip: &IRect, b: &mut dyn Blitter) {
    let Some(r) = clip.to_rect().intersect(orig) else {
        return;
    };
    // XRect_set: SkScalarToFixed = saturate2int(x · 65536), truncating.
    let fx = |v: f32| saturate2int(v * 65536.0);
    antifilldot8(
        fixed_to_fdot8(fx(r.left)),
        fixed_to_fdot8(fx(r.top)),
        fixed_to_fdot8(fx(r.right)),
        fixed_to_fdot8(fx(r.bottom)),
        b,
        true,
    );
}
