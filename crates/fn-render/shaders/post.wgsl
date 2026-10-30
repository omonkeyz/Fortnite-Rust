// Fullscreen post-process: bloom, exposure, vignette, white balance, UE 4.16 filmic tonemapper, sRGB encode.
// Port of post.frag. Order follows UE4's tonemapper (bloom added to scene colour, exposure, vignette, then the colour-grading
// stage = white balance + ACES film path), every stage is an APPROXIMATION unless marked RECOVERED.
// Also contains the HDR mip downsample (bloom + exposure metering) and the exposure-adaptation compute shader.
// Common helpers (srgb) are prepended from common.wgsl.

struct PostParams {
    wb0: vec4<f32>, wb1: vec4<f32>, wb2: vec4<f32>,   // white balance matrix columns (linear sRGB -> linear sRGB)
    film_stw: vec4<f32>,       // slope, toe, shoulder
    film_clip: vec4<f32>,      // black clip, white clip
    bloom: vec4<f32>,          // intensity, threshold, enabled
    vignette: vec4<f32>,       // intensity, enabled
    ao: vec4<f32>,             // legacy block (intensity, radius m, enabled, bias): unused since the UE 4.16 SSAO (ssao_ue.rs) has its own uniform
    proj: vec4<f32>,           // tan(half fov x), tan(half fov y), near, far
    view: vec4<f32>,           // width, height, display_exponent, debug_mode
    backdrop: vec4<f32>,       // pause-menu backdrop: dim (0..1), blur (0..1)
    grade_sat: vec4<f32>,      // colour-grading saturation per region (shadows, midtones, highlights) = region * global ColorSaturation
    grade_contrast: vec4<f32>, // ColorContrast per region
    grade_gamma: vec4<f32>,    // ColorGamma per region
    grade_range: vec4<f32>,    // ColorCorrectionShadowsMax, ColorCorrectionHighlightsMin, 1 = contrast/gamma not identity, LUT output scale (1.05)
    // SCALABILITY: x = r.Tonemapper.Sharpen / 6 (TonemapperParams.y, PostProcessTonemap.cpp:950-953), y = 1: grain quantization
    // (r.Tonemapper.GrainQuantization > 0, PostProcessTonemap.cpp:609-617), zw = GrainRandomFull.xy (GrainRandomFromFrame, PostProcessTonemap.h:34-38)
    tonemap_ext: vec4<f32>,
    // POSTFX SETTINGS (effect_settings.rs): scene colour fringe, xy = FringeUVParams (.x scales the G sample position, .y the R one about the view
    // centre), z = 1 when USE_COLOR_FRINGE (SceneFringeIntensity > 0.01 after r.SceneColorFringeQuality, PostProcessTonemap.cpp:620)
    fringe: vec4<f32>,
    scene_tint: vec4<f32>,
}
struct ExposureState { ev: f32, target_ev: f32, exposure: f32, avg_luminance: f32 }

@group(0) @binding(0) var<uniform> pp: PostParams;
@group(0) @binding(1) var scene: texture_2d<f32>;
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var<storage, read> exposure_state: ExposureState;
@group(0) @binding(4) var lin_samp: sampler;
// BLOOM HOOK: the bloom image of fn_postfx::bloom (UE 4.16 chain: SceneColorHalfRes -> 5 downsamples -> Bloom6..1 Gaussians, tint / 6),
// half resolution, NOT exposed, NOT multiplied by BloomIntensity (PostprocessInput1 of FPostProcessTonemapPS).
@group(0) @binding(5) var bloom_tex: texture_2d<f32>;
// COLOR LUT (GLOBAL LOOK): the 32^3 colour-grading LUT of FCombineLUTs (color_lut.rs / color_lut.wgsl, rebuilt every frame from `pp`), = OutDeviceColor / 1.05
// rounded to 10 bits. Binding 10 because ssao_apply.wgsl (which also includes this file) owns 6..9.
@group(0) @binding(10) var color_lut: texture_3d<f32>;

