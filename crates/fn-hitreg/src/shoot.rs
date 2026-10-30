//! Firing a recovered gun: aim source, spread cone, per-pellet trace, damage falloff, zone multipliers, props damaged through their health.
//!
//! The model is the decoded native code of `AFortWeaponRanged` (see `spread.rs` for the spread / bloom / cone / falloff maths and the RVA list); this file is the fire
//! function (RVA 0x933630), the aim source selection (0x942c80) and the trace (0x945cc0, 0x920270) around it, applied to the props of `fn_combat::World`.
//!
//! EXACT (decoded): spread = `mult * heat` degrees (full angle) with the heat / cool curves of the stat row, `FRandomStream` seeded from `rand()` once per fire call,
//! `VRandCone` with half angle `spread * 0.5` rad, first-pellet perfect aim, the trace distance `max(RngMax, RngLong, 100 cm)`, the sweep radius `SweepRadius`, the
//! camera / muzzle origin choice, damage falloff `(0,PB) (RngPB,PB) (RngMid,Mid) (RngLong,Long) (RngMax,Max)`.
//! NOT 1:1 (labelled where used): the sweep against convex-hull props is a line (the sphere radius only inflates placeholder cylinders), the physical-surface -> damage zone
//! (head / body) decision and the gameplay-effect application are not part of the prop world here, `sin` / `cos` / `acos` are the CRT functions of Rust's `std`.
use crate::data::GunHit;
use crate::spread::{safe_normal, to_ue, from_ue, vrand_cone, DamageKind, RandomStream, SpreadState};
use fn_combat::{harvest_fx::FxEvent, Harvestable, Resource, World};
use glam::Vec3;

/// Kept so existing callers still compile: the spread / falloff stand-ins it used to switch on are replaced by the decoded model, so it has no effect any more.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy { pub assume_unrecovered: bool }

/// `DamageZone_*` columns: the zone a hit lands in (Critical = head).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone { Light, Normal, Critical, Vulnerability }

/// EPhysicalSurface indices the weapon actor tables are keyed by (sounds: 1 WOOD, 2 STONE, 3 MTL, 4 player; effects: 0 default, 1 wood, 2 stone).
/// Names: `PhysicsSettings:PhysicalSurfaces` of the cooked config (`C:\FortniteRef\scratch\CookedIniVersion.txt` L1599-1620): SurfaceType1 Wood, 2 Stone, 3 Metal,
/// 4 HumanFlesh, 5 HumanOppositionFlesh, 9 Grass, 10 Dirt, 12 WeakSpot, 14-16 WeakSpot_Wood/Stone/Metal (0 = SurfaceType_Default).
pub const SURFACE_DEFAULT: u32 = 0;
pub const SURFACE_WOOD: u32 = 1;
pub const SURFACE_STONE: u32 = 2;
pub const SURFACE_METAL: u32 = 3;
pub fn surface_of(r: Resource) -> u32 { match r { Resource::Wood => SURFACE_WOOD, Resource::Stone => SURFACE_STONE, Resource::Metal => SURFACE_METAL } }

// BUILDING HOOK: player builds (fn-building) block bullets and take their `EnvDmg` (installed by fn-app building_damage_hook.rs). Space = this crate's world (metres).
/// A building piece hit by a trace.
#[derive(Clone, Copy, Debug)]
pub struct BuildingHit { pub t: f32, pub normal: Vec3, pub object: u64, pub resource: Resource }
/// The player builds as hit targets: `trace` the nearest piece within `max_m` (`sweep_m` = sweep radius), `damage` applies `env_damage` and returns `destroyed`.
pub trait BuildingTargets: Send {
    fn trace(&self, origin: Vec3, dir: Vec3, max_m: f32, sweep_m: f32) -> Option<BuildingHit>;
    fn damage(&mut self, object: u64, env_damage: f32, pos: Vec3) -> bool;
}
pub static BUILDING_TARGETS: std::sync::Mutex<Option<Box<dyn BuildingTargets>>> = std::sync::Mutex::new(None);
pub(crate) fn building_trace(origin: Vec3, dir: Vec3, max_m: f32, sweep_m: f32) -> Option<BuildingHit> { BUILDING_TARGETS.lock().ok()?.as_ref()?.trace(origin, dir, max_m, sweep_m) }
pub(crate) fn building_damage(object: u64, env_damage: f32, pos: Vec3) -> bool { BUILDING_TARGETS.lock().ok().and_then(|mut g| g.as_mut().map(|b| b.damage(object, env_damage, pos))).unwrap_or(false) }
fn building_impact(gun: &GunHit, b: &BuildingHit, pos: Vec3, distance_m: f32, damage: f32) -> Impact {
    let destroyed = building_damage(b.object, damage, pos);
    let surface = surface_of(b.resource);
    Impact { pos, normal: b.normal, surface, distance_m, prop: None, object: b.object, damage, destroyed, zone: Zone::Normal,
        effect: gun.pres.impact_effect(surface).map(str::to_owned), sound: gun.pres.impact_sound(surface).map(str::to_owned) }
}

impl crate::data::StatRow {
    pub fn zone_multiplier(&self, z: Zone) -> f32 { match z { Zone::Light => self.zone_light, Zone::Normal => self.zone_normal, Zone::Critical => self.zone_critical, Zone::Vulnerability => self.zone_vulnerability } }
    /// Trace length in metres: `max(RngMax, RngLong)` floored at 100 cm (RVA 0x933761 `maxss [100.0]`).
    pub fn trace_range_m(&self) -> f32 { self.trace_distance_cm().max(100.) / 100. }
    /// `SweepRadius` (stat+0x238) in metres.
    pub fn sweep_radius_m(&self) -> f32 { self.sweep_radius / 100. }
}

/// The aim of one trigger pull: camera eye + forward (unit), plus the muzzle when the caller knows it (needed for the close-range muzzle correction).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot { pub eye: Vec3, pub forward: Vec3 }

/// What the fire function reads from the pawn / weapon besides the spread state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FireContext {
    /// `bIsTargeting` (`[weapon + 0x965]`).
    pub targeting: bool,
    /// The owner's velocity is exactly (0, 0, 0) (`GetVelocity()` compared with `ucomiss`, RVA 0x93b14f..0x93b168).
    pub velocity_zero: bool,
    /// The weapon muzzle in world metres (`[owner + 0x7E0]` aim-source transform with type 4, RVA 0x942dcf); `None` keeps every bullet on the camera ray.
    pub muzzle: Option<Vec3>,
    /// Weapon level (`vtable + 0x878`).
    pub level: i32,
    /// `World->TimeSeconds`.
    pub now: f32,
}
impl Default for FireContext { fn default() -> Self { FireContext { targeting: false, velocity_zero: false, muzzle: None, level: 1, now: 0. } } }

