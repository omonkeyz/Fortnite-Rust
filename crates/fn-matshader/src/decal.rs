use crate::material::ResolvedMaterial;
use crate::textures::{asset_sampler, decode_2d};
use crate::view::{slot, to_ue, ViewInput};
use fn_fx::data::PackageJson;
use fn_pak::PakSet;
use fn_render::decal_pass::{DecalAddress, DecalBind, DecalDesc, DecalDraw, DecalFilter};
use fn_render::gun_material::{GunAddress, GunFilter};
use glam::{DMat4, DVec3, DVec4};
use serde_json::Value;

pub const WEAPON_BP: &str = "/Game/Weapons/Blueprints/B_Ranged_Generic";
pub const PROGRAM: &str = "FDeferredDecalPS";

#[derive(Clone, Debug, PartialEq)]
pub struct DecalRules {
    pub use_decals: bool,
    pub material: String,
    pub texture: Option<String>,
    pub color_start: [f32; 4],
    pub color_end: [f32; 4],
    pub size_min: [f32; 3],
    pub size_max: [f32; 3],
    pub life_min: f32,
    pub life_max: f32,
    pub surfaces: Vec<u32>,
    pub min_distance: [f32; 5],
    pub allow: [bool; 5],
    pub local_bonus: f32,
    pub fade_screen_size: f32,
}

impl Default for DecalRules {
    fn default() -> Self {
        DecalRules {
            use_decals: true,
            material: String::new(),
            texture: None,
            color_start: [1.; 4],
            color_end: [1.; 4],
            size_min: [5., 5.5, 5.5],
            size_max: [5., 8., 8.],
            life_min: 2.3,
            life_max: 3.2,
            surfaces: vec![0, 1, 2, 3, 10],
            min_distance: [1024., 1792., 3840., 5120., -1.],
            allow: [true; 5],
            local_bonus: 1536.,
            fade_screen_size: 0.01,
        }
    }
}

fn f(v: &Value) -> Option<f32> { v.as_f64().map(|x| x as f32) }
fn c4(v: &Value) -> Option<[f32; 4]> {
    if let Some(a) = v.as_array() { return Some([0, 1, 2, 3].map(|i| a.get(i).and_then(f).unwrap_or(1.))); }
    Some(["R", "G", "B", "A"].map(|k| v.get(k).and_then(f).unwrap_or(1.)))
}

impl DecalRules {
    pub fn load(set: &PakSet) -> Result<DecalRules, String> {
        let pkg = PackageJson::load(set, WEAPON_BP)?;
        let cdo = pkg.exports.iter().find(|e| e.name.starts_with("Default__") && (e.props.get("DecalMaterial").is_some() || e.props.get("DecalSignificance").is_some())).ok_or_else(|| format!("{WEAPON_BP}: no class default object with decal properties"))?;
        let p = &cdo.props;
        let mut r = DecalRules::default();
        r.material = p.get("DecalMaterial").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| format!("{WEAPON_BP}: DecalMaterial not set"))?.to_owned();
        r.texture = p.get("DecalTexture").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned);
        if let Some(c) = p.get("DecalColorStart").and_then(c4) { r.color_start = c; }
        if let Some(c) = p.get("DecalColorEnd").and_then(c4) { r.color_end = c; }
        if let Some(b) = p.get("bUseImpactDecals").and_then(|v| v.as_bool()) { r.use_decals = b; }
        if let Some(v) = p.get("DecalLifespanMin").and_then(f) { r.life_min = v; }
        if let Some(v) = p.get("DecalLifespanMax").and_then(f) { r.life_max = v; }
        let v3 = |v: &Value| -> Option<[f32; 3]> { if let Some(a) = v.as_array() { Some([0, 1, 2].map(|i| a.get(i).and_then(f).unwrap_or(0.))) } else { Some(["X", "Y", "Z"].map(|k| v.get(k).and_then(f).unwrap_or(0.))) } };
        if let Some(v) = p.get("DecalSizeMin").and_then(v3) { r.size_min = v; }
        if let Some(v) = p.get("DecalSizeMax").and_then(v3) { r.size_max = v; }
        if let Some(a) = p.get("SurfaceAcceptingDecals").and_then(|v| v.as_array()) {
            r.surfaces = a.iter().filter_map(|x| x.as_u64().map(|n| n as u32).or_else(|| x.as_str().and_then(|s| s.rsplit("SurfaceType").next()).and_then(|n| n.parse().ok()))).collect();
        }
        if let Some(s) = p.get("DecalSignificance") {
            for (i, k) in ["Low", "Medium", "High", "Epic", "Cinematic"].iter().enumerate() {
                if let Some(v) = s.get(format!("MinDistance{k}")).and_then(f) { r.min_distance[i] = v; }
                if let Some(b) = s.get(format!("bAllow{k}")).and_then(|v| v.as_bool()) { r.allow[i] = b; }
            }
        }
        Ok(r)
    }

    pub fn accepts_surface(&self, surface: u32) -> bool { self.surfaces.contains(&surface) }

    pub fn significant(&self, level: usize, distance_cm: f32, local: bool) -> bool {
        let level = level.min(4);
        let threshold = if !self.allow[level] { return false } else if self.min_distance[level] < 0. { return true } else { significance(self.min_distance[level] + if local { self.local_bonus } else { 0. }) };
        threshold <= significance(distance_cm)
    }

    pub fn spawn(&self, pos_ue: DVec3, normal_ue: DVec3, now: f32, rand: &mut dyn FnMut() -> f32) -> Spawn {
        let sx = rand() * (self.size_max[0] - self.size_min[0]) + self.size_min[0];
        let sy = rand() * (self.size_max[1] - self.size_min[1]) + self.size_min[1];
        let pitch = normal_ue.z.atan2((normal_ue.x * normal_ue.x + normal_ue.y * normal_ue.y).sqrt()).to_degrees();
        let mut yaw = normal_ue.y.atan2(normal_ue.x).to_degrees() + rand() as f64 * 360.;
        yaw = yaw.rem_euclid(360.);
        if yaw > 180. { yaw -= 360.; }
        let life = rand() * (self.life_max - self.life_min) + self.life_min;
        Spawn { pos_ue, pitch, yaw, roll: 0., size: [sx, sy, sy], life, spawn_time: now }
    }
}

