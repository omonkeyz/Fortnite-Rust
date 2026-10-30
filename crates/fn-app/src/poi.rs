//! POI (2026-10-07): the towns and landmarks of 1.8, from `fn-export props poi --all` (`<props>/poi/index.json` + one `<name>.json` per streamed level of
//! `Athena_Terrain`, version 2): every actor of the 29 POI sublevels (`Athena_POI_*`, the spawn island `Athena_POI_Lobby_001` included), of the landscape
//! levels (`Athena_Terrain_LS_00..05`: terraces, cliffs, shacks, cars, fences, ... minus the trees / rocks / bushes the props layout places) and of the building
//! worlds their foundations stream in, with EVERY static-mesh component (cooked mesh + `OverrideMaterials`, full world transform, draw distance,
//! `CastShadow`), drawn by `statics_render` on the real-shader path; and each actor as a [`Harvestable`] carrying the cooked collision of its components (the
//! `BodySetup` simple shapes; the collision-LOD triangles for `CTF_UseComplexAsSimple` bodies) so the pawn walks, climbs and is blocked exactly like by the
//! layout props, and the building data of its class (`FortHealthSet.MaxHealth`, `ResourceType`) so pickaxe / bullets damage, harvest and destroy it.
//!
//! ALL POIS: every file of `index.json` is loaded (a directory without an index: every `*.json`, the old single-POI exports). `FN_POI=0` switches them off.
//! Mapping = the props' (`props::Library::spawn_layout`): UE `(x, y)` about the layout centre, moved to `props::ANCHOR`; height = UE z minus the
//! landscape's `ref_z_cm` (the ground the pawn walks on). A layout placement that IS a POI actor (same class within [`DEDUP_CM`]) is left to this
//! module ([`occupies`]): the landscape files hold no layout class, the POI files hold their level's trees / rocks.
//!
//! STREAMING (CHOSEN, labelled): the placements are grouped per file and per [`CELL_CM`] cell ([`PoiGroup`]); `statics_render` builds a group's kinds
//! (GPU mesh + materials, nearest group first, under a per-frame budget; all at once in scripted runs, [`set_sync_build`]) and draws its placements only
//! while the view is within [`stream_cm`] of the group's bounds. 35000 cm = `r.HLOD.DistanceOverride` of the cooked 1.8 config
//! (docs/spec/cooked_ini_audit.md): beyond it the real client hides the HLOD cluster's children and draws the cluster's `LODActor` proxy instead
//! (NOT PORTED: the proxies, so a POI farther than that is not drawn at all; how the cvar is consumed is UNVERIFIED, it is not in UE4.16 LODActor.cpp).
//! The collision of every actor is always present (it is the sim's, not the renderer's).
//!
//! What a POI actor does (CHOSEN / ASSUMED where the data has no answer, labelled):
//! * damageable = its class has a `<AttributeInitCategory>.<Sub>.FortHealthSet.MaxHealth` row (`AttributesBuildingProps` / `AttributesBuildingSection`) and no
//!   `bIsInvulnerable`; a `FortStaticMeshActor` (terraces, cliffs, shorelines: not a BuildingActor) never is. Max health = the row at curve time 0. ASSUMED
//!   level: every row is flat over times 0..10 (it steps at 11), so any building level <= 10 gives the same number (`CurrentBuildingLevel` of the classes is 0/1).
//! * CHOSEN (loot on hold): the loot spawners `Tiered_*` (chests, ammo boxes) are drawn and block, but are invulnerable; `Tiered_Athena_FloorLoot_01` (editor
//!   marker, `bHiddenInGame`) is not drawn.
//! * NOT PORTED: the structural collapse of POI buildings (pieces losing support do not fall), the per-class hit / death sounds and `DeathParticles` (the
//!   prop-library proto tables do not know POI classes: the generic family sounds play), `bNoCameraCollision` (the camera collides with everything), the
//!   particle systems the files list (the lights are `PoiSet::lights`, lit by local_lights_hook.rs; the loot is chest_hook.rs).
use fn_combat::{Harvestable, Kind, Resource};
use fn_combat::{Hull, HullSet};
use glam::{Mat3, Mat4, Quat, Vec3};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// `salty`: Salty Springs (`Athena_POI_Suburban_006`, level origin (3584, -8192, 1920) cm). CHOSEN: the spawn stands 22 m on +UE-Y (east) of the centre
/// facing -Y (west), i.e. on the east street of the town at (6000, -2600) looking down it; Z = the plate level: the lowest street / sidewalk level
/// (1920 cm) minus 5 cm, so the street slabs lie just above the plate instead of z-fighting with it.
pub const SALTY_CM: [f32; 3] = [6000., -4800., 1915.];
/// STREAMING: side of a placement group's cell (UE cm). CHOSEN: 256 m, the tile of docs/spec/full_map_plan.md step 3.
pub const CELL_CM: f32 = 25600.;
/// STREAMING: a group's radius = its placements' origins' bounding sphere plus this (UE cm). CHOSEN: 40 m, more than the half length of the longest
/// terrace / cliff / road pieces (`S_Elevation_*`, `S_Cave_*`: 20-30 m), so a mesh whose origin is in a far group is not cut while visible.
pub const MESH_MARGIN_CM: f32 = 4000.;
/// STREAMING: `r.HLOD.DistanceOverride` (cm), the default of [`stream_cm`].
pub const HLOD_DISTANCE_CM: f32 = 35000.;
/// A layout placement within this distance (cm, xy) of a POI actor of the same class is that actor ([`occupies`]; `props layout` rounds to 1 cm).
pub const DEDUP_CM: f32 = 5.;

static WORLD_CENTER: OnceLock<[f32; 3]> = OnceLock::new();
static POI: Mutex<Option<Arc<PoiSet>>> = Mutex::new(None);
static SYNC_BUILD: AtomicBool = AtomicBool::new(false);

