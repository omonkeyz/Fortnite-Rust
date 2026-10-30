//! DOOR HOOK (2026-10-08): every door of the game: the map's door walls (`poi::PoiSet::doors`, from the POI files' `doors[]`) and the player pieces whose
//! class has a `DoorMesh` (the door edits `PBWA_*_DoorC` / `DoorS` / `DoorSide` / `HalfWallDoor*`). One shared state machine (`fn_building::door::Door`)
//! per door; this file feeds it the Use key, the frame time and the pawn-overlap test, and hands out the results:
//! * the door leaves' collision to the sim (`GameSim::door_hulls`: movement, floor, camera) while their component collision is on;
//! * the interaction prompt (`Open` / `Close`, the Use key) of the focused door;
//! * the open / close cues (`SoundEvent::BuildingCue`, the cooked `DoorOpeningSound` / `DoorClosingSound`, at the door component);
//! * the draws: map door leaves via [`map_draws`] (building_render draws them on its movable-mesh path), player door leaves via [`piece_door`] (drawn
//!   with their piece in `building_render::draw_piece`, so they share its build-up MIDs, blueprint ghost and edit hiding).
//!
//! 1.8 facts are in `fn_building::door`. Port choices (labelled):
//! * CHOSEN: the interaction trace = the pickups' (`AFortPlayerController::InteractDistance` 256 / `InteractTraceExtent` 15 from the camera, reach measured from
//!   the pawn); the door is focused only when its leaf is the first thing that trace hits (FUN_1404c3a60 requires the hit component to be the
//!   DoorComponent) and its collision is on (`FortBuildingMesh` takes `FCC_Trace_Interaction`; off = the trace passes through).
//! * CHOSEN: a focused pickup keeps the Use key (one interaction per press; the pickups hook runs first).
//! * CHOSEN: the instigator's forward = the camera yaw (the Athena pawn's actor yaw follows the controller's), flattened.
//! * CHOSEN: the prompt's world anchor = the leaf's bounds centre (the indicator anchor of a building interaction was not looked up).
//! * vfunc_364's pawn overlap = the player capsule (`AFortPawn` radius 42 cm, half height 75 cm) against the leaf's cooked convexes.
//! * A map door is drawn while its wall's POI streaming group is resident and built (the POI streaming; `CullDistance` = 0, no own cull) and hidden with
//!   its wall once the wall is destroyed; NOT PORTED: hits on the leaf damaging the wall (the leaf is not in the weapon / pickaxe traces).
//! `FN_DOORS=0`: no doors at all (no draws, no collision, no prompt).
use fn_building::door::{Door, DoorDef, DoorEvent};
use fn_building::world::PieceId;
use fn_combat::Hull;
use fn_game::GameSim;
use glam::{Mat3, Mat4, Quat, Vec3};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// `FN_DOORS=0` switches every door off.
pub fn enabled() -> bool { static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_DOORS").map_or(true, |v| v != "0")) }

/// Reach of the per-frame door collision / overlap work around the pawn (sim metres): the door leaves within it are hulled every frame.
/// CHOSEN: 12 m (the movement reads hulls within 6 m of the capsule, the camera boom is shorter).
const NEAR_M: f32 = 12.0;

/// One door leaf to draw this frame (FX space).
#[derive(Clone, Debug)]
pub struct DoorDraw {
    pub mesh: String,
    /// Slot 0 (`DoorAnimatingMaterial`; `None` = the mesh's own).
    pub material: Option<String>,
    pub l2w: Mat4,
    pub center: Vec3,
    pub radius: f32,
}

/// A player piece's door this frame: the class's door data and the leaf's transform relative to the piece's own l2w is NOT used: building_render
/// composes it from the piece transform ([`leaf_l2w`]).
#[derive(Clone, Debug)]
pub struct PieceDoorView { pub def: Arc<DoorDef>, pub closed_yaw: f32, pub yaw: f32 }

struct MapDoor { door: Door, prop: Option<usize>, local: Arc<Vec<Hull>>, sphere: (Vec3, f32) }
struct PieceDoor { class: String, data: Arc<ClassDoor>, door: Door }
/// A door class's data: the CDO's door fields and the door mesh's convexes (mesh-local cm).
struct ClassDoor { def: Arc<DoorDef>, pieces: Vec<Vec<Vec3>>, hulls: Vec<Hull> }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key { Map(usize), Piece(PieceId) }

#[derive(Default)]
struct State {
    poi: Option<Arc<crate::poi::PoiSet>>,
    map: Vec<MapDoor>,
    pieces: HashMap<PieceId, PieceDoor>,
    classes: HashMap<String, Option<Arc<ClassDoor>>>,
    /// The props list the `prop` indices were found in (length; rebuilt when it changes).
    props_len: usize,
    focus: Option<Key>,
    use_was_down: bool,
    draws: Vec<DoorDraw>,
    log: Vec<String>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(State::default)) }

