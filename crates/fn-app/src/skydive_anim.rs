use fn_pose::{ClipSourceDir, ClipStore, FnClip, SampleMode};
use fn_render::{ModelId, SkinnedPrimitive};
use glam::{Mat4, Quat, Vec3, Vec4};
use std::collections::HashMap;
use std::sync::Mutex;

pub const GLIDER_MESH: &str = "/Game/Gadgets/Assets/VinderTech_GliderChute/Meshes/SK_M_VinderTech_GliderChute";
pub const GLIDER_ANIM_DIR: &str = "/Game/Animation/Game/MainPlayer/Parachute/Medium/Female";
pub const GLIDER_CLOSED: &str = "Skydive_HangGlider_Glide_Closed_GLIDER";
pub const GLIDER_INTO: &str = "Skydive_HangGlider_Glide_Into_GLIDER";
pub const GLIDER_IDLE: [(&str, f32); 3] = [("Skydive_HangGlider_Glide_Left_GLIDER", -7.), ("Skydive_HangGlider_Glide_GLIDER", 0.), ("Skydive_HangGlider_Glide_Right_GLIDER", 7.)];
pub const GLIDER_CROSSFADE: f32 = 0.2;
pub const ATTACH_BONE: &str = "weapon_r";

#[derive(Clone, Copy, Debug)]
struct Trs { t: Vec3, r: Quat, s: Vec3 }
impl Trs { fn mat(&self) -> Mat4 { Mat4::from_scale_rotation_translation(self.s, self.r, self.t) } }
struct GliderAnim { len: f32, frames: usize, tracks: Vec<Option<usize>>, anim: fn_anim::FnAnim }
pub struct GliderData { prims: Vec<SkinnedPrimitive>, parents: Vec<i32>, refs: Vec<Trs>, inv_ref_cs: Vec<Mat4>, anims: Vec<GliderAnim>, into_len: f32 }
pub struct Data { clips: Option<ClipStore>, glider: Option<GliderData>, notes: Vec<String> }

struct Glider { model: ModelId }

#[derive(Default)]
struct State { load: Option<std::sync::mpsc::Receiver<Result<Data, String>>>, data: Option<Data>, failed: bool, installed: bool, glider: Option<Glider>, open_t: Option<f32> }
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(State::default)) }

pub fn ensure_loading(install: impl FnOnce() -> Option<std::path::PathBuf>) {
    if !with(|s| s.load.is_none() && s.data.is_none() && !s.failed && !s.installed) { return; }
    let Some(dir) = install() else { return };
    let (tx, rx) = std::sync::mpsc::channel();
    with(|s| s.load = Some(rx));
    std::thread::Builder::new().name("skydive-anim-load".into()).spawn(move || { let _ = tx.send(load(&dir)); }).ok();
}

fn pkg_of(r: &str) -> &str { r.split('.').next().unwrap_or(r) }

pub fn load(dir: &std::path::Path) -> Result<Data, String> {
    let paks = fn_pak::PakSet::open_install(dir).map_err(|e| e.to_string())?;
    let src = ClipSourceDir::Pak(&paks);
    let mut notes = vec![];
    let mut skeletons: HashMap<String, fn_anim::SkeletonInfo> = HashMap::new();
    let seq = |p: &str, skeletons: &mut HashMap<String, fn_anim::SkeletonInfo>| -> Result<(String, FnClip), String> {
        let pkg = fn_pose::loader::package(&src, pkg_of(p))?;
        let source = fn_anim::AnimSequence::parse(&pkg).map_err(|e| format!("{p}: {e}"))?;
        let skeleton = pkg_of(source.skeleton_path.as_deref().ok_or_else(|| format!("{p}: no skeleton"))?).to_owned();
        if !skeletons.contains_key(&skeleton) { skeletons.insert(skeleton.clone(), fn_anim::SkeletonInfo::from_package(&fn_pose::loader::package(&src, &skeleton)?).map_err(|e| e.to_string())?); }
        Ok((skeleton.clone(), FnClip::from_sequence(&source, &skeletons[&skeleton], SampleMode::Ue)))
    };
    let mut clips = ClipStore::new();
    for p in fn_animgraph::skydive_clips().into_iter().chain(fn_animgraph::dbno_clips()) {
        match seq(&p, &mut skeletons) {
            Ok((sk, c)) if sk == fn_pose::loadout::PLAYER_SKELETON => clips.insert(&p, c),
            Ok((sk, _)) => notes.push(format!("{p}: skeleton {sk}, not the player's")),
            Err(e) => notes.push(e),
        }
    }
    let glider = load_glider(&src, &mut skeletons, &mut notes).map_err(|e| notes.push(format!("glider unavailable: {e}"))).ok();
    Ok(Data { clips: Some(clips), glider, notes })
}

