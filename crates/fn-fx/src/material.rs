//! A material (or material instance chain) of the original packages: its serialized flags (blend mode, shading model, two-sidedness, depth test, mask clip), the
//! parameter overrides of the instance chain, the textures its uniform expressions reference (in the order the engine's `ReferencedTextures` has), the
//! `FMaterialUniformExpressionSet` recipe of its constant buffer and the cooked shader programs of the root material.
use crate::cooked::Cooked;
use crate::data::{long_package, package_files, PackageJson};
use crate::uniform::{evaluate, parse_set, Expr, Overrides, Packed, UniformSet};
use fn_pak::PakSet;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Serialized material flags (`UMaterial` properties; the MIC `BasePropertyOverrides` replace them).
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialFlags {
    /// `BLEND_Opaque` / `Masked` / `Translucent` / `Additive` / `Modulate` (default `BLEND_Opaque`).
    pub blend: String,
    /// `MSM_DefaultLit` (default) / `MSM_Unlit` / ...
    pub shading: String,
    pub two_sided: bool,
    pub disable_depth_test: bool,
    pub opacity_mask_clip: f32,
    pub translucency_lighting_mode: String,
    pub separate_translucency: bool,
    /// STORM TAA (2026-10-07): `UMaterial::bEnableResponsiveAA` (root material only, no instance override in 4.16; `FMaterialResource::ShouldEnableResponsiveAA`).
    /// A translucent draw of such a material tags its pixels for the responsive temporal AA blend (TranslucentRendering.cpp:531-557), e.g. `M_SafeZoneSphere`.
    pub responsive_aa: bool,
    pub domain: String,
}

impl MaterialFlags {
    /// The base-pass pixel shader writes the deferred GBuffer (targets 1..3 = normal, custom, base colour) and the material is lit by the deferred lights:
    /// UE4.16 BasePassCommon.usf:20 `USES_GBUFFER = SM4 && (MATERIALBLENDING_SOLID || MATERIALBLENDING_MASKED) && !FORWARD_SHADING`, and only a lit shading
    /// model has lighting (BasePassPixelShader.usf:1084 `PIXELSHADEROUTPUT_MRT1..3`). A translucent / additive / modulate material writes `SV_Target0` only
    /// (its lighting is the translucency volume inside the program, BasePassPixelShader.usf:955), whatever its shading model.
    pub fn uses_gbuffer(&self) -> bool { self.shading != "MSM_Unlit" && matches!(self.blend.as_str(), "BLEND_Opaque" | "BLEND_Masked") }
}

/// LOAD TIME (2026-10-06): `Clone` is cheap (the cooked programs are shared): a prewarmed material can be handed out to every request for its path.
#[derive(Clone)]
pub struct Material {
    /// Object path of the requested material (the instance when one was asked for).
    pub path: String,
    /// Object path of the root `Material` of the chain (NOT necessarily the package whose shader map is used: see `shader_map_package`).
    pub base_path: String,
    /// BVISFIX (2026-10-05): long package name whose cooked shader map / uniform expression set `cooked` + `uniform` came from (the child-most package of the
    /// chain that carries programs, `UMaterialInstance::GetMaterialResource`, MaterialInstance.cpp:1362; else the root's).
    pub shader_map_package: String,
    pub flags: MaterialFlags,
    /// Effective parameters of the chain (child over parent).
    pub overrides: Overrides,
    pub texture_overrides: HashMap<String, String>,
    /// Shared with every material of the same shader-map package ([`shader_map`]).
    pub cooked: Arc<Cooked>,
    pub uniform: UniformSet,
    /// `UMaterial::ReferencedTextures` of the root (object paths).
    pub referenced_textures: Vec<String>,
    /// Material-function packages walked while collecting the textures (diagnostics).
    pub functions: Vec<String>,
    /// MATERIAL PARAMETERS (2026-10-06): the collection object path behind each `MaterialCollectionN` uniform buffer of the programs (N = index): the uniform set's
    /// `ParameterCollections` StateIds resolved through the root's `MaterialParameterCollectionInfos` (else the collections the shader-map package imports);
    /// `None` = a StateId no known collection has (the program then reads the zero buffer, logged by the binder).
    pub collections: Vec<Option<String>>,
}

/// LOAD TIME (2026-10-06): the parsed shader map of a package, shared by every material that uses it, keyed by the long package name. Instances of one
/// parent use the parent's map: e.g. three pine-leaf instances, 477 programs each, ~150 ms per parse. Weak, so a map lives only as long as a material
/// holds it.
static SHADER_MAPS: std::sync::Mutex<Option<HashMap<String, (std::sync::Weak<Cooked>, Result<UniformSet, String>)>>> = std::sync::Mutex::new(None);

/// LOAD TIME (2026-10-06): `collect_textures` of a root material (root object path -> (referenced textures, walked function packages)).
static ROOT_TEXTURES: std::sync::Mutex<Option<HashMap<String, (Vec<String>, Vec<String>)>>> = std::sync::Mutex::new(None);

