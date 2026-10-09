//! Analytic edges (`SkAnalyticEdge.cpp`) and building them from a path
//! (`SkEdgeBuilder.cpp`, `SkEdgeClipper.cpp`, `SkLineClipper.cpp`).
//! 16.16 fixed point and operation order match Skia: edge coverage
//! fractions depend on them.

use super::fixed::*;
use super::geometry::{
    chop_cubic_at, chop_cubic_at_x_extrema, chop_cubic_at_y_extrema, chop_quad_at,
    chop_quad_at_x_extrema, chop_quad_at_y_extrema, find_unit_quad_roots, Conic, Point, Rect,
    SCALAR_NEARLY_ZERO,
};
use super::path::{Path, Verb, SEG_LINE};

pub const DEFAULT_ACCURACY: i32 = 2;
const MAX_COEFF_SHIFT: i32 = 6;
const INVERSE_TABLE_SIZE: i32 = 1024;

#[rustfmt::skip]
static INVERSE_TABLE: [i32; 1025] = [
    -4096, -4100, -4104, -4108, -4112, -4116, -4120, -4124, -4128, -4132, -4136,
    -4140, -4144, -4148, -4152, -4156, -4161, -4165, -4169, -4173, -4177, -4181,
    -4185, -4190, -4194, -4198, -4202, -4206, -4211, -4215, -4219, -4223, -4228,
    -4232, -4236, -4240, -4245, -4249, -4253, -4258, -4262, -4266, -4271, -4275,
    -4279, -4284, -4288, -4293, -4297, -4301, -4306, -4310, -4315, -4319, -4324,
    -4328, -4332, -4337, -4341, -4346, -4350, -4355, -4359, -4364, -4369, -4373,
    -4378, -4382, -4387, -4391, -4396, -4401, -4405, -4410, -4415, -4419, -4424,
    -4429, -4433, -4438, -4443, -4447, -4452, -4457, -4462, -4466, -4471, -4476,
    -4481, -4485, -4490, -4495, -4500, -4505, -4510, -4514, -4519, -4524, -4529,
    -4534, -4539, -4544, -4549, -4554, -4559, -4563, -4568, -4573, -4578, -4583,
    -4588, -4593, -4599, -4604, -4609, -4614, -4619, -4624, -4629, -4634, -4639,
    -4644, -4650, -4655, -4660, -4665, -4670, -4675, -4681, -4686, -4691, -4696,
    -4702, -4707, -4712, -4718, -4723, -4728, -4733, -4739, -4744, -4750, -4755,
    -4760, -4766, -4771, -4777, -4782, -4788, -4793, -4798, -4804, -4809, -4815,
    -4821, -4826, -4832, -4837, -4843, -4848, -4854, -4860, -4865, -4871, -4877,
    -4882, -4888, -4894, -4899, -4905, -4911, -4917, -4922, -4928, -4934, -4940,
    -4946, -4951, -4957, -4963, -4969, -4975, -4981, -4987, -4993, -4999, -5005,
    -5011, -5017, -5023, -5029, -5035, -5041, -5047, -5053, -5059, -5065, -5071,
    -5077, -5084, -5090, -5096, -5102, -5108, -5115, -5121, -5127, -5133, -5140,
    -5146, -5152, -5159, -5165, -5171, -5178, -5184, -5190, -5197, -5203, -5210,
    -5216, -5223, -5229, -5236, -5242, -5249, -5256, -5262, -5269, -5275, -5282,
    -5289, -5295, -5302, -5309, -5315, -5322, -5329, -5336, -5343, -5349, -5356,
    -5363, -5370, -5377, -5384, -5391, -5398, -5405, -5412, -5418, -5426, -5433,
    -5440, -5447, -5454, -5461, -5468, -5475, -5482, -5489, -5497, -5504, -5511,
    -5518, -5526, -5533, -5540, -5548, -5555, -5562, -5570, -5577, -5584, -5592,
    -5599, -5607, -5614, -5622, -5629, -5637, -5645, -5652, -5660, -5667, -5675,
    -5683, -5691, -5698, -5706, -5714, -5722, -5729, -5737, -5745, -5753, -5761,
    -5769, -5777, -5785, -5793, -5801, -5809, -5817, -5825, -5833, -5841, -5849,
    -5857, -5866, -5874, -5882, -5890, -5899, -5907, -5915, -5924, -5932, -5940,
    -5949, -5957, -5966, -5974, -5983, -5991, -6000, -6009, -6017, -6026, -6034,
    -6043, -6052, -6061, -6069, -6078, -6087, -6096, -6105, -6114, -6123, -6132,
    -6141, -6150, -6159, -6168, -6177, -6186, -6195, -6204, -6213, -6223, -6232,
    -6241, -6250, -6260, -6269, -6278, -6288, -6297, -6307, -6316, -6326, -6335,
    -6345, -6355, -6364, -6374, -6384, -6393, -6403, -6413, -6423, -6432, -6442,
    -6452, -6462, -6472, -6482, -6492, -6502, -6512, -6523, -6533, -6543, -6553,
    -6563, -6574, -6584, -6594, -6605, -6615, -6626, -6636, -6647, -6657, -6668,
    -6678, -6689, -6700, -6710, -6721, -6732, -6743, -6754, -6765, -6775, -6786,
    -6797, -6808, -6820, -6831, -6842, -6853, -6864, -6875, -6887, -6898, -6909,
    -6921, -6932, -6944, -6955, -6967, -6978, -6990, -7002, -7013, -7025, -7037,
    -7049, -7061, -7073, -7084, -7096, -7108, -7121, -7133, -7145, -7157, -7169,
    -7182, -7194, -7206, -7219, -7231, -7244, -7256, -7269, -7281, -7294, -7307,
    -7319, -7332, -7345, -7358, -7371, -7384, -7397, -7410, -7423, -7436, -7449,
    -7463, -7476, -7489, -7503, -7516, -7530, -7543, -7557, -7570, -7584, -7598,
    -7612, -7626, -7639, -7653, -7667, -7681, -7695, -7710, -7724, -7738, -7752,
    -7767, -7781, -7796, -7810, -7825, -7839, -7854, -7869, -7884, -7898, -7913,
    -7928, -7943, -7958, -7973, -7989, -8004, -8019, -8035, -8050, -8065, -8081,
    -8097, -8112, -8128, -8144, -8160, -8176, -8192, -8208, -8224, -8240, -8256,
    -8272, -8289, -8305, -8322, -8338, -8355, -8371, -8388, -8405, -8422, -8439,
    -8456, -8473, -8490, -8507, -8525, -8542, -8559, -8577, -8594, -8612, -8630,
    -8648, -8665, -8683, -8701, -8719, -8738, -8756, -8774, -8793, -8811, -8830,
    -8848, -8867, -8886, -8905, -8924, -8943, -8962, -8981, -9000, -9020, -9039,
    -9058, -9078, -9098, -9118, -9137, -9157, -9177, -9198, -9218, -9238, -9258,
    -9279, -9300, -9320, -9341, -9362, -9383, -9404, -9425, -9446, -9467, -9489,
    -9510, -9532, -9554, -9576, -9597, -9619, -9642, -9664, -9686, -9709, -9731,
    -9754, -9776, -9799, -9822, -9845, -9868, -9892, -9915, -9939, -9962, -9986,
    -10010, -10034, -10058, -10082, -10106, -10131, -10155, -10180, -10205, -10230,
    -10255, -10280, -10305, -10330, -10356, -10381, -10407, -10433, -10459, -10485,
    -10512, -10538, -10564, -10591, -10618, -10645, -10672, -10699, -10727, -10754,
    -10782, -10810, -10837, -10866, -10894, -10922, -10951, -10979, -11008, -11037,
    -11066, -11096, -11125, -11155, -11184, -11214, -11244, -11275, -11305, -11335,
    -11366, -11397, -11428, -11459, -11491, -11522, -11554, -11586, -11618, -11650,
    -11683, -11715, -11748, -11781, -11814, -11848, -11881, -11915, -11949, -11983,
    -12018, -12052, -12087, -12122, -12157, -12192, -12228, -12264, -12300, -12336,
    -12372, -12409, -12446, -12483, -12520, -12557, -12595, -12633, -12671, -12710,
    -12748, -12787, -12826, -12865, -12905, -12945, -12985, -13025, -13066, -13107,
    -13148, -13189, -13231, -13273, -13315, -13357, -13400, -13443, -13486, -13530,
    -13573, -13617, -13662, -13706, -13751, -13797, -13842, -13888, -13934, -13981,
    -14027, -14074, -14122, -14169, -14217, -14266, -14315, -14364, -14413, -14463,
    -14513, -14563, -14614, -14665, -14716, -14768, -14820, -14873, -14926, -14979,
    -15033, -15087, -15141, -15196, -15252, -15307, -15363, -15420, -15477, -15534,
    -15592, -15650, -15709, -15768, -15827, -15887, -15947, -16008, -16070, -16131,
    -16194, -16256, -16320, -16384, -16448, -16513, -16578, -16644, -16710, -16777,
    -16844, -16912, -16980, -17050, -17119, -17189, -17260, -17331, -17403, -17476,
    -17549, -17623, -17697, -17772, -17848, -17924, -18001, -18078, -18157, -18236,
    -18315, -18396, -18477, -18558, -18641, -18724, -18808, -18893, -18978, -19065,
    -19152, -19239, -19328, -19418, -19508, -19599, -19691, -19784, -19878, -19972,
    -20068, -20164, -20262, -20360, -20460, -20560, -20661, -20763, -20867, -20971,
    -21076, -21183, -21290, -21399, -21509, -21620, -21732, -21845, -21959, -22075,
    -22192, -22310, -22429, -22550, -22671, -22795, -22919, -23045, -23172, -23301,
    -23431, -23563, -23696, -23831, -23967, -24105, -24244, -24385, -24528, -24672,
    -24818, -24966, -25115, -25266, -25420, -25575, -25731, -25890, -26051, -26214,
    -26379, -26546, -26715, -26886, -27060, -27235, -27413, -27594, -27776, -27962,
    -28149, -28339, -28532, -28728, -28926, -29127, -29330, -29537, -29746, -29959,
    -30174, -30393, -30615, -30840, -31068, -31300, -31536, -31775, -32017, -32263,
    -32513, -32768, -33026, -33288, -33554, -33825, -34100, -34379, -34663, -34952,
    -35246, -35544, -35848, -36157, -36472, -36792, -37117, -37449, -37786, -38130,
    -38479, -38836, -39199, -39568, -39945, -40329, -40721, -41120, -41527, -41943,
    -42366, -42799, -43240, -43690, -44150, -44620, -45100, -45590, -46091, -46603,
    -47127, -47662, -48210, -48770, -49344, -49932, -50533, -51150, -51781, -52428,
    -53092, -53773, -54471, -55188, -55924, -56679, -57456, -58254, -59074, -59918,
    -60787, -61680, -62601, -63550, -64527, -65536, -66576, -67650, -68759, -69905,
    -71089, -72315, -73584, -74898, -76260, -77672, -79137, -80659, -82241, -83886,
    -85598, -87381, -89240, -91180, -93206, -95325, -97541, -99864, -102300,
    -104857, -107546, -110376, -113359, -116508, -119837, -123361, -127100, -131072,
    -135300, -139810, -144631, -149796, -155344, -161319, -167772, -174762, -182361,
    -190650, -199728, -209715, -220752, -233016, -246723, -262144, -279620, -299593,
    -322638, -349525, -381300, -419430, -466033, -524288, -599186, -699050, -838860,
    -1048576, -1398101, -2097152, -4194304, 0,
];

