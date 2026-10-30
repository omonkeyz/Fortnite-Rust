//! MUZZLE HOOK (fn-app): the muzzle flash of the held firearm. The data (which particle system, which socket) is `fn-vfx`'s table read from the 1.8 weapon Blueprints
//! (`<assets>/particles-muzzle/muzzle.json`, written by `fn-vfx-export`), the component behaviour is `fn_vfx::MuzzleFlash` (`B_Ranged_Generic`'s `Muzzle(Empty)`), the simulation is
//! `fn_particles`, the drawing is the particle pass of `fn-render` through `particles::batches`.
//!
//! * [`init`] (from `particles::init`): loads the table + catalog and one [`MuzzleFlash`] per loadout firearm (`fn_pose::loadout::ITEMS[0..3]`).
//! * [`push_fire`] (from `hitreg_hook::on_arms`, once per shot the arms report): the native `OnPlayWeaponFireFX` event.
//! * [`on_gun_pose`] (from `Scene::update`): the held gun's attach matrix + bone lookup -> the `Muzzle` socket transform of this frame.
//! * [`frame`] (from `particles::submit`): runs the queued shots, ticks the flashes, returns their draw data.
//!
//! ASSUMED / PENDING (exe): WHEN the native code calls `OnPlayWeaponFireFX` and with which `bPersistentFire` (here: once per shot the arms fire, `bPersistentFire =
//! bPersistentFireFX`), the native `IsUsingScope()` (here: the ADS driver's scope view is up, `ads::Frame::scope_mesh`), the native default of `MuzzleSocketName` (here `Muzzle`, which
//! every BR weapon mesh has), and the `RarityColor` instance parameter some muzzle systems read (here: the `Constant` of the parameter distribution).
use fn_particles::assets::Catalog;
use fn_particles::sim::EmitterDraw;
use fn_particles::{Env, ParticleLight, Transform};
use fn_render::particles::ParticleFrame;
use fn_render::{CameraParams, Renderer};
use fn_vfx::{FireContext, MuzzleFlash, MuzzleTable, ReloadSmoke, ShellEject, SocketInfo};
use glam::{Mat4, Vec3};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Gun {
    wid: String,
    flash: Option<MuzzleFlash>,
    socket: Option<SocketInfo>,
    persistent_fire: bool,
    /// `Muzzle` socket transform of the latest posed frame (UE space) and the previous one (for `PartSysVelocity`).
    xf: Option<Transform>,
    prev_pos: Option<Vec3>,
    shells: Option<ShellEject>,
    smoke: Option<ReloadSmoke>,
    shells_xf: Option<Transform>,
    smoke_xf: Option<Transform>,
    shells_prev: Option<Vec3>,
}

fn make_gun(i: usize, wid: &str, table: &MuzzleTable, catalog: &Catalog) -> Gun {
    let w = table.weapon(wid);
    let flash = w.and_then(|w| w.muzzle_system.as_deref()).and_then(|s| catalog.system(s)).map(|sys| MuzzleFlash::new(sys, 0x6D75_7A7A ^ i as u64));
    let persistent_fire = w.map_or(false, |w| w.persistent_fire);
    let shells = w.and_then(|w| w.shells.clone()).and_then(|d| catalog.system(&d.system).map(|sys| ShellEject::new(sys, d, persistent_fire, 0x5348_454C ^ i as u64)));
    let smoke = w.and_then(|w| w.reload_smoke.clone()).and_then(|d| catalog.system(&d.system).map(|sys| ReloadSmoke::new(sys, d, 0x534D_4F4B ^ i as u64)));
    Gun { wid: wid.to_string(), flash, socket: w.and_then(|w| w.socket.clone()), persistent_fire, xf: None, prev_pos: None, shells, smoke, shells_xf: None, smoke_xf: None, shells_prev: None }
}

enum Event { Fire(usize), Reload(usize, u8), Stop(usize) }

struct State { catalog: Arc<Catalog>, table: Arc<MuzzleTable>, guns: Vec<Gun>, fires: Vec<Event>, scope: bool, feet_z: Option<f32>, now: f32, gpu: crate::particles::Gpu, log: Vec<String> }

static STATE: Mutex<Option<State>> = Mutex::new(None);
fn logline(st: &mut State, s: String) { if st.log.len() < 2000 { eprintln!("[muzzle] {s}"); st.log.push(s); } }

