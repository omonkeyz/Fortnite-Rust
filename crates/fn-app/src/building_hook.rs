//! BUILDING HOOK (fn-app, owner: building core agent): the app's one `fn_building::world::BuildingWorld`, the build preview, placing,
//! the build-up tick, resources, and the placed pieces' collision with the player. Logic: `fn_building::{grid, pieces, placement, world}`.
//!
//! Wiring (one call per frame): [`frame`] after the simulation step, with the build input (`crate::build_input::BuildInput`): it
//! loads the piece catalog from the paks in the background (once), applies the input's intents (enter / exit build mode, select piece,
//! material, `Place`, `Rotate`), updates the preview from the camera, places (spending `sim.world.wood / stone / metal`), ticks the
//! build-up, and hands the pieces' collision hulls to the movement (`GameSim::building_hulls`, read by `refresh_blockers`).
//!
//! Other building agents:
//! * [`with_world`] - the world (visuals read, editing / damage mutate). [`preview`] - the preview marker for the renderer.
//! * [`world_to_fx`] / [`fx_to_world`] - the world is in `fn_movement::scene` space; the FX pass is that + (0, `Z_OFFSET` * 100, 0).
//! * Input functions for scripted / other front-ends: [`enter_build_mode`], [`exit_build_mode`], [`select_piece`], [`set_material`],
//!   [`request_place`], [`rotate_preview`] (the same requests the `BuildIntent`s make).
//!
//! Grid: anchored on the Athena level origin. The port maps the level onto the baseplate with the props / storm mapping
//! (`crate::storm_hook::offset_cm`: level p -> FX p + offset), so the grid origin in world space is `offset - (0, Z_OFFSET * 100, 0)`;
//! without the real props the offset is 0.
//!
//! PLACEHOLDER VISUALS: until the visuals agent's renderer (`crate::building_render`) draws the pieces, [`placeholder_vertices`] gives
//! flat-shaded triangles of the pieces' COOKED COLLISION convexes (not their meshes) for the procedural prop pass, tinted per material,
//! plus the preview (blue = can place, red = refused). They are turned off as soon as `building_render::is_ready()`.
//!
//! Verification (`FNRT_BUILD_SCRIPT`): `kind[:material][@seconds]` entries separated by `;` (kind = wall / floor / stairs / roof, material =
//! wood / stone / metal) place pieces from the live preview at those run times, so headless `--script` runs can exercise the whole path.
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};

use fn_building::grid::Rot;
use fn_building::pieces::{Material, PieceCatalog, PieceKind};
use fn_building::placement::{self, BuildPreview, PlacementEnv, QueryResult, ViewInput};
use fn_building::world::{BuildingWorld, WorldEvent};
use fn_game::sim::Z_OFFSET;
use glam::Vec3;

/// `AFortPlayerController::EditModeDistance` (+0x1A50 = 2048, ctor @140810140): vfunc_685 @140827a80 ends the edit ray at
/// `GetPlayerViewPoint` (vtable slot 213, the camera) + view dir * this, so the reach is measured from the camera (cm).
pub const EDIT_MODE_DISTANCE: f32 = 2048.;

/// World (movement scene cm) -> FX space cm.
pub fn world_to_fx(v: Vec3) -> Vec3 { v + Vec3::new(0., Z_OFFSET * 100., 0.) }
pub fn fx_to_world(v: Vec3) -> Vec3 { v - Vec3::new(0., Z_OFFSET * 100., 0.) }
/// Sim metres (x, y up, z) -> world cm.
pub fn sim_to_world(v: Vec3) -> Vec3 { Vec3::new(v.x * 100., (v.z - Z_OFFSET) * 100., v.y * 100.) }
/// World cm -> sim metres.
pub fn world_to_sim(v: Vec3) -> Vec3 { Vec3::new(v.x / 100., v.z / 100., v.y / 100. + Z_OFFSET) }

/// The preview marker as the renderer needs it (world space).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewMarker { pub class_path: String, pub kind: PieceKind, pub material: Material, pub location: Vec3, pub yaw_deg: f32, pub mirrored: bool, pub valid: bool, pub result: QueryResult }

enum Request { Enter(PieceKind), Exit, Select(PieceKind), Material(Material), Place, /** a `FNRT_BUILD_SCRIPT` placement (script entry index) */ ScriptPlace(usize), Rotate }

struct State {
    world: BuildingWorld,
    catalog: Option<PieceCatalog>,
    set: Option<fn_pak::PakSet>,
    loading: Option<mpsc::Receiver<Result<(fn_pak::PakSet, PieceCatalog, Option<crate::building_edit_hook::EditHook>), String>>>,
    /// The editing session (editing agent), driven by `build_input::apply_edit_intents` on this same world.
    edit: Option<crate::building_edit_hook::EditHook>,
    load_tried: bool,
    preview: BuildPreview,
    building: bool,
    requests: Vec<Request>,
    event_cursor: u64,
    /// `world.revision()` the collision hulls were built for.
    hull_rev: u64,
    /// PREVIEW AFFORD (2026-10-08): `vfunc_677` (can afford the preview piece with the current resource) of this frame, see [`preview`].
    afford: bool,
    script: Vec<(PieceKind, Option<Material>, f32, bool)>,
    run_time: f32,
    log: Vec<String>,
    /// OUTLINE: the build preview's highlight of this frame (`HighlightColors` index, Valid, `world.props` indices).
    highlight: Option<(u8, bool, Vec<usize>)>,
    cues: Vec<&'static str>,
    focus: Option<fn_building::world::PieceId>,
    last_class: Option<String>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| State { world: BuildingWorld::new(), edit: None, catalog: None, set: None, loading: None, load_tried: false, preview: BuildPreview::default(), building: false,
        requests: vec![], event_cursor: 0, hull_rev: u64::MAX, afford: true, script: parse_script(), run_time: 0., log: vec![], highlight: None, cues: vec![], focus: None, last_class: None }))
}

