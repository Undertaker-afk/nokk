//! Canvas gradients: `CanvasGradient` → Blink's `Gradient::CreateRadial` →
//! `SkShaders::TwoPointConicalGradient` → `SkConicalGradient` and its
//! pipeline stages (`SkConicalGradient.cpp`, `SkGradientBaseShader.cpp`).

use super::blit::BlendMode;
use super::geometry::{nearly_equal, Matrix, Point, SCALAR_NEARLY_ZERO};
use super::pipeline::Stage;

/// Description from JS: kind (0 linear, 1 radial), centres, radii,
/// stops (position, colour 0..255 straight alpha).
#[derive(Clone, Debug)]
pub struct GradientDesc {
    pub radial: bool,
    pub p0: Point,
    pub p1: Point,
    pub r0: f32,
    pub r1: f32,
    pub stops: Vec<(f32, [f32; 4])>,
}

impl GradientDesc {
    /// Parses the flat `encodeGrad` vector from JS.
    pub fn parse(g: &[f32]) -> Option<GradientDesc> {
        if g.len() < 8 {
            return None;
        }
        let n = g[7].max(0.0) as usize;
        let mut stops = Vec::with_capacity(n);
        let mut i = 8;
        for _ in 0..n {
            if i + 5 > g.len() {
                break;
            }
            stops.push((
                g[i],
                [
                    g[i + 1] / 255.0,
                    g[i + 2] / 255.0,
                    g[i + 3] / 255.0,
                    g[i + 4] / 255.0,
                ],
            ));
            i += 5;
        }
        Some(GradientDesc {
            radial: g[0].round() as i32 == 1,
            p0: Point::new(g[1], g[2]),
            p1: Point::new(g[3], g[4]),
            r0: g[5],
            r1: g[6],
            stops,
        })
    }
}

/// Built shader: points-to-unit-space matrix and stages.
pub struct Shader {
    pub pts_to_unit: Matrix,
    pub stages: Vec<Stage>,
    pub post: Vec<Stage>,
    pub colors: Vec<[f32; 4]>,
    pub positions: Option<Vec<f32>>,
    pub colors_are_opaque: bool,
    /// `SkShaderBase::isOpaque()`: always false for conical, for radial
    /// it depends on the colours (with clamp).
    pub is_opaque: bool,
    /// One colour everywhere: `MakeDegenerateGradient` (empty shader or colour).
    pub constant: Option<[f32; 4]>,
}

const DEGENERATE_THRESHOLD: f32 = 1.0 / (1 << 15) as f32;

fn length(p: Point) -> f32 {
    (p.x * p.x + p.y * p.y).sqrt()
}

/// Blink's `Gradient::FillSkiaStops`: stops sorted, end stops at 0 and 1
/// added when missing.
fn fill_skia_stops(desc: &GradientDesc) -> (Vec<[f32; 4]>, Vec<f32>) {
    let mut stops = desc.stops.clone();
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut colors = Vec::new();
    let mut pos = Vec::new();
    if stops.is_empty() {
        pos.push(0.0);
        colors.push([0.0, 0.0, 0.0, 0.0]);
    } else if stops[0].0 > 0.0 {
        pos.push(0.0);
        colors.push(stops[0].1);
    }
    for (p, c) in &stops {
        pos.push(*p);
        colors.push(*c);
    }
    if *pos.last().unwrap() < 1.0 {
        pos.push(1.0);
        colors.push(*colors.last().unwrap());
    }
    (colors, pos)
}

/// Builds the shader for a canvas fill.
pub fn make_shader(desc: &GradientDesc) -> Option<Shader> {
    let (colors, pos) = fill_skia_stops(desc);
    if desc.radial {
        let r0 = desc.r0.max(0.0);
        let r1 = desc.r1.max(0.0);
        two_point_conical(desc.p0, r0, desc.p1, r1, colors, pos)
    } else {
        linear(desc.p0, desc.p1, colors, pos)
    }
}

