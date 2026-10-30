use crate::arms::Arms;
use crate::scene::Scene;
use fn_animgraph::{AnimState, PawnSample, CORE_ANIMS_MALE};
use fn_audio_sink::SoundEvent;
use fn_bots::brain::{Agent, Body, Bus, Loot, Poi, Sound, Storm, World};
use fn_bots::skill::WeaponClass;
use fn_bots::{Bot, Phoebes};
use fn_game::{GameSim, EYE, Z_OFFSET};
use fn_hitreg::shoot::{BodySetup, FireContext, HitBody, PawnFire, PawnHit, PawnTarget, Shot, Vitals};
use fn_hitreg::{GunHit, SpreadState};
use fn_movement::controller::Controller;
use fn_pose::CharacterPose;
use fn_render::ModelId;
use fn_ui::screens::kill_feed::{Combatant, DeathCause, Elimination, Killer};
use glam::{Mat4, Quat, Vec2, Vec3};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, OnceLock};

pub const BOT_ID_BASE: u64 = 1000;
pub const PLAYER_ID: u64 = 999;
const SLOT_BASE: usize = 100;
const SPAWNS_PER_FRAME: usize = 3;
const DRAW_M: f32 = 320.;
const FULL_MOVE_M: f32 = 110.;
const PICKAXE_DAMAGE: f32 = 20.;
const PATHS_PER_FRAME: u32 = 2;
const PATH_LEG_M: f32 = 220.;
const PHYSICS_ASSET: &str = "/Game/Characters/Player/Male/Male_Avg_Base/Fortnite_M_Avg_Player_Physics";

pub fn is_bot(id: u64) -> bool { (BOT_ID_BASE..BOT_ID_BASE + 256).contains(&id) }
static LOBBY_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub fn set_lobby_count(n: usize) { LOBBY_COUNT.store(n.min(99), std::sync::atomic::Ordering::Relaxed); }
pub fn wanted() -> usize { static V: OnceLock<Option<usize>> = OnceLock::new(); V.get_or_init(|| fn_live::var("FN_BOTS").ok().and_then(|v| v.trim().parse::<usize>().ok()).map(|n| n.min(99))).unwrap_or_else(|| LOBBY_COUNT.load(std::sync::atomic::Ordering::Relaxed)) }

static ROOTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static ROSTER: Mutex<u32> = Mutex::new(0);
static NUMBERS: Mutex<Vec<(Vec3, u32, bool, u8)>> = Mutex::new(Vec::new());
static HITS_IN: Mutex<Vec<(PawnHit, String)>> = Mutex::new(Vec::new());
static BOT_BLAST_IN: Mutex<Vec<(u64, Vec3, PawnHit, String)>> = Mutex::new(Vec::new());
static BODIES: OnceLock<Vec<BodySetup>> = OnceLock::new();
static REPORT: Mutex<String> = Mutex::new(String::new());
static GUN_WANTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static GUN_FAILED: Mutex<Vec<String>> = Mutex::new(Vec::new());
pub fn take_gun_wants() -> Vec<String> { GUN_WANTS.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default() }
pub fn gun_failed(item: &str) { if let Ok(mut v) = GUN_FAILED.lock() { v.push(item.to_owned()); } }

pub fn init(roots: &[PathBuf]) { if let Ok(mut r) = ROOTS.lock() { *r = roots.to_vec(); } }
pub fn roster() -> u32 { ROSTER.lock().map_or(0, |r| *r) }
pub fn take_numbers() -> Vec<(Vec3, u32, bool, u8)> { NUMBERS.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default() }
pub fn pawn_targets() -> Vec<PawnTarget> {
    ST.with(|cell| {
        let Ok(b) = cell.try_borrow() else { return vec![] };
        let Some(st) = b.as_ref() else { return vec![] };
        st.pawns.iter().filter(|p| p.bot.core.alive && p.bot.core.body != Body::Bus).map(target_of).collect()
    })
}

fn target_of(p: &Pawn) -> PawnTarget {
    let mut t = PawnTarget::new(p.bot.core.id, p.team, p.bot.core.feet, Vitals::new(p.bot.core.health.max(0.01), p.bot.core.shield));
    t.crouched = p.ctl.crouched;
    t.bodies = if p.bodies.is_empty() { t.coarse_bodies() } else { p.bodies.clone() };
    t
}
pub fn on_bot_blast_hits(instigator: u64, from: Vec3, row: &str, hits: &[PawnHit]) { if let Ok(mut q) = BOT_BLAST_IN.lock() { q.extend(hits.iter().filter(|h| is_bot(h.id)).map(|h| (instigator, from, h.clone(), row.to_owned()))); } }
pub fn killer_of(id: u64) -> Option<Killer> {
    ST.with(|cell| { let b = cell.try_borrow().ok()?; let st = b.as_ref()?; st.pawns.iter().find(|p| p.bot.core.id == id).map(|p| Killer::Player(Combatant::other(&p.bot.core.name))) })
}
pub fn on_player_hits(row: &str, hits: &[PawnHit]) { if let Ok(mut q) = HITS_IN.lock() { q.extend(hits.iter().filter(|h| is_bot(h.id)).map(|h| (h.clone(), row.to_owned()))); } }
pub fn report() -> String { REPORT.lock().map(|r| r.clone()).unwrap_or_default() }

struct Gfx { anim: AnimState, pose: CharacterPose, body: ModelId, head: ModelId, pick: ModelId, logged: bool, outfit: usize }

struct Outfit { rig: fn_pose::CharacterRig, body: ModelId, head: ModelId, cid: String, female: bool }

struct BotClips<'a> { base: &'a dyn fn_animgraph::ClipLibrary, extra: Option<&'a fn_pose::ClipStore> }
impl<'a> fn_animgraph::ClipLibrary for BotClips<'a> {
    fn clip(&self, name: &str) -> Option<&dyn fn_animgraph::ClipSource> { self.extra.and_then(|e| fn_animgraph::ClipLibrary::clip(e, name)).or_else(|| self.base.clip(name)) }
}

const OUTFITS: usize = 8;
const POSE_BUDGET: usize = 6;
const POSE_MS: f32 = 0.25;
const FIRE_BUDGET: usize = 6;
const NEW_GFX_PER_FRAME: usize = 3;

struct Pawn {
    bot: Bot,
    ctl: Controller,
    idx: usize,
    team: u32,
    hulls_t: f32,
    think_acc: f32,
    gfx: Option<Gfx>,
    death_t: Option<f32>,
    react: Option<(u32, f32)>,
    fire: Option<(usize, f32, bool)>,
    emote: crate::emote_hook::Remote,
    bodies: Vec<HitBody>,
    storm_t: f32,
    seed: u64,
    killer: Option<u64>,
    last_pos: Vec3,
    emote_on: Option<u32>,
    hull_cache: fn_props::HullMapCache,
    pose_n: u32,
    anim_dt: f32,
    step_acc: f32,
    bodies_at: Vec3,
    inv: Vec<(String, u32)>,
    drop_decided: bool,
}

struct State {
    rx: Option<Receiver<Result<Phoebes, String>>>,
    nav_rx: Option<Receiver<Option<fn_nav::NavMesh>>>,
    nav_raw: Option<fn_nav::NavMesh>,
    ph: Option<Arc<Phoebes>>,
    failed: bool,
    pawns: Vec<Pawn>,
    mode: Option<u8>,
    pois: Vec<Poi>,
    pool: Vec<(usize, (ModelId, ModelId, ModelId))>,
    outfits: Vec<Outfit>,
    outfit_rx: Option<OutfitRx>,
    female_clips: Option<Arc<fn_pose::ClipStore>>,
    outfit_pending: Vec<(String, crate::br_character::BrModel, bool)>,
    fire_queue: std::collections::VecDeque<(usize, fn_bots::FireRequest)>,
    frame_no: u64,
    guns: HashMap<usize, GunHit>,
    sounds: Vec<Sound>,
    rng: u64,
    clock: f32,
    prof_ms: f32,
    prof_n: u32,
    log_t: f32,
    nav: Option<Option<fn_nav::NavMesh>>,
    build_cursor: Option<u64>,
    obstacles: HashMap<fn_building::world::PieceId, u32>,
    won_set: bool,
    prof: [f32; 7],
    last_ms: [f32; 6],
    slow_at: Option<std::time::Instant>,
    spec: Option<(usize, f32, f32)>,
    requested: HashMap<String, f32>,
    pending_guns: Vec<(usize, String, f32)>,
    drops: Vec<(Vec3, Vec<(String, u32)>)>,
    plan: DropPlan,
}

#[derive(Default)]
struct DropPlan { was_on_bus: bool, jump_t: Option<f32>, prev_feet: Option<Vec3>, vel: Vec3, center: Option<Vec3>, assigned_at: Option<Vec3>, next_t: f32, landed_t: Option<f32>, spots: Vec<Vec3>, logged: u8, followers: usize }

thread_local! { static ST: RefCell<Option<State>> = const { RefCell::new(None) }; }

type OutfitList = Vec<(String, crate::br_character::BrModel, bool)>;
type OutfitRx = Receiver<(Option<(String, crate::br_character::BrModel, bool)>, Option<Arc<fn_pose::ClipStore>>)>;

#[derive(Default)]
struct Prep { rx: Option<OutfitRx>, female_clips: Option<Arc<fn_pose::ClipStore>>, pending: OutfitList, outfits: Vec<Outfit>, started: Option<std::time::Instant> }

thread_local! { static PREP: RefCell<Prep> = RefCell::new(Prep::default()); }

pub fn reset() {
    ST.with(|c| *c.borrow_mut() = None);
    PREP.with(|c| *c.borrow_mut() = Prep::default());
    if let Ok(mut r) = ROSTER.lock() { *r = 0; }
}

fn outfits_on() -> bool { crate::hitreg_hook::install_dir().is_some() && fn_live::var("FN_BOTS_OUTFITS").map_or(true, |v| v != "0") }

