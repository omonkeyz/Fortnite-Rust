use crate::pickups_dxbc::Dxbc;
use fn_anim::{AnimSequence, BoneTransform, SkeletonInfo};
use fn_fx::view::ViewParams;
use fn_render::fx::{FxFrame, FxRenderer};
use fn_render::{ModelId, Renderer};
use glam::{Mat3, Mat4, Quat, Vec3, Vec4};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub const RESOUT_PLAY_LENGTH_S: f32 = 1.75;
pub const LOCAL_SHELL_SLOT: u8 = u8::MAX;
pub const RESOUT_LEG_SPACE: f32 = -10.;
pub const RESOUT_HEAD_SPACE: f32 = 40.;
pub const HEAD_SOCKET: &str = "FX_Head";
pub const ROOT_SOCKET: &str = "Root_Effect";
pub const Z_HEIGHT: [(f32, f32, f32, f32, bool); 3] = [(0., 0., 0., 0., true), (1., 1., 2.981_182_8, -4.962_388_5, true), (1.5, 0., 0., 0., false)];

pub const DRONE_LIFE_S: f32 = 3.0;
pub const DRONE_PLAY_RATE: f32 = 1.0;
pub const DRONE_SKELETON: &str = "/Game/Gadgets/Assets/Bot_Turret/Meshes/SK_Bot_Turret_Skeleton";
pub const DRONE_ANIM: &str = "/Game/Animation/Game/MainPlayer/Gadgets/Bot_Turret_Spawn_Out";
pub const DRONE_MESH: &str = "/Game/Weapons/FORT_TRAPS/BaseDevice/MeshGame/BaseBotGame";
pub const EYE_BONE: &str = "b_Root_Anim";
pub const EYE_SOCKET_LOCATION: [f32; 3] = [0.000_198_364_26, 17.063_578, 4.909_389_5];
pub const EYE_SOCKET_ROTATION: [f32; 3] = [337.009_83, 90., 0.];
pub const DRONE_MESH_LOCATION: [f32; 3] = [-3.911_054_6e-5, 2.102_214_8, -28.655_321];
pub const DRONE_MESH_ROTATION: [f32; 3] = [0., 90., 0.];
pub const DRONE_MESH_SCALE: [f32; 3] = [0.4, 0.3, 0.3];
pub const DRONE_CUES: [(f32, &str, f32); 2] = [(0.262_797_12, "/Game/Sounds/Fort_Gadgets/Teleporter/teleporter_place_Cue", 1.), (1.432_752_8, "/Game/Sounds/Fort_Gadgets/Teleporter/teleport_Cue", 0.75)];

pub fn z_height(alpha: f32) -> f32 { fn_game::pickups::eval_keys(&Z_HEIGHT, alpha) }

pub fn dissolve_values(t: f32, head_ue: Vec3, root_ue: Vec3) -> [[f32; 4]; 5] {
    let z = z_height((t / RESOUT_PLAY_LENGTH_S).clamp(0., 1.));
    let n = (head_ue - root_ue).normalize_or_zero();
    let leg = root_ue + n * RESOUT_LEG_SPACE;
    let head = head_ue + n * RESOUT_HEAD_SPACE;
    let pct = 1. - z;
    [[head.x, head.y, head.z, 1.], [-n.x, -n.y, -n.z, 1.], [(leg - head).length(); 4], [pct; 4], [1.; 4]]
}

fn patches() -> &'static [Vec<fn_pose::br_material::ParamPatch>; 3] {
    static P: OnceLock<[Vec<fn_pose::br_material::ParamPatch>; 3]> = OnceLock::new();
    P.get_or_init(|| ["head", "hair", "body"].map(|p| fn_pose::br_material::param_patches(p, &fn_pose::br_material::DISSOLVE_PARAMS)))
}

pub fn set_dissolve(renderer: &Renderer, models: &[ModelId], values: Option<&[[f32; 4]; 5]>) -> usize {
    let p = patches();
    models.iter().map(|m| renderer.set_model_char_cb(*m, &|perm, base| values.map(|v| fn_pose::br_material::apply_patches(base, &p[perm as usize], v)))).sum()
}

pub fn game_to_fx(m: Mat4) -> Mat4 { Mat4::from_cols(Vec4::new(100., 0., 0., 0.), Vec4::new(0., 0., 100., 0.), Vec4::new(0., 100., 0., 0.), Vec4::W) * m }

fn xf(loc: [f32; 3], rot: [f32; 3], scale: [f32; 3]) -> Mat4 { Mat4::from_scale_rotation_translation(Vec3::from(scale), crate::lobby_world::rotator_quat(rot), Vec3::from(loc)) }
fn bone(b: &BoneTransform) -> Mat4 { Mat4::from_scale_rotation_translation(Vec3::from(b.scale), Quat::from_array(b.rotation), Vec3::from(b.translation)) }

