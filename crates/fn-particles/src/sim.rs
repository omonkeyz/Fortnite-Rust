//! CPU simulator of a Cascade particle system with the module semantics of UE 4.16 (`FParticleEmitterInstance` / `UParticleModule*::Spawn/Update`).
//!
//! Frame order per emitter (the original `FParticleEmitterInstance::Tick`): reset the per-frame particle values (velocity = base velocity, size = base size, color = base color,
//! rotation rate = base rate, relative time advances by `dt / lifetime`), run the update modules in list order, integrate the location (`loc += velocity * dt`, `rotation += rate * dt`),
//! kill particles whose relative time reached 1, then spawn: continuous rate (`SpawnFraction` accumulation with the sub-frame `SpawnTime` / `Increment` of the original) plus bursts
//! (`ParticleBurst.Time` is normalised to `EmitterDuration`), running the spawn modules in list order. New particles then get one update evaluation with `dt = 0`.
//!
//! Units: UE (cm, seconds, UE axes X forward / Y right / Z up) inside the simulation; [`ue_to_game`] / [`game_to_ue`] convert to the game (metres, Y up, `(x, z, y) * 0.01`).
//! Rotations of the modules are in turns (1.0 = 360 degrees). Component transform: translation + rotation (+ uniform scale) of the spawned effect.
//!
//! NOT the original (and reported as such in the crate docs): the sampling pattern of the sphere / cylinder location modules (volume-uniform here), the un-seeded random source
//! (the original used the global `FMath::SRand`), world collision (DistanceField of the UE scene -> a ground function supplied by the host).
use crate::model::*;
use crate::params::Params;
use crate::rng::Rng;
use glam::{Mat3, Vec3};
use std::f32::consts::TAU;
use std::sync::Arc;

pub const MAX_ACTIVE_PER_EMITTER: usize = 4096;

/// UE position / direction (cm) -> game (m): `(x, y, z) -> (x, z, y) * 0.01` (the axis swap of the glTF export).
pub fn ue_to_game(v: Vec3) -> [f32; 3] { [v.x * 0.01, v.z * 0.01, v.y * 0.01] }
pub fn game_to_ue(p: [f32; 3]) -> Vec3 { Vec3::new(p[0] * 100.0, p[2] * 100.0, p[1] * 100.0) }
fn swap_yz(v: Vec3) -> Vec3 { Vec3::new(v.x, v.z, v.y) }

/// Component transform in UE space.
#[derive(Clone, Copy, Debug)]
pub struct Transform { pub pos: Vec3, pub rot: Mat3, pub scale: f32 }
impl Transform {
    pub fn identity() -> Transform { Transform { pos: Vec3::ZERO, rot: Mat3::IDENTITY, scale: 1.0 } }
    /// From a game position (m) and a yaw about the game's Y axis (radians, right handed); the yaw is turned into the equivalent rotation about UE's Z axis.
    pub fn from_game(pos_m: [f32; 3], yaw_game: f32) -> Transform { Transform { pos: game_to_ue(pos_m), rot: Mat3::from_rotation_z(-yaw_game), scale: 1.0 } }
    /// `SpawnEmitterAtLocation(Location, Normal.Rotation())`: the emitter's X axis (UE forward) points along `normal_game` (a game-space direction).
    pub fn from_game_facing(pos_m: [f32; 3], normal_game: [f32; 3]) -> Transform {
        let x = swap_yz(Vec3::from(normal_game)).normalize_or_zero();
        if x == Vec3::ZERO { return Transform { pos: game_to_ue(pos_m), rot: Mat3::IDENTITY, scale: 1.0 }; }
        let up = if x.z.abs() > 0.999 { Vec3::X } else { Vec3::Z };
        let y = up.cross(x).normalize(); let z = x.cross(y);
        Transform { pos: game_to_ue(pos_m), rot: Mat3::from_cols(x, y, z), scale: 1.0 }
    }
}

/// What the host provides for collisions.
#[derive(Clone, Copy)]
pub struct Env<'a> {
    /// Ground height in UE cm (z) at UE `(x, y)` in cm; `None` = nothing to collide with.
    pub ground: Option<&'a dyn Fn(f32, f32) -> f32>,
}
impl<'a> Env<'a> { pub fn none() -> Env<'a> { Env { ground: None } } }

#[derive(Clone, Debug)]
pub struct Particle {
    pub loc: Vec3, pub vel: Vec3, pub base_vel: Vec3,
    pub size: Vec3, pub base_size: Vec3,
    pub color: [f32; 4], pub base_color: [f32; 4],
    pub rot: f32, pub rot_rate: f32, pub base_rot_rate: f32,
    pub mesh_rot: Vec3, pub mesh_rot_rate: Vec3,
    pub rel_time: f32, pub inv_life: f32,
    pub sub_image: f32,
    pub cam_offset: f32,
    pub orbit: Option<Orbit>,
    pub dynamic: [f32; 4],
    /// `FLightParticlePayload` of a `ParticleModuleLight`.
    pub light: Option<LightPayload>,
    pub collisions: u32, pub stopped: bool, pub age: f32,
    /// Spawn serial number inside the emitter (diagnostics, stable ordering).
    pub serial: u64,
    pub trail: TrailPoint,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct TrailPoint { pub tangent: Vec3, pub spawn_time: f32, pub tiled_u: f32, pub up: Vec3, pub interpolated: bool, pub spawned_tess: i32 }
#[derive(Clone, Copy, Debug, Default)]
struct RibbonState { running: f32, last_src: Vec3, last_tan: Vec3, cur_src: Vec3, cur_tan: Vec3, trail_spawn_time: f32, src_dist: f32, tiled_dist: f32, inv_life: f32, size: f32, started: bool }
#[derive(Clone, Copy, Debug)]
pub struct LightPayload { pub color_scale: [f32; 3], pub radius_scale: f32, pub exponent: f32, pub valid: bool }
#[derive(Clone, Copy, Debug)]
pub struct Orbit { pub offset: Vec3, pub rotation: Vec3, pub rate: Vec3 }

impl Particle {
    fn fresh(origin: Vec3, serial: u64) -> Particle {
        Particle { loc: origin, vel: Vec3::ZERO, base_vel: Vec3::ZERO, size: Vec3::ZERO, base_size: Vec3::ZERO, color: [1.0; 4], base_color: [1.0; 4], rot: 0.0, rot_rate: 0.0, base_rot_rate: 0.0,
            mesh_rot: Vec3::ZERO, mesh_rot_rate: Vec3::ZERO, rel_time: 0.0, inv_life: 0.0, sub_image: 0.0, cam_offset: 0.0, orbit: None, dynamic: [0.0; 4], light: None, collisions: 0, stopped: false, age: 0.0, serial, trail: TrailPoint::default() }
    }
}

pub struct EmitterInstance {
    pub index: usize,
    pub lod: usize,
    pub particles: Vec<Particle>,
    rng: Rng,
    seeded: Vec<Option<Rng>>,
    spawn_fraction: f32,
    pub emitter_time: f32,
    delay_left: f32,
    loops_done: i32,
    burst_fired: Vec<bool>,
    pub completed: bool,
    pub spawned_total: u64,
    pub dropped: u64,
    pub bursts_fired: u32,
    /// `Deactivate()`: no new particles, the live ones play out.
    pub halted: bool,
    /// `SetEmitterEnable(Name, false)` (`bHaltSpawningExternal`): the emitter keeps its clock and its particles but spawns nothing.
    pub halted_external: bool,
    /// `FParticleEmitterInstance::Location` / `OldLocation` (component location, UE world cm) of the last two ticks, for `ParticleModuleSpawnPerUnit` and the world-space spawn interpolation.
    pub location: Vec3,
    pub old_location: Vec3,
    has_location: bool,
    /// `FParticleSpawnPerUnitInstancePayload::CurrentDistanceTravelled` per module index.
    spu_leftover: Vec<f32>,
    rib: RibbonState,
}

pub struct Instance {
    pub system: Arc<ParticleSystem>,
    pub xf: Transform,
    pub emitters: Vec<EmitterInstance>,
    pub time: f32,
    pub seed: u64,
    pub id: u64,
    pub lod: usize,
    /// `UParticleSystemComponent::PartSysVelocity` (UE world space, cm/s) for `ParticleModuleVelocityInheritParent`; the host sets it (the original derives it from the component's movement).
    pub part_sys_velocity: Vec3,
    params: Params,
}

fn lod_of(em: &Emitter, lod: usize) -> Option<&Lod> { em.lods.get(lod).or_else(|| em.lods.last()).filter(|l| l.enabled) }

impl EmitterInstance {
    fn new(sys: &ParticleSystem, index: usize, lod: usize, seed: u64) -> EmitterInstance {
        let l = lod_of(&sys.emitters[index], lod);
        let nb = l.map_or(0, |l| l.spawn.bursts.len());
        let seeded = l.map(|l| l.modules.iter().map(|m| m.seed.map(Rng::new)).collect()).unwrap_or_default();
        EmitterInstance { index, lod, particles: Vec::new(), rng: Rng::from_pair(seed, index as u64 + 1), seeded, spawn_fraction: 0.0, emitter_time: 0.0, delay_left: l.map_or(0.0, |l| l.required.emitter_delay), loops_done: 0,
            burst_fired: vec![false; nb], completed: l.is_none(), spawned_total: 0, dropped: 0, bursts_fired: 0, halted: false,
            halted_external: false, location: Vec3::ZERO, old_location: Vec3::ZERO, has_location: false, spu_leftover: Vec::new(), rib: RibbonState::default() }
    }
}

impl Instance {
    /// `camera_distance_cm` selects the LOD (`ActivateAutomatic`); `seed` makes the run reproducible.
    pub fn new(system: Arc<ParticleSystem>, xf: Transform, seed: u64, camera_distance_cm: f32) -> Instance {
        let lod = system.lod_for_distance(camera_distance_cm);
        let emitters = (0..system.emitters.len()).map(|i| EmitterInstance::new(&system, i, lod, seed)).collect();
        Instance { system, xf, emitters, time: 0.0, seed, id: 0, lod, part_sys_velocity: Vec3::ZERO, params: Params::new() }
    }
    /// [`new`](Self::new) with the instance parameters already set (`SetColorParameter` ... before the first tick).
    pub fn new_with_params(system: Arc<ParticleSystem>, xf: Transform, seed: u64, camera_distance_cm: f32, params: &Params) -> Instance {
        let mut i = Instance::new(system, xf, seed, camera_distance_cm); i.set_params(params); i
    }
    /// The instance parameters (`UParticleSystemComponent::InstanceParameters`).
    pub fn params(&self) -> &Params { &self.params }
    /// Replaces the instance parameters. Design: the instance owns its copy of the system (`Arc::make_mut`, cloned once on the first change) and the parameter distributions keep the current
    /// parameter value ([`crate::ParamDist::set`]); every later evaluation reads it, exactly like the original reads the component\x27s parameters in `GetValue`. So spawn-time distributions
    /// (start colour, start size, radius, rates, lifetime) change for the NEXT spawns and the per-frame ones (`ColorScaleOverLife`, `SizeScale`, `SizeMultiplyLife`, dynamic material
    /// parameters, `SpawnPerUnit` / rate scale) change the LIVE particles on the next tick. Calling it every tick with changing values (`SizeSphere`) is cheap: no clone after the first call,
    /// and nothing at all when the parameters did not change.
    pub fn set_params(&mut self, params: &Params) {
        if &self.params == params { return; }
        self.params = params.clone();
        Arc::make_mut(&mut self.system).apply_params(params);
    }
    /// `SetFloatParameter` on the instance (see [`set_params`](Self::set_params) for what it affects).
    pub fn set_float_parameter(&mut self, name: &str, v: f32) { let mut p = self.params.clone(); p.set_float(name, v); self.set_params(&p); }
    /// `SetVectorParameter`.
    pub fn set_vector_parameter(&mut self, name: &str, v: [f32; 3]) { let mut p = self.params.clone(); p.set_vector(name, v); self.set_params(&p); }
    /// `SetColorParameter` (linear colour, 8-bit sRGB quantised like the original).
    pub fn set_color_parameter(&mut self, name: &str, c: [f32; 4]) { let mut p = self.params.clone(); p.set_color(name, c); self.set_params(&p); }

