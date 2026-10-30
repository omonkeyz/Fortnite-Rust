use crate::pickups_dxbc::Dxbc;
use fn_audio_sink::SoundEvent;
use fn_fx::view::ViewParams;
use fn_game::containers::Rng;
use fn_game::supply_drop::{self as sd, Data, DropEvent, Fall, Schedule};
use fn_game::GameSim;
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Transform};
use fn_render::fx::{FxFrame, FxRenderer};
use glam::{Mat3, Mat4, Vec3};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub const CRATE_MESH: &str = "/Game/Gadgets/Assets/SupplyDrop/Mesh/Prop_Wood_Crate_SupplyDrop.Prop_Wood_Crate_SupplyDrop";
pub const BALLOON_MESH: &str = "/Game/Athena/SupplyDrops/S_Balloon_SupplyDrop_Athena.S_Balloon_SupplyDrop_Athena";
pub const P_FLARE: &str = "/Game/Athena/SupplyDrops/P_SupplyDropFlare_Athena";
pub const P_RETICLE: &str = "/Game/Gadgets/Assets/SupplyDrop/Effects/P_SupplyDrop_Active";
pub const P_TRAIL: &str = "/Game/Gadgets/Assets/SupplyDrop/Effects/P_SupplyDrop_Trail";
pub const P_IMPACT: &str = "/Game/Gadgets/Assets/SupplyDrop/Effects/P_SupplyDrop_Impact";
pub const P_CRATE_EXPLOSION: &str = "/Game/Gadgets/Assets/SupplyDrop/Effects/P_SupplydropCrate_Explosion";
pub const P_BALLOON_EXPLOSION: &str = "/Game/Gadgets/Assets/BottleRocket/P_BottleRocket_Explo";
pub const SYSTEMS: [&str; 6] = [P_FLARE, P_RETICLE, P_TRAIL, P_IMPACT, P_CRATE_EXPLOSION, P_BALLOON_EXPLOSION];
pub const CUE_FLARE: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_Flare_Loop_Athena_Cue";
pub const CUE_RETICLE: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_Reticle_Loop_Athena_Cue";
pub const CUE_FALLING: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_Falling_Loop_Athena_Cue";
pub const CUE_APPEAR: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_Appear_Athena_Cue";
pub const CUE_RETICLE_APPEAR: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_Reticle_Appear_Athena_Cue";
pub const CUE_LAST_SECOND: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_LastSecond_Athena_Cue";
pub const CUE_LAND: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_Land_Athena_Cue";
pub const CUE_OPEN: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_Open_Athena_Cue";
pub const CUE_BALLOON_EXPLODE: &str = "/Game/Athena/Sounds/SupplyDrops/Fort_SupplyDrop_Balloon_Explode_Cue";
pub const LOOP_KEY_BASE: u32 = 0x5344_0000;
pub const BALLOON_BOOM_CM: [f32; 3] = [0., 0.000_366_210_94, 616.363_6];
pub const BALLOON_CAPSULE_CENTER_CM: [f32; 3] = [0., -0.019_241_333, 622.2183];
pub const BALLOON_CAPSULE_RADIUS_CM: f32 = 249.6861;
pub const BALLOON_CAPSULE_LENGTH_CM: f32 = 128.;
pub const BALLOON_HEALTH_BAR_CM: [f32; 3] = [0., 0., 640.];
pub const FALLING_PERCENT: &str = "FallingPercent";
pub const CRATE_HALF_EXTENT_CM: [f32; 3] = [60., 60., 60.];
pub const CRATE_BOX_CENTER_CM: [f32; 3] = [-4.8395, -4.0565, 59.8235];

pub fn enabled() -> bool { static V: OnceLock<bool> = OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_SUPPLY_DROPS").map_or(true, |v| v != "0")) }

static DATA: OnceLock<Data> = OnceLock::new();
fn data() -> Option<&'static Data> { DATA.get() }

struct Crate {
    impact: Vec3,
    yaw: f32,
    fall: Fall,
    fall_height: f32,
    balloon_hp: f32,
    balloon: bool,
    bar: bool,
    flare: Option<Instance>,
    reticle: Option<Instance>,
    trail: Option<Instance>,
    interact: bool,
    key: u32,
    loops: [bool; 3],
}

struct State {
    install: Option<PathBuf>,
    catalog: Option<Arc<Catalog>>,
    dxbc: Option<Result<Dxbc, String>>,
    schedule: Schedule,
    armed: bool,
    crates: Vec<Crate>,
    bursts: Vec<Instance>,
    clock: f32,
    serial: u32,
    errors: HashSet<String>,
    log: Vec<String>,
    focus: Option<usize>,
    use_was_down: bool,
    shots: Vec<(Vec3, Vec3, Option<f32>, f32)>,
    aid: Vec<(f32, f32, f32)>,
    aid_read: bool,
    spawned: usize,
    first_spawn: Option<f32>,
    first_land: Option<f32>,
    opened: usize,
    popped: usize,
    tracked: Vec<(u64, Vec3, f32, fn_hitreg::StatRow)>,
    damaged: usize,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); g.as_mut().map(f) }