fn outfit_step(rx: &mut Option<OutfitRx>, female_clips: &mut Option<Arc<fn_pose::ClipStore>>, pending: &mut OutfitList, outfits: &mut Vec<Outfit>, scene: &mut Scene, seed: u64) -> bool {
    if rx.is_none() && outfits.is_empty() && outfits_on() { *rx = Some(start_outfits(seed)); }
    let mut finished = rx.is_none();
    if let Some(r) = rx.as_ref() {
        match r.try_recv() {
            Ok((Some(item), fclips)) => { if fclips.is_some() { *female_clips = fclips; } pending.insert(0, item) }
            Ok((None, fclips)) => { if fclips.is_some() { *female_clips = fclips; } finished = true; }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => finished = true,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    if finished && pending.is_empty() { if rx.is_some() { *rx = Some(channel().1); } return true; }
    let Some((cid, m, female)) = pending.pop() else { return false };
    let (b0, h0, _) = scene.pawn_models();
    let pal = |i: usize| m.rig_parts.get(i).map(|p| p.palette_len()).unwrap_or(1);
    let b = scene.renderer.model_builder();
    let (bb, hb) = (b.build(&m.body, pal(0), "bot body"), b.build(&m.head, pal(1), "bot head"));
    let (body, head) = (scene.renderer.instance_model(b0), scene.renderer.instance_model(h0));
    scene.renderer.replace_model(body, bb);
    scene.renderer.replace_model(head, hb);
    let rig = fn_pose::CharacterRig { parts: m.rig_parts, sockets: scene.character.rig.sockets.clone(), opts: scene.character.rig.opts };
    eprintln!("[bots] outfit {cid} ready");
    outfits.push(Outfit { rig, body, head, cid, female });
    false
}

pub fn prewarm(scene: &mut Scene) -> bool {
    if wanted() == 0 || !outfits_on() { return true; }
    PREP.with(|c| {
        let mut p = c.borrow_mut();
        let p = &mut *p;
        let t0 = *p.started.get_or_insert_with(std::time::Instant::now);
        let seed = hash(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(11, |d| d.as_nanos() as u64));
        let done = outfit_step(&mut p.rx, &mut p.female_clips, &mut p.pending, &mut p.outfits, scene, seed);
        done || t0.elapsed().as_secs_f32() > 15.
    })
}

fn hash(mut x: u64) -> u64 { x ^= x >> 33; x = x.wrapping_mul(0xff51afd7ed558ccd); x ^= x >> 33; x = x.wrapping_mul(0xc4ceb9fe1a85ec53); x ^ (x >> 33) }
fn rand01(s: &mut u64) -> f32 { *s = hash(s.wrapping_add(0x9e3779b97f4a7c15)); (*s >> 40) as f32 / (1u64 << 24) as f32 }

fn start_load() -> Receiver<Result<Phoebes, String>> {
    let (tx, rx) = channel();
    let roots = ROOTS.lock().map(|r| r.clone()).unwrap_or_default();
    std::thread::spawn(move || {
        let dir = fn_live::var_os("FN_BOTS_DIR").map(PathBuf::from).filter(|d| d.join(fn_bots::data::FILES[0]).is_file()).or_else(|| roots.iter().map(|r| r.join("bots")).find(|d| d.join(fn_bots::data::FILES[0]).is_file()));
        let r = match dir {
            Some(d) => fn_bots::Phoebe::load_dir(&d).and_then(Phoebes::new),
            None => Err("no `bots` folder (phoebe_*.json) in the asset roots".into()),
        };
        let _ = tx.send(r);
    });
    if BODIES.get().is_none() {
        std::thread::spawn(|| {
            let Some(dir) = crate::hitreg_hook::install_dir() else { let _ = BODIES.set(vec![]); return };
            let set = fn_pak::PakSet::open_install(&dir).ok().and_then(|paks| fn_hitreg::shoot::load_body_setups(&fn_pose::ClipSourceDir::Pak(&paks), PHYSICS_ASSET).ok());
            let _ = BODIES.set(set.unwrap_or_default());
        });
    }
    rx
}

fn start_nav() -> Receiver<Option<fn_nav::NavMesh>> {
    let (tx, rx) = channel();
    let roots = ROOTS.lock().map(|r| r.clone()).unwrap_or_default();
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let m = roots.iter().find_map(|r| fn_nav::NavMesh::load_athena_in(r)).or_else(fn_nav::NavMesh::load_athena);
        eprintln!("[bots] navmesh load {} in {:.0} ms", if m.is_some() { "ok" } else { "failed" }, t.elapsed().as_secs_f32() * 1000.);
        let _ = tx.send(m);
    });
    rx
}

fn build_pois() -> Vec<Poi> {
    let Some(set) = crate::poi::current() else { return vec![] };
    let mut by_file: HashMap<usize, (Vec3, f32, f32)> = HashMap::new();
    for g in &set.groups {
        let e = by_file.entry(g.file).or_insert((Vec3::ZERO, 0., 0.));
        let w = g.placements.len().max(1) as f32;
        e.0 += g.center * w; e.1 += w; e.2 = e.2.max(g.radius);
    }
    let mut out = vec![];
    for (fi, (sum, w, r)) in by_file {
        let name = set.files.get(fi).cloned().unwrap_or_default();
        let n = name.to_ascii_lowercase();
        if ["lobby", "terrain", "ls_", "bridge", "tunnel"].iter().any(|x| n.contains(x)) || w < 40. { continue; }
        let c = sum / w;
        let g = Vec3::new(c.x, c.z, c.y) * 0.01;
        out.push(Poi { center: g, radius: (r * 0.01 * 0.35).clamp(25., 70.), name });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

struct View<'a> {
    sim: &'a GameSim,
    now: f32,
    phase: (i32, i32),
    agents: &'a [Agent],
    pois: &'a [Poi],
    occupancy: &'a [u32],
    sounds: &'a [Sound],
    storm: Option<Storm>,
    bus: Option<Bus>,
    nav: Option<&'a fn_nav::NavMesh>,
    los_used: Cell<u32>,
    paths_used: Cell<u32>,
    loot_buf: RefCell<Vec<(u64, Vec3, String, bool)>>,
    chest_buf: RefCell<Vec<(u64, Vec3)>>,
}

impl<'a> World for View<'a> {
    fn now(&self) -> f32 { self.now }
    fn game_phase(&self) -> (i32, i32) { self.phase }
    fn agents(&self) -> &[Agent] { self.agents }
    fn los(&self, from: Vec3, to: Vec3) -> bool {
        self.los_used.set(self.los_used.get() + 1);
        let d = to - from;
        let len = d.length();
        if len < 0.5 { return true; }
        let steps = ((len / 3.).ceil() as usize).clamp(1, 120);
        for i in 1..steps {
            let p = from + d * (i as f32 / steps as f32);
            if self.sim.ground_y(p.x, p.z) > p.y + 0.2 { return false; }
        }
        fn_props::trace_in(&self.sim.world.props, self.sim.fresh_prop_index(), from, to, 0.05).is_none()
    }
    fn pois(&self) -> &[Poi] { self.pois }
    fn loot_near(&self, p: Vec3, r: f32, out: &mut Vec<Loot>) {
        let mut b = self.loot_buf.borrow_mut();
        b.clear();
        crate::pickups_hook::bot_list(p, r, &mut b);
        for (id, pos, _, weapon) in b.iter() { out.push(Loot { id: *id, pos: *pos, kind: 0, weapon: *weapon }); }
        let mut c = self.chest_buf.borrow_mut();
        c.clear();
        crate::chest_hook::bot_list(p, r, &mut c);
        for (id, pos) in c.iter() { out.push(Loot { id: *id, pos: *pos, kind: 1, weapon: true }); }
    }
    fn storm(&self) -> Option<Storm> { self.storm }
    fn bus(&self) -> Option<Bus> { self.bus }
    fn sounds(&self) -> &[Sound] { self.sounds }
    fn poi_occupancy(&self, poi: usize) -> u32 { self.occupancy.get(poi).copied().unwrap_or(0) }
    fn find_path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        let nav = self.nav?;
        if self.paths_used.get() >= PATHS_PER_FRAME { return None; }
        self.paths_used.set(self.paths_used.get() + 1);
        let d = to - from;
        let flat = Vec2::new(d.x, d.z).length();
        let goal = if flat > PATH_LEG_M {
            let p = from + Vec3::new(d.x, 0., d.z) * (PATH_LEG_M / flat);
            let p = Vec3::new(p.x, self.sim.ground_y(p.x, p.z), p.z);
            nav.project(p, Vec3::new(20., 30., 20.)).unwrap_or(p)
        } else { to };
        let r = nav.find_path_ex(from, goal)?.points;
        if r.len() < 2 { return None; }
        Some(r.into_iter().map(|w| Vec3::new(w.x, w.y - 0.2, w.z)).collect())
    }
    fn random_point(&self, center: Vec3, r: f32, seed: u64) -> Option<Vec3> {
        if let Some(n) = self.nav { if let Some(p) = n.random_point_in_radius(center, r, seed) { return Some(p); } }
        let mut s = seed;
        let a = rand01(&mut s) * std::f32::consts::TAU;
        let k = rand01(&mut s).sqrt() * r;
        let x = center.x + a.cos() * k;
        let z = center.z + a.sin() * k;
        Some(Vec3::new(x, self.sim.ground_y(x, z), z))
    }
}

fn phase_of(ph: &Phoebes, sim: &GameSim, storm: &crate::storm_hook::StormMatch) -> (i32, i32) {
    use fn_game::drop_in::{GamePhase as G, GamePhaseStep as S};
    let e = |en: &str, n: &str| ph.data.enum_value(en, n);
    let Some(d) = sim.drop.as_ref() else { return (e("EAthenaGamePhase", "SafeZones"), e("EAthenaGamePhaseStep", "StormHolding")) };
    let p = match d.phase { G::Warmup => "Warmup", G::Aircraft => "Aircraft", G::SafeZones => "SafeZones" };
    let (s, _) = d.game_phase_step();
    let s = match s { S::None => match storm.bot_zone() { Some(z) if z.shrinking => "StormShrinking", Some(_) => "StormHolding", None => "None" }, S::Warmup => "Warmup", S::GetReady => "GetReady", S::BusLocked => "BusLocked", S::BusFlying => "BusFlying", S::StormForming => "StormForming" };
    (e("EAthenaGamePhase", p), e("EAthenaGamePhaseStep", s))
}

fn sandbox_ring() -> (f32, f32, f32) {
    static V: OnceLock<(f32, f32, f32)> = OnceLock::new();
    *V.get_or_init(|| fn_live::var("FN_BOTS_RING").ok().and_then(|v| { let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect(); (n.len() >= 2).then(|| (n[0], n[1], n.get(2).copied().unwrap_or(0.))) }).unwrap_or((15., 45., 0.)))
}

fn island_feet(sim: &GameSim, i: usize) -> Option<(Vec3, f32)> {
    let d = sim.drop.as_ref()?;
    let s = fn_game::drop_in::WARMUP_STARTS[i % fn_game::drop_in::WARMUP_STARTS.len()];
    let o = Vec3::from(fn_game::drop_in::SPAWN_ISLAND_LOCATION);
    let feet = d.ue_to_game(Vec3::new(s[0] + o.x, s[1] + o.y, s[2] + o.z - fn_game::drop_in::PAWN_HALF_HEIGHT));
    Some((feet, (s[3] + 90.).rem_euclid(360.)))
}

fn new_controller(sim: &GameSim, feet: Vec3, yaw: f32) -> Controller {
    let mut c = sim.new_controller();
    c.terrain = sim.terrain().map(|w| fn_terrain::Local::new(w, Z_OFFSET));
    c.position = Vec2::new(feet.x, feet.z - Z_OFFSET);
    c.height = feet.y;
    c.facing = yaw;
    c
}

fn feet_of(c: &Controller) -> Vec3 { let p = c.render_position(); Vec3::new(p.x, c.height, p.y + Z_OFFSET) }

fn class_cause(c: WeaponClass) -> DeathCause {
    match c { WeaponClass::Pistol => DeathCause::Pistol, WeaponClass::Shotgun => DeathCause::Shotgun, WeaponClass::Smg | WeaponClass::Minigun => DeathCause::Smg, WeaponClass::Sniper => DeathCause::Sniper, WeaponClass::Melee => DeathCause::Melee, WeaponClass::RocketLauncher | WeaponClass::GrenadeLauncher => DeathCause::Heavy, WeaponClass::Rifle => DeathCause::Rifle }
}

fn weapon_from(arms: &Arms, g: usize) -> Option<fn_bots::Weapon> {
    let gd = arms.data.guns.get(g)?;
    let d = &gd.def;
    let class = WeaponClass::of_row(&d.item);
    let rate = if d.firing_rate > 0. { d.firing_rate } else { 2. };
    let range = match class { WeaponClass::Shotgun => 12., WeaponClass::Smg => 25., WeaponClass::Pistol => 30., WeaponClass::Sniper => 150., WeaponClass::RocketLauncher | WeaponClass::GrenadeLauncher => 45., _ => 60. };
    Some(fn_bots::Weapon { row: format!("#{g}:{}", d.item), class, clip: d.clip_size, ammo: d.clip_size, fire_interval: 1. / rate, reload_time: d.reload_time.max(0.5), range_m: range })
}

fn gun_index(w: &fn_bots::Weapon) -> Option<usize> { w.row.strip_prefix('#')?.split(':').next()?.parse().ok() }

fn short_of(item: &str) -> &str { item.rsplit('/').next().unwrap_or(item).split('.').next().unwrap_or(item) }

fn find_gun(arms: &Arms, item: &str) -> Option<usize> { let short = short_of(item); arms.data.guns.iter().position(|g| short_of(&g.def.item).eq_ignore_ascii_case(short)) }

fn pick_gun(arms: &Arms, item: &str, seed: &mut u64) -> Option<usize> {
    let short = short_of(item);
    if let Some(i) = find_gun(arms, item) { return Some(i); }
    let want = WeaponClass::of_row(short);
    let same: Vec<usize> = arms.data.guns.iter().enumerate().filter(|(_, g)| WeaponClass::of_row(&g.def.item) == want).map(|(i, _)| i).collect();
    let pool: Vec<usize> = if same.is_empty() { (0..arms.data.guns.len()).collect() } else { same };
    if pool.is_empty() { return None; }
    Some(pool[(rand01(seed) * pool.len() as f32) as usize % pool.len()])
}

