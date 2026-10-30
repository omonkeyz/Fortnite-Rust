//! STATIC MATERIALS (matstat agent, 2026-10-05): the placed harvestable props of the layout (trees, rocks, bushes, hedges: `props::Library`) drawn with their
//! REAL cooked material shaders instead of the estimated parameters of `fn_render::static_loader` (texture-mean specular, roughness 0.5, invented wind / backlight).
//!
//! Machinery = the building pieces' (`building_render.rs`): the base-pass programs of each material's cooked shader map, translated from DXBC
//! (`fn-dxbc` -> `fn_fx::gpu::build_material_layout`), `FLocalVertexFactory` permutation, drawn in fn-render's scene pass as scene-lit FX draws
//! (`fn_fx::program::assemble_scene_lit`: the GBuffer the original pixel program writes -> base colour / metallic / specular / roughness / normal / AO /
//! shading model -> the renderer's deferred-equivalent lighting, fn-render fx_resolve.wgsl + the shading-model terms of fx_resolve_sm.wgsl).
//! * Pass: `TBasePassVSFNoLightMapPolicy` + `TBasePassPSFNoLightMapPolicy` (the Athena sky light is movable, so the opaque base pass has no sky term:
//!   UE4.16 ScenePrivate.h:2269-2273; the resolve adds it), `...Skylight` only as the fallback when a map has no plain permutation (logged).
//! * Materials per mesh slot = the class's component `OverrideMaterials` else the mesh's own (`props.json` `materials`, `source`), loaded through the instance
//!   chain with the child-most static-permutation shader map (`fn_fx::material::Material::load`).
//! * Mesh: every LOD of the cooked `UStaticMesh` (`fn_mesh`), LOD picked per view like `ComputeStaticMeshLOD` (UE4.16 SceneManagement.cpp:406: the last LOD
//!   whose `(ScreenSize * 0.5)^2` exceeds `ComputeBoundsScreenRadiusSquared`, ScreenMultiple = max(0.5 Proj[0][0], 0.5 Proj[1][1]), LODDistanceFactor 1: no
//!   `r.StaticMeshLODDistanceScale` in the cooked ini); the cooked `ScreenSize[]` of the render data.
//! * Primitive buffer per placement (`fn_render::primitive_ub::mesh_primitive`: cooked `ExtendedBounds`, the distance-field bit); the base-pass VS velocity
//!   globals follow `FVelocityDrawingPolicy::HasVelocityOnBasePass` (VelocityRendering.cpp:432-466): the component is STATIC (`ABuildingSMActor` ctor
//!   @14045b960 stores Mobility 0 = `EComponentMobility::Static` at +0x13e of its StaticMeshComponent; the props' templates serialize no Mobility), so velocity
//!   is written only when the material modifies the mesh position (a connected `WorldPositionOffset` on the root material), with PreviousLocalToWorld = the
//!   current transform (a static component never moves); otherwise identity and `SkipOutputVelocity` 1.
//! * MIRRORFIX: a negative-determinant placement draws one-sided materials with the reversed cull mode (StaticMeshRender.cpp:284), as building_render does.
//! * Shadows: each casting placement goes into the sun's cascades with its material's own cooked `TShadowDepthVS` / `TShadowDepthPS` (the wind WPO is in the
//!   shadow too), or the default material's position-only VS for an opaque non-WPO material (`fn_fx::gpu::build_shadow`); and into the distance-field scene
//!   with the cooked LOD-0 distance field (same `bAffectDistanceFieldLighting` rule as df_hook.rs).
//! * The old textured static-mesh draw of these placements is suppressed in `scene.rs` (`sync_props`, [`handles`]); `FNRT_STATIC_MATERIALS=0` turns this
//!   path off (the "before" state of verification/materials/statics).
//! LIMITS (named, not faked): no hit shake (the old `fn_combat::prop_wobble` sway was invented; the 1.8 damage feedback of a BuildingProp is not ported);
//! no dithered LOD transition (`bDitheredLODTransition` / TemporalLOD); no per-cascade caster culling (every casting placement goes to every cascade).
use fn_fx::gpu::{build_material_layout, LayoutOpts, MaterialGpu, SharedBuffers, TextureCache, Vf};
use fn_fx::material::Material;
use fn_fx::view::{bytes, sprite_vf_buffer, ViewParams, VIEW_FLOAT4S};
use fn_pak::PakSet;
use fn_render::fx::*;
use glam::{Mat3, Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// WORLD (HLOD): a mesh slot material that is an export of the mesh's own package (the HLOD proxies' `M_LOD_*` instances in `*_HLOD.uasset`) comes out of
/// `fn_mesh` as a bare object name: qualify it with the package (`<long>.<name>`).
fn qualify(long: &str, p: String) -> String { if p.contains('/') { p } else { format!("{long}.{p}") } }
pub const VS_TYPE: &str = "TBasePassVSFNoLightMapPolicy";
/// Opaque / masked lit: the plain permutation first (see the module doc).
pub const PS_TYPES: [&str; 2] = ["TBasePassPSFNoLightMapPolicy", "TBasePassPSFNoLightMapPolicySkylight"];
const MAX_STATIC_MESH_LODS: usize = 8;

/// One material built for the static-mesh vertex factory (`front_cw` = the reversed-culling variant of a mirrored placement). INSTANCING: `inst` = built
/// with its `Primitive` read per instance (opaque / masked; one draw per mesh section for all placements, see `flush_batches`).
struct Entry { mat: Arc<Material>, gpu: MaterialGpu, num_uv: usize, front_cw: bool, ps: &'static str, velocity: bool, inst: bool }
struct ShadowEntry { gpu: MaterialGpu, own: bool, mat: Arc<Material>, inst: bool }
#[derive(Clone)]
struct Lod { mesh: FxMeshId, num_uv: usize, sections: Vec<(usize, u32, u32)>, tris: u32 }
/// One prop kind: its LODs, the material entry per slot (`None` = that slot is not drawn, logged), the cooked bounds / screen sizes / distance field.
struct KindGpu {
    lods: Vec<Lod>,
    slots: Vec<Option<String>>,
    bounds: fn_render::primitive_ub::BoxSphereBounds,
    screen_size: [f32; MAX_STATIC_MESH_LODS],
    df: Option<Arc<fn_render::distance_field::MeshDistanceField>>,
    cast_shadow: bool,
    /// `CachedMaxDrawDistance` in cm (0 = none).
    cull_cm: f32,
    /// PERF (2026-10-07): dense id of this kind (index of its section table, `StaticsRender::secs`) and the first section index of each LOD.
    kid: u32,
    sec_base: Vec<u32>,
}
/// PERF: the first section index of each LOD and the section count of a kind (its `SecSlot` table: two slots per section, plain / mirrored).
fn sec_layout(lods: &[Lod]) -> Vec<u32> { let mut v = Vec::with_capacity(lods.len() + 1); let mut n = 0u32; for l in lods { v.push(n); n += l.sections.len() as u32; } v.push(n); v }
/// PERF (2026-10-07): what a section of a kind draws with, resolved once: `state` 0 unresolved / 1 not drawn / 2 entry `e`; `bkey` = its instanced batch
/// key (`StaticsRender::bkeys`, `u32::MAX` = per-placement draw); `sstate` the same for the shadow: entry `se` (the canonical one of its pipeline when it
/// runs the default material's position-only program), `sbkey` its instanced batch key.
#[derive(Clone, Copy, Default)]
struct SecSlot { state: u8, sstate: u8, e: u32, bkey: u32, se: u32, sbkey: u32 }
/// PERF: a layout prop's cached draw data (rebuilt when its proto / position or the built kinds change).
#[derive(Clone)]
struct PropC { proto: u32, pos: [u32; 3], mirror: bool, gen: u32, kind: Option<Arc<KindGpu>>, row: Option<(f32, u32)>, slot: u32, l2w: Mat4, wb: (Vec3, f32, [f32; 3]) }
#[derive(Clone)]
struct PoiC { kind: Option<Arc<KindGpu>>, c: Vec3, r: f32, origin: [f32; 3] }
struct GroupC { c: Vec3, r: f32, all_far_ok: bool, n: u32, dfs: Vec<FxDfInstance>, max_cull: f32, stream_cull: bool, unbounded: bool }
fn poi_cache_on() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_STATICS_POI_CACHE").map_or(true, |v| v != "0")) }
/// A placement's record slot: its own `Primitive` buffer only once a per-placement (not instanced) draw needs it.
struct Slot { prim: Option<FxBufId>, last: Option<[f32; 16]> }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActorMid { pub scalars: Vec<(&'static str, f32)>, pub vectors: Vec<(&'static str, [f32; 4])> }
static ACTOR_MIDS: Mutex<Option<Arc<HashMap<u32, Arc<ActorMid>>>>> = Mutex::new(None);
pub fn set_actor_mids(m: HashMap<u32, Arc<ActorMid>>) { if let Ok(mut g) = ACTOR_MIDS.lock() { *g = if m.is_empty() { None } else { Some(Arc::new(m)) }; } }
fn actor_mids() -> Option<Arc<HashMap<u32, Arc<ActorMid>>>> { ACTOR_MIDS.lock().ok().and_then(|g| g.clone()) }
/// INSTANCING: float4s per record of the instance storage (`fn_fx::program::INSTANCE_PRIMITIVE_FLOAT4S`).
const INST_F4: usize = fn_fx::program::INSTANCE_PRIMITIVE_FLOAT4S as usize;
/// INSTANCING: one instanced scene draw of this frame (material entry x mesh section) / one instanced shadow draw (shadow entry x mesh section).
struct Batch { e: usize, mesh: FxMeshId, first: u32, count: u32, ids: Vec<u32>, origin: [f32; 3] }
struct SBatch { se: usize, pipe: usize, mesh: FxMeshId, first: u32, count: u32, ids: Vec<u32>, bounds: Vec<[f32; 4]> }
fn bytemuck_u32(v: &[u32]) -> Vec<u8> { if cfg!(target_endian = "little") { bytemuck::cast_slice(v).to_vec() } else { v.iter().flat_map(|x| x.to_le_bytes()).collect() } }
/// INSTANCING: `FN_STATICS_INSTANCING=0` draws every placement on its own (the per-placement path, A/B and fallback).
fn instancing() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_STATICS_INSTANCING").map_or(true, |v| v != "0")) }
/// The view's culling terms (UE cm), shared by every placement of a frame.
struct Cull { eye: Vec3, fwd: Vec3, right: Vec3, up: Vec3, tx: f32, ty: f32, sx: f32, sy: f32, near_cm: f32, far_cm: f32, /** OCCLUSION: the newest HZB readback (fn_render::occlusion) and the eye in port metres */ occ: Option<Arc<fn_render::occlusion::HzbSnapshot>>, eye_m: Vec3, /** LOD 1:1: ComputeLODForMeshes ScreenSizeScale */ lod_scale: f32, /** PERF: the renderer's small-caster threshold x LODDistanceFactor (None = no pre-cull) */ shadow_small: Option<f32> }

/// What the last frame did (the run report / tests).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats { pub hlod_proxies: u32, pub placements: u32, pub drawn: u32, pub culled_frustum: u32, pub culled_distance: u32, pub draws: u32, pub shadow_draws: u32, pub triangles: u64, pub lod_counts: [u32; 4], /** INSTANCING: instances in the instanced scene / shadow draws */ pub instanced: u32, pub shadow_instanced: u32, /** OCCLUSION: in the view frustum but hidden behind the HZB */ pub culled_occlusion: u32 }
/// REPORT ONLY (2026-10-08): the per-frame [`Stats`] summed over the run (the report printed the last frame only), plus the occlusion snapshot use:
/// frames with a usable snapshot, frames whose newest snapshot was too old (age histogram 0..7+), frames with none at all.
#[derive(Clone, Debug, Default)]
struct RunAcc { frames: u64, sum: [f64; 12], occ_used: u64, occ_stale: u64, occ_none: u64, ages: [u64; 8] }
impl RunAcc {
    fn add(&mut self, s: &Stats, used: bool, age: Option<u64>) {
        self.frames += 1;
        let v = [s.placements, s.drawn, s.culled_frustum, s.culled_distance, s.culled_occlusion, s.draws, s.instanced, s.shadow_draws, s.shadow_instanced, s.hlod_proxies];
        for (i, x) in v.iter().enumerate() { self.sum[i] += *x as f64; }
        self.sum[10] += s.triangles as f64;
        if used { self.occ_used += 1; } else if age.is_some() { self.occ_stale += 1; } else { self.occ_none += 1; }
        if let Some(a) = age { self.ages[(a as usize).min(7)] += 1; }
    }
    fn report(&self) -> String {
        if self.frames == 0 { return String::new(); }
        let n = self.frames as f64; let a = |i: usize| self.sum[i] / n;
        format!("static materials run average ({} frames): {:.0} placements, {:.1} drawn, {:.1} frustum / {:.1} distance / {:.1} occlusion culled, {:.1} draws ({:.1} instances instanced), {:.1} shadow draws ({:.1} instances instanced), {:.1} HLOD proxies, {:.0} triangles; occlusion snapshot used {} / stale {} / none {} frames, newest age histogram {:?}\n",
            self.frames, a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8), a(9), a(10), self.occ_used, self.occ_stale, self.occ_none, self.ages)
    }
}

pub struct StaticsRender {
    set: Arc<PakSet>,
    props_dir: PathBuf,
    cat: Option<fn_propdata::PropCatalog>,
    gpu_ready: bool,
    cache: TextureCache,
    shared: Option<SharedBuffers>,
    fade_buf: Option<FxBufId>,
    kinds: HashMap<String, Result<Arc<KindGpu>, String>>,
    entries: Vec<Entry>,
    index: HashMap<(String, usize, bool), Result<usize, String>>,
    shadows: HashMap<usize, Option<ShadowEntry>>,
    default_mat: Option<Option<Arc<Material>>>,
    pool: Vec<Slot>,
    /// Stable slot per placement (proto, position bits, mirror): its Primitive buffer keeps its contents between frames.
    slot_of: HashMap<(u32, [u32; 3], bool), usize>,
    binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    vs_bufs: HashMap<(usize, usize), FxBufId>,
    shadow_binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    cur_mid: Option<Arc<ActorMid>>,
    mid_bufs: HashMap<(usize, usize), FxBufId>,
    mid_binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    mid_shadow_binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    mid_draws: Vec<(usize, usize, FxMeshId, u32, u32, [f32; 3])>,
    mid_sdraws: Vec<(usize, usize, FxMeshId, u32, u32, [f32; 4])>,
    mid_inst_binds: HashMap<(usize, usize, bool), Vec<(u32, FxBindId)>>,
    /// Handles of the static-mesh path whose placements this path draws (scene.rs drops them from the old draw).
    handled: HashSet<fn_render::StaticMeshHandle>,
    warned: HashSet<String>,
    pub log: Vec<String>,
    pub stats: Stats,
    /// GRASS (2026-10-06): the ground cover on its real cooked material (grass_render.rs), sharing this path's paks, texture cache and View / light buffers.
    pub grass: crate::grass_render::GrassRender,
    /// LOAD TIME: the next library proto `build_kinds` looks at, whether every kind is processed, and the build time summed over its calls.
    next_proto: usize, prebuilt: bool, build_ms: f32,
    /// POI (dev): the `crate::poi` kinds were built.
    poi_built: bool,
    /// WORLD: the cooked distance field per mesh object (a painted component is its own kind but shares its mesh's field).
    df_cache: HashMap<String, Option<Arc<fn_render::distance_field::MeshDistanceField>>>,
    /// WORLD: per layout prop (slot key) its landscape row: (draw distance cm, HLOD cluster), `None` = no row.
    prop_rows: HashMap<(u32, [u32; 3], bool), Option<(f32, u32)>>,
    /// WORLD (HLOD): the proxy kinds without their distance field (by POI kind index).
    proxy_kinds: HashMap<usize, Arc<KindGpu>>,
    /// WORLD: the base materials of the TextureData MIDs.
    mid_bases: HashMap<String, Arc<Material>>,
    /// WORLD (GPU memory): the uploaded meshes by object (+ painted-colour hash).
    mesh_cache: HashMap<String, Arc<MeshShared>>,
    /// INSTANCING (2026-10-07): the instance storage (one `Primitive` record per slot), its capacity in records, the CPU copy, the slots written since the last
    /// upload; the bind groups / VS globals per instanced entry; this frame's batches; the resolved section entries; the per-frame "written" sets.
    inst_store: Option<FxBufId>, inst_cap: usize, inst_cpu: Vec<[f32; 4]>, inst_dirty: Vec<u32>,
    inst_binds: HashMap<usize, Vec<(u32, FxBindId)>>, inst_shadow_binds: HashMap<usize, Vec<(u32, FxBindId)>>, inst_vs: HashMap<usize, FxBufId>,
    batches: Vec<Batch>, sbatches: Vec<SBatch>,
    /// PERF (2026-10-07): dense tables instead of per-section hashing: the section table per kind id, the interned batch keys (entry, mesh, first index,
    /// index count) with this frame's batch (frame number, index), the frame each entry / shadow entry last had its per-frame buffers written, the canonical
    /// shadow entry per default-material shadow pipeline, the layout props' cache, the slots of the POI placements / HLOD proxies.
    next_kid: u32, frame_no: u32, kgen: u32,
    secs: Vec<Vec<SecSlot>>,
    bkeys: Vec<(usize, FxMeshId, u32, u32)>, bkey_of: HashMap<(usize, usize, u32, u32), u32>, bstamp: Vec<(u32, u32)>,
    sbkeys: Vec<(usize, FxMeshId, u32, u32)>, sbkey_of: HashMap<(usize, usize, u32, u32), u32>, sbstamp: Vec<(u32, u32)>,
    wstamp: Vec<u32>, swstamp: Vec<u32>, shadow_canon: HashMap<usize, usize>,
    prop_cache: Vec<PropC>, poi_slots: Vec<u32>, proxy_slots: Vec<u32>, rr_done: HashSet<String>, rr_pipes: HashMap<String, Vec<FxPipeId>>,
    /// REPORT ONLY: the run's summed stats.
    run: RunAcc,
    const_writes: Vec<Option<Vec<(FxBufId, Vec<u8>)>>>,
    ivstamp: Vec<u32>,
    poi_c: Vec<Option<PoiC>>,
    group_c: Vec<Option<GroupC>>,
    poi_c_key: usize,
}
/// WORLD: one uploaded mesh shared by kinds: its LODs, its own slot materials (qualified), screen sizes, bounds.
struct LodPrep { desc: FxMeshDesc, num_uv: usize, sections: Vec<(usize, u32, u32)>, tris: u32 }
struct MeshPrep { lods: Vec<LodPrep>, mats: Vec<Option<String>>, screen_size: [f32; MAX_STATIC_MESH_LODS], bounds: fn_render::primitive_ub::BoxSphereBounds, name: String, warn: Option<String> }
static PREPPED_DF: Mutex<Option<HashMap<String, Result<fn_render::distance_field::MeshDistanceField, String>>>> = Mutex::new(None);
static PREPPED: Mutex<Option<HashMap<String, Result<MeshPrep, String>>>> = Mutex::new(None);
fn take_prepped(key: &str) -> Option<Result<MeshPrep, String>> { PREPPED.lock().ok()?.as_mut()?.remove(key) }
fn mesh_key(id: &str, object: &str) -> String { format!("{object}{}", id.find('#').map_or("", |i| &id[i..(i + 17).min(id.len())])) }
fn prep_mesh(set: &PakSet, id: &str, object: &str, long: &str) -> Result<MeshPrep, String> {
    let (a, x, b) = fn_fx::data::package_files(set, long)?;
    let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
    let m = fn_mesh::StaticMesh::parse_named(&p, object.split_once('.').map(|x| x.1)).map_err(|e| format!("{long}: {e}"))?;
    let vcol = if id.starts_with("poi:") { crate::poi::kind_vcol(id) } else { None };
    let mut lods = vec![];
    let mut warn = None;
    for (li, lod) in m.lods.iter().enumerate() {
        let num_uv = lod.num_tex_coords;
        let stride = fn_fx::gpu::mesh_stride(Vf::Local, num_uv) as usize;
        let n = lod.positions.len();
        let mut vb = vec![0u8; n * stride];
        let over = vcol.as_ref().and_then(|v| v.get(li)).and_then(|c| c.as_ref()).filter(|c| c.len() == n);
        if vcol.is_some() && over.is_none() && li == 0 { warn = Some(format!("{id}: painted colours do not match LOD 0 ({n} vertices): the mesh's own are used")); }
        for i in 0..n {
            let o = i * stride;
            for (k, f) in lod.positions[i].iter().enumerate() { vb[o + 4 * k..o + 4 * k + 4].copy_from_slice(&f.to_le_bytes()); }
            vb[o + 12..o + 16].copy_from_slice(&lod.tangent_x[i]);
            vb[o + 16..o + 20].copy_from_slice(&lod.tangent_z[i]);
            vb[o + 20..o + 24].copy_from_slice(&over.map(|c| c[i]).or_else(|| lod.colors.get(i).copied()).unwrap_or([255; 4]));
            for u in 0..num_uv {
                let uv = lod.uvs.get(u).and_then(|s| s.get(i)).copied().unwrap_or([0.; 2]);
                let at = o + 24 + 16 * (u / 2) + 8 * (u % 2);
                vb[at..at + 4].copy_from_slice(&uv[0].to_le_bytes()); vb[at + 4..at + 8].copy_from_slice(&uv[1].to_le_bytes());
            }
        }
        let sections: Vec<(usize, u32, u32)> = lod.sections.iter().map(|s| (s.material_index.max(0) as usize, s.first_index, s.num_triangles * 3)).collect();
        let tris = sections.iter().map(|s| s.2 / 3).sum();
        lods.push(LodPrep { desc: FxMeshDesc { label: format!("{} LOD{li}", m.name), vertices: vb, indices: lod.indices.clone() }, num_uv, sections, tris });
    }
    if lods.is_empty() { return Err(format!("{long}: no LOD")); }
    let bounds = extended_bounds(set, long, object.split_once('.').map(|x| x.1)).unwrap_or(fn_render::primitive_ub::BoxSphereBounds { origin: m.bounds.origin, extent: m.bounds.extent, radius: m.bounds.radius });
    Ok(MeshPrep { lods, mats: m.materials.iter().map(|x| x.material.clone().map(|p| qualify(long, p))).collect(), screen_size: m.screen_size, bounds, name: m.name.clone(), warn })
}
struct MeshShared { lods: Vec<Lod>, mats: Vec<Option<String>>, screen_size: [f32; MAX_STATIC_MESH_LODS], bounds: fn_render::primitive_ub::BoxSphereBounds, name: String }

const POI_UPLOAD_BYTES: u64 = 6 << 20;
fn yaw_ue(yaw_game: f32) -> Mat3 { Mat3::from_rotation_z(-yaw_game) }

/// `UStaticMesh::GetBounds()` = the tagged `ExtendedBounds` of the `StaticMesh` export (as fn_fx::gpu::load_mesh reads it), `None` when not serialized.
fn extended_bounds(set: &PakSet, long: &str, name: Option<&str>) -> Option<fn_render::primitive_ub::BoxSphereBounds> {
    let pkg = fn_fx::data::PackageJson::load(set, long).ok()?;
    let e = pkg.of_class("StaticMesh").find(|e| name.map_or(true, |n| e.name == n)).or_else(|| pkg.of_class("StaticMesh").next())?.props.get("ExtendedBounds")?.clone();
    let v3 = |v: &serde_json::Value| -> Option<[f32; 3]> {
        if let Some(a) = v.as_array() { return (a.len() == 3).then(|| [a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32, a[2].as_f64().unwrap_or(0.) as f32]); }
        Some([v.get("X")?.as_f64()? as f32, v.get("Y")?.as_f64()? as f32, v.get("Z")?.as_f64()? as f32])
    };
    Some(fn_render::primitive_ub::BoxSphereBounds { origin: v3(e.get("Origin")?)?, extent: v3(e.get("BoxExtent")?)?, radius: e.get("SphereRadius")?.as_f64()? as f32 })
}

/// Placement transform in the FX space (UE cm, Z up): the game's `R_y(yaw) * (mirror * scale * p) + pos` (static_common.wgsl `static_transform`) conjugated by
/// the `(x, y, z) m -> (x, z, y) cm` axis map of the exported meshes (fn-mesh gltf_lods.rs): `T(to_ue(pos)) * Rz(-yaw) * S(scale) * diag(mirror, 1, 1)`.
pub fn local_to_world(pos_game: Vec3, yaw_game: f32, scale: f32, mirror_x: bool) -> Mat4 {
    let p = fn_fx::view::to_ue(pos_game).as_vec3();
    Mat4::from_translation(p) * Mat4::from_mat3(yaw_ue(yaw_game)) * Mat4::from_scale(Vec3::new(if mirror_x { -scale } else { scale }, scale, scale))
}

/// `ComputeStaticMeshLOD` (UE4.16 SceneManagement.cpp:406) for a sphere at `origin` (UE cm) of `radius`, the view at `eye` with tan half fov `tan_half`.
/// LOD 1:1 (2026-10-08): a Static component is drawn from the static draw lists, whose LOD is `ComputeLODForMeshes` (SceneManagement.cpp:425-505, called by
/// `FRelevancePacket::MarkRelevant`, SceneVisibility.cpp:1633; 1.8 exe @1423c1780 from @141996530), not `ComputeStaticMeshLOD`: the last LOD whose
/// `(ScreenSize * ScreenSizeScale * 0.5)^2 >= ComputeBoundsScreenRadiusSquared` (note `>=`), ScreenSizeScale = `LODScale` = `r.StaticMeshLODDistanceScale`
/// (1: not in the cooked inis; exe default 1.0, `dyninit_CVarStaticMeshLODDistanceScale` @1401677f0) x `View.LODDistanceFactor` (1.8:
/// `FMarkRelevantStaticMeshesForViewData` @14197fba0 = View+0xb3c x the cvar) = FOV / DefaultFOV 90 (LocalPlayer.cpp:1000, fn_render::quality::lod_distance_factor).
/// `scale` = that LODScale. `FN_STATIC_LOD_FOV=0`: the old rule (scale 1, `>`).
pub fn static_mesh_lod(screen_size: &[f32; MAX_STATIC_MESH_LODS], num_lods: usize, origin: Vec3, radius: f32, eye: Vec3, tan_half: (f32, f32), scale: f32) -> usize {
    let multiple = (0.5 / tan_half.0).max(0.5 / tan_half.1);
    let r2 = (multiple * radius).powi(2) / (origin - eye).length_squared().max(1.0);
    if !lod_fov() { for i in (0..MAX_STATIC_MESH_LODS).rev() { if (screen_size[i] * 0.5).powi(2) > r2 { return i.min(num_lods.saturating_sub(1)); } } return 0; }
    // (the static meshes of LODs past the mesh's count do not exist: the walk starts at its last LOD)
    for i in (0..num_lods.min(MAX_STATIC_MESH_LODS)).rev() { if (screen_size[i] * scale * 0.5).powi(2) >= r2 { return i; } }
    0
}
/// LOD 1:1: `FN_STATIC_LOD_FOV=0` turns the `ComputeLODForMeshes` rule (LODDistanceFactor, `>=`) off.
fn lod_fov() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_STATIC_LOD_FOV").map_or(true, |v| v != "0")) }
/// LOD 1:1: this frame's `View.LODDistanceFactor` (FOV / 90), set by the app from the camera ([`set_lod_distance_factor`]).
static LOD_FACTOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);
pub fn set_lod_distance_factor(f: f32) { LOD_FACTOR.store(f.to_bits(), Ordering::Relaxed); }
pub fn lod_scale() -> f32 { if lod_fov() { f32::from_bits(LOD_FACTOR.load(Ordering::Relaxed)) * STATIC_MESH_LOD_DISTANCE_SCALE } else { 1.0 } }
/// PERF: `FN_STATICS_SHADOW_PRECULL=0` submits every caster within the cascades' reach again (the renderer then drops the small ones).
fn const_uniforms() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_STATICS_CONST_UNIFORMS").map_or(true, |v| v != "0")) }
fn vs_once() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_STATICS_VS_ONCE").map_or(true, |v| v != "0")) }
fn fort_cull_on() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_STATICS_FORT_CULL").map_or(true, |v| v != "0")) }
fn cull_log_on() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_CULL_LOG").map_or(cfg!(target_arch = "wasm32"), |v| v != "0")) }
fn shadow_precull() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_STATICS_SHADOW_PRECULL").map_or(true, |v| v != "0")) }
/// SCALABILITY: this frame's `r.ViewDistanceScale` (the app's render options), multiplied into every placement's draw distance (`FN_STATIC_VDS=0`: not).
static VDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);
pub fn set_view_distance_scale(s: f32) { let s = if cfg!(target_arch = "wasm32") { s.min(0.5) } else { s }; VDS.store(s.max(0.).to_bits(), Ordering::Relaxed); }
static STREAM_HINT: Mutex<Option<Vec3>> = Mutex::new(None);
pub fn set_stream_hint(game_m: Option<Vec3>) { if let Ok(mut g) = STREAM_HINT.lock() { *g = game_m.map(|p| fn_fx::view::to_ue(p).as_vec3()); } }
fn stream_hint() -> Option<Vec3> { STREAM_HINT.lock().ok().and_then(|g| *g) }
static READ_HINT: Mutex<Option<Vec3>> = Mutex::new(None);
pub fn set_read_hint(game_m: Option<Vec3>) { if let Ok(mut g) = READ_HINT.lock() { *g = game_m.map(|p| fn_fx::view::to_ue(p).as_vec3()); } }
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn read_hint() -> Option<Vec3> { READ_HINT.lock().ok().and_then(|g| *g) }
const READY_CHECKS: usize = 256;
fn resident_scaled() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_POI_RESIDENT_VDS").map_or(true, |v| v != "0")) }
static HARD_CM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
fn web_cap_cm(name: &str, def_m: f32) -> f32 {
    if !cfg!(target_arch = "wasm32") { return 0.; }
    fn_live::var(name).ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(def_m).max(0.) * 100.
}
fn prop_cap_cm() -> f32 { static V: std::sync::OnceLock<f32> = std::sync::OnceLock::new(); *V.get_or_init(|| web_cap_cm("FN_WEB_PROP_MAX_M", 0.)) }
static ON_BUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CAP_NOW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static CAP_AT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
const CAP_SHRINK_CM_S: f32 = 4500.;
pub fn set_on_bus(on: bool) {
    ON_BUS.store(on, Ordering::Relaxed);
    let target = if on { bus_cap_cm() } else { proxy_cap_cm() };
    let now = std::time::Instant::now();
    let dt = CAP_AT.lock().ok().and_then(|mut g| g.replace(now)).map_or(0., |t| now.duration_since(t).as_secs_f32().min(0.25));
    let cur = f32::from_bits(CAP_NOW.load(Ordering::Relaxed));
    let next = if cur <= 0. || target >= cur || target <= 0. { target } else { (cur - CAP_SHRINK_CM_S * dt).max(target) };
    CAP_NOW.store(next.to_bits(), Ordering::Relaxed);
}
fn bus_cap_cm() -> f32 { static V: std::sync::OnceLock<f32> = std::sync::OnceLock::new(); *V.get_or_init(|| web_cap_cm("FN_WEB_BUS_POI_MAX_M", 0.)) }
fn live_proxy_cap_cm() -> f32 { let c = f32::from_bits(CAP_NOW.load(Ordering::Relaxed)); if c > 0. { c } else { proxy_cap_cm() } }
fn proxy_in_cap(c: &crate::poi::HlodCluster, eye: Vec3) -> bool { let cap = live_proxy_cap_cm(); cap <= 0. || (eye.clamp(c.lo, c.hi) - eye).length() <= cap }
fn proxy_cap_cm() -> f32 { static V: std::sync::OnceLock<f32> = std::sync::OnceLock::new(); *V.get_or_init(|| web_cap_cm("FN_WEB_POI_MAX_M", 0.)) }
fn view_distance_scale() -> f32 {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| fn_live::var("FN_STATIC_VDS").map_or(true, |v| v != "0")) { f32::from_bits(VDS.load(Ordering::Relaxed)) } else { 1.0 }
}
/// `r.StaticMeshLODDistanceScale`: not set by any cooked 1.8 ini (CookedIniVersion.txt), exe default 1.0.
const STATIC_MESH_LOD_DISTANCE_SCALE: f32 = 1.0;

