//! `UParticleSystem` package -> [`ParticleSystem`]. Reads every export of the package with fn-uasset (as JSON, object references as full paths, which are unique inside a
//! package, unlike the short export names), then follows `Emitters` -> `LODLevels` -> `RequiredModule` / `SpawnModule` / `TypeDataModule` / `Modules`.
//! Tagged properties equal to the class default are not serialised; the defaults applied here are the ones of the UE 4.16 constructors and are listed next to each use.
use crate::dist::{CurvePoint, Dist, Interp, ParamDist, ParamMode, Table};
use crate::model::*;
use fn_uasset::{Package, PackageData, PackageIndex};
use serde_json::Value;
use std::collections::HashMap;

struct Pkg { exports: Vec<(String, String, Value)>, by_path: HashMap<String, usize> }

fn as_f32(v: &Value) -> Option<f32> { v.as_f64().map(|x| x as f32) }
fn pf(p: &Value, k: &str, d: f32) -> f32 { p.get(k).and_then(as_f32).unwrap_or(d) }
fn pb(p: &Value, k: &str, d: bool) -> bool { p.get(k).and_then(|v| v.as_bool()).unwrap_or(d) }
fn pi(p: &Value, k: &str, d: i64) -> i64 { p.get(k).and_then(|v| v.as_i64()).unwrap_or(d) }
fn ps<'a>(p: &'a Value, k: &str) -> Option<&'a str> { p.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) }
fn v3(v: &Value) -> Option<[f32; 3]> { let a = v.as_array()?; Some([as_f32(a.first()?)?, as_f32(a.get(1)?)?, as_f32(a.get(2)?)?]) }
fn pv3(p: &Value, k: &str, d: [f32; 3]) -> [f32; 3] { p.get(k).and_then(v3).unwrap_or(d) }

impl Pkg {
    fn load(pkg: &Package) -> Pkg {
        let mut exports = Vec::new(); let mut by_path = HashMap::new();
        for i in 0..pkg.exports.len() {
            let class = pkg.export_class_name(i);
            let props = match pkg.read_export(i) { Ok(o) => o.props.to_json(pkg), Err(_) => Value::Null };
            by_path.insert(pkg.object_path(PackageIndex(i as i32 + 1)), i);
            exports.push((class, pkg.exports[i].name.clone(), props));
        }
        Pkg { exports, by_path }
    }
    fn get(&self, path: &str) -> Option<&(String, String, Value)> { self.by_path.get(path).map(|&i| &self.exports[i]) }
    fn first_of(&self, class: &str) -> Option<&(String, String, Value)> { self.exports.iter().find(|e| e.0 == class) }