/// Loads `<root>/particles-muzzle` (or `FN_MUZZLE_DIR`) of the first asset root that has a `muzzle.json`. Returns false (silently) without it: the guns then have no muzzle flash.
pub fn init(roots: &[PathBuf]) -> bool {
    let dir = fn_live::var_os("FN_MUZZLE_DIR").map(PathBuf::from).filter(|d| d.join("muzzle.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-muzzle")).find(|d| d.join("muzzle.json").is_file()));
    let Some(dir) = dir else { eprintln!("[muzzle] no `particles-muzzle` folder with a muzzle.json in the asset roots (run fn-vfx-export): the firearms have no muzzle flash"); return false };
    let (table, catalog) = match (MuzzleTable::load(&dir.join("muzzle.json")), Catalog::load(&dir)) { (Ok(t), Ok(c)) => (t, c), (Err(e), _) | (_, Err(e)) => { eprintln!("[muzzle] cannot load {}: {e}", dir.display()); return false } };
    let catalog = Arc::new(catalog);
    let table = Arc::new(table);
    let guns: Vec<Gun> = fn_pose::loadout::ITEMS.iter().take(fn_pose::loadout_gun::STARTER_GUNS).enumerate().map(|(i, wid)| make_gun(i, wid, &table, &catalog)).collect();
    eprintln!("[muzzle] {} weapons in the table, flashes: {}", table.weapons.len(), guns.iter().map(|g| format!("{} -> {}", g.wid.rsplit('/').next().unwrap_or(""), g.flash.as_ref().map_or("none".to_string(), |f| f.system().name.clone()))).collect::<Vec<_>>().join(", "));
    *STATE.lock().unwrap() = Some(State { catalog, table, guns, fires: vec![], scope: false, feet_z: None, now: 0.0, gpu: Default::default(), log: vec![] });
    true
}

/// FIRST USE (2026-10-06): every muzzle-flash material and mesh of the catalog (all 45 BR weapons' systems) built under the loading screen
/// (`particles::warm_catalog`), so the first shot of a newly picked-up weapon decodes nothing on the UI thread.
pub fn prebuild(renderer: &mut Renderer) -> bool {
    let Ok(mut g) = STATE.lock() else { return true };
    let Some(st) = g.as_mut() else { return true };
    let cat = st.catalog.clone();
    let t = std::time::Instant::now();
    let Some((m, s)) = crate::particles::warm_catalog(renderer, &cat, &mut st.gpu) else { return false };
    if m + s > 0 { eprintln!("[muzzle] warm-up: {m} materials, {s} meshes built in {:.0} ms (before the first shot)", t.elapsed().as_secs_f32() * 1000.); }
    true
}

