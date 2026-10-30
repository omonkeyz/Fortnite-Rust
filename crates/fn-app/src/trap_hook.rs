use crate::build_input::{BuildInput, BuildIntent};
use crate::building_hook::{sim_to_world, world_to_fx};
use crate::pickups_dxbc::Dxbc;
use fn_audio_sink::SoundEvent;
use fn_building::grid;
use fn_building::world::{PieceId, PieceKind};
use fn_fx::view::ViewParams;
use fn_game::traps::{self as tr, Attach, Behavior, Event, Machine, TrapDef};
use fn_game::GameSim;
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Transform};
use fn_render::fx::{FxFrame, FxRenderer};
use glam::{Mat3, Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

pub const TRAP_SLOT: usize = 4;

pub fn enabled() -> bool { static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_TRAPS").map_or(true, |v| v != "0")) }
fn hit_owner() -> bool { fn_live::var("FN_TRAPS_HIT_OWNER").map_or(false, |v| v == "1") }

#[derive(Clone, Debug)]
pub struct TrapDraw { pub mesh: String, pub l2w: Mat4, pub materials: Vec<Option<String>>, pub all_slots: Option<String>, pub scalars: Vec<(String, f32)>, pub vectors: Vec<(String, [f32; 4])>, pub casts: bool }

struct Loaded { defs: Vec<TrapDef>, sockets: HashMap<(String, String), Vec3>, notes: Vec<String>, icons: Mutex<Vec<(String, fn_ui::assets::Image)>> }

struct Placed {
    def: usize,
    piece: PieceId,
    slot: u8,
    class: String,
    loc: Vec3,
    yaw: f32,
    m: Machine,
    age: f32,
    reset_at: Option<f32>,
    fire_at: Option<f32>,
    retract_sound_done: bool,
    idle_on: bool,
    serial: u64,
}

#[derive(Clone, Debug)]
struct Preview { def: usize, loc: Vec3, yaw: f32, valid: bool, target: Option<(PieceId, u8)>, why: &'static str }

struct Spawned { inst: Instance, owner: u64 }

#[derive(Default)]
struct State {
    loading: Option<mpsc::Receiver<Result<Loaded, String>>>,
    tried: bool,
    data: Option<Arc<Loaded>>,
    inventory: Vec<(usize, u32)>,
    selected: Option<usize>,
    traps: Vec<Placed>,
    preview: Option<Preview>,
    rotation: i32,
    patterns: HashMap<String, Option<String>>,
    sounds: Vec<SoundEvent>,
    catalog: Option<Arc<Catalog>>,
    catalog_tried: bool,
    install: Option<PathBuf>,
    roots: Vec<PathBuf>,
    dxbc: Option<Result<Dxbc, String>>,
    live: Vec<Spawned>,
    errors: HashSet<String>,
    log: Vec<String>,
    serial: u64,
    time: f32,
    script: Vec<(f32, String, bool)>,
    script_read: bool,
    stats: (u32, u32, u32, f32),
    camera: Vec3,
    floor_immune_until: f32,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(State::default)) }

pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) { with(|s| { s.install = install; s.roots = roots.to_vec(); }); }

fn say(s: &mut State, line: String) { eprintln!("[traps] {line}"); s.log.push(line); if s.log.len() > 400 { s.log.drain(..100); } }

fn socket_locations(set: &fn_pak::PakSet, mesh: &str, out: &mut HashMap<(String, String), Vec3>) {
    let long = fn_fx::data::long_package(mesh).to_owned();
    let Ok(pkg) = fn_fx::data::PackageJson::load(set, &long) else { return };
    for e in pkg.of_class("StaticMeshSocket") {
        let Some(n) = e.props.get("SocketName").and_then(|v| v.as_str()) else { continue };
        let l = e.props.get("RelativeLocation").and_then(|v| v.as_array()).map(|a| Vec3::new(a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32, a[2].as_f64().unwrap_or(0.) as f32)).unwrap_or(Vec3::ZERO);
        out.insert((mesh.to_owned(), n.to_owned()), l);
    }
}

fn start_load(s: &mut State) {
    if s.tried { return; }
    s.tried = true;
    let Some(dir) = s.install.clone() else { say(s, "no game install: traps disabled".into()); return };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = fn_pak::PakSet::open_install(&dir).map_err(|e| e.to_string()).map(|set| {
            let (defs, notes) = tr::load_all(&set);
            let mut sockets = HashMap::new();
            for d in &defs { socket_locations(&set, &d.mesh, &mut sockets); }
            let pa = fn_ui::pak_assets::PakAssets::from_set(set);
            let mut icons = vec![];
            let mut notes = notes;
            for d in &defs { match pa.decode_texture(&d.icon) { Ok(i) => icons.push((d.icon.clone(), i)), Err(e) => notes.push(format!("icon {}: {e}", d.icon)) } }
            Loaded { defs, sockets, notes, icons: Mutex::new(icons) }
        });
        let _ = tx.send(r);
    });
    s.loading = Some(rx);
}