/// Script entries to place this frame: every entry whose time has come and which is not done; untimed (t = 0) entries run one per
/// frame in script order (so each sees the world the previous one left); timed ones all fire when due, as before.
fn due_script_entries(script: &[(PieceKind, Option<Material>, f32, bool)], run_time: f32) -> Vec<usize> {
    let mut due: Vec<usize> = script.iter().enumerate().filter(|(_, e)| !e.3 && run_time >= e.2).map(|(i, _)| i).collect();
    if due.first().map_or(false, |&i| script[i].2 <= 0.) { due.truncate(1); }
    due
}

fn parse_script() -> Vec<(PieceKind, Option<Material>, f32, bool)> {
    let Ok(s) = std::env::var("FNRT_BUILD_SCRIPT") else { return vec![] };
    s.split(';').filter(|e| !e.trim().is_empty()).filter_map(|e| {
        let (spec, at) = e.split_once('@').map(|(a, b)| (a, b.trim().parse::<f32>().unwrap_or(0.))).unwrap_or((e, 0.));
        let mut it = spec.trim().split(':');
        let kind = match it.next()?.trim() { "wall" => PieceKind::Wall, "floor" => PieceKind::Floor, "stairs" | "ramp" => PieceKind::Stairs, "roof" | "cone" => PieceKind::Roof, _ => return None };
        let mat = match it.next().map(str::trim) { Some("wood") => Some(Material::Wood), Some("stone") => Some(Material::Stone), Some("metal") => Some(Material::Metal), _ => None };
        Some((kind, mat, at, false))
    }).collect()
}

/// Read / mutate the building world (visuals, editing, structural / damage).
pub fn with_world<R>(f: impl FnOnce(&mut BuildingWorld) -> R) -> R { with(|s| f(&mut s.world)) }
/// The piece catalog once loaded (class data, health, costs, collision).
pub fn with_catalog<R>(f: impl FnOnce(Option<&PieceCatalog>) -> R) -> R { with(|s| f(s.catalog.as_ref())) }
/// DOORS: the pak set the catalog was loaded from (`None` until the background load finished).
pub fn with_set<R>(f: impl FnOnce(Option<&fn_pak::PakSet>) -> R) -> R { with(|s| f(s.set.as_ref())) }
pub fn is_building() -> bool { with(|s| s.building) }

pub fn enter_build_mode(kind: PieceKind) { with(|s| s.requests.push(Request::Enter(kind))); }
pub fn exit_build_mode() { with(|s| s.requests.push(Request::Exit)); }
pub fn select_piece(kind: PieceKind) { with(|s| s.requests.push(Request::Select(kind))); }
pub fn set_material(m: Material) { with(|s| s.requests.push(Request::Material(m))); }
/// One placement attempt with the current preview (1.8 `Fire` pressed in build mode; no repeat while held).
pub fn request_place() { with(|s| s.requests.push(Request::Place)); }
/// `RotatePrimitiveClockwise`: `BuildPreviewRotationIterations` + 1.
pub fn rotate_preview() { with(|s| s.requests.push(Request::Rotate)); }

/// The edit session visuals (tiles, preview class) while editing.
pub fn edit_visuals() -> Option<fn_building::edit::EditVisuals> { with(|s| s.edit.as_ref().and_then(|e| e.visuals(&s.world))) }
/// Sound cues of the edit session since the last call.
pub fn drain_edit_sound_cues() -> Vec<&'static str> { with(|s| s.edit.as_mut().map(|e| e.drain_sound_cues()).unwrap_or_default()) }
pub fn drain_build_sound_cues() -> Vec<&'static str> { with(|s| std::mem::take(&mut s.cues)) }
pub fn preview_focus() -> Option<fn_building::world::PieceId> { with(|s| s.focus) }
/// SCRIPT BUILD KEYS: world-space (cm) centre of an edit tile / stair indicator of the running edit session (scripted aiming, `aimtile=` / `aimarrow=`).
pub fn edit_target_center(t: fn_building::edit::EditTarget) -> Option<Vec3> { with(|s| { let e = s.edit.as_ref()?; e.ctl.session.as_ref()?.target_center(&s.world, t) }) }

/// BUILDING HUD: a live piece as `GetFocusedBuildingInfo` reads it.
#[derive(Clone, Copy, Debug)]
pub struct PieceStatus { pub health: f32, pub max_health: f32, /** `GetCentroid`, world cm */ pub centroid: Vec3, /** `bUnderConstruction` */ pub under_construction: bool }
/// BUILDING HUD: [`PieceStatus`] of a live piece.
pub fn piece_status(id: fn_building::world::PieceId) -> Option<PieceStatus> {
    with(|s| s.world.get(id).filter(|p| p.health > 0.).map(|p| PieceStatus { health: p.health, max_health: p.max_health, centroid: fn_building::grid::centroid(p.kind, p.location, p.yaw_deg), under_construction: p.is_building() }))
}
/// BUILDING HUD: the piece of the running edit session (the edit tool's building, `AFortPlayerController` +0x1790).
pub fn editing_piece() -> Option<fn_building::world::PieceId> { with(|s| s.edit.as_ref().filter(|e| e.is_editing()).and_then(|e| e.ctl.session.as_ref().map(|x| x.piece))) }
/// BUILDING HUD: the preview marker in build mode: (`GetMaxHealth` = the buildable's MaxHealth, `GetCentroid` world cm).
pub fn preview_status() -> Option<(f32, Vec3)> {
    with(|s| {
        if !s.building || !s.preview.active { return None; }
        let def = s.catalog.as_ref()?.def(s.preview.kind, s.preview.material)?;
        Some((def.max_health, fn_building::grid::centroid(s.preview.kind, s.preview.loc, s.preview.rot.yaw)))
    })
}

