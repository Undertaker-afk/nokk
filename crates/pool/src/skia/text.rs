//! Canvas text as Chrome 151 on Linux lays it out.
//!
//! Layout is Blink over HarfBuzz: text size rounded to hundredths
//! (`FontDescription::EffectiveFontSize`), glyph advances from Skia
//! (linear, size truncated to 26.6 as FreeType does), GPOS kerning scaled
//! by HarfBuzz itself (`em_mult`), positions accumulated in 16.16.
//!
//! Glyphs are Skia over Fontations (`SkTypeface_Fontations`): skrifa outline
//! with the light autohinter, quarter-pixel subpixel x offset, `roundOut`
//! bounds, analytic-AA raster into an A8 mask
//! (`GenerateImageFromPath`), then the `SkMaskGamma` table
//! (contrast 0.2, gamma 1.2, by paint luminance). The shadow layer has no
//! table, but each glyph is blurred separately (mask filter in the scaler
//! context) and drawn with its own `blitMask`.

use super::aaa;
use super::blit::Blitter;
use super::blur::{self, A8Blitter, Mask};
use super::geometry::{IRect, Matrix, Point, Rect};
use super::path::{Path, PathBuilder};
use skrifa::outline::{
    DrawSettings, Engine, GlyphStyles, HintingInstance, HintingOptions, OutlinePen, SmoothMode,
    Target,
};
use skrifa::prelude::{LocationRef, Size};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use std::cell::RefCell;
use std::collections::HashMap;

/// `FontDescription::EffectiveFontSize`: text size to hundredths.
pub fn effective_size(css_px: f32) -> f32 {
    (css_px * 100.0).floor() / 100.0
}

/// `SkiaScalarToHarfBuzzPosition`: float -> 16.16, truncated.
pub fn to_hb_position(v: f32) -> i32 {
    let x = v * 65536.0;
    if x >= i32::MAX as f32 {
        i32::MAX
    } else if x <= i32::MIN as f32 {
        i32::MIN
    } else {
        x as i32
    }
}

/// `hb_font_t::em_mult`: font units -> 16.16.
pub fn em_mult(v: i32, mult: i64) -> i32 {
    ((v as i64 * mult + 32768) >> 16) as i32
}

/// Laid-out glyph: id, font from the fallback chain, x origin in CSS px.
#[derive(Clone, Copy, Debug)]
pub struct ShapedGlyph {
    pub gid: u16,
    pub font: usize,
    pub x: f32,
}

thread_local! {
    static STYLES: RefCell<HashMap<usize, &'static GlyphStyles>> = RefCell::new(HashMap::new());
    static GAMMA: RefCell<Option<&'static [[u8; 256]; 8]>> = const { RefCell::new(None) };
}

/// Autohinter glyph styles (`GlyphStyles::new`): expensive, computed once per font.
fn glyph_styles(
    bytes: &'static [u8],
    outlines: &skrifa::OutlineGlyphCollection,
) -> &'static GlyphStyles {
    STYLES.with(|m| {
        let key = bytes.as_ptr() as usize;
        if let Some(hit) = m.borrow().get(&key) {
            return *hit;
        }
        let got: &'static GlyphStyles = Box::leak(Box::new(GlyphStyles::new(outlines)));
        m.borrow_mut().insert(key, got);
        got
    })
}

// ── SkMaskGamma ───────────────────────────────────────────────────────────

/// `sk_t_scale255<3>`: three luminance bits -> 0..255.
fn scale255_3(i: u32) -> f32 {
    let base = i << 5;
    (base | (base >> 3) | (base >> 6)) as f32
}

/// `sk_float_round2int`.
fn round2int(x: f32) -> i32 {
    (x + 0.5).floor() as i32
}