pub fn frame(sim: &mut GameSim, scene: &mut Scene, arms: Option<&mut Arms>, storm: &crate::storm_hook::StormMatch, dt: f32) -> Vec<SoundEvent> {
    let n = wanted();
    if n == 0 || crate::mp_hook::in_match() { return vec![]; }
    let t0 = std::time::Instant::now();
    let mut out = vec![];
    ST.with(|cell| {
        let mut b = cell.borrow_mut();
        let st = b.get_or_insert_with(|| State { rx: Some(start_load()), nav_rx: Some(start_nav()), nav_raw: None, ph: None, failed: false, pawns: vec![], mode: None, pois: vec![], pool: vec![], guns: HashMap::new(), sounds: vec![], rng: hash(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| d.as_nanos() as u64)), clock: 0., prof_ms: 0., prof_n: 0, log_t: 0., nav: None, won_set: false, prof: [0.; 7], last_ms: [0.; 6], slow_at: None, spec: None, outfits: vec![], outfit_rx: None, female_clips: None, outfit_pending: vec![], fire_queue: Default::default(), frame_no: 0, requested: HashMap::new(), pending_guns: vec![], drops: vec![], build_cursor: None, obstacles: HashMap::new(), plan: DropPlan::default() });
        if st.ph.is_none() {
            if let Some(rx) = &st.rx {
                if let Ok(r) = rx.try_recv() {
                    st.rx = None;
                    match r {
                        Ok(p) => { eprintln!("[bots] Phoebe data ready: {} BT nodes, {} decorators/services, {} BB keys", p.graph.nodes.len(), p.graph.aux.len(), p.graph.schema.keys.len()); st.ph = Some(Arc::new(p)); }
                        Err(e) => { eprintln!("[bots] unavailable: {e}"); st.failed = true; }
                    }
                }
            }
        }
        let Some(ph) = st.ph.clone() else { return };
        let Some(arms) = arms else { return };
        if let Some(rx) = &st.nav_rx { if let Ok(m) = rx.try_recv() { st.nav_rx = None; st.nav_raw = m; if st.nav_raw.is_none() { st.nav = Some(None); eprintln!("[bots] navmesh unavailable: direct movement with collision sweeps"); } } }
        if st.nav.is_none() && st.nav_raw.is_some() { if let Some(w) = fn_terrain::world() { let m = st.nav_raw.take().unwrap().with_game_mapping(w.map); eprintln!("[bots] navmesh: fn-nav Athena mesh, {} tiles, {} polys", m.tile_count(), m.poly_count()); st.nav = Some(Some(m)); } }
        if st.pois.is_empty() && (st.clock * 0.5).fract() < dt * 0.5 { st.pois = build_pois(); if !st.pois.is_empty() { for p in &mut st.pois { p.center.y = sim.ground_y(p.center.x, p.center.z); } eprintln!("[bots] {} landing POIs from the POI sublevels", st.pois.len()); } }
        sync_obstacles(st);
        let mode = match sim.drop.as_ref() { None => 0u8, Some(d) if d.phase == fn_game::drop_in::GamePhase::Warmup => 1, Some(_) => 2 };
        let restart = st.mode.map_or(false, |m| m != mode && !(m == 1 && mode == 2));
        if restart {
            for p in st.pawns.drain(..) { if let Some(g) = p.gfx { st.pool.push((g.outfit, (g.body, g.head, g.pick))); } }
            st.won_set = false;
            st.pending_guns.clear();
            st.drops.clear();
            st.obstacles.clear();
            st.plan = DropPlan::default();
            st.build_cursor = None;
            if let Some(Some(n)) = st.nav.as_mut() { n.clear_obstacles(); }
            if let Ok(mut r) = ROSTER.lock() { *r = 0; }
            eprintln!("[bots] new {} with {n} bots", ["sandbox", "match on the spawn island", "match on the bus"][mode as usize]);
        }
        st.mode = Some(mode);
        let now = sim.drop.as_ref().map_or(st.clock + dt, |d| d.now);
        st.clock = now;
        for _ in 0..SPAWNS_PER_FRAME {
            let i = st.pawns.len();
            if i >= n { break; }
            let seed = hash(st.rng ^ (i as u64 + 1) * 0x51ed);
            let id = BOT_ID_BASE + i as u64;
            let mut bot = ph.spawn(id, i as u32 + 1, format!("Bot{}", i + 1), seed);
            let (feet, yaw, body) = match mode {
                1 => { let (f, y) = island_feet(sim, (i * 7 + 3 + (seed % 11) as usize) % 110).unwrap_or((sim.feet(), 0.)); (f, y, Body::Island) }
                2 => { let p = sim.drop.as_ref().and_then(|d| d.aircraft_game(d.now)).map_or(sim.feet(), |b| b.0); (p, 0., if sim.drop.as_ref().map_or(false, |d| d.aircraft.is_some()) { Body::Bus } else { Body::Ground }) }
                _ => { let mut s = seed; let (r0, r1, arc) = sandbox_ring(); let fw = sim.cam_view.forward; let a = if arc > 0. { fw.z.atan2(fw.x) + (rand01(&mut s) - 0.5) * arc.to_radians() } else { rand01(&mut s) * std::f32::consts::TAU }; let r = r0 + rand01(&mut s) * (r1 - r0); let f = sim.feet(); let (x, z) = (f.x + a.cos() * r, f.z + a.sin() * r); (Vec3::new(x, sim.floor_below(Vec3::new(x, f.y.max(sim.ground_y(x, z)) + 3., z)), z), rand01(&mut s) * 360., Body::Ground) }
            };
            bot.core.feet = feet; bot.core.yaw = yaw; bot.core.body = body; bot.core.eye_h = EYE;
            if body == Body::Ground { bot.core.stats.jumped_at = Some(0.); }
            bot.core.weapons.push(fn_bots::Weapon { row: "WID_Harvest_Pickaxe_Athena_C_T01".into(), class: WeaponClass::Melee, clip: 0, ammo: 0, fire_interval: 0.55, reload_time: 0., range_m: 2. });
            bot.core.equipped = Some(0);
            let ctl = new_controller(sim, feet, yaw);
            st.pawns.push(Pawn { inv: vec![], bot, ctl, idx: i, team: i as u32 + 1, hulls_t: 0., think_acc: 0., gfx: None, death_t: None, react: None, fire: None, emote: Default::default(), bodies: vec![], storm_t: 0., seed, killer: None, last_pos: feet, emote_on: None, hull_cache: Default::default(), pose_n: 0, anim_dt: 0., step_acc: 0., bodies_at: feet, drop_decided: false });
            if let Ok(mut r) = ROSTER.lock() { *r = st.pawns.len() as u32; }
            match fn_live::var("FN_BOTS_GUN").ok().filter(|v| v.starts_with("WID_")) { Some(w) => give_gun_item(st, arms, i, &w), None => if mode == 0 { give_gun_item(st, arms, i, "WID_Assault_Auto_Athena_C_Ore_T02"); } }
        }
        let player_feet = sim.feet();
        let player_alive = arms.health > 0.;
        let bus = sim.drop.as_ref().and_then(|d| { let a = d.aircraft.as_ref()?; let (pos, yaw) = d.aircraft_game(d.now)?; let y = yaw.to_radians(); Some(Bus { pos, dir: Vec3::new(y.sin(), 0., -y.cos()), can_jump: !a.locked, time_left: a.drop_end_time() - d.now }) });
        if let Some(d) = sim.drop.as_ref() {
            if d.phase != fn_game::drop_in::GamePhase::Warmup {
                for p in st.pawns.iter_mut() {
                    if p.bot.core.body == Body::Island && p.bot.core.alive { p.bot.core.body = if bus.is_some() { Body::Bus } else { Body::Ground }; p.bot.runner = fn_bots::bt::Runner::new(&ph.graph); }
                }
            }
        }
        drop_plan(st, sim, now, dt);
        let storm_v = storm.bot_zone().map(|z| Storm { center: z.center, radius: z.radius, next_center: z.next_center, next_radius: z.next_radius, state: if z.shrinking { 3 } else { 2 }, phase: z.phase as i32 });
        let mut agents: Vec<Agent> = Vec::with_capacity(st.pawns.len() + 1);
        agents.push(Agent { id: PLAYER_ID, team: 0, feet: player_feet, vel: Vec3::ZERO, alive: player_alive, knocked: false, is_bot: false, airborne: sim.controller.airborne(), hidden: sim.on_bus(), health: arms.health, shield: arms.shield });
        for p in &st.pawns { agents.push(Agent { id: p.bot.core.id, team: p.team, feet: p.bot.core.feet, vel: p.bot.core.vel, alive: p.bot.core.alive, knocked: false, is_bot: true, airborne: p.ctl.airborne(), hidden: p.bot.core.body == Body::Bus, health: p.bot.core.health, shield: p.bot.core.shield }); }
        let mut occupancy = vec![0u32; st.pois.len()];
        for p in &st.pawns { if let Some(i) = p.bot.core.poi { if i < occupancy.len() { occupancy[i] += 1; } } }
        let phase = phase_of(&ph, sim, storm);
        let sounds = std::mem::take(&mut st.sounds);
        let mut fires: Vec<(usize, fn_bots::FireRequest)> = vec![];
        let mut interacts: Vec<(usize, u64)> = vec![];
        let mut exits: Vec<usize> = vec![];
        {
            let nav = st.nav.as_ref().and_then(|n| n.as_ref());
            let view = View { sim, now, phase, agents: &agents, pois: &st.pois, occupancy: &occupancy, sounds: &sounds, storm: storm_v, bus, nav, los_used: Cell::new(0), paths_used: Cell::new(0), loot_buf: RefCell::new(vec![]), chest_buf: RefCell::new(vec![]) };
            let tt = std::time::Instant::now();
            for (pi, p) in st.pawns.iter_mut().enumerate() {
                if !p.bot.core.alive { continue; }
                let dist = p.bot.core.feet.distance(player_feet);
                let period = if p.bot.core.target.is_some() && dist < 160. { 0.05 } else if dist < 60. { 0.1 } else if dist < 160. { 0.2 } else { 0.4 };
                p.think_acc += dt;
                if p.think_acc >= period {
                    let tdt = p.think_acc.max(1e-3);
                    p.think_acc = 0.;
                    p.bot.think(&ph.graph, &view, tdt);
                    let it = &p.bot.core.intent;
                    if let Some(f) = it.fire { fires.push((pi, f)); }
                    if let Some(id) = it.interact { interacts.push((pi, id)); }
                    if it.exit_bus { exits.push(pi); }
                }
            }
            let think_ms = tt.elapsed().as_secs_f32() * 1000.;
            st.prof[0] += think_ms;
            st.last_ms[0] = think_ms;
            if let Ok(mut r) = REPORT.lock() { *r = format!("bots {} alive {} los {}", st.pawns.len(), st.pawns.iter().filter(|p| p.bot.core.alive).count(), view.los_used.get()); }
        }
        for pi in exits {
            let p = &mut st.pawns[pi];
            let Some(b) = bus else { continue };
            let mut c = new_controller(sim, b.pos, p.bot.core.yaw);
            c.velocity = Vec2::new(b.dir.x, b.dir.z) * fn_game::drop_in::AIRCRAFT_SPEED * 0.01;
            c.facing = p.bot.core.yaw; c.actor_yaw = Some(p.bot.core.yaw);
            c.component_mut().start_skydiving();
            p.ctl = c;
            p.bot.core.body = Body::Skydive;
            p.bot.core.feet = b.pos;
            p.bot.core.stats.jumped_at = Some(now);
            eprintln!("[bots] {} jumps off the bus at ({:.0}, {:.0}) toward {:?}", p.bot.core.name, b.pos.x, b.pos.z, p.bot.core.landing_name(&st.pois));
        }
        if bus.is_none() && sim.drop.as_ref().map_or(false, |d| d.phase != fn_game::drop_in::GamePhase::Warmup) {
            for p in st.pawns.iter_mut().filter(|p| p.bot.core.body == Body::Bus) {
                let at = p.bot.core.feet;
                let mut c = new_controller(sim, at, p.bot.core.yaw);
                c.component_mut().start_skydiving();
                p.ctl = c; p.bot.core.body = Body::Skydive; p.bot.core.stats.jumped_at = Some(now);
            }
        }
        let tm = std::time::Instant::now();
        let (mut hull_ms, mut step_ms) = (0f32, 0f32);
        let spec_idx = st.spec.map(|s| s.0);
        let mut fell_out = vec![];
        for (pi, p) in st.pawns.iter_mut().enumerate() {
            if !p.bot.core.alive { if let Some(t) = p.death_t.as_mut() { *t += dt; } continue; }
            match p.bot.core.body {
                Body::Bus => { if let Some(b) = bus { p.bot.core.feet = b.pos; } }
                Body::Skydive | Body::Parachute => {
                    let it = &p.bot.core.intent;
                    let y = p.bot.core.yaw.to_radians();
                    let fwd = Vec3::new(y.sin(), if it.aerial_dive { -1. } else { -0.3 }, -y.cos()).normalize();
                    let inp = if it.aerial_dive { fwd } else { Vec3::new(it.aerial.x, 0., it.aerial.y) };
                    let g2c = |v: Vec3| Vec3::new(v.x, v.z, v.y);
                    p.ctl.component_mut().set_aerial_input(g2c(inp), g2c(fwd));
                    p.ctl.facing = p.bot.core.yaw; p.ctl.actor_yaw = Some(p.bot.core.yaw);
                    auto_deploy(sim, &mut p.ctl);
                    let low = p.ctl.height - sim.ground_y(p.ctl.position.x, p.ctl.position.y + Z_OFFSET) < 40.;
                    p.hulls_t -= dt;
                    if low && p.hulls_t <= 0. {
                        p.hulls_t = 0.25;
                        p.ctl.hulls = fn_props::player_hulls_cached(&sim.world.props, sim.fresh_prop_index(), Z_OFFSET, p.ctl.position, 8., Some(&mut p.hull_cache));
                    }
                    p.ctl.step(Vec2::ZERO, false, false, dt);
                    let c = p.ctl.component();
                    p.bot.core.body = if c.is_skydiving() { if c.is_parachute_open() { Body::Parachute } else { Body::Skydive } } else if c.in_aerial_mode() { Body::Parachute } else { Body::Ground };
                    if p.bot.core.body == Body::Ground { p.bot.core.stats.landed_at = Some(now); eprintln!("[bots] {} landed at ({:.0}, {:.1}, {:.0})", p.bot.core.name, p.ctl.position.x, p.ctl.height, p.ctl.position.y + Z_OFFSET); }
                }
                Body::Ground | Body::Island => {
                    let dist = if spec_idx == Some(p.idx) { 0. } else { p.bot.core.feet.distance(player_feet) };
                    let it = p.bot.core.intent.clone();
                    let every = if dist < 8. || p.ctl.airborne() || it.jump { 0. } else if dist < 20. { 1. / 60. } else if dist < 45. { 1. / 30. } else if dist < 90. { 1. / 15. } else { 1. / 8. };
                    p.step_acc += dt;
                    if p.step_acc < every { let v = p.ctl.velocity * p.step_acc; p.bot.core.feet = feet_of(&p.ctl) + Vec3::new(v.x, 0., v.y); continue; }
                    let sdt = std::mem::take(&mut p.step_acc);
                    if dist < FULL_MOVE_M {
                        p.hulls_t -= sdt;
                        if p.hulls_t <= 0. {
                            let th = std::time::Instant::now();
                            p.hulls_t = if dist < 25. { 0.2 } else { 0.6 };
                            let ix = sim.fresh_prop_index();
                            p.ctl.hulls = fn_props::player_hulls_cached(&sim.world.props, ix, Z_OFFSET, p.ctl.position, 6., Some(&mut p.hull_cache));
                            let w = Vec2::new(p.ctl.position.x, p.ctl.position.y + Z_OFFSET);
                            p.ctl.hulls.extend(sim.building_hulls.iter().filter(|h| (Vec2::new(h.center.x, h.center.z) - w).length() - h.radius <= 6.).map(|h| h.mapped(|v| Vec3::new(v.x, v.z - Z_OFFSET, v.y), true)));
                            hull_ms += th.elapsed().as_secs_f32() * 1000.;
                        }
                        p.ctl.wants_crouch = it.crouch;
                        p.ctl.facing = if it.strafe || it.move_dir.length_squared() < 0.01 { p.bot.core.yaw } else { fn_bots::brain::yaw_to(Vec3::new(it.move_dir.x, 0., it.move_dir.y)) };
                        p.ctl.actor_yaw = None;
                        let fr = crate::water_hook::capsule_centre_ue(feet_of(&p.ctl), p.ctl.capsule_half_height_cm()).map_or(1., crate::water_hook::fluid_friction_at);
                        p.ctl.component_mut().physics_volume_fluid_friction = fr;
                        let ts = std::time::Instant::now();
                        p.ctl.step(it.move_dir, it.jump, it.sprint && !it.strafe, sdt);
                        step_ms += ts.elapsed().as_secs_f32() * 1000.;
                    } else {
                        let fr = crate::water_hook::capsule_centre_ue(feet_of(&p.ctl), p.ctl.capsule_half_height_cm()).map_or(1., crate::water_hook::fluid_friction_at);
                        let speed = if it.sprint { 5.5 } else { 4.1 } * fr;
                        let mut f = feet_of(&p.ctl) + Vec3::new(it.move_dir.x, 0., it.move_dir.y) * speed * sdt;
                        f.y = sim.floor_below(Vec3::new(f.x, f.y + 1.2, f.z));
                        p.ctl.position = Vec2::new(f.x, f.z - Z_OFFSET);
                        p.ctl.height = f.y;
                        p.ctl.velocity = it.move_dir * speed;
                        p.ctl.vertical_speed = 0.;
                    }
                }
            }
            if p.bot.core.body != Body::Bus { let f = feet_of(&p.ctl); p.bot.core.vel = (f - p.last_pos) / dt.max(1e-3); p.last_pos = f; p.bot.core.feet = f; }
            if p.bot.core.body != Body::Bus && crate::water_hook::capsule_centre_ue(p.bot.core.feet, p.ctl.capsule_half_height_cm()).map_or(p.bot.core.feet.y < -50., crate::water_hook::below_kill_z) { fell_out.push(pi); }
        }
        for pi in fell_out { eprintln!("[bots] {} fell out of the world (KillZ {})", st.pawns[pi].bot.core.name, crate::water_hook::KILL_Z_CM); kill(st, pi, Killer::Themself, crate::water_hook::KILLZ_DEATH_CAUSE); }
        st.last_ms[1] = tm.elapsed().as_secs_f32() * 1000.;
        st.prof[1] += st.last_ms[1];
        st.prof[4] += hull_ms;
        st.prof[5] += step_ms;
        let tf = std::time::Instant::now();
        let gate = sim.damage_enabled();
        for (pi, id) in interacts { loot(st, arms, pi, id); }
        resolve_guns(st, arms);
        st.fire_queue.extend(fires);
        for _ in 0..FIRE_BUDGET { let Some((pi, f)) = st.fire_queue.pop_front() else { break }; if st.pawns[pi].bot.core.alive { shoot(st, sim, arms, pi, f, gate, &mut out); } }
        while st.fire_queue.len() > 64 { st.fire_queue.pop_front(); }
        let incoming = HITS_IN.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for (h, row) in incoming {
            let Some(pi) = st.pawns.iter().position(|p| p.bot.core.id == h.id) else { continue };
            let by_row = DeathCause::from_weapon_row(&row);
            let cause = if by_row == DeathCause::Unspecified { class_cause(WeaponClass::of_row(&row)) } else { by_row };
            let killer = Killer::Player(crate::kill_feed_hook::local());
            apply_hit(st, pi, &h, PLAYER_ID, player_feet, killer, cause);
        }
        let blasts = BOT_BLAST_IN.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for (from_id, from, h, row) in blasts {
            let Some(pi) = st.pawns.iter().position(|p| p.bot.core.id == h.id) else { continue };
            let by_row = DeathCause::from_weapon_row(&row);
            let cause = if by_row == DeathCause::Unspecified { class_cause(WeaponClass::of_row(&row)) } else { by_row };
            let killer = st.pawns.iter().find(|p| p.bot.core.id == from_id).map_or(Killer::Themself, |p| Killer::Player(Combatant::other(&p.bot.core.name)));
            apply_hit(st, pi, &h, from_id, from, killer, cause);
        }
        if let Some(z) = storm.bot_zone() {
            for pi in 0..st.pawns.len() {
                let p = &mut st.pawns[pi];
                if !p.bot.core.alive || !matches!(p.bot.core.body, Body::Ground) { continue; }
                let f = p.bot.core.feet;
                if Vec2::new(f.x - z.center.x, f.z - z.center.z).length() <= z.radius { p.storm_t = 0.; continue; }
                p.storm_t += dt;
                if p.storm_t < 1. { continue; }
                p.storm_t -= 1.;
                p.bot.core.health -= z.damage_fraction * 100.;
                if p.bot.core.health <= 0. { kill(st, pi, Killer::Themself, DeathCause::OutsideSafeZone); }
            }
        }
        for (feet, items) in std::mem::take(&mut st.drops) {
            let start = feet + Vec3::Y * fn_game::pickups::PAWN_HALF_HEIGHT;
            for (i, (item, count)) in items.iter().enumerate() {
                let e = crate::mp_hook::death_toss_end(feet, i);
                let floor = sim.floor_below(Vec3::new(e.x, feet.y + 0.5, e.z));
                crate::pickups_hook::bot_death_toss(item, *count, start, Vec3::new(e.x, floor + fn_game::pickups::TOUCH_HALF_HEIGHT, e.z));
            }
            if !items.is_empty() { eprintln!("[bots] death drop at ({:.0}, {:.0}): {}", feet.x, feet.z, items.iter().map(|(i, c)| format!("{c} x {i}")).collect::<Vec<_>>().join(", ")); }
        }
        if mode == 2 && player_alive && !st.won_set && !st.pawns.is_empty() && st.pawns.len() >= n && st.pawns.iter().all(|p| !p.bot.core.alive) {
            st.won_set = true;
            crate::kill_feed_hook::set_won(true);
            eprintln!("[bots] every bot eliminated: victory");
        }
        st.last_ms[2] = tf.elapsed().as_secs_f32() * 1000.;
        st.prof[2] += st.last_ms[2];
        spectate(st, sim, dt);
        if st.outfit_rx.is_none() && st.outfits.is_empty() {
            PREP.with(|c| { let mut p = c.borrow_mut(); if p.rx.is_some() { st.outfit_rx = p.rx.take(); st.female_clips = p.female_clips.take(); st.outfit_pending = std::mem::take(&mut p.pending); st.outfits = std::mem::take(&mut p.outfits); } });
        }
        let seed = st.rng;
        outfit_step(&mut st.outfit_rx, &mut st.female_clips, &mut st.outfit_pending, &mut st.outfits, scene, seed);
        let td = std::time::Instant::now();
        draw(st, scene, arms, sim.cam_view.eye, sim.cam_view.forward, dt);
        let draw_ms = td.elapsed().as_secs_f32() * 1000.;
        st.prof[3] += draw_ms;
        let all_ms = t0.elapsed().as_secs_f32() * 1000.;
        if all_ms > 6. && st.slow_at.map_or(true, |t| t.elapsed().as_secs_f32() > 1.) { st.slow_at = Some(std::time::Instant::now()); eprintln!("[bots] slow frame {all_ms:.1} ms (think {:.1}, move {:.1}, fire/loot {:.1} [trace {:.1}, fx {:.1}, shots {}], draw {draw_ms:.1})", st.last_ms[0], st.last_ms[1], st.last_ms[2], st.last_ms[3], st.last_ms[4], st.last_ms[5]); }
        st.last_ms = [0.; 6];
        st.log_t += dt;
        if let Some((i, _, _)) = st.spec { if (st.log_t * 2.).floor() != ((st.log_t - dt) * 2.).floor() { let p = &st.pawns[i]; let c = &p.bot.core; let task = p.bot.runner.active_task().map(|t| ph.graph.nodes[t as usize].label()).unwrap_or_default(); let cid = p.gfx.as_ref().filter(|g| g.outfit > 0).map_or("player-model".to_string(), |g| st.outfits[g.outfit - 1].cid.clone()); eprintln!("[bots] spec {} {cid} {:?} task {task} goal {:?} feet ({:.1},{:.1},{:.1}) dir {:?} alert {} loot {} wl {} wp {} patrol {} interact {:?} weapons {} plans {} path {}/{}", c.name, c.body, c.nav.goal.map(|g| (g.x as i32, g.z as i32)), c.feet.x, c.feet.y, c.feet.z, c.intent.move_dir, c.bb.int("AIEvaluator_Global_AlertLevel"), c.bb.int("AIEvaluator_Loot_ExecutionStatus"), c.bb.int("AIEvaluator_WarmupLootAndShoot_ExecutionStatus"), c.bb.int("AIEvaluator_WarmupPlayEmote_ExecutionStatus"), c.bb.int("AIEvaluator_Patrol_ExecutionStatus"), c.intent.interact, c.weapons.len(), c.nav.plans, c.nav.idx, c.nav.path.len()); } }
        if st.log_t > 10. {
            st.log_t = 0.;
            let alive = st.pawns.iter().filter(|p| p.bot.core.alive).count();
            let by = |b: Body| st.pawns.iter().filter(|p| p.bot.core.alive && p.bot.core.body == b).count();
            eprintln!("[bots] {} spawned, {alive} alive (island {}, bus {}, skydive {}, glide {}, ground {}), {:.2} ms/frame avg (think {:.2}, move {:.2}, fire/loot {:.2}, draw {:.2}, hulls {:.2}, step {:.2}, pose {:.2})", st.pawns.len(), by(Body::Island), by(Body::Bus), by(Body::Skydive), by(Body::Parachute), by(Body::Ground), st.prof_ms / st.prof_n.max(1) as f32, st.prof[0] / st.prof_n.max(1) as f32, st.prof[1] / st.prof_n.max(1) as f32, st.prof[2] / st.prof_n.max(1) as f32, st.prof[3] / st.prof_n.max(1) as f32, st.prof[4] / st.prof_n.max(1) as f32, st.prof[5] / st.prof_n.max(1) as f32, st.prof[6] / st.prof_n.max(1) as f32);
            st.prof_ms = 0.; st.prof_n = 0; st.prof = [0.; 7];
        }
    });
    ST.with(|cell| { if let Some(st) = cell.borrow_mut().as_mut() { st.prof_ms += t0.elapsed().as_secs_f32() * 1000.; st.prof_n += 1; } });
    out
}