    /// `FRawDistribution*` struct -> [`Dist`].
    fn dist(&self, v: Option<&Value>) -> Dist {
        let Some(v) = v else { return Dist::default() };
        if let Some(path) = ps(v, "Distribution") {
            if let Some((class, _, p)) = self.get(path) {
                return match class.as_str() {
                    "DistributionFloatConstant" => Dist::constant(pf(p, "Constant", 0.0)),
                    "DistributionVectorConstant" => Dist::Const(pv3(p, "Constant", [0.0; 3])),
                    "DistributionFloatConstantCurve" | "DistributionVectorConstantCurve" => Dist::Curve(self.curve(p)),
                    // instance parameter (`SetFloatParameter` / `SetVectorParameter` / `SetColorParameter` of the component); evaluates to the `Constant` through the input/output map while
                    // the instance has not set it (UE4.16 Distributions.cpp:4244 / 4279), see `Instance::set_params`
                    "DistributionFloatParticleParameter" => param_dist(p, false),
                    "DistributionVectorParticleParameter" => param_dist(p, true),
                    other => { eprintln!("warning: distribution class {other} not supported ({path})"); Dist::default() }
                };
            }
        }
        if let Some(t) = v.get("Table") {
            let values: Vec<f32> = t.get("Values").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(as_f32).collect()).unwrap_or_default();
            return Dist::Table(Table {
                op: pi(t, "Op", 0) as u8, lock: pi(t, "LockFlag", 0) as u8, entry_count: pi(t, "EntryCount", if values.is_empty() { 0 } else { 1 }) as u32, entry_stride: pi(t, "EntryStride", 1) as u32,
                sub_entry_stride: pi(t, "SubEntryStride", 0) as u32, time_scale: pf(t, "TimeScale", 0.0), time_bias: pf(t, "TimeBias", 0.0), values,
            });
        }
        Dist::default()
    }
    fn curve(&self, p: &Value) -> Vec<CurvePoint> {
        let comp = |v: &Value| -> [f32; 3] { if let Some(x) = as_f32(v) { [x; 3] } else { v3(v).unwrap_or([0.0; 3]) } };
        p.get("ConstantCurve").and_then(|c| c.get("Points")).and_then(|a| a.as_array()).map(|a| a.iter().map(|pt| {
            let mode = match pt.get("InterpMode").and_then(|m| m.as_str()).unwrap_or("CIM_Linear") { "CIM_Linear" => Interp::Linear, "CIM_Constant" => Interp::Constant, _ => Interp::Curve };
            CurvePoint { t: pf(pt, "InVal", 0.0), v: pt.get("OutVal").map(comp).unwrap_or([0.0; 3]), arrive: pt.get("ArriveTangent").map(comp).unwrap_or([0.0; 3]), leave: pt.get("LeaveTangent").map(comp).unwrap_or([0.0; 3]), mode }
        }).collect()).unwrap_or_default()
    }
    fn d(&self, p: &Value, k: &str) -> Dist { self.dist(p.get(k)) }
    /// Vector distribution whose default (property absent) is the constant `d`.
    fn d_or3(&self, p: &Value, k: &str, d: [f32; 3]) -> Dist { match p.get(k) { Some(v) => self.dist(Some(v)), None => Dist::Const(d) } }
    /// A distribution whose default (property absent) is the constant `d` instead of zero.
    fn d_or(&self, p: &Value, k: &str, d: f32) -> Dist { match p.get(k) { Some(v) => self.dist(Some(v)), None => Dist::constant(d) } }
}

/// `UDistribution{Float,Vector}ParameterBase` object -> [`Dist::Param`] (UE4.16 constructors: `MaxInput` / `MaxOutput` 1, everything else 0, `DPM_Normal`).
fn param_dist(p: &Value, vector: bool) -> Dist {
    let comp = |k: &str, d: f32| -> [f32; 3] { match p.get(k) { Some(v) => v3(v).or_else(|| as_f32(v).map(|x| [x; 3])).unwrap_or([d; 3]), None => [d; 3] } };
    let mode = |k: &str| match ps(p, k).unwrap_or("DPM_Normal") { "DPM_Abs" => ParamMode::Abs, "DPM_Direct" => ParamMode::Direct, _ => ParamMode::Normal };
    let modes = if vector { [mode("ParamModes"), mode("ParamModes[1]"), mode("ParamModes[2]")] } else { let m = mode("ParamMode"); [m, m, m] };
    Dist::Param(ParamDist { name: ps(p, "ParameterName").unwrap_or("").to_string(), vector, constant: comp("Constant", 0.0), min_input: comp("MinInput", 0.0), max_input: comp("MaxInput", 1.0),
        min_output: comp("MinOutput", 0.0), max_output: comp("MaxOutput", 1.0), modes, set: None })
}

