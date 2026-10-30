//! PICKUPS DXBC (fn-app): the particle materials of the pickup effects drawn with the ORIGINAL cooked shader programs (the base-pass vertex + pixel shaders of the material's
//! `FMaterialShaderMap`, DXBC translated by `fn-dxbc`, assembled by `fn-fx`), instead of the "look" `fn_particles::assets::Look` rebuilds from parameter names (the cooked packages
//! strip the material graph; the shader bytecode keeps all of it).
//!
//! Same pipeline as the storm (`fn_matshader::storm`) and the weak-point ring (`fn_fx::ring`): `Material::load` (flags, instance chain, uniform expression set = the `Material`
//! constant buffer, referenced textures), `build_material_layout` (named buffers, samplers, scene-depth texture, pipeline), the `View` / `Primitive` / `SpriteVF` buffers in Unreal
//! conventions (`fn_fx::view`). The permutation is `TBasePassVSFNoLightMapPolicy` + `TBasePassPSFNoLightMapPolicy` of the vertex factory the emitter type uses:
//! `FParticleSpriteVertexFactory` (sprite emitters), `FMeshParticleVertexFactory` (mesh emitters); every material of the pickup effects is `MSM_Unlit` (see `pickupmat`
//! research example of fn-fx).
//!
//! What the engine feeds and where it is taken from (UE 4.16 source in `Engine/Source/Runtime/Engine/Private/Particles/ParticleSystemRender.cpp` unless noted):
//! * sprite vertex = `FParticleSpriteVertex` (`Engine/Public/ParticleHelper.h`): Position + RelativeTime, OldPosition + ParticleId, Size.xy + Rotation + SubImageIndex, Color, then the
//!   corner `TexCoord` of `GParticleTexCoordVertexBuffer` and the `DynamicParameter` stream (`ParticleSpriteVertexFactory.usf`: `ATTRIBUTE0..5`);
//! * `SpriteVF` uniform buffer = `FParticleSpriteUniformParameters`, filled like `FDynamicSpriteEmitterData::UpdateRenderThreadResourcesEmitter` (ParticleSystemRender.cpp:1258-1360);
//! * mesh instance = `FMeshParticleInstanceVertex` (`MeshParticleVertexFactory.usf`: Transform 1..3 = `ATTRIBUTE8..10`, SubUV `11`, SubUVLerpAndRelTime `12`, DynamicParameter `13`,
//!   Color `14`, Velocity `15`);
//! * `Primitive` = the component's `LocalToWorld` for local-space emitters, identity for world-space ones (`UpdateWorldSpacePrimitiveUniformBuffer`, ParticleSystemRender.cpp:1065).
use fn_fx::gpu::{build_material_layout, LayoutOpts, load_mesh, sprite_vertex_buffers_dyn, vertex_buffers, MaterialGpu, MeshGpu, SharedBuffers, TextureCache, Vf, INSTANCE_STRIDE, SPRITE_DYN_VERTEX_STRIDE};
use fn_fx::material::Material;
use fn_fx::view::{bytes, primitive_buffer, primitive_buffer_local, sprite_vf_buffer, ViewParams, VIEW_FLOAT4S};
use fn_particles::model::{Alignment, AxisLock, SubUvInterp};
use fn_particles::{DrawKind, EmitterDraw, Transform};
use fn_pak::PakSet;
use fn_render::fx::*;
use glam::{DVec3, Vec3};
use std::collections::{HashMap, HashSet};

static PKG_JSON: std::sync::Mutex<Option<HashMap<String, std::sync::Arc<fn_fx::data::PackageJson>>>> = std::sync::Mutex::new(None);
pub fn package_json(set: &PakSet, path: &str) -> Option<std::sync::Arc<fn_fx::data::PackageJson>> {
    if let Some(p) = PKG_JSON.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(path).cloned())) { return Some(p); }
    let p = std::sync::Arc::new(fn_fx::data::PackageJson::load(set, path).ok()?);
    if let Ok(mut g) = PKG_JSON.lock() { g.get_or_insert_with(HashMap::new).insert(path.to_owned(), p.clone()); }
    Some(p)
}
#[path = "lobby_ghosts.rs"]
pub mod ghosts;

/// Shader type names of the unlit translucent / additive base pass without a lightmap (`TBasePassVSFNoLightMapPolicy` / `...PS...`): the permutation the shader maps of all the
/// pickup-effect materials carry for their particle vertex factories (`pickupmat` listing).
pub const BASEPASS_VS: &str = "TBasePassVSFNoLightMapPolicy";
pub const BASEPASS_PS: &str = "TBasePassPSFNoLightMapPolicy";
/// `PivotOffset` the emitter starts from (`FParticleSpriteEmitterInstance`: `PivotOffset(-0.5f, -0.5f)`; `ParticleModulePivotOffset` adds to it), UE4.16 ParticleEmitterInstances.cpp / ParticleHelper.h.
pub const DEFAULT_PIVOT: [f32; 2] = [-0.5, -0.5];
/// Frame time used for `InvDeltaSeconds` / the OldPosition of a paused (screenshot re-submit) frame.
pub const REFERENCE_DT: f32 = 1.0 / 60.0;
/// `UniformParameters.RotationBias` for `EPAL_ROTATE_Z` (ParticleSystemRender.cpp:1312: `0.5f * PI`).
pub const ROTATE_Z_BIAS: f32 = std::f32::consts::FRAC_PI_2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind { Sprite, Mesh, HoloLocal, HoloSkin, /// STATIC ITEM: a section material of a `PickupStaticMesh` (the item's own cooked material, `FLocalVertexFactory`).
    StaticItem, Ribbon }

struct Entry { path: String, mat: Material, gpu: MaterialGpu, kind: Kind }
struct Slot { prim: FxBufId, sprite_vf: FxBufId, mat: FxBufId }

pub struct Dxbc {
    black_hole_checked: HashSet<String>,
    set: PakSet,
    cache: TextureCache,
    shared: SharedBuffers,
    view_buf: FxBufId,
    light_buf: FxBufId,
    /// `PrimitiveFade` uniform buffer: `FadeTimeScaleBias` = (0, 1) = not fading (`fn_matshader::frame::fade_buffer`); a zero buffer would fade the primitive out completely.
    fade_buf: FxBufId,
    entries: Vec<Entry>,
    index: HashMap<(String, Kind), Result<usize, String>>,
    meshes: HashMap<String, Result<MeshGpu, String>>,
    holo: HashMap<String, Result<HoloMesh, String>>,
    holo_normals: HashMap<String, Vec<String>>,
    /// `"<system>/<emitter>"` of the emitters whose `EmitterRenderMode` draws no particles (`ERM_LightsOnly`, `ERM_None`: P_Pickup's `Light` emitter only casts the particle light).
    no_draw: HashSet<String>,
    render_checked: HashSet<String>,
    pool: Vec<Slot>,
    used: usize,
    binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    touched: HashSet<usize>,
    frame: FxFrame,
    /// The view of this frame (`begin`): the origin the translucency sort keys measure from.
    view: Option<ViewParams>,
    time: f32,
    /// Human readable lines for the run report (what was ported, what each material asked for).
    pub log: Vec<String>,
    /// STATIC ITEM (2026-10-05, see [`Dxbc::draw_static_item`]): per mesh whether all its section materials built, per entry its shadow-depth programs, the
    /// per-(entry, slot) base-pass VS globals buffers and bind groups, the shadow bind groups, the default surface material of the opaque shadow override,
    /// the cooked distance fields, and the shadow entries whose globals were written this frame.
    static_ok: HashMap<String, Result<(), String>>,
    static_shadows: HashMap<usize, Option<(MaterialGpu, bool)>>,
    static_vs: HashMap<(usize, usize), FxBufId>,
    static_binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    static_shadow_binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    default_mat: Option<Option<Material>>,
    static_dfs: HashMap<String, Option<std::sync::Arc<fn_render::distance_field::MeshDistanceField>>>,
    static_shadow_written: HashSet<usize>,
    pub no_shadows: bool,
    beam_vf: FxBufId,
    key_buf: String,
}

fn put(b: &mut Vec<u8>, xs: &[f32]) { for x in xs { b.extend(x.to_le_bytes()); } }
fn swap(v: [f32; 3]) -> [f32; 3] { [v[0], v[2], v[1]] }

