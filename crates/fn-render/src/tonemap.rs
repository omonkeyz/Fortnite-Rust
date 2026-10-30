//! Constants and CPU mirrors of the UE 4.16 post-process chain implemented in shaders/post.wgsl and shaders/meter.wgsl.
//!
//! RECOVERED from the original package (identical in 1.8 and 1.10) (TODM_A_Child_Athena.DayPostProcessComponent):
//! WhiteTemp 7366.97, FilmSlope 0.92, FilmToe 0.45, FilmShoulder 0.06422, AutoExposureMinBrightness 0.75,
//! AutoExposureBias 0.47, AmbientOcclusionIntensity 0.56422. The tonemapper structure (LUT domain, white balance, ACES red
//! modifier, RRT/ODT saturation, film curve, cos^4 vignette) was read from the UE 4.16 global shaders (prototype README
//! "Tonemapper (UE 4.16)"). Also RECOVERED (TODM_A parent / exe FPostProcessSettings ctor, see each const): AutoExposureMax 1.0,
//! AO radius 160 cm / bias 2.4 cm, vignette 0.01, bloom threshold -1.0 + tints, ColorSaturationShadows/Midtones 1.25.
//! Not serialized (exe ctor defaults, exe-proven): FilmBlackClip 0, FilmWhiteClip 0.04, bloom intensity 0.675.
//! NOT implemented: depth of field, motion blur, volumetric fog, FXAA.
//!
//! # Phase-dependent post-process (SUNSET fix) and exact tonemapper details
//! The consts above are the DAY phase. The Athena time-of-day manager owns four post-process components (Morning / Day / Evening /
//! Night, `DayPhasePostProcessComponent[0..3]`) whose FPostProcessSettings differ (white temperature, Film*, auto-exposure min/bias,
//! saturation, AO intensity ...); [`PostSettings`] holds them (values RECOVERED from the merged TODM_A + TODM_A_Child_Athena packages,
//! `reports/postfx-evidence.json`, override flags re-checked against the cooked packages 2026-10-05) and the renderer reads
//! [`PostSettings::current`], driven by `time_of_day`. The phases are STACKED like UE 4.16 volumes (see [`PostSettings::blend`]): only Day
//! overrides AutoExposureBias (0.47), and its low-priority clone keeps that bias under every phase. Evening otherwise differs from Day:
//! AutoExposureMin 0.38, WhiteTemp 7940, saturation shadows 1.5 / midtones 1.15, Film slope/toe/shoulder 0.94422 / 0.316605 / 0.058165, AO 1.0.
//! Also corrected against the 4.16 shader source + the 1.8 `FPostProcessTonemapPS` DXBC (reports/postfx-shaders):
//! * the colour-LUT output is multiplied by 1.05 (`ColorLookupTable`, DXBC `mul r1.xyz, r0.yzwy, l(1.05)`) and the combine pass STORES
//!   `OutDeviceColor / 1.05` (GLOBAL LOOK 2026-10-05: CombineLUTs DXBC GlobalShaderCache#1182809 `mul o0.xyz, r0.xyzx, l(0.952381)`, missed before, which
//!   made the whole frame 5 % brighter in display space), saturated + rounded to 10 bits by the unorm A2B10G10R10 LUT; net: saturate(device colour)
//!   up to the LUT's trilinear interpolation (the LUT itself is now built and sampled like UE, see color_lut.rs);
//! * the display gamma (`InverseGamma.y = 2.2 / DisplayGamma`) is applied in LINEAR light before the sRGB encode (CombineLUTs),
//!   not on the encoded value;
//! * auto exposure is the 4.16 HISTOGRAM method (`r.DefaultFeature.AutoExposure.Method` 0 / `AutoExposureMethod` ctor default 0): the
//!   metered value is the percentile-trimmed mean of `max(r,g,b)` (not Rec.709 luma) of the half-res scene colour, the clamp
//!   `AutoExposureMin/MaxBrightness` is in those LUMINANCE units (no /0.18), exposure = 2^bias / clamped luminance, and the adaptation
//!   `1 - exp2(-dt * speed)` runs in the linear exposure domain (PostProcessEyeAdaptation.usf `ComputeEyeAdaptation`).
use bytemuck::{Pod, Zeroable};

pub const WHITE_TEMP: f64 = 7366.97216796875;
pub const FILM_SLOPE: f32 = 0.920_000_02;
pub const FILM_TOE: f32 = 0.449_999_99;
pub const FILM_SHOULDER: f32 = 0.064_219_996;
pub const FILM_BLACK_CLIP: f32 = 0.0;
pub const FILM_WHITE_CLIP: f32 = 0.04;
pub const AUTO_EXPOSURE_MIN: f32 = 0.75;
/// RECOVERED: TODM_A (parent) DayPostProcessComponent AutoExposureMaxBrightness = 1.0 (reports/postfx-evidence.json); the exe ctor
/// default 2.0 is overridden by the Day volume. (Was 2.0: with the sun in view the metered mean rose above 1.0 and darkened the frame.)
pub const AUTO_EXPOSURE_MAX: f32 = 1.0;
pub const AUTO_EXPOSURE_BIAS: f32 = 0.47;
/// LEGACY (pre-histogram metering): mid-grey divisor of the old Rec.709-luma metering. The 4.16 histogram path has NO such divisor
/// (clamp is in luminance units); kept only so old callers compile.
pub const AUTO_EXPOSURE_GREY: f32 = 0.18;
/// RECOVERED exe ctor defaults of the histogram method (reports/exe-defaults.json): AutoExposureHighPercent 98.3, HistogramLogMin -8,
/// HistogramLogMax 4 (the Day volume serialises HistogramLogMin/Max = -10.4586 / 1.57439 but WITHOUT the override flag, so they are ignored),
/// AutoExposureLowPercent 80 (Day/Night, ctor default) or 50 (Morning/Evening, tagged).
pub const AUTO_EXPOSURE_HIGH_PERCENT: f32 = 98.3;
pub const HISTOGRAM_LOG_MIN: f32 = -8.0;
pub const HISTOGRAM_LOG_MAX: f32 = 4.0;
/// Output multiplier of `ColorLookupTable()` in PostProcessTonemap.usf (`return OutDeviceColor * 1.05`), confirmed in the 1.8 tonemap DXBC.
pub const LUT_OUTPUT_SCALE: f32 = 1.05;
pub const AO_INTENSITY: f32 = 0.564_220_01;
/// RECOVERED: TODM_A (parent) AmbientOcclusionRadius = 160 cm (override flag + tagged value; the exe FPostProcessSettings ctor default 200 cm is replaced).
pub const AO_RADIUS_M: f32 = 1.6;
/// RECOVERED: TODM_A (parent) AmbientOcclusionBias = 2.4 cm (exe ctor default 3.0).
pub const AO_BIAS_M: f32 = 0.024;
pub const BLOOM_INTENSITY: f32 = 0.675;
/// RECOVERED: exe FPostProcessSettings ctor BloomThreshold = -1.0 (reports exe-defaults.json); the Day volume sets the override flag
/// without a tagged value, i.e. it keeps the ctor default (the old 1.0 was recalled from UE source).
pub const BLOOM_THRESHOLD: f32 = -1.0;
/// RECOVERED: TODM_A (parent) VignetteIntensity = 0.01 (exe ctor default 0.4 is overridden).
pub const VIGNETTE_INTENSITY: f32 = 0.01;
/// RECOVERED bloom tints Bloom1..6Tint: 1,2,4,5 = exe ctor values (override flag, no tag), Bloom3Tint 0.5 and Bloom6Tint 0 from TODM_A.
/// UE 4.16 `AddBloom` scales every tint by TintScale = 1/6 (exe-verified, see fn_postfx::bloom): bloom = sum(tint_i / 6 * blur_i) * BloomIntensity
/// (total gain 1.1165 / 6 * 0.675 = 0.126), added BEFORE the exposure. The chain itself is fn_postfx::bloom (BLOOM HOOK in renderer.rs).
pub const BLOOM_TINTS: [f32; 6] = [0.3465, 0.138, 0.5, 0.066, 0.066, 0.0];
/// RECOVERED: TODM_A ColorSaturationShadows / ColorSaturationMidtones = 1.25 (rgb; w = 1); Highlights stay at the exe default 1.0.
pub const COLOR_SATURATION_SHADOWS: f32 = 1.25;
pub const COLOR_SATURATION_MIDTONES: f32 = 1.25;
/// Exe ctor defaults (override flag set, no tag in the Day volume): ColorCorrectionShadowsMax 0.09, ColorCorrectionHighlightsMin 0.5.
pub const COLOR_CORRECTION_SHADOWS_MAX: f32 = 0.09;
pub const COLOR_CORRECTION_HIGHLIGHTS_MIN: f32 = 0.5;
/// AutoExposureSpeedUp / SpeedDown: every phase sets the override flag without a tagged value, so the exe FPostProcessSettings ctor values apply
/// (fn-exe-defaults/reports/exe-defaults.json: recovered 3 / 1, status match).
pub const ADAPT_UP: f32 = 3.0;
pub const ADAPT_DOWN: f32 = 1.0;
pub const RRT_SAT_FACTOR: f32 = 0.96;
pub const ODT_SAT_FACTOR: f32 = 0.93;
pub const AP1_LUMA: [f32; 3] = [0.272_228_72, 0.674_081_77, 0.053_689_52];