impl StaticsRender {
    pub fn new(install: &Path, props_dir: &Path) -> Result<StaticsRender, String> {
        let set = Arc::new(PakSet::open_install(install).map_err(|e| e.to_string())?);
        Ok(StaticsRender { set, props_dir: props_dir.to_path_buf(), cat: None, gpu_ready: false, cache: TextureCache::tracked(), shared: None, fade_buf: None, kinds: HashMap::new(), entries: vec![], index: HashMap::new(), shadows: HashMap::new(), default_mat: None, pool: vec![], slot_of: HashMap::new(), binds: HashMap::new(), vs_bufs: HashMap::new(), shadow_binds: HashMap::new(), cur_mid: None, mid_bufs: HashMap::new(), mid_binds: HashMap::new(), mid_shadow_binds: HashMap::new(), mid_draws: vec![], mid_sdraws: vec![], mid_inst_binds: HashMap::new(), handled: HashSet::new(), warned: HashSet::new(), log: vec![], stats: Stats::default(), grass: crate::grass_render::GrassRender::new(), next_proto: 0, prebuilt: false, build_ms: 0., poi_built: false, df_cache: HashMap::new(), prop_rows: HashMap::new(), proxy_kinds: HashMap::new(), mid_bases: HashMap::new(), mesh_cache: HashMap::new(),
            inst_store: None, inst_cap: 0, inst_cpu: vec![], inst_dirty: vec![], inst_binds: HashMap::new(), inst_shadow_binds: HashMap::new(), inst_vs: HashMap::new(),
            batches: vec![], sbatches: vec![], next_kid: 0, frame_no: 0, kgen: 1, secs: vec![], bkeys: vec![], bkey_of: HashMap::new(), bstamp: vec![], sbkeys: vec![], sbkey_of: HashMap::new(), sbstamp: vec![],
            wstamp: vec![], swstamp: vec![], shadow_canon: HashMap::new(), prop_cache: vec![], poi_slots: vec![], proxy_slots: vec![], rr_done: HashSet::new(), rr_pipes: HashMap::new(), run: RunAcc::default(), const_writes: vec![], ivstamp: vec![], poi_c: vec![], group_c: vec![], poi_c_key: 0 })
    }
    fn warn(&mut self, key: &str, msg: String) { if self.warned.insert(key.to_owned()) { eprintln!("[statics] {msg}"); self.log.push(msg); } }