fn sort_mode(s: Option<&str>) -> SortMode {
    match s { None | Some("PSORTMODE_None") => SortMode::None, Some("PSORTMODE_ViewProjDepth") => SortMode::ViewProjDepth, Some("PSORTMODE_DistanceToView") => SortMode::DistanceToView, Some("PSORTMODE_Age_OldestFirst") | Some("PSORTMODE_Age_NewestFirst") => SortMode::Age, _ => SortMode::Unsupported }
}
fn alignment(s: Option<&str>) -> Alignment {
    match s { None | Some("PSA_Square") => Alignment::Square, Some("PSA_Rectangle") => Alignment::Rectangle, Some("PSA_Velocity") => Alignment::Velocity, _ => Alignment::TypeSpecific }
}
fn interp(s: Option<&str>) -> SubUvInterp {
    match s { None | Some("PSUVIM_None") => SubUvInterp::None, Some("PSUVIM_Linear") => SubUvInterp::Linear, Some("PSUVIM_Linear_Blend") => SubUvInterp::LinearBlend, Some("PSUVIM_Random") => SubUvInterp::Random, Some("PSUVIM_Random_Blend") => SubUvInterp::RandomBlend, _ => SubUvInterp::None }
}

fn required(p: &Value) -> Required {
    Required {
        material: ps(p, "Material").unwrap_or("").to_string(),
        use_local_space: pb(p, "bUseLocalSpace", false),
        emitter_duration: pf(p, "EmitterDuration", 1.0),
        emitter_delay: pf(p, "EmitterDelay", 0.0),
        emitter_loops: pi(p, "EmitterLoops", 0) as i32,
        sort_mode: sort_mode(ps(p, "SortMode")),
        screen_alignment: alignment(ps(p, "ScreenAlignment")),
        sub_images_h: pi(p, "SubImages_Horizontal", 1).max(1) as u32,
        sub_images_v: pi(p, "SubImages_Vertical", 1).max(1) as u32,
        interpolation: interp(ps(p, "InterpolationMethod")),
        random_image_time: pf(p, "RandomImageTime", 0.0),
        opacity_source: ps(p, "OpacitySourceMode").unwrap_or("").to_string(),
        alpha_threshold: pf(p, "AlphaThreshold", 0.0),
        cutout_texture: ps(p, "CutoutTexture").map(String::from),
        legacy_emitter_time: pb(p, "bUseLegacyEmitterTime", true),
        required_enabled: pb(p, "bEnabled", true),
    }
}

fn spawn(pk: &Pkg, p: &Value) -> Spawn {
    let bursts = p.get("BurstList").and_then(|a| a.as_array()).map(|a| a.iter().map(|b| Burst { count: pi(b, "Count", 0) as i32, count_low: pi(b, "CountLow", -1) as i32, time: pf(b, "Time", 0.0) }).collect()).unwrap_or_default();
    Spawn { rate: pk.d(p, "Rate"), rate_scale: pk.d_or(p, "RateScale", 1.0), burst_scale: pk.d_or(p, "BurstScale", 1.0), bursts }
}

fn seed_of(p: &Value) -> Option<i32> { p.get("RandomSeedInfo").and_then(|s| s.get("RandomSeeds")).and_then(|a| a.as_array()).and_then(|a| a.first()).and_then(|v| v.as_i64()).map(|v| v as i32) }

