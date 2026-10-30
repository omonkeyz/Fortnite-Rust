use crate::lobby_scene::{CameraFrame, LobbyTranslucent, VolSprite, NEAR_CM, FAR_CM};
use crate::lobby_world::{LobbySceneData, Xf};
use crate::pickups_dxbc::Dxbc;
use fn_fx::view::ViewParams;
use fn_particles::{Env, Instance, ParticleSystem, Transform};
use fn_render::fx::FxRenderer;
use glam::{Mat3, Vec3};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const PAD_SPARKLE: &str = "/Game/Effects/Fort_Effects/Effects/Characters/Outlander/InTheZone/P_Outlander_Teleport";
pub const PAD_SWIRL: &str = "/Game/UI/Foundation/Lobby/materials/P_LobbySpawnSpin";

pub struct FxSpawn { pub name: String, pub system: Arc<ParticleSystem>, pub xf: Transform, pub dilation: f32 }

pub struct GhostSpec { pub group: crate::lobby_world::MatineeGroup, pub mesh: String, pub materials: Vec<Option<String>>, pub hidden: bool, pub scale: Vec3 }

pub struct LobbyFxAssets { pub spawns: Vec<FxSpawn>, pub notes: Vec<String>, pub ghosts: Vec<GhostSpec>, pub matinee: Option<(f32, bool)>, pub volume_materials: Vec<String> }

fn pkg(path: &str) -> &str { path.split('.').next().unwrap_or(path) }

fn to_transform(x: &Xf) -> Transform {
    let s = (x.s.x.abs() + x.s.y.abs() + x.s.z.abs()) / 3.0;
    Transform { pos: x.t, rot: Mat3::from_quat(x.q), scale: s }
}

pub fn load_assets(set: &fn_pak::PakSet, data: &LobbySceneData) -> LobbyFxAssets {
    let mut out = LobbyFxAssets { spawns: vec![], notes: vec![], ghosts: vec![], matinee: None, volume_materials: vec![] };
    if let Some(m) = data.matinee.as_ref().filter(|m| m.play_on_load) {
        out.matinee = Some((m.length, m.looping));
        for g in &m.groups {
            let Some(k) = data.skeletals.iter().find(|k| k.actor == g.actor) else { out.notes.push(format!("{}: {} has no skeletal actor", m.actor, g.name)); continue };
            let Some(mesh) = k.mesh.clone() else { out.notes.push(format!("{}: no SkeletalMesh", k.actor)); continue };
            out.ghosts.push(GhostSpec { group: g.clone(), mesh, materials: k.materials.clone(), hidden: k.hidden, scale: k.xf.s });
        }
    }
    let mut cache: std::collections::HashMap<String, Option<Arc<ParticleSystem>>> = Default::default();
    let mut get = |path: &str, notes: &mut Vec<String>| -> Option<Arc<ParticleSystem>> {
        let p = pkg(path).to_owned();
        cache.entry(p.clone()).or_insert_with(|| match fn_particles::import::import_system(set, &p) {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => { notes.push(format!("{p}: {e}")); None }
        }).clone()
    };
    for p in data.particles.iter().filter(|p| p.visible && p.auto_activate) {
        let Some(t) = p.template.as_deref() else { continue };
        if pkg(t) == PAD_SPARKLE || pkg(t) == PAD_SWIRL { continue; }
        if let Some(sys) = get(t, &mut out.notes) { out.spawns.push(FxSpawn { name: format!("{}.{}", p.actor, p.component), system: sys, xf: to_transform(&p.xf), dilation: 1.0 }); }
    }
    let comps: [(&str, &str, [f32; 3], [f32; 3], [f32; 3], f32); 3] = [
        ("PS-NoPlayer-Sparkle", PAD_SPARKLE, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.4, 0.5, 1.0], 0.3),
        ("PS-NoPlayer-Swirl01", PAD_SWIRL, [0.0, 0.0, 49.0], [90.0, 0.0, 0.0], [5.0, 5.0, 5.0], 0.4),
        ("PS-NoPlayer-Swirl02", PAD_SWIRL, [0.0, 0.0, 51.0], [-90.0, 0.0, 0.0], [4.0, 4.0, 4.0], 1.0),
    ];
    let pad_fx = fn_live::var("FN_LOBBY_PAD_FX").map_or(false, |v| v == "1");
    for pl in data.placements.iter().filter(|p| pad_fx && !p.hidden && !p.player_on_pad) {
        for (name, tpl, loc, rot, scale, dil) in comps {
            let Some(sys) = get(tpl, &mut out.notes) else { continue };
            let world = Xf::from_pyr(loc, rot, scale).compose(&pl.root);
            out.spawns.push(FxSpawn { name: format!("{}.{name}", pl.actor), system: sys, xf: to_transform(&world), dilation: dil });
        }
    }
    for sp in &out.spawns {
        for e in &sp.system.emitters {
            for lod in &e.lods {
                let mp = &lod.required.material;
                if mp.is_empty() || out.volume_materials.contains(mp) { continue; }
                if fn_fx::material::Material::load(set, mp).map_or(false, |m| m.flags.domain == "MD_Volume") {
                    out.notes.push(format!("{}: {} is MD_Volume: voxelized into the volumetric fog, not drawn", sp.name, mp));
                    out.volume_materials.push(mp.clone());
                }
            }
        }
    }
    out
}