/// `FN_POI=0`: no POI (the layout props of the area are placed instead).
pub fn enabled() -> bool { fn_live::var("FN_POI").map_or(true, |v| v != "0") }
/// STREAMING: the residency distance (cm, view to a group's bounds): `FN_POI_STREAM_M` (dev) or [`HLOD_DISTANCE_CM`].
pub fn stream_cm() -> f32 { static V: OnceLock<f32> = OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_POI_STREAM_M").ok().and_then(|v| v.parse::<f32>().ok()).map_or(HLOD_DISTANCE_CM, |m| m * 100.)) }
/// WORLD (HLOD): `r.HLOD.DistanceOverride` of the cooked 1.8 config (cm; docs/spec/cooked_ini_audit.md): when non-zero it replaces every cluster's
/// MinDrawDistance (FLODSceneTree::UpdateAndApplyVisibilityStates @1419a5960 reads the cvar, clamped to >= 0, and squares it with r.HLOD.DistanceScale = 1).
/// `FN_HLOD_M` (dev) overrides it.
pub fn hlod_distance_cm() -> f32 { static V: OnceLock<f32> = OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_HLOD_M").ok().and_then(|v| v.parse::<f32>().ok()).map_or(HLOD_DISTANCE_CM, |m| m * 100.)) }
/// STREAMING: scripted runs build every resident group's kinds in the frame it becomes resident (no pop-in in a screenshot); the live game spreads it.
pub fn set_sync_build(on: bool) { SYNC_BUILD.store(on, Ordering::Relaxed); }
pub fn sync_build() -> bool { SYNC_BUILD.load(Ordering::Relaxed) }

/// `--world-center`: `salty` or `X,Y[,Z]` (UE world cm; Z = the UE height of the plate, 0 = the landscape's ground reference under the point).
pub fn parse_center(s: &str) -> Result<[f32; 3], String> {
    if s.eq_ignore_ascii_case("salty") { return Ok(SALTY_CM); }
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse::<f32>()).collect::<Result<_, _>>().map_err(|_| format!("--world-center: expected salty or X,Y[,Z] in UE cm, got {s:?}"))?;
    match v.len() { 2 => Ok([v[0], v[1], 0.]), 3 => Ok([v[0], v[1], v[2]]), _ => Err("--world-center: expected X,Y[,Z]".into()) }
}
pub fn set_world_center(c: [f32; 3]) { let _ = WORLD_CENTER.set(c); }
/// The overridden layout centre (UE cm), `None` = the export's sample centre.
pub fn world_center() -> Option<[f32; 3]> { WORLD_CENTER.get().copied() }

pub struct PoiKind {
    pub id: String,
    pub mesh: String,
    pub overrides: Vec<Option<String>>,
    /// WORLD (2026-10-07): the component's painted `OverrideVertexColors` per LOD (raw FColor bytes B G R A; `None` = that LOD keeps the mesh's own colours).
    /// UE4.16 `FStaticMeshSceneProxy` binds them as the LOD's colour stream in place of the mesh's (StaticMeshRender.cpp `FLODInfo`): the street /
    /// terrace pieces' `Terrain_Mat` picks grass / asphalt / rock by them. Part of the kind id (a painted component is its own kind).
    pub vcol: Option<Arc<Vec<Option<Vec<[u8; 4]>>>>>,
    /// WORLD (2026-10-07): the BuildingSMActor's `TextureData` MID on material slot 0 (exporter `texture_data_mid`, ABuildingSMActor vfunc_310 @140461f50):
    /// (base material: TextureData[0].OverrideMaterial, `None` = the mesh's material 0; texture parameter name -> texture). Part of the kind id.
    pub mid0: Option<Arc<(Option<String>, Vec<(String, String)>)>>,
}
/// WORLD (2026-10-07): one HLOD cluster (`LODActor` of a level): its merged proxy mesh (the level's `_HLOD` package), drawn INSTEAD of its sub actors while the
/// view is at least [`hlod_distance_cm`] from the proxy's bounds box (`FLODSceneTree::UpdateAndApplyVisibilityStates` @1419a5960: squared box-to-view-origin
/// distance >= MinDrawDistance^2, MinDrawDistance replaced by `r.HLOD.DistanceOverride` when that is non-zero).
pub struct HlodCluster {
    pub name: String,
    /// The proxy mesh's kind (`PoiSet::kinds`).
    pub kind: usize,
    pub l2w: Mat4,
    /// The proxy component's `MinDrawDistance` (= the actor's `LODDrawDistance`), cm.
    pub min_cm: f32,
    /// World bounds box of the proxy, FX space cm.
    pub lo: Vec3,
    pub hi: Vec3,
    pub cast_shadow: bool,
}
impl HlodCluster {
    /// FLODSceneTree: is the proxy drawn (and its sub actors hidden) for a view at `eye` (FX space cm)?
    pub fn proxy_visible(&self, eye: Vec3) -> bool {
        let d = eye.clamp(self.lo, self.hi) - eye;
        let m = if hlod_distance_cm() > 0. { hlod_distance_cm() } else { self.min_cm };
        d.length_squared() >= m * m
    }
}
/// No HLOD cluster.
pub const NO_CLUSTER: u32 = u32::MAX;
pub struct PoiPlacement {
    pub kind: usize,
    pub class: String,
    /// FX space (UE cm, Z up, about the game origin).
    pub l2w: Mat4,
    /// Index of the actor (`PoiSet::classes`, `fn_combat::POI_PROTO | actor` on its harvestable): a destroyed actor's components are not drawn.
    pub actor: u32,
    /// The component's `CachedMaxDrawDistance` / `LDMaxDrawDistance` (cm, 0 = none).
    pub cull_cm: f32,
    pub cast_shadow: bool,
    /// WORLD: the HLOD cluster whose proxy replaces this component ([`NO_CLUSTER`] = none).
    pub cluster: u32,
}
/// STREAMING: the placements of one file inside one [`CELL_CM`] cell, streamed together.
pub struct PoiGroup {
    /// The file (`PoiSet::files`).
    pub file: usize,
    /// Bounding sphere of the placements' origins plus [`MESH_MARGIN_CM`], FX space (UE cm about the game origin).
    pub center: Vec3,
    pub radius: f32,
    pub placements: Vec<u32>,
    /// The distinct kinds of those placements.
    pub kinds: Vec<u32>,
    /// Every kind of the group was built (`statics_render`): its placements are drawn while resident.
    pub ready: AtomicBool,
    /// Within [`stream_cm`] of the view (`statics_render`, with hysteresis).
    pub resident: AtomicBool,
    /// WORLD: the HLOD clusters of its placements: the group is resident while any of them shows its sub actors (the view nearer than the HLOD distance).
    pub clusters: Vec<u32>,
    /// WORLD: the largest draw distance (cm) of its placements without a cluster ([`stream_cm`] for one without a draw distance; 0 = none): the group is
    /// resident while the view is within it.
    pub reach_cm: f32,
    pub cull_reach_cm: f32,
    pub stream_reach_cm: f32,
}
/// DOORS (2026-10-08): one door wall of the files' `doors[]` (the runtime-created DoorComponent of an `ABuildingWall`, `fn_building::door`), drawn / swung
/// / collided by door_hook.rs.
pub struct PoiDoor {
    /// The wall actor (`PoiSet::classes` index; its harvestable proto is `fn_combat::POI_PROTO | actor`): the door goes with the wall.
    pub actor: u32,
    /// The streaming group of the wall's placements ([`NO_CLUSTER`] = none): the door is drawn while that group is resident and ready.
    pub group: u32,
    pub def: fn_building::door::DoorDef,
    /// The wall's root world transform, FX space (UE cm about the game origin).
    pub t: Vec3,
    pub q: Quat,
    pub s: Vec3,
    pub mirrored: bool,
    /// The door mesh's cooked collision, mesh-local cm, one point cloud per convex ([`mesh_pieces`]).
    pub pieces: Arc<Vec<Vec<Vec3>>>,
}

/// CONTAINERS (2026-10-08): one loot container of the files' `loot[]` (chests `Tiered_Chest_*`, ammo boxes `Tiered_Short_Ammo_*`, every other
/// `Tiered_*` container; the `FloorLoot` spawners are not containers), with its SCS components the POI export recorded (`particles[]` / `lights[]` of the
/// same actor). Searched / opened / culled by chest_hook.rs; its static mesh is a normal placement (hidden with the actor's dead harvestable).
pub struct PoiContainer {
    /// The actor (`PoiSet::classes` index; its harvestable proto is `fn_combat::POI_PROTO | actor`).
    pub actor: u32,
    pub class: String,
    /// The class default `SearchLootTierGroup` (the instances do not override it in 1.8).
    pub tier_group: String,
    /// Root world transform, FX space (UE cm about the game origin).
    pub t: Vec3,
    pub q: Quat,
    pub s: Vec3,
    /// UE world location (cm) of the root (reports / dedup).
    pub ue: [f32; 3],
    /// The root mesh component's `CachedMaxDrawDistance` (cm, 0 = none): the SCS components' instance draw distance in the 1.8 levels.
    pub cull_cm: f32,
    /// The ParticleSystemComponents: (component name, template object path, world l2w FX space, `bAutoActivate`).
    pub particles: Vec<(String, String, Mat4, bool)>,
    /// The point light components: (component name, merged properties, world location FX space).
    pub lights: Vec<(String, Value, Vec3)>,
    /// The streaming group of its placements (`NO_CLUSTER` = none).
    pub group: u32,
    /// The pseudo actor whose placement is the container's `SearchedMesh` (its harvestable starts dead; chest_hook.rs swaps the two on search;
    /// `NO_CLUSTER` = none). Added by `chest_hook::add_searched`.
    pub searched_actor: u32,
}

#[derive(Clone, Debug)]
pub struct PoiFloorLoot {
    pub class: String,
    pub tier_group: String,
    pub t: Vec3,
    pub q: Quat,
    pub ue: [f32; 3],
    pub group: u32,
}

#[derive(Clone, Debug)]
pub struct PoiHidden {
    pub actor: u32,
    pub comp: String,
    pub mesh: String,
    pub overrides: Vec<Option<String>>,
    pub l2w: Mat4,
    pub cull_cm: f32,
    pub cast_shadow: bool,
}

#[derive(Clone, Debug)]
pub struct PoiLight {
    pub actor: u32,
    pub actor_class: String,
    pub component: String,
    pub kind: String,
    pub hidden: bool,
    pub t: Vec3,
    pub q: Quat,
    pub actor_ue: [f32; 3],
    pub props: Value,
}

#[derive(Default)]
pub struct PoiSet {
    pub lights: Vec<PoiLight>,
    /// CONTAINERS (2026-10-08): every loot container of the files.
    pub containers: Vec<PoiContainer>,
    pub floor_loot: Vec<PoiFloorLoot>,
    /// DOORS: every door wall of the files.
    pub doors: Vec<PoiDoor>,
    pub kinds: Vec<PoiKind>,
    pub kind_index: HashMap<String, usize>,
    pub placements: Vec<PoiPlacement>,
    pub groups: Vec<PoiGroup>,
    pub files: Vec<String>,
    /// Actor class per actor index.
    pub classes: Vec<String>,
    pub actor_keys: Vec<(Arc<str>, String, i64)>,
    /// The actors with collision, as props of the sim world (`proto` = `POI_PROTO | actor`).
    pub harvestables: Vec<Harvestable>,
    pub pieces: HashMap<u32, (fn_building::pieces::PieceKind, f32)>,
    /// Every actor's UE xy by 10 cm cell, with its class ([`occupies`]).
    occupied: HashMap<(i32, i32), Vec<(Arc<str>, [f32; 2])>>,
    /// The building worlds' ground cover (`ground_cover.instances` of the files): UE world `t`, `q`, uniform scale, `PerInstanceRandom` (foliage.rs adds them
    /// to the grass).
    pub ground_cover: Vec<GroundCover>,
    pub notes: Vec<String>,
    /// WORLD: every level's HLOD clusters.
    pub clusters: Vec<HlodCluster>,
    /// WORLD: the actors the landscape files leave to the props layout (`layout_actors`): UE xy by 10 cm cell -> (class, xy, draw distance cm, cluster).
    layout_rows: HashMap<(i32, i32), Vec<(Arc<str>, [f32; 2], f32, u32)>>,
    /// UE world cm -> FX space (the files' placement offset).
    pub off: Mat4,
    pub hidden: Vec<PoiHidden>,
    pub parents: HashMap<u32, Vec<String>>,
}
#[derive(Clone, Copy, Debug)]
pub struct GroundCover { pub t: [f32; 3], pub q: [f32; 4], pub scale: f32, pub random: f32 }

pub fn current() -> Option<Arc<PoiSet>> { POI.lock().ok()?.clone() }
/// The POI actors to add to the sim's props (empty without a POI).
pub fn harvestables() -> Vec<Harvestable> { if fn_live::var("FN_POI_COLLIDE").is_ok_and(|v| v == "0") { return vec![]; } /* dev: FN_POI_COLLIDE=0 = drawn only (the perf split) */ current().map(|s| s.harvestables.clone()).unwrap_or_default() }
pub fn building_piece(proto: u32) -> Option<(fn_building::pieces::PieceKind, f32)> { if proto & fn_combat::POI_PROTO == 0 { return None; } current()?.pieces.get(&(proto & !fn_combat::POI_PROTO)).copied() }
/// The cooked mesh + overrides of a `poi:` kind id (`statics_render`).
pub fn kind_source(id: &str) -> Option<(String, Vec<Option<String>>)> { let s = current()?; let k = &s.kinds[*s.kind_index.get(id)?]; Some((k.mesh.clone(), k.overrides.clone())) }
/// WORLD: the TextureData MID of slot 0 of a `poi:` kind, if its actor has TextureData.
pub fn kind_mid0(id: &str) -> Option<Arc<(Option<String>, Vec<(String, String)>)>> { let s = current()?; s.kinds[*s.kind_index.get(id)?].mid0.clone() }
/// WORLD: the painted vertex colours of a `poi:` kind (per LOD), if its component has any.
pub fn kind_vcol(id: &str) -> Option<Arc<Vec<Option<Vec<[u8; 4]>>>>> { let s = current()?; s.kinds[*s.kind_index.get(id)?].vcol.clone() }
/// The hex FColor bytes of a `vcol` entry.
fn unhex_colors(h: &str) -> Option<Vec<[u8; 4]>> {
    let b = h.as_bytes();
    if b.len() % 8 != 0 { return None; }
    let nib = |c: u8| -> Option<u8> { (c as char).to_digit(16).map(|d| d as u8) };
    (0..b.len() / 8).map(|i| { let mut o = [0u8; 4]; for (k, x) in o.iter_mut().enumerate() { *x = nib(b[8 * i + 2 * k])? << 4 | nib(b[8 * i + 2 * k + 1])?; } Some(o) }).collect()
}
/// FNV-1a of the vcol strings: the kind id suffix of a painted component.
fn vcol_hash(v: &[Value]) -> u64 { let mut h: u64 = 0xcbf29ce484222325; for x in v { for &c in x.as_str().unwrap_or("-").as_bytes() { h ^= c as u64; h = h.wrapping_mul(0x100000001b3); } h ^= 0xff; h = h.wrapping_mul(0x100000001b3); } h }

fn occ_key(x: f32, y: f32) -> (i32, i32) { ((x / 10.).floor() as i32, (y / 10.).floor() as i32) }
impl PoiSet {
    pub fn is_a(&self, actor: u32, class: &str) -> bool {
        self.classes.get(actor as usize).is_some_and(|c| c == class) || self.parents.get(&actor).is_some_and(|p| p.iter().any(|c| c == class))
    }
    /// Is there an actor of `class` within [`DEDUP_CM`] (xy) of UE `abs_cm`?
    pub fn occupies(&self, class: &str, abs_cm: [f32; 3]) -> bool {
        let (kx, ky) = occ_key(abs_cm[0], abs_cm[1]);
        (-1..=1).any(|dx| (-1..=1).any(|dy| self.occupied.get(&(kx + dx, ky + dy)).is_some_and(|v| v.iter().any(|(c, p)| &**c == class && (p[0] - abs_cm[0]).hypot(p[1] - abs_cm[1]) <= DEDUP_CM))))
    }
}
impl PoiSet {
    /// WORLD: the landscape file row of a layout placement at FX-space `fx` (cm) of `class`: (draw distance cm, 0 = none; HLOD cluster).
    pub fn layout_row(&self, class: &str, fx: Vec3) -> Option<(f32, u32)> {
        let a = self.off.inverse().transform_point3(fx);
        let (kx, ky) = occ_key(a.x, a.y);
        let mut best: Option<(f32, (f32, u32))> = None;
        for dx in -1..=1 { for dy in -1..=1 {
            for (c, p, cull, cl) in self.layout_rows.get(&(kx + dx, ky + dy)).map(|v| v.as_slice()).unwrap_or(&[]) {
                let d = (p[0] - a.x).hypot(p[1] - a.y);
                if &**c == class && d <= DEDUP_CM && best.map_or(true, |b| d < b.0) { best = Some((d, (*cull, *cl))); }
            }
        } }
        best.map(|b| b.1)
    }
}
/// [`PoiSet::occupies`] of the loaded set (false without one): the props layout leaves such a placement to this module.
pub fn occupies(class: &str, abs_cm: [f32; 3]) -> bool { current().is_some_and(|s| s.occupies(class, abs_cm)) }

fn f3(v: &Value) -> Option<[f32; 3]> { let a = v.as_array()?; Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32]) }
fn q4(v: &Value) -> Quat { v.as_array().map(|a| Quat::from_xyzw(a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32, a[2].as_f64().unwrap_or(0.) as f32, a[3].as_f64().unwrap_or(1.) as f32)).unwrap_or(Quat::IDENTITY) }

