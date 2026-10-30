//! FOOT FX (fn-app): the dust the 1.8 BR pawn kicks up when it starts / stops running, while it sprints and when it lands. The gate logic (thresholds,
//! DoOnce / Delay gates, rotations, the landing offset, all recovered from `PlayerPawn_Athena_Generic`) is `fn_game::foot_fx`; this module owns the real
//! Cascade systems (exported to `<assets>/particles-footfx` by `fn-particles-export`) and draws them with their ORIGINAL cooked material shaders.
//!
//! * [`init`] (from `app.rs::start_world_load`): loads the catalog (`FN_FOOTFX_DIR` overrides). Without it nothing is drawn.
//! * [`on_frame`] (once per frame, after the movement step): one Blueprint tick + `OnLanded`, applied to the two attached components and the landing bursts.
//! * [`fx_frame`] (from `app.rs`, with the other FX-pass draws): ticks them by the world delta and draws them through `pickups_dxbc::Dxbc` (the base-pass
//!   programs of each material's cooked shader map for `FMeshParticleVertexFactory` / `FParticleSpriteVertexFactory`, as the building destruction effects).
//!   That path is required here: `M_SmokeMaster_Masked_01` (the Swoosh / Puffs / Ring meshes) is a masked DefaultLit material whose vertex shader animates
//!   the `*_MorphExport` meshes from its `Morph Targets Texture` (world position offset) and whose pixel shader erodes the opacity mask; the catalog `Look`
//!   heuristic of `particles.rs` cannot express either (it picked the morph texture as the colour map and drew nothing visible).
//! * [`prebuild`] (loading screen): loads the paks / materials / meshes once by drawing one warm-up frame of the three systems (no first-use hitch).
//!
//! The two components are persistent (`UParticleSystemComponent`s on the mesh socket `FX_Feet`): `Activate(true)` = `Instance::reactivate` (live particles stay),
//! `Activate(false)` on an inactive one the same, on an active one nothing, `Deactivate` = `Instance::deactivate`. `SetWorldRotation` on an attached component
//! stores the rotation relative to its parent, so the yaw is kept relative to the body (`Controller::facing`, the yaw the mesh is drawn with) and follows it.
//! `CustomTimeDilation` scales the component's tick. `SetEmitterEnable(Puffs, ..)` is applied, but `Puffs` of `P_Walk_DustKickup_01` has every LOD level
//! disabled in the cooked asset, so it never spawns (as in the original). The landing burst is a fresh auto-destroyed system per landing.
//! NOT PORTED: the water variants (`Effect_Player_*_Water`, no water in the port), the landing wind RTT particle, LOD re-evaluation after the first activation
//! (the LOD is picked from the camera distance when the component is first activated; the third-person camera stays inside LOD 0).
use crate::pickups_dxbc::Dxbc;
use fn_fx::view::ViewParams;
use fn_game::foot_fx::{Cmd, FootFx, Input, FEET_FX_HEIGHT_CM, P_LAND, P_RUN, P_WALK};
use fn_game::GameSim;
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Transform};
use fn_render::fx::{FxFrame, FxRenderer};
use glam::{Mat3, Vec3};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One attached `UParticleSystemComponent`.
#[derive(Default)]
struct Comp { inst: Option<Instance>, active: bool, rel_yaw: f32, scale: f32, time_dilation: f32 }
struct State {
    catalog: Option<Arc<Catalog>>, install: Option<PathBuf>, dxbc: Option<Result<Dxbc, String>>, logic: FootFx, walk: Comp, run: Comp, lands: Vec<Instance>,
    seen_landings: Option<u32>, feet_ue: Vec3, parent_yaw: f32, vel_ue: Vec3, serial: u64, errors: std::collections::HashSet<String>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
static WARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn new_state(catalog: Option<Arc<Catalog>>, install: Option<PathBuf>) -> State {
    State { catalog, install, dxbc: None, logic: FootFx::default(), walk: Comp { scale: 1., time_dilation: 1., ..Default::default() }, run: Comp { scale: 1., time_dilation: 1., ..Default::default() }, lands: vec![], seen_landings: None, feet_ue: Vec3::ZERO, parent_yaw: 0., vel_ue: Vec3::ZERO, serial: 0, errors: Default::default() }
}

/// Loads `<root>/particles-footfx` (or `FN_FOOTFX_DIR`); `install` = the 1.8 install whose paks hold the cooked materials.
pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    crate::waterfall_fx::init(roots);
    let dir = fn_live::var_os("FN_FOOTFX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-footfx")).find(|d| d.join("index.json").is_file()));
    // `FN_FOOTFX=0`: off (A/B frame-time comparisons)
    let off = fn_live::var("FN_FOOTFX").map_or(false, |v| v == "0");
    let catalog = match dir.as_ref().filter(|_| !off).map(|d| Catalog::load(d)) {
        Some(Ok(c)) if [P_WALK, P_RUN, P_LAND].iter().all(|p| c.system(p).is_some()) => { eprintln!("[footfx] walk / run / land dust loaded from {}", dir.as_ref().unwrap().display()); Some(Arc::new(c)) }
        Some(Ok(_)) => { eprintln!("[footfx] the particles-footfx catalog lacks P_Walk_DustKickup_01 / P_Run_DustKickup_01 / P_Player_Land_Effect: no foot dust"); None }
        Some(Err(e)) => { eprintln!("[footfx] {e}: no foot dust"); None }
        None if off => { eprintln!("[footfx] off (FN_FOOTFX=0)"); None }
        None => { eprintln!("[footfx] no `particles-footfx` folder in the asset roots (fn-particles-export --out <assets>/particles-footfx P_Walk_DustKickup_01 P_Run_DustKickup_01 P_Player_Land_Effect): no foot dust"); None }
    };
    if install.is_none() { eprintln!("[footfx] no install (paks): the dust materials cannot be built, no foot dust drawn"); }
    WARMED.store(false, std::sync::atomic::Ordering::Relaxed);
    *STATE.lock().unwrap() = Some(new_state(catalog, install));
}

