use crate::cmc::Cmc;
use crate::scene::{Capsule, Scene};
use glam::Vec3;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
pub fn set_enabled(on: bool) { ENABLED.store(on, Ordering::Relaxed); }
pub fn enabled() -> bool { ENABLED.load(Ordering::Relaxed) }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtMultipliers { pub speed: f32, pub accel: f32, pub jump_z: f32 }
impl Default for ExtMultipliers { fn default() -> Self { ExtMultipliers { speed: 1., accel: 1., jump_z: 1. } } }

pub mod tac {
    pub const SPEED_MULTIPLIER_TOTAL: f32 = 1.4;
    pub const ACCELERATION_MULTIPLIER_TOTAL: f32 = 3.0;
    pub const JUMP_Z_MULTIPLIER_TOTAL: f32 = 1.2;
    pub const RAMP_UP_SPEED: f32 = 1.0;
    pub const RAMP_DOWN_SPEED: f32 = 1.0;
    pub const ENERGY_MAX: f32 = 100.;
    pub const MIN_ENERGY_TO_ACTIVATE: f32 = 10.;
    pub const COST_ON_START: f32 = 0.;
    pub const COST_PER_SECOND: f32 = 15.;
    pub const COST_ON_JUMP: f32 = 10.;
    pub const RECHARGE_PER_SECOND: f32 = 45.;
    pub const RECHARGE_DELAY: f32 = 4.;
    pub const RECHARGE_PERCENTAGE_LIMIT: f32 = 1.;
    pub const TIME_FALLING_ALLOWED: f32 = 0.5;
    pub const MIN_SPEED_FOR_HOLSTER: f32 = 100.;
    pub const CAMERA_FOV: f32 = 95.;
    pub const CAMERA_TRANSITION_IN: f32 = 0.8;
    pub const CAMERA_TRANSITION_OUT: f32 = 0.6;
    pub const TOGGLE_GRACE_PERIOD: f32 = 0.5;
}

pub mod clamber {
    pub const ACTIVATION_HORIZONTAL_RANGE: f32 = 150.;
    pub const ACTIVATION_VERTICAL_RANGE: f32 = 325.;
    pub const LOOK_AT_THRESHOLD_DEG: f32 = 45.;
    pub const QUEUED_INPUT_WINDOW: f32 = 0.25;
    pub const ENABLE_INPUT_DELAY: f32 = 0.25;
    pub const FORWARD_CAST_DISTANCE: f32 = 300.;
    pub const FORWARD_CAST_RADIUS: f32 = 15.;
    pub const HORIZONTAL_SURFACE_THRESHOLD_DEG: f32 = 60.;
    pub const UPWARD_DISTANCE_CAPSULE_HEIGHT_MULT: f32 = 2.;
    pub const UPWARD_START_CAPSULE_HEIGHT_MULT: f32 = 0.1;
    pub const DOWNWARD_DISTANCE_CAPSULE_HEIGHT_MULT: f32 = 2.;
    pub const MINIMUM_LEDGE_HEIGHT: f32 = 20.;
    pub const DOWNWARD_SPHERE_CAST_RADIUS: f32 = 20.;
    pub const FALLING_SPEED_THRESHOLD: f32 = -100.;
    pub const MOVE_DURATION: f32 = 0.6;
    pub const SYNC_TARGET_LEDGE_OFFSET: f32 = 10.;
    pub const RISE_FRACTION: f32 = 0.6;
    pub const MONTAGE_BLEND_PADDING: f32 = 0.1;
    pub const PLAY_RATE: f32 = 0.645 / (MOVE_DURATION + MONTAGE_BLEND_PADDING);
    pub const TARGET_Z_250UP: f32 = 175.;
    pub const TARGET_Z_150UP: f32 = 100.;
}