/// Daylight-locus chromaticity for a colour temperature (CIE D-series approximation, valid 4000..25000 K;
/// below 4000 K the Planckian approximation of Kim et al. is used).
pub fn temperature_xy(t: f64) -> (f64, f64) {
    let t = t.clamp(1667., 25000.);
    let x = if t < 4000. { -0.2661239e9 / (t * t * t) - 0.2343589e6 / (t * t) + 0.8776956e3 / t + 0.179910 }
        else if t <= 7000. { -4.6070e9 / (t * t * t) + 2.9678e6 / (t * t) + 0.09911e3 / t + 0.244063 }
        else { -2.0064e9 / (t * t * t) + 1.9018e6 / (t * t) + 0.24748e3 / t + 0.237040 };
    let y = if t < 2222. { -1.1063814 * x * x * x - 1.34811020 * x * x + 2.18555832 * x - 0.20219683 }
        else if t < 4000. { -0.9549476 * x * x * x - 1.37418593 * x * x + 2.09137015 * x - 0.16748867 }
        else { -3.000 * x * x + 2.870 * x - 0.275 };
    (x, y)
}
/// Source white of UE 4.16 `WhiteBalance()` (PostProcessCombineLUTs.usf, WhiteTint = 0 so the isothermal offset is zero): below 4000 K the
/// Planckian locus (Krystek 1985), otherwise the CIE D illuminant with the "revision of Planck's law" correction `T *= 1.4388 / 1.438`.
pub fn ue_source_white_xy(temp: f64) -> (f64, f64) {
    if temp < 4000. {
        let t = temp;
        let u = (0.860117757 + 1.54118254e-4 * t + 1.28641212e-7 * t * t) / (1.0 + 8.42420235e-4 * t + 7.08145163e-7 * t * t);
        let v = (0.317398726 + 4.22806245e-5 * t + 4.20481691e-8 * t * t) / (1.0 - 2.89741816e-5 * t + 1.61456053e-7 * t * t);
        return (3. * u / (2. * u - 8. * v + 4.), 2. * v / (2. * u - 8. * v + 4.));
    }
    let t = temp * (1.4388 / 1.438);
    let x = if t <= 7000. { 0.244063 + (0.09911e3 + (2.9678e6 - 4.6070e9 / t) / t) / t } else { 0.237040 + (0.24748e3 + (1.9018e6 - 2.0064e9 / t) / t) / t };
    (x, -3. * x * x + 2.87 * x - 0.275)
}
type M3 = [[f64; 3]; 3];
fn mul3(a: M3, b: M3) -> M3 {
    let mut r = [[0.; 3]; 3];
    for i in 0..3 { for j in 0..3 { for k in 0..3 { r[i][j] += a[i][k] * b[k][j]; } } }
    r
}
fn mulv(a: M3, v: [f64; 3]) -> [f64; 3] { std::array::from_fn(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2]) }
const SRGB_TO_XYZ: M3 = [[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]];
fn inv3(m: M3) -> M3 {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    [[c(1, 2, 1, 2) / det, -c(0, 2, 1, 2) / det, c(0, 1, 1, 2) / det],
     [-c(1, 2, 0, 2) / det, c(0, 2, 0, 2) / det, -c(0, 1, 0, 2) / det],
     [c(1, 2, 0, 1) / det, -c(0, 2, 0, 1) / det, c(0, 1, 0, 1) / det]]
}
const BRADFORD: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
/// Linear-sRGB matrix that maps a scene whose white is at `temp` Kelvin onto the 6500 K reference white
/// (UE's WhiteTemp default is 6500 = identity). Bradford chromatic adaptation. Row-major.
pub fn white_balance_matrix(temp: f64) -> [[f64; 3]; 3] {
    let xyz = |(x, y): (f64, f64)| [x / y, 1., (1. - x - y) / y];
    // UE: ChromaticAdaptation(SrcWhite(WhiteTemp), D65White = (0.3127, 0.3290)); WhiteTemp 6500 is therefore only approximately identity.
    let src = mulv(BRADFORD, xyz(ue_source_white_xy(temp)));
    let dst = mulv(BRADFORD, xyz((0.3127, 0.3290)));
    let scale = [[dst[0] / src[0], 0., 0.], [0., dst[1] / src[1], 0.], [0., 0., dst[2] / src[2]]];
    mul3(inv3(SRGB_TO_XYZ), mul3(inv3(BRADFORD), mul3(scale, mul3(BRADFORD, SRGB_TO_XYZ))))
}
/// CPU mirror of film_curve() in post.wgsl for one channel (AP1 working space, scene-referred input).
pub fn film_curve(x: f32, slope: f32, toe: f32, shoulder: f32, black: f32, white: f32) -> f32 {
    let toe_scale = 1. + black - toe;
    let shoulder_scale = 1. + white - shoulder;
    let (in_match, out_match) = (0.18f32, 0.18f32);
    let toe_match = if toe > 0.8 { (1. - toe - out_match) / slope + in_match.log10() } else {
        let bt = (out_match + black) / toe_scale - 1.;
        in_match.log10() - 0.5 * ((1. + bt) / (1. - bt)).ln() * (toe_scale / slope)
    };
    let straight_match = (1. - toe) / slope - toe_match;
    let shoulder_match = shoulder / slope - straight_match;
    let l = x.max(1e-10).log10();
    let straight = slope * (l + straight_match);
    let toe_c = if l < toe_match { -black + 2. * toe_scale / (1. + (-2. * (slope / toe_scale) * (l - toe_match)).exp()) } else { straight };
    let sh_c = if l > shoulder_match { (1. + white) - 2. * shoulder_scale / (1. + (2. * (slope / shoulder_scale) * (l - shoulder_match)).exp()) } else { straight };
    let mut t = ((l - toe_match) / (shoulder_match - toe_match)).clamp(0., 1.);
    if shoulder_match < toe_match { t = 1. - t; }
    t = (3. - 2. * t) * t * t;
    toe_c + (sh_c - toe_c) * t
}
/// Exposure scale of the 4.16 histogram method: `2^bias / clamp(avg_luminance, min, max)` (PostProcessEyeAdaptation.usf: the clamp is in
/// luminance units, `OutColor.g = ExposureOffsetMultipler * 1 / max(0.0001, TargetExposure)`). Mirror of meter.wgsl.
pub fn exposure_scale(avg_luminance: f32, min: f32, max: f32, bias_ev: f32) -> f32 {
    let lum = if avg_luminance.is_finite() { avg_luminance } else { min };
    2f32.powf(bias_ev) / lum.clamp(min.min(max), max).max(0.0001)
}
/// `ComputeEyeAdaptation` (PostProcessEyeAdaptation.usf): linear-domain exponential blend with `1 - exp2(-dt * speed)`, speed =
/// `AutoExposureSpeedUp` when the target luminance is above the old one (scene got brighter) else `AutoExposureSpeedDown`, then clamped.
pub fn adapt_exposure(old: f32, target: f32, dt: f32, up: f32, down: f32, min: f32, max: f32) -> f32 {
    let diff = target - old;
    let speed = if diff > 0. { up } else { down };
    (old + diff * (1. - (-dt * speed).exp2())).clamp(min.min(max), max)
}
/// EYE ADAPTATION STATE: one `MainEyeAdaptationCommon` step (PostProcessEyeAdaptation.usf), mirror of meter.wgsl. The persistent state is
/// `EyeAdaptation.r` = `2^bias / smoothed luminance`; the previous luminance is rebuilt with the CURRENT bias (`OldExposure = Multiplier / OldScale`,
/// `OldScale` 0 -> 1), and `None` (no valid eye adaptation yet: the histogram reduce binds `GWhiteTexture`) means `OldScale = 1`. Returns the new state.
pub fn eye_adaptation_step(old_scale: Option<f32>, target_lum: f32, dt: f32, bias_ev: f32, up: f32, down: f32, min: f32, max: f32) -> f32 {
    let mult = bias_ev.exp2();
    let s = old_scale.filter(|s| !s.is_nan()).unwrap_or(1.);
    let old = mult / if s != 0. { s } else { 1. };
    let lo = min.min(max);
    mult / adapt_exposure(old, target_lum.clamp(lo, max), dt, up, down, lo, max).max(0.0001)
}
/// CPU mirror of the histogram metering (PostProcessHistogram.usf + `ComputeEyeAdaptationExposure`): `values` are the per-pixel
/// `max(r,g,b)` of the half-res scene colour. 64 log2-spaced buckets between `log_min`/`log_max`, each pixel split over two neighbouring
/// buckets, bucket 0 dropped ("prevent adapting to extreme darks"), then the mean of the buckets between the `low`/`high` fractions of
/// the histogram mass (weights are the bucket luminance at `i / 64`). Returns the UNCLAMPED average luminance.
pub fn histogram_average_luminance(values: &[f32], low: f32, high: f32, log_min: f32, log_max: f32) -> f32 {
    let mul = 1. / (log_max - log_min);
    let add = -log_min * mul;
    let mut h = [0f32; 64];
    for &l in values {
        let pos = if l > 0. { l.log2() * mul + add } else { 0. };
        let fb = if pos.is_nan() { 0. } else { pos.clamp(0., 1.) } * 63. * 0.9999;
        let b0 = fb as usize;
        let w1 = fb.fract();
        h[b0] += 1. - w1;
        h[b0 + 1] += w1;
    }
    h[0] = 0.;
    let sum: f32 = h.iter().sum();
    let (mut min_frac, mut max_frac) = (sum * low, sum * high);
    let (mut acc_l, mut acc_w) = (0f32, 0f32);
    for (i, &w) in h.iter().enumerate() {
        let mut v = w;
        let sub = v.min(min_frac);
        v -= sub;
        min_frac -= sub;
        max_frac -= sub;
        v = v.min(max_frac);
        max_frac -= v;
        let lum = ((i as f32 / 64. - add) / mul).exp2();
        acc_l += lum * v;
        acc_w += v;
    }
    acc_l / acc_w.max(0.0001)
}
/// UE 4.16 natural (cos^4) vignette mask, RECOVERED from the tonemap VS/PS: p = sqrt(2)/sqrt(1+a^2) * (ndc.x, a*ndc.y) * intensity
/// with a = height/width, mask = 1/(1+|p|^2)^2. `uv` in 0..1.
pub fn vignette_mask(uv: (f32, f32), aspect_h_over_w: f32, intensity: f32) -> f32 {
    let a = aspect_h_over_w;
    let s = 2f32.sqrt() / (1. + a * a).sqrt() * intensity;
    let p = ((uv.0 * 2. - 1.) * s, (uv.1 * 2. - 1.) * a * s);
    let t = 1. / (1. + p.0 * p.0 + p.1 * p.1);
    t * t
}
// ---- CPU mirror of the whole colour path of post.wgsl (everything after exposure; bloom and AO excluded), used by the reference checks ----
pub const LUT_MIN: f32 = 0.002_667_708_6;
pub const LUT_MAX: f32 = 43.707_738;
fn srgb_encode(x: f32) -> f32 { if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1. / 2.4) - 0.055 } }
fn aces_red_modifier(a: glam::Vec3) -> glam::Vec3 {
    let (minrgb, maxrgb) = (a.min_element(), a.max_element());
    let saturation = (maxrgb.max(1e-10) - minrgb.max(1e-10)) / maxrgb.max(1e-2);
    let mut hue = 0.;
    if !(a.x == a.y && a.y == a.z) {
        hue = (1.732_050_8 * (a.y - a.z)).atan2(2. * a.x - a.y - a.z).to_degrees();
        if hue < 0. { hue += 360.; }
    }
    let centered = if hue > 180. { hue - 360. } else { hue };
    let t = (1. - (2. * centered / 135.).abs()).clamp(0., 1.);
    let w = t * t * (3. - 2. * t);
    let mut o = a;
    o.x += w * w * saturation * (0.03 - a.x) * (1. - 0.82);
    o
}
/// CombineLUTs colour correction (RECOVERED from the 4.16 CombineLUTs DXBC, shader 1182809): linear sRGB -> AP1, luma = AP1_LUMA, three
/// regions (shadows weight 1 - smoothstep(luma / ShadowsMax), highlights weight smoothstep((luma - HighlightsMin) / (1 - HighlightsMin)),
/// midtones the remainder), each `max(luma + sat * (c - luma), 0)` (the Day volume leaves contrast / gamma / gain / offset at their identity
/// defaults, so only saturation differs: shadows 1.25, midtones 1.25, highlights 1.0), blended and converted back to linear sRGB.
/// Mirrored by `color_grade` in post.wgsl.
pub fn color_grade(c: glam::Vec3) -> glam::Vec3 { color_grade_with(&PostSettings::DAY, c) }
/// `ColorCorrectAll` for a phase: per region `ColorCorrect` = saturation (about the AP1 luma, clamped >= 0), contrast
/// `pow(c / 0.18, contrast) * 0.18`, gamma `pow(c, 1 / gamma)` (gain 1 / offset 0: no phase sets them). The region value is the region
/// parameter times the global one (`ColorSaturationShadows * ColorSaturation` ...).
pub fn color_grade_with(s: &PostSettings, c: glam::Vec3) -> glam::Vec3 {
    use glam::{Mat3, Vec3};
    let to_ap1 = Mat3::from_cols_array(&[0.613191, 0.070207, 0.020619, 0.339512, 0.916336, 0.109567, 0.047366, 0.013450, 0.869607]);
    let from_ap1 = Mat3::from_cols_array(&[1.705052, -0.130257, -0.024003, -0.621791, 1.140803, -0.128969, -0.083258, -0.010549, 1.152972]);
    let ap1 = to_ap1 * c;
    let luma = ap1.dot(Vec3::from(AP1_LUMA));
    let smooth = |x: f32| { let x = x.clamp(0., 1.); x * x * (3. - 2. * x) };
    let sw = 1. - smooth(luma / COLOR_CORRECTION_SHADOWS_MAX);
    let hw = smooth((luma - COLOR_CORRECTION_HIGHLIGHTS_MIN) / (1. - COLOR_CORRECTION_HIGHLIGHTS_MIN));
    let mw = 1. - sw - hw;
    let (sat, con, gam) = (s.grade_sat(), s.grade_contrast(), s.grade_gamma());
    let region = |i: usize| {
        let w = (Vec3::splat(luma) + (ap1 - Vec3::splat(luma)) * sat[i]).max(Vec3::ZERO);
        let w = (w * (1. / 0.18)).powf(con[i]) * 0.18;
        w.powf(1. / gam[i])
    };
    from_ap1 * (region(0) * sw + region(1) * mw + region(2) * hw)
}
/// CPU reference of the display transform: scene-referred linear RGB (already multiplied by exposure and the vignette mask) -> sRGB-encoded
/// 0..1 colour, exactly the maths of the old post.frag after the vignette (LUT-domain clamp, white balance, ACES red modifier, AP1 saturation,
/// Film* curve, ODT saturation, sRGB encode, display exponent). The grain quantization dither (r.Tonemapper.GrainQuantization 1 at
/// PostProcessQuality@2/@3, CookedIniVersion.txt L5221 / L5238; 0 at @0/@1, L5187 / L5204) is applied by post.wgsl, not by this CPU reference.
pub fn display_transform(c_in: [f32; 3], display_exponent: f32) -> [f32; 3] { display_transform_with(&PostSettings::DAY, c_in, display_exponent) }
/// [`display_transform`] for an arbitrary phase blend. Order = `CombineLUTs` + `ColorLookupTable`: LUT-domain clamp, white balance, colour
/// grade, ACES red modifier, RRT sat, Film curve, ODT sat, AP1 -> sRGB, clamp >= 0, `pow(InverseGamma.y)` in LINEAR light, sRGB encode,
/// `/ 1.05` + unorm saturate (A2B10G10R10 LUT), `* 1.05` (tonemap PS), final saturate (8-bit target). No 32^3 LUT interpolation (post.wgsl samples the real LUT).
pub fn display_transform_with(s: &PostSettings, c_in: [f32; 3], display_exponent: f32) -> [f32; 3] {
    use glam::{Mat3, Vec3};
    let wb = white_balance_matrix(s.white_temp as f64);
    let wbm = Mat3::from_cols_array(&[wb[0][0] as f32, wb[1][0] as f32, wb[2][0] as f32, wb[0][1] as f32, wb[1][1] as f32, wb[2][1] as f32, wb[0][2] as f32, wb[1][2] as f32, wb[2][2] as f32]);
    let srgb_to_ap0 = Mat3::from_cols_array(&[0.439701, 0.089792, 0.017544, 0.382978, 0.813423, 0.111544, 0.177335, 0.096762, 0.870704]);
    let ap0_to_ap1 = Mat3::from_cols_array(&[1.451439, -0.076554, 0.008316, -0.236511, 1.176230, -0.006032, -0.214929, -0.099676, 0.997716]);
    let ap1_to_srgb = Mat3::from_cols_array(&[1.705052, -0.130257, -0.024003, -0.621791, 1.140803, -0.128969, -0.083258, -0.010549, 1.152972]);
    let luma = Vec3::from(AP1_LUMA);
    let mut c = Vec3::from(c_in).clamp(Vec3::splat(LUT_MIN), Vec3::splat(LUT_MAX));
    c = color_grade_with(s, wbm * c);
    let ap0 = aces_red_modifier(srgb_to_ap0 * c);
    let mut ap1 = (ap0_to_ap1 * ap0).max(Vec3::ZERO);
    ap1 = Vec3::splat(ap1.dot(luma)).lerp(ap1, RRT_SAT_FACTOR);
    let f = |x: f32| film_curve(x, s.film_slope, s.film_toe, s.film_shoulder, FILM_BLACK_CLIP, FILM_WHITE_CLIP);
    let mut toned = Vec3::new(f(ap1.x), f(ap1.y), f(ap1.z));
    toned = Vec3::splat(toned.dot(luma)).lerp(toned, ODT_SAT_FACTOR).max(Vec3::ZERO);
    c = (ap1_to_srgb * toned).max(Vec3::ZERO).powf(display_exponent);
    let enc = |x: f32| ((srgb_encode(x) / LUT_OUTPUT_SCALE).clamp(0., 1.) * LUT_OUTPUT_SCALE).min(1.);
    [enc(c.x), enc(c.y), enc(c.z)]
}
/// The FPostProcessSettings fields that vary between the four TOD phases (everything else is constant across phases, see the consts).
/// Order of [`PostSettings::fields_mut`]: see `F_*`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PostSettings {
    pub white_temp: f32,
    pub film_slope: f32,
    pub film_toe: f32,
    pub film_shoulder: f32,
    pub ae_min: f32,
    pub ae_max: f32,
    /// AutoExposureBias in stops.
    pub ae_bias: f32,
    /// AutoExposureLowPercent (0..100).
    pub ae_low_percent: f32,
    pub sat_shadows: f32,
    pub sat_midtones: f32,
    /// Global `ColorSaturation` (multiplies all three regions).
    pub sat_global: f32,
    pub contrast_shadows: f32,
    pub gamma_midtones: f32,
    pub ao_intensity: f32,
    pub contrast_global: f32,
    pub scene_tint: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicPp { pub weight: f32, pub scene_color_tint: Option<[f32; 3]>, pub color_contrast: Option<f32> }
