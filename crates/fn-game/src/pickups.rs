//! PICKUPS (fn-game): the world item actor of Battle Royale (`AFortPickup` / `AFortPickupAthena`) as pure, renderer-free game logic. Units are game metres, Y up (the
//! pawn eye is `(x, y, z)` of [`crate::GameSim`]); the UE numbers below are in centimetres in the comments.
//!
//! WHAT IS RECOVERED (1.8 client exe, `FortniteClient-Win64-Shipping.exe`, decompiled; RVAs are `0x140000000`-based addresses of `C:\FortniteRef\decomp\named`):
//! * `AFortPickup::AFortPickup` (140677f80): root = the `TouchCapsule` ("CollisionCylinder"), radius **30**, half height **80**; a `UProjectileMovementComponent` ("ProjectileComp0")
//!   with `bShouldBounce`, `Bounciness 0.6`, `Friction 0.5`, `BounceVelocityStopSimulatingThreshold 20`, `ProjectileGravityScale` 1 (class default), `MaxSimulationTimeStep` 0.05 and
//!   `MaxSimulationIterations` 8 (class defaults).
//! * `FUN_140695f30` (`SetupForMovementCompToss`) + `FUN_1402ad0b0` (the launch-velocity solver): the toss velocity is the horizontal direction to `LootFinalPosition` at `distance /
//!   FlyTime` plus a vertical component of `tan(45 deg) * horizontal speed` (`ArcParam` 45 degrees, clamped to [0, 90]); `MaxSpeed` becomes the launch speed. `FlyTime` is
//!   `FUN_140686500`: `(tMax - tMin) of PickupSplineCurve * (1 + rand * (PickupSplineRandomMax - 1))` = 1.4049 s * [1, 1.05] with `DefaultGameData`'s `LootBackpackCurves` and 1.05.
//!   When the movement component stops (`FUN_14067d770`, an `OnProjectileStop`) the toss state becomes `AtRest` and the rest location is the root location.
//! * `AFortPickup::vfunc_102` (`NotifyHit`): the first bounce plays `DefaultGameData.PickupLandedSound` (`UI_DropWeaponImpact_Cue`) at the root, at most once per second.
//! * `AFortPlayerPawnAthena::OnCapsuleBeginOverlap` (14043ac20): auto pickup on touch happens when the overlapped actor is a pickup that the pawn did not drop itself, the pawn is moving
//!   (any velocity component > 1e-4), and the item type is **Ammo (1), Trap (14) or WorldResource (19)**; everything else (weapons, consumables) needs the Use interaction.
//! * `AFortPlayerPawn::ServerHandlePickup` (1407fef90) -> `FUN_1406957a0`: the pickup gets `PickupTarget` = the pawn, `bPickedUp`, `FlyTime` (the same random time when none is given),
//!   the pickup sound plays at once (`ClientPlaySound(ItemDefinition.PickupSound)`), further entries of a stack fly 0.3 s later each, the actor dies `FlyTime + 5 s` later.
//! * Flight (`FUN_14069a150` / `FUN_14069a460`): along the pawn-relative path with the distance fraction given by `LootBackpackCurves` (X component, [`fly_fraction`]).
//! * The player's interaction trace: `InteractDistance` 256, `InteractTraceExtent` 15, `InteractMultiplierForActiveInteraction` 3.0 (`AFortPlayerController::AFortPlayerController`, 140810140).
//!
//! NOT RECOVERABLE FROM THE CLIENT EXE (the spawn side runs on the server: `UFortKismetLibrary::K2_SpawnPickupInWorld` is a 3-byte stub in the shipping client): who chooses `LootFinalPosition` of
//! a player drop, the start offset of the pawn drop, the random yaw. Defaults used here and labelled so by the caller: the toss starts at the owner's capsule centre and lands
//! [`DEFAULT_TOSS_DISTANCE`] (512 cm, the exe's constant `DAT_143ff904c` used when final == initial) ahead.
//!
//! Simplifications (reported): the world is the flat baseplate (y = 0) plus nothing else (the original sweeps the capsule against the level; props are not blocking here); the flight to the
//! pawn is the native 5-point Catmull-Rom spline into the pawn's `LootEntryPosition` socket.
use glam::Vec3;

/// Touch capsule (`CollisionCylinder` of `AFortPickup`), metres.
pub const TOUCH_RADIUS: f32 = 0.30;
pub const TOUCH_HALF_HEIGHT: f32 = 0.80;
/// `UProjectileMovementComponent` values set by the `AFortPickup` constructor.
pub const BOUNCINESS: f32 = 0.6;
pub const FRICTION: f32 = 0.5;
/// `BounceVelocityStopSimulatingThreshold` (20 cm/s).
pub const STOP_THRESHOLD: f32 = 0.20;
/// `UProjectileMovementComponent` class defaults.
pub const MAX_SIM_STEP: f32 = 0.05;
pub const MAX_SIM_ITERATIONS: u32 = 8;
/// `ArcParam` passed to the launch solver (degrees).
pub const ARC_DEGREES: f32 = 45.0;
/// `DAT_143ff904c`: how far ahead a toss lands when final == initial (cm -> m).
pub const DEFAULT_TOSS_DISTANCE: f32 = 5.12;
/// `DAT_143fe3710`: the pickup actor lives this long after its flight (s).
pub const LIFESPAN_AFTER_FLIGHT: f32 = 5.0;
/// `DAT_144008e2c`: extra delay of every further entry of a stack (s).
pub const ENTRY_DELAY: f32 = 0.3;
/// `FortGameData.PickupSplineRandomMax` of `DefaultGameData` (the random factor of the fly time is in `[1, this]`).
pub const FLY_RANDOM_MAX: f32 = 1.05;
/// Time span of `LootBackpackCurves` (first key -0.0011812488 s, last 1.4036946 s).
pub const FLY_CURVE_SPAN: f32 = 1.4036946 + 0.0011812488;
pub const FLY_CURVE_START: f32 = -0.0011812488;
pub const SPLINE_OFFSET: f32 = 1.75;
/// `AFortPlayerController::InteractDistance` / `InteractTraceExtent` (cm -> m).
pub const INTERACT_DISTANCE: f32 = 2.56;
pub const INTERACT_TRACE_EXTENT: f32 = 0.15;
/// `EFortItemType` values of the auto-pickup mask `0x84002` (Ammo, Trap, WorldResource).
pub const AUTO_PICKUP_TYPES: [u8; 3] = [1, 14, 19];
/// Pawn capsule (`PlayerPawn_Athena_Generic` `CollisionCylinder`, `fn_movement::params`): radius / half height in metres.
pub const PAWN_RADIUS: f32 = fn_movement::params::CAPSULE_RADIUS / 100.; // 42 cm, `AFortPawn` ctor @1407c9ff0 (+0x694)
pub const PAWN_HALF_HEIGHT: f32 = 0.75;