/// One bullet's landing, everything the audio / particle / HUD side needs (hit events).
#[derive(Clone, Debug, PartialEq)]
pub struct Impact {
    pub pos: Vec3, pub normal: Vec3, pub surface: u32, pub distance_m: f32,
    /// Index into `World::props` (None = ground).
    pub prop: Option<usize>, pub object: u64,
    pub damage: f32, pub destroyed: bool, pub zone: Zone,
    /// `ImpactPhysicalSurfaceEffects[surface]` / `ImpactPhysicalSurfaceSounds[surface]` of the weapon actor (package paths).
    pub effect: Option<String>, pub sound: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShotReport { pub impacts: Vec<Impact>, pub pellets: u32, pub misses: u32, /// where each pellet's tracer ends (hit point or max range)
    pub tracer_ends: Vec<Vec3>,
    /// the direction each pellet left along (after the cone), the origin the bullets started from and whether it was the muzzle, the spread (degrees, full angle) that was used
    pub dirs: Vec<Vec3>, pub start: Vec3, pub from_muzzle: bool, pub spread_deg: f32,
    /// Pellets that landed on pawns (`fire_at`); each also has an `Impact` with `surface == SURFACE_PLAYER`.
    pub pawn_hits: Vec<PawnHit> }

/// A world hit: distance from the trace start (m), surface normal, the prop (None = ground plane).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldHit { pub t: f32, pub normal: Vec3, pub prop: Option<usize> }

/// Nearest prop along the ray: (index, distance, normal). `radius` = sweep sphere radius (m): inflates the placeholder cylinders; the real props are traced as a line,
/// against their render triangles (`Harvestable::complex`, the bTraceComplex WeaponTrace) when present, else their convex hulls.
pub fn trace_props_swept(props: &[Harvestable], origin: Vec3, dir: Vec3, max_m: f32, radius: f32) -> Option<(usize, f32, Vec3)> {
    let mut best: Option<(usize, f32, Vec3)> = None;
    for (i, p) in props.iter().enumerate() {
        if !p.alive() { continue; }
        // COMPLEX COLLISION: the WeaponTrace is a bTraceComplex query (AFortWeapon vfunc_269 @140945cc0 -> FCollisionQueryParams ctor @142039780 +0x11): a prop
        // with render triangles is hit on them (a line, like the hulls below), the convex pieces only answer for bodies without complex geometry
        if let Some(m) = p.complex.as_deref().filter(|m| !m.is_empty()) {
            if let Some((t, n)) = m.ray(origin, dir, max_m) { if best.map_or(true, |b| t < b.1) { best = Some((i, t, n)); } }
            continue;
        }
        if let Some(h) = &p.hull {
            if h.segment_clearance(origin, origin + dir * max_m) > 0. { continue; }
            for piece in &h.hulls {
                if let Some((t, n)) = piece.ray(origin, dir, max_m) { if best.map_or(true, |b| t < b.1) { best = Some((i, t, n)); } }
            }
            continue;
        }
        // placeholder vertical cylinder (exact ray / cylinder side + cap test), radius inflated by the sweep radius
        let r = p.radius + radius;
        let (ox, oz) = (origin.x - p.pos.x, origin.z - p.pos.z);
        let (a, b, c) = (dir.x * dir.x + dir.z * dir.z, 2. * (ox * dir.x + oz * dir.z), ox * ox + oz * oz - r * r);
        let (y0, y1) = (p.pos.y, p.pos.y + p.height);
        let mut hit: Option<(f32, Vec3)> = None;
        if a > 1e-9 {
            let disc = b * b - 4. * a * c;
            if disc >= 0. { let t = (-b - disc.sqrt()) / (2. * a); let y = origin.y + dir.y * t;
                if t >= 0. && t <= max_m && y >= y0 && y <= y1 { let q = origin + dir * t; hit = Some((t, Vec3::new(q.x - p.pos.x, 0., q.z - p.pos.z).normalize_or_zero())); } }
        }
        if dir.y.abs() > 1e-9 { for (yy, n) in [(y1, Vec3::Y), (y0, -Vec3::Y)] { let t = (yy - origin.y) / dir.y; if t >= 0. && t <= max_m && hit.map_or(true, |h| t < h.0) { let q = origin + dir * t; if (q.x - p.pos.x).powi(2) + (q.z - p.pos.z).powi(2) <= r * r { hit = Some((t, n)); } } } }
        if let Some((t, n)) = hit { if best.map_or(true, |bb| t < bb.1) { best = Some((i, t, n)); } }
    }
    best
}
/// Line trace against the props (`trace_props_swept` with radius 0).
pub fn trace_props(props: &[Harvestable], origin: Vec3, dir: Vec3, max_m: f32) -> Option<(usize, f32, Vec3)> { trace_props_swept(props, origin, dir, max_m, 0.) }

/// `WeaponTrace` (`AFortWeapon` vtable +0x868, RVA 0x945cc0), EXACT: trace tag `"WeaponTrace"`, `bReturnPhysicalMaterial`, owner ignored; collision channel 20 (`ECC_GameTraceChannel7`), or channel
/// 18 (`GameTraceChannel5`) when `bTraceThroughPawns` (`weapon+0x990`) is set; a line trace when `SweepRadius` (stat +0x238) is 0, otherwise a sphere sweep of that radius; the multi-trace
/// results are de-duplicated and kept up to `TraceThroughPawnsLimit` (`weapon+0x994`), the last one is the blocking hit.
/// Ignore list: the query params are built once by the `FCollisionQueryParams(TraceTag, StatId, bTraceComplex = true, IgnoreActor)` ctor (0x2039780, which calls
/// `AddIgnoredActor` 0x203c150) with `IgnoreActor = this->Instigator` (`weapon+0x150`, AActor::Instigator in the 1.8 SDK); no other `AddIgnoredActor`, no team list.
/// Responses (`C:\FortniteRef\scratch\CookedIniVersion.txt`): `FortPawnMesh` (L1344) blocks `FCC_Trace_Weapon` and overlaps `FCC_Trace_WeaponMulti` with no team rule;
/// `FortStaticMesh` / `FortBuildingMesh` / `FortDynamicMesh` (L1352-1354) and the BlockAll edit (L1385) block it. So every live pawn but the shooter is in the
/// result, teammates included: a teammate stops a non-penetrating pellet. Friendly fire is then off by the fire ability's target filter, not by the trace:
/// `GA_Ranged_GenericDamage` `bExcludePawnFriends` true (see SOURCES below), so the teammate takes no damage ([`fire_at`]).
/// Nearest blocking hit of the props and the ground within `max_m`: the installed landscape (`fn_terrain::world()`, TERRAIN: `Athena_Terrain`'s heightfield
/// collision blocks the weapon channels) or the ground plane (y = 0).
pub fn trace_world(props: &[Harvestable], origin: Vec3, dir: Vec3, max_m: f32, sweep_m: f32) -> Option<WorldHit> {
    trace_world_on(fn_terrain::world().as_deref(), props, origin, dir, max_m, sweep_m)
}
/// [`trace_world`] against an explicit landscape (`None` = the plane). The swept bullet (`SweepRadius`) meets the landscape as a line (ASSUMED: the sweep radius
/// of the 1.8 guns is a few cm); where the ray starts over a hole of the landscape the plane stands in (CHOSEN).
pub fn trace_world_on(terrain: Option<&fn_terrain::World>, props: &[Harvestable], origin: Vec3, dir: Vec3, max_m: f32, sweep_m: f32) -> Option<WorldHit> {
    let prop = trace_props_swept(props, origin, dir, max_m, sweep_m);
    let plane = || if dir.y < -1e-6 && origin.y > 0. { let t = -origin.y / dir.y; (t <= max_m).then_some((t, Vec3::Y)) } else { None };
    let ground = match terrain {
        Some(w) if w.ground_y(origin.x, origin.z).is_some() => w.ray(origin, dir, max_m),
        _ => plane(),
    };
    match (prop, ground) {
        (Some((i, t, n)), g) if g.map_or(true, |(gt, _)| t <= gt) => Some(WorldHit { t, normal: n, prop: Some(i) }),
        (_, Some((t, n))) => Some(WorldHit { t, normal: n, prop: None }),
        _ => None,
    }
}

/// Where the bullets of this trigger pull start and which way they point before the spread: RVA 0x942c80 (`vtable + 0x858`) stores them in `CurrentDamageStartLocation`
/// (+0x93C) / `CurrentAdjustedAimDirection` (+0x948), the fire function reads them through `vtable + 0x668` (RVA 0x9245e0).
///
/// 1. `hit1` = trace from the camera along its forward over `D = max(RngMax, RngLong)`; `target` = its hit location or the far end.
/// 2. `adj = SafeNormal(target - muzzle)`; `hit2` = trace from the muzzle along `adj` for 650 cm.
/// 3. The CAMERA ray is the default. The MUZZLE ray (`start = muzzle`, `dir = adj`) is used when the muzzle trace is blocked, `bShouldAimFromMuzzleAtCloseRange` is set
///    and the weapon is not a scoped weapon that is targeting; the other branch of the exe (a hit actor of an enemy team on both traces) needs pawns, which the prop world has not.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AimSelection { pub start: Vec3, pub dir: Vec3, pub from_muzzle: bool, pub muzzle_blocked_at: Option<Vec3> }
pub fn select_aim(gun: &GunHit, props: &[Harvestable], eye: Vec3, forward: Vec3, muzzle: Option<Vec3>, targeting: bool) -> AimSelection {
    select_aim_by(gun, eye, forward, muzzle, targeting, |o, d, max_m, sweep| trace_world(props, o, d, max_m, sweep).map(|h| h.t))
}
/// `select_aim` with the world trace supplied by the caller (`origin, dir, max_m, sweep_m` -> distance of the first blocking hit): lets pawns block the aim traces like props do.
pub fn select_aim_by(gun: &GunHit, eye: Vec3, forward: Vec3, muzzle: Option<Vec3>, targeting: bool, trace: impl Fn(Vec3, Vec3, f32, f32) -> Option<f32>) -> AimSelection {
    let camera = AimSelection { start: eye, dir: forward, from_muzzle: false, muzzle_blocked_at: None };
    let Some(m) = muzzle else { return camera };
    let sweep = gun.stats.sweep_radius_m();
    let d_m = gun.stats.trace_distance_cm() / 100.;
    let hit1 = trace(eye, forward, d_m, sweep);
    let target = hit1.map_or(eye + forward * d_m, |t| eye + forward * t);
    let adj = safe_normal(target - m);
    let hit2 = trace(m, adj, 6.5, sweep);
    let close_range_ok = gun.pres.aim_from_muzzle_at_close_range && (!targeting || !gun.pres.use_scope_targeting);
    let Some(h2) = hit2 else { return camera };
    if !close_range_ok { return camera; }
    let loc2 = m + adj * h2;
    let loc1 = hit1.map_or(Vec3::ZERO, |t| eye + forward * t);
    // 100 cm^2 = (10 cm)^2 -> 0.01 m^2
    let blocked = ((loc2 - loc1).length_squared() > 0.01).then_some(loc2);
    AimSelection { start: m, dir: adj, from_muzzle: true, muzzle_blocked_at: blocked }
}

/// `FRotator(Pitch + offset, Yaw, 0).Vector()` of a direction (the projectile branch of the fire function adds `ProjectilePitchOffset`, RVA 0x933e09..0x933e49).
pub fn add_pitch_offset(dir: Vec3, offset_deg: f32) -> Vec3 {
    if offset_deg == 0. { return dir; }
    let d = to_ue(dir);
    let yaw = d.y.atan2(d.x); let pitch = d.z.atan2((d.x * d.x + d.y * d.y).sqrt()) + offset_deg.to_radians();
    let (sy, cy) = yaw.sin_cos(); let (sp, cp) = pitch.sin_cos();
    from_ue(Vec3::new(cp * cy, cp * sy, sp))
}

/// Half cone angle (radians) of a pellet: `spread * 0.5 * (PI/180)` as compiled (0x933954..0x933959: `*0.5`, `*0.017453292`).
pub fn half_angle_rad(spread_deg: f32) -> f32 { spread_deg * 0.5 * 0.017453292 }

/// Direction of pellet `index` of a cartridge of `bullets`: perfect aim (no stream draw) or `VRandCone` around `aim`.
pub(crate) fn pellet_dir(gun: &GunHit, spread: &SpreadState, ctx: &FireContext, aim: Vec3, index: u32, rs: &mut RandomStream) -> Vec3 {
    let bullets = gun.stats.bullets_per_cartridge.max(1);
    if spread.perfect_aim(index, bullets, gun.perfect_aim_when_targeting_min_spread, ctx.velocity_zero, ctx.targeting) { return aim; }
    let half = half_angle_rad(spread.spread_deg(0.));
    if half > 0. { vrand_cone(aim, half, half, rs) } else { safe_normal(aim) }
}

pub fn cosmetic_cartridge(gun: &GunHit, world: &World, start: Vec3, aim: Vec3, seed: i32) -> Vec<(Vec3, Option<Impact>)> {
    let s = &gun.stats;
    let (bullets, max_m, sweep) = (s.bullets_per_cartridge.max(1), s.trace_range_m(), s.sweep_radius_m());
    let spread = SpreadState::default();
    let ctx = FireContext::default();
    let mut rs = RandomStream::new(seed);
    (0..bullets).map(|index| {
        let dir = pellet_dir(gun, &spread, &ctx, aim, index, &mut rs);
        if dir == Vec3::ZERO { return (start, None); }
        let world_hit = trace_world(&world.props, start, dir, max_m, sweep);
        if let Some(b) = building_trace(start, dir, world_hit.map_or(max_m, |h| h.t), sweep) { let pos = start + dir * b.t; return (pos, Some(building_impact(gun, &b, pos, b.t, 0.))); }
        match world_hit {
            Some(WorldHit { t, normal, prop }) => {
                let pos = start + dir * t;
                let surface = prop.map_or(SURFACE_DEFAULT, |i| crate::surface::of_prop(&world.props[i]));
                (pos, Some(Impact { pos, normal, surface, distance_m: t, prop, object: 0, damage: 0., destroyed: false, zone: Zone::Normal,
                    effect: gun.pres.impact_effect(surface).map(str::to_owned), sound: gun.pres.impact_sound(surface).map(str::to_owned) }))
            }
            None => (start + dir * max_m, None),
        }
    }).collect()
}

/// Applies one cartridge (`BulletsPerCartridge` pellets) to the world, RVA 0x933630: `seed` is the CRT `rand()` value that seeds the `FRandomStream` (one per call), every
/// pellet leaves the chosen aim ray inside the spread cone (the first pellet of a multi-pellet cartridge exactly on it), the nearest prop / the ground takes the hit, props lose
/// `EnvDmg` at that range (through `Harvestable::health`; a destroyed prop emits `FxEvent::Broke`), `LastFireTime` is stamped per pellet and the heat is added once at the end.
pub fn fire(gun: &GunHit, world: &mut World, shot: &Shot, spread: &mut SpreadState, ctx: &FireContext, seed: i32) -> ShotReport {
    fire_at(gun, world, &mut [], &PawnFire::default(), shot, spread, ctx, seed)
}

/// `fire` with pawns in the world: every pellet is its own trace (props, ground and pawns); a pawn nearer than the world hit takes the pellet (`Combat` damage curve x
/// the damage-zone column of the bone hit, shield first then health) and, unless `opts.trace_through_pawns`, stops it.
#[allow(clippy::too_many_arguments)]
pub fn fire_at(gun: &GunHit, world: &mut World, pawns: &mut [PawnTarget], opts: &PawnFire, shot: &Shot, spread: &mut SpreadState, ctx: &FireContext, seed: i32) -> ShotReport {
    let s = &gun.stats;
    let bullets = s.bullets_per_cartridge.max(1);
    let max_m = s.trace_range_m();
    let sweep = s.sweep_radius_m();
    let aim = {
        let (props, pw) = (&world.props, &*pawns);
        select_aim_by(gun, shot.eye, shot.forward, ctx.muzzle, ctx.targeting, |o, d, m, sw| {
            let w = trace_world(props, o, d, m, sw).map(|h| h.t);
            let p = trace_pawns(pw, opts, o, d, w.unwrap_or(m), sw).first().map(|h| h.t);
            match (w, p) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) }
        })
    };
    let mut rs = RandomStream::new(seed);
    let mut rep = ShotReport { pellets: bullets, start: aim.start, from_muzzle: aim.from_muzzle, spread_deg: spread.spread_deg(0.), ..Default::default() };
    for index in 0..bullets {
        let dir = pellet_dir(gun, spread, ctx, aim.dir, index, &mut rs);
        spread.mark_fired(ctx.now);
        rep.dirs.push(dir);
        if dir == Vec3::ZERO { rep.misses += 1; continue; }
        let world_hit = trace_world(&world.props, aim.start, dir, max_m, sweep);
        let build_hit = building_trace(aim.start, dir, world_hit.map_or(max_m, |h| h.t), sweep); // BUILDING HOOK: a player build before the world hit
        // pawns strictly before the world hit (the nearest only, unless the weapon traces through pawns)
        let phits = trace_pawns(pawns, opts, aim.start, dir, build_hit.map_or(world_hit.map_or(max_m, |h| h.t), |b| b.t), sweep);
        let mut stopped_at: Option<f32> = None;
        let mut n_taken = 0usize; // unique pawn hits of this pellet's trace (0x945cc0 keeps up to TraceThroughPawnsLimit), teammates included
        for ph in phits.into_iter().take(opts.pawn_hit_cap()) {
            let t = ph.t;
            stopped_at = Some(t); n_taken += 1;
            // a teammate's mesh is in the trace result (FortPawnMesh blocks FCC_Trace_Weapon) but the target filter's bExcludePawnFriends drops it: no damage, no hit
            if opts.shooter_team.is_some() && opts.shooter_team == Some(pawns[ph.pawn].team) { continue; }
            let (imp, hit) = hit_pawn(gun, pawns, ph, aim.start + dir * t, t, index, ctx.level, opts.damage_enabled && pawn_damage_gate());
            rep.impacts.push(imp); rep.pawn_hits.push(hit);
        }
        if stopped_at.is_some() && (!opts.trace_through_pawns || n_taken >= opts.trace_through_limit as usize) { rep.tracer_ends.push(aim.start + dir * stopped_at.unwrap()); continue; }
        if let Some(b) = build_hit { // BUILDING HOOK: the piece takes the pellet's EnvDmg at its range (same falloff as props)
            let dmg = s.damage_at(DamageKind::Environmental, b.t * 100., ctx.level) * s.zone_multiplier(Zone::Normal);
            let pos = aim.start + dir * b.t;
            rep.tracer_ends.push(pos);
            rep.impacts.push(building_impact(gun, &b, pos, b.t, dmg));
            continue;
        }
        match world_hit {
            Some(WorldHit { t, normal, prop: Some(i) }) => {
                let dmg = s.damage_at(DamageKind::Environmental, t * 100., ctx.level) * s.zone_multiplier(Zone::Normal);
                let pos = aim.start + dir * t;
                let p: &mut Harvestable = &mut world.props[i];
                let dmg = if p.invulnerable { 0. } else { dmg }; // POI: landscape pieces / chests take no damage
                p.health -= dmg;
                let destroyed = p.health <= 0.;
                if destroyed { p.gone_for = 0.; }
                let object = fn_combat::harvest_fx::object_id(i, p);
                let (surface, kind, res, proto, ppos) = (crate::surface::of_prop(p), p.kind, p.res, p.proto, p.pos);
                if destroyed { world.fx.events.push(FxEvent::Broke { object, pos: ppos, kind, resource: res, proto }); }
                world.hits += 1;
                rep.tracer_ends.push(pos);
                rep.impacts.push(Impact { pos, normal, surface, distance_m: t, prop: Some(i), object, damage: dmg, destroyed, zone: Zone::Normal,
                    effect: gun.pres.impact_effect(surface).map(str::to_owned), sound: gun.pres.impact_sound(surface).map(str::to_owned) });
            }
            Some(WorldHit { t, prop: None, .. }) => {
                let pos = aim.start + dir * t;
                rep.tracer_ends.push(pos);
                rep.impacts.push(Impact { pos, normal: Vec3::Y, surface: SURFACE_DEFAULT, distance_m: t, prop: None, object: 0, damage: 0., destroyed: false, zone: Zone::Normal,
                    effect: gun.pres.impact_effect(SURFACE_DEFAULT).map(str::to_owned), sound: gun.pres.impact_sound(SURFACE_DEFAULT).map(str::to_owned) });
            }
            None => { rep.misses += 1; rep.tracer_ends.push(aim.start + dir * max_m); }
        }
    }
    spread.add_heat(s, ctx.level); // RVA 0x917e40, after the pellet loop
    rep
}

