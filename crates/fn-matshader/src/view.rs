//! The `View` uniform buffer the original base-pass programs read, filled in Unreal conventions (centimetres, left handed, Z up, row-vector matrices, translated world =
//! world + PreViewTranslation, PreViewTranslation = -eye) from the port's camera (metres, right handed, Y up).
//!
//! Member slots (float4 index in the buffer), each one fixed by what the recovered gun programs do with it:
//! * 0..3 `TranslatedWorldToClip` (VS: final `add o5 = translated * rows + row3`), 40..43 `SVPositionToTranslatedWorld` (PS: the view vector from `SV_Position`),
//!   62 `PreViewTranslation`, 120 `ViewRectMin`, 121 `ViewSizeAndInvSize`, 131 time (PS `.z` scrolls the flicker noise; VS `.w` feeds the primitive fade).
//! * 83..86 `PrevTranslatedWorldToClip`, 105 `PrevPreViewTranslation`: only the VELOCITY output reads them; set to the current matrices (no motion).
//! * 155 `SkyLightColor`, 156..158 the SH red / green / blue rows `(x, y, z, constant)`, 159..161 the quadratic terms, 162 `SHC`: the PS evaluates
//!   `max(0, SH(n)) * SkyLightColor`. They are filled so the result equals the renderer's sky ambient (`sky_radiance * (0.5 + 0.5 n.y)`, exactly the L0 + L1 SH of
//!   that hemisphere term, the quadratic terms are zero).
//! Depth convention: wgpu standard-Z (near -> 0, far -> 1, `clip.w` = view depth) as in `fn_render::camera`.
use glam::{DMat4, DVec3, DVec4, Vec3};

#[derive(Clone, Copy, Debug)]
pub struct ViewInput {
    pub eye: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
    /// tan of the half field of view (x, y).
    pub tan_half: (f32, f32),
    pub near_m: f32,
    pub far_m: f32,
    pub width: u32,
    pub height: u32,
    pub time: f32,
}

pub const VIEW_FLOAT4S: usize = 178;
pub mod slot {
    pub const TRANSLATED_WORLD_TO_CLIP: usize = 0;
    pub const WORLD_TO_CLIP: usize = 4;
    pub const SV_POSITION_TO_TRANSLATED_WORLD: usize = 40;
    pub const VIEW_FORWARD: usize = 52;
    pub const INV_DEVICE_Z_TO_WORLD_Z: usize = 57;
    pub const SCREEN_POSITION_SCALE_BIAS: usize = 58;
    pub const VIEW_ORIGIN: usize = 60;
    pub const PRE_VIEW_TRANSLATION: usize = 62;
    pub const PREV_TRANSLATED_WORLD_TO_CLIP: usize = 83;
    pub const PREV_PRE_VIEW_TRANSLATION: usize = 105;
    pub const VIEW_RECT_MIN: usize = 120;
    pub const VIEW_SIZE_AND_INV_SIZE: usize = 121;
    /// `DiffuseOverrideParameter` .. `RoughnessOverrideParameter` (125..127, 128.xy; UE4.16 SceneView.h:604-607).
    pub const DIFFUSE_OVERRIDE_PARAMETER: usize = 125;
    pub const SPECULAR_OVERRIDE_PARAMETER: usize = 126;
    pub const NORMAL_OVERRIDE_PARAMETER: usize = 127;
    pub const ROUGHNESS_OVERRIDE_PARAMETER: usize = 128;
    pub const TIME: usize = 131;
    pub const SKY_LIGHT_COLOR: usize = 155;
    pub const SKY_IRRADIANCE_SH: usize = 156;
}

/// Port world (metres, Y up) -> Unreal world (centimetres, Z up): `(x, y, z) -> (x, z, y) * 100`.
pub fn to_ue(p: Vec3) -> DVec3 { DVec3::new(p.x as f64, p.z as f64, p.y as f64) * 100.0 }
pub fn dir_to_ue(d: Vec3) -> DVec3 { DVec3::new(d.x as f64, d.z as f64, d.y as f64) }

