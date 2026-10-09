//! Skia analytic anti-aliasing: port of `SkScan_AAAPath.cpp` at the
//! Chrome 151 revision: additive blitters (mask and RLE), trapezoid
//! coverage, convex and general edge walking, `AAAFillPath`.

use super::blit::Blitter;
use super::edge::{Edge, EdgeBuilder, EdgeType, IRectF, NIL};
use super::fixed::*;
use super::geometry::IRect;
use super::path::{FillType, Path};

// ── AlphaRuns (SkAlphaRuns) ───────────────────────────────────────────────

pub struct AlphaRuns {
    pub runs: Vec<i16>,
    pub alpha: Vec<u8>,
    width: usize,
}

impl AlphaRuns {
    pub fn new(width: usize) -> AlphaRuns {
        let mut r = AlphaRuns {
            runs: vec![0; width + 1],
            alpha: vec![0; width + 1],
            width,
        };
        r.reset(width);
        r
    }
    pub fn reset(&mut self, width: usize) {
        self.width = width;
        self.runs[0] = width as i16;
        self.runs[width] = 0;
        self.alpha[0] = 0;
    }
    #[inline]
    pub fn catch_overflow(alpha: i32) -> u8 {
        (alpha - (alpha >> 8)) as u8
    }
    pub fn is_empty(&self) -> bool {
        self.alpha[0] == 0 && self.runs[self.runs[0] as usize] == 0
    }
    /// `SkAlphaRuns::Break(runs, alpha, x, count)` from offset `off`.
    fn break_run(runs: &mut [i16], alpha: &mut [u8], off: usize, x: usize, count: usize) {
        let mut ri = off;
        let mut x = x;
        let next = ri + x;
        while x > 0 {
            let n = runs[ri] as usize;
            if x < n {
                alpha[ri + x] = alpha[ri];
                runs[ri] = x as i16;
                runs[ri + x] = (n - x) as i16;
                break;
            }
            ri += n;
            x -= n;
        }
        let mut ri = next;
        let mut x = count;
        loop {
            let n = runs[ri] as usize;
            if x < n {
                alpha[ri + x] = alpha[ri];
                runs[ri] = x as i16;
                runs[ri + x] = (n - x) as i16;
                break;
            }
            x = x.wrapping_sub(n);
            if x == 0 || (x as isize) <= 0 {
                break;
            }
            ri += n;
        }
    }
    /// `SkAlphaRuns::add`: returns the new offsetX.
    pub fn add(
        &mut self,
        x: usize,
        start_alpha: u8,
        mut middle_count: usize,
        stop_alpha: u8,
        max_value: u8,
        offset_x: usize,
    ) -> usize {
        let mut off = offset_x;
        let mut last_alpha = off;
        let mut x = x - offset_x;
        if start_alpha != 0 {
            AlphaRuns::break_run(&mut self.runs, &mut self.alpha, off, x, 1);
            let tmp = self.alpha[off + x] as u32 + start_alpha as u32;
            self.alpha[off + x] = (tmp - (tmp >> 8)) as u8;
            off += x + 1;
            x = 0;
        }
        if middle_count != 0 {
            AlphaRuns::break_run(&mut self.runs, &mut self.alpha, off, x, middle_count);
            off += x;
            x = 0;
            loop {
                self.alpha[off] =
                    AlphaRuns::catch_overflow(self.alpha[off] as i32 + max_value as i32);
                let n = self.runs[off] as usize;
                off += n;
                middle_count = middle_count.saturating_sub(n);
                if middle_count == 0 {
                    break;
                }
            }
            last_alpha = off;
        }
        if stop_alpha != 0 {
            AlphaRuns::break_run(&mut self.runs, &mut self.alpha, off, x, 1);
            off += x;
            self.alpha[off] = self.alpha[off].wrapping_add(stop_alpha);
            last_alpha = off;
        }
        last_alpha
    }
}

// ── Additive blitters ────────────────────────────────────────────────────

#[inline]
fn add_alpha(a: &mut u8, delta: u8) {
    *a = AlphaRuns::catch_overflow(*a as i32 + delta as i32);
}
#[inline]
fn safely_add_alpha(a: &mut u8, delta: u8) {
    *a = (*a as i32 + delta as i32).min(0xFF) as u8;
}

enum Kind {
    Mask,
    Run,
    SafeRun,
}

/// Skia's three additive blitters in one: `MaskAdditiveBlitter`,
/// `RunBasedAdditiveBlitter` and `SafeRLEAdditiveBlitter`.
pub struct Additive<'a> {
    kind: Kind,
    real: &'a mut dyn Blitter,
    // mask
    mask: Vec<u8>,
    mask_bounds: IRect,
    mask_row_bytes: usize,
    clip_rect: IRect,
    // RLE
    curr_y: i32,
    width: i32,
    left: i32,
    top: i32,
    runs: AlphaRuns,
    offset_x: usize,
}

impl<'a> Additive<'a> {
    const MAX_WIDTH: i32 = 32;
    const MAX_STORAGE: i64 = 1024;

    pub fn can_handle_rect(bounds: &IRect) -> bool {
        let width = bounds.width();
        if width > Self::MAX_WIDTH {
            return false;
        }
        let rb = ((width + 3) & !3) as i64;
        let storage = rb * bounds.height() as i64;
        width <= Self::MAX_WIDTH && storage <= Self::MAX_STORAGE
    }

    pub fn mask(real: &'a mut dyn Blitter, ir: &IRect, clip_bounds: &IRect) -> Additive<'a> {
        let clip_rect = ir.intersect(clip_bounds).unwrap_or_default();
        let row_bytes = ir.width() as usize;
        // +2: one byte on each side for rounding error, as in Skia
        // (fStorage + 1 plus slack).
        let mask = vec![0u8; ir.height() as usize * row_bytes + 2];
        Additive {
            kind: Kind::Mask,
            real,
            mask,
            mask_bounds: *ir,
            mask_row_bytes: row_bytes,
            clip_rect,
            curr_y: 0,
            width: 0,
            left: 0,
            top: 0,
            runs: AlphaRuns::new(1),
            offset_x: 0,
        }
    }

    fn run_based(
        real: &'a mut dyn Blitter,
        ir: &IRect,
        clip_bounds: &IRect,
        safe: bool,
    ) -> Additive<'a> {
        let sect = ir.intersect(clip_bounds).unwrap_or_default();
        let left = sect.left;
        let width = sect.right - left;
        let top = sect.top;
        Additive {
            kind: if safe { Kind::SafeRun } else { Kind::Run },
            real,
            mask: Vec::new(),
            mask_bounds: IRect::default(),
            mask_row_bytes: 0,
            clip_rect: IRect::default(),
            curr_y: top - 1,
            width,
            left,
            top,
            runs: AlphaRuns::new(width.max(1) as usize),
            offset_x: 0,
        }
    }

