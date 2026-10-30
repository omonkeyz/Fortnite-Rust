//! BUILDING RENDER (owner: visuals agent): the placed Athena pieces, their build-up / damage animation, the blueprint ghost, the placement preview, the edit tiles,
//! the construction / destruction effects and the building sounds, all with the ORIGINAL cooked data. The logic (state machine, parameter names, cue tables, every
//! exe source) is `fn_building::visual`; this file turns it into draws of fn-render's FX pass and `SoundEvent`s.
//!
//! Materials: the base-pass programs of the cooked shader maps translated from DXBC (`fn-dxbc` -> `fn_fx::gpu::build_material_layout`), the `FLocalVertexFactory`
//! permutation (static meshes). Player-piece materials are static-permutation instances (`Wood_lvl1_Wall` overrides the blend mode of `Builder_2`, its package carries
//! its OWN shader map, ~1 MB): [`load_material`] takes the programs and the uniform-expression set of the first package of the instance chain that has a shader map,
//! so the instance's own permutation runs, not the root's. MID parameters (`BuildAnimPhase`, ...) are written into the instance's `Material` constant buffer.
//! Pass: `TBasePassVSFNoLightMapPolicy` + `TBasePassPSFNoLightMapPolicySkylight` (a movable, runtime-spawned static mesh has no lightmap; the Athena world has a sky
//! light), falling back to `TBasePassPSFNoLightMapPolicy` when the map has no sky-light permutation (logged).
//!
//! FX SHADOW (2026-10-05): placed pieces cast into every CSM cascade with their material's cooked `TShadowDepthVS` / `TShadowDepthPS` (same Primitive / Material
//! buffers, so the build-up WPO shows in the shadow) and join the distance-field scene (DF shadows, DFAO); previews / edit actors cast nothing (see `frame`).
//! FX RECEIVE (2026-10-05): opaque / masked lit piece materials are resolved in the renderer's scene pass with the static meshes' lighting (CSM PCF + DF
//! shadow, light function, DFAO sky diffuse, sky reflection + SSR, fog: `fn_fx::program::assemble_scene_lit`, fn-render fx_resolve.wgsl), so pieces RECEIVE
//! the cascades (each other's, the trees', the terrain's); translucent lit ones (edit tiles) keep the translucency volume.
//! LIMITS (named, not faked): the edit stair indicator scalars (`AuxVisual::scalars`) are written as `sineOffset` / `isblinkingSign` / `isX` (editing agent).
use fn_audio_sink::SoundEvent;
use fn_building::edit::EditVisuals;
use fn_building::visual::{self as vis, AnimEvent, GameDataFx, PieceAnim, PieceFrame, PieceVisualDef, Surface};
use fn_building::world::{BuildingWorld, PieceId, PieceKind, WorldEvent};
use fn_fx::gpu::{build_material_layout, load_mesh, LayoutOpts, MaterialGpu, MeshGpu, SharedBuffers, TextureCache, Vf};
use fn_fx::material::Material;
use fn_fx::uniform::{Expr, Overrides};
use fn_fx::view::{bytes, sprite_vf_buffer, ViewParams, VIEW_FLOAT4S};
use fn_pak::PakSet;
use fn_particles::assets::Catalog;
use fn_particles::{Instance, Transform};
use fn_render::fx::*;
use glam::{Mat3, Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const VS_TYPE: &str = "TBasePassVSFNoLightMapPolicy";
pub const PS_TYPES: [&str; 2] = ["TBasePassPSFNoLightMapPolicySkylight", "TBasePassPSFNoLightMapPolicy"];
/// Key offset of the construction-loop sound channels in the sink (`SoundEvent::BuildingLoop`): piece id + this.
pub const LOOP_KEY_BASE: u32 = 0x4000_0000;

/// The placement preview (`AFortPlayerController::BuildPreviewMarker`) the core / input agents report every frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub class_path: String,
    /// Actor location (UE cm, Z up) and yaw (degrees).
    pub location: Vec3,
    pub yaw_deg: f32,
    pub mirrored: bool,
    /// Placement allowed (blue) or refused (red).
    pub valid: bool,
}

/// The material of a piece instance chain with its OWN shader map (see the module doc). `path` = object path.
/// MIRRORFIX (2026-10-05): `fn_fx::material::Material::load` now does what this function patched in after loading: the `bOverride_*`-gated base property
/// overrides (`UMaterialInstance::UpdateOverridableBaseProperties`, MaterialInstance.cpp:1550; `Wood_lvl1_Static` = `bOverride_BlendMode` without a value =
/// `BLEND_Opaque`) and the child-most static-permutation shader map (`UMaterialInstance::GetMaterialResource`, :1362), for every FX user.
pub fn load_material(set: &PakSet, path: &str) -> Result<Material, String> { Material::load(set, path) }

/// Effective vector / scalar / texture parameter of a material (instance chain override, else the expression default of its uniform set) -- what
/// `UMaterialInterface::GetVectorParameterValue` / `GetScalarParameterValue` / `GetTextureParameterValue` return (used by `ExtractBlueprintParamValuesFromAnimMat`).
fn find_vector(m: &Material, name: &str) -> Option<[f32; 4]> {
    if let Some(v) = m.overrides.vectors.get(name) { return Some(*v); }
    fn walk(e: &Expr, name: &str) -> Option<[f32; 4]> {
        match e {
            Expr::VectorParameter { name: n, default } if n == name => Some(*default),
            Expr::ComponentSwizzle { e, .. } | Expr::Unary(_, e) => walk(e, name),
            Expr::FoldedMath { a, b, .. } | Expr::AppendVector { a, b, .. } | Expr::Max(a, b) | Expr::Min(a, b) | Expr::Fmod(a, b) => walk(a, name).or_else(|| walk(b, name)),
            Expr::TrigMath { x, y, .. } => walk(x, name).or_else(|| walk(y, name)),
            Expr::Clamp { x, lo, hi } => walk(x, name).or_else(|| walk(lo, name)).or_else(|| walk(hi, name)),
            _ => None,
        }
    }
    m.uniform.vectors.iter().chain(m.uniform.per_frame_vectors.iter()).find_map(|e| walk(e, name))
}
fn find_scalar(m: &Material, name: &str) -> Option<f32> {
    if let Some(v) = m.overrides.scalars.get(name) { return Some(*v); }
    fn walk(e: &Expr, name: &str) -> Option<f32> {
        match e {
            Expr::ScalarParameter { name: n, default } if n == name => Some(*default),
            Expr::ComponentSwizzle { e, .. } | Expr::Unary(_, e) => walk(e, name),
            Expr::FoldedMath { a, b, .. } | Expr::AppendVector { a, b, .. } | Expr::Max(a, b) | Expr::Min(a, b) | Expr::Fmod(a, b) => walk(a, name).or_else(|| walk(b, name)),
            Expr::TrigMath { x, y, .. } => walk(x, name).or_else(|| walk(y, name)),
            Expr::Clamp { x, lo, hi } => walk(x, name).or_else(|| walk(lo, name)).or_else(|| walk(hi, name)),
            _ => None,
        }
    }
    m.uniform.scalars.iter().chain(m.uniform.per_frame_scalars.iter()).find_map(|e| walk(e, name))
}
fn find_texture(m: &Material, name: &str) -> Option<String> {
    if let Some(t) = m.texture_overrides.get(name) { return Some(t.clone()); }
    m.uniform.textures2d.iter().find_map(|e| match e { Expr::TextureParameter { name: n, index, .. } if n == name => m.referenced_textures.get(*index as usize).cloned(), _ => None })
}

/// `ExtractBlueprintParamValuesFromAnimMat` (@14046a0d0): the `Blueprint_*` values of the animating material that go into the blueprint MID.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlueprintParams { pub texture: Option<String>, pub channels: [Option<[f32; 4]>; 4], pub grid: Option<f32> }
pub fn blueprint_params(anim: &Material) -> BlueprintParams {
    BlueprintParams {
        texture: find_texture(anim, vis::param::BLUEPRINT_TEXTURE),
        channels: std::array::from_fn(|i| find_vector(anim, vis::param::BLUEPRINT_CHANNELS[i])),
        grid: find_scalar(anim, vis::param::BLUEPRINT_GRID_MULTIPLIER),
    }
}

/// `CDO` visual data of a building class (`/Game/.../PBWA_W1_Solid.PBWA_W1_Solid_C`).
pub fn load_def(set: &PakSet, class_path: &str) -> Result<PieceVisualDef, String> {
    let long = fn_fx::data::long_package(class_path).to_owned();
    let pkg = fn_fx::data::PackageJson::load(set, &long)?;
    let cdo = pkg.exports.iter().find(|e| e.name.starts_with("Default__")).ok_or_else(|| format!("{long}: no CDO"))?;
    PieceVisualDef::from_cdo(class_path, &cdo.props)
}

/// The static mesh of any building class (any edit variant): what the editing agent asked for as `mesh_for(piece_kind, tier, edit_variant)`. Build the class
/// path with `fn_building::visual::piece_class(material, level, variant)` (or take it from `EditDb`).
pub fn mesh_for(set: &PakSet, class_path: &str) -> Result<String, String> { load_def(set, class_path).map(|d| d.static_mesh) }

/// `mirrored`: the same material built with reversed culling (see [`BuildingRender::reverse_culling_entry`]); `num_uv` to build it.
struct MatEntry { mat: Arc<Material>, gpu: MaterialGpu, ps: &'static str, num_uv: usize, front_cw: bool }
struct Slot { prim: FxBufId, mat: FxBufId }
struct PieceState { class: String, def: Arc<PieceVisualDef>, anim: PieceAnim, kind: PieceKind, location: Vec3, yaw_deg: f32, mirrored: bool, max_health: f32,
    /// HUD FIX2: the piece is being edited: `ABuildingSMActor::vfunc_337` @140481fd0 hid its mesh (vfunc_292(false): StaticMeshComponent SetVisibility(false)) and
    /// its blueprint mesh (vfunc_390(false)); `vfunc_338` @140482180 shows them again at the end of the edit.
    edit_hidden: bool }
struct Fx { catalog: Arc<Catalog>, dxbc: crate::pickups_dxbc::Dxbc, live: Vec<Instance>, no_draw: HashSet<String>, serial: u64 }

/// One pending translucent draw (sorted back to front).
struct Pending { dist: f32, draw: FxDraw }

pub struct BuildingRender {
    set: Arc<PakSet>,
    install: PathBuf,
    asset_roots: Vec<PathBuf>,
    gpu_ready: bool,
    cache: TextureCache,
    shared: Option<SharedBuffers>,
    fade_buf: Option<FxBufId>,
    defs: HashMap<String, Result<Arc<PieceVisualDef>, String>>,
    meshes: HashMap<String, Result<MeshGpu, String>>,
    entries: Vec<MatEntry>,
    index: HashMap<String, Result<usize, String>>,
    /// MIRRORFIX: entry -> its reversed-culling twin (`reverse_culling_entry`).
    mirrored: HashMap<usize, Result<usize, String>>,
    /// PERF FIX: `ExtractBlueprintParamValuesFromAnimMat` result per class path (the animating material loaded once, see `blueprint_entry`).
    bp_params: HashMap<String, BlueprintParams>,
    /// PERF FIX: the `DeathParticleSocketName` socket's `RelativeLocation` per (mesh, socket), read once from the mesh package (see `death_offset`).
    sockets: HashMap<(String, String), Option<Vec3>>,
    /// CAMERA CUT: per (material entry, slot) base-pass VS globals buffer (`PreviousLocalToWorld` / `SkipOutputVelocity`, see `draw_mesh`).
    vs_bufs: HashMap<(usize, usize), FxBufId>,
    /// CAMERA CUT: `View.bCameraCut` of the current frame (set by [`camera_cut`] for the next frame).
    camera_cut: bool,
    cut_pending: bool,
    pool: Vec<Slot>,
    used: usize,
    binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    pieces: HashMap<PieceId, PieceState>,
    cursor: u64,
    fx: Option<Fx>,
    fx_tried: bool,
    time: f32,
    /// SORTFIX: this frame's view (`frame`): the translucency sort origin and the material clocks.
    view: Option<ViewParams>,
    /// World (movement-scene cm) -> FX space translation of this frame (`building_hook::world_to_fx`).
    offset: Vec3,
    warned: HashSet<String>,
    pub log: Vec<String>,
    /// FX SHADOW (2026-10-05): per material entry its shadow-depth programs (`fn_fx::gpu::build_shadow`; `None` = casts nothing / not buildable), the
    /// per-(entry, slot) shadow bind groups, the default surface material of the opaque override, the cooked distance field per mesh, the entries whose shadow
    /// globals were written this frame, and whether the current `draw_mesh` call casts (only the placed pieces: see `frame`).
    shadows: HashMap<usize, Option<ShadowEntry>>,
    shadow_binds: HashMap<(usize, usize), Vec<(u32, FxBindId)>>,
    default_mat: Option<Option<Arc<Material>>>,
    /// FX SHADOW SELF-SHADOW BIAS: per mesh (its own `DistanceFieldSelfShadowBias`, the animating-state override with [`ANIMATING_DF_SELF_SHADOW_BIAS`]).
    dfs: HashMap<String, Option<(Arc<fn_render::distance_field::MeshDistanceField>, Arc<fn_render::distance_field::MeshDistanceField>)>>,
    shadow_written: HashSet<usize>,
    casting: bool,
    /// FX SHADOW SELF-SHADOW BIAS: the world time of the current frame (`vfunc_320`'s damage-visuals state needs it, see `draw_piece`).
    now: f64,
    /// BVISFIX (2026-10-05): the `UFortGameData` curves of the per-frame MID drive (loaded once), the damage hits reported by the hit tests per piece
    /// (world cm impact, unit vector towards the attacker) waiting for their `HealthChanged`, and the pieces playing their destruction animation after the
    /// world removed them (OnDeath `vfunc_277` -> `EBA_Destruction`, hidden at its end by `vfunc_410`).
    gamedata: Option<Arc<GameDataFx>>,
    pending_hits: HashMap<PieceId, std::collections::VecDeque<(Vec3, Vec3)>>,
    dying: Vec<PieceState>,
    /// NBOUNCE (2026-10-05): per class path its cooked collision convexes (actor-local cm, `fn_building::pieces::load_class`), for the `WallBounceOverlap`
    /// sphere query of `vfunc_314` (`None` = the class did not load: it is never overlapped, logged once).
    bounce_convexes: HashMap<String, Option<Vec<Vec<Vec3>>>>,
    /// FIRST USE (2026-10-06): the warm-up queue of [`prebuild_step`] (`None` = not started) and when it started.
    warm: Option<std::collections::VecDeque<Warm>>,
    warm_t0: Option<std::time::Instant>,
}

/// FIRST USE (2026-10-06): what [`BuildingRender::warm_step`] builds before the first placement. The user saw a freeze at every new piece type / material
/// (browser: 150-510 ms of material work plus the GPU process' pipeline compile per piece, `verification/browser/first_use1`; native 30-55 ms,
/// `verification/first_use/run2`): the first draw of a piece built its materials (WGSL + pipelines + textures), shadow-depth programs, mirrored-culling
/// variant, blueprint MID, damage overlay and distance field, and the first construction effect its particle materials.
enum Warm { Init, Class(String), Edit, Effect(String), Door(String, Option<String>) }
/// The view of the warm-up's scratch draws (only their sort keys read it; nothing is presented).
const WARM_VIEW: ViewParams = ViewParams { eye: Vec3::ZERO, forward: Vec3::NEG_Z, up: Vec3::Y, tan_half: (1.0, 0.5625), near_m: 0.1, far_m: 1000.0, width: 1920, height: 1080, time: 0.0, real_time: 0.0, delta_world: 0.0 };
/// Where the scratch draws are placed (far below the map: nothing of them is ever drawn).
const WARM_AT: Vec3 = Vec3::new(0.0, 0.0, -1.0e7);
/// FX SHADOW: the shadow-depth pipeline of one material entry; `mat` = the material whose programs run (the entry's own, or the default material).
struct ShadowEntry { gpu: MaterialGpu, own: bool, mat: Arc<Material> }

/// FX SHADOW SELF-SHADOW BIAS (2026-10-05): `ABuildingSMActor::AnimatingDistanceFieldSelfShadowBias` (+0x958, SDK FN_FortniteGame_classes.hpp): the native ctor
/// @14045b960 stores 0x42c80000 = 100.0 and none of the 329 cooked `PBWA_*` classes overrides it. `vfunc_320` @1404989e0, on entering damage-visuals state 1
/// (AnimatingMaterial), sets the mesh component's `bOverrideDistanceFieldSelfShadowBias` (+0x795 bit 0) and `SetDistanceFieldSelfShadowBias(DistanceFieldSelfShadowBias
/// + this)` (+0x7a0, 0 from the `UStaticMeshComponent` ctor @1420f0680); on leaving state 1 it copies both back from the class default's component (override off,
/// 0). The proxy then uses the override, else the mesh's `DistanceFieldSelfShadowBias` (not serialized by the PBW piece meshes: 0), clamped >= 0
/// (`FStaticMeshSceneProxy` ctor @14248b430). So a piece's distance field ignores its own occlusion within 100 cm while it animates (build-up WPO / clip, hits,
/// breaking, destruction) in the DF shadow (DistanceFieldShadowingShared.usf:145) and DFAO (DistanceFieldScreenGridLighting.usf:118) traces.
const ANIMATING_DF_SELF_SHADOW_BIAS: f32 = 100.0;

fn yaw_mat(yaw_deg: f32) -> Mat3 { Mat3::from_rotation_z(yaw_deg.to_radians()) }

impl BuildingRender {
    pub fn new(install: &Path, asset_roots: &[PathBuf]) -> Result<BuildingRender, String> {
        let set = Arc::new(PakSet::open_install(install).map_err(|e| e.to_string())?);
        Ok(BuildingRender { set, install: install.to_path_buf(), asset_roots: asset_roots.to_vec(), gpu_ready: false, cache: TextureCache::deferred(), shared: None, fade_buf: None, defs: HashMap::new(), meshes: HashMap::new(), entries: vec![], index: HashMap::new(), mirrored: HashMap::new(), bp_params: HashMap::new(), sockets: HashMap::new(), vs_bufs: HashMap::new(), camera_cut: false, cut_pending: false, pool: vec![], used: 0, binds: HashMap::new(), pieces: HashMap::new(), cursor: 0, fx: None, fx_tried: false, time: 0., view: None, offset: Vec3::ZERO, warned: HashSet::new(), log: vec![],
            shadows: HashMap::new(), shadow_binds: HashMap::new(), default_mat: None, dfs: HashMap::new(), shadow_written: HashSet::new(), casting: false, now: 0.,
            gamedata: None, pending_hits: HashMap::new(), dying: vec![], bounce_convexes: HashMap::new(), warm: None, warm_t0: None })
    }

