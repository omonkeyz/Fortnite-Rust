//! PARTICLE HOOK (fn-app): the original 1.8 Cascade particle systems of the harvest feedback. The data are the decoded `UParticleSystem`s of `fn-particles` (exported to
//! `<assets>/particles` by `fn-particles-export`), the simulation is `fn_particles::Instance`, the drawing is the particle pass of `fn-render` (`ParticleFrame`).
//!
//! * [`init`] (once, from `app.rs::start_loading`): loads the catalog and maps `Harvestable::proto` to the prop's `DeathParticles` (`props.json` `destruction.death_particles`).
//! * [`on_events`] (once per presented frame, from `harvest_fx::feed`): `FxEvent::Broke` spawns the prop's death system at the prop; `FxEvent::Hit` / `WeakSpotHit` spawn the
//!   pickaxe impact system `ImpactPhysicalSurfaceEffects[surface]` at the hit point, facing the player (see [`impact_surface`]).
//! * [`submit`] (every `Scene::draw`): ticks the live systems by the frame's dt, converts their draw data to a `ParticleFrame` and hands it to the renderer; textures, materials
//!   (`fn_particles::assets::Look`) and meshes are created on first use.
//!
//! Impact surface: the native helper of `AFortWeapon::PlayNativeImpactFX` (0x92dc60) takes the surface from the hit's physical material, else from the hit actor's
//! `IFortDamageableActorInterface` (0x5c4930), and spawns `ImpactPhysicalSurfaceEffects[surface]` only when that element is set (no fallback element). Surface names
//! are `PhysicsSettings:PhysicalSurfaces` (`C:\FortniteRef\scratch\CookedIniVersion.txt` L1599-1620: 1 Wood, 2 Stone, 3 Metal, 4 HumanFlesh). The pickaxe's array is
//! inherited along B_Melee_Impact_Pickaxe_Athena -> B_Melee_Impact_Generic -> B_Melee_Generic: [1] P_Melee_Hit_Wood, [2] P_Melee_Hit_Stone, [3] P_Melee_Hit_Metal,
//! [4]/[5] P_Melee_Impact_Default (the exporter walks the chain). A prop's surface follows the full precedence of `fn_hitreg::surface` (physical material of the hit
//! unless Default, else `ABuildingSMActor` 0x140477250: `PrimarySurfaceType`, else the `ResourceType` map), resolved per prop kind from the cooked class / mesh /
//! material chain by [`init_surfaces`]; until then (or without the paks) the `ResourceType` rule. The world collision of the original (distance fields) is a flat
//! ground at y = 0.
use fn_combat::harvest_fx::FxEvent;
use fn_game::GameSim;
use fn_particles::assets::{Blend, Catalog, Channel, Look, Ramp};
use fn_particles::{DrawKind, EmitterDraw, Env, Instance, Transform};
use fn_render::particles::*;
use fn_render::{CameraParams, Renderer};
use glam::Vec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Live systems kept at most (a long fight must not pile them up).
const MAX_LIVE: usize = 48;
/// EPhysicalSurface of a pickaxe hit on harvestable `object` (module docs: `fn_hitreg::surface::of_prop`); unknown objects are SurfaceType_Default.
fn impact_surface(sim: &GameSim, object: u64) -> u32 {
    sim.world.props.get((object & 0xffff_ffff) as usize).map_or(fn_hitreg::shoot::SURFACE_DEFAULT, fn_hitreg::surface::of_prop)
}

/// Resolves, on a worker thread, the hit surface of every prop kind (`protos[i]` = class package + static mesh of `Harvestable::proto` i) from the cooked data of the
/// install (`fn_hitreg::surface::resolve`) and publishes the table (`fn_hitreg::surface::set_proto_surfaces`), used by the pickaxe impacts here and the gun impacts.
pub fn init_surfaces(install: Option<PathBuf>, protos: Vec<(String, String)>) {
    let Some(install) = install else { eprintln!("[particles] no install: prop surfaces use the ResourceType rule"); return };
    std::thread::spawn(move || {
        let set = match fn_pak::PakSet::open_install(&install) { Ok(s) => s, Err(e) => { eprintln!("[particles] prop surfaces: cannot open the paks ({e}): ResourceType rule"); return } };
        let src = fn_pose::ClipSourceDir::Pak(&set);
        let v: Vec<Option<u32>> = protos.iter().map(|(bp, mesh)| match fn_hitreg::surface::resolve(&src, bp, Some(mesh).filter(|m| !m.is_empty()).map(|m| m.as_str())) {
            Ok(s) => Some(s.surface),
            Err(e) => { eprintln!("[particles] prop surface of {bp}: {e}"); None }
        }).collect();
        eprintln!("[particles] prop surfaces resolved for {}/{} kinds", v.iter().filter(|s| s.is_some()).count(), v.len());
        fn_hitreg::surface::set_proto_surfaces(v);
    });
}