/// PREVIEW AFFORD (2026-10-08): the preview's colour rule of the 1.8 preview update FUN_140835750 -> FUN_140831f80: the preview MID's `Code_Color` takes the
/// MID's `code_Blue` only when `!vfunc_679 && vfunc_677 && query == CanAdd`, else `code_Red` (DAT_14521f440 / DAT_14521f438); `vfunc_677` (slot 0x1528,
/// @140815aa0) = can afford the piece with the current resource, the same test placing refuses with ("Not enough {0} to build!"). So an unaffordable piece
/// shows the red preview wherever it would fit. NOT PORTED: `vfunc_679` (slot 0x1538, its meaning was not looked up; it never blocks in the sandbox).
/// `FN_BUILD_AFFORD_RED=0`: the old rule (only the grid query colours the preview).
pub fn preview_valid(result: QueryResult, afford: bool) -> bool {
    let afford_rule = fn_live::var("FN_BUILD_AFFORD_RED").map_or(true, |v| v != "0");
    result == QueryResult::CanAdd && (afford || !afford_rule)
}

/// The preview marker (None outside build mode or before the catalog is loaded).
pub fn preview() -> Option<PreviewMarker> {
    with(|s| {
        if !s.building || !s.preview.active || matches!(s.preview.result, QueryResult::ExistingActor(_)) { return None; }
        let def = s.catalog.as_ref()?.def(s.preview.kind, s.preview.material)?;
        Some(PreviewMarker { class_path: def.class.class_path.clone(), kind: s.preview.kind, material: s.preview.material, location: s.preview.loc, yaw_deg: s.preview.rot.yaw, mirrored: s.preview.mirrored, valid: preview_valid(s.preview.result, s.afford), result: s.preview.result })
    })
}

/// Props within this horizontal distance (m) of the pawn are handed to the placement sweep: the preview stays within
/// `BuildingPlacementDistance` + one cell of the pawn, so farther props can never touch a sweep box.
const PLACEMENT_PROP_RANGE_M: f32 = 30.;

const STATIC_MESH_RANGE_M: f32 = 60.;
const STATIC_MESH_REBUILD_M: f32 = 15.;

fn static_meshes(sim: &fn_game::GameSim) -> std::sync::Arc<Vec<fn_hull::Hull>> {
    static CACHE: Mutex<Option<((usize, usize), Vec3, std::sync::Arc<Vec<fn_hull::Hull>>)>> = Mutex::new(None);
    let key = (sim.world.props.len(), sim.world.props.as_ptr() as usize);
    let pawn = Vec3::new(sim.x, 0., sim.z);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, at, v)) = c.as_ref() { if *k == key && (*at - pawn).length() <= STATIC_MESH_REBUILD_M { return v.clone(); } }
    let v: Vec<fn_hull::Hull> = sim.world.props.iter().filter(|p| p.build_flags & placement::PROP_STATIC_MESH != 0).filter_map(|p| p.hull.as_ref()).filter(|s| Vec3::new(s.center.x - pawn.x, 0., s.center.z - pawn.z).length() - s.radius <= STATIC_MESH_RANGE_M).flat_map(|set| set.hulls.iter().map(|h| h.mapped(sim_to_world, true))).collect();
    let v = std::sync::Arc::new(v);
    *c = Some((key, pawn, v.clone()));
    v
}

static POI_PIECES: Mutex<Option<std::sync::Arc<Vec<placement::PoiPiece>>>> = Mutex::new(None);