fn to_ue(g: Vec3) -> Vec3 { Vec3::new(g.x, g.z, g.y) * 100. }
fn to_game(u: Vec3) -> Vec3 { Vec3::new(u.x, u.z, u.y) * 0.01 }

pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    if !enabled() { eprintln!("[supply-drop] off (FN_SUPPLY_DROPS=0)"); return; }
    let dir = fn_live::var_os("FN_SUPPLY_DROP_FX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-supplydrop")).find(|d| d.join("index.json").is_file()));
    let catalog = match dir.as_ref().map(|d| Catalog::load(d)) {
        Some(Ok(c)) => { let missing: Vec<&str> = SYSTEMS.iter().copied().filter(|s| c.system(s).is_none()).collect(); if !missing.is_empty() { eprintln!("[supply-drop] particles-supplydrop lacks {missing:?}"); } Some(Arc::new(c)) }
        Some(Err(e)) => { eprintln!("[supply-drop] {e}: no supply drop particles"); None }
        None => { eprintln!("[supply-drop] no `particles-supplydrop` folder in the asset roots: no supply drop particles"); None }
    };
    std::thread::spawn(|| {
        let d = match fn_game::cosmetic_swap::paks().and_then(|s| Data::load(&s)) { Ok(d) => d, Err(e) => { eprintln!("[supply-drop] AthenaGameData rows unreadable ({e}): the cooked defaults"); Data::default() } };
        eprintln!("[supply-drop] every {:.0} +/- {:.0} s after the storm starts; fall {:.0} s from {:.0} cm; balloon {:.0} HP; counts per phase {:?}", d.interval, d.deviation, d.fall_time, d.fall_height, d.balloon_health, d.counts);
        let _ = DATA.set(d);
    });
    let seed = fn_live::var("FN_SUPPLY_DROP_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(5, |d| d.subsec_nanos() ^ 0x5344));
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(State { install, catalog, dxbc: None, schedule: Schedule::new(seed), armed: false, crates: vec![], bursts: vec![], clock: 0., serial: 0, errors: HashSet::new(), log: vec![], focus: None, use_was_down: false, shots: vec![], aid: vec![], aid_read: false, spawned: 0, first_spawn: None, first_land: None, opened: 0, popped: 0, tracked: vec![], damaged: 0 });
}

pub fn on_shot(eye: Vec3, forward: Vec3, blocked_m: Option<f32>, damage: f32) { with(|st| if !st.crates.is_empty() { st.shots.push((eye, forward, blocked_m, damage)); }); }

fn damage_balloon(st: &mut State, i: usize, dmg: f32) {
    let c = &mut st.crates[i];
    if !c.balloon || dmg <= 0. { return; }
    c.balloon_hp -= dmg;
    c.bar = true;
    st.damaged += 1;
    let hp = c.balloon_hp;
    st.log.push(format!("[supply-drop] crate #{i}: balloon took {dmg:.1} damage, {hp:.0} HP left"));
}

pub fn segment_hit(from: Vec3, to: Vec3) -> Option<Vec3> {
    with(|st| {
        let mut best: Option<f32> = None;
        for c in st.crates.iter().filter(|c| c.balloon) { let (a, b, r) = balloon_capsule(c); if let Some(t) = segment_capsule(from, to, a, b, r) { if best.map_or(true, |x| t < x) { best = Some(t); } } }
        best.map(|t| from + (to - from).normalize_or_zero() * t)
    }).flatten()
}

pub fn on_blast(gun: &fn_hitreg::GunHit, pos: Vec3) {
    let Some(def) = fn_hitreg::explosive::def_of(gun) else { return };
    let Some(sel) = gun.ability.selection(&def.explode_tag) else { return };
    if sel.effect_classes.is_empty() { return; }
    let radius = sel.range * 0.01;
    with(|st| {
        for i in 0..st.crates.len() {
            if !st.crates[i].balloon { continue; }
            let (a, b, r) = balloon_capsule(&st.crates[i]);
            let d = ((closest_on_segment(a, b, pos) - pos).length() - r).max(0.);
            if d > radius { continue; }
            let dmg = gun.stats.damage_at(fn_hitreg::DamageKind::Combat, d * 100., 1);
            st.log.push(format!("[supply-drop] crate #{i}: blast {:.2} m from the balloon -> {dmg:.1} damage", d));
            damage_balloon(st, i, dmg);
        }
    });
}

pub fn projectile_spawn(key: u64, pos: Vec3, gun: &fn_hitreg::GunHit) { with(|st| { st.tracked.retain(|t| t.0 != key); if st.crates.iter().any(|c| c.balloon) { st.tracked.push((key, pos, 0., gun.stats.clone())); } }); }

pub fn projectile_end(key: u64, at: Option<Vec3>) {
    match at { Some(p) => projectile_move(key, p, true), None => { with(|st| st.tracked.retain(|t| t.0 != key)); } }
}

