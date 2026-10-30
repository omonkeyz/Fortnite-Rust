//! PICKUPS HOOK (fn-app): world items of Battle Royale (`AFortPickupAthena`) in the sandbox. The LOGIC is `fn_game::pickups` (toss physics, touch / Use pickup, the flight to the pawn),
//! the DATA is `fn_game::pickup_items` (the item definitions: `PickupStaticMesh` / `PickupSkeletalMesh`, `PickupSound`, `DropSound`, rarity, ...), the displayed mesh is
//! `pickup_mesh.rs`, the rarity effects are `pickups_fx.rs`. This file connects them to the app:
//!
//! * [`requests`]   the console's `spawn <item> [count]` (`fn_console::Request::SpawnPickup`) queues a spawn; [`queue_drop`] is the same for an item the player drops (inventory drop / the
//!   item a full-bar pickup replaced): it is tossed from the pawn and `PawnWhoDroppedPickup` = the pawn, so it is not auto picked up again.
//! * [`update`]     once per simulation step: resolves queued spawns (a mesh worker loads the item's mesh and textures first), runs the pickup world, plays the sounds, takes the Use key
//!   (`E`) for the pickup in front of the player, auto picks up ammo / traps / resources by touch while moving, and DELIVERS the arrived items to the inventory (weapons through the
//!   weapon runtime, consumables / ammo through the console's `Request`s, resources into the sim's counters).
//! * [`frame`]      once per rendered frame: uploads / reuses one renderer model per pickup and places it (`Scene::models`).
//! * [`prompt`]     the interaction prompt of the focused pickup (`HudWorld::interaction`).
//!
//! ASSUMED / NOT RECOVERABLE (the spawn side is server code, absent from the client exe): where a drop starts and lands (the pawn's capsule centre, 5.12 m ahead = the exe's
//! `DAT_143ff904c`), the random yaw of a pickup, the contents / counts of a console spawn (`count` = the stack of the one actor; ammo defaults to the definition's `DropCount`).
//! The flight to the pawn follows the native Catmull-Rom spline into the `LootEntryPosition` socket; items are given to the inventory when the flight ends.
use crate::arms::Arms;
use fn_audio_sink::{AudioSink, SoundEvent};
use fn_game::pickup_items::{self, Gameplay, ItemDef, Resource};
use fn_game::pickups::{Event, PawnState, Phase, PickupWorld, FLY_CURVE_SPAN, PAWN_HALF_HEIGHT, TOUCH_HALF_HEIGHT};
use fn_game::{GameSim, EYE};
use fn_render::ModelId;
use glam::{Mat4, Vec3};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// A console / inventory request waiting for the item catalog and the item's mesh.
struct Want { query: String, count: Option<u32>, dropped: bool, /** CONTAINERS: (start, final) of a container's loot toss, game metres (None = from the pawn) */ toss: Option<(Vec3, Vec3)>, net: Option<(u32, Vec3, Vec3, bool)> }
/// A resolved want waiting for its mesh.
struct Resolved { item: usize, count: u32, dropped: bool, toss: Option<(Vec3, Vec3)>, net: Option<(u32, Vec3, Vec3, bool)>, rest: Option<(Vec3, u64)> }

enum Asset { Loading, Ready(Arc<crate::pickup_mesh::MeshAsset>), Failed(String) }

struct Slot { model: ModelId, item: usize }

struct State {
    install: Option<std::path::PathBuf>,
    items: Vec<ItemDef>,
    scan_rx: Option<Receiver<Vec<ItemDef>>>,
    wants: Vec<Want>,
    resolved: Vec<Resolved>,
    assets: HashMap<usize, Asset>,
    load_tx: Sender<(usize, Result<crate::pickup_mesh::MeshAsset, String>)>,
    load_rx: Receiver<(usize, Result<crate::pickup_mesh::MeshAsset, String>)>,
    world: PickupWorld,
    slots: HashMap<u64, Slot>,
    free: Vec<Slot>,
    focus: Option<u64>,
    use_was_down: bool,
    /// Lines for the F2 console (is_error, text).
    lines: Vec<(bool, String)>,
    log: Vec<String>,
    time: f32,
    /// Scripted-run test aids (never set in play): `FNRT_PICKUP_SPAWN="query[:count]@frame;..."` queues spawns, `FNRT_PICKUP_USE="frame;..."` holds the Use key for 3 frames from each.
    aid_spawn: Vec<(u32, String, u32)>,
    aid_use: Vec<u32>,
    aid_read: bool,
    picked_up: Vec<fn_ui::screens::pickup_stream::PickedUp>,
    net_mode: bool,
    net_ids: HashMap<u64, u32>,
    claims: Vec<u32>,
    claimed: std::collections::HashSet<u32>,
    net_out: Vec<NetDrop>,
    early_gone: Vec<(u32, bool)>,
    pending_events: Vec<Event>,
    net_taken: std::collections::HashSet<u64>,
    rest_wants: Vec<(String, u32, Vec3, u64)>,
    load_queue: std::collections::VecDeque<usize>,
    in_flight: usize,
    tags: HashMap<u64, u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetDrop { pub item: String, pub count: u32, pub start: Vec3, pub end: Vec3 }

static HOST: Mutex<Option<State>> = Mutex::new(None);

fn host<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = HOST.lock().unwrap_or_else(|p| p.into_inner());
    let st = g.get_or_insert_with(|| { let (load_tx, load_rx) = channel(); State { install: None, items: vec![], scan_rx: None, wants: vec![], resolved: vec![], assets: HashMap::new(), load_tx, load_rx, world: PickupWorld::new(0x51_7C_C1), slots: HashMap::new(), free: vec![], focus: None, use_was_down: false, lines: vec![], log: vec![], time: 0., aid_spawn: vec![], aid_use: vec![], aid_read: false, picked_up: vec![], net_mode: false, net_ids: HashMap::new(), claims: vec![], claimed: Default::default(), net_out: vec![], early_gone: vec![], pending_events: vec![], net_taken: Default::default(), rest_wants: vec![], load_queue: Default::default(), in_flight: 0, tags: HashMap::new() } });
    f(st)
}
fn say(st: &mut State, err: bool, s: String) { eprintln!("[pickups] {s}"); st.lines.push((err, s.clone())); if st.log.len() < 5000 { st.log.push(s); } }