/// `SkTMaskGamma_build_correcting_lut` for Skia's (pow) gamma, one table.
fn build_lut(lum: f32, contrast: f32, gamma: f32) -> [u8; 256] {
    let to_luma = |l: f32| l.powf(gamma);
    let from_luma = |l: f32| l.powf(1.0 / gamma);
    let src = lum / 255.0;
    let lin_src = to_luma(src);
    let dst = 1.0 - src;
    let lin_dst = to_luma(dst);
    let adjusted_contrast = contrast * lin_dst;
    let mut table = [0u8; 256];
    let apply_contrast = |srca: f32| srca + ((1.0 - srca) * adjusted_contrast * srca);
    if (src - dst).abs() < 1.0 / 256.0 {
        for (i, t) in table.iter_mut().enumerate() {
            let srca = apply_contrast(i as f32 / 255.0);
            *t = round2int(255.0 * srca).clamp(0, 255) as u8;
        }
    } else {
        for (i, t) in table.iter_mut().enumerate() {
            let srca = apply_contrast(i as f32 / 255.0);
            let dsta = 1.0 - srca;
            let lin_out = lin_src * srca + dsta * lin_dst;
            let out = from_luma(lin_out);
            let result = (out - dst) / (src - dst);
            *t = round2int(255.0 * result).clamp(0, 255) as u8;
        }
    }
    table
}

/// Tables for contrast SK_GAMMA_CONTRAST=0.2 and gamma SK_GAMMA_EXPONENT=1.2
/// after quantization in `SkScalerContextRec` (51/255 and 76/64).
fn gamma_tables() -> &'static [[u8; 256]; 8] {
    GAMMA.with(|g| {
        if let Some(t) = *g.borrow() {
            return t;
        }
        let contrast = 51.0f32 / 255.0;
        let gamma = 76.0f32 / 64.0;
        let mut tables = [[0u8; 256]; 8];
        for (i, t) in tables.iter_mut().enumerate() {
            *t = build_lut(scale255_3(i as u32), contrast, gamma);
        }
        let leaked: &'static [[u8; 256]; 8] = Box::leak(Box::new(tables));
        *g.borrow_mut() = Some(leaked);
        leaked
    })
}

/// `SkComputeLuminance` + `PreprocessRec`: gray of the paint's luminance.
pub fn luminance_byte(rgb: [u8; 3]) -> u8 {
    ((rgb[0] as u32 * 54 + rgb[1] as u32 * 183 + rgb[2] as u32 * 19) >> 8) as u8
}

// ── Glyph raster (SkTypeface_Fontations + SkScalerContext) ──────────────────

/// Skia pen (`VerbsPointsPen`): y down, repeated points collapsed,
/// `close` only after a segment.
struct SkPen {
    b: PathBuilder,
    started: bool,
    current: Point,
    last_verb: u8, // 0 none, 1 move, 2 seg
}

impl SkPen {
    fn new() -> SkPen {
        SkPen {
            b: PathBuilder::new(),
            started: false,
            current: Point::default(),
            last_verb: 0,
        }
    }
    fn going_to(&mut self, p: Point) {
        if !self.started {
            self.started = true;
            self.b.move_to(self.current);
            self.last_verb = 1;
        }
        self.current = p;
    }
}

impl OutlinePen for SkPen {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = Point::new(x, -y);
        if self.started {
            self.close();
            self.started = false;
        }
        self.current = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = Point::new(x, -y);
        if self.current != p {
            self.going_to(p);
            self.b.line_to(p);
            self.last_verb = 2;
        }
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let p0 = Point::new(cx0, -cy0);
        let p1 = Point::new(x, -y);
        if self.current != p0 || self.current != p1 {
            self.going_to(p1);
            self.b.quad_to(p0, p1);
            self.last_verb = 2;
        }
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let p0 = Point::new(cx0, -cy0);
        let p1 = Point::new(cx1, -cy1);
        let p2 = Point::new(x, -y);
        if self.current != p0 || self.current != p1 || self.current != p2 {
            self.going_to(p2);
            self.b.cubic_to(p0, p1, p2);
            self.last_verb = 2;
        }
    }
    fn close(&mut self) {
        if self.last_verb != 0 {
            self.b.close();
            self.last_verb = 0;
        }
    }
}