static DYNAMIC_PP: std::sync::Mutex<Vec<DynamicPp>> = std::sync::Mutex::new(Vec::new());
pub fn set_dynamic_pp(list: Vec<DynamicPp>) { *DYNAMIC_PP.lock().unwrap_or_else(|p| p.into_inner()) = list; }
pub fn dynamic_pp() -> Vec<DynamicPp> { DYNAMIC_PP.lock().unwrap_or_else(|p| p.into_inner()).clone() }
const NF: usize = 14;
/// One phase component: `Some(v)` = `bOverride_X` true with value `v` (tagged, or the exe ctor default when flagged but untagged),
/// `None` = not overridden (the value of the layer below is kept).
type Overrides = [Option<f32>; NF];

impl PostSettings {
    /// `FPostProcessSettings` ctor defaults from the 1.8 exe (reports/exe-defaults.json): WhiteTemp 6500, FilmSlope 0.88, FilmToe 0.55,
    /// FilmShoulder 0.26, AutoExposureMin 0.03 / Max 2 / Bias 0 / LowPercent 80, saturation/contrast/gamma 1, AmbientOcclusionIntensity 0.5.
    pub const EXE_DEFAULT: PostSettings = PostSettings {
        white_temp: 6500., film_slope: 0.88, film_toe: 0.55, film_shoulder: 0.26, ae_min: 0.03, ae_max: 2.0, ae_bias: 0.0, ae_low_percent: 80.,
        sat_shadows: 1., sat_midtones: 1., sat_global: 1., contrast_shadows: 1., gamma_midtones: 1., ao_intensity: 0.5, contrast_global: 1., scene_tint: [1.; 3],
    };
    fn fields_mut(&mut self) -> [&mut f32; NF] {
        [&mut self.white_temp, &mut self.film_slope, &mut self.film_toe, &mut self.film_shoulder, &mut self.ae_min, &mut self.ae_max, &mut self.ae_bias,
         &mut self.ae_low_percent, &mut self.sat_shadows, &mut self.sat_midtones, &mut self.sat_global, &mut self.contrast_shadows, &mut self.gamma_midtones, &mut self.ao_intensity]
    }
    /// UE `OverridePostProcessSettings(Src, Weight)` (SceneView.cpp:1208): weight <= 0 does nothing, > 1 is clamped, every `bOverride_X` field
    /// becomes `FMath::Lerp(Dest, Src, Weight)` (`LERP_PP`), the others keep the value of the layers below.
    fn apply(&mut self, ov: &Overrides, weight: f32) {
        if !(weight > 0.) { return; }
        let weight = weight.min(1.);
        for (d, o) in self.fields_mut().into_iter().zip(ov.iter()) { if let Some(v) = o { *d += (*v - *d) * weight; } }
    }
    /// One phase component ALONE on top of the exe defaults (no stacking: Morning / Evening / Night then have bias 0). For reference only; the
    /// game always sees the stack, see [`Self::blend`] / [`Self::at_hour`].
    pub fn phase(p: usize) -> PostSettings { let mut s = Self::EXE_DEFAULT; s.apply(&PHASES[p.min(3)], 1.); s }
    /// The stacked settings at an in-game hour.
    pub fn at_hour(hour: f32) -> PostSettings { let (f, t, w) = crate::time_of_day::blend_at(hour); Self::blend(f, t, w) }
    pub const DAY: PostSettings = PostSettings {
        white_temp: WHITE_TEMP as f32, film_slope: FILM_SLOPE, film_toe: FILM_TOE, film_shoulder: FILM_SHOULDER, ae_min: AUTO_EXPOSURE_MIN, ae_max: AUTO_EXPOSURE_MAX,
        ae_bias: AUTO_EXPOSURE_BIAS, ae_low_percent: 80., sat_shadows: COLOR_SATURATION_SHADOWS, sat_midtones: COLOR_SATURATION_MIDTONES, sat_global: 1.,
        contrast_shadows: 1., gamma_midtones: 1., ao_intensity: AO_INTENSITY, contrast_global: 1., scene_tint: [1.; 3],
    };
    /// The TOD post-process stack for the blend (`from`, `to`, `t`) of `time_of_day::blend_at`, exactly as UE 4.16 builds it (exe-verified, see
    /// `time_of_day::pp_components`): on top of the exe defaults, the seven unbound components in `PostProcessVolumes` order (Day / Evening / Night
    /// clones at priority -7 / -6 / -5 and weight 1, then Morning .. Night at -4 .. -1 with `BlendWeight = i <= to ? 1 : 0` and `t` for `to` while a
    /// transition runs), each through `OverridePostProcessSettings` (only `bOverride_*` fields, lerped by the weight). So Day's AutoExposureBias 0.47
    /// (only Day overrides it) applies at every hour via the Day clone, and fields a phase does not override come from the layers below.
    pub fn blend(from: usize, to: usize, t: f32) -> PostSettings {
        let mut s = Self::EXE_DEFAULT;
        let c = crate::time_of_day::pp_components(from, to, t);
        for i in crate::time_of_day::pp_volume_order(&c.map(|c| c.priority)) {
            if let Some(w) = crate::time_of_day::pp_local_weight(c[i].weight) { s.apply(&PHASES[c[i].phase], w); }
        }
        s
    }
    /// Settings for the current `time_of_day` clock.
    pub fn current() -> PostSettings {
        let st = crate::time_of_day::current();
        Self::blend(st.from, st.to, st.blend)
    }
    /// Saturation per region [shadows, midtones, highlights] = region value * global `ColorSaturation`.
    pub fn grade_sat(&self) -> [f32; 3] { [self.sat_shadows * self.sat_global, self.sat_midtones * self.sat_global, self.sat_global] }
    pub fn grade_contrast(&self) -> [f32; 3] { [self.contrast_shadows * self.contrast_global, self.contrast_global, self.contrast_global] }
    pub fn with_overrides(mut self, list: &[DynamicPp]) -> PostSettings {
        for d in list {
            if !(d.weight > 0.) { continue; }
            let w = d.weight.min(1.);
            if let Some(t) = d.scene_color_tint { for i in 0..3 { self.scene_tint[i] += (t[i] - self.scene_tint[i]) * w; } }
            if let Some(c) = d.color_contrast { self.contrast_global += (c - self.contrast_global) * w; }
        }
        self
    }
    pub fn with_dynamic(self) -> PostSettings { self.with_overrides(&dynamic_pp()) }
    pub fn grade_gamma(&self) -> [f32; 3] { [1., self.gamma_midtones, 1.] }
    /// True when contrast / gamma differ from identity (skips the pow() work in the shader).
    pub fn grade_needs_pow(&self) -> bool { self.contrast_shadows != 1. || self.gamma_midtones != 1. || self.contrast_global != 1. }
    /// GPU block of the post pass for these settings. `proj` = (tan half fov x, tan half fov y, near, far).
    pub fn post_params(&self, bloom_on: bool, vignette_on: bool, ao_on: bool, proj: [f32; 4], view: [f32; 4], backdrop: [f32; 4]) -> PostParams {
        let (s, c, g) = (self.grade_sat(), self.grade_contrast(), self.grade_gamma());
        PostParams {
            wb: white_balance_columns_for(self.white_temp as f64),
            film_stw: [self.film_slope, self.film_toe, self.film_shoulder, 0.],
            film_clip: [FILM_BLACK_CLIP, FILM_WHITE_CLIP, 0., 0.],
            bloom: [BLOOM_INTENSITY, BLOOM_THRESHOLD, bloom_on as u32 as f32, 0.],
            vignette: [VIGNETTE_INTENSITY, vignette_on as u32 as f32, 0., 0.],
            ao: [self.ao_intensity, AO_RADIUS_M, ao_on as u32 as f32, AO_BIAS_M], // legacy block: no shader reads it since the UE SSAO port (ssao_ue.rs)
            proj, view, backdrop,
            grade_sat: [s[0], s[1], s[2], 0.],
            grade_contrast: [c[0], c[1], c[2], 0.],
            grade_gamma: [g[0], g[1], g[2], 0.],
            grade_range: [COLOR_CORRECTION_SHADOWS_MAX, COLOR_CORRECTION_HIGHLIGHTS_MIN, self.grade_needs_pow() as u32 as f32, LUT_OUTPUT_SCALE],
            tonemap_ext: [0.; 4],
            fringe: [1., 1., 0., 0.],
            scene_tint: [self.scene_tint[0], self.scene_tint[1], self.scene_tint[2], 1.],
        }
    }
    /// GPU block of the exposure pass.
    pub fn meter_params(&self, dt: f32, first: bool, fixed_exposure: f32) -> MeterParams {
        MeterParams {
            dt, min_brightness: self.ae_min, max_brightness: self.ae_max, bias_ev: self.ae_bias,
            first: first as u32 as f32, fixed_exposure, adapt_up: ADAPT_UP, adapt_down: ADAPT_DOWN,
            low_fraction: self.ae_low_percent.clamp(1., 99.) * 0.01, high_fraction: AUTO_EXPOSURE_HIGH_PERCENT.clamp(1., 99.) * 0.01,
            log_min: HISTOGRAM_LOG_MIN, log_max: HISTOGRAM_LOG_MAX,
        }
    }
}
const fn o(v: f32) -> Option<f32> { Some(v) }
/// Merged TODM_A (parent) + TODM_A_Child_Athena (child) phase components, in `DayPhasePostProcessComponent[0..3]` order (Morning, Day,
/// Evening, Night). RECOVERED from `reports/postfx-evidence.json` (`todm.phases.*.applied_tagged` / `overridden_untagged...`); a flagged
/// field without a tagged value takes the exe ctor default (`o(80.)` for LowPercent, `o(1.)` for the identity colour grading fields).
/// Order of fields = `fields_mut`: white_temp, slope, toe, shoulder, ae_min, ae_max, ae_bias, ae_low, sat_sh, sat_mid, sat_glob, con_sh, gamma_mid, ao.
const PHASES: [Overrides; 4] = [
    // Morning
    [o(6995.412_6), o(0.944_953_98), o(0.541_284_97), o(0.128_44), o(0.35), o(1.), None, o(50.), o(1.25), o(1.23), o(1.), o(1.), o(1.1), o(1.)],
    // Day
    [o(7366.972_2), o(0.92), o(0.45), o(0.064_22), o(0.75), o(1.), o(0.47), o(80.), o(1.25), o(1.25), o(1.), o(1.), o(1.), o(0.564_22)],
    // Evening (SUNSET)
    [o(7940.362_3), o(0.944_22), o(0.316_605), o(0.058_165), o(0.38), o(1.), None, o(50.), o(1.5), o(1.15), o(1.), o(1.), o(1.), o(1.)],
    // Night
    [o(4650.), o(0.908_257), o(0.481_743), o(0.104_037), o(0.30), o(1.), None, o(80.), o(1.2), o(1.1), o(1.15), o(0.9), o(1.05), o(1.)],
];