thread_local! { static PAWN_DAMAGE_GATE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) }; }
pub fn set_pawn_damage_gate(enabled: bool) { PAWN_DAMAGE_GATE.with(|g| g.set(enabled)); }
pub fn pawn_damage_gate() -> bool { PAWN_DAMAGE_GATE.with(|g| g.get()) }

/// One pellet / projectile landing on a pawn: `Combat` damage at `range_m` x the zone column, shield first, numbers and hit-marker flags.
pub(crate) fn hit_pawn(gun: &GunHit, pawns: &mut [PawnTarget], ph: PawnTraceHit, pos: Vec3, range_m: f32, pellet: u32, level: i32, damage: bool) -> (Impact, PawnHit) {
    let s = &gun.stats;
    let raw = s.damage_at(DamageKind::Combat, range_m * 100., level) * s.zone_multiplier(ph.zone);
    let pawn = &mut pawns[ph.pawn];
    let applied = if damage { pawn.vitals.apply_damage(raw) } else { Applied::default() };
    // exe 0x65f1c0: bIsCriticalHit when the zone is Critical AND its multiplier exceeds 1.0 (a Critical column of 1, e.g. the launchers, is not a critical hit)
    let critical = ph.zone == Zone::Critical && s.zone_critical > 1.;
    let numbers = damage_numbers(pawn.id, &applied, critical, pos, ph.normal);
    let feedback = HitFeedback { hit_marker: raw > 0., critical, fatal: applied.fatal, shield_only: applied.health_damage <= 0. && applied.shield_damage > 0., flags: pawn_hit_flags(ph.zone) };
    let imp = Impact { pos, normal: ph.normal, surface: SURFACE_PLAYER, distance_m: range_m, prop: None, object: pawn.id, damage: raw, destroyed: applied.fatal, zone: ph.zone,
        effect: gun.pres.impact_effect(SURFACE_PLAYER).map(str::to_owned), sound: gun.pres.impact_sound(SURFACE_PLAYER).map(str::to_owned) };
    (imp, PawnHit { pawn: ph.pawn, id: pawn.id, pellet, pos, normal: ph.normal, distance_m: range_m, zone: ph.zone, bone: ph.bone, damage: raw, applied, numbers, feedback })
}
/// A fired projectile (bolt-action sniper: `ProjectileTemplate`): launched along the (spread) aim at the data's `InitialSpeed`, falling under `gravity_m_s2`
/// (engine `ProjectileMovementComponent` default gravity scale; the world value is the movement crate's). `MaxSpeed` equals `InitialSpeed` in the data, so speed is constant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projectile { pub pos: Vec3, pub vel: Vec3, pub age: f32, pub travelled: f32 }
impl Projectile {
    /// The projectile branch of the fire function (RVA 0x933630): same aim selection and cone as a bullet, pitch raised by `ProjectilePitchOffset`, then `LastFireTime` + heat.
    pub fn launch(gun: &GunHit, world: &World, shot: &Shot, spread: &mut SpreadState, ctx: &FireContext, seed: i32) -> Option<Projectile> {
        let pr = gun.projectile.as_ref()?;
        let aim = select_aim(gun, &world.props, shot.eye, shot.forward, ctx.muzzle, ctx.targeting);
        let mut rs = RandomStream::new(seed);
        let bullets = gun.stats.bullets_per_cartridge.max(1);
        let mut dir = Vec3::ZERO;
        for index in 0..bullets { dir = pellet_dir(gun, spread, ctx, aim.dir, index, &mut rs); spread.mark_fired(ctx.now); }
        spread.add_heat(&gun.stats, ctx.level);
        let dir = add_pitch_offset(dir, gun.pres.projectile_pitch_offset);
        Some(Projectile { pos: aim.start, vel: dir * (pr.initial_speed / 100.).min(pr.max_speed / 100.), age: 0., travelled: 0. })
    }
    /// Advances `dt`; returns the impact (prop damage applied like `fire`) when the step's segment hits a prop or the ground. `max_flight_m`: despawn distance (native, caller's).
    pub fn step(&mut self, gun: &GunHit, world: &mut World, dt: f32, gravity_m_s2: f32, max_flight_m: f32, level: i32) -> Option<Impact> {
        let s = &gun.stats;
        self.vel.y -= gravity_m_s2 * projectile_gravity_scale(gun) * dt;
        let seg = self.vel * dt; let len = seg.length();
        if len <= 0. { return None; }
        let dir = seg / len;
        let hit = trace_world(&world.props, self.pos, dir, len, 0.);
        let travelled = self.travelled;
        // BUILDING HOOK: a player build crossed by this step before the world hit stops the projectile and takes its EnvDmg at the distance flown
        if let Some(b) = building_trace(self.pos, dir, hit.map_or(len, |h| h.t), 0.) {
            let pos = self.pos + dir * b.t;
            let imp = building_impact(gun, &b, pos, travelled + b.t, s.damage_at(DamageKind::Environmental, (travelled + b.t) * 100., level));
            self.age += dt; self.travelled += b.t; self.pos = pos; self.vel = Vec3::ZERO;
            return Some(imp);
        }
        self.age += dt; self.travelled += len;
        let at = |t: f32| self.pos + dir * t;
        let result = match hit {
            Some(WorldHit { t, normal, prop: Some(i) }) => {
                let range = travelled + t;
                let dmg = s.damage_at(DamageKind::Environmental, range * 100., level);
                let p = &mut world.props[i];
                let dmg = if p.invulnerable { 0. } else { dmg }; // POI: landscape pieces / chests take no damage
                p.health -= dmg; let destroyed = p.health <= 0.; if destroyed { p.gone_for = 0.; }
                let object = fn_combat::harvest_fx::object_id(i, p);
                let (surface, kind, res, proto, ppos) = (crate::surface::of_prop(p), p.kind, p.res, p.proto, p.pos);
                if destroyed { world.fx.events.push(FxEvent::Broke { object, pos: ppos, kind, resource: res, proto }); }
                world.hits += 1;
                Some(Impact { pos: at(t), normal, surface, distance_m: range, prop: Some(i), object, damage: dmg, destroyed, zone: Zone::Normal,
                    effect: gun.pres.impact_effect(surface).map(str::to_owned), sound: gun.pres.impact_sound(surface).map(str::to_owned) })
            }
            Some(WorldHit { t, prop: None, .. }) => Some(Impact { pos: at(t), normal: Vec3::Y, surface: SURFACE_DEFAULT, distance_m: travelled + t, prop: None, object: 0, damage: 0., destroyed: false, zone: Zone::Normal,
                effect: gun.pres.impact_effect(SURFACE_DEFAULT).map(str::to_owned), sound: gun.pres.impact_sound(SURFACE_DEFAULT).map(str::to_owned) }),
            None => None,
        };
        self.pos += seg;
        if result.is_none() && self.travelled > max_flight_m { self.vel = Vec3::ZERO; }
        result
    }
    /// `step` with pawns: the first pawn crossed by this step's segment before the world hit stops the projectile and, unless the ability's `Gameplay.Application.Hit`
    /// target filter drops it, takes the projectile's `Combat` damage at the distance flown (zone column, shield first). At most one pawn per projectile.
    ///  * EXACT (exe) the projectile notifies only its first pawn hit: `AFortProjectileAthena` vfunc_192 (RVA 0x1404380b0), the client pre-trace vfunc_205 (0x14043f6e0) and
    ///    `ServerNotifyPawnHit_Implementation` (0x14044b060) all require the flag +0x804 and clear it before handing the hit to `AFortProjectileBase` vfunc_192 (0x14094f9b0).
    ///  * EXACT (data + UE4.16) the pawn hit is a blocking hit (FortPawnCapsule blocks FCC_Object_Projectile, CookedIniVersion.txt L1342) and `B_Prj_Bullet_Sniper`'s
    ///    `ProjectileComp0` does not serialize `bShouldBounce` (zero-initialised false), so `UProjectileMovementComponent::HandleImpact` calls `StopSimulating` on it
    ///    (ProjectileMovementComponent.cpp L475-502). `bTraceThroughPawns` (WID +0x669, copied to `AFortWeaponRanged` +0x990 by RVA 0x140936f10) belongs to the hitscan
    ///    weapon trace; no projectile path reads it, so `opts.trace_through_pawns` is ignored here.
    ///  * EXACT (exe + data) teammates: the touch applies the on-touch container slot (+0x3a8, the `Gameplay.Application.Hit` spec) through 0x140917fa0 -> 0x140294fc0 ->
    ///    0x1402ac3f0, whose friend test reads `bExcludePawnFriends` (filter +0x14); `GA_Ranged_GenericProjectileImpact`'s Hit filter serializes it false, so a direct hit
    ///    damages a teammate. Only the Explode spec ([`Projectile::explode_targets`]) excludes friends.
    #[allow(clippy::too_many_arguments)]
    pub fn step_at(&mut self, gun: &GunHit, world: &mut World, pawns: &mut [PawnTarget], opts: &PawnFire, dt: f32, gravity_m_s2: f32, max_flight_m: f32, level: i32) -> (Option<Impact>, Vec<PawnHit>) {
        let mut v = self.vel; v.y -= gravity_m_s2 * projectile_gravity_scale(gun) * dt;
        let seg = v * dt; let len = seg.length();
        let mut hits = vec![];
        if len > 0. {
            let dir = seg / len;
            let world_t = trace_world(&world.props, self.pos, dir, len, 0.).map(|h| h.t);
            if let Some(ph) = trace_pawns(pawns, opts, self.pos, dir, world_t.unwrap_or(len), 0.).into_iter().next() {
                let t = ph.t; let range = self.travelled + t;
                self.pos += dir * t; self.travelled += t; self.age += dt; self.vel = Vec3::ZERO;
                let friend = opts.shooter_team.is_some() && opts.shooter_team == Some(pawns[ph.pawn].team);
                if friend && gun.ability.selection(crate::data::APPLICATION_HIT).is_some_and(|s| s.exclude_pawn_friends) { return (None, hits); }
                let (imp, hit) = hit_pawn(gun, pawns, ph, self.pos, range, 0, level, opts.damage_enabled && pawn_damage_gate());
                hits.push(hit);
                return (Some(imp), hits);
            }
        }
        (self.step(gun, world, dt, gravity_m_s2, max_flight_m, level), hits)
    }
    /// Pawns the `Gameplay.Application.Explode` spec (on-explode slot +0x430) reaches from the projectile's position: the ability's Explode selection (data, sniper:
    /// Sphere `Range` 512 cm, `MaxTargets` 12, `bExcludeObstructedByWorld` true) with its filter (`bExcludeInstigator`; `bExcludePawnFriends` true, filter +0x14 in
    /// 0x1402ac3f0). Overlap = the sphere touches the pawn capsule. INFERENCE: nearest first when more than `MaxTargets` overlap; obstruction is a straight trace to the
    /// capsule axis. The Explode container serializes no `TargetGameplayEffectClasses` (data), so the damage it carries is the caller's; empty when the ability has no
    /// Explode container.
    pub fn explode_targets(&self, gun: &GunHit, world: &World, pawns: &[PawnTarget], opts: &PawnFire) -> Vec<usize> {
        let Some(sel) = gun.ability.selection(crate::data::APPLICATION_EXPLODE) else { return vec![] };
        let radius = sel.range / 100.;
        let mut v: Vec<(f32, usize)> = pawns.iter().enumerate().filter_map(|(i, p)| {
            if p.id == opts.shooter || !p.vitals.alive() { return None; }
            if sel.exclude_pawn_friends && opts.shooter_team.is_some() && opts.shooter_team == Some(p.team) { return None; }
            let (a, b, r) = p.capsule_segment();
            let ab = b - a; let k = ((self.pos - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0., 1.); let q = a + ab * k;
            let d = (q - self.pos).length();
            if d - r > radius { return None; }
            if sel.exclude_obstructed_by_world && d > 1e-4 && trace_world(&world.props, self.pos, (q - self.pos) / d, d, 0.).is_some() { return None; }
            Some((d, i))
        }).collect();
        v.sort_by(|x, y| x.0.total_cmp(&y.0));
        if sel.max_targets > 0 { v.truncate(sel.max_targets as usize); }
        v.into_iter().map(|x| x.1).collect()
    }
    pub fn dead(&self) -> bool { self.vel == Vec3::ZERO }
}

/// ProjectileGravityScale of the projectile Blueprint (data: 0.18 on the bolt-action sniper), engine default 1 when not serialized.
pub fn projectile_gravity_scale(gun: &GunHit) -> f32 { gun.projectile.as_ref().and_then(|p| p.gravity_scale).unwrap_or(1.) }

/// World gravity for projectiles in m/s^2: `DefaultGravityZ` (-2800 cm/s^2, PROVENANCE: decrypted config, not independently verified; exe default -980; via fn-movement) x the engine `GravityScale`/`ProjectileGravityScale` default 1.
pub fn projectile_gravity_m_s2() -> f32 { -fn_movement::params::WORLD_GRAVITY_Z / 100. }


// ====================================================================================================================================================================
// PAWNS: players as hit targets (capsule gate, physics-asset bone line test, damage zone, shield-then-health, damage numbers / hit marker)
//
// SOURCES. EXACT = read from the 1.8 data / SDK dump / UE4.16 source; INFERENCE = labelled, pending the exe (Ghidra project names.tsv not yet available when this was written).
//  * EXACT  (SDK) `AFortPawn` has `bUseLineTestForDamageZoneBoneDetection` (+0x980 bit 0), `LineTestForDamageZoneBoneDetectionRadius` (+0x984), `FPawnDamageZones DamageZones[4]`
//           (+0x988; per `EFortDamageZone` Light 0 / Normal 1 / Critical 2 / Vulnerability 3: `bActive` + `TArray<FName> Bones`) and `GetDamageZone(const FHitResult&)`.
//  * EXACT  (SDK) `UFortHealthSet`: Health, MaxHealth, CurrentShield, Shield (= max), DamageResistance, DamageVulnerability, Damage and ShieldDamage meta attributes.
//  * EXACT  (SDK) `FFortDamageNumberInfo { WorldLocation, HitNormal, bIsCriticalDamage, int Damage, EFortDamageNumberType (None 0 Pawn 1 Building 2 Player 3 Shield 4 Score 5 DBNO 6) }`,
//           `FFortGameplayEffectContext { bIsFatalHit, bIsCriticalHit, bIsFullBodyHit, KnockbackMagnitude, KnockbackZAngle, StunTime }`.
//  * EXACT  (data) the player's collision capsule is radius 42 cm (AFortPawn ctor @1407c9ff0) / half height 75 cm (crouched 40, clamped to the radius by SetCapsuleSize = 42), `fn_movement::params`. The BR physics asset
//           `Characters/Player/Male/Male_Avg_Base/Fortnite_M_Avg_Player_Physics` (the PhysicsAsset of `Fortnite_M_Avg_Player`, the SkeletalMesh of `PlayerPawn_Athena_Generic.CharacterMesh0`) has one `SkeletalBodySetup` (a `KSphylElem`: bone-local Center / Orientation / Radius / Length, cm)
//           per bone, among them `head` (radius 12.05 cm). The WID flag `bTraceThroughPawns` is serialized true on the bolt-action sniper only.
//  * EXACT  (data) `GA_Ranged_GenericDamage` target filter: `Damageable` actors, `bExcludeInstigator`, `bExcludePawnFriends` true, `bExcludeDBNOPawns` false, `bTraceComplexCollision` false,
//           `bExcludeObstructedByWorld` true; the damage `GE_Ranged_GenericDamage` (template `GET_DirectPhysicalDamage`) adds the `Damage` meta attribute of `UFortHealthSet`.
//  * INFERENCE: the default `DamageZones` content (constructor of `AFortPawn` in the exe; no BR pawn Blueprint serializes it): Critical = {head}. The default of the line-test flag / radius.
//  * EXACT  (exe) `AFortPawn::GetDamageZone` RVA 0x7dd390; damage chain `UFortDamageFormulaExecutionCalculation::Execute` RVA 0x65f1c0 (weapon curve x zone column x resistance /
//           vulnerability, shield absorbs first with overflow into `Damage` -> Health, `FFortGameplayEffectContext.bIsCriticalHit` (+0x69) set when the zone is Critical and its
//           multiplier exceeds 1.0 or a dice crit fires); hit-flag bits RVA 0x937880. See `Vitals::apply_damage`.
//  * INFERENCE: round-to-nearest for the damage number integer (the `FFortDamageNumberInfo` producer is not located yet).
//  * EXACT  (data) `FortPawnCapsule` (CookedIniVersion L1342) does not list `FCC_Trace_Weapon`, whose DefaultResponse is Ignore (L1379): the weapon trace passes through the
//           capsule and only the `FortPawnMesh` physics bodies block it, so a ray inside the capsule that misses every body is no pawn hit.
//  * NOT 1:1: bone transforms come from the caller (the animation pose); `PawnTarget::coarse_bodies` is a stand-in pose-less body set.

/// `EPhysicalSurface` of a player (the weapon actor's `ImpactPhysicalSurfaceSounds[4]` is the player impact sound).
pub const SURFACE_PLAYER: u32 = 4;

/// `UFortHealthSet` Health / CurrentShield and their maxima.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vitals { pub health: f32, pub max_health: f32, pub shield: f32, pub max_shield: f32,
    /// `UFortHealthSet::DamageResistance` / `DamageVulnerability` (0 = none).
    pub resistance: f32, pub vulnerability: f32 }
