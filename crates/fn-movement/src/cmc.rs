//! Port of Unreal Engine 4.16 `UCharacterMovementComponent` (+ the `ACharacter` jump/crouch state it drives) for a single
//! locally controlled walking/falling pawn: the movement layer of the Fortnite 1.8 runtime (UE 4.16 CL 3724489).
//!
//! Every function is named after the engine function it implements (`PhysWalking`, `PhysFalling`, `CalcVelocity`,
//! `ApplyVelocityBraking`, `FindFloor`, `ComputeFloorDist`, `StepUp`, `MoveAlongFloor`, `SlideAlongSurface`, `TwoWallAdjust`,
//! `CheckFall`, `IsValidLandingSpot`, `ProcessLanded`, `DoJump`, `CanJump`, `Crouch`, `UnCrouch`, ...). The algorithms are written
//! from the UE 4.16 source (not recoverable from the pak: native code); the numbers they run on are in `movement_params.rs` with
//! their evidence status. Not ported (not reachable for a lone pawn on the baseplate): swimming, flying, root motion, moving
//! bases, physics interaction, AI path following, RVO avoidance, network prediction/correction, Fortnite's skydive/parachute modes.
//!
//! Units and axes: everything here is UE units (centimetres, X/Y horizontal, Z up). The runtime (movement.rs `Controller`) is
//! metres, Y up: controller `position.x -> X`, controller `position.y` (= world z - 12) `-> Y`, controller `height -> Z`.
//! The swap (x, z, y) -> (X, Y, Z) is handedness-flipping exactly like UE's left-handed system; no rotation enters the
//! algorithms except the facing yaw used by the directional speed curve, which `Controller` converts.
//!
//! Time: `PerformMovement` is called once per frame with the real frame time (as the engine's character tick does) and the
//! physics modes sub-step it with `MaxSimulationTimeStep` 0.05 s / `MaxSimulationIterations` 8 (`GetSimulationTimeStep`).
#![allow(dead_code)]
use glam::{Vec2, Vec3};
use crate::params as mp;
use crate::scene::{Capsule, Hit, Scene};

// ---- engine constants (UE 4.16 CharacterMovementComponent.cpp / EngineTypes / Math)
pub const MIN_TICK_TIME: f32 = 1e-6;
pub const MIN_FLOOR_DIST: f32 = 1.9;
pub const MAX_FLOOR_DIST: f32 = 2.4;
pub const BRAKE_TO_STOP_VELOCITY: f32 = 10.;
pub const SWEEP_EDGE_REJECT_DISTANCE: f32 = 0.15;
pub const MAX_STEP_SIDE_Z: f32 = 0.08;
pub const VERTICAL_SLOPE_NORMAL_Z: f32 = 0.001;
pub const KINDA_SMALL_NUMBER: f32 = 1e-4;
/// `UMovementComponent::PenetrationPullback` / `PenetrationOverlapCheckInflation`.
const PENETRATION_PULLBACK: f32 = 0.125;
const PENETRATION_OVERLAP_INFLATION: f32 = 0.1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode { Walking, Falling }

/// Tunables of the component (`UPROPERTY` values). Defaults come from `movement_params.rs`.
#[derive(Clone, Debug)]
pub struct Params {
    pub world_gravity_z: f32, pub gravity_scale: f32, pub gravity_z_scale_attribute: f32, pub terminal_velocity: f32,
    pub jump_z_velocity: f32, pub jump_max_hold_time: f32, pub jump_max_count: u32, pub apply_gravity_while_jumping: bool,
    pub air_control: f32, pub air_control_boost_multiplier: f32, pub air_control_boost_velocity_threshold: f32,
    pub falling_lateral_friction: f32, pub braking_deceleration_falling: f32,
    pub max_acceleration: f32, pub ground_friction: f32, pub braking_deceleration_walking: f32, pub braking_friction_factor: f32,
    pub braking_friction: f32, pub use_separate_braking_friction: bool, pub braking_sub_step_time: f32,
    pub max_step_height: f32, pub walkable_floor_z: f32, pub perch_radius_threshold: f32, pub perch_additional_height: f32, pub ledge_check_threshold: f32,
    pub can_walk_off_ledges: bool, pub can_walk_off_ledges_when_crouching: bool,
    pub max_simulation_time_step: f32, pub max_simulation_iterations: u32, pub max_depenetration_with_geometry: f32,
    pub radius: f32, pub stand_half_height: f32, pub crouched_half_height: f32,
    pub speed_run: f32, pub speed_sprint: f32, pub speed_crouched_run: f32, pub speed_crouched_sprint: f32, pub min_analog_walk_speed: f32,
    pub directional_speed: bool,
    /// `UFortMovementComp_CharacterAthena` consecutive-jump table `{JumpZ scale, speed scale}` (exe, see `params::JUMP_CHAIN_TABLE`); empty = no chain.
    pub jump_chain_table: [(f32, f32); 5], pub jump_chain_window: f32,
    /// `UFortMovementComp_Character::DoJump` pawn-tick-delta JumpZ scale (exe, `params::jump_frame_scale`); `false` switches the term off (a 60 Hz client frame gives 1.0 anyway).
    pub jump_frame_time_boost: bool,
    /// Bit 8 of the movement flag byte at +0x754: clamp the recorded delta to 0.05 s before the scale (exe default clear).
    pub jump_frame_dt_clamp: bool,
    pub jump_input_1p8: bool,
    pub jump_release_ignored: bool,
    pub falling_slope_safe_slide_angle: f32, pub falling_slope_safe_slide_max_speed: f32,
}
impl Default for Params {
    fn default() -> Self {
        let p = Params {
            world_gravity_z: mp::WORLD_GRAVITY_Z, gravity_scale: mp::GRAVITY_SCALE, gravity_z_scale_attribute: mp::GRAVITY_Z_SCALE_ATTRIBUTE, terminal_velocity: mp::TERMINAL_VELOCITY,
            jump_z_velocity: mp::JUMP_Z_VELOCITY, jump_max_hold_time: mp::JUMP_MAX_HOLD_TIME, jump_max_count: mp::JUMP_MAX_COUNT, apply_gravity_while_jumping: mp::APPLY_GRAVITY_WHILE_JUMPING,
            air_control: mp::AIR_CONTROL, air_control_boost_multiplier: mp::AIR_CONTROL_BOOST_MULTIPLIER, air_control_boost_velocity_threshold: mp::AIR_CONTROL_BOOST_VELOCITY_THRESHOLD,
            falling_lateral_friction: mp::FALLING_LATERAL_FRICTION, braking_deceleration_falling: mp::BRAKING_DECELERATION_FALLING,
            max_acceleration: mp::MAX_ACCELERATION, ground_friction: mp::GROUND_FRICTION, braking_deceleration_walking: mp::BRAKING_DECELERATION_WALKING, braking_friction_factor: mp::BRAKING_FRICTION_FACTOR,
            braking_friction: mp::BRAKING_FRICTION, use_separate_braking_friction: mp::USE_SEPARATE_BRAKING_FRICTION, braking_sub_step_time: mp::BRAKING_SUB_STEP_TIME,
            max_step_height: mp::MAX_STEP_HEIGHT, walkable_floor_z: mp::WALKABLE_FLOOR_Z, perch_radius_threshold: mp::PERCH_RADIUS_THRESHOLD, perch_additional_height: mp::PERCH_ADDITIONAL_HEIGHT, ledge_check_threshold: mp::LEDGE_CHECK_THRESHOLD,
            can_walk_off_ledges: mp::CAN_WALK_OFF_LEDGES, can_walk_off_ledges_when_crouching: mp::CAN_WALK_OFF_LEDGES_WHEN_CROUCHING,
            max_simulation_time_step: mp::MAX_SIMULATION_TIME_STEP, max_simulation_iterations: mp::MAX_SIMULATION_ITERATIONS, max_depenetration_with_geometry: mp::MAX_DEPENETRATION_WITH_GEOMETRY,
            radius: mp::CAPSULE_RADIUS, stand_half_height: mp::CAPSULE_HALF_HEIGHT, crouched_half_height: mp::CROUCHED_HALF_HEIGHT,
            speed_run: mp::SPEED_RUN, speed_sprint: mp::SPEED_SPRINT, speed_crouched_run: mp::SPEED_CROUCHED_RUN, speed_crouched_sprint: mp::SPEED_CROUCHED_SPRINT, min_analog_walk_speed: mp::MIN_ANALOG_WALK_SPEED,
            directional_speed: true,
            jump_chain_table: mp::JUMP_CHAIN_TABLE, jump_chain_window: mp::JUMP_CHAIN_WINDOW, jump_frame_time_boost: true, jump_frame_dt_clamp: false,
            jump_input_1p8: true, jump_release_ignored: mp::jump_release_ignored(),
            falling_slope_safe_slide_angle: mp::FALLING_SLOPE_SAFE_SLIDE_ANGLE, falling_slope_safe_slide_max_speed: mp::FALLING_SLOPE_SAFE_SLIDE_MAX_SPEED,
        };
        if mp::fixes_1p8() { p } else { p.pre_1p8_trace() }
    }
}
impl Params {
    pub fn pre_1p8_trace(mut self) -> Params {
        self.jump_max_hold_time = 0.; self.crouched_half_height = 40.; self.can_walk_off_ledges_when_crouching = false;
        self.max_simulation_time_step = 0.05; self.max_simulation_iterations = 8; self.min_analog_walk_speed = 0.; self.jump_input_1p8 = false; self.jump_release_ignored = false;
        self
    }
}
/// Experiment knobs for the two values the recovered data cannot pin (README "Gravity"): the gravity scale (engine default 1, so -980 cm/s^2)
/// and the jump speed (`JumpZVelocity`, recovered 600 cm/s). The old prototype read them from the environment inside `Params::default()`
/// (`FNRT_GRAVITY_SCALE`, `FNRT_JUMPZ`); here they are an explicit tunable of the movement parameters and `Params::default()` is pure:
///
/// ```text
/// Params::default().with_tuning(&Tuning { gravity_scale: Some(1.5), jump_z_velocity: Some(700.) })   // explicit
/// Params::tuned()                                                                                    // = default + Tuning::from_env()
/// ```
///
/// `Tuning::from_env` reads the two tuning variables (the kill switches `FN_MOVE_1P8` / `FN_JUMP_RELEASE` are read by `params`); the front-end calls it once and hands the result to
/// `Controller::with_tuning` / `GameSim::with_tuning`); values that are not finite and > 0 are ignored. Table of the resulting jumps:
/// `jump_feel::tuning_table` (`--jump-feel-test`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tuning {
    /// Replaces `Params::gravity_scale` (multiplies the world gravity -980 cm/s^2), e.g. 1.5.
    pub gravity_scale: Option<f32>,
    /// Replaces `Params::jump_z_velocity` (cm/s, recovered value 600), e.g. 700.
    pub jump_z_velocity: Option<f32>,
}
impl Tuning {
    pub const ENV_GRAVITY_SCALE: &'static str = "FNRT_GRAVITY_SCALE";
    pub const ENV_JUMP_Z: &'static str = "FNRT_JUMPZ";
    /// Parses the textual forms of the two environment variables (`None` / unparsable / non-finite / <= 0 = not set).
    pub fn parse(gravity_scale: Option<&str>, jump_z: Option<&str>) -> Tuning {
        let ok = |s: Option<&str>| s.and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite() && *v > 0.);
        Tuning { gravity_scale: ok(gravity_scale), jump_z_velocity: ok(jump_z) }
    }
    /// `FNRT_GRAVITY_SCALE` / `FNRT_JUMPZ` from the process environment.
    pub fn from_env() -> Tuning { Tuning::parse(std::env::var(Self::ENV_GRAVITY_SCALE).ok().as_deref(), std::env::var(Self::ENV_JUMP_Z).ok().as_deref()) }
    pub fn is_default(&self) -> bool { self.gravity_scale.is_none() && self.jump_z_velocity.is_none() }
}
impl Params {
    /// These parameters with `t` applied.
    pub fn with_tuning(mut self, t: &Tuning) -> Params {
        if let Some(s) = t.gravity_scale { self.gravity_scale = s; }
        if let Some(v) = t.jump_z_velocity { self.jump_z_velocity = v; }
        self
    }
    /// Defaults plus the environment tuning (`FNRT_GRAVITY_SCALE`, `FNRT_JUMPZ`): what the old `Params::default()` returned.
    pub fn tuned() -> Params { Params::default().with_tuning(&Tuning::from_env()) }
    /// Whether gravity scale or jump speed differ from the recovered / engine defaults.
    pub fn is_tuned(&self) -> bool { self.gravity_scale != mp::GRAVITY_SCALE || self.jump_z_velocity != mp::JUMP_Z_VELOCITY }
}
impl Params {
    /// One line for verification/movement-tuning.txt (`FNRT_DIAGNOSTICS=1`): the effective gravity / JumpZ and the jump numbers they give.
    pub fn tuning_line(&self) -> String {
        let g = -self.gravity_z() * 0.01; let v = self.jump_z_velocity * 0.01;
        format!("movement tuning: gravity {:.0} cm/s^2 (world {} x GravityScale {} x GravityZScale {}), JumpZ {} cm/s -> apex {:.3} m, rise {:.3} s, airtime {:.3} s{}",
            g * 100., self.world_gravity_z, self.gravity_scale, self.gravity_z_scale_attribute, self.jump_z_velocity, v * v / (2. * g), v / g, 2. * v / g,
            if self.is_tuned() { "  [tuned: GravityScale / JumpZ differ from the defaults (FNRT_GRAVITY_SCALE / FNRT_JUMPZ via Tuning)]" } else { "  [defaults]" })
    }
}