/// A firearm installed after start-up (gun index `gun` = the previous gun count): its flash from the table row of its WID (every BR weapon has one) and the catalog system it names.
/// Returns what is absent (no table row / system), never substitutes another weapon's flash; `Err` when the muzzle assets were not found at start (no flashes at all then).
pub fn add_gun(gun: usize, wid: &str) -> Result<String, String> {
    let mut g = STATE.lock().map_err(|e| e.to_string())?;
    let st = g.as_mut().ok_or("no muzzle assets loaded (particles-muzzle/muzzle.json): the firearms have no muzzle flash")?;
    if gun != st.guns.len() { return Err(format!("muzzle: gun index {gun} is out of order (have {})", st.guns.len())); }
    let g = make_gun(gun, wid, &st.table, &st.catalog);
    let w = st.table.weapon(wid);
    let note = match (w, &g.flash) { (None, _) => format!("{wid}: no row in the muzzle table, no flash"), (Some(_), None) => format!("{wid}: the table names no muzzle system / the catalog lacks it, no flash"), (Some(_), Some(f)) => format!("{wid} -> {}", f.system().name) };
    let note = format!("{note}{}{}", g.shells.as_ref().map_or(String::new(), |s| format!(", shells type {} ({}{}{})", s.data.shell_type, if s.data.on_fire { "fire" } else { "" }, if s.data.on_reload { "reload" } else { "" }, if s.data.on_pump { "pump" } else { "" })), g.smoke.as_ref().map_or(String::new(), |s| format!(", reload smoke {}", s.data.system.rsplit('/').next().unwrap_or(""))));
    st.guns.push(g);
    Ok(note)
}
/// A shot of firearm `gun` (loadout index) was fired this frame (`AFortWeapon::PlayWeaponFireFX` -> Blueprint event `OnPlayWeaponFireFX`).
pub fn push_fire(gun: usize) { push(Event::Fire(gun)); }
fn push(e: Event) { if let Ok(mut g) = STATE.lock() { if let Some(st) = g.as_mut() { st.fires.push(e); } } }
pub fn on_arms_sounds(sounds: &[crate::arms::ArmsSound]) {
    use crate::arms::ArmsSound;
    for s in sounds {
        match s {
            ArmsSound::ReloadStage { gun, stage } => push(Event::Reload(*gun, reload_stage_index(*stage))),
            ArmsSound::FireStop { gun } => push(Event::Stop(*gun)),
            _ => {}
        }
    }
}
fn reload_stage_index(s: fn_audio::weapon_events::ReloadStage) -> u8 { s as u8 }
pub fn set_feet_ue(z: Option<f32>) { if let Ok(mut g) = STATE.lock() { if let Some(st) = g.as_mut() { st.feet_z = z; } } }
pub fn remote_muzzle_ue(wid: &str, attach: Mat4) -> Option<Vec3> { let g = STATE.lock().ok()?; let st = g.as_ref()?; let s = st.table.weapon(wid)?.socket.as_ref()?; Some(fn_vfx::socket::muzzle_transform(attach, s).pos) }
pub fn catalog() -> Option<Arc<Catalog>> { STATE.lock().ok()?.as_ref().map(|st| st.catalog.clone()) }
pub fn with_instances(f: &mut dyn FnMut(&fn_particles::Instance)) {
    if let Ok(g) = STATE.lock() { if let Some(st) = g.as_ref() { for gun in &st.guns {
        if let Some(i) = gun.flash.as_ref().and_then(|f| f.instance()) { f(i); }
        if let Some(i) = gun.shells.as_ref().and_then(|s| s.instance()) { f(i); }
        if let Some(i) = gun.smoke.as_ref().and_then(|s| s.instance()) { f(i); }
    } } }
}
pub fn muzzle_ue(gun: usize) -> Option<Vec3> { STATE.lock().ok()?.as_ref()?.guns.get(gun)?.xf.map(|x| x.pos) }
/// The ADS driver's scope view state (`IsUsingScope` stand-in).
pub fn set_scope(on: bool) { if let Ok(mut g) = STATE.lock() { if let Some(st) = g.as_mut() { st.scope = on; } } }

/// Pose of the held firearm `gun` this frame: `attach` = world * RightHand socket (the weapon's attach matrix), `bone` = gun-space (`bone_mesh`) matrix of a weapon bone by name.
pub fn on_gun_pose(gun: usize, attach: Mat4, bone: &dyn Fn(&str) -> Option<Mat4>) {
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    let Some(gn) = st.guns.get_mut(gun) else { return };
    let at = |s: &Option<SocketInfo>| match s { Some(s) => bone(&s.bone).map(|b| fn_vfx::socket::muzzle_transform(attach * b, s)), None => Some(fn_vfx::socket::transform_from_game(attach)) };
    if let Some(sh) = gn.shells.as_ref() { gn.shells_xf = at(&sh.data.socket); }
    if let Some(sm) = gn.smoke.as_ref() { gn.smoke_xf = at(&sm.data.socket); }
    let Some(s) = gn.socket.as_ref() else { return };
    let Some(b) = bone(&s.bone) else { return };
    gn.xf = Some(fn_vfx::socket::muzzle_transform(attach * b, s));
}