pub fn significance(distance_cm: f32) -> f32 {
    let d = distance_cm * 0.01;
    let x = d * d;
    if x < 2500. { 1. - x * 0.0001 }
    else if x < 10000. { 0.75 - (x - 2500.) * 3.3333334e-05 }
    else if x < 62500. { 0.5 - (x - 10000.) * 4.7619046e-06 }
    else { (0.25 - (x - 62500.) * 1.3333333e-06).max(0.) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spawn { pub pos_ue: DVec3, pub pitch: f64, pub yaw: f64, pub roll: f64, pub size: [f32; 3], pub life: f32, pub spawn_time: f32 }

impl Spawn {
    pub fn alive(&self, now: f32) -> bool { now - self.spawn_time < self.life }
    pub fn axes(&self) -> [DVec3; 3] {
        let (sp, cp) = self.pitch.to_radians().sin_cos();
        let (sy, cy) = self.yaw.to_radians().sin_cos();
        let (sr, cr) = self.roll.to_radians().sin_cos();
        [
            DVec3::new(cp * cy, cp * sy, sp),
            DVec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp),
            DVec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp),
        ]
    }
    pub fn component_to_world(&self, origin: DVec3) -> DMat4 {
        let [x, y, z] = self.axes();
        DMat4::from_cols((x * self.size[0] as f64).extend(0.), (y * self.size[1] as f64).extend(0.), (z * self.size[2] as f64).extend(0.), (self.pos_ue - origin).extend(1.))
    }
}

pub struct DecalAssets {
    pub desc: DecalDesc,
    pub rules: DecalRules,
    mat: ResolvedMaterial,
    pub notes: Vec<String>,
}

const VS: &str = r#"
struct DecalVsData { m: mat4x4<f32>, cam: vec4<f32> };
@group(3) @binding(0) var<uniform> decal_vs_data: DecalVsData;
@vertex fn decal_vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let face = i / 6u;
    let k = i % 6u;
    let axis = face % 3u;
    let sgn = select(-1.0, 1.0, face < 3u);
    var corners = array<vec2<f32>, 6>(vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0));
    let c = corners[k];
    var p = vec3<f32>(0.0);
    p[axis] = sgn;
    p[(axis + 1u) % 3u] = c.x;
    p[(axis + 2u) % 3u] = c.y;
    if (sgn * decal_vs_data.cam[axis] > 1.0) { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }
    return decal_vs_data.m * vec4<f32>(p, 1.0);
}
@fragment fn decal_fs(@builtin(position) in0: vec4<f32>) -> @location(0) vec4<f32> {
    var v: array<vec4<f32>, 1>;
    v[0] = in0;
    let o = ps_fx(v);
    return o[0];
}
"#;