/// `SkGradientBaseShader`: shared stop preparation (constructor).
fn base(colors: Vec<[f32; 4]>, pos: Vec<f32>, pts_to_unit: Matrix) -> Shader {
    let mut fcolors: Vec<[f32; 4]> = Vec::new();
    let mut colors_are_opaque = true;
    let first_implicit = pos[0] > 0.0;
    let mut last_implicit = *pos.last().unwrap() != 1.0;
    if first_implicit {
        fcolors.push(colors[0]);
    }
    for c in &colors {
        fcolors.push(*c);
        colors_are_opaque = colors_are_opaque && c[3] == 1.0;
    }
    if last_implicit {
        fcolors.push(*colors.last().unwrap());
    }
    // Positions: first forced to 0, last to 1, monotonic.
    let mut positions: Vec<f32> = vec![0.0];
    let mut prev = 0.0f32;
    let start_index = if first_implicit { 0 } else { 1 };
    let count = pos.len() + last_implicit as usize;
    let mut uniform = true;
    let uniform_step = pos[start_index] - prev;
    for i in start_index..count {
        let mut curr = 1.0f32;
        if i != pos.len() {
            curr = pos[i].max(prev).min(1.0);
            if curr == 1.0 && last_implicit {
                last_implicit = false;
            }
        }
        uniform &= nearly_equal(uniform_step, curr - prev);
        positions.push(curr);
        prev = curr;
    }
    let _ = last_implicit;
    let positions = if uniform {
        None
    } else {
        // Dedupe repeats (clamp): keep the leftmost and rightmost of a group.
        let mut dp: Vec<f32> = Vec::new();
        let mut dc: Vec<[f32; 4]> = Vec::new();
        let n = fcolors.len();
        let mut i = 0usize;
        let mut j = 1usize;
        while j <= n {
            if j == n || positions[i] != positions[j] {
                let dup = j - i > 1;
                dp.push(positions[i]);
                dc.push(fcolors[i]);
                if dup {
                    dp.push(positions[j - 1]);
                    dc.push(fcolors[j - 1]);
                }
                i = j;
            }
            j += 1;
        }
        fcolors = dc;
        Some(dp)
    };
    Shader {
        pts_to_unit,
        stages: Vec::new(),
        post: Vec::new(),
        colors: fcolors,
        positions,
        colors_are_opaque,
        is_opaque: false,
        constant: None,
    }
}

fn linear(p0: Point, p1: Point, colors: Vec<[f32; 4]>, pos: Vec<f32>) -> Option<Shader> {
    if length(p1.sub(p0)) <= DEGENERATE_THRESHOLD {
        // Degenerate: with clamp, the last colour everywhere.
        let mut s = base(colors.clone(), pos, Matrix::IDENTITY);
        s.constant = Some(*colors.last().unwrap());
        return Some(s);
    }
    // pts_to_unit: unit vector along p0→p1 (SkLinearGradient).
    let vec = p1.sub(p0);
    let mag = length(vec);
    let inv = if mag != 0.0 { 1.0 / mag } else { 0.0 };
    let v = Point::new(vec.x * inv, vec.y * inv);
    // setSinCos(-v.y, v.x, p0.x, p0.y): rotation about p0.
    let (s, c) = (-v.y, v.x);
    let one_minus_c = 1.0 - c;
    let mut m = Matrix {
        sx: c,
        kx: -s,
        tx: s * p0.y + one_minus_c * p0.x,
        ky: s,
        sy: c,
        ty: -s * p0.x + one_minus_c * p0.y,
    };
    m.post_translate(-p0.x, -p0.y);
    m.post_scale(inv, inv);
    let mut s = base(colors, pos, m);
    s.is_opaque = s.colors_are_opaque;
    Some(s)
}

