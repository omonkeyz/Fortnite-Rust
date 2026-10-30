//! Player controller of the Fortnite 1.8 runtime: a metres / Y-up wrapper around the UE 4.16 `UCharacterMovementComponent` port
//! (`movement_cmc.rs`, UE centimetres / Z-up) plus the facing/turn helpers the camera and animation use.
//!
//! Axis mapping (see movement_cmc.rs): controller `position.x -> X`, `position.y` (= world z - 12) `-> Y`, `height -> Z`
//! (feet above the baseplate); metres = UE cm / 100. `velocity` is the horizontal velocity (m/s), `vertical_speed` is velocity Z (m/s).
//!
//! `step(dt)` advances whatever `dt` it is given; the movement component sub-steps long steps internally (`MaxSimulationTimeStep`
//! 0.05 s, `MaxSimulationIterations` 8). The game calls it from the fixed 1/`SIM_HZ` (30 Hz) tick (`sim_tick.rs`, `App::sim_tick`) and the renderer
//! interpolates between the last two ticks; with `FNRT_SIM_HZ=0` it is called once per rendered frame with the real frame time (the old behaviour,
//! which `jump_feel`, `jump_air_test` and `--movement-test` still exercise directly). `render_position()` is the position of the last step.
#![allow(dead_code)]
use glam::{Vec2, Vec3};
use crate::cmc::{Cmc, Params};
use crate::scene::{Blocker, Scene};