/// FX space (UE cm about the game origin) -> sim metres (the inverse of `fn_fx::view::to_ue`).
fn to_game(u: Vec3) -> Vec3 { Vec3::new(u.x, u.z, u.y) * 0.01 }
fn to_ue_dir(g: Vec3) -> Vec3 { Vec3::new(g.x, g.z, g.y) }

/// UE yaw (degrees) of a rotation: its forward (X) projected on the ground.
fn yaw_of(q: Quat) -> f32 { let f = q * Vec3::X; f.y.atan2(f.x).to_degrees() }

/// The door component's world transform (FX space): attached to the root at `DoorOffset` (root scale applies to the offset and stays on the component),
/// its world rotation = the root's with the yaw turned from the closed yaw to `yaw` (`SetWorldRotation(FRotator(ClosedPitch, Yaw, ClosedRoll))`).
pub fn leaf_l2w(root_t: Vec3, root_q: Quat, root_s: Vec3, offset: Vec3, closed_yaw: f32, yaw: f32) -> Mat4 {
    let pivot = root_t + root_q * (offset * root_s);
    let rot = Quat::from_rotation_z((yaw - closed_yaw).to_radians()) * root_q;
    Mat4::from_translation(pivot) * Mat4::from_quat(rot) * Mat4::from_scale(root_s)
}

/// The leaf's convexes at `l2w`, in sim metres (the POI hull rule: UE -> game swaps y / z, a reflection, so the winding flips unless `l2w` mirrors too).
fn local_hulls(pieces: &[Vec<Vec3>]) -> Vec<Hull> { pieces.iter().filter_map(|p| Hull::from_points(p)).collect() }
fn hulls_at(local: &[Hull], l2w: Mat4) -> Vec<Hull> {
    let flip = Mat3::from_mat4(l2w).determinant() > 0.;
    local.iter().map(|h| h.mapped(|v| to_game(l2w.transform_point3(v)), flip)).collect()
}

/// vfunc_364 stand-in: does the pawn capsule (sim metres, centre `c`) overlap any hull?
fn capsule_overlaps(hulls: &[Hull], c: Vec3) -> bool {
    let (r, hh) = (fn_game::pickups::PAWN_RADIUS, fn_game::pickups::PAWN_HALF_HEIGHT);
    let seg = (hh - r).max(0.);
    hulls.iter().any(|h| (0..=8).any(|i| { let p = c + Vec3::Y * (-seg + 2. * seg * i as f32 / 8.); h.sdf(p) < r }))
}

/// The pawn capsule centre (sim metres).
fn pawn_center(sim: &GameSim) -> Vec3 { let f = sim.feet(); Vec3::new(f.x, f.y + fn_game::pickups::PAWN_HALF_HEIGHT, f.z) }


/// The door data of a building class (CDO `DoorMesh` etc. + the door mesh's convexes), loaded once from the paks.
fn class_door(s: &mut State, set: &fn_pak::PakSet, class: &str) -> Option<Arc<ClassDoor>> {
    if let Some(c) = s.classes.get(class) { return c.clone(); }
    let r = (|| -> Result<Option<ClassDoor>, String> {
        let long = fn_fx::data::long_package(class).to_owned();
        let pkg = fn_fx::data::PackageJson::load(set, &long)?;
        let cdo = pkg.exports.iter().find(|e| e.name.starts_with("Default__")).ok_or_else(|| format!("{long}: no CDO"))?;
        let Some(def) = DoorDef::from_props(&cdo.props) else { return Ok(None) };
        let pieces = fn_building::pieces::load_convexes(set, &def.mesh)?;
        let hulls = local_hulls(&pieces);
        Ok(Some(ClassDoor { def: Arc::new(def), pieces, hulls }))
    })();
    let v = match r { Ok(v) => v.map(Arc::new), Err(e) => { s.log.push(format!("[doors] {class}: {e}")); eprintln!("[doors] {class}: {e}"); None } };
    s.classes.insert(class.to_owned(), v.clone());
    v
}