/// The game install (called once with the loadout resources): starts the background scan of the item definitions.
pub fn init(install: Option<std::path::PathBuf>) {
    host(|st| {
        if st.scan_rx.is_some() || !st.items.is_empty() { return; }
        st.install = install;
        let (tx, rx) = channel();
        st.scan_rx = Some(rx);
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let items = match pickup_items::catalog() { Ok(c) => (*c).clone(), Err(e) => { eprintln!("[pickups] item catalog unavailable: {e}"); vec![] } };
            eprintln!("[pickups] {} item definitions read in {:.2} s", items.len(), t.elapsed().as_secs_f32());
            let _ = tx.send(items);
        });
    });
}

/// The game install the item catalog was scanned from (the pickup effect materials are read from its paks).
pub fn install() -> Option<std::path::PathBuf> { host(|st| st.install.clone()) }

/// `spawn <item> [count]` requests of one console command; returns nothing to print (the answers come through [`take_lines`]).
pub fn requests(reqs: &[fn_console::Request]) {
    for r in reqs { if let fn_console::Request::SpawnPickup { query, count } = r { host(|st| { if st.scan_rx.is_none() && st.items.is_empty() { init(None); } st.wants.push(Want { query: query.clone(), count: Some(*count), dropped: false, toss: None, net: None }); }); } }
}
/// A drop: the item (asset name or display name) tossed from the pawn (`ServerSpawnInventoryDrop`). Used by the inventory drop and by the swap of a full bar.
pub fn queue_drop(item: &str, count: u32) { host(|st| { if st.scan_rx.is_none() && st.items.is_empty() { init(None); } st.wants.push(Want { query: item.rsplit('/').next().unwrap_or(item).split('.').next().unwrap_or(item).to_owned(), count: Some(count.max(1)), dropped: true, toss: None, net: None }); }); }
/// CONTAINERS (2026-10-08): a container's loot item (asset name or object path) tossed from `start` to `final_pos` (game metres): `count` is the stack as rolled
/// (no `DropCount` substitution), not dropped by the pawn (it may be picked up by touch at once).
pub fn queue_toss(item: &str, count: u32, start: Vec3, velocity: Vec3) { host(|st| { if st.scan_rx.is_none() && st.items.is_empty() { init(None); } st.wants.push(Want { query: item.rsplit('/').next().unwrap_or(item).split('.').next().unwrap_or(item).to_owned(), count: Some(count.max(1)), dropped: false, toss: Some((start, velocity)), net: None }); }); }
pub fn bot_death_toss(item: &str, count: u32, start: Vec3, end: Vec3) { queue_toss(item, count, start, fn_game::pickups::launch_velocity(start, end, fn_game::pickups::FLY_CURVE_SPAN, fn_game::pickups::ARC_DEGREES)); }
pub fn queue_rest(item: &str, count: u32, pos: Vec3, tag: u64) { host(|st| { if st.scan_rx.is_none() && st.items.is_empty() { init(None); } st.rest_wants.push((item.rsplit('/').next().unwrap_or(item).split('.').next().unwrap_or(item).to_owned(), count.max(1), pos, tag)); }); }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagState { Pending, Resting, Moved, Gone }
pub fn tag_state(tag: u64) -> TagState {
    host(|st| {
        if st.rest_wants.iter().any(|w| w.3 == tag) || st.resolved.iter().any(|r| r.rest.is_some_and(|x| x.1 == tag)) { return TagState::Pending; }
        let Some(id) = st.tags.get(&tag).copied() else { return TagState::Gone };
        match st.world.get(id) { Some(p) if matches!(p.phase, Phase::World) && p.toss == fn_game::pickups::TossState::AtRest && p.age > 0. => TagState::Resting, Some(_) => TagState::Moved, None => TagState::Gone }
    })
}
pub fn remove_tagged(tag: u64) -> Result<Option<u32>, ()> {
    host(|st| {
        let pending = st.rest_wants.iter().find(|w| w.3 == tag).map(|w| w.1).or_else(|| st.resolved.iter().find(|r| r.rest.is_some_and(|x| x.1 == tag)).map(|r| r.count));
        if pending.is_some() { st.rest_wants.retain(|w| w.3 != tag); st.resolved.retain(|r| !r.rest.is_some_and(|x| x.1 == tag)); return Ok(pending); }
        let Some(id) = st.tags.remove(&tag) else { return Ok(None) };
        match st.world.get(id).map(|p| (p.phase, p.count)) { Some((Phase::World, n)) => { st.world.pickups.retain(|q| q.id != id); if st.focus == Some(id) { st.focus = None; } Ok(Some(n)) } Some(_) => { st.tags.insert(tag, id); Err(()) } None => Ok(None) }
    })
}
pub fn set_net_mode(on: bool) { host(|st| { if st.net_mode && !on { let ids: Vec<u64> = st.net_ids.keys().copied().collect(); st.world.pickups.retain(|p| !ids.contains(&p.id)); st.net_ids.clear(); st.claimed.clear(); st.claims.clear(); st.net_out.clear(); st.early_gone.clear(); } st.net_mode = on; }); }
pub fn net_mode() -> bool { host(|st| st.net_mode) }
pub fn net_spawn(id: u32, item: &str, count: u32, start: Vec3, end: Vec3, dropped_by_me: bool) { host(|st| { if st.scan_rx.is_none() && st.items.is_empty() { init(None); } if st.net_ids.values().any(|n| *n == id) || st.wants.iter().any(|w| w.net.is_some_and(|n| n.0 == id)) { return; } st.wants.push(Want { query: item.to_owned(), count: Some(count.max(1)), dropped: dropped_by_me, toss: None, net: Some((id, start, end, dropped_by_me)) }); }); }
pub fn net_gone(id: u32, mine: bool) -> bool { host(|st| net_gone_in(st, id, mine)) }
fn net_gone_in(st: &mut State, id: u32, mine: bool) -> bool {
    st.claimed.remove(&id);
    let Some(local) = st.net_ids.iter().find(|(_, n)| **n == id).map(|(l, _)| *l) else { if st.wants.iter().any(|w| w.net.is_some_and(|n| n.0 == id)) || st.resolved.iter().any(|r| r.net.is_some_and(|n| n.0 == id)) { st.early_gone.push((id, mine)); } return false };
    st.net_ids.remove(&local);
    if mine { st.net_taken.insert(local); if let Some(e) = st.world.begin_pickup(local, false, None) { st.pending_events.push(e); } return true; }
    st.world.pickups.retain(|p| p.id != local);
    true
}
pub fn take_claims() -> Vec<u32> { host(|st| std::mem::take(&mut st.claims)) }
pub fn take_net_drops() -> Vec<NetDrop> { host(|st| std::mem::take(&mut st.net_out)) }
fn claim(st: &mut State, local: u64) -> bool { let Some(n) = st.net_ids.get(&local).copied() else { return false }; if st.claimed.insert(n) { st.claims.push(n); } true }
pub fn take_picked_up() -> Vec<fn_ui::screens::pickup_stream::PickedUp> { host(|st| std::mem::take(&mut st.picked_up)) }
fn picked(st: &mut State, def: &ItemDef, count: u32) { if count > 0 { st.picked_up.push(fn_ui::screens::pickup_stream::PickedUp { name: def.name.clone(), icon: def.small_icon.clone().unwrap_or_default(), count }); } }
/// Console lines produced since the last call: (is_error, text).
pub fn bot_list(center: Vec3, r: f32, out: &mut Vec<(u64, Vec3, String, bool)>) {
    host(|st| for p in &st.world.pickups {
        if !matches!(p.phase, Phase::World) || (p.pos - center).length() > r { continue; }
        let name = st.items.get(p.item as usize).map(|d| d.id.clone()).unwrap_or_default();
        let weapon = name.starts_with("WID_") && !name.contains("Harvest");
        out.push((p.id, p.pos, name, weapon));
    })
}
pub fn bot_take(id: u64) -> Option<(String, u32)> {
    host(|st| {
        let p = st.world.get(id)?;
        if !matches!(p.phase, Phase::World) { return None; }
        let name = (st.items.get(p.item as usize).map(|d| d.id.clone()).unwrap_or_default(), p.count);
        st.world.pickups.retain(|q| q.id != id);
        if st.focus == Some(id) { st.focus = None; }
        st.tags.retain(|_, v| *v != id);
        Some(name)
    })
}
pub fn take_lines() -> Vec<(bool, String)> { host(|st| std::mem::take(&mut st.lines)) }
/// FIRST USE (2026-10-06): builds the held pickaxe's original-material model (gun pass, `pickup_orig`) under the loading screen; its first draw built it on
/// the first in-game frame (35-145 ms, verification/first_use/run5 / run6). The scratch draw is dropped (`begin_frame`).
pub fn warm_pickaxe(scene: &mut crate::scene::Scene) {
    if scene.orig_guns.is_none() || !crate::building_render::warm_enabled() { return; }
    let mesh = scene.pick_mesh.clone();
    crate::pickup_orig::begin_frame();
    let _ = crate::pickup_orig::draw(&mut scene.renderer, scene.orig_guns.as_mut(), true, &mesh, true, Mat4::IDENTITY);
    crate::pickup_orig::begin_frame();
}
/// FIRST USE (2026-10-06): the displayed mesh of every item of the catalog, once (path, skeletal), for the effect pass' warm-up (`pickups_fx::prebuild_step`).
/// `None` while the catalog is still being read.
pub fn item_meshes() -> Option<Vec<(String, bool)>> {
    host(|st| {
        if let Some(rx) = &st.scan_rx { if let Ok(items) = rx.try_recv() { st.items = items; st.scan_rx = None; } }
        if st.scan_rx.is_some() { return None; }
        let mut out: Vec<(String, bool)> = vec![];
        for d in &st.items { let m = d.skeletal_mesh.clone().map(|m| (m, true)).or_else(|| d.static_mesh.clone().map(|m| (m, false))); if let Some(m) = m { if !out.contains(&m) { out.push(m); } } }
        Some(out)
    })
}
/// CONTAINERS (2026-10-08): item asset name -> rarity byte of the catalog (None while it is still being read).
pub fn item_rarities() -> Option<HashMap<String, u8>> {
    host(|st| {
        if let Some(rx) = &st.scan_rx { if let Ok(items) = rx.try_recv() { st.items = items; st.scan_rx = None; } }
        if st.scan_rx.is_some() { return None; }
        Some(st.items.iter().map(|d| (d.id.clone(), d.rarity)).collect())
    })
}
/// Number of pickup actors alive (scripted runs / tests).
pub fn live() -> usize { host(|st| st.world.len()) }
/// DOOR HOOK: a pickup is focused this frame (it keeps the Use key).
pub fn has_focus() -> bool { host(|st| st.focus.is_some()) }
/// The pickup world (read-only snapshot for the effect pass): (id, item index, position, rarity, state).
pub fn snapshot() -> Vec<Snap> {
    let loot = loot_point();
    host(|st| st.world.pickups.iter().map(|p| Snap {
        picked_t: if let Phase::Flying { t, .. } = p.phase { Some(t) } else { None },
        black_hole: if let Phase::Flying { t, fly_time, .. } = p.phase { loot.map(|l| { let u = fn_particles::sim::game_to_ue([l.x, l.y, l.z]); [u.x, u.y, u.z, fn_game::pickups::black_hole_strength(fn_game::pickups::curve_time(t.max(0.), fly_time))] }) } else { None },
        id: p.id, item: p.item as usize, pos: p.pos, rarity: p.rarity, yaw: p.yaw, flying: matches!(p.phase, Phase::Flying { .. }), age: p.age, tossing: p.toss == fn_game::pickups::TossState::InProgress, resource: matches!(st.items.get(p.item as usize).map(|d| &d.gameplay), Some(Gameplay::Resource(_))),
        radius_cm: st.assets.get(&(p.item as usize)).and_then(|a| if let Asset::Ready(a) = a { Some(a.radius_cm) } else { None }),
        origin_cm: st.assets.get(&(p.item as usize)).and_then(|a| if let Asset::Ready(a) = a { Some(a.origin_cm) } else { None }),
        mesh: st.items.get(p.item as usize).and_then(|d| d.skeletal_mesh.clone().map(|m| (m, true)).or_else(|| d.static_mesh.clone().map(|m| (m, false)))) }).collect())
}
#[derive(Clone, Debug)]
pub struct Snap { pub picked_t: Option<f32>, pub black_hole: Option<[f32; 4]>, pub id: u64, pub item: usize, pub pos: Vec3, pub rarity: u8, pub yaw: f32, pub flying: bool, pub age: f32, pub tossing: bool, pub resource: bool, pub radius_cm: Option<f32>,
    /// `Bounds.Origin` of the displayed mesh (UE cm, mesh space): the Blueprint moves the mesh so that this point sits at the actor location.
    pub origin_cm: Option<[f32; 3]>,
    /// The item definition's displayed mesh (`PickupSkeletalMesh` first, else `PickupStaticMesh`) and whether it is skeletal.
    pub mesh: Option<(String, bool)> }