pub struct LobbyFx {
    fx: FxRenderer,
    dx: Option<Result<Dxbc, String>>,
    install: PathBuf,
    live: Vec<(String, Instance, f32)>,
    last_time: Option<f32>,
    errors: std::collections::HashSet<String>,
    ghosts: Vec<(GhostSpec, Option<Result<crate::pickups_dxbc::ghosts::GhostMesh, String>>)>,
    matinee: Option<(f32, bool)>,
    anims: std::collections::HashMap<String, Option<fn_anim::sequence::AnimSequence>>,
    volume_materials: Vec<String>,
    dt: f32,
}

impl LobbyFx {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, install: &Path, assets: LobbyFxAssets, eye: Vec3) -> LobbyFx {
        let fx = FxRenderer::new(device, queue, crate::lobby_scene::HDR_FORMAT, crate::lobby_scene::DEPTH_FORMAT);
        let live: Vec<(String, Instance, f32)> = assets.spawns.into_iter().enumerate().map(|(i, s)| {
            let d = (s.xf.pos - eye).length();
            (s.name, Instance::new(s.system, s.xf, 0x4c4f_4242 ^ i as u64, d), s.dilation)
        }).collect();
        for n in &assets.notes { eprintln!("[lobby fx] {n}"); }
        for (n, i, _) in &live { eprintln!("[lobby fx] {n}: {} at {:?}", i.system.name, i.xf.pos); }
        for g in &assets.ghosts { eprintln!("[lobby fx] matinee group {} -> {} ({} anim keys, {} visibility keys)", g.group.name, g.mesh, g.group.anims.len(), g.group.vis.len()); }
        let ghosts = assets.ghosts.into_iter().map(|g| (g, None)).collect();
        LobbyFx { fx, dx: None, install: install.to_path_buf(), live, last_time: None, errors: Default::default(), ghosts, matinee: assets.matinee, anims: Default::default(), volume_materials: assets.volume_materials, dt: 0.0 }
    }
    pub fn count(&self) -> usize { self.live.len() }
}

fn view_params(cam: &CameraFrame, time: f32, dt: f32) -> ViewParams {
    let sw = |v: Vec3| Vec3::new(v.x, v.z, v.y);
    ViewParams { eye: sw(cam.eye) * 0.01, forward: sw(cam.fwd), up: Vec3::Y, tan_half: (cam.tan_x, cam.tan_y), near_m: NEAR_CM * 0.01, far_m: FAR_CM * 0.01, width: cam.width, height: cam.height, time, real_time: time, delta_world: dt }
}