struct Live { inst: Instance, reason: &'static str, born_frame: u32, peak: usize, serial: u64 }

#[derive(Default)]
pub(crate) struct Gpu {
    textures: HashMap<String, Option<ParticleTexId>>,
    materials: HashMap<String, Option<ParticleMatId>>,
    meshes: HashMap<String, Option<(ParticleMeshId, Arc<fn_particles::model::MeshDef>)>>,
    /// Material paths that could not be built (reported once).
    missing: std::collections::BTreeSet<String>,
    /// FIRST USE: [`warm_catalog`] ran for this state.
    warmed: bool,
    warm_at: usize,
}

/// HURT HOOK: puts a material built from its compiled shader (not the catalog `Look` heuristic) into a state's cache, so `batches` uses it for `ue_path`.
pub(crate) fn preset_material(renderer: &mut Renderer, cat: &Catalog, gpu: &mut Gpu, ue_path: &str, texture_png: &str, srgb: bool, desc: impl FnOnce(Option<ParticleTexId>) -> ParticleMaterialDesc) {
    let key = ue_path.split('.').next().unwrap_or(ue_path).to_string();
    if gpu.materials.contains_key(&key) { return; }
    let tex = tex_id(renderer, cat, gpu, texture_png, srgb);
    let id = renderer.create_particle_material(&desc(tex));
    gpu.materials.insert(key, Some(id));
}

/// FIRST USE (2026-10-06): builds every material (PNG decode + particle material) and mesh of a catalog's systems into `gpu`, the caches `batches` uses, so
/// that no first spawn decodes on the UI thread (the impact / harvest effects cost 21-25 ms at their first spawn, verification/load_time/normal_warm2.log).
/// Once per state; returns (materials, meshes) built.
pub(crate) fn warm_catalog(renderer: &mut Renderer, cat: &Catalog, gpu: &mut Gpu) -> Option<(usize, usize)> {
    if gpu.warmed || !crate::building_render::warm_enabled() { return Some((0, 0)); }
    let t = std::time::Instant::now();
    for sys in cat.systems().skip(gpu.warm_at) {
        if t.elapsed().as_secs_f32() > 0.012 { return None; }
        gpu.warm_at += 1;
        for e in &sys.emitters {
            for l in &e.lods {
                if !l.required.material.is_empty() { let _ = material_id(renderer, cat, gpu, &l.required.material); }
                if let fn_particles::model::TypeData::Mesh { mesh, .. } = &l.type_data {
                    if let Some((_, def)) = mesh_id(renderer, cat, gpu, mesh) { for m in &def.materials { if !m.is_empty() { let _ = material_id(renderer, cat, gpu, m); } } }
                }
            }
        }
    }
    gpu.warmed = true;
    Some((gpu.materials.len(), gpu.meshes.len()))
}

/// FIRST USE: [`warm_catalog`] for the harvest / impact / death systems (called under the loading screen, `App::finishing_frame`).
pub fn prebuild(renderer: &mut Renderer) -> bool {
    let Ok(mut g) = HOST.lock() else { return true };
    let Some(h) = g.as_mut() else { return true };
    let cat = h.catalog.clone();
    let t = std::time::Instant::now();
    let Some((m, s)) = warm_catalog(renderer, &cat, &mut h.gpu) else { return false };
    if m + s > 0 { eprintln!("[particles] warm-up: {m} materials, {s} meshes built in {:.0} ms (before the first harvest / impact)", t.elapsed().as_secs_f32() * 1000.); }
    true
}

struct Host {
    catalog: Arc<Catalog>,
    /// `death_particles` package path by `Harvestable::proto`.
    death: Vec<Option<String>>,
    live: Vec<Live>,
    serial: u64,
    gpu: Gpu,
    log: Vec<String>,
    frame: u32,
    spawned: Vec<(String, String, u64, usize)>,
}

static HOST: Mutex<Option<Host>> = Mutex::new(None);

fn log(h: &mut Host, s: String) { if h.log.len() < 20_000 { h.log.push(s); } }

/// Loads the exported particle assets (`<root>/particles` of an asset root, or `FN_PARTICLES_DIR`) and the proto -> death system table. Returns false (and logs) without them:
/// the harvest then simply has no particles.
pub fn init(roots: &[PathBuf], death: Vec<Option<String>>) -> bool {
    let dir = fn_live::var_os("FN_PARTICLES_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles")).find(|d| d.join("index.json").is_file()));
    let Some(dir) = dir else { eprintln!("[particles] no `particles` folder with an index.json in the asset roots (run fn-particles-export): the harvest plays no particles"); return false };
    match Catalog::load(&dir) {
        Ok(c) => {
            eprintln!("[particles] {} particle systems loaded from {}", c.system_count(), dir.display());
            prefetch(&dir);
            *HOST.lock().unwrap() = Some(Host { catalog: Arc::new(c), death, live: vec![], serial: 0, gpu: Gpu::default(), log: vec![], frame: 0, spawned: vec![] });
            true
        }
        Err(e) => { eprintln!("[particles] cannot load {}: {e}", dir.display()); false }
    }
}