impl Params {
    /// `GetGravityZ` chain, cm/s^2, negative: `((world gravity) x GravityScale) x GravityZScale` in the exe's multiplication order (0x207ea90 then 0x6d8350). GravityZScale is
    /// the `FortMovementSet` attribute, 1.0 at construction (exe 0x416dc0). The skydive / parachute `GravityScalar` factor of 0x428f00 is not ported.
    pub fn gravity_z(&self) -> f32 { (self.world_gravity_z * self.gravity_scale) * self.gravity_z_scale_attribute }
    pub fn falling_slope_safe_slide_normal_z(&self) -> f32 { (self.falling_slope_safe_slide_angle.clamp(0., 90.) * (std::f32::consts::PI / 180.)).cos() }
}
/// `FFindFloorResult`.
#[derive(Clone, Copy, Debug)]
pub struct Floor { pub blocking_hit: bool, pub walkable: bool, pub line_trace: bool, pub floor_dist: f32, pub line_dist: f32, pub hit: Hit }
impl Default for Floor { fn default() -> Self { Floor { blocking_hit: false, walkable: false, line_trace: false, floor_dist: 0., line_dist: 0., hit: Hit::none() } } }
impl Floor {
    pub fn is_walkable_floor(&self) -> bool { self.blocking_hit && self.walkable }
    pub fn distance_to_floor(&self) -> f32 { if self.line_trace { self.line_dist } else { self.floor_dist } }
    fn set_from_sweep(&mut self, hit: Hit, dist: f32, walkable: bool) {
        self.blocking_hit = hit.valid_blocking(); self.walkable = walkable; self.line_trace = false; self.floor_dist = dist; self.line_dist = 0.; self.hit = hit;
    }
    fn set_from_line_trace(&mut self, line: Hit, sweep_floor_dist: f32, line_dist: f32, walkable: bool) {
        if self.hit.blocking && line.blocking {
            let old = self.hit; self.hit = line;
            self.hit.time = old.time; self.hit.impact_point = old.impact_point; self.hit.location = old.location; self.hit.trace_start = old.trace_start; self.hit.trace_end = old.trace_end;
            self.line_trace = true; self.floor_dist = sweep_floor_dist; self.line_dist = line_dist; self.walkable = walkable;
        }
    }
}
/// `FStepDownResult`.
#[derive(Clone, Copy, Default)]
struct StepDown { computed: bool, floor: Floor }

fn safe_normal(v: Vec3) -> Vec3 { let l2 = v.length_squared(); if l2 <= 1e-8 { Vec3::ZERO } else { v / l2.sqrt() } }
fn safe_normal_2d(v: Vec3) -> Vec3 { let l2 = v.x * v.x + v.y * v.y; if l2 <= 1e-8 { Vec3::ZERO } else { let l = l2.sqrt(); Vec3::new(v.x / l, v.y / l, 0.) } }
fn size_2d(v: Vec3) -> f32 { (v.x * v.x + v.y * v.y).sqrt() }
fn size_sq_2d(v: Vec3) -> f32 { v.x * v.x + v.y * v.y }
fn nearly_zero(v: Vec3, tol: f32) -> bool { v.x.abs() <= tol && v.y.abs() <= tol && v.z.abs() <= tol }
fn clamped_max(v: Vec3, max: f32) -> Vec3 { let l2 = v.length_squared(); if l2 > max * max && l2 > 1e-8 { if max < KINDA_SMALL_NUMBER { Vec3::ZERO } else { v * (max / l2.sqrt()) } } else { v } }
fn plane_project(v: Vec3, n: Vec3) -> Vec3 { v - n * v.dot(n) }
fn normals_equal(a: Vec3, b: Vec3) -> bool { (a - b).abs().max_element() <= KINDA_SMALL_NUMBER }

#[derive(Clone, Debug)]
pub struct Cmc {
    pub p: Params,
    pub scene: Scene,
    /// Capsule centre (UpdatedComponent location), cm.
    pub loc: Vec3,
    pub velocity: Vec3,
    pub acceleration: Vec3,
    pub mode: Mode,
    pub floor: Floor,
    pub half_height: f32,
    // ACharacter state
    pub is_crouched: bool,
    pub wants_crouch: bool,
    pub pressed_jump: bool,
    pub jump_key_hold_time: f32,
    pub was_jumping: bool,
    pub jump_current_count: u32,
    /// `UFortMovementComp_CharacterAthena` +0x76c / +0x770: consecutive-jump index and the seconds left of its window (decays by the pawn tick delta).
    pub jump_chain_index: usize,
    pub jump_chain_window_remaining: f32,
    /// `UFortMovementComp_Character` +0x750: the pawn tick delta recorded by `AFortPawn::Tick` while `bPressedJump`.
    pub jump_frame_dt: f32,
    /// The DISPLAY frame delta, for a caller that simulates at a fixed rate lower than its frame rate (the 30 Hz tick lock): `AFortPawn::Tick` receives the frame delta in the
    /// real game, so the `DoJump` frame-delta scale must see the frame time, not the tick length. `None` (default) = the `dt` passed to [`Cmc::tick`] (one tick = one frame).
    pub frame_dt_override: Option<f32>,
    // input flags that Fortnite's GetMaxSpeed reads
    pub wants_sprint: bool,
    /// Unit horizontal facing of the pawn (UE X/Y), for the directional speed curve.
    pub facing: Vec3,
    pub analog_input_modifier: f32,
    crouch_maintains_base_location: bool,
    force_next_floor_check: bool,
    just_teleported: bool,
    has_base: bool,
    last_update_location: Vec3,
    impact_time_slice: f32,
    // event counters (tests / animation hooks)
    pub landed: u32,
    pub jumped: u32,
    pub fell_off_ledge: u32,
    pub air: crate::skydive::AerialState,
    pub pending_launch: Vec3,
    pub ext: crate::enhanced::ExtMultipliers,
    pub slide: Option<crate::enhanced::SlideRun>,
    pub physics_volume_fluid_friction: f32,
}

impl Cmc {
    /// Spawns the pawn standing with its feet at `feet`; walking if a floor is found, falling otherwise.
    pub fn new(p: Params, scene: Scene, feet: Vec3) -> Self {
        let hh = p.stand_half_height;
        let mut c = Cmc { p, scene, loc: feet + Vec3::new(0., 0., hh), velocity: Vec3::ZERO, acceleration: Vec3::ZERO, mode: Mode::Falling, floor: Floor::default(), half_height: hh,
            is_crouched: false, wants_crouch: false, pressed_jump: false, jump_key_hold_time: 0., was_jumping: false, jump_current_count: 0,
            jump_chain_index: 0, jump_chain_window_remaining: 0., jump_frame_dt: 0., frame_dt_override: None,
            wants_sprint: false, facing: Vec3::new(0., -1., 0.), analog_input_modifier: 0., crouch_maintains_base_location: false, force_next_floor_check: true, just_teleported: false, has_base: false,
            last_update_location: Vec3::ZERO, impact_time_slice: 0., landed: 0, jumped: 0, fell_off_ledge: 0, air: Default::default(), pending_launch: Vec3::ZERO, ext: Default::default(), slide: None, physics_volume_fluid_friction: 1. };
        let f = c.find_floor(c.loc, false, None);
        if f.is_walkable_floor() { c.mode = Mode::Walking; c.crouch_maintains_base_location = true; c.floor = f; c.adjust_floor_height(); c.has_base = true; }
        c.last_update_location = c.loc;
        c
    }
    pub fn capsule(&self) -> Capsule { Capsule { c: self.loc, r: self.p.radius, hh: self.half_height } }
    pub fn clamber_move(&mut self, loc: Vec3) { self.loc = loc; self.velocity = Vec3::ZERO; self.acceleration = Vec3::ZERO; self.set_movement_mode(Mode::Falling); self.last_update_location = loc; }
    pub fn clamber_finish(&mut self) {
        self.velocity = Vec3::ZERO;
        let f = self.find_floor(self.loc, false, None);
        if f.is_walkable_floor() { self.set_movement_mode(Mode::Walking); } else { self.set_movement_mode(Mode::Falling); }
        self.last_update_location = self.loc;
    }
    fn capsule_at(&self, c: Vec3, r: f32, hh: f32) -> Capsule { Capsule { c, r, hh } }
    pub fn feet_z(&self) -> f32 { self.loc.z - self.half_height }
    pub fn is_falling(&self) -> bool { self.mode == Mode::Falling }
    pub fn is_moving_on_ground(&self) -> bool { self.mode == Mode::Walking }
    pub fn is_crouching(&self) -> bool { self.is_crouched }
    /// `UCharacterMovementComponent::GetGravityZ` (`UMovementComponent` world gravity x GravityScale) with Fortnite's `GravityZScale` attribute (1.0, exe).
    pub fn gravity_z(&self) -> f32 { self.p.gravity_z() }
    pub fn walkable_floor_z(&self) -> f32 { self.p.walkable_floor_z }