fn poll_load(s: &mut State) {
    let Some(rx) = &s.loading else { return };
    let Ok(r) = rx.try_recv() else { return };
    s.loading = None;
    match r {
        Ok(l) => {
            for d in &l.defs {
                let line = format!("{} \"{}\" {:?} stack {} arm {} fire delay {} reload {} damage {} durability {} in loot {} shapes {}", d.id, d.name, d.attach, d.max_stack, d.arm_time, d.fire_delay, d.reload_time, d.damage, d.max_durability, d.in_loot, d.allowed_shapes.iter().map(|a| tr::short(a)).collect::<Vec<_>>().join(","));
                say(s, line);
            }
            for n in &l.notes { say(s, format!("not loaded: {n}")); }
            s.data = Some(Arc::new(l));
        }
        Err(e) => say(s, format!("trap data unavailable: {e}")),
    }
}

fn def_index(s: &State, id: &str) -> Option<usize> { s.data.as_ref()?.defs.iter().position(|d| d.id.eq_ignore_ascii_case(tr::short(id))) }

pub fn refusal(id: &str, count: u32) -> Option<&'static str> {
    if !enabled() { return Some("Can't pickup this item!"); }
    with(|s| {
        let Some(i) = def_index(s, id) else { return Some("Can't pickup this item!") };
        let max = s.data.as_ref().unwrap().defs[i].max_stack;
        let have = s.inventory.iter().find(|e| e.0 == i).map_or(0, |e| e.1);
        (have >= max && count > 0).then_some("Can't hold more of this item!")
    })
}

pub fn give(id: &str, count: u32) -> Result<String, String> {
    if !enabled() { return Err("traps are off (FN_TRAPS=0)".into()); }
    with(|s| {
        let i = def_index(s, id).ok_or_else(|| format!("{id}: not a loaded trap"))?;
        let d = s.data.as_ref().unwrap().defs[i].clone();
        let e = match s.inventory.iter_mut().find(|e| e.0 == i) { Some(e) => e, None => { s.inventory.push((i, 0)); s.inventory.last_mut().unwrap() } };
        let before = e.1;
        e.1 = (e.1 + count).min(d.max_stack);
        let now = e.1;
        if s.selected.is_none() { s.selected = Some(i); }
        let line = format!("{} x {}: trap slot now {now} (max {}){}", now - before, d.name, d.max_stack, if before + count > d.max_stack { ", the rest does not fit" } else { "" });
        say(s, line.clone());
        Ok(line)
    })
}

pub fn piece_has_trap(id: PieceId) -> bool { with(|s| s.traps.iter().any(|t| t.piece == id && !t.m.gone())) }
pub fn count() -> usize { with(|s| s.traps.len()) }

fn pattern_of(s: &mut State, set: &fn_pak::PakSet, class: &str) -> Option<String> {
    if let Some(p) = s.patterns.get(class) { return p.clone(); }
    let long = fn_fx::data::long_package(class).to_owned();
    let p = fn_fx::data::PackageJson::load(set, &long).ok().and_then(|pkg| pkg.exports.iter().find(|e| e.name.starts_with("Default__")).and_then(|c| c.props.get("EditModePatternData").and_then(|v| v.as_str()).map(str::to_owned)));
    s.patterns.insert(class.to_owned(), p.clone());
    p
}

fn slot_of(attach: Attach, wall_yaw: f32, trap_yaw: f32) -> u8 {
    match attach { Attach::Floor => 0, Attach::Ceiling => 1, Attach::Wall => if grid::normalize_axis(trap_yaw - wall_yaw).abs() < 1. { 0 } else { 1 } }
}

fn trap_l2w(loc: Vec3, yaw: f32) -> Mat4 { Mat4::from_translation(world_to_fx(loc)) * Mat4::from_rotation_z(yaw.to_radians()) }
fn to_game_fx(u: Vec3) -> Vec3 { Vec3::new(u.x, u.z, u.y) * 0.01 }