/// Runs the shots queued since the last call, ticks every flash by `dt` and returns the draw data of the live ones (merged into the particle frame by `particles::submit`).
pub fn frame(renderer: &mut Renderer, camera: &CameraParams, dt: f32) -> Option<ParticleFrame> {
    let mut guard = STATE.lock().ok()?;
    let st = guard.as_mut()?;
    st.now += dt;
    let eye = [camera.eye.x, camera.eye.y, camera.eye.z];
    let eye_ue = fn_particles::sim::game_to_ue(eye);
    for ev in std::mem::take(&mut st.fires) {
        let idx = match ev {
            Event::Fire(i) => i,
            Event::Reload(i, stage) => {
                let Some(gun) = st.guns.get_mut(i) else { continue };
                let d = gun.smoke_xf.or(gun.shells_xf).map_or(0.0, |x| (x.pos - eye_ue).length());
                if let (Some(sm), Some(x)) = (gun.smoke.as_mut(), gun.smoke_xf) { sm.set_socket(x); sm.on_reload_fx(stage, d); }
                if let (Some(sh), Some(x)) = (gun.shells.as_mut(), gun.shells_xf) { sh.set_socket(x, Vec3::ZERO); sh.on_reload_fx(stage, d); }
                continue;
            }
            Event::Stop(i) => { if let Some(sh) = st.guns.get_mut(i).and_then(|g| g.shells.as_mut()) { sh.on_stop_weapon_fire_fx(); } continue; }
        };
        let (scope, now) = (st.scope, st.now);
        let Some(gun) = st.guns.get_mut(idx) else { continue };
        let shells = if let (Some(sh), Some(x)) = (gun.shells.as_mut(), gun.shells_xf) { sh.set_socket(x, Vec3::ZERO); if sh.on_play_weapon_fire_fx(scope, (x.pos - eye_ue).length()) { format!(", shells #{} at {:?}", sh.activations, x.pos) } else { String::new() } } else { String::new() };
        let (Some(flash), Some(xf)) = (gun.flash.as_mut(), gun.xf) else { continue };
        let dist_cm = (xf.pos - eye_ue).length();
        let ctx = FireContext { using_scope: scope, camera_distance_cm: dist_cm, now, magazine_ammo: 0, time_to_next_fire: 0.0 };
        let played = flash.on_play_weapon_fire_fx(gun.persistent_fire, gun.persistent_fire, &ctx, xf);
        let line = format!("t{:.2} gun {idx} {}: {} (scale {:.2}, lod {}, {:.2} m){shells}", now, gun.wid.rsplit('/').next().unwrap_or(""), if played { "flash" } else { "suppressed (scope)" }, flash.rel_scale, flash.system().lod_for_distance(dist_cm), dist_cm / 100.);
        logline(st, line);
    }
    let ground = |_x: f32, _y: f32| 0.0f32;
    let env = Env { ground: Some(&ground) };
    let feet = st.feet_z;
    let shell_ground = |x: f32, y: f32| { let t = fn_terrain::ground_y(x * 0.01, y * 0.01) * 100.; feet.map_or(t, |f| t.max(f)) };
    let shell_env = Env { ground: Some(&shell_ground) };
    for gun in st.guns.iter_mut() {
        if let (Some(sh), Some(x)) = (gun.shells.as_mut(), gun.shells_xf) {
            let vel = match (gun.shells_prev, dt > 1e-5) { (Some(p), true) => (x.pos - p) / dt, _ => Vec3::ZERO };
            gun.shells_prev = Some(x.pos);
            sh.set_socket(x, vel);
            sh.tick(dt, (x.pos - eye_ue).length(), &shell_env);
        }
        if let (Some(sm), Some(x)) = (gun.smoke.as_mut(), gun.smoke_xf) { sm.set_socket(x); sm.tick(dt, &env); }
    }
    let mut draws: Vec<(EmitterDraw, usize)> = vec![]; // (draw, gun index: whose component sorts it)
    for (gi, gun) in st.guns.iter_mut().enumerate() {
        let (Some(flash), Some(xf)) = (gun.flash.as_mut(), gun.xf) else { continue };
        // PartSysVelocity: the component's own movement (UE cm/s, world)
        let vel = match (gun.prev_pos, dt > 1e-5) { (Some(p), true) => (xf.pos - p) / dt, _ => Vec3::ZERO };
        gun.prev_pos = Some(xf.pos);
        flash.set_socket(xf, vel);
        flash.tick(dt, &env);
        let mut d = vec![]; flash.draw(eye, &mut d); draws.extend(d.into_iter().map(|x| (x, gi)));
    }
    let mut extra: Vec<(EmitterDraw, fn_render::fx::FxSort)> = vec![];
    if !crate::gunfx_hook::draws_muzzle() {
        let cat = st.catalog.clone();
        for gun in st.guns.iter() {
            for i in gun.shells.as_ref().and_then(|s| s.instance()).into_iter().chain(gun.smoke.as_ref().and_then(|s| s.instance())) {
                let sort = crate::particles::instance_sort(i, &cat, camera);
                let mut d = vec![]; i.draw(eye, &mut d); extra.extend(d.into_iter().map(|x| (x, sort)));
            }
        }
    }
    if std::env::var_os("FNRT_MUZZLE_DEBUG").is_some() {
        let s = renderer.particle_stats().clone(); eprintln!("[muzzle debug] renderer particle stats of the previous frame: {} draw calls, {} sprites, {} mesh instances, {} skipped", s.draw_calls, s.sprites, s.mesh_instances, s.skipped);
        for (d, _) in &draws { if let Some(p) = d.particles.first() {
            let c = (camera.proj_matrix() * camera.view_matrix()) * glam::Vec4::new(p.pos[0], p.pos[1], p.pos[2], 1.);
            eprintln!("[muzzle debug] t{:.3} {}/{} {:?} mat {} n={} pos=({:.3},{:.3},{:.3}) size=({:.3},{:.3},{:.3}) color={:?} sub={:.1} ndc=({:.2},{:.2}) w={:.2} eye=({:.2},{:.2},{:.2})", st.now, d.system, d.emitter, d.kind, d.material.rsplit('/').next().unwrap_or(""), d.particles.len(), p.pos[0], p.pos[1], p.pos[2], p.size[0], p.size[1], p.size[2], p.color, p.sub_image, c.x / c.w, c.y / c.w, c.w, eye[0], eye[1], eye[2]);
        } }
    }
    if (draws.is_empty() && extra.is_empty()) || crate::gunfx_hook::draws_muzzle() { return None; }
    let mut frame = ParticleFrame::default();
    let cat = st.catalog.clone();
    // SORTFIX: every flash batch carries its component's sort data (dynamic bounds origin, TranslucentRendering.cpp:864)
    let sorts: Vec<Option<fn_render::fx::FxSort>> = st.guns.iter().map(|g| g.flash.as_ref().and_then(|f| f.instance()).map(|i| crate::particles::instance_sort(i, &cat, camera))).collect();
    for (d, gi) in &draws { let Some(sort) = sorts[*gi] else { continue }; crate::particles::batches(renderer, &cat, &mut st.gpu, d, sort, &mut frame); }
    for (d, sort) in &extra { crate::particles::batches(renderer, &cat, &mut st.gpu, d, *sort, &mut frame); }
    Some(frame)
}