fn yaw_rot(yaw_deg: f32) -> Mat3 { Mat3::from_rotation_z(yaw_deg.to_radians()) }

/// The cooked-material drawer (opens the paks on first use; a failure is remembered and reported once).
fn dxbc<'a>(slot: &'a mut Option<Result<Dxbc, String>>, install: &Option<PathBuf>, fx: &mut FxRenderer) -> Option<&'a mut Dxbc> {
    if slot.is_none() {
        let r = match install { Some(d) => Dxbc::load(d, fx), None => Err("no install".into()) };
        if let Err(e) = &r { eprintln!("[footfx] dust materials unavailable: {e}"); }
        *slot = Some(r);
    }
    slot.as_mut().and_then(|r| r.as_mut().ok())
}

/// Draws `inst`'s emitters into `dx`'s frame (errors reported once per emitter).
fn draw(dx: &mut Dxbc, fx: &mut FxRenderer, cat: &Catalog, inst: &Instance, eye: [f32; 3], dt: f32, errors: &mut std::collections::HashSet<String>) {
    let mut d = vec![];
    inst.draw(eye, &mut d);
    let bounds = crate::particles::instance_bounds(inst, cat);
    for e in &d {
        if let Err(err) = dx.draw_emitter(fx, e, &inst.xf, &bounds, dt) {
            let k = format!("{}/{}", e.system, e.emitter);
            if errors.insert(k.clone()) { eprintln!("[footfx] {k}: NOT DRAWN: {err}"); }
        }
    }
}