/// One frame (after the building hook): map / piece door bookkeeping, the Use key, the timers and swings, the sim's door collision. Returns the sounds.
pub fn frame(sim: &mut GameSim, use_down: bool, pickup_focused: bool, dt: f32) -> Vec<fn_audio_sink::SoundEvent> {
    if !enabled() { sim.door_hulls.clear(); return vec![]; }
    // the building world's door pieces (outside our lock: building_hook's lock first, never nested the other way)
    let editing = crate::building_hook::editing_piece();
    let pieces: Vec<(PieceId, String, Vec3, f32, bool)> = crate::building_hook::with_world(|w| w.iter().filter(|p| p.health > 0.).map(|p| (p.id, p.effective_class().to_owned(), crate::building_hook::world_to_fx(p.location), p.yaw_deg, p.edit.mirrored)).collect());
    let poi = crate::poi::current();
    let pawn = pawn_center(sim);
    let eye = sim.cam_view.eye;
    let fwd = sim.cam_view.forward.normalize_or_zero();
    let instigator = { let f = to_ue_dir(fwd); Vec3::new(f.x, f.y, 0.).normalize_or_zero() };
    let mut sounds = vec![];
    let props: &[fn_combat::Harvestable] = &sim.world.props;
    let hulls = with(|s| {
        // ---- map doors (once the POI set exists)
        if let Some(p) = &poi {
            if !s.poi.as_ref().is_some_and(|q| Arc::ptr_eq(q, p)) {
                let mut by_mesh: HashMap<String, Arc<Vec<Hull>>> = HashMap::new();
                s.map = p.doors.iter().map(|d| { let local = by_mesh.entry(d.def.mesh.clone()).or_insert_with(|| Arc::new(local_hulls(&d.pieces))).clone(); MapDoor { door: Door::new(yaw_of(d.q), d.mirrored), prop: None, local, sphere: local_sphere(&d.pieces) } }).collect();
                s.poi = Some(p.clone());
                s.props_len = usize::MAX;
            }
        }
        if s.props_len != props.len() {
            s.props_len = props.len();
            let by_proto: HashMap<u32, usize> = props.iter().enumerate().filter(|(_, h)| h.proto & fn_combat::POI_PROTO != 0).map(|(i, h)| (h.proto, i)).collect();
            for (i, m) in s.map.iter_mut().enumerate() { m.prop = s.poi.as_ref().and_then(|p| by_proto.get(&(fn_combat::POI_PROTO | p.doors[i].actor)).copied()); }
        }
        // ---- player doors: new / re-classed pieces get a fresh closed door (an edit spawns a new actor in 1.8), gone pieces lose theirs
        let set_ok = crate::building_hook::with_set(|set| {
            let Some(set) = set else { return false };
            for (id, class, loc, yaw, mirrored) in &pieces {
                if s.pieces.get(id).is_some_and(|d| &d.class == class) { continue; }
                match class_door(s, set, class) {
                    Some(data) => { s.pieces.insert(*id, PieceDoor { class: class.clone(), data, door: Door::new(*yaw, *mirrored) }); }
                    None => { s.pieces.remove(id); }
                }
                let _ = loc;
            }
            true
        });
        if set_ok { s.pieces.retain(|id, _| pieces.iter().any(|p| p.0 == *id)); }
        // ---- each door's world placement this frame
        let poi = s.poi.clone();
        let wall_alive = |m: &MapDoor| m.prop.map_or(true, |i| props.get(i).map_or(true, |h| h.alive()));
        let map_root = |i: usize| { let d = &poi.as_ref().unwrap().doors[i]; (d.t, d.q, d.s, d.def.offset) };
        let piece_root = |pd: &PieceDoor, id: &PieceId| pieces.iter().find(|p| p.0 == *id).map(|p| (p.2, Quat::from_rotation_z(p.3.to_radians()), Vec3::new(if p.4 { -1. } else { 1. }, 1., 1.), pd.data.def.offset));
        // near doors (sim metres) for collision / overlap / focus
        let near_map: Vec<usize> = (0..s.map.len()).filter(|&i| { let (t, ..) = map_root(i); (to_game(t) - pawn).length() <= NEAR_M && wall_alive(&s.map[i]) }).collect();
        let near_piece: Vec<PieceId> = s.pieces.keys().copied().filter(|id| pieces.iter().any(|p| p.0 == *id && (to_game(p.2) - pawn).length() <= NEAR_M)).collect();
        // ---- the Use key: the focused door (first hit of the interaction trace, collision on)
        let reach = fn_game::pickups::INTERACT_DISTANCE + (eye - pawn).length();
        let other = {
            let a = eye; let b = eye + fwd * reach;
            let prop_t = fn_props::trace_in(props, sim.fresh_prop_index(), a, b, fn_game::pickups::INTERACT_TRACE_EXTENT).map(|f| f * reach);
            // the pieces' cooked collision convexes (the movement's: a door edit's frame has its doorway open), not the damage trace's whole-wall box
            let piece = if sim.building_hulls.is_empty() { None } else { fn_combat::trace_sphere_hulls(&sim.building_hulls, a, b, fn_game::pickups::INTERACT_TRACE_EXTENT).map(|f| f * reach) };
            match (prop_t, piece) { (Some(x), Some(y)) => Some(x.min(y)), (x, y) => x.or(y) }
        };
        let mut best: Option<(f32, Key)> = None;
        let mut nearest: Option<(f32, f32)> = None; // diagnostics: (leaf hit t, hit-to-pawn distance) of any leaf, before the reach rule
        let mut consider = |key: Key, hulls: &[Hull]| {
            if let Some(f) = fn_combat::trace_sphere_hulls(hulls, eye, eye + fwd * reach, fn_game::pickups::INTERACT_TRACE_EXTENT) {
                let t = f * reach;
                let hit = eye + fwd * t;
                if nearest.map_or(true, |n| t < n.0) { nearest = Some((t, (hit - pawn).length())); }
                if (hit - pawn).length() > fn_game::pickups::INTERACT_DISTANCE + fn_game::pickups::INTERACT_TRACE_EXTENT { return; }
                if best.map_or(true, |b| t < b.0) { best = Some((t, key)); }
            }
        };
        for &i in &near_map {
            if !s.map[i].door.interactable() { continue; }
            let (t, q, sc, off) = map_root(i);
            let d = &s.map[i].door;
            consider(Key::Map(i), &hulls_at(&s.map[i].local, leaf_l2w(t, q, sc, off, d.closed_yaw, d.yaw)));
        }
        for id in &near_piece {
            let pd = &s.pieces[id];
            if !pd.door.interactable() || editing == Some(*id) { continue; }
            if let Some((t, q, sc, off)) = piece_root(pd, id) { consider(Key::Piece(*id), &hulls_at(&pd.data.hulls, leaf_l2w(t, q, sc, off, pd.door.closed_yaw, pd.door.yaw))); }
        }
        s.focus = best.filter(|(t, _)| other.map_or(true, |o| *t <= o + 1e-3)).map(|b| b.1);
        if pickup_focused { s.focus = None; }
        let down = use_down || crate::interact::script_use();
        let edge = down && !s.use_was_down;
        if edge && s.focus.is_none() && (!near_map.is_empty() || !near_piece.is_empty()) { s.log.push(format!("[doors] Use: no door focused (nearest leaf hit {:?} (t m, hit-to-pawn m; reach {:.2} m), other geometry first at {:?} m, pickup focused {pickup_focused})", nearest, fn_game::pickups::INTERACT_DISTANCE + fn_game::pickups::INTERACT_TRACE_EXTENT, other)); }
        s.use_was_down = down;
        // ---- interact, timers, swing (every door ticks: the swing and the timers run everywhere, like the actors' Tick)
        let mut events: Vec<(Key, DoorEvent)> = vec![];
        let locals: Vec<Arc<Vec<Hull>>> = s.map.iter().map(|m| m.local.clone()).collect();
        let blocked_map = |i: usize, yaw: f32, d: &Door| -> bool { let (t, q, sc, off) = map_root(i); capsule_overlaps(&hulls_at(&locals[i], leaf_l2w(t, q, sc, off, d.closed_yaw, yaw)), pawn) };
        if edge {
            if s.focus.is_some() { crate::interact::note_tap(); }
            match s.focus {
                Some(Key::Map(i)) => {
                    let mut d = s.map[i].door.clone();
                    let snapshot = d.clone();
                    for e in d.interact(instigator, &mut |y| near_map.contains(&i) && blocked_map(i, y, &snapshot)) { events.push((Key::Map(i), e)); }
                    s.log.push(format!("[doors] map door #{i} ({}): {} (yaw {:.1} -> {:.1})", poi.as_ref().map_or("", |p| p.classes.get(p.doors[i].actor as usize).map_or("", |c| c.as_str())), if d.open { "open" } else { "close" }, d.yaw, d.desired_yaw));
                    s.map[i].door = d;
                }
                Some(Key::Piece(id)) => {
                    let pd = s.pieces.get(&id).unwrap();
                    let (data, root) = (pd.data.clone(), piece_root(pd, &id));
                    let mut d = pd.door.clone();
                    let snapshot = d.clone();
                    let mut blocked = |y: f32| root.is_some_and(|(t, q, sc, off)| capsule_overlaps(&hulls_at(&data.hulls, leaf_l2w(t, q, sc, off, snapshot.closed_yaw, y)), pawn));
                    for e in d.interact(instigator, &mut blocked) { events.push((Key::Piece(id), e)); }
                    s.log.push(format!("[doors] piece door #{id} ({}): {} (yaw {:.1} -> {:.1})", pd.class.rsplit('.').next().unwrap_or(""), if d.open { "open" } else { "close" }, d.yaw, d.desired_yaw));
                    s.pieces.get_mut(&id).unwrap().door = d;
                }
                None => {}
            }
        }
        for i in 0..s.map.len() {
            if s.map[i].door.timers.is_empty() && !s.map[i].door.moving() { continue; }
            let mut d = s.map[i].door.clone();
            let snapshot = d.clone();
            let near = near_map.contains(&i);
            for e in d.tick(dt, &mut |y| near && blocked_map(i, y, &snapshot)) { events.push((Key::Map(i), e)); }
            s.map[i].door = d;
        }
        let ids: Vec<PieceId> = s.pieces.keys().copied().collect();
        for id in ids {
            let pd = &s.pieces[&id];
            if pd.door.timers.is_empty() && !pd.door.moving() { continue; }
            let (data, root) = (pd.data.clone(), piece_root(pd, &id));
            let mut d = pd.door.clone();
            let snapshot = d.clone();
            let mut blocked = |y: f32| root.is_some_and(|(t, q, sc, off)| capsule_overlaps(&hulls_at(&data.hulls, leaf_l2w(t, q, sc, off, snapshot.closed_yaw, y)), pawn));
            for e in d.tick(dt, &mut blocked) { events.push((Key::Piece(id), e)); }
            s.pieces.get_mut(&id).unwrap().door = d;
        }
        // ---- sounds at the door component (FUN_1404bee50)
        for (k, e) in events {
            let DoorEvent::Sound { opening } = e;
            let (def, at) = match k {
                Key::Map(i) => { let (t, q, sc, off) = map_root(i); (poi.as_ref().unwrap().doors[i].def.clone(), t + q * (off * sc)) }
                Key::Piece(id) => match s.pieces.get(&id).and_then(|pd| piece_root(pd, &id).map(|r| (pd.data.def.as_ref().clone(), r))) { Some((d, (t, q, sc, off))) => (d, t + q * (off * sc)), None => continue },
            };
            let path = if opening { def.opening_sound.as_deref() } else { def.closing_sound.as_deref() };
            match path.and_then(fn_audio_sink::door_cue) {
                Some(cue) => sounds.push(fn_audio_sink::SoundEvent::BuildingCue { cue, pos: Some(to_game(at)) }),
                None => if let Some(p) = path { s.log.push(format!("[doors] cue not in DOOR_CUES: {p}")); },
            }
        }
        // ---- the sim's door collision: the near leaves whose component collision is on
        let mut hulls = vec![];
        for &i in &near_map { let d = &s.map[i].door; if d.collision { let (t, q, sc, off) = map_root(i); hulls.extend(hulls_at(&s.map[i].local, leaf_l2w(t, q, sc, off, d.closed_yaw, d.yaw))); } }
        for id in &near_piece { let pd = &s.pieces[id]; if pd.door.collision { if let Some((t, q, sc, off)) = piece_root(pd, id) { hulls.extend(hulls_at(&pd.data.hulls, leaf_l2w(t, q, sc, off, pd.door.closed_yaw, pd.door.yaw))); } } }
        // ---- map door draws: resident + built groups, live walls
        s.draws.clear();
        if let Some(p) = &poi {
            for (i, m) in s.map.iter().enumerate() {
                let d = &p.doors[i];
                let shown = d.group != crate::poi::NO_CLUSTER && p.groups.get(d.group as usize).is_some_and(|g| g.resident.load(std::sync::atomic::Ordering::Relaxed) && g.ready.load(std::sync::atomic::Ordering::Relaxed));
                if !shown || !wall_alive(m) { continue; }
                let l2w = leaf_l2w(d.t, d.q, d.s, d.def.offset, m.door.closed_yaw, m.door.yaw);
                let scale = d.s.abs().max_element();
                s.draws.push(DoorDraw { mesh: d.def.mesh.clone(), material: d.def.animating_material.clone(), l2w, center: l2w.transform_point3(m.sphere.0), radius: m.sphere.1 * scale });
            }
        }
        for l in s.log.drain(..) { eprintln!("{l}"); }
        hulls
    });
    sim.door_hulls = hulls;
    sounds
}