    fn init_gpu(&mut self, fx: &mut FxRenderer) {
        if self.gpu_ready { return; }
        self.gpu_ready = true;
        let view = fx.create_buffer("statics view", VIEW_FLOAT4S * 16);
        let light = fx.create_buffer("statics light", fn_render::sky_light::FX_LIGHT_BYTES);
        let sprite_vf = fx.create_buffer("statics sprite vf", 10 * 16);
        fx.write_buffer(sprite_vf, &bytes(&sprite_vf_buffer()));
        let prim = fx.create_buffer("statics primitive (shared)", 32 * 16);
        let fade = fx.create_buffer("statics PrimitiveFade", 16);
        fx.write_buffer(fade, &bytes(&fn_matshader::frame::fade_buffer()));
        self.shared = Some(SharedBuffers { view, primitive: prim, sprite_vf, light });
        self.fade_buf = Some(fade);
    }

    /// `FMaterial::MaterialModifiesMeshPosition_RenderThread` = the shader map's `FMaterialCompilationOutput::bModifiesMeshPosition` (the velocity rule of the
    /// module doc), read from the cooked shader map the programs come from (`fn_fx::uniform::compilation_flags`); `None` = not found (logged, no velocity).
    fn modifies_position(&self, mat: &Material) -> Option<bool> {
        static SEEN: Mutex<Option<HashMap<String, Option<bool>>>> = Mutex::new(None);
        if let Some(v) = SEEN.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&mat.shader_map_package).copied())) { return v; }
        let v = (|| {
            let (a, x, b) = fn_fx::data::package_files(&self.set, &mat.shader_map_package).ok()?;
            let p = fn_ueio::Package::parse(&a, &x, &b).ok()?;
            fn_fx::uniform::compilation_flags(&x, &p.names).map(|f| f.modifies_mesh_position)
        })();
        if let Ok(mut g) = SEEN.lock() { g.get_or_insert_with(HashMap::new).insert(mat.shader_map_package.clone(), v); }
        v
    }

    fn entry(&mut self, fx: &mut FxRenderer, path: &str, num_uv: usize, front_cw: bool) -> Result<usize, String> {
        let key = (path.to_owned(), num_uv, front_cw);
        if let Some(r) = self.index.get(&key) { return r.clone(); }
        let (shared, fade) = (self.shared.unwrap(), self.fade_buf.unwrap());
        let r = (|| -> Result<usize, String> {
            // LOAD TIME: parsed by `prewarm` during the loading screen when it was in the kinds' slot list, else here
            // WORLD: "mid:<base>?<name>=<texture>&..." = a MaterialInstanceDynamic of <base> with those texture parameters (TextureData, poi.rs `mid0`)
            let mat = if let Some(rest) = path.strip_prefix("mid:") {
                let (base, params) = rest.split_once('?').unwrap_or((rest, ""));
                let b = match self.mid_bases.get(base) { Some(b) => b.clone(), None => { let b = match take_preloaded(base) { Some(m) => m?, None => Arc::new(Material::load(&self.set, base)?) }; self.mid_bases.insert(base.to_owned(), b.clone()); b } };
                let mut m = (*b).clone();
                for kv in params.split('&').filter(|s| !s.is_empty()) { if let Some((k, v)) = kv.split_once('=') { m.texture_overrides.insert(k.to_owned(), v.to_owned()); } }
                m.path = path.to_owned();
                Arc::new(m)
            } else { match self.mid_bases.get(path) { Some(b) => b.clone(), None => { let b = match take_preloaded(path) { Some(m) => m?, None => { eprintln!("[matmiss] {path}"); Arc::new(Material::load(&self.set, path)?) } }; self.mid_bases.insert(path.to_owned(), b.clone()); b } } };
            let mut last = String::new();
            for ps in PS_TYPES {
                if mat.cooked.find(Vf::Local.type_name(), ps).is_none() { last = format!("{path}: no {} / {ps} program", Vf::Local.type_name()); continue; }
                // INSTANCING (2026-10-07): opaque / masked materials read `Primitive` per instance (one draw per mesh section); a program that cannot be
                // rewritten, and every translucent one (sorted per placement), keeps the per-placement draw
                let want = instancing() && matches!(mat.flags.blend.as_str(), "BLEND_Opaque" | "BLEND_Masked");
                let build = |s: &mut Self, fx: &mut FxRenderer, inst: bool| build_material_layout(&s.set, fx, &mut s.cache, &mat, Vf::Local, VS_TYPE, ps, num_uv, false, shared, &[("PrimitiveFade", fade)], LayoutOpts { front_cw, instance_primitive: inst, ..LayoutOpts::default() });
                let (gpu, inst) = match want.then(|| build(self, fx, true)) {
                    Some(Ok(g)) => (g, true),
                    Some(Err(e)) => { self.warn(&format!("inst {path}"), format!("{path}: per-placement draws ({e})")); (build(self, fx, false)?, false) }
                    None => (build(self, fx, false)?, false),
                };
                let velocity_flag = self.modifies_position(&mat);
                let velocity = velocity_flag.unwrap_or(false);
                let line = format!("material {} (shader map of {}): {VS_TYPE} / {ps}{}, blend {} shading {} two-sided {}, bModifiesMeshPosition {}, {} uniform vectors / {} scalars / {} textures, {} targets{}",
                    path.rsplit('.').next().unwrap_or(path), mat.shader_map_package.rsplit('/').next().unwrap_or(""), if ps == PS_TYPES[1] { " (FALLBACK: no plain permutation)" } else { "" },
                    mat.flags.blend, mat.flags.shading, mat.flags.two_sided, velocity_flag.map_or("NOT FOUND (no velocity)".to_owned(), |v| v.to_string()), mat.uniform.vectors.len(), mat.uniform.scalars.len(), mat.uniform.textures2d.len(), gpu.assembled.targets, if front_cw { ", reversed culling" } else { "" });
                eprintln!("[statics] {line}");
                self.log.push(line);
                self.entries.push(Entry { mat: mat.clone(), gpu, num_uv, front_cw, ps, velocity, inst });
                return Ok(self.entries.len() - 1);
            }
            Err(last)
        })();
        if let Err(e) = &r { let m = format!("{path}: NOT BUILT ({e}): its sections are not drawn"); self.warn(&format!("mat {path} {front_cw}"), m); }
        self.index.insert(key, r.clone());
        r
    }

    /// FX SHADOW (as building_render `shadow_entry`): the material's own shadow-depth pair, or the default material's position-only VS.
    fn shadow_entry(&mut self, fx: &mut FxRenderer, e: usize) -> Option<usize> {
        if !self.shadows.contains_key(&e) {
            let (shared, fade) = (self.shared.unwrap(), self.fade_buf.unwrap());
            let (mat, num_uv, front_cw) = (self.entries[e].mat.clone(), self.entries[e].num_uv, self.entries[e].front_cw);
            let needs_default = matches!(mat.flags.blend.as_str(), "BLEND_Opaque") && mat.cooked.find(Vf::Local.type_name(), fn_fx::gpu::SHADOW_VS).is_none();
            if needs_default && self.default_mat.is_none() {
                let d = match take_preloaded(fn_fx::gpu::DEFAULT_SURFACE_MATERIAL) { Some(m) => m, None => Material::load(&self.set, fn_fx::gpu::DEFAULT_SURFACE_MATERIAL).map(Arc::new) };
                if let Err(err) = &d { self.warn("defmat", format!("{}: {err}", fn_fx::gpu::DEFAULT_SURFACE_MATERIAL)); }
                self.default_mat = Some(d.ok());
            }
            let dm = self.default_mat.clone().flatten();
            // INSTANCING: the shadow programs read `Primitive` per instance when the entry's do (else, or when they cannot be rewritten, per placement)
            let want = self.entries[e].inst;
            let mut r = fn_fx::gpu::build_shadow_ext(&self.set, fx, &mut self.cache, &mat, dm.as_deref(), Vf::Local, num_uv, shared, &[("PrimitiveFade", fade)], front_cw, want).map(|o| o.map(|(g, own)| (g, own, want)));
            if want && r.is_err() {
                let m = format!("{}: shadow per placement ({})", mat.path, r.as_ref().err().unwrap()); self.warn(&format!("inst shadow {e}"), m);
                r = fn_fx::gpu::build_shadow(&self.set, fx, &mut self.cache, &mat, dm.as_deref(), Vf::Local, num_uv, shared, &[("PrimitiveFade", fade)], front_cw).map(|o| o.map(|(g, own)| (g, own, false)));
            }
            let name = mat.path.rsplit('.').next().unwrap_or("").to_owned();
            let v = match r {
                Ok(Some((gpu, own, inst))) => {
                    let line = format!("material {name}: shadow depth {} (front_cw {front_cw}, two-sided {})", if own { format!("own {} / {}", fn_fx::gpu::SHADOW_VS, if mat.flags.blend == "BLEND_Masked" { fn_fx::gpu::SHADOW_PS } else { "null PS" }) } else { format!("default material {}", fn_fx::gpu::SHADOW_VS_POSITION_ONLY) }, mat.flags.two_sided);
                    eprintln!("[statics] {line}"); self.log.push(line);
                    Some(ShadowEntry { gpu, own, mat: if own { mat.clone() } else { dm.clone().unwrap() }, inst })
                }
                Ok(None) => None,
                Err(err) => { self.warn(&format!("shadow {e}"), format!("material {name}: shadow depth NOT built ({err}): casts no shadow")); None }
            };
            self.shadows.insert(e, v);
        }
        self.shadows[&e].as_ref().map(|_| e)
    }

    fn catalog(&mut self) -> Option<&fn_propdata::PropCatalog> {
        if self.cat.is_none() {
            match fn_propdata::PropCatalog::load(&self.props_dir) { Ok(c) => self.cat = Some(c), Err(e) => { let m = format!("{}: {e}", self.props_dir.display()); self.warn("cat", m); return None } }
        }
        self.cat.as_ref()
    }

    /// The GPU side of one kind (all LODs, the slot materials, the distance field), built once.
    fn kind(&mut self, fx: &mut FxRenderer, id: &str) -> Result<Arc<KindGpu>, String> {
        if let Some(k) = self.kinds.get(id) { return k.clone(); }
        let t0 = std::time::Instant::now();
        let r = (|| -> Result<Arc<KindGpu>, String> {
            // POI (dev): a town mesh of `crate::poi` (cooked mesh + the component's OverrideMaterials, cast shadow, no draw distance), else a catalog kind
            let poi = if id.starts_with("poi:") { crate::poi::kind_source(id) } else { None };
            let (object, raw, cat_cast_shadow, cull_cm) = match poi {
                Some((mesh, ov)) => (mesh, serde_json::json!({"materials": ov.iter().enumerate().filter_map(|(i, m)| m.as_ref().map(|m| serde_json::json!({"slot": i, "ue_path": m}))).collect::<Vec<_>>()}), true, 0.),
                None => { let k = self.catalog().ok_or("no props catalog")?.kinds.iter().find(|k| k.id == id).ok_or_else(|| format!("{id}: not in the catalog"))?; (k.mesh_ue_path.clone(), k.raw.clone(), k.render.cast_shadow, k.render.cull_distance_cm) }
            };
            let long = fn_fx::data::long_package(&object).to_owned();
            // WORLD (GPU memory): the uploaded LODs are shared by every kind of the same mesh and colour stream (the TextureData / override variants only change
            // materials): the vcol hash suffix of the id is part of the key
            let mesh_key = mesh_key(id, &object);
            let shared = match self.mesh_cache.get(&mesh_key) { Some(m) => m.clone(), None => {
            let prep = match take_prepped(&mesh_key) { Some(p) => p, None => prep_mesh(&self.set, id, &object, &long) };
            let built = (|| -> Result<Arc<MeshShared>, String> {
                let p = prep?;
                if let Some(w) = &p.warn { self.warn(&format!("vcol {id}"), w.clone()); }
                let lods = p.lods.into_iter().map(|l| Lod { mesh: fx.create_mesh(&l.desc), num_uv: l.num_uv, sections: l.sections, tris: l.tris }).collect();
                Ok(Arc::new(MeshShared { lods, mats: p.mats, screen_size: p.screen_size, bounds: p.bounds, name: p.name }))
            })()?;
            self.mesh_cache.insert(mesh_key, built.clone()); built } };
            let lods: Vec<Lod> = shared.lods.clone();
            // slot materials: the class's resolved list (component overrides included), else the mesh's own slot material
            let raw_mats = raw.get("materials").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let nslots = shared.mats.len().max(raw_mats.iter().filter_map(|v| v.get("slot").and_then(|s| s.as_u64())).max().map_or(0, |s| s as usize + 1));
            let mut slots: Vec<Option<String>> = (0..nslots).map(|s| raw_mats.iter().find(|v| v.get("slot").and_then(|x| x.as_u64()) == Some(s as u64)).and_then(|v| v.get("ue_path").and_then(|p| p.as_str()).map(str::to_owned)).or_else(|| shared.mats.get(s).cloned().flatten())).collect();
            // WORLD: a BuildingSMActor's TextureData MID replaces slot 0: base = TextureData[0].OverrideMaterial, else the MESH's material 0 ("mid:" entry)
            if let Some(mid) = if id.starts_with("poi:") { crate::poi::kind_mid0(id) } else { None } {
                if let (Some(base), Some(s0)) = (mid.0.clone().or_else(|| shared.mats.first().cloned().flatten()), slots.first_mut()) {
                    *s0 = Some(format!("mid:{base}?{}", mid.1.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")));
                }
            }
            let bounds = shared.bounds;
            // df_hook.rs `load_one`: CastShadow (catalog) && bAffectDistanceFieldLighting && the CDO's bIsMovable / bIsDynamic clear
            let flag = |ptr: &str| raw.pointer(ptr).and_then(|x| x.as_bool());
            let affects = cat_cast_shadow && flag("/ue_properties/static_mesh_component/bAffectDistanceFieldLighting").unwrap_or(true) && flag("/ue_properties/static_mesh_component/CastShadow").unwrap_or(true)
                && !flag("/ue_properties/class_default_object/bIsMovable").unwrap_or(false) && !flag("/ue_properties/class_default_object/bIsDynamic").unwrap_or(false);
            let df = match self.df_cache.get(&format!("{object}|{affects}")) { Some(d) => d.clone(), None => {
                let pre = PREPPED_DF.lock().ok().and_then(|mut g| g.as_mut().and_then(|m| m.remove(&format!("{object}|{affects}"))));
                let d = pre.unwrap_or_else(|| fn_fx::gpu::load_mesh_distance_field(&self.set, &object, affects)).map_err(|e| self.warn(&format!("df {id}"), format!("{id}: no distance field ({e})"))).ok().map(Arc::new);
                self.df_cache.insert(format!("{object}|{affects}"), d.clone()); d } };
            let cast_shadow = cat_cast_shadow && flag("/ue_properties/static_mesh_component/CastShadow").unwrap_or(true);
            let lod_line: Vec<String> = lods.iter().zip(shared.screen_size.iter()).map(|(l, s)| format!("{} tris @ {s:.3}", l.tris)).collect();
            let line = format!("kind {id}: {} ({} LODs: {}), slots {:?}, cast shadow {cast_shadow}, DF {}", object.rsplit('/').next().unwrap_or(""), lods.len(), lod_line.join(", "), slots.iter().map(|s| s.as_deref().map(|p| p.rsplit('.').next().unwrap_or(p)).unwrap_or("-")).collect::<Vec<_>>(), df.as_ref().map_or("none".to_owned(), |d| format!("{:?} affects {}", d.size, d.affects_distance_field_lighting)));
            eprintln!("[statics] {line}");
            self.log.push(line);
            let (kid, sec_base) = (self.next_kid, sec_layout(&lods)); self.next_kid += 1;
            Ok(Arc::new(KindGpu { lods, slots, bounds, screen_size: shared.screen_size, df, cast_shadow, cull_cm, kid, sec_base }))
        })();
        if let Err(e) = &r { let m = format!("{id}: NOT ON THE REAL-SHADER PATH ({e}): drawn by the old static-mesh path"); self.warn(&format!("kind {id}"), m); }
        else { let ms = t0.elapsed().as_secs_f32() * 1000.; self.log.push(format!("kind {id}: GPU data in {ms:.0} ms")); }
        self.kinds.insert(id.to_owned(), r.clone());
        if !id.starts_with("poi:") { self.kgen += 1; } // PERF: a layout kind changed: the props' cache is rebuilt
        r
    }

    /// Every material entry of a kind, built up front so the first frame on this path has no missing section.
    fn render_ready(&mut self, fx: &FxRenderer, id: &str) -> bool {
        if self.rr_done.contains(id) { return true; }
        if !self.rr_pipes.contains_key(id) {
            let Some(Ok(k)) = self.kinds.get(id) else { return true };
            let mut pipes = vec![];
            for lod in &k.lods {
                for (slot, _, _) in &lod.sections {
                    let Some(p) = k.slots.get(*slot).cloned().flatten() else { continue };
                    if let Some(Ok(e)) = self.index.get(&(p, lod.num_uv, false)) { let pp = self.entries[*e].gpu.pipe; if !pipes.contains(&pp) { pipes.push(pp); } }
                }
            }
            self.rr_pipes.insert(id.to_owned(), pipes);
        }
        if self.rr_pipes.get(id).is_some_and(|v| v.iter().any(|p| fx.pipe_pending(*p))) { return false; }
        self.rr_pipes.remove(id);
        self.rr_done.insert(id.to_owned());
        true
    }
    fn prebuild(&mut self, fx: &mut FxRenderer, k: &KindGpu) -> bool {
        let mut ok = true;
        for lod in &k.lods {
            for (slot, _, _) in &lod.sections {
                match k.slots.get(*slot).cloned().flatten() {
                    Some(p) => match self.entry(fx, &p, lod.num_uv, false) {
                        // LOAD TIME (2026-10-07): the shadow programs too (built on first draw before: every kind's shadow translation hitched the first frames)
                        Ok(e) => { if k.cast_shadow && !self.entries[e].gpu.translucent { let _ = self.shadow_entry(fx, e); } }
                        Err(_) => ok = false,
                    },
                    None => ok = false,
                }
            }
        }
        ok
    }

    fn slot(&mut self, key: (u32, [u32; 3], bool)) -> usize {
        if let Some(&s) = self.slot_of.get(&key) { return s; }
        self.pool.push(Slot { prim: None, last: None });
        self.slot_of.insert(key, self.pool.len() - 1);
        self.pool.len() - 1
    }

    /// Builds the placed kinds of the library (GPU data + every slot material) from `next_proto` on: all that are left (`budget` None), or until `budget`
    /// has elapsed (at least one kind per call). Returns true once every kind is processed. Same kinds, order and results as the old one-shot build of the
    /// first frame; only the frames it happens in change.
    fn build_kinds(&mut self, fx: &mut FxRenderer, props: &[fn_combat::Harvestable], real: &crate::props::Real, budget: Option<std::time::Duration>) -> bool {
        if self.prebuilt { return true; }
        if self.next_proto == 0 { wait_prewarm(); } // LOAD TIME: the slot materials were parsed during the loading screen
        let t0 = std::time::Instant::now();
        while self.next_proto < real.lib.protos.len() {
            let pi = self.next_proto;
            self.next_proto += 1;
            let p = &real.lib.protos[pi];
            let Some(h) = real.meshes.get(pi).copied().flatten() else { continue };
            if !props.iter().any(|x| x.proto as usize == pi) { continue; }
            let Ok(k) = self.kind(fx, &p.id) else { continue };
            self.kgen += 1; // PERF: the layout props' cache is rebuilt
            if self.prebuild(fx, &k) { self.handled.insert(h); } else { let m = format!("{}: a section material failed: the kind stays on the old static-mesh path", p.id); self.warn(&format!("partial {}", p.id), m); self.kinds.insert(p.id.clone(), Err("section material failed".into())); }
            if budget.is_some_and(|b| t0.elapsed() >= b) && self.next_proto < real.lib.protos.len() { self.build_ms += t0.elapsed().as_secs_f32() * 1000.; CHANGED.store(true, Ordering::Relaxed); return false; }
        }
        self.build_ms += t0.elapsed().as_secs_f32() * 1000.;
        self.prebuilt = true;
        let line = format!("{} kinds on the real-shader path ({} materials) in {:.0} ms", self.handled.len(), self.entries.len(), self.build_ms);
        eprintln!("[statics] {line}"); self.log.push(line);
        let own = PREWARMED_TEXTURES.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default();
        fn_fx::gpu::drop_predecoded(&own); // LOAD TIME: free any chain this prewarm decoded that the build did not use (other prewarms keep theirs)
        CHANGED.store(true, Ordering::Relaxed);
        true
    }

    /// One frame: the FX draws (scene pass + cascades) and distance-field objects of every live placement whose kind is on this path.
    pub fn frame(&mut self, fx: &mut FxRenderer, props: &[fn_combat::Harvestable], real: &crate::props::Real, view: &ViewParams, out: &mut FxFrame) {
        self.init_gpu(fx);
        HARD_CM.store(prop_cap_cm().to_bits(), Ordering::Relaxed);
        let shared = self.shared.unwrap();
        out.buffer_writes.push((shared.view, bytes(&view.view_buffer())));
        out.buffer_writes.push((shared.light, bytes(&fn_render::sky_light::fx_light_block())));
        // the kinds of the loaded library (once): GPU data + every material; a kind with any unbuildable section stays on the old path (whole, never mixed).
        // LOAD TIME: normally `prebuild_step` already built them under the loading screen; this finishes whatever is left (all of it in scripted runs).
        prof("statics s0 kinds", || self.build_kinds(fx, props, real, None));
        let eye = fn_fx::view::to_ue(view.eye).as_vec3();
        let fwd = fn_fx::view::dir_to_ue(view.forward).as_vec3().normalize();
        let up0 = fn_fx::view::dir_to_ue(view.up).as_vec3();
        let right = fwd.cross(up0).normalize();
        let up = right.cross(fwd).normalize();
        let (tx, ty) = view.tan_half;
        let (sx, sy) = ((1. + tx * tx).sqrt(), (1. + ty * ty).sqrt());
        let (near_cm, far_cm) = (view.near_m * 100., view.far_m * 100.);
        let t = view.material_time();
        let cull = Cull { eye, fwd, right, up, tx, ty, sx, sy, near_cm, far_cm, occ: fn_render::occlusion::latest(), eye_m: view.eye, lod_scale: lod_scale(), shadow_small: if shadow_precull() { fx.shadow_radius_threshold().map(|t| t * f32::from_bits(LOD_FACTOR.load(Ordering::Relaxed))) } else { None } };
        let mut st = Stats::default();
        self.frame_no = self.frame_no.wrapping_add(1).max(1); // PERF: the per-frame stamps (written buffers, batches)
        // WORLD (HLOD, 2026-10-07): which clusters show their proxy this frame (FLODSceneTree) and have it built; their sub actors are not drawn
        let mut pt = ProfLap::new(); let poi = crate::poi::current();
        let proxy_on: Vec<bool> = poi.as_ref().map(|s| { let fb = fallback_clusters(s); s.clusters.iter().enumerate().map(|(i, c)| (c.proxy_visible(eye) || fb[i]) && matches!(self.kinds.get(&s.kinds[c.kind].id), Some(Ok(_)))).collect() }).unwrap_or_default();
        let hidden = |cl: u32| cl != crate::poi::NO_CLUSTER && proxy_on.get(cl as usize).copied().unwrap_or(false);
        if self.frame_no % 60 == 0 { if let Some(s) = poi.as_ref() {
            let fb = fallback_clusters(s);
            let gap = s.clusters.iter().enumerate().filter(|(i, c)| fb[*i] && !proxy_on[*i] && (eye.clamp(c.lo, c.hi) - eye).length() < 35000.).count();
            let near: Vec<&crate::poi::PoiGroup> = s.groups.iter().filter(|g| g.resident.load(Ordering::Relaxed) && !g.ready.load(Ordering::Relaxed) && (g.center - eye).length() - g.radius < 20000.).collect();
            let unbuilt: usize = near.iter().map(|g| g.kinds.iter().filter(|&&k| !self.kinds.contains_key(&s.kinds[k as usize].id)).count()).sum();
            let compiling: usize = near.iter().map(|g| g.kinds.iter().filter(|&&k| !self.render_ready(fx, &s.kinds[k as usize].id)).count()).sum();
            if gap > 0 || !near.is_empty() { eprintln!("[poi gap] {gap} clusters without proxy or detail, {} groups within 200 m not ready ({unbuilt} kinds unbuilt, {compiling} compiling), eye {:.0} m up", near.len(), eye.z / 100.); }
        } }
        let keep_on = self.frame_no % KEEP_EVERY == 0;
        let mut keep: Vec<u32> = vec![];
        // PERF (2026-10-07): per prop index its kind, row, slot and transform, rebuilt only when the prop or the layout kinds change (was: hashing per prop per frame)
        if self.prop_cache.len() != props.len() { self.prop_cache = vec![PropC { proto: u32::MAX, pos: [0; 3], mirror: false, gen: 0, kind: None, row: None, slot: 0, l2w: Mat4::IDENTITY, wb: (Vec3::ZERO, 0., [0.; 3]) }; props.len()]; }
        for (i, p) in props.iter().enumerate() {
            if !p.alive() || fn_combat::is_poi_proto(p.proto) { continue; } // (a POI actor's harvestable has no layout mesh: drawn by the POI part below)
            let pos = [p.pos.x.to_bits(), p.pos.y.to_bits(), p.pos.z.to_bits()];
            let c = &self.prop_cache[i];
            if c.gen != self.kgen || c.proto != p.proto || c.pos != pos || c.mirror != p.mirror_x {
                let pi = p.proto as usize;
                let kind = real.meshes.get(pi).copied().flatten().filter(|h| self.handled.contains(h)).and_then(|_| match self.kinds.get(&real.lib.protos[pi].id) { Some(Ok(k)) => Some(k.clone()), _ => None });
                let key = (p.proto, pos, p.mirror_x);
                let (row, slot) = if kind.is_some() {
                    // WORLD: a layout prop of a landscape level has that actor's draw distance and HLOD cluster (`crate::poi::PoiSet::layout_row`)
                    let row = *self.prop_rows.entry(key).or_insert_with(|| poi.as_ref().and_then(|s| s.layout_row(&real.lib.protos[pi].id, fn_fx::view::to_ue(p.pos).as_vec3())));
                    (row, self.slot(key) as u32)
                } else { (None, 0) };
                let l2w = local_to_world(p.pos, p.yaw, p.scale, p.mirror_x); let wb = kind.as_ref().map_or((Vec3::ZERO, 0., [0.; 3]), |k| { let b = k.bounds.transform_by(&l2w); (Vec3::from(b.origin), b.radius, b.origin) }); self.prop_cache[i] = PropC { proto: p.proto, pos, mirror: p.mirror_x, gen: self.kgen, kind, row, slot, l2w, wb };
            }
            let c = &self.prop_cache[i];
            let Some(k) = c.kind.clone() else { continue };
            if keep_on && (c.wb.0 - eye).length() - c.wb.1 < KEEP_NEAR_CM { keep.push(k.kid); }
            if c.row.is_some_and(|r| hidden(r.1)) { continue; }
            let placed = c.row.map(|r| (r.0, true)).or_else(|| (fort_cull_on() && k.cull_cm <= 0.).then(|| (fn_render::quality::fort_cull::cull_distance_m(c.wb.1 * 0.01) * 100., true)));
            let (l2w, slot, pre) = (c.l2w, c.slot as usize, poi_cache_on().then_some(c.wb));
            self.draw_one_pre(fx, &k, l2w, slot, &cull, view, t, &mut st, out, placed, pre);
        }
        // ALL POIS (STREAMING, 2026-10-07): the POI groups (`crate::poi`: file x 256 m cell) within `crate::poi::stream_cm` of the view are resident
        // (10% hysteresis): their kinds are built nearest group first under POI_BUILD_MS per frame (all at once in scripted runs), and once a group's kinds are
        // all built its placements are drawn (and their distance-field objects pushed: a non-resident group is not in the DF scene). A section whose material
        // does not build is skipped.

        if let Some(poi) = poi {
            pt.lap("statics s1 props"); let res = self.poi_build(fx, &poi, eye, None, None); pt.lap("statics s2 poi_build"); HARD_CM.store(0, Ordering::Relaxed);
            // a destroyed actor's components are gone (its harvestable in the sim's props is dead)
            let mut dead: HashSet<u32> = props.iter().filter(|p| fn_combat::is_poi_proto(p.proto) && !p.alive()).map(|p| p.proto & !fn_combat::POI_PROTO).collect();
            dead.extend(crate::bounce_hook::hidden_actors());
            let mids = actor_mids();
            // PERF: the built kind per POI kind index, looked up once per frame (not a String-keyed lookup per placement)
            let mut kc: Vec<Option<Option<Arc<KindGpu>>>> = vec![None; poi.kinds.len()];
            let stream = crate::poi::stream_cm();
            if poi_cache_on() {
                let key = Arc::as_ptr(&poi) as usize;
                if self.poi_c_key != key { self.poi_c_key = key; self.poi_c = vec![None; poi.placements.len()]; self.group_c = (0..poi.groups.len()).map(|_| None).collect(); }
                for gi in res {
                    let g = &poi.groups[gi];
                    if !g.ready.load(Ordering::Relaxed) { continue; }
                    if self.group_c[gi].is_none() {
                        let (mut lo, mut hi, mut all_far_ok, mut n, mut dfs) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN), true, 0u32, vec![]);
                        let (mut max_cull, mut stream_cull, mut unbounded) = (0f32, false, false);
                        for &i in &g.placements {
                            let pl = &poi.placements[i as usize];
                            let kind = match self.kinds.get(&poi.kinds[pl.kind].id) { Some(Ok(k)) => Some(k.clone()), _ => None };
                            let pc = match &kind {
                                Some(k) => { let wb = k.bounds.transform_by(&pl.l2w); PoiC { kind: kind.clone(), c: Vec3::from(wb.origin), r: wb.radius, origin: wb.origin } }
                                None => PoiC { kind: None, c: Vec3::ZERO, r: 0., origin: [0.; 3] },
                            };
                            if let Some(k) = &pc.kind {
                                lo = lo.min(pc.c - Vec3::splat(pc.r)); hi = hi.max(pc.c + Vec3::splat(pc.r)); n += 1;
                                if !(pl.cluster == crate::poi::NO_CLUSTER && pl.cull_cm <= 0.) { all_far_ok = false; }
                                if pl.cull_cm > 0. { max_cull = max_cull.max(pl.cull_cm); } else if pl.cluster == crate::poi::NO_CLUSTER { stream_cull = true; } else { unbounded = true; }
                                if let Some(df) = &k.df { dfs.push(FxDfInstance { mesh: df.clone(), local_to_world: pl.l2w }); }
                            }
                            self.poi_c[i as usize] = Some(pc);
                        }
                        let (c, r) = if n > 0 { let c = (lo + hi) * 0.5; (c, (hi - lo).length() * 0.5) } else { (Vec3::ZERO, 0.) };
                        self.group_c[gi] = Some(GroupC { c, r, all_far_ok, n, dfs, max_cull, stream_cull, unbounded });
                    }
                    let gc = self.group_c[gi].as_ref().unwrap();
                    if gc.n == 0 { continue; }
                    if dead.is_empty() && !g.clusters.iter().any(|&c| hidden(c)) {
                        let (r, d) = (gc.r + 1., gc.c - eye);
                        let dist = d.length() - r;
                        let z = d.dot(cull.fwd);
                        let out_view = !(z + r > cull.near_cm && z - r < cull.far_cm && d.dot(cull.right).abs() <= z * cull.tx + r * cull.sx && d.dot(cull.up).abs() <= z * cull.ty + r * cull.sy);
                        let reach = if gc.stream_cull { (gc.max_cull * view_distance_scale()).max(stream) } else { gc.max_cull * view_distance_scale() };
                        let all_culled = !gc.unbounded && dist > reach + 1. && (!keep_on || dist > KEEP_NEAR_CM + 1.);
                        if (gc.all_far_ok && dist > stream + 1.) || (out_view && dist > POI_CSM_CM + 1.) || all_culled {
                            out.distance_field_objects.extend(gc.dfs.iter().cloned());
                            st.placements += gc.n; st.culled_distance += gc.n;
                            continue;
                        }
                    }
                    for &i in &g.placements {
                        let pl = &poi.placements[i as usize];
                        if dead.contains(&pl.actor) { continue; }
                        if hidden(pl.cluster) { continue; }
                        let Some(pc) = self.poi_c[i as usize].as_ref() else { continue };
                        let Some(k) = pc.kind.as_ref() else { continue };
                        let (c, r) = (pc.c, pc.r);
                        let d = c - eye;
                        let dist = d.length() - r;
                        let z = d.dot(cull.fwd);
                        let in_view = z + r > cull.near_cm && z - r < cull.far_cm && d.dot(cull.right).abs() <= z * cull.tx + r * cull.sx && d.dot(cull.up).abs() <= z * cull.ty + r * cull.sy;
                        if keep_on && dist < KEEP_NEAR_CM { keep.push(k.kid); }
                        if (pl.cluster == crate::poi::NO_CLUSTER && pl.cull_cm <= 0. && dist > stream) || (!in_view && dist > POI_CSM_CM) {
                            if let Some(df) = &k.df { out.distance_field_objects.push(FxDfInstance { mesh: df.clone(), local_to_world: pl.l2w }); }
                            st.placements += 1; st.culled_distance += 1;
                            continue;
                        }
                        let (k, pre) = (k.clone(), (c, r, pc.origin));
                        let iu = i as usize;
                        if self.poi_slots.len() <= iu { self.poi_slots.resize(poi.placements.len().max(iu + 1), u32::MAX); }
                        if self.poi_slots[iu] == u32::MAX { self.poi_slots[iu] = self.slot((0x8000_0000 | i, [0; 3], false)) as u32; }
                        let slot = self.poi_slots[iu] as usize;
                        self.cur_mid = mids.as_ref().and_then(|m| m.get(&pl.actor).cloned());
                        self.draw_one_pre(fx, &k, pl.l2w, slot, &cull, view, t, &mut st, out, Some((pl.cull_cm, pl.cast_shadow)), Some(pre));
                        self.cur_mid = None;
                    }
                }
            } else {
            for gi in res {
                let g = &poi.groups[gi];
                if !g.ready.load(Ordering::Relaxed) { continue; }
                for &i in &g.placements {
                    let pl = &poi.placements[i as usize];
                    if dead.contains(&pl.actor) { continue; }
                    if hidden(pl.cluster) { continue; } // WORLD (HLOD): the cluster's proxy stands for it
                    let k = kc[pl.kind].get_or_insert_with(|| match self.kinds.get(&poi.kinds[pl.kind].id) { Some(Ok(k)) => Some(k.clone()), _ => None }).clone();
                    let Some(k) = k else { continue };
                    // ALL POIS (CULLING, CHOSEN): a placement farther than the HLOD distance (`stream_cm`, from its bounds) is not drawn (the real client shows the
                    // cluster's LODActor proxy there: NOT PORTED); one outside the view and farther than POI_CSM_CM can neither be seen nor cast into the
                    // shadow cascades. Both keep their distance-field object (the DF scene of a resident group stays put while the view moves).
                    let wb = k.bounds.transform_by(&pl.l2w);
                    let (c, r) = (Vec3::from(wb.origin), wb.radius);
                    let d = c - eye;
                    let dist = d.length() - r;
                    let z = d.dot(cull.fwd);
                    let in_view = z + r > cull.near_cm && z - r < cull.far_cm && d.dot(cull.right).abs() <= z * cull.tx + r * cull.sx && d.dot(cull.up).abs() <= z * cull.ty + r * cull.sy;
                    // WORLD: a clustered placement is drawn until its proxy takes over (above); one without a cluster within its group's reach
                    if (pl.cluster == crate::poi::NO_CLUSTER && pl.cull_cm <= 0. && dist > stream) || (!in_view && dist > POI_CSM_CM) {
                        if let Some(df) = &k.df { out.distance_field_objects.push(FxDfInstance { mesh: df.clone(), local_to_world: pl.l2w }); }
                        st.placements += 1; st.culled_distance += 1;
                        continue;
                    }
                    // PERF: the placement's slot by index (was a hashed key per placement per frame)
                    let iu = i as usize;
                    if self.poi_slots.len() <= iu { self.poi_slots.resize(poi.placements.len().max(iu + 1), u32::MAX); }
                    if self.poi_slots[iu] == u32::MAX { self.poi_slots[iu] = self.slot((0x8000_0000 | i, [0; 3], false)) as u32; }
                    let slot = self.poi_slots[iu] as usize;
                    self.cur_mid = mids.as_ref().and_then(|m| m.get(&pl.actor).cloned());
                    self.draw_one(fx, &k, pl.l2w, slot, &cull, view, t, &mut st, out, Some((pl.cull_cm, pl.cast_shadow)));
                    self.cur_mid = None;
                }
            }
            }
            pt.lap("statics s3 poi placements");
            // WORLD (HLOD): the proxies in their draw range (no draw distance of their own; their distance field is not pushed: CHOSEN, the sub actors' fields
            // are not either while hidden)
            HARD_CM.store(live_proxy_cap_cm().to_bits(), Ordering::Relaxed);
            for (ci, c) in poi.clusters.iter().enumerate() {
                if !proxy_on[ci] { continue; }
                let k = match self.proxy_kinds.get(&c.kind) { Some(k) => k.clone(), None => {
                    let Some(Ok(k)) = self.kinds.get(&poi.kinds[c.kind].id).cloned() else { continue };
                    let k = Arc::new(KindGpu { df: None, lods: k.lods.iter().map(|l| Lod { mesh: l.mesh, num_uv: l.num_uv, sections: l.sections.clone(), tris: l.tris }).collect(), slots: k.slots.clone(), bounds: k.bounds, screen_size: k.screen_size, cast_shadow: k.cast_shadow, cull_cm: 0., kid: self.next_kid, sec_base: k.sec_base.clone() });
                    self.next_kid += 1;
                    self.proxy_kinds.insert(c.kind, k.clone()); k } };
                if self.proxy_slots.len() <= ci { self.proxy_slots.resize(poi.clusters.len().max(ci + 1), u32::MAX); }
                if self.proxy_slots[ci] == u32::MAX { self.proxy_slots[ci] = self.slot((0x8000_0000 | ci as u32, [1, 0, 0], false)) as u32; }
                let slot = self.proxy_slots[ci] as usize;
                self.draw_one(fx, &k, c.l2w, slot, &cull, view, t, &mut st, out, Some((0., c.cast_shadow)));
                st.hlod_proxies += 1;
            }
        }
        pt.lap("statics s4 proxies"); self.flush_batches(fx, view, t, &mut st, out); // INSTANCING: the frame's instanced draws
        if keep_on { self.keep_textures(fx, keep); }
        pt.lap("statics s5 flush"); if prof_on() { let (a, n) = MATW.with(|c| c.replace((0., 0.))); prof_add("statics mat writes ms", a); prof_add("statics mat writes count", n); }
        self.run.add(&st, cull.occ.is_some(), fn_render::occlusion::newest_age());
        self.stats = st;
        if cull_log_on() && self.run.frames >= 300 {
            let g = &self.grass.stats;
            eprintln!("[cull] {} | grass {} of {} cells drawn, {} occluded, {} instances | {}", self.run.report().trim_end(), g.cells_drawn, g.cells, g.cells_occluded, g.instances, fn_render::backdrop::cull_report());
            self.run = RunAcc::default();
        }
    }

    fn keep_textures(&mut self, fx: &mut FxRenderer, mut ks: Vec<u32>) {
        ks.sort_unstable(); ks.dedup();
        let mut es: HashSet<usize> = HashSet::new();
        for kid in ks { if let Some(v) = self.secs.get(kid as usize) { for s in v { if s.state == 2 { es.insert(s.e as usize); } } } }
        for &e in &es { if let Some(en) = self.entries.get(e) { fx.touch_binds(&en.gpu.binds); } }
        for ((e, _), b) in &self.mid_binds { if es.contains(e) { fx.touch_binds(b); } }
        for ((e, _, _), b) in &self.mid_inst_binds { if es.contains(e) { fx.touch_binds(b); } }
    }

    /// ALL POIS (STREAMING): updates which POI groups are resident around `eye` (FX space cm) and builds the kinds of the resident ones, nearest first: under
    /// `budget` (at least one kind per call; `None` = POI_BUILD_MS, or everything in scripted runs). Returns the resident groups, nearest first.
    fn poi_build(&mut self, fx: &mut FxRenderer, poi: &crate::poi::PoiSet, eye: Vec3, budget: Option<std::time::Duration>, only_cm: Option<f32>) -> Vec<usize> {
        let stream = crate::poi::stream_cm();
        let hint = stream_hint();
        #[cfg(target_arch = "wasm32")] { let kinds = &self.kinds; stream_web::request_ring(&self.set, poi, hint.or_else(read_hint).unwrap_or(eye), hint.is_some(), &|id: &str| kinds.contains_key(id)); } // BROWSER PORT: the kinds' reads on workers, ahead of the view
        let mut res: Vec<(f32, usize)> = vec![];
        // WORLD (HLOD): a group is resident while one of its clusters shows its sub actors, or the view is within the reach of its unclustered placements
        let near: Vec<bool> = poi.clusters.iter().map(|c| !c.proxy_visible(eye)).collect();
        let vds = view_distance_scale().min(1.);
        for (gi, g) in poi.groups.iter().enumerate() {
            let d = (g.center - eye).length() - g.radius;
            let was = g.resident.load(Ordering::Relaxed);
            let reach = if resident_scaled() { (g.cull_reach_cm * vds).max(g.stream_reach_cm) } else { g.reach_cm };
            let now = g.clusters.iter().any(|&c| near[c as usize]) || d < if was { reach * 1.1 } else { reach };
            if now != was { g.resident.store(now, Ordering::Relaxed); }
            if now { res.push((d, gi)); }
        }
        res.sort_by(|a, b| a.0.total_cmp(&b.0));
        let sync = budget.is_none() && crate::poi::sync_build();
        let budget = budget.unwrap_or(std::time::Duration::from_secs_f32(POI_BUILD_MS / 1000.));
        let t0 = std::time::Instant::now();
        let (mut built, mut groups) = (0usize, 0usize);
        let over = |built: usize| !sync && built > 0 && (t0.elapsed() >= budget || fn_render::fx::frame_upload_bytes() >= POI_UPLOAD_BYTES);
        let fallback = fallback_clusters(poi);
        let mut want: Vec<(f32, usize)> = poi.clusters.iter().enumerate().filter(|(i, c)| (c.proxy_visible(eye) || fallback[*i]) && proxy_in_cap(c, eye) && !self.kinds.contains_key(&poi.kinds[c.kind].id)).map(|(i, c)| ((eye.clamp(c.lo, c.hi) - eye).length(), i)).collect();
        want.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, ci) in want {
            if over(built) { break; }
            let id = poi.kinds[poi.clusters[ci].kind].id.clone();
            if self.kinds.contains_key(&id) { continue; }
            #[cfg(target_arch = "wasm32")] if !stream_web::ready(&self.set, &id, true) { continue; }
            if let Ok(k) = self.kind(fx, &id) { let _ = self.prebuild(fx, &k); }
            #[cfg(target_arch = "wasm32")] stream_web::built(&id);
            built += 1;
        }
        let mut out_of_budget = false;
        #[cfg(target_arch = "wasm32")] let mut promoted = 0usize;
        let mut checks = 0usize;
        'res: for &(d, gi) in &res {
            let g = &poi.groups[gi];
            if g.ready.load(Ordering::Relaxed) { continue; }
            if only_cm.is_some_and(|m| d >= m) { continue; }
            let mut done = true;
            for &k in &g.kinds {
                let id = &poi.kinds[k as usize].id;
                if self.kinds.contains_key(id) { continue; }
                #[cfg(target_arch = "wasm32")] if !stream_web::is_done(id) { let _ = stream_web::ready(&self.set, id, { promoted += 1; promoted <= 16 }); done = false; continue; }
                checks += 1;
                if !sync && checks > READY_CHECKS { break 'res; }
                #[cfg(target_arch = "wasm32")] if !stream_web::ready(&self.set, id, { promoted += 1; promoted <= 16 }) { done = false; continue; }
                if over(built) { done = false; out_of_budget = true; break; }
                if let Ok(k) = self.kind(fx, id) { let _ = self.prebuild(fx, &k); }
                #[cfg(target_arch = "wasm32")] stream_web::built(id);
                built += 1;
            }
            if done && !g.kinds.iter().all(|&k| self.render_ready(fx, &poi.kinds[k as usize].id)) { done = false; }
            if done { g.ready.store(true, Ordering::Relaxed); groups += 1; }
            if out_of_budget { break; }
        }
        if let (Some(h), false) = (hint, out_of_budget || over(built)) {
            let mut ahead: Vec<(f32, usize)> = poi.groups.iter().enumerate().filter(|(_, g)| !g.ready.load(Ordering::Relaxed)).map(|(i, g)| ((g.center - h).length() - g.radius, i)).filter(|(d, i)| *d < poi.groups[*i].stream_reach_cm.max(stream)).collect();
            ahead.sort_by(|a, b| a.0.total_cmp(&b.0));
            'ahead: for (_, gi) in ahead {
                for &k in &poi.groups[gi].kinds {
                    let id = &poi.kinds[k as usize].id;
                    if self.kinds.contains_key(id) { continue; }
                    #[cfg(target_arch = "wasm32")] if !stream_web::is_done(id) { continue; }
                    checks += 1;
                    if checks > READY_CHECKS { break 'ahead; }
                    #[cfg(target_arch = "wasm32")] if !stream_web::ready(&self.set, id, false) { continue; }
                    if over(built) { break 'ahead; }
                    if let Ok(k) = self.kind(fx, id) { let _ = self.prebuild(fx, &k); }
                    #[cfg(target_arch = "wasm32")] stream_web::built(id);
                    built += 1;
                }
            }
        }
        if built > 0 {
            let ms = t0.elapsed().as_secs_f32() * 1000.;
            let line = format!("POI: {built} kinds built in {ms:.0} ms ({groups} groups ready; {} resident, {} kinds on this path)", res.len(), self.kinds.keys().filter(|k| k.starts_with("poi:")).count());
            eprintln!("[statics] {line}"); if self.log.len() < 4000 { self.log.push(line); }
        }
        res.into_iter().map(|r| r.1).collect()
    }

    /// One placement of a built kind: distance field, cull, LOD, Primitive record, the scene / shadow draws of its sections (props and POI statics).
    /// INSTANCING (2026-10-07): a section whose material entry is instanced ([`Entry::inst`]) only adds the placement's record index `s` to its batch (one draw
    /// per material x mesh section, emitted by [`Self::flush_batches`]); the cull, the LOD and the record are the placement's own, as before.
    #[allow(clippy::too_many_arguments)]
    fn draw_one(&mut self, fx: &mut FxRenderer, k: &Arc<KindGpu>, l2w: Mat4, s: usize, c0: &Cull, view: &ViewParams, t: fn_fx::uniform::MaterialTime, st: &mut Stats, out: &mut FxFrame, placed: Option<(f32, bool)>) {
        self.draw_one_pre(fx, k, l2w, s, c0, view, t, st, out, placed, None)
    }
    #[allow(clippy::too_many_arguments)]
    fn draw_one_pre(&mut self, fx: &mut FxRenderer, k: &Arc<KindGpu>, l2w: Mat4, s: usize, c0: &Cull, view: &ViewParams, t: fn_fx::uniform::MaterialTime, st: &mut Stats, out: &mut FxFrame, placed: Option<(f32, bool)>, pre: Option<(Vec3, f32, [f32; 3])>) {
        let eye = c0.eye;
        // POI: the component's own draw distance and CastShadow override the kind's
        let (cull_cm, cast_shadow) = match placed { Some((c, s)) => (c, s && k.cast_shadow), None => (k.cull_cm, k.cast_shadow) };
        st.placements += 1;
        let (c, r, wo) = match pre { Some(p) => p, None => { let wb = k.bounds.transform_by(&l2w); (Vec3::from(wb.origin), wb.radius, wb.origin) } };
        let d = c - eye;
        let dist = d.length();
        // distance field: every live casting placement (the DF scene is view independent: UE4.16 FScene::AddPrimitive puts every primitive that affects
        // distance-field lighting into the distance-field scene; the draw distance only culls its rendering). PERF (2026-10-06): before the cull-distance test
        // now; a placement crossing its draw distance used to change the DF object list, which forced a full global-distance-field rebuild (DFAO spikes).
        if let Some(df) = &k.df { out.distance_field_objects.push(FxDfInstance { mesh: df.clone(), local_to_world: l2w }); }
        // SCALABILITY (2026-10-08, FN_STATIC_VDS=0 = off): MaxDrawDistance x r.ViewDistanceScale (UE4.16 FrustumCull / IsDistanceCulled, SceneVisibility.cpp:250-254 /
        // 308-343; ViewDistanceQuality@0..3 = 0.5 / 0.67 / 0.83 / 1.0, CookedIniVersion.txt:5079-5094). 1.0 at Epic: the user's image does not change.
        let cull_cm = cull_cm * view_distance_scale();
        if cull_cm > 0. && dist > cull_cm + r { st.culled_distance += 1; return; }
        let hard = f32::from_bits(HARD_CM.load(Ordering::Relaxed));
        if hard > 0. && dist - r > hard { st.culled_distance += 1; return; }
        let (fwd, right, up, tx, ty, sx, sy, near_cm, far_cm) = (c0.fwd, c0.right, c0.up, c0.tx, c0.ty, c0.sx, c0.sy, c0.near_cm, c0.far_cm);
        let z = d.dot(fwd);
        let in_frustum = z + r > near_cm && z - r < far_cm && d.dot(right).abs() <= z * tx + r * sx && d.dot(up).abs() <= z * ty + r * sy;
        // OCCLUSION (2026-10-07): a placement hidden behind an earlier frame's depth (fn_render::occlusion, conservative) is not drawn in the view; it keeps
        // its shadow and distance field (UE's view occlusion does not touch the whole-scene shadow casters)
        let occluded = in_frustum && c0.occ.as_ref().is_some_and(|o| o.sphere_occluded(Vec3::new(c.x, c.z, c.y) / 100., r / 100., c0.eye_m));
        let in_view = in_frustum && !occluded;
        // WORLD (PERF): nothing farther than the cascades reach (POI_CSM_CM: the whole-scene shadow distance + the caster extrusion) can cast into them; the
        // renderer would cull its shadow draw per cascade anyway, this only saves encoding it (the far props / HLOD proxies of the whole island)
        // PERF (2026-10-08, FN_STATICS_SHADOW_PRECULL=0 = off): a caster the renderer's small-caster cull (r.Shadow.RadiusThreshold, ShadowSetup.cpp:2454,
        // fn_render::quality::shadow_caster_screen_size_culled: bounds origin to the camera, x LODDistanceFactor) drops from EVERY cascade is not submitted at all
        // (0.1 % margin so float rounding never drops one the renderer keeps): same shadow maps, fewer shadow instances / draws to encode and test.
        let small = c0.shadow_small.is_some_and(|t| r < t * dist * 0.999);
        let cast_shadow = cast_shadow && dist - r < POI_CSM_CM && !small;
        if occluded { st.culled_occlusion += 1; }
        if !in_view && !cast_shadow { if !occluded { st.culled_frustum += 1; } return; }
        let li = static_mesh_lod(&k.screen_size, k.lods.len(), c, r, eye, (tx, ty), c0.lod_scale);
        let l2w_a = l2w.to_cols_array();
        if self.pool[s].last != Some(l2w_a) {
            let flags = fn_render::primitive_ub::PrimitiveFlags { has_distance_field: k.df.as_ref().map_or(false, |d| d.affects_distance_field_lighting), ..Default::default() };
            let rec = fn_render::primitive_ub::mesh_primitive(&l2w, &k.bounds, l2w.w_axis.truncate().to_array(), &flags);
            // INSTANCING: the record of the instance storage (written by `flush_batches`), and the placement's own buffer when it has one (per-placement draws)
            let at = s * INST_F4;
            if self.inst_cpu.len() < at + INST_F4 { self.inst_cpu.resize(at + INST_F4, [0.; 4]); }
            for (i, v) in rec.iter().take(INST_F4).enumerate() { self.inst_cpu[at + i] = *v; }
            self.inst_dirty.push(s as u32);
            if let Some(b) = self.pool[s].prim { out.buffer_writes.push((b, bytes(&rec))); }
            self.pool[s].last = Some(l2w_a);
        }
        let reverse = Mat3::from_mat4(l2w).determinant() < 0.0;
        if in_view { st.drawn += 1; if li < 4 { st.lod_counts[li] += 1; } st.triangles += k.lods[li].tris as u64; } else if !occluded { st.culled_frustum += 1; }
        let lod = &k.lods[li];
        let fno = self.frame_no;
        for (sec_i, &(_, first, count)) in lod.sections.iter().enumerate() {
            // PERF: the section's material entry and batch key, resolved once per (kind, LOD, section, mirrored)
            let sl = self.sec_slot(fx, k, li, sec_i, reverse);
            if sl.state != 2 { continue; }
            let e = sl.e as usize;
            let mesh = lod.mesh;
            if self.wstamp[e] != fno {
                self.wstamp[e] = fno; let tw = prof_on().then(std::time::Instant::now);
                // the Material uniform buffer (uniform expressions at this frame's time) and the per-frame globals, once per material per frame
                if self.const_writes.len() < self.entries.len() { self.const_writes.resize(self.entries.len(), None); }
                if self.const_writes[e].is_none() {
                    let packed = fn_fx::uniform::evaluate(&self.entries[e].mat.uniform, &self.entries[e].mat.overrides, t.world);
                    let mut cb = packed.cb; cb.resize(cb.len().max(self.entries[e].gpu.material_registers).max(1), [0.0; 4]);
                    let mut w = vec![(self.entries[e].gpu.material_buf, bytes(&cb))];
                    w.extend(self.entries[e].gpu.per_frame_writes(&self.entries[e].mat, t));
                    out.buffer_writes.extend(w.iter().cloned());
                    if const_uniforms() && self.entries[e].mat.uniform.is_time_invariant() { self.const_writes[e] = Some(w); }
                } else if let Some(w) = &self.const_writes[e] { out.buffer_writes.extend(w.iter().cloned()); }
                if let Some(tw) = tw { MATW.with(|c| { let (a, n) = c.get(); c.set((a + tw.elapsed().as_secs_f32() * 1000., n + 1.)); }); }
            }
            if in_view && sl.bkey != u32::MAX && self.cur_mid.is_some() {
                self.mid_buffer(fx, e, s, t, out);
                self.mid_draws.push((e, s, mesh, first, count, wo));
            } else if in_view {
                if sl.bkey != u32::MAX {
                    let bk = sl.bkey as usize;
                    let (f0, b0) = self.bstamp[bk];
                    let b = if f0 == fno { b0 as usize } else {
                        let (be, bm, bf, bc) = self.bkeys[bk];
                        self.batches.push(Batch { e: be, mesh: bm, first: bf, count: bc, ids: vec![], origin: wo });
                        self.bstamp[bk] = (fno, (self.batches.len() - 1) as u32);
                        self.batches.len() - 1
                    };
                    self.batches[b].ids.push(s as u32);
                } else {
                    let sort = view.sort(wo);
                    let pb = self.slot_buffer(fx, s);
                    let vs = match self.entries[e].gpu.vs_globals().map(|g| g.float4s) {
                        Some(n) => {
                            let b = *self.vs_bufs.entry((e, s)).or_insert_with(|| fx.create_buffer("statics base-pass VS globals", n * 16));
                            let prev = if self.entries[e].velocity { Some(l2w) } else { None };
                            if let Some(by) = self.entries[e].gpu.vs_globals_bytes(&self.entries[e].mat, t, prev) { out.buffer_writes.push((b, by)); }
                            Some(b)
                        }
                        None => None,
                    };
                    let binds = if self.cur_mid.is_some() {
                        let mb = self.mid_buffer(fx, e, s, t, out);
                        match self.mid_binds.get(&(e, s)) { Some(b) => b.clone(), None => { let b = self.entries[e].gpu.bind_variant_globals(fx, pb, Some(mb), vs); self.mid_binds.insert((e, s), b.clone()); b } }
                    } else { match self.binds.get(&(e, s)) { Some(b) => b.clone(), None => { let b = self.entries[e].gpu.bind_variant_globals(fx, pb, None, vs); self.binds.insert((e, s), b.clone()); b } } };
                    out.draws.push(FxDraw { pipe: self.entries[e].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, instances: None, sort });
                    st.draws += 1;
                }
            }
            if cast_shadow && !self.entries[e].gpu.translucent {
                let sl = self.sec_shadow(fx, k, li, sec_i, reverse);
                if sl.sstate == 2 {
                    let se = sl.se as usize;
                    if self.swstamp[se] != fno { self.swstamp[se] = fno; let sh = self.shadows[&se].as_ref().unwrap(); out.buffer_writes.extend(sh.gpu.shadow_globals_writes(&sh.mat, t, view.eye.to_array())); }
                    // PERF (2026-10-06): the placement's bounds (UE cm -> renderer metres, the (x, z, y) swap of `to_ue`): the renderer culls it per cascade
                    let bounds = [c.x / 100., c.z / 100., c.y / 100., r / 100.];
                    let own_mid = self.cur_mid.is_some() && self.shadows[&se].as_ref().is_some_and(|sh| sh.own);
                    if sl.sbkey != u32::MAX && own_mid {
                        self.mid_buffer(fx, se, s, t, out);
                        self.mid_sdraws.push((se, s, mesh, first, count, bounds));
                    } else if sl.sbkey != u32::MAX {
                        let bk = sl.sbkey as usize;
                        let (f0, b0) = self.sbstamp[bk];
                        let b = if f0 == fno { b0 as usize } else {
                            let (bse, bm, bf, bc) = self.sbkeys[bk];
                            let pipe = self.shadows.get(&bse).and_then(|s| s.as_ref()).map_or(0, |s| s.gpu.pipe.0);
                            self.sbatches.push(SBatch { se: bse, pipe, mesh: bm, first: bf, count: bc, ids: vec![], bounds: vec![] });
                            self.sbstamp[bk] = (fno, (self.sbatches.len() - 1) as u32);
                            self.sbatches.len() - 1
                        };
                        self.sbatches[b].ids.push(s as u32); self.sbatches[b].bounds.push(bounds);
                    } else {
                        let pb = self.slot_buffer(fx, s);
                        let sb = if own_mid {
                            let mb = self.mid_buffer(fx, se, s, t, out);
                            match self.mid_shadow_binds.get(&(se, s)) { Some(b) => b.clone(), None => { let sh = self.shadows[&se].as_ref().unwrap(); let b = sh.gpu.bind_variant_globals(fx, pb, Some(mb), None); self.mid_shadow_binds.insert((se, s), b.clone()); b } }
                        } else { match self.shadow_binds.get(&(se, s)) { Some(b) => b.clone(), None => {
                            let mb = self.entries[se].gpu.material_buf; let sh = self.shadows[&se].as_ref().unwrap();
                            let b = sh.gpu.bind_variant_globals(fx, pb, if sh.own { Some(mb) } else { None }, None); self.shadow_binds.insert((se, s), b.clone()); b } } };
                        let pipe = self.shadows[&se].as_ref().unwrap().gpu.pipe;
                        out.shadow_draws.push(FxShadowDraw { pipe, binds: sb, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, bounds: Some(bounds), instances: None });
                        st.shadow_draws += 1;
                    }
                }
            }
        }
    }

    /// PERF (2026-10-07): the [`SecSlot`] of section `sec_i` of LOD `li` of a kind (mirrored = the reversed-culling entry of a one-sided material, MIRRORFIX),
    /// its material entry and instanced batch key resolved on first use; also sizes the per-entry frame stamps.
    fn sec_slot(&mut self, fx: &mut FxRenderer, k: &KindGpu, li: usize, sec_i: usize, reverse: bool) -> SecSlot {
        let kid = k.kid as usize;
        let idx = (k.sec_base[li] as usize + sec_i) * 2 + reverse as usize;
        if self.secs.len() <= kid { self.secs.resize(kid + 1, vec![]); }
        let n = *k.sec_base.last().unwrap_or(&0) as usize * 2;
        if self.secs[kid].len() < n { self.secs[kid].resize(n, SecSlot::default()); }
        let s = self.secs[kid][idx];
        if s.state != 0 { return s; }
        let lod = &k.lods[li];
        let (slot_i, first, count) = lod.sections[sec_i];
        let e = (|| {
            let path = k.slots.get(slot_i).cloned().flatten()?;
            if debug_skipped(&path) { return None; }
            let e0 = self.entry(fx, &path, lod.num_uv, false).ok()?;
            // MIRRORFIX: reversed culling for a one-sided material of a mirrored placement
            Some(if reverse && !self.entries[e0].mat.flags.two_sided { self.entry(fx, &path, lod.num_uv, true).unwrap_or(e0) } else { e0 })
        })();
        let mut s = SecSlot { state: 1, sstate: 0, e: 0, bkey: u32::MAX, se: 0, sbkey: u32::MAX };
        if let Some(e) = e {
            s.state = 2; s.e = e as u32;
            if self.wstamp.len() < self.entries.len() { self.wstamp.resize(self.entries.len(), 0); }
            if self.entries[e].inst {
                let key = (e, lod.mesh.0, first, count);
                let nk = self.bkeys.len() as u32;
                let bk = *self.bkey_of.entry(key).or_insert(nk);
                if bk == nk { self.bkeys.push((e, lod.mesh, first, count)); self.bstamp.push((0, 0)); }
                s.bkey = bk;
            }
        }
        self.secs[kid][idx] = s;
        s
    }

    /// PERF (2026-10-07): the shadow half of a [`SecSlot`] (state 2 = a resolved entry), resolved on first use: the shadow entry of the section's material
    /// (FX SHADOW: its own shadow-depth pair, or the default material's position-only VS); every entry of the default material with the same pipeline shares one
    /// canonical entry (same program, same culling, no material buffer: the same depth), so their instances share one draw per mesh section.
    fn sec_shadow(&mut self, fx: &mut FxRenderer, k: &KindGpu, li: usize, sec_i: usize, reverse: bool) -> SecSlot {
        let idx = (k.sec_base[li] as usize + sec_i) * 2 + reverse as usize;
        let s = self.secs[k.kid as usize][idx];
        if s.sstate != 0 { return s; }
        let mut s = s;
        s.sstate = 1;
        if let Some(se) = self.shadow_entry(fx, s.e as usize) {
            let sh = self.shadows[&se].as_ref().unwrap();
            let canon = if sh.own { se } else { *self.shadow_canon.entry(sh.gpu.pipe.0).or_insert(se) };
            if self.swstamp.len() < self.entries.len() { self.swstamp.resize(self.entries.len(), 0); }
            s.sstate = 2; s.se = canon as u32;
            if self.shadows[&canon].as_ref().unwrap().inst {
                let (_, first, count) = k.lods[li].sections[sec_i];
                let key = (canon, k.lods[li].mesh.0, first, count);
                let nk = self.sbkeys.len() as u32;
                let bk = *self.sbkey_of.entry(key).or_insert(nk);
                if bk == nk { self.sbkeys.push((canon, k.lods[li].mesh, first, count)); self.sbstamp.push((0, 0)); }
                s.sbkey = bk;
            }
        }
        self.secs[k.kid as usize][idx] = s;
        s
    }

    /// The per-placement `Primitive` buffer of slot `s` (draws that are not instanced), created on first use from the slot's record.
    fn mid_buffer(&mut self, fx: &mut FxRenderer, e: usize, s: usize, t: fn_fx::uniform::MaterialTime, out: &mut FxFrame) -> FxBufId {
        let mut ov = self.entries[e].mat.overrides.clone();
        if let Some(m) = self.cur_mid.as_ref() {
            for (n, v) in &m.scalars { ov.scalars.insert((*n).to_owned(), *v); }
            for (n, v) in &m.vectors { ov.vectors.insert((*n).to_owned(), *v); }
        }
        let packed = fn_fx::uniform::evaluate(&self.entries[e].mat.uniform, &ov, t.world);
        let mut cb = packed.cb; cb.resize(cb.len().max(self.entries[e].gpu.material_registers).max(1), [0.0; 4]);
        let n = cb.len();
        let b = *self.mid_bufs.entry((e, s)).or_insert_with(|| fx.create_buffer("statics placement MID", n * 16));
        out.buffer_writes.push((b, bytes(&cb)));
        b
    }
    fn slot_buffer(&mut self, fx: &mut FxRenderer, s: usize) -> FxBufId {
        if let Some(b) = self.pool[s].prim { return b; }
        let b = fx.create_buffer("statics primitive", INST_F4 * 16);
        fx.write_buffer(b, &bytes(&self.inst_cpu[s * INST_F4..(s + 1) * INST_F4]));
        self.pool[s].prim = Some(b);
        b
    }

    /// INSTANCING (2026-10-07): the frame's instanced draws: the records written since the last frame go to the instance storage (grown, and its bind groups
    /// rebuilt, when the slots outgrow it), then one scene draw per (material entry, mesh section) with its instances' record indices, and one shadow draw per
    /// (shadow entry, mesh section) with each instance's bounds (the renderer keeps the per-cascade caster test per instance).
    fn flush_batches(&mut self, fx: &mut FxRenderer, view: &ViewParams, t: fn_fx::uniform::MaterialTime, st: &mut Stats, out: &mut FxFrame) {
        let need = self.pool.len().max(1);
        if self.inst_store.is_none() || need > self.inst_cap {
            let cap = (need * 2).next_power_of_two().max(1024);
            let b = fx.create_buffer("statics instance primitives", cap * INST_F4 * 16);
            if self.inst_cpu.len() < cap * INST_F4 { self.inst_cpu.resize(cap * INST_F4, [0.; 4]); }
            fx.write_buffer(b, &bytes(&self.inst_cpu[..need * INST_F4]));
            self.inst_store = Some(b); self.inst_cap = cap;
            self.inst_binds.clear(); self.inst_shadow_binds.clear(); self.mid_inst_binds.clear();
            self.inst_dirty.clear();
        }
        let store = self.inst_store.unwrap();
        if !self.inst_dirty.is_empty() {
            self.inst_dirty.sort_unstable(); self.inst_dirty.dedup();
            // contiguous runs of slots in one write each
            let mut i = 0;
            while i < self.inst_dirty.len() {
                let mut j = i + 1;
                while j < self.inst_dirty.len() && self.inst_dirty[j] == self.inst_dirty[j - 1] + 1 { j += 1; }
                let (a, b) = (self.inst_dirty[i] as usize, self.inst_dirty[j - 1] as usize + 1);
                fx.write_buffer_range(store, (a * INST_F4 * 16) as u64, &bytes(&self.inst_cpu[a * INST_F4..b * INST_F4]));
                i = j;
            }
            self.inst_dirty.clear();
        }
        // PERF: draws of one pipeline / material entry / mesh next to each other (the FX pass skips the repeated pipeline, bind group and buffer sets)
        let entries = &self.entries;
        self.batches.sort_unstable_by_key(|b| (entries[b.e].gpu.pipe.0, b.e, b.mesh.0, b.first));
        self.sbatches.sort_unstable_by_key(|b| (b.pipe, b.se, b.mesh.0, b.first));
        for bi in 0..self.batches.len() {
            let e = self.batches[bi].e;
            let vs = match self.entries[e].gpu.vs_globals().map(|g| g.float4s) {
                Some(n) => {
                    let b = *self.inst_vs.entry(e).or_insert_with(|| fx.create_buffer("statics base-pass VS globals (instanced)", n * 16));
                    if self.ivstamp.len() <= e { self.ivstamp.resize(self.entries.len().max(e + 1), 0); }
                    if !vs_once() || self.ivstamp[e] != self.frame_no {
                        self.ivstamp[e] = self.frame_no;
                        // velocity: PreviousLocalToWorld is read from each instance's LocalToWorld (the rewritten program); `Some` only sets SkipOutputVelocity 0
                        let prev = if self.entries[e].velocity { Some(Mat4::IDENTITY) } else { None };
                        if let Some(by) = self.entries[e].gpu.vs_globals_bytes(&self.entries[e].mat, t, prev) { out.buffer_writes.push((b, by)); }
                    }
                    Some(b)
                }
                None => None,
            };
            let binds = match self.inst_binds.get(&e) { Some(b) => b.clone(), None => { let b = self.entries[e].gpu.bind_variant_globals(fx, store, None, vs); self.inst_binds.insert(e, b.clone()); b } };
            let bt = &mut self.batches[bi];
            let ids = std::mem::take(&mut bt.ids);
            let n = ids.len() as u32;
            out.draws.push(FxDraw { pipe: self.entries[e].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh: bt.mesh, first_index: bt.first, index_count: bt.count }, instances: Some(FxInstances { bytes: bytemuck_u32(&ids), count: n }), sort: view.sort(bt.origin) });
            st.draws += 1; st.instanced += n;
        }
        for bi in 0..self.sbatches.len() {
            let se = self.sbatches[bi].se;
            let sb = match self.inst_shadow_binds.get(&se) { Some(b) => b.clone(), None => {
                let mb = self.entries[se].gpu.material_buf; let sh = self.shadows[&se].as_ref().unwrap();
                let b = sh.gpu.bind_variant_globals(fx, store, if sh.own { Some(mb) } else { None }, None); self.inst_shadow_binds.insert(se, b.clone()); b } };
            let pipe = self.shadows[&se].as_ref().unwrap().gpu.pipe;
            let bt = &mut self.sbatches[bi];
            let (ids, bounds) = (std::mem::take(&mut bt.ids), std::mem::take(&mut bt.bounds));
            st.shadow_draws += 1; st.shadow_instanced += ids.len() as u32;
            out.shadow_draws.push(FxShadowDraw { pipe, binds: sb, geom: FxGeom::MeshRange { mesh: bt.mesh, first_index: bt.first, index_count: bt.count }, bounds: None, instances: Some(FxShadowInstances { ids, bounds }) });
        }
        for (e, sl, mesh, first, count, origin) in std::mem::take(&mut self.mid_draws) {
            let vs = match self.entries[e].gpu.vs_globals().map(|g| g.float4s) {
                Some(n) => {
                    let b = *self.inst_vs.entry(e).or_insert_with(|| fx.create_buffer("statics base-pass VS globals (instanced)", n * 16));
                    if self.ivstamp.len() <= e { self.ivstamp.resize(self.entries.len().max(e + 1), 0); }
                    if !vs_once() || self.ivstamp[e] != self.frame_no {
                        self.ivstamp[e] = self.frame_no;
                        let prev = if self.entries[e].velocity { Some(Mat4::IDENTITY) } else { None };
                        if let Some(by) = self.entries[e].gpu.vs_globals_bytes(&self.entries[e].mat, t, prev) { out.buffer_writes.push((b, by)); }
                    }
                    Some(b)
                }
                None => None,
            };
            let mb = self.mid_bufs[&(e, sl)];
            let binds = match self.mid_inst_binds.get(&(e, sl, false)) { Some(b) => b.clone(), None => { let b = self.entries[e].gpu.bind_variant_globals(fx, store, Some(mb), vs); self.mid_inst_binds.insert((e, sl, false), b.clone()); b } };
            out.draws.push(FxDraw { pipe: self.entries[e].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, instances: Some(FxInstances { bytes: bytemuck_u32(&[sl as u32]), count: 1 }), sort: view.sort(origin) });
            st.draws += 1; st.instanced += 1;
        }
        for (se, sl, mesh, first, count, bounds) in std::mem::take(&mut self.mid_sdraws) {
            let mb = self.mid_bufs[&(se, sl)];
            let sb = match self.mid_inst_binds.get(&(se, sl, true)) { Some(b) => b.clone(), None => { let sh = self.shadows[&se].as_ref().unwrap(); let b = sh.gpu.bind_variant_globals(fx, store, Some(mb), None); self.mid_inst_binds.insert((se, sl, true), b.clone()); b } };
            let pipe = self.shadows[&se].as_ref().unwrap().gpu.pipe;
            st.shadow_draws += 1; st.shadow_instanced += 1;
            out.shadow_draws.push(FxShadowDraw { pipe, binds: sb, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, bounds: None, instances: Some(FxShadowInstances { ids: vec![sl as u32], bounds: vec![bounds] }) });
        }
        self.batches.clear(); self.sbatches.clear(); // (the batch stamps are per frame number)
    }

    pub fn report(&self) -> String {
        if self.log.is_empty() { return String::new(); }
        let s = &self.stats;
        format!("static materials (real cooked shaders): {} placements, {} drawn (LOD {:?}), {} frustum / {} distance / {} occlusion culled, {} draws ({} instances instanced), {} shadow draws ({} instances instanced), {} triangles\n{}  {}\n", s.placements, s.drawn, s.lod_counts, s.culled_frustum, s.culled_distance, s.culled_occlusion, s.draws, s.instanced, s.shadow_draws, s.shadow_instanced, s.triangles, self.run.report(), self.log.join("\n  "))
    }
}

