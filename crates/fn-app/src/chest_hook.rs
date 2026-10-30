//! CHEST HOOK (2026-10-08): every BR loot container of the map: the chests (`Tiered_Chest_6_Parent` and its child `Tiered_Chest_3_Child`) and the ammo
//! boxes (`Tiered_Short_Ammo_3_Parent`), placed in `Athena_Terrain`'s sublevels and the foundation-streamed building worlds (the POI export's `loot[]`,
//! `poi::PoiSet::containers`). The rules are `fn_game::containers` (decompile references there); this file runs them for the app:
//! * meshes: the closed container is an ordinary POI placement (`statics_render`, cooked material). Its `SearchedMesh` (the class default: the live 1.8 client
//!   applies no tier mesh set, `ReplicatedLootTier` -1) is a placement of a pseudo actor ([`add_searched`]) whose harvestable starts dead; on search the two harvestables swap
//!   (`statics_render` draws only live actors' components, the sim collides only with live ones), as `vfunc_428` swaps the mesh. A container the spawn
//!   selection culls loses both.
//! * the chest's SCS components: `Closed_Chest` (`P_Loot_Chest_Closed`, `bAutoActivate` false; `ReceiveBeginPlay` activates it unless searched) and
//!   `Chest_Ambient_Sound` (`Tiered_Chest_Ambient_Cue`, an auto-activated AudioComponent). Ubergraph (in-memory offsets: `computedjump` is 10 bytes in memory,
//!   6 on disk): `OnLoot` (211) = `pushflow 272` -> `Closed_Chest.Deactivate`, `SetLightVisibility(false)`, `jump 15` = `Chest_Ambient_Sound.FadeOut(3, 0)`,
//!   pop -> the burst; `OnSetSearched` / `ReceiveDestroyed` (846 / 841) `jump 216` = the same Deactivate / light off / FadeOut chain: the hum fades out over
//!   3 s when the chest is searched. `TrasureLight` (PointLightComponent, 1700 / 300 cm / (255,192,139), destroyed by the
//!   construction script below High effects) is lit by `local_lights_hook` (fn-render local_lights.rs) while the chest is live and unsearched ([`trasure_light_states`]).
//! * the search: the Use hold on the focused container (the interaction trace of the pickups / doors), `SearchingSoundCueLoop` on the container while it runs
//!   (`FadeOut(1.0)` when it ends), the prompt "Search" with the KeybindWidget hold ring; on completion the chest's `OnLoot` (`Tiered_Chest_Open_T01_Cue`
//!   at the actor, `Closed_Chest.Deactivate`, light off, `P_Loot_Chest_T1` at the actor turned pitch + 90 with `color` = `RarityData[HighestRarity].Color2`),
//!   the searched mesh, and the loot tossed out as pickups (`pickups_hook::queue_toss`).
//! Kill switches: `FN_CHESTS=0` (no chest effects, sounds, search or loot; the closed meshes stay as placed), `FN_AMMOBOXES=0` (the same for the ammo
//! boxes), `FN_CHEST_SPAWN_ALL=1` (no per-match selection: every placed container spawns), `FN_CHEST_SEED=<n>` (the match seed of the selection / rolls).
use crate::pickups_dxbc::Dxbc;
use fn_audio_sink::SoundEvent;
use fn_fx::view::ViewParams;
use fn_game::containers::{self as ct, ClassDef, LootTables, Rng};
use fn_game::GameSim;
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Transform};
use fn_render::fx::{FxFrame, FxRenderer};
use glam::{Mat3, Mat4, Quat, Vec3};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub const P_CLOSED: &str = "/Game/Effects/Fort_Effects/World/Chests/P_Loot_Chest_Closed";
pub const P_OPEN: &str = "/Game/Effects/Fort_Effects/World/Chests/P_Loot_Chest_T1";
pub const AMBIENT_CUE: &str = "/Game/Sounds/Foley_Loot/Containers/Treasure_Chest/Tiered_Chest_Ambient_Cue";
pub const OPEN_CUE: &str = "/Game/Sounds/Foley_Loot/Containers/Treasure_Chest/Tiered_Chest_Open_T01_Cue";
pub const CHEST_CLASS: &str = "Tiered_Chest_6_Parent";
pub const AMMO_CLASS: &str = "Tiered_Short_Ammo_3_Parent";
/// The container classes placed on the 1.8 map.
pub const CLASSES: [&str; 4] = [CHEST_CLASS, AMMO_CLASS, "Tiered_Chest_3_Child", "Tiered_Short_CraftingIngredients_3_Parent"];
/// Sound channel keys: `LOOP_KEY_BASE + 2 * i` = the ambient loop of container `i`, `+ 1` = its searching loop (the building hook uses 0x4000_0000).
pub const LOOP_KEY_BASE: u32 = 0x5000_0000;
/// `Tiered_Chest_Ambient_Cue` `AttenuationOverrides`: logarithmic, shape extents 0, `FalloffDistance` 1000 cm (silent beyond). The loop is started within
/// this distance of the listener and stopped beyond it + [`HYSTERESIS_CM`] (CHOSEN: UE keeps an inaudible looping sound virtual).
pub const AMBIENT_RANGE_CM: f32 = 1000.;
pub const HYSTERESIS_CM: f32 = 200.;
/// `Chest_Ambient_Sound.FadeOut(3.0, 0.0)` of the ubergraph (OnLoot / OnSetSearched / ReceiveDestroyed).
pub const AMBIENT_FADE_OUT: f32 = 3.;