fn gravity_of(sim: &GameSim) -> f32 {
    let p = fn_movement::cmc::Params::default().with_tuning(&sim.tuning);
    (p.world_gravity_z * p.gravity_scale).abs() / 100.
}

static LOOT_POINT: Mutex<Option<Vec3>> = Mutex::new(None);
pub fn set_loot_point(p: Vec3) { *LOOT_POINT.lock().unwrap_or_else(|e| e.into_inner()) = Some(p); }
fn loot_point() -> Option<Vec3> { *LOOT_POINT.lock().unwrap_or_else(|e| e.into_inner()) }

fn pawn_state(sim: &GameSim) -> PawnState {
    let feet = sim.feet();
    let c = &sim.controller;
    let pos = Vec3::new(feet.x, feet.y + PAWN_HALF_HEIGHT, feet.z);
    PawnState { pos, eye: sim.cam_view.eye, forward: sim.cam_view.forward, moving: c.velocity.length_squared() > 1e-8 || c.vertical_speed.abs() > 1e-4, loot: loot_point().unwrap_or(pos) }
}

/// Why the pawn cannot take `item` now (the `FortPickup.GetInteractErrorText` strings): `None` = it can.
fn cannot_hold(def: &ItemDef, count: u32, arms: Option<&Arms>, sim: &GameSim) -> Option<&'static str> {
    const DEFAULT: &str = "Can't pickup this item!";
    const CAPACITY: &str = "Can't hold more of this item!";
    match &def.gameplay {
        Gameplay::Weapon => if arms.is_none() { Some(DEFAULT) } else if def.item_type == 17 { None } else { Some(DEFAULT) },
        Gameplay::Healable(idx) => match arms { Some(a) => a.heal_pickup_refusal(*idx, count), None => Some(DEFAULT) }, // INVENTORY HOOK: stack / backpack rules of `arms/inventory_ops.rs`
        Gameplay::Ammo => match arms { Some(a) => (a.ammo_count(&def.package) >= ammo_max(a, def)).then_some(CAPACITY), None => Some(DEFAULT) },
        Gameplay::Resource(r) => { let v = match r { Resource::Wood => sim.world.wood, Resource::Stone => sim.world.stone, Resource::Metal => sim.world.metal }; (v >= 999 && count > 0).then_some(CAPACITY) }
        Gameplay::Trap => crate::trap_hook::refusal(&def.id, count),
        Gameplay::Other => Some(DEFAULT),
    }
}
fn ammo_max(arms: &Arms, def: &ItemDef) -> u32 { arms.ammo_gun_of(&def.package).and_then(|g| arms.data.ammo.max_stack.get(g).copied()).unwrap_or(def.max_stack) }
fn owned_gun(arms: &Arms, def: &ItemDef) -> bool { arms.data.guns.iter().enumerate().any(|(g, d)| d.def.item.rsplit('/').next().unwrap_or("").split('.').next() == Some(def.id.as_str()) && arms.gun_loc(g).is_some()) }