/// The map door leaves to draw this frame (FX space), see [`frame`].
pub fn map_draws() -> Vec<DoorDraw> { if !enabled() { return vec![]; } with(|s| s.draws.clone()) }

/// A player piece's door (its class has a `DoorMesh`), for `building_render::draw_piece`.
pub fn piece_door(id: PieceId) -> Option<PieceDoorView> {
    if !enabled() { return None; }
    with(|s| s.pieces.get(&id).map(|p| PieceDoorView { def: p.data.def.clone(), closed_yaw: p.door.closed_yaw, yaw: p.door.yaw }))
}

/// The focused door's interaction prompt: (key text, `Open` / `Close`, world anchor in sim metres).
pub fn prompt(key: &str) -> Option<(String, String, Vec3)> {
    if !enabled() { return None; }
    with(|s| {
        let k = s.focus?;
        let (text, at) = match k {
            Key::Map(i) => { let p = s.poi.as_ref()?; let d = &p.doors[i]; let door = &s.map[i].door; (door.prompt(), leaf_center(&d.pieces, leaf_l2w(d.t, d.q, d.s, d.def.offset, door.closed_yaw, door.yaw))) }
            Key::Piece(id) => {
                let pd = s.pieces.get(&id)?;
                let (loc, yaw, mir) = crate::building_hook::with_world(|w| w.get(id).map(|p| (crate::building_hook::world_to_fx(p.location), p.yaw_deg, p.edit.mirrored)))?;
                (pd.door.prompt(), leaf_center(&pd.data.pieces, leaf_l2w(loc, Quat::from_rotation_z(yaw.to_radians()), Vec3::new(if mir { -1. } else { 1. }, 1., 1.), pd.data.def.offset, pd.door.closed_yaw, pd.door.yaw)))
            }
        };
        Some((key.to_owned(), text.to_owned(), to_game(at)))
    })
}
/// CHOSEN prompt anchor: the centre of the leaf's convexes' bounds (FX space).
fn leaf_center(pieces: &[Vec<Vec3>], l2w: Mat4) -> Vec3 {
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in pieces.iter().flatten() { let w = l2w.transform_point3(*p); lo = lo.min(w); hi = hi.max(w); }
    if lo.x > hi.x { l2w.w_axis.truncate() } else { (lo + hi) * 0.5 }
}

