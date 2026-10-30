//! The held firearms drawn with their ORIGINAL material: the base-pass programs of the cooked `WeaponMat_Base` / `BackPackShrinkingEffect` shader maps, translated from
//! DXBC by `fn-dxbc` and driven by `fn-matshader` (permutation, material-instance parameters, textures, View / Primitive / bone buffers), drawn by fn-render's gun pass at
//! the player's `RightHand` socket. The weapon's own skeleton rig + clips (fire / reload montages) drive the bone matrices exactly as for the stand-in model.
use fn_matshader::gun::{FrameInput, GunModel, GunScene};
use fn_matshader::view::ViewInput;
use fn_pose::loadout::{Resources, ITEMS};
use fn_pose::{CharacterPose, CharacterRig, ClipStore};
use fn_animgraph::Node;
use fn_render::{CameraParams, Renderer};
use fn_render::gun_material::GunFrame;
use glam::Mat4;
use std::collections::HashMap;
use std::path::Path;

struct OrigGun { model: GunModel, rig: CharacterRig, clips: ClipStore, pose: CharacterPose }
struct Pending { gun: usize, to_world: Mat4, skin: Vec<Mat4> }

/// `scene` is shared with the dropped weapons and the held pickaxe (`pickup_orig.rs`): one texture / material / pipeline cache, so a weapon that was drawn on the
/// ground is mostly cache hits when it is equipped (FIRST USE, 2026-10-06: they had a scene and a pak set each and built every weapon twice on the UI thread).
pub struct OrigGuns { scene: GunScene, guns: Vec<OrigGun>, pending: Option<Pending>, /** the install's paks (a shared mount, fn-pak) */ set: fn_pak::PakSet, remote_models: HashMap<(usize, usize), Option<(GunModel, Vec<Mat4>)>>, remote_pending: Vec<((usize, usize), Mat4)> }