/// One glyph's mask in device coordinates relative to its origin.
pub struct GlyphMask {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub image: Vec<u8>,
}

/// Scaler context for one font, text size and matrix.
pub struct Scaler<'a> {
    outlines: skrifa::OutlineGlyphCollection<'a>,
    instance: Option<HintingInstance>,
    remaining: Matrix,
    scale: f32,
}

/// `SkScalerContextRec::computeMatrices(kVertical)` for a non-perspective
/// matrix: A = size * post2x2; s = |A.scaleY|; sA is the remainder.
fn compute_matrices(text_size: f32, post: &Matrix) -> (f32, Matrix) {
    let a = Matrix {
        sx: text_size * post.sx,
        kx: text_size * post.kx,
        ky: text_size * post.ky,
        sy: text_size * post.sy,
        tx: 0.0,
        ty: 0.0,
    };
    let skewed_or_flipped = a.kx != 0.0 || a.ky != 0.0 || a.sx < 0.0 || a.sy < 0.0;
    if skewed_or_flipped {
        // Rotation/skew: Skia's scaler removes rotation via Givens; here we
        // take the general case sA = A * S^-1 with s = |scaleY|.
        let s = a.sy.abs().max(1e-6);
        let mut sa = a;
        sa.pre_scale(1.0 / s, 1.0 / s);
        return (s, sa);
    }
    let s = a.sy.abs();
    if s <= 1e-6 || !s.is_finite() {
        return (1.0, Matrix::scale(0.0, 0.0));
    }
    if a.sx == a.sy {
        (s, Matrix::IDENTITY)
    } else {
        let mut sa = Matrix::IDENTITY;
        sa.sx = a.sx / s;
        (s, sa)
    }
}

impl<'a> Scaler<'a> {
    /// `text_size` is the rounded size, `post` the canvas matrix 2x2.
    pub fn new(
        bytes: &'static [u8],
        font: &FontRef<'a>,
        text_size: f32,
        post: &Matrix,
    ) -> Option<Scaler<'a>> {
        let (scale, remaining) = compute_matrices(text_size, post);
        let outlines = font.outline_glyphs();
        // kSlight -> light autohinter (`AutoHintingControl::ForceForGlyf`).
        let styles = glyph_styles(bytes, &outlines);
        let instance = HintingInstance::new(
            &outlines,
            Size::new(scale),
            LocationRef::default(),
            HintingOptions {
                engine: Engine::Auto(Some(styles.clone())),
                target: Target::Smooth {
                    mode: SmoothMode::Light,
                    symmetric_rendering: true,
                    preserve_linear_metrics: false,
                },
            },
        )
        .ok();
        Some(Scaler {
            outlines,
            instance,
            remaining,
            scale,
        })
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Glyph outline in device pixels (`generatePathImpl`).
    pub fn path(&self, gid: u16) -> Option<Path> {
        let glyph = self.outlines.get(GlyphId::from(gid))?;
        let mut pen = SkPen::new();
        let settings = match &self.instance {
            Some(inst) => DrawSettings::hinted(inst, false),
            None => DrawSettings::unhinted(Size::new(self.scale), LocationRef::default()),
        };
        glyph.draw(settings, &mut pen).ok()?;
        pen.close();
        let path = pen.b.detach();
        if self.remaining.is_identity() {
            Some(path)
        } else {
            Some(path.transform(&self.remaining))
        }
    }

    /// Glyph outline with subpixel offset (`internalGetPath`: makeOffset).
    pub fn offset_path(&self, gid: u16, sub_x: u32, sub_y: u32) -> Option<Path> {
        let mut path = self.path(gid)?;
        if sub_x != 0 || sub_y != 0 {
            path = path.transform(&Matrix::translate(sub_x as f32 * 0.25, sub_y as f32 * 0.25));
        }
        Some(path)
    }