/// Warms the OS file cache for the PNGs on a worker thread (decoding happens on first use).
fn prefetch(dir: &Path) {
    let dir = dir.join("textures");
    std::thread::spawn(move || { for e in std::fs::read_dir(dir).into_iter().flatten().flatten() { let _ = std::fs::read(e.path()); } });
}

fn spawn(h: &mut Host, system: &str, pos: [f32; 3], normal: Option<[f32; 3]>, yaw: f32, eye: [f32; 3], reason: &'static str, key: u64) {
    let Some(sys) = h.catalog.system(system) else { log(h, format!("f{:05} particles: system {system} not in the catalog ({reason})", h.frame)); return };
    let xf = match normal { Some(n) => Transform::from_game_facing(pos, n), None => Transform::from_game(pos, yaw) };
    let dist_cm = ((pos[0] - eye[0]).powi(2) + (pos[1] - eye[1]).powi(2) + (pos[2] - eye[2]).powi(2)).sqrt() * 100.;
    h.serial += 1;
    let seed = key.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ h.serial.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mut inst = Instance::new(sys.clone(), xf, seed, dist_cm);
    inst.id = h.serial;
    let lod = inst.lod;
    log(h, format!("f{:05} particles spawn {} ({reason}) at ({:.2},{:.2},{:.2}) lod {lod} dist {:.1} m emitters {} seed {seed:#x}", h.frame, sys.name, pos[0], pos[1], pos[2], dist_cm / 100., sys.emitters.len()));
    if h.live.len() >= MAX_LIVE { let old = h.live.remove(0); finish(h, old); }
    h.live.push(Live { inst, reason, born_frame: h.frame, peak: 0, serial: h.serial });
}

fn finish(h: &mut Host, l: Live) {
    let per: Vec<String> = l.inst.emitters.iter().map(|e| format!("{}={}", l.inst.system.emitters[e.index].name, e.spawned_total)).collect();
    log(h, format!("f{:05} particles done {} ({}) lived {:.2}s spawned {} peak live {} [{}]", h.frame, l.inst.system.name, l.reason, l.inst.time, l.inst.spawned_total(), l.peak, per.join(" ")));
    h.spawned.push((l.inst.system.name.clone(), l.reason.to_string(), l.inst.spawned_total(), l.peak));
}

/// The harvest events of this frame (see the module docs). `sim` gives the prop transform, the frame number and the camera.
pub fn on_events(events: &[FxEvent], sim: &GameSim) {
    let mut guard = HOST.lock().unwrap();
    let Some(h) = guard.as_mut() else { return };
    h.frame = sim.frames;
    let eye = sim.cam_view.eye; let eye3 = [eye.x, eye.y, eye.z];
    for e in events {
        match *e {
            FxEvent::Broke { object, pos, proto, .. } => {
                let yaw = sim.world.props.get((object & 0xffff_ffff) as usize).map_or(0., |p| p.yaw);
                match h.death.get(proto as usize).cloned().flatten() {
                    Some(sys) => spawn(h, &sys, [pos.x, pos.y, pos.z], None, yaw, eye3, "break", object),
                    None => log(h, format!("f{:05} particles: prop proto {proto} has no DeathParticles (nothing to play)", h.frame)),
                }
            }
            FxEvent::Hit { object, pos, critical, .. } => impact(h, object, impact_surface(sim, object), pos, eye, if critical { "hit-crit" } else { "hit" }),
            FxEvent::WeakSpotHit { object, pos, .. } => impact(h, object, impact_surface(sim, object), pos, eye, "hit-weakspot"),
            FxEvent::WeakSpotSpawned { .. } => {}
        }
    }
}

