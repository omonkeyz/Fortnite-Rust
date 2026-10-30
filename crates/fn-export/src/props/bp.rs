//! Blueprint class resolution for building-prop actors: walks the `Foo_C` -> `Parent_C` chain through the pak set and
//! merges the class default objects (child overrides parent) plus the root `StaticMeshComponent` template.
use fn_pak::PakSet;
use fn_uasset::{ObjectRef, PackageData};
use serde_json::{Map, Value};

/// One layer of the class chain.
#[derive(Debug, Clone)]
pub struct Layer {
    /// `/Game/...` package of the Blueprint (or the native class path for the last, native layer).
    pub package: String,
    pub class: String,
}

#[derive(Debug, Clone)]
pub struct Bp {
    pub package: String,
    /// leaf first; last entry is the first native (`/Script/...`) class.
    pub chain: Vec<Layer>,
    /// merged CDO properties (JSON, object refs as paths)
    pub cdo: Map<String, Value>,
    /// merged properties of the root StaticMeshComponent template (name given by the CDO's RootComponent)
    pub smc: Map<String, Value>,
    pub smc_name: String,
    /// POI: every component template of the chain (SCS `<Name>_GEN_VARIABLE` / inheritable-component overrides and the CDO's native subobjects), keyed by the
    /// instance component name (`_GEN_VARIABLE` stripped), merged root class first: the archetype a placed actor's component is delta-serialized against.
    pub templates: Map<String, Value>,
}

pub fn merge(dst: &mut Map<String, Value>, src: &Map<String, Value>) {
    for (k, v) in src {
        // struct properties are serialised delta-style (only the fields that differ from the parent), so merge
        // them field by field
        if let (Some(Value::Object(d)), Value::Object(s)) = (dst.get_mut(k), v) {
            if s.contains_key("_struct") {
                merge(d, s);
                continue;
            }
        }
        dst.insert(k.clone(), v.clone());
    }
}

/// Last path segment after `:` or `.`.
pub fn leaf(path: &str) -> &str {
    path.rsplit(|c| c == ':' || c == '.').next().unwrap_or(path)
}

/// Package name (`/Game/A/B`) of an object path (`/Game/A/B.B_C`).
pub fn package_of(path: &str) -> &str {
    path.split('.').next().unwrap_or(path)
}

pub fn load_bp(set: &PakSet, package: &str) -> Result<Bp, String> {
    let mut chain = vec![];
    let mut cdo_layers: Vec<Map<String, Value>> = vec![];
    let mut smc_layers: Vec<(String, Map<String, Value>)> = vec![];
    let mut tpl_layers: Vec<Vec<(String, Map<String, Value>)>> = vec![];
    let mut cur = package_of(package).to_string();
    for _ in 0..16 {
        let pd = PackageData::load(set, &cur).map_err(|e| format!("{cur}: {e}"))?;
        let pkg = pd.package().map_err(|e| format!("{cur}: {e}"))?;
        let class = pkg.class_export().map(|i| pkg.exports[i].name.clone()).unwrap_or_default();
        chain.push(Layer { package: cur.clone(), class });
        let mut cdo = Map::new();
        if let Some(i) = pkg.default_object_export() {
            if let Ok(o) = pkg.read_export(i) {
                if let Value::Object(m) = o.props.to_json(&pkg) {
                    cdo = m;
                }
            }
        }
        // StaticMeshComponent templates of this layer: keyed by name
        for i in pkg.exports_of_class("StaticMeshComponent") {
            if let Ok(o) = pkg.read_export(i) {
                if let Value::Object(m) = o.props.to_json(&pkg) {
                    smc_layers.push((pkg.exports[i].name.clone(), m));
                }
            }
        }
        // POI: every component template of this layer
        let mut var_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for i in 0..pkg.exports.len() {
            let cn = pkg.export_class_name(i);
            if cn != "SCS_Node" && cn != "InheritableComponentHandler" { continue; }
            let Ok(o) = pkg.read_export(i) else { continue };
            let Value::Object(m) = o.props.to_json(&pkg) else { continue };
            if cn == "SCS_Node" {
                if let (Some(t), Some(v)) = (m.get("ComponentTemplate").and_then(|v| v.as_str()), m.get("InternalVariableName").and_then(|v| v.as_str())) { var_of.insert(leaf(t).to_string(), v.to_string()); }
            } else if let Some(recs) = m.get("Records").and_then(|v| v.as_array()) {
                for r in recs {
                    if let (Some(t), Some(v)) = (r.get("ComponentTemplate").and_then(|v| v.as_str()), r.get("ComponentKey").and_then(|k| k.get("SCSVariableName")).and_then(|v| v.as_str())) { var_of.insert(leaf(t).to_string(), v.to_string()); }
                }
            }
        }
        let mut tl = vec![];
        for i in 0..pkg.exports.len() {
            let cn = pkg.export_class_name(i);
            if !cn.ends_with("Component") { continue; }
            if let Ok(o) = pkg.read_export(i) {
                if let Value::Object(m) = o.props.to_json(&pkg) {
                    let name = var_of.get(&pkg.exports[i].name).cloned().unwrap_or_else(|| pkg.exports[i].name.trim_end_matches("_GEN_VARIABLE").to_string());
                    tl.push((name, m));
                }
            }
        }
        tpl_layers.push(tl);
        cdo_layers.push(cdo);
        match pkg.super_class() {
            Some(ObjectRef::Import { package: sp, .. }) if sp.starts_with("/Game/") => cur = sp,
            Some(r) => {
                chain.push(Layer { package: r.path().to_string(), class: r.name().to_string() });
                break;
            }
            None => break,
        }
    }
    let mut cdo = Map::new();
    for l in cdo_layers.iter().rev() {
        merge(&mut cdo, l);
    }
    let smc_name = cdo.get("RootComponent").and_then(|v| v.as_str()).map(|p| leaf(p).to_string()).unwrap_or_default();
    let mut smc = Map::new();
    // layers are leaf-first; apply root-first
    for (n, m) in smc_layers.iter().rev() {
        if *n == smc_name {
            merge(&mut smc, m);
        }
    }
    let mut templates: Map<String, Value> = Map::new();
    for l in tpl_layers.iter().rev() {
        for (n, m) in l {
            match templates.get_mut(n) { Some(Value::Object(d)) => merge(d, m), _ => { templates.insert(n.clone(), Value::Object(m.clone())); } }
        }
    }
    Ok(Bp { package: package_of(package).to_string(), chain, cdo, smc, smc_name, templates })
}

impl Bp {
    pub fn str_of(&self, key: &str) -> Option<&str> {
        self.cdo.get(key).and_then(|v| v.as_str())
    }
    /// The static mesh object path the actor shows (component template first, then the actor's own `StaticMesh` var).
    pub fn static_mesh(&self) -> Option<String> {
        self.smc.get("StaticMesh").and_then(|v| v.as_str()).or_else(|| self.str_of("StaticMesh")).filter(|s| !s.is_empty()).map(String::from)
    }
    pub fn attr_key(&self) -> (String, String) {
        let k = self.cdo.get("AttributeInitKey");
        let g = |n: &str| k.and_then(|k| k.get(n)).and_then(|v| v.as_str()).unwrap_or("").to_string();
        (g("AttributeInitCategory"), g("AttributeInitSubCategory"))
    }
    pub fn resource_type(&self) -> String {
        self.str_of("ResourceType").unwrap_or("").trim_start_matches("EFortResourceType::").to_string()
    }
}