pub struct DroneData { pub skel: SkeletonInfo, pub seq: AnimSequence, pub eye: usize }

impl DroneData {
    pub fn load(install: &std::path::Path) -> Result<DroneData, String> {
        let paks = fn_pak::PakSet::open_install(install).map_err(|e| e.to_string())?;
        let src = fn_pose::ClipSourceDir::Pak(&paks);
        let skel = SkeletonInfo::from_package(&fn_pose::loader::package(&src, DRONE_SKELETON)?).map_err(|e| format!("{DRONE_SKELETON}: {e}"))?;
        let seq = AnimSequence::parse(&fn_pose::loader::package(&src, DRONE_ANIM)?).map_err(|e| format!("{DRONE_ANIM}: {e}"))?;
        let eye = skel.bone_names.iter().position(|b| b.eq_ignore_ascii_case(EYE_BONE)).ok_or_else(|| format!("{DRONE_SKELETON}: no bone {EYE_BONE}"))?;
        Ok(DroneData { skel, seq, eye })
    }
    pub fn eye_component(&self, t: f32) -> Mat4 {
        let local = self.seq.pose_at(t.min(self.seq.length), &self.skel);
        let mut m = Mat4::IDENTITY;
        let mut i = self.eye as i32;
        while i >= 0 { m = bone(&local[i as usize]) * m; i = self.skel.parents[i as usize]; }
        m * xf(EYE_SOCKET_LOCATION, EYE_SOCKET_ROTATION, [1.; 3])
    }
    pub fn mesh_component(&self, t: f32) -> Mat4 { self.eye_component(t) * xf(DRONE_MESH_LOCATION, DRONE_MESH_ROTATION, DRONE_MESH_SCALE) }
}

struct Drone { t: f32, root_game: Mat4, at: Vec3, rays: Option<fn_particles::Instance>, victory: Option<fn_particles::Instance>, serial: u64 }

pub const P_RAYS: &str = "/Game/Effects/Fort_Effects/Effects/Characters/Generic/ResIn/3dPrintingRaysOut";
pub const P_VICTORY: &str = "/Game/Gadgets/Assets/Teleporter/Effects/P_Teleporter_Victory";
pub const P_REVIVED: &str = "/Game/Athena/Effects/Particles/P_PlayerRevived_Athena";
pub const RAYS_AT_S: f32 = 0.248_828_46;
pub const REZIN_CHARACTER: &str = "/Game/Effects/Fort_Effects/Effects/Characters/Generic/ResIn/RezIn_Character.RezIn_Character";
pub const RAYS_LOCATION_OFFSET: [f32; 3] = [1.25, 0., 0.];
pub const RAYS_ROTATION_OFFSET: [f32; 3] = [-60., 0., 90.];
pub const VICTORY_AT_S: f32 = 0.846_663_3;
pub const REVIVE_RANGE_CM: f32 = 500.;

pub struct ShellPart { pub mesh: String, pub joints: Vec<String>, pub materials: Vec<Option<String>> }
static SHELL_PARTS: Mutex<Vec<ShellPart>> = Mutex::new(Vec::new());
pub fn set_shell_parts(parts: Vec<ShellPart>) { *SHELL_PARTS.lock().unwrap_or_else(|p| p.into_inner()) = parts; }

struct Shell { slot: u8, world: Mat4, skins: Vec<Vec<Mat4>>, values: [[f32; 4]; 5] }
struct ShellMesh { ghost: crate::pickups_dxbc::ghosts::GhostMesh, map: Vec<Option<usize>>, copied: Vec<fn_fx::uniform::Overrides> }

#[derive(Default)]
struct State { shells: Vec<Shell>, shell_meshes: Vec<Option<Result<ShellMesh, String>>>, install: Option<PathBuf>, catalog: Option<std::sync::Arc<fn_particles::assets::Catalog>>, data: Option<Result<std::sync::Arc<DroneData>, String>>, loading: bool, prewarmed: bool, dxbc: Option<Result<Dxbc, String>>, drones: Vec<Drone>, bursts: Vec<fn_particles::Instance>, serial: u64, errors: HashSet<String>, logged: HashSet<String> }
static STATE: Mutex<Option<State>> = Mutex::new(None);
static LOADED: Mutex<Option<Result<std::sync::Arc<DroneData>, String>>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(State::default)) }

pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    let dir = fn_live::var_os("FN_ELIMFX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-elimfx")).find(|d| d.join("index.json").is_file()));
    let catalog = match dir.as_ref().map(|d| fn_particles::assets::Catalog::load(d)) {
        Some(Ok(c)) => { eprintln!("[elimfx] particles-elimfx loaded from {} (rays {}, victory {}, revived {})", dir.as_ref().unwrap().display(), c.system(P_RAYS).is_some(), c.system(P_VICTORY).is_some(), c.system(P_REVIVED).is_some()); Some(std::sync::Arc::new(c)) }
        Some(Err(e)) => { eprintln!("[elimfx] {e}: no drone / revive particles"); None }
        None => { eprintln!("[elimfx] no `particles-elimfx` folder in the asset roots: no drone / revive particles"); None }
    };
    with(|s| { s.install = install; s.catalog = catalog; });
}

