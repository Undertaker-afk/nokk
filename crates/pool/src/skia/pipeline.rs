//! `SkRasterPipeline` in exact highp (float), the variant Chrome uses for
//! gradients (the conic and dither stages exist only there).
//! Arithmetic follows `SkRasterPipeline_opts.h` for AVX2: `mad` is a real
//! fma, byte rounding is round-half-even, `1/255` is a multiply by a
//! constant.

use super::blit::{BlendMode, Blitter};
use super::geometry::IRect;

#[inline]
fn mad(f: f32, m: f32, a: f32) -> f32 {
    f.mul_add(m, a)
}
#[inline]
fn inv(v: f32) -> f32 {
    1.0 - v
}
#[inline]
fn from_byte(b: u8) -> f32 {
    b as f32 * (1.0 / 255.0)
}
#[inline]
fn to_unorm(v: f32) -> u8 {
    // round(min(max(0, mad(v, 255, 0)), 255)): cvtps_epi32, round-half-even.
    let s = mad(v, 255.0, 0.0).max(0.0).min(255.0);
    s.round_ties_even() as u8
}
#[inline]
fn clamp_01(v: f32) -> f32 {
    v.max(0.0).min(1.0)
}

/// The stages we need (named as in Skia).
#[derive(Clone, Debug)]
pub enum Stage {
    SeedShader,
    MatrixTranslate([f32; 2]),
    MatrixScaleTranslate([f32; 4]),
    Matrix2x3([f32; 9]),
    XyToRadius,
    XyTo2ptConicalStrip {
        p0: f32,
    },
    XyTo2ptConicalFocalOnCircle,
    XyTo2ptConicalWellBehaved {
        p0: f32,
    },
    XyTo2ptConicalGreater {
        p0: f32,
    },
    XyTo2ptConicalSmaller {
        p0: f32,
    },
    Alter2ptConicalCompensateFocal {
        p1: f32,
    },
    Alter2ptConicalUnswap,
    NegateX,
    Mask2ptConicalNan,
    Mask2ptConicalDegenerates,
    ClampX1,
    EvenlySpaced2StopGradient {
        factor: [f32; 4],
        bias: [f32; 4],
    },
    Gradient {
        ts: Vec<f32>,
        factors: Vec<[f32; 4]>,
        biases: Vec<[f32; 4]>,
    },
    ApplyVectorMask,
    MoveSrcDst,
    UniformColor([f32; 4]),
    Blend(BlendMode),
    Dither(f32),
    Clamp01,
    Premul,
    Unpremul,
    Scale1Float(f32),
    Lerp1Float(f32),
    /// Coverage from the mask (value supplied at call time).
    ScaleU8,
    LerpU8,
    LoadDst,
    Store,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Px {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// State of one pipeline lane for pixel (dx, dy).
struct Lane {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
    dr: f32,
    dg: f32,
    db: f32,
    da: f32,
    mask: bool,
}

/// `SkBlendMode_AppendStages` in highp: blending premul colors.
fn blend_highp(mode: BlendMode, l: &mut Lane) {
    let (s, d) = ([l.r, l.g, l.b, l.a], [l.dr, l.dg, l.db, l.da]);
    let sa = s[3];
    let da = d[3];
    let mut out = [0f32; 4];
    let per = |f: &dyn Fn(f32, f32) -> f32, out: &mut [f32; 4]| {
        for i in 0..4 {
            out[i] = f(s[i], d[i]);
        }
    };
    let per_srcover_alpha = |f: &dyn Fn(f32, f32) -> f32, out: &mut [f32; 4]| {
        for i in 0..3 {
            out[i] = f(s[i], d[i]);
        }
        out[3] = mad(da, inv(sa), sa);
    };
    match mode {
        BlendMode::Clear => out = [0.0; 4],
        BlendMode::Src => out = s,
        BlendMode::Dst => out = d,
        BlendMode::SrcATop => per(&|s, d| mad(s, da, d * inv(sa)), &mut out),
        BlendMode::DstATop => per(&|s, d| mad(d, sa, s * inv(da)), &mut out),
        BlendMode::SrcIn => per(&|s, _d| s * da, &mut out),
        BlendMode::DstIn => per(&|_s, d| d * sa, &mut out),
        BlendMode::SrcOut => per(&|s, _d| s * inv(da), &mut out),
        BlendMode::DstOut => per(&|_s, d| d * inv(sa), &mut out),
        BlendMode::SrcOver => per(&|s, d| mad(d, inv(sa), s), &mut out),
        BlendMode::DstOver => per(&|s, d| mad(s, inv(da), d), &mut out),
        BlendMode::Modulate => per(&|s, d| s * d, &mut out),
        BlendMode::Multiply => per(&|s, d| mad(s, inv(da), mad(d, inv(sa), s * d)), &mut out),
        BlendMode::Plus => per(&|s, d| (s + d).min(1.0), &mut out),
        BlendMode::Screen => per(&|s, d| s + d - s * d, &mut out),
        BlendMode::Xor => per(&|s, d| mad(s, inv(da), d * inv(sa)), &mut out),
        BlendMode::Darken => per_srcover_alpha(&|s, d| s + d - (s * da).max(d * sa), &mut out),
        BlendMode::Lighten => per_srcover_alpha(&|s, d| s + d - (s * da).min(d * sa), &mut out),
        BlendMode::Difference => {
            per_srcover_alpha(&|s, d| s + d - 2.0 * (s * da).min(d * sa), &mut out)
        }
        BlendMode::Exclusion => per_srcover_alpha(&|s, d| s + d - 2.0 * s * d, &mut out),
        _ => per(&|s, d| mad(d, inv(sa), s), &mut out),
    }
    l.r = out[0];
    l.g = out[1];
    l.b = out[2];
    l.a = out[3];
}

/// Pipeline built once per blitter and run per pixel.
pub struct Pipeline {
    pub stages: Vec<Stage>,
}

impl Pipeline {
    /// Run pixel (dx, dy). `dst` is the premul RGBA8 destination (for LoadDst),
    /// `cov` the coverage byte for ScaleU8/LerpU8. Returns the bytes for Store.
    pub fn run_px(&self, dx: i32, dy: i32, dst: [u8; 4], cov: u8) -> [u8; 4] {
        let mut l = Lane {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
            dr: 0.0,
            dg: 0.0,
            db: 0.0,
            da: 0.0,
            mask: true,
        };
        for st in &self.stages {
            match st {
                Stage::SeedShader => {
                    l.r = dx as f32 + 0.5;
                    l.g = dy as f32 + 0.5;
                    l.b = 0.0;
                    l.a = 0.0;
                }
                Stage::MatrixTranslate(m) => {
                    l.r += m[0];
                    l.g += m[1];
                }
                Stage::MatrixScaleTranslate(m) => {
                    l.r = mad(l.r, m[0], m[2]);
                    l.g = mad(l.g, m[1], m[3]);
                }
                Stage::Matrix2x3(m) => {
                    let rr = mad(l.r, m[0], mad(l.g, m[1], m[2]));
                    let gg = mad(l.r, m[3], mad(l.g, m[4], m[5]));
                    l.r = rr;
                    l.g = gg;
                }
                Stage::XyToRadius => {
                    let x2 = l.r * l.r;
                    let y2 = l.g * l.g;
                    l.r = (x2 + y2).sqrt();
                }
                Stage::XyTo2ptConicalStrip { p0 } => {
                    let (x, y) = (l.r, l.g);
                    l.r = x + (p0 - y * y).sqrt();
                }
                Stage::XyTo2ptConicalFocalOnCircle => {
                    let (x, y) = (l.r, l.g);
                    l.r = x + y * y / x;
                }
                Stage::XyTo2ptConicalWellBehaved { p0 } => {
                    let (x, y) = (l.r, l.g);
                    l.r = (x * x + y * y).sqrt() - x * p0;
                }
                Stage::XyTo2ptConicalGreater { p0 } => {
                    let (x, y) = (l.r, l.g);
                    l.r = (x * x - y * y).sqrt() - x * p0;
                }
                Stage::XyTo2ptConicalSmaller { p0 } => {
                    let (x, y) = (l.r, l.g);
                    l.r = -(x * x - y * y).sqrt() - x * p0;
                }
                Stage::Alter2ptConicalCompensateFocal { p1 } => l.r += p1,
                Stage::Alter2ptConicalUnswap => l.r = 1.0 - l.r,
                Stage::NegateX => l.r = -l.r,
                Stage::Mask2ptConicalNan => {
                    let degenerate = l.r.is_nan();
                    if degenerate {
                        l.r = 0.0;
                    }
                    l.mask = !degenerate;
                }
                Stage::Mask2ptConicalDegenerates => {
                    let degenerate = l.r <= 0.0 || l.r.is_nan();
                    if degenerate {
                        l.r = 0.0;
                    }
                    l.mask = !degenerate;
                }
                Stage::ClampX1 => l.r = clamp_01(l.r),
                Stage::EvenlySpaced2StopGradient { factor, bias } => {
                    let t = l.r;
                    l.r = mad(t, factor[0], bias[0]);
                    l.g = mad(t, factor[1], bias[1]);
                    l.b = mad(t, factor[2], bias[2]);
                    l.a = mad(t, factor[3], bias[3]);
                }
                Stage::Gradient {
                    ts,
                    factors,
                    biases,
                } => {
                    let t = l.r;
                    let mut idx = 0usize;
                    for i in 1..ts.len() {
                        if t >= ts[i] {
                            idx += 1;
                        }
                    }
                    let f = factors[idx];
                    let b = biases[idx];
                    l.r = mad(t, f[0], b[0]);
                    l.g = mad(t, f[1], b[1]);
                    l.b = mad(t, f[2], b[2]);
                    l.a = mad(t, f[3], b[3]);
                }
                Stage::ApplyVectorMask => {
                    if !l.mask {
                        l.r = 0.0;
                        l.g = 0.0;
                        l.b = 0.0;
                        l.a = 0.0;
                    }
                }
                Stage::MoveSrcDst => {
                    l.dr = l.r;
                    l.dg = l.g;
                    l.db = l.b;
                    l.da = l.a;
                }
                Stage::UniformColor(c) => {
                    l.r = c[0];
                    l.g = c[1];
                    l.b = c[2];
                    l.a = c[3];
                }
                Stage::Blend(mode) => blend_highp(*mode, &mut l),
                Stage::Dither(rate) => {
                    let x = dx as u32;
                    let y = (dy as u32) ^ x;
                    let m = (y & 1) << 5
                        | (x & 1) << 4
                        | (y & 2) << 2
                        | (x & 2) << 1
                        | (y & 4) >> 1
                        | (x & 4) >> 2;
                    let dither = mad(m as f32, 2.0 / 128.0, -63.0 / 128.0);
                    l.r = mad(dither, *rate, l.r);
                    l.g = mad(dither, *rate, l.g);
                    l.b = mad(dither, *rate, l.b);
                    l.r = l.r.min(l.a).max(0.0);
                    l.g = l.g.min(l.a).max(0.0);
                    l.b = l.b.min(l.a).max(0.0);
                }
                Stage::Clamp01 => {
                    l.r = clamp_01(l.r);
                    l.g = clamp_01(l.g);
                    l.b = clamp_01(l.b);
                    l.a = clamp_01(l.a);
                }
                Stage::Premul => {
                    l.r *= l.a;
                    l.g *= l.a;
                    l.b *= l.a;
                }
                Stage::Unpremul => {
                    let s = 1.0 / l.a;
                    let scale = if s < f32::INFINITY { s } else { 0.0 };
                    l.r *= scale;
                    l.g *= scale;
                    l.b *= scale;
                }
                Stage::Scale1Float(c) => {
                    l.r *= c;
                    l.g *= c;
                    l.b *= c;
                    l.a *= c;
                }
                Stage::Lerp1Float(c) => {
                    l.r = mad(l.r - l.dr, *c, l.dr);
                    l.g = mad(l.g - l.dg, *c, l.dg);
                    l.b = mad(l.b - l.db, *c, l.db);
                    l.a = mad(l.a - l.da, *c, l.da);
                }
                Stage::ScaleU8 => {
                    let c = from_byte(cov);
                    l.r *= c;
                    l.g *= c;
                    l.b *= c;
                    l.a *= c;
                }
                Stage::LerpU8 => {
                    let c = from_byte(cov);
                    l.r = mad(l.r - l.dr, c, l.dr);
                    l.g = mad(l.g - l.dg, c, l.dg);
                    l.b = mad(l.b - l.db, c, l.db);
                    l.a = mad(l.a - l.da, c, l.da);
                }
                Stage::LoadDst => {
                    l.dr = from_byte(dst[0]);
                    l.dg = from_byte(dst[1]);
                    l.db = from_byte(dst[2]);
                    l.da = from_byte(dst[3]);
                }
                Stage::Store => {
                    return [to_unorm(l.r), to_unorm(l.g), to_unorm(l.b), to_unorm(l.a)];
                }
            }
        }
        [to_unorm(l.r), to_unorm(l.g), to_unorm(l.b), to_unorm(l.a)]
    }
}

/// `SkRasterPipelineBlitter` with an arbitrary color pipeline (shader,
/// color filter, dither) in highp.
pub struct PipelineBlitter<'a> {
    data: &'a mut [u8],
    width: i32,
    height: i32,
    color: Vec<Stage>,
    mode: BlendMode,
    rect_p: Pipeline,
    anti_h_p: Pipeline,
    mask_p: Pipeline,
}

impl<'a> PipelineBlitter<'a> {
    /// `color`: color stages (Skia appends `clamp_01` after them); `mode`:
    /// paint blend mode; `is_opaque` reduces srcover to src.
    pub fn new(
        data: &'a mut [u8],
        width: i32,
        height: i32,
        color: Vec<Stage>,
        mode: BlendMode,
        is_opaque: bool,
    ) -> Self {
        let mut mode = mode;
        if is_opaque && mode == BlendMode::SrcOver {
            mode = BlendMode::Src;
        }
        let blend = |p: &mut Vec<Stage>| {
            if mode != BlendMode::Src {
                p.push(Stage::Blend(mode));
            }
        };
        // blitRect: color, clamp, [load dst, blend], store. Chrome disables the
        // srcover_rgba_8888 fast path (the canvas has a colorSpace), so srcover
        // takes the general path too.
        let mut rect_p = color.clone();
        rect_p.push(Stage::Clamp01);
        if mode != BlendMode::Src {
            rect_p.push(Stage::LoadDst);
            blend(&mut rect_p);
        }
        rect_p.push(Stage::Store);
        // blitAntiH: coverage as float.
        let mut anti_h_p = color.clone();
        anti_h_p.push(Stage::Clamp01);
        if mode.should_pre_scale_coverage() {
            anti_h_p.push(Stage::Scale1Float(0.0));
            anti_h_p.push(Stage::LoadDst);
            blend(&mut anti_h_p);
        } else {
            anti_h_p.push(Stage::LoadDst);
            blend(&mut anti_h_p);
            anti_h_p.push(Stage::Lerp1Float(0.0));
        }
        anti_h_p.push(Stage::Store);
        // blitMask (A8): coverage from the mask.
        let mut mask_p = color.clone();
        mask_p.push(Stage::Clamp01);
        if mode.should_pre_scale_coverage() {
            mask_p.push(Stage::ScaleU8);
            mask_p.push(Stage::LoadDst);
            blend(&mut mask_p);
        } else {
            mask_p.push(Stage::LoadDst);
            blend(&mut mask_p);
            mask_p.push(Stage::LerpU8);
        }
        mask_p.push(Stage::Store);
        PipelineBlitter {
            data,
            width,
            height,
            color,
            mode,
            rect_p: Pipeline { stages: rect_p },
            anti_h_p: Pipeline { stages: anti_h_p },
            mask_p: Pipeline { stages: mask_p },
        }
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }
    #[inline]
    fn put(&mut self, x: i32, y: i32, p: [u8; 4]) {
        let i = ((y * self.width + x) * 4) as usize;
        self.data[i..i + 4].copy_from_slice(&p);
    }
    #[inline]
    fn in_y(&self, y: i32) -> bool {
        y >= 0 && y < self.height
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

    fn run_rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        for yy in y..y + h {
            if !self.in_y(yy) {
                continue;
            }
            let Some((x0, x1)) = self.xr(x, w) else {
                continue;
            };
            for xx in x0..x1 {
                let d = self.get(xx, yy);
                let out = self.rect_p.run_px(xx, yy, d, 255);
                self.put(xx, yy, out);
            }
        }
    }

