//! Working-space color and tag-driven color-space conversion.
//!
//! The working representation is **linear, premultiplied-alpha RGBA** `f32`
//! (blending happens in linear premultiplied space — see rules/gpu.md §6).
//! Tagged inputs (sRGB UI tokens, Rec.709/2020/P3 video with PQ/HLG transfers)
//! are converted into the working space via [`TaggedColor::to_working`]. The
//! conversion is matrix-driven and never hardcodes the working primaries, so the
//! working space can move to a wider gamut later without touching call sites
//! (specs §6.1; MVP working space is linear-709).

/// Working-space color: linear light, premultiplied alpha.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// RGB primaries of a color space. All MVP spaces share the D65 white point,
/// so conversions between them need no chromatic adaptation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSpace {
    Srgb,
    Rec709,
    Rec2020,
    DisplayP3,
}

/// Transfer function relating encoded values to linear light.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Transfer {
    Srgb,
    Linear,
    Pq,
    Hlg,
    Gamma(f32),
}

/// A color tagged with its source space + transfer, prior to conversion into the
/// working space. `value` is straight-alpha RGBA in the tagged encoding.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TaggedColor {
    pub value: [f32; 4],
    pub space: ColorSpace,
    pub transfer: Transfer,
}

impl Color {
    /// Fully transparent (all channels zero).
    pub const TRANSPARENT: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Decode straight sRGB RGBA (RGB via the standard piecewise sRGB transfer,
    /// alpha already linear) into linear premultiplied working color.
    pub fn from_srgb([r, g, b, a]: [f32; 4]) -> Self {
        Color {
            r: srgb_to_linear(r),
            g: srgb_to_linear(g),
            b: srgb_to_linear(b),
            a,
        }
        .premultiply()
    }

    /// Like [`Color::from_srgb`] but from 8-bit channels.
    pub fn from_srgb_u8([r, g, b, a]: [u8; 4]) -> Self {
        Self::from_srgb([
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        ])
    }

    /// Multiply RGB by alpha (straight → premultiplied).
    pub fn premultiply(self) -> Self {
        Color {
            r: self.r * self.a,
            g: self.g * self.a,
            b: self.b * self.a,
            a: self.a,
        }
    }

    /// Divide RGB by alpha (premultiplied → straight); zero alpha yields transparent.
    pub fn unpremultiply(self) -> Self {
        if self.a == 0.0 {
            return Color::TRANSPARENT;
        }
        Color {
            r: self.r / self.a,
            g: self.g / self.a,
            b: self.b / self.a,
            a: self.a,
        }
    }

    /// Linear interpolation toward `other` by `t` (not clamped). Interpolating in
    /// premultiplied linear space avoids the dark fringing of straight-alpha lerps.
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Color {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }
}

impl Transfer {
    /// Decode one encoded channel to linear light.
    fn to_linear(self, c: f32) -> f32 {
        match self {
            Transfer::Linear => c,
            Transfer::Srgb => srgb_to_linear(c),
            Transfer::Gamma(g) => c.powf(g),
            Transfer::Pq => pq_eotf(c),
            Transfer::Hlg => hlg_inverse_oetf(c),
        }
    }
}

type Mat3 = [[f32; 3]; 3];

impl ColorSpace {
    /// Linear-RGB → CIE XYZ matrix (D65).
    fn rgb_to_xyz(self) -> Mat3 {
        match self {
            // sRGB and Rec.709 share the same primaries.
            ColorSpace::Srgb | ColorSpace::Rec709 => [
                [0.4123908, 0.3575843, 0.1804808],
                [0.212_639, 0.7151687, 0.0721923],
                [0.0193308, 0.1191948, 0.9505322],
            ],
            ColorSpace::Rec2020 => [
                [0.636_958, 0.1446169, 0.168_881],
                [0.2627002, 0.6779981, 0.0593017],
                [0.0000000, 0.0280727, 1.0609851],
            ],
            ColorSpace::DisplayP3 => [
                [0.4865709, 0.2656677, 0.1982173],
                [0.2289746, 0.6917385, 0.0792869],
                [0.0000000, 0.0451134, 1.0439444],
            ],
        }
    }

