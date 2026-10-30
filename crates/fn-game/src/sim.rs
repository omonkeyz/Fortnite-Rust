//! `GameSim`: the renderer-free, OS-free game loop of the old `App::update` / `App::update_camera` / `App::step_anim` (main.rs), driven by
//! a per-frame [`FrameInput`] snapshot. The front-end (fn-app) fills `FrameInput` from keyboard/mouse/gamepad through the user's bindings
//! and `fn_settings::ModeLatch`es; the headless harness (fn-sim) fills it from scripts. Everything after the input snapshot is exactly the
//! old order of operations, so the same input stream gives the same numbers.
//!
//! Coordinates (old main.rs): the pawn eye is `(x, y, z)` in world metres with `y = EYE + feet height`, `z = 12 + controller.y`; the
//! controller works in "controller space" (position.y = world z - 12).
use fn_animgraph::{AnimState, Montage, Node, PawnSample};
use fn_camera::{self as camera, Camera, View};
use fn_combat::{Notify, Swing, World};
use fn_movement::{cmc::Tuning, smooth_toward, Controller};
use fn_settings::ModeLatch;
use crate::sim_tick::Snapshot;
use glam::{Vec2, Vec3};
use std::sync::Arc;

/// Half size (m) of the square baseplate.
pub const HALF: f32 = 256.0;
/// Eye height above the feet (m).
pub const EYE: f32 = 1.75;
/// World z of the controller origin (controller y = world z - 12).
pub const Z_OFFSET: f32 = 12.;
/// (x, eye y, z, yaw, pitch) of the spawn. TERRAIN: the eye stands EYE above the installed landscape (`fn_terrain::ground_y`, 0 on the flat plate).
pub fn spawn() -> (f32, f32, f32, f32, f32) { (0.0, EYE + fn_terrain::ground_y(0., 12.), 12.0, 0.0, -12.0) }
/// TERRAIN: how far (m) inside the landscape edge the pawn is kept. CHOSEN: 1 m (more than the 0.42 m capsule radius, so the capsule never overhangs the edge).
pub const EDGE_MARGIN: f32 = 1.;
/// ALL POIS: how far above the landscape [`GameSim::stand_on_floor`] looks down from (m). CHOSEN: 200 m, above every POI roof / terrace of the 1.8 map.
pub const SPAWN_PROBE_M: f32 = 200.;
/// ALL POIS (PERF): [`GameSim::camera_props`] radius (m). CHOSEN: 40 m, far more than the longest camera boom / feeler trace (a few metres).
pub const CAMERA_PROPS_M: f32 = 40.;
/// The xz box (game metres, min / max) the pawn is kept in: the landscape bounds minus [`EDGE_MARGIN`] when one is installed, else the old plate (+-HALF - 0.5).
pub fn world_box(terrain: Option<&fn_terrain::World>) -> (Vec2, Vec2) {
    match terrain { Some(w) => { let (a, b) = w.bounds(); (a + EDGE_MARGIN, b - EDGE_MARGIN) } None => (Vec2::splat(-HALF + 0.5), Vec2::splat(HALF - 0.5)) }
}

/// The coarse jump animation stage the old HUD/locomotion code used: 1 rising, 2 apex, 3 falling, 4 landing one-shot (0.8333 s), 0 none.
pub fn animation_stage(height: f32, vertical_speed: f32, previous: u8, elapsed: f32) -> u8 {
    if height > 0. { if vertical_speed > 1. { 1 } else if vertical_speed < -1. { 3 } else { 2 } }
    else if (1..=3).contains(&previous) || (previous == 4 && elapsed < 0.833333) { 4 } else { 0 }
}
/// World-space step for held keys: `forward`/`side` in -1..1, `yaw` in degrees; |result| = `speed * dt` for any diagonal.
pub fn horizontal_step(forward: f32, side: f32, yaw: f32, speed: f32, dt: f32) -> (f32, f32) {
    let length = (forward * forward + side * side).sqrt().max(1.0);
    let angle = yaw.to_radians();
    ((forward * angle.sin() + side * angle.cos()) / length * speed * dt,
     (-forward * angle.cos() + side * angle.sin()) / length * speed * dt)
}
/// Locomotion blend between two adjacent of the eight source directions: (from, to, fraction).
pub fn direction_blend(forward: f32, side: f32) -> (&'static str, &'static str, f32) {
    // Source sample angles in reports/blendspace-samples.json, including +/-180 wrap.
    let sector = (side.atan2(forward) / std::f32::consts::FRAC_PI_4).rem_euclid(8.);
    let lo = sector.floor() as usize; let names = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    (names[lo % 8], names[(lo + 1) % 8], sector.fract())
}
/// Nearest of the eight direction names (diagnostics).
pub fn direction_name(forward: f32, side: f32) -> &'static str { let sector = (side.atan2(forward) / std::f32::consts::FRAC_PI_4).round() as i32; ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][sector.rem_euclid(8) as usize] }

/// One frame of player intent, after key bindings / gamepad merging (the old `App::update` locals).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameInput {
    /// Movement axes, -1..1 (W/S, D/A; keyboard + gamepad already merged and clamped).
    pub forward: f32, pub side: f32,
    /// Raw key states (`bindings.down(A_JUMP/A_CROUCH/A_SPRINT/A_FIRE)` | gamepad). Crouch/sprint go through the hold/toggle latches.
    pub jump: bool, pub crouch_key: bool, pub sprint_key: bool, pub attack: bool,
    /// Scripted additions OR'ed after the latches (the old `ps.jump` / `ps.crouch` / `ps.run`); real input leaves them false.
    pub script_jump: bool, pub force_crouch: bool, pub force_run: bool,
    /// Degrees added to yaw / pitch this frame (arrow keys, right stick rate * dt, mouse counts * sensitivity).
    pub yaw_delta: f32, pub pitch_delta: f32,
    /// Absolute camera override applied first (scripted camera): yaw in degrees (any value, wrapped), pitch in degrees.
    pub set_yaw: Option<f32>, pub set_pitch: Option<f32>,
    /// Start the Melee_Equip montage (pickaxe slot selected).
    pub equip: bool,
    /// The selected hotbar slot changed this frame: an outgoing melee montage / hit / equip is dropped (Swing::default(), equip off, latched attack cleared) so it is never carried into the new item.
    pub selection_changed: bool,
    /// A non-melee item is selected (!HudState::melee_equipped()): the attack key is ignored (no swing, no hit). Default false = the pickaxe is equipped.
    pub non_melee: bool,
    /// R key: back to the spawn.
    pub reset: bool,
}

/// A source of per-frame input (keyboard/gamepad adapter in the app, script in the harness).
pub trait InputSource { fn poll(&mut self, frame: u32, dt: f32) -> FrameInput; }
/// Closure adapter: `ScriptInput(|frame| FrameInput { .. })`.
pub struct ScriptInput<F: FnMut(u32) -> FrameInput>(pub F);
impl<F: FnMut(u32) -> FrameInput> InputSource for ScriptInput<F> { fn poll(&mut self, frame: u32, _dt: f32) -> FrameInput { (self.0)(frame) } }