impl Dxbc {
    pub fn load(install: &std::path::Path, fx: &mut FxRenderer) -> Result<Dxbc, String> {
        let set = PakSet::open_install(install).map_err(|e| e.to_string())?;
        let view_buf = fx.create_buffer("pickup view", VIEW_FLOAT4S * 16);
        let light_buf = fx.create_buffer("pickup light", fn_render::sky_light::FX_LIGHT_BYTES); // SKY LIGHT: FxLight with the SH (fn-fx program.rs)
        let sprite_vf = fx.create_buffer("pickup sprite vf", 10 * 16);
        fx.write_buffer(sprite_vf, &bytes(&sprite_vf_buffer()));
        let prim = fx.create_buffer("pickup primitive (shared)", 32 * 16);
        let fade_buf = fx.create_buffer("pickup PrimitiveFade", 16);
        let beam_vf = fx.create_buffer("pickup BeamTrailVF", 3 * 16);
        fx.write_buffer(fade_buf, &bytes(&fn_matshader::frame::fade_buffer()));
        let shared = SharedBuffers { view: view_buf, primitive: prim, sprite_vf, light: light_buf };
        let mut no_draw = HashSet::new();
        for pkg_path in [crate::pickups_fx::P_PICKUP, crate::pickups_fx::P_RIBBON, crate::pickups_fx::P_LOOT] {
            let Ok(pkg) = fn_fx::data::PackageJson::load(&set, pkg_path) else { continue };
            let sys = pkg_path.rsplit('/').next().unwrap_or(pkg_path);
            for e in pkg.exports.iter().filter(|e| e.class.starts_with("Particle") && e.class.ends_with("Emitter")) {
                let mode = e.props.get("EmitterRenderMode").and_then(|v| v.as_str()).unwrap_or("ERM_Normal");
                if mode != "ERM_Normal" { no_draw.insert(format!("{sys}/{}", e.props.get("EmitterName").and_then(|v| v.as_str()).unwrap_or(""))); }
            }
        }
        Ok(Dxbc { black_hole_checked: HashSet::new(), fade_buf, no_draw, render_checked: HashSet::new(), set, cache: TextureCache::deferred(), shared, view_buf, light_buf, entries: vec![], index: HashMap::new(), meshes: HashMap::new(), holo: HashMap::new(), holo_normals: HashMap::new(), pool: vec![], used: 0, binds: HashMap::new(), touched: HashSet::new(), frame: FxFrame::default(), view: None, time: 0., log: vec![],
            static_ok: HashMap::new(), static_shadows: HashMap::new(), static_vs: HashMap::new(), static_binds: HashMap::new(), static_shadow_binds: HashMap::new(), default_mat: None, static_dfs: HashMap::new(), static_shadow_written: HashSet::new(), no_shadows: false, beam_vf, key_buf: String::new() })
    }
    pub fn set(&self) -> &PakSet { &self.set }
    pub fn prefetch_packages(&self, paths: Vec<String>) {
        let set = self.set.clone();
        let _ = std::thread::Builder::new().name("emitter packages".into()).spawn(move || { for p in paths { let _ = package_json(&set, &p); } });
    }
    pub fn skip_non_normal_emitters(&mut self, pkg_path: &str) -> usize {
        let Some(pkg) = package_json(&self.set, pkg_path) else { return 0 };
        let sys = pkg_path.rsplit('/').next().unwrap_or(pkg_path).to_owned();
        let mut n = 0;
        for e in pkg.exports.iter().filter(|e| e.class.starts_with("Particle") && e.class.ends_with("Emitter")) {
            let mode = e.props.get("EmitterRenderMode").and_then(|v| v.as_str()).unwrap_or("ERM_Normal");
            if mode != "ERM_Normal" { self.no_draw.insert(format!("{sys}/{}", e.props.get("EmitterName").and_then(|v| v.as_str()).unwrap_or(""))); n += 1; }
        }
        n
    }
    pub fn warm_ribbon_materials(&mut self, fx: &mut FxRenderer, pkg_path: &str) -> usize {
        let Some(pkg) = package_json(&self.set, pkg_path) else { return 0 };
        let leaf = |v: &serde_json::Value| v.as_str().map(|s| s.rsplit([':', '.']).next().unwrap_or(s).to_owned());
        let mut mats = vec![];
        for e in pkg.exports.iter().filter(|e| e.class.starts_with("Particle") && e.class.ends_with("Emitter")) {
            for l in e.props.get("LODLevels").and_then(|v| v.as_array()).into_iter().flatten().filter_map(leaf) {
                let Some(lod) = pkg.exports.iter().find(|x| x.name == l) else { continue };
                let td = lod.props.get("TypeDataModule").and_then(leaf).and_then(|t| pkg.exports.iter().find(|x| x.name == t)).map(|t| t.class.clone()).unwrap_or_default();
                if !td.contains("TypeDataRibbon") { continue; }
                let mat = lod.props.get("RequiredModule").and_then(leaf).and_then(|r| pkg.exports.iter().find(|x| x.name == r)).and_then(|r| r.props.get("Material")).and_then(|m| m.as_str()).map(str::to_owned);
                if let Some(m) = mat { if !mats.contains(&m) { mats.push(m); } }
            }
        }
        let mut n = 0;
        for m in mats { match self.material(fx, &m, Kind::Ribbon, 0) { Ok(_) => n += 1, Err(e) => { let line = format!("{pkg_path}: ribbon material {m}: {e}"); eprintln!("[pickups-fx] {line}"); self.log.push(line); } } }
        n
    }
    pub fn skip_trail_emitters(&mut self, pkg_path: &str) -> usize {
        let Some(pkg) = package_json(&self.set, pkg_path) else { return 0 };
        let sys = pkg_path.rsplit('/').next().unwrap_or(pkg_path).to_owned();
        let mut n = 0;
        for e in pkg.exports.iter().filter(|e| e.class.starts_with("Particle") && e.class.ends_with("Emitter")) {
            let leaf = |v: &serde_json::Value| v.as_str().map(|s| s.rsplit([':', '.']).next().unwrap_or(s).to_owned());
            let td = e.props.get("LODLevels").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(leaf)
                .and_then(|l| pkg.exports.iter().find(|x| x.name == l)).and_then(|l| l.props.get("TypeDataModule")).and_then(leaf)
                .and_then(|t| pkg.exports.iter().find(|x| x.name == t)).map(|t| t.class.clone()).unwrap_or_default();
            if td.contains("TypeDataBeam") || td.contains("TypeDataAnimTrail") { self.no_draw.insert(format!("{sys}/{}", e.props.get("EmitterName").and_then(|v| v.as_str()).unwrap_or(""))); n += 1; }
        }
        n
    }

    /// Starts a frame: view buffer, lights (unused by unlit materials) and the per-slot pools are reset.
    pub fn begin(&mut self, view: &ViewParams) {
        self.used = 0;
        self.touched.clear();
        self.static_shadow_written.clear(); // STATIC ITEM
        self.time = view.time;
        self.view = Some(*view);
        self.frame = FxFrame::default();
        self.frame.buffer_writes.push((self.view_buf, bytes(&view.view_buffer())));
        self.frame.buffer_writes.push((self.light_buf, bytes(&fn_render::sky_light::fx_light_block()))); // SKY LIGHT: sun + View.SkyIrradianceEnvironmentMap
        let (f, u) = (Vec3::new(view.forward.x, view.forward.z, view.forward.y), Vec3::new(view.up.x, view.up.z, view.up.y));
        let r = u.cross(f).normalize_or_zero();
        self.frame.buffer_writes.push((self.beam_vf, bytes(&[[r.x, r.y, r.z, 0.0], [u.x, u.y, u.z, 0.0], [0.0; 4]])));
    }

    pub fn override_light(&mut self, block: &[[f32; 4]; 10]) { self.frame.buffer_writes.push((self.light_buf, bytes(block))); }

