//! `props poi --center X,Y --radius M [--name NAME]`: every actor of `Athena_Terrain` (persistent level + always-loaded sublevels) within `radius` metres of a
//! UE world point, plus the building sublevels the `BuildingFoundation`s there stream in (`AdditionalWorlds`, placed with the foundation actor's transform),
//! recursively. This is how a POI is put together in 1.8: Salty Springs (`Athena_POI_Suburban_006`: five foundations streaming `Athena_SUB_*_House_*`),
//! Lonely Lodge (`Athena_POI_RangerOutpost`, LevelTransform (54272, 73216, 1920) yaw 90, every piece placed directly + the `Athena_Terrain_LS_05` 1x1 shacks
//! around it); `props layout` only keeps the harvestable `*_C` actors of the scanned levels.
//!
//! Output `<out>/poi/<name>.json` (version 2):
//! * `actors[]`: one per placed actor: class, native class (the first `/Script` class of the Blueprint chain), level, root world transform, the building data
//!   of the class (`AttributeInitKey` -> `FortHealthSet.MaxHealth` row of `AttributesBuildingProps` / `AttributesBuildingSection` with its curve keys,
//!   `ResourceType`, `BuildingResourceAmountOverride`, `bIsInvulnerable`, `CurrentBuildingLevel`, `CentroidOffset`, `bNoCameraCollision`, ...); the row's curve keys
//!   are in `health_rows{}` (once per row).
//! * `placements[]`: EVERY visible static-mesh component of every actor (not only the root): actor index, component name, mesh, `OverrideMaterials`
//!   (template, then instance, per slot), world transform `t` (UE cm) / `q` (FQuat) / `s`, collision profile + whether it blocks the pawn, `CastShadow`,
//!   draw distance (`CachedMaxDrawDistance` / `LDMaxDrawDistance`). A component's properties = its archetype (the Blueprint chain's component template of the
//!   same name: SCS `_GEN_VARIABLE` or the CDO subobject, merged root class first) overlaid with the placed instance's delta. A BuildingSMActor's main
//!   component shows the actor's own `StaticMesh` (instance, then class default), as `ABuildingSMActor` construction does.
//!   `InstancedFoliageActor` instances (`FoliageInstancedStaticMeshComponent` PerInstanceSMData) are placements of a per-component pseudo actor.
//! * `meshes{}`: per mesh, the cooked `BodySetup` simple collision (`collision.rs` format) and, for `CTF_UseComplexAsSimple` bodies without simple shapes,
//!   the collision LOD's triangles (`complex`: positions cm + indices): the trimesh UE sweeps the pawn against in that case.
//! * `lights[]` (point / spot / rect light components: merged properties + world transform), `particles[]` (ParticleSystemComponent template + world
//!   transform), `loot[]` (loot spawners / containers), `groups[]` (foundations), `skipped{}`.
//! * `doors[]` (2026-10-08): every `ABuildingWall` with a `DoorMesh`: actor index, door mesh (its collision is in `meshes{}`), `DoorOffset`,
//!   `DoorAnimatingMaterial`, opening / closing cues, `bMirrored`, the root world transform (the door component is runtime-created, `fn_building::door`).
//! Skipped: the HLOD proxies (`LODActor`), the persistent map's own non-Blueprint backdrop actors (ocean planes, far rocks: `props backdrop`).
//!
//! ALL POIS (2026-10-07) `props poi --all [--only a,b]`: one file per streamed level of `Athena_Terrain` (its `LevelStreaming*` exports, `layout::levels`: 35 in
//! 1.8, `Athena_Terrain_Audio` left out), the WHOLE level (no radius) + the building worlds its foundations stream in, named after the level
//! (`Athena_POI_RangerOutpost` -> `rangeroutpost.json`, `Athena_Terrain_LS_05` -> `ls_05.json`, the persistent level -> `terrain.json`), with `center_cm` /
//! `radius_m` = the bounding circle of its placements, plus `poi/index.json` (file, level, kind, area, LevelTransform, counts). In a landscape level
//! (`terrain`, `ls_*`) the actors the game's props path places (catalog classes of the tree / rock / bush / hedge categories, read from `<out>/catalog.json`) are skipped.
//! In both modes: the map's own ground cover (the top-level `FoliageInstancedStaticMeshComponent`s, `props foliage`) is only counted in `skipped{}`; the
//! `S_WavyGrass_01` foliage of the streamed building worlds (which `props foliage` does not scan) goes to `ground_cover.instances` (compact rows) for the
//! game's grass path, not to `placements[]`.
use super::backdrop::rel_xf;
use super::bp::{load_bp, merge, Bp};
use super::layout::{compose, levels, Xf};
use super::PropsArgs;
use crate::{write_file, Source};
use fn_uasset::PackageData;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

struct Ctx<'a> {
    src: &'a Source,
    bps: HashMap<String, Option<Bp>>,
    actors: Vec<Value>,
    out: Vec<Value>,
    hidden: Vec<Value>,
    groups: Vec<Value>,
    lights: Vec<Value>,
    particles: Vec<Value>,
    loot: Vec<Value>,
    /// DOORS (2026-10-08): one row per `ABuildingWall` whose merged `DoorMesh` is set (the door component FUN_1404abb40 creates at runtime: it is not in
    /// the level package, so no placement shows it). See `fn_building::door`.
    doors: Vec<Value>,
    audio: usize,
    skipped: BTreeMap<String, usize>,
    health_props: BTreeMap<String, Vec<(f64, f64)>>,
    health_section: BTreeMap<String, Vec<(f64, f64)>>,
    /// ALL POIS: a landscape level (`Athena_Terrain`, `Athena_Terrain_LS_0N`): the actors the game's props path places from `layout/all.json` (a class of
    /// `<out>/catalog.json` in the categories `fn_render::static_loader::DEFAULT_CATEGORIES` tree / rock / bush / hedge) are left to it; the rest (terraces, cliffs,
    /// the foundations and their streamed shacks, cars, fences, signs, ...) goes to the file. Empty = nothing skipped.
    skip_layout: std::sync::Arc<std::collections::HashSet<String>>,
    /// ALL POIS: the `S_WavyGrass_01` instances of the streamed building worlds' InstancedFoliageActors (the same ground cover as the map's own, which
    /// `props foliage` exports; those worlds are not scanned there): `[x, y, z, qx, qy, qz, qw, scale, PerInstanceRandom]`, handed to the grass path.
    ground_cover: Vec<Value>,
    /// The curve keys of every health row an actor uses (`health_rows{}` of the file; an actor names its row, so the 51 keys are not repeated per actor).
    health_rows: BTreeMap<String, Value>,
    /// WORLD (2026-10-07): the level's HLOD clusters (`LODActor`s: proxy mesh, transform, `MinDrawDistance`, the sub actors they replace).
    hlod: Vec<Value>,
    /// WORLD: the actors left to the props layout (`skip_layout`), as `[class, x, y, z, cull_cm, hlod]` (`hlod` = cluster index or -1): the layout has no
    /// per-instance draw distance and no HLOD parent, the game matches its placements to these rows by class and position.
    layout_actors: Vec<Value>,
    /// WORLD: `BuildingTextureData` assets read (path -> its Diffuse / Normal / Specular / OverrideMaterial).
    texture_data: HashMap<String, Option<Value>>,
    volumes: Vec<Value>,
}

