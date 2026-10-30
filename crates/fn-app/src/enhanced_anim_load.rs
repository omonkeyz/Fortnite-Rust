use fn_animgraph::enhanced_anim as ea;
use fn_pose::{ClipSourceDir, ClipStore, FnClip, SampleMode};
use std::path::PathBuf;
use std::sync::Mutex;

pub const DIR: &str = "anim-enhanced";

struct Loaded { clips: ClipStore, lengths: Vec<(String, f32)>, missing: Vec<String> }

#[derive(Default)]
struct State { load: Option<std::sync::mpsc::Receiver<Result<Loaded, String>>>, data: Option<Loaded>, started: bool, installed: bool }
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(State::default)) }

pub fn ensure_loading(install: Option<PathBuf>, roots: &[PathBuf]) {
    if with(|s| s.started) { return; }
    let Some(dir) = install else { return };
    let Some(folder) = roots.iter().map(|r| r.join(DIR)).find(|d| d.join("index.json").is_file()) else { with(|s| s.started = true); eprintln!("[enhanced-anim] no `{DIR}` folder in the asset roots: Enhanced mode keeps the 1.8 animations"); return };
    let (tx, rx) = std::sync::mpsc::channel();
    with(|s| { s.started = true; s.load = Some(rx); });
    std::thread::Builder::new().name("enhanced-anim-load".into()).spawn(move || { let _ = tx.send(load(&dir, &folder)); }).ok();
}