fn module(pk: &Pkg, class: &str, name: &str, p: &Value) -> Module {
    let base = class.strip_suffix("_Seeded").unwrap_or(class);
    let seeded = class.ends_with("_Seeded");
    let hz = |p: &Value| match ps(p, "HeightAxis") { Some("PMLPC_HEIGHTAXIS_X") => Axis::X, Some("PMLPC_HEIGHTAXIS_Y") => Axis::Y, _ => Axis::Z };
    let kind = match base {
        "ParticleModuleLifetime" => ModuleKind::Lifetime { lifetime: pk.d(p, "Lifetime") },
        "ParticleModuleLocation" => ModuleKind::Location { start: pk.d(p, "StartLocation") },
        "ParticleModuleLocationPrimitiveSphere" => ModuleKind::LocationSphere { start: pk.d(p, "StartLocation"), radius: pk.d(p, "StartRadius"), surface_only: pb(p, "SurfaceOnly", false), velocity: pb(p, "Velocity", false), velocity_scale: pk.d(p, "VelocityScale"), positive_z: pb(p, "Positive_Z", true), negative_z: pb(p, "Negative_Z", true) },
        "ParticleModuleLocationPrimitiveCylinder" => ModuleKind::LocationCylinder { start: pk.d(p, "StartLocation"), radius: pk.d(p, "StartRadius"), height: pk.d(p, "StartHeight"), height_axis: hz(p), surface_only: pb(p, "SurfaceOnly", false), velocity: pb(p, "Velocity", false), velocity_scale: pk.d(p, "VelocityScale"), positive_z: pb(p, "Positive_Z", true), negative_z: pb(p, "Negative_Z", true) },
        // BVISFIX: defaults of `InitializeDefaults` (ParticleModules_Location.cpp:798-825): StartOffset 0, Height 50, Angle 90, Thickness 0
        "ParticleModuleLocationPrimitiveTriangle" => ModuleKind::LocationTriangle { start_offset: pk.d_or3(p, "StartOffset", [0.0; 3]), height: pk.d_or(p, "Height", 50.0), angle: pk.d_or(p, "Angle", 90.0), thickness: pk.d_or(p, "Thickness", 0.0) },
        "ParticleModuleVelocity" => ModuleKind::Velocity { start: pk.d(p, "StartVelocity"), radial: pk.d(p, "StartVelocityRadial"), in_world_space: pb(p, "bInWorldSpace", false) },
        "ParticleModuleSize" => ModuleKind::Size { start: pk.d(p, "StartSize") },
        "ParticleModuleSizeMultiplyLife" => ModuleKind::SizeMultiplyLife { mult: pk.d(p, "LifeMultiplier"), x: pb(p, "MultiplyX", true), y: pb(p, "MultiplyY", true), z: pb(p, "MultiplyZ", true) },
        "ParticleModuleSizeScaleBySpeed" => { let v = |k: &str| p.get(k).and_then(|a| a.as_array()).map(|a| [a.first().and_then(as_f32).unwrap_or(0.0), a.get(1).and_then(as_f32).unwrap_or(0.0)]).unwrap_or([0.0; 2]); ModuleKind::SizeScaleBySpeed { speed_scale: v("SpeedScale"), max_scale: v("MaxScale") } }
        "ParticleModuleColor" => ModuleKind::Color { color: pk.d(p, "StartColor"), alpha: pk.d(p, "StartAlpha") },
        "ParticleModuleColorOverLife" => ModuleKind::ColorOverLife { color: pk.d(p, "ColorOverLife"), alpha: pk.d(p, "AlphaOverLife"), clamp_alpha: pb(p, "bClampAlpha", true) },
        "ParticleModuleColorScaleOverLife" => ModuleKind::ColorScaleOverLife { color: pk.d(p, "ColorScaleOverLife"), alpha: pk.d(p, "AlphaScaleOverLife") },
        "ParticleModuleRotation" => ModuleKind::Rotation { start: pk.d(p, "StartRotation") },
        "ParticleModuleRotationRate" => ModuleKind::RotationRate { start: pk.d(p, "StartRotationRate") },
        "ParticleModuleMeshRotation" => ModuleKind::MeshRotation { start: pk.d(p, "StartRotation") },
        "ParticleModuleMeshRotationRate" => ModuleKind::MeshRotationRate { start: pk.d(p, "StartRotationRate") },
        "ParticleModuleAcceleration" => ModuleKind::Acceleration { accel: pk.d(p, "Acceleration"), world: pb(p, "bAlwaysInWorldSpace", false) },
        "ParticleModuleAccelerationConstant" => ModuleKind::AccelerationConstant { accel: pv3(p, "Acceleration", [0.0; 3]), world: pb(p, "bAlwaysInWorldSpace", false) },
        "ParticleModuleAccelerationDrag" => ModuleKind::Drag { coefficient: pk.d(p, "DragCoefficientRaw") },
        "ParticleModuleCameraOffset" => ModuleKind::CameraOffset { offset: pk.d(p, "CameraOffset"), spawn_time_only: pb(p, "bSpawnTimeOnly", false) },
        "ParticleModuleOrbit" => ModuleKind::Orbit { offset: pk.d(p, "OffsetAmount"), rotation: pk.d(p, "RotationAmount"), rotation_rate: pk.d(p, "RotationRateAmount") },
        "ParticleModuleParameterDynamic" => ModuleKind::ParameterDynamic { params: p.get("DynamicParams").and_then(|a| a.as_array()).map(|a| a.iter().map(|d| DynParam { name: ps(d, "ParamName").unwrap_or("").to_string(), value: pk.d(d, "ParamValue"), use_emitter_time: pb(d, "bUseEmitterTime", false), spawn_time_only: pb(d, "bSpawnTimeOnly", false), scale_velocity: pb(d, "bScaleVelocityByParamValue", false) }).collect()).unwrap_or_default() },
        "ParticleModulePivotOffset" => ModuleKind::PivotOffset { offset: p.get("PivotOffset").and_then(|a| a.as_array()).map(|a| [a.first().and_then(as_f32).unwrap_or(0.0), a.get(1).and_then(as_f32).unwrap_or(0.0)]).unwrap_or([0.0; 2]) },
        "ParticleModuleSubUV" => ModuleKind::SubUv { index: pk.d(p, "SubImageIndex") },
        "ParticleModuleSubUVMovie" => ModuleKind::SubUvMovie { index: pk.d(p, "SubImageIndex"), frame_rate: pk.d(p, "FrameRate"), starting_frame: pi(p, "StartingFrame", 0) as i32 },
        "ParticleModuleCollision" => ModuleKind::Collision { damping: pk.d(p, "DampingFactor"), damping_rotation: pk.d(p, "DampingFactorRotation"), max_collisions: pk.d(p, "MaxCollisions"), delay: pk.d(p, "DelayAmount"), particle_mass: pk.d(p, "ParticleMass"), halt_when_done: ps(p, "CollisionCompletionOption") == Some("EPCC_HaltCollisions") },
        "ParticleModuleCollisionGPU" => ModuleKind::CollisionGpu { friction: pf(p, "Friction", 0.0), resilience: pk.d(p, "Resilience"), resilience_scale_over_life: pk.d(p, "ResilienceScaleOverLife"), radius_scale: pf(p, "RadiusScale", 1.0), stop: ps(p, "Response") == Some("EParticleCollisionResponse::Stop") },
        "ParticleModuleMeshMaterial" => ModuleKind::MeshMaterial { materials: p.get("MeshMaterials").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|m| m.as_str().filter(|s| !s.is_empty()).map(String::from)).collect()).unwrap_or_default() },
        "ParticleModuleLight" => ModuleKind::Light { color_scale: pk.d(p, "ColorScaleOverLife"), brightness: pk.d(p, "BrightnessOverLife"), radius_scale: pk.d(p, "RadiusScale"), exponent: pk.d(p, "LightExponent"), inverse_squared: pb(p, "bUseInverseSquaredFalloff", true), spawn_fraction: pf(p, "SpawnFraction", 1.0), affects_translucency: pb(p, "bAffectsTranslucency", false), high_quality: pb(p, "bHighQualityLights", false) },
        "ParticleModuleVelocityInheritParent" => ModuleKind::VelocityInheritParent { scale: pk.d_or3(p, "Scale", [1.0; 3]) },
        "ParticleModuleMeshRotationRateMultiplyLife" => ModuleKind::MeshRotationRateMultiplyLife { mult: pk.d(p, "LifeMultiplier") },
        "ParticleModuleSizeScale" => ModuleKind::SizeScale { scale: pk.d(p, "SizeScale") },
        // constructor defaults (ParticleModules_Spawn.cpp:258): UnitScalar 50, MovementTolerance 0.1, MaxFrameDistance 0, SpawnPerUnit = constant 0
        "ParticleModuleSpawnPerUnit" => ModuleKind::SpawnPerUnit { unit_scalar: pf(p, "UnitScalar", 50.0), spawn_per_unit: pk.d(p, "SpawnPerUnit"), max_frame_distance: pf(p, "MaxFrameDistance", 0.0), ignore_spawn_rate_when_moving: pb(p, "bIgnoreSpawnRateWhenMoving", false),
            movement_tolerance: pf(p, "MovementTolerance", 0.1), ignore_movement: [pb(p, "bIgnoreMovementAlongX", false), pb(p, "bIgnoreMovementAlongY", false), pb(p, "bIgnoreMovementAlongZ", false)] },
        "ParticleModuleOrientationAxisLock" => ModuleKind::OrientationAxisLock { axis: AxisLock::from_ue(ps(p, "LockAxisFlags").unwrap_or("EPAL_NONE")) },
        "ParticleModuleVectorFieldLocal" => ModuleKind::Unsupported { class: class.into(), note: format!("vector field {} (VectorFieldStatic volume data) is not decoded", ps(p, "VectorField").unwrap_or("?")) },
        "ParticleModuleVectorFieldRotationRate" => ModuleKind::Unsupported { class: class.into(), note: "rotation rate of a vector field (only meaningful with ParticleModuleVectorFieldLocal)".into() },
        other => ModuleKind::Unsupported { class: other.into(), note: "module class not implemented".into() },
    };
    Module { kind, enabled: pb(p, "bEnabled", true), lod_validity: pi(p, "LODValidity", 0) as u32, seed: if seeded { seed_of(p).or(Some(0)) } else { None }, name: name.to_string() }
}

