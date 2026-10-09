//! Blitter modelled on `SkRasterPipelineBlitter` for RGBA8888 premul with a
//! solid paint: same lowp arithmetic (`SkRasterPipeline_opts.h`,
//! `namespace lowp`), same stages for each kind of call.

use super::geometry::IRect;

/// `SkBlendMode`: the subset the canvas supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Clear,
    Src,
    Dst,
    SrcOver,
    DstOver,
    SrcIn,
    DstIn,
    SrcOut,
    DstOut,
    SrcATop,
    DstATop,
    Xor,
    Plus,
    Modulate,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Multiply,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    /// `SkBlendMode_ShouldPreScaleCoverage(mode, false)`.
    pub fn should_pre_scale_coverage(self) -> bool {
        matches!(
            self,
            BlendMode::Dst
                | BlendMode::DstOver
                | BlendMode::Plus
                | BlendMode::DstOut
                | BlendMode::SrcATop
                | BlendMode::SrcOver
                | BlendMode::Xor
        )
    }
    /// Whether Skia has a lowp implementation of the mode.
    #[allow(dead_code)]
    pub fn has_lowp(self) -> bool {
        !matches!(
            self,
            BlendMode::ColorDodge
                | BlendMode::ColorBurn
                | BlendMode::SoftLight
                | BlendMode::Hue
                | BlendMode::Saturation
                | BlendMode::Color
                | BlendMode::Luminosity
        )
    }
}

/// One pixel in lowp 16-bit lanes: r,g,b,a in 0..=255 (premul).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Px {
    pub r: u16,
    pub g: u16,
    pub b: u16,
    pub a: u16,
}

#[inline]
fn div255(v: u16) -> u16 {
    ((v as u32 + 255) / 256) as u16
}
#[inline]
fn div255_accurate(v: u16) -> u16 {
    let v = v as u32 + 128;
    ((v + v / 256) / 256) as u16
}
#[inline]
fn inv(v: u16) -> u16 {
    255 - v
}
// lowp 16-bit lane arithmetic: everything mod 2^16, as in Skia's vectors.
#[inline]
fn m(a: u16, b: u16) -> u16 {
    a.wrapping_mul(b)
}
#[inline]
fn ad(a: u16, b: u16) -> u16 {
    a.wrapping_add(b)
}
#[inline]
fn sb(a: u16, b: u16) -> u16 {
    a.wrapping_sub(b)
}
#[inline]
fn lerp(from: u16, to: u16, t: u16) -> u16 {
    div255(ad(m(from, inv(t)), m(to, t)))
}
#[inline]
fn from_float(f: f32) -> u16 {
    (f * 255.0 + 0.5) as u16
}

