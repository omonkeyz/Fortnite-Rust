//! PICKUPS FX (fn-app): the rarity effects of a world item, driven like `/Game/Effects/Fort_Effects/Gameplay/Pickups/B_Pickups` drives them (the Blueprint `AFortPickupEffect` that
//! `FortGameData.PickupEffectBlueprintClass` names; its Kismet bytecode was read with `fn-inspect disasm`). The simulation is `fn_particles` (the Cascade systems with their instance
//! parameters), the drawing is the particle pass of `fn-render` through `particles::batches` (the same path as the muzzle flash).
//!
//! * `P_Pickup` (Sphere mesh `Sm_Pickup_BG_01`, Rays mesh, Light, Sparkle, GodRays) lives on the actor from the toss until the item arrives, with the Blueprint's parameters:
//!   `Color` = `Outline Rarity Colors[rarity]`, `SparkleSphereRadius` = `Component Radius (Scaled)` = bounds sphere radius * `Component Radius Multiplier` (0.75),
//!   `SparkleRateScale` = `Sparkle Spawn Rate (Ground)[rarity]`, `BGSphereRadius` = `RadiusUVScale` = `Sphere Radius` = clamp(bounds sphere radius / 40, 0.9, 1.65), the GodRays
//!   emitter only for rarity >= 4 (`SetEmitterEnable("GodRays", Rarity Level >= 4)`).
//! * on pickup: `SpawnPickedUpTrailPS` = `P_PickupRibbon_01` attached to the actor with `SPU Scale` = `Sparkle Spawn Rate (Picked Up)[rarity]`, `Lifetime` = `Lifetime (Picked Up)[rarity]`,
//!   `SparkleSphereRadius`, `Color` = the rarity colour; the Sparkle emitter is switched off and `AlphaLight` set to 0 (the Blueprint does both when the pickup is taken); a resource
//!   definition also plays `P_Loot_Effect_02` at the actor (`OnPickedUp`).
//!
//! DRAWING: every emitter and the hologram are drawn with the ORIGINAL cooked shader programs of their materials (`pickups_dxbc.rs`: the DXBC of the base pass of the sprite / mesh-particle / skin / local vertex factory, fed with the engine's per-particle data and the Blueprint's MID parameters). The old lookalike material path of `particles.rs` is only used for an emitter the DXBC path cannot draw AND only with `FNRT_PICKUP_LOOKALIKE_FALLBACK=1` (each such emitter is logged); without the flag it is left out and logged as NOT DRAWN.
//!
//! NOT DRAWN (DATA-ABSENT / not ported, reported, not faked): the ribbon `P_PickupRibbon_01/Ribbon` (`ParticleModuleTypeDataRibbon` is not decoded by the importer, and `M_Pickup_Ribbon_01` only has `FParticleBeamTrailVertexFactory` programs, no sprite ones); the three unnamed loot ribbon mesh emitters of `P_Loot_Effect_02` (`M_Loot_Ribbons_02unlit`: its cooked mesh-particle vertex program has no `SV_Position` output); dynamic point lights of `ParticleModuleLight` (the `Light` emitter is `ERM_LightsOnly`, it draws nothing else).
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Params, ParticleSystem, Transform};
use fn_render::fx::FxFrame;
use fn_render::particles::ParticleFrame;
use fn_render::{CameraParams, Renderer};
use glam::Vec3;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const P_PICKUP: &str = "/Game/Effects/Fort_Effects/Gameplay/Pickups/Effects/P_Pickup";
pub const P_RIBBON: &str = "/Game/Effects/Fort_Effects/Gameplay/Pickups/Effects/P_PickupRibbon_01";
pub const P_LOOT: &str = "/Game/Effects/Fort_Effects/Effects/LootContainers/P_Loot_Effect_02";