struct VsOut { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> }   // uv: GL convention (y up)

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    var o: VsOut;
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32((i >> 1u) & 1u) * 4 - 1);
    // full-screen triangle: (-1,-1), (3,-1), (-1,3)
    o.clip = vec4<f32>(x, y, 0.0, 1.0);
    o.uv = vec2<f32>(x, y) * 0.5 + 0.5;
    return o;
}

fn tc(p: vec2<f32>) -> vec2<f32> { return vec2<f32>(p.x, 1.0 - p.y); }   // GL uv -> texture coordinate

// ---- AO: the screen-space AO is UE 4.16's (ssao_ue.rs / ssao_ue.wgsl), applied by ssao_apply.wgsl before fog / translucency ----

// ---- Bloom (BLOOM HOOK): the chain runs in fn_postfx::bloom; the tonemapper only samples it (bilinear, screen uv) and scales it by
// ColorScale1 = BloomIntensity (+ BloomDirtMask tint * mask; the Athena volumes set no BloomDirtMask texture -> BlackDummy -> 0). ----
fn bloom_color(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(bloom_tex, lin_samp, tc(uv), 0.0).rgb * pp.bloom.x;
}

// ---- Filmic tonemapper curve (UE4 FilmToneMap structure: log-domain toe/straight/shoulder blend) ----
fn film_curve(untonemapped: vec3<f32>) -> vec3<f32> {
    let slope = pp.film_stw.x;
    let toe = pp.film_stw.y;
    let shoulder = pp.film_stw.z;
    let black = pp.film_clip.x;
    let white = pp.film_clip.y;
    let toe_scale = 1.0 + black - toe;
    let shoulder_scale = 1.0 + white - shoulder;
    let in_match = 0.18;
    let out_match = 0.18;
    var toe_match: f32;
    if (toe > 0.8) { toe_match = (1.0 - toe - out_match) / slope + log(in_match) / 2.302585093; }
    else {
        let bt = (out_match + black) / toe_scale - 1.0;
        toe_match = log(in_match) / 2.302585093 - 0.5 * log((1.0 + bt) / (1.0 - bt)) * (toe_scale / slope);
    }
    let straight_match = (1.0 - toe) / slope - toe_match;
    let shoulder_match = shoulder / slope - straight_match;
    let log_color = log(max(untonemapped, vec3<f32>(1e-10))) / 2.302585093;
    let straight = slope * (log_color + straight_match);
    var toe_color = -black + 2.0 * toe_scale / (1.0 + exp(-2.0 * (slope / toe_scale) * (log_color - toe_match)));
    var shoulder_color = (1.0 + white) - 2.0 * shoulder_scale / (1.0 + exp(2.0 * (slope / shoulder_scale) * (log_color - shoulder_match)));
    toe_color = select(straight, toe_color, log_color < vec3<f32>(toe_match));
    shoulder_color = select(straight, shoulder_color, log_color > vec3<f32>(shoulder_match));
    var t = clamp((log_color - toe_match) / (shoulder_match - toe_match), vec3<f32>(0.0), vec3<f32>(1.0));
    if (shoulder_match < toe_match) { t = 1.0 - t; }
    t = (3.0 - 2.0 * t) * t * t;
    return mix(toe_color, shoulder_color, t);
}
// ACES matrices exactly as compiled into the UE 4.16 CombineLUTs shader (WGSL mat3x3 is column-major, so each constructor lists
// the transposed rows of the HLSL dp3 constants). They include the D65 <-> D60 adaptation.
const SRGB_TO_AP0 = mat3x3<f32>(0.439701, 0.089792, 0.017544, 0.382978, 0.813423, 0.111544, 0.177335, 0.096762, 0.870704);
const AP0_TO_AP1 = mat3x3<f32>(1.451439, -0.076554, 0.008316, -0.236511, 1.176230, -0.006032, -0.214929, -0.099676, 0.997716);
const AP1_TO_SRGB = mat3x3<f32>(1.705052, -0.130257, -0.024003, -0.621791, 1.140803, -0.128969, -0.083258, -0.010549, 1.152972);
const AP1_LUMA = vec3<f32>(0.2722287168, 0.6740817658, 0.0536895174);
const RRT_SAT_FACTOR: f32 = 0.96;   // RECOVERED: 0.96 / 0.93 are literals in the 4.16 film path
const ODT_SAT_FACTOR: f32 = 0.93;
// LUT domain: LogToLin(0) .. LogToLin(1), LogToLin(x) = 0.18 * 2^((x - 0.434018) * 14). RECOVERED.
const LUT_MIN: f32 = 0.0026677086;
const LUT_MAX: f32 = 43.707738;