/// `FN_CHESTS=0` / `FN_AMMOBOXES=0`.
pub fn chests_enabled() -> bool { static V: OnceLock<bool> = OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_CHESTS").map_or(true, |v| v != "0")) }
pub fn ammo_enabled() -> bool { static V: OnceLock<bool> = OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_AMMOBOXES").map_or(true, |v| v != "0")) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Chest, AmmoBox, Other }
fn kind_of(def: Option<&ClassDef>, class: &str) -> Kind {
    let is = |n: &str| def.map_or(class == n, |d| d.is_a(n));
    if is(CHEST_CLASS) { Kind::Chest } else if is(AMMO_CLASS) { Kind::AmmoBox } else { Kind::Other }
}
fn trace_on() -> bool { static V: OnceLock<bool> = OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_CHEST_TRACE").is_ok_and(|v| v == "1")) }
fn kind_enabled(k: Kind) -> bool { match k { Kind::Chest | Kind::Other => chests_enabled(), Kind::AmmoBox => ammo_enabled() } }

/// The cooked data the containers need (read once from the paks, by whichever thread asks first).
pub struct Data {
    pub tables: LootTables,
    pub rarity_color2: Vec<[f32; 4]>,
    pub classes: HashMap<String, Arc<ClassDef>>,
    pub search_speed: HashMap<String, f32>,
    pub jiggle: [Option<fn_building::visual::RichCurve>; 4],
}
impl Data {
    /// The loot tier of a class (the `LootTier` of its Athena tier group's rows: AthenaTreasure 0, AthenaAmmoLarge 1 in 1.8).
    pub fn tier(&self, d: &ClassDef) -> i32 { ct::athena_tier_group(&d.tier_group).and_then(|g| self.tables.tiers_of(g).next()).map_or(0, |r| r.loot_tier) }
}
static DATA: OnceLock<Result<Arc<Data>, String>> = OnceLock::new();
pub fn data_ready() -> Option<Result<Arc<Data>, String>> { DATA.get().cloned() }
pub fn data() -> Result<Arc<Data>, String> {
    DATA.get_or_init(|| {
        let t = std::time::Instant::now();
        let set = fn_game::cosmetic_swap::paks()?;
        let tables = LootTables::load(&set)?;
        let rarity_color2 = ct::rarity_color2(&set)?;
        let search_speed = ct::search_speed_table(&set)?;
        let jiggle = [ct::JIGGLE_LOOT_MAGNITUDE, ct::JIGGLE_LOOT_ANGLE, ct::JIGGLE_EMPTY_MAGNITUDE, ct::JIGGLE_EMPTY_ANGLE].map(|p| match fn_building::visual::load_float_curve(&set, p) { Ok(c) => Some(c), Err(e) => { eprintln!("[chests] {e}: no search jiggle"); None } });
        let mut classes = HashMap::new();
        for c in CLASSES { match ct::load_class(&set, c) { Ok(d) => { classes.insert(c.to_owned(), Arc::new(d)); } Err(e) => eprintln!("[chests] {c}: {e}") } }
        match ct::load_class_at(&set, crate::floor_loot_hook::FLOOR_LOOT_PACKAGE, crate::floor_loot_hook::FLOOR_LOOT_CLASS) { Ok(d) => { classes.insert(crate::floor_loot_hook::FLOOR_LOOT_CLASS.to_owned(), Arc::new(d)); } Err(e) => eprintln!("[floor-loot] {e}") }
        eprintln!("[chests] loot tables: {} tier rows, {} package rows; {} container classes; read in {:.0} ms", tables.tiers.len(), tables.packages.len(), classes.len(), t.elapsed().as_secs_f32() * 1000.);
        Ok(Arc::new(Data { tables, rarity_color2, classes, search_speed, jiggle }))
    }).clone()
}

/// CONTAINERS (called by `poi::load` before the set is published): one pseudo actor per container whose placement is its `SearchedMesh` at the root
/// transform, in the container's streaming group, with a DEAD harvestable carrying the closed container's collision (CHOSEN: the searched mesh's own
/// collision is not in the POI export; the searched chest / box has the same footprint).
pub fn add_searched(set: &mut crate::poi::PoiSet) {
    if set.containers.is_empty() || (!chests_enabled() && !ammo_enabled()) { return; }
    let d = match data() { Ok(d) => d, Err(e) => { eprintln!("[chests] no searched meshes ({e})"); return } };
    let mut group_of: HashMap<u32, usize> = HashMap::new();
    for (gi, g) in set.groups.iter().enumerate() { for &pi in &g.placements { group_of.entry(set.placements[pi as usize].actor).or_insert(gi); } }
    let harv: HashMap<u32, usize> = set.harvestables.iter().enumerate().map(|(i, h)| (h.proto, i)).collect();
    let mut added = 0;
    for ci in 0..set.containers.len() {
        let (class, actor, t, q, s, cull) = { let c = &set.containers[ci]; (c.class.clone(), c.actor, c.t, c.q, c.s, c.cull_cm) };
        let Some(def) = d.classes.get(&class) else { continue };
        if !kind_enabled(kind_of(Some(def), &class)) { continue; }
        let Some(mesh) = ct::searched_mesh(def, d.tier(def)) else { continue };
        let Some(&gi) = group_of.get(&actor) else { continue };
        let id = format!("poi:{mesh}|");
        let kind = match set.kind_index.get(&id) { Some(&k) => k, None => { set.kinds.push(crate::poi::PoiKind { id: id.clone(), mesh: mesh.clone(), overrides: vec![], vcol: None, mid0: None }); set.kind_index.insert(id, set.kinds.len() - 1); set.kinds.len() - 1 } };
        let pseudo = set.classes.len() as u32;
        set.classes.push(format!("{class}:Searched"));
        let pi = set.placements.len() as u32;
        set.placements.push(crate::poi::PoiPlacement { kind, class: format!("{class}:Searched"), l2w: Mat4::from_scale_rotation_translation(s, q, t), actor: pseudo, cull_cm: cull, cast_shadow: true, cluster: crate::poi::NO_CLUSTER });
        let g = &mut set.groups[gi];
        g.placements.push(pi);
        if !g.kinds.contains(&(kind as u32)) { g.kinds.push(kind as u32); g.kinds.sort(); }
        if let Some(&hi) = harv.get(&(fn_combat::POI_PROTO | actor)) { let mut h = set.harvestables[hi].clone(); h.proto = fn_combat::POI_PROTO | pseudo; h.health = 0.; set.harvestables.push(h); }
        set.containers[ci].searched_actor = pseudo;
        added += 1;
    }
    eprintln!("[chests] {added} searched-mesh placements added (hidden until searched)");
}

struct Cont {
    poi: usize,
    kind: Kind,
    def: Option<Arc<ClassDef>>,
    /// Spawned this match (the selection of `FortAthenaMapInfo`).
    spawned: bool,
    search: ct::Search,
    /// Indices in the sim's props of its harvestable (`POI_PROTO | actor`) and of its searched pseudo actor's.
    prop: Option<usize>,
    searched_prop: Option<usize>,
    /// `Closed_Chest` (created when the view comes within its draw distance; its LOD 0 and LOD 1 (2400 cm) are identical in the cooked system, so the LOD
    /// picked at creation is never re-evaluated).
    closed: Option<Instance>,
    closed_active: bool,
    ambient_on: bool,
    search_loop_on: bool,
    light_visible: bool,
    destroyed: bool,
    bounce: Option<f64>,
}
impl Cont { fn live(&self) -> bool { self.spawned && kind_enabled(self.kind) } }

struct State {
    poi: Option<Arc<crate::poi::PoiSet>>,
    data: Option<Arc<Data>>,
    conts: Vec<Cont>,
    catalog: Option<Arc<Catalog>>,
    install: Option<PathBuf>,
    dxbc: Option<Result<Dxbc, String>>,
    bursts: Vec<Instance>,
    seed: u64,
    props_len: usize,
    focus: Option<usize>,
    use_was_down: bool,
    eye_ue: Vec3,
    serial: u64,
    log: Vec<String>,
    errors: std::collections::HashSet<String>,
    counts: (usize, usize, usize, usize),
    opened: usize,
    clock: f64,
    equip: Option<(usize, bool)>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
static WARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); g.as_mut().map(f) }

/// FX space (UE cm about the game origin) <-> sim metres.
fn to_game(u: Vec3) -> Vec3 { Vec3::new(u.x, u.z, u.y) * 0.01 }
fn to_ue(g: Vec3) -> Vec3 { Vec3::new(g.x, g.z, g.y) * 100. }

/// UE yaw / pitch / roll (degrees) of a rotation (`FQuat::Rotator`).
fn rotator(q: Quat) -> (f32, f32, f32) {
    let m = Mat3::from_quat(q);
    let x = m.x_axis;
    let yaw = x.y.atan2(x.x).to_degrees();
    let pitch = x.z.clamp(-1., 1.).asin().to_degrees();
    let roll = (-m.y_axis.z).atan2(m.z_axis.z).to_degrees();
    (yaw, pitch, roll)
}
/// `FRotationMatrix(FRotator(pitch, yaw, roll))` in UE axes: X = (cp cy, cp sy, sp).
fn rot_matrix(pitch: f32, yaw: f32, roll: f32) -> Mat3 { Mat3::from_rotation_z(yaw.to_radians()) * Mat3::from_rotation_y(-pitch.to_radians()) * Mat3::from_rotation_x(-roll.to_radians()) }

/// Loads `<root>/particles-chests` and reads the cooked data on a worker (from `app.rs::start_world_load`).
pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    if !chests_enabled() && !ammo_enabled() { eprintln!("[chests] off (FN_CHESTS=0, FN_AMMOBOXES=0)"); *STATE.lock().unwrap() = None; return; }
    if !chests_enabled() { eprintln!("[chests] chests off (FN_CHESTS=0)"); }
    if !ammo_enabled() { eprintln!("[chests] ammo boxes off (FN_AMMOBOXES=0)"); }
    let dir = fn_live::var_os("FN_CHESTS_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-chests")).find(|d| d.join("index.json").is_file()));
    let catalog = match dir.as_ref().map(|d| Catalog::load(d)) {
        Some(Ok(c)) if c.system(P_CLOSED).is_some() && c.system(P_OPEN).is_some() => { eprintln!("[chests] P_Loot_Chest_Closed / P_Loot_Chest_T1 loaded from {}", dir.as_ref().unwrap().display()); Some(Arc::new(c)) }
        Some(Ok(_)) => { eprintln!("[chests] the particles-chests catalog lacks P_Loot_Chest_Closed / P_Loot_Chest_T1: no chest particles"); None }
        Some(Err(e)) => { eprintln!("[chests] {e}: no chest particles"); None }
        None => { eprintln!("[chests] no `particles-chests` folder in the asset roots (fn-particles-export --out <assets>/particles-chests {P_CLOSED} {P_OPEN}): no chest particles"); None }
    };
    let seed = fn_live::var("FN_CHEST_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64));
    std::thread::spawn(|| { if let Err(e) = data() { eprintln!("[chests] cooked data unavailable ({e}): containers cannot be searched"); } });
    WARMED.store(false, std::sync::atomic::Ordering::Relaxed);
    *STATE.lock().unwrap() = Some(State { poi: None, data: None, conts: vec![], catalog, install, dxbc: None, bursts: vec![], seed, props_len: usize::MAX, focus: None, use_was_down: false,
        eye_ue: Vec3::ZERO, serial: 0, log: vec![], errors: Default::default(), counts: (0, 0, 0, 0), opened: 0, clock: 0., equip: None });
}