    /// The material `path` for the vertex factory `kind`: loaded and built once (`Err` is remembered, so a failing material is reported once).
    pub fn material(&mut self, fx: &mut FxRenderer, path: &str, kind: Kind, num_uv: usize) -> Result<usize, String> {
        let key = (path.to_owned(), kind);
        if let Some(r) = self.index.get(&key) { return r.clone(); }
        let r = (|| -> Result<usize, String> {
            let mat = Material::load(&self.set, path)?;
            let (vf, layout, instanced) = match kind { Kind::Sprite => (Vf::Sprite, Some(sprite_vertex_buffers_dyn()), false), Kind::Mesh => (Vf::MeshParticle, Some(vertex_buffers(Vf::MeshParticle, num_uv, true)), true), Kind::Ribbon => (Vf::BeamTrail, Some(vertex_buffers(Vf::BeamTrail, 0, false)), false), Kind::HoloLocal | Kind::HoloSkin | Kind::StaticItem => return Err("hologram / static item materials are built by holo_entry / static_entry".into()) };
            // PARTICLE LIT PATH (2026-10-05): a lit particle material (the building debris M_Wood_Chips / M_Chunks_Base / M_PropRocks_shards_01, MSM_DefaultLit) takes the
            // sky-light permutation of its particle vertex factory, `TBasePassPSFNoLightMapPolicySkylight` (cooked in their shader maps; a lit material in a scene
            // with a sky light, `TBasePassPS<LightMapPolicyType, bEnableSkyLight>`, UE4.16 BasePassRendering.h:826), resolved by the FX pass' sun + sky term like the building pieces
            // (`building_render::PS_TYPES`); unlit ones keep `TBasePassPSFNoLightMapPolicy`. Was: lit materials refused (not drawn).
            // FX RECEIVE (2026-10-05): an opaque / masked lit one (GBuffer writer) is resolved by the scene lighting with the sky diffuse added there: it takes
            // `TBasePassPSFNoLightMapPolicy` first (movable sky light: not in the deferred opaque base pass, UE4.16 ScenePrivate.h:2269-2273; same rule as
            // building_render::material), the Skylight permutation only as the fallback
            let order = if mat.flags.uses_gbuffer() { [crate::building_render::PS_TYPES[1], crate::building_render::PS_TYPES[0]] } else { crate::building_render::PS_TYPES };
            let ps = if mat.flags.shading == "MSM_Unlit" { BASEPASS_PS } else {
                order.into_iter().find(|ps| mat.cooked.find(vf.type_name(), ps).is_some()).ok_or_else(|| format!("{path}: shading model {}: no {} / {:?} program", mat.flags.shading, vf.type_name(), crate::building_render::PS_TYPES))?
            };
            let gpu = build_material_layout(&self.set, fx, &mut self.cache, &mat, vf, BASEPASS_VS, ps, num_uv, instanced, self.shared, &[("PrimitiveFade", self.fade_buf), ("BeamTrailVF", self.beam_vf)], fn_fx::gpu::LayoutOpts { vertex_layout: layout, ..Default::default() })?;
            let line = format!("{} on {}: {} / {} blend {} shading {} two-sided {} depth-test {} ; vertex inputs {:?}, {} varyings, cooked uniform expression set {} vectors / {} scalars / {} textures, per-frame globals {}",
                path.rsplit('.').next().unwrap_or(path), vf.type_name(), BASEPASS_VS, ps, mat.flags.blend, mat.flags.shading, mat.flags.two_sided, !mat.flags.disable_depth_test,
                gpu.assembled.vs_inputs.iter().map(|i| i.attribute).collect::<Vec<_>>(), gpu.assembled.varyings.len(), mat.uniform.vectors.len(), mat.uniform.scalars.len(), mat.uniform.textures2d.len(), gpu.per_frame.is_some());
            eprintln!("[pickups-fx] DXBC {line}");
            self.log.push(line);
            self.entries.push(Entry { path: path.to_owned(), mat, gpu, kind });
            Ok(self.entries.len() - 1)
        })();
        self.index.insert(key, r.clone());
        r
    }
    pub fn entry_flags(&self, i: usize) -> &fn_fx::material::MaterialFlags { &self.entries[i].mat.flags }

    fn slot(&mut self, fx: &mut FxRenderer) -> usize {
        if self.used == self.pool.len() { self.pool.push(Slot { prim: fx.create_buffer("pickup primitive", 32 * 16), sprite_vf: fx.create_buffer("pickup sprite vf", 10 * 16), mat: fx.create_buffer("pickup material", 96 * 16) }); }
        self.used += 1;
        self.used - 1
    }
    fn binds_for(&mut self, fx: &mut FxRenderer, entry: usize, slot: usize) -> Vec<(u32, FxBindId)> {
        if let Some(b) = self.binds.get(&(entry, slot)) { return b.clone(); }
        let (prim, svf) = (self.pool[slot].prim, self.pool[slot].sprite_vf);
        let b = self.entries[entry].gpu.bind_variant_ext(fx, prim, None, Some(svf));
        self.binds.insert((entry, slot), b.clone());
        b
    }

    /// `FParticleSpriteUniformParameters` of an emitter draw (ParticleSystemRender.cpp:1258-1360), as float4 slots of the `SpriteVF` buffer
    /// (AxisLockRight, AxisLockUp, TangentSelector, NormalsSphereCenter, NormalsCylinderUnitDirection, SubImageSize, CameraFacingBlend + RemoveHMDRoll, MacroUV, RotationScale / RotationBias /
    /// NormalsType / InvDeltaSeconds, PivotOffset: member order of `ParticleVertexFactory.h:110`, verified against the slots the sprite programs read).
    pub fn sprite_uniform(d: &EmitterDraw, dt: f32) -> Vec<[f32; 4]> {
        let mut v = vec![[0f32; 4]; 10];
        let (mut rot_scale, mut rot_bias) = (1.0f32, 0.0f32);
        let mut sel = [0f32; 4];
        if d.alignment == Alignment::Velocity {
            // PSA_Velocity: no rotation, velocity tangents (UniformParameters.RotationScale = 0, TangentSelector.Y = 1)
            rot_scale = 0.0; sel[1] = 1.0;
        } else if d.axis_lock == AxisLock::None {
            sel[0] = 1.0; // PSA_Square / PSA_Rectangle / TypeSpecific: camera right / up (TangentSelector.X = 1)
        } else if let Some((up, right)) = d.axis_lock_axes {
            let (up, right) = (swap(up), swap(right));
            v[0] = [right[0], right[1], right[2], 1.0];
            v[1] = [up[0], up[1], up[2], 1.0];
            if matches!(d.axis_lock, AxisLock::RotateX | AxisLock::RotateY | AxisLock::RotateZ) { sel[2] = 1.0; } else { sel[0] = 1.0; }
            if d.axis_lock == AxisLock::RotateZ { rot_bias = ROTATE_Z_BIAS; }
        }
        v[2] = sel;
        v[3] = [0.0; 4];                                      // NormalsSphereCenter (ENM_CameraFacing: zero)
        v[4] = [0.0, 0.0, 1.0, 0.0];                          // NormalsCylinderUnitDirection default
        let (h, w) = (d.sub_images.0.max(1) as f32, d.sub_images.1.max(1) as f32);
        v[5] = [h, w, 1.0 / h, 1.0 / w];                      // SubImageSize
        v[6] = [0.0; 4];                                      // CameraFacingBlend (not PSA_FacingCameraDistanceBlend), RemoveHMDRoll 0
        v[8] = [rot_scale, rot_bias, 0.0, 1.0 / dt.max(1e-4)];
        v[9] = [DEFAULT_PIVOT[0] + d.pivot_offset[0], DEFAULT_PIVOT[1] + d.pivot_offset[1], 0.0, 0.0];
        v
    }

    /// `bounds` = the component's `Bounds` (= its `LocalBounds`, see `fn_fx::view::primitive_buffer`); world-space emitters use the identity transform
    /// (`UpdateWorldSpacePrimitiveUniformBuffer`, ParticleSystemRender.cpp:7154) with the same bounds.
    fn component_primitive(xf: &Transform, local: bool, bounds: &fn_render::primitive_ub::BoxSphereBounds) -> Vec<[f32; 4]> {
        if !local { return primitive_buffer(DVec3::ZERO, 1.0, bounds); }
        let s = xf.scale;
        let ax = |c: usize| { let a = xf.rot.col(c) * s; DVec3::new(a.x as f64, a.y as f64, a.z as f64) };
        primitive_buffer_local(DVec3::new(xf.pos.x as f64, xf.pos.y as f64, xf.pos.z as f64), [ax(0), ax(1), ax(2)], bounds)
    }
    /// `CalculateTranslucentSortKey` (TranslucentRendering.cpp:858) of a primitive with bounds origin `origin` for this frame's view, priority 0 (no pickup /
    /// building component sets `TranslucencySortPriority`).
    fn sort(&self, origin: [f32; 3]) -> Result<FxSort, String> { self.view.as_ref().map(|v| v.sort(origin)).ok_or_else(|| "no view (begin was not called)".to_owned()) }

    /// Draws one emitter of a particle system instance (`xf` = the instance's component transform, UE cm; `bounds` = the component's bounds,
    /// `particles::instance_bounds`) with its original material. `Err` = not drawn (nothing is faked).
    pub fn draw_emitter(&mut self, fx: &mut FxRenderer, d: &EmitterDraw, xf: &Transform, bounds: &fn_render::primitive_ub::BoxSphereBounds, dt: f32) -> Result<(), String> {
        if d.particles.is_empty() { return Ok(()); }
        if !d.package.is_empty() && !self.render_checked.contains(&d.package) { self.render_checked.insert(d.package.clone()); self.skip_non_normal_emitters(&d.package); self.warm_ribbon_materials(fx, &d.package); }
        if !self.no_draw.is_empty() {
            let mut k = std::mem::take(&mut self.key_buf);
            k.clear(); k.push_str(&d.system); k.push('/'); k.push_str(&d.emitter);
            let skip = self.no_draw.contains(&k);
            self.key_buf = k;
            if skip { return Ok(()); }
        }
        let sort = self.sort(bounds.origin)?;
        match d.kind { DrawKind::Sprite => self.draw_sprites(fx, d, xf, bounds, sort, dt), DrawKind::Mesh => self.draw_meshes(fx, d, xf, bounds, sort, dt), DrawKind::Ribbon => self.draw_ribbon(fx, d, xf, bounds, sort) }
    }