/// WORLD (2026-10-07): a `BuildingTextureData` asset's properties (`None` = unreadable).
fn texture_data(cx: &mut Ctx, path: &str) -> Option<Value> {
    if let Some(v) = cx.texture_data.get(path) { return v.clone(); }
    let pkg_path = super::bp::package_of(path).to_string();
    let v = PackageData::load(&cx.src.set, &pkg_path).ok().and_then(|pd| { let p = pd.package().ok()?; let i = (0..p.exports.len()).find(|&i| p.export_class_name(i) == "BuildingTextureData")?; Some(p.read_export(i).ok()?.props.to_json(&p)) });
    cx.texture_data.insert(path.to_string(), v.clone());
    v
}

/// WORLD (2026-10-07): the material slot 0 of a BuildingSMActor with `TextureData` (ABuildingSMActor vfunc_310 @140461f50): a MID of TextureData[0]'s
/// `OverrideMaterial`, else of the MESH's material 0 (`UStaticMesh::GetMaterial(0)`, not the component's override), with per layer i = 0..3 (`TextureData`,
/// `TextureData[1..3]`, BuildingTextureData +0x28 / +0x30 / +0x38) the texture parameters `Diffuse` / `Normals` / `SpecularMasks` (layer 0) and
/// `Diffuse_Texture_N` / `Normals_Texture_N` / `SpecularMasks_N` (N = i + 1; FName tables @140099e30 / @14009a510 / @14009a820), each set only when the
/// asset has that texture. `None` = the actor has no TextureData.
fn texture_data_mid(cx: &mut Ctx, cdo: &Map<String, Value>) -> Option<Value> {
    let mut tex = Map::new();
    let mut base = Value::Null;
    let mut any = false;
    for i in 0..4 {
        let key = if i == 0 { "TextureData".to_string() } else { format!("TextureData[{i}]") };
        let Some(p) = cdo.get(&key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else { continue };
        let Some(td) = texture_data(cx, p) else { continue };
        any = true;
        let names = if i == 0 { ["Diffuse".to_string(), "Normals".to_string(), "SpecularMasks".to_string()] } else { [format!("Diffuse_Texture_{}", i + 1), format!("Normals_Texture_{}", i + 1), format!("SpecularMasks_{}", i + 1)] };
        for (field, name) in ["Diffuse", "Normal", "Specular"].iter().zip(names) {
            if let Some(t) = td.get(*field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) { tex.insert(name, json!(t)); }
        }
        if i == 0 { if let Some(m) = td.get("OverrideMaterial").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) { base = json!(m); } }
    }
    any.then(|| json!({"base": base, "textures": tex}))
}