fn type_data(pk: &Pkg, path: Option<&str>) -> TypeData {
    let Some((class, _, p)) = path.and_then(|x| pk.get(x)) else { return TypeData::Sprite };
    match class.as_str() {
        "ParticleModuleTypeDataMesh" => TypeData::Mesh { mesh: ps(p, "Mesh").unwrap_or("").to_string(), roll_pitch_yaw: pk.d(p, "RollPitchYawRange"), override_material: pb(p, "bOverrideMaterial", false) },
        "ParticleModuleTypeDataGpu" => {
            let e = p.get("EmitterInfo").cloned().unwrap_or(Value::Null);
            TypeData::Gpu(GpuInfo { max_particle_count: pi(&e, "MaxParticleCount", 0) as u32, collision: pb(&e, "bEnableCollision", false), collision_mode: ps(&e, "CollisionMode").unwrap_or("").to_string(), constant_acceleration: pv3(&e, "ConstantAcceleration", [0.0; 3]), max_lifetime: pf(&e, "MaxLifetime", 0.0) })
        }
        "ParticleModuleTypeDataRibbon" => {
            let d = RibbonInfo::default();
            let axis = match ps(p, "RenderAxis").unwrap_or("") { s if s.ends_with("Trails_SourceUp") => 1, s if s.ends_with("Trails_WorldUp") => 2, _ => 0 };
            TypeData::Ribbon(RibbonInfo { max_tessellation_between_particles: pi(p, "MaxTessellationBetweenParticles", d.max_tessellation_between_particles as i64) as i32,
                max_particle_in_trail_count: pi(p, "MaxParticleInTrailCount", d.max_particle_in_trail_count as i64) as i32, tiling_distance: pf(p, "TilingDistance", d.tiling_distance),
                distance_tessellation_step_size: pf(p, "DistanceTessellationStepSize", d.distance_tessellation_step_size), enable_tangent_diff_interp_scale: pb(p, "bEnableTangentDiffInterpScale", d.enable_tangent_diff_interp_scale),
                tangent_tessellation_scalar: pf(p, "TangentTessellationScalar", d.tangent_tessellation_scalar), tangent_spawning_scalar: pf(p, "TangentSpawningScalar", d.tangent_spawning_scalar),
                spawn_initial_particle: pb(p, "bSpawnInitialParticle", d.spawn_initial_particle), render_axis: axis,
                enable_previous_tangent_recalculation: pb(p, "bEnablePreviousTangentRecalculation", d.enable_previous_tangent_recalculation), tangent_recalculation_every_frame: pb(p, "bTangentRecalculationEveryFrame", d.tangent_recalculation_every_frame),
                dead_trails_on_deactivate: pb(p, "bDeadTrailsOnDeactivate", d.dead_trails_on_deactivate) })
        }
        _ => TypeData::Sprite,
    }
}