    pub fn is_mask(&self) -> bool {
        matches!(self.kind, Kind::Mask)
    }

    /// Offset of row `y` in the mask such that `row + x` is pixel (x, y).
    /// The index carries a +1 byte bias (like `fStorage + 1`) so that
    /// writing at x = left − 1 stays inside the buffer.
    #[inline]
    fn row_index(&self, y: i32) -> isize {
        1 + (y - self.mask_bounds.top) as isize * self.mask_row_bytes as isize
            - self.mask_bounds.left as isize
    }
    #[inline]
    fn mask_at(&mut self, y: i32, x: i32) -> &mut u8 {
        let i = self.row_index(y) + x as isize;
        &mut self.mask[i as usize]
    }

    // ── shared AdditiveBlitter interface ──

    pub fn blit_anti_h_run(&mut self, x: i32, y: i32, alphas: &[u8], len: i32) {
        match self.kind {
            Kind::Mask => unreachable!("mask: add alphas directly"),
            Kind::Run | Kind::SafeRun => {
                self.check_y(y);
                let mut x = x - self.left;
                let mut alphas = alphas;
                let mut len = len;
                if x < 0 {
                    len += x;
                    alphas = &alphas[(-x) as usize..];
                    x = 0;
                }
                len = len.min(self.width - x);
                if len <= 0 {
                    return;
                }
                if x < self.offset_x as i32 {
                    self.offset_x = 0;
                }
                self.offset_x = self
                    .runs
                    .add(x as usize, 0, len as usize, 0, 0, self.offset_x);
                let xu = x as usize;
                let mut i = 0usize;
                while i < len as usize {
                    let n = self.runs.runs[xu + i] as usize;
                    for j in 1..n {
                        self.runs.runs[xu + i + j] = 1;
                        self.runs.alpha[xu + i + j] = self.runs.alpha[xu + i];
                    }
                    self.runs.runs[xu + i] = 1;
                    i += n;
                }
                let safe = matches!(self.kind, Kind::SafeRun);
                for i in 0..len as usize {
                    if safe {
                        safely_add_alpha(&mut self.runs.alpha[xu + i], alphas[i]);
                    } else {
                        add_alpha(&mut self.runs.alpha[xu + i], alphas[i]);
                    }
                }
            }
        }
    }

    pub fn blit_anti_h1(&mut self, x: i32, y: i32, alpha: u8) {
        match self.kind {
            Kind::Mask => {
                add_alpha(self.mask_at(y, x), alpha);
            }
            Kind::Run => {
                self.check_y(y);
                let x = x - self.left;
                if x < self.offset_x as i32 {
                    self.offset_x = 0;
                }
                if x >= 0 && x + 1 <= self.width {
                    self.offset_x = self.runs.add(x as usize, 0, 1, 0, alpha, self.offset_x);
                }
            }
            Kind::SafeRun => {
                self.check_y(y);
                let x = x - self.left;
                if x < self.offset_x as i32 {
                    self.offset_x = 0;
                }
                if x >= 0 && x + 1 <= self.width {
                    self.offset_x = self.runs.add(x as usize, 0, 1, 0, 0, self.offset_x);
                    safely_add_alpha(&mut self.runs.alpha[x as usize], alpha);
                }
            }
        }
    }

    pub fn blit_anti_h_w(&mut self, x: i32, y: i32, width: i32, alpha: u8) {
        match self.kind {
            Kind::Mask => {
                for i in 0..width {
                    add_alpha(self.mask_at(y, x + i), alpha);
                }
            }
            Kind::Run => {
                self.check_y(y);
                let x = x - self.left;
                if x < self.offset_x as i32 {
                    self.offset_x = 0;
                }
                if x >= 0 && x + width <= self.width {
                    self.offset_x =
                        self.runs
                            .add(x as usize, 0, width as usize, 0, alpha, self.offset_x);
                }
            }
            Kind::SafeRun => {
                self.check_y(y);
                let x = x - self.left;
                if x < self.offset_x as i32 {
                    self.offset_x = 0;
                }
                if x >= 0 && x + width <= self.width {
                    self.offset_x =
                        self.runs
                            .add(x as usize, 0, width as usize, 0, 0, self.offset_x);
                    let mut i = x as usize;
                    while (i as i32) < x + width {
                        safely_add_alpha(&mut self.runs.alpha[i], alpha);
                        i += self.runs.runs[i] as usize;
                    }
                }
            }
        }
    }

    // The "real" blitter for the mask is the mask itself (blitV/blitRect/blitAntiRect
    // write into it directly); for RLE it is the underlying one.

    pub fn real_blit_v(&mut self, x: i32, y: i32, height: i32, alpha: u8) {
        match self.kind {
            Kind::Mask => {
                if alpha == 0 {
                    return;
                }
                for i in 0..height {
                    *self.mask_at(y + i, x) = alpha;
                }
            }
            _ => self.real.blit_v(x, y, height, alpha),
        }
    }
    pub fn real_blit_rect(&mut self, x: i32, y: i32, width: i32, height: i32) {
        match self.kind {
            Kind::Mask => {
                for i in 0..height {
                    for j in 0..width {
                        *self.mask_at(y + i, x + j) = 0xFF;
                    }
                }
            }
            _ => self.real.blit_rect(x, y, width, height),
        }
    }
    pub fn real_blit_anti_rect(&mut self, x: i32, y: i32, width: i32, height: i32, l: u8, r: u8) {
        match self.kind {
            Kind::Mask => {
                self.real_blit_v(x, y, height, l);
                self.real_blit_v(x + 1 + width, y, height, r);
                self.real_blit_rect(x + 1, y, width, height);
            }
            _ => self.real.blit_anti_rect(x, y, width, height, l, r),
        }
    }
    pub fn real_blit_h(&mut self, x: i32, y: i32, len: i32) {
        match self.kind {
            Kind::Mask => self.real_blit_rect(x, y, len, 1),
            _ => self.real.blit_h(x, y, len),
        }
    }
    pub fn real_blit_anti_h2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        match self.kind {
            Kind::Mask => {
                // For the mask the "real" blitter is itself, and the default
                // SkBlitter::blitAntiH2 goes through blitAntiH: for the mask that
                // is a direct add.
                add_alpha(self.mask_at(y, x), a0);
                add_alpha(self.mask_at(y, x + 1), a1);
            }
            _ => self.real.blit_anti_h2(x, y, a0, a1),
        }
    }
    pub fn real_blit_anti_h(&mut self, x: i32, y: i32, alphas: &[u8], runs: &[i16]) {
        match self.kind {
            Kind::Mask => {
                let mut i = 0usize;
                let mut xx = x;
                loop {
                    let n = runs[i];
                    if n <= 0 {
                        break;
                    }
                    for k in 0..n as i32 {
                        add_alpha(self.mask_at(y, xx + k), alphas[i]);
                    }
                    xx += n as i32;
                    i += n as usize;
                }
            }
            _ => self.real.blit_anti_h(x, y, alphas, runs),
        }
    }

    pub fn get_width(&self) -> i32 {
        match self.kind {
            Kind::Mask => self.clip_rect.width(),
            _ => self.width,
        }
    }

    pub fn flush_if_y_changed(&mut self, y: Fixed, next_y: Fixed) {
        if matches!(self.kind, Kind::Mask) {
            return;
        }
        if fixed_floor_to_int(y) != fixed_floor_to_int(next_y) {
            self.flush();
        }
    }

    fn snap_alpha(alpha: u8) -> u8 {
        if alpha > 247 {
            0xFF
        } else if alpha < 8 {
            0
        } else {
            alpha
        }
    }

    fn flush(&mut self) {
        if self.curr_y >= self.top {
            let mut x = 0usize;
            while self.runs.runs[x] != 0 {
                self.runs.alpha[x] = Additive::snap_alpha(self.runs.alpha[x]);
                x += self.runs.runs[x] as usize;
            }
            if !self.runs.is_empty() {
                let (left, y) = (self.left, self.curr_y);
                self.real
                    .blit_anti_h(left, y, &self.runs.alpha, &self.runs.runs);
                let w = self.width as usize;
                self.runs.reset(w);
                self.offset_x = 0;
            }
            self.curr_y = self.top - 1;
        }
    }

    fn check_y(&mut self, y: i32) {
        if y != self.curr_y {
            self.flush();
            self.curr_y = y;
        }
    }

    /// Flush: the mask goes to the real blitter, RLE is flushed.
    pub fn finish(mut self) {
        match self.kind {
            Kind::Mask => {
                let bounds = self.mask_bounds;
                let clip = self.clip_rect;
                if !clip.is_empty() {
                    // The mask is stored with a +1 bias (see row_index).
                    let mask = &self.mask[1..];
                    self.real
                        .blit_mask(mask, &bounds, self.mask_row_bytes, &clip);
                }
            }
            _ => self.flush(),
        }
    }
}