#[inline]
fn quick_inverse(x: FDot6) -> Fixed {
    const LAST: usize = 1024;
    if x > 0 {
        -INVERSE_TABLE[LAST - x as usize]
    } else {
        INVERSE_TABLE[(LAST as i32 + x) as usize]
    }
}

#[inline]
fn quick_div(a: FDot6, b: FDot6) -> Fixed {
    const MIN_BITS: i32 = 3;
    const MAX_ABS_A: i32 = 1 << (31 - (22 - MIN_BITS));
    let abs_a = abs32(a);
    let abs_b = abs32(b);
    if abs_b >= (1 << MIN_BITS) && abs_b < INVERSE_TABLE_SIZE && abs_a < MAX_ABS_A {
        return (a.wrapping_mul(quick_inverse(b))) >> 6;
    }
    fdot6_div(a, b)
}

#[inline]
pub fn snap_y(y: Fixed) -> Fixed {
    // ((unsigned)y + (SK_Fixed1 >> (accuracy + 1))) >> (16 - accuracy) << (16 - accuracy)
    let acc = DEFAULT_ACCURACY;
    (((y as u32).wrapping_add((FIXED_1 >> (acc + 1)) as u32)) >> (16 - acc) << (16 - acc)) as i32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeType {
    Line,
    Quad,
    Cubic,
}

/// `SkAnalyticEdge` plus the quad and cubic fields (one struct, so it
/// fits in one vector).
#[derive(Clone, Debug)]
pub struct Edge {
    pub next: usize,
    pub prev: usize,
    pub x: Fixed,
    pub dx: Fixed,
    pub upper_x: Fixed,
    pub y: Fixed,
    pub upper_y: Fixed,
    pub lower_y: Fixed,
    pub dy: Fixed,
    pub edge_type: EdgeType,
    pub curve_count: i8,
    pub curve_shift: u8,
    pub winding: i8,
    // quad
    pub qx: Fixed,
    pub qy: Fixed,
    pub qdx: Fixed,
    pub qdy: Fixed,
    pub qddx: Fixed,
    pub qddy: Fixed,
    pub qlast_x: Fixed,
    pub qlast_y: Fixed,
    pub snapped_x: Fixed,
    pub snapped_y: Fixed,
    // cubic
    pub cx: Fixed,
    pub cy: Fixed,
    pub cdx: Fixed,
    pub cdy: Fixed,
    pub cddx: Fixed,
    pub cddy: Fixed,
    pub cdddx: Fixed,
    pub cdddy: Fixed,
    pub clast_x: Fixed,
    pub clast_y: Fixed,
    pub cubic_dshift: u8,
}

pub const NIL: usize = usize::MAX;

impl Default for Edge {
    fn default() -> Self {
        Edge {
            next: NIL,
            prev: NIL,
            x: 0,
            dx: 0,
            upper_x: 0,
            y: 0,
            upper_y: 0,
            lower_y: 0,
            dy: 0,
            edge_type: EdgeType::Line,
            curve_count: 0,
            curve_shift: 0,
            winding: 1,
            qx: 0,
            qy: 0,
            qdx: 0,
            qdy: 0,
            qddx: 0,
            qddy: 0,
            qlast_x: 0,
            qlast_y: 0,
            snapped_x: 0,
            snapped_y: 0,
            cx: 0,
            cy: 0,
            cdx: 0,
            cdy: 0,
            cddx: 0,
            cddy: 0,
            cdddx: 0,
            cdddy: 0,
            clast_x: 0,
            clast_y: 0,
            cubic_dshift: 0,
        }
    }
}

impl Edge {
    #[inline]
    pub fn go_y(&mut self, y: Fixed) {
        if y == self.y.wrapping_add(FIXED_1) {
            self.x = self.x.wrapping_add(self.dx);
            self.y = y;
        } else if y != self.y {
            self.x = self
                .upper_x
                .wrapping_add(fixed_mul(self.dx, y.wrapping_sub(self.upper_y)));
            self.y = y;
        }
    }
    #[inline]
    pub fn go_y_shift(&mut self, y: Fixed, y_shift: i32) {
        self.y = y;
        self.x = self.x.wrapping_add(self.dx >> y_shift);
    }

    /// `SkAnalyticEdge::setLine`.
    pub fn set_line(p0: Point, p1: Point) -> Option<Edge> {
        let acc = DEFAULT_ACCURACY;
        let mult = (1 << DEFAULT_ACCURACY) as f32;
        let mut x0 = fdot6_to_fixed(scalar_to_fdot6(p0.x * mult)) >> acc;
        let mut y0 = snap_y(fdot6_to_fixed(scalar_to_fdot6(p0.y * mult)) >> acc);
        let mut x1 = fdot6_to_fixed(scalar_to_fdot6(p1.x * mult)) >> acc;
        let mut y1 = snap_y(fdot6_to_fixed(scalar_to_fdot6(p1.y * mult)) >> acc);
        let mut winding: i8 = 1;
        if y0 > y1 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
            winding = -1;
        }
        let dy = fixed_to_fdot6(y1.wrapping_sub(y0));
        if dy == 0 {
            return None;
        }
        let dx = fixed_to_fdot6(x1.wrapping_sub(x0));
        let slope = quick_div(dx, dy);
        let abs_slope = abs32(slope);
        let mut e = Edge::default();
        e.x = x0;
        e.dx = slope;
        e.upper_x = x0;
        e.y = y0;
        e.upper_y = y0;
        e.lower_y = y1;
        e.dy = if dx == 0 || slope == 0 {
            MAX_S32
        } else if abs_slope < INVERSE_TABLE_SIZE {
            quick_inverse(abs_slope)
        } else {
            abs32(quick_div(dy, dx))
        };
        e.edge_type = EdgeType::Line;
        e.curve_count = 0;
        e.winding = winding;
        e.curve_shift = 0;
        Some(e)
    }

    /// `SkAnalyticEdge::updateLine`.
    fn update_line(
        &mut self,
        mut x0: Fixed,
        mut y0: Fixed,
        mut x1: Fixed,
        mut y1: Fixed,
        slope: Fixed,
    ) -> bool {
        if y0 > y1 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
            self.winding = -self.winding;
        }
        let dx = fixed_to_fdot6(x1.wrapping_sub(x0));
        let dy = fixed_to_fdot6(y1.wrapping_sub(y0));
        if dy == 0 {
            return false;
        }
        let abs_slope = abs32(fixed_to_fdot6(slope));
        self.x = x0;
        self.dx = slope;
        self.upper_x = x0;
        self.y = y0;
        self.upper_y = y0;
        self.lower_y = y1;
        self.dy = if dx == 0 || slope == 0 {
            MAX_S32
        } else if abs_slope < INVERSE_TABLE_SIZE {
            quick_inverse(abs_slope)
        } else {
            abs32(quick_div(dy, dx))
        };
        true
    }

    /// `SkAnalyticEdge::update(last_y)`: true while the edge continues.
    pub fn update(&mut self) -> bool {
        if self.curve_count < 0 {
            self.update_cubic()
        } else if self.curve_count > 0 {
            self.update_quadratic()
        } else {
            false
        }
    }

    // ── quad ──

    fn set_quadratic_without_update(pts: &[Point; 3], shift_in: i32) -> Option<Edge> {
        let scale = (1i32 << (shift_in + 6)) as f32;
        let mut x0 = (pts[0].x * scale) as i32;
        let mut y0 = (pts[0].y * scale) as i32;
        let x1 = (pts[1].x * scale) as i32;
        let y1 = (pts[1].y * scale) as i32;
        let mut x2 = (pts[2].x * scale) as i32;
        let mut y2 = (pts[2].y * scale) as i32;
        let mut winding: i8 = 1;
        if y0 > y2 {
            std::mem::swap(&mut x0, &mut x2);
            std::mem::swap(&mut y0, &mut y2);
            winding = -1;
        }
        let top = fdot6_round(y0);
        let bot = fdot6_round(y2);
        if top == bot {
            return None;
        }
        let mut shift;
        {
            let dx = (left_shift(x1, 1) - x0 - x2) >> 2;
            let dy = (left_shift(y1, 1) - y0 - y2) >> 2;
            shift = diff_to_shift(dx, dy, shift_in);
        }
        if shift == 0 {
            shift = 1;
        } else if shift > MAX_COEFF_SHIFT {
            shift = MAX_COEFF_SHIFT;
        }
        let mut e = Edge::default();
        e.winding = winding;
        e.edge_type = EdgeType::Quad;
        e.curve_count = (1i32 << shift) as i8;
        e.curve_shift = (shift - 1) as u8;

        let a = fdot6_to_fixed_div2(x0 - x1 - x1 + x2);
        let b = fdot6_to_fixed(x1 - x0);
        e.qx = fdot6_to_fixed(x0);
        e.qdx = b.wrapping_add(a >> shift);
        e.qddx = a >> (shift - 1);

        let a = fdot6_to_fixed_div2(y0 - y1 - y1 + y2);
        let b = fdot6_to_fixed(y1 - y0);
        e.qy = fdot6_to_fixed(y0);
        e.qdy = b.wrapping_add(a >> shift);
        e.qddy = a >> (shift - 1);

        e.qlast_x = fdot6_to_fixed(x2);
        e.qlast_y = fdot6_to_fixed(y2);
        Some(e)
    }

    pub fn set_quadratic(pts: &[Point; 3]) -> Option<Edge> {
        let mut e = Edge::set_quadratic_without_update(pts, DEFAULT_ACCURACY)?;
        let acc = DEFAULT_ACCURACY;
        e.qx >>= acc;
        e.qy >>= acc;
        e.qdx >>= acc;
        e.qdy >>= acc;
        e.qddx >>= acc;
        e.qddy >>= acc;
        e.qlast_x >>= acc;
        e.qlast_y >>= acc;
        e.qy = snap_y(e.qy);
        e.qlast_y = snap_y(e.qlast_y);
        e.edge_type = EdgeType::Quad;
        e.snapped_x = e.qx;
        e.snapped_y = e.qy;
        if e.update_quadratic() {
            Some(e)
        } else {
            None
        }
    }

    pub fn update_quadratic(&mut self) -> bool {
        let mut success = false;
        let mut count = self.curve_count as i32;
        let mut oldx = self.qx;
        let mut oldy = self.qy;
        let mut dx = self.qdx;
        let mut dy = self.qdy;
        let shift = self.curve_shift as i32;
        let (mut newx, mut newy, mut new_snapped_x, mut new_snapped_y);
        loop {
            let slope;
            count -= 1;
            if count > 0 {
                newx = oldx.wrapping_add(dx >> shift);
                newy = oldy.wrapping_add(dy >> shift);
                if abs32(dy >> shift) >= FIXED_1 * 2
                    && left_shift64(abs32(dy) as i64, 6) > abs32(dx) as i64
                {
                    let diff_y = fixed_to_fdot6(newy.wrapping_sub(self.snapped_y));
                    slope = if diff_y != 0 {
                        quick_div(fixed_to_fdot6(newx.wrapping_sub(self.snapped_x)), diff_y)
                    } else {
                        MAX_S32
                    };
                    new_snapped_y = self.qlast_y.min(fixed_round_to_fixed(newy));
                    new_snapped_x =
                        newx.wrapping_sub(fixed_mul(slope, newy.wrapping_sub(new_snapped_y)));
                } else {
                    new_snapped_y = self.qlast_y.min(snap_y(newy));
                    new_snapped_x = newx;
                    let diff_y = fixed_to_fdot6(new_snapped_y.wrapping_sub(self.snapped_y));
                    slope = if diff_y != 0 {
                        quick_div(fixed_to_fdot6(newx.wrapping_sub(self.snapped_x)), diff_y)
                    } else {
                        MAX_S32
                    };
                }
                dx = dx.wrapping_add(self.qddx);
                dy = dy.wrapping_add(self.qddy);
            } else {
                newx = self.qlast_x;
                newy = self.qlast_y;
                new_snapped_y = newy;
                new_snapped_x = newx;
                let diff_y = fixed_to_fdot6(newy.wrapping_sub(self.snapped_y));
                slope = if diff_y != 0 {
                    quick_div(fixed_to_fdot6(newx.wrapping_sub(self.snapped_x)), diff_y)
                } else {
                    MAX_S32
                };
            }
            if slope < MAX_S32 {
                let (sx, sy) = (self.snapped_x, self.snapped_y);
                success = self.update_line(sx, sy, new_snapped_x, new_snapped_y, slope);
            }
            oldx = newx;
            oldy = newy;
            if !(count > 0 && !success) {
                break;
            }
        }
        self.qx = newx;
        self.qy = newy;
        self.qdx = dx;
        self.qdy = dy;
        self.snapped_x = new_snapped_x;
        self.snapped_y = new_snapped_y;
        self.curve_count = count as i8;
        success
    }

    #[inline]
    pub fn keep_continuous_quad(&mut self) {
        self.snapped_x = self.x;
        self.snapped_y = self.y;
    }

    // ── cubic ──

    fn set_cubic_without_update(pts: &[Point; 4], shift_in: i32) -> Option<Edge> {
        let scale = (1i32 << (shift_in + 6)) as f32;
        let mut x0 = (pts[0].x * scale) as i32;
        let mut y0 = (pts[0].y * scale) as i32;
        let mut x1 = (pts[1].x * scale) as i32;
        let mut y1 = (pts[1].y * scale) as i32;
        let mut x2 = (pts[2].x * scale) as i32;
        let mut y2 = (pts[2].y * scale) as i32;
        let mut x3 = (pts[3].x * scale) as i32;
        let mut y3 = (pts[3].y * scale) as i32;
        let mut winding: i8 = 1;
        if y0 > y3 {
            std::mem::swap(&mut x0, &mut x3);
            std::mem::swap(&mut x1, &mut x2);
            std::mem::swap(&mut y0, &mut y3);
            std::mem::swap(&mut y1, &mut y2);
            winding = -1;
        }
        let top = fdot6_round(y0);
        let bot = fdot6_round(y3);
        if top == bot {
            return None;
        }
        let mut shift;
        {
            let dx = cubic_delta_from_line(x0, x1, x2, x3);
            let dy = cubic_delta_from_line(y0, y1, y2, y3);
            shift = diff_to_shift(dx, dy, 2) + 1;
        }
        if shift > MAX_COEFF_SHIFT {
            shift = MAX_COEFF_SHIFT;
        }
        let mut up_shift = 6;
        let mut down_shift = shift + up_shift - 10;
        if down_shift < 0 {
            down_shift = 0;
            up_shift = 10 - shift;
        }
        let mut e = Edge::default();
        e.winding = winding;
        e.edge_type = EdgeType::Cubic;
        e.curve_count = left_shift(-1, shift) as i8;
        e.curve_shift = shift as u8;
        e.cubic_dshift = down_shift as u8;

        let b = left_shift(3 * (x1 - x0), up_shift);
        let c = left_shift(3 * (x0 - x1 - x1 + x2), up_shift);
        let d = left_shift(x3 + 3 * (x1 - x2) - x0, up_shift);
        e.cx = fdot6_to_fixed(x0);
        e.cdx = b.wrapping_add(c >> shift).wrapping_add(d >> (2 * shift));
        e.cddx = (2i32.wrapping_mul(c)).wrapping_add((3i32.wrapping_mul(d)) >> (shift - 1));
        e.cdddx = (3i32.wrapping_mul(d)) >> (shift - 1);

        let b = left_shift(3 * (y1 - y0), up_shift);
        let c = left_shift(3 * (y0 - y1 - y1 + y2), up_shift);
        let d = left_shift(y3 + 3 * (y1 - y2) - y0, up_shift);
        e.cy = fdot6_to_fixed(y0);
        e.cdy = b.wrapping_add(c >> shift).wrapping_add(d >> (2 * shift));
        e.cddy = (2i32.wrapping_mul(c)).wrapping_add((3i32.wrapping_mul(d)) >> (shift - 1));
        e.cdddy = (3i32.wrapping_mul(d)) >> (shift - 1);

        e.clast_x = fdot6_to_fixed(x3);
        e.clast_y = fdot6_to_fixed(y3);
        Some(e)
    }

    pub fn set_cubic(pts: &[Point; 4]) -> Option<Edge> {
        let mut e = Edge::set_cubic_without_update(pts, DEFAULT_ACCURACY)?;
        let acc = DEFAULT_ACCURACY;
        e.cx >>= acc;
        e.cy >>= acc;
        e.cdx >>= acc;
        e.cdy >>= acc;
        e.cddx >>= acc;
        e.cddy >>= acc;
        e.cdddx >>= acc;
        e.cdddy >>= acc;
        e.clast_x >>= acc;
        e.clast_y >>= acc;
        e.cy = snap_y(e.cy);
        e.snapped_y = e.cy;
        e.clast_y = snap_y(e.clast_y);
        e.edge_type = EdgeType::Cubic;
        if e.update_cubic() {
            Some(e)
        } else {
            None
        }
    }

    pub fn update_cubic(&mut self) -> bool {
        let mut success;
        let mut count = self.curve_count as i32;
        let mut oldx = self.cx;
        let mut oldy = self.cy;
        let ddshift = self.curve_shift as i32;
        let dshift = self.cubic_dshift as i32;
        let (mut newx, mut newy);
        loop {
            count += 1;
            if count < 0 {
                newx = oldx.wrapping_add(self.cdx >> dshift);
                self.cdx = self.cdx.wrapping_add(self.cddx >> ddshift);
                self.cddx = self.cddx.wrapping_add(self.cdddx);
                newy = oldy.wrapping_add(self.cdy >> dshift);
                self.cdy = self.cdy.wrapping_add(self.cddy >> ddshift);
                self.cddy = self.cddy.wrapping_add(self.cdddy);
            } else {
                newx = self.clast_x;
                newy = self.clast_y;
            }
            if newy < oldy {
                newy = oldy;
            }
            let mut new_snapped_y = snap_y(newy);
            if self.clast_y < new_snapped_y {
                new_snapped_y = self.clast_y;
                count = 0;
            }
            let dy6 = fixed_to_fdot6(new_snapped_y.wrapping_sub(self.snapped_y));
            let slope = if dy6 == 0 {
                MAX_S32
            } else {
                fdot6_div(fixed_to_fdot6(newx.wrapping_sub(oldx)), dy6)
            };
            let sy = self.snapped_y;
            success = self.update_line(oldx, sy, newx, new_snapped_y, slope);
            oldx = newx;
            oldy = newy;
            self.snapped_y = new_snapped_y;
            if !(count < 0 && !success) {
                break;
            }
        }
        self.cx = newx;
        self.cy = newy;
        self.curve_count = count as i8;
        success
    }

    #[inline]
    pub fn keep_continuous_cubic(&mut self) {
        self.cx = self.x;
        self.snapped_y = self.y;
    }
}

