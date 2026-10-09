//! Canvas shadows: shape mask (A8) → `SkMaskBlurFilter` blur → mask blit
//! with the shadow paint. Port of `SkMaskFilterBase::filterPath`,
//! `skcpu::DrawToMask`, `SkBlurMask::BoxBlur`, `SkMaskBlurFilter.cpp` and
//! `SkGaussFilter.cpp` from Skia at Chrome 151.

use super::blit::Blitter;
use super::geometry::{IRect, Rect};

/// A8 mask with its position on the canvas.
#[derive(Clone, Debug)]
pub struct Mask {
    pub bounds: IRect,
    pub row_bytes: usize,
    /// Empty means bounds only (like `fImage == nullptr` in Skia).
    pub image: Vec<u8>,
}

impl Mask {
    fn has_image(&self) -> bool {
        !self.image.is_empty()
    }
}

// ── A8 blitter (SkA8_Blitter, srcover, opaque colour) ─────────────────────

#[inline]
fn div255(prod: u32) -> u8 {
    ((prod + 128) * 257 >> 16) as u8
}
#[inline]
fn u8_lerp(a: u8, b: u8, t: u8) -> u8 {
    div255((255 - t as u32) * a as u32 + t as u32 * b as u32)
}
#[inline]
fn srcover_p(src: u8, dst: u8) -> u8 {
    src.wrapping_add(div255((255 - src as u32) * dst as u32))
}

/// `SkA8_Blitter` with `fSrc = 255`, srcover: how the shape mask is drawn.
pub struct A8Blitter<'a> {
    data: &'a mut [u8],
    width: i32,
    height: i32,
}

impl<'a> A8Blitter<'a> {
    pub fn new(data: &'a mut [u8], width: i32, height: i32) -> Self {
        A8Blitter {
            data,
            width,
            height,
        }
    }
    #[inline]
    fn idx(&self, x: i32, y: i32) -> usize {
        (y * self.width + x) as usize
    }
    fn xr(&self, x: i32, w: i32) -> Option<(i32, i32)> {
        let x0 = x.max(0);
        let x1 = (x + w).min(self.width);
        if x0 < x1 {
            Some((x0, x1))
        } else {
            None
        }
    }
    /// `A8_row_aa` with `canFoldAA`: src = div255(255·aa) = aa; dst = srcover(src, dst).
    fn row_aa(&mut self, x: i32, y: i32, w: i32, aa: u8) {
        if y < 0 || y >= self.height {
            return;
        }
        let Some((x0, x1)) = self.xr(x, w) else {
            return;
        };
        let src = div255(255 * aa as u32);
        for xx in x0..x1 {
            let i = self.idx(xx, y);
            self.data[i] = srcover_p(src, self.data[i]);
        }
    }
    fn row_bw(&mut self, x: i32, y: i32, w: i32) {
        if y < 0 || y >= self.height {
            return;
        }
        let Some((x0, x1)) = self.xr(x, w) else {
            return;
        };
        for xx in x0..x1 {
            let i = self.idx(xx, y);
            self.data[i] = srcover_p(255, self.data[i]);
        }
    }
}

impl<'a> Blitter for A8Blitter<'a> {
    fn blit_h(&mut self, x: i32, y: i32, width: i32) {
        self.row_bw(x, y, width);
    }
    fn blit_rect(&mut self, x: i32, y: i32, width: i32, height: i32) {
        for yy in y..y + height {
            self.row_bw(x, yy, width);
        }
    }
    fn blit_anti_h(&mut self, mut x: i32, y: i32, alphas: &[u8], runs: &[i16]) {
        let mut i = 0usize;
        loop {
            let run = runs[i];
            if run <= 0 {
                break;
            }
            match alphas[i] {
                0 => {}
                255 => self.row_bw(x, y, run as i32),
                aa => self.row_aa(x, y, run as i32, aa),
            }
            x += run as i32;
            i += run as usize;
        }
    }
    fn blit_v(&mut self, x: i32, y: i32, height: i32, alpha: u8) {
        if alpha == 0 {
            return;
        }
        for yy in y..y + height {
            if alpha == 255 {
                self.row_bw(x, yy, 1);
            } else {
                self.row_aa(x, yy, 1, alpha);
            }
        }
    }
    fn blit_anti_h2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        // Default SkBlitter::blitAntiH2: blitAntiH with two runs of 1.
        let runs = [1i16, 1, 0];
        let aa = [a0, a1];
        self.blit_anti_h(x, y, &aa, &runs);
    }
    fn blit_anti_v2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        let runs = [1i16, 0];
        self.blit_anti_h(x, y, &[a0], &runs);
        self.blit_anti_h(x, y + 1, &[a1], &runs);
    }
    fn blit_anti_rect(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        left_alpha: u8,
        right_alpha: u8,
    ) {
        let mut x = x;
        if left_alpha > 0 {
            self.blit_v(x, y, height, left_alpha);
        }
        x += 1;
        if width > 0 {
            self.blit_rect(x, y, width, height);
            x += width;
        }
        if right_alpha > 0 {
            self.blit_v(x, y, height, right_alpha);
        }
    }
    fn blit_mask(&mut self, mask: &[u8], mask_bounds: &IRect, row_bytes: usize, clip: &IRect) {
        for yy in clip.top..clip.bottom {
            if yy < 0 || yy >= self.height {
                continue;
            }
            let Some((x0, x1)) = self.xr(clip.left, clip.width()) else {
                continue;
            };
            let row = (yy - mask_bounds.top) as usize * row_bytes;
            for xx in x0..x1 {
                let src = mask[row + (xx - mask_bounds.left) as usize];
                let i = self.idx(xx, yy);
                let d = self.data[i];
                self.data[i] = u8_lerp(d, srcover_p(255, d), src);
            }
        }
    }
}