impl LobbyTranslucent for LobbyFx {
    fn advance(&mut self, time: f32) {
        let dt = match self.last_time { Some(t) if time >= t => (time - t).min(0.1), _ => 0.0 };
        self.last_time = Some(time);
        self.dt = dt;
        let env = Env::none();
        for (_, inst, dil) in self.live.iter_mut() { if dt > 0.0 { inst.tick(dt * *dil, &env); } }
    }
    fn volume_sprites(&self, eye_cm: Vec3) -> Vec<VolSprite> {
        if self.volume_materials.is_empty() { return vec![]; }
        let eye = [eye_cm.x * 0.01, eye_cm.z * 0.01, eye_cm.y * 0.01];
        let mut out = vec![];
        for (_, inst, _) in &self.live {
            let mut draws = vec![];
            inst.draw(eye, &mut draws);
            for e in draws.iter().filter(|e| self.volume_materials.contains(&e.material) && e.kind == fn_particles::sim::DrawKind::Sprite) {
                let facing = matches!(e.alignment, fn_particles::model::Alignment::Square | fn_particles::model::Alignment::Rectangle) && e.axis_lock == fn_particles::model::AxisLock::None;
                for p in &e.particles {
                    out.push(VolSprite {
                        material: e.material.clone(), pos_cm: Vec3::new(p.pos[0], p.pos[2], p.pos[1]) * 100.0, size_cm: [p.size[0] * 100.0, p.size[1] * 100.0], rotation: p.rotation,
                        pivot: [crate::pickups_dxbc::DEFAULT_PIVOT[0] + e.pivot_offset[0], crate::pickups_dxbc::DEFAULT_PIVOT[1] + e.pivot_offset[1]], camera_facing: facing,
                    });
                }
            }
        }
        out
    }
    fn prepare(&mut self, cam: &CameraFrame, time: f32, depth_only: &wgpu::TextureView, depth_gen: u64, light: &[[f32; 4]; 10]) {
        let dt = self.dt;
        let view = view_params(cam, time, dt);
        if self.dx.is_none() {
            let r = Dxbc::load(&self.install, &mut self.fx);
            if let Err(e) = &r { eprintln!("[lobby fx] materials unavailable: {e}"); }
            self.dx = Some(r);
        }
        let Some(Ok(dx)) = self.dx.as_mut() else { return };
        dx.begin(&view);
        dx.override_light(light);
        let eye = [view.eye.x, view.eye.y, view.eye.z];
        for (name, inst, _) in &self.live {
            let mut draws = vec![];
            inst.draw(eye, &mut draws);
            let r = 50_000.0f32;
            let bounds = fn_render::primitive_ub::BoxSphereBounds { origin: inst.xf.pos.to_array(), extent: [r, r, r], radius: r * 1.732 };
            for e in draws.iter().filter(|e| !self.volume_materials.contains(&e.material)) {
                if let Err(err) = dx.draw_emitter(&mut self.fx, e, &inst.xf, &bounds, dt) {
                    let k = format!("{name}/{}/{}", e.system, e.emitter);
                    if self.errors.insert(k.clone()) { eprintln!("[lobby fx] {k}: not drawn: {err}"); }
                }
            }
        }
        if let Some((len, looping)) = self.matinee {
            let pos = if looping && len > 0.0 { time.rem_euclid(len) } else { time.min(len) };
            let first_loop = time < len;
            for (spec, mesh) in self.ghosts.iter_mut() {
                if mesh.is_none() {
                    let r = dx.ghost_mesh(&mut self.fx, &spec.mesh);
                    if let Err(e) = &r { eprintln!("[lobby fx] ghost {}: {e}", spec.group.name); }
                    for k in &spec.group.anims {
                        if !self.anims.contains_key(&k.seq) {
                            let a = dx.ghost_anim(&k.seq);
                            if let Err(e) = &a { eprintln!("[lobby fx] ghost anim {e}"); }
                            self.anims.insert(k.seq.clone(), a.ok());
                        }
                    }
                    *mesh = Some(r);
                }
                let Some(Ok(m)) = mesh.as_ref() else { continue };
                if !spec.group.visible(pos, first_loop, spec.hidden) { continue; }
                let Some((p, pyr)) = spec.group.transform(pos) else { continue };
                let anims = &self.anims;
                let a = spec.group.anim(pos, |s| anims.get(s).and_then(|o| o.as_ref()).map(|a| a.length)).and_then(|(s, t)| anims.get(s).and_then(|o| o.as_ref()).map(|q| (q, t)));
                let skin = crate::pickups_dxbc::Dxbc::ghost_pose(m, a);
                let q = Xf::from_pyr(p.to_array(), pyr, spec.scale.to_array()).q;
                let l2w = glam::Mat4::from_scale_rotation_translation(spec.scale, q, p);
                if let Err(err) = dx.draw_ghost(&mut self.fx, m, &spec.materials, &skin, l2w, p) {
                    if self.errors.insert(format!("ghost/{}", spec.group.name)) { eprintln!("[lobby fx] ghost {}: not drawn: {err}", spec.group.name); }
                }
            }
        }
        let frame = dx.take_frame();
        self.fx.set_view_size((cam.width, cam.height));
        self.fx.set_frame(frame);
        self.fx.prepare(depth_only, depth_gen);
    }
    fn draw_opaque<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) { self.fx.draw(pass, false); }
    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.fx.draw(pass, true);
        self.fx.draw_separate(pass);
    }
}