fn load_glider(src: &ClipSourceDir, skeletons: &mut HashMap<String, fn_anim::SkeletonInfo>, notes: &mut Vec<String>) -> Result<GliderData, String> {
    let mpkg = fn_pose::loader::package(src, GLIDER_MESH)?;
    let mesh = fn_mesh::SkeletalMesh::parse(&mpkg).map_err(|e| format!("{GLIDER_MESH}: {e}"))?;
    let (prims, bones) = crate::build_anim::mesh_prims(src, GLIDER_MESH, true, None, notes)?;
    let bones = bones.unwrap_or_else(|| mesh.ref_skeleton.clone());
    let parents: Vec<i32> = bones.iter().map(|b| b.parent).collect();
    let refs: Vec<Trs> = bones.iter().map(|b| Trs { t: Vec3::from(b.ref_pos), r: Quat::from_xyzw(b.ref_rot[0], b.ref_rot[1], b.ref_rot[2], b.ref_rot[3]).normalize(), s: Vec3::from(b.ref_scale) }).collect();
    let inv_ref_cs = component_space(&parents, &refs.iter().map(Trs::mat).collect::<Vec<_>>()).iter().map(|m| m.inverse()).collect();
    let mut anims = vec![];
    let mut into_len = 0.;
    for n in [GLIDER_CLOSED, GLIDER_INTO].into_iter().chain(GLIDER_IDLE.iter().map(|x| x.0)) {
        let p = format!("{GLIDER_ANIM_DIR}/{n}");
        let pkg = fn_pose::loader::package(src, &p)?;
        let source = fn_anim::AnimSequence::parse(&pkg).map_err(|e| format!("{p}: {e}"))?;
        let skeleton = pkg_of(source.skeleton_path.as_deref().ok_or_else(|| format!("{p}: no skeleton"))?).to_owned();
        if !skeletons.contains_key(&skeleton) { skeletons.insert(skeleton.clone(), fn_anim::SkeletonInfo::from_package(&fn_pose::loader::package(src, &skeleton)?).map_err(|e| e.to_string())?); }
        let anim = fn_anim::FnAnim::from_sequence(&source, &skeletons[&skeleton]);
        if n == GLIDER_INTO { into_len = anim.length; }
        let tracks = bones.iter().map(|b| anim.bones.iter().position(|fb| fb.name.eq_ignore_ascii_case(&b.name))).collect();
        anims.push(GliderAnim { len: anim.length, frames: anim.num_frames.max(1), tracks, anim });
    }
    Ok(GliderData { prims, parents, refs, inv_ref_cs, anims, into_len })
}

fn component_space(parents: &[i32], locals: &[Mat4]) -> Vec<Mat4> {
    let mut cs: Vec<Mat4> = Vec::with_capacity(locals.len());
    for (i, l) in locals.iter().enumerate() { let m = if parents[i] >= 0 { cs[parents[i] as usize] * *l } else { *l }; cs.push(m); }
    cs
}