    /// `UParticleSystemComponent::SetEmitterEnable(EmitterName, bNewEnableState)` (`SetHaltSpawningExternal(!enable)` of every emitter instance with that `FName`, case-insensitive): a disabled
    /// emitter spawns nothing (its clock, bursts bookkeeping and live particles carry on; particles finish their life). Independent of the LOD level `bEnabled` (an emitter whose LOD level is
    /// disabled never spawns, enabling it here does not change that). Returns false when no emitter has that name.
    pub fn set_emitter_enabled(&mut self, name: &str, enabled: bool) -> bool {
        let sys = self.system.clone(); let mut found = false;
        for e in &mut self.emitters { if sys.emitters[e.index].name.eq_ignore_ascii_case(name) { e.halted_external = !enabled; found = true; } }
        found
    }
    /// False after `set_emitter_enabled(name, false)`; None for an unknown emitter.
    pub fn emitter_enabled(&self, name: &str) -> Option<bool> {
        self.emitters.iter().find(|e| self.system.emitters[e.index].name.eq_ignore_ascii_case(name)).map(|e| !e.halted_external)
    }
    pub fn live_count(&self) -> usize { self.emitters.iter().map(|e| e.particles.len()).sum() }
    pub fn spawned_total(&self) -> u64 { self.emitters.iter().map(|e| e.spawned_total).sum() }
    /// All emitters completed and nothing alive.
    pub fn finished(&self) -> bool { self.emitters.iter().all(|e| e.completed && e.particles.is_empty()) }

    /// The component's bounds box `(min, max)` in UE world cm, `UParticleSystemComponent::CalcBounds` (UE4.16 ParticleComponents.cpp:4252): the template's
    /// `FixedRelativeBoundingBox` moved by the component transform when `bUseFixedRelativeBoundingBox`, else the union of the emitters with live particles of
    /// their `ParticleBoundingBox` (`FParticleEmitterInstance::UpdateBoundingBox`, ParticleEmitterInstances.cpp:1127; mesh emitters :3131), grown by 10 % of its
    /// extent. No live particle at all leaves the `Init()` box (0, 0) as the engine does. `mesh_extent` gives the box extent (cm) of a mesh emitter's mesh bounds
    /// (`MeshTypeData->Mesh->GetBounds().GetBox().GetExtent()`). The bounds origin is what the translucency sort measures (TranslucentRendering.cpp:864).
    pub fn bounds_box(&self, mesh_extent: &dyn Fn(&str) -> Option<[f32; 3]>) -> ([f32; 3], [f32; 3]) {
        let xf = &self.xf;
        if let Some((mn, mx)) = self.system.fixed_bounds {
            // FBox::TransformBy(LocalToWorld): centre moved, extent = sum |extent_i * axis_i|
            let (c, e) = ((Vec3::from(mn) + Vec3::from(mx)) * 0.5, (Vec3::from(mx) - Vec3::from(mn)) * 0.5);
            let c = xf.pos + xf.rot * (c * xf.scale);
            let e = (xf.rot.col(0) * e.x * xf.scale).abs() + (xf.rot.col(1) * e.y * xf.scale).abs() + (xf.rot.col(2) * e.z * xf.scale).abs();
            return ((c - e).to_array(), (c + e).to_array());
        }
        let (mut mn, mut mx, mut any) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN), false);
        for e in &self.emitters {
            if e.particles.is_empty() { continue; }
            let em = &self.system.emitters[e.index];
            let Some(lod) = lod_of(em, e.lod) else { continue };
            // PivotOffset: (-0.5, -0.5, 0) + the first PivotOffset module of the HIGHEST LOD (LODLevels[0]), enabled or not
            let pivot = em.lods.first().and_then(|l| l.modules.iter().find_map(|m| if let ModuleKind::PivotOffset { offset } = &m.kind { Some(*offset) } else { None })).unwrap_or([0.0; 2]);
            let pivot = Vec3::new(-0.5 + pivot[0], -0.5 + pivot[1], 0.0);
            let mesh = if let TypeData::Mesh { mesh, .. } = &lod.type_data { Some(mesh_extent(mesh).map(Vec3::from).unwrap_or(Vec3::ZERO)) } else { None };
            for p in &e.particles {
                let at = if lod.required.use_local_space { xf.pos + xf.rot * (p.loc * xf.scale) } else { p.loc };
                let half = match mesh {
                    Some(ext) => (ext * p.size * xf.scale).abs(),
                    None => {
                        let m = match &p.orbit { Some(o) => o.offset.abs().max_element(), None => (p.size * xf.scale).abs().max_element() };
                        Vec3::splat(m + (p.size * pivot).abs().max_element())
                    }
                };
                mn = mn.min(at - half); mx = mx.max(at + half); any = true;
            }
        }
        if !any { return ([0.0; 3], [0.0; 3]); }
        let grow = (mx - mn) * 0.5 * 0.1;
        ((mn - grow).to_array(), (mx + grow).to_array())
    }

    pub fn tick(&mut self, dt: f32, env: &Env) {
        let dt = dt.clamp(0.0, 0.25);
        self.time += dt;
        let (sys, xf, psv) = (self.system.clone(), self.xf, self.part_sys_velocity);
        for e in &mut self.emitters {
            // Tick_EmitterTimeSetup: OldLocation = Location; Location = component location (both the new one on the first tick, `bJustRegistered`)
            e.old_location = if e.has_location { e.location } else { xf.pos }; e.location = xf.pos; e.has_location = true;
            let Some(lod) = lod_of(&sys.emitters[e.index], e.lod) else { e.completed = true; continue };
            if let TypeData::Ribbon(r) = &lod.type_data { tick_ribbon(e, lod, r, &xf, psv, dt, env); } else { tick_emitter(e, lod, &xf, psv, dt, env); }
        }
    }

    /// `UParticleSystemComponent::Activate(true)` on a running system in a game world (`InitParticles` -> `FParticleEmitterInstance::Init`): every emitter restarts (emitter time,
    /// spawn fraction, loop count and burst list reset, spawning resumes), the particles that are alive stay (UE4.16 ParticleComponents.cpp:4946, ParticleEmitterInstances.cpp:342).
    pub fn reactivate(&mut self) {
        let sys = self.system.clone();
        for e in &mut self.emitters {
            let Some(lod) = lod_of(&sys.emitters[e.index], e.lod) else { continue };
            e.emitter_time = 0.0; e.spawn_fraction = 0.0; e.loops_done = 0; e.delay_left = lod.required.emitter_delay; e.completed = false; e.halted = false;
            e.burst_fired = vec![false; lod.spawn.bursts.len()];
        }
    }

    /// `UParticleSystemComponent::Deactivate()` (`DeactivateSystem`): spawning stops, live particles finish their life.
    pub fn deactivate(&mut self) { for e in &mut self.emitters { e.halted = true; e.completed = true; } }

    /// Simulates `seconds` in fixed steps (tests, tools).
    pub fn run(&mut self, seconds: f32, step: f32, env: &Env) { let n = (seconds / step).round() as usize; for _ in 0..n { self.tick(step, env); } }
}

// ---- one emitter ---------------------------------------------------------------------------------------------------------------------
struct Ctx<'a> { lod: &'a Lod, xf: &'a Transform, local: bool, t: f32, psv: Vec3 }

impl<'a> Ctx<'a> {
    /// Emitter-space vector (cm) -> simulation space (the component rotation + scale when the emitter works in world space).
    fn to_sim(&self, v: Vec3) -> Vec3 { if self.local { v } else { self.xf.rot * (v * self.xf.scale) } }
    /// A vector that is already in world space -> simulation space.
    fn world_to_sim(&self, v: Vec3) -> Vec3 { if self.local { self.xf.rot.transpose() * v / self.xf.scale.max(1e-6) } else { v } }
}

fn tick_emitter(e: &mut EmitterInstance, lod: &Lod, xf: &Transform, psv: Vec3, dt: f32, env: &Env) {
    let req = &lod.required;
    let cx = Ctx { lod, xf, local: req.use_local_space, t: e.emitter_time, psv };
    // 1. reset + relative time
    for p in &mut e.particles {
        p.vel = p.base_vel; p.size = p.base_size; p.rot_rate = p.base_rot_rate; p.color = p.base_color;
        p.rel_time += p.inv_life * dt; p.age += dt;
    }
    // 2. update modules, 3. integrate, 4. kill
    update_particles(e, &cx, dt);
    for p in &mut e.particles { if p.stopped { continue; } p.loc += p.vel * dt; p.rot += p.rot_rate * dt * TAU; p.mesh_rot += p.mesh_rot_rate * dt * TAU; }
    collide(e, &cx, env);
    e.particles.retain(|p| p.rel_time < 1.0);
    // 5. emitter clock + spawning
    if e.completed || e.halted { return; }
    if e.delay_left > 0.0 { e.delay_left -= dt; if e.delay_left > 0.0 { return; } }
    e.emitter_time += dt;
    let t = e.emitter_time;
    let cx = Ctx { lod, xf, local: req.use_local_space, t, psv };
    // EmitterDuration 0 (P_Pickup's Sphere): `bLooped = (EmitterDuration > 0) && (EmitterTime >= EmitterDuration)`, the emitter never loops, its burst list fires once
    let looping = req.emitter_duration > 0.0;
    let duration = req.emitter_duration.max(1e-4);
    if !e.halted_external { spawn_step(e, lod, &cx, dt, t, duration); }
    // 6. loop bookkeeping
    if looping && e.emitter_time >= duration {
        e.loops_done += 1;
        if req.emitter_loops > 0 && e.loops_done >= req.emitter_loops { e.completed = true; } else { e.emitter_time -= duration; for f in &mut e.burst_fired { *f = false; } }
    }
}

pub fn cubic_interp(p0: Vec3, t0: Vec3, p1: Vec3, t1: Vec3, a: f32) -> Vec3 {
    let (a2, a3) = (a * a, a * a * a);
    p0 * (2.0 * a3 - 3.0 * a2 + 1.0) + t0 * (a3 - 2.0 * a2 + a) + t1 * (a3 - a2) + p1 * (-2.0 * a3 + 3.0 * a2)
}
pub fn cubic_interp_derivative(p0: Vec3, t0: Vec3, p1: Vec3, t1: Vec3, a: f32) -> Vec3 {
    let a2 = a * a;
    p0 * (6.0 * a2 - 6.0 * a) + t0 * (3.0 * a2 - 4.0 * a + 1.0) + t1 * (3.0 * a2 - 2.0 * a) + p1 * (-6.0 * a2 + 6.0 * a)
}