// CombineLUTs colour correction, RECOVERED from the 4.16 CombineLUTs DXBC (shader 1182809): sRGB -> AP1, AP1 luma, shadows / midtones /
// highlights regions (weights 1 - smoothstep(luma / ShadowsMax), smoothstep((luma - HighlightsMin) / (1 - HighlightsMin)), remainder),
// each max(luma + sat * (c - luma), 0). ColorSaturationShadows = Midtones = 1.25 (TODM_A), Highlights 1.0, ShadowsMax 0.09 and
// HighlightsMin 0.5 (exe ctor defaults); contrast/gamma/gain/offset stay identity. Mirrors tonemap::color_grade.
const SRGB_TO_AP1 = mat3x3<f32>(0.613191, 0.070207, 0.020619, 0.339512, 0.916336, 0.109567, 0.047366, 0.013450, 0.869607);
fn grade_region(ap1: vec3<f32>, luma: f32, sat: f32, contrast: f32, gamma: f32) -> vec3<f32> {
    var w = max(vec3<f32>(luma) + (ap1 - vec3<f32>(luma)) * sat, vec3<f32>(0.0));
    if (pp.grade_range.z > 0.5) {
        w = pow(w * (1.0 / 0.18), vec3<f32>(contrast)) * 0.18;
        w = pow(w, vec3<f32>(1.0 / gamma));
    }
    return w;
}
// ColorCorrectAll of the 4.16 CombineLUTs: three regions with their own saturation / contrast / gamma (gain 1, offset 0 in every phase).
// Per-phase values come from tonemap::PostSettings (Evening = sunset: shadows 1.5, midtones 1.15).
fn color_grade(c: vec3<f32>) -> vec3<f32> {
    let ap1 = SRGB_TO_AP1 * c;
    let luma = dot(ap1, AP1_LUMA);
    let xs = clamp(luma / pp.grade_range.x, 0.0, 1.0);
    let xh = clamp((luma - pp.grade_range.y) / (1.0 - pp.grade_range.y), 0.0, 1.0);
    let sw = 1.0 - xs * xs * (3.0 - 2.0 * xs);
    let hw = xh * xh * (3.0 - 2.0 * xh);
    let mw = 1.0 - sw - hw;
    let shadows = grade_region(ap1, luma, pp.grade_sat.x, pp.grade_contrast.x, pp.grade_gamma.x);
    let mids = grade_region(ap1, luma, pp.grade_sat.y, pp.grade_contrast.y, pp.grade_gamma.y);
    let highs = grade_region(ap1, luma, pp.grade_sat.z, pp.grade_contrast.z, pp.grade_gamma.z);
    return AP1_TO_SRGB * (shadows * sw + mids * mw + highs * hw);
}
// ACES RRT hue-dependent red modifier (RECOVERED, 4.16): pivot 0.03, scale 0.82, hue 0, width 135.
fn aces_red_modifier(aces_in: vec3<f32>) -> vec3<f32> {
    var aces = aces_in;
    let minrgb = min(aces.r, min(aces.g, aces.b));
    let maxrgb = max(aces.r, max(aces.g, aces.b));
    let saturation = (max(maxrgb, 1e-10) - max(minrgb, 1e-10)) / max(maxrgb, 1e-2);
    var hue = 0.0;
    if (!(aces.r == aces.g && aces.g == aces.b)) {
        hue = degrees(atan2(1.7320508 * (aces.g - aces.b), 2.0 * aces.r - aces.g - aces.b));
        if (hue < 0.0) { hue = hue + 360.0; }
    }
    let centered = select(hue, hue - 360.0, hue > 180.0);
    let w = smoothstep(0.0, 1.0, 1.0 - abs(2.0 * centered / 135.0));
    aces.r = aces.r + w * w * saturation * (0.03 - aces.r) * (1.0 - 0.82);
    return aces;
}
// COLOR LUT (GLOBAL LOOK): CombineLUTsCommon of the 1.8 DXBC (GlobalShaderCache#1182809) for one LUT-domain linear colour, up to OutDeviceColor (the
// caller stores it / 1.05): WhiteBalance, ColorGrade (ColorCorrectAll in AP1), FilmToneMap (sRGB -> AP0, ACES red modifier, AP0 -> AP1 clamped, RRT sat
// 0.96, Film* curve, ODT sat 0.93), AP1 -> sRGB clamped, ColorCorrection (MappingPolynomial: r.Color.Min/Mid/Max not in the cooked ini -> 0 / 0.5 / 1 =
// identity), lerp(FilmColor * ColorScale, OverlayColor, a) (no fade: identity), pow(max(0, x), InverseGamma.y) (y = 2.2 / DisplayGamma = view.z),
// LinearToSrgb (OutputDevice 0: r.HDR.Display.OutputDevice 0 and r.TonemapperGamma 0 are not in the cooked ini).
fn lut_device_color(linear: vec3<f32>) -> vec3<f32> {
    let wb = mat3x3<f32>(pp.wb0.xyz, pp.wb1.xyz, pp.wb2.xyz);
    let graded = color_grade(wb * linear);
    var ap0 = SRGB_TO_AP0 * graded;
    ap0 = aces_red_modifier(ap0);
    var ap1 = max(AP0_TO_AP1 * ap0, vec3<f32>(0.0));
    ap1 = mix(vec3<f32>(dot(ap1, AP1_LUMA)), ap1, RRT_SAT_FACTOR);
    var toned = film_curve(ap1);
    toned = max(mix(vec3<f32>(dot(toned, AP1_LUMA)), toned, ODT_SAT_FACTOR), vec3<f32>(0.0));
    let film = max(AP1_TO_SRGB * toned, vec3<f32>(0.0));
    return to_srgb(pow(film, vec3<f32>(pp.view.z)));
}
// ColorLookupTable (PostProcessTonemap.usf:68, 1.8 DXBC FPostProcessTonemapPS0 #1070630): LUTEncodedColor = LinToLog(c) = saturate(log2(c) / 14 + 0.610727)
// (no `+ LogToLin(0)` in the 1.8 build), UVW = enc * 31/32 + 0.5/32, trilinear clamp sample, * 1.05. log2 of 0 / negative (NaN) saturates to 0 like D3D.
fn color_lookup_table(c: vec3<f32>) -> vec3<f32> {
    let enc = clamp(log2(max(c, vec3<f32>(0.0))) * (1.0 / 14.0) + vec3<f32>(0.610727), vec3<f32>(0.0), vec3<f32>(1.0));
    let uvw = enc * (31.0 / 32.0) + vec3<f32>(0.5 / 32.0);
    return textureSampleLevel(color_lut, lin_samp, uvw, 0.0).rgb * 1.05;
}
// UE ComputeVignetteMask, RECOVERED from the 4.16 tonemap VS/PS: natural cos^4 vignette, circle-space position normalised so the
// screen corner sits at radius sqrt(2) for every aspect: p = sqrt2/sqrt(1+a^2) * (ndc.x, a*ndc.y), a = height/width; mask = 1/(1+|p I|^2)^2.
fn vignette_mask(uv: vec2<f32>) -> f32 {
    let a = pp.view.y / pp.view.x;
    let p = (uv * 2.0 - 1.0) * vec2<f32>(1.0, a) * (1.4142135624 / sqrt(1.0 + a * a)) * pp.vignette.x;
    let t = 1.0 / (1.0 + dot(p, p));
    return t * t;
}