/// `B_Pickups` CDO `Outline Rarity Colors` (index = `EFortRarity`: Handmade, Ordinary, Sturdy, Quality, Fine, Elegant, Masterwork, Epic, Badass, Legendary), linear RGBA.
pub const OUTLINE_RARITY_COLORS: [[f32; 4]; 10] = [
    [0.085, 0.067_984, 0.052_7, 1.0], [0.027_193, 0.1, 0.013_316, 1.0], [0.058_398, 0.322_668, 0.687_03, 1.0], [0.213_776, 0.073_712, 0.982_83, 1.0], [0.555, 0.328_764, 0.024_975, 1.0],
    [0.775, 0.325_329, 0.015_5, 1.0], [0.947_307, 0.234_551, 0.234_551, 1.0], [0.090_842, 0.584_078, 0.006_049, 1.0], [0.090_842, 0.584_078, 0.006_049, 1.0], [0.090_842, 0.584_078, 0.006_049, 1.0],
];
/// `Sparkle Spawn Rate (Ground)`, `Sparkle Spawn Rate (Picked Up)`, `Lifetime (Picked Up)` per rarity (the CDO arrays).
pub const SPARKLE_GROUND: [f32; 10] = [1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0, 5.5];
pub const SPARKLE_PICKED: [f32; 10] = [0.5, 2.5, 4.0, 5.5, 7.0, 8.5, 10.0, 11.5, 13.0, 14.5];
pub const LIFETIME_PICKED: [f32; 10] = [0.14, 0.175, 0.25, 0.325, 0.4, 0.475, 0.55, 0.625, 0.7, 0.775];
pub const SHRINK_SPHERE: [(f32, f32, f32, f32, bool); 4] = [
    (0.001_411_199_6, -0.009_729_385, -1.090_772_6, -1.090_772_6, true),
    (0.157_685_88, 0.166_297_14, 6.163_026, 6.163_044, true),
    (0.275_955_92, 0.849_048_85, 2.415_346, 2.415_338_5, true),
    (0.468_137_03, 0.982_944_5, 0.0, 0.0, false),
];
/// `Component Radius Multiplier` of the CDO.
pub const RADIUS_MULTIPLIER: f32 = 0.75;

/// The parameters `B_Pickups` sets on `P_Pickup` for an item of `rarity` whose mesh bounds have sphere radius `radius_cm`.
pub fn beam_params(rarity: u8, radius_cm: f32) -> Params {
    let r = (rarity as usize).min(9);
    let sphere = (radius_cm / 40.).clamp(0.9, 1.65);
    let mut p = Params::new();
    p.set_color("Color", OUTLINE_RARITY_COLORS[r]);
    p.set_float("SparkleSphereRadius", radius_cm * RADIUS_MULTIPLIER);
    p.set_float("SparkleRateScale", SPARKLE_GROUND[r]);
    p.set_vector("BGSphereRadius", [sphere; 3]);
    p.set_float("RadiusUVScale", sphere);
    p
}
/// The parameters of `SpawnPickedUpTrailPS` for `P_PickupRibbon_01`.
pub fn trail_params(rarity: u8, radius_cm: f32) -> Params {
    let r = (rarity as usize).min(9);
    let c = OUTLINE_RARITY_COLORS[r];
    let mut p = Params::new();
    p.set_float("SPU Scale", SPARKLE_PICKED[r]);
    p.set_float("SparkleSphereRadius", radius_cm * RADIUS_MULTIPLIER);
    p.set_float("Lifetime", LIFETIME_PICKED[r]);
    p.set_vector("Color", [c[0], c[1], c[2]]);
    p
}

struct Actor { beam: Instance, trail: Option<Instance>, picked: bool, accum: f32, last_seen: f32, radius_cm: f32, rarity: u8, resource: bool, /// The hologram of B_Pickups (mesh path + its MIDs), when the item mesh is known.
    holo: Option<(String, crate::pickups_dxbc::HoloActor)> }
struct Burst { inst: Instance }