/// WORLD (2026-10-07): `UStaticMeshComponent::Serialize`'s native tail = `TArray<FStaticMeshComponentLODInfo> LODData` (UE4.16
/// StaticMeshComponent.cpp:286-292 and `operator<<(FArchive&, FStaticMeshComponentLODInfo&)` :2543): per LOD `FStripDataFlags` (u8 global, u8 class), the
/// `MapBuildDataId` FGuid unless server-stripped (global & 2), then unless the class flag 1 (override colours stripped) a u8 `bLoadVertexColorData` and the
/// painted `OverrideVertexColors` (`FColorVertexBuffer::Serialize`: strip flags, i32 stride, i32 NumVertices, then unless server-stripped the bulk array i32
/// element size 4 / i32 count / count x FColor, bytes B G R A), then `PaintedVertices` only when the editor data is kept (global & 1 clear: never cooked).
/// Returns the colours per LOD (hex of the raw FColor bytes, `None` = that LOD has none); `None` = no LOD paints, or the tail is not this layout.
pub fn override_vertex_colors(tail: &[u8]) -> Option<Vec<Option<String>>> {
    let mut p = 0usize;
    let rd_i32 = |p: &mut usize| -> Option<i32> { let b = tail.get(*p..*p + 4)?; *p += 4; Some(i32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
    let rd_u8 = |p: &mut usize| -> Option<u8> { let b = *tail.get(*p)?; *p += 1; Some(b) };
    let n = rd_i32(&mut p)?;
    if !(1..=8).contains(&n) { return None; }
    let mut out = vec![];
    for _ in 0..n {
        let (global, class) = (rd_u8(&mut p)?, rd_u8(&mut p)?);
        if global & 1 == 0 { return None; } // editor data kept (PaintedVertices follow): not a cooked component
        if global & 2 == 0 { p += 16; } // MapBuildDataId
        let mut colors = None;
        if class & 1 == 0 && rd_u8(&mut p)? != 0 {
            let cglobal = rd_u8(&mut p)?; let _cclass = rd_u8(&mut p)?;
            let stride = rd_i32(&mut p)?;
            let num = rd_i32(&mut p)?;
            if stride != 4 || num < 0 { return None; }
            if cglobal & 2 == 0 && num > 0 {
                let (esize, count) = (rd_i32(&mut p)?, rd_i32(&mut p)?);
                if esize != 4 || count != num { return None; }
                let b = tail.get(p..p + 4 * count as usize)?;
                p += 4 * count as usize;
                colors = Some(b.iter().map(|x| format!("{x:02x}")).collect::<String>());
            }
        }
        out.push(colors);
    }
    if p != tail.len() || out.iter().all(|c| c.is_none()) { return None; }
    Some(out)
}

/// Component JSON of one level package (read once), merged with the Blueprint archetype on request.
struct Comps { raw: HashMap<usize, (Value, Vec<u8>)> }
impl Comps {
    fn read(&mut self, i: usize, pkg: &fn_uasset::Package<'_>) -> Option<(Value, Vec<u8>)> {
        if let Some(v) = self.raw.get(&i) { return Some(v.clone()); }
        let o = pkg.read_export(i).ok()?;
        let mut j = o.props.to_json(pkg);
        let parent_i = o.props.get("AttachParent").and_then(|v| v.as_object()).and_then(|r| r.export_index());
        if let (Value::Object(m), Some(p)) = (&mut j, parent_i) { m.insert("_attach".into(), json!(p)); }
        let r = (j, o.trailing.to_vec());
        self.raw.insert(i, r.clone());
        Some(r)
    }
    /// The archetype (the class chain's component template of the same name) overlaid with the instance's delta.
    fn merged(&mut self, c: usize, pkg: &fn_uasset::Package<'_>, bp: Option<&Bp>) -> Option<(Value, Vec<u8>)> {
        let (j, tail) = self.read(c, pkg)?;
        let mut m = bp.and_then(|b| b.templates.get(&pkg.exports[c].name)).map(obj).unwrap_or_default();
        merge(&mut m, &obj(&j));
        Some((Value::Object(m), tail))
    }
    /// World transform of a component: the AttachParent chain (max 8) inside the package, merged relative transforms, then `parent`.
    fn world(&mut self, c: usize, pkg: &fn_uasset::Package<'_>, bp: Option<&Bp>, parent: Xf) -> Option<Xf> {
        let mut chain = vec![];
        let mut cur = Some(c);
        while let Some(k) = cur {
            if chain.len() > 8 { break; }
            let (j, _) = self.merged(k, pkg, bp)?;
            cur = j.get("_attach").and_then(|v| v.as_u64()).map(|v| v as usize);
            chain.push(j);
        }
        let mut xf = parent;
        for j in chain.iter().rev() { xf = compose(rel_xf(j), xf); }
        Some(xf)
    }
}

fn skip(cx: &mut Ctx, what: String) { *cx.skipped.entry(what).or_insert(0) += 1; }

fn xf_json(x: &Xf) -> (Value, Value, Value) { (json!(x.t), json!(x.q), json!(x.s)) }

/// UE `UCollisionProfile` presets that do not block the `Pawn` channel (Engine BaseEngine.ini [/Script/Engine.CollisionProfile]); `Custom` reads the
/// component's own `CollisionResponses`.
fn blocks_pawn(body: &Value) -> (bool, String) {
    let profile = body.get("CollisionProfileName").and_then(|v| v.as_str()).unwrap_or("BlockAllDynamic").to_string(); // UStaticMeshComponent ctor default
    let enabled = body.get("CollisionEnabled").and_then(|v| v.as_str()).unwrap_or("");
    if enabled.ends_with("NoCollision") { return (false, profile); }
    let no = ["NoCollision", "OverlapAll", "OverlapAllDynamic", "OverlapOnlyPawn", "IgnoreOnlyPawn", "Trigger", "UI", "Spectator", "InvisibleWallDynamic_NoPawn", "CameraOnly", "WeaponOnly", "ProjectileOnly"];
    if no.iter().any(|p| profile.eq_ignore_ascii_case(p)) { return (false, profile); }
    let pawn = body.get("CollisionResponses").and_then(|c| c.get("ResponseArray")).and_then(|a| a.as_array()).and_then(|a| a.iter().find(|r| r.get("Channel").and_then(|c| c.as_str()) == Some("Pawn")))
        .and_then(|r| r.get("Response").and_then(|v| v.as_str())).map(|s| s.ends_with("Block"));
    (pawn.unwrap_or(true), profile)
}

fn obj(v: &Value) -> Map<String, Value> { v.as_object().cloned().unwrap_or_default() }

/// Actors of one level package placed with `parent`; `filter` = (centre, radius cm) for the top-level scan, `None` inside a streamed building world.
fn scan(cx: &mut Ctx, package: &str, parent: Xf, filter: Option<([f64; 2], f64)>, group: i64, depth: usize) -> Result<(), String> {
    let pd = PackageData::load(&cx.src.set, package).map_err(|e| format!("{package}: {e}"))?;
    let pkg = pd.package().map_err(|e| format!("{package}: {e}"))?;
    let n = pkg.exports.len();
    let level_leaf = package.rsplit('/').next().unwrap_or(package).to_string();
    // the components of each actor (export outer = the actor) and every export's JSON (lazily)
    let mut comps_of: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n { if let Some(o) = pkg.exports[i].outer_index.export_index() { if pkg.export_class_name(i).ends_with("Component") { comps_of.entry(o).or_default().push(i); } } }
    let mut cc = Comps { raw: HashMap::new() };
    // WORLD (HLOD): what each exported actor became in this file (by export index): an actor, a layout row or an HLOD cluster; the clusters' SubActors
    // are resolved against it once the whole level is scanned
    let mut ref_of: HashMap<usize, (u8, usize)> = HashMap::new();
    let mut pending: Vec<(usize, Vec<String>)> = vec![];
    for i in 0..n {
        let cn = pkg.export_class_name(i);
        if cn.ends_with("Component") || pkg.exports[i].name.starts_with("Default__") { continue; }
        let Ok(o) = pkg.read_export(i) else { continue };
        let Some(ri) = o.props.get("RootComponent").and_then(|v| v.as_object()).and_then(|r| r.export_index()) else { continue };
        let actor = o.props.to_json(&pkg);
        // the Blueprint chain (component archetypes, class defaults)
        let bp = if cn.ends_with("_C") {
            let bp_pkg = match pkg.resolve(pkg.exports[i].class_index) { fn_uasset::ObjectRef::Import { package, .. } => package, _ => String::new() };
            if !cx.bps.contains_key(&bp_pkg) { let b = load_bp(&cx.src.set, &bp_pkg).ok(); cx.bps.insert(bp_pkg.clone(), b); }
            cx.bps.get(&bp_pkg).cloned().flatten()
        } else { None };
        let bpr = bp.as_ref();
        let Some(xf) = cc.world(ri, &pkg, bpr, parent) else { continue };
        if let Some((c, r)) = filter { if ((xf.t[0] - c[0]).powi(2) + (xf.t[1] - c[1]).powi(2)).sqrt() > r { continue; } }
        // a foundation: its streamed building world(s), placed with the actor transform
        if let Some(worlds) = actor.get("AdditionalWorlds").and_then(|v| v.as_array()) {
            for w in worlds.iter().filter_map(|w| w.as_str()).filter(|w| !w.is_empty()) {
                let g = cx.groups.len() as i64;
                let wp = super::bp::package_of(w).to_string();
                cx.groups.push(json!({"actor": pkg.exports[i].name, "class": cn, "level": level_leaf, "world": wp, "t": xf.t, "q": xf.q, "s": xf.s}));
                if depth < 4 { if let Err(e) = scan(cx, &wp, xf, None, g, depth + 1) { eprintln!("[poi] {e}"); } }
            }
        }
        // the persistent map's own static meshes (ocean planes, far skybox rocks) are the backdrop export's
        if package == super::layout::MAP && !cn.ends_with("_C") { skip(cx, format!("{cn} (Athena_Terrain backdrop actor)")); continue; }
        if cn == "LODActor" {
            // WORLD (2026-10-07): the HLOD cluster: its proxy component (StaticMeshComponent0: the merged SM_LOD_* mesh of the level's _HLOD package,
            // MinDrawDistance = LODDrawDistance) and the SubActors it stands for (FLODSceneTree: the proxy is drawn and its sub actors hidden while the view is
            // at least MinDrawDistance (or r.HLOD.DistanceOverride) from the proxy's bounds box)
            let (Some((m, _)), Some(cxf)) = (cc.merged(ri, &pkg, bpr), cc.world(ri, &pkg, bpr, parent)) else { skip(cx, "LODActor (no proxy component)".into()); continue };
            let Some(mesh) = m.get("StaticMesh").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from) else { skip(cx, "LODActor (no proxy mesh)".into()); continue };
            let bounds = cx.src.package(super::bp::package_of(&mesh)).ok().and_then(|p| fn_mesh::StaticMesh::parse_named(&p, mesh.split_once('.').map(|x| x.1)).ok()).map(|sm| {
                let (o, e) = (sm.bounds.origin, sm.bounds.extent);
                let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
                for k in 0..8 {
                    let c = [o[0] as f64 + if k & 1 == 0 { -1. } else { 1. } * e[0] as f64, o[1] as f64 + if k & 2 == 0 { -1. } else { 1. } * e[1] as f64, o[2] as f64 + if k & 4 == 0 { -1. } else { 1. } * e[2] as f64];
                    let w = compose(Xf { q: [0., 0., 0., 1.], t: c, s: [1.; 3] }, cxf).t;
                    for a in 0..3 { lo[a] = lo[a].min(w[a]); hi[a] = hi[a].max(w[a]); }
                }
                json!({"min": lo, "max": hi})
            });
            let Some(bounds) = bounds else { skip(cx, "LODActor (proxy mesh not parsed)".into()); continue };
            let subs: Vec<String> = actor.get("SubActors").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str()).map(|p| p.rsplit([':', '.']).next().unwrap_or(p).to_string()).collect()).unwrap_or_default();
            let (t, q, sc) = xf_json(&cxf);
            let ci = cx.hlod.len();
            cx.hlod.push(json!({"name": pkg.exports[i].name, "level": level_leaf, "mesh": mesh, "t": t, "q": q, "s": sc, "bounds": bounds,
                "min_draw_cm": m.get("MinDrawDistance").and_then(|v| v.as_f64()).or_else(|| actor.get("LODDrawDistance").and_then(|v| v.as_f64())).unwrap_or(0.),
                "lod_level": actor.get("LODLevel").and_then(|v| v.as_i64()).unwrap_or(1), "cast_shadow": m.get("CastShadow").and_then(|v| v.as_bool()).unwrap_or(true),
                "actors": [], "layout": [], "children": [], "parent": -1, "sub_actors": subs.len()}));
            ref_of.insert(i, (2, ci));
            pending.push((ci, subs));
            continue;
        }
        let class_short = cn.trim_end_matches("_C").to_string();
        let native = bp.as_ref().and_then(|b| b.chain.last()).map(|l| l.class.clone()).unwrap_or_else(|| cn.clone());
        // ---- the actor's building data (class defaults, instance overrides) ----
        let mut cdo = bp.as_ref().map(|b| b.cdo.clone()).unwrap_or_default();
        merge(&mut cdo, &obj(&actor));
        let s = |k: &str| cdo.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let key = cdo.get("AttributeInitKey");
        let cat = key.and_then(|k| k.get("AttributeInitCategory")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let sub = key.and_then(|k| k.get("AttributeInitSubCategory")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let row = format!("{cat}.{sub}.FortHealthSet.MaxHealth");
        let (table, keys) = if cat.is_empty() { ("", None) } else if let Some(k) = cx.health_props.get(&row) { ("AttributesBuildingProps", Some(k.clone())) } else if let Some(k) = cx.health_section.get(&row) { ("AttributesBuildingSection", Some(k.clone())) } else { ("", None) };
        // ALL POIS: in a landscape level the layout's harvestables (`Gathered::kind_of`: a `Resource*` / `Prop*` category with a resource type) stay the props path's
        if depth == 0 && cn.ends_with("_C") && cx.skip_layout.contains(&class_short) {
            skip(cx, format!("{class_short} (placed by the props layout)"));
            // WORLD: the row the game matches its layout placement to (draw distance of the root component, HLOD cluster resolved below)
            let cull = cc.merged(ri, &pkg, bpr).map(|(m, _)| m.get("CachedMaxDrawDistance").and_then(|v| v.as_f64()).filter(|v| *v > 0.).or_else(|| m.get("LDMaxDrawDistance").and_then(|v| v.as_f64()).filter(|v| *v > 0.)).unwrap_or(0.)).unwrap_or(0.);
            ref_of.insert(i, (1, cx.layout_actors.len()));
            cx.layout_actors.push(json!([class_short, (xf.t[0] * 10.).round() / 10., (xf.t[1] * 10.).round() / 10., (xf.t[2] * 10.).round() / 10., cull.round(), -1]));
            continue;
        }
        if let Some(k) = keys.as_ref() { cx.health_rows.entry(row.clone()).or_insert_with(|| json!(k.iter().map(|(t, v)| json!([t, v])).collect::<Vec<_>>())); }
        let actor_i = cx.actors.len();
        ref_of.insert(i, (0, actor_i));
        let loot = class_short.contains("FloorLoot") || class_short.starts_with("Tiered_"); // the loot spawners: floor loot markers, chests, ammo boxes
        let flag = |k: &str| cdo.get(k).and_then(|v| v.as_bool());
        cx.actors.push(json!({
            "name": pkg.exports[i].name, "class": class_short, "native": native, "level": level_leaf, "group": group, "top": depth == 0,
            "t": xf.t, "q": xf.q, "s": xf.s,
            "attribute_key": {"category": cat, "sub_category": sub}, "health_row": row, "health_table": table,
            "health_t0": keys.as_ref().and_then(|k| k.first()).map(|k| k.1),
            "resource": s("ResourceType").trim_start_matches("EFortResourceType::"),
            "amount_row": cdo.get("BuildingResourceAmountOverride").and_then(|a| a.get("RowName")).and_then(|v| v.as_str()),
            "invulnerable": flag("bIsInvulnerable"), "no_camera_collision": flag("bNoCameraCollision"),
            "building_level": cdo.get("CurrentBuildingLevel"), "centroid_offset": cdo.get("CentroidOffset"),
            "do_not_block_buildings": flag("bDoNotBlockBuildings"), "destroy_on_player_building_placement": flag("bDestroyOnPlayerBuildingPlacement"),
            "use_centroid_for_block_buildings_check": flag("bUseCentroidForBlockBuildingsCheck"),
            "death_particles": cdo.get("DeathParticles"), "death_sound": cdo.get("DeathSound"), "hit_sound": cdo.get("PlayHitSound"),
            "loot_tier_group": cdo.get("SearchLootTierGroup").or_else(|| cdo.get("DestructionLootTierGroup")),
            "loot": loot,
        }));
        let parents: Vec<String> = bp.as_ref().map(|b| b.chain.iter().skip(1).take(b.chain.len().saturating_sub(2)).map(|l| l.class.trim_end_matches("_C").to_string()).collect()).unwrap_or_default();
        if !parents.is_empty() { cx.actors[actor_i]["parents"] = json!(parents); }
        if loot { cx.loot.push(json!({"actor": actor_i, "class": class_short, "native": native, "t": xf.t, "q": xf.q, "tier_group": cdo.get("SearchLootTierGroup")})); }
        // DOORS (2026-10-08): `ABuildingWall::PreInitializeComponents` @1404c0210 -> FUN_1404abb40 creates the DoorComponent when `DoorMesh` (+0x1040) is set:
        // attached to the root (= `xf`) at `DoorOffset` (+0xFF8, instance editable: the merged value), slot 0 = `DoorAnimatingMaterial` (+0x1038)
        if let Some(mesh) = cdo.get("DoorMesh").and_then(|v| v.as_str()).filter(|m| !m.is_empty()) {
            let sv = |k: &str| cdo.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty());
            cx.doors.push(json!({"actor": actor_i, "name": pkg.exports[i].name, "class": class_short, "mesh": mesh, "offset": cdo.get("DoorOffset").cloned().unwrap_or(json!([0., 0., 0.])),
                "material": sv("DoorAnimatingMaterial"), "opening_sound": sv("DoorOpeningSound"), "closing_sound": sv("DoorClosingSound"), "mirrored": flag("bMirrored").unwrap_or(false),
                "t": xf.t, "q": xf.q, "s": xf.s}));
        }
        // ---- every component ----
        let mid = texture_data_mid(cx, &cdo);
        let main_smc = actor.get("StaticMeshComponent").and_then(|v| v.as_str()).map(|p| p.rsplit(':').next().unwrap_or(p).to_string());
        let actor_mesh = actor.get("StaticMesh").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from).or_else(|| bp.as_ref().and_then(|b| b.str_of("StaticMesh").filter(|s| !s.is_empty()).map(String::from)));
        let mut drawn = 0;
        for c in comps_of.get(&i).cloned().unwrap_or_default() {
            let ccn = pkg.export_class_name(c);
            let cname = pkg.exports[c].name.clone();
            let Some((m, tail)) = cc.merged(c, &pkg, bpr) else { continue };
            let Some(cxf) = cc.world(c, &pkg, bpr, parent) else { continue };
            let (t, q, sc) = xf_json(&cxf);
            let hidden = m.get("bVisible").and_then(|v| v.as_bool()) == Some(false) || m.get("bHiddenInGame").and_then(|v| v.as_bool()) == Some(true);
            match ccn.as_str() {
                "PointLightComponent" | "SpotLightComponent" | "RectLightComponent" => {
                    let mut l = m.as_object().cloned().unwrap_or_default();
                    for k in ["_attach", "AttachParent", "LightGuid", "UCSModifiedProperties", "CreationMethod", "bNetAddressable"] { l.remove(k); }
                    cx.lights.push(json!({"actor": actor_i, "actor_class": class_short, "component": cname, "class": ccn, "hidden": hidden, "t": t, "q": q, "props": l}));
                    continue;
                }
                "ParticleSystemComponent" => { cx.particles.push(json!({"actor": actor_i, "actor_class": class_short, "component": cname, "template": m.get("Template"), "auto_activate": m.get("bAutoActivate"), "hidden": hidden, "cull_cm": m.get("CachedMaxDrawDistance"), "params": m.get("InstanceParameters"), "t": t, "q": q, "s": sc})); continue; }
                "AudioComponent" => { cx.audio += 1; continue; }
                "BrushComponent" => {
                    let setup = (0..pkg.exports.len()).find(|&b| pkg.export_class_name(b) == "BodySetup" && pkg.exports[b].outer_index.export_index() == Some(c));
                    let boxes: Vec<Value> = setup.and_then(|b| pkg.read_export(b).ok()).map(|o| o.props.to_json(&pkg)).and_then(|j| j.pointer("/AggGeom/ConvexElems").and_then(|v| v.as_array()).cloned()).unwrap_or_default()
                        .iter().filter_map(|e| { let b = e.get("ElemBox")?; Some(json!({"min": b.get("Min")?, "max": b.get("Max")?, "points": e.get("VertexData").cloned().unwrap_or(json!([]))})) }).collect();
                    cx.volumes.push(json!({"actor": actor_i, "class": class_short, "native": native, "priority": cdo.get("Priority"), "physics_on_contact": cdo.get("bPhysicsOnContact"), "water": cdo.get("bWaterVolume"), "fluid_friction": cdo.get("FluidFriction"), "t": t, "q": q, "s": sc, "boxes": boxes}));
                    continue;
                }
                _ => {}
            }
            let foliage = ccn == "FoliageInstancedStaticMeshComponent" || ccn == "HierarchicalInstancedStaticMeshComponent" || ccn == "InstancedStaticMeshComponent";
            if !ccn.ends_with("StaticMeshComponent") { continue; }
            // the mesh: the BuildingSMActor's main component shows the actor's StaticMesh
            let mesh = if main_smc.as_deref() == Some(cname.as_str()) { actor_mesh.clone().or_else(|| m.get("StaticMesh").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from)) } else { m.get("StaticMesh").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from) };
            let Some(mesh) = mesh else { skip(cx, format!("{class_short}.{cname} (component without a mesh)")); continue };
            if hidden {
                skip(cx, format!("{class_short}.{cname} (hidden in game)"));
                if !foliage {
                    let (collide, profile) = blocks_pawn(m.get("BodyInstance").unwrap_or(&Value::Null));
                    let cull = m.get("CachedMaxDrawDistance").and_then(|v| v.as_f64()).filter(|v| *v > 0.).or_else(|| m.get("LDMaxDrawDistance").and_then(|v| v.as_f64()).filter(|v| *v > 0.)).unwrap_or(0.);
                    cx.hidden.push(json!({"actor": actor_i, "comp": cname, "mesh": mesh, "overrides": m.get("OverrideMaterials").cloned().unwrap_or(json!([])), "t": t, "q": q, "s": sc, "collide": collide, "profile": profile, "cull_cm": cull, "cast_shadow": m.get("CastShadow").and_then(|v| v.as_bool()).unwrap_or(true)}));
                }
                continue;
            }
            let overrides: Vec<Value> = m.get("OverrideMaterials").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let (collide, profile) = blocks_pawn(m.get("BodyInstance").unwrap_or(&Value::Null));
            let cull = m.get("CachedMaxDrawDistance").and_then(|v| v.as_f64()).filter(|v| *v > 0.).or_else(|| m.get("LDMaxDrawDistance").and_then(|v| v.as_f64()).filter(|v| *v > 0.)).unwrap_or(0.);
            let cast_shadow = m.get("CastShadow").and_then(|v| v.as_bool()).unwrap_or(true);
            let step_up = m.get("CanCharacterStepUpOn").and_then(|v| v.as_str()).map(|s| s.trim_start_matches("ECB_").to_string());
            // ALL POIS: a top-level FoliageInstancedStaticMeshComponent is the map's ambient ground cover (S_WavyGrass_01, asserted by `props foliage`, drawn by the
            // grass path): only counted here (it was 95% of lodge.json)
            if foliage && depth == 0 && ccn == "FoliageInstancedStaticMeshComponent" { *cx.skipped.entry("FoliageInstancedStaticMeshComponent instances (ground cover: props foliage)".into()).or_insert(0) += super::foliage::component_matrices(&tail).map_or(0, |m| m.len()); continue; }
            if foliage && ccn == "FoliageInstancedStaticMeshComponent" && mesh.rsplit('/').next().is_some_and(|l| l.starts_with("S_WavyGrass_01.")) {
                match super::foliage::component_matrices(&tail) {
                    Ok(mats) => {
                        let rnd = super::foliage::random_fractions(m.get("InstancingRandomSeed").and_then(|v| v.as_i64()).unwrap_or(0) as i32, mats.len());
                        let r = |v: f64, k: f64| (v * k).round() / k;
                        for (mtx, rv) in mats.iter().zip(&rnd) {
                            let x = compose(super::foliage::matrix_to_xf(mtx), cxf);
                            cx.ground_cover.push(json!([r(x.t[0], 10.), r(x.t[1], 10.), r(x.t[2], 10.), r(x.q[0], 1e4), r(x.q[1], 1e4), r(x.q[2], 1e4), r(x.q[3], 1e4), r(x.s[0], 1e3), r(*rv as f64, 1e4)]));
                        }
                    }
                    Err(e) => { eprintln!("[poi] {level_leaf} {cname}: {e}"); skip(cx, format!("{ccn} (instances not decoded)")); }
                }
                continue;
            }
            if foliage {
                match super::foliage::component_matrices(&tail) {
                    Ok(mats) => {
                        for mtx in mats {
                            let ixf = compose(super::foliage::matrix_to_xf(&mtx), cxf);
                            cx.out.push(json!({"actor": actor_i, "comp": cname, "mesh": mesh, "overrides": overrides, "t": ixf.t, "q": ixf.q, "s": ixf.s, "collide": collide, "profile": profile, "cull_cm": cull, "cast_shadow": cast_shadow, "step_up": step_up, "instanced": true}));
                            drawn += 1;
                        }
                    }
                    Err(e) => { eprintln!("[poi] {level_leaf} {cname}: {e}"); skip(cx, format!("{ccn} (instances not decoded)")); }
                }
                continue;
            }
            let mut pl = json!({"actor": actor_i, "comp": cname, "mesh": mesh, "overrides": overrides, "t": t, "q": q, "s": sc, "collide": collide, "profile": profile, "cull_cm": cull, "cast_shadow": cast_shadow, "step_up": step_up});
            // WORLD (2026-10-07): the component's painted vertex colours (the street / terrace pieces' Terrain_Mat layer blend)
            if let Some(v) = override_vertex_colors(&tail) { pl["vcol"] = json!(v); }
            // WORLD: the BuildingSMActor's TextureData MID on its main component's slot 0
            if main_smc.as_deref() == Some(cname.as_str()) { if let Some(m) = &mid { pl["mid0"] = m.clone(); } }
            cx.out.push(pl);
            drawn += 1;
        }
        if drawn == 0 { skip(cx, format!("{class_short} (no visible static mesh)")); }
    }
    // WORLD (HLOD): the clusters' SubActors -> actors / layout rows / child clusters of this file
    let by_name: HashMap<&str, usize> = (0..n).map(|i| (pkg.exports[i].name.as_str(), i)).collect();
    for (ci, subs) in pending {
        let (mut actors, mut layout, mut children, mut missing) = (vec![], vec![], vec![], 0);
        for s in &subs {
            match by_name.get(s.as_str()).and_then(|e| ref_of.get(e)) {
                Some((0, a)) => actors.push(*a),
                Some((1, l)) => { layout.push(*l); cx.layout_actors[*l][5] = json!(ci); }
                Some((_, c)) => { children.push(*c); cx.hlod[*c]["parent"] = json!(ci); }
                None => missing += 1,
            }
        }
        let h = &mut cx.hlod[ci];
        h["actors"] = json!(actors); h["layout"] = json!(layout); h["children"] = json!(children); h["sub_missing"] = json!(missing);
    }
    Ok(())
}