    /// FIRST USE: the scratch draws of one standard piece class, exactly as `draw_piece` / the placement preview / the blueprint component draw it, in both
    /// cullings (MIRRORFIX) and every damage-visuals surface; the draws themselves are dropped.
    fn warm_class(&mut self, fx: &mut FxRenderer, class: &str) {
        let def = match self.def(class) { Ok(d) => d, Err(e) => { self.warn(&format!("warm {class}"), format!("warm-up {class}: {e}")); return } };
        let mesh = def.static_mesh.clone();
        let view = *self.view.get_or_insert(WARM_VIEW);
        let eye = fn_fx::view::to_ue(view.eye).as_vec3();
        let (mut out, mut tr) = (FxFrame::default(), vec![]);
        for mirrored in [false, true] {
            let l2w = Self::l2w(WARM_AT, 0.0, mirrored);
            self.casting = true;
            for surface in [None, def.animating_material.clone(), def.damaged_material.clone()] {
                self.draw_mesh(fx, &mut out, &mut tr, eye, &mesh, None, &mut |me, fx, _slot, own, num_uv| {
                    let path = surface.clone().unwrap_or_else(|| own.to_owned());
                    me.material(fx, &path, num_uv, &[]).ok().map(|e| (e, me.entries[e].mat.overrides.clone()))
                }, l2w);
            }
            self.casting = false;
            let d = def.clone();
            self.draw_mesh(fx, &mut out, &mut tr, eye, &mesh, None, &mut |me, fx, slot, own, num_uv| {
                if slot == 0 { return me.blueprint_entry(fx, &d, num_uv, &[], &[]); }
                me.material(fx, own, num_uv, &[]).ok().map(|e| (e, me.entries[e].mat.overrides.clone()))
            }, l2w);
            let d = def.clone();
            self.draw_mesh(fx, &mut out, &mut tr, eye, &mesh, None, &mut |me, fx, _slot, _own, num_uv| me.blueprint_entry(fx, &d, num_uv, &[], &[]), l2w);
            self.draw_mesh(fx, &mut out, &mut tr, eye, &mesh, None, &mut |me, fx, _slot, _own, num_uv| me.material(fx, vis::DAMAGE_MATERIAL, num_uv, &[]).ok().map(|e| (e, me.entries[e].mat.overrides.clone())), l2w);
            // edit mode: the matched class drawn with `PreviewMaterial` (`draw_edit`, same object-path form)
            let pa = fn_building::edit::data::assets::PREVIEW_MATERIAL;
            let pv = format!("{0}.{1}", pa, pa.rsplit('/').next().unwrap_or(""));
            self.draw_mesh(fx, &mut out, &mut tr, eye, &mesh, None, &mut |me, fx, _slot, _own, num_uv| me.material(fx, &pv, num_uv, &[]).ok().map(|e| (e, me.entries[e].mat.overrides.clone())), l2w);
        }
        let _ = (self.piece_df(&mesh, false), self.piece_df(&mesh, true));
        self.used = 0;
        self.shadow_written.clear();
    }

    /// FIRST USE: the edit-mode draws of `draw_edit` (the first edit froze 19-29 ms natively, verification/load_time/normal_warm1.log): the tile meshes
    /// with `MGreen` and the active / inactive tile textures, the stair indicator with its arrow / grid textures, both cullings. Scratch frame.
    fn warm_edit(&mut self, fx: &mut FxRenderer) {
        use fn_building::edit::data::{assets as ea, material_params as mp};
        let obj = |p: &str| format!("{0}.{1}", p, p.rsplit('/').next().unwrap_or(""));
        let view = *self.view.get_or_insert(WARM_VIEW);
        let eye = fn_fx::view::to_ue(view.eye).as_vec3();
        let (mut out, mut tr) = (FxFrame::default(), vec![]);
        let tile_mat = obj(ea::TILE_MATERIAL);
        let aux_mat = obj(ea::STAIR_AUX_MATERIAL);
        for mirrored in [false, true] {
            let l2w = Self::l2w(WARM_AT, 0.0, mirrored);
            for mesh in [ea::TILE_MESH_WALL, ea::TILE_MESH_FLOOR, ea::TILE_MESH_STAIR] {
                for tex in [ea::ACTIVE_TILE_TEXTURE, ea::INACTIVE_TILE_TEXTURE] {
                    let t = obj(tex);
                    self.draw_mesh(fx, &mut out, &mut tr, eye, mesh, None, &mut |me, fx, _s, _own, num_uv| me.material(fx, &tile_mat, num_uv, &[(mp::TEXTURE, t.clone())]).ok().map(|e| (e, me.entries[e].mat.overrides.clone())), l2w);
                }
            }
            for tex in [ea::STAIR_AUX_ARROW_TEXTURE, ea::STAIR_AUX_GRID_TEXTURE] {
                let t = obj(tex);
                self.draw_mesh(fx, &mut out, &mut tr, eye, ea::STAIR_AUX_MESH, None, &mut |me, fx, _s, _own, num_uv| me.material(fx, &aux_mat, num_uv, &[(mp::TEXTURE, t.clone())]).ok().map(|e| (e, me.entries[e].mat.overrides.clone())), l2w);
            }
        }
        self.used = 0;
    }

    /// FIRST USE: one construction / destruction effect simulated for 0.6 s far away and drawn into a scratch frame (its emitters' materials, meshes and
    /// bind groups are built as on its first real spawn).
    fn warm_effect(&mut self, fx: &mut FxRenderer, system: &str) {
        let Some(f) = self.fx.as_mut() else { return };
        let Some(sys) = f.catalog.system(system) else { return };
        let view = WARM_VIEW;
        f.dxbc.begin(&view);
        let ground = |_x: f32, _y: f32| -1.0e6f32; // no floor: every emitter keeps its particles for the scratch draws
        let env = fn_particles::Env { ground: Some(&ground) };
        let mut inst = Instance::new(sys, Transform { pos: WARM_AT, rot: Mat3::IDENTITY, scale: 1.0 }, 0x5741_524D, 0.);
        let dt = 1.0 / 30.0;
        for _ in 0..18 {
            inst.tick(dt, &env);
            let mut d = vec![];
            inst.draw([view.eye.x, view.eye.y, view.eye.z], &mut d);
            let bounds = crate::particles::instance_bounds(&inst, &f.catalog);
            for e in &d { if !f.no_draw.contains(&format!("{}/{}", e.system, e.emitter)) { let _ = f.dxbc.draw_emitter(fx, e, &inst.xf, &bounds, dt); } }
            if inst.finished() { break; }
        }
        let _ = f.dxbc.take_frame();
    }

    /// FIRST USE: builds warm-up items until `budget` is spent; true once everything is built.
    fn warm_step(&mut self, fx: &mut FxRenderer, budget: std::time::Duration) -> bool {
        let t = std::time::Instant::now();
        if self.warm.is_none() {
            let mut q = std::collections::VecDeque::new();
            q.push_back(Warm::Init);
            for m in [fn_building::pieces::Material::Wood, fn_building::pieces::Material::Stone, fn_building::pieces::Material::Metal] {
                for variant in ["Solid", "Floor", "StairW", "RoofC"] { q.push_back(Warm::Class(vis::piece_class(m, 0, variant))); }
            }
            q.push_back(Warm::Edit);
            let mut doors: Vec<(String, Option<String>)> = vec![];
            for d in crate::door_hook::map_draws() { let k = (d.mesh.clone(), d.material.clone()); if !doors.contains(&k) { doors.push(k); } }
            q.extend(doors.into_iter().map(|(m, s0)| Warm::Door(m, s0)));
            self.warm = Some(q);
            self.warm_t0 = Some(t);
        }
        while let Some(task) = self.warm.as_mut().and_then(|q| q.pop_front()) {
            match task {
                Warm::Init => {
                    self.init_gpu(fx);
                    self.init_particles(fx);
                    let names: Vec<String> = self.fx.as_ref().map(|f| f.catalog.systems().map(|s| s.name.clone()).collect()).unwrap_or_default();
                    if let Some(q) = self.warm.as_mut() { q.extend(names.into_iter().map(Warm::Effect)); }
                }
                Warm::Class(c) => self.warm_class(fx, &c),
                Warm::Edit => self.warm_edit(fx),
                Warm::Effect(s) => self.warm_effect(fx, &s),
                Warm::Door(m, s0) => self.warm_door(fx, &m, s0.as_deref()),
            }
            if t.elapsed() >= budget { break; }
        }
        let done = self.warm.as_ref().map_or(true, |q| q.is_empty());
        if done { if let Some(t0) = self.warm_t0.take() { let m = format!("warm-up: {} material entries, {} meshes built in {:.0} ms (before the first placement)", self.entries.len(), self.meshes.len(), t0.elapsed().as_secs_f32() * 1000.); eprintln!("[building] {m}"); self.log.push(m); } }
        done
    }