// ── SkGaussFilter ─────────────────────────────────────────────────────────

const GOOD_ENOUGH: f64 = 1.0 / 100.0;

fn gauss_factors(sigma: f64) -> Vec<f64> {
    let var = sigma * sigma;
    let bessel_i0 = |t: f64| -> f64 {
        let t2o4 = t * t / 4.0;
        let mut sum = 1.0;
        let mut factor = 1.0;
        let mut k = 1.0f64;
        while factor > 1.0 / 1_000_000.0 {
            factor *= t2o4 / (k * k);
            sum += factor;
            k += 1.0;
        }
        sum
    };
    let bessel_i1 = |t: f64| -> f64 {
        let t2o4 = t * t / 4.0;
        let mut sum = t / 2.0;
        let mut factor = sum;
        let mut k = 1.0f64;
        while factor > 1.0 / 1_000_000.0 {
            factor *= t2o4 / (k * (k + 1.0));
            sum += factor;
            k += 1.0;
        }
        sum
    };
    let d = var.exp();
    let mut b = [0.0f64; 6];
    b[0] = bessel_i0(var);
    b[1] = bessel_i1(var);
    let mut gauss = [0.0f64; 6];
    gauss[0] = b[0] / d;
    gauss[1] = b[1] / d;
    let mut n = 1usize;
    while gauss[n] > GOOD_ENOUGH {
        b[n + 1] = -(2.0 * n as f64 / var) * b[n] + b[n - 1];
        gauss[n + 1] = b[n + 1] / d;
        n += 1;
    }
    // normalize(n, gauss)
    let mut sum = 0.0;
    for i in (1..n).rev() {
        sum += 2.0 * gauss[i];
    }
    sum += gauss[0];
    for g in gauss.iter_mut().take(n) {
        *g /= sum;
    }
    let mut sum = 0.0;
    for i in (1..n).rev() {
        sum += 2.0 * gauss[i];
    }
    gauss[0] = 1.0 - sum;
    gauss[..n].to_vec()
}

// ── SkMaskBlurFilter ──────────────────────────────────────────────────────

fn prepare_destination(radius_x: i32, radius_y: i32, src: &Mask) -> Mask {
    let dst_w = src.bounds.width() + 2 * radius_x;
    let dst_h = src.bounds.height() + 2 * radius_y;
    let bounds = IRect::from_ltrb(
        src.bounds.left - radius_x,
        src.bounds.top - radius_y,
        src.bounds.left - radius_x + dst_w,
        src.bounds.top - radius_y + dst_h,
    );
    let image = if src.has_image() {
        vec![0u8; (dst_w * dst_h) as usize]
    } else {
        Vec::new()
    };
    Mask {
        bounds,
        row_bytes: dst_w as usize,
        image,
    }
}

#[inline]
fn mulhi(a: u16, b: u16) -> u16 {
    ((a as u32 * b as u32) >> 16) as u16
}

const HALF88: u16 = 0x80;