fn two_point_conical(
    c0: Point,
    r0: f32,
    c1: Point,
    r1: f32,
    colors: Vec<[f32; 4]>,
    pos: Vec<f32>,
) -> Option<Shader> {
    if r0 < 0.0 || r1 < 0.0 {
        return None;
    }
    let d = length(c0.sub(c1));
    if d <= DEGENERATE_THRESHOLD {
        if (r0 - r1).abs() <= DEGENERATE_THRESHOLD {
            if r1 > DEGENERATE_THRESHOLD {
                // Zero-width ring: first colour up to 1, then the last.
                let front = colors[0];
                let back = *colors.last().unwrap();
                return radial(c0, r1, vec![front, front, back], vec![0.0, 1.0, 1.0]);
            }
            let mut s = base(colors.clone(), pos, Matrix::IDENTITY);
            s.constant = Some(*colors.last().unwrap());
            return Some(s);
        } else if r0 <= DEGENERATE_THRESHOLD {
            return radial(c0, r1, colors, pos);
        }
    }
    let mut colors = colors;
    let mut pos = pos;
    if colors.len() == 1 {
        colors = vec![colors[0], colors[0]];
        pos = Vec::new();
    }
    // SkConicalGradient::Create
    let mut gradient_matrix;
    let ty;
    let mut focal = FocalData::default();
    if length(c0.sub(c1)).abs() <= SCALAR_NEARLY_ZERO {
        if r0.max(r1).abs() <= SCALAR_NEARLY_ZERO || nearly_equal(r0, r1) {
            return None;
        }
        let scale = 1.0 / r0.max(r1);
        gradient_matrix = Matrix::translate(-c1.x, -c1.y);
        gradient_matrix.post_scale(scale, scale);
        ty = ConicalType::Radial;
    } else {
        gradient_matrix =
            Matrix::poly_to_poly2([c0, c1], [Point::new(0.0, 0.0), Point::new(1.0, 0.0)])?;
        ty = if (r1 - r0).abs() <= SCALAR_NEARLY_ZERO {
            ConicalType::Strip
        } else {
            ConicalType::Focal
        };
    }
    if ty == ConicalType::Focal {
        let d_center = length(c0.sub(c1));
        if !focal.set(r0 / d_center, r1 / d_center, &mut gradient_matrix) {
            return None;
        }
    }
    let mut s = base(
        colors,
        if pos.is_empty() { vec![0.0, 1.0] } else { pos },
        gradient_matrix,
    );
    s.is_opaque = false;
    // appendGradientStages
    let d_radius = r1 - r0;
    match ty {
        ConicalType::Radial => {
            s.stages.push(Stage::XyToRadius);
            let scale = r0.max(r1) / d_radius;
            let bias = -r0 / d_radius;
            let m = Matrix::concat(&Matrix::translate(bias, 0.0), &Matrix::scale(scale, 1.0));
            append_matrix(&mut s.stages, &m);
        }
        ConicalType::Strip => {
            let scaled_r0 = r0 / length(c1.sub(c0));
            s.stages.push(Stage::XyTo2ptConicalStrip {
                p0: scaled_r0 * scaled_r0,
            });
            s.stages.push(Stage::Mask2ptConicalNan);
            s.post.push(Stage::ApplyVectorMask);
        }
        ConicalType::Focal => {
            let p0 = 1.0 / focal.r1;
            let p1 = focal.focal_x;
            if focal.is_focal_on_circle() {
                s.stages.push(Stage::XyTo2ptConicalFocalOnCircle);
            } else if focal.is_well_behaved() {
                s.stages.push(Stage::XyTo2ptConicalWellBehaved { p0 });
            } else if focal.is_swapped || 1.0 - focal.focal_x < 0.0 {
                s.stages.push(Stage::XyTo2ptConicalSmaller { p0 });
            } else {
                s.stages.push(Stage::XyTo2ptConicalGreater { p0 });
            }
            if !focal.is_well_behaved() {
                s.stages.push(Stage::Mask2ptConicalDegenerates);
            }
            if 1.0 - focal.focal_x < 0.0 {
                s.stages.push(Stage::NegateX);
            }
            if !focal.is_natively_focal() {
                s.stages.push(Stage::Alter2ptConicalCompensateFocal { p1 });
            }
            if focal.is_swapped {
                s.stages.push(Stage::Alter2ptConicalUnswap);
            }
            if !focal.is_well_behaved() {
                s.post.push(Stage::ApplyVectorMask);
            }
        }
    }
    Some(s)
}

