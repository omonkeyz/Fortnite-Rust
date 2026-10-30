//! Pure-Rust representation of a decoded `UParticleSystem` (Cascade, UE 4.16). Everything here is data read from the package; units are the original ones
//! (centimetres, seconds, turns for rotations, UE axes: X forward, Y right, Z up).
use crate::dist::Dist;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ParticleSystem {
    /// Short object name (`P_Pine_Tree_Med_Destruction`).
    pub name: String,
    /// Long package path (`/Game/Effects/.../P_Pine_Tree_Med_Destruction`).
    pub package: String,
    pub emitters: Vec<Emitter>,
    /// `LODDistances` (cm); `ActivateAutomatic` picks the last level whose distance is <= the camera distance.
    pub lod_distances: Vec<f32>,
    pub lod_method: String,
    /// `FixedRelativeBoundingBox` (min, max) in cm when `bUseFixedRelativeBoundingBox`.
    pub fixed_bounds: Option<([f32; 3], [f32; 3])>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Emitter {
    pub name: String,
    /// `LODLevels`, index = LOD level.
    pub lods: Vec<Lod>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortMode { None, ViewProjDepth, DistanceToView, Age, Unsupported }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Alignment { Square, Rectangle, Velocity, TypeSpecific }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubUvInterp { None, Linear, LinearBlend, Random, RandomBlend }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Required {
    /// Material object path (`/Game/.../M_X.M_X`); for mesh emitters the mesh's own materials are used unless a MeshMaterial module overrides them.
    pub material: String,
    pub use_local_space: bool,
    pub emitter_duration: f32,
    pub emitter_delay: f32,
    /// 0 = loop forever.
    pub emitter_loops: i32,
    pub sort_mode: SortMode,
    pub screen_alignment: Alignment,
    pub sub_images_h: u32,
    pub sub_images_v: u32,
    pub interpolation: SubUvInterp,
    pub random_image_time: f32,
    /// `OpacitySourceMode` (`OSM_RedChannel`...) of the cutout texture, kept as text.
    pub opacity_source: String,
    pub alpha_threshold: f32,
    pub cutout_texture: Option<String>,
    pub legacy_emitter_time: bool,
    /// `ParticleModuleRequired.bEnabled`: informational only. The runtime never reads it (UE4.16: only the Cascade editor writes it, in sync with `ParticleLODLevel.bEnabled`) and the LOD levels of an emitter
    /// share one Required object, so toggling a far LOD off in the editor leaves it false for LOD 0 too (P_Pickup's Sparkle). `Lod.enabled` is `ParticleLODLevel.bEnabled` alone.
    pub required_enabled: bool,
}
impl Default for Required {
    fn default() -> Required {
        Required { material: String::new(), use_local_space: false, emitter_duration: 1.0, emitter_delay: 0.0, emitter_loops: 1, sort_mode: SortMode::None, screen_alignment: Alignment::Square, sub_images_h: 1, sub_images_v: 1,
            interpolation: SubUvInterp::None, random_image_time: 0.0, opacity_source: String::new(), alpha_threshold: 0.0, cutout_texture: None, legacy_emitter_time: true, required_enabled: true }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Burst { pub count: i32, pub count_low: i32, pub time: f32 }

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Spawn { pub rate: Dist, pub rate_scale: Dist, pub burst_scale: Dist, pub bursts: Vec<Burst> }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis { X, Y, Z }

/// `EParticleAxisLock` (`ParticleModuleOrientationAxisLock.LockAxisFlags`): `X..NegZ` lock the sprite facing towards the axis, `RotateX..RotateZ` lock the rotation about the axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AxisLock { #[default] None, X, Y, Z, NegX, NegY, NegZ, RotateX, RotateY, RotateZ }

impl AxisLock {
    /// Cooked enum text (`EPAL_NEGATIVE_X`...).
    pub fn from_ue(s: &str) -> AxisLock {
        match s.rsplit("::").next().unwrap_or(s) { "EPAL_X" => AxisLock::X, "EPAL_Y" => AxisLock::Y, "EPAL_Z" => AxisLock::Z, "EPAL_NEGATIVE_X" => AxisLock::NegX, "EPAL_NEGATIVE_Y" => AxisLock::NegY, "EPAL_NEGATIVE_Z" => AxisLock::NegZ,
            "EPAL_ROTATE_X" => AxisLock::RotateX, "EPAL_ROTATE_Y" => AxisLock::RotateY, "EPAL_ROTATE_Z" => AxisLock::RotateZ, _ => AxisLock::None }
    }
    /// `ComputeLockedAxes` (UE4.16 ParticleSystemRender.cpp:294): the sprite's (up, right) as combinations of the emitter's local axes, each entry (axis 0 = X, 1 = Y, 2 = Z; sign). `up = None` for the
    /// rotation locks (`CameraUp = 0`, the sprite spins about `right`). `None` for `AxisLock::None`.
    pub fn locked_axes(self) -> Option<(Option<(usize, f32)>, (usize, f32))> {
        Some(match self {
            AxisLock::None => return None,
            AxisLock::X => (Some((2, -1.0)), (1, -1.0)), AxisLock::Y => (Some((2, -1.0)), (0, 1.0)), AxisLock::Z => (Some((0, -1.0)), (1, 1.0)),
            AxisLock::NegX => (Some((2, -1.0)), (1, 1.0)), AxisLock::NegY => (Some((2, -1.0)), (0, -1.0)), AxisLock::NegZ => (Some((0, -1.0)), (1, -1.0)),
            AxisLock::RotateX => (None, (0, 1.0)), AxisLock::RotateY => (None, (1, 1.0)), AxisLock::RotateZ => (None, (2, 1.0)),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DynParam { pub name: String, pub value: Dist, pub use_emitter_time: bool, pub spawn_time_only: bool, pub scale_velocity: bool }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GpuInfo {
    pub max_particle_count: u32,
    pub collision: bool,
    /// `EParticleCollisionMode` text (DistanceField / SceneDepth).
    pub collision_mode: String,
    pub constant_acceleration: [f32; 3],
    pub max_lifetime: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TypeData {
    Sprite,
    Mesh { mesh: String, roll_pitch_yaw: Dist, override_material: bool },
    /// `ParticleModuleTypeDataGpu`: the same modules, simulated on the GPU in the original (sprites only).
    Gpu(GpuInfo),
    Ribbon(RibbonInfo),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RibbonInfo {
    pub max_tessellation_between_particles: i32,
    pub max_particle_in_trail_count: i32,
    pub tiling_distance: f32,
    pub distance_tessellation_step_size: f32,
    pub enable_tangent_diff_interp_scale: bool,
    pub tangent_tessellation_scalar: f32,
    pub tangent_spawning_scalar: f32,
    pub spawn_initial_particle: bool,
    pub render_axis: u8,
    pub enable_previous_tangent_recalculation: bool,
    pub tangent_recalculation_every_frame: bool,
    pub dead_trails_on_deactivate: bool,
}

impl Default for RibbonInfo {
    fn default() -> Self {
        RibbonInfo { max_tessellation_between_particles: 25, max_particle_in_trail_count: 500, tiling_distance: 0.0, distance_tessellation_step_size: 15.0, enable_tangent_diff_interp_scale: false,
            tangent_tessellation_scalar: 5.0, tangent_spawning_scalar: 0.0, spawn_initial_particle: false, render_axis: 0, enable_previous_tangent_recalculation: true, tangent_recalculation_every_frame: false, dead_trails_on_deactivate: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ModuleKind {
    Lifetime { lifetime: Dist },
    Location { start: Dist },
    LocationSphere { start: Dist, radius: Dist, surface_only: bool, velocity: bool, velocity_scale: Dist, positive_z: bool, negative_z: bool },
    LocationCylinder { start: Dist, radius: Dist, height: Dist, height_axis: Axis, surface_only: bool, velocity: bool, velocity_scale: Dist, positive_z: bool, negative_z: bool },
    /// BVISFIX (2026-10-05): `UParticleModuleLocationPrimitiveTriangle` (UE4.16 ParticleModuleLocationPrimitiveTriangle.h; `SpawnEx`, ParticleModules_Location.cpp:851).
    LocationTriangle { start_offset: Dist, height: Dist, angle: Dist, thickness: Dist },
    Velocity { start: Dist, radial: Dist, in_world_space: bool },
    Size { start: Dist },
    SizeMultiplyLife { mult: Dist, x: bool, y: bool, z: bool },
    SizeScaleBySpeed { speed_scale: [f32; 2], max_scale: [f32; 2] },
    Color { color: Dist, alpha: Dist },
    ColorOverLife { color: Dist, alpha: Dist, clamp_alpha: bool },
    ColorScaleOverLife { color: Dist, alpha: Dist },
    Rotation { start: Dist },
    RotationRate { start: Dist },
    MeshRotation { start: Dist },
    MeshRotationRate { start: Dist },
    Acceleration { accel: Dist, world: bool },
    AccelerationConstant { accel: [f32; 3], world: bool },
    Drag { coefficient: Dist },
    CameraOffset { offset: Dist, spawn_time_only: bool },
    Orbit { offset: Dist, rotation: Dist, rotation_rate: Dist },
    ParameterDynamic { params: Vec<DynParam> },
    PivotOffset { offset: [f32; 2] },
    SubUv { index: Dist },
    SubUvMovie { index: Dist, frame_rate: Dist, starting_frame: i32 },
    Collision { damping: Dist, damping_rotation: Dist, max_collisions: Dist, delay: Dist, particle_mass: Dist, halt_when_done: bool },
    CollisionGpu { friction: f32, resilience: Dist, resilience_scale_over_life: Dist, radius_scale: f32, stop: bool },
    MeshMaterial { materials: Vec<String> },
    /// `ParticleModuleLight` (the dynamic light of a particle, muzzle flashes use it): `ColorScaleOverLife * BrightnessOverLife` at spawn / every update, `RadiusScale` and `LightExponent`
    /// sampled at spawn (emitter time), `bValid = rand < SpawnFraction`. The light itself is reported by `Instance::lights`.
    Light { color_scale: Dist, brightness: Dist, radius_scale: Dist, exponent: Dist, inverse_squared: bool, spawn_fraction: f32, affects_translucency: bool, high_quality: bool },
    /// `ParticleModuleVelocityInheritParent`: spawn velocity += component velocity * `Scale(EmitterTime)`.
    VelocityInheritParent { scale: Dist },
    /// `ParticleModuleMeshRotationRateMultiplyLife`: the mesh rotation rate is multiplied by `LifeMultiplier(RelativeTime)` at spawn and on every update (the original multiplies cumulatively).
    MeshRotationRateMultiplyLife { mult: Dist },
    /// `ParticleModuleSizeScale` (UE4.16 ParticleModules_Size.cpp:384): at spawn and on every update `Size = BaseSize * SizeScale(RelativeTime)` (an assignment, it replaces the size
    /// adjustments of the modules before it in the list; `EnableX/Y/Z` are never read by the original).
    SizeScale { scale: Dist },
    /// `ParticleModuleSpawnPerUnit` (spawn module, UE4.16 ParticleModules_Spawn.cpp:304): particles per distance the component moved, see `sim`.
    SpawnPerUnit { unit_scalar: f32, spawn_per_unit: Dist, max_frame_distance: f32, ignore_spawn_rate_when_moving: bool, movement_tolerance: f32, ignore_movement: [bool; 3] },
    /// `ParticleModuleOrientationAxisLock`: `LockAxisFlags`, exposed on [`crate::EmitterDraw`] (the renderer orients the sprites).
    OrientationAxisLock { axis: AxisLock },
    /// A module class this crate does not simulate (listed so the gap is visible); `class` is the cooked class name.
    Unsupported { class: String, note: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Module {
    pub kind: ModuleKind,
    pub enabled: bool,
    /// `LODValidity` bit mask (bit n = valid in LOD n); 15 = all four.
    pub lod_validity: u32,
    /// `RandomSeedInfo.RandomSeeds[0]` of the `_Seeded` variants.
    pub seed: Option<i32>,
    /// Export name in the package (diagnostics).
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lod {
    pub level: i32,
    pub enabled: bool,
    pub required: Required,
    pub spawn: Spawn,
    pub type_data: TypeData,
    /// Modules in the order of `ParticleLODLevel.Modules` (spawn and update order of the original).
    pub modules: Vec<Module>,
    pub peak_active: i32,
}

/// A texture of an exported material (PNG relative to the particle asset root).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextureRef { pub name: String, pub ue_path: String, pub png: String, pub srgb: bool, pub compression: String, pub width: u32, pub height: u32, pub has_alpha: bool }

/// A material as far as the cooked package keeps it: the root `UMaterial` settings (blend mode, shading model, two sided, clip value), the merged parameter values of the
/// instance chain, and the input -> parameter wiring where the graph still has it (the arithmetic nodes between parameters are stripped from the cooked packages).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MaterialDef {
    pub name: String,
    pub ue_path: String,
    pub root: String,
    pub blend_mode: String,
    pub shading_model: String,
    pub two_sided: bool,
    /// `UMaterial::bEnableSeparateTranslucency` of the root material (default true, UE4.16 Material.cpp:739; not a `BasePropertyOverrides` member): which
    /// translucency pass the particles of this material join. Catalogs exported before the field existed read as the engine default.
    #[serde(default = "separate_translucency_default")]
    pub separate_translucency: bool,
    pub clip: f32,
    pub textures: std::collections::BTreeMap<String, TextureRef>,
    pub scalars: std::collections::BTreeMap<String, f32>,
    pub vectors: std::collections::BTreeMap<String, [f32; 4]>,
    /// Material input (`BaseColor`, `EmissiveColor`, `Opacity`, `OpacityMask`) -> texture parameter, only when wired straight to a texture sample.
    pub direct: std::collections::BTreeMap<String, String>,
    /// Material inputs that have something connected.
    pub connected: Vec<String>,
    /// Parameter chosen as the colour texture of the particle (name heuristic when the graph is not recoverable) and how it was chosen.
    pub color_texture: Option<String>,
    pub color_source: String,
    pub notes: Vec<String>,
}

fn separate_translucency_default() -> bool { true }

impl MaterialDef {
    pub fn is_translucent(&self) -> bool { self.blend_mode.contains("Translucent") }
    pub fn is_additive(&self) -> bool { self.blend_mode.contains("Additive") }
    pub fn is_masked(&self) -> bool { self.blend_mode.contains("Masked") }
    pub fn is_modulate(&self) -> bool { self.blend_mode.contains("Modulate") }
    pub fn is_unlit(&self) -> bool { self.shading_model.contains("Unlit") }
}

/// A static mesh of a mesh emitter in game space (metres, Y up) as written by the exporter.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MeshDef {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// (first index, index count, material slot)
    pub sections: Vec<(u32, u32, u32)>,
    /// Material object path per slot (the mesh's own materials).
    pub materials: Vec<String>,
}

impl ParticleSystem {
    /// LOD level for a camera distance (cm), `ActivateAutomatic`.
    pub fn lod_for_distance(&self, cm: f32) -> usize {
        let mut lod = 0;
        for (i, d) in self.lod_distances.iter().enumerate() { if cm >= *d { lod = i; } }
        lod
    }
    /// Human readable summary of LOD 0 of every emitter (tools / logs).
    pub fn describe(&self) -> String {
        let mut s = format!("{} ({} emitters, LOD distances {:?})\n", self.name, self.emitters.len(), self.lod_distances);
        for e in &self.emitters {
            let Some(l) = e.lods.first() else { s.push_str(&format!("  {}: no LOD\n", e.name)); continue };
            let td = match &l.type_data { TypeData::Sprite => "sprite".to_string(), TypeData::Mesh { mesh, .. } => format!("mesh {}", mesh.rsplit('/').next().unwrap_or(mesh)), TypeData::Gpu(g) => format!("gpu-sprite max {} collision {}", g.max_particle_count, g.collision), TypeData::Ribbon(r) => format!("ribbon tiling {} max {}", r.tiling_distance, r.max_particle_in_trail_count) };
            let bursts: Vec<String> = l.spawn.bursts.iter().map(|b| format!("{}(low {})@{}", b.count, b.count_low, b.time)).collect();
            s.push_str(&format!("  {} [{}] mat {} bursts [{}] rate {:?} loops {} dur {} local {} align {:?} sort {:?} sub {}x{} lods {} enabled {} modules {} flags [{}]\n", e.name, td, l.required.material.rsplit('/').next().unwrap_or(""), bursts.join(","), l.spawn.rate.mean(0.0, 1)[0],
                l.required.emitter_loops, l.required.emitter_duration, l.required.use_local_space, l.required.screen_alignment, l.required.sort_mode, l.required.sub_images_h, l.required.sub_images_v, e.lods.len(), l.enabled, l.modules.len(), e.lods.iter().map(|l| format!("{}{}", if l.enabled { "L" } else { "l" }, if l.required.required_enabled { "R" } else { "r" })).collect::<Vec<_>>().join(" ")));
        }
        s
    }
    /// Unsupported module classes used by LOD 0 of any emitter (class, count).
    pub fn unsupported(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = vec![];
        for e in &self.emitters { if let Some(l) = e.lods.first() { for m in &l.modules { if let ModuleKind::Unsupported { class, .. } = &m.kind {
            match v.iter_mut().find(|x| &x.0 == class) { Some(x) => x.1 += 1, None => v.push((class.clone(), 1)) } } } } }
        v
    }
}

impl ModuleKind {
    /// Every distribution of the module (spawn / update / per-frame), for the parameter machinery.
    pub fn dists_mut(&mut self, f: &mut dyn FnMut(&mut Dist)) {
        use ModuleKind::*;
        match self {
            Lifetime { lifetime } => f(lifetime),
            Location { start } | Size { start } | Rotation { start } | RotationRate { start } | MeshRotation { start } | MeshRotationRate { start } => f(start),
            LocationSphere { start, radius, velocity_scale, .. } => { f(start); f(radius); f(velocity_scale); }
            LocationCylinder { start, radius, height, velocity_scale, .. } => { f(start); f(radius); f(height); f(velocity_scale); }
            LocationTriangle { start_offset, height, angle, thickness } => { f(start_offset); f(height); f(angle); f(thickness); }
            Velocity { start, radial, .. } => { f(start); f(radial); }
            SizeMultiplyLife { mult, .. } | MeshRotationRateMultiplyLife { mult } => f(mult),
            SizeScale { scale } | VelocityInheritParent { scale } => f(scale),
            SizeScaleBySpeed { .. } | AccelerationConstant { .. } | PivotOffset { .. } | MeshMaterial { .. } | OrientationAxisLock { .. } | Unsupported { .. } => {}
            Color { color, alpha } | ColorOverLife { color, alpha, .. } | ColorScaleOverLife { color, alpha } => { f(color); f(alpha); }
            Acceleration { accel, .. } => f(accel),
            Drag { coefficient } => f(coefficient),
            CameraOffset { offset, .. } => f(offset),
            Orbit { offset, rotation, rotation_rate } => { f(offset); f(rotation); f(rotation_rate); }
            ParameterDynamic { params } => { for p in params { f(&mut p.value); } }
            SubUv { index } => f(index),
            SubUvMovie { index, frame_rate, .. } => { f(index); f(frame_rate); }
            Collision { damping, damping_rotation, max_collisions, delay, particle_mass, .. } => { f(damping); f(damping_rotation); f(max_collisions); f(delay); f(particle_mass); }
            CollisionGpu { resilience, resilience_scale_over_life, .. } => { f(resilience); f(resilience_scale_over_life); }
            Light { color_scale, brightness, radius_scale, exponent, .. } => { f(color_scale); f(brightness); f(radius_scale); f(exponent); }
            SpawnPerUnit { spawn_per_unit, .. } => f(spawn_per_unit),
        }
    }
}

impl Lod {
    /// Every distribution of the LOD level (spawn module, type data, modules).
    pub fn dists_mut(&mut self, f: &mut dyn FnMut(&mut Dist)) {
        f(&mut self.spawn.rate); f(&mut self.spawn.rate_scale); f(&mut self.spawn.burst_scale);
        if let TypeData::Mesh { roll_pitch_yaw, .. } = &mut self.type_data { f(roll_pitch_yaw); }
        for m in &mut self.modules { m.kind.dists_mut(f); }
    }
}

impl ParticleSystem {
    /// Every distribution of every LOD of every emitter.
    pub fn dists_mut(&mut self, f: &mut dyn FnMut(&mut Dist)) { for e in &mut self.emitters { for l in &mut e.lods { l.dists_mut(f); } } }

    /// Names (and whether the vector class reads them) of the instance parameters the system reads, sorted, without duplicates (diagnostics: what `SetFloatParameter` / `SetVectorParameter` can drive).
    pub fn param_names(&self) -> Vec<(String, bool)> {
        let mut s = self.clone(); let mut v: Vec<(String, bool)> = vec![];
        s.dists_mut(&mut |d| if let Dist::Param(p) = d { if !v.iter().any(|(n, vec)| n.eq_ignore_ascii_case(&p.name) && *vec == p.vector) { v.push((p.name.clone(), p.vector)); } });
        v.sort(); v
    }

    /// Writes the instance parameters into the parameter distributions in place (`ParamDist::set`; a parameter that is not in `params` goes back to "not set"). Evaluation of the
    /// system afterwards (spawn modules for new particles, the per-frame over-life modules for live ones) uses the new values.
    pub fn apply_params(&mut self, params: &crate::Params) {
        self.dists_mut(&mut |d| if let Dist::Param(p) = d { p.set = p.lookup(params); });
    }

    /// A copy with every parameter distribution whose parameter is set replaced by the constant `GetValue` returns for it (parameters that are not set stay `Dist::Param`, evaluating
    /// to their `Constant` fallback).
    pub fn bake(&self, params: &crate::Params) -> ParticleSystem {
        let mut s = self.clone();
        s.dists_mut(&mut |d| { let nv = if let Dist::Param(p) = &*d { p.lookup(params).map(|v| Dist::Const(p.map(v))) } else { None }; if let Some(nv) = nv { *d = nv; } });
        s
    }
}