/// True when taking a weapon would replace one (the five slots are full): `GetInteractText` shows "Swap equipped" then.
fn swaps(arms: Option<&Arms>) -> bool { arms.map_or(false, |a| a.pickup_swaps()) } // INVENTORY HOOK: full backpack + a replaceable slot

fn queue_load(st: &mut State, idx: usize) {
    if st.assets.contains_key(&idx) { return; }
    st.assets.insert(idx, Asset::Loading);
    st.load_queue.push_back(idx);
}
fn start_load(st: &mut State, idx: usize) {
    if let Some(k) = st.load_queue.iter().position(|&q| q == idx) { st.load_queue.remove(k); } else if st.assets.contains_key(&idx) { return; }
    st.assets.insert(idx, Asset::Loading);
    st.in_flight += 1;
    let def = st.items[idx].clone();
    let tx = st.load_tx.clone();
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let r = fn_game::cosmetic_swap::paks().and_then(|set| {
            // FIRST USE (2026-10-06): a weapon item is drawn with its original material in the gun pass (`pickup_orig`): its mesh, materials and textures are
            // prepared here so that the UI thread only uploads them (and the equip that follows finds them in the shared gun scene)
            if let Some(m) = &def.skeletal_mesh { if let Err(e) = fn_matshader::gun::prepare_skeletal(&set, m) { eprintln!("[pickups] {}: gun-pass prepare failed ({e}): built on first draw", def.id); } }
            crate::pickup_mesh::load(&set, &def)
        });
        if let Ok(a) = &r { eprintln!("[pickups] mesh of {} ({}): {} sections, radius {:.1} cm, {:.0} ms{}", def.id, a.source, a.prims.len(), a.radius_cm, t.elapsed().as_secs_f32() * 1000., if a.notes.is_empty() { String::new() } else { format!(", notes: {}", a.notes.join("; ")) }); }
        let _ = tx.send((idx, r));
    });
}

/// Where a toss starts and ends for the pawn: from the capsule centre, `DEFAULT_TOSS_DISTANCE` ahead along the view direction (flattened), landing at the resting height.
fn toss_points(sim: &GameSim) -> (Vec3, Vec3) {
    let p = pawn_state(sim);
    let mut f = Vec3::new(p.forward.x, 0., p.forward.z);
    if f.length_squared() < 1e-6 { let y = sim.yaw.to_radians(); f = Vec3::new(y.sin(), 0., -y.cos()); }
    let f = f.normalize();
    let end = p.pos + f * fn_live::f32("FN_TOSS_DROP_DISTANCE", fn_game::pickups::DEFAULT_TOSS_DISTANCE);
    (p.pos, Vec3::new(end.x, TOUCH_HALF_HEIGHT, end.z))
}