/// Number of mips of the HDR scene target down to 1x1.
pub fn mip_count(w: u32, h: u32) -> u32 { w.max(h).max(1).ilog2() + 1 }

/// GPU parameter block of the post pass (struct PostParams in post.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct PostParams {
    pub wb: [[f32; 4]; 3],
    pub film_stw: [f32; 4],
    pub film_clip: [f32; 4],
    pub bloom: [f32; 4],
    pub vignette: [f32; 4],
    pub ao: [f32; 4],
    pub proj: [f32; 4],
    /// width, height, display exponent, debug mode.
    pub view: [f32; 4],
    /// pause-menu backdrop: dim, blur, 0, 0.
    pub backdrop: [f32; 4],
    /// colour-grading region saturation (shadows, midtones, highlights, 0): region value * global ColorSaturation.
    pub grade_sat: [f32; 4],
    pub grade_contrast: [f32; 4],
    pub grade_gamma: [f32; 4],
    /// ColorCorrectionShadowsMax, ColorCorrectionHighlightsMin, 1 when contrast/gamma are not identity, LUT output scale (1.05).
    pub grade_range: [f32; 4],
    /// SCALABILITY: x = r.Tonemapper.Sharpen / 6, y = 1 when r.Tonemapper.GrainQuantization > 0, zw = GrainRandomFull.xy (see post.wgsl); the renderer fills it.
    pub tonemap_ext: [f32; 4],
    /// POSTFX SETTINGS: scene colour fringe (USE_COLOR_FRINGE): xy = FringeUVParams (G, R sample scale about the view centre), z = 1 when on
    /// (`effect_settings::EffectSettings::fringe_on`); the renderer fills it.
    pub fringe: [f32; 4],
    pub scene_tint: [f32; 4],
}
/// GPU parameter block of the exposure compute pass (struct MeterParams in meter.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MeterParams {
    pub dt: f32,
    pub min_brightness: f32,
    pub max_brightness: f32,
    pub bias_ev: f32,
    pub first: f32,
    pub fixed_exposure: f32,
    pub adapt_up: f32,
    pub adapt_down: f32,
    /// AutoExposureLowPercent / HighPercent as fractions, HistogramLogMin / Max.
    pub low_fraction: f32,
    pub high_fraction: f32,
    pub log_min: f32,
    pub log_max: f32,
}
/// White-balance matrix as three padded column vectors for the WGSL mat3x3 constructor (column-major), DAY white temperature
/// (the lobby's own post pass uses this; the in-game pass uses [`white_balance_columns_for`] with the phase value).
pub fn white_balance_columns() -> [[f32; 4]; 3] { white_balance_columns_for(WHITE_TEMP) }
pub fn white_balance_columns_for(temp: f64) -> [[f32; 4]; 3] {
    let m = white_balance_matrix(temp);
    std::array::from_fn(|c| [m[0][c] as f32, m[1][c] as f32, m[2][c] as f32, 0.])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn white_balance_is_identity_at_6500k_and_cools_warm_light_otherwise() {
        let id = white_balance_matrix(6500.);
        // UE's WhiteBalance maps (D-illuminant(6500 * 1.4388/1.438)) onto the literal D65 (0.3127, 0.3290): identity to ~1e-3, not exactly.
        for i in 0..3 { for j in 0..3 { assert!((id[i][j] - if i == j { 1. } else { 0. }).abs() < 3e-3, "{id:?}"); } }
        // WhiteTemp 7366 K (bluish scene white) is mapped onto 6500 K: red gain > blue gain.
        let m = white_balance_matrix(WHITE_TEMP);
        let white = [m[0].iter().sum::<f64>(), m[1].iter().sum::<f64>(), m[2].iter().sum::<f64>()];
        assert!(white[0] > 1.0 && white[2] < 1.0, "{white:?}");
        // luminance-ish preserved: row sums average close to 1
        assert!(((white[0] + white[1] + white[2]) / 3. - 1.).abs() < 0.1);
    }
    #[test]
    fn film_curve_is_monotonic_and_maps_mid_grey_near_mid_grey() {
        let f = |x: f32| film_curve(x, FILM_SLOPE, FILM_TOE, FILM_SHOULDER, FILM_BLACK_CLIP, FILM_WHITE_CLIP);
        let mut prev = f(1e-4);
        for k in 1..400 {
            let x = 1e-4 * 1.05f32.powi(k);
            let y = f(x);
            assert!(y >= prev - 1e-6, "not monotonic at {x}: {y} < {prev}");
            prev = y;
        }
        let mid = f(0.18);
        assert!((mid - 0.18).abs() < 0.02, "mid grey maps to {mid}");
        assert!(f(100.) <= 1.0 + FILM_WHITE_CLIP + 1e-3);
    }
    #[test]
    fn exposure_follows_the_4_16_histogram_semantics() {
        // Evening (sunset): bias 0, min 0.38, max 1 -> exposure = 1 / clamp(L, 0.38, 1)
        let e = PostSettings::phase(crate::time_of_day::EVENING);
        assert_eq!((e.ae_bias, e.ae_min, e.ae_max), (0., 0.38, 1.));
        assert!((exposure_scale(0.6, e.ae_min, e.ae_max, e.ae_bias) - 1. / 0.6).abs() < 1e-5);
        // clamped: dark scenes never exceed 2^bias / min, bright ones never go below 2^bias / max (no mid-grey divisor)
        assert!((exposure_scale(0.001, 0.75, 1., 0.47) - 2f32.powf(0.47) / 0.75).abs() < 1e-5);
        assert!((exposure_scale(100., 0.75, 1., 0.47) - 2f32.powf(0.47)).abs() < 1e-5);
        assert!((exposure_scale(f32::NAN, 0.75, 1., 0.47) - 2f32.powf(0.47) / 0.75).abs() < 1e-5);
    }
    #[test]
    fn histogram_metering_trims_outliers_and_ignores_the_darkest_bucket() {
        // uniform 0.5 -> the mean luminance of the buckets around 0.5 (bucket resolution 12/64 stops = 0.19 stops)
        let v = vec![0.5f32; 4096];
        let l = histogram_average_luminance(&v, 0.8, 0.983, HISTOGRAM_LOG_MIN, HISTOGRAM_LOG_MAX);
        assert!((l / 0.5 - 1.).abs() < 0.15, "{l}");
        // a few very bright pixels (sun disc) fall in the top 1.7 % and are trimmed away
        let mut w = vec![0.5f32; 4000];
        w.extend(std::iter::repeat(14.).take(40));
        let l2 = histogram_average_luminance(&w, 0.8, 0.983, HISTOGRAM_LOG_MIN, HISTOGRAM_LOG_MAX);
        assert!((l2 / 0.5 - 1.).abs() < 0.2, "{l2}");
        // pure black adds nothing (bucket 0 dropped) -> average of the rest
        let mut z = vec![0.0f32; 1000];
        z.extend(std::iter::repeat(0.5).take(1000));
        let l3 = histogram_average_luminance(&z, 0.0, 1.0, HISTOGRAM_LOG_MIN, HISTOGRAM_LOG_MAX);
        assert!((l3 / 0.5 - 1.).abs() < 0.15, "{l3}");
    }
    #[test]
    fn adaptation_is_faster_up_than_down_and_converges() {
        let up = adapt_exposure(0.4, 0.8, 0.1, 3., 1., 0.3, 1.) - 0.4;
        let down = 0.8 - adapt_exposure(0.8, 0.4, 0.1, 3., 1., 0.3, 1.);
        assert!(up > down * 2.5, "{up} {down}");
        let mut x = 0.4;
        for _ in 0..600 { x = adapt_exposure(x, 0.9, 1. / 60., 3., 1., 0.3, 1.); }
        assert!((x - 0.9).abs() < 1e-3);
        // the clamp wins over the target
        assert_eq!(adapt_exposure(0.5, 2., 10., 3., 1., 0.3, 1.), 1.);
    }
    #[test]
    fn eye_adaptation_state_is_bias_over_luminance_like_ue() {
        // first frame: previous scale 1 -> previous luminance = 2^bias (Day 0.47 -> 1.385, clamped to max 1), no snap to the target
        let s = eye_adaptation_step(None, 0.8, 0., 0.47, ADAPT_UP, ADAPT_DOWN, 0.75, 1.);
        assert!((s - 0.47f32.exp2() / 1.).abs() < 1e-5, "{s}");
        // steady state: scale = 2^bias / L
        let mut st = None;
        for _ in 0..2000 { st = Some(eye_adaptation_step(st, 0.8, 1. / 60., 0.47, ADAPT_UP, ADAPT_DOWN, 0.75, 1.)); }
        assert!((st.unwrap() - 0.47f32.exp2() / 0.8).abs() < 1e-3);
        // a bias change moves the rebuilt previous luminance (UE stores bias / L, not L): with dt = 0 the scale is kept and the luminance
        // becomes mult_new / scale_old = 1 / 1.731 = 0.578 (a port storing L would keep 0.8 and jump the scale to 1.25)
        let old = st.unwrap();
        let s2 = eye_adaptation_step(Some(old), 0.8, 0., 0.0, ADAPT_UP, ADAPT_DOWN, 0.38, 1.);
        assert!((s2 - old).abs() < 1e-4, "{s2} {old}");
        assert!((1. / s2 - 0.578).abs() < 2e-3);
        let m = fn_live::wgsl!("../shaders/meter.wgsl");
        assert!(m.contains("let old_lum = mult / select(1.0, old_scale, old_scale != 0.0)") && m.contains("old_scale = 1.0"));
    }
    #[test]
    fn evening_post_differs_from_day_as_recovered() {
        let d = PostSettings::phase(crate::time_of_day::DAY);
        assert_eq!(d, PostSettings::DAY, "phase table Day must equal the Day consts");
        let e = PostSettings::phase(crate::time_of_day::EVENING);
        assert!((e.white_temp - 7940.3623).abs() < 1e-2 && (e.film_toe - 0.316605).abs() < 1e-6 && e.sat_shadows == 1.5 && e.sat_midtones == 1.15 && e.ao_intensity == 1.0);
        // transition Day -> Evening: fields both phases override lerp, the bias (Day only) goes 0.47 -> 0.47*(1-t)
        let m = PostSettings::blend(crate::time_of_day::DAY, crate::time_of_day::EVENING, 0.5);
        assert!((m.ae_min - 0.565).abs() < 1e-5 && (m.sat_shadows - 1.375).abs() < 1e-5 && (m.ae_bias - 0.47).abs() < 1e-5, "{m:?}");
    }
    /// PP STACK: literal merged rows (TODM_A + TODM_A_Child_Athena, bOverride flags re-read from the cooked packages 2026-10-05), `fields_mut` order;
    /// column 6 (AutoExposureBias) is None for every phase but Day.
    const ROWS: [[f32; NF]; 4] = [
        [6995.4126, 0.944_953_98, 0.541_284_97, 0.128_44, 0.35, 1., f32::NAN, 50., 1.25, 1.23, 1., 1., 1.1, 1.],
        [7366.9722, 0.92, 0.45, 0.064_22, 0.75, 1., 0.47, 80., 1.25, 1.25, 1., 1., 1., 0.564_22],
        [7940.3623, 0.944_22, 0.316_605, 0.058_165, 0.38, 1., f32::NAN, 50., 1.5, 1.15, 1., 1., 1., 1.],
        [4650., 0.908_257, 0.481_743, 0.104_037, 0.30, 1., f32::NAN, 80., 1.2, 1.1, 1.15, 0.9, 1.05, 1.],
    ];
    fn fields(s: &PostSettings) -> [f32; NF] { let mut c = *s; let r = c.fields_mut().map(|v| *v); r }
    /// The exe rule written out by hand (not via the component list): with the clones D, E, N (weight 1, priorities -7..-5) under Morning..Night
    /// (`BlendWeight = i <= to`), everything below `to` collapses to the PREVIOUS phase (Morning's previous is Night, via the Night clone), then `to`
    /// lerps on top at w (t in a transition, else 1). AutoExposureBias: only Day overrides it and the Day clone is always there -> 0.47 at every hour.
    fn exe_rule(hour: f32) -> [f32; NF] {
        let (f, to, t) = crate::time_of_day::blend_at(hour);
        let w = if f == to { 1. } else { t };
        let prev = (to + 3) % 4;
        std::array::from_fn(|k| if k == 6 { 0.47 } else { ROWS[prev][k] + (ROWS[to][k] - ROWS[prev][k]) * w })
    }
    /// The port before the PP STACK fix: `from` at weight 1 then `to` at `t` on the exe defaults (kept only to document the pop).
    fn old_blend(hour: f32) -> PostSettings {
        let (f, to, t) = crate::time_of_day::blend_at(hour);
        let mut s = PostSettings::EXE_DEFAULT;
        s.apply(&PHASES[f], 1.);
        if to != f { s.apply(&PHASES[to], t); }
        s
    }
    #[test]
    fn tod_stack_matches_the_exe_rule_at_the_reference_hours() {
        use crate::time_of_day::{blend_at, DAY, EVENING, MORNING, NIGHT};
        let expect_phase = [(2., NIGHT, NIGHT), (4., NIGHT, MORNING), (7., MORNING, DAY), (12., DAY, DAY), (17., DAY, EVENING), (20.5, EVENING, EVENING), (23., NIGHT, NIGHT)];
        for (h, f, to) in expect_phase { let (a, b, _) = blend_at(h); assert_eq!((a, b), (f, to), "phase pair at {h}"); }
        assert_eq!(blend_at(19.6).1, EVENING);
        for h in [2., 4., 7., 12., 17., 19.6, 20.5, 23.] {
            let got = fields(&PostSettings::at_hour(h));
            let want = exe_rule(h);
            for k in 0..NF { assert!((got[k] - want[k]).abs() <= 1e-4 * want[k].abs().max(1.), "hour {h} field {k}: {} vs exe rule {}", got[k], want[k]); }
            assert_eq!(PostSettings::at_hour(h).ae_bias, 0.47, "Day's bias stays applied at {h}");
        }
        // pure phases: the stack equals that phase's row with Day's bias
        for (h, p) in [(2., NIGHT), (12., DAY), (20.5, EVENING), (23., NIGHT), (4.6, MORNING)] {
            let g = fields(&PostSettings::at_hour(h));
            for k in 0..NF { if k != 6 { assert!((g[k] - ROWS[p][k]).abs() <= 1e-4 * ROWS[p][k].abs().max(1.), "h {h} k {k}"); } }
        }
        // the component list: registration order M, D, E, N, clones D, E, N; volume order = clones then phases (priority -7 .. -1)
        let c = crate::time_of_day::pp_components(DAY, EVENING, 0.25);
        assert_eq!(c.map(|c| c.priority), [-4., -3., -2., -1., -7., -6., -5.]);
        assert_eq!(c.map(|c| c.weight), [1., 1., 0.25, 0., 1., 1., 1.]);
        assert_eq!(crate::time_of_day::pp_volume_order(&c.map(|c| c.priority)), vec![4, 5, 6, 0, 1, 2, 3]);
        assert_eq!(crate::time_of_day::pp_volume_order(&[0., -1., 0., -1.]), vec![1, 3, 0, 2], "equal priorities keep registration order (InsertVolume)");
    }
    /// No pop through 16 h -> 20 h (the old blend dropped Day's bias at 19.6 h: 2^0.47 = 1.385x, ~28 % darker in one frame).
    #[test]
    fn tod_stack_has_no_pop_from_16h_to_20h() {
        let (d, e) = (ROWS[1], ROWS[2]);
        let step = 1. / 1800.; // 2 s of in-game time per sample
        let mut prev = fields(&PostSettings::at_hour(16.));
        let mut h = 16.;
        while h < 20. {
            h += step;
            let cur = fields(&PostSettings::at_hour(h));
            for k in 0..NF {
                let span = if k == 6 { 0. } else { (e[k] - d[k]).abs() };
                // smoothstep slope <= 1.5 over the 3 h window
                let bound = span * 1.5 / 3.0 * step * 1.02 + 1e-5 * cur[k].abs().max(1.);
                assert!((cur[k] - prev[k]).abs() <= bound, "pop at {h} h, field {k}: {} -> {}", prev[k], cur[k]);
            }
            prev = cur;
        }
        // the old blend does pop there
        assert!((old_blend(19.59).ae_bias - 0.47).abs() < 1e-3 && old_blend(19.61).ae_bias == 0.);
    }
    /// Hour sweep for verification/tod/ppstack/hour_sweep.txt: `cargo test -p fn-render tod_stack_hour_sweep -- --nocapture`.
    #[test]
    fn tod_stack_hour_sweep() {
        println!("hour  from to   t      | white_t  slope   toe     shoulder ae_min ae_max bias  low  sat_sh sat_mid sat_g con_sh gam_mid ao    | old_bias old_ae_min | max|stack-exe_rule|");
        let mut hours: Vec<f32> = (0..96).map(|k| k as f32 * 0.25).collect();
        hours.extend([19.6, 20.5]);
        hours.sort_by(|a, b| a.partial_cmp(b).unwrap());
        hours.dedup();
        for h in hours {
            let (f, to, t) = crate::time_of_day::blend_at(h);
            let s = PostSettings::at_hour(h);
            let g = fields(&s);
            let r = exe_rule(h);
            let err = (0..NF).map(|k| (g[k] - r[k]).abs() / r[k].abs().max(1.)).fold(0f32, f32::max);
            let o = old_blend(h);
            println!("{h:5.2} {f}    {to}  {t:6.4} | {:7.1} {:.5} {:.5} {:.5}  {:.4} {:.3}  {:.3} {:4.1} {:.4} {:.4}  {:.3} {:.3}  {:.4}  {:.4} | {:.3}    {:.4}     | {err:.2e}",
                s.white_temp, s.film_slope, s.film_toe, s.film_shoulder, s.ae_min, s.ae_max, s.ae_bias, s.ae_low_percent, s.sat_shadows, s.sat_midtones, s.sat_global,
                s.contrast_shadows, s.gamma_midtones, s.ao_intensity, o.ae_bias, o.ae_min);
            assert!(err < 1e-4, "{h}");
        }
    }
    #[test]
    fn vignette_corner_is_aspect_independent() {
        // README: the corner mask is 0.574 at intensity 0.4 for every aspect
        for a in [9. / 16., 3. / 4., 1., 9. / 21.] {
            let c = vignette_mask((1., 1.), a, 0.4); // formula check at 0.4; the Day value is VIGNETTE_INTENSITY = 0.01
            assert!((c - 0.574).abs() < 0.002, "a={a}: {c}");
        }
        assert!((vignette_mask((0.5, 0.5), 0.5625, 0.4) - 1.).abs() < 1e-6);
    }
    #[test]
    fn display_transform_maps_mid_grey_and_is_monotonic() {
        let mid = display_transform([0.18; 3], 1.0);
        assert!(mid.iter().all(|v| *v > 0.4 && *v < 0.65), "{mid:?}");
        let (a, b) = (display_transform([0.1; 3], 1.0), display_transform([2.0; 3], 1.0));
        assert!(b[1] > a[1] && b[1] <= 1.0);
        // display exponent < 1 brightens
        assert!(display_transform([0.18; 3], 0.8)[1] > mid[1]);
    }
    #[test]
    fn mip_chain_reaches_1x1() {
        assert_eq!(mip_count(1, 1), 1);
        assert_eq!(mip_count(1280, 720), 11);
        assert_eq!(mip_count(1920, 1080), 11);
        assert_eq!(mip_count(2560, 1440), 12);
    }
    #[test]
    fn gpu_blocks_have_the_declared_sizes() {
        assert_eq!(std::mem::size_of::<PostParams>(), 3 * 16 + 6 * 16 + 16 + 16 + 4 * 16 + 16 + 16); // + POSTFX fringe
        assert_eq!(std::mem::size_of::<MeterParams>(), 48);
    }
    #[test]
    fn wgsl_carries_the_recovered_constants() {
        let p = fn_live::wgsl!("../shaders/post.wgsl");
        for needle in ["RRT_SAT_FACTOR: f32 = 0.96", "ODT_SAT_FACTOR: f32 = 0.93", "LUT_MIN: f32 = 0.0026677086", "LUT_MAX: f32 = 43.707738", "0.439701, 0.089792, 0.017544", "1.451439, -0.076554, 0.008316", "1.705052, -0.130257, -0.024003"] {
            assert!(p.contains(needle), "post.wgsl lacks {needle}");
        }
        let m = fn_live::wgsl!("../shaders/meter.wgsl");
        assert!(m.contains("exp2(mp.bias_ev)") && m.contains("histogram"));
        assert!(p.contains("textureSampleLevel(color_lut, lin_samp, uvw, 0.0).rgb * 1.05"), "post.wgsl must sample the colour LUT and apply the 1.05 output scale");
        let l = fn_live::wgsl!("../shaders/color_lut.wgsl");
        assert!(l.contains("* (1.0 / 1.05)") && l.contains("round(stored * 1023.0) / 1023.0"), "color_lut.wgsl must store OutDeviceColor / 1.05 at 10 bits");
    }
    #[test]
    fn lut_round_trip_is_identity_below_white() {
        // CombineLUTs stores device / 1.05, ColorLookupTable multiplies by 1.05: mid grey must not be brightened (it was 123 / 255 with the missing / 1.05)
        let g = display_transform([0.18; 3], 1.0)[1];
        assert!((g * 255. - 116.8).abs() < 1.5, "{}", g * 255.);
    }
}