fn spectate(st: &mut State, sim: &mut GameSim, dt: f32) {
    static WANT: OnceLock<Option<usize>> = OnceLock::new();
    let Some(want) = *WANT.get_or_init(|| fn_live::var("FN_BOTS_SPECTATE").ok().and_then(|v| if v.trim() == "f" { Some(usize::MAX) } else { v.trim().parse::<usize>().ok() })) else { return };
    if st.pawns.is_empty() { return; }
    let n_out = st.outfits.len();
    let female_of = |p: &Pawn| { let o = p.gfx.as_ref().map(|g| g.outfit).unwrap_or(if n_out == 0 { 0 } else { 1 + (p.seed as usize % n_out) }); o > 0 && st.outfits[o - 1].female };
    let pick = if want == usize::MAX { st.pawns.iter().position(|p| p.bot.core.alive && female_of(p)) } else { Some(want.min(st.pawns.len() - 1)) };
    let Some(pick) = pick else { return };
    let cur = st.spec.map_or(pick, |s| s.0);
    let i = if st.pawns[cur].bot.core.alive || st.pawns[cur].death_t.map_or(false, |t| t < 4.) { cur } else { match st.pawns.iter().position(|p| p.bot.core.alive) { Some(i) => i, None => return } };
    let p = &st.pawns[i];
    let c = &p.bot.core;
    let (dist, h, pitch) = match c.body { Body::Bus => (28., 8., -25.), Body::Skydive | Body::Parachute => (7., 2.5, -30.), _ => (4.2, 2.0, -14.) };
    let (yaw0, pitch0) = st.spec.filter(|s| s.0 == i).map_or((c.yaw, pitch), |s| (s.1, s.2));
    let k = 1. - (-3. * dt).exp();
    let dy = ((c.yaw - yaw0 + 540.).rem_euclid(360.)) - 180.;
    let yaw = (yaw0 + dy * k).rem_euclid(360.);
    let pitch = pitch0 + (pitch - pitch0) * k;
    st.spec = Some((i, yaw, pitch));
    let (f, r, u) = fn_camera::view_basis(yaw, pitch);
    let eye = c.feet + Vec3::Y * h - f * dist;
    let v = &mut sim.cam_view;
    v.eye = eye; v.forward = f; v.right = r; v.up = u; v.yaw_deg = yaw; v.pitch_deg = pitch;
}