/// One simulation step (after `GameSim::step`). `use_down` = the Use binding is held, `key` its key text. Delivers arrived items; returns nothing (console lines: [`take_lines`]).
pub fn update(sim: &mut GameSim, mut arms: Option<&mut Arms>, weapons: &mut crate::weapon_runtime::WeaponRuntime, built: Option<&crate::console_hook::Built>, cards: Option<&mut [Option<fn_ui::screens::hud::HudItem>; 6]>, audio: &mut dyn AudioSink, dt: f32, use_down: bool) {
    let _ = (built, cards);
    let mut deliveries: Vec<(u64, usize, u32)> = vec![];
    let frame = sim.frames;
    let rest = rest_point(sim);
    let weapon_results = weapons.take_pickup_results();
    for w in crate::bots_hook::take_gun_wants() { if let Err(e) = weapons.request_prefetch(&w) { eprintln!("[bots] gun {w}: {e}"); crate::bots_hook::gun_failed(&w); } }
    host(|st| {
        st.time += dt;
        for e in std::mem::take(&mut st.pending_events) { handle(st, e, audio, sim); }
        for (id, taken) in weapon_results {
            let Some(p) = st.world.get_arrived(id) else { continue };
            let name = st.items.get(p.item as usize).map(|d| d.name.clone()).unwrap_or_default();
            let count = p.count;
            if taken { if let Some(d) = st.items.get(p.item as usize).cloned() { picked(st, &d, count); } }
            st.world.settle(id, if taken { count } else { 0 }, rest);
            if st.net_taken.remove(&id) && !taken { net_requeue(st, id, rest); }
            say(st, !taken, if taken { format!("{name}: added to the inventory") } else { format!("{name}: not picked up, it stays on the ground") });
        }
        if !st.aid_read {
            st.aid_read = true;
            if let Ok(v) = std::env::var("FNRT_PICKUP_SPAWN") { for part in v.split(';').filter(|p| !p.trim().is_empty()) { let (q, f) = part.split_once('@').unwrap_or((part, "0")); let (q, c) = q.split_once(':').unwrap_or((q, "1")); st.aid_spawn.push((f.trim().parse().unwrap_or(0), q.trim().to_owned(), c.trim().parse().unwrap_or(1))); } }
            if let Ok(v) = std::env::var("FNRT_PICKUP_USE") { st.aid_use = v.split(';').filter_map(|p| p.trim().parse().ok()).collect(); }
        }
        let due: Vec<(u32, String, u32)> = st.aid_spawn.iter().filter(|a| frame >= a.0).cloned().collect();
        st.aid_spawn.retain(|a| frame < a.0);
        for (_, q, c) in due { st.wants.push(Want { query: q, count: Some(c), dropped: false, toss: None, net: None }); }
        // 1. the catalog, the mesh loads
        if let Some(rx) = &st.scan_rx { if let Ok(items) = rx.try_recv() { st.items = items; st.scan_rx = None; } }
        while let Ok((idx, r)) = st.load_rx.try_recv() {
            st.in_flight = st.in_flight.saturating_sub(1);
            let name = st.items.get(idx).map(|i| i.id.clone()).unwrap_or_default();
            match r { Ok(a) => { st.assets.insert(idx, Asset::Ready(Arc::new(a))); } Err(e) => { say(st, true, format!("{name}: the pickup mesh could not be loaded ({e}): it is shown without a mesh")); st.assets.insert(idx, Asset::Failed(e)); } }
        }
        if st.in_flight == 0 { if let Some(idx) = st.load_queue.pop_front() { st.assets.remove(&idx); start_load(st, idx); } }
        // 2. wants -> resolved (need the catalog)
        if !st.items.is_empty() || st.scan_rx.is_none() {
            for w in std::mem::take(&mut st.wants) {
                match pickup_items::resolve(&st.items, &w.query) {
                    Ok(def) => {
                        let idx = st.items.iter().position(|i| i.id == def.id).unwrap_or(0);
                        let count = w.count.unwrap_or(1).max(1);
                        let count = if w.count == Some(1) && matches!(def.gameplay, Gameplay::Ammo) && !w.dropped && w.toss.is_none() { def.drop_count.max(1) } else { count };
                        start_load(st, idx);
                        st.resolved.push(Resolved { item: idx, count, dropped: w.dropped, toss: w.toss, net: w.net, rest: None });
                    }
                    Err(e) => say(st, true, e),
                }
            }
            for (q, c, pos, tag) in std::mem::take(&mut st.rest_wants) {
                match pickup_items::resolve(&st.items, &q) {
                    Ok(def) => { let idx = st.items.iter().position(|i| i.id == def.id).unwrap_or(0); queue_load(st, idx); st.resolved.push(Resolved { item: idx, count: c, dropped: false, toss: None, net: None, rest: Some((pos, tag)) }); }
                    Err(e) => { let _ = tag; if st.log.len() < 5000 { st.log.push(e); } }
                }
            }
        } else if !st.wants.is_empty() && st.scan_rx.is_none() { say(st, true, "the item catalog is not available (no game install)".into()); st.wants.clear(); }
        // 3. resolved -> toss (the mesh is ready or failed)
        let mut still = vec![];
        for r in std::mem::take(&mut st.resolved) {
            if matches!(st.assets.get(&r.item), Some(Asset::Loading)) { still.push(r); continue; }
            let def = st.items[r.item].clone();
            if let Some((pos, tag)) = r.rest { let id = st.world.spawn_at_rest(r.item as u32, r.count, def.item_type, def.rarity, pos); st.tags.insert(tag, id); continue; }
            let id = match (r.net, r.toss) {
                (Some((nid, a, b, mine)), _) => { let id = st.world.spawn_toss(r.item as u32, r.count, def.item_type, def.rarity, a, b, mine, gravity_of(sim)); st.net_ids.insert(id, nid); id }
                (None, Some((a, v))) => st.world.spawn_launch(r.item as u32, r.count, def.item_type, def.rarity, a, v, gravity_of(sim)),
                (None, None) => {
                    let (a, b) = toss_points(sim);
                    if st.net_mode && r.dropped { st.net_out.push(NetDrop { item: def.id.clone(), count: r.count, start: a, end: b }); say(st, false, format!("{} x {}: drop sent to the match relay", r.count, def.name)); continue; }
                    st.world.spawn_toss(r.item as u32, r.count, def.item_type, def.rarity, a, b, r.dropped, gravity_of(sim))
                }
            };
            if let Some((nid, ..)) = r.net { if let Some(k) = st.early_gone.iter().position(|g| g.0 == nid) { let (_, mine) = st.early_gone.remove(k); net_gone_in(st, nid, mine); } }
            let p = pawn_state(sim);
            if r.dropped { if let Some(c) = def.drop_sound.as_deref().and_then(fn_audio_sink::pickup_cue) { audio.event(SoundEvent::PickupCue { cue: c, pos: Some(p.pos) }); } }
            say(st, false, format!("{} x {} [{}] {} (pickup #{id}, rarity {}): tossed ahead of the player", r.count, def.name, pickup_items::rarity_label(def.rarity), if r.dropped { "dropped" } else { "spawned" }, def.rarity).replace("tossed ahead of the player", if r.toss.is_some() { "tossed from a container" } else { "tossed ahead of the player" }));
        }
        st.resolved = still;
        // 4. the Use key: the pickup in front of the player
        let pawn = pawn_state(sim);
        let use_down = use_down || st.aid_use.iter().any(|f| frame >= *f && frame < *f + 3);
        let edge = use_down && !st.use_was_down; st.use_was_down = use_down;
        st.focus = st.world.focus_hit(&pawn).filter(|&(t, _)| {
            let fwd = pawn.forward.normalize_or_zero();
            let (a, b) = (pawn.eye, pawn.eye + fwd * t);
            let ext = fn_game::pickups::INTERACT_TRACE_EXTENT;
            let prop = fn_props::trace_in(&sim.world.props, sim.fresh_prop_index(), a, b, ext).map(|f| f * t);
            let piece = if sim.building_hulls.is_empty() { None } else { fn_combat::trace_sphere_hulls(&sim.building_hulls, a, b, ext).map(|f| f * t) };
            !prop.into_iter().chain(piece).any(|o| o < t - 1e-3)
        }).map(|h| h.1);
        if edge {
            if st.focus.is_some() { crate::interact::note_tap(); }
            if let Some(fid) = st.focus {
                if let Some(p) = st.world.get(fid).cloned() {
                    let def = st.items[p.item as usize].clone();
                    match cannot_hold(&def, p.count, arms.as_deref(), sim) {
                        Some(why) => say(st, true, format!("{}: {why}", def.name)),
                        None => {
                            if claim(st, fid) { say(st, false, format!("{}: pickup claimed at the match relay", def.name)); } else if let Some(e) = st.world.begin_pickup(fid, false, None) {
                                if matches!(def.gameplay, Gameplay::Weapon) { if let Err(e) = weapons.request_prefetch(&def.id) { say(st, true, format!("{}: {e}", def.name)); } }
                                handle(st, e, audio, sim);
                            }
                        }
                    }
                }
            }
        }
        // 5. the world
        let ground = |p: glam::Vec3| sim.floor_below(p); // the props' hulls (POI floors), building pieces, else the landscape
        let items = &st.items;
        let sim_ref: &GameSim = sim;
        let arms_ref = arms.as_deref();
        let net_ids = &st.net_ids;
        let mut auto_claims: Vec<u64> = vec![];
        let events = st.world.update(dt, &pawn, &ground, &mut |p| { let ok = cannot_hold(&items[p.item as usize], p.count, arms_ref, sim_ref).is_none(); if ok && net_ids.contains_key(&p.id) { auto_claims.push(p.id); false } else { ok } });
        for l in auto_claims { claim(st, l); }
        for e in events {
            match e {
                Event::Arrived { id, item, count } => deliveries.push((id, item as usize, count)),
                other => handle(st, other, audio, sim),
            }
        }
    });
    // deliveries (outside the host lock: they call into the weapon runtime / console requests)
    for (id, item, count) in deliveries {
        let Some(def) = host(|st| st.items.get(item).cloned()) else { host(|st| st.world.settle(id, 0, rest)); continue };
        let (line, taken) = deliver(&def, id, count, sim, arms.as_deref_mut(), weapons);
        let rest_left = taken.map(|t| count.saturating_sub(t));
        host(|st| {
            if let Some(t) = taken { st.world.settle(id, t, rest); picked(st, &def, t); if st.net_taken.remove(&id) && t < count { net_requeue(st, id, rest); } }
            say(st, rest_left.is_some_and(|r| r > 0 && r == count), line);
        });
    }
}