struct State {
    /// The ORIGINAL cooked shader programs of the pickup materials (`pickups_dxbc.rs`); `None` until the first frame / when the paks are unavailable.
    dxbc: Option<crate::pickups_dxbc::Dxbc>,
    dxbc_tried: bool,
    /// `FNRT_PICKUP_LOOKALIKE_FALLBACK=1`: emitters the DXBC path cannot draw (not ported yet) fall back to the old `fn_particles::assets::Look` lookalike; off = they are left out.
    lookalike_fallback: bool,
    clock: f32,
    warned: std::collections::HashSet<String>,
    catalog: Arc<Catalog>,
    beam: Arc<ParticleSystem>,
    ribbon: Option<Arc<ParticleSystem>>,
    loot: Option<Arc<ParticleSystem>>,
    actors: HashMap<u64, Actor>,
    bursts: Vec<Burst>,
    orphan_trails: Vec<Instance>,
    gpu: crate::particles::Gpu,
    serial: u64,
    log: Vec<String>,
    /// STATIC ITEM (2026-10-05): `PickupStaticMesh` items are drawn with their cooked materials (`pickups_dxbc::Dxbc::draw_static_item`); set every frame by
    /// [`static_item_ready`] (off with `--standin-gun-material`, which keeps every pickup on the renderer `Prop` model).
    static_cooked: bool,
    /// FIRST USE (2026-10-06): the warm-up queue of [`prebuild_step`] (`None` = not started) and when it started.
    warm: Option<std::collections::VecDeque<Warm>>,
    warm_t0: Option<std::time::Instant>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// FIRST USE (2026-10-06): what the first pickup of each kind built on the UI thread, built under the loading screen instead: P_Pickup's materials (109-163 ms at
/// the first spawn natively), the hologram of every new weapon mesh (11-30 ms each) and the cooked materials of the static items (verification/first_use/run5).
enum Warm { Effects, Mesh(String, bool) }
/// The view of the warm-up's scratch draws (nothing is presented; the hologram cull and the sort keys read it).
const WARM_VIEW: fn_fx::view::ViewParams = fn_fx::view::ViewParams { eye: Vec3::ZERO, forward: Vec3::NEG_Z, up: Vec3::Y, tan_half: (1.0, 0.5625), near_m: 0.1, far_m: 1000.0, width: 1920, height: 1080, time: 0.0, real_time: 0.0, delta_world: 0.0 };

/// One budgeted step of the warm-up: call every loading-screen frame until it returns true (`App::finishing_frame`). The effects are simulated for 0.6 s
/// and drawn into a scratch frame; every item mesh of the catalog gets its hologram (weapons) or its cooked static materials (ammo, consumables).
pub fn prebuild_step(renderer: &mut Renderer, budget_ms: f32) -> bool {
    if !crate::building_render::warm_enabled() { return true; }
    let Ok(mut guard) = STATE.lock() else { return true };
    let Some(st) = guard.as_mut() else { return true };
    ensure_dxbc(st, renderer);
    if st.dxbc.is_none() { return true; }
    let t = std::time::Instant::now();
    if st.warm.is_none() {
        // the item meshes: natively by default now that identical FX pipelines are shared (29 weapon holograms: 6.0 s -> 2.0 s of loading, run6 / run8;
        // without it the first hologram cost 44 ms and each new weapon's 11-16 ms at its first spawn). In the browser only with `FN_WARM_ITEMS=1` (its
        // per-item mesh / texture reads still run on the main thread); `FN_WARM_ITEMS=0` turns it off natively.
        let items_on = match fn_live::var("FN_WARM_ITEMS").ok().as_deref() { Some("0") => false, Some(_) => true, None => true }; // WEB 60: in the browser too (a longer loading screen instead of a 83-222 ms freeze at each first spawn)
        let items = if items_on { match crate::pickups_hook::item_meshes() { Some(i) => i, None => return false } } else { vec![] }; // None: the item catalog is still being read
        let mut q = std::collections::VecDeque::new();
        q.push_back(Warm::Effects);
        q.extend(items.into_iter().map(|(p, s)| Warm::Mesh(p, s)));
        st.warm = Some(q);
        st.warm_t0 = Some(t);
    }
    while let Some(task) = st.warm.as_mut().and_then(|q| q.pop_front()) {
        let dx = st.dxbc.as_mut().unwrap();
        let view = WARM_VIEW;
        dx.begin(&view);
        match task {
            Warm::Effects => {
                let ground = |_x: f32, _y: f32| -1.0e6f32; // no floor: every emitter keeps its particles for the scratch draws
                let env = Env { ground: Some(&ground) };
                let mut systems: Vec<(Arc<ParticleSystem>, Params, bool)> = vec![(st.beam.clone(), beam_params(4, 60.), true), (st.beam.clone(), beam_params(0, 60.), false)];
                if let Some(r) = &st.ribbon { systems.push((r.clone(), trail_params(4, 60.), false)); }
                if let Some(l) = &st.loot { systems.push((l.clone(), Params::default(), false)); }
                for (sys, params, god) in systems {
                    let mut inst = Instance::new_with_params(sys, xf(Vec3::new(0., 0., -3.), 0.), 0x5741_524D, 0., &params);
                    if god { inst.set_emitter_enabled("GodRays", true); }
                    let dt = 1.0 / 30.0;
                    for _ in 0..18 {
                        inst.tick(dt, &env);
                        let mut d = vec![];
                        inst.draw([0., 0., 0.], &mut d);
                        let bounds = crate::particles::instance_bounds(&inst, &st.catalog);
                        for e in &d { let _ = dx.draw_emitter(renderer.fx(), e, &inst.xf, &bounds, dt); }
                        if inst.finished() { break; }
                    }
                }
            }
            Warm::Mesh(path, true) => {
                if dx.holo_mesh(renderer.fx(), &path, true).is_ok() {
                    let actor = crate::pickups_dxbc::HoloActor { rarity: 0, random: 0.0, normals: dx.holo_normals(&path), picked_at: None };
                    let _ = dx.draw_hologram(renderer.fx(), &path, &actor, Vec3::ZERO, glam::Mat3::IDENTITY, [0.0; 3], Vec3::ZERO);
                }
            }
            Warm::Mesh(path, false) => { let _ = dx.static_ready(renderer.fx(), &path); }
        }
        let _ = dx.take_frame();
        if t.elapsed().as_secs_f32() * 1000.0 >= budget_ms { break; }
    }
    let done = st.warm.as_ref().map_or(true, |q| q.is_empty());
    if done { if let Some(t0) = st.warm_t0.take() { let m = format!("warm-up: pickup effects + item holograms / static materials built in {:.0} ms (before the first spawn)", t0.elapsed().as_secs_f32() * 1000.); eprintln!("[pickups-fx] {m}"); st.log.push(m); } }
    done
}
/// SORTFIX: the frame's clocks (world time, real time, world delta) from `app.rs`. The effects tick on WORLD time like the engine (the components tick with the
/// world's delta and stop while it is paused; the materials see `View.GameTime` = `CurrentWorldTime`, SceneView.cpp:2276), not on the frame clock.
static WORLD_CLOCK: Mutex<(f32, f32, f32)> = Mutex::new((0.0, 0.0, 0.0));
pub fn set_world_clock(world: f32, real: f32, delta_world: f32) { if let Ok(mut c) = WORLD_CLOCK.lock() { *c = (world, real, delta_world); } }
fn world_clock() -> (f32, f32, f32) { WORLD_CLOCK.lock().map(|c| *c).unwrap_or_default() }

/// Loads `<root>/particles-pickup` (or `FN_PICKUP_FX_DIR`) of the first asset root that has an `index.json` (written by `fn-particles-export ... --material ...`, see the README).
pub fn init(roots: &[PathBuf]) -> bool {
    let dir = fn_live::var_os("FN_PICKUP_FX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-pickup")).find(|d| d.join("index.json").is_file()));
    let Some(dir) = dir else { eprintln!("[pickups-fx] no `particles-pickup` folder with an index.json in the asset roots (run fn-particles-export for P_Pickup / P_PickupRibbon_01): the pickups have no rarity effects"); return false };
    let cat = match Catalog::load(&dir) { Ok(c) => Arc::new(c), Err(e) => { eprintln!("[pickups-fx] cannot load {}: {e}", dir.display()); return false } };
    let Some(beam) = cat.system(P_PICKUP) else { eprintln!("[pickups-fx] P_Pickup is not in {}: no rarity effects", dir.display()); return false };
    let (ribbon, loot) = (cat.system(P_RIBBON), cat.system(P_LOOT));
    eprintln!("[pickups-fx] P_Pickup{}{} loaded from {}", if ribbon.is_some() { " + P_PickupRibbon_01" } else { " (no ribbon)" }, if loot.is_some() { " + P_Loot_Effect_02" } else { "" }, dir.display());
    *STATE.lock().unwrap() = Some(State { dxbc: None, dxbc_tried: false, lookalike_fallback: std::env::var_os("FNRT_PICKUP_LOOKALIKE_FALLBACK").is_some(), clock: world_clock().0, warned: Default::default(), catalog: cat, beam, ribbon, loot, actors: HashMap::new(), bursts: vec![], orphan_trails: vec![], gpu: Default::default(), serial: 0, log: vec![], static_cooked: false, warm: None, warm_t0: None });
    true
}

