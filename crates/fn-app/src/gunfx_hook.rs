use crate::pickups_dxbc::Dxbc;
use fn_audio_sink::SoundEvent;
use fn_fx::view::ViewParams;
use fn_game::audio_events::PassByTracker;
use fn_hitreg::tracer::{Tracer, TracerDef};
use fn_hitreg::{GunHit, Impact};
use fn_particles::assets::Catalog;
use fn_particles::sim::{game_to_ue, ue_to_game};
use fn_particles::{Env, Instance, Transform};
use fn_render::fx::{FxFrame, FxRenderer};
use glam::{Mat3, Vec3};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const MAX_TRACERS: usize = 256;
pub const MAX_IMPACTS: usize = 96;

struct LiveTracer { t: Tracer, def: Arc<TracerDef>, whip: Option<PassByTracker> }

#[derive(Default)]
struct State {
    roots: Vec<PathBuf>,
    install: Option<PathBuf>,
    catalog: Option<Arc<Catalog>>,
    catalog_tried: bool,
    dxbc: Option<Result<Dxbc, String>>,
    tracers: Vec<LiveTracer>,
    fx: Vec<Instance>,
    sounds: Vec<SoundEvent>,
    errors: HashSet<String>,
    log: Vec<String>,
    rnd: u64,
    stats: (u64, u64, u64, u64),
    defs: Vec<Arc<TracerDef>>,
    pawn: Option<(Vec3, Vec3)>,
    remote: Vec<(u8, glam::Mat4)>,
    trails: Vec<(u64, Vec<(Instance, Vec3)>, Option<f32>)>,
    next_key: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(|| State { rnd: 0x6775_6e66_7800_0001, ..Default::default() })) }
fn say(s: &mut State, line: String) { eprintln!("[gunfx] {line}"); if s.log.len() < 400 { s.log.push(line); } }

pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) { with(|s| { s.install = install; s.roots = roots.to_vec(); }); }