pub fn projectile_move(key: u64, pos: Vec3, end: bool) {
    with(|st| {
        let Some(k) = st.tracked.iter().position(|t| t.0 == key) else { return };
        let (_, last, flown, _) = st.tracked[k].clone();
        let mut hit: Option<(f32, usize)> = None;
        for (i, c) in st.crates.iter().enumerate() {
            if !c.balloon { continue; }
            let (a, b, r) = balloon_capsule(c);
            if let Some(t) = segment_capsule(last, pos, a, b, r) { if hit.map_or(true, |h| t < h.0) { hit = Some((t, i)); } }
        }
        match hit {
            Some((t, i)) => {
                let dmg = st.tracked[k].3.damage_at(fn_hitreg::DamageKind::Combat, (flown + t) * 100., 1);
                st.tracked.remove(k);
                st.log.push(format!("[supply-drop] crate #{i}: projectile hit the balloon after {:.1} m -> {dmg:.1} damage", flown + t));
                damage_balloon(st, i, dmg);
            }
            None if end => { st.tracked.remove(k); }
            None => { st.tracked[k].1 = pos; st.tracked[k].2 = flown + (pos - last).length(); }
        }
    });
}

fn crate_pos(c: &Crate) -> Vec3 { c.impact + Vec3::new(0., c.fall.height(c.fall_height) * 0.01, 0.) }
fn yaw_m(deg: f32) -> Mat3 { Mat3::from_rotation_z(deg.to_radians()) }
fn balloon_center(c: &Crate) -> Vec3 { let p = to_ue(crate_pos(c)); to_game(p + yaw_m(c.yaw) * (Vec3::from(BALLOON_BOOM_CM) * Vec3::from(sd::BALLOON_SCALE))) }
fn balloon_capsule(c: &Crate) -> (Vec3, Vec3, f32) {
    let k = Vec3::from(sd::BALLOON_SCALE);
    let r = BALLOON_CAPSULE_RADIUS_CM * k.x.abs().min(k.y.abs());
    let half = ((BALLOON_CAPSULE_LENGTH_CM + 2. * BALLOON_CAPSULE_RADIUS_CM) * k.z.abs() - 2. * r).max(0.) * 0.5;
    let p = to_ue(crate_pos(c));
    let ctr = p + yaw_m(c.yaw) * (Vec3::from(BALLOON_CAPSULE_CENTER_CM) * k);
    (to_game(ctr - Vec3::Z * half), to_game(ctr + Vec3::Z * half), r * 0.01)
}
fn health_bar_anchor(c: &Crate) -> Vec3 { to_game(to_ue(crate_pos(c)) + yaw_m(c.yaw) * (Vec3::from(BALLOON_HEALTH_BAR_CM) * Vec3::from(sd::BALLOON_SCALE))) }
pub fn falling_percent(t: f32) -> f32 { let t = t.clamp(0., 1.); t * t * (3. - 2. * t) }

fn instance(st: &mut State, sys: &str, pos_ue: Vec3, rot: Mat3, eye_ue: Vec3) -> Option<Instance> {
    let s = st.catalog.as_ref()?.system(sys)?;
    st.serial = st.serial.wrapping_add(1);
    Some(Instance::new(s, Transform { pos: pos_ue, rot, scale: 1. }, 0x5344_0000 ^ st.serial as u64, (pos_ue - eye_ue).length()))
}

fn cue(path: &str) -> Option<&'static str> { fn_audio_sink::supply_drop_cue(path) }

fn spawn(st: &mut State, impact: Vec3, yaw: f32, d: &Data, eye_ue: Vec3, sounds: &mut Vec<SoundEvent>) {
    let fall_time = fn_live::f32("FN_SUPPLY_DROP_FALL_TIME", d.fall_time);
    let key = LOOP_KEY_BASE + 4 * (st.spawned as u32 % 0x4000);
    let ground_ue = to_ue(impact);
    let flare = instance(st, P_FLARE, ground_ue, Mat3::IDENTITY, eye_ue);
    let reticle = instance(st, P_RETICLE, ground_ue, Mat3::IDENTITY, eye_ue);
    let top = ground_ue + Vec3::new(0., 0., d.fall_height);
    let trail = instance(st, P_TRAIL, top, yaw_m(yaw), eye_ue);
    let mut c = Crate { impact, yaw, fall: Fall::new(fall_time), fall_height: d.fall_height, balloon_hp: d.balloon_health, balloon: true, bar: false, flare, reticle, trail, interact: false, key, loops: [false; 3] };
    let mesh = crate_pos(&c);
    if let Some(q) = cue(CUE_FLARE) { sounds.push(SoundEvent::ContainerLoopFadeIn { cue: q, key, pos: Some(impact), fade: sd::DROP_AUDIO_FADE_IN }); c.loops[0] = true; }
    if let Some(q) = cue(CUE_RETICLE) { sounds.push(SoundEvent::ContainerLoopFadeIn { cue: q, key: key + 1, pos: Some(impact), fade: sd::DROP_AUDIO_FADE_IN }); c.loops[1] = true; }
    if let Some(q) = cue(CUE_FALLING) { sounds.push(SoundEvent::ParamLoopFadeIn { cue: q, key: key + 2, pos: Some(mesh), fade: sd::FALLING_AUDIO_FADE_IN, param: FALLING_PERCENT, value: falling_percent(c.fall.t) }); c.loops[2] = true; }
    if let Some(q) = cue(CUE_APPEAR) { sounds.push(SoundEvent::BuildingCue { cue: q, pos: Some(mesh) }); }
    if let Some(q) = cue(CUE_RETICLE_APPEAR) { sounds.push(SoundEvent::BuildingCue { cue: q, pos: Some(mesh) }); }
    st.log.push(format!("[supply-drop] crate #{} spawned: lands at ({:.1}, {:.1}, {:.1}) m, yaw {yaw:.0}, falls {:.0} m in {fall_time:.0} s", st.spawned, impact.x, impact.y, impact.z, d.fall_height * 0.01));
    st.spawned += 1;
    st.crates.push(c);
}