const PICKUP_FX_CULL_MARGIN_M: f32 = 1.0;
const PICKUP_ITEM_CULL_RADIUS_M: f32 = 2.0;
const PICKUP_ITEM_SHADOW_MARGIN_M: f32 = 5.0;

pub const BEAM_SECONDS_BEFORE_INACTIVE: f32 = 5.0;
fn loops_forever(sys: &ParticleSystem) -> bool { sys.emitters.iter().any(|e| e.lods.iter().any(|l| l.enabled && l.required.emitter_loops == 0)) }

fn xf(pos: Vec3, yaw: f32) -> Transform { Transform::from_game([pos.x, pos.y, pos.z], yaw) }

/// The DXBC materials (loaded on first use from the game install).
fn ensure_dxbc(st: &mut State, renderer: &mut Renderer) {
    if st.dxbc.is_some() || st.dxbc_tried { return; }
    st.dxbc_tried = true;
    match crate::pickups_hook::install() {
        Some(dir) => match crate::pickups_dxbc::Dxbc::load(&dir, renderer.fx()) { Ok(d) => st.dxbc = Some(d), Err(e) => { eprintln!("[pickups-fx] DXBC materials unavailable: {e}"); st.log.push(format!("DXBC materials unavailable: {e}")); } },
        None => eprintln!("[pickups-fx] no game install: the pickup effect materials cannot be loaded from the paks"),
    }
}