fn impact(h: &mut Host, object: u64, surface: u32, pos: Vec3, eye: Vec3, reason: &'static str) {
    // 0x92dc60: `if (effects[surface] != null) SpawnEmitterAtLocation(...)`: an unset element plays nothing
    let Some(sys) = h.catalog.impact_effects.get(&surface).cloned() else { log(h, format!("f{:05} particles: ImpactPhysicalSurfaceEffects[{surface}] is not set ({reason}, nothing to play)", h.frame)); return };
    let n = (eye - pos).normalize_or_zero();
    spawn(h, &sys, [pos.x, pos.y, pos.z], Some([n.x, n.y, n.z]), 0., [eye.x, eye.y, eye.z], reason, object ^ 0x1234_5678);
}

// ---- GPU assets ------------------------------------------------------------------------------------------------------------------------
fn tex_id(renderer: &mut Renderer, cat: &Catalog, gpu: &mut Gpu, png: &str, srgb: bool) -> Option<ParticleTexId> {
    let key = format!("{png}|{srgb}");
    if let Some(t) = gpu.textures.get(&key) { return *t; }
    let id = match std::fs::read(cat.texture_path(png)).map_err(image::ImageError::IoError).and_then(|b| image::load_from_memory(&b)) {
        Ok(img) => { let rgba = img.to_rgba8(); Some(renderer.create_particle_texture(rgba.width(), rgba.height(), rgba.as_raw(), srgb, png)) }
        Err(e) => { eprintln!("[particles] texture {png}: {e}"); None }
    };
    gpu.textures.insert(key, id); id
}

fn channel(c: Channel) -> u8 { match c { Channel::R => 0, Channel::G => 1, Channel::B => 2, Channel::A => 3 } }

fn material_id(renderer: &mut Renderer, cat: &Catalog, gpu: &mut Gpu, ue_path: &str) -> Option<ParticleMatId> {
    let key = ue_path.split('.').next().unwrap_or(ue_path).to_string();
    if let Some(m) = gpu.materials.get(&key) { return *m; }
    let id = match cat.material(ue_path) {
        Some(def) => {
            let look = Look::from_material(&def);
            let texture = look.texture.as_ref().and_then(|p| tex_id(renderer, cat, gpu, p, look.texture_srgb));
            let (ramp_mode, ramp) = match look.ramp { Ramp::None => (0, [[1.; 3]; 3]), Ramp::Two { dark, light } => (2, [dark, light, light]), Ramp::Three { darks, mids, highlights } => (3, [darks, mids, highlights]) };
            let blend = match look.blend { Blend::Translucent => fn_render::fx::FxBlend::Translucent, Blend::Additive => fn_render::fx::FxBlend::Additive, Blend::Modulate => fn_render::fx::FxBlend::Modulate, Blend::Opaque | Blend::Masked => fn_render::fx::FxBlend::Opaque };
            let desc = ParticleMaterialDesc { label: look.name.clone(), blend, masked: look.blend == Blend::Masked, clip: look.clip, unlit: look.unlit, two_sided: look.two_sided, texture, opacity_channel: look.opacity.map(channel), value_channel: look.value_channel.map(channel), ramp_mode, ramp, opacity_exponent: look.opacity_exponent, emissive: look.emissive, tint: [1.; 4], separate_translucency: def.separate_translucency,
                // PARTICLE REFLECTIONS (2026-10-05): an opaque / masked DefaultLit material whose Metallic / Specular / Roughness inputs are NOT connected writes the
                // UE4.16 input defaults 0 / 0.5 / 0.5 (MaterialShared.cpp:2893-2895) into GBufferB; connected ones are graph-driven (not evaluated here): unknown
                gbuffer: (def.shading_model == "MSM_DefaultLit" && matches!(look.blend, Blend::Opaque | Blend::Masked) && !def.connected.iter().any(|c| matches!(c.as_str(), "Metallic" | "Specular" | "Roughness"))).then_some([0.0, 0.5, 0.5]) };
            Some(renderer.create_particle_material(&desc))
        }
        None => { if gpu.missing.insert(key.clone()) { eprintln!("[particles] material {key} is not in the exported catalog: its particles are not drawn"); } None }
    };
    gpu.materials.insert(key, id); id
}