// ── Coverage fractions ───────────────────────────────────────────────────

#[inline]
fn trapezoid_to_alpha(l1: Fixed, l2: Fixed) -> u8 {
    let area = (l1.wrapping_add(l2)) / 2;
    (area >> 8) as u8
}

#[inline]
fn partial_triangle_to_alpha(a: Fixed, b: Fixed) -> u8 {
    let area = (a >> 11).wrapping_mul(a >> 11).wrapping_mul(b >> 11);
    ((area >> 8) & 0xFF) as u8
}

#[inline]
fn get_partial_alpha_fixed(alpha: u8, partial_height: Fixed) -> u8 {
    fixed_round_to_int((alpha as i32).wrapping_mul(partial_height)) as u8
}
#[inline]
fn get_partial_alpha(alpha: u8, full_alpha: u8) -> u8 {
    ((alpha as u32 * full_alpha as u32) >> 8) as u8
}
#[inline]
fn fixed_to_alpha(f: Fixed) -> u8 {
    get_partial_alpha_fixed(0xFF, f)
}

#[inline]
fn approximate_intersection(mut l1: Fixed, mut r1: Fixed, mut l2: Fixed, mut r2: Fixed) -> Fixed {
    if l1 > r1 {
        std::mem::swap(&mut l1, &mut r1);
    }
    if l2 > r2 {
        std::mem::swap(&mut l2, &mut r2);
    }
    (l1.max(l2).wrapping_add(r1.min(r2))) / 2
}

fn compute_alpha_above_line(alphas: &mut [u8], l: Fixed, r: Fixed, d_y: Fixed, full_alpha: u8) {
    let rr = fixed_ceil_to_int(r);
    if rr == 0 {
    } else if rr == 1 {
        alphas[0] = get_partial_alpha((((rr << 17) - l - r) >> 9) as u8, full_alpha);
    } else {
        let first = FIXED_1 - l;
        let last = r - ((rr - 1) << 16);
        let first_h = fixed_mul(first, d_y);
        alphas[0] = (fixed_mul(first, first_h) >> 9) as u8;
        let mut alpha16 = sat_add(first_h, d_y >> 1);
        for i in 1..(rr - 1) as usize {
            alphas[i] = (alpha16 >> 8) as u8;
            alpha16 = sat_add(alpha16, d_y);
        }
        alphas[(rr - 1) as usize] = full_alpha.wrapping_sub(partial_triangle_to_alpha(last, d_y));
    }
}

fn compute_alpha_below_line(alphas: &mut [u8], l: Fixed, r: Fixed, d_y: Fixed, full_alpha: u8) {
    let rr = fixed_ceil_to_int(r);
    if rr == 0 {
    } else if rr == 1 {
        alphas[0] = get_partial_alpha(trapezoid_to_alpha(l, r), full_alpha);
    } else {
        let first = FIXED_1 - l;
        let last = r - ((rr - 1) << 16);
        let last_h = fixed_mul(last, d_y);
        alphas[(rr - 1) as usize] = (fixed_mul(last, last_h) >> 9) as u8;
        let mut alpha16 = sat_add(last_h, d_y >> 1);
        let mut i = rr - 2;
        while i > 0 {
            alphas[i as usize] = ((alpha16 >> 8) & 0xFF) as u8;
            alpha16 = sat_add(alpha16, d_y);
            i -= 1;
        }
        alphas[0] = full_alpha.wrapping_sub(partial_triangle_to_alpha(first, d_y));
    }
}

fn blit_single_alpha(
    b: &mut Additive,
    y: i32,
    x: i32,
    alpha: u8,
    full_alpha: u8,
    use_mask_row: bool,
    no_real_blitter: bool,
) {
    if use_mask_row {
        if full_alpha == 0xFF && !no_real_blitter {
            *b.mask_at(y, x) = alpha;
        } else {
            safely_add_alpha(b.mask_at(y, x), get_partial_alpha(alpha, full_alpha));
        }
    } else if full_alpha == 0xFF && !no_real_blitter {
        b.real_blit_v(x, y, 1, alpha);
    } else {
        b.blit_anti_h1(x, y, get_partial_alpha(alpha, full_alpha));
    }
}

fn blit_two_alphas(
    b: &mut Additive,
    y: i32,
    x: i32,
    a1: u8,
    a2: u8,
    full_alpha: u8,
    use_mask_row: bool,
    no_real_blitter: bool,
) {
    if use_mask_row {
        safely_add_alpha(b.mask_at(y, x), a1);
        safely_add_alpha(b.mask_at(y, x + 1), a2);
    } else if full_alpha == 0xFF && !no_real_blitter {
        b.real_blit_anti_h2(x, y, a1, a2);
    } else {
        b.blit_anti_h1(x, y, a1);
        b.blit_anti_h1(x + 1, y, a2);
    }
}