/// The POI files to load: `index.json`'s list (the `--all` export), else every `*.json` of the directory (sorted).
fn file_list(props_dir: &Path) -> Vec<PathBuf> {
    let dir = props_dir.join("poi");
    if let Ok(b) = std::fs::read(dir.join("index.json")) {
        match serde_json::from_slice::<Value>(&b) {
            Ok(ix) => return ix["levels"].as_array().map(|a| a.iter().filter_map(|l| l["file"].as_str()).map(|f| dir.join(f)).collect()).unwrap_or_default(),
            Err(e) => eprintln!("[poi] {}: {e}", dir.join("index.json").display()),
        }
    }
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut files: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    files
}

/// The collision of one mesh as point clouds in mesh-local UE cm, one per convex piece (`fn_propdata` collision schema; complex-as-simple triangles as
/// 4 cm thick slabs, CHOSEN thickness: PhysX sweeps the zero-thickness triangle, the hull needs a volume).
pub(crate) fn mesh_pieces(c: &Value) -> Vec<Vec<Vec3>> {
    let mut out = vec![];
    if let Ok(col) = fn_propdata::Collision::from_json(c) {
        use fn_propdata::Shape;
        let quat = |q: [f32; 4]| Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize();
        let sphere = |c: Vec3, r: f32| { let mut v = vec![c + Vec3::Z * r, c - Vec3::Z * r]; for lat in 0..4 { let a = std::f32::consts::FRAC_PI_2 * (lat as f32 / 4.); for lon in 0..8 { let b = std::f32::consts::TAU * lon as f32 / 8.; for s in [-1., 1.] { v.push(c + Vec3::new(r * a.cos() * b.cos(), r * a.cos() * b.sin(), s * r * a.sin())); } } } v };
        for s in &col.shapes {
            out.push(match s {
                Shape::Convex { vertices, transform_translation, transform_rotation, transform_scale, .. } => { let (r, sc, t) = (quat(*transform_rotation), Vec3::from(*transform_scale), Vec3::from(*transform_translation)); vertices.iter().map(|v| r * (Vec3::from(*v) * sc) + t).collect() }
                Shape::Box { center, rotation, half_extent } => { let (r, c, h) = (quat(*rotation), Vec3::from(*center), Vec3::from(*half_extent)); let mut v = vec![]; for x in [-1., 1.] { for y in [-1., 1.] { for z in [-1., 1.] { v.push(c + r * (h * Vec3::new(x, y, z))); } } } v }
                Shape::Sphere { center, radius } => sphere(Vec3::from(*center), *radius),
                Shape::Capsule { center, rotation, radius, half_length } => { let (r, c) = (quat(*rotation), Vec3::from(*center)); let ax = r * Vec3::Z * *half_length; let mut v = sphere(c + ax, *radius); v.extend(sphere(c - ax, *radius)); v }
            });
        }
    }
    if let Some(cx) = c.get("complex") {
        let pos: Vec<Vec3> = cx["positions"].as_array().map(|a| a.iter().filter_map(f3).map(Vec3::from).collect()).unwrap_or_default();
        let idx: Vec<usize> = cx["indices"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as usize).collect()).unwrap_or_default();
        for t in idx.chunks_exact(3) {
            let (Some(&a), Some(&b), Some(&d)) = (pos.get(t[0]), pos.get(t[1]), pos.get(t[2])) else { continue };
            let n = (b - a).cross(d - a).normalize_or_zero() * 2.;
            if n == Vec3::ZERO { continue; }
            out.push(vec![a + n, b + n, d + n, a - n, b - n, d - n]);
        }
    }
    out
}