/// The cooked programs + uniform expression set of package `long`: exactly what [`Material::load`] parsed per material (`Cooked::parse` + `parse_set`),
/// parsed once while some material holds the result. The uniform set is only `?`-ed where the load used to parse it (a package without programs never needed it).
fn shader_map(set: &PakSet, long: &str) -> Result<(Arc<Cooked>, Result<UniformSet, String>), String> {
    if let Some((w, u)) = SHADER_MAPS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(long).cloned())) {
        if let Some(c) = w.upgrade() { return Ok((c, u)); }
    }
    let (asset, uexp, ubulk) = package_files(set, long)?;
    let p = fn_ueio::Package::parse(&asset, &uexp, &ubulk).map_err(|e| format!("{long}: {e}"))?;
    let c = Arc::new(Cooked::parse(&uexp, &p.names, p.summary.total_header_size as usize)?);
    let u = parse_set(&uexp, &p.names).map_err(|e| format!("{long}: {e}"));
    if let Ok(mut g) = SHADER_MAPS.lock() { g.get_or_insert_with(HashMap::new).insert(long.to_owned(), (Arc::downgrade(&c), u.clone())); }
    Ok((c, u))
}

/// LANDSCAPE MATERIAL (2026-10-07): the `.uexp`, name table, export ranges (name, uexp offset, size) and header size of a map package, read once and held until [`drop_map_programs`] (the landscape loader loads the instances of
/// one sublevel in a row), keyed by the long package name.
static MAP_PROGRAMS: std::sync::Mutex<Option<HashMap<String, Arc<(Vec<u8>, Vec<String>, Vec<(String, usize, usize)>, usize)>>>> = std::sync::Mutex::new(None);
/// LANDSCAPE MATERIAL: frees the parsed map packages of [`shader_map_scoped`] (the scoped maps already built stay alive in their materials).
pub fn drop_map_programs() { if let Ok(mut g) = MAP_PROGRAMS.lock() { *g = None; }  if let Ok(mut g) = MAP_JSON.lock() { *g = None; } }
/// LANDSCAPE MATERIAL: the JSON exports of the map packages [`Material::load`] walked (a sublevel's 10+ instances chain through it), until [`drop_map_programs`].
static MAP_JSON: std::sync::Mutex<Option<HashMap<String, Arc<PackageJson>>>> = std::sync::Mutex::new(None);
fn map_package_json(set: &PakSet, long: &str) -> Result<Arc<PackageJson>, String> {
    if !is_map_package(set, long) { return PackageJson::load(set, long).map(Arc::new); }
    if let Some(p) = MAP_JSON.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(long).cloned())) { return Ok(p); }
    let p = Arc::new(PackageJson::load(set, long)?);
    if let Ok(mut g) = MAP_JSON.lock() { g.get_or_insert_with(HashMap::new).insert(long.to_owned(), p.clone()); }
    Ok(p)
}

/// LANDSCAPE MATERIAL (2026-10-07): the shader map of ONE export of a map package: a landscape sublevel (`Athena_Terrain_LS_xx.umap`) carries the
/// `StaticPermutationMaterialResources` of each of its `LandscapeMaterialInstanceConstant`s inline (LS_00: 4 of its 10 instances, 486 programs each), so the
/// package-wide [`shader_map`] (first program / first uniform set of the package) would run another instance's programs. The programs and the uniform
/// expression set are the ones inside the export's serial range. `Ok(None)` = the export owns no programs (an instance of a parent that does).
fn shader_map_scoped(set: &PakSet, long: &str, export: &str) -> Result<Option<(Arc<Cooked>, Result<UniformSet, String>)>, String> {
    let key = format!("{long}:{export}");
    if let Some((w, u)) = SHADER_MAPS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&key).cloned())) {
        if let Some(c) = w.upgrade() { return Ok(Some((c, u))); }
    }
    let whole = match MAP_PROGRAMS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(long).cloned())) {
        Some(w) => w,
        None => {
            let (asset, uexp, ubulk) = package_files(set, long)?;
            let p = fn_ueio::Package::parse(&asset, &uexp, &ubulk).map_err(|e| format!("{long}: {e}"))?;
            let header = p.summary.total_header_size as usize;
            let ranges = p.exports.iter().map(|e| (e.object_name.clone(), (e.serial_offset as usize).saturating_sub(header), e.serial_size as usize)).collect();
            let w = Arc::new((uexp, p.names.clone(), ranges, header));
            if let Ok(mut g) = MAP_PROGRAMS.lock() { g.get_or_insert_with(HashMap::new).insert(long.to_owned(), w.clone()); }
            w
        }
    };
    let (uexp, names, ranges, header) = &*whole;
    let &(_, lo, size) = ranges.iter().find(|r| r.0 == export).ok_or_else(|| format!("{long}: no export {export}"))?;
    let hi = (lo + size).min(uexp.len());
    // only the owning export's streams are inflated and decoded (2026-10-08: the whole-map parse held all six sublevels' programs, multi-GB, at once)
    let c = Arc::new({ let all = Cooked::parse_range(uexp, names, *header, lo, hi)?; all.subset(lo, hi) });
    if c.programs.is_empty() { return Ok(None); }
    let u = parse_set(&uexp[lo..hi], names).map_err(|e| format!("{key}: {e}"));
    if let Ok(mut g) = SHADER_MAPS.lock() { g.get_or_insert_with(HashMap::new).insert(key, (Arc::downgrade(&c), u.clone())); }
    Ok(Some((c, u)))
}
fn is_map_package(set: &PakSet, long: &str) -> bool { set.read_game(long, "uasset").is_err() && set.read_game(long, "umap").is_ok() }