fn rot_of(m: Mat4) -> Mat3 { let r = Mat3::from_mat4(m); Mat3::from_cols(r.x_axis.normalize_or_zero(), r.y_axis.normalize_or_zero(), r.z_axis.normalize_or_zero()) }

pub fn make_rot_from_x(x: Vec3) -> Mat3 {
    let x = x.normalize_or_zero();
    let up = if x.z.abs() < 1. - 1e-4 { Vec3::Z } else { Vec3::X };
    let y = up.cross(x).normalize_or_zero();
    Mat3::from_cols(x, y, x.cross(y))
}

pub fn on_death(root_game: Mat4) {
    with(|s| {
        if s.data.is_none() && !s.loading {
            if let Some(dir) = s.install.clone() {
                s.loading = true;
                std::thread::spawn(move || { let r = DroneData::load(&dir).map(std::sync::Arc::new); if let Err(e) = &r { eprintln!("[elimfx] drone animation unavailable: {e}"); } *LOADED.lock().unwrap_or_else(|p| p.into_inner()) = Some(r); });
            } else { s.data = Some(Err("no install".into())); }
        }
        let at = root_game.w_axis.truncate();
        s.serial += 1;
        let serial = s.serial;
        s.drones.push(Drone { t: 0., root_game, at, rays: None, victory: None, serial });
        eprintln!("[elimfx] BP_VictoryDrone spawned at ({:.2}, {:.2}, {:.2}), life {DRONE_LIFE_S} s, rate {DRONE_PLAY_RATE}", at.x, at.y, at.z);
    });
}

pub fn on_revived(root_game: Mat4, root_effect_game: Vec3) {
    with(|s| {
        let Some(sys) = s.catalog.as_ref().and_then(|c| c.system(P_REVIVED)) else { if s.logged.insert("revived".into()) { eprintln!("[elimfx] {P_REVIVED} not in the catalog: not spawned"); } return };
        let pos = game_to_fx(Mat4::from_translation(root_effect_game)).w_axis.truncate();
        let normal = (game_to_fx(root_game) * Vec4::X).truncate();
        s.serial += 1;
        let mut inst = fn_particles::Instance::new(sys, fn_particles::Transform { pos, rot: make_rot_from_x(normal), scale: 1. }, 0x5245_5649 ^ s.serial, 1000.);
        inst.set_vector_parameter("RefractionRingSize", [REVIVE_RANGE_CM, REVIVE_RANGE_CM, 300.]);
        inst.set_vector_parameter("SphereSize", [REVIVE_RANGE_CM / 2.; 3]);
        eprintln!("[elimfx] P_PlayerRevived_Athena at UE ({:.0}, {:.0}, {:.0})", pos.x, pos.y, pos.z);
        s.bursts.push(inst);
    });
}

pub fn step(dt: f32) {
    let loaded = LOADED.lock().ok().and_then(|mut l| l.take());
    with(|s| {
        if let Some(r) = loaded { s.data = Some(r); s.loading = false; }
        if !s.prewarmed && !s.loading && s.data.is_none() {
            let parts: Vec<(String, Vec<Option<String>>)> = SHELL_PARTS.lock().map(|p| p.iter().map(|x| (x.mesh.clone(), x.materials.clone())).collect()).unwrap_or_default();
            if let (Some(dir), false) = (s.install.clone(), parts.is_empty()) {
                s.prewarmed = true;
                s.loading = true;
                std::thread::spawn(move || {
                    if let Ok(set) = fn_pak::PakSet::open_install(&dir) {
                        for (mesh, mats) in &parts {
                            let skel = fn_fx::data::package_files(&set, fn_fx::data::long_package(mesh)).ok().and_then(|(a, x, b)| fn_ueio::Package::parse(&a, &x, &b).ok()).and_then(|p| fn_mesh::SkeletalMesh::parse(&p).ok()).and_then(|m| m.skeleton_path);
                            if let Some(sk) = skel { let _ = fn_fx::data::package_files(&set, fn_fx::data::long_package(&sk)); }
                            for m in mats.iter().flatten() { let _ = copy_parameters(&set, REZIN_CHARACTER, m); }
                        }
                        for pkg in [P_RAYS, P_VICTORY, P_REVIVED, DRONE_MESH] { let _ = fn_fx::data::package_files(&set, pkg); }
                    }
                    let r = DroneData::load(&dir).map(std::sync::Arc::new);
                    if let Err(e) = &r { eprintln!("[elimfx] drone animation unavailable: {e}"); }
                    *LOADED.lock().unwrap_or_else(|p| p.into_inner()) = Some(r);
                });
            }
        }
        let cat = s.catalog.clone();
        for d in s.drones.iter_mut() {
            let t0 = d.t;
            d.t += dt.max(0.) * DRONE_PLAY_RATE;
            for (at, cue, volume) in DRONE_CUES { if t0 < at && d.t >= at { crate::death_hook::push_sound(fn_audio_sink::SoundEvent::CueVolume { cue, pos: Some(d.at), volume }); } }
            if let Some(c) = cat.as_ref() {
                if t0 < RAYS_AT_S && d.t >= RAYS_AT_S { if let Some(sys) = c.system(P_RAYS) { d.rays = Some(fn_particles::Instance::new(sys, fn_particles::Transform::identity(), 0x5241_5953 ^ d.serial, 1000.)); } }
                if t0 < VICTORY_AT_S && d.t >= VICTORY_AT_S { if let Some(sys) = c.system(P_VICTORY) { d.victory = Some(fn_particles::Instance::new(sys, fn_particles::Transform::identity(), 0x5649_4354 ^ d.serial, 1000.)); } }
            }
        }
        s.drones.retain(|d| d.t < DRONE_LIFE_S);
    });
}