fn env(sim: &fn_game::GameSim, with_props: bool) -> PlacementEnv {
    let o = crate::storm_hook::offset_cm();
    // the ground the port collides with: TERRAIN: the landscape in world cm (the movement scene's offset, `Local::new(_, Z_OFFSET)`), the baseplate (movement
    // scene Z = 0) where none is installed / under its holes; the level origin in world space
    let terrain = sim.terrain().map(|w| fn_terrain::Local::new(w, Z_OFFSET));
    let mut env = PlacementEnv { ground_z: Some(0.), terrain, grid_origin: fx_to_world(Vec3::from(o)), ..Default::default() };
    // structural grid (FUN_1404f2160): lower edge Z -3840 below the level origin; the X / Y size and the height (>= 20 levels) come
    // from the Athena world manager, which the port does not have, so only the lower edge bounds the query
    env.grid_bounds = Some((Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, placement::STRUCTURAL_GRID_MIN_Z), Vec3::splat(f32::INFINITY)));
    // G2: the live props in the placement sweep (BuildingProp / BuildingContainer): real props by their cooked convexes and their class's
    // +0x564 flags (bushes, hedges, small rocks ... are destroyed by a placement instead of blocking it), sandbox placeholders by their
    // trunk / rock frusta with the native defaults (canopies are leaves, not solid, see fn-props), in world cm. Id = index in `world.props`.
    if with_props && !fn_game::props::disabled() {
        let pawn = Vec3::new(sim.x, 0., sim.z);
        let near = |c: Vec3, r: f32| Vec3::new(c.x - pawn.x, 0., c.z - pawn.z).length() - r <= PLACEMENT_PROP_RANGE_M;
        let mut props = vec![];
        let mut poi_pieces = vec![];
        for (i, p) in sim.world.props.iter().enumerate().filter(|(_, p)| p.alive()) {
            let mut hulls = vec![];
            if let Some(set) = &p.hull {
                if !near(set.center, set.radius) { continue; }
                hulls.extend(set.hulls.iter().map(|h| h.mapped(sim_to_world, true)));
            } else {
                for s in fn_game::props::solids(p).into_iter().filter(|s| s.player_blocks()) {
                    if !near(s.base, s.max_radius()) { continue; }
                    // a 16-gon prism per frustum, circumscribed (radius / cos(pi / 16)) so it contains the solid
                    let k = 1. / (std::f32::consts::PI / 16.).cos();
                    let pts: Vec<Vec3> = (0..16).flat_map(|i| { let a = i as f32 * std::f32::consts::TAU / 16.; let (sa, ca) = a.sin_cos();
                        [s.base + Vec3::new(ca * s.r0 * k, s.h0, sa * s.r0 * k), s.base + Vec3::new(ca * s.r1 * k, s.h1, sa * s.r1 * k)] }).collect();
                    if let Some(h) = fn_hull::Hull::from_points(&pts) { hulls.push(h.mapped(sim_to_world, true)); }
                }
            }
            if hulls.is_empty() || p.build_flags & placement::PROP_NOT_IN_SWEEP != 0 { continue; }
            if p.build_flags & placement::PROP_STATIC_MESH != 0 { continue; }
            if let Some((k, yaw)) = crate::poi::building_piece(p.proto) { poi_pieces.push(placement::PoiPiece::new(i as u64, k, sim_to_world(p.pos), yaw, env.grid_origin, hulls.clone())); }
            let f = p.build_flags;
            let flags = placement::BlockFlags { do_not_block_buildings: f & 1 != 0, destroy_on_player_building_placement: f & 2 != 0, use_centroid: f & 4 != 0 };
            props.push(placement::PropCollider { id: i as u64, hulls, flags, centroid: sim_to_world(p.centroid) });
        }
        env.props = std::sync::Arc::new(props);
        env.poi_pieces = std::sync::Arc::new(poi_pieces);
        *POI_PIECES.lock().unwrap_or_else(|e| e.into_inner()) = Some(env.poi_pieces.clone());
    }
    if !with_props { if let Some(p) = POI_PIECES.lock().unwrap_or_else(|e| e.into_inner()).clone() { env.poi_pieces = p; } }
    if !fn_game::props::disabled() { env.static_meshes = static_meshes(sim); }
    env
}

/// `ServerCreateBuildingActor` @140845d70: after a successful placement, every prop of the sweep with `bDestroyOnPlayerBuildingPlacement`
/// dies (ABuildingActor vfunc_243 @140468b50: the normal death, so its `DeathParticles` / `DeathSound` play; no resources are given).
fn destroy_for_placement(sim: &mut fn_game::GameSim, ids: &[u64]) -> usize {
    let mut n = 0;
    for &id in ids {
        let i = id as usize;
        let Some(p) = sim.world.props.get_mut(i) else { continue };
        if !p.alive() { continue; }
        p.health = 0.; p.gone_for = 0.;
        let object = fn_combat::harvest_fx::object_id(i, p);
        let (pos, kind, resource, proto) = (p.pos, p.kind, p.res, p.proto);
        sim.world.fx.events.push(fn_combat::harvest_fx::FxEvent::Broke { object, pos, kind, resource, proto });
        n += 1;
    }
    n
}

fn resources(sim: &fn_game::GameSim, m: Material) -> u32 { match m { Material::Wood => sim.world.wood, Material::Stone => sim.world.stone, Material::Metal => sim.world.metal } }
fn spend(sim: &mut fn_game::GameSim, m: Material, n: u32) { let r = match m { Material::Wood => &mut sim.world.wood, Material::Stone => &mut sim.world.stone, Material::Metal => &mut sim.world.metal }; *r = r.saturating_sub(n); }

/// Camera / pawn of the sim in world space (UE rotator from the view direction).
fn view_input(sim: &fn_game::GameSim) -> ViewInput {
    let v = &sim.cam_view;
    let d = Vec3::new(v.forward.x, v.forward.z, v.forward.y).normalize_or_zero();
    let view = Rot { pitch: d.z.clamp(-1., 1.).asin().to_degrees(), yaw: d.y.atan2(d.x).to_degrees(), roll: 0. };
    let pawn = sim_to_world(sim.feet()) + Vec3::new(0., 0., sim.controller.capsule_half_height_cm());
    ViewInput { camera: sim_to_world(v.eye), view, pawn }
}