    /// Fill mask (`GenerateMetricsFromPath` + `GenerateImageFromPath`):
    /// `sub_x`/`sub_y` are subpixel quarters (0..3).
    pub fn fill_mask(&self, gid: u16, sub_x: u32, sub_y: u32) -> Option<GlyphMask> {
        let path = self.offset_path(gid, sub_x, sub_y)?;
        mask_from_path(&path)
    }
}

/// Stroke mask (`internalGetPath` with fFrameWidth >= 0): outline into text-size
/// space by the inverse 2x2, SkStroke, back by the matrix, rasterized as a fill.
pub fn stroke_mask(
    path: &Path,
    post: &Matrix,
    params: &super::stroke::StrokeParams,
) -> Option<GlyphMask> {
    let inverse = post.invert()?;
    let local = if post.is_identity() {
        path.clone()
    } else {
        path.transform(&inverse)
    };
    let stroked = super::stroke::stroke_path(&local, params)?;
    let dev = if post.is_identity() {
        stroked
    } else {
        stroked.transform(post)
    };
    mask_from_path(&dev)
}

/// Mask from an outline: `roundOut` bounds, AAA raster into A8 via `SkA8_Blitter`.
pub fn mask_from_path(path: &Path) -> Option<GlyphMask> {
    if path.pts.is_empty() {
        return None;
    }
    let bounds: Rect = path.bounds();
    let ir = bounds.round_out();
    let left = ir.left.clamp(i16::MIN as i32, i16::MAX as i32);
    let top = ir.top.clamp(i16::MIN as i32, i16::MAX as i32);
    let width = ir.width().clamp(0, u16::MAX as i32);
    let height = ir.height().clamp(0, u16::MAX as i32);
    if width == 0 || height == 0 {
        return None;
    }
    let mut image = vec![0u8; (width * height) as usize];
    {
        let mut dev = path.transform(&Matrix::translate(-(left as f32), -(top as f32)));
        dev.resolve_convexity();
        let mut a8 = A8Blitter::new(&mut image, width, height);
        aaa::anti_fill_path(&dev, &IRect::from_ltrb(0, 0, width, height), &mut a8);
    }
    Some(GlyphMask {
        left,
        top,
        width,
        height,
        image,
    })
}

/// `applyLUTToA8Mask` by paint luminance (index = top three bits).
pub fn apply_gamma(mask: &mut GlyphMask, lum: u8) {
    let table = &gamma_tables()[(lum >> 5) as usize];
    for v in mask.image.iter_mut() {
        *v = table[*v as usize];
    }
}

/// Blurred glyph mask (mask filter in the scaler context): bounds and
/// image already include the blur margins.
pub fn blur_mask(mask: &GlyphMask, sigma: f64) -> Option<GlyphMask> {
    let src = Mask {
        bounds: IRect::from_ltrb(
            mask.left,
            mask.top,
            mask.left + mask.width,
            mask.top + mask.height,
        ),
        row_bytes: mask.width as usize,
        image: mask.image.clone(),
    };
    let (dst, _) = blur::mask_blur(sigma, &src);
    let (w, h) = (dst.bounds.width(), dst.bounds.height());
    if w <= 0 || h <= 0 || dst.image.is_empty() {
        return None;
    }
    let mut image = vec![0u8; (w * h) as usize];
    for y in 0..h as usize {
        image[y * w as usize..(y + 1) * w as usize]
            .copy_from_slice(&dst.image[y * dst.row_bytes..y * dst.row_bytes + w as usize]);
    }
    Some(GlyphMask {
        left: dst.bounds.left,
        top: dst.bounds.top,
        width: w,
        height: h,
        image,
    })
}