pub fn shell_frame(slot: u8, t: f32, world: Mat4, skins: Vec<Vec<Mat4>>, head_game: Option<Vec3>, root_game: Option<Vec3>) {
    with(|s| {
        s.shells.retain(|x| x.slot != slot);
        if t >= RESOUT_PLAY_LENGTH_S { return; }
        let fxp = |p: Vec3| game_to_fx(Mat4::from_translation(p)).w_axis.truncate();
        let (Some(h), Some(r)) = (head_game, root_game) else { return };
        s.shells.push(Shell { slot, world, skins, values: dissolve_values(t, fxp(h), fxp(r)) });
    });
}

fn param_defaults(e: &fn_fx::uniform::Expr, out: &mut Vec<(String, Option<[f32; 4]>, Option<f32>)>) {
    use fn_fx::uniform::Expr as E;
    match e {
        E::VectorParameter { name, default } => out.push((name.clone(), Some(*default), None)),
        E::ScalarParameter { name, default } => out.push((name.clone(), None, Some(*default))),
        E::ComponentSwizzle { e, .. } | E::Unary(_, e) => param_defaults(e, out),
        E::FoldedMath { a, b, .. } | E::AppendVector { a, b, .. } | E::Max(a, b) | E::Min(a, b) | E::Fmod(a, b) => { param_defaults(a, out); param_defaults(b, out); }
        E::TrigMath { x, y, .. } => { param_defaults(x, out); param_defaults(y, out); }
        E::Clamp { x, lo, hi } => { param_defaults(x, out); param_defaults(lo, out); param_defaults(hi, out); }
        E::TextureProperty { tex, .. } => param_defaults(tex, out),
        _ => {}
    }
}
fn params_of(m: &fn_fx::material::Material) -> Vec<(String, Option<[f32; 4]>, Option<f32>)> {
    let mut v = vec![];
    for e in m.uniform.vectors.iter().chain(&m.uniform.scalars).chain(&m.uniform.per_frame_vectors).chain(&m.uniform.per_frame_scalars) { param_defaults(e, &mut v); }
    v
}

pub fn copy_parameters(set: &fn_pak::PakSet, dst: &str, src: &str) -> Result<fn_fx::uniform::Overrides, String> {
    let d = crate::pickups_dxbc::ghosts::prewarm_material(set, dst)?;
    let sm = crate::pickups_dxbc::ghosts::prewarm_material(set, src)?;
    let sp = params_of(&sm);
    let mut o = fn_fx::uniform::Overrides::default();
    for (name, vec, sc) in params_of(&d) {
        if vec.is_some() {
            if let Some(v) = sm.overrides.vectors.get(&name).copied().or_else(|| sp.iter().find(|p| p.0 == name).and_then(|p| p.1)) { o.vectors.insert(name, v); }
        } else if sc.is_some() {
            if let Some(v) = sm.overrides.scalars.get(&name).copied().or_else(|| sp.iter().find(|p| p.0 == name).and_then(|p| p.2)) { o.scalars.insert(name, v); }
        }
    }
    Ok(o)
}

fn dissolve_overrides(v: &[[f32; 4]; 5], o: &mut fn_fx::uniform::Overrides) {
    let p = fn_pose::br_material::DISSOLVE_PARAMS;
    o.vectors.insert(p[0].into(), v[0]);
    o.vectors.insert(p[1].into(), v[1]);
    o.scalars.insert(p[2].into(), v[2][0]);
    o.scalars.insert(p[3].into(), v[3][0]);
    o.scalars.insert(p[4].into(), v[4][0]);
}

pub fn live() -> usize { with(|s| s.drones.len()) }

pub const HEAD_SOCKET_BONE: &str = "head";
pub const ROOT_SOCKET_BONE: &str = "Root";