fn blit_full_alpha(
    b: &mut Additive,
    y: i32,
    x: i32,
    len: i32,
    full_alpha: u8,
    use_mask_row: bool,
    no_real_blitter: bool,
) {
    if use_mask_row {
        for i in 0..len {
            safely_add_alpha(b.mask_at(y, x + i), full_alpha);
        }
    } else if full_alpha == 0xFF && !no_real_blitter {
        b.real_blit_h(x, y, len);
    } else {
        b.blit_anti_h_w(x, y, len, full_alpha);
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_aaa_trapezoid_row(
    b: &mut Additive,
    y: i32,
    ul: Fixed,
    ur: Fixed,
    ll: Fixed,
    lr: Fixed,
    l_dy: Fixed,
    r_dy: Fixed,
    full_alpha: u8,
    use_mask_row: bool,
    no_real_blitter: bool,
) {
    let big_l = fixed_floor_to_int(ul);
    let big_r = fixed_ceil_to_int(lr);
    let len = big_r - big_l;
    if len == 1 {
        let alpha = trapezoid_to_alpha(ur - ul, lr - ll);
        blit_single_alpha(
            b,
            y,
            big_l,
            alpha,
            full_alpha,
            use_mask_row,
            no_real_blitter,
        );
        return;
    }
    let lenu = len as usize;
    let mut alphas = vec![full_alpha; lenu + 1];
    let mut temp = vec![0u8; lenu + 1];
    let mut runs = vec![1i16; lenu + 1];
    runs[lenu] = 0;

    let u_l = fixed_floor_to_int(ul);
    let l_l = fixed_ceil_to_int(ll);
    if u_l + 2 == l_l {
        let first = int_to_fixed(u_l) + FIXED_1 - ul;
        let second = ll - ul - first;
        let a1 = full_alpha.wrapping_sub(partial_triangle_to_alpha(first, l_dy));
        let a2 = partial_triangle_to_alpha(second, l_dy);
        alphas[0] = if alphas[0] > a1 { alphas[0] - a1 } else { 0 };
        alphas[1] = if alphas[1] > a2 { alphas[1] - a2 } else { 0 };
    } else {
        let off = (u_l - big_l) as usize;
        compute_alpha_below_line(
            &mut temp[off..],
            ul - int_to_fixed(u_l),
            ll - int_to_fixed(u_l),
            l_dy,
            full_alpha,
        );
        for i in u_l..l_l {
            let k = (i - big_l) as usize;
            if alphas[k] > temp[k] {
                alphas[k] -= temp[k];
            } else {
                alphas[k] = 0;
            }
        }
    }

    let u_r = fixed_floor_to_int(ur);
    let l_r = fixed_ceil_to_int(lr);
    if u_r + 2 == l_r {
        let first = int_to_fixed(u_r) + FIXED_1 - ur;
        let second = lr - ur - first;
        let a1 = partial_triangle_to_alpha(first, r_dy);
        let a2 = full_alpha.wrapping_sub(partial_triangle_to_alpha(second, r_dy));
        alphas[lenu - 2] = if alphas[lenu - 2] > a1 {
            alphas[lenu - 2] - a1
        } else {
            0
        };
        alphas[lenu - 1] = if alphas[lenu - 1] > a2 {
            alphas[lenu - 1] - a2
        } else {
            0
        };
    } else {
        let off = (u_r - big_l) as usize;
        compute_alpha_above_line(
            &mut temp[off..],
            ur - int_to_fixed(u_r),
            lr - int_to_fixed(u_r),
            r_dy,
            full_alpha,
        );
        for i in u_r..l_r {
            let k = (i - big_l) as usize;
            if alphas[k] > temp[k] {
                alphas[k] -= temp[k];
            } else {
                alphas[k] = 0;
            }
        }
    }

    if use_mask_row {
        for i in 0..lenu {
            safely_add_alpha(b.mask_at(y, big_l + i as i32), alphas[i]);
        }
    } else if full_alpha == 0xFF && !no_real_blitter {
        b.real_blit_anti_h(big_l, y, &alphas, &runs);
    } else {
        b.blit_anti_h_run(big_l, y, &alphas, len);
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_trapezoid_row(
    b: &mut Additive,
    y: i32,
    mut ul: Fixed,
    mut ur: Fixed,
    mut ll: Fixed,
    mut lr: Fixed,
    l_dy: Fixed,
    r_dy: Fixed,
    full_alpha: u8,
    use_mask_row: bool,
    no_real_blitter: bool,
) {
    if ul > ur {
        return;
    }
    if ll > lr {
        let v = approximate_intersection(ul, ll, ur, lr);
        ll = v;
        lr = v;
    }
    if ul == ur && ll == lr {
        return;
    }
    if ul > ll {
        std::mem::swap(&mut ul, &mut ll);
    }
    if ur > lr {
        std::mem::swap(&mut ur, &mut lr);
    }
    let join_left = fixed_ceil_to_fixed(ll);
    let join_rite = fixed_floor_to_fixed(ur);
    if join_left <= join_rite {
        if ul < join_left {
            let len = fixed_ceil_to_int(join_left - ul);
            if len == 1 {
                let alpha = trapezoid_to_alpha(join_left - ul, join_left - ll);
                blit_single_alpha(
                    b,
                    y,
                    ul >> 16,
                    alpha,
                    full_alpha,
                    use_mask_row,
                    no_real_blitter,
                );
            } else if len == 2 {
                let first = join_left - FIXED_1 - ul;
                let second = ll - ul - first;
                let a1 = partial_triangle_to_alpha(first, l_dy);
                let a2 = full_alpha.wrapping_sub(partial_triangle_to_alpha(second, l_dy));
                blit_two_alphas(
                    b,
                    y,
                    ul >> 16,
                    a1,
                    a2,
                    full_alpha,
                    use_mask_row,
                    no_real_blitter,
                );
            } else {
                blit_aaa_trapezoid_row(
                    b,
                    y,
                    ul,
                    join_left,
                    ll,
                    join_left,
                    l_dy,
                    MAX_S32,
                    full_alpha,
                    use_mask_row,
                    no_real_blitter,
                );
            }
        }
        if join_left < join_rite {
            blit_full_alpha(
                b,
                y,
                fixed_floor_to_int(join_left),
                fixed_floor_to_int(join_rite - join_left),
                full_alpha,
                use_mask_row,
                no_real_blitter,
            );
        }
        if lr > join_rite {
            let len = fixed_ceil_to_int(lr - join_rite);
            if len == 1 {
                let alpha = trapezoid_to_alpha(ur - join_rite, lr - join_rite);
                blit_single_alpha(
                    b,
                    y,
                    join_rite >> 16,
                    alpha,
                    full_alpha,
                    use_mask_row,
                    no_real_blitter,
                );
            } else if len == 2 {
                let first = join_rite + FIXED_1 - ur;
                let second = lr - ur - first;
                let a1 = full_alpha.wrapping_sub(partial_triangle_to_alpha(first, r_dy));
                let a2 = partial_triangle_to_alpha(second, r_dy);
                blit_two_alphas(
                    b,
                    y,
                    join_rite >> 16,
                    a1,
                    a2,
                    full_alpha,
                    use_mask_row,
                    no_real_blitter,
                );
            } else {
                blit_aaa_trapezoid_row(
                    b,
                    y,
                    join_rite,
                    ur,
                    join_rite,
                    lr,
                    MAX_S32,
                    r_dy,
                    full_alpha,
                    use_mask_row,
                    no_real_blitter,
                );
            }
        }
    } else {
        blit_aaa_trapezoid_row(
            b,
            y,
            ul,
            ur,
            ll,
            lr,
            l_dy,
            r_dy,
            full_alpha,
            use_mask_row,
            no_real_blitter,
        );
    }
}

// ── Edge list ────────────────────────────────────────────────────────────

struct EdgeList {
    e: Vec<Edge>,
    head: usize,
    tail: usize,
}

impl EdgeList {
    fn compare(a: &Edge, b: &Edge) -> std::cmp::Ordering {
        if a.upper_y != b.upper_y {
            return a.upper_y.cmp(&b.upper_y);
        }
        if a.x != b.x {
            return a.x.cmp(&b.x);
        }
        a.dx.cmp(&b.dx)
    }

    fn new(mut edges: Vec<Edge>) -> EdgeList {
        edges.sort_by(EdgeList::compare);
        let n = edges.len();
        let head = n;
        let tail = n + 1;
        let mut h = Edge::default();
        h.prev = NIL;
        h.next = 0;
        h.upper_y = MIN_S32;
        h.lower_y = MIN_S32;
        h.x = MIN_S32;
        h.dx = 0;
        h.dy = MAX_S32;
        h.upper_x = MIN_S32;
        let mut t = Edge::default();
        t.prev = n - 1;
        t.next = NIL;
        t.upper_y = MAX_S32;
        t.lower_y = MAX_S32;
        t.x = MAX_S32;
        t.dx = 0;
        t.dy = MAX_S32;
        t.upper_x = MAX_S32;
        for i in 0..n {
            edges[i].prev = if i == 0 { head } else { i - 1 };
            edges[i].next = if i + 1 == n { tail } else { i + 1 };
        }
        edges.push(h);
        edges.push(t);
        EdgeList {
            e: edges,
            head,
            tail,
        }
    }

    #[inline]
    fn remove(&mut self, i: usize) {
        let (p, n) = (self.e[i].prev, self.e[i].next);
        self.e[p].next = n;
        self.e[n].prev = p;
    }
    #[inline]
    fn insert_after(&mut self, i: usize, after: usize) {
        let n = self.e[after].next;
        self.e[i].prev = after;
        self.e[i].next = n;
        self.e[n].prev = i;
        self.e[after].next = i;
    }
    fn backward_insert_edge_based_on_x(&mut self, i: usize) {
        let x = self.e[i].x;
        let mut prev = self.e[i].prev;
        while self.e[prev].prev != NIL && self.e[prev].x > x {
            prev = self.e[prev].prev;
        }
        if self.e[prev].next != i {
            self.remove(i);
            self.insert_after(i, prev);
        }
    }
    fn backward_insert_start(&self, mut prev: usize, x: Fixed) -> usize {
        while self.e[prev].prev != NIL && self.e[prev].x > x {
            prev = self.e[prev].prev;
        }
        prev
    }
}

// ── Convex walk ──────────────────────────────────────────────────────────

fn is_smooth_enough_edge(l: &mut EdgeList, this_e: usize, next_e: usize, _stop_y: i32) -> bool {
    let e = &l.e[this_e];
    if e.curve_count < 0 {
        let ddshift = e.curve_shift as i32;
        return abs32(e.cdx) >> 1 >= abs32(e.cddx) >> ddshift
            && abs32(e.cdy) >> 1 >= abs32(e.cddy) >> ddshift
            && (e.cdy.wrapping_sub(e.cddy >> ddshift)) >> e.cubic_dshift >= FIXED_1;
    } else if e.curve_count > 0 {
        return abs32(e.qdx) >> 1 >= abs32(e.qddx)
            && abs32(e.qdy) >> 1 >= abs32(e.qddy)
            && (e.qdy.wrapping_sub(e.qddy)) >> e.curve_shift >= FIXED_1;
    }
    let n = &l.e[next_e];
    abs32(sat_sub(n.dx, e.dx)) <= FIXED_1 && n.lower_y.wrapping_sub(n.upper_y) >= FIXED_1
}

fn is_smooth_enough(
    l: &mut EdgeList,
    left_e: usize,
    rite_e: usize,
    curr_e: usize,
    stop_y: i32,
) -> bool {
    if l.e[curr_e].upper_y >= left_shift(stop_y, 16) {
        return false;
    }
    if l.e[left_e].lower_y.wrapping_add(FIXED_1) < l.e[rite_e].lower_y {
        return is_smooth_enough_edge(l, left_e, curr_e, stop_y);
    } else if l.e[left_e].lower_y > l.e[rite_e].lower_y.wrapping_add(FIXED_1) {
        return is_smooth_enough_edge(l, rite_e, curr_e, stop_y);
    }
    let mut curr_e = curr_e;
    let mut next_curr_e = l.e[curr_e].next;
    if l.e[next_curr_e].upper_y >= left_shift(stop_y, 16) {
        return false;
    }
    if l.e[next_curr_e].upper_x < l.e[curr_e].upper_x {
        std::mem::swap(&mut curr_e, &mut next_curr_e);
    }
    is_smooth_enough_edge(l, left_e, curr_e, stop_y)
        && is_smooth_enough_edge(l, rite_e, next_curr_e, stop_y)
}

#[allow(clippy::too_many_arguments)]
fn aaa_walk_convex_edges(
    l: &mut EdgeList,
    b: &mut Additive,
    _start_y: i32,
    stop_y: i32,
    left_bound: Fixed,
    rite_bound: Fixed,
    is_using_mask: bool,
) {
    let mut left_e = l.e[l.head].next;
    let mut rite_e = l.e[left_e].next;
    let mut curr_e = l.e[rite_e].next;
    let mut y = l.e[left_e].upper_y.max(l.e[rite_e].upper_y);

    'walk: loop {
        while l.e[left_e].lower_y <= y {
            if !l.e[left_e].update() {
                if fixed_floor_to_int(l.e[curr_e].upper_y) >= stop_y {
                    break 'walk;
                }
                left_e = curr_e;
                curr_e = l.e[curr_e].next;
            }
        }
        while l.e[rite_e].lower_y <= y {
            if !l.e[rite_e].update() {
                if fixed_floor_to_int(l.e[curr_e].upper_y) >= stop_y {
                    break 'walk;
                }
                rite_e = curr_e;
                curr_e = l.e[curr_e].next;
            }
        }
        if fixed_floor_to_int(y) >= stop_y {
            break;
        }
        l.e[left_e].go_y(y);
        l.e[rite_e].go_y(y);
        if l.e[left_e].x > l.e[rite_e].x
            || (l.e[left_e].x == l.e[rite_e].x && l.e[left_e].dx > l.e[rite_e].dx)
        {
            std::mem::swap(&mut left_e, &mut rite_e);
        }
        let mut local_bot_fixed = l.e[left_e].lower_y.min(l.e[rite_e].lower_y);
        if is_smooth_enough(l, left_e, rite_e, curr_e, stop_y) {
            local_bot_fixed = fixed_ceil_to_fixed(local_bot_fixed);
        }
        local_bot_fixed = local_bot_fixed.min(int_to_fixed(stop_y));

        let mut left = left_bound.max(l.e[left_e].x);
        let d_left = l.e[left_e].dx;
        let mut rite = rite_bound.min(l.e[rite_e].x);
        let d_rite = l.e[rite_e].dx;
        if (d_left | d_rite) == 0 {
            let full_left = fixed_ceil_to_int(left);
            let full_rite = fixed_floor_to_int(rite);
            let partial_left = int_to_fixed(full_left) - left;
            let partial_rite = rite - int_to_fixed(full_rite);
            let full_top = fixed_ceil_to_int(y);
            let full_bot = fixed_floor_to_int(local_bot_fixed);
            let mut partial_top = int_to_fixed(full_top) - y;
            let mut partial_bot = local_bot_fixed - int_to_fixed(full_bot);
            if full_top > full_bot {
                partial_top -= FIXED_1 - partial_bot;
                partial_bot = 0;
            }
            if full_rite >= full_left {
                if partial_top > 0 {
                    if partial_left > 0 {
                        b.blit_anti_h1(
                            full_left - 1,
                            full_top - 1,
                            fixed_to_alpha(fixed_mul(partial_top, partial_left)),
                        );
                    }
                    b.blit_anti_h_w(
                        full_left,
                        full_top - 1,
                        full_rite - full_left,
                        fixed_to_alpha(partial_top),
                    );
                    if partial_rite > 0 {
                        b.blit_anti_h1(
                            full_rite,
                            full_top - 1,
                            fixed_to_alpha(fixed_mul(partial_top, partial_rite)),
                        );
                    }
                    b.flush_if_y_changed(y, y + partial_top);
                }
                if full_bot > full_top
                    && (full_rite > full_left
                        || fixed_to_alpha(partial_left) > 0
                        || fixed_to_alpha(partial_rite) > 0)
                {
                    b.real_blit_anti_rect(
                        full_left - 1,
                        full_top,
                        full_rite - full_left,
                        full_bot - full_top,
                        fixed_to_alpha(partial_left),
                        fixed_to_alpha(partial_rite),
                    );
                }
                if partial_bot > 0 {
                    if partial_left > 0 {
                        b.blit_anti_h1(
                            full_left - 1,
                            full_bot,
                            fixed_to_alpha(fixed_mul(partial_bot, partial_left)),
                        );
                    }
                    b.blit_anti_h_w(
                        full_left,
                        full_bot,
                        full_rite - full_left,
                        fixed_to_alpha(partial_bot),
                    );
                    if partial_rite > 0 {
                        b.blit_anti_h1(
                            full_rite,
                            full_bot,
                            fixed_to_alpha(fixed_mul(partial_bot, partial_rite)),
                        );
                    }
                }
            } else {
                let width = rite - left;
                if width > 0 {
                    if partial_top > 0 {
                        b.blit_anti_h_w(
                            full_left - 1,
                            full_top - 1,
                            1,
                            fixed_to_alpha(fixed_mul(partial_top, width)),
                        );
                        b.flush_if_y_changed(y, y + partial_top);
                    }
                    if full_bot > full_top {
                        b.real_blit_v(
                            full_left - 1,
                            full_top,
                            full_bot - full_top,
                            fixed_to_alpha(width),
                        );
                    }
                    if partial_bot > 0 {
                        b.blit_anti_h_w(
                            full_left - 1,
                            full_bot,
                            1,
                            fixed_to_alpha(fixed_mul(partial_bot, width)),
                        );
                    }
                }
            }
            y = local_bot_fixed;
        } else {
            const SNAP_DIGIT: Fixed = FIXED_1 >> 4;
            const SNAP_HALF: Fixed = SNAP_DIGIT >> 1;
            const SNAP_MASK: Fixed = -1 ^ (SNAP_DIGIT - 1);
            left += SNAP_HALF;
            rite += SNAP_HALF;
            let mut count = fixed_ceil_to_int(local_bot_fixed) - fixed_floor_to_int(y);
            let l_dy = l.e[left_e].dy;
            let r_dy = l.e[rite_e].dy;
            if count > 1 {
                if (y & !0xFFFF) != y {
                    count -= 1;
                    let next_y = fixed_ceil_to_fixed(y + 1);
                    let d_y = next_y - y;
                    let next_left = left + fixed_mul(d_left, d_y);
                    let next_rite = rite + fixed_mul(d_rite, d_y);
                    blit_trapezoid_row(
                        b,
                        y >> 16,
                        left & SNAP_MASK,
                        rite & SNAP_MASK,
                        next_left & SNAP_MASK,
                        next_rite & SNAP_MASK,
                        l_dy,
                        r_dy,
                        get_partial_alpha_fixed(0xFF, d_y),
                        is_using_mask,
                        false,
                    );
                    b.flush_if_y_changed(y, next_y);
                    left = next_left;
                    rite = next_rite;
                    y = next_y;
                }
                while count > 1 {
                    count -= 1;
                    let next_y = y + FIXED_1;
                    let next_left = left + d_left;
                    let next_rite = rite + d_rite;
                    blit_trapezoid_row(
                        b,
                        y >> 16,
                        left & SNAP_MASK,
                        rite & SNAP_MASK,
                        next_left & SNAP_MASK,
                        next_rite & SNAP_MASK,
                        l_dy,
                        r_dy,
                        0xFF,
                        is_using_mask,
                        false,
                    );
                    b.flush_if_y_changed(y, next_y);
                    left = next_left;
                    rite = next_rite;
                    y = next_y;
                }
            }
            let d_y = local_bot_fixed - y;
            let next_left = (left + fixed_mul(d_left, d_y)).max(left_bound + SNAP_HALF);
            let next_rite = (rite + fixed_mul(d_rite, d_y)).min(rite_bound + SNAP_HALF);
            blit_trapezoid_row(
                b,
                y >> 16,
                left & SNAP_MASK,
                rite & SNAP_MASK,
                next_left & SNAP_MASK,
                next_rite & SNAP_MASK,
                l_dy,
                r_dy,
                get_partial_alpha_fixed(0xFF, d_y),
                is_using_mask,
                false,
            );
            b.flush_if_y_changed(y, local_bot_fixed);
            left = next_left;
            rite = next_rite;
            y = local_bot_fixed;
            left -= SNAP_HALF;
            rite -= SNAP_HALF;
        }
        l.e[left_e].x = left;
        l.e[rite_e].x = rite;
        l.e[left_e].y = y;
        l.e[rite_e].y = y;
    }
}

// ── General walk ─────────────────────────────────────────────────────────

#[inline]
fn update_next_next_y(y: Fixed, next_y: Fixed, next_next_y: &mut Fixed) {
    *next_next_y = if y > next_y && y < *next_next_y {
        y
    } else {
        *next_next_y
    };
}

fn check_intersection(l: &EdgeList, i: usize, next_y: Fixed, next_next_y: &mut Fixed) {
    let p = l.e[i].prev;
    if l.e[p].prev != NIL && l.e[p].x.wrapping_add(l.e[p].dx) > l.e[i].x.wrapping_add(l.e[i].dx) {
        *next_next_y = next_y + (FIXED_1 >> super::edge::DEFAULT_ACCURACY);
    }
}

fn check_intersection_fwd(l: &EdgeList, i: usize, next_y: Fixed, next_next_y: &mut Fixed) {
    let n = l.e[i].next;
    if l.e[n].next != NIL && l.e[i].x.wrapping_add(l.e[i].dx) > l.e[n].x.wrapping_add(l.e[n].dx) {
        *next_next_y = next_y + (FIXED_1 >> super::edge::DEFAULT_ACCURACY);
    }
}

fn insert_new_edges(l: &mut EdgeList, mut new_edge: usize, y: Fixed, next_next_y: &mut Fixed) {
    if l.e[new_edge].upper_y > y {
        update_next_next_y(l.e[new_edge].upper_y, y, next_next_y);
        return;
    }
    let prev = l.e[new_edge].prev;
    if l.e[prev].x <= l.e[new_edge].x {
        while l.e[new_edge].upper_y <= y {
            check_intersection(l, new_edge, y, next_next_y);
            update_next_next_y(l.e[new_edge].lower_y, y, next_next_y);
            new_edge = l.e[new_edge].next;
        }
        update_next_next_y(l.e[new_edge].upper_y, y, next_next_y);
        return;
    }
    let mut start = l.backward_insert_start(prev, l.e[new_edge].x);
    loop {
        let next = l.e[new_edge].next;
        let mut skip = false;
        loop {
            if l.e[start].next == new_edge {
                skip = true;
                break;
            }
            let after = l.e[start].next;
            if l.e[after].x >= l.e[new_edge].x {
                break;
            }
            start = after;
        }
        if !skip {
            l.remove(new_edge);
            l.insert_after(new_edge, start);
        }
        check_intersection(l, new_edge, y, next_next_y);
        check_intersection_fwd(l, new_edge, y, next_next_y);
        update_next_next_y(l.e[new_edge].lower_y, y, next_next_y);
        start = new_edge;
        new_edge = next;
        if !(l.e[new_edge].upper_y <= y) {
            break;
        }
    }
    update_next_next_y(l.e[new_edge].upper_y, y, next_next_y);
}

fn edges_too_close(l: &EdgeList, prev: usize, next: usize, lower_y: Fixed) -> bool {
    const SLACK: Fixed = FIXED_1;
    next != NIL
        && prev != NIL
        && l.e[next].upper_y < lower_y
        && l.e[prev].x.wrapping_add(SLACK) >= l.e[next].x.wrapping_sub(abs32(l.e[next].dx))
}

fn edges_too_close_int(prev_rite: i32, ul: Fixed, ll: Fixed) -> bool {
    prev_rite > fixed_floor_to_int(ul) || prev_rite > fixed_floor_to_int(ll)
}

#[allow(clippy::too_many_arguments)]
fn aaa_walk_edges(
    l: &mut EdgeList,
    fill_type: FillType,
    b: &mut Additive,
    start_y: i32,
    stop_y: i32,
    left_clip: Fixed,
    right_clip: Fixed,
    is_using_mask: bool,
    force_rle: bool,
    skip_intersect: bool,
) {
    let head = l.head;
    let tail = l.tail;
    l.e[head].x = left_clip;
    l.e[head].upper_x = left_clip;
    l.e[tail].x = right_clip;
    l.e[tail].upper_x = right_clip;
    let first = l.e[head].next;
    let mut y = l.e[first].upper_y.max(int_to_fixed(start_y));
    let mut next_next_y = MAX_S32;
    {
        let mut edge = l.e[head].next;
        while l.e[edge].upper_y <= y {
            l.e[edge].go_y(y);
            update_next_next_y(l.e[edge].lower_y, y, &mut next_next_y);
            edge = l.e[edge].next;
        }
        update_next_next_y(l.e[edge].upper_y, y, &mut next_next_y);
    }
    let winding_mask: i32 = if fill_type == FillType::EvenOdd {
        1
    } else {
        -1
    };
    let is_inverse = false;

    loop {
        let mut w: i32 = 0;
        let mut in_interval = is_inverse;
        let mut prev_x = l.e[head].x;
        let mut next_y = next_next_y.min(fixed_ceil_to_fixed(y + 1));
        let mut curr_e = l.e[head].next;
        let mut left_e = head;
        let mut left = left_clip;
        let mut left_dy: Fixed = 0;
        let mut prev_rite = fixed_floor_to_int(left_clip);
        next_next_y = MAX_S32;

        let mut y_shift = 0;
        if (next_y - y) & (FIXED_1 >> 2) != 0 {
            y_shift = 2;
            next_y = y + (FIXED_1 >> 2);
        } else if (next_y - y) & (FIXED_1 >> 1) != 0 {
            y_shift = 1;
        }
        let full_alpha = fixed_to_alpha(next_y - y);
        let no_real_blitter = force_rle;

        while l.e[curr_e].upper_y <= y {
            w += l.e[curr_e].winding as i32;
            let prev_in_interval = in_interval;
            in_interval = ((w & winding_mask) == 0) == is_inverse;
            let is_left = in_interval && !prev_in_interval;
            let is_rite = !in_interval && prev_in_interval;

            if is_rite {
                let mut rite = l.e[curr_e].x;
                l.e[curr_e].go_y_shift(next_y, y_shift);
                let next_left = left_clip.max(l.e[left_e].x);
                rite = right_clip.min(rite);
                let next_rite = right_clip.min(l.e[curr_e].x);
                let cn = l.e[curr_e].next;
                let too_close = full_alpha == 0xFF
                    && (edges_too_close_int(prev_rite, left, l.e[left_e].x)
                        || edges_too_close(l, curr_e, cn, next_y));
                let curr_dy = l.e[curr_e].dy;
                blit_trapezoid_row(
                    b,
                    y >> 16,
                    left,
                    rite,
                    next_left,
                    next_rite,
                    left_dy,
                    curr_dy,
                    full_alpha,
                    is_using_mask,
                    no_real_blitter || too_close,
                );
                prev_rite = fixed_ceil_to_int(rite.max(l.e[curr_e].x));
            } else {
                if is_left {
                    left = l.e[curr_e].x.max(left_clip);
                    left_dy = l.e[curr_e].dy;
                    left_e = curr_e;
                }
                l.e[curr_e].go_y_shift(next_y, y_shift);
            }

            let next = l.e[curr_e].next;
            while l.e[curr_e].lower_y <= next_y {
                let e = &mut l.e[curr_e];
                if e.curve_count < 0 {
                    e.keep_continuous_cubic();
                    if !e.update_cubic() {
                        break;
                    }
                } else if e.curve_count > 0 {
                    e.keep_continuous_quad();
                    if !e.update_quadratic() {
                        break;
                    }
                } else {
                    break;
                }
            }
            if l.e[curr_e].lower_y <= next_y {
                l.remove(curr_e);
            } else {
                update_next_next_y(l.e[curr_e].lower_y, next_y, &mut next_next_y);
                let new_x = l.e[curr_e].x;
                if new_x < prev_x {
                    l.backward_insert_edge_based_on_x(curr_e);
                } else {
                    prev_x = new_x;
                }
                if !skip_intersect {
                    check_intersection(l, curr_e, next_y, &mut next_next_y);
                }
            }
            curr_e = next;
        }

        if in_interval {
            let lp = l.e[left_e].prev;
            let too_close = full_alpha == 0xFF && edges_too_close(l, lp, left_e, next_y);
            let nl = left_clip.max(l.e[left_e].x);
            blit_trapezoid_row(
                b,
                y >> 16,
                left,
                right_clip,
                nl,
                right_clip,
                left_dy,
                0,
                full_alpha,
                is_using_mask,
                no_real_blitter || too_close,
            );
        }
        if force_rle {
            b.flush_if_y_changed(y, next_y);
        }
        y = next_y;
        if y >= int_to_fixed(stop_y) {
            break;
        }
        insert_new_edges(l, curr_e, y, &mut next_next_y);
    }
}

// ── Entry point ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn aaa_fill_path(
    path: &Path,
    clip_rect: &IRect,
    b: &mut Additive,
    mut start_y: i32,
    mut stop_y: i32,
    path_contained_in_clip: bool,
    is_using_mask: bool,
    force_rle: bool,
) {
    let clip_f = IRectF(clip_rect.to_rect());
    let builder = EdgeBuilder::build(
        path,
        if path_contained_in_clip {
            None
        } else {
            Some(&clip_f)
        },
    );
    let count = builder.edges.len();
    if count == 0 {
        return;
    }
    let mut list = EdgeList::new(builder.edges);
    if !path_contained_in_clip && start_y < clip_rect.top {
        start_y = clip_rect.top;
    }
    if !path_contained_in_clip && stop_y > clip_rect.bottom {
        stop_y = clip_rect.bottom;
    }
    let mut left_bound = int_to_fixed(clip_rect.left);
    let mut right_bound = int_to_fixed(clip_rect.right);
    if is_using_mask {
        let ir = path.bounds().round_out();
        left_bound = left_bound.max(int_to_fixed(ir.left));
        right_bound = right_bound.min(int_to_fixed(ir.right));
    }
    if path.convexity.is_convex() && count >= 2 {
        aaa_walk_convex_edges(
            &mut list,
            b,
            start_y,
            stop_y,
            left_bound,
            right_bound,
            is_using_mask,
        );
    } else {
        let skip_intersect = path.pts.len() > ((stop_y - start_y) * 2) as usize;
        aaa_walk_edges(
            &mut list,
            path.fill_type,
            b,
            start_y,
            stop_y,
            left_bound,
            right_bound,
            is_using_mask,
            force_rle,
            skip_intersect,
        );
    }
}

/// `SkScan::AAAFillPath(path, blitter, ir, clipBounds, forceRLE=false)`.
pub fn aaa_fill_path_entry(
    path: &Path,
    blitter: &mut dyn Blitter,
    ir: &IRect,
    clip_bounds: &IRect,
) {
    let contained_in_clip = clip_bounds.contains(ir);
    if Additive::can_handle_rect(ir) {
        // try_blit_fat_anti_rect
        if let Some(rect) = path.as_rect() {
            if let Some(r) = rect.intersect(&clip_bounds.to_rect()) {
                let bounds = r.round_out();
                if bounds.width() >= 3 {
                    blitter.blit_fat_anti_rect(&r);
                    return;
                }
            } else {
                return;
            }
        }
        let mut add = Additive::mask(blitter, ir, clip_bounds);
        aaa_fill_path(
            path,
            clip_bounds,
            &mut add,
            ir.top,
            ir.bottom,
            contained_in_clip,
            true,
            false,
        );
        add.finish();
    } else if path.convexity.is_convex() {
        let mut add = Additive::run_based(blitter, ir, clip_bounds, false);
        aaa_fill_path(
            path,
            clip_bounds,
            &mut add,
            ir.top,
            ir.bottom,
            contained_in_clip,
            false,
            false,
        );
        add.finish();
    } else {
        let mut add = Additive::run_based(blitter, ir, clip_bounds, true);
        aaa_fill_path(
            path,
            clip_bounds,
            &mut add,
            ir.top,
            ir.bottom,
            contained_in_clip,
            false,
            false,
        );
        add.finish();
    }
}

/// `SkScan::AntiFillPath(path, clip, blitter)` for a rect clip.
pub fn anti_fill_path(path: &Path, clip: &IRect, blitter: &mut dyn Blitter) {
    if clip.is_empty() {
        return;
    }
    let ir = safe_round_out(&path.bounds());
    if ir.is_empty() {
        return;
    }
    let Some(_clipped) = ir.intersect(clip) else {
        return;
    };
    // SkScanClipper: when the path bounds exceed the window horizontally,
    // the blitter is wrapped in SkRectClipBlitter, which changes the arithmetic
    // (blitAntiH2/V2 go through blitAntiH with runs).
    if !clip.contains(&ir) && (clip.left > ir.left || clip.right < ir.right) {
        let mut wrapped = super::blit::RectClipBlitter::new(blitter, *clip);
        aaa_fill_path_entry(path, &mut wrapped, &ir, clip);
    } else {
        aaa_fill_path_entry(path, blitter, &ir, clip);
    }
}

fn safe_round_out(r: &super::geometry::Rect) -> IRect {
    let mut dst = r.round_out();
    const SUPERSAMPLE_SHIFT: i32 = 2;
    let limit = MAX_S32 >> SUPERSAMPLE_SHIFT;
    let lim = IRect::from_ltrb(-limit, -limit, limit, limit);
    dst = dst.intersect(&lim).unwrap_or_default();
    dst
}

#[allow(dead_code)]
fn _edge_type_unused(_e: EdgeType) {}