/// The collision of each mesh: simple shapes (`collision.rs`) and, for complex-as-simple bodies without simple shapes, the collision LOD's triangles.
fn mesh_collision(src: &Source, mesh: &str) -> Value {
    let pkg_path = super::bp::package_of(mesh).to_string();
    let parsed = src.package(&pkg_path).ok().and_then(|p| fn_mesh::StaticMesh::parse(&p).ok());
    let bs = parsed.as_ref().and_then(|m| m.body_setup.clone());
    let mut c = match super::collision::collision_json(&src.set, &pkg_path, Some(bs.as_deref().unwrap_or("BodySetup"))) { Ok(c) => c, Err(e) => json!({"shapes": [], "error": e}) };
    let empty = c.get("shapes").and_then(|s| s.as_array()).map_or(true, |a| a.is_empty());
    let flag = c.get("trace_flag").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if empty && flag.contains("UseComplexAsSimple") {
        if let Some(m) = &parsed {
            let li = (m.lod_for_collision.max(0) as usize).min(m.lods.len().saturating_sub(1));
            if let Some(l) = m.lods.get(li) {
                let pos: Vec<Value> = l.positions.iter().map(|p| json!([(p[0] * 10.).round() / 10., (p[1] * 10.).round() / 10., (p[2] * 10.).round() / 10.])).collect();
                if let Value::Object(o) = &mut c { o.insert("complex".into(), json!({"lod": li, "positions": pos, "indices": l.indices})); }
            }
        }
    }
    c
}