fn tick_ribbon(e: &mut EmitterInstance, lod: &Lod, r: &RibbonInfo, xf: &Transform, psv: Vec3, dt: f32, env: &Env) {
    let req = &lod.required;
    let first = !e.rib.started;
    let pos = xf.pos;
    if first { e.rib.last_src = pos; e.rib.last_tan = Vec3::ZERO; e.rib.trail_spawn_time = e.rib.running; e.rib.started = true; }
    e.rib.cur_src = pos;
    let since = e.rib.running - e.rib.trail_spawn_time;
    e.rib.cur_tan = if since != 0.0 { (e.rib.cur_src - e.rib.last_src) / since } else { Vec3::X };
    e.particles.retain(|p| p.rel_time <= 1.0);
    let mut active = !(e.completed || e.halted);
    if active && e.delay_left > 0.0 { e.delay_left -= dt; if e.delay_left > 0.0 { active = false; } }
    if active {
        e.emitter_time += dt;
        let cx = Ctx { lod, xf, local: false, t: e.emitter_time, psv };
        if !e.halted_external { spawn_ribbon(e, lod, r, &cx, xf, dt); }
        let looping = req.emitter_duration > 0.0;
        let duration = req.emitter_duration.max(1e-4);
        if looping && e.emitter_time >= duration {
            e.loops_done += 1;
            if req.emitter_loops > 0 && e.loops_done >= req.emitter_loops { e.completed = true; } else { e.emitter_time -= duration; }
        }
    }
    for p in &mut e.particles {
        p.vel = p.base_vel; p.size = p.base_size; p.rot_rate = p.base_rot_rate; p.color = p.base_color;
        p.rel_time += p.inv_life * dt; p.age += dt;
    }
    let cx = Ctx { lod, xf, local: false, t: e.emitter_time, psv };
    update_particles(e, &cx, dt);
    for p in &mut e.particles { if p.stopped { continue; } p.loc += p.vel * dt; p.rot += p.rot_rate * dt * TAU; }
    collide(e, &cx, env);
    if r.tangent_recalculation_every_frame { recalc_tangents(&mut e.particles); }
    e.rib.running += dt;
}

fn recalc_tangents(ps: &mut [Particle]) {
    let n = ps.len();
    if n < 2 { return; }
    for i in 0..n {
        let (a, b) = if i == n - 1 { (n - 1, n - 2) } else if i == 0 { (1, 0) } else { (i + 1, i - 1) };
        let td = ps[a].trail.spawn_time - ps[b].trail.spawn_time;
        if td.abs() > 1e-8 { ps[i].trail.tangent = (ps[a].loc - ps[b].loc) / td; }
    }
}

fn spawn_ribbon(e: &mut EmitterInstance, lod: &Lod, r: &RibbonInfo, cx: &Ctx, xf: &Transform, dt: f32) {
    let t = cx.t;
    let Some((unit_scalar, spu, max_frame_distance)) = lod.modules.iter().find_map(|m| if let (true, ModuleKind::SpawnPerUnit { unit_scalar, spawn_per_unit, max_frame_distance, .. }) = (m.enabled, &m.kind) { Some((*unit_scalar, spawn_per_unit, *max_frame_distance)) } else { None }) else { return };
    let ppu = spu.eval1(t, &mut e.rng) / unit_scalar;
    let mut count = 0i32;
    if ppu >= 0.0 {
        let leftover = e.rib.src_dist;
        let travel = e.rib.cur_src - e.rib.last_src;
        let mut dist = travel.length();
        if max_frame_distance > 0.0 && dist > max_frame_distance { dist = 0.0; e.rib.last_src = e.rib.cur_src; }
        let mut check = 0.0;
        if r.tangent_spawning_scalar > 0.0 {
            let mut el = e.rib.running;
            if e.particles.is_empty() { if el == 0.0 { el = 1e-4; } e.rib.last_tan = (e.rib.cur_src - e.rib.last_src) / el; }
            let mut div = el - e.rib.trail_spawn_time;
            if div == 0.0 { div = 1e-4; }
            check = ((travel / div).normalize_or_zero().dot(e.rib.last_tan.normalize_or_zero()) - 1.0) * -0.5;
        }
        if dist > 0.0 {
            let nl = (dist + leftover) * ppu + check * r.tangent_spawning_scalar;
            count = if r.spawn_initial_particle && e.particles.is_empty() && nl < 1.0 { 1 } else { nl.floor() as i32 };
            e.rib.src_dist = ((dist + leftover) - count as f32 * unit_scalar).max(0.0);
        }
    }
    if r.max_particle_in_trail_count > 0 {
        let n = e.particles.len() as i32;
        let kill = count + n - r.max_particle_in_trail_count;
        if kill > 0 { let k = (kill as usize).min(e.particles.len()); e.particles.drain(0..k); }
        let n = e.particles.len() as i32;
        if count + n > r.max_particle_in_trail_count { count = (r.max_particle_in_trail_count - n).max(0); }
    }
    if count <= 0 { return; }
    let el = e.rib.running;
    if r.enable_previous_tangent_recalculation && !r.tangent_recalculation_every_frame {
        let spawned: Vec<usize> = (0..e.particles.len()).rev().filter(|&i| !e.particles[i].trail.interpolated).take(2).collect();
        if spawned.len() == 2 {
            let (ns, nn) = (spawned[0], spawned[1]);
            let td = el - e.particles[nn].trail.spawn_time;
            let new_tan = if td > 1e-8 { (e.rib.cur_src - e.particles[nn].loc) / td } else { Vec3::ZERO };
            let tess = e.particles[ns].trail.spawned_tess;
            if tess > 0 {
                let diff = e.particles[ns].trail.spawn_time - e.particles[nn].trail.spawn_time;
                let (p0, t0, p1) = (e.particles[nn].loc, e.particles[nn].trail.tangent, e.particles[ns].loc);
                let inv = 1.0 / tess as f32;
                for k in 0..tess as usize {
                    let i = nn + 1 + k;
                    if i >= e.particles.len() || i > ns { break; }
                    let ts = inv * (k + 1) as f32;
                    e.particles[i].loc = cubic_interp(p0, t0, p1, new_tan * diff, ts);
                    e.particles[i].trail.tangent = cubic_interp_derivative(p0, t0, p1, new_tan * diff, ts) * inv;
                }
            }
            e.rib.last_tan = new_tan;
        }
    }
    let diff = el - e.rib.trail_spawn_time;
    let inv = 1.0 / count as f32;
    let incr = dt / count as f32;
    let (ls, lt, cs, ct) = (e.rib.last_src, e.rib.last_tan * diff, e.rib.cur_src, e.rib.cur_tan * diff);
    let tiling = r.tiling_distance.abs() > 1e-8;
    for i in 0..count {
        let ts = (inv * (i + 1) as f32).clamp(0.0, 1.0);
        let pos = cubic_interp(ls, lt, cs, ct, ts);
        let tan = cubic_interp_derivative(ls, lt, cs, ct, ts);
        let spawn_time = dt - i as f32 * incr;
        if e.particles.len() >= MAX_ACTIVE_PER_EMITTER { e.dropped += 1; continue; }
        let serial = e.spawned_total;
        e.spawned_total += 1;
        let sxf = Transform { pos, rot: xf.rot, scale: xf.scale };
        let scx = Ctx { lod, xf: &sxf, local: false, t: cx.t, psv: cx.psv };
        let mut p = Particle::fresh(pos, serial);
        for (mi, m) in lod.modules.iter().enumerate() {
            if !m.enabled { continue; }
            let rng = match e.seeded.get_mut(mi) { Some(Some(r)) => r, _ => &mut e.rng };
            spawn_module(&m.kind, &mut p, &scx, rng);
        }
        if p.size == Vec3::ZERO { p.size = Vec3::ONE; }
        let newest = e.particles.last().map(|q| q.loc);
        if newest.is_none() {
            let mut inv_life = p.inv_life;
            if inv_life == 0.0 { inv_life = 1.0; }
            if 1.0 / inv_life < 0.001 { inv_life = 1.0 / 0.001; }
            e.rib.inv_life = inv_life;
            e.rib.size = p.size.x;
        }
        p.inv_life = e.rib.inv_life;
        p.size.x = e.rib.size;
        p.size.y = p.size.x;
        p.base_size = p.size;
        p.vel = p.base_vel;
        p.rel_time = spawn_time * p.inv_life;
        p.age = spawn_time;
        p.trail = TrailPoint { tangent: tan * inv, spawn_time: el - diff * (1.0 - ts), tiled_u: 0.0, up: Vec3::Z, interpolated: i != count - 1, spawned_tess: count };
        match newest {
            None => { e.rib.tiled_dist = 0.0; }
            Some(prev) => if tiling { e.rib.tiled_dist += (pos - prev).length(); p.trail.tiled_u = e.rib.tiled_dist / r.tiling_distance; },
        }
        update_particle(&mut p, &scx, 0.0, &mut e.rng);
        e.particles.push(p);
    }
    e.rib.last_src = e.rib.cur_src;
    e.rib.last_tan = e.rib.cur_tan;
    e.rib.trail_spawn_time = el;
}

pub const RIBBON_VERTEX_FLOATS: usize = 24;

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] { [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t] }

#[allow(clippy::too_many_arguments)]
fn ribbon_vertex(verts: &mut Vec<f32>, pos: Vec3, size: f32, u: f32, v: f32, u2: f32, rot: f32, color: [f32; 4], dynp: [f32; 4], rel: f32) {
    verts.extend_from_slice(&[pos.x, pos.y, pos.z, rel, pos.x, pos.y, pos.z, 0.0, size, size, rot, 0.0, u, v, u2, v]);
    verts.extend_from_slice(&color);
    verts.extend_from_slice(&dynp);
}

