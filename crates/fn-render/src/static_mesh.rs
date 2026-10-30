//! CPU side of the textured static-mesh path (real Fortnite 1.8 trees / rocks / other static props): mesh and material descriptions,
//! per-instance placement, LOD selection with hysteresis, distance fade, frustum tests and the mip-chain builders (sRGB-correct,
//! alpha-coverage preserving for masked foliage). No GPU objects here; `statics.rs` owns those.
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use image::RgbaImage;

/// Vertex of a static mesh (36 bytes): position, normal, UV0 and the vertex colour (linear UNORM8; foliage "wind"/AO data).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct StaticVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

/// One draw range of a LOD with one material. `material` indexes the handle slice given to `Renderer::create_static_mesh`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticSection { pub first_index: u32, pub index_count: u32, pub material: usize }

/// One level of detail with its own vertex / index data.
#[derive(Clone, Debug, Default)]
pub struct StaticLod {
    pub vertices: Vec<StaticVertex>,
    pub indices: Vec<u32>,
    pub sections: Vec<StaticSection>,
    /// Camera distance (metres) up to which this LOD is used; the next LOD takes over beyond it (ignored for the last LOD).
    pub max_distance: f32,
}

/// A static mesh: LOD list (index 0 = most detailed) plus cull parameters. Metres, Y up, origin at the prop's base.
#[derive(Clone, Debug, Default)]
pub struct StaticMeshData {
    pub name: String,
    pub lods: Vec<StaticLod>,
    /// Distance beyond which instances are not drawn (0 = never culled by distance). Usually `props.json` `cull_distance`.
    pub cull_distance: f32,
    /// Length of the dithered fade-out before `cull_distance` (0 = 10 % of it).
    pub fade_range: f32,
    /// False = the mesh does not cast shadows.
    pub cast_shadow: bool,
    /// FORTGLOBALS CULL CURVE: an Athena BuildingActor prop: with `cull_distance` 0 the run-time `ABuildingActor` cull distance applies
    /// (`quality::fort_cull`, from each instance's bounds sphere radius) instead of "never culled".
    pub fort_default_cull: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode { Opaque, Masked }

/// Material of a static mesh. Which fields come from the exported `.mat.json` and which are ASSUMED is documented in the crate README.
#[derive(Clone, Debug)]
pub struct StaticMaterialData {
    pub name: String,
    /// Base colour, sRGB 8 bit. The alpha channel is the opacity mask (merge a separate opacity texture into it before calling).
    pub base_color: Option<RgbaImage>,
    /// Tangent-space normal map (UE convention: DirectX green, z reconstructed from rg), linear.
    pub normal: Option<RgbaImage>,
    pub alpha_mode: AlphaMode,
    /// Opacity mask clip value (UE default 0.3333; the `Masked` blend mode compares against 0.3333 unless `OpacityMaskClipValue` is serialized).
    pub alpha_cutoff: f32,
    /// Foliage / cards: normals face the viewer, shadow pipeline does not cull, back faces get transmitted light.
    pub two_sided: bool,
    /// Linear multiplier on the base colour.
    pub tint: [f32; 3],
    /// GBuffer Roughness.
    pub roughness: f32,
    /// GBuffer Specular (DefaultLit SpecularColor = 0.08 * Specular; 0 = no specular).
    pub specular: f32,
    /// ASSUMED: share of sun light a two-sided surface transmits to its back side (backlit leaves).
    pub translucency: f32,
    /// ASSUMED wind sway: lateral displacement of the highest vertex as a fraction of the mesh height (0 = rigid).
    pub sway: f32,
    /// ASSUMED high-frequency leaf flutter amplitude in metres (0 = off).
    pub flutter: f32,
    /// Use vertex colour .r as the bend weight instead of the height ramp (set when the exported wind vertex colour is real).
    pub wind_from_vertex_color: bool,
    /// Multiply the albedo by the vertex colour rgb (untextured / vertex-painted meshes).
    pub vertex_color_albedo: bool,
}
impl Default for StaticMaterialData {
    fn default() -> Self {
        StaticMaterialData {
            name: String::new(), base_color: None, normal: None, alpha_mode: AlphaMode::Opaque, alpha_cutoff: 0.333_3, two_sided: false, tint: [1.; 3],
            // UE's unconnected Specular / Roughness inputs compile to 0.5 / 0.5 (FMaterialAttributeDefinitionMap, MaterialShared.cpp:2894-2895); was 0.25 / 0.85
            roughness: 0.5, specular: 0.5, translucency: 0.35, sway: 0., flutter: 0., wind_from_vertex_color: false, vertex_color_albedo: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StaticMeshHandle(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StaticMaterialHandle(pub usize);

/// One placed static mesh.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticMeshInstance {
    pub mesh: StaticMeshHandle,
    /// World position of the mesh origin (metres, Y up).
    pub pos: Vec3,
    /// Rotation about +Y (radians).
    pub yaw: f32,
    /// Uniform scale.
    pub scale: f32,
    /// Remaining hit-shake time in seconds (`fn_combat::Harvestable::shake`): drives the old `prop_wobble` sway (x offset growing linearly with height).
    pub shake: f32,
    /// Multiplier on the material's wind sway (1 = as authored, 0 = rigid).
    pub sway: f32,
    /// MIRRORFIX: the mesh is mirrored in its local X before the yaw (a negative-determinant placement): UE reverses its culling in every pass
    /// (`ReverseCulling = IsLocalToWorldDeterminantNegative()`, StaticMeshRender.cpp:284; shadow depth: ShadowDepthRendering.cpp:686), see `statics.rs`.
    pub mirror_x: bool,
}
impl StaticMeshInstance {
    pub fn new(mesh: StaticMeshHandle, pos: Vec3) -> Self { StaticMeshInstance { mesh, pos, yaw: 0., scale: 1., shake: 0., sway: 1., mirror_x: false } }
    pub fn with_mirror_x(mut self, mirror_x: bool) -> Self { self.mirror_x = mirror_x; self }
    pub fn with_yaw(mut self, yaw: f32) -> Self { self.yaw = yaw; self }
    pub fn with_scale(mut self, scale: f32) -> Self { self.scale = scale; self }
    pub fn with_shake(mut self, shake: f32) -> Self { self.shake = shake; self }
    pub fn with_sway(mut self, sway: f32) -> Self { self.sway = sway; self }
}

/// GPU per-instance record (48 bytes, vertex buffer with instance step mode).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct InstanceGpu {
    /// xyz position, w uniform scale (negative = the instance is mirrored in its local X, `StaticMeshInstance::mirror_x`)
    pub pos_scale: [f32; 4],
    /// cos(yaw), sin(yaw), distance-fade (1 = opaque), shake amplitude at the mesh top (metres, world)
    pub rot_fade: [f32; 4],
    /// wind phase (radians), time (s), sway multiplier, 1 / mesh height (1/m)
    pub wind: [f32; 4],
}

/// Number of LODs the culler supports per mesh.
pub const MAX_LODS: usize = 4;
/// Relative hysteresis band around every LOD switch distance (+-).
pub const LOD_HYSTERESIS: f32 = 0.10;
/// Default length of the distance fade as a share of the cull distance.
pub const DEFAULT_FADE_SHARE: f32 = 0.10;

/// Picks the LOD for camera distance `d`. `limits[i]` is the distance where LOD `i` ends (len = lod count - 1, ascending). `prev` is the LOD
/// used last frame: going to a lower detail needs `d > limit * (1 + h)`, coming back needs `d < limit * (1 - h)`, so a camera hovering at the
/// switch distance does not flicker.
pub fn select_lod(limits: &[f32], d: f32, prev: usize, h: f32) -> usize {
    let mut lod = prev.min(limits.len());
    while lod < limits.len() && d > limits[lod] * (1. + h) { lod += 1; }
    while lod > 0 && d < limits[lod - 1] * (1. - h) { lod -= 1; }
    lod
}

/// Opacity of the dithered distance fade: 1 inside `cull - fade`, 0 at `cull` (and 0 beyond); `cull <= 0` = never faded.
pub fn distance_fade(d: f32, cull: f32, fade: f32) -> f32 {
    if cull <= 0. { return 1.; }
    let f = if fade > 0. { fade } else { cull * DEFAULT_FADE_SHARE };
    ((cull - d) / f.max(1e-3)).clamp(0., 1.)
}

/// World-space frustum planes (xyz normal pointing inward, w offset) of a view-projection matrix with wgpu depth 0..1.
pub fn frustum_planes(vp: &Mat4) -> [Vec4; 6] {
    let (r0, r1, r2, r3) = (vp.row(0), vp.row(1), vp.row(2), vp.row(3));
    let mut p = [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2];
    for pl in &mut p { *pl /= pl.truncate().length().max(1e-9); }
    p
}
/// Sphere vs the first `count` planes (inside or intersecting = true).
pub fn sphere_in_planes(planes: &[Vec4], count: usize, c: Vec3, r: f32) -> bool {
    planes.iter().take(count).all(|p| p.truncate().dot(c) + p.w >= -r)
}
/// Sphere vs the four side planes of an orthographic light box (the light's near/far planes are ignored: casters outside the depth range
/// still shadow the box, the cascade fit pads the range).
pub fn sphere_in_light_box(light_vp: &Mat4, c: Vec3, r: f32) -> bool {
    let p = frustum_planes(light_vp);
    sphere_in_planes(&p, 4, c, r)
}

/// Bounding sphere of LOD 0 and the mesh height (max y over the vertices, at least 0.1 m).
pub fn mesh_bounds(m: &StaticMeshData) -> (Vec3, f32, f32) {
    let Some(l) = m.lods.first() else { return (Vec3::ZERO, 1., 1.) };
    if l.vertices.is_empty() { return (Vec3::ZERO, 1., 1.); }
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for v in &l.vertices { let p = Vec3::from(v.pos); lo = lo.min(p); hi = hi.max(p); }
    let c = (lo + hi) * 0.5;
    let r = l.vertices.iter().map(|v| (Vec3::from(v.pos) - c).length()).fold(0., f32::max);
    (c, r.max(0.05), hi.y.max(0.1))
}

/// Stable per-instance wind phase from the position (so reordering instances does not change the animation).
pub fn wind_phase(pos: Vec3) -> f32 {
    let h = (pos.x * 12.9898 + pos.z * 78.233 + pos.y * 37.719).sin() * 43_758.545;
    (h - h.floor()) * std::f32::consts::TAU
}
/// `fn_combat::prop_wobble` amplitude in metres at the top of the mesh for a remaining shake time.
pub fn shake_amplitude(shake: f32) -> f32 { if shake > 0. { (shake * 60.).sin() * shake * 0.15 } else { 0. } }

// ---------------------------------------------------------------------------------------------------------------------------
// mip chains
// ---------------------------------------------------------------------------------------------------------------------------
fn srgb_lut() -> [f32; 256] {
    std::array::from_fn(|i| { let c = i as f32 / 255.; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } })
}
fn encode_srgb_exact(x: f32) -> u8 {
    let x = x.clamp(0., 1.);
    let c = if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1. / 2.4) - 0.055 };
    (c * 255. + 0.5) as u8
}
fn encode_thresholds() -> &'static [f32; 256] {
    static T: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [f32::INFINITY; 256];
        for k in 1..256usize {
            let (mut lo, mut hi) = (0u32, 1f32.to_bits());
            if (encode_srgb_exact(f32::from_bits(hi)) as usize) < k { continue; }
            while lo < hi { let mid = lo + (hi - lo) / 2; if (encode_srgb_exact(f32::from_bits(mid)) as usize) >= k { hi = mid; } else { lo = mid + 1; } }
            t[k] = f32::from_bits(lo);
        }
        t
    })
}
fn encode_srgb(x: f32) -> u8 {
    let x = x.clamp(0., 1.);
    let t = encode_thresholds();
    let (mut lo, mut hi) = (0usize, 255usize);
    while lo < hi { let mid = (lo + hi + 1) / 2; if x >= t[mid] { lo = mid; } else { hi = mid - 1; } }
    lo as u8
}