// ---- app glue ----------------------------------------------------------------------------------------------------------------------------------
static STATE: Mutex<Option<StaticsRender>> = Mutex::new(None);
static TRIED: AtomicBool = AtomicBool::new(false);
/// Set when the handled set changed: scene.rs re-uploads the static instance list without these placements.
static CHANGED: AtomicBool = AtomicBool::new(false);
/// DEBUG (water agent, 2026-10-07): `FN_STATICS_SKIP=a,b` drops the scene + shadow draws of every section whose material path contains one of the
/// comma-separated substrings (tells which material is which surface at the same pose). Not a game feature.
fn debug_skipped(path: &str) -> bool {
    static SKIP: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    SKIP.get_or_init(|| fn_live::var("FN_STATICS_SKIP").map(|v| v.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect()).unwrap_or_default()).iter().any(|s| path.contains(s.as_str()))
}
/// PERF PROFILE (2026-10-07, debug-only): `FN_PERF_PROF=1` records the CPU time of named frame sections, printed (median / p95 / max) with the run report.
static PROF: Mutex<Vec<(&'static str, Vec<f32>)>> = Mutex::new(Vec::new());
fn prof_on() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_PERF_PROF").map_or(false, |v| v == "1")) }
pub fn prof<T>(label: &'static str, f: impl FnOnce() -> T) -> T {
    let t = std::time::Instant::now(); let r = f(); let ms = t.elapsed().as_secs_f32() * 1000.; frame_section(label, ms); prof_add(label, ms); r
}
thread_local! { static FRAME_SECTIONS: std::cell::RefCell<Vec<(&'static str, f32)>> = const { std::cell::RefCell::new(Vec::new()) }; }
fn frame_section(label: &'static str, ms: f32) { FRAME_SECTIONS.with(|v| { let mut v = v.borrow_mut(); match v.iter_mut().find(|e| e.0 == label) { Some(e) => e.1 += ms, None => v.push((label, ms)) } }); }
pub fn take_frame_sections() -> Vec<(&'static str, f32)> { FRAME_SECTIONS.with(|v| std::mem::take(&mut *v.borrow_mut())) }
thread_local! { static MATW: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((0., 0.)) }; }
/// PERF PROFILE (debug-only): consecutive sections of one function (`lap` records the time since the previous lap under `label`).
struct ProfLap(Option<std::time::Instant>);
impl ProfLap {
    fn new() -> Self { ProfLap(Some(std::time::Instant::now())) }
    fn lap(&mut self, label: &'static str) { if let Some(t) = self.0 { let ms = t.elapsed().as_secs_f32() * 1000.; frame_section(label, ms); prof_add(label, ms); self.0 = Some(std::time::Instant::now()); } }
}
fn prof_add(label: &'static str, ms: f32) { if !prof_on() { return; } if let Ok(mut g) = PROF.lock() { match g.iter_mut().find(|e| e.0 == label) { Some(e) => e.1.push(ms), None => g.push((label, vec![ms])) } } }
fn prof_report() -> String { PROF.lock().map(|g| g.iter().map(|(l, v)| { let mut s: Vec<f32> = v.iter().skip(10).copied().collect(); s.sort_by(|a, b| a.total_cmp(b)); let q = |p: f32| s.get(((s.len() as f32 * p) as usize).min(s.len().saturating_sub(1))).copied().unwrap_or(0.); format!("  prof {l:<28} median {:7.3} ms  p95 {:7.3}  max {:7.2}  ({} calls)\n", q(0.5), q(0.95), q(1.0), v.len()) }).collect()).unwrap_or_default() }
/// `FNRT_STATIC_MATERIALS=0`: keep every prop on the old static-mesh path.
pub fn enabled() -> bool { std::env::var("FNRT_STATIC_MATERIALS").map_or(true, |v| v != "0") }

/// LOAD TIME (2026-10-06): `Material::load` results parsed ahead of the first frame (path -> material), see [`prewarm`].
static PRELOADED: Mutex<Option<HashMap<String, Result<Arc<Material>, String>>>> = Mutex::new(None);
/// BROWSER PORT: POI kinds read on worker threads before the main thread builds them (fills [`PRELOADED`]).
#[cfg(target_arch = "wasm32")]
#[path = "../../../tools/web/fn-web/src/stream_web.rs"]
mod stream_web;
/// The textures [`prewarm`] predecoded (`fn_fx::gpu::predecode_texture`): what the first build drops if it did not use them.
static PREWARMED_TEXTURES: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The running [`prewarm`] thread; the first frame's build waits for it instead of parsing the same packages twice.
static PREWARM: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// A material [`prewarm`] parsed (each path is handed out once; a later request for the same path parses again, which no kind of 1.8 does).
fn take_preloaded(path: &str) -> Option<Result<Arc<Material>, String>> { PRELOADED.lock().ok()?.as_mut()?.remove(path) }

/// LOAD TIME (2026-10-06): the first real-shader frame built every kind's slot materials one after another on the UI thread, and `Material::load`
/// (the instance chain + the cooked shader map parse) is ~100-170 ms of that per material: 20 kinds / 21 materials froze the screen ~3.5-4 s on its last
/// loading frame ("Starting"). This parses those materials on a small pool while the loading screen runs; the first frame then only translates and
/// creates the pipelines. Same packages, same `Material` values, same build order: only the parse moves earlier. `kind_ids` = the props job's kinds.
pub fn prewarm(kind_ids: Vec<String>) {
    let (set, cat_dir) = {
        let Ok(mut g) = STATE.lock() else { return };
        let Some(s) = g.as_mut() else { return };
        if s.catalog().is_none() { return; }
        (s.set.clone(), s.props_dir.clone())
    };
    let h = std::thread::Builder::new().name("statics prewarm".into()).spawn(move || {
        let t0 = std::time::Instant::now();
        let Ok(cat) = fn_propdata::PropCatalog::load(&cat_dir) else { return };
        // the slot materials of each kind, resolved as `StaticsRender::kind` does (class overrides, else the mesh's own slot material)
        let mut paths: Vec<String> = vec![];
        for id in &kind_ids {
            let Some(kind) = cat.kinds.iter().find(|k| &k.id == id) else { continue };
            let long = fn_fx::data::long_package(&kind.mesh_ue_path).to_owned();
            let Ok((a, x, b)) = fn_fx::data::package_files(&set, &long) else { continue };
            let Ok(p) = fn_ueio::Package::parse(&a, &x, &b) else { continue };
            let Ok(m) = fn_mesh::StaticMesh::parse(&p) else { continue };
            let raw_mats = kind.raw.get("materials").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let nslots = m.materials.len().max(raw_mats.iter().filter_map(|v| v.get("slot").and_then(|s| s.as_u64())).max().map_or(0, |s| s as usize + 1));
            for s in 0..nslots {
                let slot = raw_mats.iter().find(|v| v.get("slot").and_then(|x| x.as_u64()) == Some(s as u64)).and_then(|v| v.get("ue_path").and_then(|p| p.as_str()).map(str::to_owned)).or_else(|| m.materials.get(s).and_then(|x| x.material.clone()));
                if let Some(path) = slot { if !paths.contains(&path) { paths.push(path); } }
            }
        }
        if !paths.iter().any(|p| p == fn_fx::gpu::DEFAULT_SURFACE_MATERIAL) { paths.push(fn_fx::gpu::DEFAULT_SURFACE_MATERIAL.to_owned()); }
        let n = paths.len();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let out: Mutex<HashMap<String, Result<Arc<Material>, String>>> = Mutex::new(HashMap::new());
        std::thread::scope(|sc| {
            for _ in 0..std::thread::available_parallelism().map_or(4, |p| p.get()).clamp(2, 6).min(n.max(1)) {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(i) else { break };
                    let r = Material::load(&set, path).map(Arc::new);
                    if let Ok(mut o) = out.lock() { o.insert(path.clone(), r); }
                });
            }
        });
        let out = out.into_inner().unwrap_or_default();
        let t_mats = t0.elapsed();
        // the 2D textures those materials bind (`Texture2D_k` slots): their stored mip chains decoded here, so the first frame's `TextureCache::load` only uploads
        let mut textures: Vec<String> = vec![];
        for m in out.values().flatten() { for k in 0..m.uniform.textures2d.len() { if let Some((p, _)) = m.texture_slot(k) { if !textures.contains(&p) { textures.push(p); } } } }
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..std::thread::available_parallelism().map_or(4, |p| p.get()).clamp(2, 6).min(textures.len().max(1)) {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = textures.get(i) else { break };
                    fn_fx::gpu::predecode_texture(&set, path);
                });
            }
        });
        eprintln!("[statics] prewarm: {} slot materials of {} kinds parsed in {:.0} ms, {} textures decoded in {:.0} ms", out.len(), kind_ids.len(), t_mats.as_secs_f32() * 1000., textures.len(), (t0.elapsed() - t_mats).as_secs_f32() * 1000.);
        if let Ok(mut g) = PREWARMED_TEXTURES.lock() { *g = textures; }
        if let Ok(mut g) = PRELOADED.lock() { let m = g.get_or_insert_with(HashMap::new); for (k, v) in out { m.entry(k).or_insert(v); } }
    });
    if let (Ok(h), Ok(mut g)) = (h, PREWARM.lock()) { *g = Some(h); }
}
/// ALL POIS: per-frame budget (ms) for building the kinds of newly resident POI groups in the live game. CHOSEN: 6 ms (at least one kind per frame; a kind
/// with new materials takes ~50 ms, so a group coming into range costs a few frames of hitching: the shader translation is not asynchronous).
const POI_BUILD_MS: f32 = 6.;
const KEEP_EVERY: u32 = 30;
const KEEP_NEAR_CM: f32 = 15000.;
/// ALL POIS (CULLING): an off-screen POI placement nearer than this (cm, from its bounds) is still submitted for the shadow cascades. CHOSEN: the CSM radius
/// `GetCSMMaxDistance` at its largest (DynamicShadowDistanceMovableLight 4500 cm x r.Shadow.DistanceScale 2, fn-render lighting.rs) + 50 m for casters
/// standing toward the sun (UE extrudes each cascade's caster volume toward the light).
const POI_CSM_CM: f32 = 9000. + 5000.;
impl StaticsRender {
    /// ALL POIS (LOAD TIME): the POI groups resident around the spawn (`fn_game::spawn`) built under the loading screen; true when they all are.
    fn poi_prebuild(&mut self, fx: &mut FxRenderer, budget: std::time::Duration) -> bool {
        let Some(poi) = crate::poi::current() else { return true };
        let eye = spawn_eye();
        let _ = self.poi_build(fx, &poi, eye, Some(budget), Some(first_cm()));
        let (done, total) = first_frame_progress(&poi, eye, &self.kinds);
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if done >= total && !LOGGED.swap(true, Ordering::Relaxed) { eprintln!("[load] first-frame POI set built: {total} kinds (groups within {:.0} m + HLOD proxies), {} FX pipelines still compiling", first_cm() / 100., fx.pending_pipelines()); }
        done >= total
    }
}
/// Waits for a running [`prewarm`] (the first frame's build calls this before it parses anything itself).
fn wait_prewarm() { let h = PREWARM.lock().ok().and_then(|mut g| g.take()); if let Some(h) = h { let t = std::time::Instant::now(); let _ = h.join(); let ms = t.elapsed().as_secs_f32() * 1000.; if ms > 1. { eprintln!("[statics] waited {ms:.0} ms for the material prewarm"); } } }