fn obj_path(v: &Value) -> Option<String> { v.as_str().map(|s| s.to_owned()).filter(|s| !s.is_empty()) }
fn num(v: &Value) -> Option<f64> { v.as_f64() }

fn flags_from(props: &Value, base: Option<&MaterialFlags>) -> MaterialFlags {
    let mut f = base.cloned().unwrap_or(MaterialFlags { blend: "BLEND_Opaque".into(), shading: "MSM_DefaultLit".into(), two_sided: false, disable_depth_test: false, opacity_mask_clip: 0.3333, translucency_lighting_mode: "TLM_VolumetricNonDirectional".into(), separate_translucency: true, responsive_aa: false, domain: "MD_Surface".into() });
    let s = |k: &str| props.get(k).and_then(|v| v.as_str()).map(|s| s.to_owned());
    if let Some(v) = s("BlendMode") { f.blend = v; }
    if let Some(v) = s("ShadingModel") { f.shading = v; }
    if let Some(v) = props.get("TwoSided").and_then(|v| v.as_bool()) { f.two_sided = v; }
    if let Some(v) = props.get("bDisableDepthTest").and_then(|v| v.as_bool()) { f.disable_depth_test = v; }
    if let Some(v) = props.get("OpacityMaskClipValue").and_then(num) { f.opacity_mask_clip = v as f32; }
    if let Some(v) = s("TranslucencyLightingMode") { f.translucency_lighting_mode = v; }
    if let Some(v) = props.get("bEnableSeparateTranslucency").and_then(|v| v.as_bool()) { f.separate_translucency = v; }
    if let Some(v) = props.get("bEnableResponsiveAA").and_then(|v| v.as_bool()) { f.responsive_aa = v; } // STORM TAA
    if let Some(v) = s("MaterialDomain") { f.domain = v; }
    f
}

/// `UMaterialInstance::UpdateOverridableBaseProperties` (UE4.16 MaterialInstance.cpp:1550): a property is the instance's `BasePropertyOverrides` value only when its
/// `bOverride_*` flag is set, else the parent's. A cooked override stores no value that equals the struct default, so a set flag without a value is the
/// `FMaterialInstanceBasePropertyOverrides` constructor default (MaterialShared.cpp:2743: clip 0.333333, `BLEND_Opaque`, `MSM_DefaultLit`, one-sided).
/// MIRRORFIX (2026-10-05): the value was taken whenever present and the flag ignored, so `Wood_lvl1_Static` (`bOverride_BlendMode`, no value = opaque) inherited
/// its parent's `BLEND_Masked` and asked its opaque shader map for an `FDepthOnlyPS` it does not have.
fn instance_flags(o: &Value, parent: &MaterialFlags) -> MaterialFlags {
    let mut f = parent.clone();
    let on = |k: &str| o.get(k).and_then(|v| v.as_bool()) == Some(true);
    let enum_of = |k: &str, default: &str| o.get(k).and_then(|v| v.as_str()).map(|s| s.rsplit("::").next().unwrap_or(s).to_owned()).unwrap_or_else(|| default.to_owned());
    if on("bOverride_OpacityMaskClipValue") { f.opacity_mask_clip = o.get("OpacityMaskClipValue").and_then(num).map_or(0.333_333, |v| v as f32); }
    if on("bOverride_BlendMode") { f.blend = enum_of("BlendMode", "BLEND_Opaque"); }
    if on("bOverride_ShadingModel") { f.shading = enum_of("ShadingModel", "MSM_DefaultLit"); }
    if on("bOverride_TwoSided") { f.two_sided = o.get("TwoSided").and_then(|v| v.as_bool().or_else(|| v.as_u64().map(|n| n != 0))).unwrap_or(false); }
    f
}