fn declarations(wgsl: &str) -> Vec<(u32, u32, String)> {
    wgsl.lines().filter_map(|l| {
        let r = l.strip_prefix("@group(")?;
        let (g, r) = r.split_once(") @binding(")?;
        let (b, r) = r.split_once(") ")?;
        Some((g.parse().ok()?, b.parse().ok()?, r.to_owned()))
    }).collect()
}
fn var_name(decl: &str) -> Option<String> { Some(decl.split("var").nth(1)?.split(':').next()?.trim().rsplit(' ').next()?.to_owned()) }

fn filter(x: GunFilter) -> DecalFilter { if x == GunFilter::Linear { DecalFilter::Linear } else { DecalFilter::Nearest } }

impl DecalAssets {
    pub fn load(set: &PakSet) -> Result<DecalAssets, String> {
        let rules = DecalRules::load(set)?;
        let mat = ResolvedMaterial::load(set, &rules.material)?;
        let mut notes = vec![format!("{}: blend {} shading {}", rules.material, mat.flags.blend, mat.flags.shading)];
        let prog = mat.maps.find(mat.map_index, "", PROGRAM).ok_or_else(|| format!("{}: no {PROGRAM}", rules.material))?;
        let body = fn_dxbc::codegen::translate_wgsl(&prog.shader, "ps_fx").map_err(|e| e.0)?;
        let mat_tex: Vec<u32> = prog.bindings.iter().filter(|b| b.buffer_name == "Material" && format!("{:?}", b.kind) == "Texture").map(|b| b.binding.register as u32).collect();
        let mat_smp: Vec<u32> = prog.bindings.iter().filter(|b| b.buffer_name == "Material" && format!("{:?}", b.kind) == "Sampler").map(|b| b.binding.register as u32).collect();
        let mut entries = vec![];
        let (mut view_bytes, mut pass_bytes, mut material_bytes) = (0, 0, 0);
        for (g, b, decl) in declarations(&body) {
            let name = var_name(&decl).ok_or("declaration name")?;
            if decl.starts_with("var<uniform>") {
                let n: usize = name.strip_prefix("cb").and_then(|x| x.parse().ok()).ok_or("uniform name")?;
                let size: usize = decl.split("vec4<f32>, ").nth(1).and_then(|r| r.split('>').next()).and_then(|x| x.parse().ok()).ok_or("uniform size")?;
                let role = match prog.ubo_names.get(n).map(String::as_str) {
                    Some("") => { pass_bytes = size * 16; DecalBind::Pass }
                    Some("View") => { view_bytes = size * 16; DecalBind::View }
                    Some("Material") => { material_bytes = size * 16; DecalBind::Material }
                    other => return Err(format!("{PROGRAM}: uniform buffer {n} is {other:?}")),
                };
                entries.push((g, b, role));
            } else if decl.contains(": sampler;") {
                let reg: u32 = name.strip_prefix('s').and_then(|x| x.parse().ok()).ok_or("sampler name")?;
                entries.push((g, b, if mat_smp.contains(&reg) { DecalBind::DecalSampler } else { DecalBind::PointSampler }));
            } else {
                let reg: u32 = name.strip_prefix('t').and_then(|x| x.parse().ok()).ok_or("texture name")?;
                entries.push((g, b, if mat_tex.contains(&reg) { DecalBind::DecalTexture } else { DecalBind::SceneDepth }));
            }
        }
        if entries.iter().any(|e| e.0 >= 3) { return Err(format!("{PROGRAM}: uses bind group 3")); }
        for r in [DecalBind::Pass, DecalBind::View, DecalBind::Material, DecalBind::DecalTexture, DecalBind::SceneDepth] {
            if entries.iter().filter(|e| e.2 == r).count() != 1 { return Err(format!("{PROGRAM}: expected one {r:?} binding, got {entries:?}")); }
        }
        entries.push((3, 0, DecalBind::Vertex));
        let tex_path = rules.texture.clone().or_else(|| mat.texture_slot(0).map(|t| t.0)).ok_or("no decal texture")?;
        let (t, info) = decode_2d(set, &tex_path)?;
        let s = asset_sampler(&info);
        let address = match s.address { GunAddress::Wrap => DecalAddress::Wrap, GunAddress::Clamp => DecalAddress::Clamp, GunAddress::Mirror => DecalAddress::Mirror };
        let (w0, h0) = t.levels.first().map(|l| (l.0, l.1)).unwrap_or((1, 1));
        notes.push(format!("{tex_path}: {w0}x{h0}, {} mips, {} {}", t.levels.len(), info.filter, info.address_x));
        let base = mat.packed(0.).cb;
        notes.push(format!("material constants {:?}", base));
        let desc = DecalDesc {
            label: format!("{} {PROGRAM}", rules.material.rsplit('.').next().unwrap_or("")),
            wgsl: format!("{body}\n{VS}"), vs_entry: "decal_vs".into(), fs_entry: "decal_fs".into(), entries,
            view_bytes, pass_bytes, material_bytes, vertex_bytes: 80,
            texture: Some((w0, h0, t.srgb, t.levels)),
            sampler: (filter(s.mag), filter(s.min), filter(s.mip), address, s.aniso),
        };
        Ok(DecalAssets { desc, rules, mat, notes })
    }