fn dxbc<'a>(slot: &'a mut Option<Result<Dxbc, String>>, install: &Option<PathBuf>, fx: &mut FxRenderer) -> Option<&'a mut Dxbc> {
    if slot.is_none() {
        let r = match install { Some(d) => Dxbc::load(d, fx), None => Err("no install".into()) };
        if let Err(e) = &r { eprintln!("[chests] chest particle materials unavailable: {e}"); }
        *slot = Some(r);
    }
    slot.as_mut().and_then(|r| r.as_mut().ok())
}
fn draw(dx: &mut Dxbc, fx: &mut FxRenderer, cat: &Catalog, inst: &Instance, eye: [f32; 3], dt: f32, errors: &mut std::collections::HashSet<String>) {
    let mut d = vec![];
    inst.draw(eye, &mut d);
    let bounds = crate::particles::instance_bounds(inst, cat);
    for e in &d {
        if let Err(err) = dx.draw_emitter(fx, e, &inst.xf, &bounds, dt) {
            let k = format!("{}/{}", e.system, e.emitter);
            if errors.insert(k.clone()) { eprintln!("[chests] {k}: NOT DRAWN: {err}"); }
        }
    }
}

/// FIRST USE: builds the chest particle materials under the loading screen (one throw-away frame of both systems).
pub fn prebuild(fx: &mut FxRenderer) {
    if WARMED.load(std::sync::atomic::Ordering::Relaxed) { return; }
    with(|st| {
        let Some(cat) = st.catalog.clone() else { return };
        let t = std::time::Instant::now();
        let Some(dx) = dxbc(&mut st.dxbc, &st.install, fx) else { return };
        let view = ViewParams { eye: Vec3::new(0., 2., 3.), forward: Vec3::new(0., -0.5, -0.85).normalize(), up: Vec3::Y, tan_half: (1., 0.6), near_m: 0.1, far_m: 1000., width: 64, height: 64, time: 0., real_time: 0., delta_world: 1. / 60. };
        dx.begin(&view);
        for p in [P_CLOSED, P_OPEN] {
            let Some(s) = cat.system(p) else { continue };
            let mut i = Instance::new(s, Transform::identity(), 1, 100.);
            for _ in 0..30 { i.tick(1. / 30., &Env::none()); }
            draw(dx, fx, &cat, &i, [0., 2., 3.], 1. / 60., &mut st.errors);
        }
        let _ = dx.take_frame();
        WARMED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("[chests] warm-up: chest particle materials built in {:.0} ms", t.elapsed().as_secs_f32() * 1000.);
    });
}