/// Builds the three firearms (loadout items 0..3). Any error names the piece that could not be recovered; the caller then keeps the gun hidden.
pub fn install(renderer: &mut Renderer, res: &Resources, install_dir: &Path) -> Result<OrigGuns, String> {
    let set = fn_pak::PakSet::open_install(install_dir).map_err(|e| e.to_string())?;
    let mut scene = GunScene::new(renderer.gun());
    let mut guns = vec![];
    for i in 0..fn_pose::loadout_gun::STARTER_GUNS {
        guns.push(build_one(&mut scene, renderer, &set, res, i, ITEMS[i])?);
    }
    for l in scene.log.drain(..) { eprintln!("{l}"); }
    Ok(OrigGuns { scene, guns, pending: None, set, remote_models: HashMap::new(), remote_pending: vec![] })
}
static STARTER_PREP: std::sync::Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> = std::sync::Mutex::new(None);
pub fn start_starter_prepare(res: &Resources, install_dir: &Path) {
    let Ok(mut g) = STARTER_PREP.lock() else { return };
    if g.is_some() { return; }
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    *g = Some(done.clone());
    let jobs: Vec<(String, Vec<Option<String>>)> = (0..fn_pose::loadout_gun::STARTER_GUNS).filter_map(|i| res.gun_item(i).map(|it| (ITEMS[i].to_owned(), it.mesh_materials.clone()))).collect();
    let dir = install_dir.to_path_buf();
    let spawned = std::thread::Builder::new().name("starter guns prepare".into()).spawn(move || {
        let t = std::time::Instant::now();
        if let Ok(set) = fn_pak::PakSet::open_install(&dir) { for (w, m) in &jobs { if let Err(e) = fn_matshader::gun::prepare_weapon(&set, w, m) { eprintln!("[arms] {w}: starter prepare failed ({e})"); } } }
        eprintln!("[arms] starter guns prepared on a worker in {:.0} ms", t.elapsed().as_secs_f32() * 1000.);
        done.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    if spawned.is_err() { if let Some(d) = g.as_ref() { d.store(true, std::sync::atomic::Ordering::Relaxed); } }
}
pub fn starter_prepare_done() -> bool { STARTER_PREP.lock().ok().and_then(|g| g.as_ref().map(|d| d.load(std::sync::atomic::Ordering::Relaxed))).unwrap_or(true) }
pub fn install_lazy(renderer: &mut Renderer, install_dir: &Path) -> Result<OrigGuns, String> {
    let set = fn_pak::PakSet::open_install(install_dir).map_err(|e| e.to_string())?;
    Ok(OrigGuns { scene: GunScene::new(renderer.gun()), guns: vec![], pending: None, set, remote_models: HashMap::new(), remote_pending: vec![] })
}
fn build_one(scene: &mut GunScene, renderer: &mut Renderer, set: &fn_pak::PakSet, res: &Resources, g: usize, wid: &str) -> Result<OrigGun, String> {
    let item = res.gun_item(g).ok_or("gun index out of range")?;
    let part = item.mesh.clone().ok_or("gun has no mesh")?;
    // FIRST USE: the mesh parsed (and its materials / textures prepared) by the weapon runtime's worker, else parsed here as before
    let mesh = match fn_matshader::gun::prepared_weapon(wid) { Some(m) => m, None => std::sync::Arc::new(fn_matshader::mesh::load_weapon_mesh(set, wid)?) };
    let model = scene.build_gun(set, renderer.gun(), &mesh, &part, &item.mesh_materials)?;
    Ok(OrigGun { model, rig: CharacterRig::new(vec![part]), clips: res.gun_clips(g)?, pose: CharacterPose::default() })
}

impl OrigGuns {
    pub fn starters_missing(&self) -> bool { self.guns.len() < fn_pose::loadout_gun::STARTER_GUNS }
    pub fn build_next_starter(&mut self, renderer: &mut Renderer, res: &Resources) -> Result<(), String> {
        let g = self.guns.len();
        if g >= fn_pose::loadout_gun::STARTER_GUNS { return Ok(()); }
        let one = build_one(&mut self.scene, renderer, &self.set, res, g, ITEMS[g])?;
        for l in self.scene.log.drain(..) { eprintln!("{l}"); }
        self.guns.push(one);
        Ok(())
    }
    /// Builds firearm `g` (installed after start-up, gun index = the previous count) with its original material.
    pub fn add(&mut self, renderer: &mut Renderer, res: &Resources, g: usize) -> Result<(), String> {
        if g != self.guns.len() { return Err(format!("gun index {g} is out of order (have {})", self.guns.len())); }
        let wid = res.gun_item(g).ok_or("gun index out of range")?.definition.clone();
        let one = build_one(&mut self.scene, renderer, &self.set, res, g, &wid)?;
        for l in self.scene.log.drain(..) { eprintln!("{l}"); }
        self.guns.push(one);
        Ok(())
    }
    /// PICKUP ITEM LIGHTING: builds a dropped weapon's (or the held pickaxe's) mesh in the held guns' scene, posed by `part`.
    pub fn build_pickup(&mut self, renderer: &mut Renderer, mesh: &fn_matshader::mesh::GunMeshData, part: &fn_pose::Part, materials: &[Option<String>]) -> Result<fn_matshader::gun::GunModel, String> {
        let r = self.scene.build_gun(&self.set, renderer.gun(), mesh, part, materials);
        for l in self.scene.log.drain(..) { eprintln!("{l}"); }
        r
    }
    /// The paks the guns are read from.
    pub fn set(&self) -> &fn_pak::PakSet { &self.set }
    /// Evaluates the held gun's skeleton for this tick; `attach` = `world * RightHand socket` (port space).
    pub fn update(&mut self, held: Option<(usize, Option<(String, f32)>)>, attach: Mat4) {
        self.pending = None;
        self.remote_pending.clear();
        let Some((g, clip)) = held else { return };
        let Some(h) = self.guns.get_mut(g) else { return };
        let tree = match &clip { Some((c, t)) => Node::clip(c, *t, false), None => Node::Ref };
        h.rig.evaluate_into(&tree, &h.clips, &mut h.pose);
        if let Some(p) = h.pose.parts.first() { self.pending = Some(Pending { gun: g, to_world: attach, skin: p.skin_matrices.clone() }); }
    }
    /// Hands the frame's gun draws to the renderer (call before `Renderer::render`).
    pub fn submit(&mut self, renderer: &mut Renderer, camera: &CameraParams, time: f32) {
        let mut frame = GunFrame::default();
        if let Some(p) = &self.pending {
            let (tx, ty) = camera.tan_half();
            let view = ViewInput { eye: camera.eye, forward: camera.forward, up: camera.up, tan_half: (tx, ty), near_m: camera.near, far_m: camera.far, width: camera.width, height: camera.height, time };
            let input = FrameInput { view, gun_to_world: p.to_world, skin: &p.skin, sky_radiance: fn_render::lighting::sky_radiance(), time, vectors: &[] };
            self.guns[p.gun].model.frame(&input, &mut frame);
        }
        if !self.remote_pending.is_empty() {
            let (tx, ty) = camera.tan_half();
            let view = ViewInput { eye: camera.eye, forward: camera.forward, up: camera.up, tan_half: (tx, ty), near_m: camera.near, far_m: camera.far, width: camera.width, height: camera.height, time };
            for (key, to_world) in &self.remote_pending {
                let Some(Some((model, skin))) = self.remote_models.get_mut(key) else { continue };
                let input = FrameInput { view, gun_to_world: *to_world, skin, sky_radiance: fn_render::lighting::sky_radiance(), time, vectors: &[] };
                model.frame(&input, &mut frame);
            }
        }
        renderer.set_gun_frame(frame);
    }
    pub fn is_drawing(&self) -> bool { self.pending.is_some() }
    /// MUZZLE HOOK: gun-space (one_mesh) matrix of a bone of firearm gun in the pose evaluated by the last update.
    pub fn bone_matrix(&self, gun: usize, bone: &str) -> Option<Mat4> { let h = self.guns.get(gun)?; let i = h.rig.parts.first()?.tree.bone_index(bone)?; h.pose.parts.first()?.bone_mesh.get(i).copied() }
}

fn build_remote(o: &mut OrigGuns, renderer: &mut Renderer, res: &Resources, g: usize) -> Result<(GunModel, Vec<Mat4>), String> {
    let item = res.gun_item(g).ok_or("gun index out of range")?;
    let part = item.mesh.clone().ok_or("gun has no mesh")?;
    let mesh = match fn_matshader::gun::prepared_weapon(&item.definition) { Some(m) => m, None => std::sync::Arc::new(fn_matshader::mesh::load_weapon_mesh(&o.set, &item.definition)?) };
    let model = o.scene.build_gun(&o.set, renderer.gun(), &mesh, &part, &item.mesh_materials)?;
    for l in o.scene.log.drain(..) { eprintln!("{l}"); }
    let pose = CharacterRig::new(vec![part]).evaluate(&Node::Ref, &res.gun_clips(g)?);
    Ok((model, pose.parts.first().map(|p| p.skin_matrices.clone()).unwrap_or_default()))
}

pub fn remote(scene: &mut crate::scene::Scene, who: usize, g: usize, attach: Mat4) {
    let (Some(o), Some(res)) = (scene.orig_guns.as_mut(), scene.loadout.as_ref()) else { return };
    if !o.remote_models.contains_key(&(who, g)) {
        let built = build_remote(o, &mut scene.renderer, res, g);
        if let Err(e) = &built { eprintln!("[mp] remote gun {g} for player {who} unavailable: {e}"); }
        o.remote_models.insert((who, g), built.ok());
    }
    o.remote_pending.push(((who, g), attach));
}