/// Dynamic particle lights of the live flashes (game space). Not drawn yet: the in-game renderer has no dynamic point lights (pending renderer work).
pub fn lights() -> Vec<ParticleLight> {
    let Ok(g) = STATE.lock() else { return vec![] };
    g.as_ref().map(|st| st.guns.iter().filter_map(|g| g.flash.as_ref()).flat_map(|f| f.lights()).collect()).unwrap_or_default()
}

/// `(gun, flash live particles)` for scripted-run reports.
pub fn report() -> String {
    let Ok(g) = STATE.lock() else { return String::new() };
    let Some(st) = g.as_ref() else { return String::new() };
    let mut s = String::new();
    for (i, gun) in st.guns.iter().enumerate() { if let Some(f) = &gun.flash { s.push_str(&format!("muzzle: gun {i} {} flashes played {} suppressed {}\n", f.system().name, f.played, f.suppressed)); } }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Every one of the 45 BR firearms has a row in the exported muzzle table and the particle system it names is in the catalog (so `add_gun` gives each a flash). Needs the exported
    /// `particles-muzzle` folder (`FN_MUZZLE_DIR`, or `assets/particles-muzzle` of an asset root in `FN_ASSETS`); skipped (printed) without it, like the game's own start-up check.
    #[test]
    fn every_br_weapon_has_a_muzzle_row_and_system_in_the_exported_catalog() {
        let roots: Vec<PathBuf> = fn_live::var_os("FN_ASSETS").map(|p| vec![PathBuf::from(p)]).unwrap_or_default();
        let dir = fn_live::var_os("FN_MUZZLE_DIR").map(PathBuf::from).filter(|d| d.join("muzzle.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-muzzle")).find(|d| d.join("muzzle.json").is_file()));
        let Some(dir) = dir else { eprintln!("muzzle assets not found (FN_MUZZLE_DIR / FN_ASSETS): skipped"); return };
        let (table, catalog) = (MuzzleTable::load(&dir.join("muzzle.json")).unwrap(), Catalog::load(&dir).unwrap());
        let mut missing = vec![];
        for w in fn_animgraph::weapon_assets::BR_WEAPON_ASSETS {
            let wid = format!("/Game/Athena/Items/Weapons/{}", w.wid);
            match table.weapon(&wid) {
                None => missing.push(format!("{}: no row", w.wid)),
                Some(r) => {
                    let Some(sys) = r.muzzle_system.as_deref() else { missing.push(format!("{}: row names no muzzle system", w.wid)); continue };
                    if catalog.system(sys).is_none() { missing.push(format!("{}: system {sys} not in the catalog", w.wid)); }
                    if r.socket.is_none() { missing.push(format!("{}: no Muzzle socket", w.wid)); }
                    if r.persistent_fire != w.persistent_fire { missing.push(format!("{}: persistent_fire {} != the generated asset table {}", w.wid, r.persistent_fire, w.persistent_fire)); }
                }
            }
        }
        assert!(missing.is_empty(), "{missing:?}");
    }
}