struct View { camera: Vec3, dir: Vec3, pawn: Vec3, half: f32 }
fn view(sim: &GameSim) -> View {
    let v = &sim.cam_view;
    let d = Vec3::new(v.forward.x, v.forward.z, v.forward.y).normalize_or_zero();
    let half = sim.controller.capsule_half_height_cm();
    View { camera: sim_to_world(v.eye), dir: d, pawn: sim_to_world(sim.feet()) + Vec3::new(0., 0., half), half }
}

fn compute_preview(s: &mut State, def_i: usize, v: &View) -> Preview {
    let data = s.data.clone().unwrap();
    let d = &data.defs[def_i];
    let along = (v.pawn - v.camera).dot(v.dir).max(0.);
    let start = v.camera + v.dir * along;
    let reach = tr::MAX_PLACEMENT_DISTANCE;
    let pieces: Vec<fn_building::world::PlacedPiece> = crate::building_hook::with_world(|w| w.iter().filter(|p| p.health > 0. && (p.location - start).length() < reach + 1200.).cloned().collect());
    let hit = crate::building_hook::with_catalog(|cat| {
        let cat = cat?;
        pieces.iter().flat_map(|p| fn_building::placement::piece_hulls(cat, p).into_iter().map(move |h| (p.id, p.kind, p.location, p.yaw_deg, p.effective_class().to_owned(), h)))
            .filter(|x| x.5.sdf(start) >= 0.).filter_map(|(id, k, l, y, c, h)| h.ray(start, v.dir, reach).map(|(t, n)| (t, n, id, k, l, y, c))).min_by(|a, b| a.0.total_cmp(&b.0))
    });
    let snap_yaw = (v.dir.y.atan2(v.dir.x).to_degrees() / 90.).round() * 90. + 90. * s.rotation as f32;
    let miss = |why| Preview { def: def_i, loc: start + v.dir * reach - Vec3::new(0., 0., 0.), yaw: snap_yaw, valid: false, target: None, why };
    let Some((t, n, id, kind, ploc, pyaw, class)) = hit else { return miss("no building in reach") };
    let hitp = start + v.dir * t;
    let (loc, yaw, kind_ok) = match d.attach {
        Attach::Floor | Attach::Ceiling => {
            let c = grid::grid_location(PieceKind::Floor, ploc, pyaw);
            let up = n.z > 0.5;
            let face_ok = kind == PieceKind::Floor && (if d.attach == Attach::Floor { up } else { n.z < -0.5 });
            if d.placement_offset.length_squared() > 1e-4 { let (l, y) = tr::best_quadrant(c, pyaw, d.placement_offset, hitp); (l, y, face_ok) }
            else { (tr::floor_transform(c, snap_yaw), snap_yaw, face_ok) }
        }
        Attach::Wall => {
            let y = if tr::wall_side(pyaw, ploc, v.camera) { pyaw } else { pyaw + 180. };
            (ploc, y, kind == PieceKind::Wall)
        }
    };
    if !kind_ok { return Preview { def: def_i, loc: hitp, yaw: snap_yaw, valid: false, target: None, why: "wrong surface for this trap" }; }
    let slot = slot_of(d.attach, pyaw, yaw);
    let pattern = crate::building_hook::with_set(|set| set.map(|set| pattern_of(s, set, &class))).flatten();
    if !tr::shape_allowed(d, pattern.as_deref()) { return Preview { def: def_i, loc, yaw, valid: false, target: None, why: "shape not in AllowedShapes" }; }
    if s.traps.iter().any(|t| t.piece == id && t.slot == slot) { return Preview { def: def_i, loc, yaw, valid: false, target: None, why: "slot already has a trap" }; }
    Preview { def: def_i, loc, yaw, valid: true, target: Some((id, slot)), why: "" }
}

fn sound(s: &mut State, path: &Option<String>, at_world: Vec3) {
    let Some(p) = path else { return };
    match fn_audio_sink::trap_cue(p) { Some(cue) => s.sounds.push(SoundEvent::BuildingCue { cue, pos: Some(to_game_fx(world_to_fx(at_world))) }), None => { if s.errors.insert(p.clone()) { say(s, format!("cue not in TRAP_CUES: {p}")); } } }
}