/// `blur_x_radius_N` over a window of 8 `s0` values (8.8): contribution to d0/d8.
fn blur_x_radius(radius: usize, s0: &[u16; 8], g: &[u16; 5], d0: &mut [u16; 8], d8: &mut [u16; 8]) {
    // D[n..n+7+2r] += shifted products s0·G[k], k from r down to 0 and back.
    // General form: for offset o in 0..=2r, s0·G[|o - r|] goes to position n+o.
    let mut v = [[0u16; 8]; 5];
    for k in 0..=radius {
        for i in 0..8 {
            v[k][i] = mulhi(s0[i], g[k]);
        }
    }
    for o in 0..=2 * radius {
        let k = (o as i32 - radius as i32).unsigned_abs() as usize;
        for i in 0..8 {
            let pos = i + o;
            if pos < 8 {
                d0[pos] = d0[pos].wrapping_add(v[k][i]);
            } else if pos < 16 {
                d8[pos - 8] = d8[pos - 8].wrapping_add(v[k][i]);
            }
        }
    }
}

fn load8(src: &[u8], off: usize, width: usize) -> [u16; 8] {
    let mut s = [0u16; 8];
    for i in 0..width.min(8) {
        s[i] = (src[off + i] as u16) << 8;
    }
    s
}
fn store8(dst: &mut [u8], off: usize, v: &[u16; 8], width: usize) {
    for i in 0..width.min(8) {
        dst[off + i] = (v[i] >> 8) as u8;
    }
}

fn blur_row(
    radius: usize,
    g: &[u16; 5],
    src: &[u8],
    src_off: usize,
    src_w: usize,
    dst: &mut [u8],
    dst_off: usize,
    dst_w: usize,
) {
    let mut d0 = [HALF88; 8];
    let mut d8 = [HALF88; 8];
    let mut x = 0usize;
    let mut so = src_off;
    let mut dof = dst_off;
    while x + 8 <= src_w {
        let s = load8(src, so, 8);
        blur_x_radius(radius, &s, g, &mut d0, &mut d8);
        store8(dst, dof, &d0, 8);
        d0 = d8;
        d8 = [HALF88; 8];
        so += 8;
        dof += 8;
        x += 8;
    }
    let src_tail = src_w - x;
    if src_tail > 0 {
        let s = load8(src, so, src_tail);
        blur_x_radius(radius, &s, g, &mut d0, &mut d8);
        let dst_tail = 8.min(dst_w - x);
        store8(dst, dof, &d0, dst_tail);
        d0 = d8;
        dof += dst_tail;
        x += dst_tail;
    }
    let dst_tail = dst_w - x;
    if dst_tail > 0 {
        store8(dst, dof, &d0, dst_tail);
    }
}

fn blur_y_radius(radius: usize, s0: &[u16; 8], g: &[u16; 5], d: &mut [[u16; 8]; 8]) -> [u16; 8] {
    // d[0..2r] is a pipeline of partial sums: answer = d[0] + s·G[r]; d[i] = d[i+1] + s·G[|i+1-r|]...
    let mut v = [[0u16; 8]; 5];
    for k in 0..=radius {
        for i in 0..8 {
            v[k][i] = mulhi(s0[i], g[k]);
        }
    }
    let n = 2 * radius; // number of buffers d01..d(2r)
    let mut answer = [0u16; 8];
    for i in 0..8 {
        answer[i] = d[0][i].wrapping_add(v[radius][i]);
    }
    // d[j] = d[j+1] + s·G[k_j], k_j = |radius - (j+1)|; the last is s·G[r] + half.
    for j in 0..n {
        let k = (radius as i32 - (j as i32 + 1)).unsigned_abs() as usize;
        for i in 0..8 {
            let next = if j + 1 < n { d[j + 1][i] } else { HALF88 };
            d[j][i] = next.wrapping_add(v[k][i]);
        }
    }
    answer
}

fn blur_column(
    radius: usize,
    width: usize,
    g: &[u16; 5],
    src: &[u8],
    src_off: usize,
    src_rb: usize,
    src_h: usize,
    dst: &mut [u8],
    dst_off: usize,
    dst_rb: usize,
) {
    let mut d = [[HALF88; 8]; 8];
    let mut so = src_off;
    let mut dof = dst_off;
    for _ in 0..src_h {
        let s = load8(src, so, width);
        let b = blur_y_radius(radius, &s, g, &mut d);
        store8(dst, dof, &b, width);
        so += src_rb;
        dof += dst_rb;
    }
    for j in 0..2 * radius {
        store8(dst, dof, &d[j], width);
        dof += dst_rb;
    }
}