fn load_catalog(s: &mut State) {
    if s.catalog_tried || (s.roots.is_empty() && fn_live::var_os("FN_GUNFX_DIR").is_none()) { return; }
    s.catalog_tried = true;
    let dir = fn_live::var_os("FN_GUNFX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| s.roots.iter().map(|r| r.join("particles-gunfx")).find(|d| d.join("index.json").is_file()));
    match dir.map(|d| (Catalog::load(&d), d)) {
        Some((Ok(c), d)) => { say(s, format!("particles loaded from {}", d.display())); s.catalog = Some(Arc::new(c)); }
        Some((Err(e), _)) => say(s, format!("{e}: no gun impact particles")),
        None => say(s, "no `particles-gunfx` folder in the asset roots: no gun impact particles".into()),
    }
}

fn def_for(s: &mut State, gun: &GunHit) -> Option<Arc<TracerDef>> {
    let d = gun.tracer.as_ref()?;
    if let Some(a) = s.defs.iter().find(|a| a.class == d.class) { return Some(a.clone()); }
    let a = Arc::new(d.clone());
    if !a.override_materials.is_empty() { say(s, format!("{}: OverrideMaterials {:?} are not applied (the mesh's own materials are drawn)", a.class, a.override_materials)); }
    s.defs.push(a.clone());
    Some(a)
}

pub fn on_shot(gun: &GunHit, ends_game: &[Vec3], start_ue: Vec3, remote: bool) {
    if gun.projectile.is_some() || ends_game.is_empty() { return; }
    crate::decals_hook::on_shot(start_ue, remote);
    with(|s| {
        let Some(def) = def_for(s, gun) else { return };
        for e in ends_game {
            let t = Tracer::new(&def, start_ue, Vec3::from(game_to_ue(e.to_array())));
            let t_dbg = (t.start, t.end, t.vel.length(), t.scale_time);
            let whip = if remote && def.pass_by_radii.1 > 0. { s.pawn.map(|(p, _)| PassByTracker::new(t.start, t.end, def.pass_by_radii, p, t.pos)) } else { None };
            if s.tracers.len() >= MAX_TRACERS { s.tracers.remove(0); }
            s.tracers.push(LiveTracer { t, def: def.clone(), whip });
            s.stats.0 += 1;
            if s.stats.0 <= 3 || s.stats.0 % 50 == 0 { let l = format!("tracer #{} {} {:?} -> {:?} speed {:.0} cm/s scale time {:.3} s{}", s.stats.0, def.class.rsplit('/').next().unwrap_or(""), t_dbg.0, t_dbg.1, t_dbg.2, t_dbg.3, if remote { " (remote)" } else { "" }); say(s, l); }
        }
    });
}

pub fn on_impact(gun: &GunHit, imp: &Impact) {
    crate::decals_hook::on_impact(gun, imp);
    with(|s| {
        s.stats.1 += 1;
        let pos_ue = Vec3::from(game_to_ue(imp.pos.to_array()));
        if let Some(path) = imp.effect.as_deref() {
            load_catalog(s);
            if let Some(cat) = s.catalog.clone() {
                let name = path.split('.').next().unwrap_or(path);
                match cat.system(name).or_else(|| cat.system(path)) {
                    Some(sys) => {
                        s.rnd = s.rnd.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                        let n = imp.normal;
                        let rot = crate::rocket_hook::rot_from_x(Vec3::new(n.x, n.z, n.y));
                        if s.fx.len() >= MAX_IMPACTS { s.fx.remove(0); }
                        s.fx.push(Instance::new(sys, Transform { pos: pos_ue, rot, scale: 1. }, s.rnd, 0.));
                        s.stats.2 += 1;
                        if s.errors.insert(format!("spawned {name}")) { say(s, format!("first {} at {:?} surface {}", name.rsplit('/').next().unwrap_or(name), pos_ue, imp.surface)); }
                    }
                    None => { if s.errors.insert(path.to_owned()) { say(s, format!("{path} is not in the particles-gunfx catalog: not spawned")); } }
                }
            }
        }
        if imp.sound.is_some() {
            match fn_audio::weapon_events::bp_event(&gun.pres.actor, &format!("impact.s{}", imp.surface)) {
                Some(id) => { s.sounds.push(SoundEvent::Weapon { id, pos: Some(imp.pos), volume: 1. }); s.stats.3 += 1; }
                None => { let k = format!("snd {} s{}", gun.pres.actor, imp.surface); if s.errors.insert(k.clone()) { say(s, format!("{k}: no weapon-table row for {:?}", imp.sound)); } }
            }
        }
    });
}

pub fn remote_shot(slot: u8, gun: &GunHit, world: &fn_combat::World, origin_game: Vec3, dir_game: Vec3, seed: i32) {
    let hits = fn_hitreg::shoot::cosmetic_cartridge(gun, world, origin_game, dir_game, seed);
    let start = with(|s| s.remote.iter().find(|r| r.0 == slot).map(|r| r.1)).and_then(|attach| crate::muzzle_hook::remote_muzzle_ue(&gun.wid, attach)).unwrap_or_else(|| Vec3::from(game_to_ue(origin_game.to_array())));
    let ends: Vec<Vec3> = hits.iter().map(|h| h.0).collect();
    on_shot(gun, &ends, start, true);
    for (_, i) in hits { if let Some(i) = i { on_impact(gun, &i); } }
}

fn trail_xf(pos_game: Vec3, vel_game: Vec3, rel: Vec3) -> Transform {
    let rot = crate::rocket_hook::rot_from_x(Vec3::new(vel_game.x, vel_game.z, vel_game.y));
    Transform { pos: Vec3::from(game_to_ue(pos_game.to_array())) + rot * rel, rot, scale: 1. }
}

pub fn projectile_spawn(gun: &GunHit, pos_game: Vec3, vel_game: Vec3) -> u64 {
    let key = projectile_spawn_fx(gun, pos_game, vel_game);
    crate::supply_drop_hook::projectile_spawn(key, pos_game, gun);
    key
}

fn projectile_spawn_fx(gun: &GunHit, pos_game: Vec3, vel_game: Vec3) -> u64 {
    with(|s| {
        s.next_key += 1;
        let key = s.next_key;
        load_catalog(s);
        let Some(cat) = s.catalog.clone() else { return key };
        let mut v = vec![];
        for (path, rel) in &gun.projectile_particles {
            let name = path.split('.').next().unwrap_or(path);
            let Some(sys) = cat.system(name).or_else(|| cat.system(path)) else { if s.errors.insert(path.clone()) { say(s, format!("{path} is not in the particles-gunfx catalog: no projectile trail")); } continue };
            s.rnd = s.rnd.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let rel = Vec3::from(*rel);
            v.push((Instance::new(sys, trail_xf(pos_game, vel_game, rel), s.rnd, 0.), rel));
        }
        if !v.is_empty() { s.trails.push((key, v, None)); }
        key
    })
}

pub fn projectile_move(key: u64, pos_game: Vec3, vel_game: Vec3) {
    crate::supply_drop_hook::projectile_move(key, pos_game, false);
    with(|s| { if let Some((_, v, None)) = s.trails.iter_mut().find(|t| t.0 == key) { for (i, rel) in v.iter_mut() { i.xf = trail_xf(pos_game, vel_game, *rel); } } });
}

pub const SNIPER_PROJECTILE: &str = "B_Prj_Bullet_Sniper";
pub const SNIPER_SURFACE_CUE: &str = "/Game/Athena/Sounds/Impacts/Sniper_Rifle_Surface_Rico_Cue";
pub const SNIPER_PLAYER_CUE: &str = "/Game/Athena/Sounds/Impacts/Sniper_Rifle_Player_Impact_Athena_Cue";
pub const STOP_DELAY_S: f32 = 0.5;

pub fn projectile_end(key: u64, gun: &GunHit, hit: Option<(Vec3, bool)>) {
    crate::supply_drop_hook::projectile_end(key, hit.map(|h| h.0));
    with(|s| {
        if let Some((pos, player)) = hit {
            if gun.pres.projectile_template.as_deref().is_some_and(|t| t.contains(SNIPER_PROJECTILE)) {
                if let Some(cue) = fn_audio_sink::gunfx_cue(if player { SNIPER_PLAYER_CUE } else { SNIPER_SURFACE_CUE }) { s.sounds.push(SoundEvent::BuildingCue { cue, pos: Some(pos) }); }
            }
        }
        let Some(t) = s.trails.iter_mut().find(|t| t.0 == key) else { return };
        if hit.is_none() { t.1.clear(); return; }
        t.1.retain(|(i, _)| i.system.emitters.iter().any(|e| e.lods.iter().any(|l| matches!(l.type_data, fn_particles::model::TypeData::Ribbon(_)))));
        for (i, _) in t.1.iter_mut() { i.deactivate(); }
        t.2 = Some(STOP_DELAY_S);
    });
}

pub fn set_remote_attach(slot: u8, attach: glam::Mat4) { with(|s| { if let Some(r) = s.remote.iter_mut().find(|r| r.0 == slot) { r.1 = attach; } else { s.remote.push((slot, attach)); } }); }

pub fn set_listener(pawn_game: Vec3, forward_game: Vec3) { crate::muzzle_hook::set_feet_ue(Some((pawn_game.y - 0.9) * 100.)); let f = Vec3::new(forward_game.x, forward_game.z, 0.).normalize_or_zero(); with(|s| s.pawn = Some((Vec3::from(game_to_ue(pawn_game.to_array())), Vec3::new(-f.y, f.x, 0.)))); }

pub fn take_sounds() -> Vec<SoundEvent> { with(|s| std::mem::take(&mut s.sounds)) }

fn ensure_dxbc<'a>(dxbc: &'a mut Option<Result<Dxbc, String>>, install: &Option<PathBuf>, catalog: &Option<Arc<Catalog>>, fx: &mut FxRenderer) -> Option<&'a mut Dxbc> {
    if dxbc.is_none() {
        let mut r = match install { Some(d) => Dxbc::load(d, fx), None => Err("no install".into()) };
        if let Ok(dx) = r.as_mut() {
            dx.no_shadows = true;
            if let Some(cat) = catalog.as_ref() { for sys in cat.systems() { dx.skip_non_normal_emitters(&sys.package); dx.skip_trail_emitters(&sys.package); } }
            if let Some(cat) = crate::muzzle_hook::catalog() { for sys in cat.systems() { dx.skip_non_normal_emitters(&sys.package); } }
            MUZZLE_DXBC.store(fn_live::var("FN_MUZZLE_DXBC").map_or(true, |v| v != "0"), std::sync::atomic::Ordering::Relaxed);
        }
        *dxbc = Some(r);
    }
    dxbc.as_mut().and_then(|r| r.as_mut().ok())
}