#[inline]
fn fdot6_to_fixed_div2(v: FDot6) -> Fixed {
    left_shift(v, 16 - 6 - 1)
}

#[inline]
fn cheap_distance(dx: FDot6, dy: FDot6) -> FDot6 {
    let dx = abs32(dx);
    let dy = abs32(dy);
    if dx > dy {
        dx + (dy >> 1)
    } else {
        dy + (dx >> 1)
    }
}

#[inline]
fn diff_to_shift(dx: FDot6, dy: FDot6, shift_aa: i32) -> i32 {
    let mut dist = cheap_distance(dx, dy);
    dist = (dist + (1 << (2 + shift_aa))) >> (3 + shift_aa);
    (32 - clz(dist as u32)) >> 1
}

#[inline]
fn cubic_delta_from_line(a: FDot6, b: FDot6, c: FDot6, d: FDot6) -> FDot6 {
    let one_third = (a * 8 - b * 15 + 6 * c + d) * 19 >> 9;
    let two_third = (a + 6 * b - c * 15 + d * 8) * 19 >> 9;
    abs32(one_third).max(abs32(two_third))
}

// ── Edge list building (SkAnalyticEdgeBuilder) ───────────────────────────

#[derive(PartialEq, Eq)]
enum Combine {
    No,
    Partial,
    Total,
}