/// Decode the particle system `package_path` (`/Game/Effects/.../P_X`) from a pak set.
pub fn import_system(set: &fn_pak::PakSet, package_path: &str) -> Result<ParticleSystem, String> {
    let long = package_path.split('.').next().unwrap_or(package_path);
    let data = PackageData::load(set, long).map_err(|e| format!("{long}: {e}"))?;
    let pkg = data.package().map_err(|e| format!("{long}: {e}"))?;
    let pk = Pkg::load(&pkg);
    let (_, name, sp) = pk.first_of("ParticleSystem").ok_or_else(|| format!("{long}: no ParticleSystem export"))?;
    let mut sys = ParticleSystem { name: name.clone(), package: long.to_string(), lod_method: ps(sp, "LODMethod").unwrap_or("").to_string(), ..Default::default() };
    sys.lod_distances = sp.get("LODDistances").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(as_f32).collect()).unwrap_or_default();
    if pb(sp, "bUseFixedRelativeBoundingBox", false) {
        if let Some(b) = sp.get("FixedRelativeBoundingBox") { sys.fixed_bounds = Some((pv3(b, "Min", [0.0; 3]), pv3(b, "Max", [0.0; 3]))); }
    }
    for ep in sp.get("Emitters").and_then(|a| a.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
        let Some((class, _, e)) = pk.get(ep) else { continue };
        if class != "ParticleSpriteEmitter" { eprintln!("warning: {name}: emitter class {class} not supported ({ep})"); continue; }
        let mut em = Emitter { name: ps(e, "EmitterName").unwrap_or("").to_string(), lods: vec![] };
        for lp in e.get("LODLevels").and_then(|a| a.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
            let Some((_, _, l)) = pk.get(lp) else { continue };
            let req = ps(l, "RequiredModule").and_then(|r| pk.get(r)).map(|(_, _, p)| required(p));
            let Some(required) = req else { continue };
            let spawn_p = ps(l, "SpawnModule").and_then(|r| pk.get(r)).map(|(_, _, p)| spawn(&pk, p)).unwrap_or_default();
            let td = type_data(&pk, ps(l, "TypeDataModule"));
            let modules = l.get("Modules").and_then(|a| a.as_array()).into_iter().flatten().filter_map(|v| v.as_str()).filter_map(|m| pk.get(m)).map(|(c, n, p)| module(&pk, c, n, p)).collect();
            em.lods.push(Lod { level: pi(l, "Level", 0) as i32, enabled: pb(l, "bEnabled", true), required, spawn: spawn_p, type_data: td, modules, peak_active: pi(l, "PeakActiveParticles", 0) as i32 });
        }
        sys.emitters.push(em);
    }
    Ok(sys)
}



/// Diagnostics: every export of the package (class, name, tagged properties as JSON) as one JSON document (`fn-particles-export --dump <package>`).
pub fn dump_package(set: &fn_pak::PakSet, package_path: &str) -> Result<String, String> {
    let long = package_path.split('.').next().unwrap_or(package_path);
    let data = PackageData::load(set, long).map_err(|e| format!("{long}: {e}"))?;
    let pkg = data.package().map_err(|e| format!("{long}: {e}"))?;
    let pk = Pkg::load(&pkg);
    let list: Vec<Value> = pk.exports.iter().enumerate().map(|(i, (c, n, p))| serde_json::json!({ "index": i + 1, "class": c, "name": n, "props": p })).collect();
    serde_json::to_string_pretty(&list).map_err(|e| e.to_string())
}
