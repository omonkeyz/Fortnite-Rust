//! One original gun material instance resolved through its parent chain: effective parameter values (scalar / vector / texture), the textures its uniform expressions
//! reference, the serialized render flags and the shader maps of the root material.
//!
//! Parameter chain and referenced-texture order come from `fn_fx::material::Material` (instance overrides over parent overrides, `UMaterial::ReferencedTextures` rebuilt
//! from the root's `Expressions` incl. material function calls). The render FLAGS are resolved here: `BasePropertyOverrides` apply only when their `bOverride_*` flag is set
//! (a stored `TwoSided = true` without `bOverride_TwoSided` is leftover data, as in `MI_FORT_Rifles_MachineGun_1`).
use crate::maps::{MaterialMaps, ShaderMap};
use fn_dxbc_cli::uniform::{evaluate, Packed};
use fn_fx::data::{long_package, package_files, PackageJson};
use fn_pak::PakSet;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Debug, PartialEq)]
pub struct Flags { pub blend: String, pub shading: String, pub two_sided: bool, pub opacity_mask_clip: f32 }

fn s(v: &Value, k: &str) -> Option<String> { v.get(k).and_then(|x| x.as_str()).map(|x| x.rsplit("::").next().unwrap_or(x).to_owned()) }
fn b(v: &Value, k: &str) -> Option<bool> { v.get(k).and_then(|x| x.as_bool()) }

/// Render flags of `object_path` (a `Material` or a `MaterialInstanceConstant` chain).
pub fn resolve_flags(set: &PakSet, object_path: &str) -> Result<Flags, String> {
    let mut chain = vec![];
    let mut cur = object_path.to_owned();
    let root = loop {
        if chain.len() > 16 { return Err(format!("{object_path}: parent chain too deep")); }
        let pkg = PackageJson::load(set, long_package(&cur))?;
        let name = cur.rsplit('.').next().unwrap().to_owned();
        let ex = pkg.by_name(&name).ok_or_else(|| format!("{cur}: export {name} missing"))?;
        match ex.class.as_str() {
            "Material" => break ex.props.clone(),
            "MaterialInstanceConstant" => {
                let parent = ex.props.get("Parent").and_then(|v| v.as_str()).filter(|p| !p.is_empty()).ok_or_else(|| format!("{cur}: instance without parent"))?.to_owned();
                chain.push(ex.props.clone());
                cur = parent;
            }
            other => return Err(format!("{cur}: unsupported material class {other}")),
        }
    };
    let mut f = Flags {
        blend: s(&root, "BlendMode").unwrap_or_else(|| "BLEND_Opaque".into()),
        shading: s(&root, "ShadingModel").unwrap_or_else(|| "MSM_DefaultLit".into()),
        two_sided: b(&root, "TwoSided").unwrap_or(false),
        opacity_mask_clip: root.get("OpacityMaskClipValue").and_then(|x| x.as_f64()).unwrap_or(0.3333) as f32,
    };
    for props in chain.iter().rev() {
        let Some(o) = props.get("BasePropertyOverrides") else { continue };
        if b(o, "bOverride_BlendMode") == Some(true) { if let Some(v) = s(o, "BlendMode") { f.blend = v; } }
        if b(o, "bOverride_ShadingModel") == Some(true) { if let Some(v) = s(o, "ShadingModel") { f.shading = v; } }
        if b(o, "bOverride_TwoSided") == Some(true) { if let Some(v) = b(o, "TwoSided") { f.two_sided = v; } }
        if b(o, "bOverride_OpacityMaskClipValue") == Some(true) { if let Some(v) = o.get("OpacityMaskClipValue").and_then(|x| x.as_f64()) { f.opacity_mask_clip = v as f32; } }
    }
    Ok(f)
}

pub struct ResolvedMaterial {
    pub path: String,
    pub root_path: String,
    pub flags: Flags,
    pub fx: fn_fx::material::Material,
    /// The root material's shader maps, shared by every instance of that root (see [`root_maps`]).
    pub maps: Arc<MaterialMaps>,
    /// The SM5 map the game runs (see [`MaterialMaps::sm5_map`]).
    pub map_index: usize,
}

/// FIRST USE (2026-10-06): one parse of a root material's shader maps per root while any instance holds it. Every gun / pickup material instance of
/// `WeaponMat_Base` used to re-read and re-parse the root package and all its programs (the bulk of `ResolvedMaterial::load`, ~1.9 s on the browser's
/// main thread for the starter guns).
static ROOT_MAPS: Mutex<Option<HashMap<String, Weak<MaterialMaps>>>> = Mutex::new(None);

fn root_maps(set: &PakSet, root_path: &str) -> Result<Arc<MaterialMaps>, String> {
    if let Some(m) = ROOT_MAPS.lock().ok().and_then(|g| g.as_ref().and_then(|c| c.get(root_path).and_then(Weak::upgrade))) { return Ok(m); }
    let root_long = long_package(root_path).to_owned();
    let (asset, uexp, ubulk) = package_files(set, &root_long)?;
    let pkg = fn_ueio::Package::parse(&asset, &uexp, &ubulk).map_err(|e| format!("{root_long}: {e}"))?;
    let label = root_path.rsplit('.').next().unwrap_or("").to_owned();
    let maps = Arc::new(MaterialMaps::parse(&uexp, &pkg.names, pkg.summary.total_header_size as usize, &label)?);
    if let Ok(mut g) = ROOT_MAPS.lock() { g.get_or_insert_with(HashMap::new).insert(root_path.to_owned(), Arc::downgrade(&maps)); }
    Ok(maps)
}

impl ResolvedMaterial {
    pub fn load(set: &PakSet, object_path: &str) -> Result<ResolvedMaterial, String> {
        let flags = resolve_flags(set, object_path)?;
        let fx = fn_fx::material::Material::load(set, object_path)?;
        let maps = root_maps(set, &fx.base_path)?;
        let map_index = maps.sm5_map()?.index;
        Ok(ResolvedMaterial { path: object_path.to_owned(), root_path: fx.base_path.clone(), flags, fx, maps, map_index })
    }
    pub fn map(&self) -> &ShaderMap { &self.maps.maps[self.map_index] }
    /// The packed Material uniform buffer at `time` (the chosen shader map's uniform expression set with this instance's effective parameters).
    pub fn packed(&self, time: f32) -> Packed { self.packed_with(time, &[]) }
    pub fn cb_into(&self, time: f32, vectors: &[(&str, [f32; 4])], out: &mut Vec<[f32; 4]>) {
        let (ov_v, ov_s) = (&self.fx.overrides.vectors, &self.fx.overrides.scalars);
        let v = |n: &str| vectors.iter().rev().find(|(k, _)| *k == n).map(|(_, x)| *x).or_else(|| ov_v.get(n).copied());
        let sc = |n: &str| ov_s.get(n).copied();
        fn_dxbc_cli::uniform::evaluate_cb_by(&self.map().uniform, &v, &sc, time, out)
    }
    pub fn packed_with(&self, time: f32, vectors: &[(&str, [f32; 4])]) -> Packed {
        let mut ov = fn_dxbc_cli::uniform::Overrides { vectors: self.fx.overrides.vectors.clone(), scalars: self.fx.overrides.scalars.clone() };
        for (k, v) in vectors { ov.vectors.insert((*k).to_owned(), *v); }
        evaluate(&self.map().uniform, &ov, time)
    }
    /// Object path + sampler source of 2D texture expression `k` (see `fn_fx::material::Material::texture_slot`).
    pub fn texture_slot(&self, k: usize) -> Option<(String, i32)> { self.fx.texture_slot(k) }
}