fn net_requeue(st: &mut State, id: u64, rest: Vec3) {
    let Some(p) = st.world.get(id).cloned() else { return };
    st.world.pickups.retain(|q| q.id != id);
    if let Some(def) = st.items.get(p.item as usize) { st.net_out.push(NetDrop { item: def.id.clone(), count: p.count, start: rest, end: rest }); }
}

fn rest_point(sim: &GameSim) -> Vec3 {
    let f = sim.feet();
    Vec3::new(f.x, sim.floor_below(Vec3::new(f.x, f.y + 0.05, f.z)) + TOUCH_HALF_HEIGHT, f.z)
}

fn handle(st: &mut State, e: Event, audio: &mut dyn AudioSink, sim: &GameSim) {
    match e {
        Event::Landed { pos, .. } => audio.event(SoundEvent::PickupCue { cue: fn_audio_sink::PICKUP_LANDED_CUE, pos: Some(pos) }),
        Event::Settled { id, pos } => { let p = sim.feet(); say(st, false, format!("pickup #{id} at rest at ({:.2}, {:.2}, {:.2}), the player stands at ({:.2}, {:.2}, {:.2})", pos.x, pos.y, pos.z, p.x, p.y, p.z)); }
        Event::PickedUp { id, auto, fly_time, .. } => {
            let name = st.world.get(id).and_then(|p| st.items.get(p.item as usize)).map(|d| (d.name.clone(), d.pickup_sound.clone()));
            if let Some((name, snd)) = name {
                // ClientPlaySound(ItemDefinition.PickupSound): 2D
                if let Some(c) = snd.as_deref().and_then(fn_audio_sink::pickup_cue) { audio.event(SoundEvent::PickupCue { cue: c, pos: None }); }
                say(st, false, format!("{name}: picked up {} (flight {fly_time:.2} s)", if auto { "by touch" } else { "with Use" }));
            }
            let _ = sim;
        }
        Event::Arrived { .. } => {}
    }
}