pub fn interp_constant_to(current: f32, target: f32, dt: f32, speed: f32) -> f32 {
    let d = target - current;
    if d * d < 1e-8 { return target; }
    let step = speed * dt;
    current + d.clamp(-step, step)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TacPhase { Off, Active, RampingDown }

#[derive(Clone, Copy, Debug, Default)]
pub struct TacInput {
    pub wants: bool,
    pub sprint_style: bool,
    pub grounded: bool,
    pub falling: bool,
    pub jumped: bool,
    pub speed_2d: f32,
    pub blocked: Option<&'static str>,
    pub toggle: bool,
}

#[derive(Clone, Debug)]
pub struct TacSprint {
    pub phase: TacPhase,
    pub speed_mult: f32,
    pub energy: f32,
    since_use: f32,
    falling_time: f32,
    jumped_air: bool,
    pub weapon_lowered: bool,
    pub camera_blend: f32,
    pub last_end: Option<&'static str>,
    grace: f32,
    wants_prev: bool,
}
impl Default for TacSprint {
    fn default() -> Self { TacSprint { phase: TacPhase::Off, speed_mult: 1., energy: tac::ENERGY_MAX, since_use: tac::RECHARGE_DELAY, falling_time: 0., jumped_air: false, weapon_lowered: false, camera_blend: 0., last_end: None, grace: 0., wants_prev: false } }
}
impl TacSprint {
    pub fn active(&self) -> bool { self.phase != TacPhase::Off }
    pub fn energy_fraction(&self) -> f32 { (self.energy / tac::ENERGY_MAX).clamp(0., 1.) }
    pub fn multipliers(&self) -> ExtMultipliers {
        match self.phase {
            TacPhase::Off => ExtMultipliers::default(),
            TacPhase::Active => ExtMultipliers { speed: self.speed_mult, accel: tac::ACCELERATION_MULTIPLIER_TOTAL, jump_z: tac::JUMP_Z_MULTIPLIER_TOTAL },
            TacPhase::RampingDown => ExtMultipliers { speed: self.speed_mult, accel: tac::ACCELERATION_MULTIPLIER_TOTAL, jump_z: 1. },
        }
    }
    fn end(&mut self, why: &'static str) { self.phase = TacPhase::Off; self.speed_mult = 1.; self.falling_time = 0.; self.jumped_air = false; self.last_end = Some(why); if why == "ability blocked" { self.camera_blend = 0.; } }
    fn can_start(&self, i: &TacInput) -> bool { i.wants && i.sprint_style && i.grounded && i.blocked.is_none() && self.energy >= tac::MIN_ENERGY_TO_ACTIVATE }

    pub fn tick(&mut self, i: &TacInput, dt: f32) {
        if i.wants && !self.wants_prev { self.grace = tac::TOGGLE_GRACE_PERIOD; }
        self.wants_prev = i.wants;
        let gated = TacInput { wants: i.wants && (!i.toggle || self.phase != TacPhase::Off || self.grace > 0.), ..*i };
        self.grace = (self.grace - dt).max(0.);
        let i = &gated;
        match self.phase {
            TacPhase::Off => {
                if self.can_start(i) {
                    self.phase = TacPhase::Active; self.speed_mult = 1.; self.energy -= tac::COST_ON_START; self.since_use = 0.; self.falling_time = 0.; self.jumped_air = false; self.last_end = None;
                }
            }
            TacPhase::Active => {
                if let Some(why) = i.blocked { self.end(why); }
                else {
                    if i.jumped { self.energy -= tac::COST_ON_JUMP; self.jumped_air = true; }
                    self.energy -= tac::COST_PER_SECOND * dt;
                    self.since_use = 0.;
                    if self.energy <= 0. { self.energy = 0.; self.end("run out of energy"); }
                    else {
                        if i.grounded { self.falling_time = 0.; self.jumped_air = false; }
                        else if i.falling && !self.jumped_air { self.falling_time += dt; }
                        if !i.wants || (i.grounded && !i.sprint_style) || self.falling_time > tac::TIME_FALLING_ALLOWED { self.phase = TacPhase::RampingDown; }
                        else { self.speed_mult = if self.speed_mult < tac::SPEED_MULTIPLIER_TOTAL { interp_constant_to(self.speed_mult, tac::SPEED_MULTIPLIER_TOTAL, dt, tac::RAMP_UP_SPEED) } else { tac::SPEED_MULTIPLIER_TOTAL }; }
                    }
                }
            }
            TacPhase::RampingDown => {
                if let Some(why) = i.blocked { self.end(why); }
                else if !i.grounded { self.end("ramping down not in ground"); }
                else if self.can_start(i) { self.phase = TacPhase::Active; }
                else {
                    self.speed_mult = interp_constant_to(self.speed_mult, 1., dt, tac::RAMP_DOWN_SPEED);
                    if self.speed_mult <= 1. { self.end("ramping down over"); }
                }
            }
        }
        if self.phase != TacPhase::Active {
            self.since_use += dt;
            let cap = tac::ENERGY_MAX * tac::RECHARGE_PERCENTAGE_LIMIT;
            if self.since_use >= tac::RECHARGE_DELAY && self.energy < cap { self.energy = (self.energy + tac::RECHARGE_PER_SECOND * dt).min(cap); }
        }
        self.weapon_lowered = self.phase == TacPhase::Active && i.speed_2d > tac::MIN_SPEED_FOR_HOLSTER;
        let target = if self.phase == TacPhase::Active { 1. } else { 0. };
        let rate = if target > self.camera_blend { 1. / tac::CAMERA_TRANSITION_IN } else { 1. / tac::CAMERA_TRANSITION_OUT };
        self.camera_blend = interp_constant_to(self.camera_blend, target, dt, rate);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ledge { pub end: Vec3, pub wall_point: Vec3, pub height: f32, pub normal: Vec3 }

pub fn find_ledge(scene: &Scene, loc: Vec3, radius: f32, half_height: f32, facing: Vec3) -> Option<Ledge> {
    use clamber::*;
    let f = Vec3::new(facing.x, facing.y, 0.).normalize_or_zero();
    if f == Vec3::ZERO { return None; }
    let feet = loc.z - half_height;
    let h = 2. * half_height;
    let start = Vec3::new(loc.x, loc.y, feet + UPWARD_START_CAPSULE_HEIGHT_MULT * h + FORWARD_CAST_RADIUS);
    let probe = Capsule { c: start, r: FORWARD_CAST_RADIUS, hh: FORWARD_CAST_RADIUS };
    let hit = scene.sweep(probe, f * FORWARD_CAST_DISTANCE);
    if !hit.blocking || hit.start_penetrating { return None; }
    let wall_dist = hit.time * FORWARD_CAST_DISTANCE + FORWARD_CAST_RADIUS - radius;
    if wall_dist > ACTIVATION_HORIZONTAL_RANGE { return None; }
    let n = Vec3::new(hit.normal.x, hit.normal.y, 0.).normalize_or_zero();
    if n == Vec3::ZERO || (-n).dot(f) < LOOK_AT_THRESHOLD_DEG.to_radians().cos() { return None; }
    let reach = (UPWARD_DISTANCE_CAPSULE_HEIGHT_MULT * h).min(ACTIVATION_VERTICAL_RANGE);
    let up = scene.sweep(Capsule { c: loc, r: radius, hh: half_height }, Vec3::new(0., 0., (feet + reach + half_height) - loc.z));
    let top_z = if up.blocking { up.location.z + half_height - 1. } else { feet + reach + half_height };
    let column = hit.impact_point - n * (DOWNWARD_SPHERE_CAST_RADIUS + 1.);
    let down_start = Vec3::new(column.x, column.y, top_z - DOWNWARD_SPHERE_CAST_RADIUS);
    let sphere = Capsule { c: down_start, r: DOWNWARD_SPHERE_CAST_RADIUS, hh: DOWNWARD_SPHERE_CAST_RADIUS };
    let down = scene.sweep(sphere, Vec3::new(0., 0., -DOWNWARD_DISTANCE_CAPSULE_HEIGHT_MULT * h));
    if !down.blocking || down.start_penetrating { return None; }
    if down.normal.z < HORIZONTAL_SURFACE_THRESHOLD_DEG.to_radians().cos() { return None; }
    let surface = down.location.z - DOWNWARD_SPHERE_CAST_RADIUS;
    let height = surface - feet;
    if !(MINIMUM_LEDGE_HEIGHT..=ACTIVATION_VERTICAL_RANGE).contains(&height) { return None; }
    let inward = -n * (radius + SYNC_TARGET_LEDGE_OFFSET);
    let end = Vec3::new(hit.impact_point.x + inward.x, hit.impact_point.y + inward.y, surface + half_height + 1.);
    if scene.overlaps(&Capsule { c: end, r: radius, hh: half_height }) { return None; }
    Some(Ledge { end, wall_point: hit.impact_point, height, normal: n })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClamberClip { pub root: &'static [[f32; 3]], pub warp_point: [f32; 3], pub anim_start: f32, pub anim_end: f32, pub warp_end: f32 }
pub const CLAMBER_100: ClamberClip = ClamberClip { root: &crate::clamber_root::UP100_ROOT, warp_point: crate::clamber_root::UP100_WARP_POINT, anim_start: 0.1, anim_end: 0.63, warp_end: 0.083 };
pub const CLAMBER_150: ClamberClip = ClamberClip { root: &crate::clamber_root::UP150_ROOT, warp_point: crate::clamber_root::UP150_WARP_POINT, anim_start: 0.1, anim_end: 0.63, warp_end: 0.083 };
pub const CLAMBER_250: ClamberClip = ClamberClip { root: &crate::clamber_root::UP250_ROOT, warp_point: crate::clamber_root::UP250_WARP_POINT, anim_start: 0., anim_end: 0.645, warp_end: 0.209 };
pub fn clamber_clip(target_distance_z: f32) -> &'static ClamberClip {
    if target_distance_z >= clamber::TARGET_Z_250UP { &CLAMBER_250 } else if target_distance_z >= clamber::TARGET_Z_150UP { &CLAMBER_150 } else { &CLAMBER_100 }
}
impl ClamberClip {
    pub fn length(&self) -> f32 { self.anim_end - self.anim_start }
    pub fn duration(&self) -> f32 { self.length() / clamber::PLAY_RATE }
    pub fn root_at(&self, anim_time: f32) -> Vec3 {
        let f = (anim_time * crate::clamber_root::SAMPLE_RATE).clamp(0., (self.root.len() - 1) as f32);
        let i = (f.floor() as usize).min(self.root.len() - 2);
        let u = f - i as f32;
        let (a, b) = (Vec3::from(self.root[i]), Vec3::from(self.root[i + 1]));
        a + (b - a) * u
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClamberMove { pub start_feet: Vec3, pub warp: Vec3, pub forward: Vec3, pub half_height: f32, pub height: f32, pub t: f32, pub last: Vec3 }
impl ClamberMove {
    pub fn clip(&self) -> &'static ClamberClip { clamber_clip(self.height) }
    fn to_world(&self, v: Vec3) -> Vec3 {
        let right = Vec3::new(-self.forward.y, self.forward.x, 0.);
        self.forward * v.y - right * v.x + Vec3::Z * v.z
    }
    pub fn feet_at(&self, t: f32) -> Vec3 {
        let c = self.clip();
        let m = (t * clamber::PLAY_RATE).clamp(0., c.length());
        let r0 = c.root_at(c.anim_start);
        let unwarped = |m: f32| self.start_feet + self.to_world(c.root_at(c.anim_start + m) - r0);
        let we = c.warp_end;
        let desired = self.warp - self.to_world(Vec3::from(c.warp_point) - c.root_at(c.anim_start + we));
        let correction = desired - unwarped(we);
        unwarped(m) + correction * (m / we).clamp(0., 1.)
    }
    pub fn center_at(&self, t: f32) -> Vec3 { self.feet_at(t) + Vec3::Z * self.half_height }
}

#[derive(Clone, Debug, Default)]
pub struct Clamber {
    pub moving: Option<ClamberMove>,
    queued: f32,
    input_delay: f32,
    pub count: u32,
}
impl Clamber {
    pub fn is_moving(&self) -> bool { self.moving.is_some() }
    pub fn press(&mut self) { self.queued = clamber::QUEUED_INPUT_WINDOW; }
    pub fn try_start(&mut self, cmc: &Cmc) -> bool {
        if self.queued <= 0. || self.input_delay > 0. || self.moving.is_some() || cmc.is_crouching() || cmc.in_aerial_mode() || cmc.is_skydiving() { return false; }
        if cmc.is_falling() && cmc.velocity.z < clamber::FALLING_SPEED_THRESHOLD { return false; }
        let Some(l) = find_ledge(&cmc.scene, cmc.loc, cmc.p.radius, cmc.half_height, cmc.facing) else { return false };
        let feet = cmc.loc - Vec3::Z * cmc.half_height;
        let warp = Vec3::new(l.wall_point.x, l.wall_point.y, feet.z + l.height);
        self.moving = Some(ClamberMove { start_feet: feet, warp, forward: -l.normal, half_height: cmc.half_height, height: l.height, t: 0., last: cmc.loc });
        self.queued = 0.;
        self.count += 1;
        true
    }
    pub fn advance(&mut self, dt: f32) -> Option<(Vec3, bool, Vec3)> {
        self.queued = (self.queued - dt).max(0.);
        let Some(m) = self.moving.as_mut() else { self.input_delay = (self.input_delay - dt).max(0.); return None };
        m.t += dt;
        let p = m.center_at(m.t);
        let v = (p - m.last) / dt.max(1e-4);
        m.last = p;
        let done = m.t >= m.clip().duration();
        if done { self.moving = None; self.input_delay = clamber::ENABLE_INPUT_DELAY; }
        Some((p, done, v))
    }
}

pub mod slide {
    pub const MIN_SPEED_TO_START_SLIDING: f32 = 450.;
    pub const BRAKING_DECELERATION: f32 = 0.;
    pub const MAX_SLIDING_SLOPE_ANGLE: f32 = 65.;
    pub const BOOST_UPHILL_GROUND_FRICTION_MULTIPLIER: f32 = 0.5 * 0.083333;
    pub const BOOST_LEVEL_GROUND_FRICTION_MULTIPLIER: f32 = 0.125 * 0.;
    pub const UPHILL_GROUND_FRICTION_MULTIPLIER: f32 = 0.125;
    pub const LEVEL_GROUND_FRICTION_MULTIPLIER: f32 = 0.046667;
    pub const DOWNHILL_GROUND_FRICTION_MULTIPLIER: f32 = -0.083333;
    pub const MAX_FORWARD_SLIDING_SPEED: f32 = 1200.;
    pub const MIN_FORWARD_SLIDING_SPEED: f32 = 400.;
    pub const MAX_FORWARD_SLIDING_ACCELERATION: f32 = 0.;
    pub const MAX_BACKWARD_SLIDING_ACCELERATION: f32 = 1500.;
    pub const SLIDING_FATIGUE_DURATION: f32 = 0.5;
    pub const SLIDING_FATIGUE_MULTIPLIER: f32 = 3.;
    pub const SLIDING_FATIGUE_MAX_COUNT: u8 = 10;
    pub const ENTER_SLIDE_FROM_FALL_INPUT_BUFFER: f32 = 1.5;
    pub const CROUCH_COOLDOWN_AFTER_SLIDE_DURATION: f32 = 0.5;
    pub const MIN_ACCELERATION_SLIDING_ANGLE: f32 = 25.;
    pub const MIN_ANGLE_FOR_ADDITIONAL_MOMENTUM: f32 = 10.;
    pub const MIN_ADDITIONAL_MOMENTUM: f32 = 200.;
    pub const MAX_ADDITIONAL_MOMENTUM: f32 = 700.;
    pub const FALLING_GRACE_PERIOD: f32 = 7.0 * 0.1;
    pub const MAX_ROTATION_ANGLE_ON_ACCELERATION: f32 = 20.;
    pub const ROTATION_ANGLE_SPEED_ON_ACCELERATION: f32 = 15.;
    pub const SLOPE_UP_ADDED_FRICTION_ANGLE: f32 = 55.;
    pub const SLOPE_UP_ADDED_FRICTION_VALUE: f32 = 15.;
    pub const DASH_PERIOD: f32 = 1.45;
    pub const SPEED_ADDED_FRICTION_CURVE: [(f32, f32); 5] = [(0., 0.), (600., 0.), (800., 0.020833), (2750., 0.083333), (4500., 0.158333)];
    pub const CAMERA_TRANSITION: f32 = 0.45;
    pub fn speed_added_friction(speed: f32) -> f32 {
        let k = &SPEED_ADDED_FRICTION_CURVE;
        if speed <= k[0].0 { return k[0].1; }
        for w in k.windows(2) { if speed <= w[1].0 { return w[0].1 + (w[1].1 - w[0].1) * (speed - w[0].0) / (w[1].0 - w[0].0); } }
        k[k.len() - 1].1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlideState { Boost, NoBoost, Falling, RequestEnd }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlideRun {
    pub state: SlideState,
    pub cap: f32,
    pub frame: glam::Vec2,
    pub fatigue: u8,
    pub boost_left: f32,
    pub fall_time: f32,
    pub just_started: bool,
    pub has_input: bool,
    pub end: Option<&'static str>,
    pub time: f32,
}
impl SlideRun {
    pub fn new(fatigue: u8, frame: glam::Vec2) -> Self {
        SlideRun { state: if slide::DASH_PERIOD > 0. { SlideState::Boost } else { SlideState::NoBoost }, cap: slide::MAX_FORWARD_SLIDING_SPEED, frame, fatigue, boost_left: slide::DASH_PERIOD, fall_time: 0., just_started: true, has_input: false, end: None, time: 0. }
    }
}

fn rotate2(v: glam::Vec2, deg: f32) -> glam::Vec2 { let (s, c) = deg.to_radians().sin_cos(); glam::Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c) }

impl Cmc {
    pub fn is_sliding(&self) -> bool { self.slide.is_some() }
    pub fn slide_pre_iteration(&mut self, dt: f32) -> (f32, f32) {
        use slide::*;
        let Some(mut s) = self.slide else { return (self.p.ground_friction, self.p.braking_deceleration_walking) };
        let g = self.p.ground_friction;
        let lim = { let c = (90. - MAX_SLIDING_SLOPE_ANGLE).to_radians().cos(); if c.abs() < 1e-4 { 0. } else { c } };
        let n = if self.floor.blocking_hit { self.floor.hit.impact_normal } else { Vec3::Z };
        let vh = glam::Vec2::new(self.velocity.x, self.velocity.y);
        let d = { let x = vh.normalize_or_zero(); if x == glam::Vec2::ZERO { s.frame } else { x } };
        let along = n.x * d.x + n.y * d.y;
        let slope = if lim == 0. { 0. } else { along.clamp(-lim, lim) / lim };
        if s.just_started {
            s.cap = vh.length().max(MAX_FORWARD_SLIDING_SPEED);
            s.frame = d;
            s.just_started = false;
        }
        let slope_deg = n.z.clamp(-1., 1.).acos().to_degrees();
        if self.floor.blocking_hit && slope_deg > MIN_ANGLE_FOR_ADDITIONAL_MOMENTUM && along > 0. {
            let nh = glam::Vec2::new(n.x, n.y).normalize_or_zero();
            let t = ((slope_deg - MIN_ANGLE_FOR_ADDITIONAL_MOMENTUM) / (90. - MIN_ANGLE_FOR_ADDITIONAL_MOMENTUM)).clamp(0., 1.);
            let m = MIN_ADDITIONAL_MOMENTUM + (MAX_ADDITIONAL_MOMENTUM - MIN_ADDITIONAL_MOMENTUM) * t;
            let down = Vec3::new(nh.x * n.z, nh.y * n.z, -glam::Vec2::new(n.x, n.y).length());
            self.velocity += down * n.z * m * dt;
        }
        let (up, lvl) = if s.state == SlideState::Boost { (BOOST_UPHILL_GROUND_FRICTION_MULTIPLIER, BOOST_LEVEL_GROUND_FRICTION_MULTIPLIER) } else { (UPHILL_GROUND_FRICTION_MULTIPLIER, LEVEL_GROUND_FRICTION_MULTIPLIER) };
        let mut friction = if slope >= 0. { (DOWNHILL_GROUND_FRICTION_MULTIPLIER * g - lvl * g) * slope + lvl * g } else {
            let f = (lvl * g - up * g) * (slope + 1.) + up * g;
            if slope_deg > SLOPE_UP_ADDED_FRICTION_ANGLE { f + SLOPE_UP_ADDED_FRICTION_VALUE } else { f }
        };
        for _ in 0..s.fatigue { friction *= SLIDING_FATIGUE_MULTIPLIER; }
        if s.state == SlideState::Falling { friction = 0.; }
        friction += speed_added_friction(glam::Vec2::new(self.velocity.x, self.velocity.y).length()) * g;
        let fwd = Vec3::new(s.frame.x, s.frame.y, 0.);
        let right = Vec3::new(-s.frame.y, s.frame.x, 0.);
        let a = self.acceleration;
        let (a_fwd, a_lat) = (a.dot(fwd), a.dot(right));
        s.has_input = a_fwd * a_fwd + a_lat * a_lat > 0.5;
        let fwd_c = a_fwd.min(MAX_FORWARD_SLIDING_ACCELERATION).max(-MAX_BACKWARD_SLIDING_ACCELERATION);
        if a.length_squared() > 1e-8 {
            let ad = a.normalize();
            let ang = ad.dot(fwd).clamp(-1., 1.).acos().to_degrees();
            if ang >= MIN_ACCELERATION_SLIDING_ANGLE {
                let v2 = glam::Vec2::new(self.velocity.x, self.velocity.y);
                let vd = v2.normalize_or_zero();
                let mut cur = vd.dot(s.frame).clamp(-1., 1.).acos().to_degrees();
                if s.frame.perp_dot(vd) < 0. { cur = -cur; }
                let mut rot = ROTATION_ANGLE_SPEED_ON_ACCELERATION * dt;
                if s.frame.perp_dot(glam::Vec2::new(ad.x, ad.y)) < 0. { rot = -rot; }
                let rot = rot.min(MAX_ROTATION_ANGLE_ON_ACCELERATION - cur).max(-MAX_ROTATION_ANGLE_ON_ACCELERATION - cur);
                let r = rotate2(v2, rot);
                self.velocity.x = r.x; self.velocity.y = r.y;
            }
        }
        self.acceleration = fwd * fwd_c + Vec3::Z * a.z;
        self.slide = Some(s);
        (friction, BRAKING_DECELERATION)
    }
    pub fn slide_post_iteration(&mut self) {
        let Some(mut s) = self.slide else { return };
        let vh = glam::Vec2::new(self.velocity.x, self.velocity.y);
        let len = vh.length();
        let clamped = if len > s.cap && len > 0. { vh * (s.cap / len) } else { vh };
        s.cap = (len * len + self.velocity.z * self.velocity.z).sqrt().max(slide::MIN_FORWARD_SLIDING_SPEED);
        self.velocity.x = clamped.x; self.velocity.y = clamped.y;
        if clamped.length() < slide::MIN_FORWARD_SLIDING_SPEED && s.state != SlideState::RequestEnd { s.state = SlideState::RequestEnd; s.end = Some("phys resolution requests sliding end"); }
        self.slide = Some(s);
    }
    pub fn slide_max_speed(&self) -> Option<f32> { self.slide.map(|s| s.cap) }
}

#[derive(Clone, Debug, Default)]
pub struct Slider {
    pub count: u32,
    clock: f32,
    fatigue_until: f32,
    fatigue: u8,
    fall_buffer: f32,
    crouch_prev: bool,
    pub crouch_cooldown: f32,
    pub stand_until_release: bool,
    pub camera_blend: f32,
    pub shake_time: Option<f32>,
    pub last_end: Option<&'static str>,
}
impl Slider {
    pub fn can_start(cmc: &Cmc, blocked: bool) -> bool {
        !blocked && cmc.is_moving_on_ground() && !cmc.is_crouching() && !cmc.in_aerial_mode() && !cmc.is_skydiving()
            && glam::Vec2::new(cmc.velocity.x, cmc.velocity.y).length() >= slide::MIN_SPEED_TO_START_SLIDING
    }
    pub fn start(&mut self, cmc: &mut Cmc) {
        self.fatigue = if self.clock < self.fatigue_until { (self.fatigue + 1).min(slide::SLIDING_FATIGUE_MAX_COUNT) } else { 0 };
        let f = glam::Vec2::new(cmc.velocity.x, cmc.velocity.y).normalize_or_zero();
        cmc.slide = Some(SlideRun::new(self.fatigue, f));
        self.count += 1;
        self.shake_time = Some(0.);
        self.last_end = None;
    }
    pub fn end(&mut self, cmc: &mut Cmc, why: &'static str) {
        let Some(s) = cmc.slide.take() else { return };
        self.fatigue_until = self.clock + slide::SLIDING_FATIGUE_DURATION;
        self.crouch_cooldown = slide::CROUCH_COOLDOWN_AFTER_SLIDE_DURATION;
        self.stand_until_release = s.has_input;
        self.last_end = Some(s.end.unwrap_or(why));
    }
    pub fn update(&mut self, cmc: &mut Cmc, crouch: bool, jump_edge: bool, blocked: bool, dt: f32) -> bool {
        self.clock += dt;
        self.crouch_cooldown = (self.crouch_cooldown - dt).max(0.);
        if let Some(t) = self.shake_time.as_mut() { *t += dt; }
        let edge = crouch && !self.crouch_prev;
        self.crouch_prev = crouch;
        if !crouch { self.stand_until_release = false; }
        let mut jumped_out = false;
        if cmc.slide.is_some() {
            if blocked { self.end(cmc, "blocked"); }
            else if jump_edge { self.end(cmc, "jumping out of slide"); self.stand_until_release = true; jumped_out = true; }
        }
        if let Some(mut s) = cmc.slide {
            s.time += dt;
            if s.state == SlideState::RequestEnd { cmc.slide = Some(s); self.end(cmc, "request end"); }
            else {
                if s.boost_left > 0. { s.boost_left = (s.boost_left - dt).max(0.); }
                if s.state == SlideState::Boost && s.boost_left <= 0. { s.state = SlideState::NoBoost; }
                if cmc.is_falling() {
                    if s.state != SlideState::Falling { s.state = SlideState::Falling; }
                    s.fall_time += dt;
                } else if s.state == SlideState::Falling { s.state = if s.boost_left > 0. { SlideState::Boost } else { SlideState::NoBoost }; s.fall_time = 0.; }
                cmc.slide = Some(s);
                if s.fall_time > slide::FALLING_GRACE_PERIOD { self.end(cmc, "falling when sliding for too long"); }
            }
        } else if !jumped_out {
            if edge && cmc.is_falling() { self.fall_buffer = slide::ENTER_SLIDE_FROM_FALL_INPUT_BUFFER; }
            let buffered = self.fall_buffer > 0. && crouch && cmc.is_moving_on_ground();
            if (edge || buffered) && Self::can_start(cmc, blocked) { self.fall_buffer = 0.; self.start(cmc); }
        }
        self.fall_buffer = (self.fall_buffer - dt).max(0.);
        let target = if cmc.slide.is_some() { 1. } else { 0. };
        self.camera_blend = interp_constant_to(self.camera_blend, target, dt, 1. / slide::CAMERA_TRANSITION);
        jumped_out
    }
}

pub mod hurdle {
    pub const CAST_RADIUS: f32 = 15.;
    pub const FORWARD_CAST_DISTANCE: f32 = 175.;
    pub const FORWARD_CAST_DISTANCE_AUTO: f32 = 140.;
    pub const SWEEP_GROUND_MAX_HEIGHT: f32 = 250.;
    pub const MAX_FLOOR_SEARCH_HEIGHT: f32 = 200.;
    pub const MAX_VERTICAL_ANGLE_DEVIATION_DOWN_ON_NORMAL: f32 = 45.;
    pub const HEIGHT_CORRECTION_MIN_FLOOR_ANGLE: f32 = 10.;
    pub const PHASE3_MIN_TOP_CLEARANCE_FOR_HURDLE_ON: f32 = 150.;
    pub const PHASE4_UP_ANGLE_TOLERANCE: f32 = 45.;
    pub const MAXIMUM_CAPSULE_RADIUS: f32 = 25.;
    pub const MAXIMUM_CAPSULE_HALF_HEIGHT: f32 = 30.;
    pub const MAX_ALLOWED_FALL_DISTANCE: f32 = 1299.;
    pub const FINAL_RESULT_GROUND_AIM_OFFSET: f32 = 50.;
    pub fn forward_cast(tactical_sprinting: bool) -> f32 { if tactical_sprinting { FORWARD_CAST_DISTANCE_MANUAL } else { FORWARD_CAST_DISTANCE } }
    pub const FORWARD_CAST_DISTANCE_MANUAL: f32 = 180.;
    pub const WALL_ANGLE_TOLERANCE: f32 = 30.;
    pub const SWEEP_GROUND_BUFFER_HEIGHT: f32 = 50.;
    pub const FLOOR_WALL_OFFSET: f32 = 30.;
    pub const MIN_HEIGHT: f32 = 40.;
    pub const UP_ANGLE_TOLERANCE: f32 = 45.;
    pub const MAX_HEIGHT: f32 = 200.;
    pub const PHASE3_CAPSULE_OFFSET: f32 = 30.;
    pub const PHASE3_CLEARANCE_CAPSULE_SIZE: f32 = 100.;
    pub const PHASE3_FORWARD_CAST_DISTANCE: f32 = 155.;
    pub const PHASE4_MAX_DOWN_DISTANCE: f32 = 250.;
    pub const PHASE4_HURDLE_ON_Z_TOLERANCE: f32 = 25.;
    pub const PHASE5_CAPSULE_OFFSET: f32 = 30.;
    pub const PHASE5_CLEARANCE_CAPSULE_SIZE: f32 = 150.;
    pub const ALLOW_HURDLE_ON_TO: bool = false;
    pub const TACTICAL_SPRINT_REQUIRED: bool = true;
    pub const MONTAGE_50UP: (f32, f32, f32, f32) = (0.19047572, 0.36965665, 0.6805651, 1.);
    pub const MONTAGE_100UP: (f32, f32, f32, f32) = (0.16666667, 0.5847781, 0.16666667 + 1.2743714 - 0.7666667, 1.15);
    pub const MONTAGE_200UP: (f32, f32, f32, f32) = (0.36666667, 0.8932311, 0.36666667 + 1.6783972 - 1.0666666, 1.);
    pub const BUCKET_50_100: f32 = 75.;
    pub const BUCKET_100_200: f32 = 150.;
    pub const FORWARD_250_ABOVE: f32 = 175.;
    pub const DISTANCE_TO_TARGET_Z_200UP: f32 = 75.;
    pub const DISTANCE_TO_TARGET_Z_100UP: f32 = 15.;
    pub const WARP_DISTANCE_250FWD: f32 = 165.;
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct HurdleMontage { pub name: &'static str, pub rate: f32, pub start_end: f32, pub near: (f32, f32), pub far: (f32, f32) }
    pub const MONTAGES: [HurdleMontage; 10] = [
    HurdleMontage { name: "Hurdle_60up_M", rate: 1.0, start_end: 0.13888885, near: (0.13888885, 0.7388976), far: (0.7388976, 1.3388889) },
    HurdleMontage { name: "Hurdle_100up_M", rate: 1.15, start_end: 0.16666667, near: (0.16666667, 0.7666641), far: (0.7666641, 1.3666667000000001) },
    HurdleMontage { name: "Hurdle_100Up_Mirrored_M", rate: 1.15, start_end: 0.16666667, near: (0.16666667, 0.7666667), far: (0.7666667, 1.3666667000000001) },
    HurdleMontage { name: "Hurdle_100up_Proto_V2_M", rate: 1.15, start_end: 0.15321009, near: (0.15321009, 0.7516597), far: (0.7516597, 1.35151515) },
    HurdleMontage { name: "Hurdle_100up_Proto_V2_Mirrored_M", rate: 1.15, start_end: 0.16666667, near: (0.16666667, 0.7666641), far: (0.7666641, 1.3666667000000001) },
    HurdleMontage { name: "Hurdle_200up_Proto_M", rate: 1.0, start_end: 0.36666667, near: (0.36666667, 1.066667), far: (1.066667, 1.83333325) },
    HurdleMontage { name: "Hurdle_100up_Narrow_M", rate: 1.15, start_end: 0.16666667, near: (0.16666667, 0.8725490229411765), far: (0.16666667, 0.8725490229411765) },
    HurdleMontage { name: "Hurdle_100up_Narrow_V2_M", rate: 1.15, start_end: 0.16666667, near: (0.16666667, 0.8725490229411765), far: (0.16666667, 0.8725490229411765) },
    HurdleMontage { name: "Hurdle_Crouch_100up_M", rate: 1.15, start_end: 0.16666667, near: (0.16666667, 0.7666641), far: (0.7666641, 1.3666668) },
    HurdleMontage { name: "Hurdle_Crouch_200up_Proto_M", rate: 1.0, start_end: 0.3, near: (0.3, 1.0), far: (1.0, 1.76666665) },
    ];
    pub const M_60UP: u8 = 0;
    pub const M_100UP: [u8; 4] = [1, 2, 3, 4];
    pub const M_200UP: u8 = 5;
    pub const M_NARROW: [u8; 2] = [6, 7];
    pub const M_CROUCH_100UP: u8 = 8;
    pub const M_CROUCH_200UP: u8 = 9;
    pub fn select(distance_to_target_z: f32, narrow: bool, crouched: bool, random4: u32, random2: u32) -> u8 {
        if narrow { return M_NARROW[(random2 % 2) as usize]; }
        if distance_to_target_z >= DISTANCE_TO_TARGET_Z_200UP { return if crouched { M_CROUCH_200UP } else { M_200UP }; }
        if distance_to_target_z >= DISTANCE_TO_TARGET_Z_100UP { return if crouched { M_CROUCH_100UP } else { M_100UP[(random4 % 4) as usize] }; }
        M_60UP
    }
    pub fn timing(montage: u8, far: bool) -> (f32, f32) {
        let m = MONTAGES[montage as usize];
        let (a, b) = if far { m.far } else { m.near };
        (m.start_end / m.rate, (m.start_end + (b - a)) / m.rate)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HurdleTarget { pub warp: Vec3, pub end: Vec3, pub height: f32, pub start_time: f32, pub duration: f32, pub montage: u8, pub far: bool }

pub fn find_hurdle(scene: &Scene, loc: Vec3, radius: f32, half_height: f32, facing: Vec3) -> Option<HurdleTarget> {
    find_hurdle_why(scene, loc, radius, half_height, facing, true).ok()
}

pub fn hurdle_phase4_landing(scene: &Scene, column: Vec3, r: f32) -> Result<(f32, f32), &'static str> {
    use hurdle::*;
    let land = scene.sweep(Capsule { c: column, r, hh: r }, Vec3::new(0., 0., -PHASE4_MAX_DOWN_DISTANCE));
    if land.start_penetrating { return Err("hurdle check 10"); }
    if land.blocking { return Ok((land.impact_point.z, land.impact_normal.z)); }
    if scene.line_down(column, MAX_ALLOWED_FALL_DISTANCE).is_none() { return Err("hurdle check 10"); }
    Ok((column.z - PHASE4_MAX_DOWN_DISTANCE, 1.))
}

pub fn hurdle_phase5_far_face(scene: &Scene, column: Vec3, land_z: f32, impact: Vec3, f: Vec3, r: f32, pawn_radius: f32) -> Result<Vec3, &'static str> {
    use hurdle::*;
    let hh = PHASE5_CLEARANCE_CAPSULE_SIZE * 0.5;
    let c = Vec3::new(column.x, column.y, land_z + PHASE5_CAPSULE_OFFSET + hh);
    let back = glam::Vec2::new(column.x - impact.x, column.y - impact.y).length() + pawn_radius;
    let hit = scene.sweep(Capsule { c, r, hh }, -f * back);
    if !hit.blocking || hit.start_penetrating { return Err("hurdle check 13"); }
    let p = hit.impact_point + f * FINAL_RESULT_GROUND_AIM_OFFSET;
    Ok(Vec3::new(p.x, p.y, land_z))
}

pub fn find_hurdle_why(scene: &Scene, loc: Vec3, radius: f32, half_height: f32, facing: Vec3, tactical_sprinting: bool) -> Result<HurdleTarget, &'static str> {
    use hurdle::*;
    let f = Vec3::new(facing.x, facing.y, 0.).normalize_or_zero();
    if f == Vec3::ZERO { return Err("hurdle check 1"); }
    let feet = loc.z - half_height;
    let r = CAST_RADIUS.min(MAXIMUM_CAPSULE_RADIUS);
    let p1_hh = ((SWEEP_GROUND_MAX_HEIGHT - SWEEP_GROUND_BUFFER_HEIGHT) * 0.5).min(MAXIMUM_CAPSULE_HALF_HEIGHT);
    let start = Vec3::new(loc.x, loc.y, feet + SWEEP_GROUND_BUFFER_HEIGHT + p1_hh);
    let hit = scene.sweep(Capsule { c: start, r, hh: p1_hh }, f * (radius + forward_cast(tactical_sprinting)));
    if !hit.blocking || hit.start_penetrating { return Err("hurdle check 2"); }
    let wn = hit.impact_normal.normalize_or_zero();
    if (-f).dot(wn).clamp(-1., 1.).acos().to_degrees() > WALL_ANGLE_TOLERANCE { return Err("hurdle check 3"); }
    if 90. - wn.z.clamp(-1., 1.).acos().to_degrees() < -MAX_VERTICAL_ANGLE_DEVIATION_DOWN_ON_NORMAL { return Err("hurdle check 4"); }
    let impact = hit.impact_point;
    let floor_from = impact - f * FLOOR_WALL_OFFSET;
    let floor = scene.line_down(floor_from, MAX_FLOOR_SEARCH_HEIGHT).map(|(_, at, n)| (at, n));
    if let Some((at, _)) = &floor { if impact.z - at.z < MIN_HEIGHT { return Err("hurdle check 5"); } }
    let top_from = Vec3::new(impact.x, impact.y, feet + SWEEP_GROUND_MAX_HEIGHT + r);
    let top_to = Vec3::new(impact.x + f.x * r, impact.y + f.y * r, feet + SWEEP_GROUND_BUFFER_HEIGHT);
    let down = scene.sweep(Capsule { c: top_from, r, hh: r }, top_to - top_from);
    if !down.blocking || down.start_penetrating { return Err("hurdle check 6"); }
    let top = down.impact_point.z;
    let top_flat = down.impact_normal.z.clamp(-1., 1.).acos().to_degrees() < UP_ANGLE_TOLERANCE;
    let mut height = top - feet;
    if let Some((at, n)) = &floor {
        height = top - at.z;
        let d = n.z;
        if d < HEIGHT_CORRECTION_MIN_FLOOR_ANGLE.to_radians().cos() {
            let horiz = glam::Vec2::new(top_from.x - at.x, top_from.y - at.y).length();
            height -= d.clamp(-1., 1.).acos().tan() * horiz;
        }
    }
    if height > MAX_HEIGHT { return Err("hurdle check 7"); }
    let p3_hh = (PHASE3_CLEARANCE_CAPSULE_SIZE * 0.5).min(MAXIMUM_CAPSULE_HALF_HEIGHT);
    let p3_start = Vec3::new(impact.x, impact.y, top + PHASE3_CAPSULE_OFFSET + p3_hh);
    let across = scene.sweep(Capsule { c: p3_start, r, hh: p3_hh }, f * (radius + PHASE3_FORWARD_CAST_DISTANCE));
    let over = !across.blocking;
    let column = if across.blocking {
        if across.start_penetrating { return Err("hurdle check 8"); }
        if glam::Vec2::new(across.impact_point.x - impact.x, across.impact_point.y - impact.y).length() < PHASE3_MIN_TOP_CLEARANCE_FOR_HURDLE_ON { return Err("hurdle check 9"); }
        let b = across.impact_point - f * (radius + 5.);
        Vec3::new(b.x, b.y, top + PHASE3_CAPSULE_OFFSET)
    } else {
        let e = p3_start + f * (radius + PHASE3_FORWARD_CAST_DISTANCE);
        Vec3::new(e.x, e.y, top + PHASE3_CAPSULE_OFFSET)
    };
    let (land_z, land_nz) = hurdle_phase4_landing(scene, column, r)?;
    if land_nz.clamp(-1., 1.).acos().to_degrees() >= PHASE4_UP_ANGLE_TOLERANCE { return Err("hurdle check 11"); }
    if top - PHASE4_HURDLE_ON_Z_TOLERANCE <= land_z && (!ALLOW_HURDLE_ON_TO || !top_flat) { return Err("hurdle check 12"); }
    let landing = if over { hurdle_phase5_far_face(scene, column, land_z, impact, f, r, radius)? } else { Vec3::new(column.x, column.y, land_z) };
    let end = Vec3::new(landing.x, landing.y, land_z + half_height + 1.);
    if scene.overlaps(&Capsule { c: end, r: radius, hh: half_height }) { return Err("hurdle check 14"); }
    let warp = Vec3::new(impact.x + f.x * r, impact.y + f.y * r, top + half_height + 1.);
    let far = Vec3::new(end.x - warp.x, end.y - warp.y, 0.).length() >= WARP_DISTANCE_250FWD;
    let montage = select(top - loc.z, false, false, 0, 0);
    let (start_time, duration) = timing(montage, far);
    Ok(HurdleTarget { warp, end, height, start_time, duration, montage, far })
}

#[derive(Clone, Debug, Default)]
pub struct Hurdle {
    pub moving: Option<(Vec3, HurdleTarget, f32, Vec3)>,
    pub count: u32,
    pub last_fail: Option<&'static str>,
    pub rng: u32,
}
impl Hurdle {
    pub fn is_moving(&self) -> bool { self.moving.is_some() }
    pub fn try_start(&mut self, cmc: &Cmc, tac_active: bool) -> bool {
        if self.moving.is_some() || (hurdle::TACTICAL_SPRINT_REQUIRED && !tac_active) || !cmc.is_moving_on_ground() || cmc.is_sliding() { return false; }
        let mut t = match find_hurdle_why(&cmc.scene, cmc.loc, cmc.p.radius, cmc.half_height, cmc.facing, tac_active) { Ok(t) => t, Err(why) => { self.last_fail = Some(why); return false } };
        self.rng = self.rng.wrapping_mul(1664525).wrapping_add(1013904223);
        t.montage = hurdle::select(t.warp.z - cmc.half_height - 1. - cmc.loc.z, false, cmc.is_crouching(), self.rng >> 16, self.rng >> 20);
        (t.start_time, t.duration) = hurdle::timing(t.montage, t.far);
        self.moving = Some((cmc.loc, t, 0., Vec3::new(cmc.velocity.x, cmc.velocity.y, 0.)));
        self.count += 1;
        true
    }
    pub fn advance(&mut self, dt: f32) -> Option<(Vec3, bool, Vec3)> {
        let (a, t, time, exit) = self.moving.as_mut()?;
        *time += dt;
        let s = |x: f32| x * x * (3. - 2. * x);
        let p = if *time < t.start_time {
            let u = (*time / t.start_time).clamp(0., 1.);
            let (h, v) = (s(u), s((u / clamber::RISE_FRACTION).min(1.)));
            Vec3::new(a.x + (t.warp.x - a.x) * h, a.y + (t.warp.y - a.y) * h, a.z + (t.warp.z - a.z) * v)
        } else {
            let u = ((*time - t.start_time) / (t.duration - t.start_time).max(1e-3)).clamp(0., 1.);
            let h = s(u);
            Vec3::new(t.warp.x + (t.end.x - t.warp.x) * h, t.warp.y + (t.end.y - t.warp.y) * h, t.warp.z + (t.end.z - t.warp.z) * u * u)
        };
        let done = *time >= t.duration;
        let v = *exit;
        if done { self.moving = None; }
        Some((p, done, v))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Enhanced { pub tac: TacSprint, pub clamber: Clamber, pub last_jumps: u32, pub slider: Slider, pub hurdle: Hurdle, pub sprint_toggle: bool, pub tac_shake: LegacyShake }
impl Enhanced {
    pub fn holster_weapon(&self) -> bool { self.tac.weapon_lowered || self.clamber.is_moving() }
}

pub mod shake {
    pub const TAC_LOOP_LOC: [(f32, f32); 3] = [(8., 1.), (2.74, 3.2), (1., 3.)];
    pub const TAC_LOOP_BLEND_IN: f32 = 1.0;
    pub const TAC_LOOP_BLEND_OUT: f32 = 0.28;
    pub const SLIDE_DURATION: f32 = 1.5;
    pub const SLIDE_BLEND_OUT: f32 = 5.0;
    pub const SLIDE_LOC_X: (f32, f32) = (200., 0.75);
    pub const SLIDE_FOV: (f32, f32) = (30., 2.);
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LegacyShake { pub time: f32, pub weight: f32 }
impl LegacyShake {
    pub fn tick(&mut self, playing: bool, dt: f32) {
        self.weight = if playing { (self.weight + dt / shake::TAC_LOOP_BLEND_IN).min(1.) } else { (self.weight - dt / shake::TAC_LOOP_BLEND_OUT).max(0.) };
        if self.weight > 0. { self.time += dt; } else { self.time = 0.; }
    }
    pub fn tac_loop_offset_cm(&self) -> Vec3 {
        let o = |k: (f32, f32)| k.0 * (self.time * k.1).sin();
        Vec3::new(o(shake::TAC_LOOP_LOC[0]), o(shake::TAC_LOOP_LOC[1]), o(shake::TAC_LOOP_LOC[2])) * self.weight
    }
}

pub fn slide_shake(t: f32) -> (f32, f32) {
    if t >= shake::SLIDE_DURATION { return (0., 0.); }
    let w = (shake::SLIDE_DURATION - t) / shake::SLIDE_BLEND_OUT;
    (shake::SLIDE_LOC_X.0 * w * (t * shake::SLIDE_LOC_X.1).sin(), shake::SLIDE_FOV.0 * w * (t * shake::SLIDE_FOV.1).sin())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmc::{Cmc, Params};
    use crate::scene::Frustum;
    use glam::Vec2;

    fn ti(wants: bool) -> TacInput { TacInput { wants, sprint_style: wants, grounded: true, falling: false, jumped: false, speed_2d: 500., blocked: None, toggle: false } }

    #[test] fn ramp_up_reaches_the_total_multiplier_at_ramp_speed() {
        let mut t = TacSprint::default();
        t.tick(&ti(true), 1. / 60.);
        assert_eq!(t.phase, TacPhase::Active);
        for _ in 0..12 { t.tick(&ti(true), 1. / 60.); }
        assert!((t.speed_mult - (1. + 12. / 60.)).abs() < 1e-4, "{}", t.speed_mult);
        for _ in 0..60 { t.tick(&ti(true), 1. / 60.); }
        assert_eq!(t.speed_mult, tac::SPEED_MULTIPLIER_TOTAL);
        let m = t.multipliers();
        assert_eq!((m.accel, m.jump_z), (tac::ACCELERATION_MULTIPLIER_TOTAL, tac::JUMP_Z_MULTIPLIER_TOTAL));
    }

    #[test] fn release_ramps_down_then_ends() {
        let mut t = TacSprint::default();
        for _ in 0..60 { t.tick(&ti(true), 1. / 60.); }
        t.tick(&ti(false), 1. / 60.);
        assert_eq!(t.phase, TacPhase::RampingDown);
        assert_eq!(t.multipliers().jump_z, 1.);
        for _ in 0..30 { t.tick(&ti(false), 1. / 60.); }
        assert_eq!(t.phase, TacPhase::Off);
        assert_eq!(t.multipliers(), ExtMultipliers::default());
    }

    #[test] fn energy_drains_per_second_and_jump_then_recharges_after_the_delay() {
        let mut t = TacSprint::default();
        for _ in 0..60 { t.tick(&ti(true), 1. / 60.); }
        assert!((t.energy - (100. - 15. * 59. / 60.)).abs() < 0.01, "{}", t.energy);
        let e = t.energy;
        t.tick(&TacInput { jumped: true, ..ti(true) }, 1. / 60.);
        assert!((e - t.energy - 10.25).abs() < 0.01);
        while t.phase == TacPhase::Active { t.tick(&ti(true), 1. / 60.); }
        assert_eq!(t.phase, TacPhase::Off);
        assert_eq!(t.energy, 0.);
        assert_eq!(t.last_end, Some("run out of energy"));
        for _ in 0..(60 * 4 - 2) { t.tick(&ti(false), 1. / 60.); }
        assert!(t.energy < 1.);
        for _ in 0..60 { t.tick(&ti(false), 1. / 60.); }
        assert!((t.energy - 45.).abs() < 1.5, "{}", t.energy);
        for _ in 0..(60 * 3) { t.tick(&ti(false), 1. / 60.); }
        assert_eq!(t.energy, tac::ENERGY_MAX);
    }

    #[test] fn low_energy_and_blocks_refuse_the_start() {
        let mut t = TacSprint { energy: 9.9, ..Default::default() };
        t.tick(&ti(true), 1. / 60.);
        assert_eq!(t.phase, TacPhase::Off);
        let mut t = TacSprint::default();
        t.tick(&TacInput { blocked: Some("DBNO or dead"), ..ti(true) }, 1. / 60.);
        assert_eq!(t.phase, TacPhase::Off);
        t.tick(&ti(true), 1. / 60.);
        t.tick(&TacInput { blocked: Some("emoting"), ..ti(true) }, 1. / 60.);
        assert_eq!((t.phase, t.last_end), (TacPhase::Off, Some("emoting")));
    }

    #[test] fn multipliers_raise_the_ground_speed_and_default_is_exact() {
        let mut c = Cmc::new(Params::default(), Scene::default(), Vec3::ZERO);
        c.wants_sprint = true;
        for _ in 0..120 { c.tick(Vec2::new(0., -1.), 1. / 60.); }
        let base = c.velocity.length();
        c.ext = ExtMultipliers { speed: tac::SPEED_MULTIPLIER_TOTAL, accel: tac::ACCELERATION_MULTIPLIER_TOTAL, jump_z: tac::JUMP_Z_MULTIPLIER_TOTAL };
        for _ in 0..120 { c.tick(Vec2::new(0., -1.), 1. / 60.); }
        assert!((c.velocity.length() - base * tac::SPEED_MULTIPLIER_TOTAL).abs() < 1., "{} vs {}", c.velocity.length(), base);
        let mut a = Cmc::new(Params::default(), Scene::default(), Vec3::ZERO);
        let mut b = Cmc::new(Params::default(), Scene::default(), Vec3::ZERO);
        b.ext = ExtMultipliers::default();
        for i in 0..300 { let inp = Vec2::new((i as f32 * 0.07).sin(), -1.); a.wants_sprint = i % 3 != 0; b.wants_sprint = a.wants_sprint; if i % 50 == 0 { a.jump(); b.jump(); } a.tick(inp, 1. / 60.); b.tick(inp, 1. / 60.); }
        assert_eq!((a.loc, a.velocity), (b.loc, b.velocity));
    }

    fn wall_scene(height: f32) -> Scene { Scene { solids: vec![Frustum { c: Vec2::new(0., -300.), z0: 0., z1: height, r0: 200., r1: 200. }], ..Default::default() } }

    #[test] fn a_ledge_in_reach_is_found_and_a_too_high_one_is_not() {
        let c = Cmc::new(Params::default(), wall_scene(150.), Vec3::new(0., -40., 0.));
        let l = find_ledge(&c.scene, c.loc, c.p.radius, c.half_height, Vec3::new(0., -1., 0.)).expect("ledge");
        assert!((l.height - 150.).abs() < 3., "{l:?}");
        assert!(l.end.y < -100. && l.end.z > 150. + c.half_height - 3., "{l:?}");
        let c = Cmc::new(Params::default(), wall_scene(400.), Vec3::new(0., -40., 0.));
        assert!(find_ledge(&c.scene, c.loc, c.p.radius, c.half_height, Vec3::new(0., -1., 0.)).is_none());
        let c = Cmc::new(Params::default(), wall_scene(150.), Vec3::new(0., -40., 0.));
        assert!(find_ledge(&c.scene, c.loc, c.p.radius, c.half_height, Vec3::new(1., 0., 0.)).is_none());
    }

    #[test] fn jump_at_a_ledge_clambers_onto_it_in_the_move_duration() {
        let mut c = Cmc::new(Params::default(), wall_scene(150.), Vec3::new(0., -40., 0.));
        let mut k = Clamber::default();
        assert!(!k.try_start(&c));
        k.press();
        assert!(k.try_start(&c));
        let duration = k.moving.unwrap().clip().duration();
        let mut t = 0.;
        let mut prev = c.loc;
        let mut peak = f32::MIN;
        loop {
            let (p, done, v) = k.advance(1. / 60.).expect("moving");
            t += 1. / 60.;
            assert!((p - prev).length() < 12., "step {} at {}", (p - prev).length(), t);
            prev = p;
            peak = peak.max(p.z - c.half_height);
            c.clamber_move(p);
            if done { c.clamber_finish(); c.velocity = v; break; }
            assert!(t < 1.);
        }
        assert!((t - duration).abs() < 1. / 59.);
        assert!(peak < 150. + 20., "peak feet {}", peak);
        for _ in 0..60 { c.tick(Vec2::ZERO, 1. / 60.); }
        assert!(c.is_moving_on_ground(), "{:?}", c.mode);
        assert!((c.feet_z() - 150.).abs() < 4., "{}", c.feet_z());
        assert!(!k.try_start(&c));
    }

    #[test] fn clamber_root_follows_the_montage_and_lands_the_warp_point_on_the_ledge_edge() {
        for (height, clip) in [(60., &CLAMBER_100), (120., &CLAMBER_150), (220., &CLAMBER_250)] {
            let m = ClamberMove { start_feet: Vec3::new(0., -60., 0.), warp: Vec3::new(0., 0., height), forward: Vec3::new(0., 1., 0.), half_height: 90., height, t: 0., last: Vec3::ZERO };
            assert_eq!(m.clip(), clip);
            assert!((m.feet_at(0.) - m.start_feet).length() < 1e-3);
            let we = clip.warp_end / clamber::PLAY_RATE;
            let r = clip.root_at(clip.anim_start + clip.warp_end);
            let wp = Vec3::from(clip.warp_point);
            let anim_edge = m.feet_at(we) + Vec3::new(wp.x - r.x, wp.y - r.y, wp.z - r.z);
            assert!((anim_edge - m.warp).length() < 0.5, "{height}: {anim_edge:?}");
            let end = m.feet_at(clip.duration());
            let rend = clip.root_at(clip.anim_end);
            assert!((end.z - (height + rend.z - wp.z)).abs() < 0.5, "{height}: {end:?}");
            assert!(end.z > height && end.z < height + 16., "{height}: {end:?}");
        }
        assert!((CLAMBER_250.duration() - (clamber::MOVE_DURATION + clamber::MONTAGE_BLEND_PADDING)).abs() < 1e-4);
    }
    fn sprint_cmc(speed_mult: f32) -> Cmc {
        let mut c = Cmc::new(Params::default(), Scene::default(), Vec3::ZERO);
        c.wants_sprint = true;
        c.ext = ExtMultipliers { speed: speed_mult, accel: tac::ACCELERATION_MULTIPLIER_TOTAL, jump_z: 1. };
        for _ in 0..180 { c.tick(Vec2::new(0., -1.), 1. / 60.); }
        c
    }

    #[test] fn slide_starts_above_the_min_speed_holds_through_the_boost_then_ends_below_min_forward_speed() {
        let mut c = sprint_cmc(tac::SPEED_MULTIPLIER_TOTAL);
        let v0 = Vec2::new(c.velocity.x, c.velocity.y).length();
        assert!(v0 > slide::MIN_SPEED_TO_START_SLIDING, "{v0}");
        let mut s = Slider::default();
        s.update(&mut c, true, false, false, 1. / 60.);
        assert!(c.is_sliding());
        c.wants_crouch = true;
        let mut t = 0.;
        let mut at_boost_end = 0.;
        while c.is_sliding() {
            c.tick(Vec2::ZERO, 1. / 60.);
            s.update(&mut c, true, false, false, 1. / 60.);
            t += 1. / 60.;
            if (t - slide::DASH_PERIOD).abs() < 0.5 / 60. { at_boost_end = Vec2::new(c.velocity.x, c.velocity.y).length(); }
            assert!(t < 10.);
        }
        assert!(at_boost_end > v0 * 0.75 && at_boost_end <= v0 + 1., "{at_boost_end} vs {v0}");
        assert!(t > slide::DASH_PERIOD && t < 4., "{t}");
        assert_eq!(s.last_end, Some("phys resolution requests sliding end"));
        assert!(c.is_crouching());
    }

    #[test] fn slow_pawns_cannot_slide_and_fatigue_counts_back_to_back_slides() {
        let mut c = Cmc::new(Params::default(), Scene::default(), Vec3::ZERO);
        for _ in 0..120 { c.tick(Vec2::new(0., -0.3), 1. / 60.); }
        let mut s = Slider::default();
        s.update(&mut c, true, false, false, 1. / 60.);
        assert!(!c.is_sliding());
        let mut c = sprint_cmc(tac::SPEED_MULTIPLIER_TOTAL);
        let mut s = Slider::default();
        s.update(&mut c, true, false, false, 1. / 60.);
        assert_eq!(c.slide.unwrap().fatigue, 0);
        s.update(&mut c, false, true, false, 1. / 60.);
        assert!(!c.is_sliding());
        assert_eq!(s.last_end, Some("jumping out of slide"));
        c.un_crouch();
        s.update(&mut c, true, false, false, 1. / 60.);
        assert_eq!(c.slide.unwrap().fatigue, 1);
    }

    #[test] fn slide_friction_follows_the_slope_and_state() {
        let mut c = sprint_cmc(1.);
        c.slide = Some(SlideRun::new(0, Vec2::new(0., -1.)));
        let (f_boost, b) = c.slide_pre_iteration(1. / 60.);
        assert_eq!(b, slide::BRAKING_DECELERATION);
        assert!(f_boost.abs() < 1e-6, "{f_boost}");
        let mut r = c.slide.unwrap(); r.state = SlideState::NoBoost; c.slide = Some(r);
        let (f, _) = c.slide_pre_iteration(1. / 60.);
        assert!((f - slide::LEVEL_GROUND_FRICTION_MULTIPLIER * c.p.ground_friction).abs() < 1e-5, "{f}");
        let mut r = c.slide.unwrap(); r.fatigue = 2; c.slide = Some(r);
        let (f2, _) = c.slide_pre_iteration(1. / 60.);
        assert!((f2 - f * 9.).abs() < 1e-4, "{f2}");
        assert!((slide::speed_added_friction(700.) - 0.0104165).abs() < 1e-5);
    }

    #[test] fn slide_input_only_brakes_and_steers_within_the_rotation_limit() {
        let mut c = sprint_cmc(tac::SPEED_MULTIPLIER_TOTAL);
        let mut s = Slider::default();
        s.update(&mut c, true, false, false, 1. / 60.);
        c.wants_crouch = true;
        for _ in 0..90 { c.tick(Vec2::new(1., 0.), 1. / 60.); s.update(&mut c, true, false, false, 1. / 60.); }
        let d = Vec2::new(c.velocity.x, c.velocity.y).normalize();
        let ang = d.dot(Vec2::new(0., -1.)).acos().to_degrees();
        assert!(ang > 10. && ang <= slide::MAX_ROTATION_ANGLE_ON_ACCELERATION + 0.5, "{ang}");
    }

    fn fence_scene(height: f32) -> Scene { Scene { solids: vec![Frustum { c: Vec2::new(0., -300.), z0: 0., z1: height, r0: 40., r1: 40. }], ..Default::default() } }

    #[test] fn hurdle_needs_tac_sprint_and_clears_a_fence_in_the_montage_time() {
        let mut c = Cmc::new(Params::default(), fence_scene(100.), Vec3::new(0., -130., 0.));
        c.facing = Vec3::new(0., -1., 0.);
        let mut h = Hurdle::default();
        assert!(!h.try_start(&c, false));
        assert!(h.try_start(&c, true));
        let target = h.moving.unwrap().1;
        assert!((target.height - 100.).abs() < 3., "{target:?}");
        assert!(hurdle::M_60UP == target.montage || hurdle::M_100UP.contains(&target.montage), "{target:?}");
        let (st, du) = hurdle::timing(target.montage, target.far);
        assert!((target.duration - du).abs() < 1e-5 && (target.start_time - st).abs() < 1e-5);
        let mut t = 0.;
        loop {
            let (p, done, v) = h.advance(1. / 60.).expect("moving");
            t += 1. / 60.;
            c.clamber_move(p);
            if done { c.clamber_finish(); c.velocity = v; break; }
            assert!(t < 2.);
        }
        assert!(c.loc.y < -340., "{:?}", c.loc);
        assert!(c.feet_z().abs() < 3., "{}", c.feet_z());
        assert!(c.is_moving_on_ground());
        let c = Cmc::new(Params::default(), wall_scene(100.), Vec3::new(0., -40., 0.));
        assert!(find_hurdle(&c.scene, c.loc, c.p.radius, c.half_height, Vec3::new(0., -1., 0.)).is_none());
        let c = Cmc::new(Params::default(), fence_scene(260.), Vec3::new(0., -130., 0.));
        assert!(find_hurdle(&c.scene, c.loc, c.p.radius, c.half_height, Vec3::new(0., -1., 0.)).is_none());
    }

    fn post_scene(r: f32, height: f32) -> Scene { Scene { solids: vec![Frustum { c: Vec2::new(0., -300.), z0: 0., z1: height, r0: r, r1: r }], ..Default::default() } }
    fn probe(scene: Scene) -> Result<HurdleTarget, &'static str> { let c = Cmc::new(Params::default(), scene, Vec3::new(0., -130., 0.)); find_hurdle_why(&c.scene, c.loc, c.p.radius, c.half_height, Vec3::new(0., -1., 0.), true) }

    #[test] fn hurdle_phase1_2_thin_fence_is_found_from_the_impact_column() {
        let t = probe(post_scene(8., 120.)).expect("a 16 cm thick fence must hurdle");
        assert!((t.height - 120.).abs() < 3., "{t:?}");
    }

    #[test] fn hurdle_phase2_rejects_a_top_above_the_max_height() {
        assert_eq!(probe(post_scene(8., 230.)).unwrap_err(), "hurdle check 7");
    }

    #[test] fn hurdle_phase3_4_reject_a_deep_platform_without_hurdle_on() {
        let e = probe(post_scene(120., 100.)).unwrap_err();
        assert!(e == "hurdle check 9" || e == "hurdle check 12", "{e}");
    }

    #[test] fn hurdle_phase4_lands_on_the_ground_behind_the_fence() {
        let mut s = post_scene(8., 100.);
        s.solids.push(Frustum { c: Vec2::new(0., -300.), z0: -400., z1: -399., r0: 1., r1: 1. });
        let c = Cmc::new(Params::default(), s, Vec3::new(0., -130., 0.));
        let mut sc = c.scene.clone();
        sc.terrain = None;
        assert!(find_hurdle_why(&sc, c.loc, c.p.radius, c.half_height, Vec3::new(0., -1., 0.), true).is_ok());
    }

    #[test] fn hurdle_phase5_lands_at_the_far_face_plus_the_ground_aim_offset() {
        let t = probe(post_scene(8., 120.)).unwrap();
        assert!((t.end.y - (-308. - hurdle::FINAL_RESULT_GROUND_AIM_OFFSET)).abs() < 2., "{t:?}");
        let s = post_scene(8., 120.);
        assert_eq!(hurdle_phase5_far_face(&s, Vec3::new(0., -700., 50.), 0., Vec3::new(0., -600., 50.), Vec3::new(0., -1., 0.), 15., 42.).unwrap_err(), "hurdle check 13");
    }

    #[test] fn hurdle_phase4_without_ground_uses_the_max_allowed_fall_distance() {
        let s = Scene::default();
        assert_eq!(hurdle_phase4_landing(&s, Vec3::new(0., 0., 1000.), 15.).unwrap(), (750., 1.));
        assert_eq!(hurdle_phase4_landing(&s, Vec3::new(0., 0., 1500.), 15.).unwrap_err(), "hurdle check 10");
        assert!((hurdle_phase4_landing(&s, Vec3::new(0., 0., 100.), 15.).unwrap().0).abs() < 1e-3);
    }

    #[test] fn hurdle_cast_distance_follows_tactical_sprinting() {
        assert_eq!(hurdle::forward_cast(true), hurdle::FORWARD_CAST_DISTANCE_MANUAL);
        assert_eq!(hurdle::forward_cast(false), hurdle::FORWARD_CAST_DISTANCE);
    }

    #[test] fn crouched_pawns_may_hurdle_and_pick_the_crouch_montages() {
        assert_eq!(hurdle::select(20., false, true, 0, 0), hurdle::M_CROUCH_100UP);
    }

    #[test] fn hurdle_montage_selection_follows_the_30_blueprint() {
        assert_eq!(hurdle::select(10., false, false, 0, 0), hurdle::M_60UP);
        assert_eq!(hurdle::select(15., false, false, 3, 0), hurdle::M_100UP[3]);
        assert_eq!(hurdle::select(74., false, true, 0, 0), hurdle::M_CROUCH_100UP);
        assert_eq!(hurdle::select(75., false, false, 0, 0), hurdle::M_200UP);
        assert_eq!(hurdle::select(80., false, true, 0, 0), hurdle::M_CROUCH_200UP);
        assert_eq!(hurdle::select(80., true, false, 0, 1), hurdle::M_NARROW[1]);
        let (st, du) = hurdle::timing(hurdle::M_100UP[0], true);
        assert!((st - 0.16666667 / 1.15).abs() < 1e-6 && (du - (0.16666667 + 0.6) / 1.15).abs() < 1e-4);
    }

    #[test] fn weapon_is_holstered_while_tac_sprinting_fast_and_for_the_whole_clamber() {
        let mut e = Enhanced::default();
        for _ in 0..30 { e.tac.tick(&ti(true), 1. / 60.); }
        assert!(e.holster_weapon());
        e.tac.tick(&TacInput { speed_2d: 50., ..ti(true) }, 1. / 60.);
        assert!(!e.holster_weapon());
        e.clamber.moving = Some(ClamberMove { start_feet: Vec3::ZERO, warp: Vec3::Z, forward: Vec3::X, half_height: 90., height: 100., t: 0., last: Vec3::ZERO });
        assert!(e.holster_weapon());
    }

    #[test] fn toggle_sprint_press_expires_after_the_grace_period() {
        let mut t = TacSprint::default();
        let idle = TacInput { wants: true, sprint_style: false, toggle: true, ..ti(true) };
        for _ in 0..40 { t.tick(&idle, 1. / 60.); }
        t.tick(&TacInput { toggle: true, ..ti(true) }, 1. / 60.);
        assert_eq!(t.phase, TacPhase::Off);
        let mut t = TacSprint::default();
        for _ in 0..20 { t.tick(&idle, 1. / 60.); }
        t.tick(&TacInput { toggle: true, ..ti(true) }, 1. / 60.);
        assert_eq!(t.phase, TacPhase::Active);
    }

    #[test] fn controller_slides_only_with_enhanced_on() {
        for on in [false, true] {
            let mut k = crate::Controller::default();
            if on { k.enhanced = Some(Enhanced::default()); }
            for _ in 0..120 { k.step(Vec2::new(0., -1.), false, true, 1. / 60.); }
            k.wants_crouch = true;
            let mut slid = false;
            for _ in 0..30 { k.step(Vec2::new(0., -1.), false, true, 1. / 60.); slid |= k.component().is_sliding(); }
            assert_eq!(slid, on);
        }
    }
}