fn wall_box(p: &fn_building::world::PlacedPiece) -> Option<(Vec3, Vec3)> {
    if p.kind != fn_building::pieces::PieceKind::Wall || p.edit.class_path.is_some() { return None; }
    let hulls = crate::building_hook::with_catalog(|c| c.map(|c| fn_building::placement::piece_hulls(c, p)))?;
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for h in &hulls { for c in [h.min, h.max] { let g = crate::building_hook::world_to_sim(c); lo = lo.min(g); hi = hi.max(g); } }
    (lo.x <= hi.x).then_some((lo, hi))
}

fn sync_obstacles(st: &mut State) {
    let Some(Some(nav)) = st.nav.as_mut() else { return };
    let cursor = st.build_cursor;
    let (changed, next) = crate::building_hook::with_world(|w| {
        let (ev, next) = w.events_since(cursor.unwrap_or(u64::MAX));
        let changed: Vec<(fn_building::world::PieceId, Option<fn_building::world::PlacedPiece>)> = match cursor {
            None => w.iter().map(|p| (p.id, Some(p.clone()))).collect(),
            Some(_) => ev.iter().filter_map(|e| match e { fn_building::world::WorldEvent::Placed(id) | fn_building::world::WorldEvent::Edited(id) => Some((*id, w.get(*id).cloned())), fn_building::world::WorldEvent::Removed(id) => Some((*id, None)), _ => None }).collect(),
        };
        (changed, next)
    });
    st.build_cursor = Some(next);
    let before = st.obstacles.len();
    let any = !changed.is_empty();
    for (id, piece) in changed {
        if let Some(o) = st.obstacles.remove(&id) { nav.remove_obstacle(o); }
        if let Some((lo, hi)) = piece.as_ref().and_then(wall_box) { st.obstacles.insert(id, nav.add_obstacle(lo, hi)); }
    }
    if any && st.obstacles.len() != before { eprintln!("[bots] nav obstacles: {} player-built walls", st.obstacles.len()); }
}

fn near_tuning(n: usize) -> (f32, f32) {
    const T: [(f32, f32, f32); 4] = [(10., 0.5, 120.), (25., 0.45, 140.), (50., 0.35, 170.), (99., 0.25, 220.)];
    let x = n as f32;
    let (s, r) = if x <= T[0].0 { (T[0].1, T[0].2) } else if x >= T[3].0 { (T[3].1, T[3].2) } else {
        let i = T.windows(2).position(|w| x <= w[1].0).unwrap_or(2);
        let (a, b) = (T[i], T[i + 1]);
        let k = (x - a.0) / (b.0 - a.0);
        (a.1 + (b.1 - a.1) * k, a.2 + (b.2 - a.2) * k)
    };
    let f = |k: &str| fn_live::var(k).ok().and_then(|v| v.trim().parse::<f32>().ok());
    (f("FN_BOTS_NEAR_SHARE").unwrap_or(s).clamp(0., 1.), f("FN_BOTS_NEAR_R").unwrap_or(r).clamp(20., 1500.))
}

fn building_spots() -> Vec<Vec3> {
    let Some(set) = crate::poi::current() else { return vec![] };
    set.groups.iter().filter(|g| g.placements.len() >= 3 && set.files.get(g.file).map_or(false, |n| { let n = n.to_ascii_lowercase(); !["lobby", "terrain", "ls_", "bridge", "tunnel"].iter().any(|x| n.contains(x)) })).map(|g| Vec3::new(g.center.x, g.center.z, g.center.y) * 0.01).collect()
}

fn flat(a: Vec3, b: Vec3) -> f32 { Vec2::new(a.x - b.x, a.z - b.z).length() }