/// Longest frame time simulated in one `step` (a hitch guard, not an engine value; the engine sub-steps anything longer than 0.05 s).
pub const MAX_FRAME_DT: f32 = 0.25;
/// Mouse look, 1.8 exe chain (C:\FortniteRef\decomp\named; CookedIniVersion.txt = the cooker's ini value list in pakchunk0):
/// 1. `UFortPlayerInputAthena::vfunc_71` @1404392c0 (the `MassageAxisInput` override; it does NOT call `SmoothMouse`, so 1.8 has no mouse smoothing
///    although `bEnableMouseSmoothing=true`, ini L1971): value = counts x `AxisProperties.Sensitivity`, x the targeting / scoped multiplier while the pawn
///    targets (+0x4f4 / +0x4f8), x `GetFOVAngle() * UInputSettings.FOVScale` when `bEnableFOVScaling` (ini L1972 true, L1973 `FOVScale=0.01111`).
/// 2. `AxisProperties.Sensitivity` of MouseX AND MouseY is REPLACED by `UFortGameUserSettings.MouseSensitivity` (+0x108):
///    `UPlayerInput::SetMouseSensitivity` @1425095f0 (one value for both axes) called from `AFortPlayerController::InitInputSystem` @140830750 and
///    `UFortGameUserSettings::ApplySettings` (vfunc_70 @1405bcb10). Its default 0.07 (0x3d8f5c29) is written by the ctor @1405b4930, equal to the
///    AxisConfig value `MouseX/MouseY Sensitivity=0.07f` (ini L1985/L1986).
/// 3. Axis mappings `Turn` = MouseX x1, `LookUp` = MouseY x-1 (ini L2001/L2003) -> `APawn::AddControllerYaw/PitchInput` -> `APlayerController::AddYawInput`
///    @1423895a0 x `InputYawScale` (+0x594, ini L3167 2.5) / `AddPitchInput` @142388900 x `InputPitchScale` (+0x598, ini L3168 -1.75).
///    Invert View = `UPlayerInput::InvertAxis("LookUp")` (read by `GetInvertAxis` in `FullscreenMapCursorVertical` @1404271f0): it negates LookUp.
/// So yaw deg/count = MouseSensitivity x 2.5 x FOV x 0.01111 (x ADS/scope multiplier); pitch (mouse up = look up) uses 1.75.
/// At the default 0.07 and the 80 deg hold FOV: 0.1556 deg/count yaw, 0.1089 deg/count pitch.
pub const MOUSE_SENSITIVITY_DEFAULT: f32 = 0.07;
/// `PlayerController:InputYawScale` (ini L3167).
pub const INPUT_YAW_SCALE: f32 = 2.5;
/// `PlayerController:InputPitchScale` (ini L3168) is -1.75; with `LookUp` Scale -1 (ini L2003) mouse-up pitches up by 1.75 per unit.
pub const INPUT_PITCH_SCALE: f32 = -1.75;
/// `InputSettings:FOVScale` (ini L1973), applied because `bEnableFOVScaling=true` (ini L1972).
pub const MOUSE_FOV_SCALE: f32 = 0.01111;
/// The FOV term of `vfunc_71`: `GetFOVAngle() * FOVScale` (1.0 when the FOV is unknown, like the exe without a camera manager).
pub fn mouse_fov_factor(fov: f32) -> f32 { if fov > 0. { fov * MOUSE_FOV_SCALE } else { 1. } }
/// Degrees of (yaw, pitch-up) per raw mouse count for the native `MouseSensitivity` value at camera FOV `fov`.
pub fn mouse_degrees_per_count(sensitivity: f32, fov: f32) -> (f32, f32) {
    let k = sensitivity * mouse_fov_factor(fov);
    (k * INPUT_YAW_SCALE, k * -INPUT_PITCH_SCALE)
}
/// Effective gravity magnitude in m/s^2 (`params::WORLD_GRAVITY_Z` -2800 cm/s^2 x GravityScale 1 x GravityZScale 1, see FALLING.md section 9;`FNRT_GRAVITY_SCALE` is a `cmc::Tuning`, see `Controller::with_tuning`).
pub fn gravity() -> f32 {
    static G: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *G.get_or_init(|| Params::default().gravity_z().abs() * 0.01)
}
/// Time until touchdown for a body `height` (m) above the floor under it (`Controller::air_height`, not the absolute feet height) moving at `vertical_speed` (m/s, up positive), 0 when grounded.
/// Used by the animation graph's landing prediction.
pub fn fall_time_left(height: f32, vertical_speed: f32) -> f32 {
    if height <= 0. { return 0.; }
    let g = gravity();
    (vertical_speed + (vertical_speed * vertical_speed + 2. * g * height).sqrt()) / g
}
/// PROVISIONAL character turn rates (deg/s). The original RotationRate is NOT serialized in any 1.8 package
/// (engine default would be 360 deg/s yaw), so these are placeholders, not extracted values.
pub const FACING_MOVING_RATE: f32 = 720.;
pub const TURN_IN_PLACE_RATE: f32 = 360.;
const TURN_IN_PLACE_START: f32 = 90.;
const TURN_IN_PLACE_STOP: f32 = 2.;
/// Signed shortest rotation (degrees, in (-180,180]) taking `from` to `to`.
pub fn shortest_angle(from: f32, to: f32) -> f32 { let d = (to - from).rem_euclid(360.); if d > 180. { d - 360. } else { d } }
/// Rotate `current` toward `target` by at most `rate*dt` degrees along the shortest arc.
pub fn turn_toward(current: f32, target: f32, rate: f32, dt: f32) -> f32 {
    let d = shortest_angle(current, target); let step = (rate * dt.max(0.)).min(d.abs());
    (current + step * d.signum()).rem_euclid(360.)
}
/// Frame-rate independent exponential smoothing (replaces `(dt*rate).min(1)` lerps).
pub fn smooth_toward(current: f32, target: f32, rate: f32, dt: f32) -> f32 { current + (target - current) * (1. - (-rate * dt.max(0.)).exp()) }

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Exported { position: Vec2, velocity: Vec2, height: f32, vertical_speed: f32, crouched: bool }

