use crate::arms::{Arms, MontageData};
use crate::scene::Scene;
use fn_animgraph::{AnimState, Montage, PawnSample, CORE_ANIMS_MALE};
use fn_building::world::{GridKey, Material, PieceId, PieceKind, PieceSpawn, WorldEvent};
use fn_game::{GameSim, EYE};
use fn_hitreg::shoot::{BodySetup, HitBody, PawnTarget, Vitals};
use fn_net::proto::{self, build_op, flag, Build, Hit, State, Storm};
use fn_net::{parse_control, Control, Event, Session};
use fn_pose::CharacterPose;
use fn_render::ModelId;
use glam::{IVec3, Mat4, Quat, Vec2, Vec3, Vec4};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[cfg(target_arch = "wasm32")]
#[path = "../../../tools/web/fn-web/src/mp_web.rs"]
mod mp_web;

const PICKAXE_DAMAGE: f32 = 15.;
const PICKAXE_RANGE_M: f32 = 2.96;
const PICKAXE_NOTIFY_S: [f32; 2] = [0.1603, 0.2015];
const SPAWN_RING_M: f32 = 1.5;
const PHYSICS_ASSET: &str = "/Game/Characters/Player/Male/Male_Avg_Base/Fortnite_M_Avg_Player_Physics";
const HIT_POS_TTL_MS: f64 = 1500.;
const NUMBER_PAWN: u8 = 1;
const NUMBER_SHIELD: u8 = 4;
const NUMBER_DBNO: u8 = 6;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static AUTOPLAY: AtomicBool = AtomicBool::new(false);
enum Inbound { Text(String), Bin(Vec<u8>) }
static INBOX: Mutex<VecDeque<Inbound>> = Mutex::new(VecDeque::new());
static NUMBERS: Mutex<Vec<(Vec3, u32, bool, u8)>> = Mutex::new(Vec::new());
static BODIES: OnceLock<Vec<BodySetup>> = OnceLock::new();
static BODIES_STARTED: AtomicBool = AtomicBool::new(false);
static USE_HELD: AtomicBool = AtomicBool::new(false);
static QUEUE: Mutex<(u8, Option<String>)> = Mutex::new((0, None));
pub const QUEUE_NONE: u8 = 0;
pub const QUEUE_QUEUED: u8 = 1;
pub const QUEUE_ASSIGNED: u8 = 2;
pub const QUEUE_ERROR: u8 = 3;