fn drop_plan(st: &mut State, sim: &GameSim, now: f32, dt: f32) {
    let in_drop = sim.drop.as_ref().map_or(false, |d| d.phase != fn_game::drop_in::GamePhase::Warmup);
    if !in_drop { st.plan.was_on_bus = sim.on_bus(); return; }
    let n = st.pawns.len().max(wanted());
    let (share, radius) = near_tuning(n);
    let on_bus = sim.on_bus();
    for p in st.pawns.iter_mut().filter(|p| !p.drop_decided && p.bot.core.body == Body::Bus) {
        p.drop_decided = true;
        let mut s = p.seed ^ 0xd209;
        p.bot.core.follow = on_bus && st.plan.jump_t.is_none() && share > 0. && rand01(&mut s) < share;
    }
    if st.plan.jump_t.is_none() && !on_bus {
        st.plan.jump_t = Some(now);
        let mut k = 0;
        for p in st.pawns.iter_mut().filter(|p| p.bot.core.follow && p.bot.core.body == Body::Bus) {
            let mut s = p.seed ^ 0x6a7;
            p.bot.core.exit_at = Some(now + if st.plan.was_on_bus { 0.4 + rand01(&mut s) * 3.6 } else { 0. });
            k += 1;
        }
        if st.plan.was_on_bus { eprintln!("[bots] player jumped: {k} bots follow off the bus within 4 s (share {share:.2}, radius {radius:.0} m, {n} bots)"); }
    }
    st.plan.was_on_bus = on_bus;
    let Some(jt) = st.plan.jump_t else { return };
    let f = sim.feet();
    if let Some(pf) = st.plan.prev_feet { let v = (f - pf) / dt.max(1e-3); let k = (dt * 3.).min(1.); st.plan.vel = st.plan.vel + (v - st.plan.vel) * k; }
    st.plan.prev_feet = Some(f);
    let cmc = sim.controller.component();
    let air = cmc.is_skydiving() || cmc.in_aerial_mode();
    let just_landed = st.plan.landed_t.is_none() && !air && now - jt > 1.5;
    if just_landed { st.plan.landed_t = Some(now); }
    let center = if st.plan.landed_t.is_some() { Vec3::new(f.x, sim.ground_y(f.x, f.z), f.z) } else {
        let h = (f.y - sim.ground_y(f.x, f.z)).max(0.);
        let t = (h / (-st.plan.vel.y).max(4.)).min(90.);
        let mut off = Vec2::new(st.plan.vel.x, st.plan.vel.z) * t;
        if off.length() > 600. { off = off.normalize() * 600.; }
        let (x, z) = (f.x + off.x, f.z + off.y);
        Vec3::new(x, sim.ground_y(x, z), z)
    };
    st.plan.center = Some(center);
    if let Some(lt) = st.plan.landed_t {
        let within = st.pawns.iter().filter(|p| p.bot.core.alive && p.bot.core.body != Body::Bus && flat(p.bot.core.feet, f) < 150.).count();
        let landed = st.pawns.iter().filter(|p| p.bot.core.alive && p.bot.core.body == Body::Ground && flat(p.bot.core.feet, f) < 150.).count();
        let alive = st.pawns.iter().filter(|p| p.bot.core.alive).count();
        if st.plan.logged == 0 { st.plan.logged = 1; eprintln!("[bots] near-player at landing: {within} within 150m ({landed} landed, followers {}, {alive} alive of {n}, share {share:.2}, radius {radius:.0} m)", st.plan.followers); }
        if st.plan.logged == 1 && now - lt > 20. { st.plan.logged = 2; eprintln!("[bots] near-player +20s: {within} within 150m ({landed} landed, followers {}, {alive} alive of {n})", st.plan.followers); }
    }
    if share <= 0. { return; }
    if now < st.plan.next_t && !just_landed { return; }
    st.plan.next_t = now + 1.;
    if !just_landed && st.plan.assigned_at.map_or(false, |a| flat(a, center) < 40.) { return; }
    let first = st.plan.assigned_at.is_none();
    st.plan.assigned_at = Some(center);
    if st.plan.spots.is_empty() { st.plan.spots = building_spots(); }
    let quota = ((n as f32) * share).round() as usize;
    let mut cur = st.pawns.iter().filter(|p| p.bot.core.alive && p.bot.core.follow).count();
    if first {
        for p in st.pawns.iter_mut() {
            if cur >= quota { break; }
            let c = &p.bot.core;
            if c.follow || !c.alive || !matches!(c.body, Body::Skydive | Body::Parachute) { continue; }
            let h = c.feet.y - sim.ground_y(c.feet.x, c.feet.z);
            if flat(c.feet, center) <= h * 1.2 { p.bot.core.follow = true; cur += 1; }
        }
    }
    let mut who: Vec<usize> = st.pawns.iter().enumerate().filter(|(_, p)| {
        let c = &p.bot.core;
        if !c.alive || !c.follow { return false; }
        match c.body { Body::Bus => true, Body::Skydive | Body::Parachute => { let h = c.feet.y - sim.ground_y(c.feet.x, c.feet.z); h > 25. && flat(c.feet, center) <= h * 1.3 + radius } _ => false }
    }).map(|(i, _)| i).collect();
    st.plan.followers = st.plan.followers.max(cur);
    if who.is_empty() { return; }
    let sep = (radius / (who.len() as f32).sqrt()).clamp(12., 50.);
    let nav = st.nav.as_ref().and_then(|n| n.as_ref());
    let mut cand: Vec<Vec3> = st.plan.spots.iter().copied().filter(|s| flat(*s, center) <= radius).collect();
    cand.sort_by_key(|s| hash((s.x.to_bits() as u64) << 32 ^ s.z.to_bits() as u64));
    let mut picked: Vec<Vec3> = vec![];
    for c in cand { if picked.len() >= who.len() { break; } if picked.iter().all(|q| flat(*q, c) >= sep) { picked.push(c); } }
    let mut s = hash(center.x.to_bits() as u64 ^ 0x5eed);
    let mut tries = 0;
    while picked.len() < who.len() && tries < who.len() * 8 {
        tries += 1;
        let a = rand01(&mut s) * std::f32::consts::TAU;
        let k = (0.25 + 0.75 * rand01(&mut s).sqrt()) * radius;
        let c = Vec3::new(center.x + a.cos() * k, 0., center.z + a.sin() * k);
        if picked.iter().all(|q| flat(*q, c) >= sep * if tries > who.len() * 4 { 0.5 } else { 1. }) { picked.push(c); }
    }
    let snap = |c: Vec3, seed: u64| nav.and_then(|m| m.random_point_in_radius(Vec3::new(c.x, sim.ground_y(c.x, c.z), c.z), 10., seed)).unwrap_or_else(|| Vec3::new(c.x, sim.ground_y(c.x, c.z), c.z));
    let mut used = vec![false; picked.len()];
    who.sort_by(|a, b| flat(st.pawns[*b].bot.core.feet, center).total_cmp(&flat(st.pawns[*a].bot.core.feet, center)));
    for &pi in &who {
        let from = st.pawns[pi].bot.core.feet;
        let Some(j) = (0..picked.len()).filter(|j| !used[*j]).min_by(|a, b| flat(picked[*a], from).total_cmp(&flat(picked[*b], from))) else { break };
        used[j] = true;
        let land = snap(picked[j], st.pawns[pi].seed ^ j as u64);
        let poi = st.pois.iter().enumerate().filter(|(_, q)| flat(q.center, land) < q.radius + radius).min_by(|a, b| flat(a.1.center, land).total_cmp(&flat(b.1.center, land))).map(|(i, _)| i);
        st.pawns[pi].bot.core.set_landing(land, poi);
    }
    eprintln!("[bots] drop zone ({:.0}, {:.0}){}: {} followers over {} spots ({} buildings in {:.0} m)", center.x, center.z, if st.plan.landed_t.is_some() { " player landed" } else { "" }, who.len(), picked.len(), st.plan.spots.iter().filter(|s| flat(**s, center) <= radius).count(), radius);
}

fn auto_deploy(sim: &GameSim, c: &mut Controller) {
    use fn_game::drop_in::*;
    let Some(d) = sim.drop.as_ref() else { return };
    let cmc = c.component();
    if !cmc.is_skydiving() || cmc.is_parachute_forced_open() { return; }
    let p = c.render_position();
    let feet_game = Vec3::new(p.x, c.height, p.y + Z_OFFSET);
    let actor_ue = d.map.to_ue(feet_game) + Vec3::new(0., 0., cmc.half_height);
    if actor_ue.z > SKYDIVING_SCAN_FOR_IMPACT_WORLD_Z { return; }
    let vn = cmc.velocity.normalize_or_zero();
    let b = PARACHUTE_DEPLOY_VELOCITY_BLEND.clamp(0., 1.);
    let dir = Vec3::new(0., 0., -1.).lerp(vn, b).normalize_or_zero();
    let mut dist = PARACHUTE_DEPLOY_TRACE_FOR_GROUND_DISTANCE;
    if PARACHUTE_DEPLOY_FIXED_VERTICAL_DISTANCE && b < 0.99 && dir.z < -1e-4 { dist /= dir.z.abs(); }
    let end = d.map.to_game(actor_ue + dir * dist);
    let ground_hit = sim.ground_y(end.x, end.z) >= end.y;
    let low = actor_ue.z <= SKYDIVING_FORCE_DEPLOY_PARACHUTE_WORLD_Z + PARACHUTE_DEPLOY_TRACE_FOR_GROUND_DISTANCE;
    if ground_hit || low { c.component_mut().force_parachute_open(); }
}

fn loot(st: &mut State, arms: &Arms, pi: usize, id: u64) {
    let p = &mut st.pawns[pi];
    let got = if id >= crate::chest_hook::BOT_CHEST_BASE { crate::chest_hook::bot_open(id).then(|| (String::new(), 0)) } else { crate::pickups_hook::bot_take(id) };
    p.bot.core.interact_done = Some(id);
    let Some((item, count)) = got else { return };
    if id >= crate::chest_hook::BOT_CHEST_BASE { eprintln!("[bots] {} opened a chest", p.bot.core.name); return; }
    if item.is_empty() || item.contains("Harvest") { return; }
    if !item.starts_with("WID_") { stash(&mut p.inv, &item, count.max(1)); return; }
    give_gun_item(st, arms, pi, &item);
}

fn give_gun_item(st: &mut State, arms: &Arms, pi: usize, item: &str) {
    match find_gun(arms, item) {
        Some(g) => add_gun(&mut st.pawns[pi], arms, g),
        None => {
            eprintln!("[bots] {} got {item}: loading its weapon pack", st.pawns[pi].bot.core.name);
            st.pending_guns.push((pi, item.to_owned(), st.clock));
            if !st.requested.contains_key(item) { st.requested.insert(item.to_owned(), st.clock); if let Ok(mut q) = GUN_WANTS.lock() { q.push(item.to_owned()); } }
        }
    }
}

fn stash(inv: &mut Vec<(String, u32)>, item: &str, count: u32) {
    match inv.iter_mut().find(|e| e.0 == item) { Some(e) => e.1 += count, None => inv.push((item.to_owned(), count)) }
}

fn add_gun(p: &mut Pawn, arms: &Arms, g: usize) {
    if p.bot.core.weapons.iter().any(|w| gun_index(w) == Some(g)) { return; }
    let Some(w) = weapon_from(arms, g) else { return };
    let gd = &arms.data.guns[g].def;
    eprintln!("[bots] {} now holds {}", p.bot.core.name, gd.item);
    if p.bot.core.weapons.len() >= 6 {
        let old = p.bot.core.weapons.remove(1);
        if let Some(og) = gun_index(&old).and_then(|i| arms.data.guns.get(i)) { let it = og.def.item.clone(); p.inv.retain(|e| e.0 != it); }
        if let Some(e) = p.bot.core.equipped { if e >= p.bot.core.weapons.len() { p.bot.core.equipped = Some(0); } }
    }
    p.bot.core.weapons.push(w);
    stash(&mut p.inv, &gd.item, 1);
}

fn resolve_guns(st: &mut State, arms: &Arms) {
    if st.pending_guns.is_empty() { return; }
    let failed: Vec<String> = GUN_FAILED.lock().map(|v| v.clone()).unwrap_or_default();
    let now = st.clock;
    let mut i = 0;
    while i < st.pending_guns.len() {
        let (pi, item, t) = st.pending_guns[i].clone();
        if let Some(g) = find_gun(arms, &item) { st.pending_guns.swap_remove(i); if st.pawns[pi].bot.core.alive { add_gun(&mut st.pawns[pi], arms, g); } continue; }
        if failed.iter().any(|f| f == &item) || now - t > 30. {
            st.pending_guns.swap_remove(i);
            let p = &mut st.pawns[pi];
            let mut s = p.seed ^ hash(item.len() as u64);
            let sub = pick_gun(arms, &item, &mut s);
            eprintln!("[bots] {item}: weapon pack unavailable, {} holds {} instead", p.bot.core.name, sub.and_then(|g| arms.data.guns.get(g)).map_or("nothing", |g| g.def.item.as_str()));
            if let (Some(g), true) = (sub, p.bot.core.alive) { add_gun(p, arms, g); }
            continue;
        }
        i += 1;
    }
}

fn gun_hit(st: &mut State, g: usize, arms: &Arms) -> Option<GunHit> {
    if let Some(h) = st.guns.get(&g) { return Some(h.clone()); }
    let row = &arms.data.guns.get(g)?.def.stat_row;
    let h = crate::hitreg_hook::gun_by_row(row)?;
    st.guns.insert(g, h.clone());
    Some(h)
}

