use crate::data::{GunHit, APPLICATION_EXPLODE};
use crate::shoot::{building_damage, building_trace, hit_pawn, pellet_dir, select_aim, trace_pawns, trace_world, FireContext, Impact, PawnFire, PawnHit, PawnTarget, PawnTraceHit, Shot, WorldHit, Zone, SURFACE_DEFAULT};
use crate::spread::{safe_normal, DamageKind, RandomStream, SpreadState};
use fn_combat::{harvest_fx::FxEvent, World};
use fn_pose::{loader, ClipSourceDir};
use fn_ueio::props::{read_props, Ctx, PropList, PropValue};
use fn_ueio::{Package, Reader};
use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Rocket, Grenade, HandGrenade }

#[derive(Clone, Debug, PartialEq)]
pub struct ExplosiveDef {
    pub kind: Kind,
    pub template: String,
    pub mesh: Option<String>,
    pub mesh_scale: Vec3,
    pub capsule_radius_cm: f32,
    pub capsule_half_height_cm: f32,
    pub trail: Option<String>,
    pub ribbon: Option<String>,
    pub trail_attached_ribbon: bool,
    pub trail_lifetime: f32,
    pub explosion_fx: Option<String>,
    pub explosion_water_fx: Option<String>,
    pub explosion_sound: Option<String>,
    pub explosion_water_sound: Option<String>,
    pub in_air_sound: Option<String>,
    pub in_air_fade_out: f32,
    pub bounce_fx: Option<String>,
    pub bounce_water_fx: Option<String>,
    pub bounce_sound: Option<String>,
    pub fuse_s: f32,
    pub explode_on_pawn_bounce: bool,
    pub impact_offset_cm: f32,
    pub explode_at_actor: bool,
    pub camera_shake: Option<ShakeDef>,
    pub force_feedback_radii: (f32, f32),
    pub post_process: Option<PpFlash>,
    pub spin_rate_deg: f32,
    pub explode_tag: String,
    pub rotation_rate_deg: Vec3,
    pub fuse_fx: Option<(String, Vec3)>,
    pub pawn_overlap_gravity: Option<f32>,
    pub explosion_rot_pitch_deg: f32,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct CameraAnimDef { pub path: String, pub length: f32, pub play_rate: f32, pub scale: f32, pub blend_in: f32, pub blend_out: f32, pub translation: [Vec<CurveKey>; 3], pub rotation: [Vec<CurveKey>; 3],
    pub pp_weight: f32, pub pp_tint: Option<[f32; 3]>, pub pp_tint_track: [Vec<CurveKey>; 3] }

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PpFlash { pub blend_radius_cm: f32, pub scene_color_tint: Option<[f32; 3]>, pub color_contrast: Option<f32>, pub fade: Vec<CurveKey>, pub fade_length: f32 }

impl PpFlash {
    pub fn weight(&self, t: f32) -> f32 { if t > self.fade_length { 0. } else { interp_curve_eval(&self.fade, t).clamp(0., 1.) } }
    pub fn local_weight(&self, dist_cm: f32) -> f32 { if dist_cm > self.blend_radius_cm { 0. } else if self.blend_radius_cm >= 1. { (1. - dist_cm / self.blend_radius_cm).clamp(0., 1.) } else { 1. } }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveKey { pub t: f32, pub v: f32, pub arrive: f32, pub leave: f32, pub mode: u8 }

#[derive(Clone, Debug, PartialEq)]
pub struct ShakeDef { pub class: String, pub inner_cm: f32, pub outer_cm: f32, pub falloff: f32, pub loc: [(f32, f32); 3], pub duration: f32, pub blend_in: f32, pub blend_out: f32, pub anim: Option<CameraAnimDef> }

pub fn interp_curve_eval(keys: &[CurveKey], t: f32) -> f32 {
    let Some(first) = keys.first() else { return 0. };
    if keys.len() == 1 || t <= first.t { return first.v; }
    let last = keys[keys.len() - 1];
    if t >= last.t { return last.v; }
    let i = keys.windows(2).position(|w| t < w[1].t).unwrap_or(keys.len() - 2);
    let (a, b) = (keys[i], keys[i + 1]);
    let d = b.t - a.t;
    if d <= 0. { return b.v; }
    let x = (t - a.t) / d;
    match a.mode {
        1 => a.v + (b.v - a.v) * x,
        4 => a.v,
        _ => {
            let (x2, x3) = (x * x, x * x * x);
            (2. * x3 - 3. * x2 + 1.) * a.v + (x3 - 2. * x2 + x) * a.leave * d + (x3 - x2) * b.arrive * d + (-2. * x3 + 3. * x2) * b.v
        }
    }
}

impl CameraAnimDef {
    pub fn duration(&self) -> f32 { self.length / self.play_rate.max(1e-6) }
    pub fn weight(&self, elapsed: f32) -> f32 {
        let dur = self.duration();
        let w_in = if self.blend_in > 0. { (elapsed / self.blend_in).min(1.) } else { 1. };
        let left = dur - elapsed;
        let w_out = if self.blend_out > 0. && left < self.blend_out { (left / self.blend_out).max(0.) } else { 1. };
        w_in.min(w_out)
    }
    pub fn pp_tint_at(&self, elapsed: f32) -> Option<[f32; 3]> {
        let base = self.pp_tint?;
        let t = (elapsed * self.play_rate).min(self.length);
        Some(std::array::from_fn(|i| if self.pp_tint_track[i].is_empty() { base[i] } else { interp_curve_eval(&self.pp_tint_track[i], t) }))
    }
    pub fn offset(&self, elapsed: f32) -> Vec3 {
        let t = (elapsed * self.play_rate).min(self.length);
        let o = |k: &Vec<CurveKey>| interp_curve_eval(k, t) - interp_curve_eval(k, 0.);
        Vec3::new(o(&self.translation[0]), o(&self.translation[1]), o(&self.translation[2]))
    }
}

impl ShakeDef {
    pub fn scale_at(&self, dist_cm: f32) -> f32 {
        if dist_cm <= self.inner_cm { 1. } else if dist_cm >= self.outer_cm { 0. } else { 1. - ((dist_cm - self.inner_cm) / (self.outer_cm - self.inner_cm)).powf(self.falloff) }
    }
}

fn axis_index(axis: &str) -> (bool, usize) {
    match axis.rsplit("::").next().unwrap_or(axis) {
        "AXIS_TranslationY" => (true, 1), "AXIS_TranslationZ" => (true, 2),
        "AXIS_RotationX" => (false, 0), "AXIS_RotationY" => (false, 1), "AXIS_RotationZ" => (false, 2),
        _ => (true, 0),
    }
}

fn curve_keys(v: Option<&PropList>) -> Vec<CurveKey> {
    let Some(track) = v else { return vec![] };
    track.array("Points").map(|a| a.as_slice()).unwrap_or(&[]).iter().filter_map(|p| {
        let f = p.as_fields()?;
        let mode = match f.text("InterpMode").unwrap_or("CIM_CurveAuto").rsplit("::").next().unwrap_or("") { "CIM_Linear" => 1, "CIM_Constant" => 4, _ => 0 };
        Some(CurveKey { t: f.float("InVal")? as f32, v: f.float("OutVal")? as f32, arrive: f.float("ArriveTangent").unwrap_or(0.) as f32, leave: f.float("LeaveTangent").unwrap_or(0.) as f32, mode })
    }).collect()
}

pub fn load_camera_anim(src: &ClipSourceDir, path: &str) -> Option<CameraAnimDef> {
    let pkg_path = path.split('.').next().unwrap_or(path);
    let pkg = loader::package(src, pkg_path).ok()?;
    let ca = (0..pkg.exports.len()).find(|&i| pkg.export_class(i) == "CameraAnim").and_then(|i| props_of(&pkg, i))?;
    let mut out = CameraAnimDef { path: pkg_path.to_owned(), length: ca.float("AnimLength").map_or(3., |v| v as f32), ..Default::default() };
    out.pp_weight = ca.float("BasePostProcessBlendWeight").map_or(1., |v| v as f32);
    if let Some(pp) = ca.fields("BasePostProcessSettings").filter(|p| p.bool("bOverride_SceneColorTint") == Some(true)) {
        out.pp_tint = Some(match pp.get("SceneColorTint") { Some(PropValue::Vec4(v)) => [v[0], v[1], v[2]], _ => [1., 1., 1.] });
    }
    for i in (0..pkg.exports.len()).filter(|&i| pkg.export_class(i) == "InterpTrackLinearColorProp") {
        let Some(p) = props_of(&pkg, i) else { continue };
        if !p.text("PropertyName").unwrap_or("").ends_with("SceneColorTint") { continue; }
        let pts = p.fields("LinearColorTrack").and_then(|t| t.array("Points")).map(|a| a.as_slice()).unwrap_or(&[]);
        for c in 0..3 {
            out.pp_tint_track[c] = pts.iter().filter_map(|pt| {
                let f = pt.as_fields()?;
                let v4 = |k: &str| match f.get(k) { Some(PropValue::Vec4(v)) => v[c], _ => 0. };
                let mode = match f.text("InterpMode").unwrap_or("CIM_CurveAuto").rsplit("::").next().unwrap_or("") { "CIM_Linear" => 1, "CIM_Constant" => 4, _ => 0 };
                Some(CurveKey { t: f.float("InVal")? as f32, v: v4("OutVal"), arrive: v4("ArriveTangent"), leave: v4("LeaveTangent"), mode })
            }).collect();
        }
        if out.pp_tint.is_none() { out.pp_tint = Some([1., 1., 1.]); }
    }
    for i in (0..pkg.exports.len()).filter(|&i| pkg.export_class(i) == "InterpTrackMoveAxis") {
        let Some(p) = props_of(&pkg, i) else { continue };
        let (tr, k) = axis_index(p.text("MoveAxis").unwrap_or("AXIS_TranslationX"));
        let keys = curve_keys(p.fields("FloatTrack"));
        if tr { out.translation[k] = keys; } else { out.rotation[k] = keys; }
    }
    Some(out)
}

fn rich_curve_keys(curve: &PropList) -> Vec<CurveKey> {
    const KEY_BYTES: usize = 27;
    if let Some(PropValue::Raw(raw)) = curve.fields("FloatCurve").and_then(|c| c.get("Keys")) {
        let n = raw.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
        let Some(start) = raw.len().checked_sub(n * KEY_BYTES) else { return vec![] };
        let f = |b: &[u8], o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        return raw[start..].chunks_exact(KEY_BYTES).map(|k| CurveKey { t: f(k, 3), v: f(k, 7), arrive: f(k, 11), leave: f(k, 19), mode: match k[0] { 0 => 1, 1 => 4, _ => 0 } }).collect();
    }
    curve.fields("FloatCurve").and_then(|c| c.array("Keys")).map(|a| a.as_slice()).unwrap_or(&[]).iter().filter_map(|k| {
        let f = k.as_fields()?;
        let mode = match f.get("InterpMode") { Some(PropValue::Byte(b)) => *b as i64, Some(PropValue::Int(i)) => *i, Some(PropValue::Enum(e)) => match e.rsplit("::").next().unwrap_or("") { "RCIM_Linear" => 0, "RCIM_Constant" => 1, _ => 2 }, _ => 2 };
        Some(CurveKey { t: f.float("Time")? as f32, v: f.float("Value")? as f32, arrive: f.float("ArriveTangent").unwrap_or(0.) as f32, leave: f.float("LeaveTangent").unwrap_or(0.) as f32, mode: match mode { 0 => 1, 1 => 4, _ => 0 } })
    }).collect()
}

fn load_pp_flash(pkg: &Package) -> Option<PpFlash> {
    let comp = (0..pkg.exports.len()).filter(|&i| pkg.export_class(i) == "PostProcessComponent").find_map(|i| props_of(pkg, i))?;
    let set = comp.fields("Settings");
    let on = |k: &str| set.and_then(|s| s.bool(k)) == Some(true);
    let v4 = |k: &str| set.and_then(|s| match s.get(k) { Some(PropValue::Vec4(v)) => Some(*v), _ => None });
    let scene_color_tint = on("bOverride_SceneColorTint").then(|| v4("SceneColorTint").map_or([1., 1., 1.], |v| [v[0], v[1], v[2]]));
    let color_contrast = on("bOverride_ColorContrast").then(|| v4("ColorContrast").map_or(1., |v| (v[0] + v[1] + v[2]) / 3. * v[3]));
    let timeline = (0..pkg.exports.len()).filter(|&i| pkg.export_class(i) == "TimelineTemplate").filter_map(|i| props_of(pkg, i)).next();
    let fade_length = timeline.as_ref().and_then(|t| t.float("TimelineLength")).map_or(1., |v| v as f32);
    let fade = (0..pkg.exports.len()).filter(|&i| pkg.export_class(i) == "CurveFloat").filter_map(|i| props_of(pkg, i)).map(|c| rich_curve_keys(&c)).next().unwrap_or_default();
    Some(PpFlash { blend_radius_cm: comp.float("BlendRadius").map_or(100., |v| v as f32), scene_color_tint, color_contrast, fade, fade_length })
}

pub fn load_shake(src: &ClipSourceDir, class_path: &str, inner_cm: f32, outer_cm: f32, falloff: f32) -> Option<ShakeDef> {
    let pkg_path = class_path.split('.').next().unwrap_or(class_path);
    let pkg = loader::package(src, pkg_path).ok()?;
    let cdo_i = pkg.exports.iter().position(|e| e.object_name.starts_with("Default__"))?;
    let c = props_of(&pkg, cdo_i)?;
    let osc = c.fields("LocOscillation");
    let axis = |n: &str| osc.and_then(|o| o.fields(n)).map_or((0., 0.), |a| (a.float("Amplitude").unwrap_or(0.) as f32, a.float("Frequency").unwrap_or(0.) as f32));
    let f = |k: &str, d: f32| c.float(k).map_or(d, |v| v as f32);
    let anim = obj_path(&pkg, c.get("Anim")).and_then(|a| load_camera_anim(src, &a)).map(|mut a| { a.play_rate = f("AnimPlayRate", 1.); a.scale = f("AnimScale", 1.); a.blend_in = f("AnimBlendInTime", 0.2); a.blend_out = f("AnimBlendOutTime", 0.2); a });
    Some(ShakeDef { class: pkg_path.to_owned(), inner_cm, outer_cm, falloff, loc: [axis("X"), axis("Y"), axis("Z")], duration: f("OscillationDuration", 0.), blend_in: f("OscillationBlendInTime", 0.1), blend_out: f("OscillationBlendOutTime", 0.2), anim })
}

struct Recipe {
    template: &'static str,
    kind: Kind,
    mesh_scale_var: Option<&'static str>,
    mesh_scale_lit: [f32; 3],
    fuse_var: Option<&'static str>,
    fuse_lit: f32,
    trail: Option<&'static str>,
    ribbon: &'static str,
    explosion: &'static str,
    explosion_water: &'static str,
    explosion_sound: &'static str,
    explosion_water_sound: &'static str,
    bounce: Option<(&'static str, &'static str, &'static str)>,
    explode_on_pawn_bounce: bool,
    impact_offset_var: Option<&'static str>,
    explode_at_actor: bool,
    shake: Option<(&'static str, f32, f32)>,
    feedback: (f32, f32),
    in_air_fade_out: f32,
}

const RECIPES: &[Recipe] = &[
    Recipe { template: "B_Prj_Pumpkin_RPG_Athena", kind: Kind::Rocket, mesh_scale_var: Some("RocketScale"), mesh_scale_lit: [1., 1., 1.], fuse_var: None, fuse_lit: 15.,
        trail: Some("Rocket Trail Template"), ribbon: "Ribbon_Trail_PS", explosion: "Explosion_Generic_PS", explosion_water: "Water_Explosion_PS",
        explosion_sound: "Rocket_Projectile_Explosion_Sound", explosion_water_sound: "Rocket_Projectile_Explosion_Water_Sound", bounce: None, explode_on_pawn_bounce: false,
        impact_offset_var: Some("Explosion Impact Offset"), explode_at_actor: false, shake: Some(("B_RocketLauncherCameraShake", 400., 850.)), feedback: (400., 850.), in_air_fade_out: 1. },
    Recipe { template: "B_Prj_Ranged_GrenadeLauncher_Athena", kind: Kind::Grenade, mesh_scale_var: None, mesh_scale_lit: [2., 2., 2.], fuse_var: Some("FuseTimer@Spawn"), fuse_lit: 0.,
        trail: None, ribbon: "RibbonTrail_PS", explosion: "Explosion_PS", explosion_water: "Explosion_Water_PS",
        explosion_sound: "Grenade_Explosion_Sound", explosion_water_sound: "Grenade_Explosion_Water_Sound", bounce: Some(("Bounce_Generic_PS", "Bounce_WaterSplash_PS", "Grenade_Projectile_Bounce_Cue")), explode_on_pawn_bounce: true,
        impact_offset_var: None, explode_at_actor: true, shake: None, feedback: (600., 1200.), in_air_fade_out: 0. },
];

fn props_of(pkg: &Package, i: usize) -> Option<PropList> {
    let (s, e) = pkg.export_range(i);
    read_props(&mut Reader::at(&pkg.data[..e], s), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).ok()
}
fn obj_path(pkg: &Package, v: Option<&PropValue>) -> Option<String> {
    match v? { PropValue::Object(i) if *i < 0 => Some(pkg.import_path(*i)), PropValue::Str(s) if s.starts_with("/Game/") => Some(s.clone()), _ => None }
}
fn vec3_of(v: Option<&PropValue>) -> Option<Vec3> {
    match v? {
        PropValue::Vec3(a) => Some(Vec3::from_array(*a)),
        PropValue::Struct { fields, .. } => Some(Vec3::new(fields.float("X")? as f32, fields.float("Y")? as f32, fields.float("Z")? as f32)),
        _ => None,
    }
}

pub fn load_def(src: &ClipSourceDir, template: &str) -> Option<ExplosiveDef> {
    let pkg_path = template.split('.').next().unwrap_or(template);
    let name = pkg_path.rsplit('/').next().unwrap_or(pkg_path);
    let r = RECIPES.iter().find(|r| r.template == name)?;
    let pkg = loader::package(src, pkg_path).ok()?;
    let cdo_i = pkg.exports.iter().position(|e| e.object_name.starts_with("Default__"))?;
    let cdo = props_of(&pkg, cdo_i)?;
    let class_props = |class: &str| (0..pkg.exports.len()).filter(|&i| pkg.export_class(i) == class).filter_map(|i| props_of(&pkg, i)).collect::<Vec<_>>();
    let capsule = class_props("CapsuleComponent").into_iter().next();
    let mesh = class_props("StaticMeshComponent").iter().find_map(|p| obj_path(&pkg, p.get("StaticMesh")));
    let in_air = class_props("AudioComponent").iter().find_map(|p| obj_path(&pkg, p.get("Sound")));
    let path = |k: &str| obj_path(&pkg, cdo.get(k));
    let import_named = |n: &str| (0..pkg.imports.len()).find(|&i| pkg.imports[i].object_name == n).map(|i| pkg.import_path(-(i as i32) - 1));
    let mesh_scale = r.mesh_scale_var.and_then(|k| vec3_of(cdo.get(k))).unwrap_or(Vec3::from_array(r.mesh_scale_lit));
    let fuse_s = r.fuse_var.and_then(|k| cdo.float(k)).map_or(r.fuse_lit, |v| v as f32);
    let shake = r.shake.and_then(|(n, a, b)| load_shake(src, &import_named(&format!("{n}_C")).or_else(|| import_named(n)).unwrap_or_else(|| n.to_owned()), a, b, 1.));
    let pp = load_pp_flash(&pkg);
    Some(ExplosiveDef {
        kind: r.kind, template: pkg_path.to_owned(), mesh, mesh_scale,
        capsule_radius_cm: capsule.as_ref().and_then(|c| c.float("CapsuleRadius")).map_or(22., |v| v as f32),
        capsule_half_height_cm: capsule.as_ref().and_then(|c| c.float("CapsuleHalfHeight")).map_or(44., |v| v as f32),
        trail: r.trail.and_then(path), ribbon: path(r.ribbon), trail_attached_ribbon: r.kind == Kind::Grenade, trail_lifetime: cdo.float("TrailLifetime").map_or(0., |v| v as f32),
        explosion_fx: path(r.explosion), explosion_water_fx: path(r.explosion_water), explosion_sound: path(r.explosion_sound), explosion_water_sound: path(r.explosion_water_sound),
        in_air_sound: in_air, in_air_fade_out: r.in_air_fade_out,
        bounce_fx: r.bounce.and_then(|b| path(b.0)), bounce_water_fx: r.bounce.and_then(|b| path(b.1)), bounce_sound: r.bounce.and_then(|b| import_named(b.2)),
        fuse_s, explode_on_pawn_bounce: r.explode_on_pawn_bounce,
        impact_offset_cm: r.impact_offset_var.and_then(|k| cdo.float(k)).map_or(0., |v| v as f32), explode_at_actor: r.explode_at_actor,
        camera_shake: shake, force_feedback_radii: r.feedback, post_process: pp, spin_rate_deg: cdo.float("RocketSpinRate").map_or(0., |v| v as f32),
        explode_tag: crate::data::APPLICATION_EXPLODE.to_owned(), rotation_rate_deg: Vec3::ZERO, fuse_fx: None, pawn_overlap_gravity: None, explosion_rot_pitch_deg: 0.,
    })
}

pub const HAND_GRENADE_FUSE_S: f32 = 2.75;
pub const HAND_GRENADE_SHAKE: (&str, f32, f32, f32) = ("B_GrenadeCameraShake_C", 200., 800., 1.);
pub const HAND_GRENADE_EXPLOSION_PITCH: f32 = 90.;

pub fn load_hand_grenade_def(src: &ClipSourceDir, template: &str, explode_tag: &str) -> Option<ExplosiveDef> {
    let pkg_path = template.split('.').next().unwrap_or(template);
    let pkg = loader::package(src, pkg_path).ok()?;
    let cdo_i = pkg.exports.iter().position(|e| e.object_name.starts_with("Default__"))?;
    let cdo = props_of(&pkg, cdo_i)?;
    let path = |k: &str| obj_path(&pkg, cdo.get(k));
    let comps = |class: &str| (0..pkg.exports.len()).filter(|&i| pkg.export_class(i) == class).filter_map(|i| Some((pkg.exports[i].object_name.clone(), props_of(&pkg, i)?))).collect::<Vec<_>>();
    let capsule = comps("CapsuleComponent").into_iter().find(|(n, _)| n == "CollisionCapsule0").map(|c| c.1);
    let mesh_c = comps("StaticMeshComponent").into_iter().next().map(|c| c.1);
    let mesh = mesh_c.as_ref().and_then(|p| obj_path(&pkg, p.get("StaticMesh")));
    let mesh_scale = mesh_c.as_ref().and_then(|p| vec3_of(p.get("RelativeScale3D"))).unwrap_or(Vec3::ONE);
    let fuse_fx = comps("ParticleSystemComponent").into_iter().filter_map(|(_, p)| Some((obj_path(&pkg, p.get("Template"))?, p.bool("bAutoActivate").unwrap_or(true), vec3_of(p.get("RelativeLocation")).unwrap_or(Vec3::ZERO)))).find(|x| x.1).map(|x| (x.0, x.2));
    let rot = comps("RotatingMovementComponent").into_iter().next().and_then(|(_, p)| vec3_of(p.get("RotationRate"))).unwrap_or(Vec3::ZERO);
    let import_named = |n: &str| (0..pkg.imports.len()).find(|&i| pkg.imports[i].object_name == n).map(|i| pkg.import_path(-(i as i32) - 1));
    let (sn, inner, outer, fall) = HAND_GRENADE_SHAKE;
    Some(ExplosiveDef {
        kind: Kind::HandGrenade, template: pkg_path.to_owned(), mesh, mesh_scale,
        capsule_radius_cm: capsule.as_ref().and_then(|c| c.float("CapsuleRadius")).map_or(22., |v| v as f32),
        capsule_half_height_cm: capsule.as_ref().and_then(|c| c.float("CapsuleHalfHeight")).map_or(44., |v| v as f32),
        trail: None, ribbon: None, trail_attached_ribbon: false, trail_lifetime: 0.,
        explosion_fx: path("P_Explosion"), explosion_water_fx: None, explosion_sound: path("Cue_CloseSound"), explosion_water_sound: None,
        in_air_sound: None, in_air_fade_out: 0.2, bounce_fx: None, bounce_water_fx: None, bounce_sound: path("Cue_Bounce"),
        fuse_s: HAND_GRENADE_FUSE_S, explode_on_pawn_bounce: false, impact_offset_cm: 0., explode_at_actor: true,
        camera_shake: import_named(sn).and_then(|c| load_shake(src, &c, inner, outer, fall)), force_feedback_radii: (0., 0.), post_process: None, spin_rate_deg: 0.,
        explode_tag: explode_tag.to_owned(), rotation_rate_deg: rot, fuse_fx, pawn_overlap_gravity: cdo.float("BouncePawnAgainstPawnGravityScale").map(|v| v as f32),
        explosion_rot_pitch_deg: HAND_GRENADE_EXPLOSION_PITCH,
    })
}

pub fn load_throwable(src: &ClipSourceDir, item_path: &str) -> Result<GunHit, String> {
    let pkg_path = item_path.split('.').next().unwrap_or(item_path);
    let pkg = loader::package(src, pkg_path)?;
    let item = props_of(&pkg, 0).ok_or("item has no properties")?;
    let h = item.fields("WeaponStatHandle").ok_or("item lacks WeaponStatHandle")?;
    let table = h.object("DataTable").filter(|i| *i < 0).map(|i| pkg.import_path(i)).ok_or("stat handle has no DataTable")?;
    let row = h.text("RowName").ok_or("stat handle has no RowName")?.to_owned();
    let stats = crate::data::StatRow::from_props(&fn_pose::loadout_stats::data_table_row(src, &table, &row)?)?;
    let ability = crate::data::fire_ability(src, &item, &pkg)?;
    let ga_path = obj_path(&pkg, item.get("PrimaryFireAbility")).ok_or("item lacks PrimaryFireAbility")?;
    let ga_pkg = loader::package(src, ga_path.split('.').next().unwrap_or(&ga_path))?;
    let ga = props_of(&ga_pkg, ga_pkg.exports.iter().position(|e| e.object_name.starts_with("Default__")).ok_or("ability has no CDO")?).ok_or("ability CDO unreadable")?;
    let template = obj_path(&ga_pkg, ga.get("Prj_Athena_Grenade")).or_else(|| obj_path(&pkg, item.get("ProjectileTemplate"))).ok_or("no projectile class")?;
    let initial_speed = ga.float("InitialSpeed").ok_or("ability lacks InitialSpeed")? as f32;
    let gravity = ga.float("Athena_Grenade_Gravity").map(|v| v as f32);
    let prj = loader::package(src, template.split('.').next().unwrap_or(&template))?;
    let pmc = (0..prj.exports.len()).find(|&i| prj.export_class(i) == "ProjectileMovementComponent").and_then(|i| props_of(&prj, i)).ok_or("projectile lacks a movement component")?;
    let explode_tag = ability.selections.first().map(|s| s.application_tag.clone()).unwrap_or_default();
    let max_speed = pmc.float("MaxSpeed").map_or(0., |v| v as f32).max(initial_speed);
    let projectile = crate::data::ProjectileData { template: template.split('.').next().unwrap_or(&template).to_owned(), table: String::new(), initial_speed_row: "GA InitialSpeed".into(), max_speed_row: "SetVelocity raises MaxSpeed to the launch speed".into(), initial_speed, max_speed,
        gravity_scale: gravity.or_else(|| pmc.float("ProjectileGravityScale").map(|v| v as f32)), life_span: None,
        should_bounce: pmc.bool("bShouldBounce").unwrap_or(false), bounciness: pmc.float("Bounciness").map_or(0.6, |v| v as f32), friction: pmc.float("Friction").map_or(0.2, |v| v as f32),
        explosive: load_hand_grenade_def(src, &template, &explode_tag) };
    let pres = crate::data::Presentation { projectile_template: Some(template.clone()), ..Default::default() };
    Ok(GunHit { wid: pkg_path.to_owned(), row, stats, pres, ability, projectile: Some(projectile), perfect_aim_when_targeting_min_spread: true, trigger: String::new(), ammo: String::new(),
        rarity: item.text("Rarity").unwrap_or("").to_owned(), tier: String::new(), min_level: 0, ammo_path: String::new(), ability_path: ga_path, actor_path: obj_path(&pkg, item.get("WeaponActorClass")).unwrap_or_default(), stat_table: table.split('.').next().unwrap_or(&table).to_owned(), tracer: None, projectile_particles: vec![] })
}

pub fn def_of(gun: &GunHit) -> Option<&ExplosiveDef> { gun.projectile.as_ref()?.explosive.as_ref() }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Launch { pub pos: Vec3, pub vel: Vec3 }

pub fn launch(gun: &GunHit, world: &World, shot: &Shot, spread: &mut SpreadState, ctx: &FireContext, seed: i32) -> Option<Launch> {
    let pr = gun.projectile.as_ref()?;
    pr.explosive.as_ref()?;
    let aim = select_aim(gun, &world.props, shot.eye, shot.forward, ctx.muzzle, ctx.targeting);
    let mut rs = RandomStream::new(seed);
    let bullets = gun.stats.bullets_per_cartridge.max(1);
    let mut dir = aim.dir;
    for index in 0..bullets { dir = pellet_dir(gun, spread, ctx, aim.dir, index, &mut rs); spread.mark_fired(ctx.now); }
    spread.add_heat(&gun.stats, ctx.level);
    let range = gun.stats.trace_range_m();
    let target = trace_world(&world.props, aim.start, dir, range, 0.).map_or(aim.start + dir * range, |h| aim.start + dir * h.t);
    let start = ctx.muzzle.unwrap_or(aim.start);
    let toward = safe_normal(target - start);
    let toward = if toward == Vec3::ZERO { dir } else { toward };
    let toward = crate::shoot::add_pitch_offset(toward, gun.pres.projectile_pitch_offset);
    let speed = (pr.initial_speed.min(pr.max_speed)) / 100.;
    Some(Launch { pos: start, vel: toward * speed })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cause { Impact, Fuse, PawnBounce }

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Bounce { pos: Vec3, normal: Vec3, surface: u32, pawn: Option<u64> },
    Stopped { pos: Vec3 },
    Explode { pos: Vec3, normal: Vec3, surface: u32, cause: Cause, direct_object: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Flying {
    pub id: u64,
    pub instigator: u64,
    pub team: Option<u32>,
    pub pos: Vec3,
    pub vel: Vec3,
    pub age: f32,
    pub bounces: u32,
    pub stopped: bool,
    pub exploded: bool,
    pub travelled: f32,
    pub gravity_override: Option<f32>,
    pub overlapping: Vec<u64>,
}

pub const BOUNCE_STOP_CM_S: f32 = 5.;
pub const MAX_SIM_STEP: f32 = 0.05;

impl Flying {
    pub fn new(id: u64, instigator: u64, team: Option<u32>, l: Launch) -> Flying { Flying { id, instigator, team, pos: l.pos, vel: l.vel, age: 0., bounces: 0, stopped: false, exploded: false, travelled: 0., gravity_override: None, overlapping: vec![] } }

    fn limit(v: Vec3, max_speed_m: f32) -> Vec3 { if max_speed_m.is_finite() && max_speed_m > 0. && v.length() > max_speed_m { v.normalize() * max_speed_m } else { v } }

    pub fn step(&mut self, gun: &GunHit, world: &World, pawns: &[PawnTarget], dt: f32, gravity_m_s2: f32) -> Vec<Event> {
        let mut out = vec![];
        if self.exploded { return out; }
        let (Some(pr), Some(def)) = (gun.projectile.as_ref(), def_of(gun)) else { return out };
        let mut left = dt;
        while left > 1e-6 && !self.exploded {
            let h = left.min(MAX_SIM_STEP); left -= h;
            self.age += h;
            if !self.stopped { self.move_step(gun, pr, def, world, pawns, h, gravity_m_s2, &mut out); }
            if !self.exploded && self.age >= def.fuse_s && def.fuse_s > 0. {
                self.exploded = true;
                out.push(Event::Explode { pos: self.pos, normal: Vec3::Y, surface: SURFACE_DEFAULT, cause: Cause::Fuse, direct_object: 0 });
            }
        }
        out
    }

    fn pawn_overlaps(&mut self, pr: &crate::data::ProjectileData, def: &ExplosiveDef, pawns: &[PawnTarget]) {
        let r = def.capsule_radius_cm / 100.;
        let mut now = vec![];
        for p in pawns.iter().filter(|p| p.id != self.instigator && p.vitals.alive()) {
            let (a, b, pr_) = p.capsule_segment();
            let ab = b - a; let k = ((self.pos - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0., 1.);
            if (a + ab * k - self.pos).length() > pr_ + r { continue; }
            now.push(p.id);
            if self.overlapping.contains(&p.id) { continue; }
            if let Some(gs) = def.pawn_overlap_gravity { self.gravity_override = Some(gs); }
            let v = -self.vel;
            self.vel = Vec3::new(v.x, v.y.abs(), v.z) * pr.bounciness;
            self.bounces += 1;
        }
        self.overlapping = now;
    }

    #[allow(clippy::too_many_arguments)]
    fn move_step(&mut self, gun: &GunHit, pr: &crate::data::ProjectileData, def: &ExplosiveDef, world: &World, pawns: &[PawnTarget], h: f32, gravity_m_s2: f32, out: &mut Vec<Event>) {
        let max_speed = pr.max_speed / 100.;
        if def.kind == Kind::HandGrenade { self.pawn_overlaps(pr, def, pawns); }
        let g = Vec3::new(0., -gravity_m_s2 * self.gravity_override.unwrap_or_else(|| crate::shoot::projectile_gravity_scale(gun)), 0.);
        let v1 = Self::limit(self.vel + g * h, max_speed);
        let delta = (self.vel + v1) * 0.5 * h;
        let len = delta.length();
        if len <= 1e-7 { self.vel = v1; return; }
        let dir = delta / len;
        let radius = def.capsule_radius_cm / 100.;
        let centre = trace_world(&world.props, self.pos, dir, len + radius, 0.).map(|hh| ((hh.t - radius).max(0.), hh));
        let bottom = trace_world(&world.props, self.pos - Vec3::Y * (radius - 0.005).max(0.), dir, len, 0.).filter(|hh| hh.normal.y > 0.).map(|hh| (hh.t, hh));
        let world_hit = match (centre, bottom) { (Some(a), Some(b)) => Some(if b.0 < a.0 { b } else { a }), (a, b) => a.or(b) };
        let building = building_trace(self.pos, dir, world_hit.map_or(len, |w| w.0), radius);
        let opts = PawnFire { shooter: self.instigator, shooter_team: self.team, ..Default::default() };
        let limit = building.as_ref().map(|b| b.t).or(world_hit.map(|w| w.0)).unwrap_or(len);
        let pawn = if def.kind == Kind::HandGrenade { None } else { trace_pawns(pawns, &opts, self.pos, dir, limit, radius).into_iter().next() };
        let hit: Option<(f32, Vec3, u32, u64, bool)> = if let Some(p) = pawn { Some((p.t, p.normal, crate::shoot::SURFACE_PLAYER, pawns[p.pawn].id, true)) }
            else if let Some(b) = building { Some((b.t, b.normal, crate::shoot::surface_of(b.resource), b.object, false)) }
            else { world_hit.map(|(t, w): (f32, WorldHit)| (t, w.normal, w.prop.map_or(SURFACE_DEFAULT, |i| crate::surface::of_prop(&world.props[i])), w.prop.map_or(0, |i| fn_combat::harvest_fx::object_id(i, &world.props[i])), false)) };
        let Some((t_hit, normal, surface, object, is_pawn)) = hit.filter(|x| x.0 <= len) else {
            self.pos += delta; self.travelled += len; self.vel = v1; return;
        };
        let t = (t_hit - 0.001).max(0.);
        let frac = (t / len).clamp(0., 1.);
        self.pos += dir * t; self.travelled += t;
        let normal = if normal.length_squared() > 1e-8 { normal.normalize() } else { -dir };
        let impact = self.pos - normal * radius;
        if !pr.should_bounce {
            self.exploded = true; self.vel = Vec3::ZERO;
            let at = impact + normal * (def.impact_offset_cm / 100.);
            out.push(Event::Explode { pos: if def.explode_at_actor { self.pos } else { at }, normal, surface, cause: Cause::Impact, direct_object: object });
            return;
        }
        let vel = if frac > 1e-4 { Self::limit(self.vel + g * (h * frac), max_speed) } else { self.vel };
        let vdn = vel.dot(normal);
        let mut nv = vel;
        if vdn < 0. {
            let proj = normal * -vdn;
            nv += proj;
            nv *= (1. - pr.friction).clamp(0., 1.);
            nv += proj * pr.bounciness.max(0.);
            nv = Self::limit(nv, max_speed);
        }
        self.vel = nv;
        self.bounces += 1;
        let pawn_id = if is_pawn { Some(object) } else { None };
        out.push(Event::Bounce { pos: impact, normal, surface, pawn: pawn_id });
        if pawn_id.is_some() && def.explode_on_pawn_bounce {
            self.exploded = true;
            out.push(Event::Explode { pos: self.pos, normal, surface, cause: Cause::PawnBounce, direct_object: object });
            return;
        }
        if self.vel.length() * 100. < BOUNCE_STOP_CM_S { self.vel = Vec3::ZERO; self.stopped = true; out.push(Event::Stopped { pos: self.pos }); }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuildingCandidate { pub object: u64, pub point: Vec3 }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Blast {
    pub pos: Vec3,
    pub radius_m: f32,
    pub max_targets: i32,
    pub pawn_hits: Vec<PawnHit>,
    pub impacts: Vec<Impact>,
    pub considered: usize,
    pub obstructed: usize,
}

enum Target { Pawn(usize, Vec3), Prop(usize, Vec3), Building(u64, Vec3) }

fn obstructed(world: &World, from: Vec3, to: Vec3, own_prop: Option<usize>, own_building: Option<u64>) -> bool {
    let d = (to - from).length();
    if d < 1e-3 { return false; }
    let dir = (to - from) / d;
    let lim = (d - 0.02).max(0.);
    if let Some(w) = trace_world(&world.props, from, dir, lim, 0.) { if w.prop.is_none() || w.prop != own_prop { return true; } }
    if let Some(b) = building_trace(from, dir, lim, 0.) { if Some(b.object) != own_building { return true; } }
    false
}

fn prop_point(p: &fn_combat::Harvestable, c: Vec3) -> (Vec3, f32) {
    if let Some(h) = &p.hull {
        let mut best: Option<(Vec3, f32)> = None;
        for piece in &h.hulls {
            let d = piece.sdf(c).max(0.);
            let q = if d <= 0. { c } else { let n = piece.closest(c, Vec3::ZERO); n.point };
            if best.map_or(true, |b| d < b.1) { best = Some((q, d)); }
        }
        if let Some(b) = best { return b; }
    }
    let y = c.y.clamp(p.pos.y, p.pos.y + p.height);
    let flat = Vec3::new(c.x - p.pos.x, 0., c.z - p.pos.z);
    let r = flat.length();
    let q = if r <= p.radius { Vec3::new(c.x, y, c.z) } else { Vec3::new(p.pos.x, y, p.pos.z) + flat / r * p.radius };
    (q, (q - c).length())
}

pub fn explode(gun: &GunHit, world: &mut World, pawns: &mut [PawnTarget], instigator: u64, team: Option<u32>, buildings: &[BuildingCandidate], pos: Vec3, level: i32) -> Blast {
    let tag = def_of(gun).map_or(APPLICATION_EXPLODE, |d| d.explode_tag.as_str());
    let Some(sel) = gun.ability.selection(tag).cloned() else { return Blast { pos, ..Default::default() } };
    let radius = sel.range / 100.;
    let mut blast = Blast { pos, radius_m: radius, max_targets: sel.max_targets, ..Default::default() };
    if sel.effect_classes.is_empty() { return blast; }
    let mut cands: Vec<(f32, Target)> = vec![];
    for (i, p) in pawns.iter().enumerate() {
        if !p.vitals.alive() { continue; }
        if sel.exclude_instigator && p.id == instigator { continue; }
        if sel.exclude_pawn_friends && team.is_some() && team == Some(p.team) && p.id != instigator { continue; }
        let (a, b, r) = p.capsule_segment();
        let ab = b - a; let k = ((pos - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0., 1.); let q = a + ab * k;
        let d = (q - pos).length();
        if d - r > radius { continue; }
        let (_, hh) = p.capsule_m();
        let centre = p.feet + Vec3::Y * hh;
        cands.push(((d - r).max(0.), Target::Pawn(i, centre)));
    }
    for (i, p) in world.props.iter().enumerate() {
        if !p.alive() { continue; }
        if (Vec3::new(p.pos.x, pos.y, p.pos.z) - pos).length() > radius + p.radius.max(0.) + 30. { continue; }
        let (q, d) = prop_point(p, pos);
        if d <= radius { cands.push((d, Target::Prop(i, q))); }
    }
    for b in buildings {
        let d = (b.point - pos).length();
        if d <= radius { cands.push((d, Target::Building(b.object, b.point))); }
    }
    blast.considered = cands.len();
    if sel.exclude_obstructed_by_world {
        let before = cands.len();
        cands.retain(|(_, t)| match t {
            Target::Pawn(i, c) => { let hh = pawns[*i].capsule_m().1; [*c, *c + Vec3::Y * hh * 0.5, *c - Vec3::Y * hh * 0.5].iter().any(|q| !obstructed(world, pos, *q, None, None)) }
            Target::Prop(i, q) => !obstructed(world, pos, *q, Some(*i), None),
            Target::Building(o, q) => !obstructed(world, pos, *q, None, Some(*o)),
        });
        blast.obstructed = before - cands.len();
    }
    cands.sort_by(|a, b| a.0.total_cmp(&b.0));
    if sel.max_targets > 0 { cands.truncate(sel.max_targets as usize); }
    let s = gun.stats.clone();
    for (d, t) in cands {
        match t {
            Target::Pawn(i, q) => {
                let normal = safe_normal(q - pos);
                let ph = PawnTraceHit { pawn: i, t: d, normal, zone: Zone::Normal, bone: None };
                let (imp, hit) = hit_pawn(gun, pawns, ph, q, d, 0, level, crate::shoot::pawn_damage_gate());
                blast.impacts.push(imp);
                blast.pawn_hits.push(hit);
            }
            Target::Prop(i, q) => {
                let dmg = s.damage_at(DamageKind::Environmental, d * 100., level);
                let p = &mut world.props[i];
                let dmg = if p.invulnerable { 0. } else { dmg };
                p.health -= dmg; let destroyed = p.health <= 0.; if destroyed { p.gone_for = 0.; }
                let object = fn_combat::harvest_fx::object_id(i, p);
                let (surface, kind, res, proto, ppos) = (crate::surface::of_prop(p), p.kind, p.res, p.proto, p.pos);
                if destroyed { world.fx.events.push(FxEvent::Broke { object, pos: ppos, kind, resource: res, proto }); }
                world.hits += 1;
                blast.impacts.push(Impact { pos: q, normal: safe_normal(pos - q), surface, distance_m: d, prop: Some(i), object, damage: dmg, destroyed, zone: Zone::Normal, effect: None, sound: None });
            }
            Target::Building(o, q) => {
                let dmg = s.damage_at(DamageKind::Environmental, d * 100., level);
                let destroyed = building_damage(o, dmg, q);
                blast.impacts.push(Impact { pos: q, normal: safe_normal(pos - q), surface: SURFACE_DEFAULT, distance_m: d, prop: None, object: o, damage: dmg, destroyed, zone: Zone::Normal, effect: None, sound: None });
            }
        }
    }
    blast
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{ProjectileData, StatRow, TargetSelection, FireAbility, Presentation};

    fn def(kind: Kind) -> ExplosiveDef {
        ExplosiveDef { kind, template: String::new(), mesh: None, mesh_scale: Vec3::ONE, capsule_radius_cm: 6., capsule_half_height_cm: 6., trail: None, ribbon: None, trail_attached_ribbon: false, trail_lifetime: 0.,
            explosion_fx: None, explosion_water_fx: None, explosion_sound: None, explosion_water_sound: None, in_air_sound: None, in_air_fade_out: 0., bounce_fx: None, bounce_water_fx: None, bounce_sound: None,
            fuse_s: if kind == Kind::Rocket { 15. } else { 2.75 }, explode_on_pawn_bounce: kind == Kind::Grenade, impact_offset_cm: if kind == Kind::Rocket { 10. } else { 0. }, explode_at_actor: kind == Kind::Grenade,
            camera_shake: None, force_feedback_radii: (0., 0.), post_process: None, spin_rate_deg: 0.,
            explode_tag: APPLICATION_EXPLODE.into(), rotation_rate_deg: Vec3::ZERO, fuse_fx: None, pawn_overlap_gravity: if kind == Kind::HandGrenade { Some(4.) } else { None }, explosion_rot_pitch_deg: 0. }
    }
    fn gun(kind: Kind) -> GunHit {
        let rocket = kind == Kind::Rocket;
        let stats = StatRow { dmg: [110.; 4], env_dmg: [375.; 4], rng: [1., 1., 1., 25000.], base_level: 1, zone_normal: 1., zone_critical: 1., zone_light: 1., bullets_per_cartridge: 1, ..Default::default() };
        let pr = ProjectileData { template: String::new(), table: String::new(), initial_speed_row: String::new(), max_speed_row: String::new(), initial_speed: if rocket { 2000. } else { 2750. }, max_speed: if rocket { 2250. } else { 2750. },
            gravity_scale: Some(if rocket { 0. } else { 0.3 }), life_span: None, should_bounce: !rocket, bounciness: 0.4, friction: if rocket { 0.6 } else { 0.45 }, explosive: Some(def(kind)) };
        let sel = TargetSelection { shape: "EFortTargetSelectionShape::Sphere".into(), test_type: String::new(), primary_source: String::new(), range: 512., cone_yaw: 0., cone_pitch: 0., cone_min_radius: 1.,
            actor_type_filter: String::new(), exclude_pawn_friends: false, exclude_obstructed_by_world: true, max_targets: 12, application_tag: APPLICATION_EXPLODE.into(), exclude_instigator: false, effect_classes: vec!["GE".into()] };
        GunHit { stats, pres: Presentation::default(), ability: FireAbility { selections: vec![sel], ..Default::default() }, projectile: Some(pr), ..Default::default() }
    }

    #[test]
    fn launchers_load_their_explosive_data_from_the_install() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT") else { return };
        let paks = fn_pak::PakSet::open_install(&root).unwrap();
        let src = ClipSourceDir::Pak(&paks);
        let r = crate::data::load(&src, "/Game/Athena/Items/Weapons/WID_Launcher_Rocket_Athena_R_Ore_T03").unwrap();
        let d = def_of(&r).unwrap();
        assert_eq!((d.kind, d.fuse_s, d.impact_offset_cm, d.capsule_radius_cm, d.mesh_scale), (Kind::Rocket, 15., 10., 6., Vec3::splat(2.)));
        assert!(d.mesh.as_deref().unwrap().ends_with("S_PumpkinLauncher_Missile"), "{:?}", d.mesh);
        assert!(d.trail.as_deref().unwrap().contains("P_RocketTrail_Pumpkin_02") && d.explosion_fx.as_deref().unwrap().contains("P_PumpkinRPG_ExplodeGoop_01"));
        assert!(d.in_air_sound.as_deref().unwrap().contains("Pumpkin_InAirLoop_Athena_Cue") && d.explosion_sound.as_deref().unwrap().contains("Pumpkin_Proj_Impact_Athena_Cue"));
        let pr = r.projectile.as_ref().unwrap();
        assert_eq!((pr.initial_speed, pr.max_speed, pr.gravity_scale, pr.should_bounce), (2000., 2250., Some(0.), false));
        let ex = r.ability.selection(APPLICATION_EXPLODE).unwrap();
        assert_eq!((ex.range, ex.max_targets, ex.exclude_instigator, ex.exclude_pawn_friends, ex.exclude_obstructed_by_world), (512., 12, false, false, true));
        assert!(ex.effect_classes.iter().any(|e| e.contains("GE_Ranged_GenericDamage")));
        assert!(r.ability.selection(crate::data::APPLICATION_HIT).is_some() && r.ability.effect_classes.is_empty());
        let g = crate::data::load(&src, "/Game/Athena/Items/Weapons/WID_Launcher_Grenade_Athena_R_Ore_T03").unwrap();
        let d = def_of(&g).unwrap();
        assert_eq!((d.kind, d.fuse_s, d.explode_on_pawn_bounce), (Kind::Grenade, 2.75, true));
        assert!(d.bounce_sound.as_deref().unwrap().contains("Grenade_Projectile_Bounce_Cue") && d.explosion_fx.as_deref().unwrap().contains("P_GrenadeLauncher_Explosion"));
        let pr = g.projectile.as_ref().unwrap();
        assert_eq!((pr.initial_speed, pr.gravity_scale, pr.should_bounce, pr.bounciness, pr.friction), (2750., Some(0.3), true, 0.4, 0.45));
        let sh = def_of(&r).unwrap().camera_shake.clone().unwrap();
        assert_eq!((sh.inner_cm, sh.outer_cm, sh.duration, sh.blend_in, sh.blend_out, sh.loc[0], sh.loc[1], sh.loc[2]), (400., 850., 0.8, 0.05, 0.23, (20., 11.808002), (23.75, 18.432001), (27.5, 16.128)));
        assert!(sh.anim.is_none());
        let pp = def_of(&r).unwrap().post_process.clone().unwrap();
        assert_eq!((pp.blend_radius_cm, pp.color_contrast, pp.fade.len(), pp.fade_length), (700., Some(2.), 3, 1.));
        let t = pp.scene_color_tint.unwrap();
        assert!((t[0] - 10.).abs() < 1e-4 && (t[1] - 5.111391).abs() < 1e-4 && (t[2] - 1.457167).abs() < 1e-4, "{t:?}");
        assert!(pp.weight(0.) == 0. && (pp.weight(0.15015185) - 1.).abs() < 1e-4 && pp.weight(1.) == 0. && (pp.weight(0.5) - 0.9512047).abs() < 1e-4, "{}", pp.weight(0.5));
        assert!(pp.local_weight(0.) == 1. && (pp.local_weight(350.) - 0.5).abs() < 1e-6 && pp.local_weight(701.) == 0.);
        assert!(def_of(&g).unwrap().post_process.is_none());
    }

    #[test]
    fn hand_grenade_loads_its_ability_projectile_and_damage_row() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT") else { return };
        let paks = fn_pak::PakSet::open_install(&root).unwrap();
        let src = ClipSourceDir::Pak(&paks);
        let g = load_throwable(&src, "/Game/Athena/Items/Consumables/Grenade/Athena_Grenade").unwrap();
        assert!(g.stat_table.ends_with("UtilityItemDamage") && g.row == "Athena_Grenade", "{} {}", g.stat_table, g.row);
        assert_eq!((g.stats.dmg, g.stats.env_dmg, g.stats.rng, g.stats.base_level, g.stats.dmg_scale), ([100.; 4], [375.; 4], [500., 2000., 3000., 3000.], 0, 0.05));
        let pr = g.projectile.as_ref().unwrap();
        assert_eq!((pr.initial_speed, pr.max_speed, pr.gravity_scale, pr.should_bounce, pr.bounciness, pr.friction), (4000., 4000., Some(0.7), true, 0.3, 0.4));
        let d = def_of(&g).unwrap();
        assert_eq!((d.kind, d.fuse_s, d.capsule_radius_cm, d.mesh_scale, d.rotation_rate_deg, d.pawn_overlap_gravity, d.explosion_rot_pitch_deg), (Kind::HandGrenade, 2.75, 12., Vec3::splat(1.1), Vec3::new(180., 60., 480.), Some(4.), 90.));
        assert!(d.mesh.as_deref().unwrap().ends_with("S_Grenade_Single") && d.explosion_fx.as_deref().unwrap().contains("P_Grenade_Explosion_Blue"));
        assert!(d.explosion_sound.as_deref().unwrap().contains("Grenade_Explosion_Athena_Cue") && d.bounce_sound.as_deref().unwrap().contains("Grenade_Projectile_Bounce_Cue"));
        let (fx, at) = d.fuse_fx.clone().unwrap();
        assert!(fx.contains("P_Grenade_Fuse") && (at.z - 11.159852).abs() < 1e-3, "{fx} {at}");
        let sel = g.ability.selection(&d.explode_tag).unwrap();
        assert_eq!((sel.range, sel.max_targets, sel.exclude_instigator, sel.exclude_pawn_friends, sel.exclude_obstructed_by_world, sel.application_tag.as_str()), (500., 8, false, false, true, "Abilities.generic.GenericProjectile.Explode"));
        assert!(sel.effect_classes.iter().any(|e| e.contains("GE_M80_Damage")));
        let sh = d.camera_shake.clone().unwrap();
        assert_eq!((sh.inner_cm, sh.outer_cm, sh.duration), (200., 800., 0.));
        let a = sh.anim.unwrap();
        assert_eq!((a.blend_in, a.blend_out, a.play_rate, a.scale), (0., 0.2, 1., 1.));
        assert!((a.length - 0.5357075).abs() < 1e-5);
        assert!((a.offset(0.02244580).z - (417.75604 - 358.8825)).abs() < 1e-3 && a.offset(0.44913408).z.abs() < 1e-3 && a.offset(0.3).x == 0.);
        assert_eq!(a.pp_weight, 1.);
        let t0 = a.pp_tint_at(0.).unwrap();
        assert!((t0[0] - 0.112306356).abs() < 1e-5 && (t0[1] - 0.6668105).abs() < 1e-5 && t0[2] == 1., "{t0:?}");
        assert_eq!(a.pp_tint_at(0.2).unwrap(), [1., 1., 1.]);
        assert!(d.post_process.is_none());
    }

    #[test]
    fn falloff_numbers_match_the_loaded_rows() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT") else { return };
        let paks = fn_pak::PakSet::open_install(&root).unwrap();
        let src = ClipSourceDir::Pak(&paks);
        let r = |w: &str| crate::data::load(&src, &format!("/Game/Athena/Items/Weapons/{w}")).unwrap();
        for (w, dmg) in [("WID_Launcher_Rocket_Athena_R_Ore_T03", 110.), ("WID_Launcher_Rocket_Athena_VR_Ore_T03", 116.), ("WID_Launcher_Rocket_Athena_SR_Ore_T03", 121.),
                         ("WID_Launcher_Grenade_Athena_R_Ore_T03", 100.), ("WID_Launcher_Grenade_Athena_VR_Ore_T03", 105.), ("WID_Launcher_Grenade_Athena_SR_Ore_T03", 110.)] {
            let g = r(w);
            let radius = g.ability.selection(APPLICATION_EXPLODE).unwrap().range;
            for d in [0., 1., 100., 256., radius - 1., radius] {
                assert_eq!(g.stats.damage_at(DamageKind::Combat, d, 1), dmg, "{w} at {d} cm");
                assert_eq!(g.stats.damage_at(DamageKind::Environmental, d, 1), 375., "{w} env at {d} cm");
            }
        }
        let h = load_throwable(&src, "/Game/Athena/Items/Consumables/Grenade/Athena_Grenade").unwrap();
        for d in [0., 250., 499., 500.] {
            assert!((h.stats.damage_at(DamageKind::Combat, d, 1) - 105.).abs() < 1e-4 && (h.stats.damage_at(DamageKind::Environmental, d, 1) - 393.75).abs() < 1e-3, "grenade at {d} cm");
            assert_eq!(h.stats.damage_at(DamageKind::Combat, d, 0), 100.);
        }
    }

    #[test]
    fn hand_grenade_passes_through_a_pawn_reflecting_back_and_up_then_falls_at_gravity_4() {
        let mut g = gun(Kind::HandGrenade);
        if let Some(p) = g.projectile.as_mut() { p.bounciness = 0.3; p.friction = 0.4; p.gravity_scale = Some(0.7); p.initial_speed = 4000.; p.max_speed = 4000.; if let Some(d) = p.explosive.as_mut() { d.capsule_radius_cm = 12.; } }
        let w = World::default();
        let pawns = [PawnTarget::new(9, 1, Vec3::new(0., 0., -3.), crate::shoot::Vitals::new(100., 0.))];
        let mut f = Flying::new(1, 7, None, Launch { pos: Vec3::new(0., 1., 0.), vel: Vec3::new(0., 0., -20.) });
        for _ in 0..30 { f.step(&g, &w, &pawns, 1. / 120., 28.); if f.gravity_override.is_some() { break; } }
        assert_eq!(f.gravity_override, Some(4.));
        assert!((f.vel.z - 20. * 0.3).abs() < 1e-3 && f.vel.x == 0., "{}", f.vel);
        assert_eq!(f.bounces, 1);
    }

    #[test]
    fn rocket_flies_straight_at_2000_and_explodes_on_the_ground_offset_by_the_impact_offset() {
        let g = gun(Kind::Rocket);
        let w = World::default();
        let mut f = Flying::new(1, 7, None, Launch { pos: Vec3::new(0., 1.5, 0.), vel: Vec3::new(0., -0.5, -20.).normalize() * 20. });
        let mut ev = vec![];
        for _ in 0..600 { ev.extend(f.step(&g, &w, &[], 1. / 120., 28.)); if f.exploded { break; } }
        let Some(Event::Explode { pos, cause, .. }) = ev.last().cloned() else { panic!("{ev:?}") };
        assert_eq!(cause, Cause::Impact);
        assert!((pos.y - 0.1).abs() < 0.01, "{pos}");
        assert!((f.vel.length()).abs() < 1e-6);
    }

    #[test]
    fn rocket_fuse_is_15_seconds() {
        let g = gun(Kind::Rocket);
        let w = World::default();
        let mut f = Flying::new(1, 7, None, Launch { pos: Vec3::new(0., 100., 0.), vel: Vec3::new(0., 0., -20.) });
        let mut t = 0f32;
        while !f.exploded && t < 20. { f.step(&g, &w, &[], 1. / 60., 28.); t += 1. / 60.; }
        assert!((t - 15.).abs() < 0.05, "{t}");
        assert!((f.travelled - 300.).abs() < 1., "{}", f.travelled);
    }

    #[test]
    fn grenade_bounces_loses_energy_and_explodes_on_the_fuse() {
        let g = gun(Kind::Grenade);
        let w = World::default();
        let mut f = Flying::new(1, 7, None, Launch { pos: Vec3::new(0., 1.5, 0.), vel: Vec3::new(0., 0., -27.5) });
        let mut ev = vec![];
        let mut t = 0f32;
        while !f.exploded && t < 5. { ev.extend(f.step(&g, &w, &[], 1. / 120., 28.)); t += 1. / 120.; }
        assert!(ev.iter().any(|e| matches!(e, Event::Bounce { .. })), "{ev:?}");
        assert!((t - 2.75).abs() < 0.02, "{t}");
        assert!(f.pos.y >= 0.05);
    }

    #[test]
    fn explosion_hits_the_instigator_flat_110_inside_512_and_nothing_outside() {
        let g = gun(Kind::Rocket);
        let mut w = World::default();
        let mut pawns = vec![PawnTarget::new(7, 0, Vec3::new(0., 0., 0.), crate::shoot::Vitals::new(100., 50.)), PawnTarget::new(8, 1, Vec3::new(0., 0., -7.), crate::shoot::Vitals::new(100., 0.))];
        let b = explode(&g, &mut w, &mut pawns, 7, Some(0), &[], Vec3::new(0., 0.1, -1.), 1);
        assert_eq!(b.pawn_hits.len(), 1);
        assert_eq!(b.pawn_hits[0].id, 7);
        assert!((b.pawn_hits[0].damage - 110.).abs() < 1e-3);
        assert_eq!(pawns[0].vitals.shield, 0.);
        assert!((pawns[0].vitals.health - 40.).abs() < 1e-3);
        assert_eq!(pawns[1].vitals.health, 100.);
    }
}