#[derive(Debug)]
pub struct Controller {
    /// Feet position on the baseplate plane, metres (x, world z - 12).
    pub position: Vec2,
    /// Horizontal velocity, m/s.
    pub velocity: Vec2,
    /// Feet height, metres (TERRAIN: absolute game y; the baseplate is 0). While walking it is the height of the floor surface under the feet (0 on the baseplate,
    /// the prop top when standing on one): use `airborne()` / `grounded()`, not `height > 0`, to decide whether the pawn is in the air.
    pub height: f32,
    /// Vertical velocity, m/s (up positive).
    pub vertical_speed: f32,
    /// Actual crouch state (`bIsCrouched`). Writing it from outside is treated as a crouch REQUEST (legacy callers); use `wants_crouch`.
    pub crouched: bool,
    /// Crouch input (`bWantsToCrouch`): the component crouches when it can and un-crouches when the key is released AND the standing capsule fits.
    pub wants_crouch: bool,
    pub running: bool,
    /// Character heading in the same degree convention as the camera yaw.
    pub facing: f32,
    pub actor_yaw: Option<f32>,
    turning: bool,
    /// Trunk/rock blockers in controller space (propcol.rs), refreshed by the caller each frame.
    pub blockers: Vec<Blocker>,
    /// Convex hulls of the real props near the pawn, in controller space (metres: x, world z - 12, up), refreshed by the caller each tick like `blockers`.
    pub hulls: Vec<fn_hull::Hull>,
    /// TERRAIN: the landscape in controller space (scene cm, see `fn_terrain::Local`; the game sets it with its z offset 12), `None` = the flat baseplate.
    pub terrain: Option<fn_terrain::Local>,
    cmc: Cmc,
    exported: Exported,
    jump_key_prev: bool,
    pub enhanced: Option<crate::enhanced::Enhanced>,
    pub ext_block: Option<&'static str>,
    /// PERF (2026-10-08): the `hulls` list `cmc.scene.hulls` was last converted from (see `step`).
    hulls_cm_src: Vec<fn_hull::Hull>,
}
impl Default for Controller {
    fn default() -> Self {
        Controller { position: Vec2::ZERO, velocity: Vec2::ZERO, height: 0., vertical_speed: 0., crouched: false, wants_crouch: false, running: false, facing: 0., actor_yaw: None, turning: false, blockers: vec![], hulls: vec![], terrain: None,
            cmc: Cmc::new(Params::default(), Scene::default(), Vec3::ZERO), exported: Exported::default(), jump_key_prev: false, enhanced: None, ext_block: None, hulls_cm_src: vec![] }
    }
}
impl Controller {
    /// A controller whose movement component uses `Params::default().with_tuning(t)` (`FNRT_GRAVITY_SCALE` / `FNRT_JUMPZ` experiments).
    pub fn with_tuning(t: &crate::cmc::Tuning) -> Self { let mut c = Controller::default(); c.cmc.p = Params::default().with_tuning(t); c }
    /// `fall_time_left` for THIS controller's gravity (same bits as the free function unless a gravity tuning is active).
    pub fn fall_time_left(&self) -> f32 {
        let (height, vertical_speed) = (self.air_height(), self.vertical_speed);
        if height <= 0. { return 0.; }
        let g = self.cmc.p.gravity_z().abs() * 0.01;
        (vertical_speed + (vertical_speed * vertical_speed + 2. * g * height).sqrt()) / g
    }
    /// The pawn is off the ground (`MOVE_Falling`).
    pub fn airborne(&self) -> bool { self.cmc.is_falling() }
    pub fn grounded(&self) -> bool { !self.cmc.is_falling() }
    /// Height above the ground while airborne (never 0 in the air), 0 while grounded - also on a prop top. Feed this, not `height`, to code that
    /// treats `> 0` as "airborne" (animation graph, combat upper-body rule, jump stage).
    /// TERRAIN: measured from the floor under the feet (the landscape / a prop top / the baseplate), since `height` is the absolute feet height.
    pub fn air_height(&self) -> f32 {
        if !self.airborne() { return 0.; }
        let feet = Vec3::new(self.position.x * 100., self.position.y * 100., self.height * 100.);
        ((feet.z - self.cmc.scene.floor_below(feet)) * 0.01).max(1e-4)
    }
    /// `jump` (the key state of THIS frame) is a fresh press: true on the rising edge, before `step` consumes it.
    pub fn jump_edge(&self, jump: bool) -> bool { jump && !self.jump_key_prev }
    /// Read access to the movement component (tests, `--movement-test` logging).
    pub fn launch(&mut self, launch_cm: glam::Vec3, xy_override: bool, z_override: bool) -> glam::Vec3 {
        let cur = self.cmc.velocity;
        let mut v = launch_cm;
        if !xy_override { v.x += cur.x; v.y += cur.y; }
        if !z_override { v.z += cur.z; }
        self.cmc.launch(v);
        v
    }
    pub fn velocity_cm(&self) -> glam::Vec3 { self.cmc.velocity }
    pub fn component(&self) -> &Cmc { &self.cmc }
    pub fn component_mut(&mut self) -> &mut Cmc { &mut self.cmc }
    /// Number of jumps the component has started (`DoJump` succeeded) since creation.
    pub fn jumps_started(&self) -> u32 { self.cmc.jumped }
    pub fn landings(&self) -> u32 { self.cmc.landed }
    /// Current standing/crouched capsule half height (cm).
    pub fn capsule_half_height_cm(&self) -> f32 { self.cmc.half_height }