/// What one application of damage did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Applied { pub shield_damage: f32, pub health_damage: f32, pub shield_broken: bool, pub fatal: bool }
impl Vitals {
    pub fn new(health: f32, shield: f32) -> Vitals { Vitals { health, max_health: health, shield, max_shield: shield, resistance: 0., vulnerability: 0. } }
    pub fn alive(&self) -> bool { self.health > 0. }
    /// EXACT (exe: `UFortDamageFormulaExecutionCalculation::Execute` RVA 0x65f1c0 after the zone multiply; `UFortHealthSet::PostGameplayEffectExecute` RVA 0x445130):
    /// `D = raw * (1 - min(DamageResistance, 0.95)) * (max(DamageVulnerability, 0) + 1)`, then while `CurrentShield > 0`: `ShieldDamage = min(CurrentShield, D)`, `D -= ShieldDamage`
    /// (the exe divides by a SetByCaller shield ratio that defaults to 1.0); the rest is the `Damage` attribute, which `PostGameplayEffectExecute` subtracts from `Health`.
    /// So a broken shield's overflow carries into health. INFERENCE: the 0.95 cap value (the exe stores 0.95 when the resistance exceeds `DAT_14404e218`, not read here);
    /// the GE spec level factor (`+0x1c0`, an int multiplied into `D`) is taken as 1 for BR.
    pub fn apply_damage(&mut self, damage: f32) -> Applied {
        let mut d = damage.max(0.);
        if d > 0. { d *= (1. - self.resistance.clamp(0., 0.95)) * (self.vulnerability.max(0.) + 1.); }
        let shield_damage = if self.shield > 0. { d.min(self.shield) } else { 0. };
        d -= shield_damage;
        self.shield -= shield_damage;
        let health_damage = d.min(self.health.max(0.));
        let was_alive = self.health > 0.;
        self.health -= health_damage;
        Applied { shield_damage, health_damage, shield_broken: shield_damage > 0. && self.shield <= 0., fatal: was_alive && self.health <= 0. && damage > 0. }
    }
}