pub fn ribbon_geometry(ps: &[Particle], r: &RibbonInfo, scale: f32, cam: Vec3, cam_up: Vec3, verts: &mut Vec<f32>, indices: &mut Vec<u32>) {
    let n = ps.len();
    if n < 2 { return; }
    let check_tan = r.tangent_tessellation_scalar.abs() > 1e-8 || r.enable_tangent_diff_interp_scale;
    let dist_tess = r.distance_tessellation_step_size.abs() > 1e-8;
    let mut interp = vec![1i32; n];
    for i in 0..n - 1 {
        let (c, pv) = (&ps[i], &ps[i + 1]);
        let d = (c.loc - pv.loc).length();
        let ct = if check_tan { (c.trail.tangent.normalize_or_zero().dot(pv.trail.tangent.normalize_or_zero()) - 1.0) * -0.5 } else { 0.0 };
        let mut dd = 0.0;
        if dist_tess { dd = d / r.distance_tessellation_step_size; if r.enable_tangent_diff_interp_scale && ct < 0.5 { dd *= 2.0 * ct.clamp(0.0, 0.5); } }
        let k = dd.trunc() as i32 + (ct * r.tangent_tessellation_scalar).trunc() as i32;
        interp[i] = k.max(1);
    }
    let total: i32 = interp.iter().map(|k| 2 * k).sum::<i32>();
    let tri = total - 2;
    if tri <= 0 { return; }
    let tex_inc = 1.0 / (tri as f32 / 2.0);
    let up_of = |p: &Particle| -> Vec3 {
        if r.render_axis != 0 { return p.trail.up; }
        let dc = (p.loc - cam).normalize_or_zero();
        let u = p.trail.tangent.normalize_or_zero().cross(dc);
        if u.length_squared() < 1e-8 { cam_up } else { u.normalize() }
    };
    let tiling = r.tiling_distance.abs() > 1e-8;
    let base = (verts.len() / RIBBON_VERTEX_FLOATS) as u32;
    let mut tex_u = 0.0f32;
    let mut prev: Option<(usize, Vec3)> = None;
    for i in (0..n).rev() {
        let p = &ps[i];
        let up = up_of(p);
        let size = p.size.x * scale;
        let rel = p.rel_time.clamp(0.0, 1.0);
        let k = interp[i];
        match prev {
            Some((qi, qup)) if k > 1 => {
                let q = &ps[qi];
                let qsize = q.size.x * scale;
                let invc = 1.0 / k as f32;
                for s in (0..k).rev() {
                    let ts = invc * s as f32;
                    let ip = cubic_interp(p.loc, p.trail.tangent, q.loc, q.trail.tangent, ts);
                    let iu = up.lerp(qup, ts);
                    let ic = lerp4(p.color, q.color, ts);
                    let isz = size + (qsize - size) * ts;
                    let id = lerp4(p.dynamic, q.dynamic, ts);
                    let u2 = if tiling { p.trail.tiled_u + (q.trail.tiled_u - p.trail.tiled_u) * ts } else { tex_u };
                    ribbon_vertex(verts, ip + iu * isz, isz, tex_u, 0.0, u2, p.rot, ic, id, rel);
                    ribbon_vertex(verts, ip - iu * isz, isz, tex_u, 1.0, u2, p.rot, ic, id, rel);
                    tex_u += tex_inc;
                }
            }
            _ => {
                let u2 = if tiling { p.trail.tiled_u } else { tex_u };
                ribbon_vertex(verts, p.loc + up * size, size, tex_u, 0.0, u2, p.rot, p.color, p.dynamic, rel);
                ribbon_vertex(verts, p.loc - up * size, size, tex_u, 1.0, u2, p.rot, p.color, p.dynamic, rel);
                tex_u += tex_inc;
            }
        }
        prev = Some((i, up));
    }
    let count = (verts.len() / RIBBON_VERTEX_FLOATS) as u32 - base;
    for k in 0..count.saturating_sub(2) {
        let a = base + k;
        if k % 2 == 0 { indices.extend([a, a + 1, a + 2]); } else { indices.extend([a + 1, a, a + 2]); }
    }
}

/// `FParticleEmitterInstance::Spawn`: spawn modules (`ParticleModuleSpawnPerUnit`), continuous rate, burst list.
fn spawn_step(e: &mut EmitterInstance, lod: &Lod, cx: &Ctx, dt: f32, t: f32, duration: f32) {
    // spawning modules in list order: SpawnPerUnit adds `Number / DeltaTime` to the rate and may switch the default spawn rate off
    let (mut spu_rate, mut process_rate) = (0.0f32, true);
    for (mi, m) in lod.modules.iter().enumerate() {
        if !m.enabled { continue; }
        if let ModuleKind::SpawnPerUnit { unit_scalar, spawn_per_unit, max_frame_distance, ignore_spawn_rate_when_moving, movement_tolerance, ignore_movement } = &m.kind {
            let (r, process) = spawn_per_unit_amount(e, mi, *unit_scalar, spawn_per_unit, *max_frame_distance, *ignore_spawn_rate_when_moving, *movement_tolerance, *ignore_movement, t, dt);
            spu_rate += r.max(0.0);
            if !process { process_rate = false; }
        }
    }
    // continuous rate (`SpawnFraction` accumulation of FParticleEmitterInstance::Tick)
    let mut rate = spu_rate;
    if process_rate { rate += lod.spawn.rate.eval1(t, &mut e.rng) * lod.spawn.rate_scale.eval1(t, &mut e.rng).max(0.0); }
    let rate = rate.max(0.0);
    let (mut count, mut start_time, mut increment) = (0usize, dt, 0.0f32);
    if rate > 0.0 {
        let old = e.spawn_fraction; let new = old + dt * rate;
        count = new.floor() as usize; e.spawn_fraction = new - count as f32;
        increment = 1.0 / rate; start_time = dt + old * increment - increment;
    }
    // bursts: `ParticleBurst.Time` is normalised to the emitter duration
    let mut burst_total = 0usize;
    for (i, b) in lod.spawn.bursts.iter().enumerate() {
        if e.burst_fired.get(i).copied().unwrap_or(true) { continue; }
        if b.time * duration <= t {
            e.burst_fired[i] = true; e.bursts_fired += 1;
            let mut n = b.count;
            if b.count_low >= 0 { n = b.count_low + (e.rng.frac() * (b.count - b.count_low + 1) as f32) as i32; }
            let scale = lod.spawn.burst_scale.eval1(t, &mut e.rng);
            burst_total += (n as f32 * scale).round().max(0.0) as usize;
        }
    }
    spawn_particles(e, cx, dt, count, start_time, increment, burst_total);
}

/// `UParticleModuleSpawnPerUnit::GetSpawnAmount` (UE4.16 ParticleModules_Spawn.cpp:304) -> (spawn rate to add = `Number * InvDeltaTime`, whether the default spawn rate is still processed).
/// The distance is the world-space movement of the component between the last two ticks (cm), minus ignored axes; the fractional rest is carried in the module payload. Like the
/// original the leftover distance subtracts `Number * UnitScalar` (exact only for SpawnPerUnit = 1).
#[allow(clippy::too_many_arguments)]
fn spawn_per_unit_amount(e: &mut EmitterInstance, mi: usize, unit_scalar: f32, dist: &crate::dist::Dist, max_frame_distance: f32, ignore_rate_when_moving: bool, tolerance: f32, ignore: [bool; 3], t: f32, dt: f32) -> (f32, bool) {
    if e.spu_leftover.len() <= mi { e.spu_leftover.resize(mi + 1, 0.0); }
    let mut moved = false; let mut rate = 0.0f32;
    let ppu = dist.eval1(t, &mut e.rng) / unit_scalar;
    // PPU of 0 turns the emitter off while moving
    if ppu >= 0.0 {
        let leftover = e.spu_leftover[mi];
        let mask = Vec3::new(if ignore[0] { 0.0 } else { 1.0 }, if ignore[1] { 0.0 } else { 1.0 }, if ignore[2] { 0.0 } else { 1.0 });
        let travel = (e.location - e.old_location) * mask;
        let mut distance = travel.length();
        if max_frame_distance > 0.0 && distance > max_frame_distance { distance = 0.0; e.spu_leftover[mi] = 0.0; }
        if distance > 0.0 {
            if distance > tolerance * unit_scalar { moved = true; }
            let number = ((distance + leftover) * ppu).floor();
            let inv_dt = if dt > 0.0 { 1.0 / dt } else { 0.0 };
            rate = number * inv_dt;
            e.spu_leftover[mi] = ((distance + leftover) - number * unit_scalar).max(0.0);
        }
    }
    // bIgnoreSpawnRateWhenMoving: false while moving; otherwise bProcessSpawnRate (true)
    (rate, if ignore_rate_when_moving { !moved } else { true })
}

fn spawn_particles(e: &mut EmitterInstance, cx: &Ctx, dt: f32, count: usize, start_time: f32, increment: f32, burst: usize) {
    let total = count + burst;
    if total == 0 { return; }
    let max_active = match &cx.lod.type_data { TypeData::Gpu(g) if g.max_particle_count > 0 => (g.max_particle_count as usize).min(MAX_ACTIVE_PER_EMITTER), _ => MAX_ACTIVE_PER_EMITTER };
    let mut st = start_time;
    for i in 0..total {
        // continuous particles are spread over the frame, bursts spawn at the burst time (sub-frame time 0 .. dt)
        let spawn_time = if i < count { let s = st.max(0.0); st -= increment; s.min(dt) } else { 0.0 };
        if e.particles.len() >= max_active { e.dropped += 1; continue; }
        let serial = e.spawned_total; e.spawned_total += 1;
        let origin = if cx.local { Vec3::ZERO } else { cx.xf.pos };
        let mut p = Particle::fresh(origin, serial);
        for (mi, m) in cx.lod.modules.iter().enumerate() {
            if !m.enabled { continue; }
            let rng = match e.seeded.get_mut(mi) { Some(Some(r)) => r, _ => &mut e.rng };
            spawn_module(&m.kind, &mut p, cx, rng);
        }
        // no (or a zero) Lifetime: OneOverMaxLifetime stays 0 and the particle never dies, like the original
        if p.size == Vec3::ZERO { p.size = Vec3::ONE; p.base_size = Vec3::ONE; }
        p.vel = p.base_vel;
        // PostSpawn: world-space emitters spread the particles of one frame over the path the component moved (`Interp` runs 1 -> 0 inside the continuous / burst group)
        if !cx.local && (e.old_location - e.location).length_squared() > 1.0 {
            let (n, k) = if i < count { (count, i) } else { (burst, i - count) };
            p.loc += (1.0 - (k + 1) as f32 / n as f32) * (e.old_location - e.location);
        }
        p.loc += p.vel * spawn_time;
        p.rel_time = spawn_time * p.inv_life; p.age = spawn_time;
        // modules that integrate over time were skipped for the part of the frame before the spawn: apply them for `spawn_time`
        // an update evaluation with dt = 0 so the first draw already has colour / size over life
        update_particle(&mut p, cx, 0.0, &mut e.rng);
        e.particles.push(p);
    }
}

fn update_particles(e: &mut EmitterInstance, cx: &Ctx, dt: f32) {
    let mut ps = std::mem::take(&mut e.particles);
    for p in ps.iter_mut() { update_particle(p, cx, dt, &mut e.rng); }
    e.particles = ps;
}

/// The update modules in list order for one particle. A stopped particle (collision response Stop) only keeps its over-life modules.
fn update_particle(p: &mut Particle, cx: &Ctx, dt: f32, rng: &mut Rng) {
    for m in &cx.lod.modules {
        if !m.enabled { continue; }
        let over_life = matches!(m.kind, ModuleKind::ColorOverLife { .. } | ModuleKind::ColorScaleOverLife { .. } | ModuleKind::SizeMultiplyLife { .. } | ModuleKind::SizeScale { .. } | ModuleKind::SizeScaleBySpeed { .. } | ModuleKind::SubUv { .. } | ModuleKind::SubUvMovie { .. } | ModuleKind::ParameterDynamic { .. });
        if p.stopped && !over_life { continue; }
        update_module(&m.kind, p, cx, dt, rng);
    }
}

// ---- spawn modules -------------------------------------------------------------------------------------------------------------------
fn v(a: [f32; 3]) -> Vec3 { Vec3::new(a[0], a[1], a[2]) }