fn sample(g: &GliderData, a: usize, t: f32, looping: bool) -> Vec<Trs> {
    let an = &g.anims[a];
    let tt = if looping && an.len > 0. { t.rem_euclid(an.len) } else { t.clamp(0., an.len.max(0.)) };
    let fpos = if an.frames > 1 && an.len > 0. { tt / an.len * (an.frames - 1) as f32 } else { 0. };
    let i0 = (fpos.floor() as usize).min(an.frames - 1);
    let i1 = (i0 + 1).min(an.frames - 1);
    let w = if i0 >= an.frames - 1 || an.anim.step { 0. } else { fpos - i0 as f32 };
    g.refs.iter().enumerate().map(|(b, r)| {
        let Some(fb) = an.tracks[b].map(|k| &an.anim.bones[k]) else { return *r };
        let k3 = |k: &Vec<[f32; 3]>| { let a = Vec3::from(k[i0.min(k.len() - 1)]); let c = Vec3::from(k[i1.min(k.len() - 1)]); a.lerp(c, w) };
        let mut t3 = fb.pos.as_ref().map_or(r.t, k3);
        if b > 0 {
            match fb.retarget {
                fn_anim::skeleton::TranslationRetarget::Skeleton => t3 = r.t,
                fn_anim::skeleton::TranslationRetarget::AnimationScaled => { let l = Vec3::from(fb.retarget_source).length(); if l > 1e-5 { t3 *= r.t.length() / l; } }
                _ => {}
            }
        }
        let q = fb.rot.as_ref().map_or(r.r, |k| { let a = Quat::from_array(k[i0.min(k.len() - 1)]).normalize(); let c = Quat::from_array(k[i1.min(k.len() - 1)]).normalize(); nlerp(a, c, w) });
        let s = fb.scale.as_ref().map_or(r.s, k3);
        Trs { t: t3, r: q, s }
    }).collect()
}

fn nlerp(a: Quat, b: Quat, w: f32) -> Quat { let b = if a.dot(b) < 0. { -b } else { b }; (a * (1. - w) + b * w).normalize() }

fn blend(a: &[Trs], b: &[Trs], w: f32) -> Vec<Trs> { a.iter().zip(b).map(|(x, y)| Trs { t: x.t.lerp(y.t, w), r: nlerp(x.r, y.r, w), s: x.s.lerp(y.s, w) }).collect() }

fn hermite(t: f32) -> f32 { let t = t.clamp(0., 1.); t * t * (3. - 2. * t) }

fn glider_pose(g: &GliderData, t: f32, yaw_delta: f32) -> Vec<Trs> {
    let x = yaw_delta.clamp(-7., 7.) / 7.;
    let idle = || { let c = sample(g, 3, t, true); if x < 0. { blend(&c, &sample(g, 2, t, true), -x) } else { blend(&c, &sample(g, 4, t, true), x) } };
    if t < GLIDER_CROSSFADE { return blend(&sample(g, 0, 0., true), &sample(g, 1, t, false), hermite(t / GLIDER_CROSSFADE)); }
    if t < g.into_len { return sample(g, 1, t, false); }
    if t < g.into_len + GLIDER_CROSSFADE { return blend(&sample(g, 1, g.into_len, false), &idle(), hermite((t - g.into_len) / GLIDER_CROSSFADE)); }
    idle()
}

fn ue_to_render() -> Mat4 { Mat4::from_cols(Vec4::new(0.01, 0., 0., 0.), Vec4::new(0., 0., 0.01, 0.), Vec4::new(0., 0.01, 0., 0.), Vec4::W) }

fn skin(g: &GliderData, pose: &[Trs]) -> Vec<Mat4> {
    let cs = component_space(&g.parents, &pose.iter().map(Trs::mat).collect::<Vec<_>>());
    let c = ue_to_render();
    let ci = c.inverse();
    cs.iter().zip(&g.inv_ref_cs).map(|(m, i)| c * *m * *i * ci).collect()
}