/// One frame (see the module docs). `install` = the paks / install directory.
pub fn frame(sim: &mut fn_game::GameSim, input: &mut crate::build_input::BuildInput, install: Option<PathBuf>, dt: f32) {
    // intents of this frame from the build input layer
    for i in input.last_intents() {
        use crate::build_input::BuildIntent as I;
        match *i {
            I::EnterBuildMode(k) => enter_build_mode(k),
            I::ExitBuildMode => exit_build_mode(),
            I::SelectPiece(k) => select_piece(k),
            I::MaterialChanged(m) => set_material(m),
            I::Place => request_place(),
            I::Rotate => rotate_preview(),
            _ => {}
        }
    }
    let mut unable: Option<Material> = None;
    let mut placed = false;
    with(|s| {
        // background catalog load (99 Blueprints + tables, ~1 s)
        if !s.load_tried {
            s.load_tried = true;
            if let Some(dir) = install {
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || { let r = fn_pak::PakSet::open_install(&dir).map_err(|e| e.to_string()).and_then(|set| { let c = PieceCatalog::load(&set)?; let e = crate::building_edit_hook::EditHook::from_paks(&set).map_err(|e| eprintln!("[building] edit data unavailable: {e}")).ok(); Ok((set, c, e)) }); let _ = tx.send(r); });
                s.loading = Some(rx);
            } else { eprintln!("[building] no game install: building disabled"); }
        }
        if let Some(rx) = &s.loading {
            if let Ok(r) = rx.try_recv() {
                s.loading = None;
                match r {
                    Ok((set, cat, edit)) => {
                        s.edit = edit;
                        for n in &cat.notes { s.log.push(n.clone()); }
                        for d in cat.defs() { eprintln!("[building] {:?} {:?}: {} health {} (start {:.0}) build {} s cost {}", d.kind, d.material, d.class.class_path.rsplit('.').next().unwrap_or(""), d.max_health, d.start_health(), d.build_time, d.cost); }
                        s.catalog = Some(cat); s.set = Some(set);
                    }
                    Err(e) => eprintln!("[building] piece catalog unavailable: {e}"),
                }
            }
        }
        s.run_time += dt;
        if s.cues.len() > 64 { let n = s.cues.len() - 64; s.cues.drain(..n); }
        // scripted placements (verification)
        let due = due_script_entries(&s.script, s.run_time);
        for i in &due {
            let (k, m, _, _) = s.script[*i];
            if s.catalog.is_none() { break; }
            s.script[*i].3 = true;
            s.requests.push(Request::Enter(k));
            if let Some(m) = m { s.requests.push(Request::Material(m)); }
            s.requests.push(Request::ScriptPlace(*i));
        }
        let material_before = s.preview.material;
        if input.trap_held() { s.building = false; s.preview.active = false; }
        else if input.is_building() { s.building = true; s.preview.material = input.material(); if let Some(k) = input.piece() { s.preview.set_piece(k); } }
        let Some(cat) = s.catalog.as_ref() else { s.requests.clear(); return };
        let env = env(sim, s.building || !s.requests.is_empty()); // props only matter for a preview / placement this frame
        let reqs = std::mem::take(&mut s.requests);
        let vi = view_input(sim);
        for r in reqs {
            match r {
                Request::Enter(k) => { s.building = true; s.preview.set_piece(k); }
                Request::Exit => { s.building = false; s.preview.active = false; }
                Request::Select(k) => s.preview.set_piece(k),
                Request::Material(m) => { if s.building && m != material_before { s.cues.push(fn_building::edit::data::sounds::SELECT[m.index()].1); } s.preview.material = m; }
                Request::Rotate => { s.preview.rotate(true); if s.building { s.cues.push(fn_building::edit::data::sounds::ROTATE.1); } }
                r @ (Request::Place | Request::ScriptPlace(_)) => {
                    let scripted = if let Request::ScriptPlace(i) = r { Some(i) } else { None };
                    if !s.building { continue; }
                    s.preview.prepare_place();
                    s.preview.update(&s.world, cat, &env, &vi);
                    if s.preview.snapped { s.cues.push(fn_building::edit::data::sounds::GRID_SNAP.1); }
                    let have = resources(sim, s.preview.material);
                    match placement::place(&mut s.world, cat, &env, &s.preview, have) {
                        Ok((id, cost)) => {
                            spend(sim, s.preview.material, cost); placed = true;
                            let gone = destroy_for_placement(sim, &placement::placement_destroys(&env, s.preview.kind, s.preview.loc, s.preview.rot.yaw));
                            s.log.push(format!("placed #{id} {:?} {:?} at {:?} yaw {}{}", s.preview.kind, s.preview.material, s.preview.loc, s.preview.rot.yaw, if gone > 0 { format!(" (destroyed {gone} prop(s) in the way)") } else { String::new() }));
                        }
                        Err(QueryResult::InvalidActor) if cat.def(s.preview.kind, s.preview.material).map_or(false, |d| have < d.cost && placement::query(&s.world, cat, d, s.preview.loc, s.preview.rot.yaw, s.preview.mirrored, &env) == QueryResult::CanAdd) => {
                            unable = Some(s.preview.material);
                            s.cues.push(fn_building::edit::data::sounds::UNABLE_TO_AFFORD.1);
                            // a scripted entry that cannot be afforded yet (resources arrive later, e.g. `mats=`) is retried next frame, not dropped
                            if let Some(i) = scripted {
                                s.script[i].3 = false;
                                let msg = format!("script place waiting: cannot afford {:?} {:?} (have {have})", s.preview.kind, s.preview.material);
                                if s.log.last() != Some(&msg) { s.log.push(msg); } // logged once per wait, never silent
                            }
                        }
                        Err(r) => { s.cues.push(fn_building::edit::data::sounds::UNABLE_TO_PLACE.1); let msg = format!("place refused: {r:?} ({:?} at {:?})", s.preview.kind, s.preview.loc); if fn_live::var("FN_BUILD_SOUND_LOG").map_or(false, |v| v == "1") { let (blocking, _) = placement::sweep_listing(&env, s.preview.kind, s.preview.loc, s.preview.rot.yaw); eprintln!("[build-place] {msg} blocking {:?}", blocking.iter().filter_map(|&i| sim.world.props.get(i as usize).map(|p| p.proto)).collect::<Vec<_>>()); } if s.log.last() != Some(&msg) && s.log.len() < 4096 { s.log.push(msg); } }
                    }
                }
            }
        }
        let class = if s.building { cat.def(s.preview.kind, s.preview.material).map(|d| d.class.class_path.clone()) } else { None };
        if class.is_some() && !s.edit.as_ref().map_or(false, |e| e.is_editing()) && class != s.last_class { s.cues.push(fn_building::edit::data::sounds::CHANGE.1); }
        if !s.edit.as_ref().map_or(false, |e| e.is_editing()) { s.last_class = class; }
        // editing (PerformBuildingEditInteraction / tile select / reset / confirm / cancel), on this world; target = the piece under the crosshair
        if let Some(edit) = s.edit.as_mut() {
            let hulls: Vec<(fn_building::world::PieceId, fn_hull::Hull)> = s.world.iter().flat_map(|p| placement::piece_hulls(cat, p).into_iter().map(move |h| (p.id, h))).collect();
            // G4: the edit ray of AFortPlayerController vfunc_685 @140827a80 / "EditModeTrace" (FUN_14084f8a0): from the pawn projected
            // onto the view ray (Cam + Dir * dot(Pawn - Cam, Dir)) to Cam + Dir * EditModeDistance (reach 2048 from the CAMERA), so pieces
            // between the camera and the pawn are never hit; start-penetrating hits are skipped (object multi trace).
            let dir = vi.view.vector();
            let along = (vi.pawn - vi.camera).dot(dir);
            let start = vi.camera + dir * along;
            let reach = EDIT_MODE_DISTANCE - along;
            let target = if let Some(f) = s.focus.filter(|_| s.building && !edit.is_editing()) { Some(f) } else if reach <= 0. { None } else {
                hulls.iter().filter(|(_, h)| h.sdf(start) >= 0.).filter_map(|(id, h)| h.ray(start, dir, reach).map(|(t, _)| (t, *id))).min_by(|a, b| a.0.total_cmp(&b.0)).map(|x| x.1)
            };
            let intents = input.last_intents().to_vec();
            // run log of the edit intents: session (piece, matched state valid, matched class) before / after, the piece's class afterwards
            let short = |c: &str| c.rsplit('.').next().unwrap_or("").to_owned();
            let snap = |e: &crate::building_edit_hook::EditHook| e.ctl.session.as_ref().map(|x| (x.piece, x.state_valid, short(&x.state.class_path)));
            let pre = snap(edit);
            crate::build_input::apply_edit_intents(input, &intents, edit, &mut s.world, target, vi.pawn);
            {
                use crate::build_input::BuildIntent as I;
                let ev: Vec<&I> = intents.iter().filter(|i| matches!(i, I::EditBegin | I::EditSelectPressed | I::EditSelectReleased | I::EditReset | I::EditConfirm)).collect();
                if !ev.is_empty() {
                    let post = snap(edit);
                    let class = pre.as_ref().map(|p| p.0).or(target).and_then(|id| s.world.get(id)).map(|p| short(p.effective_class())).unwrap_or_default();
                    s.log.push(format!("t {:.3} edit {ev:?} target {target:?}: session before {pre:?} after {post:?}; piece class now {class}", s.run_time));
                }
            }
            // editing agent: dt drives the match flash / roof tile easing (vfunc_76); death cancels (FUN_14084f8a0) - GameSim has no pawn death state yet, so alive
            edit.tick_frame(&s.world, vi.pawn, false, vi.camera, dir, dt);
        }
        let editing = s.edit.as_ref().map_or(false, |e| e.is_editing());
        if s.building && !editing { s.preview.update(&s.world, cat, &env, &vi); if s.preview.snapped { s.cues.push(fn_building::edit::data::sounds::GRID_SNAP.1); } } else if editing { s.preview.active = false; }
        s.focus = match s.preview.result { QueryResult::ExistingActor(id) if s.building && !editing && s.preview.active && id != placement::POI_PIECE_ID => Some(id), _ => None };
        // OUTLINE: FUN_140835750 -> vfunc_672: Obstructed by building actors -> those, HighlightColors[0] Invalid; otherwise (not ExistingActor) the actors the
        // placement would destroy, HighlightColors[2] Valid; nothing else is highlighted
        s.highlight = if s.building && !editing && s.preview.active {
            let (blocking, destroyed) = placement::sweep_listing(&env, s.preview.kind, s.preview.loc, s.preview.rot.yaw);
            match s.preview.result {
                QueryResult::Obstructed => (!blocking.is_empty()).then(|| (0, false, blocking.iter().map(|&i| i as usize).collect())),
                QueryResult::ExistingActor(_) => None,
                _ => (!destroyed.is_empty()).then(|| (2, true, destroyed.iter().map(|&i| i as usize).collect())),
            }
        } else { None };
        s.world.tick(dt);
        // BUILDING DAMAGE HOOK: queued hits, structural collapse, snapshot of this same world (right after BuildingWorld::tick)
        crate::building_damage_hook::install();
        // STRUCTURAL FIX (2026-10-05): the structural system takes the class cubes (catalog), the grid origin and the ground (`env`)
        let _destroyed = crate::building_damage_hook::tick_with(&mut s.world, Some(cat), &env);
        // edit variants: make sure their class data (collision, cube) is loaded
        let (events, cur) = s.world.events_since(s.event_cursor);
        let edited: Vec<String> = events.iter().filter_map(|e| if let WorldEvent::Edited(id) = e { s.world.get(*id).map(|p| p.effective_class().to_owned()) } else { None }).collect();
        s.event_cursor = cur;
        if let (Some(set), Some(cat)) = (s.set.as_ref(), s.catalog.as_mut()) { for c in edited { cat.ensure_class(set, &c); } }
        // collision: the pieces' cooked convexes, world cm -> sim metres (axis swap = reflection: flip)
        // PREVIEW AFFORD: this frame's vfunc_677 for the preview (the same have >= cost test `placement::place` refuses with)
        s.afford = s.catalog.as_ref().and_then(|c| c.def(s.preview.kind, s.preview.material)).map_or(true, |d| resources(sim, s.preview.material) >= d.cost);
        if s.world.revision() != s.hull_rev {
            s.hull_rev = s.world.revision();
            let cat = s.catalog.as_ref().unwrap();
            sim.building_hulls = s.world.iter().flat_map(|p| placement::piece_hulls(cat, p)).map(|h| h.mapped(world_to_sim, true)).collect();
        }
    });
    if let Some(m) = unable { input.unable_to_afford(m); }
    if placed { crate::build_anim::on_piece_placed(); }
}