fn shoot(st: &mut State, sim: &mut GameSim, arms: &mut Arms, pi: usize, f: fn_bots::FireRequest, gate: bool, out: &mut Vec<SoundEvent>) {
    if !matches!(st.pawns[pi].bot.core.body, Body::Ground | Body::Island) { return; }
    let (id, team, eye, wclass, wrow) = { let c = &st.pawns[pi].bot.core; let w = &c.weapons[f.weapon]; (c.id, st.pawns[pi].team, c.eye(), w.class, w.clone()) };
    if wclass == WeaponClass::Melee {
        let pos = if f.target == PLAYER_ID { sim.feet() } else { st.pawns.iter().find(|p| p.bot.core.id == f.target).map_or(eye, |p| p.bot.core.feet) };
        if pos.distance(eye) > 3.2 { return; }
        let dmg = if gate { PICKAXE_DAMAGE } else { 0. };
        let name = st.pawns[pi].bot.core.name.clone();
        if f.target == PLAYER_ID { hurt_player(arms, dmg, eye, &name, DeathCause::Melee); }
        else if let Some(ti) = st.pawns.iter().position(|p| p.bot.core.id == f.target) {
            let mut v = Vitals::new(st.pawns[ti].bot.core.health.max(0.01), st.pawns[ti].bot.core.shield);
            let a = v.apply_damage(dmg);
            let h = PawnHit { pawn: 0, id: f.target, pellet: 0, pos, normal: Vec3::Y, distance_m: 1., zone: fn_hitreg::shoot::Zone::Normal, bone: None, damage: PICKAXE_DAMAGE, applied: a, numbers: vec![], feedback: Default::default() };
            apply_hit(st, ti, &h, id, eye, Killer::Player(Combatant::other(&name)), DeathCause::Melee);
        }
        st.pawns[pi].fire = None;
        return;
    }
    let Some(g) = gun_index(&wrow) else { return };
    let Some(gun) = gun_hit(st, g, arms) else { return };
    if fn_hitreg::explosive::def_of(&gun).is_some() {
        let seed = (hash(st.rng ^ st.clock.to_bits() as u64 ^ id) & 0x7fff_ffff) as i32;
        let ctx = FireContext { targeting: st.pawns[pi].bot.core.intent.ads, velocity_zero: st.pawns[pi].bot.core.vel.length_squared() < 1e-4, muzzle: None, level: 1, now: st.clock };
        let mut spread = SpreadState::default();
        if let Some(l) = fn_hitreg::explosive::launch(&gun, &sim.world, &Shot { eye, forward: f.dir }, &mut spread, &ctx, seed) { crate::rocket_hook::spawn(&gun.wid, crate::rocket_hook::gun_arc(&gun), id, Some(team), l, false); }
        st.pawns[pi].fire = Some((g, 0., st.pawns[pi].bot.core.intent.ads));
        if eye.distance(sim.feet()) < DRAW_M { if let Some(ev) = fn_audio::weapon_events::bp_event(&gun.pres.actor, "fire") { out.push(SoundEvent::Weapon { id: ev, pos: Some(eye), volume: 1. }); } }
        st.sounds.push(Sound { pos: eye, loudness: 100., instigator: id, team });
        return;
    }
    let mut pawns: Vec<PawnTarget> = vec![];
    if arms.health > 0. && !sim.on_bus() {
        let mut t = PawnTarget::new(PLAYER_ID, 0, sim.feet(), Vitals::new(arms.health.max(0.01), arms.shield));
        t.crouched = sim.controller.crouched;
        t.bodies = t.coarse_bodies();
        pawns.push(t);
    }
    let range = gun.stats.trace_range_m() + 5.;
    let cone = (gun.stats.spread.max(1.) * 2.).to_radians().tan();
    let dir = f.dir.normalize_or_zero();
    for p in &st.pawns {
        if p.bot.core.id == id || !p.bot.core.alive || p.bot.core.body == Body::Bus { continue; }
        let c = p.bot.core.feet + Vec3::Y - eye;
        let along = c.dot(dir);
        if along < -2. || along > range || (c - dir * along).length() > 2.5 + along.max(0.) * cone { continue; }
        pawns.push(target_of(p));
    }
    let seed = (hash(st.rng ^ st.clock.to_bits() as u64 ^ id) & 0x7fff_ffff) as i32;
    let ctx = FireContext { targeting: st.pawns[pi].bot.core.intent.ads, velocity_zero: st.pawns[pi].bot.core.vel.length_squared() < 1e-4, muzzle: None, level: 1, now: st.clock };
    let opts = PawnFire { shooter: id, shooter_team: Some(team), ..Default::default() };
    let mut spread = SpreadState::default();
    let t_trace = std::time::Instant::now();
    let rep = fire_near(sim, &gun, &mut pawns, &opts, &Shot { eye, forward: f.dir }, &mut spread, &ctx, seed);
    st.last_ms[3] += t_trace.elapsed().as_secs_f32() * 1000.;
    st.last_ms[5] += 1.;
    let t_fx = std::time::Instant::now();
    st.pawns[pi].fire = Some((g, 0., st.pawns[pi].bot.core.intent.ads));
    let start = st.pawns[pi].gfx.as_ref().map(|_| crate::muzzle_hook::remote_muzzle_ue(&gun.wid, Mat4::from_translation(eye))).flatten().unwrap_or_else(|| Vec3::from(fn_particles::sim::game_to_ue(eye.to_array())));
    if eye.distance(sim.feet()) < DRAW_M {
        crate::gunfx_hook::on_shot(&gun, &rep.tracer_ends, start, true);
        for i in &rep.impacts { crate::gunfx_hook::on_impact(&gun, i); }
        if let Some(ev) = fn_audio::weapon_events::bp_event(&gun.pres.actor, "fire") { out.push(SoundEvent::Weapon { id: ev, pos: Some(eye), volume: 1. }); }
    }
    st.last_ms[4] += t_fx.elapsed().as_secs_f32() * 1000.;
    st.sounds.push(Sound { pos: eye, loudness: 100., instigator: id, team });
    let name = st.pawns[pi].bot.core.name.clone();
    for h in &rep.pawn_hits {
        if h.id == PLAYER_ID {
            arms.shield = (arms.shield - h.applied.shield_damage).max(0.);
            arms.health = (arms.health - h.applied.health_damage).max(0.);
            crate::hurt_hook::on_damage(h.applied.health_damage <= 0. && h.applied.shield_damage > 0., Vec3::from(fn_particles::sim::game_to_ue(eye.to_array())));
            crate::kill_feed_hook::local_damaged(class_cause(wclass), Killer::Player(Combatant::other(&name)), arms.health);
            eprintln!("[bots] {name} hit the player: {:.0} shield + {:.0} health (raw {:.0}), health now {:.0}", h.applied.shield_damage, h.applied.health_damage, h.damage, arms.health);
        } else if let Some(ti) = st.pawns.iter().position(|p| p.bot.core.id == h.id) {
            apply_hit(st, ti, h, id, eye, Killer::Player(Combatant::other(&name)), class_cause(wclass));
        }
    }
}

fn fire_near(sim: &mut GameSim, gun: &GunHit, pawns: &mut [PawnTarget], opts: &PawnFire, shot: &Shot, spread: &mut SpreadState, ctx: &FireContext, seed: i32) -> fn_hitreg::shoot::ShotReport {
    let dir = shot.forward.normalize_or_zero();
    let range = gun.stats.trace_range_m();
    let end = shot.eye + dir * range;
    let lateral = 2. + range * (gun.stats.spread.max(1.) * 4.).min(15.).to_radians().tan();
    let mut cand: Vec<u32> = vec![];
    match sim.fresh_prop_index() { Some(ix) => ix.segment_candidates(Vec2::new(shot.eye.x, shot.eye.z), Vec2::new(end.x, end.z), lateral, &mut cand), None => return fn_hitreg::shoot::fire_at(gun, &mut sim.world, pawns, opts, shot, spread, ctx, seed) }
    cand.sort_unstable();
    cand.dedup();
    cand.retain(|&i| (i as usize) < sim.world.props.len());
    let near: Vec<fn_combat::Harvestable> = cand.iter().map(|&i| sim.world.props[i as usize].clone()).collect();
    let full = std::mem::replace(&mut sim.world.props, near);
    let ev0 = sim.world.fx.events.len();
    let mut rep = fn_hitreg::shoot::fire_at(gun, &mut sim.world, pawns, opts, shot, spread, ctx, seed);
    let near = std::mem::replace(&mut sim.world.props, full);
    let mut objects: Vec<(u64, u64)> = vec![];
    for imp in rep.impacts.iter_mut() {
        let Some(k) = imp.prop else { continue };
        let Some(&i) = cand.get(k) else { continue };
        let i = i as usize;
        let new = fn_combat::harvest_fx::object_id(i, &near[k]);
        objects.push((imp.object, new));
        imp.prop = Some(i);
        imp.object = new;
    }
    for (k, p) in near.into_iter().enumerate() { sim.world.props[cand[k] as usize] = p; }
    for e in sim.world.fx.events[ev0..].iter_mut() {
        if let fn_combat::harvest_fx::FxEvent::Broke { object, .. } = e { if let Some(n) = objects.iter().find(|o| o.0 == *object) { *object = n.1; } }
    }
    rep
}

fn hurt_player(arms: &mut Arms, dmg: f32, from: Vec3, name: &str, cause: DeathCause) {
    let mut v = Vitals::new(arms.health.max(0.01), arms.shield);
    let a = v.apply_damage(dmg);
    arms.shield = (arms.shield - a.shield_damage).max(0.);
    arms.health = (arms.health - a.health_damage).max(0.);
    crate::hurt_hook::on_damage(a.health_damage <= 0. && a.shield_damage > 0., Vec3::from(fn_particles::sim::game_to_ue(from.to_array())));
    crate::kill_feed_hook::local_damaged(cause, Killer::Player(Combatant::other(name)), arms.health);
}

fn apply_hit(st: &mut State, ti: usize, h: &PawnHit, from_id: u64, from_pos: Vec3, killer: Killer, cause: DeathCause) {
    let p = &mut st.pawns[ti];
    if !p.bot.core.alive { return; }
    p.bot.core.shield = (p.bot.core.shield - h.applied.shield_damage).max(0.);
    p.bot.core.health -= h.applied.health_damage;
    p.bot.core.damaged_by.push((from_id, from_pos));
    let mut s = p.seed ^ (st.clock.to_bits() as u64);
    p.react = Some((crate::death_hook::pick_react(&mut s), 0.));
    if from_id == PLAYER_ID && h.damage > 0. {
        if let Ok(mut q) = NUMBERS.lock() { for n in &h.numbers { if n.damage > 0 { q.push((n.location, n.damage as u32, n.critical, n.kind as u8)); } } }
    }
    if p.bot.core.health <= 0. { p.killer = Some(from_id); kill(st, ti, killer, cause); }
}

fn kill(st: &mut State, ti: usize, killer: Killer, cause: DeathCause) {
    let p = &mut st.pawns[ti];
    if !p.bot.core.alive { return; }
    p.bot.core.alive = false;
    p.bot.core.health = 0.;
    p.death_t = Some(0.);
    let name = p.bot.core.name.clone();
    let pos = p.bot.core.feet;
    let inv = std::mem::take(&mut p.inv);
    st.drops.push((pos, inv));
    if let Some(k) = p.killer { if let Some(kp) = st.pawns.iter_mut().find(|q| q.bot.core.id == k) { kp.bot.core.last_kill = Some((pos, st.clock)); } }
    crate::kill_feed_hook::push(Elimination { killer, victim: Combatant::other(&name), cause, dbno: false });
}

fn bones_of(rig: &fn_pose::CharacterRig, pose: &CharacterPose, world: Mat4) -> (Option<Vec3>, Option<Vec3>) {
    let Some((part, p)) = rig.parts.first().zip(pose.parts.first()) else { return (None, None) };
    let m = crate::mp_hook::ue_bone_to_mesh();
    let at = |b: &str| part.tree.bone_index(b).and_then(|i| p.bone_mesh.get(i)).map(|x| (world * *x * m).w_axis.truncate());
    (at(crate::elim_fx::HEAD_SOCKET_BONE), at(crate::elim_fx::ROOT_SOCKET_BONE))
}

fn start_outfits(seed: u64) -> OutfitRx {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut out = 0usize;
        let mut female_clips = None;
        let set = crate::hitreg_hook::install_dir().and_then(|d| fn_pak::PakSet::open_install(&d).ok());
        if let Some(set) = set {
            let cat = fn_pose::cosmetics::Catalog::load(&set);
            let mut cids: Vec<(String, String)> = cat.items.iter().filter(|c| c.category == fn_pose::cosmetics::Category::Outfit && !c.test_asset && c.id.contains("Athena_Commando_")).filter_map(|c| c.hero.clone().map(|h| (c.id.clone(), h))).collect();
            let mut s = seed;
            for i in (1..cids.len()).rev() { let j = (rand01(&mut s) * (i + 1) as f32) as usize % (i + 1); cids.swap(i, j); }
            for (cid, hero) in cids {
                if out >= OUTFITS { break; }
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fn_pose::br_character::with_cooked_only(|| fn_pose::br_character::BrCharacter::load(&fn_pose::ClipSourceDir::Pak(&set), &hero))));
                match r {
                    Ok(Ok(ch)) => {
                        if ch.female && female_clips.is_none() { match crate::br_character::female_clip_store(&set) { Ok(c) => female_clips = Some(c), Err(e) => { eprintln!("[bots] outfit {cid}: female animation set unavailable ({e})"); continue; } } }
                        eprintln!("[bots] outfit {cid} ({hero}) decoded{}", if ch.female { " (female)" } else { "" });
                        let item = (cid, crate::br_character::from_character(&ch), ch.female);
                        drop(ch);
                        if tx.send((Some(item), female_clips.clone())).is_err() { return; }
                        out += 1;
                    }
                    Ok(Err(e)) => eprintln!("[bots] outfit {cid}: {e}"),
                    Err(_) => eprintln!("[bots] outfit {cid}: loader panicked"),
                }
            }
        }
        let _ = tx.send((None, female_clips));
    });
    rx
}

fn held_index(core: Option<&str>) -> Option<&'static str> { core.and_then(|c| CORE_ANIMS_MALE.iter().find(|(k, _)| *k == c).map(|(k, _)| *k)) }