    /// CIE XYZ → linear-RGB matrix (D65); inverse of [`ColorSpace::rgb_to_xyz`].
    fn xyz_to_rgb(self) -> Mat3 {
        match self {
            ColorSpace::Srgb | ColorSpace::Rec709 => [
                [3.240_97, -1.5373832, -0.4986108],
                [-0.9692436, 1.8759675, 0.0415551],
                [0.0556301, -0.203_977, 1.0569715],
            ],
            ColorSpace::Rec2020 => [
                [1.7166512, -0.3556708, -0.2533663],
                [-0.6666844, 1.6164812, 0.0157685],
                [0.0176399, -0.0427706, 0.9421031],
            ],
            ColorSpace::DisplayP3 => [
                [2.493_497, -0.9313836, -0.4027108],
                [-0.829_489, 1.7626641, 0.0236247],
                [0.0358458, -0.0761724, 0.9568845],
            ],
        }
    }
}

impl TaggedColor {
    /// Convert into the working space: decode the transfer to linear, convert the
    /// primaries (source → XYZ → working), then premultiply. Matrix-driven, so the
    /// working primaries are not hardcoded. Out-of-gamut results are not clamped —
    /// gamut mapping belongs to the output transform pass (post-MVP).
    ///
    /// Each `value` channel is expected in the transfer's normalized `[0, 1]`
    /// domain; out-of-domain inputs are not validated and may yield NaN.
    pub fn to_working(&self, working: ColorSpace) -> Color {
        let [r, g, b, a] = self.value;
        let lin = [
            self.transfer.to_linear(r),
            self.transfer.to_linear(g),
            self.transfer.to_linear(b),
        ];
        let conv = if self.space == working {
            lin
        } else {
            let xyz = mat_vec(self.space.rgb_to_xyz(), lin);
            mat_vec(working.xyz_to_rgb(), xyz)
        };
        Color {
            r: conv[0],
            g: conv[1],
            b: conv[2],
            a,
        }
        .premultiply()
    }
}

/// Standard piecewise sRGB EOTF (encoded → linear).
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// SMPTE ST 2084 (PQ) EOTF, normalized to [0, 1] (1.0 = peak, i.e. 10000 nits).
fn pq_eotf(e: f32) -> f32 {
    const M1: f32 = 0.159_301_76;
    const M2: f32 = 78.84375;
    const C1: f32 = 0.8359375;
    const C2: f32 = 18.851_563;
    const C3: f32 = 18.6875;
    let ep = e.powf(1.0 / M2);
    let num = (ep - C1).max(0.0);
    let den = C2 - C3 * ep;
    (num / den).powf(1.0 / M1)
}

/// BT.2100 HLG inverse-OETF (signal → scene linear).
fn hlg_inverse_oetf(e: f32) -> f32 {
    const A: f32 = 0.17883277;
    const B: f32 = 0.28466892;
    const C: f32 = 0.559_910_7;
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - C) / A).exp() + B) / 12.0
    }
}