fn tracer_rot(t: &Tracer) -> Mat3 { crate::rocket_hook::rot_from_x(t.dir()) * Mat3::from_diagonal(t.scale3()) }

pub const WARM_MESHES: [&str; 2] = ["/Game/Weapons/Tracers/Effects/Meshes/SM_tracer_01.SM_tracer_01", "/Game/Weapons/Tracers/Effects/Meshes/SM_Shotgun_Tracer.SM_Shotgun_Tracer"];

pub fn prebuild(fx: &mut FxRenderer) -> bool {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if DONE.load(std::sync::atomic::Ordering::Relaxed) { return true; }
    with(|s| {
        load_catalog(s);
        if s.install.is_none() { if s.catalog_tried { DONE.store(true, std::sync::atomic::Ordering::Relaxed); } return; }
        let t = std::time::Instant::now();
        let State { dxbc, install, errors, catalog, .. } = s;
        let Some(dx) = ensure_dxbc(dxbc, install, catalog, fx) else { DONE.store(true, std::sync::atomic::Ordering::Relaxed); return };
        let view = ViewParams { eye: Vec3::new(0., 2., 3.), forward: Vec3::new(0., -0.5, -0.85).normalize(), up: Vec3::Y, tan_half: (1., 0.6), near_m: 0.1, far_m: 1000., width: 64, height: 64, time: 0., real_time: 0., delta_world: 1. / 60. };
        dx.begin(&view);
        if NEXT.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            for m in WARM_MESHES { if let Err(e) = dx.static_ready(fx, m) { eprintln!("[gunfx] {m}: {e}"); } else { let _ = dx.draw_static_item(fx, m, Vec3::ZERO, Mat3::IDENTITY, [0.; 3]); } }
            let _ = dx.take_frame();
            dx.begin(&view);
        }
        let muzzle_cat = crate::muzzle_hook::catalog();
        let systems: Vec<_> = catalog.as_ref().map(|c| c.systems().map(|x| (x.clone(), c.clone())).collect::<Vec<_>>()).unwrap_or_default().into_iter().chain(muzzle_cat.as_ref().map(|c| c.systems().map(|x| (x.clone(), c.clone())).collect::<Vec<_>>()).unwrap_or_default()).collect();
        let mut n = 0;
        for (sys, cat) in systems.iter().skip(NEXT.load(std::sync::atomic::Ordering::Relaxed)).cloned() {
            if n > 0 && t.elapsed().as_secs_f32() > 0.012 { let _ = dx.take_frame(); return; }
            let mut i = Instance::new(sys, Transform::identity(), 1, 100.);
            for k in 0..24 {
                if k > 0 && t.elapsed().as_secs_f32() > 0.012 { let _ = dx.take_frame(); return; }
                i.xf.pos = Vec3::new(k as f32 * 150., 0., 0.);
                i.tick(1. / 16., &Env::none());
                let mut d = vec![]; i.draw([0., 2., 3.], &mut d);
                let bounds = crate::particles::instance_bounds(&i, &cat);
                for e in &d { if let Err(err) = dx.draw_emitter(fx, e, &i.xf, &bounds, 1. / 60.) { let k = format!("{}/{}", e.system, e.emitter); if errors.insert(k.clone()) { eprintln!("[gunfx] {k}: NOT DRAWN: {err}"); } } }
                let _ = dx.take_frame();
                dx.begin(&view);
            }
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            n += 1;
        }
        let _ = dx.take_frame();
        DONE.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("[gunfx] warm-up: {} tracer meshes + {} particle systems built in {:.0} ms", WARM_MESHES.len(), systems.len(), t.elapsed().as_secs_f32() * 1000.);
    });
    DONE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
    let muzzle: Vec<_> = crate::muzzle_hook::lights().iter().map(crate::rocket_hook::particle_light).collect();
    if !muzzle.is_empty() { crate::local_lights_hook::add_dynamic(muzzle); }
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    let mut muzzles = 0;
    if draws_muzzle() { crate::muzzle_hook::with_instances(&mut |_| muzzles += 1); }
    if st.tracers.is_empty() && st.fx.is_empty() && st.trails.is_empty() && muzzles == 0 { return; }
    let env = Env::none();
    let pawn = st.pawn;
    let mut whips = vec![];
    st.tracers.retain_mut(|l| {
        let alive = l.t.tick(dt);
        if let (Some(w), Some((p, r))) = (l.whip.as_mut(), pawn) { if let Some(ws) = w.tick(p, r, l.t.pos) { whips.push(ws); } }
        alive
    });
    for w in whips {
        let at = Vec3::from(ue_to_game(w.location));
        st.sounds.push(SoundEvent::Weapon { id: "whip.far", pos: Some(at), volume: w.far_volume });
        st.sounds.push(SoundEvent::Weapon { id: "whip.close", pos: Some(at), volume: w.close_volume });
    }
    for i in st.fx.iter_mut() { i.tick(dt, &env); }
    for (_, v, left) in st.trails.iter_mut() { for (i, _) in v.iter_mut() { i.tick(dt, &env); } if let Some(l) = left.as_mut() { *l -= dt; if *l <= 0. { v.clear(); } } }
    st.trails.retain(|t| !t.1.is_empty());
    st.fx.retain(|i| !i.finished());
    let lights: Vec<_> = st.fx.iter().flat_map(|i| i.lights()).map(|l| crate::rocket_hook::particle_light(&l)).collect();
    if !lights.is_empty() { crate::local_lights_hook::add_dynamic(lights); }
    let eye = [view.eye.x, view.eye.y, view.eye.z];
    let State { dxbc, install, tracers, fx: shots, errors, catalog, trails, .. } = st;
    let Some(dx) = ensure_dxbc(dxbc, install, catalog, fx) else { return };
    dx.begin(view);
    for l in tracers.iter() {
        if let Err(e) = dx.draw_static_item(fx, &l.def.mesh, l.t.pos, tracer_rot(&l.t), [0.; 3]) { if errors.insert(format!("mesh {}", l.def.mesh)) { eprintln!("[gunfx] {}: NOT DRAWN: {e}", l.def.mesh); } }
    }
    if let Some(cat) = catalog.clone() {
        for i in shots.iter().chain(trails.iter().flat_map(|t| t.1.iter().map(|x| &x.0))) {
            let mut d = vec![];
            i.draw(eye, &mut d);
            let bounds = crate::particles::instance_bounds(i, &cat);
            for e in &d { if let Err(err) = dx.draw_emitter(fx, e, &i.xf, &bounds, dt) { let k = format!("{}/{}", e.system, e.emitter); if errors.insert(k.clone()) { eprintln!("[gunfx] {k}: NOT DRAWN: {err}"); } } }
        }
    }
    if muzzles > 0 {
        if let Some(mcat) = crate::muzzle_hook::catalog() {
            crate::muzzle_hook::with_instances(&mut |i| {
                let mut d = vec![];
                i.draw(eye, &mut d);
                let bounds = crate::particles::instance_bounds(i, &mcat);
                if fn_live::var_os("FN_GUNFX_DEBUG").is_some() { for e in &d { if let Some(p) = e.particles.first() { eprintln!("[gunfx debug] muzzle {}/{} {:?} n={} local {} pos {:?} size {:?} color {:?} dyn {:?} rel {:.2} xf {:?} eye {:?}", e.system, e.emitter, e.kind, e.particles.len(), e.use_local_space, p.pos, p.size, p.color, p.dynamic, p.rel_time, i.xf.pos, eye); } } }
                for e in &d { if let Err(err) = dx.draw_emitter(fx, e, &i.xf, &bounds, dt) { let k = format!("{}/{}", e.system, e.emitter); if errors.insert(k.clone()) { eprintln!("[gunfx] muzzle {k}: NOT DRAWN: {err}"); } } }
            });
        }
    }
    let fr = dx.take_frame();
    if fn_live::var_os("FN_GUNFX_DEBUG").is_some() || !fr.draws.is_empty() && errors.insert("first draw".into()) { eprintln!("[gunfx] frame: {} tracers, {} effects -> {} draws", tracers.len(), shots.len(), fr.draws.len()); }
    out.buffer_writes.extend(fr.buffer_writes);
    out.draws.extend(fr.draws);
}