    /// BVISFIX: the game data of the MID drive (loaded on first use; a missing curve is logged and its step skipped, as the exe does with a null curve).
    fn gamedata(&mut self) -> Arc<GameDataFx> {
        if self.gamedata.is_none() {
            let (g, errs) = GameDataFx::load(&self.set);
            for e in errs { self.warn(&format!("gd {e}"), format!("game data: {e}")); }
            let line = format!("game data: bounce {} keys, completion bounce {} keys, finished highlight {} keys, blueprint damage {:?}, crit dmg pct {:?}, crit anim cap {:?}, destroy anim {:?} s",
                g.bounce.as_ref().map_or(0, |c| c.0.keys.len()), g.edit_bounce.as_ref().map_or(0, |c| c.0.keys.len()), g.full_health.as_ref().map_or(0, |c| c.0.keys.len()),
                g.blueprint_damage.as_ref().map(|c| c.time_range()), g.crit_pct, g.crit_cap, g.destroy_duration);
            eprintln!("[building] {line}");
            self.log.push(line);
            self.gamedata = Some(Arc::new(g));
        }
        self.gamedata.clone().unwrap()
    }

    /// BVISFIX: what the piece's components report this frame (`vis::PieceFrame`): the root `Bounds` = the cooked mesh bounds moved by the actor transform.
    fn piece_frame(&mut self, fx: &mut FxRenderer, s: &PieceState) -> PieceFrame {
        let (kind, location, yaw_deg, mirrored, mesh) = (s.kind, s.location, s.yaw_deg, s.mirrored, s.def.static_mesh.clone());
        let l2w = Self::l2w(location, yaw_deg, mirrored);
        let b = match self.mesh(fx, &mesh) { Ok(()) => Some(self.meshes[&mesh].as_ref().unwrap().bounds.transform_by(&l2w)), Err(_) => None };
        PieceFrame { kind, location, yaw_deg, bounds_origin: b.map_or(location, |b| Vec3::from(b.origin)), sphere_radius: b.map_or(0., |b| b.radius) }
    }

    /// BVISFIX: `ABuildingActor::GetCentroid` @14046ced0: actor rotation (no scale) * `CentroidOffset` + location (`vis::centroid_offset`, per class kind).
    fn centroid(s: &PieceState) -> Vec3 { s.location + yaw_mat(s.yaw_deg) * vis::centroid_offset(s.kind) }

    /// NBOUNCE: the `WallBounceOverlap` query of `vfunc_314` @14048fb20 (`OverlapMultiByChannel`, `FCC_Trace_World`, ignore self, sphere `radius` at `center`,
    /// FX space): every other live piece whose class collision convexes, at its transform, overlap the sphere (`vis::sphere_overlaps_hulls`). One entry per
    /// piece: the exe lists an actor once per overlapping component, and the repeated `vfunc_313` / direction edits are idempotent.
    fn bounce_neighbours(&mut self, self_id: PieceId, center: Vec3, radius: f32) -> Vec<(PieceId, PieceKind)> {
        let cands: Vec<(PieceId, String, PieceKind, Vec3, f32, bool)> = self.pieces.iter().filter(|(id, _)| **id != self_id).map(|(id, s)| (*id, s.class.clone(), s.kind, s.location, s.yaw_deg, s.mirrored)).collect();
        let mut out = vec![];
        for (id, class, kind, loc, yaw, mirrored) in cands {
            if !self.bounce_convexes.contains_key(&class) {
                let r = fn_building::pieces::load_class(&self.set, &class);
                if let Err(e) = &r { self.warn(&format!("bounce {class}"), format!("{class}: no collision for the WallBounceOverlap query ({e})")); }
                self.bounce_convexes.insert(class.clone(), r.ok().map(|c| c.convexes));
            }
            let Some(conv) = self.bounce_convexes[&class].as_ref() else { continue };
            // cheap reject: the convexes lie within their farthest local vertex of the actor location
            let reach = conv.iter().flatten().map(|v| v.length()).fold(0f32, f32::max);
            if (loc - center).length() > radius + reach { continue; }
            let hulls = fn_building::placement::transform_hulls(conv, loc, yaw, mirrored);
            if vis::sphere_overlaps_hulls(&hulls, center, radius) { out.push((id, kind)); }
        }
        out
    }
    pub fn set(&self) -> &PakSet { &self.set }

    fn warn(&mut self, key: &str, msg: String) { if self.warned.insert(key.to_owned()) { eprintln!("[building] {msg}"); self.log.push(msg); } }

    fn def(&mut self, class: &str) -> Result<Arc<PieceVisualDef>, String> {
        if !self.defs.contains_key(class) { let r = load_def(&self.set, class).map(Arc::new); self.defs.insert(class.to_owned(), r); }
        self.defs[class].clone()
    }
    fn mesh(&mut self, fx: &mut FxRenderer, path: &str) -> Result<(), String> {
        if !self.meshes.contains_key(path) { let r = load_mesh(&self.set, fx, Vf::Local, &format!("{}.{}", fn_fx::data::long_package(path), path.rsplit('/').next().unwrap_or(path).split('.').next().unwrap_or(""))); self.meshes.insert(path.to_owned(), r); }
        self.meshes[path].as_ref().map(|_| ()).map_err(|e| e.clone())
    }

    /// The material `path` built for the static-mesh vertex factory; `textures` = texture parameter overrides baked into the pipeline (MID texture parameters).
    fn material(&mut self, fx: &mut FxRenderer, path: &str, num_uv: usize, textures: &[(&str, String)]) -> Result<usize, String> {
        let key = format!("{path}|{num_uv}|{textures:?}");
        if let Some(r) = self.index.get(&key) { return r.clone(); }
        let (shared, fade) = (self.shared.unwrap(), self.fade_buf.unwrap());
        let r = (|| -> Result<usize, String> {
            let mut mat = match preloaded(path) { Some(m) => m, None => load_material(&self.set, path)? }; // LOAD TIME: parsed by `prewarm` when it was a standard piece's
            for (n, t) in textures { mat.texture_overrides.insert((*n).to_owned(), t.clone()); }
            let mut last = String::new();
            // FX RECEIVE (2026-10-05): the Athena sky light is MOVABLE (AFortTimeOfDayManager ctor @14062c240 stores Mobility = 2, see fn-render distance_field_ao.rs),
            // and the deferred renderer draws a movable sky light's diffuse in a later pass, not in the opaque base pass (UE4.16 ScenePrivate.h:2269-2273
            // ShouldRenderSkylightInBasePass_Internal; translucent materials keep it, :2262-2265). Opaque / masked lit pieces therefore use the
            // `TBasePassPSFNoLightMapPolicy` permutation (scene colour = emissive) and get the sky diffuse, DFAO-occluded, from the scene resolve
            // (fn-render fx_resolve.wgsl); the Skylight one would add the sky twice.
            let order = if mat.flags.uses_gbuffer() { [PS_TYPES[1], PS_TYPES[0]] } else { PS_TYPES };
            for ps in order {
                if mat.cooked.find(Vf::Local.type_name(), ps).is_none() { last = format!("{path}: no {} / {ps} program", Vf::Local.type_name()); continue; }
                let gpu = build_material_layout(&self.set, fx, &mut self.cache, &mat, Vf::Local, VS_TYPE, ps, num_uv, false, shared, &[("PrimitiveFade", fade)], LayoutOpts::default())?;
                let line = format!("{} (shader map of {}): {VS_TYPE} / {ps}, blend {} shading {} two-sided {}, {} uniform vectors / {} scalars / {} textures",
                    // BVISFIX: the package whose cooked shader map is used (`Material::shader_map_package`), not the chain's root material (`base_path`)
                    path.rsplit('.').next().unwrap_or(path), mat.shader_map_package.rsplit('/').next().unwrap_or(""), mat.flags.blend, mat.flags.shading, mat.flags.two_sided, mat.uniform.vectors.len(), mat.uniform.scalars.len(), mat.uniform.textures2d.len());
                eprintln!("[building] material {line}");
                self.log.push(line);
                self.entries.push(MatEntry { mat: Arc::new(mat), gpu, ps, num_uv, front_cw: false });
                return Ok(self.entries.len() - 1);
            }
            Err(last)
        })();
        self.index.insert(key, r.clone());
        r
    }

    /// MIRRORFIX (2026-10-05): the entry `e` for a primitive whose LocalToWorld has a negative determinant (a mirrored piece / preview / edit actor, `l2w`'s -1 X
    /// scale): UE draws such a mesh with `ReverseCulling = IsLocalToWorldDeterminantNegative()` (StaticMeshRender.cpp:284) -> `CM_CCW` instead of `CM_CW`
    /// (DrawingPolicy.cpp:45), i.e. the same programs with the reversed cull mode (`LayoutOpts::front_cw`, fn_render::fx::FxCull::BackCw). A two-sided material
    /// culls nothing either way (its `TwoSidedSign` takes `Primitive.LocalToWorldDeterminantSign`, MaterialTemplate.usf:1841, already in the primitive buffer).
    fn reverse_culling_entry(&mut self, fx: &mut FxRenderer, e: usize) -> usize {
        if self.entries[e].mat.flags.two_sided || self.entries[e].front_cw { return e; }
        if let Some(r) = self.mirrored.get(&e) { return r.as_ref().copied().unwrap_or(e); }
        let (shared, fade) = (self.shared.unwrap(), self.fade_buf.unwrap());
        let (mat, ps, num_uv) = (self.entries[e].mat.clone(), self.entries[e].ps, self.entries[e].num_uv);
        let r = build_material_layout(&self.set, fx, &mut self.cache, &mat, Vf::Local, VS_TYPE, ps, num_uv, false, shared, &[("PrimitiveFade", fade)], LayoutOpts { front_cw: true, ..LayoutOpts::default() })
            .map(|gpu| { self.entries.push(MatEntry { mat, gpu, ps, num_uv, front_cw: true }); self.entries.len() - 1 });
        match &r {
            Err(err) => { let m = format!("{}: reversed-culling variant NOT built ({err}): the mirrored draw keeps the normal cull mode", self.entries[e].mat.path); self.warn(&format!("mirror {e}"), m); }
            Ok(_) => { let m = format!("material {}: reversed-culling variant (mirrored primitive, CM_CCW)", self.entries[e].mat.path.rsplit('.').next().unwrap_or("")); eprintln!("[building] {m}"); self.log.push(m); }
        }
        self.mirrored.insert(e, r.clone());
        r.unwrap_or(e)
    }

    /// FX SHADOW: the shadow-depth programs of entry `e` (built once): the material's own cooked `TShadowDepthVS` / `TShadowDepthPS` pair, or the default
    /// material's position-only VS for an opaque non-WPO material (`fn_fx::gpu::shadow_programs`), with the entry's culling (TwoSided, MIRRORFIX reversed cull).
    fn shadow_entry(&mut self, fx: &mut FxRenderer, e: usize) -> Option<usize> {
        if !self.shadows.contains_key(&e) {
            let (shared, fade) = (self.shared.unwrap(), self.fade_buf.unwrap());
            let (mat, num_uv, front_cw) = (self.entries[e].mat.clone(), self.entries[e].num_uv, self.entries[e].front_cw);
            let needs_default = matches!(mat.flags.blend.as_str(), "BLEND_Opaque") && mat.cooked.find(Vf::Local.type_name(), fn_fx::gpu::SHADOW_VS).is_none();
            if needs_default && self.default_mat.is_none() {
                let d = preloaded(fn_fx::gpu::DEFAULT_SURFACE_MATERIAL).map(Ok).unwrap_or_else(|| load_material(&self.set, fn_fx::gpu::DEFAULT_SURFACE_MATERIAL)).map(Arc::new);
                if let Err(err) = &d { self.warn("defmat", format!("{}: {err}", fn_fx::gpu::DEFAULT_SURFACE_MATERIAL)); }
                self.default_mat = Some(d.ok());
            }
            let dm = self.default_mat.clone().flatten();
            let r = fn_fx::gpu::build_shadow(&self.set, fx, &mut self.cache, &mat, dm.as_deref(), Vf::Local, num_uv, shared, &[("PrimitiveFade", fade)], front_cw);
            let name = mat.path.rsplit('.').next().unwrap_or("").to_owned();
            let v = match r {
                Ok(Some((gpu, own))) => {
                    let line = format!("material {name}: shadow depth {} (front_cw {front_cw}, two-sided {})", if own { format!("own {} / {}", fn_fx::gpu::SHADOW_VS, if mat.flags.blend == "BLEND_Masked" { fn_fx::gpu::SHADOW_PS } else { "null PS" }) } else { format!("default material {}", fn_fx::gpu::SHADOW_VS_POSITION_ONLY) }, mat.flags.two_sided);
                    eprintln!("[building] {line}"); self.log.push(line);
                    Some(ShadowEntry { gpu, own, mat: if own { mat.clone() } else { dm.clone().unwrap() } })
                }
                Ok(None) => None,
                Err(err) => { self.warn(&format!("shadow {e}"), format!("material {name}: shadow depth NOT built ({err}): casts no shadow")); None }
            };
            self.shadows.insert(e, v);
        }
        self.shadows[&e].as_ref().map(|_| e)
    }

    /// FX SHADOW: the cooked distance field of a piece mesh (once per mesh). `bAffectDistanceFieldLighting` stays true for the player pieces: the 1.8
    /// `ABuildingActor::PostInitializeComponents` (@140487b00) clears it (bit 6 of +0x2a9) only when the actor's `bIsMovable` or, on a client, `bIsDynamic` is
    /// set (+0x554 bits 28 / 26), and nothing sets them for walls / floors / stairs / roofs (`ABuildingActor` ctor @140458440 clears 26 / 27, the
    /// `ABuildingSMActor` / `ABuildingWall` / `ABuildingFloor` / `ABuildingStairs` / `ABuildingRoof` ctors never set 26 / 27 / 28, the PBWA class defaults
    /// serialize neither); `ABuildingSMActor`'s ctor (@14045b960) sets CastShadow / bCastDynamicShadow / bCastStaticShadow on its mesh component.
    /// FX SHADOW SELF-SHADOW BIAS: `animating` = damage-visuals state 1, whose component overrides the self-shadow bias ([`ANIMATING_DF_SELF_SHADOW_BIAS`]).
    fn warm_door(&mut self, fx: &mut FxRenderer, mesh: &str, slot0: Option<&str>) {
        if self.mesh(fx, mesh).is_ok() {
            let (num_uv, sections, paths) = { let g = self.meshes[mesh].as_ref().unwrap(); (g.num_uv, g.sections.clone(), g.material_paths.clone()) };
            for (slot, _, _) in sections {
                let own = paths.get(slot).cloned().flatten();
                let path = if slot == 0 { slot0.map(str::to_owned).or(own) } else { own };
                if let Some(p) = path { let _ = self.material(fx, &p, num_uv, &[]); }
            }
        }
        let _ = self.piece_df(mesh, false);
    }