/// lowp blend stage: `s`, `d` premul; returns the result.
fn blend_lowp(mode: BlendMode, s: Px, d: Px) -> Px {
    // Per-channel modes (BLEND_MODE with the shared alpha formula).
    let per_channel = |f: &dyn Fn(u16, u16, u16, u16) -> u16| -> Px {
        Px {
            r: f(s.r, d.r, s.a, d.a),
            g: f(s.g, d.g, s.a, d.a),
            b: f(s.b, d.b, s.a, d.a),
            a: f(s.a, d.a, s.a, d.a),
        }
    };
    // Fixed-alpha modes: a = a + div255(da*inv(a)).
    let per_channel_srcover_alpha = |f: &dyn Fn(u16, u16, u16, u16) -> u16| -> Px {
        Px {
            r: f(s.r, d.r, s.a, d.a),
            g: f(s.g, d.g, s.a, d.a),
            b: f(s.b, d.b, s.a, d.a),
            a: ad(s.a, div255(m(d.a, inv(s.a)))),
        }
    };
    match mode {
        BlendMode::Clear => Px::default(),
        BlendMode::Src => s,
        BlendMode::Dst => d,
        BlendMode::SrcATop => per_channel(&|s, d, sa, da| div255(ad(m(s, da), m(d, inv(sa))))),
        BlendMode::DstATop => per_channel(&|s, d, sa, da| div255(ad(m(d, sa), m(s, inv(da))))),
        BlendMode::SrcIn => per_channel(&|s, _d, _sa, da| div255(m(s, da))),
        BlendMode::DstIn => per_channel(&|_s, d, sa, _da| div255(m(d, sa))),
        BlendMode::SrcOut => per_channel(&|s, _d, _sa, da| div255(m(s, inv(da)))),
        BlendMode::DstOut => per_channel(&|_s, d, sa, _da| div255(m(d, inv(sa)))),
        BlendMode::SrcOver => per_channel(&|s, d, sa, _da| ad(s, div255(m(d, inv(sa))))),
        BlendMode::DstOver => per_channel(&|s, d, _sa, da| ad(d, div255(m(s, inv(da))))),
        BlendMode::Modulate => per_channel(&|s, d, _sa, _da| div255(m(s, d))),
        BlendMode::Multiply => {
            per_channel(&|s, d, sa, da| div255(ad(ad(m(s, inv(da)), m(d, inv(sa))), m(s, d))))
        }
        BlendMode::Plus => per_channel(&|s, d, _sa, _da| ad(s, d).min(255)),
        BlendMode::Screen => per_channel(&|s, d, _sa, _da| sb(ad(s, d), div255(m(s, d)))),
        BlendMode::Xor => per_channel(&|s, d, sa, da| div255(ad(m(s, inv(da)), m(d, inv(sa))))),
        BlendMode::Darken => {
            per_channel_srcover_alpha(&|s, d, sa, da| sb(ad(s, d), div255(m(s, da).max(m(d, sa)))))
        }
        BlendMode::Lighten => {
            per_channel_srcover_alpha(&|s, d, sa, da| sb(ad(s, d), div255(m(s, da).min(m(d, sa)))))
        }
        BlendMode::Difference => per_channel_srcover_alpha(&|s, d, sa, da| {
            sb(ad(s, d), m(2, div255(m(s, da).min(m(d, sa)))))
        }),
        BlendMode::Exclusion => {
            per_channel_srcover_alpha(&|s, d, _sa, _da| sb(ad(s, d), m(2, div255(m(s, d)))))
        }
        BlendMode::HardLight => per_channel_srcover_alpha(&|s, d, sa, da| {
            let t = if m(2, s) <= sa {
                m(m(2, s), d)
            } else {
                sb(m(sa, da), m(m(2, sb(sa, s)), sb(da, d)))
            };
            div255(ad(ad(m(s, inv(da)), m(d, inv(sa))), t))
        }),
        BlendMode::Overlay => per_channel_srcover_alpha(&|s, d, sa, da| {
            let t = if m(2, d) <= da {
                m(m(2, s), d)
            } else {
                sb(m(sa, da), m(m(2, sb(sa, s)), sb(da, d)))
            };
            div255(ad(ad(m(s, inv(da)), m(d, inv(sa))), t))
        }),
        // No lowp implementation: compute in float (highp), as Skia does.
        _ => blend_highp(mode, s, d),
    }
}

fn blend_highp(mode: BlendMode, s: Px, d: Px) -> Px {
    let f = |v: u16| v as f32 * (1.0 / 255.0);
    let (sr, sg, sb, sa) = (f(s.r), f(s.g), f(s.b), f(s.a));
    let (dr, dg, db, da) = (f(d.r), f(d.g), f(d.b), f(d.a));
    let (r, g, b, a) = match mode {
        BlendMode::ColorDodge => {
            let ch = |s: f32, d: f32| -> f32 {
                if d == 0.0 {
                    s * (1.0 - da)
                } else if s == sa {
                    s * (1.0 - da) + d * (1.0 - sa) + sa * da
                } else {
                    s * (1.0 - da)
                        + d * (1.0 - sa)
                        + sa * (da * (d * sa / (da * (sa - s))).min(1.0)).min(sa * da)
                }
            };
            (ch(sr, dr), ch(sg, dg), ch(sb, db), sa + da - sa * da)
        }
        BlendMode::ColorBurn => {
            let ch = |s: f32, d: f32| -> f32 {
                if d == da {
                    s * (1.0 - da) + d * (1.0 - sa) + sa * da
                } else if s == 0.0 {
                    d * (1.0 - sa)
                } else {
                    s * (1.0 - da)
                        + d * (1.0 - sa)
                        + sa * (da - (da * (da - d) * sa / (s * da)).min(da))
                }
            };
            (ch(sr, dr), ch(sg, dg), ch(sb, db), sa + da - sa * da)
        }
        BlendMode::SoftLight => {
            let ch = |s: f32, d: f32| -> f32 {
                let m = if da > 0.0 { d / da } else { 0.0 };
                let s2 = 2.0 * s;
                let m4 = 4.0 * m;
                let dark_src = d * (sa + (s2 - sa) * (1.0 - m));
                let dark_dst = (m4 * m4 + m4) * (m - 1.0) + 7.0 * m;
                let lite_dst = m.sqrt() - m;
                let lite_src =
                    d * sa + da * (s2 - sa) * if 4.0 * d <= da { dark_dst } else { lite_dst };
                s * (1.0 - da) + d * (1.0 - sa) + if s2 <= sa { dark_src } else { lite_src }
            };
            (ch(sr, dr), ch(sg, dg), ch(sb, db), sa + da - sa * da)
        }
        _ => {
            // hue/saturation/color/luminosity: rare on canvas; source-over.
            (
                sr + dr * (1.0 - sa),
                sg + dg * (1.0 - sa),
                sb + db * (1.0 - sa),
                sa + da * (1.0 - sa),
            )
        }
    };
    let u = |v: f32| ((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32) as u16;
    Px {
        r: u(r),
        g: u(g),
        b: u(b),
        a: u(a),
    }
}

/// Pixel surface: RGBA8888 premul, row-major.
pub struct Surface<'a> {
    pub data: &'a mut [u8],
    pub width: i32,
    pub height: i32,
}