fn spawn_fx(s: &mut State, system: &str, pos_fx: Vec3, rot: Mat3) { spawn_fx_owned(s, system, pos_fx, rot, 0); }
fn spawn_fx_owned(s: &mut State, system: &str, pos_fx: Vec3, rot: Mat3, owner: u64) {
    let Some(cat) = s.catalog.clone() else { return };
    let Some(sys) = cat.system(system) else { if s.errors.insert(system.to_owned()) { say(s, format!("{system} is not in the particles-traps catalog: not spawned")); } return };
    s.serial += 1;
    s.live.push(Spawned { inst: Instance::new(sys, Transform { pos: pos_fx, rot, scale: 1. }, 0x5452_4150 ^ s.serial, 0.), owner });
}

fn emitter_xf(data: &Loaded, d: &TrapDef, e: &tr::Emitter, loc: Vec3, yaw: f32) -> (Vec3, Mat3) {
    let local = match &e.socket { Some(sock) => data.sockets.get(&(d.mesh.clone(), sock.clone())).copied().unwrap_or(Vec3::ZERO), None => e.location };
    let r = Mat3::from_rotation_z(yaw.to_radians());
    let er = Mat3::from_rotation_z(e.rotation[1].to_radians()) * Mat3::from_rotation_y(-e.rotation[0].to_radians()) * Mat3::from_rotation_x(e.rotation[2].to_radians());
    (world_to_fx(loc + r * local), r * er)
}

pub fn prebuild_catalog() { with(load_catalog); }
pub fn warm_ribbons(fx: &mut FxRenderer) {
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    let Some(cat) = st.catalog.clone() else { return };
    let State { dxbc, install, .. } = st;
    if dxbc.is_none() { *dxbc = Some(match install { Some(d) => Dxbc::load(d, fx), None => Err("no install".into()) }); }
    let Some(Ok(dx)) = dxbc.as_mut() else { return };
    for sys in cat.systems() { if sys.emitters.iter().any(|e| e.lods.iter().any(|l| matches!(l.type_data, fn_particles::model::TypeData::Ribbon(_)))) { dx.skip_non_normal_emitters(&sys.package); dx.warm_ribbon_materials(fx, &sys.package); } }
}