/// `FPawnDamageZones DamageZones[4]`: per zone (index = `Zone as usize`) whether it is active and its bone names.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DamageZoneBones { pub active: [bool; 4], pub bones: [Vec<String>; 4] }
impl DamageZoneBones {
    /// INFERENCE (see above): only the head bone is Critical.
    pub fn br_default() -> DamageZoneBones { let mut z = DamageZoneBones::default(); z.active[Zone::Critical as usize] = true; z.bones[Zone::Critical as usize] = vec!["head".into()]; z }
    /// `AFortPawn::GetDamageZone(HitResult)`: the active zone listing the hit bone; no match = Normal (INFERENCE for the fall-through).
    pub fn zone_of(&self, bone: Option<&str>) -> Zone {
        let Some(b) = bone else { return Zone::Normal };
        for z in [Zone::Light, Zone::Normal, Zone::Critical, Zone::Vulnerability] {
            if self.active[z as usize] && self.bones[z as usize].iter().any(|n| n.eq_ignore_ascii_case(b)) { return z; }
        }
        Zone::Normal
    }
}

/// A physics-asset body in world space (metres): capsule segment `a..b` with `radius` (a sphere has `a == b`).
#[derive(Clone, Debug, PartialEq)]
pub struct HitBody { pub bone: String, pub a: Vec3, pub b: Vec3, pub radius: f32, pub obb: Option<glam::Mat4> }

/// One player-like hit target.
#[derive(Clone, Debug, PartialEq)]
pub struct PawnTarget {
    pub id: u64, pub team: u32,
    /// Bottom of the capsule, world metres (Y up).
    pub feet: Vec3, pub crouched: bool,
    pub vitals: Vitals,
    /// Physics-asset bodies in the current pose; empty = capsule only (every hit is Normal).
    pub bodies: Vec<HitBody>,
    pub zones: DamageZoneBones,
}
impl PawnTarget {
    pub fn new(id: u64, team: u32, feet: Vec3, vitals: Vitals) -> PawnTarget { PawnTarget { id, team, feet, crouched: false, vitals, bodies: vec![], zones: DamageZoneBones::br_default() } }
    /// Capsule radius / half height, metres (`fn_movement::params`, RECOVERED from `PlayerPawn_Athena_Generic.CollisionCylinder`).
    pub fn capsule_m(&self) -> (f32, f32) {
        use fn_movement::params::{CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CROUCHED_HALF_HEIGHT};
        (CAPSULE_RADIUS / 100., if self.crouched { CROUCHED_HALF_HEIGHT.max(CAPSULE_RADIUS) } else { CAPSULE_HALF_HEIGHT } / 100.) // SetCapsuleSize clamps HalfHeight >= Radius
    }
    /// Capsule axis segment (hemisphere centres).
    pub fn capsule_segment(&self) -> (Vec3, Vec3, f32) { let (r, hh) = self.capsule_m(); (self.feet + Vec3::Y * r, self.feet + Vec3::Y * (2. * hh - r), r) }
    /// INFERENCE stand-in for a pose: head / torso / legs bodies stacked in the capsule (head radius = the physics asset's `head` body, 9.14 cm). Use real bodies when a pose exists.
    pub fn coarse_bodies(&self) -> Vec<HitBody> {
        let (_, hh) = self.capsule_m(); let k = 2. * hh / 1.5; let y = |h: f32| self.feet + Vec3::Y * (h * k);
        vec![HitBody { bone: "head".into(), a: y(1.28), b: y(1.39), radius: 0.0914, obb: None },
             HitBody { bone: "spine_03".into(), a: y(0.92), b: y(1.04), radius: 0.15, obb: None },
             HitBody { bone: "pelvis".into(), a: y(0.80), b: y(0.90), radius: 0.15, obb: None },
             HitBody { bone: "thigh_l".into(), a: y(0.10), b: y(0.75), radius: 0.11, obb: None }]
    }
}

/// Per-trigger-pull options of `fire_at`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PawnFire {
    /// `PawnTarget::id` of the shooter (`bExcludeInstigator`).
    pub shooter: u64, pub shooter_team: Option<u32>,
    /// `UFortWeaponRangedItemDefinition::bTraceThroughPawns` (+0x669): every pawn along the pellet takes it. INFERENCE: the name is the only evidence of the semantics.
    /// Hitscan only: projectiles ignore it ([`Projectile::step_at`]).
    pub trace_through_pawns: bool,
    /// `AFortWeaponRanged::TraceThroughPawnsLimit` (+0x994): the weapon trace keeps at most this many unique hits (RVA 0x945cc0 loop). Not serialized by any 1.8 weapon Blueprint and the
    /// native default was not located, so the default is unlimited.
    pub trace_through_limit: u32,
    /// `bUseLineTestForDamageZoneBoneDetection` radius (`LineTestForDamageZoneBoneDetectionRadius`), metres; INFERENCE default 0.
    pub line_test_radius_m: f32,
    pub damage_enabled: bool,
}
impl Default for PawnFire { fn default() -> Self {
        PawnFire { shooter: u64::MAX, shooter_team: None, trace_through_pawns: false, trace_through_limit: u32::MAX, line_test_radius_m: 0., damage_enabled: true } } }
impl PawnFire { fn pawn_hit_cap(&self) -> usize { if self.trace_through_pawns { self.trace_through_limit as usize } else { 1 } } }

/// `EFortDamageNumberType` values of the number a hit spawns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageNumberKind { Pawn = 1, Building = 2, Player = 3, Shield = 4 }
/// `FFortDamageNumberInfo` (location, normal, critical flag, integer damage, type).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageNumber { pub target: u64, pub kind: DamageNumberKind, pub damage: i32, pub critical: bool, pub location: Vec3, pub normal: Vec3 }
/// The `FFortGameplayEffectContext` flags the HUD hit marker is driven by (INFERENCE: which flag picks which marker art).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HitFeedback { pub hit_marker: bool, pub critical: bool, pub fatal: bool, pub shield_only: bool,
    /// EXACT (exe, `AFortWeapon` hit handler vtable slot 200, RVA 0x937880): the per-cartridge hit-flag bits for this hit (`pawn_hit_flags`).
    pub flags: u32 }
/// Hit-flag bits of RVA 0x937880: pawn 1, building 2, other actor 4, no actor 8; with a pawn the zone adds Light 0x80 / Critical 0x100 / Vulnerability 0x200 (Normal adds nothing).
pub const HIT_PAWN: u32 = 1; pub const HIT_BUILDING: u32 = 2; pub const HIT_OTHER: u32 = 4; pub const HIT_NONE: u32 = 8;
pub fn pawn_hit_flags(z: Zone) -> u32 { HIT_PAWN | match z { Zone::Light => 0x80, Zone::Critical => 0x100, Zone::Vulnerability => 0x200, Zone::Normal => 0 } }
/// INFERENCE: one Shield number for the shield part and one Player number for the health part; integers are round-to-nearest; zero values are not shown.
pub fn damage_numbers(target: u64, a: &Applied, critical: bool, location: Vec3, normal: Vec3) -> Vec<DamageNumber> {
    let mut v = vec![];
    for (kind, d) in [(DamageNumberKind::Shield, a.shield_damage), (DamageNumberKind::Player, a.health_damage)] {
        let n = d.round() as i32; if n > 0 { v.push(DamageNumber { target, kind, damage: n, critical, location, normal }); }
    }
    v
}