impl<'a> Surface<'a> {
    #[inline]
    fn load(&self, x: i32, y: i32) -> Px {
        let i = ((y * self.width + x) * 4) as usize;
        let d = &self.data[i..i + 4];
        Px {
            r: d[0] as u16,
            g: d[1] as u16,
            b: d[2] as u16,
            a: d[3] as u16,
        }
    }
    #[inline]
    fn store(&mut self, x: i32, y: i32, p: Px) {
        let i = ((y * self.width + x) * 4) as usize;
        let d = &mut self.data[i..i + 4];
        d[0] = p.r.min(255) as u8;
        d[1] = p.g.min(255) as u8;
        d[2] = p.b.min(255) as u8;
        d[3] = p.a.min(255) as u8;
    }
}

/// `SkRectClipBlitter`: the wrapper `SkScanClipper` puts in front of the
/// real blitter when path bounds exceed the clip horizontally.
/// It matters not for clipping (our blitters never write outside the clip) but
/// because it does not override `blitAntiH2`/`blitAntiV2`: they fall to the base
/// `SkBlitter::blitAntiH2/V2`, i.e. `blitAntiH` with runs, which uses
/// different blend arithmetic in `SkARGB32_*_Blitter`.
pub struct RectClipBlitter<'a> {
    inner: &'a mut dyn Blitter,
    clip: IRect,
}

impl<'a> RectClipBlitter<'a> {
    pub fn new(inner: &'a mut dyn Blitter, clip: IRect) -> Self {
        RectClipBlitter { inner, clip }
    }
    #[inline]
    fn y_in(&self, y: i32) -> bool {
        ((y - self.clip.top) as u32) < (self.clip.height() as u32)
    }
    #[inline]
    fn x_in(&self, x: i32) -> bool {
        ((x - self.clip.left) as u32) < (self.clip.width() as u32)
    }
}

fn compute_anti_width(runs: &[i16]) -> i32 {
    let mut width = 0i32;
    let mut i = 0usize;
    loop {
        let c = runs[i];
        if c <= 0 {
            break;
        }
        width += c as i32;
        i += c as usize;
    }
    width
}

/// `SkAlphaRuns::BreakAt`: split a run at `x`.
fn break_at(runs: &mut [i16], alpha: &mut [u8], mut x: i32) {
    let mut i = 0usize;
    while x > 0 {
        let n = runs[i] as i32;
        let mut n_val = n;
        if x < n {
            alpha[i + x as usize] = alpha[i];
            runs[i] = x as i16;
            runs[i + x as usize] = (n - x) as i16;
            n_val = x;
        }
        i += n_val as usize;
        x -= n_val;
    }
}