fn sphere_point(rng: &mut Rng, surface: bool, pos_z: bool, neg_z: bool) -> Vec3 {
    for _ in 0..16 {
        let u = Vec3::from(rng.unit_vec());
        if (!pos_z && u.z > 0.0) || (!neg_z && u.z < 0.0) { continue; }
        let r = if surface { 1.0 } else { rng.frac().cbrt() };
        return u * r;
    }
    Vec3::Z
}

fn spawn_module(kind: &ModuleKind, p: &mut Particle, cx: &Ctx, rng: &mut Rng) {
    let t = cx.t;
    match kind {
        ModuleKind::Lifetime { lifetime } => {
            let l = lifetime.eval1(t, rng).max(0.0);
            // `Particle.OneOverMaxLifetime = MaxLife > 0 ? 1 / (MaxLife + Lifetime) : 1 / Lifetime` (several Lifetime modules add up)
            let max_life = if p.inv_life > 0.0 { 1.0 / p.inv_life } else { 0.0 };
            // UParticleModuleLifetime::SpawnEx (ParticleModules.cpp:3860): the first module sets `MaxLifetime > 0 ? 1 / MaxLifetime : 0`; a particle with OneOverMaxLifetime 0 never ages (the lifetime-0 mesh emitters of P_Pickup)
            p.inv_life = if max_life > 0.0 { 1.0 / (l + max_life) } else if l > 0.0 { 1.0 / l } else { 0.0 };
        }
        ModuleKind::Location { start } => { p.loc += cx.to_sim(v(start.eval3(t, rng))); }
        ModuleKind::LocationSphere { start, radius, surface_only, velocity, velocity_scale, positive_z, negative_z } => {
            let r = radius.eval1(t, rng); let u = sphere_point(rng, *surface_only, *positive_z, *negative_z);
            let off = u * r; p.loc += cx.to_sim(v(start.eval3(t, rng)) + off);
            if *velocity { let dir = off.normalize_or_zero(); let s = velocity_scale.eval1(t, rng); p.vel += cx.to_sim(dir * s); p.base_vel += cx.to_sim(dir * s); }
        }
        ModuleKind::LocationCylinder { start, radius, height, height_axis, surface_only, velocity, velocity_scale, positive_z, negative_z } => {
            let r = radius.eval1(t, rng); let h = height.eval1(t, rng);
            let a = rng.range(0.0, TAU); let rr = if *surface_only { 1.0 } else { rng.frac().sqrt() };
            let mut hz = rng.range(-0.5, 0.5);
            if !*positive_z && hz > 0.0 { hz = -hz; } if !*negative_z && hz < 0.0 { hz = -hz; }
            if *surface_only && rng.frac() < 0.25 { hz = if hz >= 0.0 { 0.5 } else { -0.5 }; }
            let (rx, ry) = (a.cos() * r * rr, a.sin() * r * rr); let hh = hz * h;
            let off = match height_axis { Axis::Z => Vec3::new(rx, ry, hh), Axis::X => Vec3::new(hh, rx, ry), Axis::Y => Vec3::new(rx, hh, ry) };
            p.loc += cx.to_sim(v(start.eval3(t, rng)) + off);
            if *velocity { let mut dir = off; match height_axis { Axis::Z => dir.z = 0.0, Axis::X => dir.x = 0.0, Axis::Y => dir.y = 0.0 } let dir = dir.normalize_or_zero(); let s = velocity_scale.eval1(t, rng); p.vel += cx.to_sim(dir * s); p.base_vel += cx.to_sim(dir * s); }
        }
        ModuleKind::LocationTriangle { start_offset, height, angle, thickness } => {
            // BVISFIX: UParticleModuleLocationPrimitiveTriangle::SpawnEx (UE4.16 ParticleModules_Location.cpp:851-893): the four distributions at EmitterTime, the
            // corners Offset + (+H/2, 0, 0), (-H/2, +B, 0), (-H/2, -B, 0) with B = H * tan(Angle / 2 deg), three fractions normalised by their sum (at least
            // KINDA_SMALL_NUMBER) as barycentric weights, then Z = frac * Thickness - Thickness / 2; the offset goes through EmitterToSimulation.TransformVector
            let o = v(start_offset.eval3(t, rng)); let h = height.eval1(t, rng); let ang = angle.eval1(t, rng); let th = thickness.eval1(t, rng);
            let b = h * (0.5 * ang * std::f32::consts::PI / 180.0).tan();
            let corners = [o + Vec3::new(h * 0.5, 0.0, 0.0), o + Vec3::new(-h * 0.5, b, 0.0), o + Vec3::new(-h * 0.5, -b, 0.0)];
            let bc = [rng.frac(), rng.frac(), rng.frac()];
            let z = rng.frac();
            let sum = (bc[0] + bc[1] + bc[2]).max(1.0e-4);
            let mut off = Vec3::ZERO;
            for i in 0..3 { off += (bc[i] / sum) * corners[i]; }
            off.z = z * th - 0.5 * th;
            p.loc += cx.to_sim(off);
        }
        ModuleKind::Velocity { start, radial, in_world_space } => {
            let vel = v(start.eval3(t, rng)); let s = if *in_world_space { cx.world_to_sim(vel) } else { cx.to_sim(vel) };
            p.vel += s; p.base_vel += s;
            let rs = radial.eval1(t, rng);
            if rs != 0.0 { let origin = if cx.local { Vec3::ZERO } else { cx.xf.pos }; let d = (p.loc - origin).normalize_or_zero() * rs; p.vel += d; p.base_vel += d; }
        }
        ModuleKind::Size { start } => { let s = v(start.eval3(t, rng)); p.size += s; p.base_size += s; }
        // UParticleModuleSizeScale::Spawn: `Particle.Size = BaseSize * SizeScale(RelativeTime)` (EnableX/Y/Z are not read by the original)
        ModuleKind::SizeScale { scale } => { p.size = p.base_size * v(scale.eval3(p.rel_time.clamp(0.0, 1.0), rng)); }
        ModuleKind::Color { color, alpha } => {
            let c = color.eval3(t, rng); let a = alpha.eval1(t, rng);
            p.color = [c[0], c[1], c[2], a]; p.base_color = p.color;
        }
        ModuleKind::Rotation { start } => { p.rot += start.eval1(t, rng) * TAU; }
        ModuleKind::RotationRate { start } => { let r = start.eval1(t, rng); p.rot_rate += r; p.base_rot_rate += r; }
        ModuleKind::MeshRotation { start } => { p.mesh_rot += v(start.eval3(t, rng)) * TAU; }
        ModuleKind::MeshRotationRate { start } => { p.mesh_rot_rate += v(start.eval3(t, rng)); }
        ModuleKind::CameraOffset { offset, spawn_time_only } => { if *spawn_time_only { p.cam_offset = offset.eval1(t, rng); } }
        ModuleKind::Orbit { offset, rotation, rotation_rate } => {
            p.orbit = Some(Orbit { offset: v(offset.eval3(t, rng)), rotation: v(rotation.eval3(t, rng)), rate: v(rotation_rate.eval3(t, rng)) });
        }
        ModuleKind::SubUvMovie { starting_frame, .. } => { p.sub_image = *starting_frame as f32; }
        ModuleKind::Light { color_scale, brightness, radius_scale, exponent, inverse_squared, spawn_fraction, .. } => {
            // UParticleModuleLight::SpawnEx: Brightness / ColorScale at the particle's relative time, RadiusScale / LightExponent at the emitter time; exponent 0 = inverse squared falloff
            let rel = p.rel_time.clamp(0.0, 1.0);
            let b = brightness.eval1(rel, rng); let c = color_scale.eval3(rel, rng);
            let radius = radius_scale.eval1(t, rng); let e = if *inverse_squared { 0.0 } else { exponent.eval1(t, rng) };
            p.light = Some(LightPayload { color_scale: [c[0] * b, c[1] * b, c[2] * b], radius_scale: radius, exponent: e, valid: rng.frac() < *spawn_fraction });
        }
        ModuleKind::VelocityInheritParent { scale } => {
            // component velocity (world) -> emitter space when the emitter is local; scaled by Scale(EmitterTime)
            let vel = cx.world_to_sim(cx.psv) * v(scale.eval3(t, rng));
            p.vel += vel; p.base_vel += vel;
        }
        ModuleKind::MeshRotationRateMultiplyLife { mult } => { p.mesh_rot_rate *= v(mult.eval3(p.rel_time.clamp(0.0, 1.0), rng)); }
        _ => {}
    }
}

// ---- update modules -------------------------------------------------------------------------------------------------------------------
fn update_module(kind: &ModuleKind, p: &mut Particle, cx: &Ctx, dt: f32, rng: &mut Rng) {
    let rel = p.rel_time.clamp(0.0, 1.0);
    match kind {
        ModuleKind::Acceleration { accel, world } => {
            let a = v(accel.eval3(rel, rng)); let a = if *world && cx.local { cx.world_to_sim(a) } else { a };
            p.vel += a * dt; p.base_vel += a * dt;
        }
        ModuleKind::AccelerationConstant { accel, world } => {
            let a = v(*accel); let a = if *world && cx.local { cx.world_to_sim(a) } else { a };
            p.vel += a * dt; p.base_vel += a * dt;
        }
        ModuleKind::Drag { coefficient } => {
            let k = (1.0 - coefficient.eval1(rel, rng) * dt).max(0.0);
            p.vel *= k; p.base_vel *= k;
        }
        ModuleKind::ColorOverLife { color, alpha, clamp_alpha } => {
            let c = color.eval3(rel, rng); let mut a = alpha.eval1(rel, rng); if *clamp_alpha { a = a.clamp(0.0, 1.0); }
            p.color = [c[0], c[1], c[2], a];
        }
        ModuleKind::ColorScaleOverLife { color, alpha } => {
            let c = color.eval3(rel, rng); let a = alpha.eval1(rel, rng);
            p.color = [p.color[0] * c[0], p.color[1] * c[1], p.color[2] * c[2], p.color[3] * a];
        }
        ModuleKind::SizeScale { scale } => { p.size = p.base_size * v(scale.eval3(rel, rng)); }
        ModuleKind::SizeMultiplyLife { mult, x, y, z } => {
            let m = v(mult.eval3(rel, rng));
            if *x { p.size.x *= m.x; } if *y { p.size.y *= m.y; } if *z { p.size.z *= m.z; }
        }
        ModuleKind::SizeScaleBySpeed { speed_scale, max_scale } => {
            let speed = p.vel.length();
            p.size.x *= (speed * speed_scale[0]).clamp(1.0, max_scale[0].max(1.0)); p.size.y *= (speed * speed_scale[1]).clamp(1.0, max_scale[1].max(1.0));
        }
        ModuleKind::SubUv { index } => { p.sub_image = index.eval1(rel, rng); }
        ModuleKind::SubUvMovie { index, frame_rate, starting_frame } => {
            let fr = frame_rate.eval1(rel, rng);
            p.sub_image = *starting_frame as f32 + fr * p.age + index.eval1(rel, rng);
        }
        ModuleKind::CameraOffset { offset, spawn_time_only } => { if !*spawn_time_only { p.cam_offset = offset.eval1(rel, rng); } }
        ModuleKind::Orbit { .. } => { if let Some(o) = p.orbit.as_mut() { o.rotation += o.rate * dt; } }
        ModuleKind::Light { color_scale, brightness, .. } => {
            if let Some(l) = p.light.as_mut() { let b = brightness.eval1(rel, rng); let c = color_scale.eval3(rel, rng); l.color_scale = [c[0] * b, c[1] * b, c[2] * b]; }
        }
        // the spawn evaluation already applied once; the original multiplies again on every update tick (not on the extra dt = 0 first-frame evaluation of this port)
        ModuleKind::MeshRotationRateMultiplyLife { mult } => { if dt > 0.0 { p.mesh_rot_rate *= v(mult.eval3(rel, rng)); } }
        ModuleKind::ParameterDynamic { params } => {
            for (i, d) in params.iter().take(4).enumerate() {
                if d.spawn_time_only && p.age > dt + 1e-6 { continue; }
                let t = if d.use_emitter_time { cx.t } else { rel };
                p.dynamic[i] = d.value.eval1(t, rng);
            }
        }
        _ => {}
    }
}