/// (Re)builds the container list when the POI set changes (with the per-match spawn selection); finds the harvestables; keeps their life in sync.
fn sync(st: &mut State, sim: &mut GameSim) {
    if st.data.is_none() { match DATA.get() { Some(Ok(d)) => st.data = Some(d.clone()), _ => return } }
    let Some(d) = st.data.clone() else { return };
    let Some(poi) = crate::poi::current() else { return };
    if !st.poi.as_ref().is_some_and(|p| Arc::ptr_eq(p, &poi)) {
        st.conts = poi.containers.iter().enumerate().map(|(i, c)| {
            let def = d.classes.get(&c.class).cloned();
            let kind = kind_of(def.as_deref(), &c.class);
            Cont { poi: i, kind, def, spawned: true, search: ct::Search::default(), prop: None, searched_prop: None, closed: None, closed_active: false, ambient_on: false, search_loop_on: false, light_visible: false, destroyed: false, bounce: None }
        }).collect();
        st.poi = Some(poi.clone());
        st.props_len = usize::MAX;
        // FortAthenaMapInfo (TreasureChestClass / AmmoBoxClass are exact classes): the per-match selection, each class on its own
        let all = fn_live::var("FN_CHEST_SPAWN_ALL").is_ok_and(|v| v == "1");
        let mut rng = Rng::new(st.seed ^ 0x5350_4157_4E);
        for (k, class, (min, max)) in [(Kind::Chest, CHEST_CLASS, ct::TREASURE_CHEST_SPAWN_PERCENT), (Kind::AmmoBox, AMMO_CLASS, ct::AMMO_BOX_SPAWN_PERCENT)] {
            let idx: Vec<usize> = st.conts.iter().enumerate().filter(|(_, c)| c.kind == k && poi.containers[c.poi].class == class).map(|(i, _)| i).collect();
            let keep = if all { vec![true; idx.len()] } else { ct::select_spawned(idx.len(), min, max, &mut rng) };
            for (j, &i) in idx.iter().enumerate() { st.conts[i].spawned = keep[j]; }
        }
        let n = |k: Kind| (st.conts.iter().filter(|c| c.kind == k).count(), st.conts.iter().filter(|c| c.kind == k && c.spawned).count());
        let ((cp, cs), (ap, asp)) = (n(Kind::Chest), n(Kind::AmmoBox));
        st.counts = (cp, cs, ap, asp);
        if trace_on() {
            let dups = (0..poi.containers.len()).flat_map(|a| (a + 1..poi.containers.len()).map(move |b| (a, b))).filter(|&(a, b)| (poi.containers[a].t - poi.containers[b].t).length() < 30.).map(|(a, b)| format!("#{a}/#{b} {} {:?}", poi.containers[a].class, poi.containers[a].t)).collect::<Vec<_>>();
            st.log.push(format!("[chests] {} container pairs within 30 cm: {}", dups.len(), dups.join("; ")));
        }
        st.log.push(format!("[chests] {} containers: {cp} chests placed, {cs} spawned this match; {ap} ammo boxes placed, {asp} spawned; {} other (seed {}){}", st.conts.len(), st.conts.len() - cp - ap, st.seed, if all { ", FN_CHEST_SPAWN_ALL=1" } else { "" }));
    }
    let props = &mut sim.world.props;
    if st.props_len != props.len() {
        st.props_len = props.len();
        let by_proto: HashMap<u32, usize> = props.iter().enumerate().filter(|(_, h)| fn_combat::is_poi_proto(h.proto)).map(|(i, h)| (h.proto, i)).collect();
        for c in st.conts.iter_mut() {
            let pc = &poi.containers[c.poi];
            c.prop = by_proto.get(&(fn_combat::POI_PROTO | pc.actor)).copied();
            c.searched_prop = (pc.searched_actor != crate::poi::NO_CLUSTER).then(|| by_proto.get(&(fn_combat::POI_PROTO | pc.searched_actor)).copied()).flatten();
        }
        // the life of both harvestables: a culled container has neither; a searched one only the searched mesh
        for c in st.conts.iter().filter(|c| kind_enabled(c.kind)) {
            let (closed, searched) = (c.spawned && !c.search.searched() && !c.destroyed, c.spawned && c.search.searched());
            if let Some(h) = c.prop.and_then(|i| props.get_mut(i)) { h.health = if closed { h.max_hp.max(1.) } else { 0. }; }
            if let Some(h) = c.searched_prop.and_then(|i| props.get_mut(i)) { h.health = if searched { h.max_hp.max(1.) } else { 0. }; }
        }
    }
}