    fn draw_sprites(&mut self, fx: &mut FxRenderer, d: &EmitterDraw, xf: &Transform, bounds: &fn_render::primitive_ub::BoxSphereBounds, sort: FxSort, dt: f32) -> Result<(), String> {
        let m = self.material(fx, &d.material, Kind::Sprite, 0)?;
        let dtu = if dt > 0.0 { dt } else { REFERENCE_DT };
        let (mut vb, mut ib) = (Vec::with_capacity(d.particles.len() * 4 * SPRITE_DYN_VERTEX_STRIDE as usize), Vec::with_capacity(d.particles.len() * 6));
        let blend_sub = matches!(d.interpolation, SubUvInterp::LinearBlend | SubUvInterp::RandomBlend);
        let inv_rot = xf.rot.transpose();
        for (n, p) in d.particles.iter().enumerate() {
            let world = fn_particles::sim::game_to_ue(p.pos);
            let vel = fn_particles::sim::game_to_ue(p.velocity);
            let (pos, old) = if d.use_local_space { (inv_rot * (world - xf.pos) / xf.scale, inv_rot * (world - vel * dtu - xf.pos) / xf.scale) } else { (world, world - vel * dtu) };
            let size = [p.size[0] * 100.0, p.size[1] * 100.0];
            let sub = if blend_sub { p.sub_image } else { p.sub_image.floor() };
            for uv in [[0f32, 0.], [0., 1.], [1., 1.], [1., 0.]] {
                put(&mut vb, &[pos.x, pos.y, pos.z, p.rel_time, old.x, old.y, old.z, n as f32, size[0], size[1], p.rotation, sub]);
                put(&mut vb, &p.color);
                put(&mut vb, &uv);
                put(&mut vb, &p.dynamic);
            }
            let base = (n * 4) as u32;
            ib.extend([base, base + 2, base + 3, base, base + 1, base + 2]); // GParticleIndexBuffer order, UE4.16 ParticleResources.cpp:80-85
        }
        let slot = self.slot(fx);
        self.frame.buffer_writes.push((self.pool[slot].prim, bytes(&Self::component_primitive(xf, d.use_local_space, bounds))));
        self.frame.buffer_writes.push((self.pool[slot].sprite_vf, bytes(&Self::sprite_uniform(d, dtu))));
        let binds = self.binds_for(fx, m, slot);
        self.touched.insert(m);
        self.frame.push(FxDraw { pipe: self.entries[m].gpu.pipe, binds, geom: FxGeom::Dynamic { vertices: vb, indices: ib }, instances: None, sort });
        Ok(())
    }

    fn draw_ribbon(&mut self, fx: &mut FxRenderer, d: &EmitterDraw, xf: &Transform, bounds: &fn_render::primitive_ub::BoxSphereBounds, sort: FxSort) -> Result<(), String> {
        if d.ribbon_indices.is_empty() { return Ok(()); }
        let m = self.material(fx, &d.material, Kind::Ribbon, 0)?;
        let mut k = std::mem::take(&mut self.key_buf);
        k.clear(); k.push_str("ribbon "); k.push_str(&d.system); k.push('/'); k.push_str(&d.emitter);
        let first = !self.render_checked.contains(&k);
        if first { self.render_checked.insert(k.clone()); }
        self.key_buf = k;
        if first {
            let v = &d.ribbon_vertices;
            let n = v.len() / fn_particles::sim::RIBBON_VERTEX_FLOATS;
            let w = (0..n).map(|i| v[i * fn_particles::sim::RIBBON_VERTEX_FLOATS + 8]).fold(0f32, f32::max);
            let line = format!("ribbon {}/{}: {} particles, {} vertices, max half width {:.2} cm, head at ({:.0}, {:.0}, {:.0})", d.system, d.emitter, d.particles.len(), n, w, v[0], v[1], v[2]);
            eprintln!("[pickups-fx] {line}"); self.log.push(line);
        }
        let mut vb = Vec::with_capacity(d.ribbon_vertices.len() * 4);
        put(&mut vb, &d.ribbon_vertices);
        let slot = self.slot(fx);
        self.frame.buffer_writes.push((self.pool[slot].prim, bytes(&Self::component_primitive(xf, false, bounds))));
        let binds = self.binds_for(fx, m, slot);
        self.touched.insert(m);
        self.frame.push(FxDraw { pipe: self.entries[m].gpu.pipe, binds, geom: FxGeom::Dynamic { vertices: vb, indices: d.ribbon_indices.clone() }, instances: None, sort });
        Ok(())
    }

    fn draw_meshes(&mut self, fx: &mut FxRenderer, d: &EmitterDraw, xf: &Transform, bounds: &fn_render::primitive_ub::BoxSphereBounds, sort: FxSort, dt: f32) -> Result<(), String> {
        let mesh_path = d.mesh.clone();
        if !self.meshes.contains_key(&mesh_path) {
            let r = load_mesh(&self.set, fx, Vf::MeshParticle, &mesh_path);
            self.meshes.insert(mesh_path.clone(), r);
        }
        let (mesh_id, num_uv, sections, slot_paths) = match &self.meshes[&mesh_path] { Ok(m) => (m.id, m.num_uv, m.sections.clone(), m.material_paths.clone()), Err(e) => return Err(e.clone()) };
        let dtu = if dt > 0.0 { dt } else { REFERENCE_DT };
        let mut inst = Vec::with_capacity(d.particles.len() * INSTANCE_STRIDE as usize);
        for p in &d.particles {
            let pos = fn_particles::sim::game_to_ue(p.pos);
            let vel = fn_particles::sim::game_to_ue(p.velocity);
            let sz = [p.size[0], p.size[2], p.size[1]];
            // `orient` = the columns in GAME axis order (X, Y = UE Z, Z = UE Y), see fn_particles::sim `col`: UE local X, Y, Z are orient[0], orient[2], orient[1]
            let ax: Vec<[f32; 3]> = (0..3).map(|i| { let a = swap(p.orient[[0, 2, 1][i]]); [a[0] * sz[i], a[1] * sz[i], a[2] * sz[i]] }).collect();
            for r in 0..3 { put(&mut inst, &[ax[0][r], ax[1][r], ax[2][r], [pos.x, pos.y, pos.z][r]]); }          // Transform1..3: transposed matrix, position in W
            for _ in 0..4 { inst.extend(0i32.to_le_bytes()); }                                                       // SubUVParams
            put(&mut inst, &[0.0, p.rel_time, 0.0, 0.0]);                                                            // SubUVLerp, RelativeTime
            put(&mut inst, &p.dynamic);                                                                              // DynamicParameter
            put(&mut inst, &p.color);                                                                                // ParticleColor
            let sp = vel.length();
            let dir = if sp > 1e-4 { vel / sp } else { Vec3::ZERO };
            put(&mut inst, &[dir.x, dir.y, dir.z, sp]);                                                              // ParticleVelocity: direction, speed
        }
        let _ = dtu;
        let slot = self.slot(fx);
        self.frame.buffer_writes.push((self.pool[slot].prim, bytes(&Self::component_primitive(xf, d.use_local_space, bounds))));
        for (sec_slot, first, count) in sections {
            // material of the section: the MeshMaterial module's list, else the Required module's material when bOverrideMaterial, else the mesh's own slot material
            let path = d.material_override.get(sec_slot).cloned().filter(|s| !s.is_empty())
                .or_else(|| if d.mesh_override_material && !d.material.is_empty() { Some(d.material.clone()) } else { None })
                .or_else(|| slot_paths.get(sec_slot).cloned().flatten())
                .ok_or_else(|| format!("{mesh_path}: section material slot {sec_slot} has no material"))?;
            let m = self.material(fx, &path, Kind::Mesh, num_uv)?;
            let binds = self.binds_for(fx, m, slot);
            self.touched.insert(m);
            self.frame.push(FxDraw { pipe: self.entries[m].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh: mesh_id, first_index: first, index_count: count }, instances: Some(FxInstances { count: d.particles.len() as u32, bytes: inst.clone() }), sort });
        }
        Ok(())
    }

    /// The finished frame: the draws plus this frame's `Material` constant buffers (time dependent expressions) and per-frame globals of the materials used.
    pub fn take_frame(&mut self) -> FxFrame {
        let mut f = std::mem::take(&mut self.frame);
        // every material-owned globals buffer (both stages) at the view's world / real / delta times (FMaterialShader::SetParameters, ShaderBaseClasses.cpp:287),
        // not the pixel-only per_frame_write with one clock for both
        let t = self.view.as_ref().map_or(fn_fx::uniform::MaterialTime::single(self.time), |v| v.material_time());
        for &i in &self.touched {
            let e = &self.entries[i];
            f.buffer_writes.push((e.gpu.material_buf, bytes(&e.mat.packed(self.time).cb)));
            f.buffer_writes.extend(e.gpu.per_frame_writes(&e.mat, t));
        }
        f
    }
    pub fn material_names(&self) -> Vec<String> { self.entries.iter().map(|e| format!("{} ({:?})", e.path, e.kind)).collect() }
}