    // =====================================================================================================================
    // ACharacter jump state
    // =====================================================================================================================
    /// `ACharacter::Jump` (key pressed).
    /// UE4.16 Character.cpp:956-960.
    pub fn jump(&mut self) { self.pressed_jump = true; self.jump_key_hold_time = 0.; }
    /// `ACharacter::StopJumping` (key released), Character.cpp:962-966.
    pub fn stop_jumping(&mut self) { self.pressed_jump = false; self.reset_jump_state(); }
    /// `ACharacter::ResetJumpState`, Character.cpp:302-311 (it does NOT clear `bPressedJump`).
    pub fn reset_jump_state(&mut self) {
        self.was_jumping = false; self.jump_key_hold_time = 0.;
        if !self.is_falling() { self.jump_current_count = 0; }
    }
    /// `ACharacter::IsJumpProvidingForce`, Character.cpp:317-320 (`GetJumpMaxHoldTime()` is `JumpMaxHoldTime`, Character.cpp:1022-1025).
    pub fn is_jump_providing_force(&self) -> bool {
        if self.p.jump_input_1p8 { return self.pressed_jump && 0. < self.jump_key_hold_time && self.jump_key_hold_time < self.p.jump_max_hold_time; }
        self.pressed_jump && self.jump_key_hold_time < self.p.jump_max_hold_time
    }
    /// `UCharacterMovementComponent::IsJumpAllowed` = `NavAgentProps.bCanJump` (default true; the pawn CDO serializes only AgentHeight).
    pub fn is_jump_allowed(&self) -> bool { true }
    /// `ACharacter::CanJumpInternal_Implementation`, Character.cpp:262-300, line for line.
    pub fn can_jump(&self) -> bool {
        // 265-272
        let mut can = !self.is_crouched && self.is_jump_allowed() && !self.wants_crouch && (self.is_moving_on_ground() || self.is_falling());
        if can {
            // 277-296
            if self.p.jump_max_hold_time <= 0. || !self.was_jumping {
                can = if self.jump_current_count == 0 && self.is_falling() { self.jump_current_count + 1 < self.p.jump_max_count } else { self.jump_current_count < self.p.jump_max_count };
            } else {
                can = self.is_jump_providing_force() && (self.jump_current_count < self.p.jump_max_count || (self.was_jumping && self.jump_current_count == self.p.jump_max_count));
            }
        }
        can
    }
    /// `UCharacterMovementComponent::DoJump` (exe 0x2078570) with an explicit `JumpZVelocity`: `Velocity.Z = JumpZVelocity; SetMovementMode(MOVE_Falling)`.
    fn base_do_jump(&mut self, jump_z: f32) -> bool {
        if self.can_jump() { self.velocity.z = jump_z; self.set_movement_mode(Mode::Falling); return true; }
        false
    }
    /// `UFortMovementComp_Character::DoJump` (exe 0x6d4f20): JumpZVelocity x [`mp::jump_frame_scale`] of the pawn tick delta recorded this frame, then the base DoJump.
    fn fort_do_jump(&mut self, jump_z: f32) -> bool {
        let mut z = jump_z;
        if self.p.jump_frame_time_boost && self.jump_frame_dt > mp::JUMP_FRAME_DT_THRESHOLD {
            if self.p.jump_frame_dt_clamp { self.jump_frame_dt = self.jump_frame_dt.min(mp::JUMP_FRAME_DT_CLAMP); }
            z *= mp::jump_frame_scale(self.jump_frame_dt);
        }
        self.base_do_jump(z)
    }
    /// `UFortMovementComp_CharacterAthena::DoJump` (exe 0x421d20): the consecutive-jump table, the Fort frame-delta scale, the base jump, then on success the window / index are
    /// stored and the horizontal speed is clamped to `style speed x table.1` (ASSUMED: the style speed of vtable 0x888 is this port's [`Cmc::style_max_speed`]).
    pub fn do_jump(&mut self) -> bool {
        let n = self.p.jump_chain_table.len();
        if n == 0 { return self.fort_do_jump(self.p.jump_z_velocity * self.ext.jump_z); }
        let held = self.p.jump_input_1p8 && self.was_jumping;
        let idx = if held { self.jump_chain_index.min(n - 1) } else if self.jump_chain_window_remaining > 0. { (self.jump_chain_index + 1).min(n - 1) } else { 0 };
        let (z_scale, speed_scale) = self.p.jump_chain_table[idx];
        let ok = self.fort_do_jump(self.p.jump_z_velocity * self.ext.jump_z * z_scale);
        if ok && !held {
            self.jump_chain_window_remaining = self.p.jump_chain_window; self.jump_chain_index = idx;
            let limit = self.style_max_speed() * speed_scale;
            if limit < KINDA_SMALL_NUMBER { self.velocity.x = 0.; self.velocity.y = 0.; }
            else if size_sq_2d(self.velocity) > limit * limit { let k = limit / size_2d(self.velocity); self.velocity.x *= k; self.velocity.y *= k; }
        }
        ok
    }
    /// `ACharacter::CheckJumpInput`.
    /// `ACharacter::CheckJumpInput`, Character.cpp:968-1010 (called from `TickComponent` before the acceleration is built, CharacterMovementComponent.cpp:1149).
    pub fn check_jump_input(&mut self, dt: f32) {
        if self.aerial_check_jump_input() { return; }
        if self.p.jump_input_1p8 {
            if self.pressed_jump {
                self.jump_key_hold_time += dt;
                let did_jump = self.can_jump() && self.do_jump();
                if !self.was_jumping && did_jump { self.jump_current_count += 1; self.jumped += 1; }
                self.was_jumping = did_jump;
            } else if self.was_jumping && !self.is_falling() {
                self.reset_jump_state();
            }
            return;
        }
        if self.pressed_jump {
            // 976-980: a first jump that starts while already falling counts as used
            let first_jump = self.jump_current_count == 0;
            if first_jump && self.is_falling() { self.jump_current_count += 1; }
            // 982: CanJump() && DoJump()
            let did_jump = self.can_jump() && self.do_jump();
            if did_jump {
                if !self.was_jumping { self.jump_current_count += 1; self.jumped += 1; } // 986-990 (OnJumped)
                if !first_jump { self.jump_key_hold_time += dt; } // 994-997
            }
            self.was_jumping = did_jump; // 1000
        } else if self.was_jumping && !self.is_falling() {
            self.reset_jump_state(); // 1005-1008
        }
    }
    /// `ACharacter::ClearJumpInput`, Character.cpp:1013-1020; `PerformMovement` calls it right BEFORE `StartNewPhysics` (CharacterMovementComponent.cpp:2131).
    pub fn clear_jump_input(&mut self) {
        if self.pressed_jump && self.jump_key_hold_time >= self.p.jump_max_hold_time { self.pressed_jump = false; }
    }

    // =====================================================================================================================
    // Tick / PerformMovement
    // =====================================================================================================================
    /// `TickComponent` -> `ControlledCharacterMove` for a locally controlled pawn. `input` is the movement input vector (X, Y), length <= 1.
    pub fn tick(&mut self, input: Vec2, dt: f32) {
        if dt <= 0. { return; }
        self.pawn_tick(dt);
        // CharacterMovementComponent.cpp:1149 then 1152-1153, then PerformMovement (which calls ClearJumpInput itself, :2131)
        self.check_jump_input(dt);
        // ConstrainInputAcceleration + ScaleInputAcceleration
        self.acceleration = if self.in_aerial_mode() { clamped_max(self.aerial_constrained_input(), 1.) * self.get_max_acceleration() } else { clamped_max(Vec3::new(input.x, input.y, 0.), 1.) * self.ground_max_acceleration() };
        self.air.accel_snapshot = self.acceleration;
        self.analog_input_modifier = self.compute_analog_input_modifier();
        self.perform_movement(dt);
    }
    /// The pawn actor tick that runs before the movement component's tick: `AFortPlayerPawnAthena::Tick` (exe 0x41f900) decays the consecutive-jump window by the delta
    /// (`max(0, window - dt)`), `AFortPawn::Tick` (0x7d0b00) hands the delta to the movement component (0x6d24a0) while `bPressedJump` is set.
    fn pawn_tick(&mut self, dt: f32) {
        self.jump_chain_window_remaining = (self.jump_chain_window_remaining - dt).max(0.);
        if self.pressed_jump { self.jump_frame_dt = self.frame_dt_override.unwrap_or(dt); }
    }
    /// `ComputeAnalogInputModifier`.
    fn compute_analog_input_modifier(&self) -> f32 {
        if self.acceleration.length_squared() > 0. && self.ground_max_acceleration() > 1e-8 { (self.acceleration.length() / self.ground_max_acceleration()).clamp(0., 1.) } else { 0. }
    }
    /// `PerformMovement`.
    pub fn perform_movement(&mut self, dt: f32) {
        self.force_next_floor_check |= self.is_moving_on_ground() && self.loc != self.last_update_location;
        self.update_character_state_before_movement();
        self.handle_pending_launch();
        self.clear_jump_input(); // CharacterMovementComponent.cpp:2131
        self.start_new_physics(dt, 0);
        // uncrouch if no longer allowed to be crouched
        if self.is_crouching() && !self.can_crouch_in_current_state() { self.un_crouch(); }
        self.update_character_state_after_movement();
        self.last_update_location = self.loc;
    }
    fn update_character_state_before_movement(&mut self) {
        let crouching = self.is_crouching();
        if crouching && (!self.wants_crouch || !self.can_crouch_in_current_state()) { self.un_crouch(); }
        else if !crouching && self.wants_crouch && self.can_crouch_in_current_state() { self.crouch(); }
    }
    fn update_character_state_after_movement(&mut self) {
        if self.is_crouching() && !self.can_crouch_in_current_state() { self.un_crouch(); }
    }
    /// `CanCrouchInCurrentState`: UE4.16 CharacterMovementComponent.cpp:2309-2316 = `CanEverCrouch && (IsFalling || IsMovingOnGround)`; the 1.8 exe base function (0x20702e0) is the same
    /// and `UFortMovementComp_CharacterAthena` (0x41ec50) only adds `&& !<skydive/parachute state>`, which this port never enters. So a pawn CAN crouch in the air.
    pub fn can_crouch_in_current_state(&self) -> bool { (self.is_moving_on_ground() || self.is_falling()) && !self.in_aerial_mode() && !self.is_skydiving() }
    pub fn falling_lateral_friction(&self) -> f32 { match self.air_control_params() { Some(a) => a.lateral_friction, None => self.p.falling_lateral_friction } }
    /// `StartNewPhysics`.
    fn start_new_physics(&mut self, dt: f32, iterations: u32) {
        if dt < MIN_TICK_TIME || iterations >= self.p.max_simulation_iterations { return; }
        match self.mode { Mode::Walking => self.phys_walking(dt, iterations), Mode::Falling => self.phys_falling(dt, iterations) }
    }
    /// `SetMovementMode` + `OnMovementModeChanged` (CMC) + `ACharacter::OnMovementModeChanged`.
    fn set_movement_mode(&mut self, new: Mode) {
        if self.mode == new { return; }
        self.mode = new;
        if new == Mode::Walking {
            self.velocity.z = 0.; self.crouch_maintains_base_location = true;
            self.floor = self.find_floor(self.loc, false, None);
            self.adjust_floor_height();
            self.has_base = self.floor.is_walkable_floor();
        } else { self.floor = Floor::default(); self.crouch_maintains_base_location = false; self.has_base = false; }
        // ACharacter::OnMovementModeChanged, Character.cpp:929-934
        if !self.pressed_jump { self.reset_jump_state(); }
    }
    /// `GetSimulationTimeStep`.
    fn get_simulation_time_step(&self, mut remaining: f32, iterations: u32) -> f32 {
        if remaining > self.p.max_simulation_time_step && iterations < self.p.max_simulation_iterations { remaining = self.p.max_simulation_time_step.min(remaining * 0.5); }
        remaining.max(MIN_TICK_TIME)
    }