fn mat_vec(m: Mat3, v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn color_close(c: Color, r: f32, g: f32, b: f32, a: f32, tol: f32) -> bool {
        close(c.r, r, tol) && close(c.g, g, tol) && close(c.b, b, tol) && close(c.a, a, tol)
    }

    #[test]
    fn from_srgb_should_decode_white_to_linear_white() {
        assert!(color_close(
            Color::from_srgb([1.0, 1.0, 1.0, 1.0]),
            1.0,
            1.0,
            1.0,
            1.0,
            1e-6
        ));
    }

    #[test]
    fn from_srgb_should_apply_piecewise_transfer() {
        // sRGB 0.5 decodes to ~0.21404 linear.
        let c = Color::from_srgb([0.5, 0.5, 0.5, 1.0]);
        assert!(close(c.r, 0.21404, 1e-4));
    }

    #[test]
    fn from_srgb_should_premultiply_by_alpha() {
        // Linear white at alpha 0.5 premultiplies to 0.5 in every RGB channel.
        assert!(color_close(
            Color::from_srgb([1.0, 1.0, 1.0, 0.5]),
            0.5,
            0.5,
            0.5,
            0.5,
            1e-6
        ));
    }

    #[test]
    fn from_srgb_u8_should_match_normalized_from_srgb() {
        let a = Color::from_srgb_u8([255, 128, 0, 255]);
        let b = Color::from_srgb([1.0, 128.0 / 255.0, 0.0, 1.0]);
        assert!(color_close(a, b.r, b.g, b.b, b.a, 1e-6));
    }

    #[test]
    fn premultiply_unpremultiply_should_round_trip() {
        let premul = Color::new(0.2, 0.3, 0.4, 0.5);
        let round = premul.unpremultiply().premultiply();
        assert!(color_close(round, 0.2, 0.3, 0.4, 0.5, 1e-6));
    }

    #[test]
    fn unpremultiply_should_return_transparent_for_zero_alpha() {
        assert_eq!(
            Color::new(0.0, 0.0, 0.0, 0.0).unpremultiply(),
            Color::TRANSPARENT
        );
    }

    #[test]
    fn lerp_should_interpolate_premultiplied_channels() {
        let mid = Color::TRANSPARENT.lerp(Color::new(1.0, 1.0, 1.0, 1.0), 0.5);
        assert!(color_close(mid, 0.5, 0.5, 0.5, 0.5, 1e-6));
    }

    #[test]
    fn to_working_should_be_identity_for_same_space_linear() {
        let tc = TaggedColor {
            value: [0.5, 0.3, 0.2, 1.0],
            space: ColorSpace::Rec709,
            transfer: Transfer::Linear,
        };
        assert!(color_close(
            tc.to_working(ColorSpace::Rec709),
            0.5,
            0.3,
            0.2,
            1.0,
            1e-6
        ));
    }

    #[test]
    fn to_working_should_decode_pq_endpoints() {
        let lo = TaggedColor {
            value: [0.0, 0.0, 0.0, 1.0],
            space: ColorSpace::Rec709,
            transfer: Transfer::Pq,
        };
        let hi = TaggedColor {
            value: [1.0, 1.0, 1.0, 1.0],
            space: ColorSpace::Rec709,
            transfer: Transfer::Pq,
        };
        assert!(close(lo.to_working(ColorSpace::Rec709).r, 0.0, 1e-5));
        assert!(close(hi.to_working(ColorSpace::Rec709).r, 1.0, 1e-4));
    }

    #[test]
    fn to_working_should_decode_hlg_half_to_one_twelfth() {
        let tc = TaggedColor {
            value: [0.5, 0.5, 0.5, 1.0],
            space: ColorSpace::Rec709,
            transfer: Transfer::Hlg,
        };
        assert!(close(tc.to_working(ColorSpace::Rec709).r, 1.0 / 12.0, 1e-5));
    }

    #[test]
    fn to_working_should_roundtrip_identical_primaries() {
        // sRGB and Rec.709 share primaries, so the matrix path is a near-identity.
        let tc = TaggedColor {
            value: [0.5, 0.3, 0.2, 1.0],
            space: ColorSpace::Srgb,
            transfer: Transfer::Linear,
        };
        assert!(color_close(
            tc.to_working(ColorSpace::Rec709),
            0.5,
            0.3,
            0.2,
            1.0,
            1e-4
        ));
    }

    #[test]
    fn to_working_should_map_d65_white_across_gamuts() {
        // RGB(1,1,1) is the D65 white point in every space, so it maps to working white.
        let tc = TaggedColor {
            value: [1.0, 1.0, 1.0, 1.0],
            space: ColorSpace::Rec2020,
            transfer: Transfer::Linear,
        };
        assert!(color_close(
            tc.to_working(ColorSpace::Rec709),
            1.0,
            1.0,
            1.0,
            1.0,
            5e-3
        ));
    }

    #[cfg(feature = "bytemuck")]
    #[test]
    fn color_should_be_tightly_packed_pod() {
        assert_eq!(
            bytemuck::bytes_of(&Color::new(1.0, 0.0, 0.0, 1.0)).len(),
            16
        );
    }
}