fn mesh_id(renderer: &mut Renderer, cat: &Catalog, gpu: &mut Gpu, path: &str) -> Option<(ParticleMeshId, Arc<fn_particles::model::MeshDef>)> {
    let key = path.split('.').next().unwrap_or(path).to_string();
    if let Some(m) = gpu.meshes.get(&key) { return m.clone(); }
    let id = cat.mesh(path).map(|m| (renderer.create_particle_mesh(&ParticleMeshDesc { label: m.name.clone(), positions: m.positions.clone(), normals: m.normals.clone(), uvs: m.uvs.clone(), indices: m.indices.clone() }), m));
    if id.is_none() && gpu.missing.insert(key.clone()) { eprintln!("[particles] mesh {key} is not in the exported catalog: its particles are not drawn"); }
    gpu.meshes.insert(key, id.clone()); id
}

/// Box extent (UE cm) of a catalog mesh: the AABB of its positions (exported as game metres, `(x, z, y) * 0.01`), which is the cooked render-data box
/// (`FStaticMeshRenderData::Bounds` is built from the vertex positions; no bounds extension on the particle meshes).
pub(crate) fn mesh_extent_cm(cat: &Catalog, path: &str) -> Option<[f32; 3]> {
    let m = cat.mesh(path)?;
    let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &m.positions { for k in 0..3 { mn[k] = mn[k].min(p[k]); mx[k] = mx[k].max(p[k]); } }
    if m.positions.is_empty() { return None; }
    let e = |k: usize| (mx[k] - mn[k]) * 0.5 * 100.0;
    Some([e(0), e(2), e(1)])
}
/// The translucency sort data of a particle system component: `CalculateTranslucentSortKey` (TranslucentRendering.cpp:858) on the origin of its dynamic bounds
/// (`Instance::bounds_box` = `UParticleSystemComponent::CalcBounds`), `TranslucencySortPriority` 0 (`UPrimitiveComponent` default; `SpawnEmitterAtLocation` /
/// `SpawnEmitterAttached` components never set it).
pub(crate) fn instance_sort(inst: &Instance, cat: &Catalog, camera: &CameraParams) -> fn_render::fx::FxSort { camera_sort(instance_bounds(inst, cat).origin, camera) }
/// The component's `Bounds` (`FBoxSphereBounds` of `CalcBounds`' box, BoxSphereBounds.h:62): the Primitive buffer's bounds and the sort origin.
pub(crate) fn instance_bounds(inst: &Instance, cat: &Catalog) -> fn_render::primitive_ub::BoxSphereBounds {
    let (mn, mx) = inst.bounds_box(&|m| mesh_extent_cm(cat, m));
    fn_render::primitive_ub::BoxSphereBounds::from_box(mn, mx)
}
pub(crate) fn camera_sees(center: Vec3, r: f32, camera: &CameraParams) -> bool {
    let (tx, ty) = camera.tan_half();
    let fwd = camera.forward.normalize_or_zero();
    let right = fwd.cross(camera.up).normalize_or_zero();
    let up = right.cross(fwd).normalize_or_zero();
    let c = center - camera.eye;
    let (z, x, y) = (c.dot(fwd), c.dot(right), c.dot(up));
    z >= -r && x.abs() <= z * tx + r * (1. + tx * tx).sqrt() && y.abs() <= z * ty + r * (1. + ty * ty).sqrt()
}
pub(crate) fn camera_sees_ue(b: &fn_render::primitive_ub::BoxSphereBounds, margin_m: f32, camera: &CameraParams) -> bool {
    camera_sees(Vec3::from(fn_particles::sim::ue_to_game(Vec3::from(b.origin))), b.radius * 0.01 + margin_m, camera)
}
/// `CalculateTranslucentSortKey` for a bounds origin (UE cm) seen from the port camera, priority 0 (see [`instance_sort`]).
pub(crate) fn camera_sort(origin: [f32; 3], camera: &CameraParams) -> fn_render::fx::FxSort {
    let eye = fn_particles::sim::game_to_ue([camera.eye.x, camera.eye.y, camera.eye.z]);
    let f = fn_particles::sim::game_to_ue([camera.forward.x, camera.forward.y, camera.forward.z]).normalize_or_zero();
    fn_render::fx::FxSort::new(origin, eye.to_array(), f.to_array(), 0)
}