    // =====================================================================================================================
    // Speed / acceleration (Fortnite's GetMaxSpeed reads the pawn stats; the mapping stat -> mode is ASSUMED, the values are recovered)
    // =====================================================================================================================
    pub fn get_max_acceleration(&self) -> f32 { match self.air_control_params() { Some(a) => a.max_acceleration, None => self.ground_max_acceleration() } }
    pub fn ground_max_acceleration(&self) -> f32 { self.p.max_acceleration * self.ext.accel }
    /// `GetMaxSpeed`: crouched run, sprint or run stat, scaled by the recovered directional curve (angle between the acceleration and the facing).
    pub fn get_max_speed(&self) -> f32 {
        if let Some(a) = self.air_control_params() { return self.aerial_max_speed(&a); }
        if let Some(cap) = self.slide_max_speed() { return cap; }
        let mut speed = self.style_max_speed() * self.physics_volume_fluid_friction;
        // `UFortMovementComp_CharacterAthena::GetMaxSpeed` (exe 0x4343e0, `cmp byte [rbx+0x1a8],3` = MOVE_Falling): while the consecutive-jump window is open the speed is
        // multiplied by the current table entry's second value.
        if self.mode == Mode::Falling && self.jump_chain_window_remaining > 0. && self.jump_chain_index < self.p.jump_chain_table.len() { speed *= self.p.jump_chain_table[self.jump_chain_index].1; }
        speed *= self.ext.speed;
        // (the analog scaling is `CalcVelocity`'s, CharacterMovementComponent.cpp:2934, not `GetMaxSpeed`'s)
        speed
    }
    /// The movement-style speed (crouched run / sprint / run stat) times the recovered directional curve: what `UFortMovementComp_Character::GetMaxSpeed` (0x6d8fe0, vtable
    /// 0x888 style speed) feeds into `CalcVelocity`. The stat -> style mapping is ASSUMED (see the module docs), the values are recovered.
    pub fn style_max_speed(&self) -> f32 {
        let sprint = self.wants_sprint && !self.is_crouched && self.acceleration.length_squared() > 0.;
        let base = if self.is_crouched { if sprint { self.p.speed_crouched_sprint } else { self.p.speed_crouched_run } } else if sprint { self.p.speed_sprint } else { self.p.speed_run };
        let mut speed = base;
        if self.p.jump_input_1p8 {
            if self.p.directional_speed && self.facing.length_squared() > 0.5 {
                let cos = safe_normal_2d(self.velocity).dot(self.facing).clamp(-1., 1.);
                speed *= mp::directional_multiplier(cos.acos().to_degrees());
            }
            return speed;
        }
        if self.p.directional_speed && self.acceleration.length_squared() > 0. && self.facing.length_squared() > 0.5 {
            let a = safe_normal_2d(self.acceleration); let cos = a.dot(self.facing).clamp(-1., 1.);
            speed *= mp::directional_multiplier(cos.acos().to_degrees());
        }
        speed
    }
    fn is_exceeding_max_speed(&self, max_speed: f32) -> bool { let m = max_speed.max(0.); self.velocity.length_squared() > m * m * 1.01 }

    /// `CalcVelocity`.
    fn calc_velocity(&mut self, dt: f32, friction: f32, fluid: bool, braking_deceleration: f32) {
        if dt < MIN_TICK_TIME { return; }
        let friction = friction.max(0.);
        // CharacterMovementComponent.cpp:2934 (RequestedSpeed is 0: no path following here); a full keyboard input has AnalogInputModifier 1
        let max_speed = (self.get_max_speed() * self.analog_input_modifier).max(if self.in_aerial_mode() { 0. } else { self.p.min_analog_walk_speed }).max(0.);
        let zero_accel = self.acceleration == Vec3::ZERO;
        let over_max = self.is_exceeding_max_speed(max_speed);
        if zero_accel || over_max {
            let old = self.velocity;
            let actual = if self.p.use_separate_braking_friction { self.p.braking_friction } else { friction };
            self.apply_velocity_braking(dt, actual, braking_deceleration);
            if over_max && self.velocity.length_squared() < max_speed * max_speed && self.acceleration.dot(old) > 0. { self.velocity = safe_normal(old) * max_speed; }
        } else {
            let accel_dir = safe_normal(self.acceleration); let vel_size = self.velocity.length();
            self.velocity = self.velocity - (self.velocity - accel_dir * vel_size) * (dt * friction).min(1.);
        }
        if fluid { self.velocity *= 1. - (friction * dt).min(1.); }
        let new_max = if self.is_exceeding_max_speed(max_speed) { self.velocity.length() } else { max_speed };
        self.velocity += self.acceleration * dt;
        self.velocity = clamped_max(self.velocity, new_max);
    }
    /// `ApplyVelocityBraking`.
    fn apply_velocity_braking(&mut self, dt: f32, friction: f32, braking_deceleration: f32) {
        if self.velocity == Vec3::ZERO || dt < MIN_TICK_TIME { return; }
        let friction = (friction * self.p.braking_friction_factor.max(0.)).max(0.);
        let braking = braking_deceleration.max(0.);
        let zero_friction = friction == 0.; let zero_braking = braking == 0.;
        if zero_friction && zero_braking { return; }
        let old = self.velocity;
        let mut remaining = dt;
        let max_step = self.p.braking_sub_step_time.clamp(1. / 75., 1. / 20.);
        let rev_accel = if zero_braking { Vec3::ZERO } else { -braking * safe_normal(self.velocity) };
        while remaining >= MIN_TICK_TIME {
            let step = if remaining > max_step && !zero_friction { max_step.min(remaining * 0.5) } else { remaining };
            remaining -= step;
            self.velocity += (-friction * self.velocity + rev_accel) * step;
            if self.velocity.dot(old) <= 0. { self.velocity = Vec3::ZERO; return; }
        }
        let v2 = self.velocity.length_squared();
        if v2 <= KINDA_SMALL_NUMBER || (!zero_braking && v2 <= BRAKE_TO_STOP_VELOCITY * BRAKE_TO_STOP_VELOCITY) { self.velocity = Vec3::ZERO; }
    }
    /// `MaintainHorizontalGroundVelocity` (bMaintainHorizontalGroundVelocity = true).
    fn maintain_horizontal_ground_velocity(&mut self) { if self.velocity.z != 0. { self.velocity.z = 0.; } }

    // =====================================================================================================================
    // Collision helpers (UWorld::Sweep/Overlap are `Scene`)
    // =====================================================================================================================
    /// `UMovementComponent::MoveUpdatedComponent` with sweep: moves as far as the capsule can go and reports the hit.
    fn move_updated(&mut self, delta: Vec3, sweep: bool) -> Hit {
        if !sweep { self.loc += delta; return Hit::none(); }
        let h = self.scene.sweep(self.capsule(), delta);
        if h.blocking { self.loc = h.location; } else { self.loc += delta; }
        h
    }
    /// `GetPenetrationAdjustment` (CMC clamps to MaxDepenetrationWithGeometry).
    fn get_penetration_adjustment(&self, hit: &Hit) -> Vec3 {
        let adj = hit.normal * (hit.depth + PENETRATION_PULLBACK);
        clamped_max(adj, self.p.max_depenetration_with_geometry)
    }
    /// `ResolvePenetration`.
    fn resolve_penetration(&mut self, adjustment: Vec3, hit: &Hit) -> bool {
        if adjustment == Vec3::ZERO { return false; }
        let start = hit.trace_start;
        let infl = Capsule { c: start + adjustment, r: self.p.radius + PENETRATION_OVERLAP_INFLATION, hh: self.half_height + PENETRATION_OVERLAP_INFLATION };
        if !self.scene.overlaps(&infl) { self.loc += adjustment; return true; }
        let mut out = self.move_updated(adjustment, true);
        let mut moved = out.time > 0. || !out.blocking;
        if !moved && out.start_penetrating {
            let second = self.get_penetration_adjustment(&out); let combined = adjustment + second;
            if second != adjustment && combined != Vec3::ZERO { out = self.move_updated(combined, true); moved = out.time > 0. || !out.blocking; }
        }
        if !moved && out.start_penetrating {
            let move_delta = hit.trace_end - hit.trace_start;
            if move_delta != Vec3::ZERO { out = self.move_updated(adjustment + move_delta, true); moved = out.time > 0. || !out.blocking; }
        }
        moved
    }
    /// `SafeMoveUpdatedComponent`.
    fn safe_move(&mut self, delta: Vec3) -> Hit {
        let mut hit = self.move_updated(delta, true);
        if hit.start_penetrating {
            let adj = self.get_penetration_adjustment(&hit);
            if self.resolve_penetration(adj, &hit) { hit = self.move_updated(delta, true); }
        }
        hit
    }
    /// `IsWithinEdgeTolerance`.
    fn is_within_edge_tolerance(&self, capsule_loc: Vec3, impact: Vec3, radius: f32) -> bool {
        let d2 = size_sq_2d(impact - capsule_loc); let r = (SWEEP_EDGE_REJECT_DISTANCE + KINDA_SMALL_NUMBER).max(radius - SWEEP_EDGE_REJECT_DISTANCE);
        d2 < r * r
    }
    /// `IsWalkable`.
    pub fn is_walkable(&self, h: &Hit) -> bool {
        if !h.valid_blocking() { return false; }
        if h.impact_normal.z < KINDA_SMALL_NUMBER { return false; }
        h.impact_normal.z >= self.p.walkable_floor_z
    }
    /// `CanStepUp`.
    fn can_step_up(&self, h: &Hit) -> bool { h.valid_blocking() && self.mode != Mode::Falling }