/// `SkShaders::RadialGradient` → `SkRadialGradient`: pts_to_unit translates to
/// the centre and scales by 1/r; stage `xy_to_radius`.
fn radial(center: Point, radius: f32, colors: Vec<[f32; 4]>, pos: Vec<f32>) -> Option<Shader> {
    if radius <= DEGENERATE_THRESHOLD {
        let mut s = base(colors.clone(), pos, Matrix::IDENTITY);
        s.constant = Some(*colors.last().unwrap());
        return Some(s);
    }
    let inv = 1.0 / radius;
    let mut m = Matrix::translate(-center.x, -center.y);
    m.post_scale(inv, inv);
    let mut s = base(colors, pos, m);
    s.is_opaque = s.colors_are_opaque;
    s.stages.push(Stage::XyToRadius);
    Some(s)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConicalType {
    Radial,
    Strip,
    Focal,
}

#[derive(Default, Clone, Copy)]
struct FocalData {
    r1: f32,
    focal_x: f32,
    is_swapped: bool,
}

impl FocalData {
    fn is_focal_on_circle(&self) -> bool {
        (1.0 - self.r1).abs() <= SCALAR_NEARLY_ZERO
    }
    fn is_well_behaved(&self) -> bool {
        !self.is_focal_on_circle() && self.r1 > 1.0
    }
    fn is_natively_focal(&self) -> bool {
        self.focal_x.abs() <= SCALAR_NEARLY_ZERO
    }
    fn set(&mut self, mut r0: f32, mut r1: f32, matrix: &mut Matrix) -> bool {
        self.is_swapped = false;
        self.focal_x = r0 / (r0 - r1);
        if (self.focal_x - 1.0).abs() <= SCALAR_NEARLY_ZERO {
            matrix.post_translate(-1.0, 0.0);
            matrix.post_scale(-1.0, 1.0);
            std::mem::swap(&mut r0, &mut r1);
            self.focal_x = 0.0;
            self.is_swapped = true;
        }
        let from = [Point::new(self.focal_x, 0.0), Point::new(1.0, 0.0)];
        let to = [Point::new(0.0, 0.0), Point::new(1.0, 0.0)];
        let Some(focal_matrix) = Matrix::poly_to_poly2(from, to) else {
            return false;
        };
        matrix.post_concat(&focal_matrix);
        self.r1 = r1 / (1.0 - self.focal_x).abs();
        if self.is_focal_on_circle() {
            matrix.post_scale(0.5, 0.5);
        } else {
            matrix.post_scale(
                self.r1 / (self.r1 * self.r1 - 1.0),
                1.0 / (self.r1 * self.r1 - 1.0).abs().sqrt(),
            );
        }
        matrix.post_scale((1.0 - self.focal_x).abs(), (1.0 - self.focal_x).abs());
        true
    }
}

/// `SkRasterPipeline::appendMatrix`.
pub fn append_matrix(stages: &mut Vec<Stage>, m: &Matrix) {
    if m.is_identity() {
        return;
    }
    if m.is_translate_only() {
        stages.push(Stage::MatrixTranslate([m.tx, m.ty]));
    } else if m.is_scale_translate() {
        stages.push(Stage::MatrixScaleTranslate([m.sx, m.sy, m.tx, m.ty]));
    } else {
        stages.push(Stage::Matrix2x3(m.get9()));
    }
}

/// `SkGradientBaseShader::appendStages` plus the paint colour pipeline: colour
/// stages for the blitter (no clamp_01 or coverage). `ctm` is the canvas matrix,
/// `paint_alpha` globalAlpha, `color_filter` the shadow colour (srcin), `dither`.
pub fn color_stages(
    shader: &Shader,
    ctm: &Matrix,
    paint_alpha: f32,
    color_filter: Option<[f32; 4]>,
    dither: bool,
) -> Option<Vec<Stage>> {
    let mut p: Vec<Stage> = Vec::new();
    if let Some(c) = shader.constant {
        // Constant colour: appendConstantColor premul.
        let pm = [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]];
        p.push(Stage::UniformColor(pm));
    } else {
        // MatrixRec::apply: total = ptsToUnit · CTM⁻¹, seed_shader, matrix.
        let inv = ctm.invert()?;
        let total = Matrix::concat(&shader.pts_to_unit, &inv);
        p.push(Stage::SeedShader);
        append_matrix(&mut p, &total);
        p.extend(shader.stages.iter().cloned());
        // Tile clamp: clamp_x_1 only for evenly spaced stops.
        if shader.positions.is_none() {
            p.push(Stage::ClampX1);
        }
        // SkColor4fXformer: sRGB → sRGB, no premul for opaque colours;
        // otherwise there is no premul interpolation (fInPremul = kNo for canvas),
        // so colours stay straight and a later stage premultiplies.
        let colors = &shader.colors;
        if colors.len() == 2 && shader.positions.is_none() {
            let (l, r) = (colors[0], colors[1]);
            let factor = [r[0] - l[0], r[1] - l[1], r[2] - l[2], r[3] - l[3]];
            p.push(Stage::EvenlySpaced2StopGradient { factor, bias: l });
        } else {
            let ts: Vec<f32> = match &shader.positions {
                Some(pos) => pos.clone(),
                None => (0..colors.len())
                    .map(|i| i as f32 / (colors.len() - 1) as f32)
                    .collect(),
            };
            // init_stop_pos / init_stop_evenly: factor = (c_r - c_l)/gap, bias = c_l - factor·t_l;
            // index 0: colour before the first stop (factor 0).
            let mut factors = vec![[0.0f32; 4]; colors.len()];
            let mut biases = vec![[0.0f32; 4]; colors.len()];
            factors[0] = [0.0; 4];
            biases[0] = colors[0];
            for i in 1..colors.len() {
                let (t_l, t_r) = (ts[i - 1], ts[i]);
                let (c_l, c_r) = (colors[i - 1], colors[i]);
                let gap = t_r - t_l;
                let mut f = [0.0f32; 4];
                let mut b = [0.0f32; 4];
                for k in 0..4 {
                    if gap == 0.0 {
                        f[k] = 0.0;
                        b[k] = c_l[k];
                    } else {
                        f[k] = (c_r[k] - c_l[k]) * (1.0 / gap);
                        b[k] = c_l[k] - f[k] * t_l;
                    }
                }
                factors[i] = f;
                biases[i] = b;
            }
            // Last entry: colour after the last stop (factor 0).
            let last = *colors.last().unwrap();
            factors.push([0.0; 4]);
            biases.push(last);
            let mut ts2 = ts.clone();
            ts2.push(1.0);
            p.push(Stage::Gradient {
                ts: ts2,
                factors,
                biases,
            });
        }
        if !shader.colors_are_opaque {
            p.push(Stage::Premul);
        }
        p.extend(shader.post.iter().cloned());
    }
    if paint_alpha != 1.0 {
        p.push(Stage::Scale1Float(paint_alpha));
    }
    if let Some(c) = color_filter {
        // SkBlendModeColorFilter (srcin): move_src_dst, premul colour, srcin.
        p.push(Stage::MoveSrcDst);
        p.push(Stage::UniformColor([
            c[0] * c[3],
            c[1] * c[3],
            c[2] * c[3],
            c[3],
        ]));
        p.push(Stage::Blend(BlendMode::SrcIn));
    }
    if dither && shader.constant.is_none() {
        p.push(Stage::Dither(1.0 / 255.0));
    }
    Some(p)
}