    fn piece_df(&mut self, mesh_path: &str, animating: bool) -> Option<Arc<fn_render::distance_field::MeshDistanceField>> {
        if !self.dfs.contains_key(mesh_path) {
            let object = format!("{}.{}", fn_fx::data::long_package(mesh_path), mesh_path.rsplit('/').next().unwrap_or(mesh_path).split('.').next().unwrap_or(""));
            let r = fn_fx::gpu::load_mesh_distance_field(&self.set, &object, true);
            if let Err(e) = &r { let m = format!("{mesh_path}: no distance field ({e})"); self.warn(&format!("df {mesh_path}"), m); }
            self.dfs.insert(mesh_path.to_owned(), r.ok().map(|d| { let mut a = d.clone(); a.self_shadow_bias = ANIMATING_DF_SELF_SHADOW_BIAS; (Arc::new(d), Arc::new(a)) }));
        }
        self.dfs[mesh_path].as_ref().map(|(base, anim)| if animating { anim.clone() } else { base.clone() })
    }

    fn slot(&mut self, fx: &mut FxRenderer) -> usize {
        if self.used == self.pool.len() { self.pool.push(Slot { prim: fx.create_buffer("building primitive", 32 * 16), mat: fx.create_buffer("building material", 128 * 16) }); }
        self.used += 1;
        self.used - 1
    }

    fn init_gpu(&mut self, fx: &mut FxRenderer) {
        if self.gpu_ready { return; }
        self.gpu_ready = true;
        let view = fx.create_buffer("building view", VIEW_FLOAT4S * 16);
        let light = fx.create_buffer("building light", fn_render::sky_light::FX_LIGHT_BYTES); // SKY LIGHT: FxLight with the SH (fn-fx program.rs)
        let sprite_vf = fx.create_buffer("building sprite vf", 10 * 16);
        fx.write_buffer(sprite_vf, &bytes(&sprite_vf_buffer()));
        let prim = fx.create_buffer("building primitive (shared)", 32 * 16);
        let fade = fx.create_buffer("building PrimitiveFade", 16);
        fx.write_buffer(fade, &bytes(&fn_matshader::frame::fade_buffer()));
        self.shared = Some(SharedBuffers { view, primitive: prim, sprite_vf, light });
        self.fade_buf = Some(fade);
    }