    // =====================================================================================================================
    // Floor finding
    // =====================================================================================================================
    /// `FindFloor` (bAlwaysCheckFloor = true: the cached-location shortcut is never taken).
    pub fn find_floor(&self, capsule_loc: Vec3, _can_use_cached: bool, downward: Option<&Hit>) -> Floor {
        let height_check_adjust = if self.is_moving_on_ground() { MAX_FLOOR_DIST + KINDA_SMALL_NUMBER } else { -MAX_FLOOR_DIST };
        let sweep_dist = MAX_FLOOR_DIST.max(self.p.max_step_height + height_check_adjust);
        let line_dist = sweep_dist;
        let mut out = Floor::default();
        if line_dist > 0. || sweep_dist > 0. { out = self.compute_floor_dist(capsule_loc, line_dist, sweep_dist, self.p.radius, downward); }
        // "perch"
        if out.blocking_hit && !out.line_trace && self.should_compute_perch_result(&out.hit, true) {
            let mut max_perch = MAX_FLOOR_DIST.max(self.p.max_step_height + height_check_adjust);
            if self.is_moving_on_ground() { max_perch += self.p.perch_additional_height.max(0.); }
            if let Some(perch) = self.compute_perch_result(self.valid_perch_radius(), &out.hit, max_perch) {
                let avg = (MIN_FLOOR_DIST + MAX_FLOOR_DIST) * 0.5;
                let move_up = avg - out.floor_dist;
                if move_up + perch.floor_dist >= max_perch { out.floor_dist = avg; }
                if !out.walkable { let fd = out.floor_dist; out.set_from_line_trace(perch.hit, fd, fd.max(perch.floor_dist), true); }
            } else { out.walkable = false; }
        }
        out
    }
    fn valid_perch_radius(&self) -> f32 { self.p.perch_radius_threshold.clamp(0.11, self.p.radius) }
    fn should_compute_perch_result(&self, hit: &Hit, check_radius: bool) -> bool {
        if !hit.valid_blocking() { return false; }
        if self.p.perch_radius_threshold <= SWEEP_EDGE_REJECT_DISTANCE { return false; }
        if check_radius { let d2 = size_sq_2d(hit.impact_point - hit.location); let r = self.valid_perch_radius(); if d2 <= r * r { return false; } }
        true
    }
    /// `ComputePerchResult`.
    fn compute_perch_result(&self, test_radius: f32, hit: &Hit, max_floor_dist: f32) -> Option<Floor> {
        if max_floor_dist <= 0. { return None; }
        let in_hit_above_base = (hit.impact_point.z - (hit.location.z - self.half_height)).max(0.);
        let perch_line_dist = (max_floor_dist - in_hit_above_base).max(0.);
        let perch_sweep_dist = max_floor_dist.max(0.);
        let actual_sweep = perch_sweep_dist + self.p.radius;
        let out = self.compute_floor_dist(hit.location, perch_line_dist, actual_sweep, test_radius, None);
        if !out.is_walkable_floor() { return None; }
        if in_hit_above_base + out.floor_dist > max_floor_dist { return None; } // hit something past the max distance: not walkable
        Some(out)
    }
    /// `ComputeFloorDist`.
    pub fn compute_floor_dist(&self, capsule_loc: Vec3, line_distance: f32, sweep_distance: f32, sweep_radius: f32, downward: Option<&Hit>) -> Floor {
        let mut out = Floor::default();
        let (pawn_radius, pawn_hh) = (self.p.radius, self.half_height);
        let mut skip_sweep = false;
        if let Some(d) = downward {
            if d.valid_blocking() && d.trace_start.z > d.trace_end.z && size_sq_2d(d.trace_start - d.trace_end) <= 1e-8 && self.is_within_edge_tolerance(d.location, d.impact_point, pawn_radius) {
                skip_sweep = true;
                let walkable = self.is_walkable(d); let dist = capsule_loc.z - d.location.z;
                out.set_from_sweep(*d, dist, walkable);
                if walkable { return out; }
            }
        }
        if sweep_distance < line_distance { return out; }
        let mut blocking = false;
        if !skip_sweep && sweep_distance > 0. && sweep_radius > 0. {
            let shrink_scale = 0.9; let shrink_scale_overlap = 0.1;
            let mut shrink_h = (pawn_hh - pawn_radius) * (1. - shrink_scale);
            let mut trace_dist = sweep_distance + shrink_h;
            let (mut r, mut hh) = (sweep_radius, pawn_hh - shrink_h);
            let mut hit = self.scene.sweep(self.capsule_at(capsule_loc, r, hh), Vec3::new(0., 0., -trace_dist));
            blocking = hit.blocking;
            if blocking {
                if hit.start_penetrating || !self.is_within_edge_tolerance(capsule_loc, hit.impact_point, r) {
                    shrink_h = (pawn_hh - pawn_radius) * (1. - shrink_scale_overlap);
                    trace_dist = sweep_distance + shrink_h;
                    r = (r - SWEEP_EDGE_REJECT_DISTANCE - KINDA_SMALL_NUMBER).max(0.);
                    hh = (pawn_hh - shrink_h).max(r);
                    hit = self.scene.sweep(self.capsule_at(capsule_loc, r, hh), Vec3::new(0., 0., -trace_dist));
                    blocking = hit.blocking;
                }
                let max_pen = MAX_FLOOR_DIST.max(pawn_radius);
                let sweep_result = (hit.time * trace_dist - shrink_h).max(-max_pen);
                out.set_from_sweep(hit, sweep_result, false);
                if hit.valid_blocking() && self.is_walkable(&hit) && sweep_result <= sweep_distance { out.walkable = true; return out; }
            }
        }
        // no sweep hit and not stuck: no floor within the sweep distance
        if !out.blocking_hit && !out.hit.start_penetrating { out.floor_dist = sweep_distance; return out; }
        let _ = blocking;
        // line trace
        if line_distance > 0. {
            let shrink_h = pawn_hh; let trace_dist = line_distance + shrink_h;
            if let Some((d, pt, n)) = self.scene.line_down(capsule_loc, trace_dist) {
                let time = d / trace_dist;
                if time > 0. {
                    let max_pen = MAX_FLOOR_DIST.max(pawn_radius);
                    let line_result = (time * trace_dist - shrink_h).max(-max_pen);
                    out.blocking_hit = true;
                    let lh = Hit { blocking: true, start_penetrating: false, time, depth: 0., location: capsule_loc - Vec3::new(0., 0., d), impact_point: pt, normal: n, impact_normal: n, trace_start: capsule_loc, trace_end: capsule_loc - Vec3::new(0., 0., trace_dist) };
                    if line_result <= line_distance && self.is_walkable(&lh) { let fd = out.floor_dist; out.set_from_line_trace(lh, fd, line_result, true); return out; }
                }
            }
        }
        out.walkable = false; out.floor_dist = sweep_distance;
        out
    }
    /// `AdjustFloorHeight`.
    fn adjust_floor_height(&mut self) {
        if !self.floor.is_walkable_floor() { return; }
        let mut old_dist = self.floor.floor_dist;
        if self.floor.line_trace {
            if old_dist < MIN_FLOOR_DIST && self.floor.line_dist >= MIN_FLOOR_DIST { return; } else { old_dist = self.floor.line_dist; }
        }
        if old_dist < MIN_FLOOR_DIST || old_dist > MAX_FLOOR_DIST {
            let initial_z = self.loc.z; let avg = (MIN_FLOOR_DIST + MAX_FLOOR_DIST) * 0.5; let move_dist = avg - old_dist;
            let adjust = self.safe_move(Vec3::new(0., 0., move_dist));
            if !adjust.valid_blocking() { self.floor.floor_dist += move_dist; }
            else if move_dist > 0. { self.floor.floor_dist += self.loc.z - initial_z; }
            else {
                self.floor.floor_dist = self.loc.z - adjust.location.z;
                if self.is_walkable(&adjust) { let fd = self.floor.floor_dist; self.floor.set_from_sweep(adjust, fd, true); }
            }
            // CharacterMovementComponent.cpp:5092 also has `|| OldFloorDist < 0` (moved out of penetration). KNOWN DEVIATION kept: with the port's hull scene a wall-pressed pawn reports
            // negative floor distances every tick, and treating that as a teleport stops `Velocity` from reflecting the blocked move (fn-movement scene::hull_tests fails).
            self.just_teleported |= false;
            self.force_next_floor_check = true;
        }
    }