/// Everything the old `App` owned that is game state (no GL objects, no window, no HUD).
pub struct GameSim {
    pub swing: Swing, pub world: World, pub controller: Controller, pub cam: Camera, pub cam_view: View, pub anim: AnimState,
    pub x: f32, pub y: f32, pub z: f32, pub yaw: f32, pub pitch: f32,
    pub crouch_camera: f32, pub anim_time: f32, pub locomotion_blend: f32, pub jump_stage: u8, pub state_time: f32,
    pub crouch_latch: ModeLatch, pub sprint_latch: ModeLatch, pub was_moving: bool,
    pub melee_sprint: MeleeSprintGate,
    pub reloading: bool, pub sprint_cancels_reload: bool, pub sprint_cancelled_reload: bool,
    pub heal_anchor: Option<Vec3>, pub heal_prev: (bool, bool),
    /// Input.CrouchMode == Toggle / Input.ToggleSprint (settings).
    pub crouch_toggle: bool, pub sprint_toggle: bool,
    pub frame_dt: f32, pub frames: u32, pub frame_cam: Option<camera::CamState>,
    /// Movement experiment knobs (`FNRT_GRAVITY_SCALE` / `FNRT_JUMPZ`, see `fn_movement::cmc::Tuning`); applied to every controller this sim creates.
    pub tuning: Tuning,
    /// Montages driven from outside the pickaxe state (the front-end's weapon arms: equip / fire / reload / heal on the player's slots); appended to [`GameSim::montages`].
    pub arms: Vec<Montage>,
    /// ADS HOOK: hold / aim-down-sights camera modes, blend weight, pawn hiding and the targeting speed limit (see `ads_view`); default = the pickaxe camera.
    pub ads: crate::ads_view::AdsView,
    /// COSMETIC SWAP: the BR skin / pickaxe / back bling / glider selected at run time (see `cosmetic_swap`); the front-end rebuilds the pawn / pickaxe from [`crate::cosmetic_swap::Selection::take_pending`].
    pub cosmetics: crate::cosmetic_swap::Selection,
    /// F2 CONSOLE `speed <scale>`: multiplies the four recovered speed stats after the ADS speed limit is applied each tick (1.0 = recovered values).
    pub speed_scale: f32,
    /// STORM HOOK: the safe zone / storm the visuals draw (static for now, F2 `storm`); see `storm_state`.
    pub storm: crate::storm_state::StormState,
    /// BUILDING HOOK: collision convexes of the placed player pieces (sim metres, like the prop hulls), set by fn-app building_hook.rs; `refresh_blockers` adds the ones near the pawn.
    pub building_hulls: Vec<fn_combat::Hull>,
    /// DOORS (2026-10-08): the door components whose collision is on (`fn_building::door::Door::collision`), at their current swing, sim metres; set every
    /// frame by fn-app door_hook.rs (`FortBuildingMesh`: blocks the pawn and the camera like the pieces).
    pub door_hulls: Vec<fn_combat::Hull>,
    /// TERRAIN: the landscape this sim walks on; `None` = the process-wide one (`fn_terrain::world()`, installed by the loader), else the flat plate.
    /// Tests set it instead of installing the global (the unit tests of a binary run in parallel).
    pub terrain: Option<Arc<fn_terrain::World>>,
    /// F2 CONSOLE `fly` / `ghost` / `walk` (engine UCheatManager::Fly / Ghost / Walk): free flight along the view, jump = up, crouch = down, sprint = fast.
    pub fly: FlyMode,
    /// FALL DAMAGE: `FallingStartedZ` / landings of the pawn (fall_damage.rs); fn-app drains the landings and applies the damage to the pawn's health / shield.
    pub fall: crate::fall_damage::FallTracker,
    /// PERF (2026-10-08): xz grid over `world.props` for the per-tick collision queries (fn_props::index); rebuilt by [`GameSim::sync_prop_index`] when the
    /// props list changes, ignored while stale (the linear scans run). `FN_PROP_INDEX=0` = never built.
    pub prop_index: Option<fn_props::PropIndex>,
    /// PERF (2026-10-08): controller-space copies of the prop hulls the player query used last tick (fn_props::HullMapCache; off with `FN_PROP_INDEX=0`).
    pub hull_map_cache: fn_props::HullMapCache,
    pub drop: Option<crate::drop_in::DropIn>,
    pub bus_jump_prev: bool,
    pub dbno: bool,
    pub ext_block_app: Option<&'static str>,
}
/// Movement mode of the F2 `fly` / `ghost` / `walk` cheats. PORT-ONLY dev tool: `Fly` keeps the pawn above the landscape, `Ghost` collides with nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlyMode { #[default] Walk, Fly, Ghost }
/// CHOSEN (dev convenience, not 1.8): flight speed m/s, x3 while sprinting.
pub const FLY_SPEED: f32 = 15.;
impl Default for GameSim { fn default() -> Self { Self::new() } }
impl GameSim {
    /// The state `App` starts a run in (sandbox props, spawn pose).
    pub fn new() -> Self {
        let (x, y, z, yaw, pitch) = spawn();
        let mut g = GameSim { swing: Swing::default(), world: World::sandbox(), controller: Controller::default(), cam: Camera::default(), cam_view: View::default(), anim: AnimState::new(),
            x, y, z, yaw, pitch, crouch_camera: 0., anim_time: 0., locomotion_blend: 0., jump_stage: 0, state_time: 0.,
            crouch_latch: ModeLatch::default(), sprint_latch: ModeLatch::default(), was_moving: false, melee_sprint: MeleeSprintGate::default(), reloading: false, sprint_cancels_reload: false, sprint_cancelled_reload: false, heal_anchor: None, heal_prev: (false, false), crouch_toggle: false, sprint_toggle: false, frame_dt: 0., frames: 0, frame_cam: None, tuning: Tuning::default(), arms: vec![], ads: Default::default(), cosmetics: Default::default(), speed_scale: 1., storm: Default::default(), building_hulls: vec![], door_hulls: vec![], terrain: None, fly: FlyMode::Walk, fall: Default::default(), prop_index: None, hull_map_cache: Default::default(), drop: None, bus_jump_prev: false, dbno: false, ext_block_app: None };
        g.settle_anim(); // the pawn starts standing idle (old App::settle_anim, run once before the first frame of every path)
        g
    }
    /// Runs the graph for 1.5 s of standing idle at the current camera yaw / pitch, so a render-only path (menus, captures that draw without stepping) shows the SAME idle
    /// pose the game does. A never-stepped AnimState is a different pose (jog lean additive at alpha 1, aim offset applied twice, camera pitch ignored).
    pub fn settle_anim(&mut self) { for _ in 0..90 { self.step_anim(Vec2::ZERO, 1. / 60.); } }
    /// A sim whose pawn uses the gravity / jump-speed tuning `t` (the front-end passes `Tuning::from_env()`; default = the recovered values).
    pub fn with_tuning(t: Tuning) -> Self { let mut g = Self::new(); g.tuning = t; g.controller = Controller::with_tuning(&t); g }
    /// A fresh controller carrying this sim's tuning (reset / respawn).
    pub fn new_controller(&self) -> Controller { Controller::with_tuning(&self.tuning) }
    pub fn to_spawn(&mut self) { (self.x, self.y, self.z, self.yaw, self.pitch) = spawn(); self.y = EYE + self.ground_y(self.x, self.z); }
    /// TERRAIN: the landscape in use (this sim's own, else the installed one).
    pub fn terrain(&self) -> Option<Arc<fn_terrain::World>> { self.terrain.clone().or_else(fn_terrain::world) }
    /// TERRAIN: ground height (game metres) at game (x, z): the landscape, 0 off it / on the plate.
    pub fn ground_y(&self, x: f32, z: f32) -> f32 { match self.terrain() { Some(w) => w.ground_y(x, z).unwrap_or(if w.hole(x, z) { -1.0e4 } else { 0. }), None => 0. } }
    /// The highest surface at or below world point `p` (game metres, Y up): the props' real collision hulls (POI floors / roofs), placed building pieces,
    /// else the landscape. A straight down trace like `Scene::floor_below`, in world space.
    pub fn floor_below(&self, p: Vec3) -> f32 {
        let land = self.ground_y(p.x, p.z);
        let mut best = if land <= p.y + 0.5 { land } else { -1.0e4 };
        let w = Vec2::new(p.x, p.z);
        let mut hit = |h: &fn_combat::Hull| {
            if (Vec2::new(h.center.x, h.center.z) - w).length() > h.radius { return; }
            if let Some((t, _)) = h.ray(p, Vec3::NEG_Y, 1e4) { best = best.max(p.y - t); }
        };
        let mut prop = |pr: &fn_combat::Harvestable| {
            let Some(set) = &pr.hull else { return };
            if !pr.alive() || (Vec2::new(set.center.x, set.center.z) - w).length() > set.radius { return; }
            for h in &set.hulls { hit(h); }
        };
        // PERF (2026-10-08): the index's candidates (ascending, same test) instead of every prop
        match self.fresh_prop_index() {
            Some(ix) => { let mut ids = vec![]; ix.hull_candidates(w, 0., &mut ids); for i in ids { prop(&self.world.props[i as usize]); } }
            None => for pr in &self.world.props { prop(pr); },
        }
        for h in &self.building_hulls { hit(h); }
        for h in &self.door_hulls { hit(h); } // DOORS
        best
    }
    /// DOORS: the camera's blocking hulls: the pieces, plus the door leaves when there are any (borrowed when there are none: no copy).
    pub fn camera_hulls<'a>(building: &'a [fn_combat::Hull], doors: &'a [fn_combat::Hull]) -> std::borrow::Cow<'a, [fn_combat::Hull]> {
        if doors.is_empty() { return std::borrow::Cow::Borrowed(building); }
        std::borrow::Cow::Owned(building.iter().chain(doors.iter()).cloned().collect())
    }
    /// PERF (2026-10-08): rebuilds [`GameSim::prop_index`] when `world.props` is not the list it was built for (load, `--props`, scripted resets).
    /// Destroyed / regrown props need no rebuild: every query still tests `alive()`.
    pub fn sync_prop_index(&mut self) {
        if !fn_props::index::enabled() { self.prop_index = None; return; }
        if self.prop_index.as_ref().map_or(true, |i| !i.matches(&self.world.props)) {
            let t = std::time::Instant::now();
            self.prop_index = Some(fn_props::PropIndex::build(&self.world.props));
            if self.world.props.len() >= 1000 { eprintln!("[props] collision index: {} props in {:.1} ms", self.world.props.len(), t.elapsed().as_secs_f32() * 1000.); }
        }
    }
    /// The prop index when it describes the current props (`None` = use the linear scans).
    pub fn fresh_prop_index(&self) -> Option<&fn_props::PropIndex> { self.prop_index.as_ref().filter(|i| i.matches(&self.world.props)) }
    /// TERRAIN: [`world_box`] of this sim's ground.
    pub fn world_box(&self) -> (Vec2, Vec2) { world_box(self.terrain().as_deref()) }
    /// TERRAIN: hands the controller the current landscape (in controller space: z offset [`Z_OFFSET`]) when it changed (the loader installs it after the sim
    /// exists; a respawn makes a fresh controller); the pawn then stands on the new ground (a walking pawn, or one under it, is put on it).
    pub fn sync_terrain(&mut self) {
        let t = self.terrain();
        let same = match (&self.controller.terrain, &t) { (Some(a), Some(b)) => Arc::ptr_eq(&a.world, b), (None, None) => true, _ => false };
        if same { return; }
        self.controller.terrain = t.map(|w| fn_terrain::Local::new(w, Z_OFFSET));
        let p = self.controller.position;
        let g = self.ground_y(p.x, p.y + Z_OFFSET);
        if self.controller.grounded() || self.controller.height < g { self.controller.height = g; }
    }
    /// Feet position in world metres (x, ground-relative y, z).
    pub fn feet(&self) -> Vec3 { Vec3::new(self.x, self.y - EYE, self.z) }
    /// Hands the player controller the current trunk/rock blockers (world z = controller y + 12).
    pub fn refresh_blockers(&mut self) {
        self.sync_prop_index(); // PERF (2026-10-08): grid candidates instead of full scans; same lists in the same order
        let ix = self.prop_index.as_ref().filter(|i| i.matches(&self.world.props));
        self.controller.blockers = fn_props::player_blockers_in(&self.world.props, ix, Z_OFFSET);
        // real 1.8 props: their convex hulls within reach of the capsule (a prop is 1-9 hulls; the rest cannot be touched this tick)
        let cache = if fn_props::index::enabled() { Some(&mut self.hull_map_cache) } else { None };
        self.controller.hulls = fn_props::player_hulls_cached(&self.world.props, ix, Z_OFFSET, self.controller.position, 6., cache);
        // BUILDING HOOK: placed pieces near the pawn (same reach and mapping as the prop hulls above)
        let w = Vec2::new(self.controller.position.x, self.controller.position.y + Z_OFFSET);
        self.controller.hulls.extend(self.building_hulls.iter().filter(|h| (Vec2::new(h.center.x, h.center.z) - w).length() - h.radius <= 6.).map(|h| h.mapped(|v| Vec3::new(v.x, v.z - Z_OFFSET, v.y), true)));
        // DOORS: the door leaves near the pawn, same reach and mapping
        self.controller.hulls.extend(self.door_hulls.iter().filter(|h| (Vec2::new(h.center.x, h.center.z) - w).length() - h.radius <= 6.).map(|h| h.mapped(|v| Vec3::new(v.x, v.z - Z_OFFSET, v.y), true)));
    }
    /// Athena melee camera (fn-camera): pivot/offsets/boom/collision against the baseplate and the live harvestables.
    pub fn update_camera(&mut self, dt: f32) {
        let feet = self.feet();
        self.sync_prop_index();
        let near = self.camera_prop_ids();
        let hulls = Self::camera_hulls(&self.building_hulls, &self.door_hulls); // DOORS: the pieces + the door leaves
        let world = Self::camera_world(&self.world.props, &near, &hulls, self.terrain());
        let mode = self.drop_tick_mode().or(self.enhanced_camera().map(|c| c.0)).unwrap_or(self.ads.tick_mode());
        self.cam_view = self.cam.update(mode, camera::Input { feet, crouched: self.controller.crouched, yaw_deg: self.yaw, pitch_deg: self.pitch, dt }, world);
    }
    /// One animation-graph tick from the movement controller's state (the AnimBlueprint reads the pawn, never the keys): `input` is the
    /// movement input direction handed to `Controller::step` this frame.
    pub fn step_anim(&mut self, input: Vec2, dt: f32) {
        let aerial = self.aerial_anim(); self.anim.set_aerial(aerial);
        let c = &self.controller;
        let sample = PawnSample { velocity: (c.velocity.x, c.velocity.y), input: (input.x, input.y), facing_deg: self.yaw, camera_yaw_deg: self.yaw, camera_pitch_deg: self.pitch,
            height: c.air_height(), vertical_speed: c.vertical_speed, fall_time_left: c.fall_time_left(), crouched: c.crouched, running: c.running, melee_swinging: self.swing.stage > 0 };
        self.anim.step_sample(&sample, dt);
        self.step_enhanced_anim(dt);
        self.controller.facing = self.anim.visual_yaw(self.yaw); // the visible body yaw is the graph's (Actor Yaw Offset), on every path that ticks the graph
    }
    fn step_enhanced_anim(&mut self, dt: f32) {
        use fn_animgraph::enhanced_anim::{self as ea, EnhancedInput, MontageReq};
        use fn_movement::enhanced::TacPhase;
        if let Some(w) = self.controller.enhanced.as_mut().and_then(|e| e.hurdle.last_fail.take()) { eprintln!("[enhanced-anim] hurdle refused: {w}"); }
        let Some(e) = self.controller.enhanced.as_ref().filter(|_| ea::clips_ready()) else { let i = EnhancedInput::default(); self.anim.enh.step(&i, dt, &ea::clip_length); return };
        let c = self.controller.component();
        let v = &self.anim.vars;
        let mut montage_start = None;
        if e.hurdle.count != self.anim.enh.seen.0 {
            if let Some((_, t, _, _)) = e.hurdle.moving { montage_start = Some(MontageReq::Hurdle { montage: t.montage, far: t.far }); }
        }
        if e.clamber.count != self.anim.enh.seen.1 {
            if let Some(m) = e.clamber.moving { montage_start = Some(MontageReq::Clamber { target_distance_z: m.height }); }
        }
        self.anim.enh.seen = (e.hurdle.count, e.clamber.count);
        let jump_now = c.jumped != self.anim.enh.seen_jumps;
        self.anim.enh.seen_jumps = c.jumped;
        let (slope_pitch, slope_roll) = {
            let n = if c.floor.blocking_hit { c.floor.hit.normal } else { glam::Vec3::Z };
            let f = Vec3::new(c.facing.x, c.facing.y, 0.).normalize_or_zero();
            let r = Vec3::new(-f.y, f.x, 0.);
            (-n.dot(f).clamp(-1., 1.).asin().to_degrees(), n.dot(r).clamp(-1., 1.).asin().to_degrees())
        };
        let slide = c.slide.as_ref();
        let slope = slide.map_or(0., |_| { let n = if c.floor.blocking_hit { c.floor.hit.normal } else { glam::Vec3::Z }; let vel = Vec2::new(c.velocity.x, c.velocity.y).normalize_or_zero(); let up = n.z.clamp(-1., 1.).acos().to_degrees(); if Vec2::new(n.x, n.y).dot(vel) > 0. { up } else { -up } });
        let i = EnhancedInput {
            enabled: true,
            female: v.female,
            tactical: ea::fullbody_tactical(e.tac.phase != TacPhase::Off, self.controller.running, slide.is_some(), v.is_accelerating_2d, !c.is_falling(), c.velocity.z > 0. && c.is_falling(), c.is_falling()),
            in_air: c.is_falling(),
            jumped: c.velocity.z > 0.,
            sliding: slide.is_some(),
            slide_slope_angle: slope,
            slide_velocity_angle: v.local_velocity_yaw_angle.abs(),
            slide_velocity_yaw: v.local_velocity_yaw_angle,
            sprint_yaw: v.sprint_yaw_angle,
            montage_start,
            jump_now,
            yaw_rate: if dt > 1e-6 { v.yaw_delta_current_tick / dt } else { 0. },
            yaw_delta: v.yaw_delta_smoothed,
            speed2d: v.speed2d,
            lean_alpha: v.lean_additive_alpha,
            weapon: v.weapon_core_anim,
            slope_pitch,
            slope_roll,
            moving: v.is_moving_2d,
            lateral_accel: v.local_acceleration[1],
        };
        let before = self.anim.enh.state_key();
        self.anim.enh.step(&i, dt, &ea::clip_length);
        if self.anim.enh.state_key() != before { eprintln!("[enhanced-anim] {}", self.anim.enh.describe()); }
    }
    /// Montages by slot: Melee_Equip on UpperBody; the pickaxe swing on both of its slot tracks (standing-still clip on FullBodyInPlace,
    /// in-motion clip on UpperBody; the graph's ShouldBypassFullbodyInPlace blend picks between them).
    pub fn montages(&self) -> Vec<Montage> {
        let mut m = vec![];
        if self.anim.equip.active { m.push(Montage { slot: "UpperBody", clip: "Melee_WideGrip_Equip_CMM".to_owned(), time: self.anim.equip.clip_time(), weight: self.anim.equip.weight(), blend: fn_animgraph::SlotBlend::Pose }); }
        if let (Some(standing), Some(moving)) = (self.swing.clip(false), self.swing.clip(true)) {
            let w = self.swing.weight();
            m.push(Montage { slot: "FullBodyInPlace", clip: standing.to_owned(), time: self.swing.time, weight: w, blend: fn_animgraph::SlotBlend::Pose });
            m.push(Montage { slot: "UpperBody", clip: moving.to_owned(), time: self.swing.time, weight: w, blend: fn_animgraph::SlotBlend::Pose });
        }
        m.extend(self.arms.iter().cloned());
        m
    }
    /// Selects the held item's core animation set (`None` = the pickaxe path).
    pub fn set_held(&mut self, held: Option<(&'static str, bool)>) { self.anim.set_held(held); }
    /// The held firearm's fire clock for the native relaxed update (see `fn_animgraph::WeaponClock`); `None` = pickaxe / consumable.
    pub fn set_weapon_clock(&mut self, clock: Option<fn_animgraph::WeaponClock>) { self.anim.set_weapon_clock(clock); }
    /// The AnimBlueprint's pose tree for this tick (evaluate with `fn_animgraph::Skeleton::eval_pose`).
    pub fn pose_tree(&self) -> Node { self.anim.plan(&self.montages()) }

    /// Advances one frame with a REAL frame time: the only clamp is `time_audit::MAX_DELTA_TIME` (UE `MaxDeltaTime` stand-in, 0.1 s; it used to
    /// be 50 ms, which ran the whole game in slow motion below 20 fps). Same as `step(inp, raw_dt.min(MAX_DELTA_TIME))`.
    pub fn step_real(&mut self, inp: &FrameInput, raw_dt: f32) { self.step(inp, crate::time_audit::clamp_frame_dt(raw_dt)); }

    // ---- the frame / tick split of the old `App::update` + `App::sim_tick` (see `crate::sim_tick`, README "Fixed 30 Hz tick") ----
    /// The per-FRAME half of the old `App::update` (everything that runs at display rate, whatever `SIM_HZ` is): equip montage trigger, camera yaw/pitch (input,
    /// not simulation), harvestable shake decay / regrow timer, and the crouch / sprint hold-or-toggle latches. Returns the held input the ticks of this frame use
    /// (`jump` and `attack` still raw: the caller latches them, see [`crate::tick_driver::TickDriver`]).
    pub fn begin_frame(&mut self, inp: &FrameInput, dt: f32) -> TickInput {
        self.frame_dt = dt;
        self.drop_begin_frame(dt);
        if inp.selection_changed { self.swing = Swing::default(); self.anim.equip.active = false; } // never carry an outgoing melee montage / hit into a newly selected hotbar item
        if inp.equip { self.anim.equip.start(); }
        self.anim_time += dt;
        if let Some(y) = inp.set_yaw { self.yaw = y.rem_euclid(360.); }
        if let Some(p) = inp.set_pitch { self.pitch = p; }
        self.world.update(dt); // cosmetic (shake, regrow): per frame so the wobble is not stepped at the tick rate
        self.yaw += inp.yaw_delta; self.pitch += inp.pitch_delta;
        self.yaw = self.yaw.rem_euclid(360.); self.pitch = camera::clamp_pitch(self.pitch);
        let (forward, side) = (inp.forward, inp.side);
        // Crouch/sprint: hold or toggle per settings (Input.CrouchMode / ToggleSprint).
        let moving = forward != 0. || side != 0.;
        let crouch_on = self.crouch_latch.update(inp.crouch_key, self.crouch_toggle, inp.jump);
        let run_on = self.sprint_latch.update(inp.sprint_key, self.sprint_toggle, self.was_moving && !moving);
        self.was_moving = moving;
        TickInput { forward, side, crouch: crouch_on || inp.force_crouch, run: run_on || inp.force_run, jump: inp.jump || inp.script_jump, attack: inp.attack && !inp.non_melee && !self.items_blocked() }
    }
    /// One fixed simulation tick of `h` seconds (the old `App::sim_tick`, minus the pose evaluation the renderer side adds): swing montage + its TriggerAbility hit
    /// trace, the movement controller (the CMC port), jump/fall stage, animation graph variables, camera smoothing + collision. Reads `self.yaw` / `self.pitch`.
    pub fn tick(&mut self, t: &TickInput, h: f32) {
        let knocked = TickInput { crouch: false, run: false, jump: false, attack: false, ..*t };
        let t = if self.dbno { &knocked } else { t };
        if self.drop_tick_aircraft(t, h) { self.cam_view.hide_pawn = true; return; }
        let swing_idle = self.swing.stage == 0;
        if let Some(Notify::TriggerAbility { .. }) = self.swing.step(t.attack, h) {
            // TriggerAbility notify: trace from the character along its facing; a hit takes the Bounce section.
            let f = self.controller.facing.to_radians();
            let p = self.controller.render_position();
            // HARVEST HOOK: the camera ray is the aim of the ability's WeakSpots line selection (fn_combat::harvest_fx)
            self.world.fx.aim = Some(fn_combat::harvest_fx::Aim { eye: self.cam_view.eye, dir: self.cam_view.forward });
            // 1.8 (decomp AFortPawn vfunc 331 TriggerAbilityFromAnimation + ability fn 14029c670): the montage jumps to MontageSectionIfBlocked (Bounce1/2) only when the
            // ability's blocked flag (+0x9f0) is set, i.e. the strike hit at least one actor with bCanBeDamaged == false and NO damageable actor. A harvest hit on a prop (damaged)
            // never bounces, so the swing plays through (Swing -> End / next Swing). The sandbox world has no undamageable actors, so `Swing::blocked()` is not reachable here yet.
            let _harvested = self.world.strike(Vec3::new(p.x, self.controller.render_height(), Z_OFFSET + p.y), Vec3::new(f.sin(), 0., -f.cos()));
        }
        if self.fly != FlyMode::Walk { self.tick_fly(t, h); return; }
        let (dx, dz) = horizontal_step(t.forward, t.side, self.yaw, 1., 1.);
        // Jump request (ACharacter::Jump/CheckJumpInput/CanJump): the controller sees the key STATE; a rising edge is one jump attempt that is dropped
        // at the end of that tick. CanJump refuses it while falling (JumpMaxCount 1), so an air press never re-triggers anything. The jump key also
        // cancels a crouch (ASSUMED), so the crouch request is dropped on the press tick.
        let crouch_wanted = t.crouch && !self.controller.jump_edge(t.jump);
        self.controller.wants_crouch = crouch_wanted;
        self.sync_terrain(); // TERRAIN
        self.refresh_blockers();
        let sprint_blocked = self.ads.apply_speed(&mut self.controller); // ADS HOOK: targeting limits the speed stats to SpeedWalk and cancels sprint
        if self.dbno { let p = &mut self.controller.component_mut().p; let s = fn_movement::params::SPEED_DBNO; p.speed_run = s; p.speed_sprint = s; p.speed_crouched_run = s; p.speed_crouched_sprint = s; }
        if self.speed_scale != 1. { let p = &mut self.controller.component_mut().p; p.speed_run *= self.speed_scale; p.speed_sprint *= self.speed_scale; p.speed_crouched_run *= self.speed_scale; p.speed_crouched_sprint *= self.speed_scale; } // F2 CONSOLE `speed`
        self.controller.actor_yaw = fn_movement::params::fixes_1p8().then_some(self.yaw);
        self.fall.before_step(self.controller.height, self.controller.airborne()); // FALL DAMAGE
        let gate = SprintGateIn { swing_started: swing_idle && self.swing.stage > 0, light: self.swing.blocks_interact_build(), sprinting: self.controller.running, reloading: self.reloading, sprint_cancels_reload: self.sprint_cancels_reload, toggle: self.sprint_toggle };
        let (run, cancel_reload) = self.melee_sprint.step(t, gate, &mut self.sprint_latch, h);
        self.sprint_cancelled_reload |= cancel_reload;
        self.drop_before_step(t);
        self.sync_enhanced(sprint_blocked);
        let ext_before = self.controller.enhanced.as_ref().map(|e| (e.tac.phase, e.clamber.count, e.clamber.is_moving(), e.slider.count, self.controller.component().is_sliding(), e.hurdle.count, e.hurdle.is_moving()));
        self.controller.step(Vec2::new(dx, dz), t.jump, run && !sprint_blocked, h);
        if let Some(e) = self.controller.enhanced.as_ref() {
            let c = self.controller.component();
            let now = (e.tac.phase, e.clamber.count, e.clamber.is_moving(), e.slider.count, c.is_sliding(), e.hurdle.count, e.hurdle.is_moving());
            if ext_before != Some(now) { eprintln!("[enhanced] tac {:?} speed x{:.3} energy {:.1} end {:?} | clamber #{} moving {} | slide #{} active {} state {:?} end {:?} | hurdle #{} moving {} | v {:.0} cm/s feet {:.2} m", e.tac.phase, e.tac.speed_mult, e.tac.energy, e.tac.last_end, e.clamber.count, e.clamber.is_moving(), e.slider.count, c.is_sliding(), c.slide.map(|s| s.state), e.slider.last_end, e.hurdle.count, e.hurdle.is_moving(), Vec2::new(c.velocity.x, c.velocity.y).length(), self.controller.height); }
        }
        self.drop_after_step();
        self.fall.after_step(self.controller.height, self.controller.airborne(), self.controller.landings()); // FALL DAMAGE
        self.crouch_camera = smooth_toward(self.crouch_camera, if self.controller.crouched { 0.5 } else { 0. }, 12., h);
        let stage = animation_stage(self.controller.air_height(), self.controller.vertical_speed, self.jump_stage, self.state_time);
        if stage != self.jump_stage { self.state_time = 0.; self.jump_stage = stage; } else { self.state_time += h; }
        let target = (self.controller.velocity.length() / 1.5).min(1.);
        self.locomotion_blend = smooth_toward(self.locomotion_blend, target, 12., h);
        self.step_anim(Vec2::new(dx, dz), h);
        // Clamp the simulation itself (not just the camera) so the controller can't overshoot an invisible wall.
        // TERRAIN: the landscape bounds (minus EDGE_MARGIN) when one is installed, else the plate
        let (lo, hi) = self.world_box(); let z = Vec2::new(0., Z_OFFSET);
        if !self.in_warmup() { self.controller.clamp_to_box(lo - z, hi - z); }
        self.controller.facing = self.anim.visual_yaw(self.yaw); // visible body yaw from the graph (Actor Yaw Offset / turn-in-place)
        self.drop_update_camera_blend(h);
        let p = self.controller.render_position(); let height = self.controller.render_height();
        self.advance_camera(Vec3::new(p.x, height, Z_OFFSET + p.y), h);
    }
    /// F2 `fly` / `ghost`: moves the feet along the view (forward includes pitch), jump / crouch = up / down; the movement component is not stepped (it picks the
    /// position up as a teleport when `walk` hands control back, and falls from there).
    fn tick_fly(&mut self, t: &TickInput, h: f32) {
        let (yaw, pitch) = (self.yaw.to_radians(), self.pitch.to_radians());
        let fwd = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos());
        let right = Vec3::new(yaw.cos(), 0., yaw.sin());
        let up = (t.jump as i32 - t.crouch as i32) as f32;
        let mut v = fwd * t.forward + right * t.side + Vec3::Y * up;
        if v.length_squared() > 1. { v = v.normalize(); }
        let v = v * FLY_SPEED * if t.run { 3. } else { 1. };
        let p = self.controller.render_position();
        let mut feet = Vec3::new(p.x, self.controller.render_height(), p.y) + v * h;
        if self.fly == FlyMode::Fly { feet.y = feet.y.max(self.ground_y(feet.x, feet.z + Z_OFFSET)); }
        self.controller.position = Vec2::new(feet.x, feet.z); self.controller.velocity = Vec2::ZERO; self.controller.vertical_speed = 0.;
        self.controller.height = feet.y;
        self.controller.facing = self.anim.visual_yaw(self.yaw);
        self.locomotion_blend = smooth_toward(self.locomotion_blend, 0., 12., h);
        self.step_anim(Vec2::ZERO, h);
        self.advance_camera(Vec3::new(feet.x, feet.y, Z_OFFSET + feet.z), h);
    }
    /// Camera collision world = everything that blocks ECC_Camera (the penetration sweep @1404f85a0 is `SweepSingleByChannel` on
    /// channel 4): the baseplate (landscape), the live harvestables (`BuildingProp`: trees / rocks, profile `FortBuildingMesh` =
    /// WorldStatic, Camera not overridden = Block, CookedIniVersion.txt line 1353) and the placed player pieces (`ABuildingSMActor` ctor
    /// @14045b960 gives StaticMeshComponent0 `FortBuildingMesh`; cooked PBWA_* components keep its responses), as their current class
    /// convexes at their transforms (`building_hulls`, the hulls the movement collides with). Any future camera-blocking world geometry
    /// (POI buildings, static mesh actors) goes in here too. TERRAIN: `terrain` is the landscape (the sim's [`GameSim::terrain`]), `None` = the baseplate.
    /// ALL POIS (PERF, 2026-10-07): the props the camera's traces can touch this tick: every prop without a hull, and the hull props whose bounding sphere comes
    /// within [`CAMERA_PROPS_M`] of the feet (with all 35k POI actors in the props, a full scan per feeler trace cost ~1 ms per tick).
    pub fn camera_props(&self) -> Vec<fn_combat::Harvestable> { self.camera_prop_ids().iter().map(|&i| self.world.props[i as usize].clone()).collect() }
    /// PERF (2026-10-08): [`GameSim::camera_props`] as ascending indices into `world.props` (no per-tick clones), from the prop index's candidates
    /// when it is fresh (the same filter decides: the same set).
    pub fn camera_prop_ids(&self) -> Vec<u32> {
        let f = self.feet();
        let keep = |p: &fn_combat::Harvestable| p.hull.as_ref().map_or(true, |h| (h.center - f).length() - h.radius <= CAMERA_PROPS_M);
        let linear = || (0..self.world.props.len() as u32).filter(|&i| keep(&self.world.props[i as usize])).collect::<Vec<u32>>();
        let Some(ix) = self.fresh_prop_index() else { return linear() };
        let mut ids = vec![];
        ix.hull_candidates(Vec2::new(f.x, f.z), CAMERA_PROPS_M, &mut ids);
        ids.retain(|&i| keep(&self.world.props[i as usize]));
        ids.extend_from_slice(ix.plain());
        ids.sort_unstable();
        if fn_props::index::verify() { let lin = linear(); if lin != ids { eprintln!("[prop index] MISMATCH camera props: {} indexed vs {} linear", ids.len(), lin.len()); } }
        ids
    }
    /// `ids` = the props of `props` the traces test ([`GameSim::camera_prop_ids`]).
    pub fn camera_world<'a>(props: &'a [fn_combat::Harvestable], ids: &'a [u32], buildings: &'a [fn_combat::Hull], terrain: Option<Arc<fn_terrain::World>>) -> impl Fn(Vec3, Vec3, f32) -> Option<f32> + 'a {
        move |a: Vec3, b: Vec3, r: f32| {
            let min = |x: Option<f32>, y: Option<f32>| match (x, y) { (Some(x), Some(y)) => Some(x.min(y)), (x, y) => x.or(y) };
            let pieces = if buildings.is_empty() { None } else { fn_combat::trace_sphere_hulls(buildings, a, b, r) };
            min(min(camera::trace_ground_on(terrain.as_deref(), a, b, r), fn_props::trace_ids(props, ids, a, b, r)), pieces)
        }
    }
    /// One camera smoothing tick (pivot lag, shoulder/crouch offset, boom feelers, penetration blend) at the simulation step.
    pub fn advance_camera(&mut self, feet: Vec3, dt: f32) {
        self.sync_prop_index();
        let near = self.camera_prop_ids();
        let hulls = Self::camera_hulls(&self.building_hulls, &self.door_hulls); // DOORS: the pieces + the door leaves
        let world = Self::camera_world(&self.world.props, &near, &hulls, self.terrain());
        let mode = self.drop_tick_mode().or(self.enhanced_camera().map(|c| c.0)).unwrap_or(self.ads.tick_mode());
        self.cam.advance(mode, camera::Input { feet, crouched: self.controller.crouched, yaw_deg: self.yaw, pitch_deg: self.pitch, dt }, world);
    }
    /// What the renderer needs from the state after the last tick (position, floor height, visible facing, camera smoothing state); the driver adds poses.
    pub fn snapshot(&self) -> Snapshot {
        let p = self.controller.render_position();
        Snapshot { pos: p, height: self.controller.render_height(), facing: self.controller.facing, cam: self.cam.state(), poses: vec![] }
    }
    /// Shows `s` (usually an interpolation of two tick snapshots): pawn position x/y/z and the camera view composed from the interpolated smoothing state and
    /// THIS frame's yaw/pitch (mouse look is input, not simulation).
    pub fn apply_view(&mut self, s: &Snapshot) {
        if self.on_bus() { if let Some((bus, bus_yaw)) = self.drop.as_ref().and_then(|d| d.aircraft_view()) { self.x = bus.x; self.y = bus.y; self.z = bus.z; self.ads.fov = crate::drop_in::AIRCRAFT_CAMERA_FOV; self.cam_view = crate::drop_in::aircraft_view(bus, bus_yaw, self.yaw, self.pitch); } return; }
        self.x = s.pos.x; self.z = Z_OFFSET + s.pos.y; self.y = EYE + s.height;
        let feet = Vec3::new(s.pos.x, s.height, Z_OFFSET + s.pos.y);
        self.sync_prop_index();
        let near = self.camera_prop_ids();
        let hulls = Self::camera_hulls(&self.building_hulls, &self.door_hulls); // DOORS
        let follow_mode = self.drop_tick_mode().or(self.enhanced_camera().map(|c| c.0)).unwrap_or(self.ads.tick_mode());
        let st = if camera::frame_follow_on() { let f = camera::CamState::follow(follow_mode, self.frame_cam, &s.cam, feet, self.yaw, self.pitch, self.frame_dt); self.frame_cam = Some(f); f } else { s.cam };
        let world = Self::camera_world(&self.world.props, &near, &hulls, self.terrain());
        let dropped = if self.drop.is_some() { self.drop_compose_ref(&st, feet, &world) } else { None };
        let held_view = dropped.is_none();
        self.cam_view = match dropped { Some((v, fov)) => { self.ads.fov = fov; v } None => self.ads.compose(&st, feet, self.yaw, self.pitch, &world) };
        drop(world);
        if held_view { self.apply_enhanced_view(&st, feet); } // ADS HOOK: hold view mixed with the ADS view (weight 0 = Camera::compose(MELEE))
    }
    fn enhanced_camera(&self) -> Option<(&'static camera::Mode, f32)> {
        if self.drop.is_some() || self.ads.active { return None; }
        let e = self.controller.enhanced.as_ref()?;
        if e.slider.camera_blend > 0. { return Some((&ENHANCED_SLIDING_CAMERA, e.slider.camera_blend)); }
        if e.tac.camera_blend > 0. { return Some((&ENHANCED_TAC_SPRINT_CAMERA, e.tac.camera_blend)); }
        None
    }
    fn apply_enhanced_view(&mut self, st: &camera::CamState, feet: Vec3) {
        let Some(e) = self.controller.enhanced.as_ref() else { return };
        let (slide_t, tac_shake) = (e.slider.shake_time, e.tac_shake);
        if let Some((mode, w)) = self.enhanced_camera() {
            self.sync_prop_index();
            let near = self.camera_prop_ids();
            let hulls = Self::camera_hulls(&self.building_hulls, &self.door_hulls);
            let world = Self::camera_world(&self.world.props, &near, &hulls, self.terrain());
            let v = camera::Camera::compose(mode, st, feet, self.yaw, self.pitch, world);
            self.cam_view.eye = self.cam_view.eye.lerp(v.eye, w); self.cam_view.safe = self.cam_view.safe.lerp(v.safe, w); self.cam_view.desired = self.cam_view.desired.lerp(v.desired, w);
            if w >= 0.5 { self.cam_view.hide_pawn = v.hide_pawn; }
            self.ads.fov += (mode.fov - self.ads.fov) * w;
        }
        let mut loc = tac_shake.tac_loop_offset_cm();
        if let Some(t) = slide_t { let (x, fov) = fn_movement::enhanced::slide_shake(t); loc.x += x; self.ads.fov += fov; }
        let v = &mut self.cam_view;
        v.eye += (v.forward * loc.x + v.right * loc.y + v.up * loc.z) * camera::CM;
    }
    fn sync_enhanced(&mut self, sprint_blocked: bool) {
        static ENV_ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let on = fn_movement::enhanced::enabled() || *ENV_ON.get_or_init(|| fn_live::var("FN_ENHANCED_MODE").is_ok_and(|v| v == "1"));
        if on && self.controller.enhanced.is_none() { self.controller.enhanced = Some(Default::default()); }
        if !on { self.controller.enhanced = None; return; }
        if let Some(e) = self.controller.enhanced.as_mut() { e.sprint_toggle = self.sprint_toggle; }
        self.controller.ext_block = if self.dbno { Some("DBNO or dead") } else if self.on_bus() || self.controller.component().is_skydiving() || self.controller.component().in_aerial_mode() { Some("on pawn aircraft") } else if sprint_blocked { Some("ability blocked") } else { self.ext_block_app };
    }
    /// The R key / a scripted reset: back to the spawn with a fresh controller.
    pub fn respawn(&mut self) { self.drop = None; self.to_spawn(); self.controller = self.new_controller(); self.sync_terrain(); self.stand_on_floor(); }
    /// ALL POIS (2026-10-07): puts the pawn on the highest collision surface under its (x, z) ([`GameSim::floor_below`] from [`SPAWN_PROBE_M`] above the
    /// landscape: a POI terrace / floor / roof over the landscape) when that is above it; the landscape alone left it inside the POI geometry.
    pub fn stand_on_floor(&mut self) {
        self.sync_terrain(); // first: a later sync onto a newly installed landscape would put a grounded pawn back on the landscape
        self.sync_prop_index();
        let (x, z) = (self.x, self.z);
        let top = self.floor_below(Vec3::new(x, self.ground_y(x, z) + SPAWN_PROBE_M, z));
        if top > self.controller.height + 1e-3 { self.controller.height = top; self.controller.vertical_speed = 0.; }
        // SPAWN FIX (2026-10-07): the eye follows the controller in every case. `sync_terrain` above may already have raised the controller onto the
        // landscape installed after the pawn was placed (without moving the eye): the test above then saw no gap and the pawn stayed at its pre-landscape
        // height, under the ground (browser build without POI files: feet 0.00 m, landscape 1.49 m; natively wherever no POI floor is under the spawn).
        self.y = EYE + self.controller.height;
    }

    /// Advances one frame in PER-FRAME mode (the old `FNRT_SIM_HZ=0`): one tick of the real frame time, drawn without interpolation. This is the path the harness
    /// (`fn-sim`) and the movement/jump logs use; the fixed-rate game loop is [`crate::tick_driver::TickDriver`]. `dt` is the frame time (the app clamps real
    /// frames to `MAX_DELTA_TIME` = 0.1 s; scripted runs use 1/60).
    pub fn step(&mut self, inp: &FrameInput, dt: f32) {
        let held = self.begin_frame(inp, dt);
        self.tick(&held, dt);
        let s = self.snapshot(); self.apply_view(&s);
        if inp.reset { self.respawn(); }
        self.frames += 1;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MeleeSprintGate { pub cut: bool, pub cooldown: f32, pub was_reloading: bool }
pub const SPRINT_RECAST_S: f32 = 0.5;
pub const MOVEMENT_CANCEL_LEASH_M: f32 = 0.75;
impl GameSim {
    pub fn movement_cancels_action(&mut self, active: bool) -> Option<&'static str> {
        let (running, airborne) = (self.controller.running, self.controller.airborne());
        let prev = std::mem::replace(&mut self.heal_prev, (running, airborne));
        if !active { self.heal_anchor = None; return None; }
        let p = self.feet();
        let a = *self.heal_anchor.get_or_insert(p);
        let why = if running && !prev.0 { Some("by sprint") } else if airborne && !prev.1 { Some("by jump / fall") } else if (p - a).length() > MOVEMENT_CANCEL_LEASH_M { Some("by movement (leash 75 cm)") } else { None };
        if why.is_some() { self.heal_anchor = None; }
        why
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SprintGateIn { pub swing_started: bool, pub light: bool, pub sprinting: bool, pub reloading: bool, pub sprint_cancels_reload: bool, pub toggle: bool }
impl MeleeSprintGate {
    pub fn step(&mut self, t: &TickInput, g: SprintGateIn, latch: &mut ModeLatch, h: f32) -> (bool, bool) {
        self.cooldown = (self.cooldown - h).max(0.);
        let forward = t.forward > 0.;
        let reload_started = g.reloading && !self.was_reloading;
        self.was_reloading = g.reloading;
        let cancel = g.sprinting && (g.swing_started || !forward || reload_started || (g.reloading && !g.sprint_cancels_reload));
        if cancel {
            self.cooldown = SPRINT_RECAST_S;
            if g.toggle { latch.cancel(); } else if t.attack { self.cut = true; }
        }
        if self.cut && t.run && !t.attack && forward && !g.reloading { self.cut = false; }
        let run = t.run && forward && !g.light && !self.cut && self.cooldown <= 0. && (!g.reloading || g.sprint_cancels_reload);
        (run, run && g.reloading)
    }
}
/// Held input of one frame as the ticks of that frame read it (the old `AppSim.forward/side/crouch_on/run_on` + the latched `jump` / `attack`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickInput { pub forward: f32, pub side: f32, pub crouch: bool, pub run: bool, pub jump: bool, pub attack: bool }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn landing_is_one_shot_and_jump_can_interrupt() { assert_eq!(animation_stage(0., 0., 3, 0.1), 4); assert_eq!(animation_stage(0., 0., 4, 0.5), 4); assert_eq!(animation_stage(0., 0., 4, 0.84), 0); assert_eq!(animation_stage(0.1, 6., 4, 0.2), 1); }
    #[test] fn eight_direction_selection() { for (f, s, name) in [(1., 0., "N"), (1., 1., "NE"), (0., 1., "E"), (-1., 1., "SE"), (-1., 0., "S"), (-1., -1., "SW"), (0., -1., "W"), (1., -1., "NW")] { assert_eq!(direction_name(f, s), name); } }
    #[test] fn direction_blending_interpolates_and_wraps() { let angle = std::f32::consts::PI / 8.; let (a, b, t) = direction_blend(angle.cos(), angle.sin()); assert_eq!((a, b), ("N", "NE")); assert!((t - 0.5).abs() < 1e-6); let (a, b, t) = direction_blend(angle.cos(), -angle.sin()); assert_eq!((a, b), ("NW", "N")); assert!((t - 0.5).abs() < 1e-6); assert_eq!(direction_blend(-1., 0.).0, "S"); }
    #[test] fn diagonal_speed_matches_straight_speed() { let (x, z) = horizontal_step(1.0, 1.0, 0.0, 5.0, 1.0); assert!(((x * x + z * z).sqrt() - 5.0).abs() < 0.0001); }
    #[test] fn movement_follows_camera_heading() { let (x, z) = horizontal_step(1.0, 0.0, 90.0, 5.0, 1.0); assert!((x - 5.0).abs() < 0.0001 && z.abs() < 0.0001); }
    #[test] fn idle_does_not_move_and_spawn_is_on_the_ground() { assert_eq!(horizontal_step(0.0, 0.0, 50.0, 10.0, 1.0), (0.0, 0.0)); let (x, y, z, _, _) = spawn(); let (lo, hi) = world_box(fn_terrain::world().as_deref()); assert!(x > lo.x && x < hi.x && z > lo.y && z < hi.y && y == EYE + fn_terrain::ground_y(x, z)); }

    fn run(frames: u32, f: impl Fn(u32) -> FrameInput) -> GameSim { let mut g = GameSim::new(); for i in 0..frames { g.step(&f(i), 1. / 60.); } g }
    /// The runtime feeds the held firearm's fire clock through `set_weapon_clock`: the graph reaches bIsRelaxedLevel1 after 5 s without a shot (and switches the upper-body
    /// pose to the relaxed one), a shot / ADS-free firing cancels it, putting the rifle away clears it (1.8 FortAnimInstance rule, see fn-animgraph AIM_RELAXED.md).
    #[test] fn weapon_clock_drives_the_relaxed_pose_and_a_shot_cancels_it() {
        use fn_animgraph::WeaponClock;
        let mut g = GameSim::new(); g.world.props.clear(); g.set_held(Some(("AssaultRifle", false)));
        let (dt, mut now, mut fire) = (1. / 30., 0f32, -10f32);
        let run = |g: &mut GameSim, now: &mut f32, fire: f32, secs: f32| { for _ in 0..(secs / dt).round() as usize { *now += dt; g.set_weapon_clock(Some(WeaponClock { world_time: *now, last_fire_time: fire, equipping: false, reloading: false, charging: false })); g.step_anim(Vec2::ZERO, dt); } };
        let relaxed = |g: &GameSim| { let mut v = vec![]; g.pose_tree().clip_names(&mut v); v.iter().any(|n| n == "M_AssaultRifle_Relaxed") };
        run(&mut g, &mut now, fire, 4.5);
        assert!(!g.anim.vars.b_is_relaxed_level1 && !relaxed(&g), "4.5 s: still ready (counter {})", g.anim.vars.relaxed_level_time_counter);
        run(&mut g, &mut now, fire, 1.0);
        assert!(g.anim.vars.b_is_relaxed_level1 && !g.anim.vars.b_is_relaxed_level2 && g.anim.vars.temp_is_relaxed_level1 && relaxed(&g), "5.5 s: relaxed level 1");
        fire = now; run(&mut g, &mut now, fire, 0.3);
        assert!(!g.anim.vars.b_is_relaxed_level1 && !relaxed(&g) && !g.anim.vars.b_recently_fired, "a shot cancels relaxed; bRecentlyFired lasts 0.2 s");
        run(&mut g, &mut now, fire, 15.5);
        assert!(g.anim.vars.b_is_relaxed_level2, "15 s after the last shot: level 2");
        g.set_weapon_clock(None); g.step_anim(Vec2::ZERO, dt);
        assert!(!g.anim.vars.b_is_relaxed_level1 && !g.anim.vars.b_is_relaxed_level2, "pickaxe in hand: never relaxed");
    }    #[test] fn walking_forward_moves_along_minus_z_at_the_run_speed_and_the_camera_follows() {
        let g = run(120, |_| FrameInput { forward: 1., ..Default::default() });
        assert!(g.z < 12. - 2., "forward is -Z: z {}", g.z); assert!((g.controller.velocity.length() - 4.1).abs() < 0.05, "{}", g.controller.velocity.length());
        assert!((g.cam_view.eye - g.feet()).length() > 1.5 && g.cam_view.eye.is_finite(), "third-person boom behind the pawn");
    }
    #[test] fn holding_fire_next_to_a_tree_harvests_it_and_bounces() {
        let mut g = GameSim::new(); g.world.props.clear(); g.world.props.push(fn_combat::Harvestable::new(fn_combat::Kind::Tree, Vec3::new(0., 0., 12. - 1.8)));
        for _ in 0..300 { g.step(&FrameInput { attack: true, ..Default::default() }, 1. / 60.); }
        assert!(g.world.hits >= 3 && g.world.wood > 0, "hits {} wood {}", g.world.hits, g.world.wood);
    }
    #[test] fn same_inputs_give_bit_identical_state() {
        let f = |i: u32| FrameInput { forward: if (30..200).contains(&i) { 1. } else { 0. }, side: if (100..140).contains(&i) { 1. } else { 0. }, jump: i == 80 || i == 150, sprint_key: (60..180).contains(&i), yaw_delta: if i < 90 { 1.5 } else { 0. }, ..Default::default() };
        let (a, b) = (run(300, f), run(300, f));
        assert_eq!((a.x.to_bits(), a.y.to_bits(), a.z.to_bits()), (b.x.to_bits(), b.y.to_bits(), b.z.to_bits())); assert_eq!(a.anim.describe(), b.anim.describe()); assert_eq!(format!("{:?}", a.cam_view), format!("{:?}", b.cam_view));
    }
    #[test] fn enhanced_camera_modes_blend_the_view_and_fov() {
        let mut g = GameSim::new(); g.world.props.clear();
        for _ in 0..30 { g.step(&FrameInput::default(), 1. / 60.); }
        let s = g.snapshot(); g.apply_view(&s); let (eye0, fov0) = (g.cam_view.eye, g.ads.fov);
        let mut e = fn_movement::enhanced::Enhanced::default(); e.tac.camera_blend = 1.;
        g.controller.enhanced = Some(e); g.apply_view(&s);
        assert!((g.ads.fov - ENHANCED_TAC_SPRINT_CAMERA.fov).abs() < 1e-4 && fov0 == 80., "{} {}", g.ads.fov, fov0);
        assert!((g.cam_view.eye - eye0).length() > 0.05);
        let mut e = fn_movement::enhanced::Enhanced::default(); e.slider.camera_blend = 1.; e.slider.shake_time = Some(0.6);
        g.controller.enhanced = Some(e); g.apply_view(&s);
        let (_, fov_shake) = fn_movement::enhanced::slide_shake(0.6);
        assert!((g.ads.fov - (ENHANCED_SLIDING_CAMERA.fov + fov_shake)).abs() < 1e-3, "{}", g.ads.fov);
    }
    #[test] fn reset_returns_to_the_spawn() { let mut g = run(60, |_| FrameInput { forward: 1., ..Default::default() }); g.step(&FrameInput { reset: true, ..Default::default() }, 1. / 60.); assert_eq!((g.x, g.z, g.yaw), (0., 12., 0.)); assert_eq!(g.controller.position, Vec2::ZERO); }
    /// The edge of the world stops the pawn (the plate here: no landscape in a unit-test binary; `the_landscape_is_the_ground_*` covers the landscape edge).
    #[test] fn the_world_edge_stops_the_pawn() { let g = run(60 * 100, |_| FrameInput { forward: 1., sprint_key: true, ..Default::default() }); assert!(g.z >= g.world_box().0.y - 0.01 && g.z <= -HALF + 0.5 + 0.01, "z {}", g.z); }
    /// TERRAIN: a hilly synthetic landscape (game = UE / 100, 2 m quads, +-60 m) through `GameSim::terrain`.
    fn hills() -> Arc<fn_terrain::World> {
        let hf = fn_terrain::Heightfield::synthetic(Vec2::splat(-6000.), 200., 2, 31, |x, y| 300. * (x / 1500.).sin() + 200. * (y / 1100.).cos() - 150.);
        Arc::new(fn_terrain::World { hf, map: fn_terrain::Mapping { centre_cm: [0.; 3], anchor: Vec3::ZERO, ref_z_cm: 0. } })
    }
    #[test] fn the_landscape_is_the_ground_spawn_walk_camera_and_edge() {
        let w = hills();
        let mut g = GameSim::new(); g.world.props.clear(); g.terrain = Some(w.clone()); g.respawn();
        assert!((g.y - EYE - w.ground_y(0., 12.).unwrap()).abs() < 1e-5, "spawn eye on the ground");
        // the capsule base vs the ground under its axis: between 0 and r (1 / cos - 1) + floor dist (these hills are under 15 deg: < 2 cm + 2.4 cm)
        let off = |g: &GameSim| g.controller.height - w.ground_y(g.x, g.z).unwrap();
        for i in 0..30 { g.step(&FrameInput::default(), 1. / 60.); assert!(g.controller.grounded() && off(&g).abs() < 0.05, "idle frame {i}: {}", off(&g)); }
        let (mut worst, mut climbed) = (0f32, (f32::MAX, f32::MIN));
        for i in 0..(30 * 60) {
            g.step(&FrameInput { forward: 1., sprint_key: true, yaw_delta: if i % 600 < 60 { 1.5 } else { 0. }, ..Default::default() }, 1. / 60.);
            assert!(g.controller.grounded(), "frame {i}: airborne at ({}, {}) feet {} ground {}", g.x, g.z, g.controller.height, w.ground_y(g.x, g.z).unwrap());
            worst = worst.max(off(&g).abs()); climbed = (climbed.0.min(g.controller.height), climbed.1.max(g.controller.height));
            assert!(g.cam_view.eye.y > w.ground_y(g.cam_view.eye.x, g.cam_view.eye.z).unwrap_or(-10.), "frame {i}: camera under the landscape");
        }
        assert!(worst < 0.05 && climbed.1 - climbed.0 > 2., "feet vs ground worst {worst} m, height range {climbed:?}");
        // the landscape edge (+-60 m) stops the pawn EDGE_MARGIN inside it
        let (lo, hi) = g.world_box(); assert!((lo - Vec2::splat(-59.)).length() < 1e-3 && (hi - Vec2::splat(59.)).length() < 1e-3);
        for _ in 0..(40 * 60) { g.step(&FrameInput { forward: 1., sprint_key: true, ..Default::default() }, 1. / 60.); }
        assert!(g.x >= lo.x - 1e-3 && g.x <= hi.x + 1e-3 && g.z >= lo.y - 1e-3 && g.z <= hi.y + 1e-3 && (g.x - lo.x).abs().min((g.x - hi.x).abs()).min((g.z - lo.y).abs()).min((g.z - hi.y).abs()) < 1e-3, "at the edge: ({}, {})", g.x, g.z);
        // R: back on the ground at the spawn
        g.step(&FrameInput { reset: true, ..Default::default() }, 1. / 60.); g.step(&FrameInput::default(), 1. / 60.);
        assert!(g.controller.grounded() && off(&g).abs() < 0.05 && (g.z - 12.).abs() < 1e-3);
    }
    /// G1 (docs/spec/building_core_gaps.md): placed pieces block ECC_Camera, so the boom stops in front of a wall behind the pawn
    /// and pulls in inside a 1x1 box.
    #[test] fn the_camera_collides_with_placed_pieces() {
        let slab = |lo: Vec3, hi: Vec3| { let mut v = vec![]; for x in [lo.x, hi.x] { for y in [lo.y, hi.y] { for z in [lo.z, hi.z] { v.push(Vec3::new(x, y, z)); } } } fn_combat::Hull::from_points(&v).unwrap() };
        // spawn (0, 12) looking -Z: the 2.8 m boom heads to +Z; a wall plane 1.2 m behind the pawn
        let mut g = GameSim::new(); g.world.props.clear();
        g.building_hulls = vec![slab(Vec3::new(-2.56, 0., 13.2), Vec3::new(2.56, 3.84, 13.36))];
        for _ in 0..90 { g.step(&FrameInput::default(), 1. / 60.); }
        assert!(g.cam_view.eye.z <= 13.2 - 0.08 + 1e-3 && g.cam_view.blocked_pct < 0.6, "eye {:?} kept {}", g.cam_view.eye, g.cam_view.blocked_pct);
        // inside a 5.12 m box (4 walls + roof) the eye stays inside for every yaw
        let mut g = GameSim::new(); g.world.props.clear();
        let (c, h) = (Vec3::new(0., 0., 12.), 2.56);
        g.building_hulls = vec![slab(c + Vec3::new(-h, 0., -h - 0.16), c + Vec3::new(h, 3.84, -h)), slab(c + Vec3::new(-h, 0., h), c + Vec3::new(h, 3.84, h + 0.16)),
            slab(c + Vec3::new(-h - 0.16, 0., -h), c + Vec3::new(-h, 3.84, h)), slab(c + Vec3::new(h, 0., -h), c + Vec3::new(h + 0.16, 3.84, h)), slab(c + Vec3::new(-h, 3.84, -h), c + Vec3::new(h, 4.0, h))];
        for i in 0..360 {
            g.step(&FrameInput { yaw_delta: 2., pitch_delta: if i < 30 { -1. } else { 0. }, ..Default::default() }, 1. / 60.);
            let e = g.cam_view.eye;
            assert!(e.x.abs() < h - 0.07 && (e.z - 12.).abs() < h - 0.07 && e.y < 3.84 - 0.07, "frame {i}: eye {e:?} left the box");
        }
    }
    /// PERF (2026-10-08): the prop index changes no query: the camera's prop set and the floor below a point equal the linear scans, over a field of
    /// hull props (towers, slabs, some destroyed) and placeholders, at random points.
    #[test] fn indexed_camera_props_and_floor_equal_the_linear_scans() {
        use fn_combat::{Harvestable, Hull, HullSet, Kind, Resource};
        let mut s = 0x1357_9bdfu32;
        let mut rnd = |a: f32, b: f32| { s ^= s << 13; s ^= s >> 17; s ^= s << 5; a + (b - a) * ((s >> 8) as f32 / (1u32 << 24) as f32) };
        let mut g = GameSim::new();
        for i in 0..3000 {
            let p = Vec3::new(rnd(-400., 400.), 0., rnd(-400., 400.));
            let (hx, hz, y0, y1) = (rnd(0.3, 15.), rnd(0.3, 15.), rnd(0., 20.), rnd(20.5, 40.));
            let mut v = vec![];
            for x in [-hx, hx] { for y in [y0, y1] { for z in [-hz, hz] { v.push(p + Vec3::new(x, y, z)); } } }
            let mut pr = if i % 50 == 0 { Harvestable::new(Kind::Rock, p) } else { Harvestable::real(Kind::Tree, Resource::Wood, p, 0., 1., 600., 0, Some(Arc::new(HullSet::new(vec![Hull::from_points(&v).unwrap()])))) };
            if i % 9 == 0 { pr.health = 0.; }
            g.world.props.push(pr);
        }
        g.sync_prop_index();
        assert!(g.fresh_prop_index().is_some());
        let mut hits = 0;
        for _ in 0..400 {
            g.x = rnd(-450., 450.); g.z = rnd(-450., 450.); g.y = rnd(0., 45.);
            let p = Vec3::new(g.x, rnd(0., 60.), g.z);
            let (ids, floor) = (g.camera_prop_ids(), g.floor_below(p));
            let ix = g.prop_index.take();
            assert!(g.fresh_prop_index().is_none());
            let (lin_ids, lin_floor) = (g.camera_prop_ids(), g.floor_below(p));
            g.prop_index = ix;
            assert_eq!(ids, lin_ids);
            assert_eq!(floor.to_bits(), lin_floor.to_bits(), "floor below {p:?}");
            if floor > 0. { hits += 1; }
        }
        assert!(hits > 20, "floors found: {hits}");
        // a new props list makes the index stale until the next sync
        g.world.props.truncate(10);
        assert!(g.fresh_prop_index().is_none());
        g.sync_prop_index();
        assert!(g.fresh_prop_index().is_some());
    }
    #[test] fn a_pickaxe_swing_ends_sprint_and_hold_sprint_returns_after_release() {
        let mut g = GameSim::new();
        let h = 1. / 30.;
        let run = |g: &mut GameSim, n: usize, attack: bool, sprint: bool| { let mut v = vec![]; for _ in 0..n { let t = g.begin_frame(&FrameInput { forward: 1., sprint_key: sprint, attack, ..Default::default() }, h); g.tick(&t, h); v.push((g.controller.velocity.length(), g.controller.running, g.swing.stage)); } v };
        let s = run(&mut g, 60, false, true);
        assert!(s.last().unwrap().1 && (s.last().unwrap().0 - 5.5).abs() < 0.05, "sprinting at 5.5 m/s: {:?}", s.last());
        let s = run(&mut g, 1, true, true);
        assert!(!s[0].1 && s[0].2 == 1, "the swing press cancels sprint on its activation tick: {:?}", s[0]);
        let s = run(&mut g, 20, true, true);
        assert!(s.iter().all(|r| !r.1), "no sprint while the combo is held");
        assert!((s.last().unwrap().0 - 4.1).abs() < 0.05, "run speed 4.1 m/s during the swing: {:?}", s.last());
        let s = run(&mut g, 40, false, true);
        let back = s.iter().position(|r| r.1).expect("hold sprint comes back after the release");
        assert!(back > 0 && s[..back].iter().all(|r| !r.1));
        let mut g = GameSim::new(); g.sprint_toggle = true;
        run(&mut g, 1, false, true); run(&mut g, 60, false, false);
        assert!(g.controller.running);
        run(&mut g, 1, true, false); run(&mut g, 60, false, false);
        assert!(!g.controller.running && !g.sprint_latch.active(), "a swing ends a toggled sprint");
    }
}