/// One emitter's draw data -> batches of the particle pass; `sort` = the sort data of the emitter's component ([`instance_sort`]).
pub(crate) fn batches(renderer: &mut Renderer, cat: &Catalog, gpu: &mut Gpu, d: &EmitterDraw, sort: fn_render::fx::FxSort, frame: &mut ParticleFrame) {
    match d.kind {
        DrawKind::Sprite => {
            let Some(material) = material_id(renderer, cat, gpu, &d.material) else { return };
            let sprites = d.particles.iter().map(|p| ParticleSprite { pos: p.pos, size: [p.size[0], p.size[1]], rotation: p.rotation, color: p.color, sub_image: p.sub_image, velocity: p.velocity, dynamic: p.dynamic }).collect();
            frame.sprites.push(SpriteBatch { material, columns: d.sub_images.0, rows: d.sub_images.1, blend_frames: matches!(d.interpolation, fn_particles::model::SubUvInterp::LinearBlend | fn_particles::model::SubUvInterp::RandomBlend), velocity_aligned: d.alignment == fn_particles::model::Alignment::Velocity, pivot: d.pivot_offset, sprites, sort });
        }
        DrawKind::Mesh => {
            let Some((mesh, def)) = mesh_id(renderer, cat, gpu, &d.mesh) else { return };
            let instances: Vec<ParticleMeshInstance> = d.particles.iter().map(|p| ParticleMeshInstance { pos: p.pos, orient: p.orient, scale: p.size, color: p.color, dynamic: p.dynamic }).collect();
            for (first, count, slot) in def.sections.iter().copied() {
                // MeshMaterial module (override list, one entry per slot) else the mesh's own material of the slot
                let path = d.material_override.get(slot as usize).or_else(|| d.material_override.last()).cloned().or_else(|| def.materials.get(slot as usize).cloned()).unwrap_or_default();
                let Some(material) = material_id(renderer, cat, gpu, &path) else { continue };
                frame.meshes.push(MeshBatch { mesh, first_index: first, index_count: count, material, instances: instances.clone(), sort });
            }
        }
        DrawKind::Ribbon => {}
    }
}

/// Advances every live system by `dt` (the ground for the collisions: TERRAIN, the landscape or the flat y = 0); returns the indices of the systems that finished.
fn tick_all(h: &mut Host, dt: f32) -> Vec<usize> {
    // TERRAIN: the landscape (flat ground at y = 0 when none is installed); the particles run in `game_to_ue` cm: (x, y) = game (x, z) * 100
    let env_ground = |x: f32, y: f32| fn_terrain::ground_y(x * 0.01, y * 0.01) * 100.;
    let env = Env { ground: Some(&env_ground) };
    let mut done = vec![];
    for (i, l) in h.live.iter_mut().enumerate() {
        l.inst.tick(dt, &env);
        l.peak = l.peak.max(l.inst.live_count());
        if l.inst.finished() { done.push(i); }
    }
    done
}