/// Gives `count` of `def` to the player (`AFortPickup::vfunc_191`: what fits is taken, the rest stays). Returns the console line and the amount taken (None = the weapon runtime answers later).
fn deliver(def: &ItemDef, id: u64, count: u32, sim: &mut GameSim, arms: Option<&mut Arms>, weapons: &mut crate::weapon_runtime::WeaponRuntime) -> (String, Option<u32>) {
    let rest = |taken: u32, what: &str| -> String { if taken >= count { format!("{count} x {}: {what}", def.name) } else if taken == 0 { format!("{}: {} (it stays on the ground)", def.name, what) } else { format!("{taken} x {}: {what} ({} stay on the ground)", def.name, count - taken) } };
    match &def.gameplay {
        Gameplay::Weapon => match weapons.request_pickup(&def.id, id) { Ok(_) => (format!("{}: arrived, going to the quickbar", def.name), None), Err(e) => (format!("{}: {e} (it stays on the ground)", def.name), Some(0)) },
        Gameplay::Healable(idx) => match arms.map(|a| a.pickup_heal(*idx, count, true)) {
            Some(Ok(rep)) => {
                let taken = count - rep.leftover.min(count);
                let mut line = rest(taken, if taken == 0 { rep.refusal.map_or("Can't pickup this item!", |r| r.text()) } else { "added to the inventory" });
                if let Some(old) = rep.replaced { line.push_str(&format!("; the backpack was full: {old:?} was dropped")); }
                (line, Some(taken))
            }
            Some(Err(e)) => (format!("{}: {e} (it stays on the ground)", def.name), Some(0)),
            None => (format!("{}: no live loadout (it stays on the ground)", def.name), Some(0)),
        },
        Gameplay::Ammo => match arms {
            Some(a) => { let max = ammo_max(a, def); let (added, _) = a.pickup_ammo_item(&def.package, max, count); (rest(added, if added == 0 { "Can't hold more of this item!" } else { "added to the ammo pool" }), Some(added)) }
            None => (format!("{}: no live loadout (it stays on the ground)", def.name), Some(0)),
        },
        Gameplay::Resource(r) => {
            let v = match r { Resource::Wood => &mut sim.world.wood, Resource::Stone => &mut sim.world.stone, Resource::Metal => &mut sim.world.metal };
            let taken = count.min(999u32.saturating_sub(*v));
            *v += taken;
            let now = *v;
            (format!("{}, now {now}", rest(taken, if taken == 0 { "Can't hold more of this item!" } else { "added" })), Some(taken))
        }
        Gameplay::Trap => match crate::trap_hook::give(&def.id, count) { Ok(l) => (l, Some(count)), Err(e) => (format!("{}: {e} (it stays on the ground)", def.name), Some(0)) },
        Gameplay::Other => (format!("{}: this sandbox has no gameplay for the item (it stays on the ground)", def.name), Some(0)),
    }
}

/// The interaction prompt of the focused pickup: (key text, text, world anchor, the card `ShowPickupWidget` fills). `backpack_full` =
/// `GetBackpackItemCounts` items >= max (the red backpack icon of the failure row).
pub fn prompt(arms: Option<&Arms>, sim: &GameSim, key: &str, backpack_full: bool) -> Option<(String, String, Vec3, fn_ui::screens::interaction_cooked::PickupCard)> {
    host(|st| {
        let id = st.focus?;
        let p = st.world.get(id)?;
        let def = st.items.get(p.item as usize)?;
        let why = cannot_hold(def, p.count, arms, sim);
        // `GetInteractText`: "Pick up" / "Swap\nequipped"; `GetInteractErrorText` replaces it when the pickup is refused
        let swap = match &def.gameplay { Gameplay::Weapon => swaps(arms) && !arms.is_some_and(|a| owned_gun(a, def)), Gameplay::Healable(idx) => arms.is_some_and(|a| a.heal_pickup_swaps(*idx, p.count)), _ => false };
        let verb = if swap { "Swap\nequipped" } else { "Pick up" };
        let action = match why { Some(w) => w.to_owned(), None => verb.to_owned() };
        let name = if p.count > 1 { format!("{} x{}", def.name, p.count) } else { def.name.clone() };
        // PICKUP CARD: `PrimaryPickupDummyItem` = the definition at the pickup's count (`HUD-PickupItemWidget.InitializeItemData`)
        let item = fn_ui::screens::inventory_panel::InvItem { id: 0, name: def.name.clone(), description: String::new(), rarity: def.rarity, icon: String::new(), count: p.count, item_type: def.item_type,
            equipable: false, is_resource: matches!(def.gameplay, Gameplay::Resource(_)), can_be_dropped: true, slot: None, def_path: def.package.clone() };
        let card = fn_ui::screens::interaction_cooked::PickupCard { item, action: why.is_none().then(|| verb.to_owned()), error: why.map(|w| w.to_owned()), backpack_full };
        Some((key.to_owned(), format!("{action}\n{name} ({})", pickup_items::rarity_label(def.rarity)), p.pos, card))
    })
}

fn slim_asset(assets: &mut HashMap<usize, Asset>, item: usize) {
    let Some(Asset::Ready(a)) = assets.get(&item) else { return };
    if a.prims.is_empty() { return; }
    let slim = crate::pickup_mesh::MeshAsset { prims: vec![], radius_cm: a.radius_cm, extent_cm: a.extent_cm, origin_cm: a.origin_cm, notes: vec![], source: a.source.clone() };
    assets.insert(item, Asset::Ready(Arc::new(slim)));
}