/// Run report line.
pub fn report() -> String {
    with(|s| {
        let open_map = s.map.iter().filter(|m| m.door.open).count();
        let open_piece = s.pieces.values().filter(|p| p.door.open).count();
        if s.map.is_empty() && s.pieces.is_empty() { return String::new(); }
        format!("doors: {} map doors ({} open, {} drawn last frame), {} player doors ({} open)\n", s.map.len(), open_map, s.draws.len(), s.pieces.len(), open_piece)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_leaf_turns_about_its_pivot_and_keeps_the_root_scale() {
        // a wall at (1000, 0, 0) facing +X, door pivot at DoorOffset (-64, 0, 0)
        let (t, q, s) = (Vec3::new(1000., 0., 0.), Quat::IDENTITY, Vec3::ONE);
        let closed = leaf_l2w(t, q, s, Vec3::new(-64., 0., 0.), 0., 0.);
        assert!((closed.transform_point3(Vec3::ZERO) - Vec3::new(936., 0., 0.)).length() < 1e-3);
        assert!((closed.transform_point3(Vec3::new(128., 0., 0.)) - Vec3::new(1064., 0., 0.)).length() < 1e-3);
        let open = leaf_l2w(t, q, s, Vec3::new(-64., 0., 0.), 0., 90.);
        assert!((open.transform_point3(Vec3::ZERO) - Vec3::new(936., 0., 0.)).length() < 1e-3, "the pivot stays");
        assert!((open.transform_point3(Vec3::new(128., 0., 0.)) - Vec3::new(936., 128., 0.)).length() < 1e-3, "+90 yaw turns +X to +Y");
        // a mirrored piece (scale x -1) yawed 90: the offset is mirrored then rotated, the leaf keeps the negative scale
        let m = leaf_l2w(Vec3::ZERO, Quat::from_rotation_z(90f32.to_radians()), Vec3::new(-1., 1., 1.), Vec3::new(-72., 0., 0.), 90., 90.);
        assert!((m.transform_point3(Vec3::ZERO) - Vec3::new(0., 72., 0.)).length() < 1e-3);
        assert!(Mat3::from_mat4(m).determinant() < 0.);
    }
    #[test] fn yaw_of_reads_the_ue_yaw() {
        assert!((yaw_of(Quat::from_rotation_z(30f32.to_radians())) - 30.).abs() < 1e-3);
        // the Salty Rural_House_Wall_1 door: q = (0, 0, -1, ~0) -> yaw 180 / -180
        assert!((yaw_of(Quat::from_xyzw(0., 0., -1., 9.5e-7)).abs() - 180.).abs() < 1e-3);
    }
    #[test] fn a_capsule_inside_a_leaf_overlaps_and_one_beside_it_does_not() {
        // a 4 cm thick, 128 x 220 cm leaf box at the origin, UE cm
        let pts: Vec<Vec3> = [0., 128.].iter().flat_map(|&x| [-2., 2.].iter().flat_map(move |&y| [0., 220.].iter().map(move |&z| Vec3::new(x, y, z)))).collect();
        let h = hulls_at(&local_hulls(&[pts]), Mat4::IDENTITY);
        assert_eq!(h.len(), 1);
        assert!(capsule_overlaps(&h, to_game(Vec3::new(64., 30., 100.))), "30 cm from the leaf: inside the 42 cm radius");
        assert!(!capsule_overlaps(&h, to_game(Vec3::new(64., 60., 100.))), "60 cm away");
    }
}

fn local_sphere(pieces: &[Vec<Vec3>]) -> (Vec3, f32) {
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in pieces.iter().flatten() { lo = lo.min(*p); hi = hi.max(*p); }
    if lo.x > hi.x { return (Vec3::ZERO, 300.); }
    ((lo + hi) * 0.5, (hi - lo).length() * 0.5 * 1.25)
}

pub fn leaf_visible(view: &fn_fx::view::ViewParams, center_fx: Vec3, radius_cm: f32) -> bool {
    let c = to_game(center_fx);
    let r = radius_cm * 0.01;
    let d = c - view.eye;
    let f = view.forward.normalize_or_zero();
    let z = d.dot(f);
    if z < -r { return false; }
    let right = f.cross(view.up).normalize_or_zero();
    let up = right.cross(f);
    if d.dot(right).abs() > z.max(0.) * view.tan_half.0 + r * 1.5 { return false; }
    if d.dot(up).abs() > z.max(0.) * view.tan_half.1 + r * 1.5 { return false; }
    fn_render::occlusion::latest().map_or(true, |s| !s.sphere_occluded(c, r, view.eye))
}