/// The file name of a streamed level in `--all` mode: `Athena_POI_RangerOutpost` -> `rangeroutpost`, `Athena_Terrain_LS_05` -> `ls_05`, the persistent level ->
/// `terrain`; `None` = not exported (`Athena_Terrain_Audio`: sound actors only).
pub fn level_file_name(package: &str) -> Option<String> {
    let leaf = package.rsplit('/').next().unwrap_or(package);
    if leaf.ends_with("_Audio") { return None; }
    if package == super::layout::MAP { return Some("terrain".into()); }
    Some(leaf.trim_start_matches("Athena_POI_").trim_start_matches("Athena_Terrain_").to_ascii_lowercase())
}

/// One POI document: the scanned actors, their mesh collision, the area (`center_cm`, `radius_m`: the bounding circle of every placement / actor when no
/// explicit area was asked).
fn document(src: &Source, cx: &Ctx, name: &str, area: Option<([f64; 2], f64)>, lv_list: Vec<Value>, level: Option<&str>) -> (Value, String) {
    // DOORS: the door meshes' collision too (the door component's own body: it blocks the pawn while its collision is on)
    let meshes: BTreeSet<String> = cx.out.iter().chain(cx.doors.iter()).filter_map(|p| p["mesh"].as_str().map(String::from)).collect();
    let mut mesh_doc = Map::new();
    let (mut complex, mut no_col) = (0, 0);
    for m in &meshes {
        let c = mesh_collision(src, m);
        if c.get("complex").is_some() { complex += 1; }
        if c.get("shapes").and_then(|s| s.as_array()).map_or(true, |a| a.is_empty()) && c.get("complex").is_none() { no_col += 1; }
        mesh_doc.insert(m.clone(), c);
    }
    // ALL POIS: the area of a whole level = the bounding circle (xy) of its placements and actors
    let (center, radius_m) = area.unwrap_or_else(|| {
        let pts: Vec<[f64; 2]> = cx.out.iter().chain(cx.actors.iter()).filter_map(|p| p["t"].as_array().map(|t| [t[0].as_f64().unwrap_or(0.), t[1].as_f64().unwrap_or(0.)])).collect();
        if pts.is_empty() { return ([0.; 2], 0.); }
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for p in &pts { for k in 0..2 { lo[k] = lo[k].min(p[k]); hi[k] = hi[k].max(p[k]); } }
        let c = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
        let r = pts.iter().map(|p| ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt()).fold(0., f64::max);
        ([c[0].round(), c[1].round()], (r / 100.).ceil())
    });
    let levels_used: BTreeMap<String, usize> = cx.actors.iter().fold(BTreeMap::new(), |mut b, a| { *b.entry(a["level"].as_str().unwrap_or("").to_string()).or_insert(0) += 1; b });
    let mut line = format!("{name}: {} actors, {} mesh placements ({} distinct meshes: {} complex-as-simple, {} without collision), {} lights, {} particle systems, {} audio, {} loot actors, {} doors, {} building worlds streamed by foundations, {} ground-cover instances of those worlds; area ({:.0}, {:.0}) r {radius_m:.0} m",
        cx.actors.len(), cx.out.len(), meshes.len(), complex, no_col, cx.lights.len(), cx.particles.len(), cx.audio, cx.loot.len(), cx.doors.len(), cx.groups.len(), cx.ground_cover.len(), center[0], center[1]);
    for (l, n) in &levels_used { line.push_str(&format!("\n  level {l}: {n} actors")); }
    let doc = json!({
        "version": 2, "name": name, "level": level, "center_cm": center, "radius_m": radius_m,
        "units": "UE world cm (left handed, Z up); q = FQuat (x, y, z, w); t / q / s = world transform (the component's for placements / lights)",
        "levels": lv_list, "levels_used": levels_used, "groups": cx.groups, "actors": cx.actors, "placements": cx.out, "hidden_placements": cx.hidden, "meshes": mesh_doc,
        "lights": cx.lights, "particles": cx.particles, "loot": cx.loot, "doors": cx.doors, "volumes": cx.volumes, "audio_components": cx.audio, "skipped": cx.skipped, "health_rows": cx.health_rows,
        "hlod": cx.hlod, "layout_actors": {"fields": ["class", "x", "y", "z", "cull_cm", "hlod"], "rows": cx.layout_actors},
        "ground_cover": {"mesh": "/Game/Environments/Foliage/Meshes/S_WavyGrass_01.S_WavyGrass_01", "fields": ["x", "y", "z", "qx", "qy", "qz", "qw", "scale", "random"], "instances": cx.ground_cover},
    });
    (doc, line)
}