    fn init_particles(&mut self, fx: &mut FxRenderer) {
        if self.fx_tried { return; }
        self.fx_tried = true;
        let dir = fn_live::var_os("FN_BUILDING_FX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| self.asset_roots.iter().map(|r| r.join("particles-building")).find(|d| d.join("index.json").is_file()));
        let Some(dir) = dir else { self.warn("fxdir", "no `particles-building` folder (fn-particles-export of the PBW Build_Sparkle / Destruction systems): no construction / destruction effects".into()); return };
        let cat = match Catalog::load(&dir) { Ok(c) => Arc::new(c), Err(e) => { self.warn("fxcat", format!("cannot load {}: {e}", dir.display())); return } };
        let dxbc = match crate::pickups_dxbc::Dxbc::load(&self.install, fx) { Ok(d) => d, Err(e) => { self.warn("fxdxbc", format!("particle materials unavailable: {e}")); return } };
        // emitters whose EmitterRenderMode draws nothing (ERM_None / ERM_LightsOnly)
        let mut no_draw = HashSet::new();
        for sys in cat.systems() {
            let Ok(pkg) = fn_fx::data::PackageJson::load(&self.set, &sys.package) else { continue };
            let name = sys.package.rsplit('/').next().unwrap_or(&sys.package).to_owned();
            for e in pkg.exports.iter().filter(|e| e.class.starts_with("Particle") && e.class.ends_with("Emitter")) {
                let mode = e.props.get("EmitterRenderMode").and_then(|v| v.as_str()).unwrap_or("ERM_Normal");
                if mode != "ERM_Normal" { no_draw.insert(format!("{name}/{}", e.props.get("EmitterName").and_then(|v| v.as_str()).unwrap_or(""))); }
            }
        }
        self.log.push(format!("particles-building: {} systems from {}", cat.system_count(), dir.display()));
        self.fx = Some(Fx { catalog: cat, dxbc, live: vec![], no_draw, serial: 0 });
    }

    fn spawn_effect(&mut self, system: &str, pos: Vec3, rot: Mat3) {
        let Some(f) = self.fx.as_mut() else { return };
        let Some(sys) = f.catalog.system(system) else { let k = format!("nosys {system}"); let m = format!("{system} is not in particles-building: not played"); self.warn(&k, m); return };
        f.serial += 1;
        f.live.push(Instance::new(sys, Transform { pos, rot, scale: 1.0 }, f.serial.wrapping_mul(0x9E37_79B9_7F4A_7C15), 0.));
        self.log.push(format!("effect {system} at {pos:?}"));
    }

    /// The world events since the last frame: spawn / remove / edit the per-piece visual state, sounds and effects.
    fn events(&mut self, fx: &mut FxRenderer, world: &BuildingWorld, sounds: &mut Vec<SoundEvent>) {
        let (evs, next) = world.events_since(self.cursor);
        let evs: Vec<WorldEvent> = evs.to_vec();
        self.cursor = next;
        let now = world.time();
        for e in evs {
            match e {
                WorldEvent::Placed(id) => {
                    let Some(p) = world.get(id) else { continue };
                    let class = p.effective_class().to_owned();
                    let def = match self.def(&class) { Ok(d) => d, Err(e) => { self.warn(&class, format!("{class}: {e}")); continue } };
                    let (anim, ev) = PieceAnim::placed(&def, now, p.health, p.max_health, p.is_building());
                    let pos = vis::ue_to_game(p.location + self.offset);
                    for a in ev { if let AnimEvent::ConstructionSoundStart(cue) = a { sounds.push(SoundEvent::BuildingLoop { cue, key: LOOP_KEY_BASE + id, pos: Some(pos) }); } }
                    self.pieces.insert(id, PieceState { class, def, anim, kind: p.kind, location: p.location + self.offset, yaw_deg: p.yaw_deg, mirrored: p.edit.mirrored, max_health: p.max_health, edit_hidden: false });
                }
                WorldEvent::Edited(id) => {
                    // ReplaceBuildingActor: a new actor of the edit class; location / yaw are already final in the world (see fn_building::edit)
                    let Some(p) = world.get(id) else { continue };
                    let class = p.effective_class().to_owned();
                    let def = match self.def(&class) { Ok(d) => d, Err(e) => { self.warn(&class, format!("{class}: {e}")); continue } };
                    // editing agent: the new actor is initialised with InitReason 2 (ABuildingSMActor::vfunc_247 @14047bed0): with a BuildTime and an
                    // AnimatingMaterial it plays the construct animation (MulticastRequestBuildingAnimation(4)) and builds up like a placement
                    // (BuildingWorld::rebuild_for_edit put it back in the build-up), else it is shown built
                    let anim = if p.is_building() {
                        let (anim, ev) = PieceAnim::placed(&def, now, p.health, p.max_health, true);
                        let pos = vis::ue_to_game(p.location + self.offset);
                        for a in ev { if let AnimEvent::ConstructionSoundStart(cue) = a { sounds.push(SoundEvent::BuildingLoop { cue, key: LOOP_KEY_BASE + id, pos: Some(pos) }); } }
                        anim
                    } else { PieceAnim::built(&def) };
                    self.pieces.insert(id, PieceState { class, def, anim, kind: p.kind, location: p.location + self.offset, yaw_deg: p.yaw_deg, mirrored: p.edit.mirrored, max_health: p.max_health, edit_hidden: false });
                }
                WorldEvent::HealthChanged { id, old, new } if new < old => {
                    // BVISFIX: a damaging hit (`vfunc_276` -> `vfunc_314` -> `vfunc_313`) with the hit the hit tests reported for this piece ([`report_hit`]); a
                    // health loss without a reported hit (verification scenes) bounces at the centroid towards +Z (ASSUMED, named)
                    let hit = self.pending_hits.get_mut(&id).and_then(|q| q.pop_front());
                    let Some(s) = self.pieces.get(&id) else { continue };
                    let (impact, to_attacker) = match hit { Some((p, d)) => (p + self.offset, d), None => (Self::centroid(s), Vec3::Z) };
                    let (def, frame) = (s.def.clone(), { let s = self.pieces.remove(&id).unwrap(); let f = self.piece_frame(fx, &s); self.pieces.insert(id, s); f });
                    // NBOUNCE: a fatal hit (new = 0) is OnDeath `vfunc_277`, not `vfunc_276` (`PieceAnim::on_damage` ignores it; the `Removed` event follows)
                    if new <= 0. { continue; }
                    // NBOUNCE: `WallBounceOverlap` (`vfunc_314` @14048fb20, `bPropagatesBounceEffects`): the live pieces whose collision the sphere
                    // (clamp(SphereRadius, 0, 300) at the hit location) overlaps, self excluded (their order does not matter: the floor / wall edits of the
                    // shared direction only zero components, which commute)
                    let neighbours: Vec<(PieceId, PieceKind)> = if def.propagates_bounce { self.bounce_neighbours(id, impact, vis::damage_bounce_radius(&frame)) } else { vec![] };
                    let kinds: Vec<PieceKind> = neighbours.iter().map(|n| n.1).collect();
                    let s = self.pieces.get_mut(&id).unwrap();
                    let Some(b) = s.anim.on_damage(&def, &frame, now, old - new, new, impact, to_attacker, &kinds) else { continue };
                    for (nid, _) in neighbours {
                        let Some(h) = world.get(nid).map(|p| p.health) else { continue };
                        if let Some(n) = self.pieces.get_mut(&nid) { let nd = n.def.clone(); n.anim.on_neighbour_bounce(&nd, &b, h); }
                    }
                }
                WorldEvent::Removed(id) => {
                    sounds.push(SoundEvent::BuildingLoopStop { key: LOOP_KEY_BASE + id }); // EndPlay @140469ac0
                    self.pending_hits.remove(&id);
                    let Some(mut s) = self.pieces.remove(&id) else { continue };
                    // vfunc_316: death particles + death sound at the death location (the socket, else GetCentroid)
                    let pos = self.death_location(&s);
                    if let Some(c) = fn_audio_sink::building_cue(&s.def.death_cue()) { sounds.push(SoundEvent::BuildingCue { cue: c, pos: Some(vis::ue_to_game(pos)) }); }
                    let sys = s.def.death_particles_or_fallback(s.kind);
                    self.spawn_effect(&sys, pos, yaw_mat(s.yaw_deg));
                    // BVISFIX: OnDeath (`vfunc_277` @140486330) starts the destruction animation (`vfunc_397(3)` -> `vfunc_402`); the actor stays until `vfunc_410`
                    // hides it at the end (the world already removed the piece's gameplay state)
                    let (g, frame) = (self.gamedata(), self.piece_frame(fx, &s));
                    let max = s.max_health;
                    s.anim.on_death(&s.def.clone(), &g, &frame, now, max);
                    if !s.anim.hidden { self.dying.push(s); }
                }
                _ => {}
            }
        }
    }

    /// `vfunc_316` @140485ab0: the `DeathParticleSocketName` (`DeathFX`) socket of the mesh when it has one, else `GetCentroid` (BVISFIX: was the actor location).
    fn death_location(&mut self, s: &PieceState) -> Vec3 {
        let Some(sock) = s.def.death_particle_socket.as_deref() else { return Self::centroid(s) };
        // the mesh's `StaticMeshSocket` exports (`SocketName`, `RelativeLocation`; the socket's rotation does not move the location); PERF FIX: read once per mesh
        let key = (s.def.static_mesh.clone(), sock.to_owned());
        if !self.sockets.contains_key(&key) {
            let long = fn_fx::data::long_package(&s.def.static_mesh);
            let loc = fn_fx::data::PackageJson::load(&self.set, long).ok().and_then(|pkg| pkg.of_class("StaticMeshSocket").find(|e| e.props.get("SocketName").and_then(|v| v.as_str()).map_or(false, |n| n.eq_ignore_ascii_case(sock)))
                .and_then(|e| e.props.get("RelativeLocation").and_then(|v| v.as_array()).map(|a| Vec3::new(a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32, a[2].as_f64().unwrap_or(0.) as f32))));
            self.sockets.insert(key.clone(), loc);
        }
        match self.sockets[&key] {
            Some(l) => s.location + yaw_mat(s.yaw_deg) * Vec3::new(if s.mirrored { -l.x } else { l.x }, l.y, l.z),
            None => Self::centroid(s),
        }
    }

    fn l2w(location: Vec3, yaw_deg: f32, mirrored: bool) -> Mat4 {
        Mat4::from_translation(location) * Mat4::from_mat3(yaw_mat(yaw_deg)) * Mat4::from_scale(Vec3::new(if mirrored { -1. } else { 1. }, 1., 1.))
    }

    /// Queues the draws of `mesh` with material `entry` and `ov` (MID parameters) at `l2w`.
    fn draw_mesh(&mut self, fx: &mut FxRenderer, out: &mut FxFrame, translucent: &mut Vec<Pending>, eye_ue: Vec3, mesh_path: &str, slot_filter: Option<usize>, entry_for: &mut dyn FnMut(&mut Self, &mut FxRenderer, usize, &str, usize) -> Option<(usize, Overrides)>, l2w: Mat4) {
        if let Err(e) = self.mesh(fx, mesh_path) { let m = format!("{mesh_path}: NOT DRAWN: {e}"); self.warn(mesh_path, m); return; }
        let (mesh, num_uv, sections, paths, cooked, has_df) = { let g = self.meshes[mesh_path].as_ref().unwrap(); (g.id, g.num_uv, g.sections.clone(), g.material_paths.clone(), g.bounds, g.has_distance_field) };
        // shared UE4.16 Primitive builder (fn_render::primitive_ub, 18 float4s) from the cooked mesh bounds (MeshGpu::bounds = UStaticMesh::GetBounds); SORTFIX: the
        // distance-field bit (PerObjectGBufferData, PrimitiveUniformShaderParameters.h:80) = HasDistanceFieldRepresentation (StaticMeshRender.cpp:1374): the 1.8 config
        // builds mesh distance fields (r.GenerateMeshDistanceFields=True) and no piece Blueprint changes CastShadow / bCastDynamicShadow / bAffectDistanceFieldLighting
        let flags = fn_render::primitive_ub::PrimitiveFlags { has_distance_field: has_df, ..Default::default() };
        let prim = fn_render::primitive_ub::mesh_primitive(&l2w, &cooked, l2w.w_axis.truncate().to_array(), &flags);
        // SORTFIX: the translucency sort key is |Bounds.Origin - ViewOrigin| with Bounds = the cooked bounds moved by the component (TranslucentRendering.cpp:864),
        // not the vertex-box centre
        let world_origin = cooked.transform_by(&l2w).origin;
        let dist = (Vec3::from(world_origin) - eye_ue).length();
        let sort = match self.view.as_ref() { Some(v) => v.sort(world_origin), None => return };
        // MIRRORFIX: ReverseCulling = IsLocalToWorldDeterminantNegative() (StaticMeshRender.cpp:284), see `reverse_culling_entry`
        let reverse_culling = glam::Mat3::from_mat4(l2w).determinant() < 0.0;
        for (slot_i, first, count) in sections {
            if slot_filter.map_or(false, |f| f != slot_i) { continue; }
            let own = paths.get(slot_i).cloned().flatten().unwrap_or_default();
            let Some((e, ov)) = entry_for(self, fx, slot_i, &own, num_uv) else { continue };
            let e = if reverse_culling { self.reverse_culling_entry(fx, e) } else { e };
            let packed = fn_fx::uniform::evaluate(&self.entries[e].mat.uniform, &ov, self.time);
            let s = self.slot(fx);
            out.buffer_writes.push((self.pool[s].prim, bytes(&prim)));
            let mut cb = packed.cb; cb.resize(cb.len().max(self.entries[e].gpu.material_registers).max(1), [0.0; 4]);
            out.buffer_writes.push((self.pool[s].mat, bytes(&cb)));
            // every material-owned globals buffer at the world / real / delta times (FMaterialShader::SetParameters, ShaderBaseClasses.cpp:287), not the pixel-only one
            if let Some(v) = self.view.as_ref() { out.buffer_writes.extend(self.entries[e].gpu.per_frame_writes(&self.entries[e].mat, v.material_time())); }
            // CAMERA CUT (2026-10-05): the base-pass VS velocity globals per primitive (`TBasePassVertexShaderPolicyParamType::SetMesh`, BasePassRendering.inl:31-66). A
            // piece is a movable runtime-spawned component, so `HasVelocityOnBasePass` is true (VelocityRendering.cpp:458) with `PreviousLocalToWorld` = its transform
            // (pieces do not move; no motion-blur info = the current one) and `SkipOutputVelocity` 0, except on a camera cut (`View.bCameraCut`, :436): identity, skip 1.
            let vs = match self.entries[e].gpu.vs_globals().map(|g| g.float4s) {
                Some(n) => {
                    let b = *self.vs_bufs.entry((e, s)).or_insert_with(|| fx.create_buffer("building base-pass VS globals", n * 16));
                    let t = self.view.as_ref().map(|v| v.material_time()).unwrap_or(fn_fx::uniform::MaterialTime::single(self.time));
                    if let Some(bytes_) = self.entries[e].gpu.vs_globals_bytes(&self.entries[e].mat, t, if self.camera_cut { None } else { Some(l2w) }) { out.buffer_writes.push((b, bytes_)); }
                    Some(b)
                }
                None => None,
            };
            let binds = match self.binds.get(&(e, s)) { Some(b) => b.clone(), None => { let (pb, mb) = (self.pool[s].prim, self.pool[s].mat); let b = self.entries[e].gpu.bind_variant_globals(fx, pb, Some(mb), vs); self.binds.insert((e, s), b.clone()); b } };
            let draw = FxDraw { pipe: self.entries[e].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, instances: None, sort };
            // FX SHADOW: a casting primitive's section also goes into the sun's cascades with the same Primitive / Material buffers (so the build-up's
            // `BuildAnimPhase` WPO runs in the shadow VS too: the cooked TShadowDepthVS evaluates the material's WorldPositionOffset)
            if self.casting && !self.entries[e].gpu.translucent {
                if let Some(se) = self.shadow_entry(fx, e) {
                    if self.shadow_written.insert(se) { if let Some(v) = self.view.as_ref() { let sh = self.shadows[&se].as_ref().unwrap(); out.buffer_writes.extend(sh.gpu.shadow_globals_writes(&sh.mat, v.material_time(), v.eye.to_array())); } }
                    let sb = match self.shadow_binds.get(&(se, s)) { Some(b) => b.clone(), None => { let (pb, mb) = (self.pool[s].prim, self.pool[s].mat); let sh = self.shadows[&se].as_ref().unwrap(); let b = sh.gpu.bind_variant_globals(fx, pb, if sh.own { Some(mb) } else { None }, None); self.shadow_binds.insert((se, s), b.clone()); b } };
                    let pipe = self.shadows[&se].as_ref().unwrap().gpu.pipe;
                    out.shadow_draws.push(FxShadowDraw { pipe, binds: sb, geom: FxGeom::MeshRange { mesh, first_index: first, index_count: count }, bounds: None, instances: None });
                }
            }
            if self.entries[e].gpu.translucent { translucent.push(Pending { dist, draw }); } else { out.draws.push(draw); }
        }
    }

    fn overrides_with(base: &Material, scalars: &[(&str, f32)], vectors: &[(&str, [f32; 4])]) -> Overrides {
        let mut o = base.overrides.clone();
        for (n, v) in scalars { o.scalars.insert((*n).to_owned(), *v); }
        for (n, v) in vectors { o.vectors.insert((*n).to_owned(), *v); }
        o
    }

    /// The blueprint MID (`BlueprintParentMaterial` with the animating material's `Blueprint_*` values) for a class; `extra` = per-draw scalars / vectors.
    fn blueprint_entry(&mut self, fx: &mut FxRenderer, def: &PieceVisualDef, num_uv: usize, scalars: &[(&str, f32)], vectors: &[(&str, [f32; 4])]) -> Option<(usize, Overrides)> {
        // PERF FIX (2026-10-05): the animating material is loaded ONCE per class path and its `Blueprint_*` values kept, as UE keeps the loaded material resident
        // (the CDO's `AnimatingMaterial` is a hard reference, loaded with the class; `ExtractBlueprintParamValuesFromAnimMat` @14046a0d0 only reads the parameters).
        // It was re-loaded (packages + the ~1 MB cooked shader map parsed) on every draw of the preview / blueprint: ~260 ms per frame in build mode.
        let bp = match self.bp_params.get(&def.class_path) {
            Some(b) => b.clone(),
            None => {
                let bp = match def.animating_material.as_deref().map(|a| preloaded(a).map(Ok).unwrap_or_else(|| load_material(&self.set, a))) { Some(Ok(m)) => blueprint_params(&m), Some(Err(e)) => { self.warn(&format!("bpsrc {}", def.class_path), format!("{}: animating material: {e}", def.class_path)); BlueprintParams::default() } None => BlueprintParams::default() };
                self.bp_params.insert(def.class_path.clone(), bp.clone());
                bp
            }
        };
        let tex: Vec<(&str, String)> = bp.texture.iter().map(|t| (vis::param::BLUEPRINT_TEXTURE, t.clone())).collect();
        let e = match self.material(fx, vis::BLUEPRINT_MATERIAL, num_uv, &tex) { Ok(e) => e, Err(err) => { self.warn("bp", format!("blueprint material NOT DRAWN: {err}")); return None } };
        let mut v: Vec<(&str, [f32; 4])> = vec![];
        for i in 0..4 { if let Some(c) = bp.channels[i] { v.push((vis::param::BLUEPRINT_CHANNELS[i], c)); } }
        v.extend_from_slice(vectors);
        let mut s: Vec<(&str, f32)> = bp.grid.map(|g| (vis::param::BLUEPRINT_GRID_MULTIPLIER, g)).into_iter().collect();
        s.extend_from_slice(scalars);
        Some((e, Self::overrides_with(&self.entries[e].mat, &s, &v)))
    }

    /// One frame: world events, animation ticks, sounds, and the FX draws appended to `out`. Returns the sounds to hand to the audio sink.
    pub fn frame(&mut self, fx: &mut FxRenderer, world: &BuildingWorld, offset: Vec3, preview: Option<&Preview>, edit: Option<&EditVisuals>, view: &ViewParams, dt: f32, out: &mut FxFrame) -> Vec<SoundEvent> {
        self.offset = offset;
        self.camera_cut = std::mem::take(&mut self.cut_pending); // CAMERA CUT: this frame's View.bCameraCut
        let off = offset;
        self.init_gpu(fx);
        self.init_particles(fx);
        self.used = 0;
        self.shadow_written.clear(); // FX SHADOW
        self.casting = false; // FX SHADOW: previews / edit actors clear CastShadow (ABuildingPlayerPrimitivePreview ctor @14045ac30, edit tiles @1404b82e0, stair indicators @1404b6050)
        self.time = view.time;
        self.now = world.time(); // FX SHADOW SELF-SHADOW BIAS
        self.view = Some(*view);
        let mut sounds = vec![];
        let shared = self.shared.unwrap();
        out.buffer_writes.push((shared.view, bytes(&view.view_buffer())));
        out.buffer_writes.push((shared.light, bytes(&fn_render::sky_light::fx_light_block()))); // SKY LIGHT: sun + View.SkyIrradianceEnvironmentMap
        self.events(fx, world, &mut sounds);
        let eye_ue = fn_fx::view::to_ue(view.eye).as_vec3();
        let now = world.time();
        let mut translucent: Vec<Pending> = vec![];
        let g = self.gamedata();
        // ---- pieces
        let ids: Vec<PieceId> = self.pieces.keys().copied().collect();
        for id in ids {
            let Some(p) = world.get(id) else { self.pieces.remove(&id); continue };
            let (health, max, build_time, building) = (p.health, p.max_health, p.build.duration, p.is_building());
            {
                let st = self.pieces.get_mut(&id).unwrap();
                st.location = p.location + off; st.yaw_deg = p.yaw_deg; st.mirrored = p.edit.mirrored; st.max_health = max;
            }
            let st = self.pieces.remove(&id).unwrap();
            let frame = self.piece_frame(fx, &st);
            let mut st = st;
            let def = st.def.clone();
            // BVISFIX: the core's under-construction state (HealthToAutoBuild > 0) drives the material and the construction sound fade
            let evs = st.anim.tick(&def, &g, &frame, now, dt, health, max, build_time, building);
            self.piece_events(&st, id, evs, &mut sounds, now);
            // HUD FIX2: edit mode (AFortPlayerController vfunc_684 @140836d70 -> building vfunc 0xa88 = ABuildingSMActor::vfunc_337 @140481fd0) hides the
            // edited piece's StaticMeshComponent (vfunc_292(false), SetVisibility(false, false): no draw, no shadow, no distance field) and its blueprint mesh
            // (vfunc_390(false)) while the tiles / preview show. vfunc_338 @140482180 (confirm / cancel) shows the mesh again and the blueprint mesh only when
            // Health < MaxHealth (vfunc 0x720 < 0x728). ABuildingWall::vfunc_337 also hides its DoorComponent (+0x1048): DOORS, see `draw_piece_door`.
            let edited = edit.map_or(false, |ev| ev.piece == id);
            if st.edit_hidden && !edited { st.anim.blueprint_visible = st.anim.blueprint && health < max; }
            st.edit_hidden = edited;
            if !edited { self.draw_piece(fx, out, &mut translucent, eye_ue, &st); }
            // DOORS (2026-10-08): the piece's DoorComponent (door_hook.rs / fn_building::door), hidden with the mesh while edited (ABuildingWall::vfunc_337)
            if !edited { if let Some(d) = crate::door_hook::piece_door(id) { self.draw_piece_door(fx, out, &mut translucent, eye_ue, &st, &d); } }
            self.pieces.insert(id, st);
        }
        // ---- BVISFIX: pieces the world removed, playing their destruction animation until `vfunc_410` hides them
        let dying = std::mem::take(&mut self.dying);
        for mut st in dying {
            let frame = self.piece_frame(fx, &st);
            let def = st.def.clone();
            let max = st.max_health;
            let _ = st.anim.tick(&def, &g, &frame, now, dt, 0., max, 0., false);
            if st.anim.hidden { continue; }
            self.draw_piece(fx, out, &mut translucent, eye_ue, &st);
            self.dying.push(st);
        }
        // ---- DOORS (2026-10-08): the map door walls' DoorComponents (door_hook.rs): movable runtime-spawned static meshes like the pieces
        prewarm_doors(&self.set);
        for d in crate::door_hook::map_draws() { if crate::door_hook::leaf_visible(view, d.center, d.radius) { self.draw_door_leaf(fx, out, &mut translucent, eye_ue, &d.mesh, d.material.as_deref(), &[], &[], None, d.l2w); } }
        for t in crate::trap_hook::draws(view.time) { self.draw_trap(fx, out, &mut translucent, eye_ue, &t); }
        // ---- placement preview (BuildPreviewMarker)
        if let Some(pv) = preview {
            match self.def(&pv.class_path) {
                Ok(d) => {
                    let l2w = Self::l2w(pv.location + self.offset, pv.yaw_deg, pv.mirrored);
                    let mesh = d.static_mesh.clone();
                    let valid = pv.valid;
                    self.draw_mesh(fx, out, &mut translucent, eye_ue, &mesh, None, &mut |me, fx, _s, _own, num_uv| {
                        // Code_Color = the MID's own code_Blue / Code_Red (FUN_140831f80)
                        let (e, mut o) = me.blueprint_entry(fx, &d, num_uv, &[], &[])?;
                        let c = find_vector(&me.entries[e].mat, vis::preview_color_source(valid)).unwrap_or(if valid { vis::CODE_BLUE_DEFAULT } else { vis::CODE_RED_DEFAULT });
                        o.vectors.insert(vis::param::CODE_COLOR.to_owned(), c);
                        Some((e, o))
                    }, l2w);
                }
                Err(e) => self.warn(&pv.class_path, format!("preview {}: {e}", pv.class_path)),
            }
        }
        // ---- edit mode (tiles, stair indicators, the matched class with PreviewMaterial)
        if let Some(ev) = edit { self.draw_edit(fx, out, &mut translucent, eye_ue, ev); }
        // translucent: back to front
        translucent.sort_by(|a, b| b.dist.partial_cmp(&a.dist).unwrap_or(std::cmp::Ordering::Equal));
        out.draws.extend(translucent.into_iter().map(|p| p.draw));
        // ---- effects
        self.tick_effects(fx, view, dt, out);
        sounds
    }

    /// The sounds / effects of a piece's animation events.
    fn piece_events(&mut self, st: &PieceState, id: PieceId, evs: Vec<AnimEvent>, sounds: &mut Vec<SoundEvent>, now: f64) {
        for e in evs {
            // one run-log line per lifecycle event (verification of the completion / flash-peak / fade timing)
            if !matches!(e, AnimEvent::ConstructionSoundStart(_)) { eprintln!("[building] #{id} {} {e:?} at t={:.3} s", st.class.rsplit('.').next().unwrap_or(""), now); }
            match e {
                AnimEvent::Completed => sounds.push(SoundEvent::BuildingCue { cue: vis::CONSTRUCTION_COMPLETE_CUE, pos: Some(vis::ue_to_game(st.location)) }),
                // BVISFIX: vfunc_317 at the finished-highlight peak (spawned by vfunc_323), not at completion
                AnimEvent::ConstructedEffect => self.spawn_effect(&st.def.constructed_effect_or_fallback(), st.location, yaw_mat(st.yaw_deg)),
                // BVISFIX: ABuildingSMActor::Tick @1404967f0: ConstructionAudioComponent->FadeOut(0.5, 0) once the under-construction bit is clear (the building
                // loops share the sink's fire-loop channel table, whose stop takes the fade)
                AnimEvent::ConstructionSoundFadeOut(fade) => sounds.push(SoundEvent::WeaponLoopStop { key: LOOP_KEY_BASE + id, fade }),
                AnimEvent::ConstructionSoundStart(_) => {}
            }
        }
    }

    /// The draws of one piece: its mesh with the damage-visuals material and the MID parameters, the blueprint ghost, the damage overlay.
    fn draw_piece(&mut self, fx: &mut FxRenderer, out: &mut FxFrame, translucent: &mut Vec<Pending>, eye_ue: Vec3, st: &PieceState) {
        let (def, anim) = (st.def.clone(), st.anim.clone());
        let l2w = Self::l2w(st.location, st.yaw_deg, st.mirrored);
        let mid: Vec<(&str, f32)> = anim.mid.scalars();
        let mid_v: Vec<(&str, [f32; 4])> = anim.mid.vectors();
        let outward = anim.mid.outward_motion_scale;
        let surface_mat = match anim.surface { Surface::Base => None, Surface::Animating => def.animating_material.clone(), Surface::DamagedStatic => def.damaged_material.clone().or(def.animating_material.clone()) };
        let mesh = def.static_mesh.clone();
        // FX SHADOW: the placed piece casts (ABuildingSMActor ctor @14045b960: CastShadow, bCastDynamicShadow, bCastStaticShadow on its mesh component)
        self.casting = true;
        self.draw_mesh(fx, out, translucent, eye_ue, &mesh, None, &mut |me, fx, _slot, own, num_uv| {
            let path = surface_mat.clone().unwrap_or_else(|| own.to_owned());
            match me.material(fx, &path, num_uv, &[]) {
                Ok(e) => {
                    let mut o = Self::overrides_with(&me.entries[e].mat, &mid, &mid_v);
                    // BVISFIX: vfunc_402 writes the MID's own outwardMotion(WS) back times s
                    if let Some(s) = outward { if let Some(v) = find_scalar(&me.entries[e].mat, vis::param::OUTWARD_MOTION) { o.scalars.insert(vis::param::OUTWARD_MOTION.to_owned(), v * s); } }
                    Some((e, o))
                }
                Err(err) => { me.warn(&path, format!("{path}: NOT DRAWN: {err}")); None }
            }
        }, l2w);
        self.casting = false;
        // FX SHADOW: and is a distance-field object (DF shadows + DFAO) with its cooked mesh distance field (see `piece_df`); FX SHADOW SELF-SHADOW BIAS: with the
        // animating override while `vfunc_320`'s damage-visuals state is 1 = `vfunc_272` (`PieceAnim::update_surface`'s `animating`, before its MID fallback)
        let state1 = anim.kind != vis::AnimKind::None || anim.under_construction || anim.mid_fx_active(self.now);
        if let Some(df) = self.piece_df(&mesh, state1) { out.distance_field_objects.push(FxDfInstance { mesh: df, local_to_world: l2w }); }
        if anim.blueprint_visible {
            // BVISFIX: the blueprint component (`vfunc_387` @14048ec40) is the full mesh: slot 0 gets the blueprint MID (pulse, bounce, Code_Emissive,
            // break / opacity parameters), the other slots keep the mesh's own materials
            let pulse = anim.blueprint_pulse;
            let mut bs: Vec<(&str, f32)> = vec![(vis::param::JONS_DANCE_TIME, pulse), (vis::param::INTERACT, pulse)];
            bs.extend(anim.bp.scalars());
            let bv = anim.bp.vectors();
            let d = def.clone();
            self.draw_mesh(fx, out, translucent, eye_ue, &mesh, None, &mut |me, fx, slot, own, num_uv| {
                if slot == 0 { return me.blueprint_entry(fx, &d, num_uv, &bs, &bv); }
                match me.material(fx, own, num_uv, &[]) { Ok(e) => Some((e, me.entries[e].mat.overrides.clone())), Err(err) => { me.warn(own, format!("{own}: NOT DRAWN: {err}")); None } }
            }, l2w);
        }
        if let Some(ov) = anim.overlay {
            // BVISFIX: the damage overlay (`vfunc_322` @14048ef60): same mesh, shadows off, `DamageMaterial` on every slot
            let (os, ovv) = (ov.scalars(), ov.vectors());
            self.draw_mesh(fx, out, translucent, eye_ue, &mesh, None, &mut |me, fx, _slot, _own, num_uv| {
                match me.material(fx, vis::DAMAGE_MATERIAL, num_uv, &[]) { Ok(e) => Some((e, Self::overrides_with(&me.entries[e].mat, &os, &ovv))), Err(err) => { me.warn("overlay", format!("damage overlay NOT DRAWN: {err}")); None } }
            }, l2w);
        }
    }

    /// DOORS (2026-10-08): one DoorComponent (FUN_1404abb40): `mesh` with slot 0 = `slot0` (`DoorAnimatingMaterial`; `None` = the mesh's own), the other slots
    /// their own materials, MID parameters `mid` / `mid_v` (a player door's MIDs get the piece's writes: `ABuildingWall::vfunc_318` / `vfunc_399`), casting
    /// (CastShadow + bCastDynamicShadow + bCastStaticShadow set by FUN_1404abb40) and a distance-field object like the pieces.
    #[allow(clippy::too_many_arguments)]
    fn draw_door_leaf(&mut self, fx: &mut FxRenderer, out: &mut FxFrame, translucent: &mut Vec<Pending>, eye_ue: Vec3, mesh: &str, slot0: Option<&str>, mid: &[(&str, f32)], mid_v: &[(&str, [f32; 4])], outward: Option<f32>, l2w: Mat4) {
        let slot0 = slot0.map(|s| s.to_owned());
        self.casting = true;
        self.draw_mesh(fx, out, translucent, eye_ue, mesh, None, &mut |me, fx, slot, own, num_uv| {
            let path = if slot == 0 { slot0.clone().unwrap_or_else(|| own.to_owned()) } else { own.to_owned() };
            match me.material(fx, &path, num_uv, &[]) {
                Ok(e) => {
                    let mut o = Self::overrides_with(&me.entries[e].mat, mid, mid_v);
                    if let Some(s) = outward { if let Some(v) = find_scalar(&me.entries[e].mat, vis::param::OUTWARD_MOTION) { o.scalars.insert(vis::param::OUTWARD_MOTION.to_owned(), v * s); } }
                    Some((e, o))
                }
                Err(err) => { me.warn(&path, format!("door {path}: NOT DRAWN: {err}")); None }
            }
        }, l2w);
        self.casting = false;
        if let Some(df) = self.piece_df(mesh, false) { out.distance_field_objects.push(FxDfInstance { mesh: df, local_to_world: l2w }); }
    }

    fn draw_trap(&mut self, fx: &mut FxRenderer, out: &mut FxFrame, translucent: &mut Vec<Pending>, eye_ue: Vec3, t: &crate::trap_hook::TrapDraw) {
        let sc: Vec<(&str, f32)> = t.scalars.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        let vc: Vec<(&str, [f32; 4])> = t.vectors.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        self.casting = t.casts;
        self.draw_mesh(fx, out, translucent, eye_ue, &t.mesh, None, &mut |me, fx, slot, own, num_uv| {
            let path = t.all_slots.clone().or_else(|| t.materials.get(slot).cloned().flatten()).unwrap_or_else(|| own.to_owned());
            match me.material(fx, &path, num_uv, &[]) {
                Ok(e) => Some((e, Self::overrides_with(&me.entries[e].mat, &sc, &vc))),
                Err(err) => { me.warn(&path, format!("trap {path}: NOT DRAWN: {err}")); None }
            }
        }, t.l2w);
        self.casting = false;
    }

    /// DOORS: a player piece's door leaf at its swing (`door_hook::leaf_l2w` from the piece transform), with the piece's MID parameters and, while the piece
    /// shows its blueprint, the door's own blueprint component (`ABuildingWall::vfunc_387` @1404c6560: attached to the DoorComponent, the door mesh, slot 0
    /// = a blueprint MID that joins the piece's blueprint MID list; destroyed with the piece's by `vfunc_388`).
    fn draw_piece_door(&mut self, fx: &mut FxRenderer, out: &mut FxFrame, translucent: &mut Vec<Pending>, eye_ue: Vec3, st: &PieceState, d: &crate::door_hook::PieceDoorView) {
        let (def, anim) = (st.def.clone(), st.anim.clone());
        let l2w = crate::door_hook::leaf_l2w(st.location, glam::Quat::from_rotation_z(st.yaw_deg.to_radians()), Vec3::new(if st.mirrored { -1. } else { 1. }, 1., 1.), d.def.offset, d.closed_yaw, d.yaw);
        let (mid, mid_v) = (anim.mid.scalars(), anim.mid.vectors());
        self.draw_door_leaf(fx, out, translucent, eye_ue, &d.def.mesh, d.def.animating_material.as_deref(), &mid, &mid_v, anim.mid.outward_motion_scale, l2w);
        if anim.blueprint_visible {
            let pulse = anim.blueprint_pulse;
            let mut bs: Vec<(&str, f32)> = vec![(vis::param::JONS_DANCE_TIME, pulse), (vis::param::INTERACT, pulse)];
            bs.extend(anim.bp.scalars());
            let bv = anim.bp.vectors();
            self.draw_mesh(fx, out, translucent, eye_ue, &d.def.mesh, None, &mut |me, fx, slot, own, num_uv| {
                if slot == 0 { return me.blueprint_entry(fx, &def, num_uv, &bs, &bv); }
                match me.material(fx, own, num_uv, &[]) { Ok(e) => Some((e, me.entries[e].mat.overrides.clone())), Err(err) => { me.warn(own, format!("{own}: NOT DRAWN: {err}")); None } }
            }, l2w);
        }
    }

    fn draw_edit(&mut self, fx: &mut FxRenderer, out: &mut FxFrame, translucent: &mut Vec<Pending>, eye_ue: Vec3, ev: &EditVisuals) {
        use fn_building::edit::data::assets as ea;
        let actor = Self::l2w(ev.actor_location + self.offset, ev.actor_yaw_deg, ev.actor_mirrored);
        let tile_mat = format!("{0}.{1}", ev.tile_material, ev.tile_material.rsplit('/').next().unwrap_or(""));
        for t in &ev.tiles {
            let l2w = actor * Mat4::from_scale_rotation_translation(t.local_scale, glam::Quat::IDENTITY, t.local_position);
            let tex = format!("{0}.{1}", t.texture, t.texture.rsplit('/').next().unwrap_or(""));
            let (active, interact, color) = (t.active, t.interact, t.color);
            let tm = tile_mat.clone();
            self.draw_mesh(fx, out, translucent, eye_ue, ev.tile_mesh, None, &mut |me, fx, _s, _own, num_uv| {
                let e = match me.material(fx, &tm, num_uv, &[(fn_building::edit::data::material_params::TEXTURE, tex.clone())]) { Ok(e) => e, Err(err) => { me.warn("tile", format!("edit tile material NOT DRAWN: {err}")); return None } };
                // editing agent: InteractParameter = what the exe last wrote (hover 1/0, match flash curve), material default until written
                let mut sc = vec![(fn_building::edit::data::material_params::IS_X, if active { 0. } else { 1. })];
                if let Some(v) = interact { sc.push((fn_building::edit::data::material_params::INTERACT, v)); }
                let o = Self::overrides_with(&me.entries[e].mat, &sc, &[(fn_building::edit::data::material_params::COLOR, color)]);
                Some((e, o))
            }, l2w);
        }
        for a in &ev.aux {
            let l2w = actor * Mat4::from_translation(a.local_position) * Mat4::from_mat3(yaw_mat(a.local_yaw_deg));
            let tex = format!("{0}.{1}", a.texture, a.texture.rsplit('/').next().unwrap_or(""));
            let mat = format!("{0}.{1}", ea::STAIR_AUX_MATERIAL, ea::STAIR_AUX_MATERIAL.rsplit('/').next().unwrap_or(""));
            let scalars = a.scalars;
            self.draw_mesh(fx, out, translucent, eye_ue, ea::STAIR_AUX_MESH, None, &mut |me, fx, _s, _own, num_uv| {
                let e = match me.material(fx, &mat, num_uv, &[(fn_building::edit::data::material_params::TEXTURE, tex.clone())]) { Ok(e) => e, Err(err) => { me.warn("aux", format!("stair indicator NOT DRAWN: {err}")); return None } };
                // editing agent: Stair::vfunc_103 @1404c3da0 scalars (sineOffset, isblinkingSign, isX) per indicator mode
                let names = fn_building::edit::data::material_params::AUX_SCALARS;
                let sc: Vec<(&str, f32)> = (0..3).map(|k| (names[k], scalars[k])).collect();
                Some((e, Self::overrides_with(&me.entries[e].mat, &sc, &[])))
            }, l2w);
        }
        if let Some(pv) = ev.preview.as_ref().filter(|p| p.visible) {
            let mesh = match pv.static_mesh.clone().map(Ok).unwrap_or_else(|| self.def(&pv.class_path).map(|d| d.static_mesh.clone())) { Ok(m) => m, Err(e) => { self.warn(&pv.class_path, format!("edit preview {}: {e}", pv.class_path)); return } };
            let l2w = Self::l2w(pv.location + self.offset, pv.yaw_deg, pv.mirrored);
            let mat = format!("{0}.{1}", pv.material, pv.material.rsplit('/').next().unwrap_or(""));
            self.draw_mesh(fx, out, translucent, eye_ue, &mesh, None, &mut |me, fx, _s, _own, num_uv| {
                let e = match me.material(fx, &mat, num_uv, &[]) { Ok(e) => e, Err(err) => { me.warn("editpv", format!("edit preview material NOT DRAWN: {err}")); return None } };
                Some((e, me.entries[e].mat.overrides.clone()))
            }, l2w);
        }
    }

    fn tick_effects(&mut self, fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
        let Some(f) = self.fx.as_mut() else { return };
        let eye = [view.eye.x, view.eye.y, view.eye.z];
        let ground = |_x: f32, _y: f32| 0.0f32;
        let env = fn_particles::Env { ground: Some(&ground) };
        f.dxbc.begin(view);
        let mut errs = vec![];
        for inst in f.live.iter_mut() {
            inst.tick(dt, &env);
            let mut d = vec![];
            inst.draw(eye, &mut d);
            let bounds = crate::particles::instance_bounds(inst, &f.catalog); // SORTFIX: the effect component's bounds (Primitive buffer + sort origin)
            for e in &d {
                if f.no_draw.contains(&format!("{}/{}", e.system, e.emitter)) { continue; }
                if let Err(err) = f.dxbc.draw_emitter(fx, e, &inst.xf, &bounds, dt) { errs.push((format!("{}/{}", e.system, e.emitter), err)); }
            }
        }
        f.live.retain(|i| !i.finished());
        let fr = f.dxbc.take_frame();
        out.buffer_writes.extend(fr.buffer_writes);
        out.draws.extend(fr.draws);
        for (k, e) in errs { self.warn(&k, format!("{k}: NOT DRAWN: {e}")); }
    }

    pub fn report(&self) -> String { if self.log.is_empty() { String::new() } else { format!("building visuals:\n  {}\n", self.log.join("\n  ")) } }
}

// ---- app glue: one instance, fed by the building core every frame ----------------------------------------------------------------
static STATE: Mutex<Option<BuildingRender>> = Mutex::new(None);

/// LOAD TIME (2026-10-06): the standard pieces' materials parsed by [`prewarm`] (path -> material). Not consumed: every request gets a clone. That is
/// cheap because the cooked programs are shared, and the blueprint material is built once per piece type with that piece's MID textures.
static PRELOADED: Mutex<Option<HashMap<String, Arc<Material>>>> = Mutex::new(None);
/// A clone of the prewarmed material for `path` (None before the prewarm finished, or for a material it did not cover: then it loads as before).
fn preloaded(path: &str) -> Option<Material> { PRELOADED.lock().ok()?.as_ref()?.get(path).map(|m| (**m).clone()) }

/// LOAD TIME (2026-10-06): the first build-mode equip froze the frame ~0.37 s (the blueprint material's `Material::load` parses its cooked shader map,
/// 150-200 ms, plus its textures). So did the first piece of each type (its mesh, animating and damaged materials). This parses those materials for
/// the standard pieces on a pool during the loading screen, and decodes their textures: Wood / Stone / Metal level 1 Solid, Floor, StairW, RoofC, plus
/// the blueprint and default-surface materials. Same packages and values, only earlier.
/// Browser build too: there `fn_fx::gpu::PREDECODE_BUDGET` is 0, so only the parsed materials are held (no texture chains). The browser agent measured
/// 0.6-0.7 s first-placement hitches without it (verification/browser/launch4).
pub fn prewarm() {
    let Some((set, roots)) = STATE.lock().ok().and_then(|g| g.as_ref().map(|b| (b.set.clone(), b.asset_roots.clone()))) else { return };
    let spawned = std::thread::Builder::new().name("building prewarm".into()).spawn(move || {
        let t0 = std::time::Instant::now();
        let mut paths: Vec<String> = vec![vis::BLUEPRINT_MATERIAL.to_owned(), fn_fx::gpu::DEFAULT_SURFACE_MATERIAL.to_owned()];
        // the construction / destruction particle materials (the `particles-building` catalog `init_particles` loads, same folder rule). Held here only to
        // keep their shared shader maps alive: their first spawn's `Material::load` (pickups_dxbc) then skips the parse. The browser agent measured
        // 0.4-0.6 s first-spawn hitches (M_Wall_Sparkle2, M_Smoke_Destruction, M_Wood_Chips)
        let fx_dir = fn_live::var_os("FN_BUILDING_FX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-building")).find(|d| d.join("index.json").is_file()));
        if let Some(cat) = fx_dir.and_then(|d| Catalog::load(&d).ok()) {
            for sys in cat.systems() { for e in &sys.emitters { for l in &e.lods { let p = &l.required.material; if !p.is_empty() && !paths.contains(p) { paths.push(p.clone()); } } } }
        }
        let mut anims: Vec<String> = vec![];
        // BROWSER PORT: these package reads (the piece classes and their meshes) are kept for the loading screen's warm-up, which reads them again on
        // the page's main thread (load_def / load_mesh), where every read waits for the network (fn_pak::retain_reads; natively a plain call)
        fn_pak::retain_reads(|| for m in [fn_building::pieces::Material::Wood, fn_building::pieces::Material::Stone, fn_building::pieces::Material::Metal] {
            for variant in ["Solid", "Floor", "StairW", "RoofC"] {
                let Ok(def) = load_def(&set, &vis::piece_class(m, 0, variant)) else { continue };
                if let Some(a) = &def.animating_material { anims.push(a.clone()); }
                for p in [def.animating_material.clone(), def.damaged_material.clone()].into_iter().flatten() { if !paths.contains(&p) { paths.push(p); } }
                // the mesh's slot materials (what `load_mesh` lists as `MeshGpu::material_paths`)
                let long = fn_fx::data::long_package(&def.static_mesh).to_owned();
                let Ok((a, x, b)) = fn_fx::data::package_files(&set, &long) else { continue };
                let Ok(pkg) = fn_ueio::Package::parse(&a, &x, &b) else { continue };
                let Ok(mesh) = fn_mesh::StaticMesh::parse(&pkg) else { continue };
                for p in mesh.materials.iter().filter_map(|s| s.material.clone()) { if !paths.contains(&p) { paths.push(p); } }
            }
        });
        let next = std::sync::atomic::AtomicUsize::new(0);
        let out: Mutex<HashMap<String, Arc<Material>>> = Mutex::new(HashMap::new());
        std::thread::scope(|sc| {
            for _ in 0..std::thread::available_parallelism().map_or(4, |p| p.get()).clamp(2, 6).min(paths.len().max(1)) {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(path) = paths.get(i) else { break };
                    if let Ok(m) = load_material(&set, path) { if let Ok(mut o) = out.lock() { o.insert(path.clone(), Arc::new(m)); } }
                });
            }
        });
        let out = out.into_inner().unwrap_or_default();
        let t_mats = t0.elapsed();
        // their 2D textures, and each piece's blueprint MID texture (`blueprint_params` of its animating material), decoded for `TextureCache::load`
        let mut textures: Vec<String> = vec![];
        for m in out.values() { for k in 0..m.uniform.textures2d.len() { if let Some((p, _)) = m.texture_slot(k) { if !textures.contains(&p) { textures.push(p); } } } }
        for a in &anims { if let Some(t) = out.get(a).and_then(|m| blueprint_params(m).texture) { if !textures.contains(&t) { textures.push(t); } } }
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..std::thread::available_parallelism().map_or(4, |p| p.get()).clamp(2, 6).min(textures.len().max(1)) {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(path) = textures.get(i) else { break };
                    fn_fx::gpu::predecode_texture(&set, path);
                });
            }
        });
        eprintln!("[building] prewarm: {} of {} materials parsed in {:.0} ms, {} textures decoded in {:.0} ms", out.len(), paths.len(), t_mats.as_secs_f32() * 1000., textures.len(), (t0.elapsed() - t_mats).as_secs_f32() * 1000.);
        if let Ok(mut g) = PRELOADED.lock() { let m = g.get_or_insert_with(HashMap::new); for (k, v) in out { m.entry(k).or_insert(v); } }
    });
    if let Err(e) = spawned { eprintln!("[building] prewarm not started: {e}"); }
}