fn collide(e: &mut EmitterInstance, cx: &Ctx, env: &Env) {
    let Some(ground) = env.ground else { return };
    // GPU collision module (DistanceField in the original) and the CPU collision module, both against the host's ground function.
    let gpu = cx.lod.modules.iter().find_map(|m| match (&m.kind, m.enabled) { (ModuleKind::CollisionGpu { friction, resilience, resilience_scale_over_life, radius_scale, stop }, true) => Some((*friction, resilience, resilience_scale_over_life, *radius_scale, *stop)), _ => None });
    let cpu = cx.lod.modules.iter().find_map(|m| match (&m.kind, m.enabled) { (ModuleKind::Collision { damping, max_collisions, halt_when_done, .. }, true) => Some((damping, max_collisions, *halt_when_done)), _ => None });
    if gpu.is_none() && cpu.is_none() { return; }
    let (rot, scale) = (cx.xf.rot, cx.xf.scale.max(1e-6));
    for p in &mut e.particles {
        if p.stopped { continue; }
        let wpos = if cx.local { cx.xf.pos + rot * (p.loc * scale) } else { p.loc };
        let radius = gpu.map_or(0.0, |g| 0.5 * p.size.x.abs().max(p.size.y.abs()) * g.3 * scale);
        let gz = ground(wpos.x, wpos.y);
        if wpos.z - radius > gz { continue; }
        let wvel = if cx.local { rot * (p.vel * scale) } else { p.vel };
        let (n, vn) = (Vec3::Z, wvel.z);
        if vn > 0.0 { continue; }                                   // moving away from the ground (spawned below it, or already bounced)
        let vt = wvel - n * vn;
        let mut rng = Rng::new((p.serial as i32).wrapping_mul(7919) ^ 0x5bd1);
        let new_vel;
        if let Some((friction, resilience, over_life, _, stop)) = gpu {
            if stop { new_vel = Vec3::ZERO; p.stopped = true; }
            else {
                // `Resilience` x `ResilienceScaleOverLife(relative time)`; friction removes that share of the tangential velocity
                let res = resilience.eval1(p.rel_time, &mut rng).max(0.0) * over_life.eval1(p.rel_time, &mut rng).max(0.0);
                new_vel = n * (-vn * res) + vt * (1.0 - friction).clamp(0.0, 1.0);
                if new_vel.length() < 5.0 { p.stopped = true; }
            }
        } else if let Some((damping, max_collisions, halt)) = cpu {
            let d = v(damping.eval3(p.rel_time, &mut rng)); let d = if d == Vec3::ZERO { Vec3::splat(0.5) } else { d };
            new_vel = (wvel - 2.0 * vn * n) * d; p.collisions += 1;
            let max = max_collisions.eval1(0.0, &mut rng); let max = if max <= 0.0 { 3 } else { max as u32 };
            if halt && (p.collisions >= max || new_vel.length() < 5.0) { p.stopped = true; }
        } else { continue; }
        let new_world = Vec3::new(wpos.x, wpos.y, gz + radius);
        p.loc = if cx.local { rot.transpose() * (new_world - cx.xf.pos) / scale } else { new_world };
        let sv = if cx.local { rot.transpose() * new_vel / scale } else { new_vel };
        p.vel = sv; p.base_vel = sv;
    }
}
// ---- draw data ------------------------------------------------------------------------------------------------------------------------
/// One particle ready to draw, in game space (metres, Y up).
#[derive(Clone, Copy, Debug)]
pub struct DrawParticle {
    pub pos: [f32; 3],
    /// Sprites: width, height (m), z unused. Meshes: scale factors.
    pub size: [f32; 3],
    pub rotation: f32,
    pub color: [f32; 4],
    /// Fractional sub-image index.
    pub sub_image: f32,
    pub velocity: [f32; 3],
    /// Meshes: orientation as a column-major 3x3 in game space.
    pub orient: [[f32; 3]; 3],
    /// Dynamic material parameters (`ParticleModuleParameterDynamic`, up to 4).
    pub dynamic: [f32; 4],
    pub rel_time: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawKind { Sprite, Mesh, Ribbon }

pub struct EmitterDraw {
    pub system: String,
    pub package: String,
    pub emitter: String,
    pub emitter_index: usize,
    pub kind: DrawKind,
    /// Sprites: `Required.Material`. Meshes: the override materials when a MeshMaterial module is present, else empty (the mesh's own materials).
    pub material: String,
    pub material_override: Vec<String>,
    pub mesh: String,
    pub sub_images: (u32, u32),
    pub interpolation: SubUvInterp,
    pub alignment: Alignment,
    pub sort: SortMode,
    pub pivot_offset: [f32; 2],
    /// `ParticleModuleOrientationAxisLock.LockAxisFlags` of the enabled lock module (`AxisLock::None` without one). Sprite emitters only; the renderer orients the sprites (mesh emitters use
    /// `TypeDataMesh.AxisLockOption`, not decoded).
    pub axis_lock: AxisLock,
    /// The locked sprite axes in game space (unit vectors, `ComputeLockedAxes` of UE4.16 ParticleSystemRender.cpp:294 with the component rotation applied when the emitter uses local space):
    /// `(up, right)`; `up` is zero for the rotation locks (`RotateX/Y/Z`: the sprite spins about `right`). `None` for `AxisLock::None`.
    pub axis_lock_axes: Option<([f32; 3], [f32; 3])>,
    /// `Required.bUseLocalSpace` of the emitter LOD (`particles` are world space either way; the FX draw puts them back into component space for the `Primitive` buffer, like the engine).
    pub use_local_space: bool,
    /// Mesh emitters: `TypeDataMesh.bOverrideMaterial` (the mesh is drawn with `material` of the Required module instead of its own slot materials).
    pub mesh_override_material: bool,
    /// LOD level the instance draws this emitter at.
    pub lod_level: usize,
    pub particles: Vec<DrawParticle>,
    pub ribbon_vertices: Vec<f32>,
    pub ribbon_indices: Vec<u32>,
}

fn euler_matrix(rot: Vec3) -> Mat3 {
    // UE FRotator(pitch = rot.y, yaw = rot.z, roll = rot.x) -> rotation matrix (left-handed engine convention approximated by the standard Z-Y-X composition)
    Mat3::from_rotation_z(rot.z) * Mat3::from_rotation_y(-rot.y) * Mat3::from_rotation_x(rot.x)
}

impl Instance {
    /// Appends the draw data of every emitter that has live particles. `camera_game` = eye position (m, game axes), used for the camera offset and the depth sort.
    pub fn draw(&self, camera_game: [f32; 3], out: &mut Vec<EmitterDraw>) {
        let cam = game_to_ue(camera_game);
        for e in &self.emitters {
            if e.particles.is_empty() { continue; }
            let Some(lod) = lod_of(&self.system.emitters[e.index], e.lod) else { continue };
            let (kind, mesh) = match &lod.type_data { TypeData::Mesh { mesh, .. } => (DrawKind::Mesh, mesh.clone()), TypeData::Ribbon(_) => (DrawKind::Ribbon, String::new()), _ => (DrawKind::Sprite, String::new()) };
            let material_override = lod.modules.iter().find_map(|m| if let (true, ModuleKind::MeshMaterial { materials }) = (m.enabled, &m.kind) { Some(materials.clone()) } else { None }).unwrap_or_default();
            let pivot = lod.modules.iter().find_map(|m| if let (true, ModuleKind::PivotOffset { offset }) = (m.enabled, &m.kind) { Some(*offset) } else { None }).unwrap_or([0.0; 2]);
            let xf = &self.xf;
            let axis_lock = lod.modules.iter().find_map(|m| if let (true, ModuleKind::OrientationAxisLock { axis }) = (m.enabled, &m.kind) { Some(*axis) } else { None }).unwrap_or_default();
            let axis_lock_axes = axis_lock.locked_axes().map(|(up, right)| {
                let l2w = if lod.required.use_local_space { xf.rot } else { Mat3::IDENTITY };
                let dir = |(a, sign): (usize, f32)| { let c = swap_yz(l2w.col(a).normalize_or_zero() * sign); [c.x, c.y, c.z] };
                (up.map_or([0.0; 3], dir), dir(right))
            });
            let mut list: Vec<(f32, DrawParticle)> = Vec::with_capacity(e.particles.len());
            for p in &e.particles {
                let mut world = if lod.required.use_local_space { xf.pos + xf.rot * (p.loc * xf.scale) } else { p.loc };
                let wvel = if lod.required.use_local_space { xf.rot * (p.vel * xf.scale) } else { p.vel };
                if let Some(o) = &p.orbit { world += euler_matrix(o.rotation * TAU) * o.offset * xf.scale; }
                if p.cam_offset != 0.0 { let d = (cam - world).normalize_or_zero(); world += d * p.cam_offset; }
                let depth = (world - cam).length_squared();
                let size = p.size * xf.scale;
                let (size_out, orient) = match kind {
                    DrawKind::Sprite | DrawKind::Ribbon => ([size.x * 0.01, (if matches!(lod.required.screen_alignment, Alignment::Square) { size.x } else { size.y }) * 0.01, size.z * 0.01], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
                    DrawKind::Mesh => {
                        let rpy = if let TypeData::Mesh { roll_pitch_yaw, .. } = &lod.type_data { v(roll_pitch_yaw.mean(0.0, 3)) } else { Vec3::ZERO };
                        let m = Mat3::from_rotation_z(rpy.z.to_radians()) * euler_matrix(p.mesh_rot);
                        let m = xf.rot * m;
                        // game space: P R P with P = swap(y, z)
                        let col = |c: Vec3| { let g = swap_yz(m * swap_yz(c)); [g.x, g.y, g.z] };
                        ([p.size.x * xf.scale, p.size.z * xf.scale, p.size.y * xf.scale], [col(Vec3::X), col(Vec3::Y), col(Vec3::Z)])
                    }
                };
                list.push((depth, DrawParticle { pos: ue_to_game(world), size: size_out, rotation: p.rot, color: p.color, sub_image: p.sub_image, velocity: ue_to_game(wvel), orient, dynamic: p.dynamic, rel_time: p.rel_time.clamp(0.0, 1.0) }));
            }
            if lod.required.sort_mode != SortMode::None && kind != DrawKind::Ribbon { list.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)); }
            let (mut ribbon_vertices, mut ribbon_indices) = (vec![], vec![]);
            if let TypeData::Ribbon(r) = &lod.type_data { ribbon_geometry(&e.particles, r, xf.scale, cam, Vec3::Z, &mut ribbon_vertices, &mut ribbon_indices); }
            out.push(EmitterDraw { system: self.system.name.clone(), package: self.system.package.clone(), emitter: self.system.emitters[e.index].name.clone(), emitter_index: e.index, kind, material: lod.required.material.clone(), material_override, mesh,
                sub_images: (lod.required.sub_images_h, lod.required.sub_images_v), interpolation: lod.required.interpolation, alignment: lod.required.screen_alignment, sort: lod.required.sort_mode, pivot_offset: pivot, axis_lock, axis_lock_axes, use_local_space: lod.required.use_local_space, mesh_override_material: matches!(&lod.type_data, TypeData::Mesh { override_material: true, .. }), lod_level: e.lod,
                particles: list.into_iter().map(|x| x.1).collect(), ribbon_vertices, ribbon_indices });
        }
    }
}