pub struct EdgeBuilder {
    pub edges: Vec<Edge>,
}

impl EdgeBuilder {
    fn combine_vertical(edge: &Edge, last: &mut Edge) -> Combine {
        let approx = |a: Fixed, b: Fixed| abs32(a.wrapping_sub(b)) < 0x100;
        if last.edge_type != EdgeType::Line || last.dx != 0 || edge.x != last.x {
            return Combine::No;
        }
        if edge.winding == last.winding {
            if edge.lower_y == last.upper_y {
                last.upper_y = edge.upper_y;
                last.y = last.upper_y;
                return Combine::Partial;
            }
            if approx(edge.upper_y, last.lower_y) {
                last.lower_y = edge.lower_y;
                return Combine::Partial;
            }
            return Combine::No;
        }
        if approx(edge.upper_y, last.upper_y) {
            if approx(edge.lower_y, last.lower_y) {
                return Combine::Total;
            }
            if edge.lower_y < last.lower_y {
                last.upper_y = edge.lower_y;
                last.y = last.upper_y;
                return Combine::Partial;
            }
            last.upper_y = last.lower_y;
            last.y = last.upper_y;
            last.lower_y = edge.lower_y;
            last.winding = edge.winding;
            return Combine::Partial;
        }
        if approx(edge.lower_y, last.lower_y) {
            if edge.upper_y > last.upper_y {
                last.lower_y = edge.upper_y;
                return Combine::Partial;
            }
            last.lower_y = last.upper_y;
            last.upper_y = edge.upper_y;
            last.y = last.upper_y;
            last.winding = edge.winding;
            return Combine::Partial;
        }
        Combine::No
    }