pub const ENHANCED_TAC_SPRINT_CAMERA: camera::Mode = camera::Mode {
    name: "Athena_PlayerCameraMode_TacticalSprint", fov: 95., forward_distance: -250.,
    origin_interp: [14., 60., 60.],
    vt_offset: &[camera::Sample { pitch: 0., offset: [86., 52., 55.] }],
    vt_offset_crouch: &[camera::Sample { pitch: 0., offset: [38., 31., 30.] }],
    vt_interp: [12., 12., 12.],
    cs_offset: &[camera::Sample { pitch: 0., offset: [0., 0., 0.] }, camera::Sample { pitch: 90., offset: [0., 0., 150.] }, camera::Sample { pitch: -90., offset: [25., 0., -85.] }, camera::Sample { pitch: 45., offset: [-45., 0., 65.] }, camera::Sample { pitch: -45., offset: [15., 0., -30.] }],
    hide_pawn_pct: 0.15, transition_time: 0.8,
    ..camera::BASE
};

pub const ENHANCED_SLIDING_CAMERA: camera::Mode = camera::Mode {
    name: "Athena_PlayerCameraMode_Sliding", fov: 90.,
    origin_interp: [60., 1.0e9, 60.],
    vt_offset: &[camera::Sample { pitch: 0., offset: [38., 65., -5.] }],
    vt_offset_crouch: &[camera::Sample { pitch: 0., offset: [38., 45., 10.] }],
    vt_interp: [8., 8., 8.],
    cs_offset: &[camera::Sample { pitch: 0., offset: [0., 0., 0.] }, camera::Sample { pitch: 90., offset: [30., 0., 100.] }, camera::Sample { pitch: -90., offset: [50., 0., -20.] }],
    hide_pawn_pct: 0.31,
    ..camera::BASE
};