/// One dynamic particle light (`FSimpleLightEntry` of `GatherParticleLightData`, UE4.16 ParticleSystemRender.cpp:856), game space (metres, Y up).
#[derive(Clone, Copy, Debug)]
pub struct ParticleLight {
    pub pos: [f32; 3],
    /// Attenuation radius in metres.
    pub radius_m: f32,
    /// `Particle.Color.rgb * Particle.Color.A * ColorScale` (linear, unbounded).
    pub color: [f32; 3],
    /// 0 = inverse squared falloff, else the light falloff exponent.
    pub exponent: f32,
    pub affects_translucency: bool,
}

impl Instance {
    /// The lights of the particles that carry a valid `ParticleModuleLight` payload (simple-light path: `bHighQualityLights` ones are skipped like the original does there).
    pub fn lights(&self) -> Vec<ParticleLight> {
        let mut out = vec![];
        for e in &self.emitters {
            let Some(lod) = lod_of(&self.system.emitters[e.index], e.lod) else { continue };
            let Some((true, hq, translucency)) = lod.modules.iter().find_map(|m| if let ModuleKind::Light { high_quality, affects_translucency, .. } = &m.kind { Some((m.enabled, *high_quality, *affects_translucency)) } else { None }) else { continue };
            if hq { continue; }
            let xf = &self.xf;
            for p in &e.particles {
                let Some(l) = p.light.filter(|l| l.valid) else { continue };
                let size = p.size * xf.scale;
                // GetParticleSize: PSA_Square uses Size.X for both axes
                let sy = if matches!(lod.required.screen_alignment, Alignment::Square) { size.x } else { size.y };
                let radius = l.radius_scale * (size.x.abs() + sy.abs()) * 0.5;
                let color = [p.color[0] * p.color[3] * l.color_scale[0], p.color[1] * p.color[3] * l.color_scale[1], p.color[2] * p.color[3] * l.color_scale[2]];
                if radius <= 1e-4 && color.iter().cloned().fold(0.0, f32::max) <= 1e-4 { continue; }
                let mut world = if lod.required.use_local_space { xf.pos + xf.rot * (p.loc * xf.scale) } else { p.loc };
                if let Some(o) = &p.orbit { world += euler_matrix(o.rotation * TAU) * o.offset * xf.scale; }
                out.push(ParticleLight { pos: ue_to_game(world), radius_m: radius * 0.01, color, exponent: l.exponent, affects_translucency: translucency });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dist::{Dist, Table};

    fn sys(modules: Vec<ModuleKind>, spawn: Spawn, req: Required) -> Arc<ParticleSystem> {
        let lod = Lod { level: 0, enabled: true, required: req, spawn, type_data: TypeData::Sprite, modules: modules.into_iter().map(|kind| Module { kind, enabled: true, lod_validity: 15, seed: None, name: String::new() }).collect(), peak_active: 0 };
        Arc::new(ParticleSystem { name: "T".into(), emitters: vec![Emitter { name: "e".into(), lods: vec![lod] }], lod_distances: vec![0.0], ..Default::default() })
    }
    fn burst(n: i32) -> Spawn { Spawn { rate: Dist::constant(0.0), rate_scale: Dist::constant(1.0), burst_scale: Dist::constant(1.0), bursts: vec![Burst { count: n, count_low: -1, time: 0.0 }] } }
    fn rate(r: f32) -> Spawn { Spawn { rate: Dist::constant(r), rate_scale: Dist::constant(1.0), burst_scale: Dist::constant(1.0), bursts: vec![] } }
    fn uniform(lo: f32, hi: f32) -> Dist { Dist::Table(Table { op: 2, lock: 0, entry_count: 1, entry_stride: 2, sub_entry_stride: 1, time_scale: 0.0, time_bias: 0.0, values: vec![lo, hi] }) }
    fn req(duration: f32, loops: i32, local: bool) -> Required { Required { emitter_duration: duration, emitter_loops: loops, use_local_space: local, ..Default::default() } }

    #[test] fn a_burst_spawns_exactly_its_count_once_and_particles_live_their_lifetime() {
        let s = sys(vec![ModuleKind::Lifetime { lifetime: Dist::constant(0.5) }, ModuleKind::Size { start: Dist::constant3([10.0; 3]) }], burst(7), req(1.0, 1, false));
        let mut i = Instance::new(s, Transform::identity(), 1, 0.0);
        i.tick(1.0 / 60.0, &Env::none()); assert_eq!(i.live_count(), 7); assert_eq!(i.spawned_total(), 7);
        i.run(0.45, 1.0 / 60.0, &Env::none()); assert_eq!(i.live_count(), 7, "alive before 0.5 s");
        i.run(0.1, 1.0 / 60.0, &Env::none()); assert_eq!(i.live_count(), 0, "dead after 0.5 s");
        i.run(2.0, 1.0 / 60.0, &Env::none()); assert_eq!(i.spawned_total(), 7, "EmitterLoops 1: the burst never repeats"); assert!(i.finished());
    }
    #[test] fn continuous_rate_accumulates_the_spawn_fraction() {
        let s = sys(vec![ModuleKind::Lifetime { lifetime: Dist::constant(10.0) }], rate(30.0), req(1.0, 1, false));
        let mut i = Instance::new(s, Transform::identity(), 1, 0.0);
        i.run(0.99, 1.0 / 60.0, &Env::none()); let n = i.spawned_total();
        assert!((29..=30).contains(&n), "{n}");
        i.run(1.0, 1.0 / 60.0, &Env::none()); assert!(i.spawned_total() <= 31, "spawning stops when the single loop of the emitter ends: {}", i.spawned_total());
    }
    #[test] fn looping_emitters_repeat_their_bursts_and_loop_zero_never_stops() {
        let s = sys(vec![ModuleKind::Lifetime { lifetime: Dist::constant(0.1) }], burst(3), req(0.5, 0, false));
        let mut i = Instance::new(s, Transform::identity(), 1, 0.0);
        i.run(2.05, 1.0 / 60.0, &Env::none()); assert!((12..=15).contains(&i.spawned_total()), "{}", i.spawned_total()); assert!(!i.finished());
    }
    #[test] fn velocity_acceleration_and_world_vs_local_space() {
        let m = || vec![ModuleKind::Lifetime { lifetime: Dist::constant(5.0) }, ModuleKind::Velocity { start: Dist::constant3([100.0, 0.0, 0.0]), radial: Dist::constant(0.0), in_world_space: false }, ModuleKind::AccelerationConstant { accel: [0.0, 0.0, -900.0], world: true }];
        // yawed 90 degrees about Z: the emitter-space +X velocity points along world +Y; gravity stays world -Z in both space modes
        let xf = Transform { pos: Vec3::new(10.0, 0.0, 500.0), rot: Mat3::from_rotation_z(std::f32::consts::FRAC_PI_2), scale: 1.0 };
        for local in [false, true] {
            let mut i = Instance::new(sys(m(), burst(1), req(1.0, 1, local)), xf, 1, 0.0);
            i.run(1.0, 1.0 / 120.0, &Env::none());
            let mut out = vec![]; i.draw([0.0, 0.0, 0.0], &mut out); let p = &out[0].particles[0];
            let w = game_to_ue(p.pos);
            assert!((w.x - 10.0).abs() < 2.0 && (w.y - 100.0).abs() < 3.0, "local={local}: {w:?}");
            assert!((w.z - (500.0 - 450.0)).abs() < 8.0, "local={local} fell {:?} cm in 1 s (0.5 g t^2 = 450)", w.z);
        }
    }
    #[test] fn color_and_size_over_life() {
        let ramp = Dist::Table(Table { op: 1, lock: 0, entry_count: 2, entry_stride: 1, sub_entry_stride: 0, time_scale: 1.0, time_bias: 0.0, values: vec![1.0, 0.0] });
        let mult = Dist::Table(Table { op: 1, lock: 0, entry_count: 2, entry_stride: 3, sub_entry_stride: 0, time_scale: 1.0, time_bias: 0.0, values: vec![0.5, 0.5, 0.5, 1.0, 1.0, 1.0] });
        let s = sys(vec![ModuleKind::Lifetime { lifetime: Dist::constant(1.0) }, ModuleKind::Size { start: Dist::constant3([10.0; 3]) }, ModuleKind::ColorOverLife { color: Dist::constant3([1.0, 0.5, 0.25]), alpha: ramp, clamp_alpha: true }, ModuleKind::SizeMultiplyLife { mult, x: true, y: true, z: true }], burst(1), req(1.0, 1, false));
        let mut i = Instance::new(s, Transform::identity(), 1, 0.0);
        i.run(0.5, 1.0 / 100.0, &Env::none());
        let p = &i.emitters[0].particles[0];
        assert!((p.color[3] - 0.5).abs() < 0.03 && p.color[0] == 1.0 && p.color[1] == 0.5, "{:?}", p.color);
        assert!((p.size.x - 7.5).abs() < 0.4, "size {}", p.size.x);
    }
    #[test] fn the_ground_stops_a_falling_particle_and_the_run_is_reproducible() {
        let m = vec![ModuleKind::Lifetime { lifetime: Dist::constant(5.0) }, ModuleKind::Size { start: Dist::constant3([10.0; 3]) }, ModuleKind::Velocity { start: Dist::constant3([0.0, 0.0, 0.0]), radial: Dist::constant(0.0), in_world_space: true },
            ModuleKind::Location { start: uniform_vec(100.0, 300.0) }, ModuleKind::AccelerationConstant { accel: [0.0, 0.0, -900.0], world: true }, ModuleKind::CollisionGpu { friction: 1.0, resilience: Dist::constant(0.0), resilience_scale_over_life: Dist::constant(1.0), radius_scale: 1.0, stop: false }];
        let ground = |_: f32, _: f32| 0.0f32; let env = Env { ground: Some(&ground) };
        let run = || { let mut i = Instance::new(sys(m.clone(), burst(20), req(1.0, 1, false)), Transform::identity(), 42, 0.0); i.run(2.0, 1.0 / 60.0, &env); i.emitters[0].particles.iter().map(|p| (p.loc.z, p.stopped)).collect::<Vec<_>>() };
        let a = run(); assert_eq!(a.len(), 20); assert!(a.iter().all(|(z, stopped)| *stopped && (*z - 5.0).abs() < 0.1), "{a:?}");
        assert_eq!(a, run());
    }
    fn uniform_vec(lo: f32, hi: f32) -> Dist { Dist::Table(Table { op: 2, lock: 0, entry_count: 1, entry_stride: 6, sub_entry_stride: 3, time_scale: 0.0, time_bias: 0.0, values: vec![0.0, 0.0, lo, 0.0, 0.0, hi] }) }
    #[test] fn seeded_modules_ignore_the_instance_seed() {
        let mk = |start: Dist| { let mut s = (*sys(vec![ModuleKind::Lifetime { lifetime: Dist::constant(5.0) }, ModuleKind::Location { start }], burst(5), req(1.0, 1, false))).clone();
            s.emitters[0].lods[0].modules[1].seed = Some(15); Arc::new(s) };
        let place = |seed: u64| { let mut i = Instance::new(mk(uniform_vec(0.0, 100.0)), Transform::identity(), seed, 0.0); i.tick(0.0, &Env::none()); i.tick(1.0 / 60.0, &Env::none()); i.emitters[0].particles.iter().map(|p| p.loc.z.to_bits()).collect::<Vec<_>>() };
        assert_eq!(place(1), place(99), "RandomSeedInfo makes the same pieces fly every time"); assert_eq!(place(1).len(), 5);
    }
    #[test] fn emitter_delay_and_unsupported_modules_are_reported() {
        let mut r = req(1.0, 1, false); r.emitter_delay = 0.3;
        let mut i = Instance::new(sys(vec![ModuleKind::Lifetime { lifetime: Dist::constant(1.0) }], burst(2), r), Transform::identity(), 1, 0.0);
        i.run(0.25, 1.0 / 60.0, &Env::none()); assert_eq!(i.spawned_total(), 0); i.run(0.2, 1.0 / 60.0, &Env::none()); assert_eq!(i.spawned_total(), 2);
        let mut s = (*sys(vec![], burst(1), req(1.0, 1, false))).clone(); s.emitters[0].lods[0].modules.push(Module { kind: ModuleKind::Unsupported { class: "X".into(), note: String::new() }, enabled: true, lod_validity: 15, seed: None, name: String::new() });
        assert_eq!(s.unsupported(), vec![("X".to_string(), 1)]);
    }
    #[test] fn light_module_reports_color_radius_and_follows_the_brightness_curve() {
        // size 100 cm, colour (1,.5,0) alpha .25, brightness 40 -> 20 at the end of life, radius scale 1: radius 100 cm = 1 m, colour = rgb * .25 * brightness
        let bright = Dist::Table(Table { op: 1, lock: 0, entry_count: 2, entry_stride: 1, sub_entry_stride: 0, time_scale: 1.0, time_bias: 0.0, values: vec![40.0, 20.0] });
        let m = vec![ModuleKind::Lifetime { lifetime: Dist::constant(1.0) }, ModuleKind::Size { start: Dist::constant3([100.0; 3]) }, ModuleKind::Color { color: Dist::constant3([1.0, 0.5, 0.0]), alpha: Dist::constant(0.25) },
            ModuleKind::Light { color_scale: Dist::constant3([1.0; 3]), brightness: bright, radius_scale: Dist::constant(1.0), exponent: Dist::constant(4.0), inverse_squared: false, spawn_fraction: 1.0, affects_translucency: false, high_quality: false }];
        let mut i = Instance::new(sys(m, burst(1), req(1.0, 1, true)), Transform { pos: Vec3::new(0.0, 0.0, 100.0), rot: Mat3::IDENTITY, scale: 1.0 }, 1, 0.0);
        i.tick(1.0 / 60.0, &Env::none());
        let l = i.lights(); assert_eq!(l.len(), 1);
        assert!((l[0].radius_m - 1.0).abs() < 1e-3 && l[0].exponent == 4.0, "{:?}", l[0]);
        assert!((l[0].color[0] - 10.0).abs() < 0.2 && (l[0].color[1] - 5.0).abs() < 0.1 && l[0].color[2] == 0.0, "{:?}", l[0].color);
        assert!((l[0].pos[1] - 1.0).abs() < 1e-3, "game Y is UE Z: {:?}", l[0].pos);
        i.run(0.5, 1.0 / 60.0, &Env::none());
        let l = i.lights(); assert!((l[0].color[0] - 7.5).abs() < 0.3, "brightness halfway: {:?}", l[0].color);
        i.run(0.6, 1.0 / 60.0, &Env::none()); assert!(i.lights().is_empty(), "dead particle, no light");
    }
    #[test] fn reactivate_restarts_the_emitter_but_keeps_live_particles_and_deactivate_stops_spawning() {
        let mut i = Instance::new(sys(vec![ModuleKind::Lifetime { lifetime: Dist::constant(5.0) }], burst(3), req(0.1, 1, false)), Transform::identity(), 1, 0.0);
        i.run(0.3, 1.0 / 60.0, &Env::none()); assert_eq!(i.spawned_total(), 3); assert!(i.emitters[0].completed);
        i.reactivate(); i.run(0.3, 1.0 / 60.0, &Env::none()); assert_eq!(i.spawned_total(), 6, "the burst fires again on Activate(true)"); assert_eq!(i.live_count(), 6, "earlier particles stay");
        i.reactivate(); i.deactivate(); i.run(0.3, 1.0 / 60.0, &Env::none()); assert_eq!(i.spawned_total(), 6, "Deactivate: no new particles"); assert_eq!(i.live_count(), 6);
    }
    #[test] fn a_ribbon_spawns_per_unit_distance_and_builds_one_strip_from_the_newest_point() {
        let mods = vec![ModuleKind::Lifetime { lifetime: Dist::constant(2.0) }, ModuleKind::Size { start: Dist::constant3([0.5, 0.0, 0.0]) },
            ModuleKind::SpawnPerUnit { unit_scalar: 100.0, spawn_per_unit: Dist::constant(1.0), max_frame_distance: 0.0, ignore_spawn_rate_when_moving: false, movement_tolerance: 30.0, ignore_movement: [false; 3] }];
        let mut s = (*sys(mods, rate(0.0), req(1.0, 0, false))).clone();
        s.emitters[0].lods[0].type_data = TypeData::Ribbon(RibbonInfo { tiling_distance: 1000.0, ..Default::default() });
        let mut i = Instance::new(Arc::new(s), Transform::identity(), 3, 0.0);
        for k in 0..10 { i.xf.pos = Vec3::new(k as f32 * 300.0, 0.0, 0.0); i.tick(1.0 / 60.0, &Env::none()); }
        let n = i.emitters[0].particles.len();
        assert!((26..=27).contains(&n), "one point per 100 cm over 2700 cm: {n}");
        assert!(i.emitters[0].particles.iter().all(|p| (p.size.x - 0.5).abs() < 1e-6 && p.inv_life == 0.5));
        let ps = &i.emitters[0].particles;
        assert!(ps.windows(2).all(|w| w[1].loc.x > w[0].loc.x), "chronological order along the path");
        let mut d = vec![];
        i.draw([0.0, 10.0, 0.0], &mut d);
        let r = d.iter().find(|e| e.kind == DrawKind::Ribbon).expect("ribbon draw");
        let verts = r.ribbon_vertices.len() / RIBBON_VERTEX_FLOATS;
        assert!(verts > 2 * n, "distance tessellation adds points between particles: {verts}");
        assert_eq!(r.ribbon_indices.len(), (verts - 2) * 3);
        let first = &r.ribbon_vertices[0..RIBBON_VERTEX_FLOATS];
        assert!((first[0] - ps[n - 1].loc.x).abs() < 1e-3 && first[12] == 0.0 && (first[8] - 0.5).abs() < 1e-6, "the strip starts at the newest point with U = 0");
        let last = &r.ribbon_vertices[(verts - 1) * RIBBON_VERTEX_FLOATS..];
        assert!(last[12] > 0.95 && last[12] <= 1.0 + 1e-4, "U runs 0..1 along the trail: {}", last[12]);
    }
    #[test] fn velocity_inherit_parent_adds_the_component_velocity() {
        let m = vec![ModuleKind::Lifetime { lifetime: Dist::constant(5.0) }, ModuleKind::VelocityInheritParent { scale: Dist::constant3([0.5; 3]) }];
        let mut i = Instance::new(sys(m, burst(1), req(1.0, 1, false)), Transform::identity(), 1, 0.0); i.part_sys_velocity = Vec3::new(200.0, 0.0, 0.0);
        i.tick(1.0 / 60.0, &Env::none()); assert_eq!(i.emitters[0].particles[0].vel, Vec3::new(100.0, 0.0, 0.0));
    }
    #[test] fn facing_transform_points_the_emitter_x_axis_along_the_normal() {
        let xf = Transform::from_game_facing([1.0, 2.0, 3.0], [0.0, 0.0, -1.0]);
        let x = xf.rot * Vec3::X; let g = ue_to_game(x * 100.0);
        assert!(g[2] < -0.99 && g[0].abs() < 1e-4, "{g:?}");
        assert_eq!(ue_to_game(game_to_ue([1.0, 2.0, 3.0])), [1.0, 2.0, 3.0]);
    }
    /// BVISFIX: UParticleModuleLocationPrimitiveTriangle (ParticleModules_Location.cpp:851): every spawn lies inside the triangle (height 340, angle 90 -> base
    /// half-width 340, apex at +170 X) about the offset, Z within the thickness, and the emitter rotation applies (TransformVector).
    #[test] fn triangle_location_spawns_inside_the_triangle() {
        let m = vec![ModuleKind::Lifetime { lifetime: Dist::constant(5.0) }, ModuleKind::LocationTriangle { start_offset: Dist::constant3([0.0, 0.0, 50.0]), height: Dist::constant(340.0), angle: Dist::constant(90.0), thickness: Dist::constant(10.0) }];
        let xf = Transform { pos: Vec3::ZERO, rot: Mat3::from_rotation_z(std::f32::consts::FRAC_PI_2), scale: 1.0 };
        let mut i = Instance::new(sys(m, burst(200), req(1.0, 1, false)), xf, 7, 0.0);
        i.tick(1.0 / 60.0, &Env::none());
        let mut out = vec![]; i.draw([0.0, 0.0, 0.0], &mut out);
        assert_eq!(out[0].particles.len(), 200);
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        for p in &out[0].particles {
            let w = game_to_ue(p.pos);
            // undo the 90 degree yaw: local x = world y, local y = -world x
            let (lx, ly) = (w.y, -w.x);
            assert!((-170.01..=170.01).contains(&lx) && ly.abs() <= (170.0 - lx) + 0.01, "outside the triangle: {lx} {ly}");
            // SpawnEx overwrites the offset's Z with the thickness term (`LocationOffset.Z = ZPos * TriThickness - 0.5f * TriThickness`)
            assert!(w.z.abs() <= 5.001, "thickness: {}", w.z);
            min_x = min_x.min(lx); max_x = max_x.max(lx);
        }
        assert!(min_x < -100.0 && max_x > 0.0, "spread {min_x}..{max_x}");
    }
}