fn ground(sim: &GameSim, x: f32, z: f32) -> f32 { sim.floor_below(Vec3::new(x, sd::GROUND_TRACE_CM * 0.01, z)) }

fn closest_on_segment(a: Vec3, b: Vec3, p: Vec3) -> Vec3 { let ab = b - a; a + ab * ((p - a).dot(ab) / ab.length_squared().max(1e-9)).clamp(0., 1.) }
fn ray_capsule(o: Vec3, d: Vec3, a: Vec3, b: Vec3, r: f32) -> Option<f32> {
    let ba = b - a;
    let oa = o - a;
    let baba = ba.dot(ba);
    let bard = ba.dot(d);
    let baoa = ba.dot(oa);
    let rdoa = d.dot(oa);
    let oaoa = oa.dot(oa);
    let qa = baba - bard * bard;
    let qb = baba * rdoa - baoa * bard;
    let qc = baba * oaoa - baoa * baoa - r * r * baba;
    if (closest_on_segment(a, b, o) - o).length() <= r { return Some(0.); }
    let mut best: Option<f32> = None;
    if qa.abs() > 1e-9 {
        let h = qb * qb - qa * qc;
        if h >= 0. {
            let t = (-qb - h.sqrt()) / qa;
            let y = baoa + t * bard;
            if t >= 0. && y > 0. && y < baba { best = Some(t); }
        }
    }
    for c in [a, b] {
        if let Some(t) = ray_sphere(o, d, c, r) { if best.map_or(true, |x| t < x) { best = Some(t); } }
    }
    best
}
fn segment_capsule(p0: Vec3, p1: Vec3, a: Vec3, b: Vec3, r: f32) -> Option<f32> {
    let seg = p1 - p0;
    let len = seg.length();
    if len < 1e-6 { return ((closest_on_segment(a, b, p0) - p0).length() <= r).then_some(0.); }
    ray_capsule(p0, seg / len, a, b, r).filter(|t| *t <= len)
}
fn ray_sphere(o: Vec3, d: Vec3, c: Vec3, r: f32) -> Option<f32> {
    let m = o - c;
    let b = m.dot(d);
    let q = m.length_squared() - r * r;
    if q > 0. && b > 0. { return None; }
    let disc = b * b - q;
    if disc < 0. { return None; }
    Some((-b - disc.sqrt()).max(0.))
}