    pub fn material_block(&self, s: &Spawn) -> Vec<u8> {
        let mut ov = fn_dxbc_cli::uniform::Overrides { vectors: self.mat.fx.overrides.vectors.clone(), scalars: self.mat.fx.overrides.scalars.clone() };
        ov.scalars.insert("Spawntime".into(), s.spawn_time);
        ov.scalars.insert("Lifetime".into(), s.life);
        ov.vectors.insert("Emissive Color A".into(), self.rules.color_start);
        ov.vectors.insert("Emissive Color B".into(), self.rules.color_end);
        let mut cb = fn_dxbc_cli::uniform::evaluate(&self.mat.map().uniform, &ov, s.spawn_time).cb;
        cb.resize(self.desc.material_bytes / 16, [0.; 4]);
        crate::view::bytes(&cb)
    }

    pub fn view_block(&self, view: &ViewInput) -> (Vec<[f32; 4]>, Vec<u8>) {
        let mut v = view.view_buffer([0.; 3]);
        let (w, h) = (view.width.max(1) as f32, view.height.max(1) as f32);
        if v.len() <= 122 { v.resize(123, [0.; 4]); }
        v[122] = [w, h, 1. / w, 1. / h];
        let mut b = v.clone();
        b.resize(self.desc.view_bytes / 16, [0.; 4]);
        (v, crate::view::bytes(&b))
    }

    pub fn fade_alpha(&self, s: &Spawn, view: &ViewInput) -> f32 {
        if self.rules.fade_screen_size <= 0. { return 1.; }
        let d = (to_ue(view.eye) - s.pos_ue).length() as f32;
        let r = s.size.iter().fold(0f32, |a, x| a.max(x.abs()));
        let c = (2. * view.tan_half.0) / view.width.max(1) as f32 * 600. * self.rules.fade_screen_size;
        ((r / d.max(1e-3) - c) / (c * 0.5)).min(1.)
    }

    pub fn draw(&self, s: &Spawn, material: &[u8], view_rows: &[[f32; 4]], view: &ViewInput) -> Option<DecalDraw> {
        let fade = self.fade_alpha(s, view);
        if fade <= 0. { return None; }
        let eye = to_ue(view.eye);
        let rows = |at: usize| DMat4::from_cols_array_2d(&std::array::from_fn(|i| std::array::from_fn(|j| view_rows[at + i][j] as f64)));
        let tw2c = rows(slot::TRANSLATED_WORLD_TO_CLIP);
        let sv2tw = rows(slot::SV_POSITION_TO_TRANSLATED_WORLD);
        let c2tw = s.component_to_world(eye);
        let inv = c2tw.inverse();
        let sv2d = inv * sv2tw;
        let c2w = s.component_to_world(DVec3::ZERO);
        let w2d = c2w.inverse();
        let mut pass = vec![[0f32; 4]; self.desc.pass_bytes / 16];
        let put = |p: &mut Vec<[f32; 4]>, at: usize, m: &DMat4| { for i in 0..4 { if at + i < p.len() { let c = m.col(i); p[at + i] = [c.x as f32, c.y as f32, c.z as f32, c.w as f32]; } } };
        put(&mut pass, 0, &sv2d);
        put(&mut pass, 4, &c2w);
        put(&mut pass, 8, &w2d);
        if pass.len() > 12 { pass[12] = [fade, 0., 0., 0.]; }
        let m = tw2c * c2tw;
        let cam = inv * DVec4::new(0., 0., 0., 1.);
        let mut vtx: Vec<f32> = m.to_cols_array().iter().map(|x| *x as f32).collect();
        vtx.extend([cam.x as f32, cam.y as f32, cam.z as f32, 1.]);
        let vertex: Vec<u8> = vtx.iter().flat_map(|x| x.to_le_bytes()).collect();
        Some(DecalDraw { pass: crate::view::bytes(&pass), material: material.to_vec(), vertex })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn significance_follows_the_exe_curve() {
        assert!((significance(0.) - 1.).abs() < 1e-6);
        assert!((significance(5000.) - 0.75).abs() < 1e-5);
        assert!((significance(10000.) - 0.5).abs() < 1e-5);
        assert!((significance(25000.) - 0.25).abs() < 1e-5);
        assert_eq!(significance(60000.), 0.);
    }

    #[test] fn significance_rule_is_a_distance_cut_per_level() {
        let mut r = DecalRules::default();
        r.min_distance = [1024., 1400., 1800., 2200., 5000.];
        r.allow[0] = false;
        assert!(r.significant(3, 2100., false));
        assert!(!r.significant(3, 2300., false));
        assert!(r.significant(3, 2300., true));
        assert!(!r.significant(0, 10., true));
        assert!(r.significant(1, 1300., false) && !r.significant(1, 1500., false));
    }