pub fn active() -> bool { ACTIVE.load(Ordering::Relaxed) }
pub fn in_match() -> bool { active() && MP.with(|m| m.borrow().as_ref().map_or(false, |mp| mp.s.started && mp.s.ended.is_none())) }
pub fn players_alive() -> Option<u32> { MP.with(|m| m.borrow().as_ref().filter(|mp| mp.s.started).map(|mp| mp.s.names.keys().filter(|s| if **s == mp.s.slot { !mp.s.dead } else { !mp.s.remotes.get(s).map_or(false, |r| r.dead) }).count() as u32)) }
pub fn take_autoplay() -> bool { AUTOPLAY.load(Ordering::Relaxed) && AUTOPLAY.swap(false, Ordering::Relaxed) }
pub fn request_autoplay() { AUTOPLAY.store(true, Ordering::Relaxed); }
pub fn set_use_held(down: bool) { USE_HELD.store(down, Ordering::Relaxed); }
pub fn queue_state() -> (u8, Option<String>) { QUEUE.lock().map(|q| q.clone()).unwrap_or((QUEUE_NONE, None)) }
pub fn reset_queue() { if let Ok(mut q) = QUEUE.lock() { *q = (QUEUE_NONE, None); } }
fn set_queue(state: u8, code: Option<String>) { if let Ok(mut q) = QUEUE.lock() { *q = (state, code); } }
pub fn send_unqueue() -> bool { send_text(r#"{"t":"unqueue"}"#) }
pub fn send_queue(playlist: usize, fill: bool) -> bool { set_queue(QUEUE_NONE, None); send_text(&serde_json::json!({ "t": "queue", "playlist": playlist.min(2), "fill": fill }).to_string()) }
fn with_mp<R>(f: impl FnOnce(&Mp) -> R) -> Option<R> { MP.with(|m| m.borrow().as_ref().filter(|mp| mp.s.started).map(f)) }
pub fn local_knocked() -> bool { active() && with_mp(|mp| mp.s.dbno && !mp.s.dead).unwrap_or(false) }
pub fn local_being_revived() -> bool { active() && with_mp(|mp| mp.s.reviver.is_some()).unwrap_or(false) }
pub fn teams_alive() -> Option<usize> { if !active() { return None; } with_mp(|mp| mp.s.teams_alive()) }
pub fn team_alive() -> bool { active() && with_mp(|mp| !mp.s.dead || mp.s.teammates().iter().any(|t| mp.s.alive(*t))).unwrap_or(false) }
pub fn squad_play() -> bool { active() && with_mp(|mp| mp.s.squad).unwrap_or(false) }
pub fn spectating_for() -> Option<(String, f32)> { if !active() { return None; } with_mp(|mp| mp.spectate.and_then(|(_, t)| t).map(|t| (mp.s.name_of(t), ((mp.now_ms() - mp.view_since) / 1000.) as f32))).flatten() }
pub fn spectate_vitals() -> Option<(f32, f32)> { if !active() { return None; } with_mp(|mp| mp.spectate.and_then(|(_, t)| t).and_then(|t| mp.s.remotes.get(&t)).map(|r| (r.health, r.shield))).flatten() }
pub fn spectating() -> Option<String> { if !active() { return None; } with_mp(|mp| mp.spectate.and_then(|(_, t)| t).map(|t| mp.s.name_of(t))).flatten() }
pub fn team_hud() -> Vec<fn_ui::screens::team_info::TeamMember> {
    if !active() { return vec![]; }
    with_mp(|mp| mp.s.teammates().into_iter().map(|s| {
        let r = mp.s.remotes.get(&s);
        let mut m = fn_ui::screens::team_info::TeamMember::new(&mp.s.name_of(s), 0, r.map_or(1., |r| r.health / 100.), r.map_or(0., |r| r.shield / 100.));
        m.dbno = r.map_or(false, |r| r.dbno); m.dead = r.map_or(false, |r| r.dead); m.reviving = r.map_or(false, |r| r.reviver.is_some());
        m
    }).collect()).unwrap_or_default()
}
pub fn team_map() -> Vec<(u8, f32, f32, f32, bool)> {
    if !active() { return vec![]; }
    with_mp(|mp| mp.s.teammates().into_iter().enumerate().filter_map(|(i, s)| { let r = mp.s.remotes.get(&s)?; if r.dead { return None; } let smp = r.interp.latest()?; Some(((i + 1).min(3) as u8, smp.pos.x, smp.pos.z, smp.yaw, r.dbno)) }).collect()).unwrap_or_default()
}
pub fn team_indicator_points() -> Vec<(String, Vec3, bool, bool)> {
    if !active() { return vec![]; }
    with_mp(|mp| mp.targets.iter().filter(|t| !t.dead && mp.s.is_teammate(t.slot)).map(|t| {
        let rel = if t.knocked { fn_ui::screens::team_indicator_cooked::RELATIVE_LOCATION_DBNO_CM } else { fn_ui::screens::team_indicator_cooked::RELATIVE_LOCATION_STANDING_CM };
        let at = t.feet + Vec3::Y * ((fn_movement::params::CAPSULE_HALF_HEIGHT + rel[2]) / 100.);
        (mp.s.name_of(t.slot), at, t.knocked, mp.s.remotes.get(&t.slot).map_or(false, |r| r.reviver.is_some()))
    }).collect()).unwrap_or_default()
}
pub fn revive_prompt(key: &str) -> Option<(String, String, Vec3, f32)> {
    if !active() { return None; }
    with_mp(|mp| {
        let t = mp.revive_focus?;
        let feet = mp.targets.iter().find(|x| x.slot == t)?.feet;
        let now = mp.now_ms();
        Some((key.to_owned(), crate::dbno_hook::REVIVE_TEXT.to_owned(), feet + Vec3::Y * 0.5, mp.revive.progress(now).unwrap_or(0.)))
    }).flatten()
}
pub fn pawn_fire() -> fn_hitreg::shoot::PawnFire {
    let team = if active() { with_mp(|mp| mp.s.team as u32) } else { None };
    fn_hitreg::shoot::PawnFire { shooter: u64::MAX, shooter_team: team, damage_enabled: fn_hitreg::shoot::pawn_damage_gate(), ..Default::default() }
}
pub fn remote_feet() -> Vec<Vec3> { if !active() { return vec![]; } with_mp(|mp| mp.s.remotes.values().filter(|r| !r.dead).filter_map(|r| r.interp.latest().map(|smp| smp.pos - Vec3::new(0., fn_game::sim::EYE, 0.))).collect()).unwrap_or_default() }
pub fn local_dead() -> bool { active() && MP.with(|m| m.borrow().as_ref().map_or(false, |mp| mp.s.started && mp.s.dead)) }
pub fn take_numbers() -> Vec<(Vec3, u32, bool, u8)> { NUMBERS.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default() }
fn push(m: Inbound) {
    ACTIVE.store(true, Ordering::Relaxed);
    if let Ok(mut q) = INBOX.lock() { if q.len() >= 4096 { q.pop_front(); } q.push_back(m); }
}
pub fn push_text(s: String) { if note_party(&s) { push(Inbound::Text(s)); } }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Peek { pub code: String, pub open: bool, pub leader: String, pub size: usize, pub max: usize, pub reached: bool }

static PEEKS: Mutex<Vec<Peek>> = Mutex::new(Vec::new());
static PARTY_ERROR: Mutex<Option<String>> = Mutex::new(None);
static CONNECTING: AtomicBool = AtomicBool::new(false);

pub fn take_peeks() -> Vec<Peek> { PEEKS.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default() }
pub fn take_party_error() -> Option<String> { PARTY_ERROR.lock().ok().and_then(|mut e| e.take()) }
pub fn connecting() -> bool { CONNECTING.load(Ordering::Relaxed) }

pub fn party_create(_name: &str) -> bool {
    if connecting() || party().is_some_and(|p| p.me.is_some()) { return false; }
    #[cfg(target_arch = "wasm32")]
    { let ok = mp_web::create(_name); CONNECTING.store(ok, Ordering::Relaxed); return ok; }
    #[cfg(not(target_arch = "wasm32"))]
    false
}

pub fn party_join(_code: &str, _name: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    { if let Ok(mut g) = PARTY.lock() { *g = None; } let ok = mp_web::join(_code, _name); CONNECTING.store(ok, Ordering::Relaxed); return ok; }
    #[cfg(not(target_arch = "wasm32"))]
    false
}

pub fn party_peek(_code: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    return mp_web::peek(_code);
    #[cfg(not(target_arch = "wasm32"))]
    false
}

pub fn party_leave() {
    #[cfg(target_arch = "wasm32")]
    mp_web::leave();
    CONNECTING.store(false, Ordering::Relaxed);
    if let Ok(mut g) = PARTY.lock() { *g = None; }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartyMember { pub slot: u8, pub name: String, pub host: bool, pub ready: bool, pub outfit: String, pub playlist: usize }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartyView { pub me: Option<u8>, pub code: String, pub members: Vec<PartyMember> }

impl PartyView {
    pub fn local(&self) -> Option<&PartyMember> { self.me.and_then(|m| self.members.iter().find(|p| p.slot == m)) }
    pub fn others(&self) -> Vec<&PartyMember> { let mut v: Vec<&PartyMember> = self.members.iter().filter(|p| Some(p.slot) != self.me).collect(); v.sort_by_key(|p| p.slot); v }
    pub fn is_leader(&self) -> bool { self.local().map_or(true, |p| p.host) }
    pub fn leader(&self) -> Option<&PartyMember> { self.members.iter().find(|p| p.host) }
}

static PARTY: Mutex<Option<PartyView>> = Mutex::new(None);

pub fn party() -> Option<PartyView> { PARTY.lock().ok().and_then(|p| p.clone()) }

fn note_party(text: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return true };
    let kind = v.get("t").and_then(|t| t.as_str());
    if v.get("m").and_then(|x| x.as_u64()) == Some(1) { return true; }
    match kind {
        Some("queued") => { set_queue(QUEUE_QUEUED, None); return false; }
        Some("unqueued") => { set_queue(QUEUE_NONE, None); eprintln!("[mp] matchmaking: cancelled (unqueued)"); return false; }
        Some("unqueue_error") => { eprintln!("[mp] matchmaking: cancel refused ({}), the match is already forming", v.get("code").and_then(|x| x.as_str()).unwrap_or("")); return false; }
        Some("assign") => { set_queue(QUEUE_ASSIGNED, None); eprintln!("[mp] matchmaking: assigned to match {} (team {})", v.get("match").and_then(|x| x.as_str()).unwrap_or(""), v.get("team").and_then(|x| x.as_u64()).unwrap_or(0)); return false; }
        Some("queue_error") => { set_queue(QUEUE_ERROR, v.get("code").and_then(|x| x.as_str()).map(str::to_owned)); return false; }
        _ => {}
    }
    if kind == Some("peek") {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
        let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let p = Peek { code: s("code"), open: v.get("open").and_then(|x| x.as_bool()).unwrap_or(false), leader: s("leader"), size: n("size"), max: n("max"), reached: v.get("reached").and_then(|x| x.as_bool()).unwrap_or(true) };
        if let Ok(mut q) = PEEKS.lock() { q.push(p); }
        return false;
    }
    if kind == Some("error") || kind == Some("closed") {
        CONNECTING.store(false, Ordering::Relaxed);
        if let Ok(mut e) = PARTY_ERROR.lock() { *e = Some(v.get("code").and_then(|x| x.as_str()).unwrap_or("closed").to_owned()); }
        if kind == Some("closed") { return false; }
    }
    let Ok(mut g) = PARTY.lock() else { return true };
    match kind {
        Some("welcome") => {
            CONNECTING.store(false, Ordering::Relaxed);
            let p = g.get_or_insert_with(PartyView::default);
            p.me = v.get("slot").and_then(|s| s.as_u64()).map(|s| s as u8);
            p.code = v.get("code").and_then(|c| c.as_str()).unwrap_or("").to_owned();
        }
        Some("lobby") => {
            let p = g.get_or_insert_with(PartyView::default);
            p.members = v.get("players").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|m| Some(PartyMember {
                slot: m.get("slot")?.as_u64()? as u8, name: m.get("name")?.as_str()?.to_owned(), host: m.get("host")?.as_bool()?,
                ready: m.get("ready").and_then(|r| r.as_bool()).unwrap_or(false), outfit: m.get("outfit").and_then(|o| o.as_str()).unwrap_or("").to_owned(),
                playlist: m.get("playlist").and_then(|o| o.as_u64()).unwrap_or(0).min(2) as usize,
            })).collect()).unwrap_or_default();
        }
        Some("end") | Some("kicked") => { *g = None; CONNECTING.store(false, Ordering::Relaxed); }
        _ => {}
    }
    true
}

fn send_text(_json: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    return mp_web::send_text(_json);
    #[cfg(not(target_arch = "wasm32"))]
    false
}