/// `EFortPickupTossState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TossState { NotTossed, InProgress, AtRest }

/// Where the actor is in its life.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// On the ground (or flying through the air in a toss: see [`Pickup::toss`]).
    World,
    /// `bPickedUp`: flying to the pawn; `t` seconds since the flight started (negative = the stack delay), `fly_time` the total, `start` where it left.
    Flying { t: f32, fly_time: f32, start: Vec3 },
}

#[derive(Clone, Debug)]
pub struct Pickup {
    pub id: u64,
    /// Index into the host's item table.
    pub item: u32,
    pub count: u32,
    /// `EFortItemType` of the definition (auto pickup rule).
    pub item_type: u8,
    /// `EFortRarity` byte (0 Handmade .. 9).
    pub rarity: u8,
    /// Capsule centre (the actor location), metres.
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    pub toss: TossState,
    pub phase: Phase,
    /// Seconds since the actor was created.
    pub age: f32,
    /// `PawnWhoDroppedPickup == the local pawn`: no auto pickup for it.
    pub dropped_by_pawn: bool,
    /// `MaxSpeed` of the movement component (the launch speed; 0 = unlimited).
    pub max_speed: f32,
    /// Gravity of the movement component, m/s^2 (positive down).
    pub gravity: f32,
    /// The pawn capsule overlapped the touch capsule on the previous update (begin overlap = the edge).
    overlapping: bool,
    bounces: u32,
    /// Seconds the actor has been picked up (for the lifespan).
    picked_for: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// First bounce / landing: play `PickupLandedSound` (throttled to once a second world-wide, like `LastLandedSoundPlayTime`).
    Landed { id: u64, pos: Vec3 },
    /// The movement component stopped: `TossState::AtRest` at `pos`.
    Settled { id: u64, pos: Vec3 },
    /// The pawn picked it up (touch or the Use key): play the item's `PickupSound`, start the pickup FX; `fly_time` seconds until it arrives.
    PickedUp { id: u64, pos: Vec3, auto: bool, fly_time: f32 },
    /// The flight ended: give `count` of `item` to the pawn; the actor waits for [`PickupWorld::settle`] (`AFortPickup::vfunc_191`: what was not taken stays in the world).
    Arrived { id: u64, item: u32, count: u32 },
}

/// What the world needs to know about the pawn this frame.
#[derive(Clone, Copy, Debug)]
pub struct PawnState {
    /// Capsule centre.
    pub pos: Vec3,
    /// Camera eye and forward (the interaction trace).
    pub eye: Vec3,
    pub forward: Vec3,
    /// `CharacterMovement->Velocity` is not zero.
    pub moving: bool,
    pub loot: Vec3,
}

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// LootBackpackCurves (X component: the fraction of the path covered), RichCurve keys as cooked
// ---------------------------------------------------------------------------------------------------------------------------------------------------
/// `(time, value, arrive tangent, leave tangent, cubic)` of `/Game/GameplayCurves/LootBackpackCurves` `FloatCurves[0]`.
const CURVE_X: [(f32, f32, f32, f32, bool); 6] = [
    (-0.001_181_248_8, -0.000_079_999_445, 0.0, 0.0, false),
    (0.649_199_96, 0.767, 1.037_575_2, 1.037_575_2, true),
    (0.789_200_01, 0.82, 0.0, 0.0, false),
    (1.310_355_5, 0.848_848_6, 0.135_803_86, 0.135_803_86, true),
    (1.369_806_1, 0.898_848_5, 2.522_788_3, 2.522_788_3, false),
    (1.403_694_6, 0.999_950_05, 0.0, 0.0, false),
];

const CURVE_Y: [(f32, f32, f32, f32, bool); 2] = [
    (0.999_388_8, 0.0, 0.0, 0.0, true),
    (1.249_562_1, 1.0, 11.273_595, 0.0, true),
];

/// `FRichCurve::Eval` of those keys (linear / cubic Hermite with the tangents scaled by the key interval, constant outside).
pub fn fly_fraction(t: f32) -> f32 { eval_keys(&CURVE_X, t) }
pub fn black_hole_strength(t: f32) -> f32 { eval_keys(&CURVE_Y, t) }
pub fn curve_time(t: f32, fly_time: f32) -> f32 { FLY_CURVE_START + (t / fly_time.max(1e-6)).clamp(0., 1.) * FLY_CURVE_SPAN }