fn prewarm_doors(set: &Arc<PakSet>) {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.load(std::sync::atomic::Ordering::Relaxed) { return; }
    let Some(poi) = crate::poi::current() else { return };
    if poi.doors.is_empty() || STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) { return; }
    let set = set.clone();
    let spawned = std::thread::Builder::new().name("door prewarm".into()).spawn(move || {
        let t0 = std::time::Instant::now();
        let mut meshes: Vec<String> = vec![];
        let mut paths: Vec<String> = vec![];
        for d in &poi.doors {
            if !meshes.contains(&d.def.mesh) { meshes.push(d.def.mesh.clone()); }
            if let Some(m) = &d.def.animating_material { if !paths.contains(m) { paths.push(m.clone()); } }
        }
        for mesh in &meshes {
            let long = fn_fx::data::long_package(mesh).to_owned();
            let Ok((a, x, b)) = fn_fx::data::package_files(&set, &long) else { continue };
            let Ok(pkg) = fn_ueio::Package::parse(&a, &x, &b) else { continue };
            let Ok(sm) = fn_mesh::StaticMesh::parse(&pkg) else { continue };
            for p in sm.materials.iter().filter_map(|s| s.material.clone()) { if !paths.contains(&p) { paths.push(p); } }
        }
        let have: HashSet<String> = PRELOADED.lock().ok().and_then(|g| g.as_ref().map(|m| m.keys().cloned().collect())).unwrap_or_default();
        paths.retain(|p| !have.contains(p));
        let next = std::sync::atomic::AtomicUsize::new(0);
        let out: Mutex<HashMap<String, Arc<Material>>> = Mutex::new(HashMap::new());
        std::thread::scope(|sc| {
            for _ in 0..std::thread::available_parallelism().map_or(4, |p| p.get()).clamp(2, 4).min(paths.len().max(1)) {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(path) = paths.get(i) else { break };
                    if let Ok(m) = load_material(&set, path) { if let Ok(mut o) = out.lock() { o.insert(path.clone(), Arc::new(m)); } }
                });
            }
        });
        let out = out.into_inner().unwrap_or_default();
        let mut textures: Vec<String> = vec![];
        for m in out.values() { for k in 0..m.uniform.textures2d.len() { if let Some((p, _)) = m.texture_slot(k) { if !textures.contains(&p) { textures.push(p); } } } }
        if let Ok(mut g) = PRELOADED.lock() { let m = g.get_or_insert_with(HashMap::new); for (k, v) in out.iter() { m.entry(k.clone()).or_insert(v.clone()); } }
        for t in &textures { fn_fx::gpu::predecode_texture(&set, t); }
        eprintln!("[building] door prewarm: {} door meshes, {} materials parsed, {} textures decoded in {:.0} ms", meshes.len(), out.len(), textures.len(), t0.elapsed().as_secs_f32() * 1000.);
    });
    if let Err(e) = spawned { eprintln!("[building] door prewarm not started: {e}"); }
}