type Rows = [[f64; 4]; 4];
fn to_rows(m: &DMat4) -> Rows { std::array::from_fn(|i| { let r = m.row(i); [r.x, r.y, r.z, r.w] }) }
fn mul_rows(a: &Rows, b: &Rows) -> Rows { std::array::from_fn(|i| std::array::from_fn(|j| (0..4).map(|k| a[i][k] * b[k][j]).sum())) }
fn invert_rows(m: &Rows) -> Rows {
    let mut a = *m;
    let mut inv: Rows = std::array::from_fn(|i| std::array::from_fn(|j| if i == j { 1.0 } else { 0.0 }));
    for c in 0..4 {
        let p = (c..4).max_by(|&x, &y| a[x][c].abs().partial_cmp(&a[y][c].abs()).unwrap()).unwrap();
        a.swap(c, p); inv.swap(c, p);
        let d = a[c][c];
        for j in 0..4 { a[c][j] /= d; inv[c][j] /= d; }
        for r in 0..4 { if r != c { let f = a[r][c]; for j in 0..4 { a[r][j] -= f * a[c][j]; inv[r][j] -= f * inv[c][j]; } } }
    }
    inv
}

impl ViewInput {
    /// The 178 float4 slots of the `View` uniform buffer; `sky` = the renderer's sky light radiance (linear RGB).
    pub fn view_buffer(&self, sky: [f32; 3]) -> Vec<[f32; 4]> {
        let eye = to_ue(self.eye);
        let fwd = dir_to_ue(self.forward).normalize();
        let up0 = dir_to_ue(self.up).normalize();
        // UE: right = up x forward; re-orthogonalise the up vector
        let right = up0.cross(fwd).normalize();
        let up = fwd.cross(right).normalize();
        let (sx, sy) = (1.0 / self.tan_half.0 as f64, 1.0 / self.tan_half.1 as f64);
        let (near, far) = (self.near_m as f64 * 100.0, self.far_m as f64 * 100.0);
        let (a, b) = (far / (far - near), -far * near / (far - near));
        // translated world (row vector) -> clip: rows of the matrix are the float4 slots 0..3
        let row = |i: usize| DVec4::new(right[i] * sx, up[i] * sy, fwd[i] * a, fwd[i]);
        let m = DMat4::from_cols(DVec4::new(row(0).x, row(1).x, row(2).x, 0.0), DVec4::new(row(0).y, row(1).y, row(2).y, 0.0), DVec4::new(row(0).z, row(1).z, row(2).z, b), DVec4::new(row(0).w, row(1).w, row(2).w, 0.0));
        let mut v = vec![[0f32; 4]; VIEW_FLOAT4S];
        let put = |v: &mut Vec<[f32; 4]>, at: usize, r: [f64; 4]| v[at] = [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32];
        let m_rows = to_rows(&m);
        for i in 0..4 { put(&mut v, slot::TRANSLATED_WORLD_TO_CLIP + i, m_rows[i]); put(&mut v, slot::PREV_TRANSLATED_WORLD_TO_CLIP + i, m_rows[i]); }
        // world -> clip: rel = p - eye  =>  clip = p * R + (-eye * R) + row3
        let t = [-eye.x, -eye.y, -eye.z];
        let mut r3 = m_rows[3];
        for k in 0..4 { r3[k] += t[0] * m_rows[0][k] + t[1] * m_rows[1][k] + t[2] * m_rows[2][k]; }
        for i in 0..3 { put(&mut v, slot::WORLD_TO_CLIP + i, m_rows[i]); }
        put(&mut v, slot::WORLD_TO_CLIP + 3, r3);
        // SvPosition -> translated world: (x_px, y_px, z, 1) * S * M^-1
        let (wd, ht) = (self.width.max(1) as f64, self.height.max(1) as f64);
        let s: Rows = [[2.0 / wd, 0.0, 0.0, 0.0], [0.0, -2.0 / ht, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [-1.0, 1.0, 0.0, 1.0]];
        let prod = mul_rows(&s, &invert_rows(&m_rows));
        for i in 0..4 { put(&mut v, slot::SV_POSITION_TO_TRANSLATED_WORLD + i, prod[i]); }
        v[slot::VIEW_FORWARD] = [fwd.x as f32, fwd.y as f32, fwd.z as f32, 0.0];
        v[slot::INV_DEVICE_Z_TO_WORLD_Z] = [0.0, 0.0, (1.0 / b) as f32, (a / b) as f32];
        v[slot::SCREEN_POSITION_SCALE_BIAS] = [0.5, -0.5, 0.5, 0.5];
        v[slot::VIEW_ORIGIN] = [eye.x as f32, eye.y as f32, eye.z as f32, 0.0];
        v[slot::PRE_VIEW_TRANSLATION] = [-eye.x as f32, -eye.y as f32, -eye.z as f32, 0.0];
        v[slot::PREV_PRE_VIEW_TRANSLATION] = v[slot::PRE_VIEW_TRANSLATION];
        v[slot::VIEW_RECT_MIN] = [0.0; 4];
        v[slot::VIEW_SIZE_AND_INV_SIZE] = [wd as f32, ht as f32, (1.0 / wd) as f32, (1.0 / ht) as f32];
        v[slot::TIME] = [0.0, 0.0, self.time, self.time];
        // MATERIAL PARAMETERS (2026-10-06): Diffuse / Specular / Normal / RoughnessOverrideParameter = the FSceneView defaults a shipping build uploads (UE4.16
        // SceneView.cpp:592-595; fn_fx::view::view_slot::DIFFUSE_OVERRIDE_PARAMETER): base passes scale SubsurfaceColor by the diffuse one (BasePassPixelShader.usf:632)
        for s in [slot::DIFFUSE_OVERRIDE_PARAMETER, slot::SPECULAR_OVERRIDE_PARAMETER, slot::NORMAL_OVERRIDE_PARAMETER] { v[s] = [0.0, 0.0, 0.0, 1.0]; }
        v[slot::ROUGHNESS_OVERRIDE_PARAMETER][0] = 0.0;
        v[slot::ROUGHNESS_OVERRIDE_PARAMETER][1] = 1.0;
        // sky: SkyLightColor * max(0, SHAr/g/b . (n, 1)); radiance(n) = sky * (0.5 + 0.5 n_z) in UE axes (port Y up = UE Z up)
        v[slot::SKY_LIGHT_COLOR] = [1.0, 1.0, 1.0, 1.0];
        for c in 0..3 { v[slot::SKY_IRRADIANCE_SH + c] = [0.0, 0.0, 0.5 * sky[c], 0.5 * sky[c]]; }
        v
    }
    /// [`view_buffer`](Self::view_buffer) with the renderer's real sky light: `sh` = `fn_render::lighting::sky_sh()` (`View.SkyIrradianceEnvironmentMap` SHAr, SHAg,
    /// SHAb, SHBr, SHBg, SHBb, SHC with `SkyLightColor` folded in). SKY LIGHT (sun-axis fix 2026-10-05): `lit_common.wgsl` `sky_lookup` now takes the UE axes
    /// as (x, z, y) of the renderer (`sky_light::world_to_ue_dir`), the same map as this module's [`dir_to_ue`], so the 7 constants go into
    /// `View.SkyIrradianceEnvironmentMap` UNROTATED (the earlier (-y', x', z') rotation matched the old (-z, x, y) map and turned the sky 90 deg).
    /// `View.SkyLightColor` = 1 because the colour is folded in (`max(0, x) * c == max(0, x * c)`, c >= 0). The gun is lit by the same sky light as
    /// every other surface of the scene (SLS_SpecifiedCubemap, full sphere), not by the hemisphere stand-in.
    pub fn view_buffer_sh(&self, sh: &[[f32; 4]; 7]) -> Vec<[f32; 4]> {
        let mut v = self.view_buffer([0.0; 3]);
        v[slot::SKY_LIGHT_COLOR] = [1.0, 1.0, 1.0, 1.0];
        v[slot::SKY_IRRADIANCE_SH..slot::SKY_IRRADIANCE_SH + 7].copy_from_slice(sh);
        v
    }
    /// GUN SHADOW: the View buffer of the shadow depth pass. Identical to [`view_buffer_sh`](Self::view_buffer_sh) except `TranslatedWorldToClip` (slots 0..3), which
    /// maps the translated UE world (cm, `world + PreViewTranslation`, PreViewTranslation = -eye) to the PORT world (m, Y up, w = 1): `(t.x, t.z, t.y) * 0.01 + eye`.
    /// The vertex program's SV_Position is then the world position, which `gun_shadow_depth_outputs` (fn-render gun_resolve.wgsl) puts through the cascade's
    /// LightViewProj. (UE draws the material's TShadowDepthVS with the shadow view's matrices; the world position the base-pass VS computes is the same one.)
    pub fn shadow_view_buffer_sh(&self, sh: &[[f32; 4]; 7]) -> Vec<[f32; 4]> {
        let mut v = self.view_buffer_sh(sh);
        v[slot::TRANSLATED_WORLD_TO_CLIP] = [0.01, 0.0, 0.0, 0.0];
        v[slot::TRANSLATED_WORLD_TO_CLIP + 1] = [0.0, 0.0, 0.01, 0.0];
        v[slot::TRANSLATED_WORLD_TO_CLIP + 2] = [0.0, 0.01, 0.0, 0.0];
        v[slot::TRANSLATED_WORLD_TO_CLIP + 3] = [self.eye.x, self.eye.y, self.eye.z, 1.0];
        v
    }
}

pub fn bytes(v: &[[f32; 4]]) -> Vec<u8> { let mut b = Vec::with_capacity(v.len() * 16); for r in v { for f in r { b.extend_from_slice(&f.to_le_bytes()); } } b }

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> ViewInput { ViewInput { eye: Vec3::new(1., 2., 3.), forward: Vec3::new(0.3, -0.1, -1.).normalize(), up: Vec3::Y, tan_half: (0.84, 0.47), near_m: 0.1, far_m: 3000., width: 1280, height: 720, time: 1.5 } }
    fn apply(v: &[[f32; 4]], at: usize, p: [f32; 4]) -> [f32; 4] { let mut o = [0f32; 4]; for i in 0..4 { for j in 0..4 { o[j] += p[i] * v[at + i][j]; } } o }
    fn project(v: &ViewInput, p: Vec3) -> [f32; 3] {
        let view = glam::Mat4::look_to_rh(v.eye, v.forward, v.up);
        let proj = glam::Mat4::perspective_rh(2. * v.tan_half.1.atan(), v.tan_half.0 / v.tan_half.1, v.near_m, v.far_m);
        let c = proj * view * glam::Vec4::new(p.x, p.y, p.z, 1.);
        [c.x / c.w, c.y / c.w, c.z / c.w]
    }
    /// GUN SHADOW: the shadow View buffer turns a translated UE position back into the port world position.
    #[test] fn shadow_view_maps_translated_world_to_port_world() {
        let vp = view();
        let v = vp.shadow_view_buffer_sh(&[[0.; 4]; 7]);
        let p = Vec3::new(12.5, -3., 40.);
        let ue = to_ue(p);
        let pvt = v[slot::PRE_VIEW_TRANSLATION];
        let t = [ue.x as f32 + pvt[0], ue.y as f32 + pvt[1], ue.z as f32 + pvt[2], 1.];
        let o = apply(&v, slot::TRANSLATED_WORLD_TO_CLIP, t);
        assert!((Vec3::new(o[0], o[1], o[2]) - p).length() < 1e-4 && o[3] == 1., "{o:?}");
    }
    #[test] fn translated_world_to_clip_equals_the_port_projection() {
        let vp = view(); let v = vp.view_buffer([1., 1., 1.]);
        let p = Vec3::new(2., 1.5, -2.);
        let want = project(&vp, p);
        let ue = to_ue(p); let e = to_ue(vp.eye);
        let c = apply(&v, slot::TRANSLATED_WORLD_TO_CLIP, [(ue.x - e.x) as f32, (ue.y - e.y) as f32, (ue.z - e.z) as f32, 1.0]);
        for k in 0..3 { assert!((c[k] / c[3] - want[k]).abs() < 3e-4, "axis {k}: {:?} vs {want:?}", [c[0] / c[3], c[1] / c[3], c[2] / c[3]]); }
        assert!((c[3] / 100. - (p - vp.eye).dot(vp.forward)).abs() < 1e-3, "clip.w is the view depth in cm");
    }
    #[test] fn sv_position_maps_back_to_the_translated_world() {
        let vp = view(); let v = vp.view_buffer([1., 1., 1.]);
        let p = Vec3::new(2., 1.5, -2.);
        let n = project(&vp, p);
        let (px, py) = ((n[0] * 0.5 + 0.5) * vp.width as f32, (0.5 - n[1] * 0.5) * vp.height as f32);
        let h = apply(&v, slot::SV_POSITION_TO_TRANSLATED_WORLD, [px, py, n[2], 1.0]);
        let ue = to_ue(p); let e = to_ue(vp.eye);
        let want = [(ue.x - e.x) as f32, (ue.y - e.y) as f32, (ue.z - e.z) as f32];
        for k in 0..3 { assert!((h[k] / h[3] - want[k]).abs() < 0.3, "{:?} vs {want:?}", [h[0] / h[3], h[1] / h[3], h[2] / h[3]]); }
    }
    /// The rotated SH evaluated on a normal in this module's UE axes equals `sky_lookup` (lit_common.wgsl) on the same renderer normal.
    #[test] fn the_scene_sky_sh_is_rotated_into_these_axes() {
        let sh: [[f32; 4]; 7] = std::array::from_fn(|i| std::array::from_fn(|k| ((i * 4 + k) as f32 * 0.37).sin()));
        let v = view().view_buffer_sh(&sh);
        let lit = |n: Vec3| -> [f32; 3] { let u = [n.x, n.z, n.y, 1.0]; /* lit_common.wgsl sky_lookup (sun-axis fix) */ let vb = [u[0] * u[1], u[1] * u[2], u[2] * u[2], u[2] * u[0]];
            std::array::from_fn(|c| (0..4).map(|k| sh[c][k] * u[k]).sum::<f32>() + (0..4).map(|k| sh[3 + c][k] * vb[k]).sum::<f32>() + sh[6][c] * (u[0] * u[0] - u[1] * u[1])) };
        let gun = |n: Vec3| -> [f32; 3] { let d = dir_to_ue(n); let u = [d.x as f32, d.y as f32, d.z as f32, 1.0]; let vb = [u[0] * u[1], u[1] * u[2], u[2] * u[2], u[2] * u[0]];
            std::array::from_fn(|c| (0..4).map(|k| v[slot::SKY_IRRADIANCE_SH + c][k] * u[k]).sum::<f32>() + (0..4).map(|k| v[slot::SKY_IRRADIANCE_SH + 3 + c][k] * vb[k]).sum::<f32>() + v[slot::SKY_IRRADIANCE_SH + 6][c] * (u[0] * u[0] - u[1] * u[1])) };
        for n in [Vec3::X, Vec3::Y, Vec3::Z, -Vec3::Z, Vec3::new(0.3, -0.5, 0.8).normalize(), Vec3::new(-0.7, 0.2, -0.1).normalize()] {
            let (a, b) = (lit(n), gun(n));
            for c in 0..3 { assert!((a[c] - b[c]).abs() < 1e-5, "{n:?}: {a:?} vs {b:?}"); }
        }
    }
    #[test] fn sky_sh_reproduces_the_hemisphere_ambient() {
        let vp = view(); let v = vp.view_buffer([0.2, 0.4, 0.8]);
        let eval = |n: [f32; 3]| -> [f32; 3] { std::array::from_fn(|c| { let r = v[slot::SKY_IRRADIANCE_SH + c]; (r[0] * n[0] + r[1] * n[1] + r[2] * n[2] + r[3]).max(0.0) * v[slot::SKY_LIGHT_COLOR][c] }) };
        let up = eval([0., 0., 1.]); let side = eval([1., 0., 0.]); let down = eval([0., 0., -1.]);
        assert!((up[2] - 0.8).abs() < 1e-6 && (side[2] - 0.4).abs() < 1e-6 && down[2].abs() < 1e-6);
    }
}