/// STATIC ITEM (2026-10-05): called by `pickups_hook::frame` (before [`frame`]) for a `PickupStaticMesh` item: `true` = this frame draws it with its own cooked
/// materials ([`frame`] -> `Dxbc::draw_static_item`), so its renderer `Prop` model must not be drawn. `enabled` = the original-material path is on (the same
/// switch as the held gun / dropped weapons, `--standin-gun-material` turns it off).
pub fn static_item_ready(renderer: &mut Renderer, mesh_path: &str, enabled: bool) -> bool {
    let Ok(mut guard) = STATE.lock() else { return false };
    let Some(st) = guard.as_mut() else { return false };
    st.static_cooked = enabled;
    if !enabled { return false; }
    ensure_dxbc(st, renderer);
    st.dxbc.as_mut().map_or(false, |dx| dx.static_ready(renderer.fx(), mesh_path).is_ok())
}

/// Called from `particles::submit` every drawn frame: ticks the effects of the live pickups by the WORLD time elapsed since the last call (a re-submit of the same
/// frame, e.g. a screenshot pass, and a paused world advance nothing) and returns their draw data. `_frame_dt` (the frame clock) is not used (see [`set_world_clock`]).
pub fn frame(renderer: &mut Renderer, camera: &CameraParams, _frame_dt: f32) -> Option<ParticleFrame> {
    let mut guard = STATE.lock().ok()?;
    let st = guard.as_mut()?;
    let (world, real, delta_world) = world_clock();
    let dt = (world - st.clock).max(0.0);
    let snaps = crate::pickups_hook::snapshot();
    let eye = [camera.eye.x, camera.eye.y, camera.eye.z];
    let eye_ue = fn_particles::sim::game_to_ue(eye);
    let ground = |_x: f32, _y: f32| 0.0f32;
    let env = Env { ground: Some(&ground) };
    // the ORIGINAL cooked programs of the materials (pickups_dxbc.rs); loaded on the first frame
    ensure_dxbc(st, renderer);
    let alive: std::collections::HashSet<u64> = snaps.iter().map(|s| s.id).collect();
    // gone: the effect actor is destroyed with the pickup; a live trail finishes on its own
    let gone: Vec<u64> = st.actors.keys().filter(|id| !alive.contains(id)).copied().collect();
    for id in gone { if let Some(a) = st.actors.remove(&id) { if let Some(t) = a.trail { st.orphan_trails.push(t); } } }
    for s in &snaps {
        let Some(radius) = s.radius_cm.or(Some(30.)) else { continue };
        if !st.actors.contains_key(&s.id) {
            st.serial += 1;
            let seed = (s.id.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ st.serial;
            let p = xf(s.pos, s.yaw);
            let dist = (p.pos - eye_ue).length();
            let mut beam = Instance::new_with_params(st.beam.clone(), p, seed, dist, &beam_params(s.rarity, radius));
            beam.set_emitter_enabled("GodRays", s.rarity >= 4);
            let resource = s.resource;
            st.log.push(format!("pickup #{} rarity {}: P_Pickup started (radius {radius:.1} cm, sphere {:.2}, sparkle x{}, godrays {})", s.id, s.rarity, (radius / 40.).clamp(0.9, 1.65), SPARKLE_GROUND[(s.rarity as usize).min(9)], s.rarity >= 4));
            let holo = match (&s.mesh, st.dxbc.as_mut()) {
                (Some((path, skel)), Some(dx)) => match dx.holo_mesh(renderer.fx(), path, *skel) {
                    Ok(()) => { let normals = dx.holo_normals(path); let random = ((s.id.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as f32 / (1u64 << 24) as f32) * crate::pickups_dxbc::RANDOM_RANGE; Some((path.clone(), crate::pickups_dxbc::HoloActor { rarity: s.rarity, random, normals, picked_at: None })) }
                    Err(e) => { st.log.push(format!("pickup #{} hologram NOT DRAWN: {e}", s.id)); None }
                },
                _ => None,
            };
            st.actors.insert(s.id, Actor { beam, trail: None, picked: false, accum: 0., last_seen: 0., radius_cm: radius, rarity: s.rarity, resource, holo });
        }
        let (loot, ribbon) = (st.loot.clone(), st.ribbon.clone());
        let a = st.actors.get_mut(&s.id).unwrap();
        let p = xf(s.pos, s.yaw);
        a.beam.xf = p;
        if s.flying && !a.picked {
            a.picked = true;
            if let Some((_, h)) = a.holo.as_mut() { h.picked_at = Some(st.clock); }
            a.beam.set_emitter_enabled("Sparkle", false);
            a.beam.set_float_parameter("AlphaLight", 0.);
            if let Some(r) = &ribbon {
                let mut t = Instance::new_with_params(r.clone(), p, s.id ^ 0x7241_11B0, 0., &trail_params(a.rarity, a.radius_cm));
                t.xf = p;
                a.trail = Some(t);
            }
            if a.resource { if let Some(l) = &loot { let i = Instance::new(l.clone(), Transform::from_game([s.pos.x, s.pos.y, s.pos.z], 0.), s.id ^ 0x100F, 0.); st.bursts.push(Burst { inst: i }); } }
        }
        if let Some(t) = a.trail.as_mut() { t.xf = p; }
        if let Some(t) = s.picked_t.filter(|_| a.picked) { let k = 1. - fn_game::pickups::eval_keys(&SHRINK_SPHERE, t.max(0.)); a.beam.set_vector_parameter("SizeSphere", [k; 3]); }
    }
    // tick + draw
    st.clock = world;
    // SORTFIX: each instance = one particle system component: its draws carry the component's sort data (dynamic / fixed bounds origin, TranslucentRendering.cpp:864)
    let cat0 = st.catalog.clone();
    let mut groups: Vec<(Vec<fn_particles::EmitterDraw>, Transform, fn_render::primitive_ub::BoxSphereBounds)> = vec![];
    let mut collect = |inst: &Instance| -> bool { let b = crate::particles::instance_bounds(inst, &cat0); if !crate::particles::camera_sees_ue(&b, PICKUP_FX_CULL_MARGIN_M, camera) { return false; } let mut d = vec![]; inst.draw(eye, &mut d); if !d.is_empty() { groups.push((d, inst.xf, b)); } true };
    let inactive = if loops_forever(&st.beam) { BEAM_SECONDS_BEFORE_INACTIVE.max(0.1) } else { 0. };
    for a in st.actors.values_mut() {
        if !(inactive > 0. && a.accum > inactive && world > a.last_seen + inactive) { a.beam.tick(dt, &env); a.accum += dt; }
        if collect(&a.beam) { a.last_seen = world; }
        if let Some(t) = a.trail.as_mut() { t.tick(dt, &env); collect(t); }
    }
    for t in st.orphan_trails.iter_mut() { t.tick(dt, &env); collect(t); }
    st.orphan_trails.retain(|t| !t.finished());
    for b in st.bursts.iter_mut() { b.inst.tick(dt, &env); collect(&b.inst); }
    st.bursts.retain(|b| !b.inst.finished());
    let view = fn_fx::view::ViewParams { eye: camera.eye, forward: camera.forward, up: camera.up, tan_half: camera.tan_half(), near_m: camera.near, far_m: camera.far, width: camera.width, height: camera.height, time: st.clock, real_time: real, delta_world };
    if let Some(dx) = st.dxbc.as_mut() { dx.begin(&view); }
    // STATIC ITEM: the `PickupStaticMesh` items with their own cooked materials (pickups_hook::frame hid their Prop models via `static_item_ready`)
    if let (true, Some(dx)) = (st.static_cooked, st.dxbc.as_mut()) {
        for s in &snaps {
            let (Some((path, false)), Some(origin)) = (s.mesh.as_ref(), s.origin_cm) else { continue };
            let t = xf(s.pos, s.yaw);
            if !crate::particles::camera_sees(s.pos, PICKUP_ITEM_CULL_RADIUS_M + PICKUP_ITEM_SHADOW_MARGIN_M, camera) { dx.static_item_df(path, t.pos, t.rot, origin); continue; }
            if let Err(e) = dx.draw_static_item_with(renderer.fx(), path, t.pos, t.rot, origin, s.black_hole) { if st.warned.insert(format!("static {path}")) { let m = format!("static item {path}: NOT DRAWN with its cooked materials: {e}"); eprintln!("[pickups-fx] {m}"); st.log.push(m); } }
        }
    }
    // the holograms (B_Pickups hologram component)
    let st_clock = st.clock;
    if let Some(dx) = st.dxbc.as_mut() .filter(|_| std::env::var_os("FNRT_PICKUP_NO_HOLOGRAM").is_none() && std::env::var("FNRT_PICKUP_NO_HOLOGRAM_AFTER").ok().and_then(|v| v.parse::<f32>().ok()).map_or(true, |t| st_clock <= t)) {
        for s in &snaps {
            let (Some(a), Some(origin)) = (st.actors.get(&s.id), s.origin_cm) else { continue };
            let Some((path, h)) = a.holo.as_ref() else { continue };
            if !crate::particles::camera_sees(s.pos, PICKUP_ITEM_CULL_RADIUS_M + PICKUP_FX_CULL_MARGIN_M, camera) { continue; }
            let t = xf(s.pos, s.yaw);
            if let Err(e) = dx.draw_hologram(renderer.fx(), path, h, t.pos, t.rot, origin, eye_ue) { if st.warned.insert(format!("holo {path}")) { let m = format!("hologram of {path}: NOT DRAWN: {e}"); eprintln!("[pickups-fx] {m}"); st.log.push(m); } }
        }
    }
    let mut frame = ParticleFrame::default();
    let cat = st.catalog.clone();
    let mut any_legacy = false;
    for (draws, xf, bounds) in &groups {
        let sort = crate::particles::camera_sort(bounds.origin, camera);
        for d in draws {
            if std::env::var_os("FNRT_PICKUP_DEBUG").is_some() && dt > 0. && (st.clock * 60.) as u32 % 30 == 0 { if let Some(p) = d.particles.first() { eprintln!("[pickups-fx] debug t={:.2} {}/{} {:?} n={} pos {:?} size {:?} color {:?} dyn {:?} rel {:.2} mat {}", st.clock, d.system, d.emitter, d.kind, d.particles.len(), p.pos, p.size, p.color, p.dynamic, p.rel_time, d.material.rsplit('/').next().unwrap_or("")); } }
            let routed = match st.dxbc.as_mut() { Some(dx) => dx.draw_emitter(renderer.fx(), d, xf, bounds, dt), None => Err("DXBC materials unavailable".to_owned()) };
            if let Err(e) = routed {
                let key = format!("{}/{}", d.system, d.emitter);
                if st.lookalike_fallback {
                    if st.warned.insert(key.clone()) { let m = format!("{key}: not ported to the original shader programs ({e}): drawn with the LOOKALIKE material (FNRT_PICKUP_LOOKALIKE_FALLBACK)"); eprintln!("[pickups-fx] {m}"); st.log.push(m); }
                    crate::particles::batches(renderer, &cat, &mut st.gpu, d, sort, &mut frame); any_legacy = true;
                } else if st.warned.insert(key.clone()) { let m = format!("{key}: NOT DRAWN, not ported to the original shader programs: {e}"); eprintln!("[pickups-fx] {m}"); st.log.push(m); }
            }
        }
    }
    if let Some(dx) = st.dxbc.as_mut() { let f = dx.take_frame(); if !f.draws.is_empty() { renderer.append_fx_frame(f); } else { renderer.append_fx_frame(FxFrame::default()); } }
    if groups.is_empty() || (!any_legacy && frame.sprites.is_empty() && frame.meshes.is_empty()) { return None; }
    Some(frame)
}

/// Epilogue of a scripted run.
pub fn report() -> String { STATE.lock().ok().and_then(|g| g.as_ref().map(|s| if s.log.is_empty() { String::new() } else { format!("pickup fx:\n  {}\n", s.log.join("\n  ")) })).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_blueprint_arrays_are_the_cdo_values() {
        assert_eq!(OUTLINE_RARITY_COLORS[4][0], 0.555); assert_eq!(OUTLINE_RARITY_COLORS[2][2], 0.687_03); assert_eq!(SPARKLE_GROUND[9], 5.5); assert_eq!(SPARKLE_PICKED[1], 2.5); assert_eq!(LIFETIME_PICKED[0], 0.14);
    }
    /// The real P_Pickup (needs the exported `particles-pickup`): what each emitter draws for three rarities.
    #[test] fn p_pickup_particle_colours_follow_the_rarity() {
        let dir = fn_live::var_os("FN_PICKUP_FX_DIR").map(PathBuf::from).into_iter().chain([PathBuf::from("C:/Users/Brady/Desktop/Fortnite Rust/FortniteRuntime/assets/particles-pickup")]).find(|d| d.join("index.json").is_file());
        let Some(dir) = dir else { eprintln!("no particles-pickup: skipped"); return };
        let cat = Catalog::load(&dir).unwrap(); let sys = cat.system(P_PICKUP).unwrap();
        for rarity in [0u8, 2, 4] {
            let mut i = Instance::new_with_params(sys.clone(), Transform::identity(), 5, 100., &beam_params(rarity, 40.));
            i.set_emitter_enabled("GodRays", rarity >= 4);
            let ground = |_: f32, _: f32| 0.0f32; let env = Env { ground: Some(&ground) };
            for _ in 0..60 { i.tick(1. / 60., &env); }
            let mut d = vec![]; i.draw([0., 0., 3.], &mut d);
            for e in &d { if let Some(p) = e.particles.first() { eprintln!("rarity {rarity} {}/{} n={} color {:?} size {:?} mat {}", e.system, e.emitter, e.particles.len(), p.color, p.size, e.material.rsplit('/').next().unwrap_or("")); } }
        }
    }
    #[test] fn beam_params_follow_set_rarity_color_and_the_radius_clamp() {
        let p = beam_params(4, 93.8);
        assert_eq!(p.get_float("SparkleSphereRadius"), Some(93.8 * 0.75));
        assert_eq!(p.get_any_vector("BGSphereRadius"), Some([1.65; 3]), "93.8 / 40 clamps to 1.65");
        assert_eq!(beam_params(0, 10.).get_any_vector("BGSphereRadius"), Some([0.9; 3]), "small meshes clamp to 0.9");
        assert_eq!(beam_params(3, 40.).get_float("SparkleRateScale"), Some(2.5));
    }
}