impl<'a> Blitter for RectClipBlitter<'a> {
    fn blit_h(&mut self, left: i32, y: i32, width: i32) {
        if !self.y_in(y) {
            return;
        }
        let l = left.max(self.clip.left);
        let r = (left + width).min(self.clip.right);
        if r > l {
            self.inner.blit_h(l, y, r - l);
        }
    }
    fn blit_anti_h(&mut self, left: i32, y: i32, alphas: &[u8], runs: &[i16]) {
        if !self.y_in(y) || left >= self.clip.right {
            return;
        }
        let mut x0 = left;
        let mut x1 = left + compute_anti_width(runs);
        if x1 <= self.clip.left {
            return;
        }
        let mut runs_v: Vec<i16> = runs.to_vec();
        let mut aa_v: Vec<u8> = alphas.to_vec();
        let mut off = 0usize;
        if x0 < self.clip.left {
            let dx = self.clip.left - x0;
            break_at(&mut runs_v, &mut aa_v, dx);
            off = dx as usize;
            x0 = self.clip.left;
        }
        if x1 > self.clip.right {
            x1 = self.clip.right;
            break_at(&mut runs_v[off..], &mut aa_v[off..], x1 - x0);
            runs_v[off + (x1 - x0) as usize] = 0;
        }
        self.inner.blit_anti_h(x0, y, &aa_v[off..], &runs_v[off..]);
    }
    fn blit_v(&mut self, x: i32, y: i32, height: i32, alpha: u8) {
        if !self.x_in(x) {
            return;
        }
        let y0 = y.max(self.clip.top);
        let y1 = (y + height).min(self.clip.bottom);
        if y0 < y1 {
            self.inner.blit_v(x, y0, y1 - y0, alpha);
        }
    }
    fn blit_rect(&mut self, left: i32, y: i32, width: i32, height: i32) {
        let r = IRect::from_ltrb(left, y, left + width, y + height);
        if let Some(r) = r.intersect(&self.clip) {
            self.inner.blit_rect(r.left, r.top, r.width(), r.height());
        }
    }
    fn blit_anti_h2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        // SkBlitter::blitAntiH2 (not overridden by SkRectClipBlitter).
        let runs = [1i16, 1, 0];
        let aa = [a0, a1];
        self.blit_anti_h(x, y, &aa, &runs);
    }
    fn blit_anti_v2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        // SkBlitter::blitAntiV2.
        let runs = [1i16, 0];
        self.blit_anti_h(x, y, &[a0], &runs);
        self.blit_anti_h(x, y + 1, &[a1], &runs);
    }
    fn blit_anti_rect(
        &mut self,
        left: i32,
        y: i32,
        width: i32,
        height: i32,
        mut left_alpha: u8,
        mut right_alpha: u8,
    ) {
        let full = IRect::from_ltrb(left, y, left + width + 2, y + height);
        let Some(r) = full.intersect(&self.clip) else {
            return;
        };
        if r.left != left {
            left_alpha = 255;
        }
        if r.right != left + width + 2 {
            right_alpha = 255;
        }
        if left_alpha == 255 && right_alpha == 255 {
            self.inner.blit_rect(r.left, r.top, r.width(), r.height());
        } else if r.width() == 1 {
            if r.left == left {
                self.inner.blit_v(r.left, r.top, r.height(), left_alpha);
            } else {
                self.inner.blit_v(r.left, r.top, r.height(), right_alpha);
            }
        } else {
            self.inner.blit_anti_rect(
                r.left,
                r.top,
                r.width() - 2,
                r.height(),
                left_alpha,
                right_alpha,
            );
        }
    }
    fn blit_mask(&mut self, mask: &[u8], mask_bounds: &IRect, row_bytes: usize, clip: &IRect) {
        if let Some(r) = clip.intersect(&self.clip) {
            self.inner.blit_mask(mask, mask_bounds, row_bytes, &r);
        }
    }
}

/// `SkBlitter` interface (what the scan converters call).
pub trait Blitter {
    fn blit_h(&mut self, x: i32, y: i32, width: i32);
    fn blit_anti_h(&mut self, x: i32, y: i32, alphas: &[u8], runs: &[i16]);
    fn blit_v(&mut self, x: i32, y: i32, height: i32, alpha: u8);
    fn blit_rect(&mut self, x: i32, y: i32, width: i32, height: i32);
    fn blit_anti_h2(&mut self, x: i32, y: i32, a0: u8, a1: u8);
    fn blit_anti_v2(&mut self, x: i32, y: i32, a0: u8, a1: u8);
    fn blit_anti_rect(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        left_alpha: u8,
        right_alpha: u8,
    );
    /// A8 mask: `mask[(yy-top)*row_bytes + (xx-left)]`, drawn within `clip`.
    fn blit_mask(&mut self, mask: &[u8], mask_bounds: &IRect, row_bytes: usize, clip: &IRect);
    /// `SkBlitter::blitFatAntiRect`.
    fn blit_fat_anti_rect(&mut self, rect: &super::geometry::Rect) {
        let bounds = rect.round_out();
        if bounds.height() == 0 {
            return;
        }
        let scalar_to_alpha = |a: f32| -> u8 {
            let alpha = (a * 255.0) as u8;
            if alpha > 247 {
                0xFF
            } else if alpha < 8 {
                0
            } else {
                alpha
            }
        };
        let w = bounds.width() as usize;
        let mut runs = vec![0i16; w + 1];
        let mut alphas = vec![0u8; w + 1];
        runs[0] = 1;
        runs[1] = (w as i16) - 2;
        runs[w - 1] = 1;
        runs[w] = 0;
        let partial_l = bounds.left as f32 + 1.0 - rect.left;
        let partial_r = rect.right - (bounds.right as f32 - 1.0);
        let mut partial_t = bounds.top as f32 + 1.0 - rect.top;
        let partial_b = rect.bottom - (bounds.bottom as f32 - 1.0);
        if bounds.height() == 1 {
            partial_t = rect.bottom - rect.top;
        }
        alphas[0] = scalar_to_alpha(partial_l * partial_t);
        alphas[1] = scalar_to_alpha(partial_t);
        alphas[w - 1] = scalar_to_alpha(partial_r * partial_t);
        self.blit_anti_h(bounds.left, bounds.top, &alphas, &runs);
        if bounds.height() > 2 {
            self.blit_anti_rect(
                bounds.left,
                bounds.top + 1,
                bounds.width() - 2,
                bounds.height() - 2,
                scalar_to_alpha(partial_l),
                scalar_to_alpha(partial_r),
            );
        }
        if bounds.height() > 1 {
            alphas[0] = scalar_to_alpha(partial_l * partial_b);
            alphas[1] = scalar_to_alpha(partial_b);
            alphas[w - 1] = scalar_to_alpha(partial_r * partial_b);
            self.blit_anti_h(bounds.left, bounds.bottom - 1, &alphas, &runs);
        }
    }
}