/// Ticks the live systems by `dt` and submits their draw data to the renderer (every `Scene::draw`; `dt = 0` re-submits the same frame, e.g. a screenshot pass).
pub fn submit(renderer: &mut Renderer, camera: &CameraParams, dt: f32) {
    // MUZZLE HOOK: the muzzle flashes ride in the same particle frame (set_particle_frame replaces the previous one)
    let muzzle = crate::muzzle_hook::frame(renderer, camera, dt);
    let pickups = crate::pickups_fx::frame(renderer, camera, dt); // PICKUPS FX: the rarity effects of the world items
    let hurt = crate::hurt_hook::frame(renderer, camera, dt); // HURT HOOK: the damage direction lens effect (hurt_hook.rs)
    let mut guard = HOST.lock().unwrap();
    let Some(h) = guard.as_mut().filter(|h| !h.live.is_empty()) else { { let mut f = muzzle.unwrap_or_default(); if let Some(p) = pickups { f.sprites.extend(p.sprites); f.meshes.extend(p.meshes); } if let Some(p) = hurt { f.sprites.extend(p.sprites); f.meshes.extend(p.meshes); } renderer.set_particle_frame(f); } return };
    let eye = [camera.eye.x, camera.eye.y, camera.eye.z];
    let done = tick_all(h, dt);
    // draw the systems that are alive (a finished one has nothing left to draw), far to near
    let mut order: Vec<usize> = (0..h.live.len()).filter(|i| !done.contains(i)).collect();
    let d2 = |l: &Live| { let p = fn_particles::sim::ue_to_game(l.inst.xf.pos); (p[0] - eye[0]).powi(2) + (p[1] - eye[1]).powi(2) + (p[2] - eye[2]).powi(2) };
    order.sort_by(|a, b| d2(&h.live[*b]).partial_cmp(&d2(&h.live[*a])).unwrap_or(std::cmp::Ordering::Equal));
    let mut frame = ParticleFrame::default();
    let (cat, mut gpu) = (h.catalog.clone(), std::mem::take(&mut h.gpu));
    let vp = camera.proj_matrix() * camera.view_matrix();
    for i in order {
        let mut draws = vec![]; h.live[i].inst.draw(eye, &mut draws);
        if std::env::var_os("FNRT_PARTICLE_DEBUG").is_some() && dt > 0. && h.frame % 20 == 0 {
            for d in draws.iter().take(40) { if let Some(p) = d.particles.first() {
                let c = vp * glam::Vec4::new(p.pos[0], p.pos[1], p.pos[2], 1.);
                let msg = format!("f{:05} particles debug {}/{} {:?} n={} first pos=({:.2},{:.2},{:.2}) size=({:.3},{:.3},{:.3}) color={:?} sub={:.1} ndc=({:.2},{:.2}) w={:.2}", h.frame, d.system, d.emitter, d.kind, d.particles.len(), p.pos[0], p.pos[1], p.pos[2], p.size[0], p.size[1], p.size[2], p.color, p.sub_image, c.x / c.w, c.y / c.w, c.w);
                log(h, msg);
            } }
        }
        let sort = instance_sort(&h.live[i].inst, &cat, camera);
        for d in &draws { batches(renderer, &cat, &mut gpu, d, sort, &mut frame); }
    }
    h.gpu = gpu;
    if dt > 0. && (h.frame % 15 == 0 || h.live.iter().any(|l| l.inst.time < 0.05)) {
        let (s, m) = (frame.sprites.iter().map(|b| b.sprites.len()).sum::<usize>(), frame.meshes.iter().map(|b| b.instances.len()).sum::<usize>());
        let st = renderer.particle_stats().clone();
        log(h, format!("f{:05} particles frame: {} sprite batches / {s} sprites, {} mesh batches / {m} instances submitted; renderer drew last frame {} draw calls ({} sprites, {} mesh instances, {} skipped)", h.frame, frame.sprites.len(), frame.meshes.len(), st.draw_calls, st.sprites, st.mesh_instances, st.skipped));
    }
    if let Some(m) = muzzle { frame.sprites.extend(m.sprites); frame.meshes.extend(m.meshes); } // MUZZLE HOOK
    if let Some(p) = pickups { frame.sprites.extend(p.sprites); frame.meshes.extend(p.meshes); } // PICKUPS FX
    if let Some(p) = hurt { frame.sprites.extend(p.sprites); frame.meshes.extend(p.meshes); } // HURT HOOK
    renderer.set_particle_frame(frame);
    for i in done.into_iter().rev() { let l = h.live.remove(i); finish(h, l); }
}

/// Epilogue of a scripted run: writes `<name>-particles.txt` and returns the report section.
pub fn run_report(out: &Path, name: &str) -> String {
    let mut guard = HOST.lock().unwrap();
    let Some(h) = guard.as_mut() else { return String::new() };
    let live = std::mem::take(&mut h.live);
    for l in live { finish(h, l); }
    if h.log.is_empty() { return String::new(); }
    let path = out.join(format!("{name}-particles.txt"));
    let _ = std::fs::write(&path, h.log.join("\n") + "\n");
    let spawns = h.log.iter().filter(|l| l.contains(" particles spawn ")).count();
    let total: u64 = h.spawned.iter().map(|s| s.2).sum();
    format!("particles: {spawns} systems spawned ({} finished), {total} particles in total, log {}\n", h.spawned.len(), path.display())
}