#[allow(clippy::too_many_arguments)]
pub fn frame(sim: &mut GameSim, zones: Option<(f32, usize, Vec<[f32; 3]>)>, use_down: bool, blocked: bool, dt: f32) -> Vec<SoundEvent> {
    let mut sounds = vec![];
    if !enabled() { return sounds; }
    let Some(d) = data() else { return sounds };
    let mut loot: Vec<(String, u32, Vec3, Vec3)> = vec![];
    let frames = sim.frames;
    let eye = sim.cam_view.eye;
    let fwd = sim.cam_view.forward.normalize_or_zero();
    let feet = sim.feet();
    let pawn = Vec3::new(feet.x, feet.y + fn_game::pickups::PAWN_HALF_HEIGHT, feet.z);
    let gravity = { let p = fn_movement::cmc::Params::default().with_tuning(&sim.tuning); (p.world_gravity_z * p.gravity_scale).abs() * 0.01 };
    with(|st| {
        st.clock += dt;
        let now = st.clock;
        let eye_ue = to_ue(eye);
        if !st.aid_read {
            st.aid_read = true;
            if let Ok(v) = std::env::var("FNRT_SUPPLY_DROP").or_else(|_| fn_live::var("FNRT_SUPPLY_DROP")) { for p in v.split(';').filter(|p| !p.trim().is_empty()) { let mut it = p.split(':'); let f = it.next().unwrap_or("0"); let dist = it.next().unwrap_or("12"); let side = it.next().unwrap_or("0"); st.aid.push((f.trim().parse().unwrap_or(0.), dist.trim().parse().unwrap_or(12.), side.trim().parse().unwrap_or(0.))); } }
        }
        let due: Vec<(f32, f32)> = st.aid.iter().filter(|a| now >= a.0).map(|a| (a.1, a.2)).collect();
        st.aid.retain(|a| now < a.0);
        for (dist, side) in due {
            let f = Vec3::new(fwd.x, 0., fwd.z).try_normalize().unwrap_or(Vec3::X);
            let p = pawn + f * dist + Vec3::new(-f.z, 0., f.x) * side;
            let impact = Vec3::new(p.x, ground(sim, p.x, p.z), p.z);
            spawn(st, impact, 0., d, eye_ue, &mut sounds);
        }
        match &zones {
            Some((zclock, phase, locs)) => {
                if !st.armed { st.schedule.arm(*zclock, d); st.armed = true; st.log.push(format!("[supply-drop] storm started: first TrySupplyDrop in {:.0} s", st.schedule.next.map_or(0., |t| t - zclock))); }
                let off = crate::storm_hook::offset_cm();
                for s in st.schedule.tick(*zclock, d, *phase, locs, &sd::island) {
                    let (x, z) = ((s.at[0] + off[0]) * 0.01, (s.at[1] + off[1]) * 0.01);
                    let impact = Vec3::new(x, ground(sim, x, z), z);
                    spawn(st, impact, s.yaw, d, eye_ue, &mut sounds);
                }
            }
            None => {
                if st.armed {
                    st.armed = false; st.schedule.next = None;
                    for c in st.crates.drain(..) { for k in 0..3 { if c.loops[k] { sounds.push(SoundEvent::ContainerLoopStop { key: c.key + k as u32, fade: 0. }); } } }
                    st.bursts.clear(); st.focus = None;
                }
            }
        }
        if st.first_spawn.is_none() && !st.crates.is_empty() { st.first_spawn = Some(now); }
        let (fs, fl) = (st.first_spawn, st.first_land);
        let aid_frame = |k: &str| fn_live::var(k).ok().and_then(|v| v.trim().parse::<f32>().ok()).is_some_and(|f| match k { "FNRT_SUPPLY_DROP_POP" => fs.is_some_and(|s| now >= s + f), _ => fl.is_some_and(|l| now >= l + f) });
        static POPPED_AID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if aid_frame("FNRT_SUPPLY_DROP_POP") && !st.crates.is_empty() && !POPPED_AID.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let b = balloon_center(&st.crates[0]);
            st.shots.push((eye, (b - eye).normalize_or_zero(), None, d.balloon_health));
        }
        static HIT_AID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if let Some((at, dmg)) = fn_live::var("FNRT_SUPPLY_DROP_HIT").ok().and_then(|v| { let (a, b) = v.split_once(':')?; Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?)) }) {
            if fs.is_some_and(|s| now >= s + at) && !st.crates.is_empty() && !HIT_AID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                let b = balloon_center(&st.crates[0]);
                st.shots.push((eye, (b - eye).normalize_or_zero(), None, dmg));
            }
        }
        for (o, dir, blocked_m, dmg) in std::mem::take(&mut st.shots) {
            let dir = dir.normalize_or_zero();
            let mut best: Option<(f32, usize)> = None;
            for (i, c) in st.crates.iter().enumerate() {
                if !c.balloon { continue; }
                let (a, b, r) = balloon_capsule(c);
                if let Some(t) = ray_capsule(o, dir, a, b, r) { if blocked_m.map_or(true, |b| t <= b) && best.map_or(true, |b| t < b.0) { best = Some((t, i)); } }
            }
            let Some((_, i)) = best else { continue };
            damage_balloon(st, i, dmg);
        }
        for i in 0..st.crates.len() {
            if st.crates[i].balloon && st.crates[i].balloon_hp <= 1. {
                let c = &mut st.crates[i];
                c.balloon = false;
                c.bar = false;
                let bc = balloon_center(c);
                let mp = crate_pos(c);
                let popped = c.fall.pop(now);
                if let Some(b) = instance(st, P_BALLOON_EXPLOSION, to_ue(bc), Mat3::IDENTITY, eye_ue) { st.bursts.push(b); }
                st.popped += 1;
                if popped {
                    if let Some(q) = cue(CUE_BALLOON_EXPLODE) { sounds.push(SoundEvent::BuildingCue { cue: q, pos: Some(mp) }); }
                    st.log.push(format!("[supply-drop] f{frames} crate #{i}: balloon destroyed, the crate drops at play rate -> {}", sd::POPPED_PLAY_RATE));
                }
            }
        }
        let mut landed: Vec<Vec3> = vec![];
        for i in 0..st.crates.len() {
            let evs = st.crates[i].fall.step(now, dt);
            let c = &mut st.crates[i];
            let mesh = crate_pos(c);
            if c.loops[2] { sounds.push(SoundEvent::LoopMove { key: c.key + 2, pos: mesh }); if c.fall.finished_at.is_none() { sounds.push(SoundEvent::LoopParam { key: c.key + 2, param: FALLING_PERCENT, value: falling_percent(c.fall.t) }); } }
            let mesh_ue = to_ue(mesh);
            if let Some(t) = c.trail.as_mut() { t.xf.pos = mesh_ue; }
            for e in evs {
                match e {
                    DropEvent::DisableFlare => { if let Some(f) = c.flare.as_mut() { f.deactivate(); } if c.loops[0] { sounds.push(SoundEvent::ContainerLoopStop { key: c.key, fade: sd::FLARE_FADE_OUT }); c.loops[0] = false; } }
                    DropEvent::AlmostLanded => { if !c.fall.popped() { if let Some(q) = cue(CUE_LAST_SECOND) { sounds.push(SoundEvent::BuildingCue { cue: q, pos: Some(mesh) }); } } }
                    DropEvent::Finished => {}
                    DropEvent::Landed => {
                        if let Some(r) = c.reticle.as_mut() { r.deactivate(); }
                        if c.loops[1] { sounds.push(SoundEvent::ContainerLoopStop { key: c.key + 1, fade: 0. }); c.loops[1] = false; }
                        if c.loops[2] { sounds.push(SoundEvent::ContainerLoopStop { key: c.key + 2, fade: sd::FALLING_AUDIO_FADE_IN }); c.loops[2] = false; }
                        if let Some(q) = cue(CUE_LAND) { sounds.push(SoundEvent::BuildingCue { cue: q, pos: Some(c.impact) }); }
                        if let Some(f) = c.flare.as_mut() { f.deactivate(); }
                        if let Some(t) = c.trail.as_mut() { t.deactivate(); }
                        c.interact = true;
                        landed.push(to_ue(c.impact));
                    }
                }
            }
        }
        if st.first_land.is_none() && !landed.is_empty() { st.first_land = Some(now); }
        for at in landed {
            if let Some(b) = instance(st, P_IMPACT, at, Mat3::from_rotation_z(90f32.to_radians()) * Mat3::from_rotation_y(-90f32.to_radians()), eye_ue) { st.bursts.push(b); }
            st.log.push(format!("[supply-drop] f{frames} crate landed at ({:.0}, {:.0}, {:.0}) cm: Search enabled", at.x, at.y, at.z));
        }
        let reach = fn_game::pickups::INTERACT_DISTANCE + (eye - pawn).length();
        let mut best: Option<(f32, usize)> = None;
        for (i, c) in st.crates.iter().enumerate() {
            if !c.interact { continue; }
            let ctr = to_game(to_ue(c.impact) + yaw_m(c.yaw) * Vec3::from(CRATE_BOX_CENTER_CM));
            let he = Vec3::new(CRATE_HALF_EXTENT_CM[0], CRATE_HALF_EXTENT_CM[2], CRATE_HALF_EXTENT_CM[1]) * 0.01 + Vec3::splat(fn_game::pickups::INTERACT_TRACE_EXTENT);
            let (lo, hi) = (ctr - he, ctr + he);
            let inv = Vec3::new(1. / fwd.x, 1. / fwd.y, 1. / fwd.z);
            let (t1, t2) = ((lo - eye) * inv, (hi - eye) * inv);
            let (tn, tf) = (t1.min(t2).max_element(), t1.max(t2).min_element());
            if tf < tn.max(0.) || tn > reach { continue; }
            let t = tn.max(0.);
            if (eye + fwd * t - pawn).length() > fn_game::containers::interact_limit_m() + 0.6 { continue; }
            if best.map_or(true, |b| t < b.0) { best = Some((t, i)); }
        }
        st.focus = if blocked { None } else { best.map(|b| b.1) };
        let down = use_down || crate::interact::script_use();
        static OPEN_AID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let aid_open = aid_frame("FNRT_SUPPLY_DROP_OPEN") && st.crates.iter().any(|c| c.interact) && !OPEN_AID.swap(true, std::sync::atomic::Ordering::Relaxed);
        if aid_open { st.focus = st.crates.iter().position(|c| c.interact); }
        let pressed = (down && !st.use_was_down) || aid_open;
        st.use_was_down = down;
        if pressed { if let Some(i) = st.focus.take() {
            crate::interact::note_tap();
            let c = st.crates.remove(i);
            if c.balloon { if let Some(b) = instance(st, P_BALLOON_EXPLOSION, to_ue(balloon_center(&c)), Mat3::IDENTITY, eye_ue) { st.bursts.push(b); } }
            for k in 0..3 { if c.loops[k] { sounds.push(SoundEvent::ContainerLoopStop { key: c.key + k as u32, fade: 0. }); } }
            let at = to_ue(c.impact);
            if let Some(q) = cue(CUE_OPEN) { sounds.push(SoundEvent::BuildingCue { cue: q, pos: Some(c.impact) }); }
            if let Some(b) = instance(st, P_CRATE_EXPLOSION, at, yaw_m(c.yaw), eye_ue) { st.bursts.push(b); }
            let set = crate::chest_hook::data_ready().and_then(|r| r.ok());
            let mut rng = Rng::new(0x5344_4c54 ^ (st.opened as u64 + 1) ^ (now.to_bits() as u64));
            let drops = set.map(|s| s.tables.roll(sd::LOOT_GROUP, &mut rng)).unwrap_or_default();
            for x in &drops {
                let o = sd::loot_offset(&mut rng);
                let start = to_game(at + yaw_m(c.yaw) * Vec3::from(o));
                let h = to_game(Vec3::new(o[0], o[1], 0.)).normalize_or_zero();
                loot.push((x.item.clone(), x.count.max(1) as u32, start, h + Vec3::Y * (gravity).sqrt()));
            }
            st.opened += 1;
            st.log.push(format!("[supply-drop] f{frames} crate searched: {} -> [{}]", sd::LOOT_GROUP, drops.iter().map(|x| format!("{} x{}", x.item.rsplit('.').next().unwrap_or(&x.item), x.count)).collect::<Vec<_>>().join(", ")));
        } }
        for b in st.bursts.iter_mut() { b.tick(dt, &Env::none()); }
        for c in st.crates.iter_mut() { for i in [c.flare.as_mut(), c.reticle.as_mut(), c.trail.as_mut()].into_iter().flatten() { i.tick(dt, &Env::none()); } }
        st.bursts.retain(|b| !b.finished());
        for l in st.log.drain(..) { eprintln!("{l}"); }
    });
    for (item, count, a, v) in loot { crate::pickups_hook::queue_toss(&item, count, a, v); }
    sounds
}