pub fn send_member(ready: bool, outfit: &str, playlist: usize) -> bool { send_text(&serde_json::json!({ "t": "member", "ready": ready, "outfit": outfit, "playlist": playlist.min(2) }).to_string()) }
pub fn send_start() -> bool { send_text(r#"{"t":"start"}"#) }
pub fn push_bin(b: Vec<u8>) { push(Inbound::Bin(b)); }

fn send(_b: &[u8]) {
    #[cfg(target_arch = "wasm32")]
    mp_web::send(_b);
}
fn feed(text: &str) {
    eprintln!("[mp] {text}");
    #[cfg(target_arch = "wasm32")]
    mp_web::feed(text);
}

fn ensure_bodies() {
    if BODIES.get().is_some() || BODIES_STARTED.load(Ordering::Relaxed) { return; }
    let Some(dir) = crate::hitreg_hook::install_dir() else { return };
    BODIES_STARTED.store(true, Ordering::Relaxed);
    std::thread::spawn(move || {
        let r = fn_pak::PakSet::open_install(&dir).map_err(|e| e.to_string()).and_then(|paks| fn_hitreg::shoot::load_body_setups(&fn_pose::ClipSourceDir::Pak(&paks), PHYSICS_ASSET));
        match r {
            Ok(b) => { eprintln!("[mp] hit bodies: {} physics-asset bodies from {PHYSICS_ASSET}", b.len()); let _ = BODIES.set(b); }
            Err(e) => { eprintln!("[mp] hit bodies unavailable ({e}): capsule stand-in bodies"); let _ = BODIES.set(vec![]); }
        }
    });
}

pub fn ue_bone_to_mesh() -> Mat4 { Mat4::from_cols(Vec4::new(0.01, 0., 0., 0.), Vec4::new(0., 0., 0.01, 0.), Vec4::new(0., 0.01, 0., 0.), Vec4::W) }

struct RemoteGfx { anim: AnimState, pose: CharacterPose, body: ModelId, head: ModelId, pick: ModelId, fire: Option<(usize, f32, bool)>, logged: bool, death: Option<(f32, Vec3, f32)>, react: Option<(u32, f32)>, dbno: crate::dbno_hook::RemoteDbno, dbno_death: bool, dissolving: bool, last_world: Mat4, root_effect: Option<Vec3> , emote: crate::emote_hook::Remote }

#[derive(Clone)]
struct Target { slot: u8, team: u32, feet: Vec3, dead: bool, crouched: bool, health: f32, shield: f32, bodies: Vec<HitBody>, knocked: bool }

struct Mp {
    s: Session,
    t0: Instant,
    gfx: BTreeMap<u8, RemoteGfx>,
    pool: Vec<(ModelId, ModelId, ModelId)>,
    build_cursor: Option<u64>,
    keys: HashMap<PieceId, GridKey>,
    applying: HashSet<PieceId>,
    swing: (u8, f32, bool),
    spawned: bool,
    targets: Vec<Target>,
    last_fire: HashMap<u8, u32>,
    last_hit: HashMap<u8, (Vec3, f64)>,
    pieces_seen: usize,
    seed: u64,
    revive: crate::dbno_hook::Revive,
    revive_focus: Option<u8>,
    spectate: Option<(f32, Option<u8>)>,
    killer: Option<u8>,
    view_since: f64,
}

thread_local! { static MP: RefCell<Option<Mp>> = const { RefCell::new(None) }; }

impl Mp {
    fn now_ms(&self) -> f64 { self.t0.elapsed().as_secs_f64() * 1000. }
    fn new(s: Session) -> Mp {
        Mp { s, t0: Instant::now(), gfx: BTreeMap::new(), pool: vec![], build_cursor: None, keys: HashMap::new(), applying: HashSet::new(), swing: (0, 0., false), spawned: false, targets: vec![], last_fire: HashMap::new(), last_hit: HashMap::new(), pieces_seen: 0, seed: 0x5eed_d1e5, revive: Default::default(), revive_focus: None, spectate: None, killer: None, view_since: 0. }
    }
}

fn held_index(core: Option<&str>) -> u8 { core.and_then(|c| CORE_ANIMS_MALE.iter().position(|(k, _)| *k == c)).map_or(0, |i| i as u8 + 1) }
fn held_core(i: u8) -> Option<&'static str> { if i == 0 { None } else { CORE_ANIMS_MALE.get(i as usize - 1).map(|(k, _)| *k) } }

fn teleport(sim: &mut GameSim, eye: Vec3) {
    let floor = sim.floor_below(Vec3::new(eye.x, eye.y + 1., eye.z));
    let h = (eye.y - EYE).max(floor);
    sim.controller.position = Vec2::new(eye.x, eye.z - fn_game::Z_OFFSET);
    sim.controller.velocity = Vec2::ZERO;
    sim.controller.vertical_speed = 0.;
    sim.controller.height = h;
    sim.x = eye.x; sim.z = eye.z; sim.y = EYE + h;
}

fn spawn_point(sim: &GameSim, slot: u8, players: usize) -> (Vec3, f32) {
    let (x, y, z, _, _) = fn_game::spawn();
    let n = players.max(2) as f32;
    let a = slot as f32 * std::f32::consts::TAU / n;
    let at = Vec3::new(x + a.cos() * SPAWN_RING_M, y, z + a.sin() * SPAWN_RING_M);
    let d = Vec3::new(x, y, z) - at;
    (Vec3::new(at.x, EYE + sim.ground_y(at.x, at.z), at.z), d.x.atan2(-d.z).to_degrees().rem_euclid(360.))
}

fn kind_u8(k: PieceKind) -> u8 { PieceKind::ALL.iter().position(|x| *x == k).unwrap_or(0) as u8 }
fn mat_u8(m: Material) -> u8 { match m { Material::Wood => 0, Material::Stone => 1, Material::Metal => 2 } }
fn kind_of(v: u8) -> Option<PieceKind> { PieceKind::ALL.get(v as usize).copied() }
fn mat_of(v: u8) -> Option<Material> { [Material::Wood, Material::Stone, Material::Metal].get(v as usize).copied() }

fn pawn_target(t: &Target) -> PawnTarget {
    let mut p = PawnTarget::new(t.slot as u64, t.team, t.feet, Vitals::new(t.health.max(0.01), t.shield));
    p.crouched = t.crouched;
    p.bodies = if t.bodies.is_empty() { p.coarse_bodies() } else { t.bodies.clone() };
    p
}

pub fn pawn_targets() -> Vec<PawnTarget> {
    if !active() { return vec![]; }
    MP.with(|m| m.borrow().as_ref().map(|mp| mp.targets.iter().filter(|t| !t.dead).map(pawn_target).collect()).unwrap_or_default())
}

fn remember_hits(mp: &mut Mp, hits: &[fn_hitreg::shoot::PawnHit]) {
    let now = mp.now_ms();
    for h in hits { mp.last_hit.insert(h.id as u8, (h.pos, now)); }
}

pub fn on_projectile_hits(row: &str, hits: &[fn_hitreg::shoot::PawnHit]) {
    if !active() { return; }
    MP.with(|m| {
        let mut b = m.borrow_mut();
        let Some(mp) = b.as_mut() else { return };
        if !mp.s.started || mp.s.dead { return; }
        let weapon = fn_net::weapon_id(row);
        let Some(seq) = mp.last_fire.get(&weapon).copied() else { return };
        remember_hits(mp, hits);
        for h in hits { if h.damage > 0. { mp.s.send_hit(Hit { fire_seq: seq, target: h.id as u8, weapon, flags: h.feedback.critical as u8, pellets: 1, damage: h.damage }); } }
    });
}

pub fn on_cartridge(row: &str, eye: Vec3, dir: Vec3, hits: &[fn_hitreg::shoot::PawnHit]) {
    if !active() { return; }
    MP.with(|m| {
        let mut b = m.borrow_mut();
        let Some(mp) = b.as_mut() else { return };
        if !mp.s.started || mp.s.dead { return; }
        let weapon = fn_net::weapon_id(row);
        let seq = mp.s.send_fire(weapon, eye, dir, 0);
        mp.last_fire.insert(weapon, seq);
        remember_hits(mp, hits);
        if !hits.is_empty() { eprintln!("[mp] cartridge bones: {}", hits.iter().map(|h| format!("{}@{:.2}m {:?}", h.bone.as_deref().unwrap_or("-"), h.distance_m, h.pos)).collect::<Vec<_>>().join(", ")); }
        let mut per: BTreeMap<u8, (f32, u8, bool)> = BTreeMap::new();
        for h in hits {
            let e = per.entry(h.id as u8).or_insert((0., 0, false));
            e.0 += h.damage; e.1 += 1; e.2 |= h.feedback.critical;
        }
        for (target, (damage, pellets, head)) in per {
            if damage > 0. {
                eprintln!("[mp] hit claim fire {seq} -> {} for {damage:.1} ({pellets} pellet(s){})", mp.s.name_of(target), if head { ", head" } else { "" });
                mp.s.send_hit(Hit { fire_seq: seq, target, weapon, flags: head as u8, pellets, damage });
            }
        }
    });
}

fn melee(mp: &mut Mp, sim: &GameSim) {
    let (stage, time) = (sim.swing.stage, sim.swing.time);
    let (ps, pt, _) = mp.swing;
    if (stage == 1 || stage == 2) && (stage != ps || time < pt) { mp.swing.2 = true; }
    mp.swing.0 = stage; mp.swing.1 = time;
    let notify = PICKAXE_NOTIFY_S[if stage == 2 { 1 } else { 0 }];
    if !(mp.swing.2 && (stage == 1 || stage == 2) && time >= notify) { return; }
    mp.swing.2 = false;
    if mp.s.dead { return; }
    let eye = sim.cam_view.eye;
    let fwd = sim.cam_view.forward;
    let my_team = mp.s.team as u32;
    let target = mp.targets.iter().filter(|t| !t.dead && t.team != my_team).filter_map(|t| {
        let (a, b, r) = pawn_target(t).capsule_segment();
        fn_hitreg::shoot::ray_capsule(eye, fwd, PICKAXE_RANGE_M, a, b, r + fn_combat::MELEE_RADIUS).map(|(d, _)| (t.slot, d, eye + fwd * d))
    }).min_by(|a, b| a.1.total_cmp(&b.1));
    eprintln!("[mp] pickaxe swing {stage} notify at {time:.3} s: {}", target.map_or("no pawn in reach".into(), |t| format!("hit {} at {:.2} m", mp.s.name_of(t.0), t.1)));
    if let Some((slot, _, pos)) = target {
        let seq = mp.s.send_fire(fn_net::PICKAXE, eye, fwd, 0);
        let now = mp.now_ms();
        mp.last_hit.insert(slot, (pos, now));
        mp.s.send_hit(Hit { fire_seq: seq, target: slot, weapon: fn_net::PICKAXE, flags: 0, pellets: 1, damage: PICKAXE_DAMAGE });
    }
}

fn local_state(sim: &GameSim, arms: Option<&Arms>, show_pick: bool) -> State {
    let c = &sim.controller;
    let mut f = 0u16;
    if c.crouched { f |= flag::CROUCHED; }
    if c.running { f |= flag::RUNNING; }
    if c.airborne() { f |= flag::AIRBORNE; }
    if sim.ads.active && sim.ads.weight > 0.5 { f |= flag::TARGETING; }
    if crate::building_hook::is_building() { f |= flag::BUILDING; }
    if sim.swing.stage > 0 { f |= flag::SWINGING; }
    if show_pick { f |= flag::PICKAXE; }
    if sim.cam_view.hide_pawn { f |= flag::HIDDEN; }
    if sim.items_blocked() { f |= flag::AERIAL; }
    let f = flag::with_gun(f, arms.and_then(|a| a.gun_index()).filter(|_| !show_pick));
    let held = held_index(arms.and_then(|a| a.held_core()).map(|h| h.0));
    State { seq: 0, t_ms: 0, pos: Vec3::new(sim.x, sim.y, sim.z), vel: Vec3::new(c.velocity.x, c.vertical_speed, c.velocity.y), yaw: sim.yaw, pitch: sim.pitch, flags: f, held, air_dm: (c.air_height() * 10.).clamp(0., 255.) as u8 }
}

fn sync_builds(mp: &mut Mp) {
    let n = crate::building_hook::with_world(|w| w.len());
    if n != mp.pieces_seen { mp.pieces_seen = n; }
    let (events, next) = crate::building_hook::with_world(|w| {
        let start = mp.build_cursor.unwrap_or(u64::MAX);
        let (ev, next) = w.events_since(if start == u64::MAX { u64::MAX } else { start });
        let out: Vec<(WorldEvent, Option<fn_building::world::PlacedPiece>)> = if start == u64::MAX { vec![] } else { ev.iter().map(|e| (e.clone(), match e { WorldEvent::Placed(id) | WorldEvent::Edited(id) => w.get(*id).cloned(), _ => None })).collect() };
        (out, next)
    });
    mp.build_cursor = Some(next);
    for (e, piece) in events {
        match (e, piece) {
            (WorldEvent::Placed(id), Some(p)) => {
                mp.keys.insert(id, p.key);
                if mp.applying.remove(&id) { continue; }
                mp.s.send_build(&Build { op: build_op::PLACE, kind: kind_u8(p.kind), material: mat_u8(p.material), rot: p.key.rot, cell: p.key.cell.to_array(), loc: p.location, yaw: p.yaw_deg, rot_iter: 0, mirrored: false, class_path: String::new() });
            }
            (WorldEvent::Edited(id), Some(p)) => {
                if mp.applying.remove(&id) { continue; }
                let cp = p.edit.class_path.clone().unwrap_or_default();
                mp.s.send_build(&Build { op: build_op::EDIT, kind: kind_u8(p.kind), material: mat_u8(p.material), rot: p.key.rot, cell: p.key.cell.to_array(), loc: p.location, yaw: p.yaw_deg, rot_iter: p.edit.rotation_iterations.clamp(-128, 127) as i8, mirrored: p.edit.mirrored, class_path: cp });
            }
            (WorldEvent::Removed(id), _) => {
                let key = mp.keys.remove(&id);
                if mp.applying.remove(&id) { continue; }
                if let Some(k) = key { mp.s.send_build(&Build { op: build_op::REMOVE, kind: kind_u8(k.kind), material: 0, rot: k.rot, cell: k.cell.to_array(), loc: Vec3::ZERO, yaw: 0., rot_iter: 0, mirrored: false, class_path: String::new() }); }
            }
            _ => {}
        }
    }
}

fn apply_build(mp: &mut Mp, b: &Build) {
    let (Some(kind), Some(material)) = (kind_of(b.kind), mat_of(b.material)) else { return };
    let key = GridKey { kind, cell: IVec3::from_array(b.cell), rot: b.rot & 3 };
    match b.op {
        build_op::PLACE => {
            let Some(def) = crate::building_hook::with_catalog(|c| c.and_then(|c| c.def(kind, material).cloned())) else { return };
            let id = crate::building_hook::with_world(|w| {
                if let Some(old) = w.at_key(&key) { mp.applying.insert(old); }
                w.spawn(PieceSpawn { kind, material, class_path: def.class.class_path.clone(), key, location: b.loc, yaw_deg: b.yaw, max_health: def.max_health, start_health: def.start_health(), build_time: def.build_time })
            });
            mp.applying.insert(id);
            mp.keys.insert(id, key);
        }
        build_op::EDIT => {
            crate::building_hook::with_world(|w| {
                let Some(id) = w.at_key(&key) else { return };
                mp.applying.insert(id);
                w.set_edit(id, fn_building::world::EditState { class_path: (!b.class_path.is_empty()).then(|| b.class_path.clone()), rotation_iterations: b.rot_iter as i32, mirrored: b.mirrored });
            });
        }
        build_op::REMOVE => {
            crate::building_hook::with_world(|w| { if let Some(id) = w.at_key(&key) { mp.applying.insert(id); w.remove(id); } });
        }
        _ => {}
    }
}

fn storm_of(sim: &GameSim) -> Storm { let s = &sim.storm; Storm { phase: 0, state: s.enabled as u8, center: s.center_cm, next: [s.radius_cm, 0., 0.], time_left: 0. } }

fn montage_at(m: &MontageData, t: f32) -> Option<Montage> { let (clip, time) = m.sample(t)?; let w = m.weight(t); (w > 0.).then_some(Montage { slot: m.slot, clip, time, weight: w, blend: m.blend }) }

pub(crate) fn socket_bones(scene: &Scene, pose: &CharacterPose, world: Mat4) -> (Option<Vec3>, Option<Vec3>) {
    let Some((part, p)) = scene.character.rig.parts.first().zip(pose.parts.first()) else { return (None, None) };
    let at = |b: &str| part.tree.bone_index(b).and_then(|i| p.bone_mesh.get(i)).map(|m| (world * *m * ue_bone_to_mesh()).w_axis.truncate());
    (at(crate::elim_fx::HEAD_SOCKET_BONE), at(crate::elim_fx::ROOT_SOCKET_BONE))
}

fn draw_remotes(mp: &mut Mp, scene: &mut Scene, arms: Option<&Arms>, dt: f32) {
    let now = mp.now_ms();
    let live: Vec<u8> = mp.s.remotes.keys().copied().collect();
    let gone: Vec<u8> = mp.gfx.keys().copied().filter(|s| !live.contains(s)).collect();
    for s in gone { if let Some(g) = mp.gfx.remove(&s) { mp.pool.push((g.body, g.head, g.pick)); } }
    mp.targets.clear();
    let (b0, h0, p0) = scene.pawn_models();
    let setups = BODIES.get();
    let to_mesh = ue_bone_to_mesh();
    for (slot, r) in mp.s.remotes.iter() {
        let Some(smp) = r.sample(now) else { continue };
        let feet = smp.pos - Vec3::Y * EYE;
        let crouched = smp.flags & flag::CROUCHED != 0;
        let mut target = Target { slot: *slot, team: mp.s.team_of(*slot) as u32, feet, dead: r.dead, crouched, health: r.health, shield: r.shield, bodies: vec![], knocked: r.dbno };
        let dying = r.dead && mp.gfx.get(slot).map_or(false, |g| g.death.map_or(true, |d| d.0 < crate::death_hook::HIDE_AFTER_S));
        if (r.dead && !dying) || (!r.dead && smp.flags & flag::HIDDEN != 0) { mp.targets.push(target); continue; }
        if !mp.gfx.contains_key(slot) {
            let (body, head, pick) = mp.pool.pop().unwrap_or_else(|| (scene.renderer.instance_model(b0), scene.renderer.instance_model(h0), scene.renderer.instance_model(p0)));
            crate::elim_fx::set_dissolve(&scene.renderer, &[body, head], None);
            let mut anim = AnimState::new();
            let still = PawnSample { velocity: (0., 0.), input: (0., 0.), facing_deg: smp.yaw, camera_yaw_deg: smp.yaw, camera_pitch_deg: smp.pitch, height: 0., vertical_speed: 0., fall_time_left: 0., crouched: false, running: false, melee_swinging: false };
            for _ in 0..60 { anim.step_sample(&still, 1. / 60.); }
            mp.gfx.insert(*slot, RemoteGfx { anim, pose: CharacterPose::default(), body, head, pick, fire: None, logged: false, death: None, react: None, dbno: Default::default(), dbno_death: false, dissolving: false, last_world: Mat4::IDENTITY, root_effect: None, emote: Default::default() });
            eprintln!("[mp] remote pawn {} ({}) drawn at {:?}", slot, r.name, smp.pos);
        }
        let Some(g) = mp.gfx.get_mut(slot) else { continue };
        let death_data = arms.and_then(|a| a.data.death.as_ref());
        if !r.dead { g.death = None; g.dbno_death = r.dbno; }
        let knocked = r.dbno && !r.dead;
        g.anim.set_dbno(knocked);
        let mut feet = feet;
        let mut yaw = smp.yaw;
        let mut death_t = None;
        if r.dead {
            let first = g.death.is_none();
            let d = g.death.get_or_insert((0., feet, smp.yaw));
            let t0 = if first { -1. } else { d.0 };
            if !first { d.0 += dt.max(0.); }
            feet = d.1; yaw = d.2; death_t = Some(d.0);
            if first { g.fire = None; g.react = None; eprintln!("[mp] remote {} died: PlayerDeath_Athena at {:?}", slot, feet); crate::elim_fx::on_death(Mat4::from_rotation_translation(Quat::from_rotation_y((180. - g.anim.visual_yaw(yaw)).to_radians()), feet) * ue_bone_to_mesh()); }
            if let Some(dd) = death_data { let dm = if g.dbno_death { dd.death_dbno.as_ref().unwrap_or(&dd.death) } else { &dd.death }; for cue in crate::death_hook::cues_between(dm, t0, d.0) { crate::death_hook::push_sound(fn_audio_sink::SoundEvent::BuildingCue { cue, pos: Some(feet + Vec3::Y * 0.9) }); } }
            target.feet = feet;
        }
        g.anim.set_held(if death_t.is_some() || knocked { None } else { held_core(smp.held).map(|k| (k, smp.flags & flag::TARGETING != 0)) });
        let v2 = if death_t.is_some() { Vec2::ZERO } else { Vec2::new(smp.vel.x, smp.vel.z) };
        let input = if v2.length() > 0.3 { v2.normalize() } else { Vec2::ZERO };
        let airborne = death_t.is_none() && smp.flags & flag::AIRBORNE != 0;
        let height = if airborne { smp.air_m.max(1e-4) } else { 0. };
        let sample = PawnSample { velocity: (v2.x, v2.y), input: (input.x, input.y), facing_deg: yaw, camera_yaw_deg: yaw, camera_pitch_deg: if death_t.is_some() { 0. } else { smp.pitch },
            height, vertical_speed: if airborne { smp.vel.y } else { 0. }, fall_time_left: if airborne { fn_movement::controller::fall_time_left(height, smp.vel.y) } else { 0. },
            crouched: crouched && death_t.is_none(), running: death_t.is_none() && smp.flags & flag::RUNNING != 0, melee_swinging: death_t.is_none() && smp.flags & flag::SWINGING != 0 };
        g.anim.step_sample(&sample, dt.max(1e-4));
        { let c = std::mem::take(&mut g.anim.dbno_crawl); crate::dbno_hook::crawl_sounds(&c, feet); }
        let mut montages = vec![];
        if let (Some(t), Some(dd)) = (death_t, death_data) { montages.extend(crate::death_hook::montage_at(if g.dbno_death { dd.death_dbno.as_ref().unwrap_or(&dd.death) } else { &dd.death }, t)); }
        if death_t.is_none() { montages.extend(g.dbno.step(death_data, knocked, dt, feet + Vec3::Y * 0.4)); }
        if let Some((pick, t)) = g.react.as_mut() {
            *t += dt;
            match death_data.and_then(|dd| dd.reacts.get(*pick as usize)).and_then(|m| crate::death_hook::montage_at(m, *t)) { Some(m) => montages.push(m), None => g.react = None }
        }
        let mut emote_holster = false;
        if death_t.is_none() { let (m, h) = g.emote.step(dt, feet, *slot, !knocked); montages.extend(m); emote_holster = h; } else if g.emote.playing.is_some() { g.emote.step(dt, feet, *slot, false); }
        if let Some((gun, t, ads)) = g.fire.as_mut() {
            *t += dt;
            let data = arms.and_then(|a| a.data.guns.get(*gun));
            match data.and_then(|d| montage_at(if *ads { d.fire_ads.as_ref().unwrap_or(&d.fire) } else { &d.fire }, *t)) { Some(m) => montages.push(m), None => g.fire = None }
        }
        let tree = g.anim.plan(&montages);
        scene.character.rig.evaluate_into(&tree, &*scene.character.clips, &mut g.pose);
        let facing = g.anim.visual_yaw(yaw);
        let holstered = emote_holster || knocked || death_t.map_or(false, |t| death_data.map_or(true, |dd| dd.holstered(t)));
        let world = Mat4::from_rotation_translation(Quat::from_rotation_y((180. - facing).to_radians()), feet);
        g.last_world = world;
        if let Some(p) = g.pose.parts.first() { scene.renderer.update_model(g.body, world, &p.skin_matrices); }
        if let Some(p) = g.pose.parts.get(1) { scene.renderer.update_model(g.head, world, &p.skin_matrices); }
        if death_t.is_some() || g.dissolving { let (h, r) = socket_bones(scene, &g.pose, world); g.dissolving = crate::elim_fx::remote_dissolve(&scene.renderer, &[g.body, g.head], h, r, death_t); if let Some(t) = death_t { crate::elim_fx::shell_frame(*slot, t, world, g.pose.parts.iter().take(2).map(|p| p.skin_matrices.clone()).collect(), h, r); } }
        g.root_effect = socket_bones(scene, &g.pose, world).1;
        scene.models.push(g.body);
        scene.models.push(g.head);
        let socket = g.pose.socket("RightHand").unwrap_or(Mat4::IDENTITY);
        if smp.flags & flag::PICKAXE != 0 && !holstered {
            scene.renderer.update_model(g.pick, world, &[socket]);
            scene.models.push(g.pick);
        }
        if let (Some(gun), false) = (flag::gun(smp.flags), holstered) { crate::gun_orig::remote(scene, *slot as usize, gun, world * socket); crate::gunfx_hook::set_remote_attach(*slot, world * socket); }
        if let (Some(setups), Some(part), Some(pose)) = (setups, scene.character.rig.parts.first(), g.pose.parts.first()) {
            if !setups.is_empty() {
                target.bodies = fn_hitreg::shoot::place_bodies(setups, |bone| part.tree.bone_index(bone).and_then(|i| pose.bone_mesh.get(i)).map(|m| world * *m * to_mesh));
                if !g.logged {
                    g.logged = true;
                    let head = target.bodies.iter().find(|b| b.bone.eq_ignore_ascii_case("head")).map(|b| (b.a + b.b) * 0.5);
                    let bone = part.tree.bone_index("head").and_then(|i| pose.bone_mesh.get(i)).map(|m| (world * *m).w_axis.truncate());
                    eprintln!("[mp] remote {} hit bodies: {} placed, head body at {:?}, head bone at {:?}, feet {:?}", slot, target.bodies.len(), head, bone, feet);
                }
            }
        }
        mp.targets.push(target);
    }
}

fn revive_frame(mp: &mut Mp, sim: &GameSim) {
    let mates: Vec<(u8, Vec3)> = mp.targets.iter().filter(|t| t.knocked && !t.dead && mp.s.is_teammate(t.slot)).map(|t| (t.slot, t.feet)).collect();
    let pawn = sim.feet();
    mp.revive_focus = if mp.s.dead || mp.s.dbno { None } else { crate::dbno_hook::revive_focus(pawn, sim.cam_view.forward, &mates) };
    let able = !mp.s.dead && !mp.s.dbno;
    for (t, on) in mp.revive.update(USE_HELD.load(Ordering::Relaxed), mp.revive_focus, able) {
        eprintln!("[mp] revive {} {}", if on { "start" } else { "stop" }, mp.s.name_of(t));
        mp.s.send_revive(t, on);
    }
}

fn spectate_frame(mp: &mut Mp, sim: &mut GameSim, dt: f32) {
    let Some((t, cur)) = mp.spectate.as_mut() else { return };
    let before = *t;
    *t += dt.max(0.);
    let first = fn_net::modes::dbno::SPECTATE_FIRST_S;
    let retry = fn_net::modes::dbno::SPECTATE_RETRY_S;
    let due = *t >= first && (before < first || ((*t - first) / retry).floor() > ((before - first) / retry).floor());
    let cur_ok = cur.is_some_and(|c| mp.s.alive(c));
    if due && !cur_ok {
        let alive: Vec<u8> = mp.s.names.keys().copied().filter(|s| *s != mp.s.slot && mp.s.alive(*s)).collect();
        let mates: Vec<u8> = alive.iter().copied().filter(|s| mp.s.is_teammate(*s)).collect();
        let pick = crate::dbno_hook::spectate_pick(mp.s.squad, &mates, mp.killer, mp.killer.is_some_and(|k| mp.s.alive(k)), &alive);
        let next = pick.map(|p| match p { crate::dbno_hook::Pick::Teammate(s) | crate::dbno_hook::Pick::Killer(s) | crate::dbno_hook::Pick::Any(s) => s });
        if let Some(p) = pick { eprintln!("[mp] spectating {:?} {}", p, next.map_or(String::new(), |s| mp.s.name_of(s))); }
        if matches!(pick, Some(crate::dbno_hook::Pick::Killer(_))) { mp.killer = None; }
        if *cur != next { mp.view_since = mp.t0.elapsed().as_secs_f64() * 1000.; }
        *cur = next;
    }
    let Some(target) = *cur else { return };
    let now = mp.now_ms();
    let Some(smp) = mp.s.remotes.get(&target).and_then(|r| r.sample(now)) else { return };
    teleport(sim, smp.pos);
    sim.yaw = smp.yaw;
    sim.pitch = smp.pitch;
}

pub const DEATH_DROP_RADIUS_M: f32 = 1.5;

pub fn death_toss_end(feet: Vec3, i: usize) -> Vec3 {
    let a = i as f32 * 2.399_963;
    let r = DEATH_DROP_RADIUS_M * (0.5 + 0.5 * ((i % 3) as f32 / 2.));
    Vec3::new(feet.x + a.cos() * r, feet.y, feet.z + a.sin() * r)
}

fn death_drop(mp: &mut Mp, sim: &mut GameSim, arms: &mut Arms) {
    let w = &mut sim.world;
    let mut res = [w.wood, w.stone, w.metal];
    let drops = crate::inventory::death_drops(arms, &mut res);
    (w.wood, w.stone, w.metal) = (res[0], res[1], res[2]);
    let feet = sim.feet();
    let start = feet + Vec3::Y * fn_game::pickups::PAWN_HALF_HEIGHT;
    for (i, (item, count)) in drops.iter().enumerate() {
        let e = death_toss_end(feet, i);
        let floor = sim.floor_below(Vec3::new(e.x, feet.y + 0.5, e.z));
        let id = item.rsplit('/').next().unwrap_or(item).split('.').next().unwrap_or(item);
        mp.s.send_pickup_spawn(proto::pickup_flag::DEATH, id, *count, start, Vec3::new(e.x, floor + fn_game::pickups::TOUCH_HALF_HEIGHT, e.z));
    }
    eprintln!("[mp] death drop: {} stacks sent to the relay", drops.len());
}

fn number_pos(mp: &Mp, victim: u8) -> Vec3 {
    let now = mp.now_ms();
    if let Some((p, t)) = mp.last_hit.get(&victim) { if now - t < HIT_POS_TTL_MS { return *p; } }
    mp.targets.iter().find(|t| t.slot == victim).map_or(Vec3::ZERO, |t| t.feet + Vec3::Y * 1.2)
}

fn on_events(mp: &mut Mp, sim: &mut GameSim, mut arms: Option<&mut Arms>) {
    for e in mp.s.take_events() {
        match e {
            Event::Started { .. } => { eprintln!("[mp] match started"); crate::kill_feed_hook::reset_eliminations(); }
            Event::Ended(reason) => feed(&format!("Match ended ({reason})")),
            Event::Damaged { attacker, amount, shield_hit, head, dbno } => {
                let from = mp.s.remotes.get(&attacker).and_then(|r| r.interp.latest().map(|s| s.pos)).unwrap_or(Vec3::new(sim.x, sim.y, sim.z));
                if amount > 0. { crate::hurt_hook::on_damage(shield_hit, fn_particles::sim::game_to_ue([from.x, from.y, from.z])); }
                eprintln!("[mp] took {amount:.1} {} damage from {}{}", if shield_hit { "shield" } else { "health" }, if dbno && attacker == proto::NO_KILLER { "DBNO bleed".to_owned() } else { mp.s.name_of(attacker) }, if head { " (head)" } else { "" });
            }
            Event::Hurt { victim } => {
                let pick = crate::death_hook::pick_react(&mut mp.seed);
                if let Some(g) = mp.gfx.get_mut(&victim) { if g.death.is_none() { g.react = Some((pick, 0.)); eprintln!("[mp] remote {victim} hit react: AdditiveHitReacts section {}", crate::death_hook::REACT_SECTIONS[pick as usize]); } }
            }
            Event::DamageShown { victim, amount, shield_hit, head, dbno } => {
                let pos = number_pos(mp, victim);
                let n = amount.round().max(0.) as u32;
                if n > 0 { if let Ok(mut q) = NUMBERS.lock() { q.push((pos, n, head, if dbno { NUMBER_DBNO } else if shield_hit { NUMBER_SHIELD } else { NUMBER_PAWN })); } }
                eprintln!("[mp] hit {} for {amount:.1} {}{}", mp.s.name_of(victim), if shield_hit { "shield" } else { "health" }, if head { " (head)" } else { "" });
            }
            Event::OwnHealth { health, shield, dead } => {
                if let Some(a) = arms.as_deref_mut() { a.health = health; a.shield = shield; }
                eprintln!("[mp] own vitals {health:.0} / {shield:.0}{}", if dead { " (dead)" } else { "" });
            }
            Event::Knocked { victim, by, weapon, head } => {
                use fn_ui::screens::kill_feed::{Combatant, DeathCause, Elimination, Killer};
                let who = |slot: u8| if slot == mp.s.slot { crate::kill_feed_hook::local() } else if mp.s.is_teammate(slot) { Combatant::teammate(&mp.s.name_of(slot)) } else { Combatant::other(&mp.s.name_of(slot)) };
                let row = fn_net::caps::WEAPONS.get(weapon as usize).map_or("", |w| w.row);
                let (k, cause) = if by == proto::NO_KILLER { (Killer::Themself, DeathCause::FallDamage) } else if by == victim { (Killer::Themself, DeathCause::from_weapon_row(row)) } else { (Killer::Player(who(by)), DeathCause::from_weapon_row(row)) };
                eprintln!("[mp] DBNO: {} knocked down by {} with {row}{}", mp.s.name_of(victim), mp.s.name_of(by), if head { " (head)" } else { "" });
                if victim == mp.s.slot { mp.revive.clear(); }
                if mp.s.is_teammate(victim) { if let Some(c) = fn_audio_sink::death_cue(crate::dbno_hook::TEAMMATE_DBNO_CUE) { crate::death_hook::push_sound(fn_audio_sink::SoundEvent::BuildingCue { cue: c, pos: None }); } }
                crate::kill_feed_hook::push(Elimination { killer: k, victim: who(victim), cause, dbno: true });
            }
            Event::Revived { victim, by } => {
                eprintln!("[mp] DBNO: {} revived by {}", mp.s.name_of(victim), mp.s.name_of(by));
                let at = if victim == mp.s.slot { Vec3::new(sim.x, sim.y, sim.z) } else { mp.targets.iter().find(|t| t.slot == victim).map_or(Vec3::new(sim.x, sim.y, sim.z), |t| t.feet + Vec3::Y * 0.9) };
                crate::death_hook::push_sound(fn_audio_sink::SoundEvent::BuildingCue { cue: fn_audio_sink::DEATH_CUES[2], pos: Some(at) });
                if by == mp.s.slot { mp.revive.clear(); }
                if let Some(g) = mp.gfx.get(&victim) { if let Some(r) = g.root_effect { crate::elim_fx::on_revived(g.last_world * ue_bone_to_mesh(), r); } }
            }
            Event::ReviveStarted { victim, by } => {
                eprintln!("[mp] DBNO: {} reviving {}", mp.s.name_of(by), mp.s.name_of(victim));
                if by == mp.s.slot && mp.revive.target == Some(victim) { mp.revive.started = Some(mp.now_ms()); }
            }
            Event::ReviveStopped { victim, by } => {
                eprintln!("[mp] DBNO: revive of {} by {} stopped", mp.s.name_of(victim), mp.s.name_of(by));
                if by == mp.s.slot { mp.revive.started = None; }
            }
            Event::Elim { victim, killer, weapon, head, flags } => {
                use fn_ui::screens::kill_feed::{Combatant, DeathCause, Elimination, Killer};
                if victim == mp.s.slot { if let Some(a) = arms.as_deref_mut() { a.health = 0.; a.shield = 0.; death_drop(mp, sim, a); } crate::death_hook::set_local_dbno_death(flags & proto::elim_flag::WAS_DBNO != 0); mp.killer = (killer != proto::NO_KILLER && killer != victim).then_some(killer); mp.spectate = Some((0., None)); mp.revive.clear(); }
                let who = |slot: u8| if slot == mp.s.slot { crate::kill_feed_hook::local() } else if mp.s.is_teammate(slot) { Combatant::teammate(&mp.s.name_of(slot)) } else { Combatant::other(&mp.s.name_of(slot)) };
                let row = fn_net::caps::WEAPONS.get(weapon as usize).map_or("", |w| w.row);
                let base = if flags & proto::elim_flag::BLED_OUT != 0 { DeathCause::DbnoTimeout } else { DeathCause::from_weapon_row(row) };
                let (k, cause) = if killer == proto::NO_KILLER { (Killer::Themself, DeathCause::FallDamage) } else if killer == victim { (Killer::Themself, base) } else { (Killer::Player(who(killer)), base) };
                eprintln!("[mp] elimination: {} by {} with {row}{}{}{}{}", mp.s.name_of(victim), mp.s.name_of(killer), if head { " (head)" } else { "" }, if flags & proto::elim_flag::WAS_DBNO != 0 { " (was DBNO)" } else { "" }, if flags & proto::elim_flag::BLED_OUT != 0 { " (bled out)" } else { "" }, if flags & proto::elim_flag::TEAM_WIPE != 0 { " (team wipe)" } else { "" });
                let already = victim == mp.s.slot && crate::kill_feed_hook::local_death().is_some();
                if victim == mp.s.slot { crate::kill_feed_hook::note_local_death(cause, k.clone()); }
                if killer == mp.s.slot && victim != mp.s.slot { crate::death_hook::killed_by_local(); eprintln!("[mp] PlayerKilled_Cue (local instigator, remote victim)"); }
                if !already { crate::kill_feed_hook::push(Elimination { killer: k, victim: who(victim), cause, dbno: false }); }
            }
            Event::Respawn(pos) => {
                let n = mp.s.names.len();
                let (at, yaw) = if pos == Vec3::ZERO { spawn_point(sim, mp.s.slot, n) } else { (pos, sim.yaw) };
                teleport(sim, at);
                sim.yaw = yaw;
                if let Some(a) = arms.as_deref_mut() { a.health = 100.; a.shield = 0.; }
                eprintln!("[mp] respawned at {at:?}");
            }
            Event::Correct(pos) => teleport(sim, pos),
            Event::Build { build, .. } => apply_build(mp, &build),
            Event::Storm(s) => { sim.storm.center_cm = s.center; sim.storm.radius_cm = s.next[0].max(1.); sim.storm.enabled = s.state != 0; }
            Event::Joined { name, .. } => eprintln!("[mp] {name} joined"),
            Event::Left { name, .. } => eprintln!("[mp] {name} left"),
            Event::RemoteFire { slot, fire } => {
                let gun = mp.s.remotes.get(&slot).and_then(|r| r.interp.latest()).and_then(|s| flag::gun(s.flags).map(|g| (g, s.flags & flag::TARGETING != 0)));
                if let (Some((g, ads)), Some(gfx)) = (gun, mp.gfx.get_mut(&slot)) { if fire.weapon != fn_net::PICKAXE { gfx.fire = Some((g, 0., ads)); } }
                if fire.weapon != fn_net::PICKAXE { if let Some(gh) = fn_net::caps::WEAPONS.get(fire.weapon as usize).and_then(|w| crate::hitreg_hook::gun_by_row(w.row)) { crate::gunfx_hook::remote_shot(slot, &gh, &sim.world, fire.origin, fire.dir, fire.seq as i32); } }
            }
            Event::Interact { .. } => {}
            Event::Emote { slot, op, item } => { if let Some(g) = mp.gfx.get_mut(&slot) { g.emote.on_net(op, item, slot); } }
            Event::PickupSpawned { id, owner, flags, count, start, end, item } => {
                let mine = owner == mp.s.slot && flags & proto::pickup_flag::DROPPED != 0;
                eprintln!("[mp] pickup #{id}: {count} x {item} from {}{}", mp.s.name_of(owner), if flags & proto::pickup_flag::DEATH != 0 { " (death drop)" } else { "" });
                crate::pickups_hook::net_spawn(id, &item, count as u32, start, end, mine);
            }
            Event::PickupGone { id, taker } => {
                let mine = taker == mp.s.slot;
                if mine { eprintln!("[mp] pickup #{id} granted to us"); } else { eprintln!("[mp] pickup #{id} taken by {}", mp.s.name_of(taker)); }
                crate::pickups_hook::net_gone(id, mine);
            }
        }
    }
}

pub fn frame(sim: &mut GameSim, scene: &mut Scene, mut arms: Option<&mut Arms>, dt: f32) {
    if !active() { return; }
    MP.with(|cell| {
        let mut slot = cell.borrow_mut();
        let inbound: Vec<Inbound> = INBOX.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default();
        for m in inbound {
            match m {
                Inbound::Text(t) => {
                    let match_text = t.contains("\"m\":1");
                    if !match_text && slot.as_ref().map_or(false, |m| m.s.matched && m.s.ended.is_none()) { continue; }
                    let Some(c) = parse_control(&t) else { continue };
                    if slot.as_ref().map_or(true, |m| m.s.ended.is_some()) || (match_text && matches!(c, Control::Welcome { .. })) {
                        if let Control::Welcome { slot: s, host, code, .. } = &c { let pool = slot.take().map(|m| m.pool).unwrap_or_default(); let mut m = Mp::new(Session::new(*s, *host, code.clone())); m.pool = pool; *slot = Some(m); if match_text { eprintln!("[mp] joined match {code} as slot {s}"); } }
                    }
                    if let Some(mp) = slot.as_mut() { mp.s.control(c); }
                }
                Inbound::Bin(b) => if let Some(mp) = slot.as_mut() { let now = mp.now_ms(); mp.s.recv(&b, now); },
            }
        }
        let Some(mp) = slot.as_mut() else { return };
        ensure_bodies();
        on_events(mp, sim, arms.as_deref_mut());
        crate::pickups_hook::set_net_mode(mp.s.started && mp.s.ended.is_none());
        if mp.s.ended.is_some() {
            for (_, g) in std::mem::take(&mut mp.gfx) { mp.pool.push((g.body, g.head, g.pick)); }
            mp.s.remotes.clear();
            mp.targets.clear();
            ACTIVE.store(false, Ordering::Relaxed);
            for b in mp.s.take_outbox() { send(&b); }
            return;
        }
        if !mp.s.started { for b in mp.s.take_outbox() { send(&b); } return; }
        if !mp.spawned {
            mp.spawned = true;
            let (at, yaw) = spawn_point(sim, mp.s.slot, mp.s.names.keys().copied().max().map_or(2, |m| m as usize + 1));
            teleport(sim, at);
            sim.yaw = yaw;
            sim.pitch = 0.;
        }
        let now = mp.now_ms();
        if let Some(a) = arms.as_deref() { if !mp.s.dead { mp.s.report_local_vitals(a.health, a.shield); } }
        if let (true, Some(a)) = (mp.s.dead, arms.as_deref_mut()) { a.health = 0.; a.shield = 0.; }
        sim.dbno = mp.s.dbno && !mp.s.dead;
        sim.anim.set_dbno(sim.dbno);
        { let c = std::mem::take(&mut sim.anim.dbno_crawl); crate::dbno_hook::crawl_sounds(&c, sim.feet()); }
        if mp.s.tick_send(dt) && !mp.s.dead { let st = local_state(sim, arms.as_deref(), scene.show_pickaxe && !sim.dbno); mp.s.send_state(st, now); }
        revive_frame(mp, sim);
        spectate_frame(mp, sim, dt);
        if mp.s.host { let storm = storm_of(sim); mp.s.host_tick(dt, Some(storm)); }
        if crate::building_hook::with_catalog(|c| c.is_some()) { sync_builds(mp); }
        melee(mp, sim);
        for c in crate::pickups_hook::take_claims() { mp.s.send_pickup_claim(c); }
        for d in crate::pickups_hook::take_net_drops() { if !mp.s.dead { mp.s.send_pickup_spawn(proto::pickup_flag::DROPPED, &d.item, d.count, d.start, d.end); } }
        for (op, item) in crate::emote_hook::take_net() { if !mp.s.dead { mp.s.send_emote(op, item); } }
        draw_remotes(mp, scene, arms.as_deref(), dt);
        for b in mp.s.take_outbox() { send(&b); }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn held_index_roundtrips() {
        let k = CORE_ANIMS_MALE[0].0;
        assert_eq!(held_core(held_index(Some(k))), Some(k));
        assert_eq!(held_index(None), 0);
        assert_eq!(held_core(0), None);
    }
    #[test] fn inactive_costs_nothing() {
        assert!(!active());
        assert!(pawn_targets().is_empty());
        on_cartridge("x", Vec3::ZERO, Vec3::X, &[]);
    }
    #[test] fn gun_bits_roundtrip() {
        let f = flag::with_gun(flag::CROUCHED | flag::RUNNING, Some(3));
        assert_eq!(flag::gun(f), Some(3));
        assert_eq!(f & 3, 3);
        assert_eq!(flag::gun(flag::with_gun(f, None)), None);
    }
    #[test] fn relay_damage_caps_match_every_br_stat_row() {
        let root = std::env::var("FN_ROOT").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        let Ok(paks) = fn_pak::PakSet::open_install(&root) else { eprintln!("no install: skipped"); return };
        let src = fn_pose::ClipSourceDir::Pak(&paks);
        let guns: Vec<fn_hitreg::GunHit> = fn_hitreg::br_weapon_wids(&paks).iter().filter_map(|w| fn_hitreg::load(&src, w).ok()).filter(|g| g.stat_table.ends_with("AthenaRangedWeapons")).collect();
        assert!(guns.len() >= 40, "{}", guns.len());
        let ts = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/web/fn-mp-worker/src/caps.ts")).unwrap();
        for g in &guns {
            let s = &g.stats;
            let cap = fn_net::caps::WEAPONS.iter().find(|c| c.row == g.row).unwrap_or_else(|| panic!("{}: no relay cap", g.row));
            let pellets = s.bullets_per_cartridge.max(1) as f32;
            let peak = s.dmg.iter().copied().fold(0f32, f32::max) * s.dmg_scale.max(1.);
            let crit = s.zone_critical.max(1.);
            let want = (peak * pellets * crit * 100.).ceil() / 100.;
            assert!((cap.max_shot - want).abs() < 0.011, "{}: cap {} vs peak Dmg x pellets x DamageZone_Critical {}", g.row, cap.max_shot, want);
            assert_eq!(cap.pellets as f32, pellets, "{}", g.row);
            let head_pb = s.damage_at(fn_hitreg::DamageKind::Combat, 0., 1) * crit * pellets;
            assert!(head_pb <= cap.max_shot + 0.01, "{}: point-blank headshot {} exceeds the relay cap {}", g.row, head_pb, cap.max_shot);
            let line = format!("{{\"row\":\"{}\",\"maxShot\":", g.row);
            let at = ts.find(&line).unwrap_or_else(|| panic!("{}: not in caps.ts", g.row));
            let v: f32 = ts[at + line.len()..].split(',').next().unwrap().parse().unwrap();
            assert!((v - cap.max_shot).abs() < 1e-3, "{}: caps.ts {} vs caps.rs {}", g.row, v, cap.max_shot);
            eprintln!("{:48} DmgPB {:6.2} x{} pellets x{} crit -> cap {:7.2}", g.row, s.dmg[0], pellets, crit, cap.max_shot);
        }
    }
    #[test] fn pickaxe_damage_and_cap_match_the_melee_stat_row() {
        let root = std::env::var("FN_ROOT").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        let Ok(paks) = fn_pak::PakSet::open_install(&root) else { eprintln!("no install: skipped"); return };
        let row = fn_pose::loadout_stats::data_table_row(&fn_pose::ClipSourceDir::Pak(&paks), "/Game/Athena/Items/Weapons/AthenaMeleeWeapons", "Harvest_Pickaxe_Athena_C_T01").unwrap();
        let f = |k: &str| row.get(k).and_then(|v| v.as_f64()).unwrap_or_else(|| panic!("no {k}")) as f32;
        eprintln!("pickaxe row: DmgPB {} DmgMid {} DmgLong {} DmgMaxRange {} RngPB {} RngMax {} DamageZone_Critical {} EnvDmgPB {}", f("DmgPB"), f("DmgMid"), f("DmgLong"), f("DmgMaxRange"), f("RngPB"), f("RngMax"), f("DamageZone_Critical"), f("EnvDmgPB"));
        assert_eq!(f("DmgPB"), PICKAXE_DAMAGE);
        assert_eq!(fn_net::caps::WEAPONS[0].max_shot, PICKAXE_DAMAGE * f("DamageZone_Critical").max(1.));
    }
    #[test] fn ue_bone_space_maps_like_the_mesh_import() {
        let p = ue_bone_to_mesh().transform_point3(Vec3::new(100., 200., 300.));
        assert_eq!(p.to_array(), fn_mesh::gltf::ue_to_gltf_position([100., 200., 300.]));
    }
}