/// Share of texels with alpha >= `cutoff` (0..1).
pub fn coverage(img: &RgbaImage, cutoff: f32) -> f32 {
    let t = (cutoff * 255.).round().clamp(0., 255.) as u8;
    let n = (img.width() * img.height()).max(1) as f32;
    img.pixels().filter(|p| p[3] >= t).count() as f32 / n
}
/// Scales the alpha of `img` so that its coverage at `cutoff` matches `target` (binary search on the scale; the standard fix for foliage that
/// thins out in the mip chain).
pub fn preserve_coverage(img: &mut RgbaImage, cutoff: f32, target: f32) {
    let (mut lo, mut hi) = (0.25f32, 6.0f32);
    let at = |img: &RgbaImage, s: f32| -> f32 {
        let t = cutoff * 255.;
        let n = (img.width() * img.height()).max(1) as f32;
        img.pixels().filter(|p| p[3] as f32 * s >= t).count() as f32 / n
    };
    for _ in 0..14 { let mid = 0.5 * (lo + hi); if at(img, mid) < target { lo = mid } else { hi = mid } }
    let s = 0.5 * (lo + hi);
    for p in img.pixels_mut() { p[3] = (p[3] as f32 * s).round().clamp(0., 255.) as u8; }
}

/// Mip chain of an sRGB colour texture: the colour is filtered in LINEAR light (2x2 box, weighted by alpha so the garbage colour under
/// transparent texels never bleeds in) and re-encoded; for `masked` textures the alpha of every level is rescaled to keep the coverage of
/// level 0 at `cutoff`, so distant foliage keeps its density and the alpha test does not shimmer or thin out.
pub fn build_mips_srgb(img: &RgbaImage, masked: Option<f32>) -> Vec<RgbaImage> {
    let lut = srgb_lut();
    let mut levels = vec![img.clone()];
    let target = masked.map(|c| coverage(img, c));
    while levels.last().map_or(false, |l| l.width() > 1 || l.height() > 1) {
        let prev = levels.last().unwrap();
        let (w, h) = ((prev.width() / 2).max(1), (prev.height() / 2).max(1));
        let mut out = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (mut rgb, mut a, mut wsum) = ([0f32; 3], 0f32, 0f32);
                for dy in 0..2 {
                    for dx in 0..2 {
                        let p = prev.get_pixel((x * 2 + dx).min(prev.width() - 1), (y * 2 + dy).min(prev.height() - 1));
                        let wt = if masked.is_some() { p[3] as f32 / 255. + 1e-4 } else { 1. };
                        for c in 0..3 { rgb[c] += lut[p[c] as usize] * wt; }
                        a += p[3] as f32;
                        wsum += wt;
                    }
                }
                out.put_pixel(x, y, image::Rgba([encode_srgb(rgb[0] / wsum), encode_srgb(rgb[1] / wsum), encode_srgb(rgb[2] / wsum), (a / 4. + 0.5) as u8]));
            }
        }
        if let (Some(c), Some(t)) = (masked, target) { if t > 0. && t < 1. { preserve_coverage(&mut out, c, t); } }
        levels.push(out);
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_srgb_encode_matches_powf() {
        let t = encode_thresholds();
        let mut b = 0u32;
        while b <= 1f32.to_bits() { let x = f32::from_bits(b); assert_eq!(encode_srgb(x), encode_srgb_exact(x), "{x}"); b += 4099; }
        for k in 1..256 { let c = t[k].to_bits(); for d in [c.saturating_sub(2), c.saturating_sub(1), c, c + 1, c + 2] { let x = f32::from_bits(d); assert_eq!(encode_srgb(x), encode_srgb_exact(x), "{x}"); } }
        for x in [-1f32, 2., f32::NAN, 0., 1.] { assert_eq!(encode_srgb(x), encode_srgb_exact(x)); }
    }

    #[test]
    fn lod_selection_has_hysteresis() {
        let limits = [20., 50.];
        assert_eq!(select_lod(&limits, 10., 0, LOD_HYSTERESIS), 0);
        // just past the limit but inside the band: stay on LOD 0
        assert_eq!(select_lod(&limits, 21., 0, LOD_HYSTERESIS), 0);
        assert_eq!(select_lod(&limits, 23., 0, LOD_HYSTERESIS), 1);
        // coming back needs to cross 20 * 0.9
        assert_eq!(select_lod(&limits, 19., 1, LOD_HYSTERESIS), 1);
        assert_eq!(select_lod(&limits, 17., 1, LOD_HYSTERESIS), 0);
        // far jumps skip LODs, and a jump back skips them too
        assert_eq!(select_lod(&limits, 200., 0, LOD_HYSTERESIS), 2);
        assert_eq!(select_lod(&limits, 5., 2, LOD_HYSTERESIS), 0);
        // single LOD
        assert_eq!(select_lod(&[], 1e6, 0, LOD_HYSTERESIS), 0);
    }
    #[test]
    fn a_camera_oscillating_inside_the_band_never_flips() {
        let limits = [30.];
        let mut lod = 0;
        let mut flips = 0;
        for i in 0..200 { let d = 30. + 2. * (i as f32 * 0.7).sin(); let n = select_lod(&limits, d, lod, LOD_HYSTERESIS); if n != lod { flips += 1; } lod = n; }
        assert_eq!(flips, 0);
    }
    #[test]
    fn distance_fade_ramps_to_zero_at_the_cull_distance() {
        assert_eq!(distance_fade(10., 100., 0.), 1.);
        assert_eq!(distance_fade(89., 100., 0.), 1.);
        assert!((distance_fade(95., 100., 0.) - 0.5).abs() < 1e-5);
        assert_eq!(distance_fade(100., 100., 0.), 0.);
        assert_eq!(distance_fade(500., 100., 20.), 0.);
        assert_eq!(distance_fade(1e6, 0., 0.), 1.);
        assert!((distance_fade(90., 100., 20.) - 0.5).abs() < 1e-5);
    }
    #[test]
    fn frustum_culls_behind_and_beside_but_keeps_what_is_in_view() {
        let view = Mat4::look_to_rh(Vec3::ZERO, -Vec3::Z, Vec3::Y);
        let vp = Mat4::perspective_rh(1.0, 16. / 9., 0.1, 200.) * view;
        let pl = frustum_planes(&vp);
        assert!(sphere_in_planes(&pl, 6, Vec3::new(0., 0., -10.), 1.));
        assert!(!sphere_in_planes(&pl, 6, Vec3::new(0., 0., 10.), 1.), "behind");
        assert!(!sphere_in_planes(&pl, 6, Vec3::new(100., 0., -10.), 1.), "far to the side");
        assert!(sphere_in_planes(&pl, 6, Vec3::new(100., 0., -10.), 200.), "big sphere intersects");
        assert!(!sphere_in_planes(&pl, 6, Vec3::new(0., 0., -300.), 1.), "beyond far");
    }
    #[test]
    fn light_box_test_ignores_depth_but_not_the_sides() {
        let vp = Mat4::orthographic_rh(-10., 10., -10., 10., 0., 50.) * Mat4::look_at_rh(Vec3::new(0., 0., 30.), Vec3::ZERO, Vec3::Y);
        assert!(sphere_in_light_box(&vp, Vec3::new(5., 0., -500.), 1.), "far behind the box along the light still casts into it");
        assert!(!sphere_in_light_box(&vp, Vec3::new(40., 0., 0.), 1.));
    }
    #[test]
    fn mesh_bounds_and_height() {
        let mut m = StaticMeshData::default();
        m.lods.push(StaticLod { vertices: vec![StaticVertex { pos: [-1., 0., 0.], ..Default::default() }, StaticVertex { pos: [1., 6., 0.], ..Default::default() }], ..Default::default() });
        let (c, r, h) = mesh_bounds(&m);
        assert!((c - Vec3::new(0., 3., 0.)).length() < 1e-6);
        assert!((r - (1f32 + 9.).sqrt()).abs() < 1e-5);
        assert_eq!(h, 6.);
    }
    #[test]
    fn wind_phase_is_stable_and_spread() {
        assert_eq!(wind_phase(Vec3::new(1., 0., 2.)), wind_phase(Vec3::new(1., 0., 2.)));
        assert_ne!(wind_phase(Vec3::new(1., 0., 2.)), wind_phase(Vec3::new(1.5, 0., 2.)));
        assert!((0.0..std::f32::consts::TAU).contains(&wind_phase(Vec3::new(-7., 0., 31.))));
    }
    #[test]
    fn shake_matches_the_combat_wobble() {
        for s in [0., 0.05, 0.2, 0.3] {
            let h = fn_combat::Harvestable { shake: s, ..fn_combat::Harvestable::new(fn_combat::Kind::Tree, Vec3::ZERO) };
            assert!((shake_amplitude(s) - fn_combat::prop_wobble(&h)).abs() < 1e-7);
        }
    }
    /// Leaf-like alpha (random blobs): a plain box-filtered alpha loses coverage in the coarse mips, the coverage-preserving chain keeps the
    /// share of texels above the cutoff of level 0 (within quantisation) so distant foliage keeps its density.
    #[test]
    fn masked_mips_keep_their_alpha_coverage() {
        let img = crate::synthetic::leaf_texture(5);
        let target = coverage(&img, 0.5);
        let mips = build_mips_srgb(&img, Some(0.5));
        let naive = build_mips_srgb(&img, None);
        assert_eq!(mips.len(), 9);
        let (mut err_keep, mut err_naive) = (0f32, 0f32);
        for i in 1..6 {
            let (c, n) = (coverage(&mips[i], 0.5), coverage(&naive[i], 0.5));
            assert!((c - target).abs() < 0.06, "mip {i}: coverage {c} vs {target}");
            err_keep += (c - target).abs();
            err_naive += (n - target).abs();
        }
        assert!(err_naive > 2. * err_keep, "naive {err_naive} vs preserved {err_keep}");
    }
    #[test]
    fn srgb_mips_average_in_linear_light() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 0, 255]));
        let m = build_mips_srgb(&img, None);
        // linear 0.5 encodes to sRGB ~188, not 128
        assert!((m[1].get_pixel(0, 0)[0] as i32 - 188).abs() <= 1, "{}", m[1].get_pixel(0, 0)[0]);
    }
    #[test]
    fn transparent_texel_colour_does_not_bleed_into_masked_mips() {
        let mut img = RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([0, 200, 0, 255]));
        for (x, y) in [(1, 0), (0, 1), (1, 1)] { img.put_pixel(x, y, image::Rgba([255, 0, 255, 0])); }
        let m = build_mips_srgb(&img, Some(0.3));
        let p = m[1].get_pixel(0, 0);
        assert!(p[1] > 190 && p[0] < 40 && p[2] < 40, "bled colour {p:?}");
    }
}
