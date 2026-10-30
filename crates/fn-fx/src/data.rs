//! Package access: raw bytes of a long package path and a JSON tree of its exports (tagged properties through `fn-uasset`), keyed by full object path.
use fn_pak::PakSet;
use fn_uasset::{PackageData, PackageIndex};
use serde_json::Value;
use std::collections::HashMap;

/// One export as JSON (`fn-inspect props` shape).
pub struct ExportJson { pub index: usize, pub name: String, pub class: String, pub path: String, pub props: Value }

/// All exports of a package as JSON plus the lookup by object path.
pub struct PackageJson {
    pub long: String,
    pub exports: Vec<ExportJson>,
    by_path: HashMap<String, usize>,
}
impl PackageJson {
    pub fn load(set: &PakSet, long: &str) -> Result<PackageJson, String> {
        let data = PackageData::load(set, long).map_err(|e| format!("{long}: {e}"))?;
        let pkg = data.package().map_err(|e| format!("{long}: {e}"))?;
        let mut exports = vec![];
        let mut by_path = HashMap::new();
        for i in 0..pkg.exports.len() {
            let e = pkg.read_export(i).map_err(|e| format!("{long}: export {i}: {e}"))?;
            let path = pkg.object_path(PackageIndex(i as i32 + 1));
            by_path.insert(path.clone(), i);
            exports.push(ExportJson { index: i, name: e.name.clone(), class: e.class.clone(), path, props: e.props.to_json(&pkg) });
        }
        Ok(PackageJson { long: long.to_owned(), exports, by_path })
    }
    pub fn load_classes(set: &PakSet, long: &str, keep: impl Fn(&str) -> bool) -> Result<PackageJson, String> {
        let data = PackageData::load(set, long).map_err(|e| format!("{long}: {e}"))?;
        let pkg = data.package().map_err(|e| format!("{long}: {e}"))?;
        let mut exports = vec![];
        let mut by_path = HashMap::new();
        for i in 0..pkg.exports.len() {
            if !keep(&pkg.export_class_name(i)) { continue; }
            let e = pkg.read_export(i).map_err(|e| format!("{long}: export {i}: {e}"))?;
            let path = pkg.object_path(PackageIndex(i as i32 + 1));
            by_path.insert(path.clone(), exports.len());
            exports.push(ExportJson { index: i, name: e.name.clone(), class: e.class.clone(), path, props: e.props.to_json(&pkg) });
        }
        Ok(PackageJson { long: long.to_owned(), exports, by_path })
    }
    pub fn get(&self, path: &str) -> Option<&ExportJson> { self.by_path.get(path).map(|&i| &self.exports[i]) }
    pub fn by_name(&self, name: &str) -> Option<&ExportJson> { self.exports.iter().find(|e| e.name == name) }
    pub fn of_class<'a>(&'a self, class: &'a str) -> impl Iterator<Item = &'a ExportJson> + 'a { self.exports.iter().filter(move |e| e.class == class) }
}

/// `.uasset`, `.uexp` bytes of a long package path.
pub fn package_files(set: &PakSet, long: &str) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), String> {
    // LANDSCAPE MATERIAL (2026-10-07): a map package (`.umap`: the landscape sublevels' material instances, heightmaps, weightmaps) when there is no `.uasset`
    let a = match set.read_game(long, "uasset") { Ok(c) => c.into_owned(), Err(e) => set.read_game(long, "umap").map_err(|_| format!("{long}: {e}"))?.into_owned() };
    let x = set.read_game(long, "uexp").map(|c| c.into_owned()).unwrap_or_default();
    let b = set.read_game(long, "ubulk").map(|c| c.into_owned()).unwrap_or_default();
    Ok((a, x, b))
}

/// `/Game/Foo/Bar.Bar` -> `/Game/Foo/Bar`.
pub fn long_package(object_path: &str) -> &str { object_path.split('.').next().unwrap_or(object_path) }