/// Once per rendered frame (after `Scene::update`): one renderer model per pickup, placed at the actor; the first use of an item's mesh uploads it.
pub fn frame(scene: &mut crate::scene::Scene) {
    crate::pickup_orig::begin_frame(); // PICKUP ITEM LIGHTING
    // HELD PICKAXE LIGHTING (docs/spec/smallfix_followups.md section 3): with the original gun material on, the held pickaxe runs its cooked material too
    // (`MI_FORT_Melee_Pick` -> `WeaponMat_Base` base-pass programs, the gun pass), so held tool, held gun and dropped weapons share one lighting path;
    // its renderer model (the `shade_character` Prop fallback) is drawn only when that material cannot be built.
    if let (true, Some(to_world)) = (scene.orig_guns.is_some(), scene.pick_world) {
        let mesh = scene.pick_mesh.clone();
        if crate::pickup_orig::draw(&mut scene.renderer, scene.orig_guns.as_mut(), true, &mesh, true, to_world) { scene.hide_pick_model(); }
    }
    // STATIC ITEM (2026-10-05): a `PickupStaticMesh` item (ammo, consumables, resources) is drawn with its own cooked materials by the pickup FX pass
    // (`pickups_dxbc::Dxbc::draw_static_item`, the same switch as the dropped weapons: off with `--standin-gun-material`); its renderer model only when they
    // cannot be built. Decided here, before the host lock (the FX state reads the pickup snapshot under its own lock).
    let orig_on = scene.orig_guns.is_some();
    let static_paths: std::collections::HashSet<String> = host(|st| st.world.pickups.iter().filter_map(|p| st.items.get(p.item as usize)).filter(|d| d.skeletal_mesh.is_none()).filter_map(|d| d.static_mesh.clone()).collect());
    let cooked_static: std::collections::HashSet<String> = static_paths.into_iter().filter(|p| crate::pickups_fx::static_item_ready(&mut scene.renderer, p, orig_on)).collect();
    host(|st| {
        // models of vanished pickups go to the free list
        let alive: std::collections::HashSet<u64> = st.world.pickups.iter().map(|p| p.id).collect();
        let gone: Vec<u64> = st.slots.keys().filter(|id| !alive.contains(id)).copied().collect();
        for id in gone { if let Some(s) = st.slots.remove(&id) { st.free.push(s); } }
        let loot = loot_point();
        let pickups: Vec<(u64, usize, Vec3, f32, Option<[f32; 4]>)> = st.world.pickups.iter().map(|p| (p.id, p.item as usize, p.pos, p.yaw, match (p.phase, loot) { (Phase::Flying { t, fly_time, .. }, Some(l)) => { let u = fn_particles::sim::game_to_ue([l.x, l.y, l.z]); Some([u.x, u.y, u.z, fn_game::pickups::black_hole_strength(fn_game::pickups::curve_time(t.max(0.), fly_time))]) } _ => None })).collect();
        for (id, item, pos, yaw, black_hole) in pickups {
            let Some(Asset::Ready(asset)) = st.assets.get(&item) else { continue };
            let asset = asset.clone();
            // PICKUP ITEM LIGHTING: a weapon item uses the held gun's lighting path (its original WeaponMat_Base material in the gun pass) whenever the held gun does.
            // FIRST USE (2026-10-06): decided before the renderer model exists, which is now created only for an item drawn with it (it used to be uploaded for every
            // weapon pickup, with its CPU mip chains, and never drawn)
            let orig_on = scene.orig_guns.is_some();
            if let Some(def) = st.items.get(item) { if let Some(m) = &def.skeletal_mesh {
                if crate::pickup_orig::draw_with(&mut scene.renderer, scene.orig_guns.as_mut(), orig_on, m, true, crate::pickup_orig::to_world(pos, yaw, asset.origin_cm), black_hole) { slim_asset(&mut st.assets, item); continue; }
            } else if def.static_mesh.as_ref().is_some_and(|m| cooked_static.contains(m)) { slim_asset(&mut st.assets, item); continue; } }
            if asset.prims.is_empty() { continue; } // STATIC ITEM: drawn by the pickup FX pass
            if !st.slots.contains_key(&id) {
                let slot = match st.free.iter().position(|s| s.item == item) {
                    Some(i) => st.free.swap_remove(i),
                    None => {
                        let m = match st.free.pop() {
                            Some(old) => { let built = scene.model_builder().build(&asset.prims, 1, &format!("pickup {}", item)); scene.renderer.replace_model(old.model, built); old.model }
                            None => scene.renderer.create_model(&asset.prims, 1),
                        };
                        Slot { model: m, item }
                    }
                };
                st.slots.insert(id, slot);
            }
            let slot = &st.slots[&id];
            let world = Mat4::from_translation(pos) * Mat4::from_rotation_y(yaw);
            scene.renderer.update_model(slot.model, world, &[Mat4::IDENTITY]);
            scene.models.push(slot.model);
        }
    });
}

/// Epilogue of a scripted run: the log (what happened to every pickup).
pub fn run_report() -> String { host(|st| if st.log.is_empty() { String::new() } else { format!("pickups: {} actors alive, log:\n  {}\n", st.world.len(), st.log.join("\n  ")) }) }

#[allow(dead_code)]
const _: (f32, f32) = (FLY_CURVE_SPAN, EYE);

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn toss_points_land_five_twelve_ahead_at_the_resting_height() {
        let sim = GameSim::new();
        let (a, b) = toss_points(&sim);
        assert!((a.y - (sim.feet().y + PAWN_HALF_HEIGHT)).abs() < 1e-5);
        assert!((Vec3::new(b.x - a.x, 0., b.z - a.z).length() - 5.12).abs() < 1e-4);
        assert_eq!(b.y, TOUCH_HALF_HEIGHT);
    }
    #[test] fn hold_rules_use_the_definition_and_the_arms() {
        let sim = GameSim::new();
        let def = |gp: Gameplay, ty: u8| ItemDef { id: "X".into(), package: String::new(), name: "X".into(), rarity: 0, item_type: ty, class: String::new(), max_stack: 1, drop_count: 1, static_mesh: None, skeletal_mesh: None, pickup_sound: None, drop_sound: None, mesh_transform: None, gameplay: gp, small_icon: None };
        assert_eq!(cannot_hold(&def(Gameplay::Other, 14), 1, None, &sim), Some("Can't pickup this item!"));
        assert_eq!(cannot_hold(&def(Gameplay::Resource(Resource::Wood), 19), 5, None, &sim), None);
        let mut s2 = GameSim::new(); s2.world.wood = 999;
        assert_eq!(cannot_hold(&def(Gameplay::Resource(Resource::Wood), 19), 5, None, &s2), Some("Can't hold more of this item!"));
    }
}