pub fn eval_keys(k: &[(f32, f32, f32, f32, bool)], t: f32) -> f32 {
    if t <= k[0].0 { return k[0].1; }
    if t >= k[k.len() - 1].0 { return k[k.len() - 1].1; }
    let i = (0..k.len() - 1).rposition(|i| k[i].0 <= t).unwrap_or(0);
    let (a, b) = (k[i], k[i + 1]);
    let diff = b.0 - a.0;
    let alpha = (t - a.0) / diff;
    if !a.4 { return a.1 + (b.1 - a.1) * alpha; }
    let (p0, t0, p1, t1) = (a.1, a.3 * diff, b.1, b.2 * diff);
    let (a2, a3) = (alpha * alpha, alpha * alpha * alpha);
    (2. * a3 - 3. * a2 + 1.) * p0 + (a3 - 2. * a2 + alpha) * t0 + (a3 - a2) * t1 + (-2. * a3 + 3. * a2) * p1
}

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// launch solver
// ---------------------------------------------------------------------------------------------------------------------------------------------------
fn catmull_rom(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> Vec3 {
    let (t2, t3) = (t * t, t * t * t);
    ((p2 - p0) * t + p1 * 2. + (p0 * 2. - p1 * 5. + p2 * 4. - p3) * t2 + (-p0 + p1 * 3. - p2 * 3. + p3) * t3) * 0.5
}

pub fn flight_point(start: Vec3, target: Vec3, x: f32) -> Vec3 {
    let up = Vec3::Y * SPLINE_OFFSET;
    let p = [start + up, start, target + up, target, target - up];
    let segments = (p.len() - 3) as f32;
    let u = (x.clamp(0., 1.) * segments).clamp(0., segments);
    if (u - segments).abs() <= 1e-8 { return p[p.len() - 2]; }
    let i = (u.floor() as usize).min(p.len() - 4);
    catmull_rom(p[i], p[i + 1], p[i + 2], p[i + 3], u - i as f32)
}

/// `FUN_1402ad0b0`: the launch velocity (m/s) of a toss from `start` to `end` that takes `fly_time` at a fixed `arc` angle: horizontal speed = horizontal distance / fly_time,
/// vertical = `tan(arc) * horizontal speed` (raised when the end is above the start). Degenerate cases (no horizontal distance / no time) fly straight at `speed`.
pub fn launch_velocity(start: Vec3, end: Vec3, fly_time: f32, arc_deg: f32) -> Vec3 {
    let d = end - start;
    let h = Vec3::new(d.x, 0., d.z);
    let speed3 = d.length() / fly_time.max(1e-6);
    if h.length() <= 1e-6 || fly_time <= 1e-6 || arc_deg <= 0. { return d.normalize_or_zero() * speed3; }
    let arc = (arc_deg.to_radians()).clamp(0., std::f32::consts::FRAC_PI_2);
    let t = fly_time; let inv = 1. / t;
    let hs = h.length() * inv; // horizontal speed
    let vz = arc.tan() * hs;
    let mut vy = vz;
    if d.y > 0. { vy = (t * vz * 0.25 + d.y) * inv * 4.; } // `(fVar9 * vz * 0.25 + dz) * (1/T) * 4`
    h.normalize() * hs + Vec3::Y * vy
}

/// Tiny deterministic generator for the fly time / yaw (the original uses the CRT `rand()`).
fn next(rng: &mut u32) -> f32 { *rng ^= *rng << 13; *rng ^= *rng >> 17; *rng ^= *rng << 5; (*rng & 0x7fff) as f32 / 32768. }

#[derive(Debug)]
pub struct PickupWorld {
    pub pickups: Vec<Pickup>,
    pub arrived: Vec<Pickup>,
    next_id: u64,
    rng: u32,
    /// World clock (s).
    pub time: f32,
    last_landed_sound: f32,
}
impl Default for PickupWorld { fn default() -> Self { PickupWorld { pickups: vec![], arrived: vec![], next_id: 1, rng: 0x9E37_79B9, time: 0., last_landed_sound: -10. } } }

fn capsule_overlap(a: Vec3, ar: f32, ahh: f32, b: Vec3, br: f32, bhh: f32) -> bool {
    // two vertical capsules: distance between their core segments
    let (sa, sb) = (ahh - ar, bhh - br);
    let dy = ((a.y - b.y).abs() - (sa + sb)).max(0.);
    let dh = ((a.x - b.x).powi(2) + (a.z - b.z).powi(2)).sqrt();
    (dh * dh + dy * dy).sqrt() <= ar + br
}

impl PickupWorld {
    pub fn new(seed: u32) -> PickupWorld { PickupWorld { rng: seed.max(1), ..Default::default() } }

    /// `FUN_140686500`: a random fly time.
    pub fn fly_time(&mut self) -> f32 { FLY_CURVE_SPAN * (1. + next(&mut self.rng) * (FLY_RANDOM_MAX - 1.)) }

    fn push(&mut self, item: u32, count: u32, item_type: u8, rarity: u8, pos: Vec3, dropped_by_pawn: bool) -> usize {
        let id = self.next_id; self.next_id += 1;
        let yaw = next(&mut self.rng) * std::f32::consts::TAU;
        self.pickups.push(Pickup { id, item, count, item_type, rarity, pos, vel: Vec3::ZERO, yaw, toss: TossState::NotTossed, phase: Phase::World, age: 0., dropped_by_pawn, max_speed: 0., gravity: 9.8, overlapping: false, bounces: 0, picked_for: 0. });
        self.pickups.len() - 1
    }

    /// A pickup lying still at `pos` (the capsule centre; `TossState::AtRest`): what a level placed loot item is.
    pub fn spawn_at_rest(&mut self, item: u32, count: u32, item_type: u8, rarity: u8, pos: Vec3) -> u64 {
        let i = self.push(item, count, item_type, rarity, pos, false);
        self.pickups[i].toss = TossState::AtRest;
        self.pickups[i].id
    }

    /// `AFortPickup::TossPickup(FinalLocation, ItemOwner, ...)`: the actor is created at `initial` (the owner's capsule centre) and launched towards `final_pos` (`FUN_140695f30`).
    /// `gravity` = the owner's `GetGravityZ()` magnitude in m/s^2.
    pub fn spawn_toss(&mut self, item: u32, count: u32, item_type: u8, rarity: u8, initial: Vec3, final_pos: Vec3, dropped_by_pawn: bool, gravity: f32) -> u64 {
        let fly = self.fly_time();
        let i = self.push(item, count, item_type, rarity, initial, dropped_by_pawn);
        let v = launch_velocity(initial, final_pos, fly, ARC_DEGREES);
        let p = &mut self.pickups[i];
        p.vel = v; p.max_speed = v.length(); p.gravity = gravity; p.toss = TossState::InProgress;
        p.id
    }

    pub fn spawn_launch(&mut self, item: u32, count: u32, item_type: u8, rarity: u8, initial: Vec3, velocity: Vec3, gravity: f32) -> u64 {
        let i = self.push(item, count, item_type, rarity, initial, false);
        let p = &mut self.pickups[i];
        p.vel = velocity; p.max_speed = velocity.length(); p.gravity = gravity; p.toss = TossState::InProgress;
        p.id
    }

    pub fn get(&self, id: u64) -> Option<&Pickup> { self.pickups.iter().find(|p| p.id == id) }
    pub fn get_arrived(&self, id: u64) -> Option<&Pickup> { self.arrived.iter().find(|p| p.id == id) }
    pub fn settle(&mut self, id: u64, taken: u32, rest: Vec3) -> bool {
        let Some(i) = self.arrived.iter().position(|p| p.id == id) else { return false };
        let mut p = self.arrived.remove(i);
        p.count = p.count.saturating_sub(taken);
        if p.count == 0 { return true; }
        p.phase = Phase::World; p.toss = TossState::AtRest; p.pos = rest; p.vel = Vec3::ZERO; p.overlapping = true; p.picked_for = 0.;
        self.pickups.push(p);
        true
    }
    pub fn len(&self) -> usize { self.pickups.len() }

    /// Starts the pickup flight of `id` towards the pawn (`ServerHandlePickup`). Returns the event the host plays the pickup sound / FX for; `None` when it is already flying.
    pub fn begin_pickup(&mut self, id: u64, auto: bool, fly_time: Option<f32>) -> Option<Event> {
        let fly = fly_time.unwrap_or_else(|| self.fly_time());
        let p = self.pickups.iter_mut().find(|p| p.id == id)?;
        if !matches!(p.phase, Phase::World) { return None; }
        p.phase = Phase::Flying { t: 0., fly_time: fly, start: p.pos };
        p.vel = Vec3::ZERO;
        Some(Event::PickedUp { id, pos: p.pos, auto, fly_time: fly })
    }

    /// The pickup the Use key would take: the aim trace (`InteractTraceExtent` sphere along the view for `InteractDistance` measured from the pawn) against the touch capsules.
    pub fn focus(&self, pawn: &PawnState) -> Option<u64> { self.focus_hit(pawn).map(|b| b.1) }

    pub fn focus_hit(&self, pawn: &PawnState) -> Option<(f32, u64)> {
        let dir = pawn.forward.normalize_or_zero();
        if dir == Vec3::ZERO { return None; }
        let reach = INTERACT_DISTANCE + (pawn.eye - pawn.pos).length();
        let mut best: Option<(f32, f32, u64)> = None;
        for p in &self.pickups {
            if !matches!(p.phase, Phase::World) { continue; }
            // the capsule's core segment and the ray
            let seg_half = TOUCH_HALF_HEIGHT - TOUCH_RADIUS;
            let (a, b) = (p.pos - Vec3::Y * seg_half, p.pos + Vec3::Y * seg_half);
            let (t, dist) = ray_segment(pawn.eye, dir, a, b);
            if t < 0. || t > reach || dist > TOUCH_RADIUS + INTERACT_TRACE_EXTENT { continue; }
            // reach is measured from the pawn
            let hit = pawn.eye + dir * t;
            let closest = Vec3::new(p.pos.x, hit.y.clamp(a.y, b.y), p.pos.z);
            if (closest - pawn.pos).length() > INTERACT_DISTANCE + TOUCH_RADIUS { continue; }
            let r = TOUCH_RADIUS + INTERACT_TRACE_EXTENT;
            let contact = (t - (r * r - dist * dist).max(0.).sqrt()).max(0.);
            if best.map_or(true, |(bt, _, _)| t < bt) { best = Some((t, contact, p.id)); }
        }
        best.map(|b| (b.1, b.2))
    }

    /// Advances every actor by `dt` seconds. `ground(p)` = the highest floor at or below point p. `can_pick(item, count)` is asked before an auto pickup starts (false = nothing happens).
    pub fn update(&mut self, dt: f32, pawn: &PawnState, ground: &dyn Fn(Vec3) -> f32, can_pick: &mut dyn FnMut(&Pickup) -> bool) -> Vec<Event> {
        let mut ev = vec![];
        self.time += dt;
        let time = self.time;
        let mut begin: Vec<u64> = vec![];
        for p in self.pickups.iter_mut() {
            p.age += dt;
            match p.phase {
                Phase::World => {
                    if p.toss == TossState::InProgress { step_projectile(p, dt, ground, time, &mut self.last_landed_sound, &mut ev); }
                    // begin overlap of the pawn's capsule with the touch capsule
                    let now = capsule_overlap(p.pos, TOUCH_RADIUS, TOUCH_HALF_HEIGHT, pawn.pos, PAWN_RADIUS, PAWN_HALF_HEIGHT);
                    let began = now && !p.overlapping;
                    p.overlapping = now;
                    if began && pawn.moving && !p.dropped_by_pawn && AUTO_PICKUP_TYPES.contains(&p.item_type) && can_pick(p) { begin.push(p.id); }
                }
                Phase::Flying { ref mut t, fly_time, start } => {
                    *t += dt; p.picked_for += dt;
                    if *t >= 0. { p.pos = flight_point(start, pawn.loot, fly_fraction(curve_time(*t, fly_time))); }
                }
            }
        }
        for id in begin { if let Some(e) = self.begin_pickup(id, true, None) { ev.push(e); } }
        // arrivals wait for the host's settle
        let mut keep = Vec::with_capacity(self.pickups.len());
        for p in self.pickups.drain(..) {
            match p.phase {
                Phase::Flying { t, fly_time, .. } if t >= fly_time => { ev.push(Event::Arrived { id: p.id, item: p.item, count: p.count }); self.arrived.push(p); }
                _ => keep.push(p),
            }
        }
        self.pickups = keep;
        ev
    }
}

/// Closest approach of the ray `o + t d` (d unit) to the segment `a..b`: `(t, distance)`.
fn ray_segment(o: Vec3, d: Vec3, a: Vec3, b: Vec3) -> (f32, f32) {
    let u = b - a; let w = o - a;
    let (uu, ud, dd, uw, dw) = (u.dot(u), u.dot(d), d.dot(d), u.dot(w), d.dot(w));
    let den = uu * dd - ud * ud;
    let s = if den.abs() < 1e-9 { 0. } else { (((uw * dd) - (dw * ud)) / den).clamp(0., 1.) };
    let t = ((d.dot(a + u * s - o)) / dd).max(0.);
    let dist = ((o + d * t) - (a + u * s)).length();
    (t, dist)
}

/// One `UProjectileMovementComponent::TickComponent` for a toss (4.16: Velocity-Verlet move delta, sweep, bounce with `Bounciness` / `Friction`, stop under the threshold).
fn step_projectile(p: &mut Pickup, dt: f32, ground: &dyn Fn(Vec3) -> f32, now: f32, last_landed: &mut f32, ev: &mut Vec<Event>) {
    let mut remaining = dt;
    let mut iterations = 0;
    while remaining >= 1e-4 && iterations < MAX_SIM_ITERATIONS {
        iterations += 1;
        let tick = remaining.min(MAX_SIM_STEP);
        remaining -= tick;
        let old_v = p.vel;
        let new_v = limit(old_v + Vec3::NEG_Y * p.gravity * tick, p.max_speed); // ComputeVelocity
        let delta = old_v * tick + (new_v - old_v) * (0.5 * tick); // ComputeMoveDelta
        // sweep the capsule against the floor: its bottom is `TOUCH_HALF_HEIGHT` below the centre
        let floor_c = ground(Vec3::new(p.pos.x + delta.x, p.pos.y, p.pos.z + delta.z)) + TOUCH_HALF_HEIGHT;
        let end_y = p.pos.y + delta.y;
        if end_y > floor_c || delta.y >= 0. && p.pos.y > floor_c {
            p.pos += delta; p.vel = new_v;
            continue;
        }
        // blocking hit
        let t_hit = if delta.y < -1e-9 { ((p.pos.y - floor_c) / -delta.y).clamp(0., 1.) } else { 0. };
        p.pos += delta * t_hit; p.pos.y = p.pos.y.max(floor_c);
        let v = if t_hit < 1. { limit(old_v + Vec3::NEG_Y * p.gravity * tick * t_hit, p.max_speed) } else { new_v };
        // ComputeBounceResult (normal = +Y)
        let n = Vec3::Y;
        let vdotn = v.dot(n);
        let mut out = v;
        if vdotn <= 0. {
            let projected = n * -vdotn;
            let mut tangent = v + projected;
            tangent *= (1. - FRICTION).clamp(0., 1.);
            out = tangent + projected * BOUNCINESS.max(0.);
        }
        out = limit(out, p.max_speed);
        p.bounces += 1;
        if p.bounces == 1 && now - *last_landed > 1.0 { *last_landed = now; ev.push(Event::Landed { id: p.id, pos: p.pos }); }
        if out.length_squared() < STOP_THRESHOLD * STOP_THRESHOLD {
            p.vel = Vec3::ZERO; p.toss = TossState::AtRest;
            ev.push(Event::Settled { id: p.id, pos: p.pos });
            return;
        }
        p.vel = out;
        remaining += tick * (1. - t_hit);
    }
}

fn limit(v: Vec3, max: f32) -> Vec3 { if max > 0. && v.length() > max { v.normalize() * max } else { v } }

#[cfg(test)]
mod tests {
    use super::*;
    const G: f32 = 9.8;
    fn pawn_at(p: Vec3, moving: bool) -> PawnState { PawnState { pos: p, eye: p + Vec3::new(0., 1.0, 0.), forward: Vec3::new(0., 0., -1.), moving, loot: p + Vec3::new(0., 0.45, 0.) } }
    fn run(w: &mut PickupWorld, pawn: &PawnState, secs: f32) -> Vec<Event> { let mut all = vec![]; let n = (secs * 60.) as usize; for _ in 0..n { all.extend(w.update(1. / 60., pawn, &|_| 0., &mut |_| true)); } all }

    #[test] fn the_loot_curve_is_the_cooked_one() {
        assert!((fly_fraction(-0.1) - -0.00008).abs() < 1e-4);
        assert!((fly_fraction(0.6492) - 0.767).abs() < 1e-5, "the cubic key");
        assert!((fly_fraction(0.7892) - 0.82).abs() < 1e-5);
        assert!((fly_fraction(2.0) - 0.99995).abs() < 1e-5);
        let mut last = -1.;
        for i in 0..=140 { let v = fly_fraction(i as f32 * 0.01); assert!(v >= last - 1e-4, "monotonic at {i}: {v} < {last}"); last = v; }
        assert!((FLY_CURVE_SPAN - 1.404_875_8).abs() < 1e-5);
    }
    #[test] fn the_flight_is_the_catmull_rom_spline_through_start_and_loot_point() {
        let (s, t) = (Vec3::new(2., 0.8, 0.), Vec3::new(0., 1.2, 0.));
        assert!((flight_point(s, t, 0.) - s).length() < 1e-6);
        assert!((flight_point(s, t, 1.) - t).length() < 1e-6);
        assert!((flight_point(s, t, 0.5) - (t + Vec3::Y * SPLINE_OFFSET)).length() < 1e-5, "half way = the point 175 cm above the loot entry");
        let q = flight_point(s, t, 0.25);
        assert!(q.y > s.y + 0.5 && q.x < s.x && q.x > t.x, "the first segment rises towards the point above the pawn: {q:?}");
        let r = flight_point(s, t, 0.9);
        assert!(r.y > t.y && r.y < t.y + SPLINE_OFFSET && r.x.abs() < 0.2, "the second segment drops into the backpack from above: {r:?}");
    }
    #[test] fn the_black_hole_strength_is_the_y_curve() {
        assert_eq!(black_hole_strength(0.5), 0.);
        assert_eq!(black_hole_strength(0.999), 0.);
        assert_eq!(black_hole_strength(1.3), 1.);
        let m = black_hole_strength(1.12);
        assert!(m > 0.1 && m < 0.9, "{m}");
        assert!((curve_time(1.4, 1.4) - 1.4036946).abs() < 1e-5);
    }
    #[test] fn fly_time_is_the_curve_span_times_one_to_one_point_oh_five() {
        let mut w = PickupWorld::new(7);
        for _ in 0..200 { let t = w.fly_time(); assert!((1.4048..=1.4752).contains(&t), "{t}"); }
    }
    #[test] fn launch_velocity_is_45_degrees_at_distance_over_flytime() {
        let v = launch_velocity(Vec3::new(0., 0.9, 0.), Vec3::new(0., 0.8, -5.12), 1.4, 45.);
        assert!((Vec3::new(v.x, 0., v.z).length() - 5.12 / 1.4).abs() < 1e-4);
        assert!((v.y - Vec3::new(v.x, 0., v.z).length()).abs() < 1e-4, "tan 45 = 1");
        assert!(v.z < 0.);
        let up = launch_velocity(Vec3::ZERO, Vec3::new(0., 1., 3.), 1., 45.);
        assert!(up.y > 3.0, "a target above gets a steeper launch: {up:?}");
        assert_eq!(launch_velocity(Vec3::ZERO, Vec3::new(0., 1., 0.), 1., 45.).y, 1.0, "no horizontal distance: straight line");
    }
    #[test] fn a_toss_bounces_once_with_the_recovered_restitution_and_comes_to_rest() {
        let mut w = PickupWorld::new(3);
        let id = w.spawn_toss(0, 1, 17, 2, Vec3::new(0., 1.0, 0.), Vec3::new(0., 0.8, -5.12), true, G);
        let pawn = pawn_at(Vec3::new(30., 0.75, 30.), false);
        let ev = run(&mut w, &pawn, 12.);
        assert!(ev.iter().any(|e| matches!(e, Event::Landed { id: i, .. } if *i == id)), "the first bounce plays the landed sound");
        assert!(ev.iter().filter(|e| matches!(e, Event::Landed { .. })).count() == 1);
        assert!(ev.iter().any(|e| matches!(e, Event::Settled { id: i, .. } if *i == id)), "{ev:?}");
        let p = w.get(id).unwrap();
        assert_eq!(p.toss, TossState::AtRest);
        assert!((p.pos.y - TOUCH_HALF_HEIGHT).abs() < 1e-4, "the 160 cm touch capsule rests on the floor: centre at {}", p.pos.y);
        assert!(p.pos.z < -1.0 && p.pos.z > -7., "it travelled forward: {:?}", p.pos);
    }
    #[test] fn bounce_reflects_with_bounciness_and_friction() {
        let mut p = Pickup { id: 1, item: 0, count: 1, item_type: 17, rarity: 0, pos: Vec3::new(0., 0.8, 0.), vel: Vec3::new(2., -5., 0.), yaw: 0., toss: TossState::InProgress, phase: Phase::World, age: 0., dropped_by_pawn: false, max_speed: 0., gravity: 0., overlapping: false, bounces: 0, picked_for: 0. };
        let mut ev = vec![]; let mut last = -10.;
        step_projectile(&mut p, 0.01, &|_| 0., 5., &mut last, &mut ev);
        assert!((p.vel.y - 3.0).abs() < 1e-4, "vy 5 -> 0.6 * 5: {:?}", p.vel);
        assert!((p.vel.x - 1.0).abs() < 1e-4, "vx 2 -> * (1 - 0.5): {:?}", p.vel);
    }
    #[test] fn landed_sound_is_throttled_to_once_a_second() {
        let mut w = PickupWorld::new(5);
        w.spawn_toss(0, 1, 17, 0, Vec3::new(0., 1.0, 0.), Vec3::new(1., 0.8, -4.), true, G);
        w.spawn_toss(1, 1, 17, 0, Vec3::new(0., 1.0, 0.), Vec3::new(-1., 0.8, -4.), true, G);
        let pawn = pawn_at(Vec3::new(50., 0.75, 50.), false);
        let ev = run(&mut w, &pawn, 6.);
        assert_eq!(ev.iter().filter(|e| matches!(e, Event::Landed { .. })).count(), 1, "two pickups landing together make one sound");
    }
    #[test] fn only_ammo_traps_and_resources_are_picked_up_by_walking_into_them_and_only_while_moving() {
        for (ty, auto) in [(1u8, true), (14, true), (19, true), (17, false), (10, false), (15, false)] {
            let mut w = PickupWorld::new(1);
            w.spawn_at_rest(0, 5, ty, 0, Vec3::new(0., TOUCH_HALF_HEIGHT, 0.));
            let still = w.update(1. / 60., &pawn_at(Vec3::new(0.2, 0.75, 0.), false), &|_| 0., &mut |_| true);
            assert!(still.is_empty(), "standing still is no begin-overlap trigger");
            let mut w = PickupWorld::new(1);
            w.spawn_at_rest(0, 5, ty, 0, Vec3::new(0., TOUCH_HALF_HEIGHT, 0.));
            let ev = w.update(1. / 60., &pawn_at(Vec3::new(0.2, 0.75, 0.), true), &|_| 0., &mut |_| true);
            assert_eq!(ev.iter().any(|e| matches!(e, Event::PickedUp { auto: true, .. })), auto, "type {ty}");
        }
    }
    #[test] fn a_pickup_the_pawn_dropped_is_never_auto_picked_up() {
        let mut w = PickupWorld::new(1);
        let id = w.spawn_toss(0, 5, 1, 0, Vec3::new(0., 1.0, 0.), Vec3::new(0., 0.8, -5.), true, G);
        let ev = run(&mut w, &pawn_at(Vec3::new(0., 0.75, -0.5), true), 3.);
        assert!(!ev.iter().any(|e| matches!(e, Event::PickedUp { .. })));
        assert!(w.get(id).is_some());
    }
    #[test] fn begin_overlap_is_an_edge_and_the_host_can_veto() {
        let mut w = PickupWorld::new(1);
        w.spawn_at_rest(0, 5, 1, 0, Vec3::new(0., TOUCH_HALF_HEIGHT, 0.));
        let pawn = pawn_at(Vec3::new(0.2, 0.75, 0.), true);
        assert!(w.update(1. / 60., &pawn, &|_| 0., &mut |_| false).is_empty(), "can_pick false: nothing starts");
        assert!(w.update(1. / 60., &pawn, &|_| 0., &mut |_| true).is_empty(), "the overlap already began: no second begin event");
        assert!(w.update(1. / 60., &pawn_at(Vec3::new(5., 0.75, 0.), true), &|_| 0., &mut |_| true).is_empty());
        assert!(!w.update(1. / 60., &pawn, &|_| 0., &mut |_| true).is_empty(), "leaving and entering again begins a new overlap");
    }
    #[test] fn a_picked_up_item_flies_along_the_curve_and_arrives_after_the_fly_time() {
        let mut w = PickupWorld::new(2);
        let id = w.spawn_at_rest(7, 3, 17, 4, Vec3::new(2., TOUCH_HALF_HEIGHT, 0.));
        let ev = w.begin_pickup(id, false, Some(1.4)).unwrap();
        assert!(matches!(ev, Event::PickedUp { fly_time, auto: false, .. } if fly_time == 1.4));
        assert!(w.begin_pickup(id, false, None).is_none(), "already flying");
        let pawn = pawn_at(Vec3::new(0., 0.75, 0.), false);
        let mut arrived = None; let mut t = 0.0f32;
        let mut mid = Vec3::ZERO;
        while arrived.is_none() && t < 3. {
            for e in w.update(1. / 60., &pawn, &|_| 0., &mut |_| true) { if let Event::Arrived { item, count, .. } = e { arrived = Some((item, count, t)); } }
            t += 1. / 60.;
            if (t - 0.65).abs() < 0.009 { mid = w.get(id).map(|p| p.pos).unwrap_or(mid); }
        }
        let (item, count, at) = arrived.expect("arrives");
        assert_eq!((item, count), (7, 3)); assert!((at - 1.4).abs() < 0.03, "{at}");
        assert!(mid.x < 0.8, "after 0.65 s the curve has covered 77 % of the way: x = {}", mid.x);
    }
    fn arrive_all(w: &mut PickupWorld, pawn: &PawnState) -> Vec<(u64, u32, u32)> {
        let mut out = vec![];
        for _ in 0..240 { for e in w.update(1. / 60., pawn, &|_| 0., &mut |_| true) { if let Event::Arrived { id, item, count } = e { out.push((id, item, count)); } } }
        out
    }
    #[test] fn a_partly_taken_bandage_pile_stays_on_the_ground_with_the_rest() {
        let mut w = PickupWorld::new(4);
        let id = w.spawn_at_rest(3, 15, 10, 1, Vec3::new(1.5, TOUCH_HALF_HEIGHT, 0.));
        w.begin_pickup(id, false, Some(1.4)).unwrap();
        let pawn = pawn_at(Vec3::new(0., 0.75, 0.), false);
        let arrived = arrive_all(&mut w, &pawn);
        assert_eq!(arrived, vec![(id, 3, 15)]);
        assert!(w.get(id).is_none() && w.get_arrived(id).is_some(), "held until the host settles it");
        let rest = Vec3::new(0., TOUCH_HALF_HEIGHT, 0.);
        assert!(w.settle(id, 9, rest));
        let p = w.get(id).expect("the rest stays in the world as the same pickup");
        assert_eq!((p.count, p.toss, p.pos), (6, TossState::AtRest, rest));
        assert!(matches!(p.phase, Phase::World));
        assert!(w.arrived.is_empty());
        let look = PawnState { pos: Vec3::new(0., 0.75, 1.0), eye: Vec3::new(0., 1.6, 1.0), forward: Vec3::new(0., -0.6, -1.).normalize(), moving: false, loot: Vec3::new(0., 1.2, 1.0) };
        assert_eq!(w.focus(&look), Some(id), "the rest can be picked up again with Use");
        assert!(!w.settle(id, 1, rest), "settled once");
    }
    #[test] fn an_ammo_rest_at_the_feet_is_not_picked_up_again_until_the_pawn_walks_back_in() {
        let mut w = PickupWorld::new(4);
        let id = w.spawn_at_rest(0, 30, 1, 0, Vec3::new(0.2, TOUCH_HALF_HEIGHT, 0.));
        let pawn = pawn_at(Vec3::new(0., 0.75, 0.), true);
        assert!(w.update(1. / 60., &pawn, &|_| 0., &mut |_| true).iter().any(|e| matches!(e, Event::PickedUp { auto: true, .. })));
        let arrived = arrive_all(&mut w, &pawn);
        assert_eq!(arrived.len(), 1);
        w.settle(id, 10, Vec3::new(0., TOUCH_HALF_HEIGHT, 0.));
        assert_eq!(w.get(id).unwrap().count, 20);
        assert!(w.update(1. / 60., &pawn, &|_| 0., &mut |_| true).is_empty(), "still overlapping: no new begin overlap");
    }
    #[test] fn a_fully_taken_pickup_is_gone() {
        let mut w = PickupWorld::new(4);
        let id = w.spawn_at_rest(0, 2, 17, 0, Vec3::new(1., TOUCH_HALF_HEIGHT, 0.));
        w.begin_pickup(id, false, None);
        arrive_all(&mut w, &pawn_at(Vec3::ZERO, false));
        assert!(w.settle(id, 2, Vec3::ZERO));
        assert!(w.get(id).is_none() && w.get_arrived(id).is_none() && w.len() == 0);
    }
    #[test] fn several_pickups_grabbed_in_quick_succession_all_arrive_and_nothing_vanishes() {
        let mut w = PickupWorld::new(9);
        let ids: Vec<u64> = (0..4).map(|i| w.spawn_at_rest(0, 30, 1, 0, Vec3::new(i as f32 * 0.5 - 0.75, TOUCH_HALF_HEIGHT, -1.5))).collect();
        let pawn = pawn_at(Vec3::new(0., 0.75, 0.), false);
        for (k, id) in ids.iter().enumerate() {
            assert!(w.begin_pickup(*id, false, None).is_some());
            for _ in 0..(k * 3) { w.update(1. / 60., &pawn, &|_| 0., &mut |_| true); }
        }
        let arrived = arrive_all(&mut w, &pawn);
        assert_eq!(arrived.len(), 4, "every pickup arrives");
        let mut room = 50u32;
        let mut taken_total = 0;
        for (id, _, count) in &arrived { let t = (*count).min(room); room -= t; taken_total += t; w.settle(*id, t, Vec3::new(0., TOUCH_HALF_HEIGHT, 0.)); }
        let on_ground: u32 = w.pickups.iter().map(|p| p.count).sum();
        assert_eq!((taken_total, on_ground), (50, 70), "what did not fit stays on the ground");
        assert_eq!(w.pickups.len(), 3, "the first pile was fully taken, the second partly, two untouched");
    }
    #[test] fn the_use_key_picks_what_the_view_ray_hits_within_reach() {
        let mut w = PickupWorld::new(1);
        let near = w.spawn_at_rest(0, 1, 17, 1, Vec3::new(0., TOUCH_HALF_HEIGHT, -2.0));
        let _off = w.spawn_at_rest(1, 1, 17, 1, Vec3::new(3.0, TOUCH_HALF_HEIGHT, -2.0));
        let _far = w.spawn_at_rest(2, 1, 17, 1, Vec3::new(0., TOUCH_HALF_HEIGHT, -9.0));
        let pawn = PawnState { pos: Vec3::new(0., 0.75, 0.), eye: Vec3::new(0., 1.6, 0.), forward: Vec3::new(0., -0.1, -1.).normalize(), moving: false, loot: Vec3::new(0., 1.2, 0.) };
        assert_eq!(w.focus(&pawn), Some(near));
        let away = PawnState { forward: Vec3::new(0., 0., 1.), ..pawn };
        assert_eq!(w.focus(&away), None);
    }
}