pub fn prompt(key: &str) -> Option<(String, String, Vec3)> {
    with(|st| { let c = &st.crates[st.focus?]; Some((key.to_owned(), fn_game::containers::search_text().to_owned(), to_game(to_ue(c.impact) + Vec3::new(0., 0., 120.)))) }).flatten()
}
pub fn health_bars() -> Vec<(Vec3, f32, f32)> {
    let max = data().map_or(0., |d| d.balloon_health);
    with(|st| st.crates.iter().filter(|c| c.balloon && c.bar).map(|c| (health_bar_anchor(c), c.balloon_hp.max(0.), max)).collect()).unwrap_or_default()
}
pub fn has_focus() -> bool { with(|st| st.focus.is_some()).unwrap_or(false) }

fn draw_inst(dx: &mut Dxbc, fx: &mut FxRenderer, cat: &Catalog, inst: &Instance, eye: [f32; 3], dt: f32, errors: &mut HashSet<String>) {
    let mut d = vec![];
    inst.draw(eye, &mut d);
    let bounds = crate::particles::instance_bounds(inst, cat);
    for e in &d { if let Err(err) = dx.draw_emitter(fx, e, &inst.xf, &bounds, dt) { let k = format!("{}/{}", e.system, e.emitter); if errors.insert(k.clone()) { eprintln!("[supply-drop] {k}: NOT DRAWN: {err}"); } } }
}