pub fn run(src: &Source, a: &PropsArgs) -> Result<(), String> {
    let mut center = None;
    let mut radius_m = 250.0;
    let mut name = "poi".to_string();
    let mut all = false;
    let mut only: Option<Vec<String>> = None;
    let mut it = a.rest.iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().cloned().ok_or(format!("{k} needs a value"));
        match k.as_str() {
            "--center" => { let s = v()?; let p: Vec<f64> = s.split(',').map(|x| x.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|_| "--center X,Y (UE cm)")?; if p.len() < 2 { return Err("--center X,Y".into()); } center = Some([p[0], p[1]]); }
            "--radius" => radius_m = v()?.parse().map_err(|_| "--radius needs metres")?,
            "--name" => name = v()?,
            "--all" => all = true,
            "--only" => only = Some(v()?.split(',').map(|s| s.trim().to_ascii_lowercase()).collect()),
            o => return Err(format!("props poi: unknown option {o}")),
        }
    }
    let health_props = super::export::load_curve_table(src, "/Game/Balance/DataTables/AttributesBuildingProps")?;
    let health_section = super::export::load_curve_table(src, "/Game/Balance/DataTables/AttributesBuildingSection")?;
    // the classes the game's props layout places (catalog kinds of the default categories): left out of the landscape levels' files
    let layout_classes: std::collections::HashSet<String> = std::fs::read(a.out.join("catalog.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|c| c["kinds"].as_array().map(|k| k.iter().filter(|k| ["tree", "rock", "bush", "hedge"].contains(&k["category"].as_str().unwrap_or(""))).filter_map(|k| k["id"].as_str().map(String::from)).collect())).unwrap_or_default();
    if all && layout_classes.is_empty() { eprintln!("[poi] no {}: the landscape levels keep their layout props too", a.out.join("catalog.json").display()); }
    let layout_classes = std::sync::Arc::new(layout_classes);
    let none = std::sync::Arc::new(std::collections::HashSet::new());
    let new_ctx = |skip_layout: bool| Ctx { src, bps: HashMap::new(), actors: vec![], out: vec![], hidden: vec![], groups: vec![], lights: vec![], particles: vec![], loot: vec![], doors: vec![], audio: 0, skipped: BTreeMap::new(), health_props: health_props.clone(), health_section: health_section.clone(), skip_layout: if skip_layout { layout_classes.clone() } else { none.clone() }, ground_cover: vec![], health_rows: BTreeMap::new(), hlod: vec![], layout_actors: vec![], texture_data: HashMap::new(), volumes: vec![] };
    let lvls = levels(src)?;
    let lv_list: Vec<Value> = lvls.iter().map(|lv| json!({"package": lv.package, "t": lv.xf.t, "q": lv.xf.q})).collect();
    if all {
        // ALL POIS (2026-10-07): one file per streamed level of Athena_Terrain (the real list: its `LevelStreaming*` exports, `layout::levels`), the whole level
        // (no radius) + the building worlds its foundations stream in; the landscape levels without the layout's harvestables (`skip_layout`)
        let jobs: Vec<(String, super::layout::Level)> = lvls.iter().filter_map(|lv| level_file_name(&lv.package).map(|n| (n, lv.clone())))
            .filter(|(n, _)| only.as_ref().map_or(true, |o| o.contains(n))).collect();
        let index = std::sync::Mutex::new(vec![]);
        let results = crate::parallel(&jobs, a.jobs, |(n, lv)| -> Result<(), String> {
            let landscape = lv.package == super::layout::MAP || n.starts_with("ls_");
            let mut cx = new_ctx(landscape);
            scan(&mut cx, &lv.package, lv.xf, None, -1, 0)?;
            let leaf = lv.package.rsplit('/').next().unwrap_or("");
            let (doc, line) = document(src, &cx, n, None, lv_list.clone(), Some(leaf));
            let path = a.out.join("poi").join(format!("{n}.json"));
            write_file(&path, serde_json::to_string(&doc).unwrap().as_bytes())?;
            println!("{line}\n  wrote {}", path.display());
            index.lock().unwrap().push(json!({"file": format!("{n}.json"), "level": lv.package, "kind": if landscape { "landscape" } else { "poi" }, "center_cm": doc["center_cm"], "radius_m": doc["radius_m"],
                "level_transform": {"t": lv.xf.t, "q": lv.xf.q}, "actors": cx.actors.len(), "placements": cx.out.len()}));
            Ok(())
        });
        let mut failed = 0;
        for (r, (n, _)) in results.into_iter().zip(&jobs) { if let Err(e) = r { eprintln!("[poi] {n}: {e}"); failed += 1; } }
        let mut idx = index.into_inner().unwrap();
        idx.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
        if only.is_none() { write_file(&a.out.join("poi").join("index.json"), serde_json::to_string_pretty(&json!({"version": 1, "map": super::layout::MAP, "levels": idx})).unwrap().as_bytes())?; }
        println!("{} levels exported, {failed} failed", jobs.len() - failed);
        return if failed == 0 { Ok(()) } else { Err(format!("{failed} levels failed")) };
    }
    let center = center.ok_or("props poi needs --center X,Y (UE world cm) or --all")?;
    let mut cx = new_ctx(false);
    for lv in &lvls {
        if let Err(e) = scan(&mut cx, &lv.package, lv.xf, Some((center, radius_m * 100.)), -1, 0) { eprintln!("[poi] {e}"); }
    }
    let (doc, line) = document(src, &cx, &name, Some((center, radius_m)), lv_list, None);
    println!("{line}");
    for g in &cx.groups { println!("  {} {} -> {}", g["level"].as_str().unwrap_or(""), g["actor"].as_str().unwrap_or(""), g["world"].as_str().unwrap_or("")); }
    let path = a.out.join("poi").join(format!("{name}.json"));
    write_file(&path, serde_json::to_string(&doc).unwrap().as_bytes())?;
    println!("wrote {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The cooked native tail of `Athena_POI_Suburban_006:PersistentLevel:S_Elevation_Ground_Z14125.StaticMeshComponent0` (Salty Springs street piece,
    /// 365 bytes): one LOD with a MapBuildDataId and 81 painted colours (= the 9 x 9 vertices of S_Elevation_Ground_Z).
    const SALTY_STREET_TAIL: &str = "010000000100aeaf2b5950bfb94fa9504bda457ac4e5010100040000005100000004000000510000000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff0000005f000000ff000000ff0000007700000000000000ff00000000000000ff000000ff0000006a00000000000000ff000000ff00000000000000ff0000009800000000000000ff000000ff0000000000000095000000ff000000ff000000ff00000000000000aa000000ff00000000000000000000002d00000000000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff000000ff00";
    fn unhex(s: &str) -> Vec<u8> { (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect() }
    #[test] fn the_painted_vertex_colours_of_a_cooked_component_tail_are_read() {
        let tail = unhex(SALTY_STREET_TAIL);
        assert_eq!(tail.len(), 365);
        let v = override_vertex_colors(&tail).expect("one painted LOD");
        assert_eq!(v.len(), 1);
        let c = v[0].as_ref().unwrap();
        assert_eq!(c.len(), 81 * 8, "81 FColors as hex");
        assert_eq!(&c[..8], &SALTY_STREET_TAIL[82..90]);
    }
    #[test] fn a_tail_without_painted_colours_or_with_trailing_bytes_gives_none() {
        assert_eq!(override_vertex_colors(&[0, 0, 0, 0]), None, "LODData empty");
        // one LOD, editor stripped, MapBuildDataId, bLoadVertexColorData 0
        let mut t = vec![1, 0, 0, 0, 1, 0]; t.extend([7u8; 16]); t.push(0);
        assert_eq!(override_vertex_colors(&t), None);
        let mut bad = unhex(SALTY_STREET_TAIL); bad.push(0);
        assert_eq!(override_vertex_colors(&bad), None, "a tail that is not exactly LODData");
    }
}