    // =====================================================================================================================
    // PhysWalking
    // =====================================================================================================================
    fn phys_walking(&mut self, dt: f32, mut iterations: u32) {
        if dt < MIN_TICK_TIME { return; }
        self.just_teleported = false;
        let mut checked_fall = false; let mut tried_ledge_move = false;
        let mut remaining = dt;
        while remaining >= MIN_TICK_TIME && iterations < self.p.max_simulation_iterations {
            iterations += 1; self.just_teleported = false;
            let time_tick = self.get_simulation_time_step(remaining, iterations);
            remaining -= time_tick;
            let old_has_base = self.has_base; let old_location = self.loc; let old_floor = self.floor;
            self.maintain_horizontal_ground_velocity();
            self.acceleration.z = 0.;
            if self.slide.is_some() { let accel_in = self.acceleration; let (friction, braking) = self.slide_pre_iteration(time_tick); self.calc_velocity(time_tick, friction, false, braking); self.acceleration = accel_in; }
            else { self.calc_velocity(time_tick, self.p.ground_friction, false, self.p.braking_deceleration_walking); }
            if self.is_falling() { self.start_new_physics(remaining + time_tick, iterations - 1); return; }
            let move_velocity = self.velocity; let delta = time_tick * move_velocity; let zero_delta = nearly_zero(delta, KINDA_SMALL_NUMBER);
            let mut step_down = StepDown::default();
            if zero_delta { remaining = 0.; }
            else {
                self.move_along_floor(move_velocity, time_tick, &mut step_down);
                if self.is_falling() {
                    let desired = delta.length();
                    if desired > KINDA_SMALL_NUMBER { let actual = size_2d(self.loc - old_location); remaining += time_tick * (1. - (actual / desired).min(1.)); }
                    self.start_new_physics(remaining, iterations); return;
                }
            }
            // Update floor. StepUp might have already done it for us.
            if step_down.computed { self.floor = step_down.floor; } else { self.floor = self.find_floor(self.loc, zero_delta, None); }
            // check for ledges here
            let check_ledges = !self.can_walk_off_ledges();
            if check_ledges && !self.floor.is_walkable_floor() {
                let new_delta = if tried_ledge_move { Vec3::ZERO } else { self.get_ledge_move(old_location, delta, Vec3::new(0., 0., -1.)) };
                if new_delta != Vec3::ZERO {
                    self.revert_move(old_location, old_has_base, old_floor, false);
                    tried_ledge_move = true; self.velocity = new_delta / time_tick; remaining += time_tick; continue;
                } else {
                    let must_jump = zero_delta || !old_has_base;
                    if (must_jump || !checked_fall) && self.check_fall(&old_floor, delta, old_location, remaining, time_tick, iterations, must_jump) { return; }
                    self.revert_move(old_location, old_has_base, old_floor, true); break;
                }
            } else {
                if self.floor.is_walkable_floor() {
                    // ShouldCatchAir is false in the base component
                    self.adjust_floor_height(); self.has_base = true;
                } else if self.floor.hit.start_penetrating && remaining <= 0. {
                    let mut hit = self.floor.hit; hit.trace_end = hit.trace_start + Vec3::new(0., 0., MAX_FLOOR_DIST);
                    let adj = self.get_penetration_adjustment(&hit);
                    self.resolve_penetration(adj, &hit); self.force_next_floor_check = true;
                }
                if !self.floor.is_walkable_floor() && !self.floor.hit.start_penetrating {
                    let must_jump = self.just_teleported || zero_delta || !old_has_base;
                    if (must_jump || !checked_fall) && self.check_fall(&old_floor, delta, old_location, remaining, time_tick, iterations, must_jump) { return; }
                    checked_fall = true;
                }
            }
            if self.is_moving_on_ground() && !self.just_teleported && time_tick >= MIN_TICK_TIME { self.velocity = (self.loc - old_location) / time_tick; }
            if self.slide.is_some() { self.slide_post_iteration(); }
            if self.loc == old_location { break; } // no movement this iteration: future iterations would be stuck too
        }
        if self.is_moving_on_ground() { self.maintain_horizontal_ground_velocity(); }
    }
    /// `CanWalkOffLedges`.
    pub fn can_walk_off_ledges(&self) -> bool {
        if !self.p.can_walk_off_ledges_when_crouching && self.is_crouching() { return false; }
        self.p.can_walk_off_ledges
    }
    /// `RevertMove`.
    fn revert_move(&mut self, old_location: Vec3, old_has_base: bool, old_floor: Floor, fail_move: bool) {
        self.loc = old_location; self.just_teleported = false;
        if old_has_base { self.floor = old_floor; self.has_base = true; } else { self.floor = Floor::default(); self.has_base = false; }
        if fail_move { self.velocity = Vec3::ZERO; self.acceleration = Vec3::ZERO; }
    }
    /// `CheckFall`: walked off an edge -> `StartFalling`.
    fn check_fall(&mut self, _old_floor: &Floor, delta: Vec3, old_location: Vec3, remaining: f32, time_tick: f32, iterations: u32, must_jump: bool) -> bool {
        if must_jump || self.can_walk_off_ledges() {
            self.fell_off_ledge += 1;
            if self.is_moving_on_ground() { self.start_falling(iterations, remaining, time_tick, delta, old_location); }
            return true;
        }
        false
    }
    /// `StartFalling`.
    fn start_falling(&mut self, iterations: u32, remaining: f32, time_tick: f32, delta: Vec3, sub_loc: Vec3) {
        let desired = delta.length(); let actual = size_2d(self.loc - sub_loc);
        let remaining = if desired < KINDA_SMALL_NUMBER { 0. } else { remaining + time_tick * (1. - (actual / desired).min(1.)) };
        if self.is_moving_on_ground() { self.set_movement_mode(Mode::Falling); } else { self.force_next_floor_check = true; }
        self.start_new_physics(remaining, iterations);
    }
    /// `GetLedgeMove` / `CheckLedgeDirection`.
    fn get_ledge_move(&self, old_location: Vec3, delta: Vec3, grav_dir: Vec3) -> Vec3 {
        if delta == Vec3::ZERO { return Vec3::ZERO; }
        let mut side = Vec3::new(delta.y, -delta.x, 0.);
        if self.check_ledge_direction(old_location, side, grav_dir) { return side; }
        side = -side;
        if self.check_ledge_direction(old_location, side, grav_dir) { return side; }
        Vec3::ZERO
    }
    fn check_ledge_direction(&self, old_location: Vec3, side_step: Vec3, grav_dir: Vec3) -> bool {
        let dest = old_location + side_step;
        let cap = self.capsule_at(old_location, self.p.radius, self.half_height);
        let mut res = self.scene.sweep(cap, side_step);
        if !res.blocking || self.is_walkable(&res) {
            if !res.blocking { res = self.scene.sweep(self.capsule_at(dest, self.p.radius, self.half_height), grav_dir * (self.p.max_step_height + self.p.ledge_check_threshold)); }
            if res.blocking && self.is_walkable(&res) { return true; }
        }
        false
    }
    /// `ComputeGroundMovementDelta`.
    fn compute_ground_movement_delta(&self, delta: Vec3, ramp_hit: &Hit, hit_from_line_trace: bool) -> Vec3 {
        let floor_normal = ramp_hit.impact_normal; let contact_normal = ramp_hit.normal;
        if floor_normal.z < 1. - KINDA_SMALL_NUMBER && floor_normal.z > KINDA_SMALL_NUMBER && contact_normal.z > KINDA_SMALL_NUMBER && !hit_from_line_trace && self.is_walkable(ramp_hit) {
            let floor_dot_delta = floor_normal.dot(delta);
            return Vec3::new(delta.x, delta.y, -floor_dot_delta / floor_normal.z);
        }
        delta
    }
    /// `MoveAlongFloor`.
    fn move_along_floor(&mut self, in_velocity: Vec3, dt: f32, step_down: &mut StepDown) {
        if !self.floor.is_walkable_floor() { return; }
        let delta = Vec3::new(in_velocity.x, in_velocity.y, 0.) * dt;
        let mut ramp = self.compute_ground_movement_delta(delta, &self.floor.hit, self.floor.line_trace);
        let mut hit = self.safe_move(ramp);
        let mut last_slice = dt;
        if hit.start_penetrating {
            self.slide_along_surface(delta, 1., hit.normal, &mut hit);
        } else if hit.valid_blocking() {
            let mut percent_applied = hit.time;
            if hit.time > 0. && hit.normal.z > KINDA_SMALL_NUMBER && self.is_walkable(&hit) {
                let initial_remaining = 1. - percent_applied;
                ramp = self.compute_ground_movement_delta(delta * initial_remaining, &hit, false);
                last_slice *= initial_remaining;
                hit = self.safe_move(ramp);
                let second = hit.time * initial_remaining;
                percent_applied = (percent_applied + second).clamp(0., 1.);
            }
            if hit.valid_blocking() {
                if self.can_step_up(&hit) {
                    let grav = Vec3::new(0., 0., -1.);
                    if !self.step_up(grav, delta * (1. - percent_applied), &hit, Some(step_down)) {
                        let mut h = hit; self.slide_along_surface(delta, 1. - percent_applied, h.normal, &mut h);
                    }
                }
                let _ = last_slice;
            }
        }
    }
    /// `StepUp`.
    fn step_up(&mut self, grav_dir: Vec3, delta: Vec3, in_hit: &Hit, out_step_down: Option<&mut StepDown>) -> bool {
        if !self.can_step_up(in_hit) || self.p.max_step_height <= 0. { return false; }
        let old_location = self.loc;
        let (pawn_radius, pawn_hh) = (self.p.radius, self.half_height);
        let initial_impact_z = in_hit.impact_point.z;
        if initial_impact_z > old_location.z + (pawn_hh - pawn_radius) { return false; }
        if grav_dir == Vec3::ZERO { return false; }
        let mut step_travel_up = self.p.max_step_height; let mut step_travel_down = step_travel_up;
        let step_side_z = -in_hit.impact_normal.dot(grav_dir);
        let mut pawn_initial_floor_base_z = old_location.z - pawn_hh;
        let mut pawn_floor_point_z = pawn_initial_floor_base_z;
        if self.is_moving_on_ground() && self.floor.is_walkable_floor() {
            let floor_dist = self.floor.distance_to_floor().max(0.);
            pawn_initial_floor_base_z -= floor_dist;
            step_travel_up = (step_travel_up - floor_dist).max(0.);
            step_travel_down = self.p.max_step_height + MAX_FLOOR_DIST * 2.;
            let hit_vertical_face = !self.is_within_edge_tolerance(in_hit.location, in_hit.impact_point, pawn_radius);
            if !self.floor.line_trace && !hit_vertical_face { pawn_floor_point_z = self.floor.hit.impact_point.z; } else { pawn_floor_point_z -= self.floor.floor_dist; }
        }
        if initial_impact_z <= pawn_initial_floor_base_z { return false; }
        // step up - treat as vertical wall
        let sweep_up = self.move_updated(-grav_dir * step_travel_up, true);
        if sweep_up.start_penetrating { self.loc = old_location; return false; }
        // step fwd
        let mut hit = self.move_updated(delta, true);
        if hit.blocking {
            if hit.start_penetrating { self.loc = old_location; return false; }
            if self.is_falling() { return true; }
            let forward_hit_time = hit.time;
            let mut h = hit; let forward_slide = self.slide_along_surface(delta, 1. - hit.time, hit.normal, &mut h);
            if self.is_falling() { self.loc = old_location; return false; }
            if forward_hit_time == 0. && forward_slide == 0. { self.loc = old_location; return false; }
        }
        // step down
        hit = self.move_updated(grav_dir * step_travel_down, true);
        if hit.start_penetrating { self.loc = old_location; return false; }
        let mut result = StepDown::default();
        if hit.valid_blocking() {
            let delta_z = hit.impact_point.z - pawn_floor_point_z;
            if delta_z > self.p.max_step_height { self.loc = old_location; return false; }
            if !self.is_walkable(&hit) {
                let normal_towards_me = delta.dot(hit.impact_normal) < 0.;
                if normal_towards_me { self.loc = old_location; return false; }
                if hit.location.z > old_location.z { self.loc = old_location; return false; }
            }
            if !self.is_within_edge_tolerance(hit.location, hit.impact_point, pawn_radius) { self.loc = old_location; return false; }
            if delta_z > 0. && !self.can_step_up(&hit) { self.loc = old_location; return false; }
            if out_step_down.is_some() {
                result.floor = self.find_floor(self.loc, false, Some(&hit));
                if hit.location.z > old_location.z && !result.floor.blocking_hit && step_side_z < MAX_STEP_SIDE_Z { self.loc = old_location; return false; }
                result.computed = true;
            }
        }
        if let Some(o) = out_step_down { *o = result; }
        true
    }