// ---- hologram ---------------------------------------------------------------------------------------------------------------------------
// The hologram of `B_Pickups` (`/Game/Effects/Fort_Effects/Gameplay/Pickups/B_Pickups`, Kismet read with `fn-inspect disasm`): `UserConstructionScript` adds a second mesh component
// (`NODE_AddSkeletalMeshComponent-1` / `NODE_AddStaticMeshComponent-1`, same mesh as the item, attached to the item's mesh component so it has the same transform), `CreateHologramMIDs`
// gives every material slot a `MaterialInstanceDynamic` of `Hologram Material` (the CDO: `M_Pickup_Blueprint_01`, additive, unlit, two sided) with the parameters below, the component is
// visible from construction (nothing in the graph un-hides it; `SetHiddenInGame(true)` happens after the pickup animation) and culled beyond `SetCullDistance` of `OnTossed`.
pub const HOLOGRAM_MATERIAL: &str = "/Game/Effects/Fort_Effects/Gameplay/Pickups/Materials/M_Pickup_Blueprint_01.M_Pickup_Blueprint_01"; // Default__B_Pickups_C."Hologram Material"
/// `Default__B_Pickups_C."Empty Normal"`: the `Normals` of a slot whose item material has no `Normals` texture parameter (CreateHologramMIDs: `switch(IsValid(texture)){false: Empty Normal}`).
pub const EMPTY_NORMAL: &str = "/Game/Packages/Fortnite_UniqueMaterials_1/T_Normal.T_Normal";
/// `Default__B_Pickups_C."Sweep Brightness"` / `"Sweep Speed"` by rarity level (CreateHologramMIDs: `Array_Get(Sweep Brightness, Rarity Level)` -> MID `Sweep Brightness`, `Sweep Speed`).
pub const SWEEP_BRIGHTNESS: [f32; 10] = [0.0, 0.4, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
pub const SWEEP_SPEED: [f32; 10] = [0.0, 0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85, 0.95, 1.05];
/// `Random` of every MID = `RandomFloatInRange(0, 10)` (CreateHologramMIDs bytecode constants f0, f10).
pub const RANDOM_RANGE: f32 = 10.0;
/// `Default__B_Pickups_C`: `Picked Up Fade In Time`, `Picked Up Hold Time`, `Picked Up Fade Out Time`, `Picked Up Sweep Speed`; the `+ f0.05` of the hide `Delay` is a bytecode constant of ExecuteUbergraph_B_Pickups.
pub const PICKED_FADE_IN: f32 = 0.11;
pub const PICKED_HOLD: f32 = 0.15;
pub const PICKED_FADE_OUT: f32 = 0.1;
pub const PICKED_SWEEP_SPEED: f32 = 2.05;
pub const PICKED_HIDE_PAD: f32 = 0.05;
/// `OnTossed`: `SetCullDistance(Divide(select(IsAthenaGameState, 1300, 4000), GetNumericCVar("r.ViewDistanceScale")))` (Battle Royale = `FortGameStateAthena` -> 4000 cm; the cvar is 1 at the default scalability).
pub const HOLOGRAM_CULL_CM: f32 = 4000.0;
/// Identity `BoneMatrices` rows (3 float4 per bone) of a skeletal mesh at its reference pose: the Blueprint's `SkeletalMeshComponent` has no animation, `RefBasesInvMatrix * RefPose = I`.
pub const BONE_FLOAT4S: usize = 768;

/// The mesh of an item as the hologram draws it (the item's own `PickupSkeletalMesh` / `PickupStaticMesh`).
pub struct HoloMesh { skin: bool, mesh: FxMeshId, num_uv: usize, sections: Vec<(usize, u32, u32)>, slot_paths: Vec<Option<String>>, sphere: ([f32; 3], f32), /// `LocalObjectBoundsMin` / `Max` of `FPrimitiveUniformShaderParameters` (mesh bounds box, local cm; Primitive float4 15 and 16).
    bounds: ([f32; 3], [f32; 3]), bones: Option<FxBufId>,
    /// GPU skin: the vertex program's loose parameters (its unnamed buffer 0): `MeshOrigin` (float4 6) / `MeshExtension` (float4 7) of the mesh's vertex buffer, which the program
    /// applies to every position (`mad r0.xyz, v0.xyz, cb0[7].xyz, cb0[6].xyz` of the cooked TGPUSkinVertexFactoryfalse base-pass VS; UE4.16 SkeletalRenderGPUSkin.cpp:903).
    /// Left unbound (the zero buffer) every vertex collapsed onto the origin and a skeletal item's hologram drew nothing.
    vs_globals: Option<FxBufId>,
    /// STATIC ITEM: LOD 0 of the static mesh has a cooked distance field (`fn_fx::gpu::MeshGpu::has_distance_field`); always false for a skeletal mesh.
    has_df: bool }
/// Float4s of the GPU-skin base-pass vertex program's unnamed buffer (`dcl_constantbuffer CB0[16]`, M_Pickup_Blueprint_01 / TGPUSkinVertexFactoryfalse / TBasePassVSFNoLightMapPolicy).
pub const SKIN_VS_GLOBALS_FLOAT4S: usize = 16;

/// The state of one pickup's hologram (`B_Pickups` variables + the MIDs it created).
pub struct HoloActor { pub rarity: u8, pub random: f32, pub normals: Vec<String>, pub picked_at: Option<f32> }

impl Dxbc {
    /// Loads the displayed mesh of an item for the hologram (cached by path).
    pub fn holo_mesh(&mut self, fx: &mut FxRenderer, path: &str, skeletal: bool) -> Result<(), String> {
        if self.holo.contains_key(path) { return self.holo[path].as_ref().map(|_| ()).map_err(|e| e.clone()); }
        let r = (|| -> Result<HoloMesh, String> {
            if skeletal {
                let long = fn_fx::data::long_package(path);
                let (a, x, b) = fn_fx::data::package_files(&self.set, long)?;
                let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
                let sm = fn_mesh::SkeletalMesh::parse(&p).map_err(|e| format!("{long}: {e}"))?;
                let g = fn_matshader::mesh::GunMeshData::from_skeletal(&sm)?;
                let mesh = fx.create_mesh(&FxMeshDesc { label: g.name.clone(), vertices: g.vertices.clone(), indices: g.indices.clone() });
                let mut ident = vec![[0f32; 4]; BONE_FLOAT4S];
                for b in 0..BONE_FLOAT4S / 3 { ident[3 * b] = [1., 0., 0., 0.]; ident[3 * b + 1] = [0., 1., 0., 0.]; ident[3 * b + 2] = [0., 0., 1., 0.]; }
                let bones = fx.create_buffer("pickup hologram identity bones", BONE_FLOAT4S * 16);
                fx.write_buffer(bones, &bytes(&ident));
                let sections = g.sections.iter().map(|s| (s.material_index, s.first_index, s.index_count)).collect();
                let mut globals = vec![[0f32; 4]; SKIN_VS_GLOBALS_FLOAT4S];
                globals[6] = [g.mesh_origin[0], g.mesh_origin[1], g.mesh_origin[2], 0.0];
                globals[7] = [g.mesh_extension[0], g.mesh_extension[1], g.mesh_extension[2], 0.0];
                let vs_globals = fx.create_buffer("pickup hologram skin VF globals", SKIN_VS_GLOBALS_FLOAT4S * 16);
                fx.write_buffer(vs_globals, &bytes(&globals));
                Ok(HoloMesh { vs_globals: Some(vs_globals), skin: true, mesh, num_uv: 1, sections, slot_paths: sm.materials.iter().map(|m| m.material.clone()).collect(), sphere: (g.bounds.origin, g.bounds.radius), bounds: { let b = &sm.bounds; (std::array::from_fn(|k| b.origin[k] - b.extent[k]), std::array::from_fn(|k| b.origin[k] + b.extent[k])) }, bones: Some(bones), has_df: false })
            } else {
                let g = load_mesh(&self.set, fx, Vf::Local, path)?;
                // cooked bounds (UStaticMesh::GetBounds, fn_fx::gpu::MeshGpu::bounds): sphere radius and box as the engine has them, not the vertex box / half diagonal
                Ok(HoloMesh { skin: false, mesh: g.id, num_uv: g.num_uv, sections: g.sections.clone(), slot_paths: g.material_paths.clone(), sphere: (g.bounds.origin, g.bounds.radius), bounds: (g.bounds.min(), g.bounds.max()), bones: None, vs_globals: None, has_df: g.has_distance_field })
            }
        })();
        self.holo.insert(path.to_owned(), r);
        self.holo[path].as_ref().map(|_| ()).map_err(|e| e.clone())
    }

    /// `Normals` texture of the item material in each slot (CreateHologramMIDs: the item's material instance's `Normals` texture parameter, `Empty Normal` when it has none).
    pub fn holo_normals(&mut self, path: &str) -> Vec<String> {
        if let Some(n) = self.holo_normals.get(path) { return n.clone(); }
        let Some(Ok(m)) = self.holo.get(path) else { return vec![] };
        let slots = m.slot_paths.clone();
        let n: Vec<String> = slots.iter().map(|p| {
            let found = p.as_ref().and_then(|p| Material::load(&self.set, p).ok()).and_then(|mat| {
                if let Some(t) = mat.texture_overrides.get("Normals") { return Some(t.clone()); }
                mat.uniform.textures2d.iter().find_map(|e| if let fn_fx::uniform::Expr::TextureParameter { name, index, .. } = e { if name == "Normals" { mat.referenced_textures.get(*index as usize).cloned() } else { None } } else { None })
            });
            found.unwrap_or_else(|| EMPTY_NORMAL.to_owned())
        }).collect();
        self.holo_normals.insert(path.to_owned(), n.clone());
        n
    }

    fn holo_entry(&mut self, fx: &mut FxRenderer, mesh_path: &str, skin: bool, normals: &str, bones: Option<FxBufId>, vs_globals: Option<FxBufId>, num_uv: usize) -> Result<usize, String> {
        // a skin pipeline binds its mesh's own vertex-factory globals: one entry per mesh
        let key = (if skin { format!("holo|{normals}|{mesh_path}") } else { format!("holo|{normals}") }, if skin { Kind::HoloSkin } else { Kind::HoloLocal });
        if let Some(r) = self.index.get(&key) { return r.clone(); }
        let r = (|| -> Result<usize, String> {
            let mut mat = Material::load(&self.set, HOLOGRAM_MATERIAL)?;
            mat.texture_overrides.insert("Normals".to_owned(), normals.to_owned());
            if std::env::var_os("FNRT_PICKUP_HOLO_NODEPTH").is_some() { mat.flags.disable_depth_test = true; }
            let vf = if skin { Vf::Skin } else { Vf::Local };
            let gpu = build_material_layout(&self.set, fx, &mut self.cache, &mat, vf, BASEPASS_VS, BASEPASS_PS, num_uv, false, self.shared, &[("PrimitiveFade", self.fade_buf)], LayoutOpts { vertex_storage: bones, vs_globals, ..Default::default() })?;
            let line = format!("{} on {}: blend {} two-sided {} ; vertex inputs {:?}, per-frame globals {}", HOLOGRAM_MATERIAL.rsplit('.').next().unwrap(), vf.type_name(), mat.flags.blend, mat.flags.two_sided, gpu.assembled.vs_inputs.iter().map(|i| i.attribute).collect::<Vec<_>>(), gpu.per_frame.is_some());
            eprintln!("[pickups-fx] DXBC {line}");
            self.log.push(line);
            self.entries.push(Entry { path: HOLOGRAM_MATERIAL.to_owned(), mat, gpu, kind: if skin { Kind::HoloSkin } else { Kind::HoloLocal } });
            Ok(self.entries.len() - 1)
        })();
        self.index.insert(key, r.clone());
        r
    }

    /// The `SetScalarParameterValue` / `SetVectorParameterValue` calls of `CreateHologramMIDs` and `SetHologramPickedUpParams`, as the overrides of the MID.
    pub fn holo_overrides(actor: &HoloActor) -> fn_fx::uniform::Overrides {
        let r = (actor.rarity as usize).min(9);
        let mut ov = fn_fx::uniform::Overrides::default();
        ov.vectors.insert("Color".into(), crate::pickups_fx::OUTLINE_RARITY_COLORS[r]);
        ov.scalars.insert("Random".into(), actor.random);
        ov.scalars.insert("Sweep Brightness".into(), SWEEP_BRIGHTNESS[r]);
        ov.scalars.insert("Sweep Speed".into(), SWEEP_SPEED[r]);
        if let Some(t) = actor.picked_at {
            ov.scalars.insert("Random".into(), -(t - t.floor()));
            if actor.rarity == 0 {
                // SetHologramPickedUpParams(true): TimestampB = now, TransitionLength_Out = Picked Up Fade Out Time, Sweep Speed = 0
                ov.scalars.insert("TimestampB".into(), t); ov.scalars.insert("TransitionLength_Out".into(), PICKED_FADE_OUT); ov.scalars.insert("Sweep Speed".into(), 0.0);
            } else {
                // SetHologramPickedUpParams(false): TimestampA = now, TransitionLength_In = Picked Up Fade In Time, TimestampB = now + Hold + Fade In, TransitionLength_Out = Fade Out, Sweep Speed = Picked Up Sweep Speed
                ov.scalars.insert("TimestampA".into(), t); ov.scalars.insert("TransitionLength_In".into(), PICKED_FADE_IN);
                ov.scalars.insert("TimestampB".into(), t + PICKED_HOLD + PICKED_FADE_IN); ov.scalars.insert("TransitionLength_Out".into(), PICKED_FADE_OUT); ov.scalars.insert("Sweep Speed".into(), PICKED_SWEEP_SPEED);
            }
        }
        ov
    }
    /// Seconds after the pickup at which the Blueprint hides the hologram (`SetHiddenInGame(true)` after the `Delay`).
    pub fn holo_hide_after(rarity: u8) -> f32 { if rarity == 0 { PICKED_FADE_OUT } else { PICKED_FADE_IN + PICKED_HOLD + PICKED_FADE_OUT + PICKED_HIDE_PAD } }

    /// Draws the hologram of one pickup: `pos_ue` = actor location (UE cm), `rot` = actor rotation, `origin_cm` = `Bounds.Origin` of the mesh (OnTossed moves the mesh so that this point is at the actor location).
    pub fn draw_hologram(&mut self, fx: &mut FxRenderer, mesh_path: &str, actor: &HoloActor, pos_ue: Vec3, rot: glam::Mat3, origin_cm: [f32; 3], eye_ue: Vec3) -> Result<(), String> {
        let (skin, mesh, num_uv, sections, bones, sphere, bounds, vs_globals) = match self.holo.get(mesh_path) { Some(Ok(m)) => (m.skin, m.mesh, m.num_uv, m.sections.clone(), m.bones, m.sphere, m.bounds, m.vs_globals), Some(Err(e)) => return Err(e.clone()), None => return Err("hologram mesh not loaded".into()) };
        let now = self.time;
        if let Some(t) = actor.picked_at { if now - t > Self::holo_hide_after(actor.rarity) { return Ok(()); } }
        if (pos_ue - eye_ue).length() > HOLOGRAM_CULL_CM { return Ok(()); }
        let origin = Vec3::from(origin_cm);
        let l2w = glam::Mat4::from_translation(pos_ue - rot * origin) * glam::Mat4::from_mat3(rot);
        // shared UE4.16 Primitive builder (fn_render::primitive_ub, 18 float4s: flags, orientation, scales, local bounds 15 / 16, LightingChannelMask, LpvBias) from the
        // mesh box + its sphere radius, ActorWorldPosition = the actor location; replaces the old 16-float4 copy + resize(17) (which dropped LpvBias and zeroed the channel mask)
        let local_bounds = fn_render::primitive_ub::BoxSphereBounds { origin: std::array::from_fn(|k| (bounds.0[k] + bounds.1[k]) * 0.5), extent: std::array::from_fn(|k| (bounds.1[k] - bounds.0[k]) * 0.5), radius: sphere.1 };
        let prim = fn_render::primitive_ub::mesh_primitive(&l2w, &local_bounds, pos_ue.to_array(), &fn_render::primitive_ub::PrimitiveFlags::default());
        if std::env::var_os("FNRT_PICKUP_DEBUG").is_some() { eprintln!("[pickups-fx] hologram {mesh_path}: {} sections, actor at {:?}, origin {:?}, l2w translation {:?}, sphere {:?}, now {now:.2}", sections.len(), pos_ue, origin_cm, l2w.w_axis, sphere); }
        for (slot_i, first, count) in sections {
            let Some(normals) = actor.normals.get(slot_i).cloned() else { continue };
            let e = self.holo_entry(fx, mesh_path, skin, &normals, bones, vs_globals, num_uv)?;
            let ov = { let mut o = self.entries[e].mat.overrides.clone(); let h = Self::holo_overrides(actor); o.vectors.extend(h.vectors); o.scalars.extend(h.scalars); o };
            let packed = fn_fx::uniform::evaluate(&self.entries[e].mat.uniform, &ov, now);
            let slot = self.slot(fx);
            self.frame.buffer_writes.push((self.pool[slot].prim, bytes(&prim)));
            let regs = packed.cb.len().max(self.entries[e].gpu.material_registers);
            let mut cb = packed.cb; cb.resize(regs, [0.0; 4]);
            self.frame.buffer_writes.push((self.pool[slot].mat, bytes(&cb)));
            let (pb, mb) = (self.pool[slot].prim, self.pool[slot].mat);
            // FIRST USE (2026-10-06): one bind group set per (entry, pool slot), as `binds_for`: it was created again every frame for every hologram section
            // (FX bind groups are never freed, so the table grew by a few per pickup per frame)
            let binds = match self.binds.get(&(e, slot)) { Some(b) => b.clone(), None => { let b = self.entries[e].gpu.bind_variant_ext(fx, pb, Some(mb), None); self.binds.insert((e, slot), b.clone()); b } };
            // the hologram mesh component's Bounds = its mesh bounds moved by its transform (the translucency sort origin, TranslucentRendering.cpp:864)
            let sort = self.sort(local_bounds.transform_by(&l2w).origin)?;
            self.frame.push(FxDraw { pipe: self.entries[e].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, instances: None, sort });
        }
        Ok(())
    }
}

// ---- static items -----------------------------------------------------------------------------------------------------------------------
// STATIC ITEM (2026-10-05, spec pickup_visual_gaps.md section 4 "Ammo, consumables and resources"): the item mesh of a `PickupStaticMesh` item drawn like any static
// mesh in 1.8, with its OWN cooked materials. `B_Pickups`' construction script adds the item's `StaticMeshComponent` (`NODE_AddStaticMeshComponent-0`) with no
// override material (only the hologram component gets the `Hologram Material` MIDs, `CreateHologramMIDs`), so every section draws the mesh's slot material: the
// `FLocalVertexFactory` base pass `TBasePassVSFNoLightMapPolicy` + `TBasePassPSFNoLightMapPolicy` (movable, no lightmap and no precomputed lighting in Athena;
// the movable sky light is resolved after the opaque base pass, ScenePrivate.h:2269-2273), fallback the `...Skylight` PS, exactly as the building pieces
// (`building_render::material`); an opaque / masked lit material is resolved by the renderer's scene lighting (`assemble_scene_lit`: sun, CSM + DF shadows,
// light function, DFAO-occluded sky diffuse, sky reflection, fog). The component casts (CastShadow / bCastDynamicShadow defaults true, MeshComponent.cpp:16 /
// PrimitiveComponent.cpp:171; the template serializes no override, see section 7) into the CSM cascades with the material's cooked `TShadowDepthVS` / PS (or
// the default material's position-only VS, `fn_fx::gpu::shadow_programs`) and joins the distance-field scene (`bAffectDistanceFieldLighting` default true,
// PrimitiveComponent.cpp:173, `HasDistanceFieldRepresentation`, StaticMeshRender.cpp:1374). Transform = the hologram's (the hologram component is attached to
// the item mesh with the same transform): actor location + yaw, the mesh's `Bounds.Origin` moved onto the actor (`OnTossed`).
impl Dxbc {
    /// The section material `path` of a static item for the local vertex factory (built once; `Err` remembered).
    fn static_entry(&mut self, fx: &mut FxRenderer, path: &str, num_uv: usize) -> Result<usize, String> {
        let key = (format!("static|{path}|{num_uv}"), Kind::StaticItem);
        if let Some(r) = self.index.get(&key) { return r.clone(); }
        let r = (|| -> Result<usize, String> {
            let mat = Material::load(&self.set, path)?;
            let ps_types = crate::building_render::PS_TYPES;
            let order = if mat.flags.uses_gbuffer() { [ps_types[1], ps_types[0]] } else { ps_types };
            let ps = order.into_iter().find(|ps| mat.cooked.find(Vf::Local.type_name(), ps).is_some()).ok_or_else(|| format!("{path}: no {} / {:?} program", Vf::Local.type_name(), ps_types))?;
            let gpu = build_material_layout(&self.set, fx, &mut self.cache, &mat, Vf::Local, BASEPASS_VS, ps, num_uv, false, self.shared, &[("PrimitiveFade", self.fade_buf)], LayoutOpts::default())?;
            let line = format!("static item material {} (shader map of {}): {BASEPASS_VS} / {ps}, blend {} shading {} two-sided {}, {}",
                path.rsplit('.').next().unwrap_or(path), mat.shader_map_package.rsplit('/').next().unwrap_or(""), mat.flags.blend, mat.flags.shading, mat.flags.two_sided,
                if mat.flags.uses_gbuffer() && fx.scene_lit_prelude().is_some() { "scene-lit (renderer lighting + shadows)" } else { "FX pass" });
            eprintln!("[pickups] {line}");
            self.log.push(line);
            self.entries.push(Entry { path: path.to_owned(), mat, gpu, kind: Kind::StaticItem });
            Ok(self.entries.len() - 1)
        })();
        self.index.insert(key, r.clone());
        r
    }
    /// The shadow-depth programs of static-item entry `e` (built once; `None` = casts nothing: translucent / unlit, or not buildable, logged).
    fn static_shadow(&mut self, fx: &mut FxRenderer, e: usize, num_uv: usize) {
        if self.static_shadows.contains_key(&e) { return; }
        let needs_default = self.entries[e].mat.flags.blend == "BLEND_Opaque" && self.entries[e].mat.cooked.find(Vf::Local.type_name(), fn_fx::gpu::SHADOW_VS).is_none();
        if needs_default && self.default_mat.is_none() {
            let d = Material::load(&self.set, fn_fx::gpu::DEFAULT_SURFACE_MATERIAL);
            if let Err(err) = &d { let m = format!("{}: {err}", fn_fx::gpu::DEFAULT_SURFACE_MATERIAL); eprintln!("[pickups] {m}"); self.log.push(m); }
            self.default_mat = Some(d.ok());
        }
        let r = fn_fx::gpu::build_shadow(&self.set, fx, &mut self.cache, &self.entries[e].mat, self.default_mat.as_ref().and_then(|d| d.as_ref()), Vf::Local, num_uv, self.shared, &[("PrimitiveFade", self.fade_buf)], false);
        let name = self.entries[e].path.rsplit('.').next().unwrap_or("").to_owned();
        let v = match r {
            Ok(Some((gpu, own))) => { let m = format!("static item material {name}: shadow depth with {}", if own { "its own TShadowDepthVS / PS" } else { "the default material's position-only VS" }); eprintln!("[pickups] {m}"); self.log.push(m); Some((gpu, own)) }
            Ok(None) => None,
            Err(err) => { let m = format!("static item material {name}: shadow depth NOT built ({err}): casts no shadow"); eprintln!("[pickups] {m}"); self.log.push(m); None }
        };
        self.static_shadows.insert(e, v);
    }
    /// Loads the static mesh `mesh_path` and builds every section's material; `Ok` = [`Self::draw_static_item`] draws it (the caller then hides its renderer model).
    pub fn static_ready(&mut self, fx: &mut FxRenderer, mesh_path: &str) -> Result<(), String> {
        if let Some(r) = self.static_ok.get(mesh_path) { return r.clone(); }
        let r = (|| -> Result<(), String> {
            self.holo_mesh(fx, mesh_path, false)?;
            let (num_uv, sections, slots) = match self.holo.get(mesh_path) { Some(Ok(m)) => (m.num_uv, m.sections.clone(), m.slot_paths.clone()), _ => return Err("mesh not loaded".into()) };
            for (slot_i, _, _) in sections {
                let p = slots.get(slot_i).cloned().flatten().ok_or_else(|| format!("{mesh_path}: section material slot {slot_i} has no material"))?;
                let e = self.static_entry(fx, &p, num_uv)?;
                if !self.entries[e].gpu.translucent && !self.no_shadows { self.static_shadow(fx, e, num_uv); }
            }
            Ok(())
        })();
        let line = match &r { Ok(()) => format!("{mesh_path}: static item drawn with its cooked materials"), Err(e) => format!("{mesh_path}: cooked materials NOT available, the renderer model is drawn: {e}") };
        eprintln!("[pickups] {line}");
        self.log.push(line);
        self.static_ok.insert(mesh_path.to_owned(), r.clone());
        r
    }
    /// Draws one static item (see the section comment): `pos_ue` = actor location (UE cm), `rot` = actor rotation, `origin_cm` = the mesh's `Bounds.Origin`.
    pub fn draw_static_item(&mut self, fx: &mut FxRenderer, mesh_path: &str, pos_ue: Vec3, rot: glam::Mat3, origin_cm: [f32; 3]) -> Result<(), String> { self.draw_static_item_with(fx, mesh_path, pos_ue, rot, origin_cm, None) }
    pub fn static_item_df(&mut self, mesh_path: &str, pos_ue: Vec3, rot: glam::Mat3, origin_cm: [f32; 3]) {
        if self.no_shadows { return; }
        let has_df = matches!(self.holo.get(mesh_path), Some(Ok(m)) if m.has_df);
        if !has_df { return; }
        let Some(Some(df)) = self.static_dfs.get(mesh_path).cloned() else { return };
        let l2w = glam::Mat4::from_translation(pos_ue - rot * Vec3::from(origin_cm)) * glam::Mat4::from_mat3(rot);
        self.frame.distance_field_objects.push(FxDfInstance { mesh: df, local_to_world: l2w });
    }
    pub fn draw_static_item_with(&mut self, fx: &mut FxRenderer, mesh_path: &str, pos_ue: Vec3, rot: glam::Mat3, origin_cm: [f32; 3], black_hole: Option<[f32; 4]>) -> Result<(), String> {
        self.static_ready(fx, mesh_path)?;
        let (mesh, num_uv, sections, slot_paths, sphere, bounds, has_df) = match self.holo.get(mesh_path) { Some(Ok(m)) => (m.mesh, m.num_uv, m.sections.clone(), m.slot_paths.clone(), m.sphere, m.bounds, m.has_df), _ => return Err("mesh not loaded".into()) };
        let Some(view) = self.view else { return Err("no view (begin was not called)".into()) };
        let l2w = glam::Mat4::from_translation(pos_ue - rot * Vec3::from(origin_cm)) * glam::Mat4::from_mat3(rot);
        let local_bounds = fn_render::primitive_ub::BoxSphereBounds { origin: std::array::from_fn(|k| (bounds.0[k] + bounds.1[k]) * 0.5), extent: std::array::from_fn(|k| (bounds.1[k] - bounds.0[k]) * 0.5), radius: sphere.1 };
        let flags = fn_render::primitive_ub::PrimitiveFlags { has_distance_field: has_df, ..Default::default() };
        let prim = fn_render::primitive_ub::mesh_primitive(&l2w, &local_bounds, pos_ue.to_array(), &flags);
        let sort = self.sort(local_bounds.transform_by(&l2w).origin)?;
        let t = view.material_time();
        for (slot_i, first, count) in sections {
            let Some(path) = slot_paths.get(slot_i).cloned().flatten() else { continue };
            let e = self.static_entry(fx, &path, num_uv)?;
            let packed = match black_hole {
                None => fn_fx::uniform::evaluate(&self.entries[e].mat.uniform, &self.entries[e].mat.overrides, self.time),
                Some(v) => {
                    let mut ov = fn_fx::uniform::Overrides { vectors: self.entries[e].mat.overrides.vectors.clone(), scalars: self.entries[e].mat.overrides.scalars.clone() };
                    ov.vectors.insert(crate::pickup_orig::BLACK_HOLE_PARAM.to_owned(), v);
                    let with = fn_fx::uniform::evaluate(&self.entries[e].mat.uniform, &ov, self.time);
                    if self.black_hole_checked.insert(path.clone()) {
                        let without = fn_fx::uniform::evaluate(&self.entries[e].mat.uniform, &self.entries[e].mat.overrides, self.time);
                        let line = format!("[pickups-fx] static item material {path}: {} {}", crate::pickup_orig::BLACK_HOLE_PARAM, if with.cb != without.cb { "read by the cooked program (backpack shrink drawn)" } else { "NOT a parameter of this material (1.8 reparents it to BackPackShrinkingEffect; not ported: no shrink)" });
                        eprintln!("{line}"); self.log.push(line);
                    }
                    with
                }
            };
            let s = self.slot(fx);
            let (pb, mb) = (self.pool[s].prim, self.pool[s].mat);
            self.frame.buffer_writes.push((pb, bytes(&prim)));
            let mut cb = packed.cb; cb.resize(cb.len().max(self.entries[e].gpu.material_registers).max(1), [0.0; 4]);
            self.frame.buffer_writes.push((mb, bytes(&cb)));
            self.touched.insert(e); // per-frame globals at the view's clocks (take_frame)
            // base-pass VS globals per primitive (BasePassRendering.inl:31-66): PreviousLocalToWorld = this transform, SkipOutputVelocity 0 (as building_render)
            let vs = match self.entries[e].gpu.vs_globals().map(|g| g.float4s) {
                Some(n) => {
                    let b = *self.static_vs.entry((e, s)).or_insert_with(|| fx.create_buffer("pickup static item VS globals", n * 16));
                    if let Some(bytes_) = self.entries[e].gpu.vs_globals_bytes(&self.entries[e].mat, t, Some(l2w)) { self.frame.buffer_writes.push((b, bytes_)); }
                    Some(b)
                }
                None => None,
            };
            let binds = match self.static_binds.get(&(e, s)) { Some(b) => b.clone(), None => { let b = self.entries[e].gpu.bind_variant_globals(fx, pb, Some(mb), vs); self.static_binds.insert((e, s), b.clone()); b } };
            self.frame.push(FxDraw { pipe: self.entries[e].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, instances: None, sort });
            // shadow casting into the CSM cascades (same Primitive / Material buffers)
            if self.entries[e].gpu.translucent || self.no_shadows { continue; }
            let Some(Some((sg, own))) = self.static_shadows.get(&e) else { continue };
            let (pipe, own) = (sg.pipe, *own);
            if self.static_shadow_written.insert(e) {
                let m = if own { &self.entries[e].mat } else { match self.default_mat.as_ref().and_then(|d| d.as_ref()) { Some(d) => d, None => continue } };
                let w = sg.shadow_globals_writes(m, t, view.eye.to_array());
                self.frame.buffer_writes.extend(w);
            }
            let sb = match self.static_shadow_binds.get(&(e, s)) { Some(b) => b.clone(), None => { let b = self.static_shadows[&e].as_ref().unwrap().0.bind_variant_globals(fx, pb, if own { Some(mb) } else { None }, None); self.static_shadow_binds.insert((e, s), b.clone()); b } };
            self.frame.shadow_draws.push(FxShadowDraw { pipe, binds: sb, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, bounds: None, instances: None });
        }
        // the distance-field scene (DF shadows, DFAO), once per placed item
        if has_df && !self.no_shadows {
            if !self.static_dfs.contains_key(mesh_path) {
                let done = STATIC_DF_DONE.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(mesh_path).cloned()));
                match done {
                    Some(r) => {
                        if let Err(err) = &r { let m = format!("{mesh_path}: no distance field ({err})"); eprintln!("[pickups] {m}"); self.log.push(m); }
                        self.static_dfs.insert(mesh_path.to_owned(), r.ok());
                    }
                    None => {
                        if STATIC_DF_STARTED.lock().ok().is_some_and(|mut g| g.get_or_insert_with(HashSet::new).insert(mesh_path.to_owned())) {
                            let (set, path) = (self.set.clone(), mesh_path.to_owned());
                            let spawned = std::thread::Builder::new().name("pickup df".into()).spawn(move || {
                                let r = fn_fx::gpu::load_mesh_distance_field(&set, &path, true).map(std::sync::Arc::new);
                                if let Ok(mut g) = STATIC_DF_DONE.lock() { g.get_or_insert_with(HashMap::new).insert(path, r); }
                            });
                            if spawned.is_err() { let r = fn_fx::gpu::load_mesh_distance_field(&self.set, mesh_path, true); self.static_dfs.insert(mesh_path.to_owned(), r.ok().map(std::sync::Arc::new)); }
                        }
                    }
                }
            }
            if let Some(df) = self.static_dfs.get(mesh_path).cloned().flatten() { self.frame.distance_field_objects.push(FxDfInstance { mesh: df, local_to_world: l2w }); }
        }
        Ok(())
    }
}