/// Solid paint: straight-alpha color 0..255 and blend mode.
#[derive(Clone, Copy, Debug)]
pub struct SolidPaint {
    pub rgba: [u8; 4],
    pub mode: BlendMode,
}

// ── Integer helpers from SkColorPriv/SkColorData ──────────────────────────────

#[inline]
fn mul_div255_round(a: u32, b: u32) -> u32 {
    // SkMul16ShiftRound(a, b, 8)
    let prod = a * b + (1 << 7);
    (prod + (prod >> 8)) >> 8
}
/// `SkPreMultiplyColor`.
#[inline]
fn premultiply(rgba: [u8; 4]) -> [u8; 4] {
    let a = rgba[3] as u32;
    if a == 255 {
        return rgba;
    }
    [
        mul_div255_round(rgba[0] as u32, a) as u8,
        mul_div255_round(rgba[1] as u32, a) as u8,
        mul_div255_round(rgba[2] as u32, a) as u8,
        rgba[3],
    ]
}
/// `SkAlphaMulQ(c, scale)`: each channel `(c * scale) >> 8`, scale in 0..=256.
#[inline]
fn alpha_mul_q(c: [u8; 4], scale: u32) -> [u8; 4] {
    [
        ((c[0] as u32 * scale) >> 8) as u8,
        ((c[1] as u32 * scale) >> 8) as u8,
        ((c[2] as u32 * scale) >> 8) as u8,
        ((c[3] as u32 * scale) >> 8) as u8,
    ]
}
#[inline]
fn add4(a: [u8; 4], b: [u8; 4]) -> [u8; 4] {
    // Per-channel add: unsaturated in Skia (cannot overflow).
    [
        a[0].wrapping_add(b[0]),
        a[1].wrapping_add(b[1]),
        a[2].wrapping_add(b[2]),
        a[3].wrapping_add(b[3]),
    ]
}
/// `skvx::approx_scale(x, y)` = (x·y + x) / 256.
#[inline]
fn approx_scale(x: u8, y: u8) -> u8 {
    ((x as u32 * y as u32 + x as u32) / 256) as u8
}
#[inline]
fn approx_scale4(c: [u8; 4], y: u8) -> [u8; 4] {
    [
        approx_scale(c[0], y),
        approx_scale(c[1], y),
        approx_scale(c[2], y),
        approx_scale(c[3], y),
    ]
}
/// `SkBlendARGB32(src, dst, aa)`.
#[inline]
fn blend_argb32(src: [u8; 4], dst: [u8; 4], aa: u8) -> [u8; 4] {
    let src_scale = aa as u32 + 1;
    let prod = 0xFFFF - src[3] as u32 * src_scale;
    let dst_scale = (prod + (prod >> 8)) >> 8;
    let ch = |s: u8, d: u8| ((s as u32 * src_scale + d as u32 * dst_scale) >> 8) as u8;
    [
        ch(src[0], dst[0]),
        ch(src[1], dst[1]),
        ch(src[2], dst[2]),
        ch(src[3], dst[3]),
    ]
}
/// `SkFastFourByteInterp(src, dst, w)`: scale = w + (w >> 7).
#[inline]
fn fast_four_byte_interp(src: [u8; 4], dst: [u8; 4], w: u8) -> [u8; 4] {
    let scale = w as u32 + (w as u32 >> 7);
    let ch = |s: u8, d: u8| ((s as u32 * scale + (256 - scale) * d as u32) >> 8) as u8;
    [
        ch(src[0], dst[0]),
        ch(src[1], dst[1]),
        ch(src[2], dst[2]),
        ch(src[3], dst[3]),
    ]
}
/// `SkBlitRow::Color32` for one pixel: dst = ((dst*invA) >> 8) + color.
#[inline]
fn color32_px(dst: [u8; 4], color: [u8; 4]) -> [u8; 4] {
    match color[3] {
        0 => dst,
        255 => color,
        a => {
            let inv_a = 256 - a as u32;
            let ch = |d: u8, c: u8| (((d as u32 * inv_a) >> 8) + c as u32) as u8;
            [
                ch(dst[0], color[0]),
                ch(dst[1], color[1]),
                ch(dst[2], color[2]),
                ch(dst[3], color[3]),
            ]
        }
    }
}