/// One frame (after the doors): the Use hold, the search, the open, the loot, the loops. `use_down` = the Use binding is held; `blocked` = a pickup or a
/// door has the focus this frame (CHOSEN as the doors: one interaction per press, earlier hooks first). Returns the sounds.
pub fn frame(sim: &mut GameSim, use_down: bool, blocked: bool, equip: (usize, bool), dt: f32) -> Vec<SoundEvent> {
    let mut sounds = vec![];
    let mut tosses: Vec<(String, u32, Vec3, Vec3)> = vec![];
    let gravity_cm = { let p = fn_movement::cmc::Params::default().with_tuning(&sim.tuning); (p.world_gravity_z * p.gravity_scale).abs() };
    with(|st| {
        st.clock += dt as f64;
        let equip_changed = st.equip.is_some_and(|e| e != equip);
        st.equip = Some(equip);
        sync(st, sim);
        let (Some(poi), Some(d)) = (st.poi.clone(), st.data.clone()) else { return };
        for i in BOT_OPENS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default() {
            if !st.conts.get(i).is_some_and(|c| c.live() && !c.search.searched() && !c.destroyed) { continue; }
            st.conts[i].search = ct::Search { state: ct::SearchState::Searched, elapsed: 0., duration: 0. };
            let top_cm = st.conts[i].prop.and_then(|pi| sim.world.props.get(pi)).map(|h| (h.pos.y + h.height) * 100.);
            open(st, i, &poi, &d, sim.frames, gravity_cm, top_cm, &mut sounds, &mut tosses);
        }
        for i in 0..st.conts.len() {
            let c = &st.conts[i];
            if c.destroyed || !c.live() || c.search.searched() { continue; }
            let proto = fn_combat::POI_PROTO | poi.containers[c.poi].actor;
            if !c.prop.and_then(|pi| sim.world.props.get(pi)).is_some_and(|h| !h.alive()) || !crate::demolition_hook::cascade_killed(proto) { continue; }
            let c = &mut st.conts[i];
            c.destroyed = true;
            if c.ambient_on { sounds.push(SoundEvent::ContainerLoopStop { key: LOOP_KEY_BASE + 2 * i as u32, fade: AMBIENT_FADE_OUT }); c.ambient_on = false; }
            if c.search_loop_on { sounds.push(SoundEvent::ContainerLoopStop { key: LOOP_KEY_BASE + 2 * i as u32 + 1, fade: ct::SEARCH_LOOP_FADE }); c.search_loop_on = false; }
            c.search = ct::Search::default();
            if let Some(x) = c.closed.as_mut() { x.deactivate(); }
            c.closed_active = false;
            c.light_visible = false;
            st.log.push(format!("[chests] f{} {} #{i} destroyed with what it was attached to (no loot: ABuildingContainer vfunc_240 spills only for a player instigator)", sim.frames, poi.containers[st.conts[i].poi].class));
        }
        let eye = sim.cam_view.eye;
        let fwd = sim.cam_view.forward.normalize_or_zero();
        let feet = sim.feet();
        let pawn = Vec3::new(feet.x, feet.y + fn_game::pickups::PAWN_HALF_HEIGHT, feet.z);
        st.eye_ue = to_ue(eye);
        let props: &[fn_combat::Harvestable] = &sim.world.props;
        // ---- focus: the first thing the interaction trace hits is a live, unsearched container (InteractDistance 256 / InteractTraceExtent 15 from the
        //      camera, reach measured from the pawn, as pickups_hook / door_hook; the held target keeps the focus while it is still the first hit)
        let reach = fn_game::pickups::INTERACT_DISTANCE + (eye - pawn).length();
        let prop_t = fn_props::trace_in(props, sim.fresh_prop_index(), eye, eye + fwd * reach, fn_game::pickups::INTERACT_TRACE_EXTENT).map(|f| f * reach);
        let piece_t = if sim.building_hulls.is_empty() { None } else { fn_combat::trace_sphere_hulls(&sim.building_hulls, eye, eye + fwd * reach, fn_game::pickups::INTERACT_TRACE_EXTENT).map(|f| f * reach) };
        let all_t = match (prop_t, piece_t) { (Some(x), Some(y)) => Some(x.min(y)), (x, y) => x.or(y) };
        let mut best: Option<(f32, usize)> = None;
        for (i, c) in st.conts.iter().enumerate() {
            if !c.live() || c.search.searched() { continue; }
            let Some(h) = c.prop.and_then(|pi| props.get(pi)).filter(|h| h.alive()) else { continue };
            if (h.pos - pawn).length() > 6. { continue; }
            if let Some(f) = fn_props::trace(std::slice::from_ref(h), eye, eye + fwd * reach, fn_game::pickups::INTERACT_TRACE_EXTENT) {
                let t = f * reach;
                if (eye + fwd * t - pawn).length() > ct::interact_limit_m() { continue; }
                if best.map_or(true, |b| t < b.0) { best = Some((t, i)); }
            }
        }
        let prev_focus = st.focus;
        st.focus = best.filter(|(t, _)| all_t.map_or(true, |o| *t <= o + 1e-3)).map(|b| b.1);
        if blocked { st.focus = None; }
        if st.focus != prev_focus && trace_on() { st.log.push(format!("[chests] f{} focus {:?} -> {:?} (best {:?}, first hit {:?}, blocked {blocked}, pawn {:.2},{:.2},{:.2})", sim.frames, prev_focus, st.focus, best, all_t, pawn.x, pawn.y, pawn.z)); }
        let down = use_down || crate::interact::script_use();
        let pressed = down && !st.use_was_down;
        st.use_was_down = down;
        // ---- the search (only the focused container starts; a running one ends when it loses the focus)
        for i in 0..st.conts.len() {
            let focused = st.focus == Some(i);
            if !focused && !st.conts[i].search.active() { continue; }
            let def = st.conts[i].def.clone();
            let duration = def.as_deref().map_or(ct::SEARCH_TIME_FALLBACK, |c| ct::search_time(c, d.tier(c), &d.search_speed));
            let ev = st.conts[i].search.update(&ct::SearchInput { use_down: down, use_pressed: pressed, focused, duration, cancel: equip_changed }, dt);
            let pc = &poi.containers[st.conts[i].poi];
            let at = to_game(pc.t);
            let key = LOOP_KEY_BASE + 2 * i as u32 + 1;
            for e in ev {
                match e {
                    ct::SearchEvent::Begin => {
                        st.log.push(format!("[chests] f{} search begins: {} #{i} ({:.2} s, row {})", sim.frames, pc.class, duration, def.as_deref().map_or(String::new(), |c| ct::search_speed_row(c, d.tier(c)))));
                        if let Some(cue) = def.as_ref().and_then(|c| c.searching_loop.as_deref()).and_then(fn_audio_sink::container_cue) { sounds.push(SoundEvent::ContainerLoop { cue, key, pos: Some(at) }); st.conts[i].search_loop_on = true; }
                    }
                    ct::SearchEvent::Interrupted | ct::SearchEvent::Complete => {
                        if st.conts[i].search_loop_on { sounds.push(SoundEvent::ContainerLoopStop { key, fade: ct::SEARCH_LOOP_FADE }); st.conts[i].search_loop_on = false; }
                        if e == ct::SearchEvent::Interrupted { st.log.push(format!("[chests] f{} search interrupted: {} #{i}{}", sim.frames, pc.class, if equip_changed { " (equipment changed)" } else { "" })); }
                        else { let top_cm = st.conts[i].prop.and_then(|pi| props.get(pi)).map(|h| (h.pos.y + h.height) * 100.); open(st, i, &poi, &d, sim.frames, gravity_cm, top_cm, &mut sounds, &mut tosses); }
                    }
                }
            }
        }
        // ---- the chest's ambient AudioComponent near the listener (faded out for good by OnLoot / OnSetSearched, see `open`)
        for (i, c) in st.conts.iter_mut().enumerate() {
            if c.kind != Kind::Chest || !c.live() || c.search.searched() || c.destroyed { continue; }
            let pc = &poi.containers[c.poi];
            let dist = (pc.t - st.eye_ue).length();
            let want = dist <= AMBIENT_RANGE_CM || (c.ambient_on && dist <= AMBIENT_RANGE_CM + HYSTERESIS_CM);
            if want && !c.ambient_on { if let Some(cue) = fn_audio_sink::container_cue(AMBIENT_CUE) { sounds.push(SoundEvent::ContainerLoop { cue, key: LOOP_KEY_BASE + 2 * i as u32, pos: Some(to_game(pc.t)) }); c.ambient_on = true; } }
            if !want && c.ambient_on { sounds.push(SoundEvent::ContainerLoopStop { key: LOOP_KEY_BASE + 2 * i as u32, fade: 0. }); c.ambient_on = false; }
        }
        let fwd_ue = to_ue(fwd);
        let pawn_yaw = fwd_ue.y.atan2(fwd_ue.x).to_degrees();
        jiggle_mids(st, &poi, &d, pawn_yaw);
        for l in st.log.drain(..) { eprintln!("{l}"); }
    });
    for (item, count, a, b) in tosses { crate::pickups_hook::queue_toss(&item, count, a, b); }
    sounds
}