fn small_blur(sigma: f64, src: &Mask) -> (Mask, (i32, i32)) {
    let factors = gauss_factors(sigma);
    let radius = factors.len() - 1;
    let mut g = [0u16; 5];
    for (i, f) in factors.iter().enumerate() {
        g[i] = (f * (1u32 << 16) as f64).round() as u16;
    }
    let mut dst = prepare_destination(radius as i32, radius as i32, src);
    if !src.has_image() {
        return (dst, (radius as i32, radius as i32));
    }
    let src_w = src.bounds.width() as usize;
    let src_h = src.bounds.height() as usize;
    let dst_w = dst.bounds.width() as usize;
    let dst_h = dst.bounds.height() as usize;
    let dst_rb = dst.row_bytes;
    // Vertical pass: columns of 8 into dst, offset by radius in x.
    let mut x = 0usize;
    while x + 8 <= src_w {
        blur_column(
            radius,
            8,
            &g,
            &src.image,
            x,
            src.row_bytes,
            src_h,
            &mut dst.image,
            radius + x,
            dst_rb,
        );
        x += 8;
    }
    let x_tail = src_w - x;
    if x_tail > 0 {
        blur_column(
            radius,
            x_tail,
            &g,
            &src.image,
            x,
            src.row_bytes,
            src_h,
            &mut dst.image,
            radius + x,
            dst_rb,
        );
    }
    // Horizontal pass in place: source is dst offset by radius.
    let tmp = dst.image.clone();
    for y in 0..dst_h {
        blur_row(
            radius,
            &g,
            &tmp,
            y * dst_rb + radius,
            src_w,
            &mut dst.image,
            y * dst_rb,
            dst_w,
        );
    }
    (dst, (radius as i32, radius as i32))
}

struct PlanGauss {
    weight: u64,
    border: i32,
    sliding_window: i32,
    pass0: usize,
    pass1: usize,
    pass2: usize,
}

impl PlanGauss {
    fn new(sigma: f64) -> PlanGauss {
        let possible_window =
            (sigma * 3.0 * (2.0 * std::f64::consts::PI).sqrt() / 4.0 + 0.5).floor() as i32;
        let window = possible_window.max(1);
        let pass0 = (window - 1) as usize;
        let pass1 = (window - 1) as usize;
        let pass2 = if window & 1 == 1 { window - 1 } else { window } as usize;
        let border = if window & 1 == 1 {
            3 * ((window - 1) / 2)
        } else {
            3 * (window / 2) - 1
        };
        let sliding_window = 2 * border + 1;
        let window2 = window as i64 * window as i64;
        let window3 = window2 * window as i64;
        let divisor = if window & 1 == 1 {
            window3
        } else {
            window3 + window2
        };
        let weight = (1.0 / divisor as f64 * (1u64 << 32) as f64).round() as u64;
        PlanGauss {
            weight,
            border,
            sliding_window,
            pass0,
            pass1,
            pass2,
        }
    }
    fn blur(
        &self,
        src: &[u8],
        src_step: usize,
        src_n: usize,
        dst: &mut [u8],
        dst_start: usize,
        dst_stride: usize,
        dst_n: usize,
        width: usize,
    ) {
        let no_change = if self.sliding_window as usize > width {
            self.sliding_window as usize - width
        } else {
            0
        };
        let mut b0 = vec![0u32; self.pass0.max(1)];
        let mut b1 = vec![0u32; self.pass1.max(1)];
        let mut b2 = vec![0u32; self.pass2.max(1)];
        let (mut c0, mut c1, mut c2) = (0usize, 0usize, 0usize);
        let (mut sum0, mut sum1, mut sum2) = (0u32, 0u32, 0u32);
        let half: u64 = 1 << 31;
        let final_scale = |sum: u32| -> u8 { ((self.weight * sum as u64 + half) >> 32) as u8 };
        let mut di = dst_start;
        let mut produced = 0usize;
        let step = |lead: u32,
                    b0: &mut Vec<u32>,
                    b1: &mut Vec<u32>,
                    b2: &mut Vec<u32>,
                    c0: &mut usize,
                    c1: &mut usize,
                    c2: &mut usize,
                    sum0: &mut u32,
                    sum1: &mut u32,
                    sum2: &mut u32|
         -> u8 {
            *sum0 = sum0.wrapping_add(lead);
            *sum1 = sum1.wrapping_add(*sum0);
            *sum2 = sum2.wrapping_add(*sum1);
            let out = final_scale(*sum2);
            if self.pass2 > 0 {
                *sum2 = sum2.wrapping_sub(b2[*c2]);
                b2[*c2] = *sum1;
                *c2 = if *c2 + 1 < self.pass2 { *c2 + 1 } else { 0 };
            }
            if self.pass1 > 0 {
                *sum1 = sum1.wrapping_sub(b1[*c1]);
                b1[*c1] = *sum0;
                *c1 = if *c1 + 1 < self.pass1 { *c1 + 1 } else { 0 };
            }
            if self.pass0 > 0 {
                *sum0 = sum0.wrapping_sub(b0[*c0]);
                b0[*c0] = lead;
                *c0 = if *c0 + 1 < self.pass0 { *c0 + 1 } else { 0 };
            }
            out
        };
        for i in 0..src_n {
            let lead = src[i * src_step] as u32;
            let out = step(
                lead, &mut b0, &mut b1, &mut b2, &mut c0, &mut c1, &mut c2, &mut sum0, &mut sum1,
                &mut sum2,
            );
            dst[di] = out;
            di += dst_stride;
            produced += 1;
        }
        for _ in 0..no_change {
            let out = step(
                0, &mut b0, &mut b1, &mut b2, &mut c0, &mut c1, &mut c2, &mut sum0, &mut sum1,
                &mut sum2,
            );
            dst[di] = out;
            di += dst_stride;
            produced += 1;
        }
        // Remainder, right to left.
        for v in b0.iter_mut() {
            *v = 0;
        }
        for v in b1.iter_mut() {
            *v = 0;
        }
        for v in b2.iter_mut() {
            *v = 0;
        }
        sum0 = 0;
        sum1 = 0;
        sum2 = 0;
        let mut dcur = dst_start + dst_n * dst_stride;
        let mut si = src_n;
        while dcur > di {
            dcur -= dst_stride;
            si -= 1;
            let lead = src[si * src_step] as u32;
            let out = step(
                lead, &mut b0, &mut b1, &mut b2, &mut c0, &mut c1, &mut c2, &mut sum0, &mut sum1,
                &mut sum2,
            );
            dst[dcur] = out;
        }
        let _ = produced;
    }
}