/// Creates the renderer side (call once with the install and the asset roots). `false` when the paks cannot be opened.
pub fn init(install: &Path, asset_roots: &[PathBuf]) -> bool {
    match BuildingRender::new(install, asset_roots) { Ok(b) => { *STATE.lock().unwrap() = Some(b); true } Err(e) => { eprintln!("[building] visuals unavailable: {e}"); false } }
}
pub fn is_ready() -> bool { STATE.lock().map(|g| g.is_some()).unwrap_or(false) }
/// FIRST USE (2026-10-06): whether the loading screen runs the first-use warm-ups (this one and `pickups_fx::prebuild_step`). Natively always. In the browser
/// build too since its FX pipelines compile asynchronously (fn-render fx_async.rs: in parallel on the browser's worker threads; synchronously each cost
/// 0.5-1.1 s of the GPU process, verification/browser/launch8, launch14); `FN_WARM_WEB=0` (page query `?FN_WARM_WEB=0`) turns them off there.
pub fn warm_enabled() -> bool { !cfg!(target_arch = "wasm32") || fn_live::var("FN_WARM_WEB").map_or(true, |v| v != "0") }
/// FIRST USE (2026-10-06): one budgeted step of the warm-up ([`BuildingRender::warm_step`]): call every loading-screen frame until it returns true
/// (`App::finishing_frame`, after the statics' `prebuild_step`). True at once when there are no building visuals.
pub fn prebuild_step(fx: &mut FxRenderer, budget_ms: f32) -> bool {
    if !warm_enabled() { return true; }
    let Ok(mut g) = STATE.lock() else { return true };
    match g.as_mut() { Some(b) => b.warm_step(fx, std::time::Duration::from_secs_f32(budget_ms.max(0.0) / 1000.0)), None => true }
}
pub fn prebuild_doors(fx: &mut FxRenderer, budget_ms: f32) -> bool {
    if !warm_enabled() { return true; }
    static AT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let Some(poi) = crate::poi::current() else { return true };
    let Ok(mut g) = STATE.lock() else { return true };
    let Some(b) = g.as_mut() else { return true };
    let t = std::time::Instant::now();
    prewarm_doors(&b.set);
    b.init_gpu(fx);
    b.init_particles(fx);
    let _ = b.gamedata();
    let mut kinds: Vec<(String, Option<String>)> = vec![];
    for d in &poi.doors { let k = (d.def.mesh.clone(), d.def.animating_material.clone()); if !kinds.contains(&k) { kinds.push(k); } }
    let view = *b.view.get_or_insert(WARM_VIEW);
    let eye = fn_fx::view::to_ue(view.eye).as_vec3();
    let (mut out, mut tr) = (FxFrame::default(), vec![]);
    while let Some((mesh, mat)) = kinds.get(AT.load(std::sync::atomic::Ordering::Relaxed)).cloned() {
        AT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for mirrored in [false, true] { b.draw_door_leaf(fx, &mut out, &mut tr, eye, &mesh, mat.as_deref(), &[], &[], None, BuildingRender::l2w(WARM_AT, 0.0, mirrored)); }
        b.used = 0;
        b.shadow_written.clear();
        if t.elapsed().as_secs_f32() * 1000. >= budget_ms { break; }
    }
    let done = AT.load(std::sync::atomic::Ordering::Relaxed) >= kinds.len();
    if done { static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false); if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) { eprintln!("[building] door warm-up: {} door kinds of {} map doors", kinds.len(), poi.doors.len()); } }
    done
}
/// The frame of the building visuals (see [`BuildingRender::frame`]); no-op before [`init`].
pub fn frame(fx: &mut FxRenderer, world: &BuildingWorld, offset: Vec3, preview: Option<&Preview>, edit: Option<&EditVisuals>, view: &ViewParams, dt: f32, out: &mut FxFrame) -> Vec<SoundEvent> {
    let Ok(mut g) = STATE.lock() else { return vec![] };
    match g.as_mut() { Some(b) => b.frame(fx, world, offset, preview, edit, view, dt, out), None => vec![] }
}
/// CAMERA CUT: the next frame is a camera cut (`PlayerCameraManager::bGameCameraCutThisFrame` -> `FSceneView::bCameraCut`, LocalPlayer.cpp:690): the pieces write no
/// base-pass velocity on it (see `draw_mesh`). The port has no camera-cut source yet: call this when one exists.
#[allow(dead_code)]
pub fn camera_cut() { if let Ok(mut g) = STATE.lock() { if let Some(b) = g.as_mut() { b.cut_pending = true; } } }
pub fn report() -> String { STATE.lock().ok().and_then(|g| g.as_ref().map(|b| b.report())).unwrap_or_default() }
/// BVISFIX: a damaging hit on piece `id` (called by the hit tests when they queue the damage): `impact_cm` = the HitResult ImpactPoint (world cm, the
/// `BuildingWorld` space), `to_attacker` = unit vector from the impact towards the damage causer. Consumed by the piece's next health loss (`vfunc_276`).
pub fn report_hit(id: PieceId, impact_cm: Vec3, to_attacker: Vec3) {
    if let Ok(mut g) = STATE.lock() { if let Some(b) = g.as_mut() { let q = b.pending_hits.entry(id).or_default(); q.push_back((impact_cm, to_attacker.normalize_or(Vec3::Z))); if q.len() > 64 { q.pop_front(); } } }
}
/// The `SoundEvent` of an edit-mode cue (`EditHook::drain_sound_cues`: `BuildPreviewEditEnter` / `Exit` / `Tile` of `PlayerUIFeedback`), 2D at the listener.
pub fn edit_cue_event(cue: &str) -> Option<SoundEvent> { fn_audio_sink::building_cue(cue).map(|c| SoundEvent::BuildingCue { cue: c, pos: None }) }