fn jiggle_mids(st: &mut State, poi: &crate::poi::PoiSet, d: &Data, pawn_yaw: f32) {
    let loot_curves = fn_live::var("FN_CHEST_JIGGLE").is_ok_and(|v| v == "loot");
    let (mi, ai) = if loot_curves { (0, 1) } else { (2, 3) };
    let (Some(mag), Some(ang)) = (d.jiggle[mi].as_ref(), d.jiggle[ai].as_ref()) else { return };
    let mut m = HashMap::new();
    for c in st.conts.iter_mut() {
        let Some(t0) = c.bounce else { continue };
        let since = (st.clock - t0) as f32;
        if since >= ct::BOUNCE_WINDOW || c.destroyed { c.bounce = None; continue; }
        let Some(def) = c.def.as_deref() else { continue };
        let pc = &poi.containers[c.poi];
        let center = ct::bounce_center(def, pc.t, pc.q, pc.s);
        let radius = ct::bounce_radius(def);
        let (v, hit) = ct::jiggle(&|t| mag.eval(t), mag.time_range(), &|t| ang.eval(t), ang.time_range(), since, pawn_yaw);
        let mid = Arc::new(crate::statics_render::ActorMid {
            scalars: vec![("Code_HitAnimation", hit), ("WSDamageRadius", if radius <= 0. { 0. } else { 1. / radius }), ("InvertDamageFalloff", 0.)],
            vectors: vec![("WSDeformationCenter", [center.x, center.y, center.z, 1.]), ("Code_DeformationVector", [v.x, v.y, v.z, 1.])],
        });
        if trace_on() { st.log.push(format!("[chests] jiggle {} t {since:.3} s: Code_DeformationVector ({:.3}, {:.3}, {:.3}) Code_HitAnimation {hit:.3} centre ({:.0}, {:.0}, {:.0}) radius {radius:.0} (pawn yaw {pawn_yaw:.1}, {} curves)", pc.class, v.x, v.y, v.z, center.x, center.y, center.z, if loot_curves { "SeachLoot" } else { "SeachEmptyLoot" })); }
        if pc.searched_actor != crate::poi::NO_CLUSTER { m.insert(pc.searched_actor, mid.clone()); }
        m.insert(pc.actor, mid);
    }
    crate::statics_render::set_actor_mids(m);
}