enum Strategy {
    /// `SkARGB32_Blitter` / `_Opaque_` / `_Black_`: source-over on N32.
    Legacy {
        pm: [u8; 4],
        opaque: bool,
        black: bool,
    },
    /// `SkRasterPipelineBlitter`: the other blend modes.
    Pipeline {
        src: Px,
        mode: BlendMode,
        memset: Option<Px>,
    },
}

/// Solid-paint blitter, chosen as in `SkBlitter::Choose`.
pub struct SolidBlitter<'a> {
    surf: Surface<'a>,
    st: Strategy,
}

impl<'a> SolidBlitter<'a> {
    pub fn new(surf: Surface<'a>, paint: &SolidPaint) -> SolidBlitter<'a> {
        let mut mode = paint.mode;
        // CheckFastPath: "copy" with an opaque solid paint is srcover.
        if mode == BlendMode::Src && paint.rgba[3] == 255 {
            mode = BlendMode::SrcOver;
        }
        if mode == BlendMode::SrcOver {
            let pm = premultiply(paint.rgba);
            let black = paint.rgba == [0, 0, 0, 255];
            return SolidBlitter {
                surf,
                st: Strategy::Legacy {
                    pm,
                    opaque: paint.rgba[3] == 255,
                    black,
                },
            };
        }
        // Clear is Src with a transparent color.
        let rgba = if mode == BlendMode::Clear {
            [0, 0, 0, 0]
        } else {
            paint.rgba
        };
        if mode == BlendMode::Clear {
            mode = BlendMode::Src;
        }
        // appendConstantColor: premul in float, then *255 + 0.5 -> u16.
        let f = |v: u8| v as f32 * (1.0 / 255.0);
        let a = f(rgba[3]);
        let pmf = [f(rgba[0]) * a, f(rgba[1]) * a, f(rgba[2]) * a, a];
        let u = |v: f32| (v * 255.0 + 0.5) as u16;
        let src = Px {
            r: u(pmf[0]),
            g: u(pmf[1]),
            b: u(pmf[2]),
            a: u(pmf[3]),
        };
        let is_opaque = a == 1.0;
        if is_opaque && mode == BlendMode::SrcOver {
            mode = BlendMode::Src;
        }
        let memset = if mode == BlendMode::Src {
            Some(src)
        } else {
            None
        };
        SolidBlitter {
            surf,
            st: Strategy::Pipeline { src, mode, memset },
        }
    }

    #[inline]
    fn clip_x(&self, x: i32, w: i32) -> Option<(i32, i32)> {
        let x0 = x.max(0);
        let x1 = (x + w).min(self.surf.width);
        if x0 < x1 {
            Some((x0, x1))
        } else {
            None
        }
    }
    #[inline]
    fn in_y(&self, y: i32) -> bool {
        y >= 0 && y < self.surf.height
    }
    #[inline]
    fn get(&self, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * self.surf.width + x) * 4) as usize;
        let d = &self.surf.data[i..i + 4];
        [d[0], d[1], d[2], d[3]]
    }
    #[inline]
    fn put(&mut self, x: i32, y: i32, p: [u8; 4]) {
        let i = ((y * self.surf.width + x) * 4) as usize;
        self.surf.data[i..i + 4].copy_from_slice(&p);
    }

    // ── pipeline (modes other than source-over) ──

    #[inline]
    fn pipe_coverage(&mut self, x: i32, y: i32, cov: u16) {
        let Strategy::Pipeline { src, mode, memset } = self.st else {
            unreachable!()
        };
        let s = src;
        let d = self.surf.load(x, y);
        let out = if mode == BlendMode::Src {
            // Src does not pre-scale coverage: lerp(d, s, cov).
            let _ = memset;
            Px {
                r: lerp(d.r, s.r, cov),
                g: lerp(d.g, s.g, cov),
                b: lerp(d.b, s.b, cov),
                a: lerp(d.a, s.a, cov),
            }
        } else if mode.should_pre_scale_coverage() {
            let s2 = Px {
                r: div255(m(s.r, cov)),
                g: div255(m(s.g, cov)),
                b: div255(m(s.b, cov)),
                a: div255(m(s.a, cov)),
            };
            blend_lowp(mode, s2, d)
        } else {
            let b = blend_lowp(mode, s, d);
            Px {
                r: lerp(d.r, b.r, cov),
                g: lerp(d.g, b.g, cov),
                b: lerp(d.b, b.b, cov),
                a: lerp(d.a, b.a, cov),
            }
        };
        self.surf.store(x, y, out);
    }
    #[inline]
    fn pipe_full(&mut self, x: i32, y: i32) {
        let Strategy::Pipeline { src, mode, memset } = self.st else {
            unreachable!()
        };
        if let Some(c) = memset {
            self.surf.store(x, y, c);
            return;
        }
        let d = self.surf.load(x, y);
        let out = blend_lowp(mode, src, d);
        self.surf.store(x, y, out);
    }

    /// Pixel with coverage `aa` (0..=255) in a row, like the chosen blitter's
    /// `blitAntiH`.
    fn span(&mut self, x: i32, y: i32, w: i32, aa: u8) {
        if !self.in_y(y) {
            return;
        }
        let Some((x0, x1)) = self.clip_x(x, w) else {
            return;
        };
        if aa == 0 {
            return;
        }
        match self.st {
            Strategy::Legacy { pm, opaque, black } => {
                if black {
                    if aa == 255 {
                        for xx in x0..x1 {
                            self.put(xx, y, [0, 0, 0, 255]);
                        }
                    } else {
                        let dst_scale = 256 - aa as u32;
                        for xx in x0..x1 {
                            let d = self.get(xx, y);
                            self.put(xx, y, add4([0, 0, 0, aa], alpha_mul_q(d, dst_scale)));
                        }
                    }
                } else if opaque && aa == 255 {
                    for xx in x0..x1 {
                        self.put(xx, y, pm);
                    }
                } else {
                    let sc = if aa == 255 {
                        pm
                    } else {
                        alpha_mul_q(pm, aa as u32 + 1)
                    };
                    for xx in x0..x1 {
                        let d = self.get(xx, y);
                        self.put(xx, y, color32_px(d, sc));
                    }
                }
            }
            Strategy::Pipeline { .. } => {
                if aa == 255 {
                    for xx in x0..x1 {
                        self.pipe_full(xx, y);
                    }
                } else {
                    // blitAntiH: coverage goes through float and from_float.
                    let cv = from_float(aa as f32 * (1.0 / 255.0));
                    for xx in x0..x1 {
                        self.pipe_coverage(xx, y, cv);
                    }
                }
            }
        }
    }
}