// ---- verification scene (FNRT_BUILDING_DEMO=1): pieces in front of the spawn, independent of the input / core agents ---------------
/// A demo world: the four BR pieces in the three tiers, one building, one damaged, so a screenshot shows every visual state. The class paths are the
/// `BuildingItemData_*` defaults of the core (wall `PBWA_*1_Solid`, floor `PBWA_*1_Floor`, ramp `PBWA_*1_StairW`, cone `PBWA_*1_RoofC`).
pub fn demo_world(origin_ue: Vec3, view_yaw_deg: f32) -> BuildingWorld {
    let (fwd, right) = (yaw_mat(view_yaw_deg) * Vec3::X, yaw_mat(view_yaw_deg + 90.) * Vec3::X);
    use fn_building::pieces::Material as M;
    use fn_building::world::{GridKey, PieceSpawn};
    let mut w = BuildingWorld::new();
    let kinds = [(PieceKind::Wall, "Solid"), (PieceKind::Floor, "Floor"), (PieceKind::Stairs, "StairW"), (PieceKind::Roof, "RoofC")];
    for (mi, m) in [M::Wood, M::Stone, M::Metal].into_iter().enumerate() {
        for (ki, (k, v)) in kinds.iter().enumerate() {
            // columns across the view, rows away from the camera
            let loc = origin_ue + right * ((ki as f32 - 1.5) * 700.) + fwd * (mi as f32 * 800.);
            let class = vis::piece_class(m, 0, v);
            let build_time = if mi == 0 && ki == 0 { 6.0 } else { 0. };
            w.spawn(PieceSpawn { kind: *k, material: m, class_path: class, key: GridKey { kind: *k, cell: glam::IVec3::new(ki as i32, mi as i32, 0), rot: 0 }, location: loc, yaw_deg: view_yaw_deg + 90., max_health: 200., start_health: if build_time > 0. { 20. } else { 200. }, build_time });
        }
    }
    w
}

struct Demo { world: BuildingWorld, damaged: bool, hit: bool, removed: bool, wood_dead: bool, preview: Preview }
static DEMO: Mutex<Option<Demo>> = Mutex::new(None);

/// Verification only (`FNRT_BUILDING_DEMO=1`, nothing happens otherwise): a [`demo_world`] in front of the pawn (`feet_m` = game metres), ticked here; at 2.0 s
/// the wood floor takes 50 % damage (breaking animation, bounce), at 2.5 s the metal wall 30 % (bounce, blueprint glow), at 3.0 s the stone wall dies from a
/// hit (destruction animation, death effect + sound at its centroid), at 4.0 s the wood floor dies; the wood wall builds for 6 s (completion flash, sparkle); a red (refused) preview
/// stands next to the rows. Returns the sounds.
pub fn demo_frame(fx: &mut FxRenderer, feet_m: Vec3, view: &ViewParams, dt: f32, out: &mut FxFrame) -> Vec<SoundEvent> {
    if std::env::var_os("FNRT_BUILDING_DEMO").is_none() || !is_ready() { return vec![]; }
    let mut g = DEMO.lock().unwrap();
    if g.is_none() {
        let f = fn_fx::view::dir_to_ue(view.forward).as_vec3(); let yaw = f.y.atan2(f.x).to_degrees(); let fh = Vec3::new(f.x, f.y, 0.).normalize_or(Vec3::X);
        let o = fn_fx::view::to_ue(feet_m).as_vec3() + fh * 1200.;
        let preview = Preview { class_path: vis::piece_class(fn_building::pieces::Material::Wood, 0, "Solid"), location: o + yaw_mat(yaw + 90.) * Vec3::X * (-2.5 * 700.), yaw_deg: yaw + 90., mirrored: false, valid: std::env::var_os("FNRT_BUILDING_DEMO_VALID").is_some() };
        *g = Some(Demo { world: demo_world(o, yaw), damaged: false, hit: false, removed: false, wood_dead: false, preview });
    }
    let d = g.as_mut().unwrap();
    d.world.tick(dt);
    let t = d.world.time();
    // BVISFIX: every hit is reported like the hit tests do (impact = the piece centroid, towards the camera) so the bounce / blueprint glow / destruction run
    let eye = fn_fx::view::to_ue(view.eye).as_vec3();
    let hit = |w: &mut BuildingWorld, kind: PieceKind, m: fn_building::pieces::Material, dmg: f32| {
        let Some(p) = w.iter().find(|p| p.kind == kind && p.material == m).cloned() else { return };
        let c = p.location + yaw_mat(p.yaw_deg) * vis::centroid_offset(p.kind);
        report_hit(p.id, c, (eye - c).normalize_or(Vec3::Z));
        w.apply_damage(p.id, dmg);
    };
    if t >= 2.0 && !d.damaged { d.damaged = true; hit(&mut d.world, PieceKind::Floor, fn_building::pieces::Material::Wood, 100.); }
    if t >= 2.5 && !d.hit { d.hit = true; hit(&mut d.world, PieceKind::Wall, fn_building::pieces::Material::Metal, 60.); }
    // the stone wall dies from a hit (was: removed without damage), so the destruction animation has its LastDamageAmount / hit direction
    if t >= 3.0 && !d.removed { d.removed = true; hit(&mut d.world, PieceKind::Wall, fn_building::pieces::Material::Stone, 1000.); }
    // and the damaged wood floor dies at 4.0 s (wood destruction + debris)
    if t >= 4.0 && !d.wood_dead { d.wood_dead = true; hit(&mut d.world, PieceKind::Floor, fn_building::pieces::Material::Wood, 1000.); }
    let pv = d.preview.clone();
    frame(fx, &d.world, Vec3::ZERO, Some(&pv), None, view, dt, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn install() -> Option<PathBuf> { fn_live::var_os("FN_TEST_ROOT").map(PathBuf::from).filter(|p| p.is_dir()) }

    /// Every player piece class resolves to a mesh, its animating material carries its own shader map with the static-mesh base pass, and the blueprint params copy.
    #[test] fn piece_materials_have_their_own_shader_maps() {
        let Some(root) = install() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let set = PakSet::open_install(&root).unwrap();
        for m in [fn_building::pieces::Material::Wood, fn_building::pieces::Material::Stone, fn_building::pieces::Material::Metal] {
            for v in ["Solid", "Floor", "StairW", "RoofC"] {
                let class = vis::piece_class(m, 0, v);
                let d = load_def(&set, &class).unwrap();
                eprintln!("{class}: mesh {} anim {:?} damaged {:?} sub {} frame {} level {} cue {}", d.static_mesh, d.animating_material, d.damaged_material, d.animating_sub_objects, d.num_frame_sub_objects, d.level, d.construction_cue());
                let anim = load_material(&set, d.animating_material.as_deref().unwrap()).unwrap();
                assert!(anim.cooked.find("FLocalVertexFactory", VS_TYPE).is_some(), "{class}: no static-mesh base pass VS in {}", anim.path);
                assert!(PS_TYPES.iter().any(|ps| anim.cooked.find("FLocalVertexFactory", ps).is_some()), "{class}: no base pass PS");
                let bp = blueprint_params(&anim);
                assert!(bp.texture.is_some(), "{class}: {bp:?}");
            }
        }
        let bp = load_material(&set, vis::BLUEPRINT_MATERIAL).unwrap();
        assert_eq!(find_vector(&bp, "Code_Red").unwrap()[0], vis::CODE_RED_DEFAULT[0]);
        assert_eq!(find_vector(&bp, "code_Blue").unwrap()[2], vis::CODE_BLUE_DEFAULT[2]);
    }

    /// BVISFIX: the `UFortGameData` curves of the per-frame MID drive load from the paks (the vector curve's static array included), the finished highlight
    /// peaks at 0.3218 s (the sparkle time), the destroy duration / crit rows read, and the CDO level is `CurrentBuildingLevel`.
    #[test] fn game_data_fx_curves_load() {
        let Some(root) = install() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let set = PakSet::open_install(&root).unwrap();
        let (g, errs) = GameDataFx::load(&set);
        assert!(errs.is_empty(), "{errs:?}");
        let fh = g.full_health.as_ref().unwrap();
        let peak = fh.0.keys.iter().fold((0f32, 0f32), |m, k| if k.1 > m.1 { (k.0, k.1) } else { m });
        assert!((peak.0 - 0.3218).abs() < 1e-3 && (peak.1 - 1.9993).abs() < 1e-3, "{peak:?}");
        assert_eq!(fh.time_range(), (0.0, 1.0));
        let bp = g.blueprint_damage.as_ref().unwrap();
        assert!(bp.0[0].0.keys.len() >= 8 && bp.0[1].0.keys.len() >= 2, "{bp:?}");
        assert!(g.bounce.as_ref().unwrap().0.keys.len() > 5 && g.edit_bounce.is_some());
        assert_eq!(g.destroy_duration, [1.5, 1.5, 1.5]);
        eprintln!("crit pct {:?} cap {:?}", g.crit_pct, g.crit_cap);
        for (m, l) in [(fn_building::pieces::Material::Wood, 0), (fn_building::pieces::Material::Stone, 1), (fn_building::pieces::Material::Metal, 2)] {
            assert_eq!(load_def(&set, &vis::piece_class(m, l, "Solid")).unwrap().level, l);
        }
    }

    /// The cooked durations / cues the logic module carries equal the 1.8 data.
    #[test] fn game_data_constants_match_the_paks() {
        let Some(root) = install() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let set = PakSet::open_install(&root).unwrap();
        let pkg = fn_fx::data::PackageJson::load(&set, "/Game/Balance/DefaultGameData").unwrap();
        let cdo = &pkg.exports.iter().find(|e| e.class == "FortGameData" || e.name == "DefaultGameData").unwrap().props;
        let s = cdo["BuildingConstructionSounds[1]"]["Sounds[2]"].as_str().unwrap();
        assert!(s.starts_with(vis::CONSTRUCTION_CUES[1][2]), "{s}");
        let d = cdo["PlayerBuiltBuildingDeathSounds"]["Sounds"].as_str().unwrap();
        assert!(d.starts_with(vis::PLAYER_BUILT_DEATH_CUES[0]), "{d}");
    }
}