/// Run log lines (placements, refusals, catalog notes) for the run report.
pub fn run_report() -> String {
    with(|s| {
        if s.log.is_empty() && s.world.is_empty() { return String::new(); }
        let mut out = String::from("building core:\n");
        for l in &s.log { out.push_str("  "); out.push_str(l); out.push('\n'); }
        for p in s.world.iter() { out.push_str(&format!("  piece #{} {:?} {:?} at {:?} yaw {} health {:.0}/{:.0} building {}\n", p.id, p.kind, p.material, p.location, p.yaw_deg, p.health, p.max_health, p.is_building())); }
        out
    })
}

// ---- PLACEHOLDER visuals (until the visuals agent's renderer is ready) ----------------------------------------------------------
/// Triangles (sim metres, flat normals) of the pieces' collision convexes and of the preview, for the procedural prop pass.
/// `None` when nothing changed since `rev` (pass the returned revision back) or when the real renderer is active.
pub fn placeholder_vertices(rev: &mut u64) -> Option<Vec<fn_render::PropVertex>> {
    if crate::building_render::is_ready() { if *rev != 0 { *rev = 0; return Some(vec![]); } return None; }
    with(|s| {
        let cat = s.catalog.as_ref()?;
        let pv = if s.building && s.preview.active { Some((s.preview.kind, s.preview.material, s.preview.loc, s.preview.rot.yaw, s.preview.mirrored, s.preview.result == QueryResult::CanAdd)) } else { None };
        // revision: world revision mixed with the preview transform
        let mut h = s.world.revision().wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        if let Some((k, m, l, y, _, ok)) = pv { for v in [l.x, l.y, l.z, y, k as u8 as f32, m as u8 as f32, ok as u8 as f32] { h = (h ^ v.to_bits() as u64).wrapping_mul(0x100000001b3); } }
        if h == *rev { return None; }
        *rev = h;
        let mut out = Vec::new();
        let mut push = |hulls: Vec<fn_hull::Hull>, color: [f32; 3]| {
            for hull in hulls { for t in hull.tris.iter() {
                let p = t.map(|i| world_to_sim(hull.verts[i as usize]));
                // world -> sim is a reflection: re-wind so the normal points out
                let n = (p[2] - p[0]).cross(p[1] - p[0]).normalize_or_zero();
                for v in [p[0], p[2], p[1]] { out.push(fn_render::PropVertex { pos: v.to_array(), normal: n.to_array(), color }); }
            } }
        };
        for p in s.world.iter() {
            let base = match p.material { Material::Wood => [0.45, 0.30, 0.16], Material::Stone => [0.42, 0.42, 0.42], Material::Metal => [0.55, 0.58, 0.62] };
            let k = 0.5 + 0.5 * p.health_fraction();
            push(placement::piece_hulls(cat, p), base.map(|c| c * k));
        }
        if let Some((k, m, l, y, mir, ok)) = pv {
            if let Some(d) = cat.def(k, m) { push(placement::transform_hulls(&d.class.convexes, l, y, mir), if ok { [0.15, 0.35, 0.9] } else { [0.9, 0.15, 0.15] }); }
        }
        Some(out)
    })
}