fn load(install: &std::path::Path, folder: &std::path::Path) -> Result<Loaded, String> {
    let paks = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
    let skel = fn_pose::clip::load_player_skeleton_info(&ClipSourceDir::Pak(&paks))?;
    let index: serde_json::Value = serde_json::from_slice(&std::fs::read(folder.join("index.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut clips = ClipStore::new();
    let mut lengths = vec![];
    for c in index["clips"].as_array().into_iter().flatten() {
        let (Some(name), Some(file)) = (c["name"].as_str(), c["file"].as_str()) else { continue };
        let bytes = std::fs::read(folder.join(file)).map_err(|e| format!("{file}: {e}"))?;
        let fa = fn_anim::FnAnim::read(&bytes).map_err(|e| format!("{file}: {e}"))?;
        lengths.push((ea::key(name), fa.length));
        clips.insert(&ea::key(name), FnClip::from_fnanim(&fa, &skel, SampleMode::Ue));
    }
    let missing = ea::all_clips().into_iter().filter(|n| !clips.clips.contains_key(&ea::key(n))).collect();
    Ok(Loaded { clips, lengths, missing })
}

fn receive_and_install(s: &mut State, scene: &mut crate::scene::Scene) {
    if let Some(rx) = &s.load {
        if let Ok(r) = rx.try_recv() {
            s.load = None;
            match r {
                Ok(d) => { if !d.missing.is_empty() { eprintln!("[enhanced-anim] missing clips: {}", d.missing.join(" ")); } s.data = Some(d); }
                Err(e) => eprintln!("[enhanced-anim] unavailable: {e}"),
            }
        }
    }
    if s.installed { return; }
    let Some(d) = s.data.take() else { return };
    s.installed = true;
    let n = d.clips.clips.len();
    let base = std::mem::replace(&mut scene.character.clips, Box::new(ClipStore::new()));
    scene.character.clips = Box::new(EnhancedClips { base, extra: d.clips, dead: DeadClip { st: dead_state() } });
    ea::set_clip_lengths(d.lengths);
    ea::set_dead_source_ready(true);
    eprintln!("[enhanced-anim] {n} Fortnite 30.0 clips (tactical sprint, slide, hurdle, clamber) added to the character library");
}

pub fn prebuild_step(scene: &mut crate::scene::Scene) -> bool { with(|s| { receive_and_install(s, scene); s.load.is_none() }) }

pub fn frame(scene: &mut crate::scene::Scene) { with(|s| receive_and_install(s, scene)); }

pub fn orbit(c: &mut fn_render::camera::CameraParams, sim: &fn_game::GameSim) {
    static DEG: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    let Some(deg) = *DEG.get_or_init(|| fn_live::var("FN_ENH_ORBIT").ok().and_then(|v| v.trim().parse::<f32>().ok())) else { return };
    let pivot = glam::Vec3::new(sim.x, sim.y - fn_game::EYE + 1.0, sim.z);
    let q = glam::Quat::from_rotation_y(deg.to_radians());
    c.eye = pivot + q * (c.eye - pivot);
    c.forward = q * c.forward;
    c.up = q * c.up;
}

#[derive(Default)]
struct DeadState { hist: Vec<std::collections::HashMap<String, (glam::Vec3, glam::Quat)>>, dt: f32, snap: u64, dst_id: u64, dst: std::collections::HashMap<String, (glam::Vec3, glam::Quat)>, src: std::collections::HashMap<String, DeadBone> }

#[derive(Clone, Copy)]
struct DeadBone { p: glam::Vec3, q: glam::Quat, v: glam::Vec3, w: glam::Vec3, hl_v: glam::Vec3, hl_w: glam::Vec3 }

fn decay_half_life(diff: glam::Vec3, vel: glam::Vec3) -> glam::Vec3 {
    let hl = ea::DEAD_HALF_LIFE;
    let c = |d: f32, v: f32| { let v = if v.abs() < 1e-4 { 1e-4f32.copysign(v) } else { v }; (hl * (d / v)).clamp(ea::DEAD_HALF_LIFE_MIN, ea::DEAD_HALF_LIFE_MAX) };
    glam::Vec3::new(c(diff.x, vel.x), c(diff.y, vel.y), c(diff.z, vel.z))
}

fn decay_offset(hl: glam::Vec3, t: f32) -> glam::Vec3 {
    let k = |h: f32| h / std::f32::consts::LN_2 * (1. - (-t.max(0.) / h).exp2());
    glam::Vec3::new(k(hl.x), k(hl.y), k(hl.z))
}

fn dead_state() -> std::sync::Arc<Mutex<DeadState>> {
    static S: std::sync::OnceLock<std::sync::Arc<Mutex<DeadState>>> = std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Arc::new(Mutex::new(DeadState::default()))).clone()
}

pub struct DeadClip { st: std::sync::Arc<Mutex<DeadState>> }

fn snapshot(s: &mut DeadState) {
    s.src.clear();
    let Some(cur) = s.hist.last() else { return };
    let prev = if s.hist.len() >= 2 { Some(&s.hist[s.hist.len() - 2]) } else { None };
    let dt = s.dt.max(1e-4);
    let vmax = ea::DEAD_MAX_TRANSLATION_VELOCITY_CM * 0.01;
    let wmax = ea::DEAD_MAX_ROTATION_VELOCITY_DEG.to_radians();
    for (k, (p, q)) in cur {
        let (v, w) = match prev.and_then(|m| m.get(k)) {
            Some((pp, pq)) => {
                let v = ((*p - *pp) / dt).clamp_length_max(vmax);
                let pq = if pq.dot(*q) < 0. { -*pq } else { *pq };
                let w = ((*q * pq.inverse()).to_scaled_axis() / dt).clamp_length_max(wmax);
                (v, w)
            }
            None => (glam::Vec3::ZERO, glam::Vec3::ZERO),
        };
        let (hl_v, hl_w) = match s.dst.get(k) {
            Some((dp, dq)) => {
                let dq = if dq.dot(*q) < 0. { -*dq } else { *dq };
                (decay_half_life(*dp - *p, v), decay_half_life((dq * q.inverse()).to_scaled_axis(), w))
            }
            None => (glam::Vec3::splat(ea::DEAD_HALF_LIFE_MIN), glam::Vec3::splat(ea::DEAD_HALF_LIFE_MIN)),
        };
        s.src.insert(k.clone(), DeadBone { p: *p, q: *q, v, w, hl_v, hl_w });
    }
}

impl fn_animgraph::ClipSource for DeadClip {
    fn sample_bone(&self, bone: &str, t: f32, _looping: bool, _bind: fn_animgraph::BoneXform) -> Option<fn_animgraph::BoneXform> {
        let mut s = self.st.lock().unwrap_or_else(|p| p.into_inner());
        let id = ea::dead_request_id();
        if s.snap != id { snapshot(&mut s); s.snap = id; }
        let b = *s.src.get(bone)?;
        Some((b.p + b.v * decay_offset(b.hl_v, t), (glam::Quat::from_scaled_axis(b.w * decay_offset(b.hl_w, t)) * b.q).normalize()))
    }
    fn is_additive(&self) -> bool { false }
    fn duration(&self) -> f32 { 1. }
    fn has_bone(&self, bone: &str) -> bool { self.st.lock().map(|s| s.src.contains_key(bone) || s.hist.last().is_some_and(|h| h.contains_key(bone))).unwrap_or(false) }
}

pub struct EnhancedClips { pub base: Box<dyn fn_animgraph::ClipLibrary + Send>, pub extra: ClipStore, dead: DeadClip }
impl fn_animgraph::ClipLibrary for EnhancedClips {
    fn clip(&self, name: &str) -> Option<&dyn fn_animgraph::ClipSource> {
        if name == ea::DEAD_BLEND_KEY { return Some(&self.dead); }
        self.base.clip(name).or_else(|| self.extra.clip(name))
    }
}

pub fn observe(rig: &fn_pose::CharacterRig, pose: &fn_pose::CharacterPose, dt: f32) {
    if !ea::dead_source_ready() { return; }
    let mut m = std::collections::HashMap::new();
    for (part, res) in rig.parts.iter().zip(&pose.parts) {
        for (n, x) in part.tree.names.iter().zip(&res.bone_local) { m.entry(n.to_lowercase()).or_insert(*x); }
    }
    let st = dead_state();
    let mut s = st.lock().unwrap_or_else(|p| p.into_inner());
    s.hist.push(m);
    if s.hist.len() > 2 { s.hist.remove(0); }
    s.dt = dt;
}

fn dead_target(n: &fn_animgraph::Node) -> Option<&fn_animgraph::Node> {
    use fn_animgraph::Node;
    match n {
        Node::Mix(v) if v.len() == 2 && matches!(&v[0].0, Node::Clip { name, .. } if name == ea::DEAD_BLEND_KEY) => Some(&v[1].0),
        Node::Mix(v) => v.iter().find_map(|(c, _)| dead_target(c)),
        _ => None,
    }
}

pub fn capture_dst(rig: &fn_pose::CharacterRig, tree: &fn_animgraph::Node, clips: &dyn fn_animgraph::ClipLibrary) {
    if !ea::dead_source_ready() { return; }
    let id = ea::dead_request_id();
    let st = dead_state();
    {
        let s = st.lock().unwrap_or_else(|p| p.into_inner());
        if s.dst_id == id || s.snap == id { return; }
    }
    let Some(target) = dead_target(tree) else { return };
    let mut pose = fn_pose::CharacterPose::default();
    rig.evaluate_into(target, clips, &mut pose);
    let mut m = std::collections::HashMap::new();
    for (part, res) in rig.parts.iter().zip(&pose.parts) {
        for (n, x) in part.tree.names.iter().zip(&res.bone_local) { m.entry(n.to_lowercase()).or_insert(*x); }
    }
    let mut s = st.lock().unwrap_or_else(|p| p.into_inner());
    s.dst = m;
    s.dst_id = id;
}