/// Glyph device position (`prepare_for_direct_mask_drawing`): position matrix
/// plus half a sample step, then `floor`; subpixel per
/// `SkPackedGlyphID::PackIDSkPoint` and axis alignment.
pub struct DevicePos {
    pub x: i32,
    pub y: i32,
    pub sub_x: u32,
    pub sub_y: u32,
}

/// `pos_matrix` = CTM.preTranslate(origin), without the rounding bias.
pub fn device_position(
    pos_matrix: &Matrix,
    glyph_x: f32,
    axis_x_only: bool,
    axis_y_only: bool,
) -> Option<DevicePos> {
    // halfAxisSampleFreq: subpixel x -> 1/8, y -> 1/2 (kX).
    let (hx, hy) = if axis_x_only {
        (0.125f32, 0.5f32)
    } else if axis_y_only {
        (0.5, 0.125)
    } else {
        (0.125, 0.125)
    };
    let mut m = *pos_matrix;
    m.tx += hx;
    m.ty += hy;
    let p = m.map_point(Point::new(glyph_x, 0.0));
    if !p.x.is_finite() || !p.y.is_finite() {
        return None;
    }
    let fx = p.x.floor();
    let fy = p.y.floor();
    let sub = |v: f32, f: f32| -> u32 { (((v - f) + 1.0) * 4.0) as i32 as u32 & 3 };
    let sub_x = if axis_y_only { 0 } else { sub(p.x, fx) };
    let sub_y = if axis_x_only { 0 } else { sub(p.y, fy) };
    Some(DevicePos {
        x: fx as i32,
        y: fy as i32,
        sub_x,
        sub_y,
    })
}

/// Axis alignment (`computeAxisAlignmentForHText`) for the canvas matrix.
pub fn axis_alignment(post: &Matrix) -> (bool, bool) {
    if post.ky == 0.0 {
        (true, false)
    } else if post.sx == 0.0 {
        (false, true)
    } else {
        (false, false)
    }
}

/// `paintMasks`: draw a glyph mask with the blitter inside the canvas clip.
pub fn blit_glyph(blitter: &mut dyn Blitter, mask: &GlyphMask, x: i32, y: i32, clip: &IRect) {
    let bounds = IRect::from_ltrb(
        mask.left + x,
        mask.top + y,
        mask.left + x + mask.width,
        mask.top + y + mask.height,
    );
    if let Some(cr) = bounds.intersect(clip) {
        blitter.blit_mask(&mask.image, &bounds, mask.width as usize, &cr);
    }
}

/// `SkMatrix::preTranslate` for a non-perspective matrix.
pub fn pre_translate(m: &Matrix, dx: f32, dy: f32) -> Matrix {
    let mut out = *m;
    if m.is_translate_only() {
        out.tx += dx;
        out.ty += dy;
    } else {
        out.tx += m.sx * dx + m.kx * dy;
        out.ty += m.ky * dx + m.sy * dy;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_size_floors_to_hundredths() {
        assert_eq!(effective_size(27.77777777), 27.77);
        assert_eq!(effective_size(10.666666666666666), 10.66);
        assert_eq!(effective_size(40.0), 40.0);
    }

    #[test]
    fn em_mult_matches_harfbuzz() {
        // 27.77px: x_scale = 1819934, upem 2048 -> R/y kerning -82 = -72868.
        let x_scale = to_hb_position(27.77) as i64;
        let x_mult = (x_scale << 16) / 2048;
        assert_eq!(em_mult(-82, x_mult), -72868);
        assert_eq!(em_mult(-264, x_mult), -234601);
    }

    #[test]
    fn gamma_table_is_identity_at_ends() {
        let t = gamma_tables();
        assert_eq!(t[0][0], 0);
        assert_eq!(t[0][255], 255);
        assert_eq!(t[4][0], 0);
        assert_eq!(t[4][255], 255);
    }
}