/// `SkMaskBlurFilter::blur`: (mask, margins).
pub fn mask_blur(sigma: f64, src: &Mask) -> (Mask, (i32, i32)) {
    let sigma = sigma.clamp(0.0, 135.0);
    if sigma < 2.0 {
        return small_blur(sigma, src);
    }
    let plan_w = PlanGauss::new(sigma);
    let plan_h = PlanGauss::new(sigma);
    let (border_w, border_h) = (plan_w.border, plan_h.border);
    let mut dst = prepare_destination(border_w, border_h, src);
    if !src.has_image() {
        return (dst, (border_w, border_h));
    }
    let src_w = src.bounds.width() as usize;
    let src_h = src.bounds.height() as usize;
    let dst_w = dst.bounds.width() as usize;
    let dst_h = dst.bounds.height() as usize;
    let tmp_w = src_h;
    let tmp_h = dst_w;
    let mut tmp = vec![0u8; tmp_w * tmp_h];
    // Horizontal pass with transpose: row y → column y in tmp.
    for y in 0..src_h {
        let src_row = &src.image[y * src.row_bytes..y * src.row_bytes + src_w];
        plan_w.blur(src_row, 1, src_w, &mut tmp, y, tmp_w, tmp_h, src_w);
    }
    // Vertical pass (over tmp memory) and transpose back.
    let dst_rb = dst.row_bytes;
    for y in 0..tmp_h {
        let row = tmp[y * tmp_w..y * tmp_w + tmp_w].to_vec();
        plan_h.blur(&row, 1, tmp_w, &mut dst.image, y, dst_rb, dst_h, tmp_w);
    }
    (dst, (border_w, border_h))
}

/// `SkMaskBlurFilter::hasNoBlur`.
pub fn has_no_blur(sigma: f64) -> bool {
    sigma < 1.0 / 3.0
}

/// `compute_mask_bounds`: mask bounds for the path plus the blur margin.
pub fn compute_mask_bounds(dev_bounds: &Rect, clip: &IRect, sigma: f64) -> Option<IRect> {
    let outset = Rect::from_ltrb(
        dev_bounds.left - 0.5,
        dev_bounds.top - 0.5,
        dev_bounds.right + 0.5,
        dev_bounds.bottom + 0.5,
    );
    let mut bounds = outset.round_out();
    let src = Mask {
        bounds,
        row_bytes: 0,
        image: Vec::new(),
    };
    let (_, (mx, my)) = mask_blur(sigma, &src);
    let clip_out = IRect::from_ltrb(
        clip.left - mx.min(128),
        clip.top - my.min(128),
        clip.right + mx.min(128),
        clip.bottom + my.min(128),
    );
    bounds = bounds.intersect(&clip_out)?;
    Some(bounds)
}