/// FIRST USE: opens the paks and builds every material / mesh of the three systems under the loading screen (one throw-away frame of each, moved so the
/// SpawnPerUnit emitters spawn too).
pub fn prebuild(fx: &mut FxRenderer) -> bool {
    static AT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if WARMED.load(std::sync::atomic::Ordering::Relaxed) { return true; }
    let Ok(mut g) = STATE.lock() else { return true };
    let Some(st) = g.as_mut() else { return true };
    let Some(cat) = st.catalog.clone() else { return true };
    let t = std::time::Instant::now();
    let first = st.dxbc.is_none();
    let Some(dx) = dxbc(&mut st.dxbc, &st.install, fx) else { return true };
    if first { return false; }
    let view = ViewParams { eye: Vec3::new(0., 2., 3.), forward: Vec3::new(0., -0.5, -0.85).normalize(), up: Vec3::Y, tan_half: (1., 0.6), near_m: 0.1, far_m: 1000., width: 64, height: 64, time: 0., real_time: 0., delta_world: 1. / 60. };
    dx.begin(&view);
    let env = Env::none();
    let at = AT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for p in [P_WALK, P_RUN, P_LAND].into_iter().skip(at).take(1) {
        let Some(s) = cat.system(p) else { continue };
        let mut i = Instance::new(s, Transform::identity(), 1, 100.);
        for k in 0..12 { i.xf.pos = Vec3::new(k as f32 * 10., 0., 0.); i.tick(1. / 30., &env); if k == 0 || k == 11 { draw(dx, fx, &cat, &i, [0., 2., 3.], 1. / 60., &mut st.errors); } }
    }
    if at == 0 { crate::waterfall_fx::warm(dx, fx); }
    let _ = dx.take_frame();
    if at + 1 < 3 { return false; }
    WARMED.store(true, std::sync::atomic::Ordering::Relaxed);
    eprintln!("[footfx] warm-up: dust materials / meshes built in {:.0} ms", t.elapsed().as_secs_f32() * 1000.);
    true
}

/// The dust of this frame, ticked by the world delta `dt`, as FX-pass draws appended to `out`.
pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
    let Ok(mut guard) = STATE.lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let Some(cat) = st.catalog.clone() else { return };
    let falls = crate::waterfall_fx::tick(view, dt);
    if st.walk.inst.is_none() && st.run.inst.is_none() && st.lands.is_empty() && !falls { return; }
    // TERRAIN: the particles' ground (collision modules), as in particles::tick_all
    let env_ground = |x: f32, y: f32| fn_terrain::ground_y(x * 0.01, y * 0.01) * 100.;
    let env = Env { ground: Some(&env_ground) };
    let (feet, parent, vel) = (st.feet_ue, st.parent_yaw, st.vel_ue);
    for c in [&mut st.walk, &mut st.run] {
        let Some(i) = c.inst.as_mut() else { continue };
        i.xf = Transform { pos: feet, rot: yaw_rot(parent + c.rel_yaw), scale: c.scale };
        i.part_sys_velocity = vel;
        i.tick(dt * c.time_dilation, &env);
    }
    for l in st.lands.iter_mut() { l.tick(dt, &env); }
    st.lands.retain(|l| !l.finished());
    let eye = [view.eye.x, view.eye.y, view.eye.z];
    let debug = dt > 0. && fn_live::var_os("FN_FOOTFX_DEBUG").is_some();
    let State { dxbc: slot, install, walk, run, lands, errors, .. } = st;
    let Some(dx) = dxbc(slot, install, fx) else { return };
    dx.begin(view);
    for i in [walk.inst.as_ref(), run.inst.as_ref()].into_iter().flatten().chain(lands.iter()).filter(|i| !i.finished()) {
        if debug { eprintln!("[footfx debug] {} live {} at UE ({:.0}, {:.0}, {:.0})", i.system.name, i.live_count(), i.xf.pos.x, i.xf.pos.y, i.xf.pos.z); }
        draw(dx, fx, &cat, i, eye, dt, errors);
    }
    if falls { crate::waterfall_fx::draw_into(dx, fx, view, dt); }
    let fr = dx.take_frame();
    out.buffer_writes.extend(fr.buffer_writes);
    out.draws.extend(fr.draws);
}