/// One file read and turned into kinds / placements / actors (file-local indices; [`load`] merges them).
struct FileOut {
    name: String,
    kinds: Vec<PoiKind>,
    placements: Vec<PoiPlacement>,
    classes: Vec<String>,
    actor_keys: Vec<(Arc<str>, String, i64)>,
    /// `proto` = the file-local actor index (rebased on merge).
    harvestables: Vec<Harvestable>,
    pieces: Vec<(u32, fn_building::pieces::PieceKind, f32)>,
    actor_xy: Vec<[f32; 2]>,
    ground_cover: Vec<GroundCover>,
    /// WORLD: the file's HLOD clusters (kind = file-local kind index) and layout rows (UE xy, class, cull, file-local cluster).
    clusters: Vec<HlodCluster>,
    layout_rows: Vec<(String, [f32; 2], f32, u32)>,
    /// DOORS: the file's door walls (`actor` file-local, `group` unset).
    doors: Vec<PoiDoor>,
    /// CONTAINERS: the file's loot containers (`actor` file-local, `group` unset).
    containers: Vec<PoiContainer>,
    floor_loot: Vec<PoiFloorLoot>,
    lights: Vec<PoiLight>,
    hidden: Vec<PoiHidden>,
    parents: Vec<(u32, Vec<String>)>,
    note: String,
    hulls: usize,
    damageable: usize,
    invulnerable: usize,
}

