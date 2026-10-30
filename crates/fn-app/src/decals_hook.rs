use fn_hitreg::{GunHit, Impact};
use fn_matshader::decal::{DecalAssets, Spawn};
use fn_matshader::view::ViewInput;
use fn_render::decal_pass::{DecalDraw, DecalFrame};
use fn_render::{CameraParams, Renderer};
use glam::{DVec3, Vec3};
use std::sync::{mpsc, Mutex};

pub const MAX_LIVE: usize = 1024;

struct Live { s: Spawn, material: Vec<u8> }

struct State {
    assets: Option<DecalAssets>,
    loading: Option<mpsc::Receiver<Result<DecalAssets, String>>>,
    installed: bool,
    failed: bool,
    live: Vec<Live>,
    now: f32,
    eye_ue: Option<DVec3>,
    shooter: Option<(DVec3, bool)>,
    rnd: u64,
    spawned: u64,
    skipped: [u64; 3],
}

static STATE: Mutex<State> = Mutex::new(State { assets: None, loading: None, installed: false, failed: false, live: vec![], now: 0., eye_ue: None, shooter: None, rnd: 0x6465_6361_6c73_0001, spawned: 0, skipped: [0; 3] });

fn lock() -> std::sync::MutexGuard<'static, State> { STATE.lock().unwrap_or_else(|e| e.into_inner()) }

fn frand(s: &mut State) -> f32 {
    s.rnd ^= s.rnd << 13; s.rnd ^= s.rnd >> 7; s.rnd ^= s.rnd << 17;
    ((s.rnd >> 40) as u32 & 0x7fff) as f32 / 32767.
}

pub fn start(install_dir: &std::path::Path) {
    let mut s = lock();
    if s.loading.is_some() || s.assets.is_some() || s.failed { return; }
    if fn_live::var("FN_DECALS").is_ok_and(|v| v == "0") { s.failed = true; eprintln!("[decals] OFF (FN_DECALS=0)"); return; }
    let (tx, rx) = mpsc::channel();
    let dir = install_dir.to_owned();
    let _ = std::thread::Builder::new().name("fn-decals-load".into()).spawn(move || {
        let r = fn_pak::PakSet::open_install(&dir).map_err(|e| e.to_string()).and_then(|set| DecalAssets::load(&set));
        let _ = tx.send(r);
    });
    s.loading = Some(rx);
}

fn poll(s: &mut State, renderer: &mut Renderer) {
    if let Some(rx) = s.loading.as_ref() {
        if let Ok(r) = rx.try_recv() {
            s.loading = None;
            match r {
                Ok(a) => { for n in &a.notes { eprintln!("[decals] {n}"); } s.assets = Some(a); }
                Err(e) => { s.failed = true; eprintln!("[decals] NOT available, bullet impacts leave no decals: {e}"); }
            }
        }
    }
    if s.installed || s.failed { return; }
    let Some(a) = s.assets.as_ref() else { return };
    let t = std::time::Instant::now();
    match renderer.decals().create(a.desc.clone(), fn_render::renderer::HDR_FORMAT) {
        Ok(()) => {
            s.installed = true;
            renderer.decals().set_frame(DecalFrame { view: vec![0; a.desc.view_bytes], decals: vec![DecalDraw { pass: vec![0; a.desc.pass_bytes], material: vec![0; a.desc.material_bytes], vertex: vec![0; a.desc.vertex_bytes] }] });
            eprintln!("[decals] ON: {} (SM5 {}, DXBC -> WGSL), pipeline + texture in {:.0} ms; rules {:?}", a.desc.label, fn_matshader::decal::PROGRAM, t.elapsed().as_secs_f32() * 1000., a.rules);
        }
        Err(e) => { s.failed = true; s.assets = None; eprintln!("[decals] program rejected: {e}"); }
    }
}

pub fn prewarm(renderer: &mut Renderer) -> bool {
    let mut s = lock();
    poll(&mut s, renderer);
    s.installed || s.failed || (s.loading.is_none() && s.assets.is_none())
}

pub fn on_shot(start_ue: Vec3, remote: bool) { lock().shooter = Some((start_ue.as_dvec3(), !remote)); }

pub fn on_impact(gun: &GunHit, imp: &Impact) {
    if gun.projectile.is_some() { return; }
    let mut s = lock();
    let Some(a) = s.assets.as_ref() else { return };
    let rules = &a.rules;
    if !rules.use_decals { return; }
    if !rules.accepts_surface(imp.surface) { s.skipped[0] += 1; return; }
    let (shooter, local) = s.shooter.unwrap_or((DVec3::ZERO, true));
    let distance = s.eye_ue.map_or(0., |e| (e - shooter).length() as f32);
    let level = crate::local_lights_hook::effects_quality() as usize;
    if !rules.significant(level, distance, local) { s.skipped[1] += 1; return; }
    let pos = fn_particles::sim::game_to_ue(imp.pos.to_array()).as_dvec3();
    let n = Vec3::new(imp.normal.x, imp.normal.z, imp.normal.y).as_dvec3();
    if n.length_squared() < 1e-8 { s.skipped[2] += 1; return; }
    let now = s.now;
    let mut seq = [0f32; 4];
    for v in seq.iter_mut() { *v = frand(&mut s); }
    let mut k = 0;
    let a = s.assets.as_ref().unwrap();
    let sp = a.rules.spawn(pos, n.normalize(), now, &mut || { let v = seq[k.min(3)]; k += 1; v });
    let material = a.material_block(&sp);
    if s.live.len() >= MAX_LIVE { s.live.remove(0); }
    s.live.push(Live { s: sp, material });
    s.spawned += 1;
    if s.spawned <= 3 || s.spawned % 100 == 0 { eprintln!("[decals] #{} surface {} at {:?} size {:?} life {:.2} s yaw {:.0} pitch {:.0} ({} live; skipped surface {} significance {})", s.spawned, imp.surface, pos, sp.size, sp.life, sp.yaw, sp.pitch, s.live.len(), s.skipped[0], s.skipped[1]); }
}

pub fn submit(renderer: &mut Renderer, camera: &CameraParams, world_time: f32) {
    let mut s = lock();
    poll(&mut s, renderer);
    s.now = world_time;
    s.eye_ue = Some(fn_matshader::view::to_ue(camera.eye));
    if !s.installed { return; }
    s.live.retain(|l| l.s.alive(world_time) && l.s.spawn_time <= world_time + 1.);
    if s.live.is_empty() { return; }
    let Some(a) = s.assets.as_ref() else { return };
    let (tx, ty) = camera.tan_half();
    let view = ViewInput { eye: camera.eye, forward: camera.forward, up: camera.up, tan_half: (tx, ty), near_m: camera.near, far_m: camera.far, width: camera.width, height: camera.height, time: world_time };
    let (rows, vbytes) = a.view_block(&view);
    let decals: Vec<DecalDraw> = s.live.iter().filter_map(|l| a.draw(&l.s, &l.material, &rows, &view)).collect();
    drop(s);
    if decals.is_empty() { return; }
    renderer.decals().set_frame(DecalFrame { view: vbytes, decals });
}

pub fn live() -> usize { lock().live.len() }