/// Search complete: `OnLoot` (chest), the loot, the searched mesh.
#[allow(clippy::too_many_arguments)]
fn open(st: &mut State, i: usize, poi: &crate::poi::PoiSet, d: &Data, frame: u32, gravity_cm: f32, top_cm: Option<f32>, sounds: &mut Vec<SoundEvent>, tosses: &mut Vec<(String, u32, Vec3, Vec3)>) {
    let (kind, def) = (st.conts[i].kind, st.conts[i].def.clone());
    let pc = &poi.containers[st.conts[i].poi];
    let group = def.as_ref().and_then(|c| ct::athena_tier_group(&c.tier_group));
    st.serial += 1;
    let mut rng = Rng::new(st.seed ^ (0x4c4f_4f54 + st.serial));
    let drops = group.map(|g| d.tables.roll(g, &mut rng)).unwrap_or_default();
    let items = crate::pickups_hook::item_rarities();
    // CHOSEN (server-set `HighestRarity`): the highest rarity of the rolled items
    let highest = drops.iter().filter_map(|x| items.as_ref().and_then(|m| m.get(x.item.rsplit('.').next().unwrap_or(&x.item)).copied())).max().unwrap_or(0);
    st.log.push(format!("[chests] f{frame} {} #{i} searched: group {} -> {} drops [{}], highest rarity {}", pc.class, group.unwrap_or("None"), drops.len(), drops.iter().map(|x| format!("{} x{}", x.item.rsplit('.').next().unwrap_or(&x.item), x.count)).collect::<Vec<_>>().join(", "), fn_game::pickup_items::rarity_label(highest)));
    let tier = def.as_deref().map_or(0, |c| d.tier(c));
    for x in &drops {
        let (a, v) = ct::toss_launch(def.as_deref(), pc.t, pc.q, pc.s, &mut rng);
        let a = Vec3::new(a.x, a.y, a.z.max(top_cm.unwrap_or(a.z)) + fn_game::pickups::TOUCH_HALF_HEIGHT * 100.);
        let _ = (tier, gravity_cm);
        tosses.push((x.item.clone(), x.count.max(1) as u32, to_game(a), to_game(v)));
    }
    if kind == Kind::Chest {
        // Tiered_Chest_6_Parent.OnLoot: DoOnce PlaySoundAtLocation(Tiered_Chest_Open_T01_Cue, actor location); Closed_Chest.Deactivate; SetLightVisibility(false);
        // SpawnEmitterAtLocation(P_Loot_Chest_T1, actor location, MakeRotator(roll, pitch + 90, yaw), bAutoDestroy) + SetVectorParameter(color, Color2 of HighestRarity)
        if let Some(cue) = fn_audio_sink::container_cue(OPEN_CUE) { sounds.push(SoundEvent::BuildingCue { cue, pos: Some(to_game(pc.t)) }); }
        if st.conts[i].ambient_on { sounds.push(SoundEvent::ContainerLoopStop { key: LOOP_KEY_BASE + 2 * i as u32, fade: AMBIENT_FADE_OUT }); st.conts[i].ambient_on = false; }
        if let Some(c) = st.conts[i].closed.as_mut() { c.deactivate(); }
        st.conts[i].closed_active = false;
        st.conts[i].light_visible = false;
        if let Some(sys) = st.catalog.as_ref().and_then(|c| c.system(P_OPEN)) {
            let (yaw, pitch, roll) = rotator(pc.q);
            let dist = (pc.t - st.eye_ue).length();
            let mut inst = Instance::new(sys, Transform { pos: pc.t, rot: rot_matrix(pitch + 90., yaw, roll), scale: 1. }, 0x4f50_454e ^ st.serial, dist);
            let c2 = d.rarity_color2.get(highest as usize).copied().unwrap_or([1.; 4]);
            inst.set_vector_parameter("color", [c2[0], c2[1], c2[2]]);
            st.bursts.push(inst);
        }
    }
    // vfunc_428: marked searched, SearchedMesh swapped in (the harvestables swap on the next sync)
    st.conts[i].bounce = Some(st.clock);
    st.props_len = usize::MAX;
    st.opened += 1;
}

/// The chest particles of this frame (the closed-chest mist near the view, the open bursts), on their cooked materials.
pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
    with(|st| {
        let Some(cat) = st.catalog.clone() else { return };
        let Some(poi) = st.poi.clone() else { return };
        let eye_ue = fn_fx::view::to_ue(view.eye).as_vec3();
        let env = Env::none();
        // Closed_Chest: activated by ReceiveBeginPlay on a spawned, unsearched chest; drawn within the component's draw distance (the level instance's
        // CachedMaxDrawDistance, 7652.8 cm in the house worlds; the template's 2000 is overridden there) while its POI group is resident.
        let closed_sys = cat.system(P_CLOSED);
        for c in st.conts.iter_mut().filter(|c| c.kind == Kind::Chest) {
            let pc = &poi.containers[c.poi];
            let resident = pc.group == crate::poi::NO_CLUSTER || poi.groups.get(pc.group as usize).is_some_and(|g| g.resident.load(std::sync::atomic::Ordering::Relaxed) && g.ready.load(std::sync::atomic::Ordering::Relaxed));
            let wanted = c.live() && !c.search.searched() && !c.destroyed;
            let dist = (pc.t - eye_ue).length();
            let in_range = resident && (pc.cull_cm <= 0. || dist <= pc.cull_cm);
            if wanted && in_range && c.closed.is_none() {
                if let (Some(s), Some((_, _, l2w, _))) = (closed_sys.clone(), pc.particles.iter().find(|p| p.1.split('.').next() == Some(P_CLOSED))) {
                    let (sc, q, t) = l2w.to_scale_rotation_translation();
                    st.serial += 1;
                    c.closed = Some(Instance::new(s, Transform { pos: t, rot: Mat3::from_quat(q), scale: sc.x }, 0x434c_4f53 ^ st.serial, dist));
                    c.closed_active = true;
                    c.light_visible = true;
                }
            }
            if !in_range { c.closed = None; c.closed_active = false; }
            if let Some(i) = c.closed.as_mut() { i.tick(dt, &env); }
            if c.closed.as_ref().is_some_and(|i| !c.closed_active && i.finished()) { c.closed = None; }
        }
        for b in st.bursts.iter_mut() { b.tick(dt, &env); }
        st.bursts.retain(|b| !b.finished());
        if st.bursts.is_empty() && st.conts.iter().all(|c| c.closed.is_none()) { return; }
        let eye = [eye_ue.x, eye_ue.y, eye_ue.z];
        let State { dxbc: slot, install, conts, bursts, errors, .. } = st;
        let Some(dx) = dxbc(slot, install, fx) else { return };
        dx.begin(view);
        for i in conts.iter().filter_map(|c| c.closed.as_ref()).chain(bursts.iter()) { draw(dx, fx, &cat, i, eye, dt, errors); }
        let fr = dx.take_frame();
        out.buffer_writes.extend(fr.buffer_writes);
        out.draws.extend(fr.draws);
    });
}