    #[test] fn spawn_randomises_yaw_and_keeps_the_normal_pitch() {
        let r = DecalRules::default();
        let mut k = 0;
        let vals = [0.5f32, 0.0, 0.25, 1.0];
        let mut rnd = || { let v = vals[k % 4]; k += 1; v };
        let s = r.spawn(DVec3::ZERO, DVec3::new(0., 0., 1.), 10., &mut rnd);
        assert_eq!(s.size, [5., 5.5, 5.5]);
        assert!((s.pitch - 90.).abs() < 1e-9);
        assert!((s.yaw - 90.).abs() < 1e-9);
        assert!((s.life - 3.2).abs() < 1e-6);
        let x = s.axes()[0];
        assert!((x - DVec3::Z).length() < 1e-9);
        assert!(s.alive(13.1) && !s.alive(13.3));
    }

    #[test] fn decal_space_matrix_maps_the_box_to_unit_cube() {
        let s = Spawn { pos_ue: DVec3::new(100., 200., 50.), pitch: 0., yaw: 30., roll: 0., size: [5., 7., 7.], life: 3., spawn_time: 0. };
        let c2w = s.component_to_world(DVec3::ZERO);
        let p = c2w * DVec4::new(1., -1., 1., 1.);
        let back = c2w.inverse() * p;
        assert!((back - DVec4::new(1., -1., 1., 1.)).length() < 1e-9);
        assert!(((c2w * DVec4::new(0., 0., 0., 1.)).truncate() - s.pos_ue).length() < 1e-9);
    }

    #[test] fn decal_material_loads_and_validates() {
        let Ok(set) = PakSet::open_install(fn_fx::default_install()) else { eprintln!("no 1.8 install: skipped"); return };
        let a = DecalAssets::load(&set).unwrap();
        for n in &a.notes { eprintln!("{n}"); }
        assert_eq!(a.rules.min_distance, [1024., 1400., 1800., 2200., 5000.]);
        assert_eq!(a.rules.allow, [false, true, true, true, true]);
        assert_eq!(a.rules.surfaces, vec![0, 1, 2, 3, 10]);
        let gpu = match fn_render::Gpu::headless() { Ok(g) => g, Err(e) => { eprintln!("no GPU: {e}"); return } };
        let mut r = fn_render::decal_pass::DecalRenderer::new(&gpu.device, &gpu.queue);
        r.create(a.desc.clone(), fn_render::renderer::HDR_FORMAT).unwrap();
        let view = ViewInput { eye: glam::Vec3::new(0., 1.8, 0.), forward: glam::Vec3::NEG_Z, up: glam::Vec3::Y, tan_half: (1., 0.6), near_m: 0.1, far_m: 1000., width: 640, height: 360, time: 1.5 };
        let s = a.rules.spawn(to_ue(glam::Vec3::new(0., 0., -3.)), DVec3::Z, 1., &mut || 0.5);
        let (rows, _) = a.view_block(&view);
        let d = a.draw(&s, &a.material_block(&s), &rows, &view).unwrap();
        assert_eq!(d.pass.len(), a.desc.pass_bytes);
        assert_eq!(d.vertex.len(), 80);
    }
}