    fn add_line(&mut self, p0: Point, p1: Point) {
        if let Some(edge) = Edge::set_line(p0, p1) {
            let is_vertical = edge.dx == 0 && edge.edge_type == EdgeType::Line;
            let combine = if is_vertical && !self.edges.is_empty() {
                let last = self.edges.last_mut().unwrap();
                EdgeBuilder::combine_vertical(&edge, last)
            } else {
                Combine::No
            };
            match combine {
                Combine::Total => {
                    self.edges.pop();
                }
                Combine::Partial => {}
                Combine::No => self.edges.push(edge),
            }
        }
    }
    fn add_quad(&mut self, pts: &[Point; 3]) {
        if let Some(e) = Edge::set_quadratic(pts) {
            self.edges.push(e);
        }
    }
    fn add_cubic(&mut self, pts: &[Point; 4]) {
        if let Some(e) = Edge::set_cubic(pts) {
            self.edges.push(e);
        }
    }
    fn handle_quad(&mut self, pts: &[Point; 3]) {
        let mut mono = [Point::default(); 5];
        let n = chop_quad_at_y_extrema(pts, &mut mono);
        for i in 0..n {
            let q = [mono[i * 2], mono[i * 2 + 1], mono[i * 2 + 2]];
            self.add_quad(&q);
        }
    }

    /// `SkEdgeBuilder::buildEdges(path, clip)`: `clip` is None when the path
    /// lies entirely inside the clip.
    pub fn build(path: &Path, clip: Option<&IRectF>) -> EdgeBuilder {
        let mut b = EdgeBuilder { edges: Vec::new() };
        let can_cull_to_the_right = !path.convexity.is_convex();
        let iter = EdgeIter::new(path);
        if path.segment_mask == SEG_LINE {
            // buildPoly
            for e in iter {
                if let EdgeSeg::Line(p0, p1) = e {
                    if let Some(c) = clip {
                        let lines = clip_line([p0, p1], &c.0, can_cull_to_the_right);
                        for i in 0..lines.1 {
                            b.add_line(lines.0[i], lines.0[i + 1]);
                        }
                    } else {
                        b.add_line(p0, p1);
                    }
                }
            }
            return b;
        }
        match clip {
            Some(c) => {
                let clip = c.0;
                let mut clipper = EdgeClipper::new(can_cull_to_the_right);
                for e in iter {
                    let consume = |clipper: &EdgeClipper, b: &mut EdgeBuilder| {
                        for (verb, pts) in clipper.iter() {
                            if pts.iter().any(|p| !p.is_finite()) {
                                b.edges.clear();
                                return false;
                            }
                            match verb {
                                Verb::Line => b.add_line(pts[0], pts[1]),
                                Verb::Quad => b.add_quad(&[pts[0], pts[1], pts[2]]),
                                Verb::Cubic => b.add_cubic(&[pts[0], pts[1], pts[2], pts[3]]),
                                _ => {}
                            }
                        }
                        true
                    };
                    match e {
                        EdgeSeg::Line(p0, p1) => {
                            if clipper.clip_line(p0, p1, &clip) && !consume(&clipper, &mut b) {
                                return b;
                            }
                        }
                        EdgeSeg::Quad(pts) => {
                            if clipper.clip_quad(&pts, &clip) && !consume(&clipper, &mut b) {
                                return b;
                            }
                        }
                        EdgeSeg::Conic(pts, w) => {
                            let (qpts, n) = Conic::new(pts[0], pts[1], pts[2], w).to_quads(0.25);
                            for i in 0..n {
                                let q = [qpts[i * 2], qpts[i * 2 + 1], qpts[i * 2 + 2]];
                                if clipper.clip_quad(&q, &clip) && !consume(&clipper, &mut b) {
                                    return b;
                                }
                            }
                        }
                        EdgeSeg::Cubic(pts) => {
                            if clipper.clip_cubic(&pts, &clip) && !consume(&clipper, &mut b) {
                                return b;
                            }
                        }
                    }
                }
            }
            None => {
                for e in iter {
                    match e {
                        EdgeSeg::Line(p0, p1) => b.add_line(p0, p1),
                        EdgeSeg::Quad(pts) => b.handle_quad(&pts),
                        EdgeSeg::Conic(pts, w) => {
                            let (qpts, n) = Conic::new(pts[0], pts[1], pts[2], w).to_quads(0.25);
                            for i in 0..n {
                                let q = [qpts[i * 2], qpts[i * 2 + 1], qpts[i * 2 + 2]];
                                b.handle_quad(&q);
                            }
                        }
                        EdgeSeg::Cubic(pts) => {
                            let mut mono = [Point::default(); 10];
                            let n = chop_cubic_at_y_extrema(&pts, &mut mono);
                            for i in 0..n {
                                let c = [
                                    mono[i * 3],
                                    mono[i * 3 + 1],
                                    mono[i * 3 + 2],
                                    mono[i * 3 + 3],
                                ];
                                b.add_cubic(&c);
                            }
                        }
                    }
                }
            }
        }
        b
    }
}