pub fn prebuild(fx: &mut FxRenderer) -> bool {
    static STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let step = STEP.load(std::sync::atomic::Ordering::Relaxed);
    if step >= 2 + SYSTEMS.len() { return true; }
    let t = std::time::Instant::now();
    let ok = with(|st| {
        if st.install.is_none() { return false; }
        if st.dxbc.is_none() {
            st.dxbc = Some(match st.install.as_ref() { Some(i) => Dxbc::load(i, fx), None => Err("no install".into()) });
            if let (Some(Ok(dx)), Some(cat)) = (st.dxbc.as_ref(), st.catalog.as_ref()) { dx.prefetch_packages(SYSTEMS.iter().filter_map(|s| cat.system(s).map(|x| x.package.clone())).collect()); }
            return true;
        }
        let State { dxbc, errors, catalog, .. } = st;
        let Some(Ok(dx)) = dxbc.as_mut() else { return false };
        let view = ViewParams { eye: Vec3::new(0., 2., 3.), forward: Vec3::new(0., -0.5, -0.85).normalize(), up: Vec3::Y, tan_half: (1., 0.6), near_m: 0.1, far_m: 1000., width: 64, height: 64, time: 0., real_time: 0., delta_world: 1. / 60. };
        dx.begin(&view);
        match step {
            0 | 1 => { let mesh = if step == 0 { CRATE_MESH } else { BALLOON_MESH }; if let Err(e) = dx.draw_static_item(fx, mesh, Vec3::ZERO, Mat3::IDENTITY, [0.; 3]) { if errors.insert(mesh.to_owned()) { eprintln!("[supply-drop] warm-up {mesh}: {e}"); } } }
            k => if let Some(cat) = catalog.clone() {
                if let Some(sys) = cat.system(SYSTEMS[(k - 2).min(SYSTEMS.len() - 1)]) {
                    let mut i = Instance::new(sys, Transform::identity(), 1, 100.);
                    for _ in 0..12 { i.tick(1. / 16., &Env::none()); draw_inst(dx, fx, &cat, &i, [0., 2., 3.], 1. / 60., errors); }
                }
            },
        }
        let _ = dx.take_frame();
        STEP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        true
    }).unwrap_or(false);
    if !ok { STEP.store(usize::MAX / 2, std::sync::atomic::Ordering::Relaxed); return true; }
    let ms = t.elapsed().as_secs_f32() * 1000.;
    if ms > 30. { eprintln!("[supply-drop] warm-up step {step}: {ms:.0} ms"); }
    false
}

pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
    with(|st| {
        if st.crates.is_empty() && st.bursts.is_empty() { return; }
        if st.dxbc.is_none() { st.dxbc = Some(match st.install.as_ref() { Some(i) => Dxbc::load(i, fx), None => Err("no install".into()) }); }
        let eye_ue = fn_fx::view::to_ue(view.eye).as_vec3();
        let eye = [eye_ue.x, eye_ue.y, eye_ue.z];
        let State { dxbc, errors, crates, bursts, catalog, .. } = st;
        let dx = match dxbc.as_mut().unwrap() { Ok(d) => d, Err(e) => { if errors.insert("dxbc".into()) { eprintln!("[supply-drop] crate meshes unavailable: {e}"); } return; } };
        dx.begin(view);
        for c in crates.iter() {
            let root = Mat4::from_translation(to_ue(crate_pos(c))) * Mat4::from_mat3(yaw_m(c.yaw));
            let mut parts = vec![(CRATE_MESH, root)];
            if c.balloon { parts.push((BALLOON_MESH, root * Mat4::from_scale(Vec3::from(sd::BALLOON_SCALE)))); }
            for (mesh, m) in parts { if let Err(e) = dx.draw_static_item(fx, mesh, m.w_axis.truncate(), Mat3::from_mat4(m), [0.; 3]) { if errors.insert(mesh.to_owned()) { eprintln!("[supply-drop] {mesh}: NOT DRAWN: {e}"); } } }
        }
        if let Some(cat) = catalog.clone() {
            for c in crates.iter() { for i in [c.flare.as_ref(), c.reticle.as_ref(), c.trail.as_ref()].into_iter().flatten() { draw_inst(dx, fx, &cat, i, eye, dt, errors); } }
            for b in bursts.iter() { draw_inst(dx, fx, &cat, b, eye, dt, errors); }
        }
        let fr = dx.take_frame();
        out.buffer_writes.extend(fr.buffer_writes);
        out.draws.extend(fr.draws);
        out.shadow_draws.extend(fr.shadow_draws);
        out.distance_field_objects.extend(fr.distance_field_objects);
    });
}

pub fn report() -> String { with(|st| if st.spawned == 0 { String::new() } else { format!("supply drops: {} spawned, {} balloon hits, {} balloons popped, {} searched, {} in the world\n", st.spawned, st.damaged, st.popped, st.opened, st.crates.len()) }).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_shot_through_the_balloon_sphere_hits_it() {
        assert!(ray_sphere(Vec3::ZERO, Vec3::X, Vec3::new(10., 0., 0.), 2.).is_some_and(|t| (t - 8.).abs() < 1e-4));
        assert!(ray_sphere(Vec3::ZERO, Vec3::X, Vec3::new(10., 3., 0.), 2.).is_none());
        assert!(ray_sphere(Vec3::ZERO, Vec3::X, Vec3::new(-10., 0., 0.), 2.).is_none());
    }
    #[test] fn the_balloon_capsule_is_the_cooked_sphyl_scaled() {
        let c = Crate { impact: Vec3::ZERO, yaw: 0., fall: Fall::new(60.), fall_height: 0., balloon_hp: 1250., balloon: true, bar: false, flare: None, reticle: None, trail: None, interact: false, key: 0, loops: [false; 3] };
        let (a, b, r) = balloon_capsule(&c);
        assert!((r - 2.0225).abs() < 1e-3, "{r}");
        assert!(((a.y + b.y) * 0.5 - 5.5999).abs() < 1e-3, "{a:?} {b:?}");
        assert!(((b.y - a.y) - 1.6015).abs() < 2e-3, "{a:?} {b:?}");
        assert!(ray_capsule(Vec3::new(-10., 5.6, 0.), Vec3::X, a, b, r).is_some_and(|t| (t - (10. - r)).abs() < 1e-3));
        assert!(ray_capsule(Vec3::new(-10., 8.5, 0.), Vec3::X, a, b, r).is_none());
        assert!(segment_capsule(Vec3::new(-10., 5.6, 0.), Vec3::new(-5., 5.6, 0.), a, b, r).is_none());
        assert!(segment_capsule(Vec3::new(0., 20., 0.), Vec3::new(0., 0., 0.), a, b, r).is_some_and(|t| (t - (20. - b.y - r)).abs() < 1e-3));
    }
    #[test] fn falling_percent_is_the_timeline_crossfade_curve() {
        assert_eq!(falling_percent(0.), 0.); assert_eq!(falling_percent(1.), 1.); assert!((falling_percent(0.5) - 0.5).abs() < 1e-6); assert!(falling_percent(0.25) < 0.25);
    }
    #[test] fn the_balloon_sits_above_the_crate() {
        let c = Crate { impact: Vec3::new(1., 2., 3.), yaw: 30., fall: Fall::new(60.), fall_height: 10_000., balloon_hp: 1250., balloon: true, bar: false, flare: None, reticle: None, trail: None, interact: false, key: 0, loops: [false; 3] };
        let b = balloon_center(&c);
        assert!((b.y - (2. + 100. + 616.3636 * 0.9 * 0.01)).abs() < 1e-3, "{b:?}");
        assert!((b.x - 1.).abs() < 1e-3 && (b.z - 3.).abs() < 1e-3);
    }
}