static PLACEHOLDER: Mutex<(u64, Vec<fn_render::PropVertex>)> = Mutex::new((0, Vec::new()));
static TEMPLATES: std::sync::OnceLock<fn_render::props::PropTemplates> = std::sync::OnceLock::new();

/// PLACEHOLDER: true when the placeholder triangles changed since the last upload (scene.rs re-uploads the prop pass then).
pub fn placeholder_dirty() -> bool {
    let mut g = PLACEHOLDER.lock().unwrap_or_else(|e| e.into_inner());
    let mut rev = g.0;
    match placeholder_vertices(&mut rev) { Some(v) => { *g = (rev, v); true } None => false }
}
/// PLACEHOLDER: uploads the procedural props plus the building placeholder triangles (`Renderer::set_prop_instances` when there are none).
pub fn upload_props(r: &mut fn_render::Renderer, instances: &[fn_render::props::PropInstance]) {
    let g = PLACEHOLDER.lock().unwrap_or_else(|e| e.into_inner());
    if g.1.is_empty() { r.set_prop_instances(instances); return; }
    let mut v = TEMPLATES.get_or_init(fn_render::props::PropTemplates::default).build(instances);
    v.extend_from_slice(&g.1);
    r.set_props(&v);
}


#[cfg(test)]
mod script_camera_tests {
    use super::*;
    use fn_building::grid;