    /// Advance one frame: `input` is the world-space movement direction (|input| <= 1, x and controller-y axes), `jump` the jump KEY STATE
    /// (a rising edge is `ACharacter::Jump`, a falling edge `StopJumping`), `run` the sprint request, `dt` the frame time.
    pub fn step(&mut self, input: Vec2, jump: bool, run: bool, dt: f32) {
        let dt = dt.clamp(0., MAX_FRAME_DT);
        self.cmc.scene.solids.clear();
        self.cmc.scene.solids.extend(Scene::from_blockers(&self.blockers).solids);
        // PERF (2026-10-08): the cm scene hulls are a pure function of `hulls` (each is `mapped(v * 100)`: `Hull::finish` of its vertices and triangles), so they are
        // rebuilt only when the list differs from the one converted last (vertex bits / triangles compared); the same scene either way. `FN_PROP_INDEX=0` = every step.
        if !same_hull_list(&self.hulls, &self.hulls_cm_src) || !hull_cache_on() {
            self.cmc.scene.hulls = if self.hulls.is_empty() { vec![] } else { Scene::hulls_to_cm(&self.hulls) };
            self.hulls_cm_src.clone_from(&self.hulls);
        }
        self.cmc.scene.terrain = self.terrain.clone();
        // after the scene refresh: an external move is judged against the floor it lands on (TERRAIN)
        self.import_external_changes();
        let input = input.clamp_length_max(1.);
        self.cmc.wants_sprint = run;
        self.cmc.wants_crouch = self.wants_crouch;
        let f = self.actor_yaw.unwrap_or(self.facing).to_radians(); self.cmc.facing = Vec3::new(f.sin(), -f.cos(), 0.);
        // ASSUMED input layer (not engine code): a jump press whose crouch REQUEST is already off (key released / toggle cancelled on this very press) stands the capsule up
        // first. UE4.16 `CanJumpInternal` refuses while `bIsCrouched` (Character.cpp:265) and `CheckJumpInput` runs before `PerformMovement` un-crouches, so without this the press would be lost.
        let mut jump_consumed = false;
        let mut clamber_pos = None;
        let mut hurdle_pos = None;
        let jump_edge = jump && !self.jump_key_prev;
        if let Some(e) = self.enhanced.as_mut() {
            let busy = e.hurdle.is_moving() || e.clamber.is_moving();
            if e.slider.update(&mut self.cmc, self.wants_crouch, jump_edge, self.ext_block.is_some() || busy, dt) { self.cmc.un_crouch(); }
            if self.cmc.is_sliding() { self.cmc.wants_crouch = true; } else if e.slider.stand_until_release { self.cmc.wants_crouch = false; }
            if jump_edge && e.hurdle.try_start(&self.cmc, e.tac.active()) { jump_consumed = true; }
            else if jump_edge { e.clamber.press(); }
            if !jump_consumed { jump_consumed = e.clamber.try_start(&self.cmc); }
            hurdle_pos = e.hurdle.advance(dt);
            clamber_pos = e.clamber.advance(dt);
            let jumped = self.cmc.jumped != e.last_jumps;
            e.last_jumps = self.cmc.jumped;
            let v = self.cmc.velocity;
            let ti = crate::enhanced::TacInput { wants: run, sprint_style: run && (!self.cmc.is_crouched || self.cmc.is_sliding()) && input.length_squared() > 0., grounded: self.cmc.is_moving_on_ground() || hurdle_pos.is_some(), falling: self.cmc.is_falling() && hurdle_pos.is_none(), jumped,
                speed_2d: hurdle_pos.map_or(Vec2::new(v.x, v.y).length(), |(p, _, _)| Vec2::new(p.x - self.cmc.loc.x, p.y - self.cmc.loc.y).length() / dt.max(1e-4)), blocked: self.ext_block.or(if clamber_pos.is_some() { Some("clambering") } else { None }), toggle: e.sprint_toggle };
            e.tac.tick(&ti, dt);
            e.tac_shake.tick(e.tac.phase == crate::enhanced::TacPhase::Active && !self.cmc.is_sliding(), dt);
            self.cmc.ext = e.tac.multipliers();
        } else {
            if self.cmc.ext != crate::enhanced::ExtMultipliers::default() { self.cmc.ext = crate::enhanced::ExtMultipliers::default(); }
            if self.cmc.slide.is_some() { self.cmc.slide = None; }
        }
        if jump_edge && !jump_consumed && clamber_pos.is_none() && hurdle_pos.is_none() && !(self.cmc.p.jump_release_ignored && self.cmc.is_falling()) { if !self.cmc.p.jump_input_1p8 && self.cmc.is_crouched && !self.cmc.wants_crouch { self.cmc.un_crouch(); } self.cmc.jump(); }
        if !jump && self.jump_key_prev && !self.cmc.p.jump_release_ignored { self.cmc.stop_jumping(); }
        self.jump_key_prev = jump;
        match (hurdle_pos, clamber_pos) {
            (Some((p, done, v)), _) => { self.cmc.clamber_move(p); if done { self.cmc.clamber_finish(); self.cmc.velocity = v; } }
            (None, Some((p, done, v))) => { self.cmc.clamber_move(p); if done { self.cmc.clamber_finish(); self.cmc.velocity = v; } }
            (None, None) => self.cmc.tick(input, dt),
        }
        self.running = run && !self.cmc.is_crouched && input.length_squared() > 0.;
        self.export();
    }
    /// Position to render. The simulation already runs at the frame rate, so this is the simulated position.
    pub fn render_position(&self) -> Vec2 { self.position }
    pub fn render_height(&self) -> f32 {
        let hover = self.cmc.feet_z() * 0.01 - self.height;
        if self.height == self.exported.height && (0. ..=crate::cmc::MAX_FLOOR_DIST * 0.01).contains(&hover) { self.height + hover } else { self.height }
    }
    fn export(&mut self) {
        let c = &self.cmc;
        self.position = Vec2::new(c.loc.x, c.loc.y) * 0.01;
        self.velocity = Vec2::new(c.velocity.x, c.velocity.y) * 0.01;
        let h = c.feet_height() * 0.01; self.height = if h.abs() < 1e-5 { 0. } else { h };
        self.vertical_speed = c.velocity.z * 0.01;
        self.crouched = c.is_crouched;
        self.exported = Exported { position: self.position, velocity: self.velocity, height: self.height, vertical_speed: self.vertical_speed, crouched: self.crouched };
    }
    /// Callers may still assign `position`, `velocity`, `height`, `vertical_speed` or `crouched` directly (spawn, tests, clamp_to_box):
    /// whatever differs from what the last step exported is imported into the component as a teleport / request.
    fn import_external_changes(&mut self) {
        let s = self.exported;
        if self.position != s.position && self.height == s.height { self.cmc.loc.x = self.position.x * 100.; self.cmc.loc.y = self.position.y * 100.; let hh = self.cmc.half_height; self.cmc.teleport_to_feet(Vec3::new(self.cmc.loc.x, self.cmc.loc.y, self.cmc.loc.z - hh)); }
        if self.height != s.height {
            let hh = self.cmc.half_height; let mut feet = Vec3::new(self.position.x * 100., self.position.y * 100., self.height * 100.);
            // TERRAIN: grounded = the feet at / under the floor below them (within the floor search distance), not "height <= 0": the ground is the landscape.
            // Feet put under the ground (or under the landscape) are lifted onto it (CHOSEN; the old plate did the same through the floor search).
            let floor = self.cmc.scene.floor_below(feet + Vec3::new(0., 0., hh));
            if feet.z <= floor + crate::cmc::MAX_FLOOR_DIST {
                feet.z = feet.z.max(floor);
                self.cmc.place_on_ground(feet);
            } else { self.cmc.loc = feet + Vec3::new(0., 0., hh); self.cmc.start_falling_externally(); }
        }
        if self.velocity != s.velocity { self.cmc.velocity.x = self.velocity.x * 100.; self.cmc.velocity.y = self.velocity.y * 100.; }
        if self.vertical_speed != s.vertical_speed { self.cmc.velocity.z = self.vertical_speed * 100.; }
        if self.crouched != s.crouched { self.wants_crouch = self.crouched; }
    }
    /// Keep the simulation inside the world box (so the controller cannot overshoot an
    /// invisible clamp and feel sticky); cancels velocity into the wall.
    pub fn clamp_to_box(&mut self, min: Vec2, max: Vec2) {
        self.position = self.position.clamp(min, max);
        if self.position.x <= min.x && self.velocity.x < 0. || self.position.x >= max.x && self.velocity.x > 0. { self.velocity.x = 0.; }
        if self.position.y <= min.y && self.velocity.y < 0. || self.position.y >= max.y && self.velocity.y > 0. { self.velocity.y = 0.; }
    }
    /// Update character heading from the camera yaw (variable frame dt, rate-based so frame-rate
    /// independent). Moving: track camera quickly (strafe-style, as the animation set assumes).
    /// Idle: hold heading until the camera is >90 deg away, then turn in place until aligned.
    pub fn update_facing(&mut self, camera_yaw: f32, dt: f32) {
        let delta = shortest_angle(self.facing, camera_yaw).abs();
        let moving = self.velocity.length_squared() > 0.01;
        if moving || self.airborne() { self.turning = false; self.facing = turn_toward(self.facing, camera_yaw, FACING_MOVING_RATE, dt); return; }
        if delta > TURN_IN_PLACE_START { self.turning = true; }
        if self.turning { self.facing = turn_toward(self.facing, camera_yaw, TURN_IN_PLACE_RATE, dt); if shortest_angle(self.facing, camera_yaw).abs() <= TURN_IN_PLACE_STOP { self.turning = false; } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn shortest_angle_wraps() { assert_eq!(shortest_angle(350., 10.), 20.); assert_eq!(shortest_angle(10., 350.), -20.); assert_eq!(shortest_angle(0., 180.), 180.); assert!(shortest_angle(90., 90.).abs() < 1e-6); assert!((shortest_angle(0., 540.) - 180.).abs() < 1e-4); }
    #[test] fn turn_toward_takes_short_arc_and_never_overshoots() { assert!((turn_toward(359., 1., 100., 0.01) - 0.).abs() < 1e-3 || (turn_toward(359., 1., 100., 0.01) - 360.).abs() < 1e-3); assert_eq!(turn_toward(10., 12., 720., 1.), 12.); let mut a = 10.; for _ in 0..100 { a = turn_toward(a, 350., 360., 1. / 60.); } assert!((a - 350.).abs() < 1e-3); }
    #[test] fn idle_character_holds_heading_then_turns_in_place() { let mut c = Controller::default(); c.update_facing(60., 1. / 60.); assert_eq!(c.facing, 0.); c.update_facing(120., 1. / 60.); assert!(c.facing > 0. && c.facing <= 6.0001); for _ in 0..120 { c.update_facing(120., 1. / 60.); } assert!((c.facing - 120.).abs() < 1e-3); }
    #[test] fn moving_character_tracks_camera_across_wrap() { let mut c = Controller { facing: 350., velocity: Vec2::X, ..Default::default() }; c.update_facing(10., 1. / 60.); assert!(c.facing > 350. || c.facing < 10.); let mut seen_mid = false; for _ in 0..30 { c.update_facing(10., 1. / 60.); if c.facing > 20. && c.facing < 340. { seen_mid = true; } } assert!(!seen_mid, "must never swing the long way round"); assert!((shortest_angle(c.facing, 10.)).abs() < 1e-3); }
    #[test] fn facing_rate_is_frame_rate_independent() { let mut r = vec![]; for rate in [30., 60., 240.] { let mut c = Controller { velocity: Vec2::X, ..Default::default() }; for _ in 0..(rate * 0.1) as u32 { c.update_facing(179., 1. / rate); } r.push(c.facing); } assert!((r[0] - r[2]).abs() < 1e-3 && (r[1] - r[2]).abs() < 1e-3); }
    #[test] fn smoothing_is_frame_rate_independent() { let mut r = vec![]; for rate in [40., 60., 240.] { let mut v = 0.; for _ in 0..(rate * 0.25) as u32 { v = smooth_toward(v, 1., 12., 1. / rate); } r.push(v); } assert!((r[0] - r[2]).abs() < 1e-4 && (r[1] - r[2]).abs() < 1e-4); }
    #[test] fn mouse_scale_is_original_default() {
        let (y, p) = mouse_degrees_per_count(MOUSE_SENSITIVITY_DEFAULT, 80.);
        assert!((y - 0.07 * 2.5 * 80. * 0.01111).abs() < 1e-6 && (y - 0.15554).abs() < 1e-4, "{y}");
        assert!((p - 0.07 * 1.75 * 80. * 0.01111).abs() < 1e-6, "{p}");
        assert_eq!(mouse_fov_factor(0.), 1.); assert!((mouse_fov_factor(15.) - 0.16665).abs() < 1e-5);
    }
    #[test] fn clamp_to_box_stops_sticky_overshoot() {
        let mut c = Controller::default(); c.velocity = Vec2::new(5., 0.);
        for _ in 0..600 { c.step(Vec2::X, false, false, 1. / 60.); c.clamp_to_box(Vec2::splat(-10.), Vec2::splat(10.)); }
        assert_eq!(c.position.x, 10.); assert_eq!(c.velocity.x, 0.);
        c.step(-Vec2::X, false, false, 1. / 60.); assert!(c.position.x < 10., "must leave the wall immediately");
    }
    #[test] fn fall_time_left_matches_the_ballistic_formula() {
        let g = gravity();
        assert!((fall_time_left(0., 0.)).abs() < 1e-6); assert!((fall_time_left(1e-4, 6.) - 2. * 6. / g).abs() < 1e-3);
        let apex = 6. * 6. / (2. * g); assert!((fall_time_left(apex, 0.) - (2. * apex / g).sqrt()).abs() < 1e-3);
    }
}