impl<'a> Blitter for SolidBlitter<'a> {
    fn blit_h(&mut self, x: i32, y: i32, width: i32) {
        self.blit_rect(x, y, width, 1);
    }
    fn blit_rect(&mut self, x: i32, y: i32, width: i32, height: i32) {
        for yy in y..y + height {
            self.span(x, yy, width, 255);
        }
    }
    fn blit_anti_h(&mut self, mut x: i32, y: i32, alphas: &[u8], runs: &[i16]) {
        let mut i = 0usize;
        loop {
            let run = runs[i];
            if run <= 0 {
                break;
            }
            self.span(x, y, run as i32, alphas[i]);
            x += run as i32;
            i += run as usize;
        }
    }
    fn blit_v(&mut self, x: i32, y: i32, height: i32, alpha: u8) {
        match self.st {
            Strategy::Legacy { pm, black, .. } => {
                if alpha == 0 || pm[3] == 0 {
                    return;
                }
                if x < 0 || x >= self.surf.width {
                    return;
                }
                let color = if black && alpha != 255 {
                    // The black blitter does not override blitV: SkARGB32_Blitter::blitV.
                    alpha_mul_q(pm, alpha as u32 + 1)
                } else if alpha != 255 {
                    alpha_mul_q(pm, alpha as u32 + 1)
                } else {
                    pm
                };
                let dst_scale = 256 - color[3] as u32;
                for yy in y..y + height {
                    if !self.in_y(yy) {
                        continue;
                    }
                    let d = self.get(x, yy);
                    self.put(x, yy, add4(color, alpha_mul_q(d, dst_scale)));
                }
            }
            Strategy::Pipeline { .. } => {
                let bounds = IRect::from_ltrb(x, y, x + 1, y + height);
                self.blit_mask(&[alpha], &bounds, 0, &bounds);
            }
        }
    }
    fn blit_anti_h2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        match self.st {
            Strategy::Legacy { pm, opaque, black } => {
                if !self.in_y(y) {
                    return;
                }
                for (k, a) in [(0, a0), (1, a1)] {
                    let xx = x + k;
                    if xx < 0 || xx >= self.surf.width {
                        continue;
                    }
                    let d = self.get(xx, y);
                    let out = if black {
                        add4([0, 0, 0, a], alpha_mul_q(d, 256 - a as u32))
                    } else if opaque {
                        fast_four_byte_interp(pm, d, a)
                    } else {
                        blend_argb32(pm, d, a)
                    };
                    self.put(xx, y, out);
                }
            }
            Strategy::Pipeline { .. } => {
                let bounds = IRect::from_ltrb(x, y, x + 2, y + 1);
                self.blit_mask(&[a0, a1], &bounds, 2, &bounds);
            }
        }
    }
    fn blit_anti_v2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        match self.st {
            Strategy::Legacy { pm, opaque, black } => {
                if x < 0 || x >= self.surf.width {
                    return;
                }
                for (k, a) in [(0, a0), (1, a1)] {
                    let yy = y + k;
                    if !self.in_y(yy) {
                        continue;
                    }
                    let d = self.get(x, yy);
                    let out = if black {
                        add4([0, 0, 0, a], alpha_mul_q(d, 256 - a as u32))
                    } else if opaque {
                        fast_four_byte_interp(pm, d, a)
                    } else {
                        blend_argb32(pm, d, a)
                    };
                    self.put(x, yy, out);
                }
            }
            Strategy::Pipeline { .. } => {
                let bounds = IRect::from_ltrb(x, y, x + 1, y + 2);
                self.blit_mask(&[a0, a1], &bounds, 1, &bounds);
            }
        }
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
            if !self.in_y(yy) {
                continue;
            }
            let Some((x0, x1)) = self.clip_x(clip.left, clip.width()) else {
                continue;
            };
            let row = (yy - mask_bounds.top) as usize * row_bytes;
            for xx in x0..x1 {
                let aa = mask[row + (xx - mask_bounds.left) as usize];
                match self.st {
                    Strategy::Legacy { pm, opaque, black } => {
                        let d = self.get(xx, yy);
                        let out = if black {
                            // (aa & alpha) + d·approx(255−aa)
                            add4([0, 0, 0, aa], approx_scale4(d, 255 - aa))
                        } else if opaque {
                            add4(approx_scale4(pm, aa), approx_scale4(d, 255 - aa))
                        } else {
                            let left = approx_scale4(pm, aa);
                            add4(left, approx_scale4(d, 255 - left[3]))
                        };
                        self.put(xx, yy, out);
                    }
                    Strategy::Pipeline { memset, .. } => {
                        let cov = aa as u16;
                        if memset.is_some() && cov == 255 {
                            self.pipe_full(xx, yy);
                        } else {
                            self.pipe_coverage(xx, yy, cov);
                        }
                    }
                }
            }
        }
    }
}

/// Read pixels like `readPixels(kUnpremul)`: highp `unpremul` and
/// `store_8888` with `to_unorm` rounding.
pub fn read_unpremul(data: &[u8], out: &mut [u8]) {
    for (src, dst) in data.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let a = src[3] as f32 * (1.0 / 255.0);
        let scale = if a == 0.0 { 0.0 } else { 1.0 / a };
        let conv = |v: u8| -> u8 {
            let f = v as f32 * (1.0 / 255.0) * scale;
            // to_unorm: round(min(max(0, v*255), 255)); round on AVX2 is
            // cvtps_epi32, round-half-even.
            let s = (f * 255.0).clamp(0.0, 255.0);
            s.round_ties_even() as u8
        };
        dst[0] = conv(src[0]);
        dst[1] = conv(src[1]);
        dst[2] = conv(src[2]);
        dst[3] = src[3];
    }
}

#[allow(dead_code)]
pub(crate) fn div255_accurate_pub(v: u16) -> u16 {
    div255_accurate(v)
}