static STATIC_DF_DONE: std::sync::Mutex<Option<HashMap<String, Result<std::sync::Arc<fn_render::distance_field::MeshDistanceField>, String>>>> = std::sync::Mutex::new(None);
static STATIC_DF_STARTED: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);

#[cfg(test)]
mod tests {
    use super::*;
    fn draw(align: Alignment, lock: AxisLock) -> EmitterDraw {
        EmitterDraw { system: String::new(), package: String::new(), emitter: String::new(), emitter_index: 0, kind: DrawKind::Sprite, material: String::new(), material_override: vec![], mesh: String::new(), sub_images: (4, 2), interpolation: SubUvInterp::None, alignment: align, sort: fn_particles::model::SortMode::None,
            pivot_offset: [0.0, 0.5], axis_lock: lock, axis_lock_axes: lock.locked_axes().map(|_| ([0., 0., 1.], [1., 0., 0.])), use_local_space: true, mesh_override_material: false, lod_level: 0, particles: vec![], ribbon_vertices: vec![], ribbon_indices: vec![] }
    }
    #[test] fn the_sprite_uniform_follows_the_engine_rules() {
        let v = Dxbc::sprite_uniform(&draw(Alignment::Square, AxisLock::None), 1.0 / 30.0);
        assert_eq!(v[2], [1., 0., 0., 0.]); assert_eq!(v[5], [4., 2., 0.25, 0.5]); assert_eq!(v[9], [-0.5, 0.0, 0., 0.]); assert!((v[8][3] - 30.0).abs() < 1e-3); assert_eq!(v[8][0], 1.0);
        let v = Dxbc::sprite_uniform(&draw(Alignment::Rectangle, AxisLock::RotateZ), 1.0 / 60.0);
        assert_eq!(v[2], [0., 0., 1., 0.]); assert_eq!(v[8][1], ROTATE_Z_BIAS); assert_eq!(v[0][3], 1.0); assert_eq!(v[1][3], 1.0);
        let v = Dxbc::sprite_uniform(&draw(Alignment::Velocity, AxisLock::None), 1.0 / 60.0);
        assert_eq!((v[2], v[8][0]), ([0., 1., 0., 0.], 0.0));
    }
}