/// PERF (2026-10-08): `a` and `b` give the same `Scene::hulls_to_cm` (same length; per hull the same vertex bits and triangles, all `mapped` reads).
fn same_hull_list(a: &[fn_hull::Hull], b: &[fn_hull::Hull]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.tris == y.tris && x.verts.len() == y.verts.len() && x.verts.iter().zip(&y.verts).all(|(p, q)| p.to_array().map(f32::to_bits) == q.to_array().map(f32::to_bits)))
}
/// `FN_PROP_INDEX=0` also switches this cache off (the old per-step conversion).
fn hull_cache_on() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_PROP_INDEX").map_or(true, |v| v != "0")) }
#[cfg(test)]
mod hull_cache_tests {
    use super::*;
    /// PERF (2026-10-08): the cm scene hulls are converted only when the list changes, and always equal a fresh conversion.
    #[test] fn the_scene_hulls_follow_every_change_of_the_hull_list() {
        let cube = |x: f32| { let mut v = vec![]; for a in [0., 1.] { for b in [0., 1.] { for c in [0., 2.] { v.push(Vec3::new(x + a, 3. + b, c)); } } } fn_hull::Hull::from_points(&v).unwrap() };
        let mut c = Controller::default();
        for list in [vec![], vec![cube(0.)], vec![cube(0.)], vec![cube(1.)], vec![cube(1.), cube(3.)], vec![cube(3.), cube(1.)], vec![]] {
            c.hulls = list.clone();
            c.step(Vec2::ZERO, false, false, 1. / 60.);
            let want = if list.is_empty() { vec![] } else { Scene::hulls_to_cm(&list) };
            assert_eq!(format!("{:?}", c.cmc.scene.hulls), format!("{want:?}"));
        }
    }
}