/// One Blueprint tick of the pawn's dust (`dt` = frame time).
pub fn on_frame(sim: &GameSim, dt: f32) {
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    let Some(cat) = st.catalog.clone() else { return };
    let c = &sim.controller;
    let cmc = c.component();
    // UE world = controller space + the game's z offset (game z = controller y + Z_OFFSET); actor location = capsule centre
    let shift = Vec3::new(0., fn_game::sim::Z_OFFSET * 100., 0.);
    let actor = cmc.loc + shift;
    st.feet_ue = Vec3::new(actor.x, actor.y, cmc.feet_z() + FEET_FX_HEIGHT_CM);
    st.vel_ue = cmc.velocity;
    // port yaw f faces UE (sin f, -cos f): UE yaw = f - 90. The actor follows the control yaw, the mesh (the components' parent) the body yaw.
    st.parent_yaw = c.facing - 90.;
    let landings = c.landings();
    let landed = st.seen_landings.map_or(false, |n| n != landings);
    st.seen_landings = Some(landings);
    let on_ground = sim.fly == fn_game::sim::FlyMode::Walk && cmc.is_moving_on_ground();
    let cmds = st.logic.update(&Input { on_ground, velocity: cmc.velocity, landed, actor_loc: actor, actor_yaw_deg: sim.yaw - 90., dt });
    let eye = fn_particles::sim::game_to_ue([sim.cam_view.eye.x, sim.cam_view.eye.y, sim.cam_view.eye.z]);
    let dist = (eye - st.feet_ue).length();
    for cmd in cmds {
        match cmd {
            Cmd::WalkActivate { yaw_deg, scale, time_dilation, puffs } => {
                let (feet, parent) = (st.feet_ue, st.parent_yaw);
                st.serial += 1; let seed = 0x5741_4c4b ^ st.serial;
                let w = &mut st.walk;
                w.rel_yaw = yaw_deg - parent; w.scale = scale; w.time_dilation = time_dilation; w.active = true;
                let xf = Transform { pos: feet, rot: yaw_rot(yaw_deg), scale };
                match w.inst.as_mut() { Some(i) => { i.xf = xf; i.reactivate(); } None => w.inst = cat.system(P_WALK).map(|s| Instance::new(s, xf, seed, dist)) }
                if let Some(i) = w.inst.as_mut() { i.set_emitter_enabled("Puffs", puffs); }
                eprintln!("[footfx] f{} walk dust (yaw {yaw_deg:.0}, scale {scale}, dilation {time_dilation}, puffs {puffs}) speed {:.0}", sim.frames, Vec3::new(cmc.velocity.x, cmc.velocity.y, 0.).length());
            }
            Cmd::RunActivate { yaw_deg } => {
                let (feet, parent) = (st.feet_ue, st.parent_yaw);
                st.serial += 1; let seed = 0x5255_4e00 ^ st.serial;
                let r = &mut st.run;
                r.rel_yaw = yaw_deg - parent;
                if !r.active {
                    let xf = Transform { pos: feet, rot: yaw_rot(yaw_deg), scale: r.scale };
                    match r.inst.as_mut() { Some(i) => { i.xf = xf; i.reactivate(); } None => r.inst = cat.system(P_RUN).map(|s| Instance::new(s, xf, seed, dist)) }
                    r.active = true;
                    eprintln!("[footfx] f{} run dust on (yaw {yaw_deg:.0})", sim.frames);
                }
            }
            Cmd::RunDeactivate => { if st.run.active { st.run.active = false; if let Some(i) = st.run.inst.as_mut() { i.deactivate(); } eprintln!("[footfx] f{} run dust off", sim.frames); } }
            Cmd::WalkDeactivate => { if st.walk.active { st.walk.active = false; if let Some(i) = st.walk.inst.as_mut() { i.deactivate(); } } }
            Cmd::Land { loc_ue, yaw_deg } => {
                st.serial += 1;
                if let Some(s) = cat.system(P_LAND) {
                    let d = (eye - loc_ue).length();
                    st.lands.push(Instance::new(s, Transform { pos: loc_ue, rot: yaw_rot(yaw_deg), scale: 1. }, 0x4c41_4e44 ^ st.serial, d));
                    eprintln!("[footfx] f{} land dust at UE ({:.0}, {:.0}, {:.0}) yaw {yaw_deg:.0}", sim.frames, loc_ue.x, loc_ue.y, loc_ue.z);
                }
            }
        }
    }
}