    // =====================================================================================================================
    // Sliding (UMovementComponent / CMC overrides)
    // =====================================================================================================================
    /// `UMovementComponent::ComputeSlideVector` (+ CMC's HandleSlopeBoosting while falling).
    fn compute_slide_vector(&self, delta: Vec3, time: f32, normal: Vec3, hit: &Hit) -> Vec3 {
        let mut result = plane_project(delta, normal) * time;
        if self.is_falling() { result = self.handle_slope_boosting(result, delta, time, normal, hit); }
        result
    }
    fn handle_slope_boosting(&self, slide: Vec3, delta: Vec3, time: f32, normal: Vec3, hit: &Hit) -> Vec3 {
        let mut result = slide;
        if result.z > 0. {
            let z_limit = delta.z * time;
            if result.z - z_limit > KINDA_SMALL_NUMBER {
                if z_limit > 0. { result *= z_limit / result.z; } else { result = Vec3::ZERO; }
                let remainder_xy = (slide - result) * Vec3::new(1., 1., 0.);
                let normal_xy = safe_normal_2d(normal);
                result += plane_project(remainder_xy, normal_xy);
            }
        }
        let max = self.p.falling_slope_safe_slide_max_speed;
        if self.is_falling() && self.velocity.z < 0. && self.impact_time_slice > 1e-4 && time > 1e-4 && max > 0. && hit.impact_normal.z >= self.p.falling_slope_safe_slide_normal_z() {
            let dt = self.impact_time_slice * time;
            result = clamped_max(result / dt, max) * dt;
        }
        result
    }
    /// `UMovementComponent::TwoWallAdjust` (+ the CMC override for ground movement).
    fn two_wall_adjust(&self, delta: &mut Vec3, hit: &Hit, old_hit_normal: Vec3) {
        let in_delta = *delta;
        let mut d = *delta; let hit_normal = hit.normal;
        if old_hit_normal.dot(hit_normal) <= 0. {
            let desired = d; let new_dir = safe_normal(hit_normal.cross(old_hit_normal));
            d = d.dot(new_dir) * (1. - hit.time) * new_dir;
            if desired.dot(d) < 0. { d = -d; }
        } else {
            let desired = d;
            d = self.compute_slide_vector(d, 1. - hit.time, hit_normal, hit);
            if d.dot(desired) <= 0. { d = Vec3::ZERO; }
            else if (hit_normal.dot(old_hit_normal) - 1.).abs() < KINDA_SMALL_NUMBER { d += hit_normal * 0.01; }
        }
        *delta = d;
        if self.is_moving_on_ground() {
            if delta.z > 0. {
                if (hit.normal.z >= self.p.walkable_floor_z || self.is_walkable(hit)) && hit.normal.z > KINDA_SMALL_NUMBER {
                    let time = 1. - hit.time;
                    let scaled = safe_normal(*delta) * in_delta.length();
                    *delta = Vec3::new(in_delta.x, in_delta.y, scaled.z / hit.normal.z) * time;
                    if delta.z > self.p.max_step_height { *delta *= self.p.max_step_height / delta.z; }
                } else { delta.z = 0.; }
            } else if delta.z < 0. && self.floor.floor_dist < MIN_FLOOR_DIST && self.floor.blocking_hit { delta.z = 0.; }
        }
    }
    /// `UCharacterMovementComponent::SlideAlongSurface` -> `UMovementComponent::SlideAlongSurface`. Returns the fraction of `time` applied.
    fn slide_along_surface(&mut self, delta: Vec3, time: f32, in_normal: Vec3, hit: &mut Hit) -> f32 {
        if !hit.blocking { return 0.; }
        let mut normal = in_normal;
        if self.is_moving_on_ground() {
            if normal.z > 0. { if !self.is_walkable(hit) { normal = safe_normal_2d(normal); } }
            else if normal.z < -KINDA_SMALL_NUMBER && self.floor.floor_dist < MIN_FLOOR_DIST && self.floor.blocking_hit {
                let floor_normal = self.floor.hit.normal;
                if delta.dot(floor_normal) < 0. && floor_normal.z < 1. - KINDA_SMALL_NUMBER { normal = floor_normal; }
                normal = safe_normal_2d(normal);
            }
        }
        let old_hit_normal = normal;
        let mut slide = self.compute_slide_vector(delta, time, normal, hit);
        if slide.dot(delta) > 0. {
            *hit = self.safe_move(slide);
            let first = hit.time; let mut applied = first;
            if hit.valid_blocking() {
                self.two_wall_adjust(&mut slide, hit, old_hit_normal);
                if !nearly_zero(slide, 1e-3) && slide.dot(delta) > 0. {
                    *hit = self.safe_move(slide);
                    applied += hit.time * (1. - first);
                }
            }
            return applied.clamp(0., 1.);
        }
        0.
    }