pub fn remote_dissolve(renderer: &Renderer, models: &[ModelId], head_game: Option<Vec3>, root_game: Option<Vec3>, t: Option<f32>) -> bool {
    let Some(t) = t else { set_dissolve(renderer, models, None); return false };
    let fxp = |p: Vec3| game_to_fx(Mat4::from_translation(p)).w_axis.truncate();
    match (head_game.map(fxp), root_game.map(fxp)) {
        (Some(h), Some(r)) => { let v = dissolve_values(t, h, r); set_dissolve(renderer, models, Some(&v)); true }
        _ => { with(|s| if s.logged.insert("sockets".into()) { eprintln!("[elimfx] the pawn pose has no {HEAD_SOCKET} / {ROOT_SOCKET} socket: no dissolve"); }); false }
    }
}

fn build_shell_mesh(dx: &mut Dxbc, fx: &mut FxRenderer, mesh: &str, joints: &[String], mats: &[Option<String>]) -> Result<ShellMesh, String> {
    let r = dx.ghost_mesh(fx, mesh).map(|ghost| {
        let map = ghost.ref_names.iter().map(|n| joints.iter().position(|j| j.eq_ignore_ascii_case(n))).collect();
        let copied = mats.iter().map(|m| match m { Some(m) => copy_parameters(dx.set(), REZIN_CHARACTER, m).unwrap_or_else(|e| { eprintln!("[elimfx] RezIn_Character parameters from {m}: {e}"); Default::default() }), None => Default::default() }).collect();
        eprintln!("[elimfx] DuplicateResOutMesh shell: {mesh} ({} bones, {} sections) drawn with {REZIN_CHARACTER}", ghost.ref_names.len(), mats.len());
        ShellMesh { ghost, map, copied }
    });
    if let Err(e) = &r { eprintln!("[elimfx] DuplicateResOutMesh shell {mesh}: NOT DRAWN: {e}"); }
    r
}