fn read_file(path: &Path, off: Mat4) -> Result<FileOut, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    drop(bytes);
    crate::water_hook::ingest_poi(&doc);
    let to_game = |u: Vec3| Vec3::new(u.x, u.z, u.y) * 0.01; // FX space (UE cm about the game origin) -> game metres (inverse of fn_fx::view::to_ue)
    let name = path.file_name().and_then(|x| x.to_str()).unwrap_or("").to_owned();
    let actors = doc["actors"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    let meshes = doc["meshes"].as_object();
    let mut out = FileOut { name: name.clone(), kinds: vec![], placements: vec![], classes: actors.iter().map(|a| a["class"].as_str().unwrap_or("").to_owned()).collect(), actor_keys: { let mut lv: HashMap<String, Arc<str>> = HashMap::new(); actors.iter().map(|a| { let l = a["level"].as_str().unwrap_or(""); let l = lv.entry(l.to_owned()).or_insert_with(|| Arc::from(l)).clone(); (l, a["name"].as_str().unwrap_or("").to_owned(), a["group"].as_i64().unwrap_or(-1)) }).collect() }, harvestables: vec![], pieces: vec![],
        actor_xy: actors.iter().map(|a| f3(&a["t"]).map_or([f32::NAN; 2], |t| [t[0], t[1]])).collect(), ground_cover: vec![], clusters: vec![], layout_rows: vec![], doors: vec![], containers: vec![], floor_loot: vec![], lights: vec![], hidden: vec![], parents: actors.iter().enumerate().filter_map(|(i, a)| a["parents"].as_array().map(|p| (i as u32, p.iter().filter_map(|c| c.as_str().map(String::from)).collect()))).collect(), note: String::new(), hulls: 0, damageable: 0, invulnerable: 0 };
    let mut by_id: HashMap<String, usize> = HashMap::new();
    // WORLD (HLOD): the clusters (proxy kinds first) and which actor each one replaces
    let mut cluster_of: HashMap<usize, u32> = HashMap::new();
    let off_env = |k: &str| fn_live::var(k).is_ok_and(|v| v == "0"); // dev A/B: FN_HLOD=0 / FN_POI_VCOL=0 / FN_POI_MID=0
    let (no_hlod, no_vcol, no_mid) = (off_env("FN_HLOD"), off_env("FN_POI_VCOL"), off_env("FN_POI_MID"));
    for h in doc["hlod"].as_array().map(|a| a.as_slice()).unwrap_or(&[]).iter().filter(|_| !no_hlod) {
        let (Some(mesh), Some(t), Some(sc), Some(lo), Some(hi)) = (h["mesh"].as_str(), f3(&h["t"]), f3(&h["s"]), f3(&h["bounds"]["min"]), f3(&h["bounds"]["max"])) else { continue };
        let id = format!("poi:{mesh}|");
        let kind = match by_id.get(&id) { Some(&k) => k, None => { out.kinds.push(PoiKind { id: id.clone(), mesh: mesh.to_owned(), overrides: vec![], vcol: None, mid0: None }); by_id.insert(id, out.kinds.len() - 1); out.kinds.len() - 1 } };
        let ci = out.clusters.len() as u32;
        for a in h["actors"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) { if let Some(a) = a.as_u64() { cluster_of.insert(a as usize, ci); } }
        // the box corners through the same offset (a translation: the box stays axis aligned)
        let (lo, hi) = (off.transform_point3(Vec3::from(lo)), off.transform_point3(Vec3::from(hi)));
        out.clusters.push(HlodCluster { name: h["name"].as_str().unwrap_or("").to_owned(), kind, l2w: off * Mat4::from_scale_rotation_translation(Vec3::from(sc), q4(&h["q"]), Vec3::from(t)),
            min_cm: h["min_draw_cm"].as_f64().unwrap_or(0.) as f32, lo: lo.min(hi), hi: lo.max(hi), cast_shadow: h["cast_shadow"].as_bool().unwrap_or(true) });
    }
    for r in doc.pointer("/layout_actors/rows").and_then(|v| v.as_array()).map(|a| a.as_slice()).unwrap_or(&[]) {
        let Some(r) = r.as_array().filter(|r| r.len() >= 6) else { continue };
        let cl = r[5].as_i64().filter(|c| *c >= 0).map_or(NO_CLUSTER, |c| c as u32);
        out.layout_rows.push((r[0].as_str().unwrap_or("").to_owned(), [r[1].as_f64().unwrap_or(0.) as f32, r[2].as_f64().unwrap_or(0.) as f32], r[4].as_f64().unwrap_or(0.) as f32, cl));
    }
    // COLLISION: each mesh's convex pieces built ONCE in mesh-local cm, then mapped per placement (a hull of affinely mapped points = the mapped hull)
    let mut hulls_of: HashMap<String, Arc<Vec<Hull>>> = HashMap::new();
    let mut hulls_flipped: HashMap<String, Arc<Vec<Hull>>> = HashMap::new();
    let mut actor_hulls: HashMap<u32, Vec<Hull>> = HashMap::new();
    let (mut n, mut grass, mut invisible) = (0, 0, 0usize);
    for p in doc["placements"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let ai = p["actor"].as_u64().unwrap_or(0) as usize;
        let a = actors.get(ai).unwrap_or(&Value::Null);
        let class = a["class"].as_str().unwrap_or("");
        if class.contains("FloorLoot") { continue; } // the loot spawner's editor marker, not drawn in game
        // the foliage of a level `props foliage` already exports (the ground cover the grass path draws; files before ALL POIS still list it)
        if p["instanced"].as_bool() == Some(true) && a["top"].as_bool() == Some(true) { grass += 1; continue; }
        let (Some(t), Some(s)) = (f3(&p["t"]), f3(&p["s"])) else { continue };
        let mesh = p["mesh"].as_str().unwrap_or("");
        let overrides: Vec<Option<String>> = p["overrides"].as_array().map(|a| a.iter().map(|v| v.as_str().filter(|s| !s.is_empty()).map(String::from)).collect()).unwrap_or_default();
        if p["collide"].as_bool() != Some(true) && invisible_mesh(mesh, &overrides) { invisible += 1; continue; }
        let mut id = format!("poi:{}|{}", mesh, overrides.iter().map(|o| o.as_deref().unwrap_or("")).collect::<Vec<_>>().join(","));
        // WORLD: a component with painted vertex colours is its own kind (same mesh, its colour stream)
        let vraw = p["vcol"].as_array().filter(|v| !v.is_empty() && !no_vcol);
        if let Some(v) = vraw { id.push_str(&format!("#{:016x}", vcol_hash(v))); }
        // WORLD: the TextureData MID of slot 0 (base + sorted texture parameters) is part of the kind
        let mid0 = p.get("mid0").filter(|m| m.is_object() && !no_mid).map(|m| {
            let mut t: Vec<(String, String)> = m["textures"].as_object().map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))).collect()).unwrap_or_default();
            t.sort();
            Arc::new((m["base"].as_str().map(String::from), t))
        });
        if let Some(m) = &mid0 { id.push_str(&format!("~{}?{}", m.0.as_deref().unwrap_or(""), m.1.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&"))); }
        let kind = match by_id.get(&id) { Some(&k) => k, None => {
            let vcol = vraw.map(|v| Arc::new(v.iter().map(|x| x.as_str().and_then(unhex_colors)).collect::<Vec<_>>()));
            out.kinds.push(PoiKind { id: id.clone(), mesh: mesh.to_owned(), overrides, vcol, mid0 }); by_id.insert(id, out.kinds.len() - 1); out.kinds.len() - 1 } };
        let l2w = off * Mat4::from_scale_rotation_translation(Vec3::from(s), q4(&p["q"]), Vec3::from(t));
        out.placements.push(PoiPlacement { kind, class: class.to_owned(), l2w, actor: ai as u32, cull_cm: p["cull_cm"].as_f64().unwrap_or(0.) as f32, cast_shadow: p["cast_shadow"].as_bool().unwrap_or(true), cluster: cluster_of.get(&ai).copied().unwrap_or(NO_CLUSTER) });
        n += 1;
        if p["collide"].as_bool() == Some(true) {
            let local = hulls_of.entry(mesh.to_owned()).or_insert_with(|| Arc::new(meshes.and_then(|m| m.get(mesh)).map(mesh_pieces).unwrap_or_default().iter().filter_map(|pc| Hull::from_points(pc)).collect())).clone();
            // UE cm (left handed) -> game metres swaps y / z: a reflection, so the winding flips unless the placement mirrors too
            let flip = Mat3::from_mat4(l2w).determinant() > 0.;
            let src = if flip { hulls_flipped.entry(mesh.to_owned()).or_insert_with(|| Arc::new(local.iter().map(Hull::reflected_topology).collect())).clone() } else { local };
            let e = actor_hulls.entry(ai as u32).or_default();
            for h in src.iter() { e.push(h.mapped_shared(|v| to_game(l2w.transform_point3(v)))); }
        }
    }
    for p in doc["hidden_placements"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let (Some(t), Some(s), Some(mesh)) = (f3(&p["t"]), f3(&p["s"]), p["mesh"].as_str()) else { continue };
        out.hidden.push(PoiHidden { actor: p["actor"].as_u64().unwrap_or(0) as u32, comp: p["comp"].as_str().unwrap_or("").to_owned(), mesh: mesh.to_owned(),
            overrides: p["overrides"].as_array().map(|a| a.iter().map(|v| v.as_str().filter(|s| !s.is_empty()).map(String::from)).collect()).unwrap_or_default(),
            l2w: off * Mat4::from_scale_rotation_translation(Vec3::from(s), q4(&p["q"]), Vec3::from(t)), cull_cm: p["cull_cm"].as_f64().unwrap_or(0.) as f32, cast_shadow: p["cast_shadow"].as_bool().unwrap_or(true) });
    }
    // the actors as props: collision + building data
    let mut ids: Vec<u32> = actor_hulls.keys().copied().collect();
    ids.sort();
    for actor in ids {
        let hulls = actor_hulls.remove(&actor).unwrap_or_default();
        if hulls.is_empty() { continue; }
        out.hulls += hulls.len();
        let a = &actors[actor as usize];
        let class = a["class"].as_str().unwrap_or("");
        let native = a["native"].as_str().unwrap_or("");
        let hp = a["health_t0"].as_f64().unwrap_or(0.) as f32;
        let dmg = hp > 0. && a["invulnerable"].as_bool() != Some(true) && !class.starts_with("Tiered_") && native != "FortStaticMeshActor";
        let (res, no_yield) = match a["resource"].as_str().unwrap_or("") { "Wood" => (Resource::Wood, false), "Stone" => (Resource::Stone, false), "Metal" => (Resource::Metal, false), _ => (Resource::Wood, true) };
        let kind = if res == Resource::Stone { Kind::Rock } else { Kind::Tree };
        let (at, aq, as_) = (f3(&a["t"]).map(Vec3::from).unwrap_or(Vec3::ZERO), q4(&a["q"]), f3(&a["s"]).map(Vec3::from).unwrap_or(Vec3::ONE));
        let pos = to_game(off.transform_point3(at));
        let mut h = Harvestable::real(kind, res, pos, 0., 1., if dmg { hp } else { 1. }, actor, Some(Arc::new(HullSet::new(hulls))));
        h.invulnerable = !dmg;
        h.no_yield = no_yield;
        // GetCentroid: the actor transform applied to CentroidOffset; the +0x564 placement-sweep bits (see fn_combat::Harvestable::build_flags)
        let (native_flags, native_co) = native_build_defaults(native);
        let co = f3(&a["centroid_offset"]).map(Vec3::from).unwrap_or(Vec3::from(native_co));
        h.centroid = to_game(off.transform_point3(at + aq * (co * as_)));
        let mut flags = native_flags;
        for (key, bit) in [("do_not_block_buildings", 1u8), ("destroy_on_player_building_placement", 2), ("use_centroid_for_block_buildings_check", 4)] {
            if let Some(b) = a[key].as_bool() { if b { flags |= bit } else { flags &= !bit } }
        }
        h.build_flags = flags;
        if dmg { out.damageable += 1 } else { out.invulnerable += 1 }
        if let Some(k) = match native { "BuildingWall" => Some(fn_building::pieces::PieceKind::Wall), "BuildingFloor" => Some(fn_building::pieces::PieceKind::Floor), "BuildingStairs" => Some(fn_building::pieces::PieceKind::Stairs), "BuildingRoof" => Some(fn_building::pieces::PieceKind::Roof), _ => None } {
            let fwd = aq * Vec3::X;
            out.pieces.push((actor, k, fwd.y.atan2(fwd.x).to_degrees()));
        }
        out.harvestables.push(h);
    }
    // DOORS: the door walls (their door mesh's collision is in `meshes{}`, read once per mesh)
    let mut door_pieces: HashMap<String, Arc<Vec<Vec<Vec3>>>> = HashMap::new();
    for d in doc["doors"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let (Some(def), Some(t), Some(s)) = (fn_building::door::DoorDef::from_props(&serde_json::json!({"DoorMesh": d["mesh"], "DoorOffset": d["offset"], "DoorAnimatingMaterial": d["material"],
            "DoorOpeningSound": d["opening_sound"], "DoorClosingSound": d["closing_sound"]})), f3(&d["t"]), f3(&d["s"])) else { continue };
        let pieces = door_pieces.entry(def.mesh.clone()).or_insert_with(|| Arc::new(meshes.and_then(|m| m.get(&def.mesh)).map(mesh_pieces).unwrap_or_default())).clone();
        out.doors.push(PoiDoor { actor: d["actor"].as_u64().unwrap_or(0) as u32, group: NO_CLUSTER, def, t: off.transform_point3(Vec3::from(t)), q: q4(&d["q"]).normalize(), s: Vec3::from(s), mirrored: d["mirrored"].as_bool().unwrap_or(false), pieces });
    }
    // CONTAINERS: the loot containers (`loot[]` minus the FloorLoot spawners) with their particle / light components and the root mesh's draw distance
    for l in doc["loot"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let class = l["class"].as_str().unwrap_or("");
        if class.contains("FloorLoot") {
            if let Some(t) = f3(&l["t"]) { out.floor_loot.push(PoiFloorLoot { class: class.to_owned(), tier_group: l["tier_group"].as_str().unwrap_or("").to_owned(), t: off.transform_point3(Vec3::from(t)), q: q4(&l["q"]).normalize(), ue: t, group: NO_CLUSTER }); }
            continue;
        }
        if !class.starts_with("Tiered_") { continue; }
        let ai = l["actor"].as_u64().unwrap_or(u64::MAX);
        let a = actors.get(ai as usize).unwrap_or(&Value::Null);
        let (Some(t), q, s) = (f3(&l["t"]), q4(&l["q"]).normalize(), f3(&a["s"]).map(Vec3::from).unwrap_or(Vec3::ONE)) else { continue };
        let cull = doc["placements"].as_array().and_then(|ps| ps.iter().find(|p| p["actor"].as_u64() == Some(ai))).and_then(|p| p["cull_cm"].as_f64()).unwrap_or(0.) as f32;
        let particles = doc["particles"].as_array().map(|ps| ps.iter().filter(|p| p["actor"].as_u64() == Some(ai)).filter_map(|p| {
            let (pt, ps_) = (f3(&p["t"])?, f3(&p["s"]).unwrap_or([1.; 3]));
            Some((p["component"].as_str().unwrap_or("").to_owned(), p["template"].as_str()?.to_owned(), off * Mat4::from_scale_rotation_translation(Vec3::from(ps_), q4(&p["q"]), Vec3::from(pt)), p["auto_activate"].as_bool().unwrap_or(true)))
        }).collect()).unwrap_or_default();
        let lights = doc["lights"].as_array().map(|ls| ls.iter().filter(|p| p["actor"].as_u64() == Some(ai)).filter_map(|p| Some((p["component"].as_str().unwrap_or("").to_owned(), p["props"].clone(), off.transform_point3(Vec3::from(f3(&p["t"])?))))).collect()).unwrap_or_default();
        out.containers.push(PoiContainer { actor: ai as u32, class: class.to_owned(), tier_group: l["tier_group"].as_str().unwrap_or("").to_owned(), t: off.transform_point3(Vec3::from(t)), q, s, ue: t, cull_cm: cull, particles, lights, group: NO_CLUSTER, searched_actor: NO_CLUSTER });
    }
    for p in doc["lights"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let Some(t) = f3(&p["t"]) else { continue };
        let ai = p["actor"].as_u64().unwrap_or(u64::MAX);
        let a = actors.get(ai as usize).unwrap_or(&Value::Null);
        out.lights.push(PoiLight { actor: ai as u32, actor_class: p["actor_class"].as_str().unwrap_or("").to_owned(), component: p["component"].as_str().unwrap_or("").to_owned(),
            kind: p["class"].as_str().unwrap_or("").to_owned(), hidden: p["hidden"].as_bool().unwrap_or(false), t: off.transform_point3(Vec3::from(t)), q: q4(&p["q"]).normalize(),
            actor_ue: f3(&a["t"]).unwrap_or(t), props: p["props"].clone() });
    }
    // ALL POIS: the building worlds' ground cover, for the grass path
    if let Some(rows) = doc.pointer("/ground_cover/instances").and_then(|v| v.as_array()) {
        for r in rows {
            let Some(v) = r.as_array().filter(|v| v.len() >= 9).map(|v| v.iter().map(|x| x.as_f64().unwrap_or(0.) as f32).collect::<Vec<f32>>()) else { continue };
            out.ground_cover.push(GroundCover { t: [v[0], v[1], v[2]], q: [v[3], v[4], v[5], v[6]], scale: v[7], random: v[8] });
        }
    }
    out.note = format!("{name}: {n} placements of {} actors, {} ground-cover instances{}", actors.len(), out.ground_cover.len(), if grass > 0 { format!(" ({grass} map ground-cover instances left to the grass path)") } else { String::new() });
    if invisible > 0 { out.note.push_str(&format!(", {invisible} placements of meshes that render nothing skipped")); }
    Ok(out)
}

const INVISIBLE_MATERIAL: &str = "/Game/Environments/Props/Rubble/Materials/M_Tire_JumpCollision_Clear.M_Tire_JumpCollision_Clear";
fn invisible_mesh(mesh: &str, overrides: &[Option<String>]) -> bool {
    mesh.starts_with("/Game/Environments/Props/Rubble/Meshes/TirePiles_0") && mesh.ends_with("_JumpCollision")
        && overrides.iter().all(|o| o.as_deref().is_none_or(|m| m == INVISIBLE_MATERIAL))
}

/// Reads every POI file ([`file_list`]) and keeps the result for `statics_render` / the props / the grass. `center` = the layout centre (UE cm, mapped to
/// `props::ANCHOR`), `base_cm` = the UE z of the game's y = 0 when no landscape is installed.
pub fn native_build_defaults(native: &str) -> (u8, [f32; 3]) {
    use fn_building::placement::{PROP_NOT_IN_SWEEP, PROP_STATIC_MESH};
    match native {
        "BuildingWall" | "BuildingCorner" | "BuildingPropWall" => (4, [0., 0., 192.]),
        "BuildingPropCorner" => (5, [0., 0., 192.]),
        "BuildingFloor" | "BuildingRoof" => (4, [0., 256., 0.]),
        "BuildingStairs" | "BuildingPillar" => (4, [0., 256., 192.]),
        "BuildingProp" | "BuildingContainer" | "KeepItemContainer" => (0, [0.; 3]),
        "BuildingGameplayActor" => (5, [0.; 3]),
        "FortStaticMeshActor" | "StaticMeshActor" => (PROP_STATIC_MESH, [0.; 3]),
        n if n.starts_with("Building") => (4, [0.; 3]),
        _ => (PROP_NOT_IN_SWEEP, [0.; 3]),
    }
}

pub fn load(props_dir: &Path, center: [f32; 3], base_cm: f32) {
    if !enabled() { eprintln!("[poi] FN_POI=0: no POI"); return; }
    let t0 = std::time::Instant::now();
    // TERRAIN: with the landscape installed the ground is real, so the towns are placed in the landscape's mapping (`ref_z_cm` = the game's y = 0)
    let base_cm = fn_terrain::world().map_or(base_cm, |w| w.map.ref_z_cm);
    let anchor_ue = fn_fx::view::to_ue(crate::props::ANCHOR).as_vec3();
    let off = Mat4::from_translation(anchor_ue + Vec3::new(-center[0], -center[1], -base_cm));
    let paths = file_list(props_dir);
    if paths.is_empty() { eprintln!("[poi] no POI export in {} (fn-export props poi --all)", props_dir.join("poi").display()); return; }
    // the files are independent: read + hull them on the loader's workers (the largest first)
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(std::fs::metadata(&paths[i]).map_or(0, |m| m.len())));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<FileOut, String>>>> = Mutex::new((0..paths.len()).map(|_| None).collect());
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 8);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(&i) = order.get(k) else { break };
                let r = read_file(&paths[i], off);
                if let Ok(mut g) = results.lock() { g[i] = Some(r); }
            });
        }
    });
    let mut set = PoiSet { off, ..Default::default() };
    let (mut hulls_total, mut damageable, mut invulnerable, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for r in results.into_inner().unwrap_or_default().into_iter().flatten() {
        let f = match r { Ok(f) => f, Err(e) => { eprintln!("[poi] {e}"); failed += 1; continue } };
        let fi = set.files.len();
        set.files.push(f.name.clone());
        let base_actor = set.classes.len() as u32;
        // the file's kinds into the shared table
        let kmap: Vec<usize> = f.kinds.into_iter().map(|k| match set.kind_index.get(&k.id) { Some(&i) => i, None => { set.kind_index.insert(k.id.clone(), set.kinds.len()); set.kinds.push(k); set.kinds.len() - 1 } }).collect();
        for (ai, (c, xy)) in f.classes.iter().zip(&f.actor_xy).enumerate() {
            let _ = ai;
            if xy[0].is_finite() { set.occupied.entry(occ_key(xy[0], xy[1])).or_default().push((Arc::from(c.as_str()), *xy)); }
        }
        set.classes.extend(f.classes);
        set.actor_keys.extend(f.actor_keys);
        for mut h in f.hidden { h.actor += base_actor; set.hidden.push(h); }
        for (a, p) in f.parents { set.parents.insert(base_actor + a, p); }
        // WORLD (HLOD): the file's clusters and layout rows, rebased
        let base_cluster = set.clusters.len() as u32;
        for mut c in f.clusters { c.kind = kmap[c.kind]; set.clusters.push(c); }
        for (c, xy, cull, cl) in f.layout_rows { set.layout_rows.entry(occ_key(xy[0], xy[1])).or_default().push((Arc::from(c.as_str()), xy, cull, if cl == NO_CLUSTER { NO_CLUSTER } else { base_cluster + cl })); }
        // STREAMING: the file's placements by CELL_CM cell of their UE xy (FX space = UE about the game origin: the cell is taken there)
        let p0 = set.placements.len();
        let mut cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for mut p in f.placements {
            p.kind = kmap[p.kind];
            p.actor += base_actor;
            if p.cluster != NO_CLUSTER { p.cluster += base_cluster; }
            let w = p.l2w.w_axis.truncate();
            cells.entry(((w.x / CELL_CM).floor() as i32, (w.y / CELL_CM).floor() as i32)).or_default().push(set.placements.len() as u32);
            set.placements.push(p);
        }
        let mut keys: Vec<(i32, i32)> = cells.keys().copied().collect();
        keys.sort();
        let mut group_of_cell: HashMap<(i32, i32), u32> = HashMap::new();
        for k in keys {
            group_of_cell.insert(k, set.groups.len() as u32);
            let pl = cells.remove(&k).unwrap_or_default();
            let pts: Vec<Vec3> = pl.iter().map(|&i| set.placements[i as usize].l2w.w_axis.truncate()).collect();
            let (lo, hi) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
            let c = (lo + hi) * 0.5;
            let r = pts.iter().map(|p| (*p - c).length()).fold(0., f32::max) + MESH_MARGIN_CM;
            let mut kinds: Vec<u32> = pl.iter().map(|&i| set.placements[i as usize].kind as u32).collect();
            kinds.sort(); kinds.dedup();
            let mut clusters: Vec<u32> = pl.iter().map(|&i| set.placements[i as usize].cluster).filter(|c| *c != NO_CLUSTER).collect();
            clusters.sort(); clusters.dedup();
            // CHOSEN: a placement without a cluster and without a draw distance is kept within the old residency (stream_cm)
            let reach_cm = pl.iter().map(|&i| &set.placements[i as usize]).filter(|p| p.cluster == NO_CLUSTER).map(|p| if p.cull_cm > 0. { p.cull_cm } else { stream_cm() }).fold(0., f32::max);
            let cull_reach_cm = pl.iter().map(|&i| &set.placements[i as usize]).filter(|p| p.cluster == NO_CLUSTER && p.cull_cm > 0.).map(|p| p.cull_cm).fold(0., f32::max);
            let stream_reach_cm = if pl.iter().any(|&i| { let p = &set.placements[i as usize]; p.cluster == NO_CLUSTER && p.cull_cm <= 0. }) { stream_cm() } else { 0. };
            set.groups.push(PoiGroup { file: fi, center: c, radius: r, placements: pl, kinds, ready: AtomicBool::new(false), resident: AtomicBool::new(false), clusters, reach_cm, cull_reach_cm, stream_reach_cm });
        }
        let _ = p0;
        // DOORS: the file's doors, with the streaming group their wall's placements are in
        if !f.doors.is_empty() {
            let mut group_of: HashMap<u32, u32> = HashMap::new();
            for (gi, g) in set.groups.iter().enumerate().filter(|(_, g)| g.file == fi) { for &pi in &g.placements { group_of.entry(set.placements[pi as usize].actor).or_insert(gi as u32); } }
            for mut d in f.doors { d.actor += base_actor; d.group = group_of.get(&d.actor).copied().unwrap_or(NO_CLUSTER); set.doors.push(d); }
        }
        for mut l in f.lights { l.actor += base_actor; set.lights.push(l); }
        // CONTAINERS: the file's containers, with the streaming group of their placements
        if !f.containers.is_empty() {
            let mut group_of: HashMap<u32, u32> = HashMap::new();
            for (gi, g) in set.groups.iter().enumerate().filter(|(_, g)| g.file == fi) { for &pi in &g.placements { group_of.entry(set.placements[pi as usize].actor).or_insert(gi as u32); } }
            for mut c in f.containers { c.actor += base_actor; c.group = group_of.get(&c.actor).copied().unwrap_or(NO_CLUSTER); set.containers.push(c); }
        }
        for mut fl in f.floor_loot {
            let k = ((fl.t.x / CELL_CM).floor() as i32, (fl.t.y / CELL_CM).floor() as i32);
            fl.group = group_of_cell.get(&k).copied().or_else(|| (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (k.0 + dx, k.1 + dy))).find_map(|c| group_of_cell.get(&c).copied())).unwrap_or(NO_CLUSTER);
            set.floor_loot.push(fl);
        }
        for mut h in f.harvestables { h.proto = fn_combat::POI_PROTO | (base_actor + h.proto); set.harvestables.push(h); }
        for (a, k, y) in f.pieces { set.pieces.insert(base_actor + a, (k, y)); }
        set.ground_cover.extend(f.ground_cover);
        hulls_total += f.hulls; damageable += f.damageable; invulnerable += f.invulnerable;
        set.notes.push(f.note);
    }
    eprintln!("[poi] DOORS: {} door walls ({} without a streaming group)", set.doors.len(), set.doors.iter().filter(|d| d.group == NO_CLUSTER).count());
    eprintln!("[poi] WORLD: {} HLOD clusters (proxy drawn beyond {:.0} m of its bounds), {} layout rows; {} painted-colour kinds", set.clusters.len(), hlod_distance_cm() / 100., set.layout_rows.values().map(|v| v.len()).sum::<usize>(), set.kinds.iter().filter(|k| k.vcol.is_some()).count());
    eprintln!("[poi] {} files ({failed} failed), {} placements of {} mesh kinds in {} streaming groups ({:.0} m cells, resident within {:.0} m), {} ground-cover instances; layout centre ({:.0}, {:.0}) cm, ground ref z {base_cm:.0} cm; {} colliding actors ({} damageable, {} invulnerable, {} convex pieces); {:.0} ms\n[poi] {}",
        set.files.len(), set.placements.len(), set.kinds.len(), set.groups.len(), CELL_CM / 100., stream_cm() / 100., set.ground_cover.len(), center[0], center[1], set.harvestables.len(), damageable, invulnerable, hulls_total, t0.elapsed().as_secs_f32() * 1000., set.notes.join("\n[poi] "));
    crate::bounce_hook::add_broken(&mut set);
    crate::chest_hook::add_searched(&mut set); // CONTAINERS: the searched-mesh pseudo actors (hidden until a search)
    if !set.placements.is_empty() { if let Ok(mut g) = POI.lock() { *g = Some(Arc::new(set)); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// PERF (dev, `--ignored`): where the sim's per-tick time goes with every POI actor in the props (the exported assets of the shared folder).
    #[test] #[ignore] fn sim_cost_with_all_poi_props() {
        let dir = PathBuf::from(r"C:\Users\Brady\Desktop\Fortnite Rust\FortniteRuntime\assets\props");
        if !dir.join("poi").join("index.json").exists() { return; }
        load(&dir, [51152., 71640., 0.], 768.);
        let mut g = fn_game::GameSim::new();
        g.world.props = harvestables();
        let n = g.world.props.len();
        let time = |name: &str, f: &mut dyn FnMut()| { let t = std::time::Instant::now(); for _ in 0..60 { f(); } eprintln!("{name}: {:.3} ms per call ({n} props)", t.elapsed().as_secs_f32() * 1000. / 60.); };
        time("refresh_blockers", &mut || g.refresh_blockers());
        time("update_camera", &mut || g.update_camera(1. / 60.));
        time("step", &mut || g.step(&fn_game::FrameInput::default(), 1. / 60.));
        let (eye, fwd) = (Vec3::new(0., 2., 12.), Vec3::new(0., 0., -1.));
        time("focused_prop", &mut || { std::hint::black_box(fn_combat::harvest_fx::focused_prop(&g.world.props, eye, fwd, eye)); });
        time("fn_props::trace", &mut || { std::hint::black_box(fn_props::trace(&g.world.props, eye, eye + fwd * 3., 0.2)); });
        time("reticle::aim_on_target", &mut || { std::hint::black_box(fn_combat::reticle::aim_on_target(&g.world.props, eye, fwd, eye)); });
        time("building_hud::target", &mut || { std::hint::black_box(crate::building_hud::target(&g, crate::building_hud::Tool::Pickaxe, eye, fwd, eye, |_| None).0.reticle_building); });
        time("world.update", &mut || g.world.update(1. / 60.));
        let mut det = fn_game::audio_events::MomentDetector::new(); let mut out = vec![];
        time("audio MomentDetector::observe", &mut || { out.clear(); det.observe(&g, None, 1. / 60., &mut out); });
    }
    #[test] fn painted_colours_decode_as_raw_fcolor_bytes_and_hash_by_content() {
        assert_eq!(unhex_colors("000000ff5f00ff10"), Some(vec![[0, 0, 0, 255], [0x5f, 0, 0xff, 0x10]]));
        assert_eq!(unhex_colors("000000f"), None, "not whole FColors");
        assert_eq!(unhex_colors("zz0000ff"), None);
        let (a, b) = (vec![Value::from("000000ff")], vec![Value::from("000000fe")]);
        assert_ne!(vcol_hash(&a), vcol_hash(&b));
        assert_eq!(vcol_hash(&a), vcol_hash(&[Value::from("000000ff")]));
    }
    #[test] fn an_hlod_proxy_shows_from_the_override_distance_to_its_bounds_box() {
        let c = HlodCluster { name: "c".into(), kind: 0, l2w: Mat4::IDENTITY, min_cm: 60000., lo: Vec3::new(0., 0., 0.), hi: Vec3::new(10000., 10000., 1000.), cast_shadow: true };
        let d = hlod_distance_cm();
        assert_eq!(d, HLOD_DISTANCE_CM, "r.HLOD.DistanceOverride replaces the cluster's 600 m");
        assert!(!c.proxy_visible(Vec3::new(5000., 5000., 500.)), "inside the box: the sub actors");
        assert!(!c.proxy_visible(Vec3::new(10000. + d - 1., 5000., 500.)));
        assert!(c.proxy_visible(Vec3::new(10000. + d + 1., 5000., 500.)), "box distance, not centre distance");
    }
    #[test] fn a_layout_placement_finds_its_landscape_row_by_class_and_position() {
        let mut s = PoiSet { off: Mat4::from_translation(Vec3::new(100., 200., 0.)), ..Default::default() };
        s.layout_rows.entry(occ_key(1000., 2000.)).or_default().push((Arc::from("Athena_Tree_Large_1"), [1000., 2000.], 47648., 3));
        assert_eq!(s.layout_row("Athena_Tree_Large_1", Vec3::new(1102., 2199., 50.)), Some((47648., 3)), "FX space = UE + off");
        assert_eq!(s.layout_row("Athena_Tree_Pine_01", Vec3::new(1100., 2200., 0.)), None);
        assert_eq!(s.layout_row("Athena_Tree_Large_1", Vec3::new(1120., 2200., 0.)), None, "20 cm away");
    }
    #[test] fn dedup_matches_class_and_position() {
        let mut s = PoiSet::default();
        s.occupied.entry(occ_key(1000., 2000.)).or_default().push((Arc::from("Athena_Tree_Large_1"), [1000., 2000.]));
        assert!(s.occupies("Athena_Tree_Large_1", [1003., 1998., 50.]));
        assert!(s.occupies("Athena_Tree_Large_1", [999.5, 2000.4, 0.]), "across a 10 cm cell border");
        assert!(!s.occupies("Athena_Tree_Pine_01", [1000., 2000., 0.]), "another class at the same spot");
        assert!(!s.occupies("Athena_Tree_Large_1", [1010., 2000., 0.]), "10 cm away");
    }
    #[test] fn poi_building_actors_keep_their_native_placement_defaults() {
        use fn_building::placement::{PROP_NOT_IN_SWEEP, PROP_STATIC_MESH};
        assert_eq!(native_build_defaults("BuildingWall"), (4, [0., 0., 192.]));
        assert_eq!(native_build_defaults("BuildingFloor"), (4, [0., 256., 0.]));
        assert_eq!(native_build_defaults("BuildingProp").0, 0);
        assert_eq!(native_build_defaults("FortStaticMeshActor").0, PROP_STATIC_MESH);
        assert_eq!(native_build_defaults("InstancedFoliageActor").0, PROP_NOT_IN_SWEEP);
    }
    #[test] fn a_mapped_local_hull_equals_the_hull_of_the_mapped_points() {
        let pts: Vec<Vec3> = [[0., 0., 0.], [100., 0., 0.], [0., 50., 0.], [0., 0., 30.], [100., 50., 30.]].iter().map(|v| Vec3::from(*v)).collect();
        let l2w = Mat4::from_scale_rotation_translation(Vec3::splat(1.5), Quat::from_rotation_z(0.7), Vec3::new(500., -200., 80.));
        let to_game = |u: Vec3| Vec3::new(u.x, u.z, u.y) * 0.01;
        let a = Hull::from_points(&pts).unwrap().mapped(|v| to_game(l2w.transform_point3(v)), true);
        let b = Hull::from_points(&pts.iter().map(|v| to_game(l2w.transform_point3(*v))).collect::<Vec<_>>()).unwrap();
        for p in [Vec3::new(5.5, 1., -1.5), Vec3::new(5.0, 0.9, -2.5), Vec3::new(6.5, 0.5, -1.)] { assert!((a.sdf(p) - b.sdf(p)).abs() < 1e-3, "{p}: {} vs {}", a.sdf(p), b.sdf(p)); }
        let local = Hull::from_points(&pts).unwrap().reflected_topology();
        let c = local.mapped_shared(|v| to_game(l2w.transform_point3(v)));
        assert!(Arc::ptr_eq(&c.tris, &local.tris) && Arc::ptr_eq(&c.edges, &local.edges));
        for p in [Vec3::new(5.5, 1., -1.5), Vec3::new(5.0, 0.9, -2.5), Vec3::new(6.5, 0.5, -1.), Vec3::new(4., 0., 0.)] { assert!((c.sdf(p) - b.sdf(p)).abs() < 1e-3, "{p}: {} vs {}", c.sdf(p), b.sdf(p)); }
    }
}