/// A pellet landing on a pawn.
#[derive(Clone, Debug, PartialEq)]
pub struct PawnHit {
    pub pawn: usize, pub id: u64, pub pellet: u32, pub pos: Vec3, pub normal: Vec3, pub distance_m: f32, pub zone: Zone, pub bone: Option<String>,
    /// Damage before the shield / health split (`Combat` curve x zone column).
    pub damage: f32, pub applied: Applied, pub numbers: Vec<DamageNumber>, pub feedback: HitFeedback,
}
/// Result of tracing one ray through the pawns.
#[derive(Clone, Debug, PartialEq)]
pub struct PawnTraceHit { pub pawn: usize, pub t: f32, pub normal: Vec3, pub zone: Zone, pub bone: Option<String> }

/// Ray against a capsule (segment `a..b`, `radius`), first hit with `0 <= t <= max_t`; a start inside the capsule hits at 0. Returns (t, outward normal).
pub fn ray_capsule(o: Vec3, d: Vec3, max_t: f32, a: Vec3, b: Vec3, r: f32) -> Option<(f32, Vec3)> {
    let ba = b - a; let baba = ba.length_squared();
    // start inside: distance from the axis segment
    let s = if baba > 1e-12 { ((o - a).dot(ba) / baba).clamp(0., 1.) } else { 0. };
    let axis_pt = a + ba * s;
    if (o - axis_pt).length_squared() <= r * r { return Some((0., (-d).normalize_or_zero())); }
    let mut best: Option<f32> = None;
    let mut take = |t: f32| { if t >= 0. && t <= max_t && best.map_or(true, |bt| t < bt) { best = Some(t); } };
    let sphere = |c: Vec3| -> Option<f32> { let oc = o - c; let bq = oc.dot(d); let cq = oc.length_squared() - r * r; let h = bq * bq - cq; (h >= 0.).then(|| -bq - h.sqrt()) };
    if let Some(t) = sphere(a) { take(t); }
    if let Some(t) = sphere(b) { take(t); }
    if baba > 1e-12 {
        let oa = o - a; let bard = ba.dot(d); let baoa = ba.dot(oa);
        let k2 = baba - bard * bard; let k1 = baba * oa.dot(d) - baoa * bard; let k0 = baba * oa.length_squared() - baoa * baoa - r * r * baba;
        if k2.abs() > 1e-12 {
            let h = k1 * k1 - k2 * k0;
            if h >= 0. { let t = (-k1 - h.sqrt()) / k2; let y = baoa + t * bard; if y > 0. && y < baba { take(t); } }
        }
    }
    let t = best?;
    let p = o + d * t;
    let s = if baba > 1e-12 { ((p - a).dot(ba) / baba).clamp(0., 1.) } else { 0. };
    Some((t, (p - (a + ba * s)).normalize_or_zero()))
}

pub fn ray_body(o: Vec3, d: Vec3, max_t: f32, body: &HitBody, extra: f32) -> Option<(f32, Vec3)> {
    let Some(w) = body.obb else { return ray_capsule(o, d, max_t, body.a, body.b, body.radius + extra) };
    let grow = Vec3::new(w.x_axis.truncate().length(), w.y_axis.truncate().length(), w.z_axis.truncate().length());
    let w = w * glam::Mat4::from_scale(Vec3::ONE + Vec3::splat(extra) / grow.max(Vec3::splat(1e-6)));
    let inv = w.inverse();
    let lo = inv.transform_point3(o);
    let ld = inv.transform_vector3(d);
    let (mut t0, mut t1, mut axis) = (0f32, max_t, None);
    for k in 0..3 {
        if ld[k].abs() < 1e-9 { if lo[k].abs() > 1. { return None; } continue; }
        let (mut a, mut b) = ((-1. - lo[k]) / ld[k], (1. - lo[k]) / ld[k]);
        let mut s = -1f32;
        if a > b { std::mem::swap(&mut a, &mut b); s = 1.; }
        if a > t0 { t0 = a; axis = Some((k, s)); }
        t1 = t1.min(b);
        if t0 > t1 { return None; }
    }
    let (k, s) = axis?;
    let mut n = Vec3::ZERO; n[k] = s;
    Some((t0, inv.transpose().transform_vector3(n).normalize_or_zero()))
}

/// Pawns a ray crosses before `max_m` (nearest first). Only the shooter is skipped (the trace's single ignored actor, the Instigator: see [`trace_world`]); teammates are
/// returned (their mesh blocks the weapon channel) and [`fire_at`] drops them from damage (`bExcludePawnFriends`). Dead pawns are skipped (INFERENCE: the `Ragdoll` profile,
/// CookedIniVersion.txt L1323, keeps the game channels at their DefaultResponse Ignore).
/// The capsule (radius + `sweep_m`, the weapon's sphere sweep) gates the hit; the bone is then found by a line test of `opts.line_test_radius_m` against the physics bodies.
pub fn trace_pawns(pawns: &[PawnTarget], opts: &PawnFire, o: Vec3, d: Vec3, max_m: f32, sweep_m: f32) -> Vec<PawnTraceHit> {
    let mut v: Vec<PawnTraceHit> = vec![];
    for (i, p) in pawns.iter().enumerate() {
        if p.id == opts.shooter || !p.vitals.alive() { continue; }
        let (a, b, r) = p.capsule_segment();
        let Some((t, normal)) = ray_capsule(o, d, max_m, a, b, r + sweep_m) else { continue };
        if t >= max_m { continue; }
        let mut bone: Option<(f32, &str, Vec3)> = None;
        for body in &p.bodies {
            if let Some((bt, bn)) = ray_body(o, d, max_m, body, sweep_m + opts.line_test_radius_m) { if bone.map_or(true, |x| bt < x.0) { bone = Some((bt, body.bone.as_str(), bn)); } }
        }
        if !p.bodies.is_empty() && bone.is_none() { continue; }
        let (t, normal) = bone.map_or((t, normal), |x| (x.0, x.2));
        let bone = bone.map(|x| x.1.to_owned());
        v.push(PawnTraceHit { pawn: i, t, normal, zone: p.zones.zone_of(bone.as_deref()), bone });
    }
    v.sort_by(|x, y| x.t.total_cmp(&y.t));
    v
}

// ------------------------------------------------------------------ physics asset bodies

/// One `KSphylElem` of a `SkeletalBodySetup` (bone-local, UE axes, cm). A sphere is a zero-length sphyl.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sphyl { pub center: Vec3, pub orientation: [f32; 4], pub radius: f32, pub length: f32 }
#[derive(Clone, Debug, PartialEq)]
pub struct BodySetup { pub bone: String, pub shapes: Vec<Sphyl>, pub boxes: Vec<BoxElem> }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxElem { pub center: Vec3, pub orientation: [f32; 4], pub extent: Vec3 }

/// Reads the `SkeletalBodySetup` exports (`BoneName`, `AggGeom.SphylElems` / `SphereElems`) of a physics asset package, e.g.
/// `/Game/Characters/Player/Male/Male_Avg_Base/MPR/Player_Medium_Apose_PhysicsAsset`.
pub fn load_body_setups(src: &fn_pose::ClipSourceDir, package: &str) -> Result<Vec<BodySetup>, String> {
    use fn_ueio::{props::{read_props, Ctx, PropValue}, Reader};
    let pkg = fn_pose::loader::package(src, package)?;
    let mut out = vec![];
    for i in 0..pkg.exports.len() {
        if pkg.export_class(i) != "SkeletalBodySetup" { continue; }
        let (s, e) = pkg.export_range(i);
        let p = read_props(&mut Reader::at(&pkg.data[..e], s), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())?;
        let Some(bone) = p.text("BoneName") else { continue };
        let mut shapes = vec![];
        if let Some(g) = p.fields("AggGeom") {
            for el in g.array("SphylElems").map(|v| v.as_slice()).unwrap_or(&[]) {
                let Some(f) = el.as_fields() else { continue };
                let c = match f.get("Center") { Some(PropValue::Vec3(c)) => Vec3::from(*c), _ => Vec3::ZERO };
                let q = match f.get("Orientation") { Some(PropValue::Vec4(q)) => *q, _ => [0., 0., 0., 1.] };
                shapes.push(Sphyl { center: c, orientation: q, radius: f.float("Radius").unwrap_or(0.) as f32, length: f.float("Length").unwrap_or(0.) as f32 });
            }
            for el in g.array("SphereElems").map(|v| v.as_slice()).unwrap_or(&[]) {
                let Some(f) = el.as_fields() else { continue };
                let c = match f.get("Center") { Some(PropValue::Vec3(c)) => Vec3::from(*c), _ => Vec3::ZERO };
                shapes.push(Sphyl { center: c, orientation: [0., 0., 0., 1.], radius: f.float("Radius").unwrap_or(0.) as f32, length: 0. });
            }
        }
        let mut boxes = vec![];
        if let Some(g) = p.fields("AggGeom") {
            for el in g.array("BoxElems").map(|v| v.as_slice()).unwrap_or(&[]) {
                let Some(f) = el.as_fields() else { continue };
                let c = match f.get("Center") { Some(PropValue::Vec3(c)) => Vec3::from(*c), _ => Vec3::ZERO };
                let q = match f.get("Orientation") { Some(PropValue::Vec4(q)) => *q, _ => [0., 0., 0., 1.] };
                boxes.push(BoxElem { center: c, orientation: q, extent: Vec3::new(f.float("X").unwrap_or(0.) as f32, f.float("Y").unwrap_or(0.) as f32, f.float("Z").unwrap_or(0.) as f32) });
            }
        }
        if !shapes.is_empty() || !boxes.is_empty() { out.push(BodySetup { bone: bone.to_owned(), shapes, boxes }); }
    }
    Ok(out)
}