pub fn prebuild(fx: &mut FxRenderer) -> bool {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if DONE.load(std::sync::atomic::Ordering::Relaxed) { return true; }
    let parts = SHELL_PARTS.lock().map(|p| p.iter().map(|x| (x.mesh.clone(), x.joints.clone(), x.materials.clone())).collect::<Vec<_>>()).unwrap_or_default();
    let step = STEP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = std::time::Instant::now();
    with(|s| {
        if s.install.is_none() { DONE.store(true, std::sync::atomic::Ordering::Relaxed); return; }
        if s.dxbc.is_none() {
            s.dxbc = Some(match s.install.as_ref() { Some(i) => Dxbc::load(i, fx), None => Err("no install".into()) });
            if let (Some(Ok(dx)), Some(cat)) = (s.dxbc.as_ref(), s.catalog.as_ref()) { dx.prefetch_packages([P_RAYS, P_VICTORY, P_REVIVED].iter().filter_map(|p| cat.system(p).map(|x| x.package.clone())).collect()); }
            return;
        }
        let State { dxbc, shell_meshes, catalog, errors, .. } = s;
        let Some(Ok(dx)) = dxbc.as_mut() else { DONE.store(true, std::sync::atomic::Ordering::Relaxed); return };
        let view = ViewParams { eye: Vec3::new(0., 2., 3.), forward: Vec3::new(0., -0.5, -0.85).normalize(), up: Vec3::Y, tan_half: (1., 0.6), near_m: 0.1, far_m: 1000., width: 64, height: 64, time: 0., real_time: 0., delta_world: 1. / 60. };
        dx.begin(&view);
        if shell_meshes.len() != parts.len() { *shell_meshes = (0..parts.len()).map(|_| None).collect(); }
        let systems = [P_RAYS, P_VICTORY, P_REVIVED];
        let k = step.saturating_sub(1);
        if k < parts.len() {
            let (mesh, joints, mats) = &parts[k];
            if shell_meshes[k].is_none() { shell_meshes[k] = Some(build_shell_mesh(dx, fx, mesh, joints, mats)); }
            if let Some(Ok(sm)) = shell_meshes[k].as_ref() {
                let rez = vec![Some(REZIN_CHARACTER.to_owned()); 64];
                let values = dissolve_values(0.5, Vec3::new(0., 0., 180.), Vec3::ZERO);
                let skin = vec![Mat4::IDENTITY; sm.map.len()];
                let extra: Vec<fn_fx::uniform::Overrides> = sm.copied.iter().map(|o| { let mut o = o.clone(); dissolve_overrides(&values, &mut o); o }).collect();
                if let Err(e) = dx.draw_ghost_with(fx, &sm.ghost, &rez, &skin, Mat4::IDENTITY, Vec3::ZERO, &extra) { if errors.insert(format!("warm shell {mesh}")) { eprintln!("[elimfx] warm-up shell {mesh}: {e}"); } }
            }
        } else if k == parts.len() {
            if let Err(e) = dx.draw_static_item(fx, DRONE_MESH, Vec3::ZERO, Mat3::IDENTITY, [0.; 3]) { eprintln!("[elimfx] warm-up {DRONE_MESH}: {e}"); }
        } else if let (Some(cat), Some(p)) = (catalog.clone(), systems.get(k - parts.len() - 1)) {
            if let Some(sys) = cat.system(p) {
                let mut i = fn_particles::Instance::new(sys, fn_particles::Transform::identity(), 1, 100.);
                for _ in 0..12 {
                    i.tick(1. / 16., &fn_particles::Env::none());
                    let mut d = vec![];
                    i.draw([0., 2., 3.], &mut d);
                    let bounds = crate::particles::instance_bounds(&i, &cat);
                    for e in &d { let _ = dx.draw_emitter(fx, e, &i.xf, &bounds, 1. / 60.); }
                }
            }
        }
        let _ = dx.take_frame();
        if k >= parts.len() + systems.len() { DONE.store(true, std::sync::atomic::Ordering::Relaxed); eprintln!("[elimfx] warm-up: {} dissolve shells, drone mesh, drone / revive particles built over {} frames", parts.len(), step + 1); }
    });
    let ms = t.elapsed().as_secs_f32() * 1000.;
    if ms > 30. { eprintln!("[elimfx] warm-up step {step}: {ms:.0} ms"); }
    DONE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
    with(|s| {
        if s.drones.is_empty() && s.bursts.is_empty() && s.shells.is_empty() { return; }
        let data = s.data.clone().and_then(|r| r.ok());
        if s.dxbc.is_none() { s.dxbc = Some(match s.install.as_ref() { Some(i) => Dxbc::load(i, fx), None => Err("no install".into()) }); }
        let env = fn_particles::Env::none();
        let State { dxbc, errors, drones, bursts, catalog, shells, shell_meshes, .. } = s;
        for b in bursts.iter_mut() { b.tick(dt, &env); }
        bursts.retain(|b| !b.finished());
        let dx = match dxbc.as_mut().unwrap() { Ok(d) => d, Err(e) => { if errors.insert("dxbc".into()) { eprintln!("[elimfx] drone mesh unavailable: {e}"); } return; } };
        dx.begin(view);
        let eye = [view.eye.x, view.eye.y, view.eye.z];
        let emit = |dx: &mut Dxbc, fx: &mut FxRenderer, inst: &fn_particles::Instance, errors: &mut HashSet<String>| {
            let Some(cat) = catalog.as_ref() else { return };
            let mut d = vec![];
            inst.draw(eye, &mut d);
            let bounds = crate::particles::instance_bounds(inst, cat);
            for e in &d { if let Err(err) = dx.draw_emitter(fx, e, &inst.xf, &bounds, dt) { let k = format!("{}/{}", e.system, e.emitter); if errors.insert(k.clone()) { eprintln!("[elimfx] {k}: NOT DRAWN: {err}"); } } }
        };
        for d in drones.iter_mut() {
            let root = game_to_fx(d.root_game);
            if let Some(data) = data.as_ref() {
                let m = root * data.mesh_component(d.t);
                if let Err(e) = dx.draw_static_item(fx, DRONE_MESH, m.w_axis.truncate(), Mat3::from_mat4(m), [0.; 3]) { if errors.insert(DRONE_MESH.into()) { eprintln!("[elimfx] {DRONE_MESH}: NOT DRAWN: {e}"); } }
                if let Some(r) = d.rays.as_mut() {
                    let e = root * data.eye_component(d.t) * xf(RAYS_LOCATION_OFFSET, RAYS_ROTATION_OFFSET, [1.; 3]);
                    r.xf.pos = e.w_axis.truncate(); r.xf.rot = rot_of(e);
                    r.tick(dt, &env);
                    emit(dx, fx, r, errors);
                }
            }
            if let Some(v) = d.victory.as_mut() { v.xf.pos = root.w_axis.truncate(); v.xf.rot = rot_of(root); v.tick(dt, &env); emit(dx, fx, v, errors); }
        }
        for b in bursts.iter() { emit(dx, fx, b, errors); }
        let lights: Vec<_> = drones.iter().flat_map(|d| d.rays.iter().chain(d.victory.iter())).chain(bursts.iter()).flat_map(|i| i.lights()).map(|l| crate::rocket_hook::particle_light(&l)).collect();
        if !lights.is_empty() { crate::local_lights_hook::add_dynamic(lights); }
        let parts = SHELL_PARTS.lock().map(|p| p.iter().map(|x| (x.mesh.clone(), x.joints.clone(), x.materials.clone())).collect::<Vec<_>>()).unwrap_or_default();
        if !shells.is_empty() && shell_meshes.len() != parts.len() { *shell_meshes = (0..parts.len()).map(|_| None).collect(); }
        let c = crate::mp_hook::ue_bone_to_mesh();
        let ci = c.inverse();
        let rez = vec![Some(REZIN_CHARACTER.to_owned()); 64];
        for sh in shells.iter() {
            let l2w = game_to_fx(sh.world) * c;
            for (i, (mesh, joints, mats)) in parts.iter().enumerate() {
                let Some(skin_char) = sh.skins.get(i) else { continue };
                if shell_meshes[i].is_none() { shell_meshes[i] = Some(build_shell_mesh(dx, fx, mesh, joints, mats)); }
                let Some(Ok(sm)) = shell_meshes[i].as_ref() else { continue };
                let skin: Vec<Mat4> = sm.map.iter().map(|k| k.and_then(|k| skin_char.get(k)).map_or(Mat4::IDENTITY, |m| ci * *m * c)).collect();
                let extra: Vec<fn_fx::uniform::Overrides> = sm.copied.iter().map(|o| { let mut o = o.clone(); dissolve_overrides(&sh.values, &mut o); o }).collect();
                if let Err(e) = dx.draw_ghost_with(fx, &sm.ghost, &rez, &skin, l2w, l2w.w_axis.truncate(), &extra) { if errors.insert(format!("shell {mesh}")) { eprintln!("[elimfx] DuplicateResOutMesh shell {mesh}: NOT DRAWN: {e}"); } }
            }
        }
        shells.clear();
        let fr = dx.take_frame();
        out.buffer_writes.extend(fr.buffer_writes);
        out.draws.extend(fr.draws);
        out.shadow_draws.extend(fr.shadow_draws);
        out.distance_field_objects.extend(fr.distance_field_objects);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paks() -> Option<fn_pak::PakSet> {
        let dir = crate::hitreg_hook::install_dir().or_else(|| std::env::var_os("FN_ROOT").map(PathBuf::from))?;
        fn_pak::PakSet::open_install(&dir).ok()
    }
    fn props(p: &fn_pak::PakSet, pkg: &str) -> Vec<(String, serde_json::Value)> {
        let pd = fn_uasset::PackageData::load(p, pkg).unwrap();
        let pk = pd.package().unwrap();
        (0..pk.exports.len()).map(|i| (pk.exports[i].name.clone(), pk.read_export(i).map(|o| o.to_json(&pk)["properties"].clone()).unwrap_or_default())).collect()
    }
    fn f(v: &serde_json::Value) -> f32 { v.as_f64().unwrap() as f32 }
    #[test] fn dissolve_follows_the_timeline_from_visible_to_gone() {
        let (h, r) = (Vec3::new(0., 0., 170.), Vec3::new(0., 0., 0.));
        let a = dissolve_values(0., h, r);
        assert_eq!((a[3][0], a[4][0]), (1., 1.));
        assert_eq!(a[1], [0., 0., -1., 1.]);
        assert_eq!(a[0], [0., 0., 210., 1.]);
        assert!((a[2][0] - 220.).abs() < 1e-3);
        assert!(dissolve_values(RESOUT_PLAY_LENGTH_S, h, r)[3][0].abs() < 1e-6);
        let mid = dissolve_values(RESOUT_PLAY_LENGTH_S * 0.5, h, r)[3][0];
        assert!(mid > 0. && mid < 1.);
    }
    #[test] fn constants_match_the_cooked_blueprints() {
        let Some(p) = paks() else { eprintln!("no install (FN_ROOT): skipped"); return };
        let ro = props(&p, "/Game/Effects/Fort_Effects/Effects/Characters/Generic/ResIn/DuplicateResOutMesh");
        let get = |v: &Vec<(String, serde_json::Value)>, n: &str| v.iter().find(|x| x.0 == n).map(|x| x.1.clone()).unwrap();
        let cdo = get(&ro, "Default__DuplicateResOutMesh_C");
        assert_eq!((f(&cdo["Head Space"]), f(&cdo["Leg Space"])), (RESOUT_HEAD_SPACE, RESOUT_LEG_SPACE));
        assert!(cdo.get("Gun True").map_or(true, |v| v == false));
        let keys = get(&ro, "CurveFloat_0__256DA8D9")["FloatCurve"]["Keys"].as_array().unwrap().clone();
        for (k, e) in keys.iter().zip(Z_HEIGHT) { assert_eq!((f(&k["Time"]), f(&k["Value"])), (e.0, e.1)); }
        assert_eq!(f(&keys[1]["ArriveTangent"]), Z_HEIGHT[1].2);
        assert_eq!(f(&get(&ro, "CharacterSpawnInTimeline_Template")["TimelineLength"]), 1.);
        let dr = props(&p, "/Game/Blueprints/Adventures/SpawnTeleporter/BP_VictoryDrone");
        assert_eq!(f(&get(&dr, "Default__BP_VictoryDrone_C")["AnimPlayRate"]), DRONE_PLAY_RATE);
        let sm = get(&dr, "StaticMesh_GEN_VARIABLE");
        assert!(sm["StaticMesh"].as_str().unwrap().starts_with(DRONE_MESH));
        let v3 = |v: &serde_json::Value| [f(&v[0]), f(&v[1]), f(&v[2])];
        assert_eq!((v3(&sm["RelativeLocation"]), v3(&sm["RelativeRotation"]), v3(&sm["RelativeScale3D"])), (DRONE_MESH_LOCATION, DRONE_MESH_ROTATION, DRONE_MESH_SCALE));
        let sk = get(&dr, "SkeletalMesh_GEN_VARIABLE");
        assert_eq!((sk["bVisible"].as_bool(), sk["bHiddenInGame"].as_bool()), (Some(false), Some(true)));
        let skel = props(&p, DRONE_SKELETON);
        let eye = skel.iter().map(|x| &x.1).find(|v| v["SocketName"] == "Eye").unwrap();
        assert_eq!((eye["BoneName"].as_str(), v3(&eye["RelativeLocation"]), v3(&eye["RelativeRotation"])), (Some(EYE_BONE), EYE_SOCKET_LOCATION, EYE_SOCKET_ROTATION));
        let mo = props(&p, "/Game/Animation/Game/MainPlayer/Montages/Gadgets/Bot_Turret_Spawn_Out_Montage");
        let notifies = get(&mo, "Bot_Turret_Spawn_Out_Montage")["Notifies"].as_array().unwrap().clone();
        let at = |n: &str| notifies.iter().find(|x| x["NotifyName"] == n).map(|x| f(&x["LinkValue"])).unwrap();
        assert_eq!((at("3dPrintingRaysOut"), at("P_Teleporter_Victory")), (RAYS_AT_S, VICTORY_AT_S));
        for (t, cue, vol) in DRONE_CUES {
            assert_eq!(at(cue.rsplit('/').next().unwrap()), t);
            let snd = mo.iter().find(|x| x.1["Sound"].as_str().map_or(false, |s| s.starts_with(cue))).unwrap();
            assert_eq!(snd.1.get("VolumeMultiplier").map_or(1., f), vol);
        }
        let rays = get(&mo, "AnimNotify_PlayParticleEffect_0");
        assert_eq!((rays["PSTemplate"].as_str().unwrap().split('.').next(), rays["SocketName"].as_str(), v3(&rays["LocationOffset"]), v3(&rays["RotationOffset"])), (Some(P_RAYS), Some("eye"), RAYS_LOCATION_OFFSET, RAYS_ROTATION_OFFSET));
        let vic = get(&mo, "AnimNotify_PlayParticleEffect_1");
        assert_eq!((vic["PSTemplate"].as_str().unwrap().split('.').next(), vic.get("SocketName")), (Some(P_VICTORY), None));
    }
    #[test] fn light_only_emitters_are_not_drawn() {
        let Some(p) = paks() else { eprintln!("no install (FN_ROOT): skipped"); return };
        for (sys, emitter) in [(P_RAYS, "Light"), (P_REVIVED, "Light")] {
            let ex = props(&p, sys);
            let e = ex.iter().find(|x| x.1["EmitterName"] == emitter).unwrap();
            assert_eq!(e.1["EmitterRenderMode"].as_str(), Some("ERM_LightsOnly"));
        }
    }
    #[test] fn rezin_shell_takes_the_pawn_parameters_it_shares() {
        let Some(p) = paks() else { eprintln!("no install (FN_ROOT): skipped"); return };
        let o = copy_parameters(&p, REZIN_CHARACTER, "/Game/Characters/Player/Male/Medium/Bodies/M_Med_Soldier_04/Materials/BR01/M_Med_Soldier_04_CAU_BR01.M_Med_Soldier_04_CAU_BR01").unwrap();
        assert_eq!(o.scalars.get("GridDistortionAmount"), Some(&0.02));
        assert!(o.scalars.get("Swipe Band Width").is_none());
        let mut v = o.clone();
        dissolve_overrides(&dissolve_values(0., Vec3::new(0., 0., 170.), Vec3::ZERO), &mut v);
        assert_eq!(v.scalars.get("DissolveGradientPercentage"), Some(&1.));
    }
    #[test] fn drone_anim_poses_the_eye() {
        let Some(dir) = crate::hitreg_hook::install_dir().or_else(|| std::env::var_os("FN_ROOT").map(PathBuf::from)) else { eprintln!("no install: skipped"); return };
        let d = DroneData::load(&dir).unwrap();
        let (a, b) = (d.mesh_component(0.).w_axis.truncate(), d.mesh_component(1.5).w_axis.truncate());
        assert!(a.is_finite() && b.is_finite());
        eprintln!("drone mesh at t=0 {a:?}, t=1.5 {b:?}, anim length {}", d.seq.length);
    }
}