fn draw(st: &mut State, scene: &mut Scene, arms: &Arms, eye: Vec3, fwd: Vec3, dt: f32) {
    let (b0, h0, p0) = scene.pawn_models();
    let setups = BODIES.get().filter(|s| !s.is_empty());
    let to_mesh = crate::mp_hook::ue_bone_to_mesh();
    let death_data = arms.data.death.as_ref();
    st.frame_no += 1;
    let frame_no = st.frame_no;
    let mut cand: Vec<(f32, usize)> = vec![];
    for (i, p) in st.pawns.iter().enumerate() {
        if p.bot.core.body == Body::Bus || (!p.bot.core.alive && p.death_t.map_or(true, |t| t > crate::death_hook::HIDE_AFTER_S + 2.)) { continue; }
        let d = p.bot.core.feet.distance(eye);
        if d > DRAW_M { continue; }
        let to = (p.bot.core.feet + Vec3::Y - eye).normalize_or_zero();
        let in_view = d < 6. || to.dot(fwd) > 0.35;
        if !in_view && p.gfx.as_ref().map_or(false, |g| g.logged) { continue; }
        let want = if d < 12. { 1. / 60. } else if d < 30. { 1. / 30. } else if d < 60. { 1. / 15. } else if d < 120. { 1. / 8. } else { 1. / 4. };
        let urgent = p.gfx.as_ref().map_or(true, |g| !g.logged) || st.spec.map_or(false, |s| s.0 == i);
        let stale = (p.anim_dt + dt) / want;
        if urgent || stale >= 1. { cand.push((if urgent { 1e9 } else { stale / (1. + d * 0.01) }, i)); }
    }
    cand.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut chosen = vec![false; st.pawns.len()];
    for (k, (pr, i)) in cand.iter().enumerate() { if k < POSE_BUDGET || *pr >= 1e9 { chosen[*i] = true; } }
    let mut pose_ms = 0f32;
    let mut fresh = 0usize;
    for (pi, p) in st.pawns.iter_mut().enumerate() {
        let visible_body = p.bot.core.body != Body::Bus && p.bot.core.feet.distance(eye) < DRAW_M;
        let dying = !p.bot.core.alive && p.death_t.map_or(false, |t| t < crate::death_hook::HIDE_AFTER_S + 2.);
        if !visible_body || (!p.bot.core.alive && !dying) {
            if let Some(g) = p.gfx.take() { st.pool.push((g.outfit, (g.body, g.head, g.pick))); }
            p.bodies.clear();
            continue;
        }
        if p.gfx.as_ref().is_some_and(|g| g.outfit == 0 && !st.outfits.is_empty()) { if let Some(g) = p.gfx.take() { st.pool.push((g.outfit, (g.body, g.head, g.pick))); } }
        if p.gfx.is_none() && fresh >= NEW_GFX_PER_FRAME && st.spec.map_or(true, |s| s.0 != pi) { continue; }
        if p.gfx.is_none() {
            fresh += 1;
            let tgx = std::time::Instant::now();
            let outfit = if st.outfits.is_empty() { 0 } else { 1 + (p.seed as usize % st.outfits.len()) };
            let (ob, oh) = if outfit == 0 { (b0, h0) } else { (st.outfits[outfit - 1].body, st.outfits[outfit - 1].head) };
            let (body, head, pick) = match st.pool.iter().position(|e| e.0 == outfit) { Some(i) => st.pool.swap_remove(i).1, None => (scene.renderer.instance_model(ob), scene.renderer.instance_model(oh), scene.renderer.instance_model(p0)) };
            crate::elim_fx::set_dissolve(&scene.renderer, &[body, head], None);
            let mut anim = AnimState::new();
            if outfit > 0 && st.outfits[outfit - 1].female { anim.set_female(true); }
            let still = PawnSample { velocity: (0., 0.), input: (0., 0.), facing_deg: p.bot.core.yaw, camera_yaw_deg: p.bot.core.yaw, camera_pitch_deg: 0., height: 0., vertical_speed: 0., fall_time_left: 0., crouched: false, running: false, melee_swinging: false };
            for _ in 0..30 { anim.step_sample(&still, 1. / 30.); }
            p.gfx = Some(Gfx { anim, pose: CharacterPose::default(), body, head, pick, logged: false, outfit });
            let ms = tgx.elapsed().as_secs_f32() * 1000.; if ms > 2. { eprintln!("[bots] {} body instance took {ms:.1} ms", p.bot.core.name); }
        }
        let dist = p.bot.core.feet.distance(eye);
        let far = dist > 90.;
        let to = (p.bot.core.feet + Vec3::Y - eye).normalize_or_zero();
        let in_view = dist < 6. || to.dot(fwd) > 0.35;
        let g = p.gfx.as_mut().unwrap();
        let c = &p.bot.core;
        let death_t = p.death_t.filter(|_| !c.alive);
        let gun = c.equipped.and_then(|i| c.weapons.get(i)).and_then(gun_index);
        let melee = c.equipped.and_then(|i| c.weapons.get(i)).map_or(true, |w| w.class == WeaponClass::Melee);
        let core = gun.and_then(|gi| arms.data.guns.get(gi)).map(|gd| gd.core);
        let aerial = matches!(c.body, Body::Skydive | Body::Parachute);
        g.anim.set_held(if death_t.is_some() || aerial { None } else { held_index(core).map(|k| (k, c.intent.ads)) });
        if aerial {
            let cm = p.ctl.component();
            let y = c.yaw.to_radians();
            let (f, r) = (Vec3::new(y.sin(), -y.cos(), 0.), Vec3::new(y.cos(), y.sin(), 0.));
            let local = |v: Vec3| [v.dot(f), v.dot(r), v.z];
            g.anim.set_aerial(Some(fn_animgraph::AerialAnim { skydiving: cm.is_skydiving(), parachute_open: cm.is_parachute_open(), from_launch_pad: false, local_acceleration: local(cm.air.accel_snapshot), local_velocity: local(cm.velocity) }));
        } else { g.anim.set_aerial(None); }
        let v2 = if death_t.is_some() { Vec2::ZERO } else { p.ctl.velocity };
        let input = if v2.length() > 0.3 { v2.normalize() } else { Vec2::ZERO };
        let air = p.ctl.air_height();
        let airborne = death_t.is_none() && p.ctl.airborne();
        let sample = PawnSample { velocity: (v2.x, v2.y), input: (input.x, input.y), facing_deg: p.ctl.facing, camera_yaw_deg: c.yaw, camera_pitch_deg: if death_t.is_some() { 0. } else { c.pitch },
            height: if airborne { air.max(1e-4) } else { 0. }, vertical_speed: if airborne { p.ctl.vertical_speed } else { 0. }, fall_time_left: if airborne { p.ctl.fall_time_left() } else { 0. },
            crouched: p.ctl.crouched && death_t.is_none(), running: death_t.is_none() && p.ctl.running, melee_swinging: false };
        p.pose_n = p.pose_n.wrapping_add(1);
        p.anim_dt += dt;
        let urgent_pose = g.logged == false || st.spec.map_or(false, |s| s.0 == pi);
        let skip_pose = !chosen[pi] || (!urgent_pose && pose_ms >= POSE_MS);
        if !skip_pose { g.anim.step_sample(&sample, p.anim_dt.max(1e-4)); p.anim_dt = 0.; }
        let feet = c.feet;
        let mut montages = vec![];
        if let (Some(t), Some(dd)) = (death_t, death_data) {
            let first = t <= dt + 1e-4;
            if first { p.fire = None; p.react = None; crate::elim_fx::on_death(Mat4::from_rotation_translation(Quat::from_rotation_y((180. - g.anim.visual_yaw(c.yaw)).to_radians()), feet) * to_mesh); }
            montages.extend(crate::death_hook::montage_at(&dd.death, t));
        }
        if let Some((pick, t)) = p.react.as_mut() {
            *t += dt;
            match death_data.and_then(|dd| dd.reacts.get(*pick as usize)).and_then(|m| crate::death_hook::montage_at(m, *t)) { Some(m) => montages.push(m), None => p.react = None }
        }
        if let Some((gi, t, ads)) = p.fire.as_mut() {
            *t += dt;
            let gd = arms.data.guns.get(*gi);
            match gd.and_then(|d| crate::death_hook::montage_at(if *ads { d.fire_ads.as_ref().unwrap_or(&d.fire) } else { &d.fire }, *t)) { Some(m) => montages.push(m), None => p.fire = None }
        }
        let slot = (SLOT_BASE + p.idx).min(255) as u8;
        let want_emote = c.intent.emote.filter(|_| death_t.is_none());
        if want_emote != p.emote_on { match want_emote { Some(k) => p.emote.on_net(fn_net::proto::emote_op::PLAY, (k % 4) as u8, slot), None => p.emote.on_net(fn_net::proto::emote_op::STOP, 0, slot) } p.emote_on = want_emote; }
        if death_t.is_none() { let (m, _) = p.emote.step(dt, feet, slot, true); montages.extend(m); }
        let rig = if g.outfit == 0 { &scene.character.rig } else { &st.outfits[g.outfit - 1].rig };
        if !skip_pose {
            let tree = g.anim.plan(&montages);
            let female = g.outfit > 0 && st.outfits[g.outfit - 1].female;
            let lib = BotClips { base: &*scene.character.clips, extra: if female { st.female_clips.as_deref() } else { None } };
            let t = std::time::Instant::now();
            rig.evaluate_into(&tree, &lib, &mut g.pose);
            pose_ms += t.elapsed().as_secs_f32() * 1000.;
        }
        g.logged = true;
        let facing = g.anim.visual_yaw(c.yaw);
        let world = Mat4::from_rotation_translation(Quat::from_rotation_y((180. - facing).to_radians()), feet);
        let skin = |i: usize| if skip_pose { &[][..] } else { g.pose.parts.get(i).map_or(&[][..], |pp| &pp.skin_matrices[..]) };
        if !skip_pose || dist < 40. || (frame_no + pi as u64) % 3 == 0 {
            scene.renderer.update_model(g.body, world, skin(0));
            scene.renderer.update_model(g.head, world, skin(1));
        }
        if let Some(t) = death_t { let (h, r) = bones_of(rig, &g.pose, world); crate::elim_fx::remote_dissolve(&scene.renderer, &[g.body, g.head], h, r, Some(t)); }
        scene.models.push(g.body);
        scene.models.push(g.head);
        let socket = g.pose.socket("RightHand").unwrap_or(Mat4::IDENTITY);
        if death_t.is_none() && !aerial {
            if melee { scene.renderer.update_model(g.pick, world, &[socket]); scene.models.push(g.pick); }
            else if let Some(gi) = gun { let tg = std::time::Instant::now(); crate::gun_orig::remote(scene, SLOT_BASE + p.idx, gi, world * socket); crate::gunfx_hook::set_remote_attach(slot, world * socket); let ms = tg.elapsed().as_secs_f32() * 1000.; if ms > 2. { eprintln!("[bots] gun {gi} model for {} took {ms:.1} ms", c.name); } }
        }
        if skip_pose {
            let delta = feet - p.bodies_at;
            if delta.length_squared() > 1e-6 { for b in p.bodies.iter_mut() { b.a += delta; b.b += delta; if let Some(m) = b.obb.as_mut() { m.w_axis += delta.extend(0.); } } p.bodies_at = feet; }
            continue;
        }
        p.bodies.clear();
        p.bodies_at = feet;
        let rig = if g.outfit == 0 { &scene.character.rig } else { &st.outfits[g.outfit - 1].rig };
        if let (Some(setups), Some(part), Some(pose), true) = (setups, rig.parts.first(), g.pose.parts.first(), death_t.is_none() && !far) {
            p.bodies = fn_hitreg::shoot::place_bodies(setups, |bone| part.tree.bone_index(bone).and_then(|i| pose.bone_mesh.get(i)).map(|m| world * *m * to_mesh));
        }
    }
    st.prof[6] += pose_ms;
}

trait LandingName { fn landing_name(&self, pois: &[Poi]) -> Option<String>; }
impl LandingName for fn_bots::brain::Core { fn landing_name(&self, pois: &[Poi]) -> Option<String> { self.poi.and_then(|i| pois.get(i)).map(|p| p.name.clone()) } }