fn load_catalog(s: &mut State) {
    if s.catalog_tried { return; }
    s.catalog_tried = true;
    let dir = fn_live::var_os("FN_TRAPS_FX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| s.roots.iter().map(|r| r.join("particles-traps")).find(|d| d.join("index.json").is_file()));
    match dir.map(|d| (Catalog::load(&d), d)) {
        Some((Ok(c), d)) => { say(s, format!("particles loaded from {}", d.display())); s.catalog = Some(Arc::new(c)); }
        Some((Err(e), _)) => say(s, format!("{e}: no trap particles")),
        None => say(s, "no `particles-traps` folder in the asset roots: no trap particles".into()),
    }
}

fn run_script(s: &mut State, input: &mut BuildInput) -> Vec<(String, bool)> {
    if !s.script_read {
        s.script_read = true;
        if let Ok(v) = std::env::var("FNRT_TRAP_SCRIPT") {
            for part in v.split(';').filter(|p| !p.trim().is_empty()) {
                let (spec, at) = part.split_once('@').map(|(a, b)| (a.trim(), b.trim().parse().unwrap_or(0.))).unwrap_or((part.trim(), 0.));
                s.script.push((at, spec.to_owned(), false));
            }
        }
    }
    let mut out = vec![];
    for e in s.script.iter_mut() {
        if e.2 || s.time < e.0 { continue; }
        e.2 = true;
        out.push((e.1.clone(), true));
    }
    let _ = input;
    out
}

fn place_at(s: &mut State, def_i: usize, piece: PieceId, slot: u8, loc: Vec3, yaw: f32, class: String) {
    let data = s.data.clone().unwrap();
    let d = &data.defs[def_i];
    s.serial += 1;
    let p = Placed { def: def_i, piece, slot, class, loc, yaw, m: Machine::new(tr::Timing::of(d)), age: 0., reset_at: None, fire_at: None, retract_sound_done: true, idle_on: false, serial: s.serial };
    say(s, format!("placed {} on piece #{piece} slot {slot} at ({:.0}, {:.0}, {:.0}) yaw {:.0}; arms in {} s", d.id, loc.x, loc.y, loc.z, yaw, d.arm_time));
    sound(s, &d.sounds.placed.clone(), loc);
    if let Some(dust) = d.fx.placed_dust.clone() {
        let near = (s.camera - loc).length() <= tr::PLACEMENT_FX_DISTANCE;
        if near { let at = data.sockets.get(&(d.mesh.clone(), "PlacedFX".to_owned())).copied().unwrap_or(Vec3::ZERO); spawn_fx(s, &dust, world_to_fx(loc + tr::rot_z(yaw, at)), Mat3::from_rotation_z(yaw.to_radians())); }
    }
    s.traps.push(p);
    s.stats.0 += 1;
}

fn consume(s: &mut State, def_i: usize, input: &mut BuildInput) {
    if let Some(e) = s.inventory.iter_mut().find(|e| e.0 == def_i) { e.1 = e.1.saturating_sub(1); }
    s.inventory.retain(|e| e.1 > 0);
    if !s.inventory.iter().any(|e| Some(e.0) == s.selected) { s.selected = s.inventory.first().map(|e| e.0); }
    input.exit_build_mode();
}

pub fn frame(sim: &mut GameSim, input: &mut BuildInput, arms: Option<&mut crate::arms::Arms>, dt: f32) -> Vec<SoundEvent> {
    if !enabled() { input.set_slot_occupied(TRAP_SLOT, false); return vec![]; }
    let v = view(sim);
    let pawn_world = v.pawn;
    let half = v.half;
    let radius = fn_game::pickups::PAWN_RADIUS * 100.;
    let mut arms = arms;
    let mut launch: Option<Vec3> = None;
    let mut damage: Vec<(f32, Vec3)> = vec![];
    let intents: Vec<BuildIntent> = input.last_intents().to_vec();
    let alive: Vec<(PieceId, String)> = crate::building_hook::with_world(|w| w.iter().filter(|p| p.health > 0.).map(|p| (p.id, p.effective_class().to_owned())).collect());
    let sounds = with(|s| {
        s.time += dt;
        s.camera = v.camera;
        start_load(s);
        poll_load(s);
        load_catalog(s);
        let Some(data) = s.data.clone() else { return std::mem::take(&mut s.sounds) };
        for (cmd, _) in run_script(s, input) {
            let mut it = cmd.split(':');
            match it.next() {
                Some("give") => { let id = it.next().unwrap_or(""); let n = it.next().and_then(|x| x.parse().ok()).unwrap_or(1); if let Err(e) = give_locked(s, id, n) { say(s, e); } }
                Some("equip") => input.enter_build_mode(Some(TRAP_SLOT)),
                Some("standon") => {
                    let pick = crate::building_hook::with_world(|w| w.iter().filter(|p| p.health > 0. && p.kind == PieceKind::Floor).max_by_key(|p| p.id).map(|p| grid::grid_location(PieceKind::Floor, p.location, p.yaw_deg)));
                    if let Some(c) = pick {
                        let p = crate::building_hook::world_to_sim(c + Vec3::new(0., 0., 200.));
                        let h = sim.floor_below(p);
                        sim.controller.position = glam::Vec2::new(p.x, p.z - fn_game::sim::Z_OFFSET);
                        sim.controller.velocity = glam::Vec2::ZERO; sim.controller.vertical_speed = 0.; sim.controller.height = h;
                        sim.x = p.x; sim.z = p.z; sim.y = fn_game::EYE + h;
                        say(s, format!("script standon: pawn on the floor at ({:.0}, {:.0}, {:.0}), feet {h:.2} m", c.x, c.y, c.z));
                    }
                }
                Some("place") => s.script.push((s.time, "__place".into(), false)),
                Some("attach") => {
                    let id = it.next().unwrap_or("");
                    let Some(di) = def_index(s, id) else { say(s, format!("script attach: unknown trap {id}")); continue };
                    let d = data.defs[di].clone();
                    let want = if d.attach == Attach::Wall { PieceKind::Wall } else { PieceKind::Floor };
                    let pick = crate::building_hook::with_world(|w| w.iter().filter(|p| p.health > 0. && p.kind == want).max_by_key(|p| p.id).map(|p| (p.id, p.location, p.yaw_deg, p.effective_class().to_owned())));
                    match pick {
                        Some((pid, pl, py, class)) => {
                            let (loc, yaw) = match d.attach { Attach::Wall => (pl, if tr::wall_side(py, pl, v.camera) { py } else { py + 180. }), _ => { let c = grid::grid_location(PieceKind::Floor, pl, py); if d.placement_offset.length_squared() > 1e-4 { tr::best_quadrant(c, py, d.placement_offset, c) } else { (tr::floor_transform(c, py), py) } } };
                            let slot = slot_of(d.attach, py, yaw);
                            let pattern = crate::building_hook::with_set(|set| set.map(|set| pattern_of(s, set, &class))).flatten();
                            if !tr::shape_allowed(&d, pattern.as_deref()) { say(s, format!("script attach {id}: piece #{pid} pattern {pattern:?} not allowed")); }
                            else if s.traps.iter().any(|t| t.piece == pid && t.slot == slot) { say(s, format!("script attach {id}: piece #{pid} slot {slot} taken")); }
                            else if s.inventory.iter().any(|e| e.0 == di) { place_at(s, di, pid, slot, loc, yaw, class); consume(s, di, input); }
                            else { say(s, format!("script attach {id}: none in the inventory")); }
                        }
                        None => say(s, format!("script attach {id}: no {want:?} piece placed yet")),
                    }
                }
                _ => {}
            }
        }
        let scripted_place = s.script.iter_mut().filter(|e| e.1 == "__place" && !e.2).map(|e| { e.2 = true; }).count() > 0;
        input.set_slot_occupied(TRAP_SLOT, !s.inventory.is_empty());
        input.set_trap_icon(s.selected.map(|i| (data.defs[i].icon.clone(), data.defs[i].rarity)));
        let held = input.trap_held() && s.selected.is_some();
        if held {
            for i in &intents { if *i == BuildIntent::Rotate { s.rotation = s.rotation.wrapping_add(1); } }
            let sel = s.selected.unwrap();
            let pv = compute_preview(s, sel, &v);
            let place = intents.contains(&BuildIntent::Place) || scripted_place;
            if place {
                match (pv.valid, pv.target) {
                    (true, Some((pid, slot))) => { let class = alive.iter().find(|a| a.0 == pid).map(|a| a.1.clone()).unwrap_or_default(); place_at(s, pv.def, pid, slot, pv.loc, pv.yaw, class); consume(s, pv.def, input); s.preview = None; }
                    _ => say(s, format!("place refused: {}", pv.why)),
                }
            }
            if input.trap_held() { s.preview = Some(pv); }
        } else { s.preview = None; }
        let mut removed = vec![];
        for (k, t) in s.traps.iter().enumerate() {
            match alive.iter().find(|a| a.0 == t.piece) {
                None => removed.push((k, "its building was destroyed")),
                Some((_, c)) if *c != t.class => removed.push((k, "its building was edited")),
                _ => {}
            }
        }
        for (k, why) in removed.into_iter().rev() { let t = s.traps.remove(k); s.live.retain(|f| f.owner != t.serial); let id = data.defs[t.def].id.clone(); say(s, format!("{id} on piece #{} removed: {why}", t.piece)); }
        let owner_hit = hit_owner();
        let mut k = 0;
        while k < s.traps.len() {
            let di = s.traps[k].def;
            let d = &data.defs[di];
            let (loc, yaw) = (s.traps[k].loc, s.traps[k].yaw);
            let local = tr::rot_z(-yaw, pawn_world - loc);
            let can_trigger = !d.trigger_excludes_instigator || owner_hit;
            let immune = d.attach == Attach::Floor && s.time < s.floor_immune_until;
            let touching = can_trigger && !immune && tr::in_box(local, d.trigger_center, d.trigger_extent, radius, half);
            let evs = s.traps[k].m.update(dt, touching);
            s.traps[k].age += dt;
            let age = s.traps[k].age;
            for e in evs {
                match e {
                    Event::ReloadEnd => {
                        s.traps[k].reset_at = Some(age); s.traps[k].fire_at = None;
                        sound(s, &d.sounds.active.clone(), loc);
                        if !s.traps[k].idle_on { s.traps[k].idle_on = true; for em in d.fx.idle.clone() { let (p, r) = emitter_xf(&data, d, &em, loc, yaw); { let o = s.traps[k].serial; spawn_fx_owned(s, &em.system, p, r, o); } } }
                    }
                    Event::DelayBegin => {}
                    Event::Activate => {
                        s.traps[k].fire_at = Some(age); s.traps[k].retract_sound_done = false; s.traps[k].idle_on = false;
                        { let o = s.traps[k].serial; s.live.retain(|f| f.owner != o); }
                        sound(s, &d.sounds.fire.clone(), loc);
                        for em in d.fx.fire.clone() { let (p, r) = emitter_xf(&data, d, &em, loc, yaw); spawn_fx(s, &em.system, p, r); }
                        s.stats.1 += 1;
                        let line = format!("{} on piece #{} fired (durability {} / {})", d.id, s.traps[k].piece, s.traps[k].m.durability, d.max_durability);
                        say(s, line);
                    }
                    Event::Damage => {
                        let in_target = touching || tr::in_box(local, d.trace_location, d.target.half, radius, half);
                        match d.behavior {
                            Behavior::Damage if in_target && can_trigger => { damage.push((d.damage, loc + tr::rot_z(yaw, d.trace_location))); }
                            Behavior::Launch { .. } if in_target && !crate::bounce_hook::launched() => { crate::bounce_hook::mark_launched(); s.floor_immune_until = s.time + fn_live::f32("FN_TRAP_FLOOR_IMMUNE_TIME", tr::FLOOR_TRAP_IMMUNE_TIME); let pv = Vec3::new(sim.controller.velocity.x, sim.controller.velocity.y, 0.) * 100.; launch = tr::launch_velocity(d.behavior, pv); }
                            _ => {}
                        }
                    }
                    Event::ReloadBegin => {}
                    Event::OutOfDurability => {
                        { let o = s.traps[k].serial; s.live.retain(|f| f.owner != o); }
                        sound(s, &d.sounds.explode.clone(), loc);
                        if let Some(sys) = d.fx.destruction.clone() { spawn_fx(s, &sys, world_to_fx(loc), Mat3::IDENTITY); }
                        let line = format!("{} on piece #{} out of durability", d.id, s.traps[k].piece);
                        say(s, line);
                    }
                    Event::Destroy | Event::Placed => {}
                }
            }
            if let (Some(sp), Some(ft)) = (d.spike.as_ref(), s.traps[k].fire_at) {
                if !s.traps[k].retract_sound_done && age - ft >= sp.fire_len + sp.hold + sp.retract_sound_at { s.traps[k].retract_sound_done = true; sound(s, &d.sounds.reload.clone(), loc); }
            }
            if s.traps[k].m.gone() { let t = s.traps.remove(k); s.live.retain(|f| f.owner != t.serial); let line = format!("{} on piece #{} destroyed", data.defs[t.def].id, t.piece); say(s, line); continue; }
            k += 1;
        }
        std::mem::take(&mut s.sounds)
    });
    if let Some(a) = arms.as_deref_mut() {
        for (dmg, from) in damage {
            let (h0, s0) = (a.health, a.shield);
            let (h, sh) = tr::shield_first(dmg, a.health, a.shield);
            a.health = h; a.shield = sh;
            with(|s| { s.stats.2 += 1; s.stats.3 += dmg; say(s, format!("hit the pawn for {dmg}: shield {s0} -> {sh}, health {h0} -> {h}")); });
            crate::hurt_hook::on_damage(s0 > sh, world_to_fx(from));
            crate::kill_feed_hook::local_damaged(fn_ui::screens::kill_feed::DeathCause::Unspecified, fn_ui::screens::kill_feed::Killer::Themself, h);
        }
    }
    if let Some(l) = launch {
        let fin = sim.controller.launch(l, false, true);
        sim.fall.ignore_next = true;
        with(|s| say(s, format!("LaunchCharacterJump(({:.0}, {:.0}, {:.0}), XY add, Z override, bIgnoreFallDamage): pawn velocity ({:.0}, {:.0}, {:.0}) cm/s", l.x, l.y, l.z, fin.x, fin.y, fin.z)));
    }
    sounds
}

fn give_locked(s: &mut State, id: &str, count: u32) -> Result<(), String> {
    let i = def_index(s, id).ok_or_else(|| format!("{id}: not a loaded trap"))?;
    let max = s.data.as_ref().unwrap().defs[i].max_stack;
    match s.inventory.iter_mut().find(|e| e.0 == i) { Some(e) => e.1 = (e.1 + count).min(max), None => s.inventory.push((i, count.min(max))) }
    if s.selected.is_none() { s.selected = Some(i); }
    say(s, format!("script give {id} x {count}"));
    Ok(())
}

pub fn draws(view_time: f32) -> Vec<TrapDraw> {
    if !enabled() { return vec![]; }
    with(|s| {
        let Some(data) = s.data.clone() else { return vec![] };
        let mut out = vec![];
        for t in &s.traps {
            let d = &data.defs[t.def];
            let l2w = trap_l2w(t.loc, t.yaw);
            out.push(TrapDraw { mesh: d.mesh.clone(), l2w, materials: vec![], all_slots: None, scalars: vec![], vectors: vec![], casts: true });
            if let Some(sp) = &d.spike {
                let since_reset = t.reset_at.map(|r| t.age - r);
                let since_fire = t.fire_at.map(|f| t.age - f);
                if let Some(z) = tr::spike_scale(sp, since_reset, since_fire).filter(|z| *z > 1e-4) {
                    let mut scalars = vec![];
                    let mut vectors = vec![];
                    if let Some((speed, color, exp)) = sp.sweep {
                        let ev = since_fire.or(since_reset).unwrap_or(0.);
                        scalars.push(("TimeStampOffset".to_owned(), -(view_time - ev)));
                        scalars.push(("SweepSpeed".to_owned(), speed));
                        scalars.push(("SweepExponent".to_owned(), exp));
                        vectors.push(("SweepColor".to_owned(), color));
                    }
                    out.push(TrapDraw { mesh: sp.mesh.clone(), l2w: l2w * Mat4::from_scale(Vec3::new(1., 1., z)), materials: sp.materials.iter().map(|m| Some(m.clone())).collect(), all_slots: None, scalars, vectors, casts: true });
                }
            }
        }
        if let Some(p) = &s.preview {
            let d = &data.defs[p.def];
            let l2w = trap_l2w(p.loc, p.yaw);
            let sc = vec![(tr::PREVIEW_PARAM.to_owned(), if p.valid { 1. } else { 0. })];
            out.push(TrapDraw { mesh: d.mesh.clone(), l2w, materials: vec![], all_slots: Some(tr::PREVIEW_MATERIAL.to_owned()), scalars: sc.clone(), vectors: vec![], casts: false });
            if p.valid { if let Some(r) = &d.range_mesh { out.push(TrapDraw { mesh: r.clone(), l2w, materials: vec![], all_slots: Some(tr::PREVIEW_MATERIAL.to_owned()), scalars: sc, vectors: vec![], casts: false }); } }
        }
        out
    })
}

pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    let Some(cat) = st.catalog.clone() else { return };
    if st.live.is_empty() { return; }
    let env = Env::none();
    for s in st.live.iter_mut() { s.inst.tick(dt, &env); }
    st.live.retain(|s| !s.inst.finished());
    if st.live.is_empty() { return; }
    let eye = [view.eye.x, view.eye.y, view.eye.z];
    let State { dxbc, install, live, errors, .. } = st;
    if dxbc.is_none() { *dxbc = Some(match install { Some(d) => Dxbc::load(d, fx), None => Err("no install".into()) }); }
    let Some(Ok(dx)) = dxbc.as_mut() else { return };
    dx.begin(view);
    for s in live.iter() {
        let mut d = vec![];
        s.inst.draw(eye, &mut d);
        let bounds = crate::particles::instance_bounds(&s.inst, &cat);
        for e in &d { if let Err(err) = dx.draw_emitter(fx, e, &s.inst.xf, &bounds, dt) { let k = format!("{}/{}", e.system, e.emitter); if errors.insert(k.clone()) { eprintln!("[traps] {k}: NOT DRAWN: {err}"); } } }
    }
    let fr = dx.take_frame();
    out.buffer_writes.extend(fr.buffer_writes);
    out.draws.extend(fr.draws);
}

pub fn report() -> String {
    with(|s| {
        if s.data.is_none() && s.log.is_empty() { return String::new(); }
        let mut out = format!("traps: {} placed so far, {} fired, {} pawn hits ({} damage), {} live, inventory {:?}\n", s.stats.0, s.stats.1, s.stats.2, s.stats.3, s.traps.len(), s.inventory.iter().map(|e| (s.data.as_ref().map(|d| d.defs[e.0].id.clone()).unwrap_or_default(), e.1)).collect::<Vec<_>>());
        for l in &s.log { out.push_str("  "); out.push_str(l); out.push('\n'); }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn wall_slots_follow_the_side() {
        assert_eq!(slot_of(Attach::Wall, 90., 90.), 0);
        assert_eq!(slot_of(Attach::Wall, 90., 270.), 1);
        assert_eq!(slot_of(Attach::Floor, 0., 0.), 0);
        assert_eq!(slot_of(Attach::Ceiling, 0., 0.), 1);
    }
}

pub fn take_ui_images() -> Vec<(String, fn_ui::assets::Image)> {
    with(|s| s.data.as_ref().map(|d| std::mem::take(&mut *d.icons.lock().unwrap_or_else(|p| p.into_inner()))).unwrap_or_default())
}