/// The frame number of the most recent break (for screenshot timing in scripts).
pub fn live_systems() -> usize { HOST.lock().ok().and_then(|g| g.as_ref().map(|h| h.live.len())).unwrap_or(0) }

#[cfg(test)]
mod tests {
    use super::*;
    use fn_combat::{Kind, Resource};

    fn dir() -> Option<PathBuf> {
        let c = fn_live::var_os("FN_PARTICLES_DIR").map(PathBuf::from).into_iter().chain([PathBuf::from(r"C:\Users\Brady\Desktop\Fortnite Rust\FortniteRuntime\assets\particles")]);
        c.into_iter().find(|d| d.join("index.json").is_file())
    }

    /// The hook on the real exported assets: a break plays the prop's own DeathParticles (pine = P_Pine_Tree_Med_Destruction), every hit plays the pickaxe impact system, both
    /// are simulated to the end and counted in the log (deterministic seeds: the same events give the same counts).
    #[test] fn a_break_plays_the_death_system_and_a_hit_the_pickaxe_impact_and_both_finish() {
        let Some(d) = dir() else { eprintln!("no exported particles: skipped"); return };
        let run = || {
            *HOST.lock().unwrap() = None;
            let root = d.parent().unwrap().to_path_buf();
            assert!(init(&[root], vec![Some("/Game/Effects/Fort_Effects/World/Destruction/Trees/P_Pine_Tree_Med_Destruction.P_Pine_Tree_Med_Destruction".into()), None]));
            let mut sim = GameSim::new(); sim.step(&Default::default(), 1. / 60.);
            // PARTICLE FIX: the impact follows the prop's surface (module docs; 0x92dc60 plays `ImpactPhysicalSurfaceEffects[surface]`, `fn_hitreg::surface::of_prop`): a hit
            // on a Wood sandbox prop (GameSim::new() plants placeholders; ResourceType rule Wood -> SurfaceType1, `fn_hitreg::shoot::surface_of`) plays [1]
            // P_Melee_Hit_Wood; a hit naming no prop is SurfaceType_Default and plays P_Melee_Impact_Default
            let wood_prop = (0..sim.world.props.len() as u64).find(|&i| impact_surface(&sim, i) == 1).expect("a Wood sandbox prop");
            let ev = [FxEvent::Hit { object: wood_prop, pos: Vec3::new(0., 1.1, -3.), critical: false, connect_from: None }, FxEvent::Hit { object: 0xffff, pos: Vec3::new(0., 1.1, -3.), critical: false, connect_from: None }, FxEvent::Broke { object: 1, pos: Vec3::new(0., 0., -3.), kind: Kind::Tree, resource: Resource::Wood, proto: 0 }, FxEvent::Broke { object: 2, pos: Vec3::ZERO, kind: Kind::Rock, resource: Resource::Stone, proto: 1 }];
            on_events(&ev, &sim);
            let mut g = HOST.lock().unwrap(); let h = g.as_mut().unwrap();
            assert_eq!(h.live.len(), 3, "wood impact + default impact + pine death system; proto 1 has no DeathParticles");
            assert!(h.log.iter().any(|l| l.contains("proto 1 has no DeathParticles")));
            let mut frames = 0; while !h.live.is_empty() && frames < 60 * 12 { for i in tick_all(h, 1. / 60.).into_iter().rev() { let l = h.live.remove(i); finish(h, l); } frames += 1; }
            assert!(h.live.is_empty(), "both systems end (took {frames} frames)");
            let done: Vec<(String, u64)> = h.spawned.iter().map(|s| (s.0.clone(), s.2)).collect();
            let pine = done.iter().find(|s| s.0 == "P_Pine_Tree_Med_Destruction").unwrap().1; let imp = done.iter().find(|s| s.0 == "P_Melee_Impact_Default").unwrap().1;
            assert!((37..=70).contains(&pine), "pine particles {pine}"); assert!((3..=4).contains(&imp), "impact particles {imp}");
            let wood = done.iter().find(|s| s.0 == "P_Melee_Hit_Wood").map(|s| s.1); assert!(wood.map_or(false, |n| n > 0), "wood impact particles {wood:?} ({done:?})");
            done
        };
        assert_eq!(run(), run(), "same events, same counts");
        *HOST.lock().unwrap() = None;
    }
}