/// Clip rect in float (`recoverClip`).
pub struct IRectF(pub Rect);

// ── SkPathEdgeIter ────────────────────────────────────────────────────────

pub enum EdgeSeg {
    Line(Point, Point),
    Quad([Point; 3]),
    Conic([Point; 3], f32),
    Cubic([Point; 4]),
}

pub struct EdgeIter<'a> {
    path: &'a Path,
    vi: usize,
    pi: usize,
    ci: usize,
    move_to: Point,
    needs_close_line: bool,
}

impl<'a> EdgeIter<'a> {
    pub fn new(path: &'a Path) -> Self {
        EdgeIter {
            path,
            vi: 0,
            pi: 0,
            ci: 0,
            move_to: Point::default(),
            needs_close_line: false,
        }
    }
    fn closeline(&mut self) -> EdgeSeg {
        let last = self.path.pts[self.pi - 1];
        self.needs_close_line = false;
        EdgeSeg::Line(last, self.move_to)
    }
}

impl<'a> Iterator for EdgeIter<'a> {
    type Item = EdgeSeg;
    fn next(&mut self) -> Option<EdgeSeg> {
        loop {
            if self.vi >= self.path.verbs.len() {
                return if self.needs_close_line {
                    Some(self.closeline())
                } else {
                    None
                };
            }
            let verb = self.path.verbs[self.vi];
            self.vi += 1;
            match verb {
                Verb::Move => {
                    if self.needs_close_line {
                        let res = self.closeline();
                        self.move_to = self.path.pts[self.pi];
                        self.pi += 1;
                        return Some(res);
                    }
                    self.move_to = self.path.pts[self.pi];
                    self.pi += 1;
                }
                Verb::Close => {
                    if self.needs_close_line {
                        return Some(self.closeline());
                    }
                }
                Verb::Line => {
                    self.needs_close_line = true;
                    let p = &self.path.pts;
                    let r = EdgeSeg::Line(p[self.pi - 1], p[self.pi]);
                    self.pi += 1;
                    return Some(r);
                }
                Verb::Quad => {
                    self.needs_close_line = true;
                    let p = &self.path.pts;
                    let r = EdgeSeg::Quad([p[self.pi - 1], p[self.pi], p[self.pi + 1]]);
                    self.pi += 2;
                    return Some(r);
                }
                Verb::Conic => {
                    self.needs_close_line = true;
                    let p = &self.path.pts;
                    let w = self.path.conics[self.ci];
                    self.ci += 1;
                    let r = EdgeSeg::Conic([p[self.pi - 1], p[self.pi], p[self.pi + 1]], w);
                    self.pi += 2;
                    return Some(r);
                }
                Verb::Cubic => {
                    self.needs_close_line = true;
                    let p = &self.path.pts;
                    let r = EdgeSeg::Cubic([
                        p[self.pi - 1],
                        p[self.pi],
                        p[self.pi + 1],
                        p[self.pi + 2],
                    ]);
                    self.pi += 3;
                    return Some(r);
                }
            }
        }
    }
}

// ── SkLineClipper ─────────────────────────────────────────────────────────