/// `UMaterialExpression::GetReferencedTexture` is non-null for `UMaterialExpressionTextureBase` subclasses (UE4.16 Classes/Materials: TextureSample*, TextureObject,
/// TextureObjectParameter, ParticleSubUV, AntialiasedTextureMask); `FontSample` (font pages) is not handled (no weapon / effect material of this port uses one).
fn texture_expression(class: &str) -> bool {
    class.starts_with("MaterialExpressionTextureSample") || class == "MaterialExpressionTextureObject" || class == "MaterialExpressionTextureObjectParameter"
        || class == "MaterialExpressionParticleSubUV" || class == "MaterialExpressionAntialiasedTextureMask"
        || landscape_weightmap_expression(class)
}
/// LANDSCAPE MATERIAL (2026-10-07): the landscape layer expressions return `GEngine->WeightMapPlaceholderTexture` from `GetReferencedTexture` (UE4.16
/// MaterialExpressionLandscapeLayerBlend.cpp:288, LayerSample.cpp:58, LayerSwitch.cpp:80, LayerWeight.cpp:96, VisibilityMask.cpp:51; BaseEngine.ini
/// `WeightMapPlaceholderTextureName`), so the placeholder is in `ExpressionTextureReferences` at their position: the `Weightmap0..` texture parameters index it.
/// MEASURED: without it every M_Athena_Terrain_Landscape texture index past the first layer expression resolved one texture early (grass albedo = DefaultDiffuse).
fn landscape_weightmap_expression(class: &str) -> bool {
    matches!(class, "MaterialExpressionLandscapeLayerBlend" | "MaterialExpressionLandscapeLayerSample" | "MaterialExpressionLandscapeLayerSwitch" | "MaterialExpressionLandscapeLayerWeight" | "MaterialExpressionLandscapeVisibilityMask")
}
const WEIGHTMAP_PLACEHOLDER: &str = "/Engine/EngineMaterials/WeightMapPlaceholderTexture.WeightMapPlaceholderTexture";
/// GRASS (2026-10-06): the `Texture` a texture expression has when its export does not serialize one: the class default object's, i.e. what the constructor
/// sets (a property equal to the archetype is not saved). UE4.16 MaterialExpressions.cpp: `TextureObjectParameter` (:1781), `TextureObject` (:1856),
/// `TextureSampleParameter2D` (:2029, inherited by `TextureSampleParameterSubUV`) and `AntialiasedTextureMask` (:11127) load `/Engine/EngineResources/DefaultTexture`,
/// `TextureSampleParameterCube` (:2123) `DefaultTextureCube`; `TextureSample` / `ParticleSubUV` have none. MEASURED: `Fortnite_SampleRealTimeWindData`'s
/// `MaterialExpressionTextureObject_6/7/8` serialize no `Texture`; without their DefaultTexture the grass `M_FieldGrassMatching` list was one short, so its
/// `Texture2D_0` (index 6) resolved to T_Grasslands_D instead of the blade normal T_WavyGrass_01_N and `GrassTextureToMatch` (index 7) to nothing.
fn default_texture(class: &str) -> Option<&'static str> {
    match class {
        "MaterialExpressionTextureObject" | "MaterialExpressionTextureObjectParameter" | "MaterialExpressionTextureSampleParameter2D"
        | "MaterialExpressionTextureSampleParameterSubUV" | "MaterialExpressionAntialiasedTextureMask" => Some("/Engine/EngineResources/DefaultTexture.DefaultTexture"),
        "MaterialExpressionTextureSampleParameterCube" => Some("/Engine/EngineResources/DefaultTextureCube.DefaultTextureCube"),
        _ => None,
    }
}
/// `UMaterialExpression::GetReferencedTexture` of a cooked texture expression: its serialized `Texture` (an explicit `None` stays none), else the class default.
fn expression_texture(class: &str, props: &Value) -> Option<String> {
    if landscape_weightmap_expression(class) { return Some(WEIGHTMAP_PLACEHOLDER.to_owned()); }
    match props.get("Texture") { Some(v) => obj_path(v), None => default_texture(class).map(str::to_owned) }
}
fn function_expressions(set: &PakSet, function: &str) -> Result<(PackageJson, Vec<Value>), String> {
    let fp = PackageJson::load(set, long_package(function))?;
    let list = fp.exports.iter().find(|x| x.class == "MaterialFunction").and_then(|fe| fe.props.get("FunctionExpressions")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    Ok((fp, list))
}
/// `UMaterialFunction::GetDependentFunctions` (UE4.16 MaterialExpressions.cpp:8791): post-order, nested calls first, `AddUnique`.
fn dependent_functions(set: &PakSet, function: &str, out: &mut Vec<String>, depth: usize) -> Result<(), String> {
    if depth > 16 { return Err("material function nesting".into()); }
    let (fp, list) = function_expressions(set, function)?;
    for e in &list {
        let Some(ex) = obj_path(e).and_then(|p| fp.get(&p)) else { continue };
        if ex.class != "MaterialExpressionMaterialFunctionCall" { continue }
        if let Some(g) = ex.props.get("MaterialFunction").and_then(obj_path) {
            dependent_functions(set, &g, out, depth + 1)?;
            if !out.contains(&g) { out.push(g); }
        }
    }
    Ok(())
}

/// Textures of a material in `UMaterial::ReferencedTextures` order = UE4.16 `UMaterial::AppendReferencedTextures` (Material.cpp:4152): for each entry of `Expressions`, a
/// function call appends the textures of `[F] + F.GetDependentFunctions()` in that order, each function contributing only its OWN `FunctionExpressions` textures
/// (`UMaterialFunction::AppendReferencedTextures`, no recursion); any other texture expression appends its texture; duplicates removed (`AddUnique`).
/// (A depth-first walk that inlines nested function textures at their call site gives a different order: for `WeaponMat_Base` it shifted the `Normals` /
/// `Diffuse` / `SpecularMasks` / `EmissiveTexture` defaults and made the default `EmissiveTexture` (T_Black, index 9) resolve to `ShieldPowerAnim`.)
fn collect_textures(set: &PakSet, pkg: &PackageJson, expressions: &[Value], out: &mut Vec<String>, functions: &mut Vec<String>, _depth: usize) -> Result<(), String> {
    let add = |t: String, out: &mut Vec<String>| { if !out.contains(&t) { out.push(t); } };
    for e in expressions {
        let Some(path) = obj_path(e) else { continue };
        let Some(ex) = pkg.get(&path) else { continue };
        if ex.class == "MaterialExpressionMaterialFunctionCall" {
            let Some(f) = ex.props.get("MaterialFunction").and_then(obj_path) else { continue };
            let mut list = vec![f.clone()];
            dependent_functions(set, &f, &mut list, 0)?;
            for g in &list {
                let long = long_package(g).to_owned();
                if !functions.contains(&long) { functions.push(long); }
                let (fp, fl) = function_expressions(set, g)?;
                for fe in &fl {
                    let Some(x) = obj_path(fe).and_then(|p| fp.get(&p)) else { continue };
                    if texture_expression(&x.class) { if let Some(t) = expression_texture(&x.class, &x.props) { add(t, out); } }
                }
            }
        } else if texture_expression(&ex.class) {
            if let Some(t) = expression_texture(&ex.class, &ex.props) { add(t, out); }
        }
    }
    Ok(())
}

static PRELOADED: std::sync::Mutex<Option<HashMap<String, Material>>> = std::sync::Mutex::new(None);
static PRELOAD_QUEUE: std::sync::Mutex<std::collections::VecDeque<String>> = std::sync::Mutex::new(std::collections::VecDeque::new());
pub static IS_MAIN_THREAD: std::sync::OnceLock<fn() -> bool> = std::sync::OnceLock::new();
pub fn preload(install: std::path::PathBuf, paths: Vec<String>, threads: usize) {
    if let Ok(mut q) = PRELOAD_QUEUE.lock() { q.extend(paths); }
    for _ in 0..threads.max(1) {
        let dir = install.clone();
        let _ = std::thread::Builder::new().name("material preload".into()).spawn(move || {
            let Ok(set) = PakSet::open_install(&dir) else { return };
            loop {
                let Some(p) = PRELOAD_QUEUE.lock().ok().and_then(|mut q| q.pop_front()) else { break };
                if PRELOADED.lock().map_or(true, |g| g.as_ref().map_or(false, |m| m.contains_key(&p))) { continue; }
                if let Ok(m) = Material::load_uncached(&set, &p) { if let Ok(mut g) = PRELOADED.lock() { g.get_or_insert_with(HashMap::new).insert(p, m); } }
            }
        });
    }
}

impl Material {
    /// Loads `object_path` (`/Game/.../Name.Name`, a `Material` or `MaterialInstanceConstant`) and everything above it.
    pub fn load(set: &PakSet, object_path: &str) -> Result<Material, String> {
        if let Some(m) = PRELOADED.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(object_path).cloned())) { return Ok(m); }
        let main = IS_MAIN_THREAD.get().map_or(false, |f| f());
        if main { if let Ok(mut q) = PRELOAD_QUEUE.lock() { q.retain(|p| p != object_path); } }
        let t = std::time::Instant::now();
        let r = Material::load_uncached(set, object_path);
        if main { eprintln!("[matrec] {:.1} {object_path}", t.elapsed().as_secs_f32() * 1000.); }
        if main { if let (Ok(m), Ok(mut g)) = (r.as_ref(), PRELOADED.lock()) { g.get_or_insert_with(HashMap::new).insert(object_path.to_owned(), m.clone()); } }
        r
    }
    fn load_uncached(set: &PakSet, object_path: &str) -> Result<Material, String> {
        let t0 = std::time::Instant::now(); // LOAD TIME probe (FN_FX_TIMING=1)
        let mut chain: Vec<(String, Arc<PackageJson>)> = vec![];
        let mut cur = object_path.to_owned();
        let (root_path, root_pkg) = loop {
            if chain.len() > 16 { return Err(format!("{object_path}: material parent chain too deep")); }
            let long = long_package(&cur).to_owned();
            let pkg = map_package_json(set, &long)?; // LANDSCAPE MATERIAL: a map package is parsed once (its instances chain into it)
            let name = cur.rsplit('.').next().unwrap_or(&cur).to_owned();
            let ex = pkg.by_name(&name).ok_or_else(|| format!("{cur}: export {name} missing"))?;
            match ex.class.as_str() {
                "Material" => break (cur, pkg),
                // LANDSCAPE MATERIAL: `ULandscapeMaterialInstanceConstant` is a `UMaterialInstanceConstant` (LandscapeMaterialInstanceConstant.h)
                "MaterialInstanceConstant" | "LandscapeMaterialInstanceConstant" => {
                    let parent = ex.props.get("Parent").and_then(obj_path).ok_or_else(|| format!("{cur}: instance without parent"))?;
                    chain.push((cur.clone(), pkg));
                    cur = parent;
                }
                other => return Err(format!("{cur}: unsupported material class {other}")),
            }
        };
        let t_chain = t0.elapsed();
        let root_name = root_path.rsplit('.').next().unwrap().to_owned();
        let root_ex = root_pkg.by_name(&root_name).unwrap();
        let mut flags = flags_from(&root_ex.props, None);
        let mut overrides = Overrides::default();
        let mut texture_overrides = HashMap::new();
        // parent-most instance first so that children override
        for (path, pkg) in chain.iter().rev() {
            let name = path.rsplit('.').next().unwrap();
            let ex = pkg.by_name(name).unwrap();
            if let Some(o) = ex.props.get("BasePropertyOverrides") { flags = instance_flags(o, &flags); }
            if let Some(a) = ex.props.get("ScalarParameterValues").and_then(|v| v.as_array()) {
                for p in a { if let (Some(n), Some(v)) = (p.get("ParameterName").and_then(|v| v.as_str()), p.get("ParameterValue").and_then(num)) { overrides.scalars.insert(n.to_owned(), v as f32); } }
            }
            if let Some(a) = ex.props.get("VectorParameterValues").and_then(|v| v.as_array()) {
                for p in a {
                    let (Some(n), Some(v)) = (p.get("ParameterName").and_then(|v| v.as_str()), p.get("ParameterValue").and_then(|v| v.as_array())) else { continue };
                    let c: Vec<f32> = v.iter().map(|x| x.as_f64().unwrap_or(0.) as f32).collect();
                    if c.len() == 4 { overrides.vectors.insert(n.to_owned(), [c[0], c[1], c[2], c[3]]); }
                }
            }
            if let Some(a) = ex.props.get("TextureParameterValues").and_then(|v| v.as_array()) {
                for p in a { if let (Some(n), Some(t)) = (p.get("ParameterName").and_then(|v| v.as_str()), p.get("ParameterValue").and_then(obj_path)) { texture_overrides.insert(n.to_owned(), t); } }
            }
        }
        // shader programs and uniform expression set of the material RESOURCE: `UMaterialInstance::GetMaterialResource` (UE4.16 MaterialInstance.cpp:1362) returns the
        // instance's own `StaticPermutationMaterialResources` when it has one (`bHasStaticPermutationResource`: a static-switch / base-property-override permutation
        // that cooked its own shader map, e.g. `Wood_lvl1_Static`), else the parent's: the child-most package of the chain that carries programs, else the root.
        // (MIRRORFIX 2026-10-05: this took the root's map for every instance, so `Wood_lvl1_Static` ran `Builder_2`'s programs with masked flags.)
        let mut cooked_src: Option<(String, Arc<Cooked>, UniformSet)> = None;
        for (path, _) in &chain {
            let long = long_package(path).to_owned();
            // LANDSCAPE MATERIAL: an instance inside a map package owns its programs per export (shader_map_scoped)
            if is_map_package(set, &long) {
                let name = path.rsplit('.').next().unwrap_or(path);
                if let Some((c, u)) = shader_map_scoped(set, &long, name)? { cooked_src = Some((long, c, u?)); break; }
                continue;
            }
            let (c, u) = shader_map(set, &long)?;
            if c.programs.is_empty() { continue; }
            cooked_src = Some((long, c, u?));
            break;
        }
        let (shader_map_package, cooked, uniform) = match cooked_src {
            Some((l, c, u)) => (l, c, u),
            None => {
                let root_long = long_package(&root_path).to_owned();
                let (c, u) = shader_map(set, &root_long)?;
                (root_long, c, u?)
            }
        };
        let t_cooked = t0.elapsed();
        let exprs: Vec<Value> = root_ex.props.get("Expressions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        // LOAD TIME (2026-10-06): a function of the root material alone (its expressions and their material functions), so every instance of one root
        // reuses the first walk: the three firearms + the pickaxe share WeaponMat_Base's 46 functions (83-376 ms per walk)
        let cached = ROOT_TEXTURES.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&root_path).cloned()));
        let (referenced_textures, functions) = match cached {
            Some(v) => v,
            None => {
                let (mut rt, mut f) = (vec![], vec![]);
                collect_textures(set, &root_pkg, &exprs, &mut rt, &mut f, 0)?;
                if let Ok(mut g) = ROOT_TEXTURES.lock() { g.get_or_insert_with(HashMap::new).insert(root_path.clone(), (rt.clone(), f.clone())); }
                (rt, f)
            }
        };
        let t_textures = t0.elapsed();
        // MATERIAL PARAMETERS: MaterialCollectionN = uniform.parameter_collections[N] (a collection StateId) -> the root's MaterialParameterCollectionInfos entry with
        // that StateId; a StateId the root does not list is looked up among the collections the shader-map package imports
        let infos: Vec<(String, String)> = root_ex.props.get("MaterialParameterCollectionInfos").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|i| Some((i.get("StateId")?.as_str()?.to_owned(), obj_path(i.get("ParameterCollection")?)?))).collect()).unwrap_or_default();
        let mut imported: Option<Vec<crate::mpc::Collection>> = None;
        let collections = uniform.parameter_collections.iter().map(|id| {
            if let Some((_, p)) = infos.iter().find(|(s, _)| s == id) { return Some(p.clone()); }
            let list = imported.get_or_insert_with(|| fn_uasset::PackageData::load(set, &shader_map_package).ok().and_then(|pd| pd.package().ok().map(|p| p.imports.iter().enumerate().filter(|(_, i)| i.class_name == "MaterialParameterCollection").map(|(k, _)| p.object_path(fn_uasset::PackageIndex(-(k as i32) - 1))).collect::<Vec<String>>())).unwrap_or_default().iter().filter_map(|p| crate::mpc::Collection::load(set, p).ok()).collect());
            list.iter().find(|c| &c.state_id == id).map(|c| c.path.clone())
        }).collect();
        if fn_live::var_os("FN_FX_TIMING").is_some() { let ms = |d: std::time::Duration| d.as_secs_f32() * 1000.; eprintln!("[fx-timing] Material::load {}: chain {:.1} ms, cooked {:.1} ms, textures/functions {:.1} ms ({} functions), collections {:.1} ms, total {:.1} ms", object_path.rsplit('.').next().unwrap_or(object_path), ms(t_chain), ms(t_cooked - t_chain), ms(t_textures - t_cooked), functions.len(), ms(t0.elapsed() - t_textures), ms(t0.elapsed())); }
        let mut overrides = overrides; texture_properties(set, &uniform, &texture_overrides, &referenced_textures, &mut overrides); // FOOT FX
        Ok(Material { path: object_path.to_owned(), base_path: root_path, shader_map_package, flags, overrides, texture_overrides, cooked, uniform, referenced_textures, functions, collections })
    }

    /// The packed material constant buffer (`cb[i]` of the `Material` uniform buffer) at `time`.
    pub fn packed(&self, time: f32) -> Packed { evaluate(&self.uniform, &self.overrides, time) }

    /// The object path of 2D texture slot `k` of the material uniform buffer (`Texture2D_k`): the instance's texture parameter when the expression is a parameter
    /// that is overridden, else the referenced texture the expression indexes.
    pub fn texture_slot(&self, k: usize) -> Option<(String, i32)> {
        match self.uniform.textures2d.get(k)? {
            Expr::Texture { index, sampler_source } => self.referenced_textures.get(*index as usize).map(|t| (t.clone(), *sampler_source)),
            Expr::TextureParameter { name, index, sampler_source } => self.texture_overrides.get(name).cloned().or_else(|| self.referenced_textures.get(*index as usize).cloned()).map(|t| (t, *sampler_source)),
            _ => None,
        }
    }
    /// STATIC ITEM (2026-10-05): 2D texture slot `k` resolves to NULL in the engine: a texture (parameter) expression that is not overridden and whose `TextureIndex`
    /// is outside the material's `ExpressionTextureReferences` (`GetIndexedTexture` returns NULL with a one-time warning, UE4.16 MaterialUniformExpressions.h:268-286),
    /// which `FillUniformBuffer` replaces by `GWhiteTexture` (MaterialUniformExpressions.cpp:394-399). (GRASS 2026-10-06: the case once attributed to
    /// `MI_MedKit_Inst` / `MI_FORT_Wood_Drop` was a miscount: `BackPackShrinkingEffect`'s `SphereReflectionMap` serializes no `Texture`, so it holds the class
    /// default DefaultTexture (index 8) and `EmissiveTexture` (index 9) is its own default T_Black, see [`default_texture`]. No 1.8 material is known to hit it.)
    pub fn texture_slot_is_null(&self, k: usize) -> bool {
        match self.uniform.textures2d.get(k) {
            Some(Expr::Texture { index, .. }) => self.referenced_textures.get(*index as usize).is_none(),
            Some(Expr::TextureParameter { name, index, .. }) => !self.texture_overrides.contains_key(name) && self.referenced_textures.get(*index as usize).is_none(),
            _ => false,
        }
    }
}