    // =====================================================================================================================
    // PhysFalling
    // =====================================================================================================================
    /// `UFortMovementComp_CharacterAthena::NewFallVelocity` (exe 0x43a380, vtable 0x630) outside the skydive / parachute custom modes (which are not ported): the Fortnite
    /// override replaces the stock vector clamp of `UCharacterMovementComponent::NewFallVelocity` (0x2084ed0) by a vertical one,
    /// `Result = Initial; Result.Z = max(Initial.Z + Gravity.Z * dt, -|PhysicsVolume->TerminalVelocity|)` (RVAs 0x43a5a9..0x43a5db; X / Y pass through untouched). For the pure -Z gravity
    /// this port always uses it is the same number the stock code gives.
    fn new_fall_velocity(&self, initial: Vec3, gravity: Vec3, dt: f32) -> Vec3 {
        if let Some(a) = self.air_control_params() { return self.aerial_new_fall_velocity(&a, initial, gravity, dt); }
        let mut r = initial;
        r.z = (initial.z + gravity.z * dt).max(-self.p.terminal_velocity.abs());
        r
    }
    #[cfg(test)]
    pub(crate) fn velocity_after_fall_step_for_test(&self, initial: Vec3, gravity: Vec3, dt: f32) -> Vec3 { self.new_fall_velocity(initial, gravity, dt) }
    /// `BoostAirControl` + `GetAirControl`.
    fn get_air_control(&self, tick_air_control: f32, fall_accel: Vec3) -> Vec3 {
        let mut t = tick_air_control;
        if t != 0. && self.p.air_control_boost_multiplier > 0. && size_sq_2d(self.velocity) < self.p.air_control_boost_velocity_threshold * self.p.air_control_boost_velocity_threshold { t = (self.p.air_control_boost_multiplier * t).min(1.); }
        t * fall_accel
    }
    /// `GetFallingLateralAcceleration`.
    fn get_falling_lateral_acceleration(&self) -> Vec3 {
        let mut a = Vec3::new(self.acceleration.x, self.acceleration.y, 0.);
        if size_sq_2d(a) > 0. { a = self.get_air_control(self.p.air_control, a); a = clamped_max(a, self.get_max_acceleration()); }
        a
    }
    /// `LimitAirControl`.
    fn limit_air_control(&self, fall_accel: Vec3, hit: &Hit, check_for_valid_landing_spot: bool) -> Vec3 {
        let mut result = fall_accel;
        if hit.valid_blocking() && hit.normal.z > VERTICAL_SLOPE_NORMAL_Z {
            if !check_for_valid_landing_spot || !self.is_valid_landing_spot(hit.location, hit) {
                if fall_accel.dot(hit.normal) < 0. { let n2 = safe_normal_2d(hit.normal); result = fall_accel - n2 * fall_accel.dot(n2); }
            }
        } else if hit.start_penetrating { return if result.dot(hit.normal) > 0. { result } else { Vec3::ZERO }; }
        result
    }
    /// `IsValidLandingSpot`.
    fn is_valid_landing_spot(&self, capsule_location: Vec3, hit: &Hit) -> bool {
        if !hit.blocking { return false; }
        if !hit.start_penetrating {
            if !self.is_walkable(hit) { return false; }
            let lower_hemisphere_z = hit.location.z - self.half_height + self.p.radius;
            if hit.impact_point.z >= lower_hemisphere_z { return false; }
            if !self.is_within_edge_tolerance(hit.location, hit.impact_point, self.p.radius) { return false; }
        } else if hit.normal.z < KINDA_SMALL_NUMBER { return false; }
        let f = self.find_floor(capsule_location, false, Some(hit));
        f.is_walkable_floor()
    }
    /// `ShouldCheckForValidLandingSpot`.
    fn should_check_for_valid_landing_spot(&self, hit: &Hit) -> bool {
        if hit.normal.z > KINDA_SMALL_NUMBER && !normals_equal(hit.normal, hit.impact_normal) { return self.is_within_edge_tolerance(self.loc, hit.impact_point, self.p.radius); }
        false
    }
    /// `ProcessLanded` (+ `ACharacter::Landed`, `SetPostLandedPhysics`).
    fn process_landed(&mut self, _hit: &Hit, remaining: f32, iterations: u32) {
        self.landed += 1;
        self.aerial_landed();
        if self.is_falling() { self.set_movement_mode(Mode::Walking); }
        self.start_new_physics(remaining, iterations);
    }
    /// `PhysFalling`.
    fn phys_falling(&mut self, dt: f32, mut iterations: u32) {
        if dt < MIN_TICK_TIME { return; }
        let mut fall_accel = self.get_falling_lateral_acceleration(); fall_accel.z = 0.;
        let has_air_control = size_sq_2d(fall_accel) > 0.;
        let mut remaining = dt;
        while remaining >= MIN_TICK_TIME && iterations < self.p.max_simulation_iterations {
            iterations += 1;
            let time_tick = self.get_simulation_time_step(remaining, iterations);
            remaining -= time_tick;
            let old_location = self.loc; self.just_teleported = false;
            let old_velocity = self.velocity; let mut velocity_no_air_control = self.velocity;
            // Apply input
            if has_air_control {
                let (saved_acc, saved_vel) = (self.acceleration, self.velocity);
                self.acceleration = Vec3::ZERO; self.velocity.z = 0.;
                self.calc_velocity(time_tick, self.falling_lateral_friction(), false, self.p.braking_deceleration_falling);
                velocity_no_air_control = Vec3::new(self.velocity.x, self.velocity.y, old_velocity.z);
                self.acceleration = saved_acc; self.velocity = saved_vel;
            }
            {
                let saved_acc = self.acceleration; self.acceleration = fall_accel; self.velocity.z = 0.;
                self.calc_velocity(time_tick, self.falling_lateral_friction(), false, self.p.braking_deceleration_falling);
                self.velocity.z = old_velocity.z; self.acceleration = saved_acc;
            }
            if !has_air_control { velocity_no_air_control = self.velocity; }
            // Apply gravity
            // (exe 0x2088d76: GetGravityZ via vtable 0x3a0, then NewFallVelocity via 0x630 with the plain sub-step; the >= 4.17 JumpForceTimeRemaining / bApplyGravityWhileJumping
            // block does not exist in the 1.8 PhysFalling)
            let gravity = Vec3::new(0., 0., self.gravity_z());
            self.velocity = self.new_fall_velocity(self.velocity, gravity, time_tick);
            velocity_no_air_control = self.new_fall_velocity(velocity_no_air_control, gravity, time_tick);
            let air_control_accel = (self.velocity - velocity_no_air_control) / time_tick;
            // Move (midpoint integration)
            let mut adjusted = 0.5 * (old_velocity + self.velocity) * time_tick;
            let mut hit = self.safe_move(adjusted);
            let mut last_slice = time_tick;
            let mut sub_remaining = time_tick * (1. - hit.time);
            if hit.blocking {
                if self.is_valid_landing_spot(self.loc, &hit) { remaining += sub_remaining; self.process_landed(&hit, remaining, iterations); return; }
                if !hit.start_penetrating && self.should_check_for_valid_landing_spot(&hit) {
                    let pawn_loc = self.loc; let f = self.find_floor(pawn_loc, false, None);
                    if f.is_walkable_floor() && self.is_valid_landing_spot(pawn_loc, &f.hit) { remaining += sub_remaining; let fh = f.hit; self.process_landed(&fh, remaining, iterations); return; }
                }
                // HandleImpact: no state change for this pawn
                self.impact_time_slice = last_slice;
                if !self.is_falling() { return; }
                if has_air_control {
                    let delta_v = self.limit_air_control(air_control_accel, &hit, false) * last_slice;
                    adjusted = (velocity_no_air_control + delta_v) * last_slice;
                }
                let old_hit_normal = hit.normal; let old_hit_impact_normal = hit.impact_normal;
                let mut delta = self.compute_slide_vector(adjusted, 1. - hit.time, old_hit_normal, &hit);
                if sub_remaining > KINDA_SMALL_NUMBER && !self.just_teleported { self.velocity = delta / sub_remaining; }
                if sub_remaining > KINDA_SMALL_NUMBER && delta.dot(adjusted) > 0. {
                    hit = self.safe_move(delta);
                    if hit.blocking {
                        last_slice = sub_remaining; sub_remaining *= 1. - hit.time;
                        if self.is_valid_landing_spot(self.loc, &hit) { remaining += sub_remaining; self.process_landed(&hit, remaining, iterations); return; }
                        self.impact_time_slice = last_slice;
                        if !self.is_falling() { return; }
                        if has_air_control && hit.normal.z > VERTICAL_SLOPE_NORMAL_Z {
                            let last_move_no_air = velocity_no_air_control * last_slice;
                            delta = self.compute_slide_vector(last_move_no_air, 1., old_hit_normal, &hit);
                        }
                        let _pre_two_wall = delta;
                        self.two_wall_adjust(&mut delta, &hit, old_hit_normal);
                        if has_air_control {
                            let delta_v = self.limit_air_control(air_control_accel, &hit, false) * sub_remaining;
                            if delta_v.dot(old_hit_normal) > 0. { delta += delta_v * sub_remaining; }
                        }
                        if sub_remaining > KINDA_SMALL_NUMBER && !self.just_teleported { self.velocity = delta / sub_remaining; }
                        let ditch = old_hit_impact_normal.z > 0. && hit.impact_normal.z > 0. && delta.z.abs() <= KINDA_SMALL_NUMBER && hit.impact_normal.dot(old_hit_impact_normal) < 0.;
                        hit = self.safe_move(delta);
                        if hit.time == 0. && hit.blocking {
                            let mut side = safe_normal_2d(old_hit_normal + hit.impact_normal);
                            if nearly_zero(side, KINDA_SMALL_NUMBER) { side = safe_normal(Vec3::new(old_hit_normal.y, -old_hit_normal.x, 0.)); }
                            hit = self.safe_move(side);
                        }
                        if ditch || self.is_valid_landing_spot(self.loc, &hit) || (hit.blocking && hit.time == 0.) {
                            remaining = 0.; self.process_landed(&hit, remaining, iterations); return;
                        } else if self.p.perch_radius_threshold > 0. && hit.blocking && hit.time == 1. && old_hit_impact_normal.z >= self.p.walkable_floor_z {
                            // "virtual ditch within the perch radius": the engine adds random jitter; the port nudges deterministically instead
                            let moved_z = (self.loc.z - old_location.z).abs(); let moved_2d_sq = size_sq_2d(self.loc - old_location);
                            if moved_z <= 0.2 * time_tick && moved_2d_sq <= 4. * time_tick {
                                self.velocity.z = (self.p.jump_z_velocity * 0.25).max(1.);
                                let d = self.velocity * time_tick; hit = self.safe_move(d); let _ = hit;
                            }
                        }
                    }
                }
            }
            if size_sq_2d(self.velocity) <= KINDA_SMALL_NUMBER * 10. { self.velocity.x = 0.; self.velocity.y = 0.; }
        }
    }

    // =====================================================================================================================
    // Crouch
    // =====================================================================================================================
    /// `Crouch` (the pawn only ever crouches to a smaller capsule).
    pub fn crouch(&mut self) {
        if !self.can_crouch_in_current_state() { return; }
        let ch = self.p.crouched_half_height.max(self.p.radius).max(0.);
        if self.half_height == ch { self.is_crouched = true; return; }
        let old = self.half_height; self.half_height = ch;
        let adjust = old - ch;
        if adjust < 0. {
            // crouching to a larger height (never with the recovered numbers): cancel when encroached
            let at = self.capsule_at(self.loc - Vec3::new(0., 0., adjust), self.p.radius, old);
            if self.scene.overlaps(&at) { self.half_height = old; return; }
        }
        if self.crouch_maintains_base_location { self.loc.z -= adjust; }
        self.is_crouched = true; self.force_next_floor_check = true;
    }
    /// `UnCrouch`: refused (stays crouched) while the standing capsule would overlap geometry above.
    pub fn un_crouch(&mut self) {
        let stand = self.p.stand_half_height;
        if self.half_height == stand { self.is_crouched = false; return; }
        let adjust = stand - self.half_height; let cur = self.half_height;
        let inflation = KINDA_SMALL_NUMBER * 10.;
        let pawn_location = self.loc;
        let standing_hh = stand + inflation;
        let mut encroached;
        if !self.crouch_maintains_base_location {
            // expand in place
            encroached = self.scene.overlaps(&self.capsule_at(pawn_location, self.p.radius, standing_hh));
            if encroached && adjust > 0. {
                let pawn_hh = cur; let shrink_half = pawn_hh - self.p.radius; let trace_dist = pawn_hh - shrink_half;
                let short = self.capsule_at(pawn_location, self.p.radius, shrink_half);
                let hit = self.scene.sweep(short, Vec3::new(0., 0., -trace_dist));
                if hit.start_penetrating { encroached = true; }
                else {
                    let dist_to_base = hit.time * trace_dist + shrink_half;
                    let new_loc = Vec3::new(pawn_location.x, pawn_location.y, pawn_location.z - dist_to_base + standing_hh + inflation + MIN_FLOOR_DIST / 2.);
                    encroached = self.scene.overlaps(&self.capsule_at(new_loc, self.p.radius, standing_hh));
                    if !encroached { self.loc = new_loc; }
                }
            }
        } else {
            // expand while keeping the base location
            let mut standing_loc = pawn_location + Vec3::new(0., 0., standing_hh - cur);
            encroached = self.scene.overlaps(&self.capsule_at(standing_loc, self.p.radius, standing_hh));
            if encroached && self.is_moving_on_ground() {
                let min_floor = KINDA_SMALL_NUMBER * 10.;
                if self.floor.blocking_hit && self.floor.floor_dist > min_floor {
                    standing_loc.z -= self.floor.floor_dist - min_floor;
                    encroached = self.scene.overlaps(&self.capsule_at(standing_loc, self.p.radius, standing_hh));
                }
            }
            if !encroached { self.loc = standing_loc; self.force_next_floor_check = true; }
        }
        if encroached { return; }
        self.is_crouched = false; self.half_height = stand;
    }

    /// Teleport (external position change): the next floor check is forced, like `bJustTeleported`.
    pub fn teleport_to_feet(&mut self, feet: Vec3) {
        self.loc = feet + Vec3::new(0., 0., self.half_height); self.force_next_floor_check = true;
        if self.is_moving_on_ground() { self.floor = self.find_floor(self.loc, false, None); self.adjust_floor_height(); self.has_base = self.floor.is_walkable_floor(); if !self.floor.is_walkable_floor() { self.set_movement_mode(Mode::Falling); } }
        self.last_update_location = self.loc;
    }
    /// TERRAIN: an external move that puts the feet on the floor (spawn / teleport onto the landscape): walking at `feet` (the floor is found as for a teleport;
    /// an unwalkable one makes it fall).
    pub fn place_on_ground(&mut self, feet: Vec3) {
        self.loc = feet + Vec3::new(0., 0., self.half_height);
        if self.mode != Mode::Walking { self.velocity.z = 0.; self.set_movement_mode(Mode::Walking); }
        self.teleport_to_feet(feet);
    }
    /// Puts the pawn in the air (external height change) without a jump.
    pub fn launch(&mut self, v: Vec3) { self.pending_launch = v; }
    fn handle_pending_launch(&mut self) -> bool {
        if self.pending_launch == Vec3::ZERO { return false; }
        self.velocity = self.pending_launch;
        self.set_movement_mode(Mode::Falling);
        self.pending_launch = Vec3::ZERO;
        self.force_next_floor_check = true;
        true
    }
    pub fn start_falling_externally(&mut self) { if self.mode == Mode::Walking { self.set_movement_mode(Mode::Falling); } }
    /// Height of the feet above the world floor: the floor surface while walking (the capsule hovers 1.9..2.4 cm above it), the capsule base otherwise.
    pub fn feet_height(&self) -> f32 {
        if self.is_moving_on_ground() && self.floor.is_walkable_floor() { self.floor.hit.impact_point.z.min(self.feet_z()) } else { self.feet_z() }
    }
}