/// Places the bodies in the world. `bone_to_world(bone)` maps the bone's local UE space (cm, as stored in the asset) to world metres (a `Mat4` that includes the UE -> world axis
/// change and the 0.01 scale); a bone it does not know contributes nothing. A sphyl's axis is its local Z (UE `FKSphylElem`), `length` is the cylinder part.
pub fn place_bodies(setups: &[BodySetup], bone_to_world: impl Fn(&str) -> Option<glam::Mat4>) -> Vec<HitBody> {
    let mut v = vec![];
    for s in setups {
        let Some(m) = bone_to_world(&s.bone) else { continue };
        let scale = m.x_axis.truncate().length();
        for sh in &s.shapes {
            let q = glam::Quat::from_xyzw(sh.orientation[0], sh.orientation[1], sh.orientation[2], sh.orientation[3]).normalize();
            let half = q * Vec3::Z * (sh.length * 0.5);
            v.push(HitBody { bone: s.bone.clone(), a: m.transform_point3(sh.center - half), b: m.transform_point3(sh.center + half), radius: sh.radius * scale, obb: None });
        }
        for bx in &s.boxes {
            let q = glam::Quat::from_xyzw(bx.orientation[0], bx.orientation[1], bx.orientation[2], bx.orientation[3]).normalize();
            let local = glam::Mat4::from_scale_rotation_translation(bx.extent * 0.5, q, bx.center);
            let c = m.transform_point3(bx.center);
            v.push(HitBody { bone: s.bone.clone(), a: c, b: c, radius: 0., obb: Some(m * local) });
        }
    }
    v
}

#[cfg(test)]
mod pawn_tests {
    use super::*;
    use crate::data::{Presentation, StatRow};
    use crate::spread::SpreadInputs;
    #[allow(dead_code)] fn ar_synthetic() -> StatRow { StatRow { spread: 0.15, standing_still_spread_mult: 0.55, heat: [(2.85, 8.75), (4.1, 8.75), (5.5, 10.)], cool: [(2.85, 2.7), (4.1, 3.), (5.5, 3.2)], bullets_per_cartridge: 1, base_level: 1, dmg: [32., 22.848, 22.848, 22.848], rng: [2500., 3750., 25000., 25000.], zone_normal: 1., zone_critical: 2.5, ..Default::default() } }
    fn gun(stats: StatRow) -> GunHit { GunHit { wid: "t".into(), row: "t".into(), stats, pres: Presentation::default(), ability: Default::default(), projectile: None, perfect_aim_when_targeting_min_spread: true, trigger: String::new(), ammo: String::new(), rarity: String::new(), tier: String::new(), min_level: 0, ammo_path: String::new(), ability_path: String::new(), actor_path: String::new(), stat_table: String::new(), tracer: None, projectile_particles: vec![] } }
    /// Shooter eye at the origin looking down -Z; a perfect-aim state (ADS at rest) so the single bullet is exactly on the aim ray.
    fn shoot(g: &GunHit, pawns: &mut [PawnTarget], eye: Vec3, opts: &PawnFire) -> ShotReport {
        let mut st = SpreadState::default();
        for k in 0..600 { st.tick(&g.stats, &SpreadInputs { now: k as f32 / 60., targeting: true, ads_alpha: 1., ..Default::default() }, 1. / 60., 1); }
        let ctx = FireContext { targeting: true, velocity_zero: true, now: 100., ..Default::default() };
        fire_at(g, &mut World::default(), pawns, opts, &Shot { eye, forward: -Vec3::Z }, &mut st, &ctx, 7)
    }
    fn enemy(z: f32) -> PawnTarget { let mut p = PawnTarget::new(2, 1, Vec3::new(0., 0., z), Vitals::new(100., 100.)); p.bodies = p.coarse_bodies(); p }
    fn opts() -> PawnFire { PawnFire { shooter: 1, ..Default::default() } }

    /// A gun loaded from the 1.8 data (the tests skip when the install is not on this machine).
    fn real(wid: &str) -> Option<GunHit> {
        let dir = fn_live::var("FN_INSTALL").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        let paks = fn_pak::PakSet::open_install(&dir).ok()?;
        crate::data::load(&fn_pose::ClipSourceDir::Pak(&paks), wid).ok()
    }
    const SR_AR: &str = "/Game/Athena/Items/Weapons/WID_Assault_AutoHigh_Athena_SR_Ore_T03";
    const COMMON_PUMP: &str = "/Game/Athena/Items/Weapons/WID_Shotgun_Standard_Athena_C_Ore_T03";