/// The focused (or still searching) container's prompt: (key text, context text, world anchor in sim metres, hold `Percentage`). CHOSEN anchor: the
/// actor's centroid (`CentroidOffset`), as the door hook anchors on its leaf.
pub fn prompt(key: &str) -> Option<(String, String, Vec3, f32)> {
    with(|st| {
        let i = st.focus.filter(|&i| st.conts.get(i).is_some_and(|c| c.live() && !c.search.searched() && !c.destroyed)).or_else(|| st.conts.iter().position(|c| c.search.active()))?;
        let c = &st.conts[i];
        let pc = &st.poi.as_ref()?.containers[c.poi];
        let co = c.def.as_ref().and_then(|d| d.props.get("CentroidOffset")).and_then(|v| v.as_array()).map(|a| Vec3::new(0., 0., a.get(2).and_then(|z| z.as_f64()).unwrap_or(0.) as f32)).unwrap_or(Vec3::ZERO);
        Some((key.to_owned(), ct::search_text().to_owned(), to_game(pc.t + pc.q * (co * pc.s)), c.search.progress()))
    }).flatten()
}

/// A container search hold runs this frame (`GA_DefaultPlayer_InteractSearch` active: the pawn plays its Search montage, see `Arms::set_interact_search`).
pub fn trasure_light_states() -> Vec<(u32, bool)> {
    with(|st| { let Some(poi) = st.poi.clone() else { return vec![] }; st.conts.iter().filter(|c| c.kind == Kind::Chest).map(|c| (poi.containers[c.poi].actor, c.live() && !c.search.searched() && !c.destroyed)).collect() }).unwrap_or_default()
}
pub const BOT_CHEST_BASE: u64 = 0x4348_0000_0000_0000;
static BOT_OPENS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
pub fn bot_list(center: Vec3, r: f32, out: &mut Vec<(u64, Vec3)>) {
    with(|st| {
        let Some(poi) = st.poi.clone() else { return };
        for (i, c) in st.conts.iter().enumerate() {
            if c.kind != Kind::Chest || !c.live() || c.search.searched() || c.destroyed { continue; }
            let p = to_game(poi.containers[c.poi].t);
            if (p - center).length() <= r { out.push((BOT_CHEST_BASE + i as u64, p)); }
        }
    });
}
pub fn bot_open(id: u64) -> bool {
    let Some(i) = id.checked_sub(BOT_CHEST_BASE).map(|i| i as usize) else { return false };
    let ok = with(|st| st.conts.get(i).is_some_and(|c| c.live() && !c.search.searched() && !c.destroyed)).unwrap_or(false);
    if ok { if let Ok(mut q) = BOT_OPENS.lock() { q.push(i); } }
    ok
}
pub fn searching() -> bool { with(|st| st.conts.iter().any(|c| c.search.active())).unwrap_or(false) }

/// Run report line.
pub fn report() -> String {
    with(|st| {
        if st.conts.is_empty() { return String::new(); }
        let (cp, cs, ap, asp) = st.counts;
        let lights: usize = st.poi.as_ref().map_or(0, |p| st.conts.iter().filter(|c| c.spawned).map(|c| p.containers[c.poi].lights.len()).sum());
        format!("chests: {cp} chests placed / {cs} spawned, {ap} ammo boxes placed / {asp} spawned, {} searched; closed-chest FX live {}; {lights} TrasureLight point lights on spawned chests, lit by the local-light path until searched\n", st.opened, st.conts.iter().filter(|c| c.closed.is_some()).count())
    }).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_container_materials_take_the_search_jiggle() {
        let Ok(set) = fn_pak::PakSet::open_install(fn_fx::default_install()) else { return };
        for m in ["/Game/Environments/Props/Boxes/Materials/MI_TreasureChestlvl1.MI_TreasureChestlvl1", "/Game/Environments/Props/Boxes/Materials/MI_LOOT_ShortAmmoCrate.MI_LOOT_ShortAmmoCrate"] {
            let mat = fn_fx::material::Material::load(&set, m).unwrap();
            let (a, x, b) = fn_fx::data::package_files(&set, &mat.shader_map_package).unwrap();
            let p = fn_ueio::Package::parse(&a, &x, &b).unwrap();
            let fl = fn_fx::uniform::compilation_flags(&x, &p.names);
            let u = format!("{:?}", mat.uniform);
            assert!(fl.is_some_and(|f| f.uses_world_position_offset && f.modifies_mesh_position), "{m}: {fl:?}");
            for n in ["WSDeformationCenter", "Code_DeformationVector", "Code_HitAnimation", "WSDamageRadius"] { assert!(u.contains(&format!("\"{n}\"")), "{m}: {n}"); }
        }
    }
    #[test] fn the_open_burst_points_up_from_an_upright_chest() {
        let q = Quat::from_rotation_z(30f32.to_radians());
        let (yaw, pitch, roll) = rotator(q);
        assert!((yaw - 30.).abs() < 1e-3 && pitch.abs() < 1e-3 && roll.abs() < 1e-3);
        let m = rot_matrix(pitch + 90., yaw, roll);
        assert!((m.x_axis - Vec3::Z).length() < 1e-4, "MakeRotator(roll, pitch + 90, yaw): the emitter's X axis is world up");
        assert!((rot_matrix(0., 90., 0.).x_axis - Vec3::Y).length() < 1e-4, "yaw 90 turns X to +Y (UE)");
    }
}