    fn run_anti_h(&mut self, x: i32, y: i32, w: i32, aa: u8) {
        if !self.in_y(y) {
            return;
        }
        let Some((x0, x1)) = self.xr(x, w) else {
            return;
        };
        let cov = aa as f32 * (1.0 / 255.0);
        // Plug the coverage into the Scale1Float/Lerp1Float stages.
        for st in self.anti_h_p.stages.iter_mut() {
            match st {
                Stage::Scale1Float(c) | Stage::Lerp1Float(c) => *c = cov,
                _ => {}
            }
        }
        for xx in x0..x1 {
            let d = self.get(xx, y);
            let out = self.anti_h_p.run_px(xx, y, d, 255);
            self.put(xx, y, out);
        }
    }

    #[allow(dead_code)]
    pub fn mode(&self) -> BlendMode {
        self.mode
    }
    #[allow(dead_code)]
    pub fn color_stages(&self) -> &[Stage] {
        &self.color
    }
}

impl<'a> Blitter for PipelineBlitter<'a> {
    fn blit_h(&mut self, x: i32, y: i32, width: i32) {
        self.run_rect(x, y, width, 1);
    }
    fn blit_rect(&mut self, x: i32, y: i32, width: i32, height: i32) {
        self.run_rect(x, y, width, height);
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
                255 => self.run_rect(x, y, run as i32, 1),
                aa => self.run_anti_h(x, y, run as i32, aa),
            }
            x += run as i32;
            i += run as usize;
        }
    }
    fn blit_v(&mut self, x: i32, y: i32, height: i32, alpha: u8) {
        let bounds = IRect::from_ltrb(x, y, x + 1, y + height);
        self.blit_mask(&[alpha], &bounds, 0, &bounds);
    }
    fn blit_anti_h2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        let bounds = IRect::from_ltrb(x, y, x + 2, y + 1);
        self.blit_mask(&[a0, a1], &bounds, 2, &bounds);
    }
    fn blit_anti_v2(&mut self, x: i32, y: i32, a0: u8, a1: u8) {
        let bounds = IRect::from_ltrb(x, y, x + 1, y + 2);
        self.blit_mask(&[a0, a1], &bounds, 1, &bounds);
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
            let Some((x0, x1)) = self.xr(clip.left, clip.width()) else {
                continue;
            };
            let row = (yy - mask_bounds.top) as usize * row_bytes;
            for xx in x0..x1 {
                let aa = mask[row + (xx - mask_bounds.left) as usize];
                let d = self.get(xx, yy);
                let out = self.mask_p.run_px(xx, yy, d, aa);
                self.put(xx, yy, out);
            }
        }
    }
}