    #[test]
    fn every_br_row_damage_falloff_and_headshot_match_the_cooked_columns() {
        let dir = fn_live::var("FN_INSTALL").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        let Ok(paks) = fn_pak::PakSet::open_install(&dir) else { eprintln!("install missing, skipped"); return };
        let src = fn_pose::ClipSourceDir::Pak(&paks);
        let guns: Vec<GunHit> = crate::data::br_weapon_wids(&paks).iter().filter_map(|w| crate::data::load(&src, w).ok()).filter(|g| g.stat_table.ends_with("AthenaRangedWeapons")).collect();
        assert_eq!(guns.len(), 45);
        for g in &guns {
            let s = &g.stats;
            let col = |n: &str| -> f32 { s.columns.iter().find(|c| c.0 == n).unwrap_or_else(|| panic!("{}: no column {n}", g.row)).1.parse().unwrap() };
            assert_eq!(s.dmg, [col("DmgPB"), col("DmgMid"), col("DmgLong"), col("DmgMaxRange")], "{}", g.row);
            assert_eq!(s.rng, [col("RngPB"), col("RngMid"), col("RngLong"), col("RngMax")], "{}", g.row);
            assert_eq!((s.zone_critical, s.zone_normal, s.dmg_scale), (col("DamageZone_Critical"), col("DamageZone_Normal"), col("DmgScale")), "{}", g.row);
            assert_eq!(s.bullets_per_cartridge, col("BulletsPerCartridge") as u32, "{}", g.row);
            let at = |cm: f32| s.damage_at(DamageKind::Combat, cm, 1);
            for (r, d) in [(0., s.dmg[0]), (s.rng[0], s.dmg[0]), (s.rng[1], s.dmg[1]), (s.rng[2], s.dmg[2]), (s.rng[3], s.dmg[3]), (s.rng[3] * 2., s.dmg[3])] { assert!((at(r) - d).abs() < 1e-3, "{} at {r} cm: {} vs {d}", g.row, at(r)); }
            if s.rng[1] > s.rng[0] { let m = (s.rng[0] + s.rng[1]) * 0.5; assert!((at(m) - (s.dmg[0] + s.dmg[1]) * 0.5).abs() < 1e-3, "{} linear between PB and Mid", g.row); }
            let mut v = Vitals::new(100., 100.);
            let crit = s.damage_at(DamageKind::Combat, 500., 1) * s.zone_multiplier(Zone::Critical);
            let a = v.apply_damage(crit);
            assert!((a.shield_damage + a.health_damage - crit.min(200.)).abs() < 1e-3 && a.shield_damage == crit.min(100.), "{} shield first", g.row);
        }
    }
    #[test]
    fn body_shot_damage_follows_falloff_at_several_ranges() {
        let Some(g) = real(SR_AR) else { eprintln!("install missing, skipped"); return };
        let s = &g.stats;
        let (pb, mid, long) = (s.dmg[0], s.dmg[1], s.dmg[2]);
        let (r0, r1) = (s.rng[0] / 100., s.rng[1] / 100.);
        // linear between (RngPB, DmgPB) and (RngMid, DmgMid), constant before / after (the five-key curve)
        let cases = [(10., pb), (r0 * 0.5, pb), (r0, pb), ((r0 + r1) * 0.5, (pb + mid) * 0.5), (r1, mid), (100., long)];
        for (m, expect) in cases {
            let mut p = [enemy(-m)];
            let r = shoot(&g, &mut p, Vec3::new(0., 0.9, 0.), &opts());
            assert_eq!(r.pawn_hits.len(), 1, "{m} m");
            let h = &r.pawn_hits[0];
            assert_eq!(h.zone, Zone::Normal, "{m} m torso");
            // the hit is at the capsule surface (CAPSULE_RADIUS, 0.42 m nearer) and the curve is evaluated at that distance
            let at = |d: f32| if d <= r0 { pb } else if d >= r1 { mid } else { pb + (mid - pb) * (d - r0) / (r1 - r0) };
            let _ = expect;
            let want = if m >= 100. { long } else { at(h.distance_m) };
            assert!((h.damage - want).abs() < 0.02, "{m} m: {} vs {want}", h.damage);
            assert!(h.distance_m < m && h.distance_m > m - fn_movement::params::CAPSULE_RADIUS / 100., "{m} m: body surface at {}", h.distance_m);
        }
    }
    #[test]
    fn spawn_island_hits_mark_and_react_but_leave_health_and_shield() {
        let g = gun(ar_synthetic());
        let mut p = [enemy(-20.)];
        let r = shoot(&g, &mut p, Vec3::new(0., 1.0, 0.), &PawnFire { damage_enabled: false, ..opts() });
        let h = &r.pawn_hits[0];
        assert!(h.damage > 0. && h.feedback.hit_marker && !h.feedback.fatal && h.numbers.is_empty());
        assert_eq!(h.applied, Applied::default());
        assert_eq!((p[0].vitals.health, p[0].vitals.shield), (100., 100.));
        assert!(r.impacts.iter().any(|i| i.surface == SURFACE_PLAYER && i.damage > 0.));
        set_pawn_damage_gate(false);
        let r = shoot(&g, &mut p, Vec3::new(0., 1.0, 0.), &opts());
        set_pawn_damage_gate(true);
        assert!(r.pawn_hits[0].feedback.hit_marker && (p[0].vitals.health, p[0].vitals.shield) == (100., 100.));
        let r = shoot(&g, &mut p, Vec3::new(0., 1.0, 0.), &opts());
        assert!(r.pawn_hits[0].applied.shield_damage > 0. && p[0].vitals.shield < 100.);
    }
    #[test]
    fn headshot_is_critical_zone_column_and_shows_a_critical_number() {
        let Some(g) = real(SR_AR) else { eprintln!("install missing, skipped"); return };
        let mut p = [enemy(-20.)];
        let r = shoot(&g, &mut p, Vec3::new(0., 1.34, 0.), &opts()); // head height
        let h = &r.pawn_hits[0];
        assert_eq!((h.zone, h.bone.as_deref()), (Zone::Critical, Some("head")));
        let base = g.stats.damage_at(DamageKind::Combat, h.distance_m * 100., 1);
        assert!((h.damage - base * g.stats.zone_critical).abs() < 1e-3 && g.stats.zone_critical > 1.);
        assert!(h.feedback.critical && h.feedback.hit_marker && !h.feedback.fatal);
        assert_eq!((h.applied.shield_damage, h.applied.health_damage), (h.damage, 0.));
        assert_eq!(h.numbers, vec![DamageNumber { target: 2, kind: DamageNumberKind::Shield, damage: h.damage.round() as i32, critical: true, location: h.pos, normal: h.normal }]);
        assert_eq!(r.impacts[0].surface, SURFACE_PLAYER);
    }    #[test]
    fn resistance_vulnerability_and_shield_order_match_the_exec_calc() {
        let mut v = Vitals::new(100., 50.); v.resistance = 0.5; v.vulnerability = 1.;
        let a = v.apply_damage(60.); // 60 * 0.5 * 2 = 60 -> 50 shield, 10 health
        assert_eq!((a.shield_damage, a.health_damage), (50., 10.));
        let mut w = Vitals::new(100., 0.); w.resistance = 2.; // capped at 0.95
        assert!((w.apply_damage(100.).health_damage - 5.).abs() < 1e-4);
    }
    #[test]
    fn shield_goes_first_overflow_into_health_and_kills() {
        let mut v = Vitals::new(100., 100.);
        let a = v.apply_damage(130.);
        assert_eq!((a.shield_damage, a.health_damage, a.shield_broken, a.fatal), (100., 30., true, false));
        assert_eq!((v.shield, v.health), (0., 70.));
        let a = v.apply_damage(500.);
        assert_eq!((a.health_damage, a.fatal), (70., true));
        assert!(!v.alive() && !v.apply_damage(10.).fatal);
    }
    #[test]
    fn shotgun_pellets_are_separate_traces_each_damaging() {
        let Some(g) = real(COMMON_PUMP) else { eprintln!("install missing, skipped"); return };
        let n = g.stats.bullets_per_cartridge;
        assert!(n > 1);
        let mut st = SpreadState::default();
        let mut p = [enemy(-6.)];
        let ctx = FireContext { now: 100., ..Default::default() };
        let r = fire_at(&g, &mut World::default(), &mut p, &opts(), &Shot { eye: Vec3::new(0., 0.9, 0.), forward: -Vec3::Z }, &mut st, &ctx, 12345);
        assert_eq!((r.pellets, r.dirs.len(), r.pawn_hits.len() as u32), (n, n as usize, n));
        for h in &r.pawn_hits { let base = g.stats.damage_at(DamageKind::Combat, h.distance_m * 100., 1); assert!((h.damage - base * g.stats.zone_multiplier(h.zone)).abs() < 1e-3); }
        let total: f32 = r.pawn_hits.iter().map(|h| h.applied.shield_damage + h.applied.health_damage).sum();
        assert!((100. - p[0].vitals.shield + (100. - p[0].vitals.health) - total).abs() < 1e-3, "every pellet's damage landed on the vitals");
    }    #[test]
    fn shooter_friends_and_dead_pawns_are_ignored_and_nearest_blocks() {
        let g = gun(ar_synthetic());
        let mut me = enemy(-5.); me.id = 1;
        let mut friend = enemy(-6.); friend.id = 3; friend.team = 9;
        let (mut near, mut far) = (enemy(-10.), enemy(-20.)); near.id = 4; far.id = 5;
        let mut pawns = [me, friend.clone(), near.clone(), far];
        let o = PawnFire { shooter: 1, shooter_team: Some(9), ..Default::default() };
        let r = shoot(&g, &mut pawns, Vec3::new(0., 0.9, 0.), &o);
        assert!(r.pawn_hits.is_empty(), "the shooter is the only ignored actor: the teammate's mesh stops the bullet and bExcludePawnFriends drops it from damage");
        assert_eq!((pawns[1].vitals.health, pawns[2].vitals.health), (100., 100.), "no friendly fire, and the enemy behind the teammate is not hit");
        let r = shoot(&g, &mut pawns, Vec3::new(0., 0.9, 0.), &PawnFire { trace_through_pawns: true, ..o });
        assert_eq!(r.pawn_hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![4, 5], "bTraceThroughPawns: both enemies, the teammate overlapped without damage");
        assert_eq!(pawns[1].vitals.health, 100.);
        // without the teammate in the way the nearest enemy stops the bullet
        let mut me = enemy(-5.); me.id = 1;
        let mut dead = friend; dead.team = 2; dead.vitals.health = 0.;
        let mut pawns = [me, dead, near, enemy(-20.)];
        pawns[3].id = 5;
        let r = shoot(&g, &mut pawns, Vec3::new(0., 0.9, 0.), &o);
        assert_eq!(r.pawn_hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![4], "own pawn and a dead pawn skipped, nearest enemy stops the bullet");
        assert_eq!(pawns[3].vitals.health, 100.);
    }
    #[test]
    fn sniper_projectile_hits_a_teammate_stops_at_the_first_pawn_and_explode_excludes_friends() {
        let Some(g) = real("/Game/Athena/Items/Weapons/WID_Sniper_BoltAction_Scope_Athena_R_Ore_T03") else { eprintln!("install missing, skipped"); return };
        let (hit, ex) = (g.ability.selection(crate::data::APPLICATION_HIT).unwrap(), g.ability.selection(crate::data::APPLICATION_EXPLODE).unwrap());
        assert_eq!((hit.exclude_pawn_friends, ex.exclude_pawn_friends, ex.range, ex.max_targets), (false, true, 512., 12), "GA_Ranged_GenericProjectileImpact filters");
        assert!(g.stats.bullets_per_cartridge >= 1);
        let mut me = enemy(5.); me.id = 1;
        let mut friend = enemy(-6.); friend.id = 3; friend.team = 9;
        let mut behind = enemy(-9.); behind.id = 4;
        let mut pawns = [me, friend, behind];
        // the WID's bTraceThroughPawns is set on this sniper; the projectile ignores it (hitscan-only flag)
        let o = PawnFire { shooter: 1, shooter_team: Some(9), trace_through_pawns: true, ..Default::default() };
        let mut p = Projectile { pos: Vec3::new(0., 0.9, 0.), vel: Vec3::new(0., 0., -300.), age: 0., travelled: 0. };
        let mut world = World::default();
        let mut got = vec![];
        for _ in 0..20 { if p.dead() { break; } let (_, h) = p.step_at(&g, &mut world, &mut pawns, &o, 1. / 120., 0., 400., 1); got.extend(h); }
        assert_eq!(got.iter().map(|h| h.id).collect::<Vec<_>>(), vec![3], "direct hit applies the Hit spec to the teammate and stops the projectile");
        assert!(p.dead() && pawns[1].vitals.shield < 100. && pawns[2].vitals.shield == 100.);
        // the Explode spec from the stop point: the teammate (and the shooter) are filtered, the enemy 3 m further is inside the 512 cm sphere
        assert_eq!(p.explode_targets(&g, &world, &pawns, &o), vec![2]);
    }
    #[test]
    fn capsule_gate_and_ray_capsule_geometry() {
        let (a, b) = (Vec3::new(0., 0.34, 0.), Vec3::new(0., 1.16, 0.));
        let (t, n) = ray_capsule(Vec3::new(0., 0.8, 5.), -Vec3::Z, 100., a, b, 0.34).unwrap();
        assert!((t - 4.66).abs() < 1e-4 && (n - Vec3::Z).length() < 1e-4);
        assert!(ray_capsule(Vec3::new(1., 0.8, 5.), -Vec3::Z, 100., a, b, 0.34).is_none());
        let (t, _) = ray_capsule(Vec3::new(0., 3., 0.), -Vec3::Y, 100., a, b, 0.34).unwrap(); // down onto the cap
        assert!((t - (3. - 1.5)).abs() < 1e-4);
        assert!(ray_capsule(Vec3::new(0., 0.8, 5.), -Vec3::Z, 4., a, b, 0.34).is_none(), "beyond max range");
        // a pellet inside the capsule that misses the 15 cm pelvis body by 5 cm hits only when the weapon sweeps 10 cm (the capsule ignores FCC_Trace_Weapon)
        let p = enemy(-5.); let o = PawnFire::default();
        assert!(trace_pawns(&[p.clone()], &o, Vec3::new(0.20, 0.85, 0.), -Vec3::Z, 50., 0.).is_empty());
        assert_eq!(trace_pawns(&[p], &o, Vec3::new(0.20, 0.85, 0.), -Vec3::Z, 50., 0.10).len(), 1);
    }
    #[test]
    fn damage_zone_lookup_uses_active_bone_lists() {
        let mut z = DamageZoneBones::br_default();
        assert_eq!((z.zone_of(Some("head")), z.zone_of(Some("pelvis")), z.zone_of(None)), (Zone::Critical, Zone::Normal, Zone::Normal));
        z.active[Zone::Critical as usize] = false;
        assert_eq!(z.zone_of(Some("head")), Zone::Normal, "inactive zone (DamageZones[i].bActive false)");
        z.active[Zone::Light as usize] = true; z.bones[Zone::Light as usize] = vec!["foot_l".into()];
        assert_eq!(z.zone_of(Some("foot_l")), Zone::Light);
    }
    #[test]
    fn placed_physics_bodies_use_the_local_z_axis() {
        let s = [BodySetup { bone: "head".into(), shapes: vec![Sphyl { center: Vec3::ZERO, orientation: [0., 0., 0., 1.], radius: 9.14, length: 10.97 }], boxes: vec![] }];
        let m = glam::Mat4::from_translation(Vec3::new(1., 2., 3.)) * glam::Mat4::from_scale(Vec3::splat(0.01));
        let v = place_bodies(&s, |b| (b == "head").then_some(m));
        assert_eq!(v.len(), 1);
        assert!((v[0].a - Vec3::new(1., 2., 3. - 0.05485)).length() < 1e-4 && (v[0].b - Vec3::new(1., 2., 3. + 0.05485)).length() < 1e-4 && (v[0].radius - 0.0914).abs() < 1e-5);
        assert!(place_bodies(&s, |_| None).is_empty());
        let bx = [BodySetup { bone: "hand_l".into(), shapes: vec![], boxes: vec![BoxElem { center: Vec3::new(10., 0., 0.), orientation: [0., 0., 0., 1.], extent: Vec3::new(20., 10., 18.) }] }];
        let v = place_bodies(&bx, |_| Some(m));
        let (t, n) = ray_body(Vec3::new(1.1, 2., 5.), -Vec3::Z, 10., &v[0], 0.).unwrap();
        assert!((t - (5. - 3. - 0.09)).abs() < 1e-4 && (n - Vec3::Z).length() < 1e-4, "{t} {n}");
        assert!(ray_body(Vec3::new(1.25, 2., 5.), -Vec3::Z, 10., &v[0], 0.).is_none());
        assert!(ray_body(Vec3::new(1.25, 2., 5.), -Vec3::Z, 10., &v[0], 0.06).is_some());
    }
    #[test]
    #[ignore = "reads the install"]
    fn real_physics_asset_has_a_head_body() {
        let paks = fn_pak::PakSet::open_install(&fn_live::var("FN_INSTALL").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into())).unwrap();
        let src = fn_pose::ClipSourceDir::Pak(&paks);
        let b = load_body_setups(&src, "/Game/Characters/Player/Male/Male_Avg_Base/Fortnite_M_Avg_Player_Physics").unwrap();
        let head = b.iter().find(|s| s.bone == "head").expect("head body");
        assert!((head.shapes[0].radius - 12.0475).abs() < 1e-3 && b.len() == 21, "{} bodies", b.len());
        assert!(b.iter().all(|s| !s.bone.starts_with("clavicle")), "the pawn mesh physics asset has no clavicle bodies");
    }
}