/// FOOT FX (2026-10-07): the `TextureProperty` uniform expressions of `set` (e.g. the texel size of `M_SmokeMaster_Masked_01`'s `Morph Targets Texture`): the size of
/// the texture each one reads (`Texture->Resource->GetSizeX/Y()`, the cooked `FTexturePlatformData` size), stored in `ov` under `uniform::texture_property_key`.
#[cfg(feature = "render")]
fn webtex_size(path: &str) -> Option<(f32, f32)> { crate::webtex::source_size(path) }
#[cfg(not(feature = "render"))]
fn webtex_size(_path: &str) -> Option<(f32, f32)> { None }

fn texture_properties(set: &PakSet, u: &crate::uniform::UniformSet, tex_ov: &HashMap<String, String>, referenced: &[String], ov: &mut Overrides) {
    fn walk<'a>(e: &'a Expr, out: &mut Vec<(&'a Expr, i8)>) {
        match e {
            Expr::TextureProperty { tex, property } => out.push((tex, *property)),
            Expr::ComponentSwizzle { e, .. } | Expr::Unary(_, e) => walk(e, out),
            Expr::FoldedMath { a, b, .. } | Expr::AppendVector { a, b, .. } | Expr::Max(a, b) | Expr::Min(a, b) | Expr::Fmod(a, b) => { walk(a, out); walk(b, out) }
            Expr::TrigMath { x, y, .. } => { walk(x, out); walk(y, out) }
            Expr::Clamp { x, lo, hi } => { walk(x, out); walk(lo, out); walk(hi, out) }
            _ => {}
        }
    }
    let mut found = vec![];
    for e in u.vectors.iter().chain(&u.scalars).chain(&u.per_frame_scalars).chain(&u.per_frame_vectors).chain(&u.per_frame_prev_scalars).chain(&u.per_frame_prev_vectors) { walk(e, &mut found); }
    for (tex, property) in found {
        let Some(key) = crate::uniform::texture_property_key(tex, property) else { continue };
        let path = match tex { Expr::Texture { index, .. } => referenced.get(*index as usize).cloned(), Expr::TextureParameter { name, index, .. } => tex_ov.get(name).cloned().or_else(|| referenced.get(*index as usize).cloned()), _ => None };
        let Some(path) = path else { continue };
        let long = crate::data::long_package(&path);
        let size = webtex_size(&path).or_else(|| crate::data::package_files(set, long).ok().and_then(|(a, x, b)| fn_ueio::Package::parse(&a, &x, &b).ok()).and_then(|p| fn_texture::Texture2D::parse(&p).ok()).map(|t| (t.size_x as f32, t.size_y as f32)));
        let Some((w, h)) = size.filter(|s| s.0 > 0. && s.1 > 0.) else { eprintln!("[fx] TextureProperty: size of {path} unknown (0 used)"); continue };
        ov.vectors.insert(key, if property == 1 { [1. / w, 1. / h, 0., 0.] } else { [w, h, 0., 0.] });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// GRASS (2026-10-06): a texture expression that serializes no `Texture` holds its class default (`UMaterialExpression::GetReferencedTexture` of the CDO value).
    /// `BackPackShrinkingEffect` (the pickup materials' root): `SphereReflectionMap` (TextureSampleParameter2D_2) has none -> DefaultTexture at index 8, so the
    /// list has 10 references and `EmissiveTexture` (index 9) is its parameter default T_Black (TextureObjectParameter_4), not a NULL / GWhiteTexture slot.
    /// The grass `M_FieldGrassMatching`: `Fortnite_SampleRealTimeWindData`'s TextureObject_6/7/8 add DefaultTexture at 5, so `Texture2D_0` (index 6) is the blade
    /// normal T_WavyGrass_01_N and `GrassTextureToMatch` (index 7) T_Grasslands_D, as its cooked programs sample them (normal at UV0, colour at world XY / 800).
    #[test] fn unserialized_texture_takes_the_class_default() {
        let Ok(set) = PakSet::open_install(crate::default_install()) else { eprintln!("no install: skipped"); return };
        let m = Material::load(&set, "/Game/Items/Art_noLOD/Materials/MI_MedKit_Inst.MI_MedKit_Inst").unwrap();
        assert_eq!(m.referenced_textures.len(), 10, "{:?}", m.referenced_textures);
        assert_eq!(m.referenced_textures[8], "/Engine/EngineResources/DefaultTexture.DefaultTexture");
        let k = m.uniform.textures2d.iter().position(|e| matches!(e, Expr::TextureParameter { name, .. } if name == "EmissiveTexture")).unwrap();
        assert_eq!(m.texture_slot(k).map(|t| t.0), Some("/Game/Weapons/FORT_TRAPS/General/T_Black.T_Black".to_owned()));
        assert!((0..m.uniform.textures2d.len()).all(|j| m.texture_slot(j).is_some() && !m.texture_slot_is_null(j)));
        let grass = Material::load(&set, "/Game/Environments/Foliage/Materials/M_FieldGrassMatching.M_FieldGrassMatching").unwrap();
        let slots: Vec<String> = (0..grass.uniform.textures2d.len()).map(|j| grass.texture_slot(j).map(|t| t.0.rsplit('.').next().unwrap_or("").to_owned()).unwrap_or_default()).collect();
        assert_eq!(slots, vec!["T_WavyGrass_01_N", "T_Grasslands_D", "T_WavyGrass_01", "T_FieldGrass_01_M", "T_SwirlNoise_N"]);
    }
    /// MIRRORFIX: `Wood_lvl1_Static` sets `bOverride_BlendMode` without a value (= `BLEND_Opaque`) over a masked parent and cooked its own (opaque) shader map, which
    /// has no `FDepthOnlyPS`; `Wood_lvl1_Wall` overrides to `BLEND_Masked` and its own map has one (the masked depth-only permutation).
    #[test] fn instance_overrides_and_static_permutation_map() {
        let Ok(set) = PakSet::open_install(crate::default_install()) else { eprintln!("no install: skipped"); return };
        let st = Material::load(&set, "/Game/Packages/PBW/Wood/Materials/Wood_lvl1_Static.Wood_lvl1_Static").unwrap();
        assert_eq!(st.flags.blend, "BLEND_Opaque");
        assert!(st.cooked.find("FLocalVertexFactory", "TBasePassPSFNoLightMapPolicy").is_some());
        assert!(st.cooked.find("FLocalVertexFactory", "FDepthOnlyPS").is_none(), "an opaque permutation has no masked depth-only PS");
        let wall = Material::load(&set, "/Game/Packages/PBW/Wood/Materials/Wood_lvl1_Wall.Wood_lvl1_Wall").unwrap();
        assert_eq!(wall.flags.blend, "BLEND_Masked");
        assert!(wall.cooked.find("FLocalVertexFactory", "FDepthOnlyPS").is_some());
    }
    /// PP2 FIX (water agent, 2026-10-07): the Lonely Lodge PivotPainter2 instances' uniform set has no vector and no scalar expression (`0, 0, 6 textures`): the
    /// six texture expressions are the Texture2D slots (was: read as the scalars, "texture slot 3 unresolved" -> NOT BUILT); the stream water keeps its lists.
    #[test] fn textures_only_uniform_set() {
        let Ok(set) = PakSet::open_install(crate::default_install()) else { eprintln!("no install: skipped"); return };
        let m = Material::load(&set, "/Game/Environments/Props/Spiritual/Materials/PP2/MI_Crypt_CobWeb_01_Ambient.MI_Crypt_CobWeb_01_Ambient").unwrap();
        assert!(m.shader_map_package.ends_with("/MI_Crypt_CobWeb_01_Storm"), "{}", m.shader_map_package);
        assert_eq!((m.uniform.vectors.len(), m.uniform.scalars.len()), (10, 14), "{:?}", m.uniform.scalars);
        assert!(m.uniform.scalars.iter().any(|e| matches!(e, crate::uniform::Expr::ComponentSwizzle { .. })));
        assert_eq!(m.uniform.textures2d.len(), 6);
        assert!((0..6).all(|k| m.texture_slot(k).is_some()));
        let w = Material::load(&set, "/Game/WaterAndWind/Materials/Water/Stream_Flowmaps_01_StreamToLake.Stream_Flowmaps_01_StreamToLake").unwrap();
        assert_eq!((w.uniform.textures2d.len(), w.flags.disable_depth_test), (11, true));
    }
}