static MUZZLE_DXBC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn draws_muzzle() -> bool { MUZZLE_DXBC.load(std::sync::atomic::Ordering::Relaxed) }

pub fn live() -> (usize, usize) { with(|s| (s.tracers.len(), s.fx.len())) }

pub fn report() -> String {
    with(|s| {
        if s.stats == (0, 0, 0, 0) && s.log.is_empty() { return String::new(); }
        let mut out = format!("gunfx: {} tracers, {} impacts ({} effects spawned, {} impact sounds), {} live tracers, {} live effects\n", s.stats.0, s.stats.1, s.stats.2, s.stats.3, s.tracers.len(), s.fx.len());
        for l in &s.log { out.push_str("  "); out.push_str(l); out.push('\n'); }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_tracer_mesh_points_along_the_shot_and_stretches_only_its_length() {
        let def = TracerDef { mesh_min_x: -162.66, mesh_max_x: 0., mesh_scale_mult: [8., 1., 1.], mesh_scale_time: 0.05, speed: 16000., speed_scale_mult: (2., 2.), scale_speed: true, scale_on_death: true, speed_scale_range: (2048., 5000.), ..Default::default() };
        let mut t = Tracer::new(&def, Vec3::ZERO, Vec3::new(0., 1000., 0.));
        t.tick(0.05);
        let m = tracer_rot(&t);
        let x = m * Vec3::X;
        assert!((x - Vec3::new(0., 8., 0.)).length() < 1e-3, "{x:?}");
        assert!(((m * Vec3::Y).length() - 1.).abs() < 1e-4 && ((m * Vec3::Z).length() - 1.).abs() < 1e-4);
    }
}