    /// SCRIPT CAMERA -> BUILD TARGETING: a script's `yaw=` / `pitch=` / `turn=` are `FrameInput::set_yaw` / `set_pitch` / `yaw_delta`, which
    /// `GameSim::begin_frame` (fn-game sim.rs) writes into `sim.yaw` / `sim.pitch`, the one control rotation the live mouse / pad deltas also write;
    /// `GameSim::step` -> `apply_view` composes `sim.cam_view` from it, and `view_input` (the preview's `ViewInput`, vfunc_688 @140834290 reads
    /// the player view point) reads `sim.cam_view`. This pins that chain: every scripted turn moves the view the preview traces from, and a live
    /// look delta reaching the same rotation gives a bit-identical `ViewInput`.
    #[test] fn scripted_camera_turns_drive_the_build_preview_view_exactly_like_live_look() {
        let s = crate::script::Script::parse("t", "0-30 yaw=0 pitch=-12\n30-60 yaw=90 pitch=-12\n60-90 yaw=180 pitch=-12\n90-120 yaw=270 pitch=-12\n120-150 turn=1\nend 150").unwrap();
        let (mut scripted, mut live) = (fn_game::GameSim::new(), fn_game::GameSim::new());
        scripted.world.props.clear(); live.world.props.clear();
        let mut cells = vec![];
        for f in 0..150u32 {
            let inp = s.input_at(f);
            scripted.step(&inp, 1. / 30.);
            // live: the mouse writes only deltas (InputState::gather -> yaw_delta / pitch_delta); aim them at the scripted rotation
            let (ty, tp) = (scripted.yaw, scripted.pitch);
            let l = fn_game::FrameInput { yaw_delta: grid::normalize_axis(ty - live.yaw), pitch_delta: tp - live.pitch, ..Default::default() };
            live.step(&l, 1. / 30.);
            let (a, b) = (view_input(&scripted), view_input(&live));
            assert!((a.camera - b.camera).length() < 1e-2 && (a.pawn - b.pawn).length() < 1e-3, "f{f}: scripted {a:?} live {b:?}");
            assert!(grid::normalize_axis(a.view.yaw - b.view.yaw).abs() < 1e-2 && (a.view.pitch - b.view.pitch).abs() < 1e-2, "f{f}: scripted {a:?} live {b:?}");
            if [29, 59, 89, 119, 149].contains(&f) {
                // the wall the preview would snap to: a point ahead along the view (inside BuildingPlacementDistance), vfunc_251 grid snap
                let p = a.pawn + Vec3::new(a.view.vector().x, a.view.vector().y, 0.).normalize() * 200.;
                cells.push((a.view.yaw, grid::snap_transform(PieceKind::Wall, p, a.view, 0)));
            }
        }
        // each scripted turn moved the view (yaw 0 / 90 / 180 / 270 / 300 in sim terms) and with it the snapped wall
        for i in 0..cells.len() { for j in i + 1..cells.len() {
            let ((ya, (la, ra)), (yb, (lb, rb))) = (cells[i], cells[j]);
            assert!(grid::normalize_axis(ya - yb).abs() > 20., "view yaw did not follow the script: {cells:?}");
            if j < 4 { assert!((la - lb).length() > 1. || grid::normalize_axis(ra.yaw - rb.yaw).abs() > 1., "same wall for different script yaws: {cells:?}"); } // the 4 quadrant yaws
        } }
    }
}

#[cfg(test)]
mod script_schedule_tests {
    use super::*;
    #[test] fn the_preview_is_blue_only_when_it_fits_and_is_affordable() {
        // FUN_140831f80: code_Blue iff CanAdd && vfunc_677 (afford); every other query result is red whatever the resources
        assert!(preview_valid(QueryResult::CanAdd, true));
        assert!(!preview_valid(QueryResult::CanAdd, false), "no materials: red");
        assert!(!preview_valid(QueryResult::Obstructed, true), "blocked by a tree / rock / house: red");
        assert!(!preview_valid(QueryResult::NoStructuralSupport, true));
        assert!(!preview_valid(QueryResult::Obstructed, false));
    }
    #[test] fn untimed_entries_run_one_per_frame_in_order_and_timed_ones_wait_for_their_time() {
        let mut sc = vec![(PieceKind::Wall, None, 0., false), (PieceKind::Stairs, None, 0., false), (PieceKind::Floor, None, 2., false)];
        assert_eq!(due_script_entries(&sc, 0.), vec![0]);
        sc[0].3 = true; assert_eq!(due_script_entries(&sc, 0.03), vec![1]);
        sc[1].3 = true; assert!(due_script_entries(&sc, 1.9).is_empty());
        assert_eq!(due_script_entries(&sc, 2.), vec![2]);
        sc[1].3 = false; // an entry that could not be afforded yet is un-marked and is retried first
        assert_eq!(due_script_entries(&sc, 2.), vec![1]);
    }
}

/// OUTLINE: the build preview's highlight of this frame: (`UFortGameData::HighlightColors` index, Valid side, indices into `sim.world.props`).
pub fn highlight() -> Option<(u8, bool, Vec<usize>)> { with(|s| s.highlight.clone()) }
pub fn highlight_piece() -> Option<(String, Vec3, f32, bool)> { with(|s| { let id = s.focus?; let p = s.world.get(id)?; let cls = s.catalog.as_ref()?.class(p.effective_class())?; Some((cls.static_mesh.clone(), world_to_fx(p.location), p.yaw_deg, p.edit.mirrored)) }) }
/// BUILDING HUD: the preview's `InteractionCost` (the buildable's resource cost), `BuildingMaterial` and `GetResourceCount(BuildingMaterial)`.
pub fn preview_cost(sim: &fn_game::GameSim) -> Option<(u32, Material, u32)> {
    with(|s| {
        if !s.building || !s.preview.active { return None; }
        let def = s.catalog.as_ref()?.def(s.preview.kind, s.preview.material)?;
        Some((def.cost, s.preview.material, resources(sim, s.preview.material)))
    })
}