fn pin_unsorted(value: f64, mut l0: f64, mut l1: f64) -> f64 {
    if l1 < l0 {
        std::mem::swap(&mut l0, &mut l1);
    }
    if value < l0 {
        l0
    } else if value > l1 {
        l1
    } else {
        value
    }
}
fn pin_unsorted_f(value: f32, mut l0: f32, mut l1: f32) -> f32 {
    if l1 < l0 {
        std::mem::swap(&mut l0, &mut l1);
    }
    if value < l0 {
        l0
    } else if value > l1 {
        l1
    } else {
        value
    }
}
fn midpoint(a: f32, b: f32) -> f32 {
    (0.5 * (a as f64 + b as f64)) as f32
}
fn sect_with_horizontal(src: &[Point; 2], y: f32) -> f32 {
    let dy = src[1].y - src[0].y;
    if dy.abs() <= SCALAR_NEARLY_ZERO {
        midpoint(src[0].x, src[1].x)
    } else {
        let (x0, y0, x1, y1) = (
            src[0].x as f64,
            src[0].y as f64,
            src[1].x as f64,
            src[1].y as f64,
        );
        let result = x0 + (y as f64 - y0) * (x1 - x0) / (y1 - y0);
        pin_unsorted(result, x0, x1) as f32
    }
}
fn sect_with_vertical(src: &[Point; 2], x: f32) -> f32 {
    let dx = src[1].x - src[0].x;
    if dx.abs() <= SCALAR_NEARLY_ZERO {
        midpoint(src[0].y, src[1].y)
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
fn sect_clamp_with_vertical(src: &[Point; 2], x: f32) -> f32 {
    let y = sect_with_vertical(src, x);
    pin_unsorted_f(y, src[0].y, src[1].y)
}

/// `SkLineClipper::ClipLine`: up to 3 segments, points `lines[0..=n]`.
pub fn clip_line(pts: [Point; 2], clip: &Rect, can_cull_to_the_right: bool) -> ([Point; 4], usize) {
    let mut lines = [Point::default(); 4];
    let (index0, index1) = if pts[0].y < pts[1].y { (0, 1) } else { (1, 0) };
    if pts[index1].y <= clip.top || pts[index0].y >= clip.bottom {
        return (lines, 0);
    }
    let mut tmp = pts;
    if pts[index0].y < clip.top {
        tmp[index0] = Point::new(sect_with_horizontal(&pts, clip.top), clip.top);
    }
    if tmp[index1].y > clip.bottom {
        tmp[index1] = Point::new(sect_with_horizontal(&pts, clip.bottom), clip.bottom);
    }
    let mut result_storage = [Point::default(); 4];
    let mut line_count = 1usize;
    let (index0, index1, mut reverse) = if pts[0].x < pts[1].x {
        (0, 1, false)
    } else {
        (1, 0, true)
    };
    let result: &[Point];
    if tmp[index1].x <= clip.left {
        tmp[0].x = clip.left;
        tmp[1].x = clip.left;
        reverse = false;
        result = &tmp;
    } else if tmp[index0].x >= clip.right {
        if can_cull_to_the_right {
            return (lines, 0);
        }
        tmp[0].x = clip.right;
        tmp[1].x = clip.right;
        reverse = false;
        result = &tmp;
    } else {
        let mut r = 0usize;
        if tmp[index0].x < clip.left {
            result_storage[r] = Point::new(clip.left, tmp[index0].y);
            r += 1;
            result_storage[r] = Point::new(clip.left, sect_clamp_with_vertical(&tmp, clip.left));
        } else {
            result_storage[r] = tmp[index0];
        }
        r += 1;
        if tmp[index1].x > clip.right {
            result_storage[r] = Point::new(clip.right, sect_clamp_with_vertical(&tmp, clip.right));
            r += 1;
            result_storage[r] = Point::new(clip.right, tmp[index1].y);
        } else {
            result_storage[r] = tmp[index1];
        }
        line_count = r;
        result = &result_storage;
    }
    if reverse {
        for i in 0..=line_count {
            lines[line_count - i] = result[i];
        }
    } else {
        lines[..=line_count].copy_from_slice(&result[..=line_count]);
    }
    (lines, line_count)
}

// ── SkEdgeClipper ─────────────────────────────────────────────────────────

pub struct EdgeClipper {
    can_cull_to_the_right: bool,
    verbs: Vec<Verb>,
    pts: Vec<Point>,
}

impl EdgeClipper {
    pub fn new(can_cull_to_the_right: bool) -> Self {
        EdgeClipper {
            can_cull_to_the_right,
            verbs: Vec::new(),
            pts: Vec::new(),
        }
    }
    fn reset(&mut self) {
        self.verbs.clear();
        self.pts.clear();
    }
    pub fn iter(&self) -> impl Iterator<Item = (Verb, &[Point])> {
        let mut pi = 0usize;
        self.verbs.iter().map(move |v| {
            let n = match v {
                Verb::Line => 2,
                Verb::Quad => 3,
                Verb::Cubic => 4,
                _ => 0,
            };
            let s = &self.pts[pi..pi + n];
            pi += n;
            (*v, s)
        })
    }
    fn append_line(&mut self, p0: Point, p1: Point) {
        self.verbs.push(Verb::Line);
        self.pts.push(p0);
        self.pts.push(p1);
    }
    fn append_vline(&mut self, x: f32, mut y0: f32, mut y1: f32, reverse: bool) {
        self.verbs.push(Verb::Line);
        if reverse {
            std::mem::swap(&mut y0, &mut y1);
        }
        self.pts.push(Point::new(x, y0));
        self.pts.push(Point::new(x, y1));
    }
    fn append_quad(&mut self, pts: &[Point; 3], reverse: bool) {
        self.verbs.push(Verb::Quad);
        if reverse {
            self.pts.push(pts[2]);
            self.pts.push(pts[1]);
            self.pts.push(pts[0]);
        } else {
            self.pts.extend_from_slice(pts);
        }
    }
    fn append_cubic(&mut self, pts: &[Point; 4], reverse: bool) {
        self.verbs.push(Verb::Cubic);
        if reverse {
            for i in 0..4 {
                self.pts.push(pts[3 - i]);
            }
        } else {
            self.pts.extend_from_slice(pts);
        }
    }

    pub fn clip_line(&mut self, p0: Point, p1: Point, clip: &Rect) -> bool {
        self.reset();
        let (lines, n) = clip_line([p0, p1], clip, self.can_cull_to_the_right);
        for i in 0..n {
            self.append_line(lines[i], lines[i + 1]);
        }
        !self.verbs.is_empty()
    }

    fn clip_mono_quad(&mut self, src: &[Point; 3], clip: &Rect) {
        let mut pts = *src;
        let mut reverse = sort_increasing_y3(&mut pts);
        if pts[2].y <= clip.top || pts[0].y >= clip.bottom {
            return;
        }
        chop_quad_in_y(&mut pts, clip);
        if pts[0].x > pts[2].x {
            pts.swap(0, 2);
            reverse = !reverse;
        }
        if pts[2].x <= clip.left {
            self.append_vline(clip.left, pts[0].y, pts[2].y, reverse);
            return;
        }
        if pts[0].x >= clip.right {
            if !self.can_cull_to_the_right {
                self.append_vline(clip.right, pts[0].y, pts[2].y, reverse);
            }
            return;
        }
        if pts[0].x < clip.left {
            if let Some(t) = chop_mono_quad_at(pts[0].x, pts[1].x, pts[2].x, clip.left) {
                let tmp = chop_quad_at(&pts, t);
                self.append_vline(clip.left, tmp[0].y, tmp[2].y, reverse);
                let mut p2 = tmp[2];
                p2.x = clip.left;
                let mut p3 = tmp[3];
                if p3.x < clip.left {
                    p3.x = clip.left;
                }
                pts[0] = p2;
                pts[1] = p3;
            } else {
                self.append_vline(clip.left, pts[0].y, pts[2].y, reverse);
                return;
            }
        }
        if pts[2].x > clip.right {
            if let Some(t) = chop_mono_quad_at(pts[0].x, pts[1].x, pts[2].x, clip.right) {
                let mut tmp = chop_quad_at(&pts, t);
                if tmp[1].x > clip.right {
                    tmp[1].x = clip.right;
                }
                tmp[2].x = clip.right;
                self.append_quad(&[tmp[0], tmp[1], tmp[2]], reverse);
                self.append_vline(clip.right, tmp[2].y, tmp[4].y, reverse);
            } else {
                pts[1].x = pts[1].x.min(clip.right);
                pts[2].x = pts[2].x.min(clip.right);
                self.append_quad(&pts, reverse);
            }
        } else {
            self.append_quad(&pts, reverse);
        }
    }

    pub fn clip_quad(&mut self, src: &[Point; 3], clip: &Rect) -> bool {
        self.reset();
        let bounds = Rect::bounds(src);
        if !(bounds.top >= clip.bottom || bounds.bottom <= clip.top) {
            let mut mono_y = [Point::default(); 5];
            let ny = chop_quad_at_y_extrema(src, &mut mono_y);
            for y in 0..ny {
                let q = [mono_y[y * 2], mono_y[y * 2 + 1], mono_y[y * 2 + 2]];
                let mut mono_x = [Point::default(); 5];
                let nx = chop_quad_at_x_extrema(&q, &mut mono_x);
                for x in 0..nx {
                    let qq = [mono_x[x * 2], mono_x[x * 2 + 1], mono_x[x * 2 + 2]];
                    self.clip_mono_quad(&qq, clip);
                }
            }
        }
        !self.verbs.is_empty()
    }

    fn clip_mono_cubic(&mut self, src: &[Point; 4], clip: &Rect) {
        let mut pts = *src;
        let mut reverse = sort_increasing_y4(&mut pts);
        if pts[3].y <= clip.top || pts[0].y >= clip.bottom {
            return;
        }
        chop_cubic_in_y(&mut pts, clip);
        if pts[0].x > pts[3].x {
            pts.swap(0, 3);
            pts.swap(1, 2);
            reverse = !reverse;
        }
        if pts[3].x <= clip.left {
            self.append_vline(clip.left, pts[0].y, pts[3].y, reverse);
            return;
        }
        if pts[0].x >= clip.right {
            if !self.can_cull_to_the_right {
                self.append_vline(clip.right, pts[0].y, pts[3].y, reverse);
            }
            return;
        }
        if pts[0].x < clip.left {
            let mut tmp = chop_mono_cubic_at_x(&pts, clip.left);
            self.append_vline(clip.left, tmp[0].y, tmp[3].y, reverse);
            tmp[3].x = clip.left;
            if tmp[4].x < clip.left {
                tmp[4].x = clip.left;
            }
            pts[0] = tmp[3];
            pts[1] = tmp[4];
            pts[2] = tmp[5];
        }
        if pts[3].x > clip.right {
            let mut tmp = chop_mono_cubic_at_x(&pts, clip.right);
            tmp[3].x = clip.right;
            if tmp[2].x > clip.right {
                tmp[2].x = clip.right;
            }
            self.append_cubic(&[tmp[0], tmp[1], tmp[2], tmp[3]], reverse);
            self.append_vline(clip.right, tmp[3].y, tmp[6].y, reverse);
        } else {
            self.append_cubic(&pts, reverse);
        }
    }

    pub fn clip_cubic(&mut self, src: &[Point; 4], clip: &Rect) -> bool {
        self.reset();
        let bounds = Rect::bounds(src);
        if bounds.bottom > clip.top && bounds.top < clip.bottom {
            let limit = (1 << 22) as f32;
            if bounds.left < -limit
                || bounds.top < -limit
                || bounds.right > limit
                || bounds.bottom > limit
            {
                return self.clip_line(src[0], src[3], clip);
            }
            let mut mono_y = [Point::default(); 10];
            let ny = chop_cubic_at_y_extrema(src, &mut mono_y);
            for y in 0..ny {
                let c = [
                    mono_y[y * 3],
                    mono_y[y * 3 + 1],
                    mono_y[y * 3 + 2],
                    mono_y[y * 3 + 3],
                ];
                let mut mono_x = [Point::default(); 10];
                let nx = chop_cubic_at_x_extrema(&c, &mut mono_x);
                for x in 0..nx {
                    let cc = [
                        mono_x[x * 3],
                        mono_x[x * 3 + 1],
                        mono_x[x * 3 + 2],
                        mono_x[x * 3 + 3],
                    ];
                    self.clip_mono_cubic(&cc, clip);
                }
            }
        }
        !self.verbs.is_empty()
    }
}

fn sort_increasing_y3(pts: &mut [Point; 3]) -> bool {
    if pts[0].y > pts[2].y {
        pts.swap(0, 2);
        true
    } else {
        false
    }
}
fn sort_increasing_y4(pts: &mut [Point; 4]) -> bool {
    if pts[0].y > pts[3].y {
        pts.swap(0, 3);
        pts.swap(1, 2);
        true
    } else {
        false
    }
}

fn chop_mono_quad_at(c0: f32, c1: f32, c2: f32, target: f32) -> Option<f32> {
    let a = c0 - c1 - c1 + c2;
    let b = 2.0 * (c1 - c0);
    let c = c0 - target;
    let (roots, n) = find_unit_quad_roots(a, b, c);
    if n > 0 {
        Some(roots[0])
    } else {
        None
    }
}

fn chop_quad_in_y(pts: &mut [Point; 3], clip: &Rect) {
    if pts[0].y < clip.top {
        if let Some(t) = chop_mono_quad_at(pts[0].y, pts[1].y, pts[2].y, clip.top) {
            let mut tmp = chop_quad_at(pts, t);
            tmp[2].y = clip.top;
            if tmp[3].y < clip.top {
                tmp[3].y = clip.top;
            }
            pts[0] = tmp[2];
            pts[1] = tmp[3];
        } else {
            for p in pts.iter_mut() {
                if p.y < clip.top {
                    p.y = clip.top;
                }
            }
        }
    }
    if pts[2].y > clip.bottom {
        if let Some(t) = chop_mono_quad_at(pts[0].y, pts[1].y, pts[2].y, clip.bottom) {
            let mut tmp = chop_quad_at(pts, t);
            if tmp[1].y > clip.bottom {
                tmp[1].y = clip.bottom;
            }
            tmp[2].y = clip.bottom;
            pts[1] = tmp[1];
            pts[2] = tmp[2];
        } else {
            for p in pts.iter_mut() {
                if p.y > clip.bottom {
                    p.y = clip.bottom;
                }
            }
        }
    }
}

/// `mono_cubic_closestT`: Skia's fallback when the exact root is not found.
fn mono_cubic_closest_t(src: &[f32; 4], mut x: f32) -> f32 {
    let mut t = 0.5f32;
    let mut best_t = t;
    let mut step = 0.25f32;
    let d = src[0];
    let a = src[3] + 3.0 * (src[1] - src[2]) - d;
    let b = 3.0 * (src[2] - src[1] - src[1] + d);
    let c = 3.0 * (src[1] - d);
    x -= d;
    let mut closest = f32::MAX;
    loop {
        let loc = ((a * t + b) * t + c) * t;
        let dist = (loc - x).abs();
        if closest > dist {
            closest = dist;
            best_t = t;
        }
        let last_t = t;
        t += if loc < x { step } else { -step };
        step *= 0.5;
        if !(closest > 0.25 && last_t != t) {
            break;
        }
    }
    best_t
}

/// `SkChopMonoCubicAtY` via the double root (`SkBezierCubic`): first
/// crossing with y on [0,1], split in double.
fn chop_mono_cubic_at_axis(src: &[Point; 4], vertical_axis: bool, value: f32) -> [Point; 7] {
    let coord = |p: &Point| {
        if vertical_axis {
            p.y as f64
        } else {
            p.x as f64
        }
    };
    let p0 = coord(&src[0]);
    let p1 = coord(&src[1]);
    let p2 = coord(&src[2]);
    let p3 = coord(&src[3]);
    let v = value as f64;
    // Monotonic cubic: one crossing; bisection/Newton in double.
    let f = |t: f64| -> f64 {
        let mt = 1.0 - t;
        mt * mt * mt * p0 + 3.0 * mt * mt * t * p1 + 3.0 * mt * t * t * p2 + t * t * t * p3 - v
    };
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let flo = f(lo);
    let fhi = f(hi);
    if flo == 0.0 {
        return subdivide_cubic_double(src, 0.0);
    }
    if fhi == 0.0 {
        return subdivide_cubic_double(src, 1.0);
    }
    if (flo < 0.0) == (fhi < 0.0) {
        let arr = if vertical_axis {
            [src[0].y, src[1].y, src[2].y, src[3].y]
        } else {
            [src[0].x, src[1].x, src[2].x, src[3].x]
        };
        return chop_cubic_at(src, mono_cubic_closest_t(&arr, value));
    }
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        let fm = f(mid);
        if fm == 0.0 {
            lo = mid;
            hi = mid;
            break;
        }
        if (fm < 0.0) == (flo < 0.0) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    subdivide_cubic_double(src, 0.5 * (lo + hi))
}

fn subdivide_cubic_double(src: &[Point; 4], t: f64) -> [Point; 7] {
    let p: Vec<(f64, f64)> = src.iter().map(|p| (p.x as f64, p.y as f64)).collect();
    let mix = |a: (f64, f64), b: (f64, f64)| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    let ab = mix(p[0], p[1]);
    let bc = mix(p[1], p[2]);
    let cd = mix(p[2], p[3]);
    let abc = mix(ab, bc);
    let bcd = mix(bc, cd);
    let abcd = mix(abc, bcd);
    let f = |q: (f64, f64)| Point::new(q.0 as f32, q.1 as f32);
    [src[0], f(ab), f(abc), f(abcd), f(bcd), f(cd), src[3]]
}

fn chop_mono_cubic_at_y(src: &[Point; 4], y: f32) -> [Point; 7] {
    chop_mono_cubic_at_axis(src, true, y)
}
fn chop_mono_cubic_at_x(src: &[Point; 4], x: f32) -> [Point; 7] {
    chop_mono_cubic_at_axis(src, false, x)
}

fn chop_cubic_in_y(pts: &mut [Point; 4], clip: &Rect) {
    if pts[0].y < clip.top {
        let mut tmp = chop_mono_cubic_at_y(pts, clip.top);
        if tmp[3].y < clip.top && tmp[4].y < clip.top && tmp[5].y < clip.top {
            let tmp2 = [tmp[3], tmp[4], tmp[5], tmp[6]];
            tmp = chop_mono_cubic_at_y(&tmp2, clip.top);
        }
        tmp[3].y = clip.top;
        if tmp[4].y < clip.top {
            tmp[4].y = clip.top;
        }
        pts[0] = tmp[3];
        pts[1] = tmp[4];
        pts[2] = tmp[5];
    }
    if pts[3].y > clip.bottom {
        let mut tmp = chop_mono_cubic_at_y(pts, clip.bottom);
        tmp[3].y = clip.bottom;
        if tmp[2].y > clip.bottom {
            tmp[2].y = clip.bottom;
        }
        pts[1] = tmp[1];
        pts[2] = tmp[2];
        pts[3] = tmp[3];
    }
}