/// Opens the paks + the props catalog once (no-op without them or when disabled).
pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    if !enabled() || TRIED.swap(true, Ordering::Relaxed) { return; }
    let (Some(install), Some(dir)) = (install, crate::props::find_dir(roots)) else { eprintln!("[statics] no install / props folder: props keep the old static-mesh path"); return };
    match StaticsRender::new(&install, &dir) {
        Ok(s) => {
            *STATE.lock().unwrap() = Some(s);
            // VRAM (2026-10-07): this path shades the props, so their textured static-mesh copy keeps only the custom depth's textures (fn_render::static_loader::
            // set_prop_textures_depth_only; a kind this path then fails is re-uploaded in full, `crate::props::upgrade_depth_only_kinds`). `FN_PROPS_DEDUP=0`: full copy.
            if fn_live::var("FN_PROPS_DEDUP").map_or(true, |v| v != "0") { fn_render::static_loader::set_prop_textures_depth_only(true); }
        }
        Err(e) => eprintln!("[statics] unavailable: {e}"),
    }
}
/// VRAM: the layout kinds are all processed (built, or left on the old path) — from then on [`handles`] is final for them.
pub fn props_built() -> bool { STATE.lock().ok().map_or(false, |g| g.as_ref().map_or(false, |s| s.prebuilt)) }
/// VRAM: the catalog entry of a prop kind (the full re-upload of a kind this path does not draw).
pub fn catalog_kind(id: &str) -> Option<fn_propdata::PropKind> { STATE.lock().ok().and_then(|mut g| g.as_mut().and_then(|s| s.catalog().and_then(|c| c.kinds.iter().find(|k| k.id == id).cloned()))) }
/// LOAD TIME (2026-10-06): builds the real-shader kinds a few at a time while the loading screen is up (about `budget_ms` per call, at least one kind),
/// so the first in-game frame has none left to build. Runs in the Finishing stage of both the exe and the browser build: the screen stays live instead
/// of freezing on its last loading frame ("Starting"). Returns true when everything is built, or when there is nothing to build.
pub fn prebuild_step(fx: &mut FxRenderer, props: &[fn_combat::Harvestable], real: Option<&crate::props::Real>, budget_ms: f32) -> bool {
    let Some(real) = real else { return true };
    let Ok(mut g) = STATE.lock() else { return true };
    match g.as_mut() { Some(s) => { s.init_gpu(fx); let b = std::time::Duration::from_secs_f32(budget_ms.max(0.) / 1000.); s.build_kinds(fx, props, real, Some(b)) && s.poi_prebuild(fx, b) } None => true }
}
/// LOBBY PREPARE (2026-10-07): only the POI part of [`prebuild_step`] (the groups resident at the spawn), which needs no placed props: run while the
/// lobby (game picker) is up once the world data has loaded, so the match's loading screen has little or nothing of it left. True when done.
pub fn poi_prebuild_step(fx: &mut FxRenderer, budget_ms: f32) -> bool {
    let Ok(mut g) = STATE.lock() else { return true };
    match g.as_mut() { Some(s) => { s.init_gpu(fx); s.poi_prebuild(fx, std::time::Duration::from_secs_f32(budget_ms.max(0.) / 1000.)) } None => true }
}
/// LOADING SCREEN: (POI kinds built, POI kinds of the resident groups) for the progress of the "preparing the world" step.
pub fn poi_prepare_progress() -> (usize, usize) {
    let (Some(poi), Ok(g)) = (crate::poi::current(), STATE.lock()) else { return (0, 0) };
    let Some(s) = g.as_ref() else { return (0, 0) };
    first_frame_progress(&poi, spawn_eye(), &s.kinds)
}
fn spawn_eye() -> Vec3 {
    let (x, _, z, _, _) = fn_game::spawn();
    fn_fx::view::to_ue(Vec3::new(x, fn_terrain::ground_y(x, z) + 2., z)).as_vec3()
}
fn first_cm() -> f32 { static V: std::sync::OnceLock<f32> = std::sync::OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_POI_FIRST_M").or_else(|_| fn_live::var("FNWEB_POI_START_M")).ok().and_then(|v| v.parse::<f32>().ok()).map_or(6000., |m| m * 100.)) }
fn fallback_clusters(poi: &crate::poi::PoiSet) -> Vec<bool> {
    let mut v = vec![false; poi.clusters.len()];
    for g in poi.groups.iter().filter(|g| !g.ready.load(Ordering::Relaxed)) { for &c in &g.clusters { if let Some(x) = v.get_mut(c as usize) { *x = true; } } }
    v
}
fn first_frame_progress<V>(poi: &crate::poi::PoiSet, eye: Vec3, kinds: &HashMap<String, V>) -> (usize, usize) {
    let first = first_cm();
    let mut seen = HashSet::new();
    let (mut done, mut total) = (0, 0);
    for gr in poi.groups.iter().filter(|gr| gr.resident.load(Ordering::Relaxed) && (gr.center - eye).length() - gr.radius < first) {
        for &k in &gr.kinds { if seen.insert(k as usize) { total += 1; if kinds.contains_key(&poi.kinds[k as usize].id) { done += 1; } } }
    }
    let fb = fallback_clusters(poi);
    for (i, c) in poi.clusters.iter().enumerate() {
        if !((fb[i] || c.proxy_visible(eye)) && proxy_in_cap(c, eye)) { continue; }
        if seen.insert(c.kind) { total += 1; if kinds.contains_key(&poi.kinds[c.kind].id) { done += 1; } }
    }
    (done, total)
}
/// The frame of the real-shader props (no-op before [`init`] / without real props).
pub fn frame(fx: &mut FxRenderer, props: &[fn_combat::Harvestable], real: Option<&crate::props::Real>, view: &ViewParams, out: &mut FxFrame) {
    let Some(real) = real else { return };
    let Ok(mut g) = STATE.lock() else { return };
    if let Some(s) = g.as_mut() { s.frame(fx, props, real, view, out); }
}
/// Is this static-mesh handle drawn by the real-shader path (scene.rs leaves it out of the old static draw)?
pub fn handles(h: fn_render::StaticMeshHandle) -> bool { STATE.lock().ok().map_or(false, |g| g.as_ref().map_or(false, |s| s.handled.contains(&h))) }
/// The handled set changed since the last call (scene.rs re-uploads its static instance list).
pub fn take_changed() -> bool { CHANGED.swap(false, Ordering::Relaxed) }
pub fn handled_meshes() -> Vec<fn_render::StaticMeshHandle> { STATE.lock().ok().and_then(|g| g.as_ref().map(|s| s.handled.iter().copied().collect())).unwrap_or_default() }
pub fn report() -> String { format!("{}{}", prof_report(), STATE.lock().ok().and_then(|g| g.as_ref().map(|s| format!("{}{}", s.report(), s.grass.report()))).unwrap_or_default()) }
pub fn stats() -> Option<Stats> { STATE.lock().ok().and_then(|g| g.as_ref().map(|s| s.stats.clone())) }

/// GRASS (2026-10-06): this frame's ground cover on its real cooked material (grass_render.rs); no-op before [`init`] / without the foliage export. When the
/// path comes up the handled set changed: scene.rs re-uploads its static list without the old grass cards ([`take_changed`], [`grass_active`]).
pub fn grass_frame(fx: &mut FxRenderer, grass: Option<&crate::foliage::Grass>, view: &ViewParams, view_distance_scale: f32, out: &mut FxFrame) {
    let Some(grass) = grass else { return };
    let Ok(mut g) = STATE.lock() else { return };
    let Some(s) = g.as_mut() else { return };
    s.init_gpu(fx);
    let shared = s.shared.unwrap();
    let StaticsRender { set, cache, grass: gr, .. } = s;
    if gr.frame(set, cache, fx, shared, grass, view, view_distance_scale, out) { CHANGED.store(true, Ordering::Relaxed); }
}
pub fn grass_prebuild(fx: &mut FxRenderer, grass: Option<&crate::foliage::Grass>) {
    let Some(grass) = grass else { return };
    let Ok(mut g) = STATE.lock() else { return };
    let Some(s) = g.as_mut() else { return };
    s.init_gpu(fx);
    let shared = s.shared.unwrap();
    let StaticsRender { set, cache, grass: gr, .. } = s;
    if gr.prebuild(set, cache, fx, shared, grass) { CHANGED.store(true, Ordering::Relaxed); }
}
/// GRASS: the ground cover is drawn by the real-shader path (scene.rs leaves the blades out of the old static draw).
pub fn grass_active() -> bool { STATE.lock().ok().map_or(false, |g| g.as_ref().map_or(false, |s| s.grass.active())) }
pub fn grass_stats() -> Option<crate::grass_render::GrassStats> { STATE.lock().ok().and_then(|g| g.as_ref().map(|s| s.grass.stats.clone())) }

#[cfg(test)]
mod tests {
    use super::*;
    fn install() -> Option<PathBuf> { fn_live::var_os("FN_TEST_ROOT").map(PathBuf::from).filter(|p| p.is_dir()) }
    fn props_dir() -> Option<PathBuf> {
        let d = fn_live::var_os("FN_PROPS_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3).unwrap().join("FortniteRuntime").join("assets").join("props"));
        d.join("catalog.json").is_file().then_some(d)
    }

    /// The FX-space transform is the game transform conjugated by the (x, z, y) * 100 axis map, mirror and yaw included.
    #[test] fn local_to_world_matches_the_static_path() {
        for (yaw, mirror) in [(0.3f32, false), (-2.1, true), (1.57, false)] {
            let (pos, scale) = (Vec3::new(3., 1., -7.), 1.3);
            let l2w = local_to_world(pos, yaw, scale, mirror);
            let local_ue = Vec3::new(120., -40., 300.);
            // static_common.wgsl: world = R_y(yaw) * (p * m * scale) + pos, with p = the glTF vertex = (x, z, y) * 0.01 of the UE vertex
            let p = Vec3::new(local_ue.x, local_ue.z, local_ue.y) * 0.01;
            let m = if mirror { Vec3::new(-1., 1., 1.) } else { Vec3::ONE };
            let game = glam::Quat::from_rotation_y(yaw) * (p * m * scale) + pos;
            let want = fn_fx::view::to_ue(game).as_vec3();
            let got = l2w.transform_point3(local_ue);
            assert!((want - got).length() < 1e-2, "{yaw} {mirror}: {want} vs {got}");
        }
    }

    /// ComputeStaticMeshLOD walks from the last LOD down: far = coarse, near = LOD 0.
    #[test] fn lod_selection_follows_screen_size() {
        let ss = [1.0, 0.5, 0.25, 0.1, 0., 0., 0., 0.];
        let th = (1.0, 0.5625);
        assert_eq!(static_mesh_lod(&ss, 4, Vec3::ZERO, 500., Vec3::new(300., 0., 0.), th, 1.), 0);
        assert_eq!(static_mesh_lod(&ss, 4, Vec3::ZERO, 500., Vec3::new(1e6, 0., 0.), th, 1.), 3);
        assert_eq!(static_mesh_lod(&ss, 2, Vec3::ZERO, 500., Vec3::new(1e6, 0., 0.), th, 1.), 1, "clamped to the mesh's LOD count");
    }
    /// ComputeLODForMeshes: ScreenSize x LODScale (r.StaticMeshLODDistanceScale x FOV / 90) against the screen radius, `>=`: a FOV below 90 keeps the finer LOD
    /// farther out (the distance where LOD i starts scales by 1 / LODScale).
    #[test] fn lod_scale_moves_the_switch_distance() {
        let ss = [1.0, 0.5, 0.25, 0.1, 0., 0., 0., 0.];
        let th = (1.0, 1.0);
        // screen radius = 0.5 * radius / dist (multiple 0.5 / tan 1.0 on both axes): LOD 1 starts where 0.5 * ScreenSize[1] = 0.25 >= 0.5 * 500 / d, i.e. d >= 1000
        assert_eq!(static_mesh_lod(&ss, 4, Vec3::ZERO, 500., Vec3::new(1000., 0., 0.), th, 1.), 1, ">= at the boundary");
        assert_eq!(static_mesh_lod(&ss, 4, Vec3::ZERO, 500., Vec3::new(1000., 0., 0.), th, 80. / 90.), 0, "FOV 80: LOD 0 until 1125");
        assert_eq!(static_mesh_lod(&ss, 4, Vec3::ZERO, 500., Vec3::new(1130., 0., 0.), th, 80. / 90.), 1);
    }

    /// Every material of the current layout's kinds loads with the static-mesh base-pass pair and the scene-lit assembly succeeds (the audit of
    /// docs/spec/material_coverage.md as a test).
    #[test] fn layout_materials_have_cooked_base_pass_programs() {
        let (Some(root), Some(dir)) = (install(), props_dir()) else { eprintln!("FN_TEST_ROOT / props not available: skipped"); return };
        let set = PakSet::open_install(&root).unwrap();
        let cat = fn_propdata::PropCatalog::load(&dir).unwrap();
        let lib = crate::props::Library::from_catalog_poi(&cat, crate::props::DEFAULT_RADIUS_M, false);
        let ids = lib.kinds_needed(crate::props::Mode::Layout);
        assert!(ids.len() >= 10, "{ids:?}");
        let prelude = fn_render::fx::default_scene_lit_prelude();
        let mut n = 0;
        for id in &ids {
            let k = cat.kinds.iter().find(|k| &k.id == id).unwrap();
            for m in k.raw["materials"].as_array().unwrap() {
                let path = m["ue_path"].as_str().unwrap();
                let mat = Material::load(&set, path).unwrap_or_else(|e| panic!("{id}: {path}: {e}"));
                let vs = mat.cooked.find(Vf::Local.type_name(), VS_TYPE).unwrap_or_else(|| panic!("{path}: no VS"));
                let ps = mat.cooked.find(Vf::Local.type_name(), PS_TYPES[0]).unwrap_or_else(|| panic!("{path}: no PS"));
                assert!(mat.flags.uses_gbuffer(), "{path}: {:?}", mat.flags);
                let clip = if mat.flags.blend == "BLEND_Masked" { mat.cooked.find(Vf::Local.type_name(), "FDepthOnlyPS") } else { None };
                fn_fx::program::assemble_scene_lit(vs, ps, clip, &prelude).unwrap_or_else(|e| panic!("{path}: {e}"));
                n += 1;
            }
        }
        eprintln!("{} kinds, {n} slot materials: all assemble", ids.len());
    }
}