fn receive_and_install(s: &mut State, scene: &mut crate::scene::Scene) {
    if let Some(rx) = &s.load { if let Ok(r) = rx.try_recv() { s.load = None; match r { Ok(d) => { for n in &d.notes { eprintln!("[skydive-anim] {n}"); } eprintln!("[skydive-anim] skydive / glider animation set and glider mesh loaded"); s.data = Some(d); } Err(e) => { eprintln!("[skydive-anim] unavailable: {e}"); s.failed = true; } } } }
    if let Some(d) = s.data.as_mut() {
        if !s.installed {
            s.installed = true;
            if let Some(extra) = d.clips.take() {
                let base = std::mem::replace(&mut scene.character.clips, Box::new(ClipStore::new()));
                eprintln!("[skydive-anim] {} skydive clips added to the character library", extra.clips.len());
                scene.character.clips = Box::new(crate::weapon_runtime::LayeredClips { base, extra });
            }
            if let Some(gd) = d.glider.as_ref() { s.glider = Some(Glider { model: scene.renderer.create_model(&gd.prims, gd.refs.len()) }); }
        }
    }
}

pub fn prebuild_step(scene: &mut crate::scene::Scene) -> bool {
    with(|s| { receive_and_install(s, scene); s.load.is_none() })
}

pub fn frame(scene: &mut crate::scene::Scene, sim: &fn_game::GameSim, dt: f32) {
    with(|s| {
        receive_and_install(s, scene);
        let open = sim.controller.component().is_parachute_open() && !sim.on_bus();
        if !open { s.open_t = None; return; }
        let t = s.open_t.map_or(0., |t| t + dt.max(0.));
        s.open_t = Some(t);
        if sim.cam_view.hide_pawn { return; }
        let (Some(g), Some(d)) = (s.glider.as_mut(), s.data.as_ref().and_then(|d| d.glider.as_ref())) else { return };
        let Some(bi) = scene.character.rig.parts.first().and_then(|p| p.tree.bone_index(ATTACH_BONE)) else { return };
        let Some(bone) = scene.character.pose.parts.first().and_then(|p| p.bone_mesh.get(bi).copied()) else { return };
        let joints: Vec<Mat4> = skin(d, &glider_pose(d, t, sim.anim.vars.yaw_delta_smoothed)).iter().map(|k| bone * *k).collect();
        scene.renderer.update_model(g.model, crate::scene::pawn_matrix(sim), &joints);
        scene.models.push(g.model);
    });
}

#[cfg(test)]
mod tests {
    #[test] fn skydive_set_and_glider_load() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT").map(std::path::PathBuf::from) else { return };
        let d = super::load(&root).unwrap();
        for n in &d.notes { eprintln!("note: {n}"); }
        let clips = d.clips.as_ref().unwrap();
        for c in fn_animgraph::skydive_clips() { assert!(clips.clips.contains_key(&c), "{c}"); }
        let g = d.glider.as_ref().expect("glider");
        assert!(!g.prims.is_empty() && g.into_len > 4.);
        let id = super::skin(g, &g.refs);
        assert!(id.iter().all(|m| (*m - glam::Mat4::IDENTITY).abs().to_cols_array().iter().all(|v| *v < 1e-3)), "the reference pose skins to identity");
        for t in [0., 0.1, 0.5, 1., 2., 4., 4.8, 5.5] {
            let pose = super::glider_pose(g, t, 0.);
            let cs = super::component_space(&g.parents, &pose.iter().map(super::Trs::mat).collect::<Vec<_>>());
            let ext = cs.iter().map(|m| m.w_axis.truncate().length()).fold(0f32, f32::max);
            eprintln!("GLIDER t {t:.2}: farthest bone {ext:.0} cm from the root");
            assert!(ext < 600., "t {t}: a bone {ext} cm away (stretched)");
        }
    }
}