// Pause-menu backdrop blur: trilinear taps of the HDR mip chain (box-filtered), 3x3 kernel at lod = blur * 5.
fn blurred_scene(uv: vec2<f32>) -> vec4<f32> {
    let lod = clamp(pp.backdrop.y, 0.0, 1.0) * 5.0;
    let texel = 1.0 / vec2<f32>(textureDimensions(scene, i32(floor(lod))));
    var acc = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let w = select(1.0, 2.0, x == 0) * select(1.0, 2.0, y == 0);
            acc = acc + textureSampleLevel(scene, lin_samp, tc(uv + vec2<f32>(f32(x), f32(y)) * texel * 1.5), lod) * w;
            wsum = wsum + w;
        }
    }
    return acc / wsum;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let debug_mode = i32(pp.view.w);
    let exposure = exposure_state.exposure;
    var scene_sample = textureSampleLevel(scene, lin_samp, tc(uv), 0.0);
    // USE_COLOR_FRINGE (PostProcessTonemap.usf:155-159 / :332-341): FringeUV = ScreenPosToUV(ScreenPos * FringeUVParams.{r,g}); R from FringeUV.zw (scale
    // .g = 1/(1+Offset*ScaleR)), G from FringeUV.xy (scale .r), B from the unshifted UV (bilinear clamp sampler, no grain jitter: GrainJitter 0)
    if (pp.fringe.z > 0.5) {
        let sp = uv * 2.0 - 1.0;
        scene_sample.r = textureSampleLevel(scene, lin_samp, tc(sp * pp.fringe.y * 0.5 + 0.5), 0.0).r;
        scene_sample.g = textureSampleLevel(scene, lin_samp, tc(sp * pp.fringe.x * 0.5 + 0.5), 0.0).g;
    }
    let blurred = pp.backdrop.y > 0.0;
    if (blurred) { scene_sample = blurred_scene(uv); }
    var c = scene_sample.rgb;
    // USE_SHARPEN (PostProcessTonemap.usf:357-412): Luminance (0.3, 0.59, 0.11) of the centre and of 4 neighbours (-1,0) (1,0) (-1,-1) (-1,1),
    // EdgeMask = saturate(1 - ExposureScale * max|A0 - L|), SceneColor += ((C1 + C2 + C3 + C4) - 4 SceneColor) * (-EdgeMask * Sharpen / 6)
    if (pp.tonemap_ext.x > 0.0 && !blurred) {
        let texel = 1.0 / pp.view.xy;
        let c1 = textureSampleLevel(scene, lin_samp, tc(uv) + vec2<f32>(-1.0, 0.0) * texel, 0.0).rgb;
        let c2 = textureSampleLevel(scene, lin_samp, tc(uv) + vec2<f32>(1.0, 0.0) * texel, 0.0).rgb;
        let c3 = textureSampleLevel(scene, lin_samp, tc(uv) + vec2<f32>(-1.0, -1.0) * texel, 0.0).rgb;
        let c4 = textureSampleLevel(scene, lin_samp, tc(uv) + vec2<f32>(-1.0, 1.0) * texel, 0.0).rgb;
        let lw = vec3<f32>(0.3, 0.59, 0.11);
        let a0 = dot(c, lw);
        let ln = vec4<f32>(dot(c1, lw), dot(c2, lw), dot(c3, lw), dot(c4, lw));
        let d = abs(vec4<f32>(a0) - ln);
        let hdr_edge = exposure_state.exposure * max(max(d.x, d.y), max(d.z, d.w));
        let edge_mask = clamp(1.0 - hdr_edge, 0.0, 1.0);
        let lerp_factor = -edge_mask * pp.tonemap_ext.x;
        c = c + ((c1 + c2 + c3 + c4) - c * 4.0) * lerp_factor;
    }
    // AO ORDER: the screen-space AO is no longer applied here; ssao_apply.wgsl (ssao_pass.rs) applies it to the HDR scene right after the opaque
    // scene, before fog / translucency / bloom / the histogram, matching UE 4.16 where SSAO only acts inside the lighting.
    let raw_scene = scene_sample.rgb;
    var bloom = vec3<f32>(0.0);
    if (pp.bloom.z > 0.5 && !blurred) { bloom = bloom_color(uv); }
    // PostProcessTonemap.usf: LinearColor = SceneColor * ColorScale0 + CombinedBloom * ColorScale1; LinearColor *= ExposureScale
    c = (c * pp.scene_tint.rgb + bloom) * exposure;
    if (pp.vignette.y > 0.5) { c = c * vignette_mask(uv); }
    if (debug_mode == 5) { return vec4<f32>(vec3<f32>(vignette_mask(uv)), 1.0); }
    if (debug_mode == 2) { return vec4<f32>(clamp(raw_scene, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0); }   // the AO pass wrote the raw AO term in this mode
    if (debug_mode == 3) { return vec4<f32>(to_srgb(clamp(bloom * 4.0, vec3<f32>(0.0), vec3<f32>(1.0))), 1.0); }
    if (debug_mode == 4) { return vec4<f32>(to_srgb(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0))), 1.0); }
    // The tonemap pass samples the 32^3 colour-grading LUT with saturate(log2(c)/14 + 0.610727): input clamped to the LUT domain
    // before white balance (RECOVERED). Direct evaluation replaces the trilinear 32^3 interpolation; the grain dither is applied below when the level enables it.
    if (debug_mode == 1) {
        let wb = mat3x3<f32>(pp.wb0.xyz, pp.wb1.xyz, pp.wb2.xyz);
        return vec4<f32>(to_srgb(clamp(color_grade(wb * clamp(c, vec3<f32>(LUT_MIN), vec3<f32>(LUT_MAX))), vec3<f32>(0.0), vec3<f32>(1.0))), 1.0);
    }
    // COLOR LUT (GLOBAL LOOK): the colour transform (white balance, grading, ACES film path, display gamma, sRGB) comes from the 32^3 LUT that
    // color_lut.wgsl bakes from the same `pp` block (OutDeviceColor / 1.05, 10-bit), sampled trilinearly and * 1.05 (ColorLookupTable). The old direct
    // evaluation lacked CombineLUTs' / 1.05 and came out 5 % brighter (mid grey 123 instead of 117 / 255, highlights clipped at 0.952).
    var out_c = color_lookup_table(c);
    // USE_GRAIN_QUANTIZATION (PostProcessTonemap.usf:474-481): Grain = frac(sin(GrainUV.x + GrainUV.y * 543.31) * 493013) with GrainUV = TexCoord +
    // GrainRandomFull.xy; OutDeviceColor += Grain / 256 - 0.5 / 256 (after the LUT, before the 8-bit write: DXBC `mad r0.xyz, lut, 1.05, grain`)
    if (pp.tonemap_ext.y > 0.5) {
        let guv = tc(uv) + pp.tonemap_ext.zw;
        let grain = fract(sin(guv.x + guv.y * 543.31) * 493013.0);
        out_c = out_c + (grain * (1.0 / 256.0) - 0.5 / 256.0);
    }
    out_c = clamp(out_c, vec3<f32>(0.0), vec3<f32>(1.0));   // the unorm back buffer write
    return vec4<f32>(out_c * (1.0 - clamp(pp.backdrop.x, 0.0, 1.0)), 1.0);
}
