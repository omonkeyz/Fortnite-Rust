//! Research tool (MATSTAT audit, 2026-10-05): which cooked material shaders the static props of the CURRENT layout and the terrain / backdrop use, and whether
//! each one has the base-pass programs the FX path needs (DXBC -> WGSL, `fn_fx::gpu::build_material_layout` + `program::assemble_scene_lit`).
//!
//! `material_coverage [props dir] [radius m]` (defaults: `<repo>/../FortniteRuntime/assets/props`, 150 m = `fn-app props::DEFAULT_RADIUS_M`).
//! The layout selection is `fn-app props::Library::from_catalog`: every placement of `layout/all.json` whose kind is in the loaded categories
//! (`fn_render::static_loader::DEFAULT_CATEGORIES`) within the radius (XY) of `layout/sample.json`'s centre. Materials per mesh slot = `props.json` `materials`
//! (the component overrides of the class included, `source`). Prints a Markdown table.
use fn_fx::gpu::Vf;
use fn_fx::material::Material;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const CATEGORIES: [&str; 4] = ["tree", "rock", "bush", "hedge"];
const VS: &str = "TBasePassVSFNoLightMapPolicy";
const PS: &str = "TBasePassPSFNoLightMapPolicy";
const PS_SKY: &str = "TBasePassPSFNoLightMapPolicySkylight";
const DEPTH_PS: &str = "FDepthOnlyPS";

fn json(p: &std::path::Path) -> Result<Value, String> { serde_json::from_slice(&std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?).map_err(|e| format!("{}: {e}", p.display())) }

struct Row { kinds: BTreeSet<String>, placements: usize, slots: BTreeSet<String> }

fn check(set: &fn_pak::PakSet, path: &str, vf: &str) -> String {
    let m = match Material::load(set, path) { Ok(m) => m, Err(e) => return format!("LOAD FAILED: {e} | | | | | |") };
    let has = |t: &str| m.cooked.find(vf, t).is_some();
    let (vs, ps, sky, depth) = (has(VS), has(PS), has(PS_SKY), m.cooked.programs.iter().any(|p| p.vertex_factory == vf && p.shader_type.starts_with(DEPTH_PS)));
    let shadow = has(fn_fx::gpu::SHADOW_VS);
    // the scene-lit assembly (translation of both programs) is the real test that the path can run it
    let asm = match (m.cooked.find(vf, VS), m.cooked.find(vf, PS)) {
        (Some(v), Some(p)) => {
            let clip = if m.flags.blend == "BLEND_Masked" { m.cooked.programs.iter().find(|q| q.vertex_factory == vf && q.shader_type.starts_with(DEPTH_PS) && q.major >= 5).map(|e| &**e) } else { None };
            let r = if m.flags.uses_gbuffer() { fn_fx::program::assemble_scene_lit(v, p, clip, &fn_render::fx::default_scene_lit_prelude()).map(|a| a.targets) } else { fn_fx::program::assemble(v, p, false, clip).map(|a| a.targets) };
            match r { Ok(t) => format!("OK ({t} targets)"), Err(e) => format!("FAIL: {}", e.chars().take(90).collect::<String>()) }
        }
        _ => "n/a".into(),
    };
    let y = |b: bool| if b { "yes" } else { "NO" };
    let vfs: BTreeSet<&str> = m.cooked.programs.iter().map(|p| p.vertex_factory.as_str()).collect();
    format!("{} | {} | {} | {} | {} | `{}` | {} / {} / {} / depth {} / shadowVS {} | {} | {} progs, VFs {:?}",
        m.base_path.rsplit('.').next().unwrap_or(""), m.flags.shading, m.flags.blend, m.flags.two_sided, m.flags.opacity_mask_clip, m.shader_map_package.rsplit('/').next().unwrap_or(""),
        y(vs), y(ps), y(sky), y(depth), y(shadow), asm, m.cooked.programs.len(), vfs)
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--sig <material>...`: the output signature (semantic, index, register) of the base-pass PS = which SV_Target holds GBufferD / velocity
    if args.first().map(|s| s.as_str()) == Some("--sig") {
        let set = fn_pak::PakSet::open_install(fn_fx::default_install()).map_err(|e| e.to_string())?;
        for p in &args[1..] {
            let m = Material::load(&set, p)?;
            for ty in [VS, PS] { if let Some(pr) = m.cooked.find(Vf::Local.type_name(), ty) { println!("{p} {ty} out {:?}", fn_fx::cooked::container_output_registers(&pr.blob.bytes)); } }
        }
        return Ok(());
    }
    let dir = args.first().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3).unwrap().join("FortniteRuntime").join("assets").join("props"));
    let radius_cm = args.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(150.) * 100.;
    let set = fn_pak::PakSet::open_install(fn_fx::default_install()).map_err(|e| e.to_string())?;
    let cat = json(&dir.join("catalog.json"))?;
    let mut kind_dir = BTreeMap::new();
    for k in cat["kinds"].as_array().ok_or("catalog kinds")? {
        if CATEGORIES.contains(&k["category"].as_str().unwrap_or("")) { kind_dir.insert(k["id"].as_str().unwrap_or("").to_owned(), k["dir"].as_str().unwrap_or("").to_owned()); }
    }
    let sample = json(&dir.join("layout/sample.json"))?;
    let c: Vec<f64> = sample["center_abs"].as_array().ok_or("center")?.iter().map(|v| v.as_f64().unwrap_or(0.)).collect();
    let all = json(&dir.join("layout/all.json"))?;
    let classes: Vec<&str> = all["classes"].as_array().ok_or("classes")?.iter().map(|v| v.as_str().unwrap_or("")).collect();
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for i in all["instances"].as_array().ok_or("instances")? {
        let a = i.as_array().unwrap();
        let cls = classes[a[0].as_u64().unwrap() as usize];
        if !kind_dir.contains_key(cls) { continue; }
        let (dx, dy) = (a[1].as_f64().unwrap() - c[0], a[2].as_f64().unwrap() - c[1]);
        if dx * dx + dy * dy <= (radius_cm as f64).powi(2) { *count.entry(cls.to_owned()).or_default() += 1; }
    }
    let total: usize = count.values().sum();
    println!("# layout: {} placements, {} kinds within {} m of {:?}\n", total, count.len(), radius_cm / 100., c);
    let mut mats: BTreeMap<String, Row> = BTreeMap::new();
    println!("| kind | placements | mesh | slots (material, source) |\n|---|---|---|---|");
    for (id, n) in &count {
        let p = json(&dir.join(&kind_dir[id]).join("props.json"))?;
        let mesh = p["mesh"]["ue_path"].as_str().unwrap_or("?");
        let mut slots = vec![];
        for m in p["materials"].as_array().cloned().unwrap_or_default() {
            let path = m["ue_path"].as_str().unwrap_or("").to_owned();
            slots.push(format!("{}: {} ({})", m["slot"], path.rsplit('.').next().unwrap_or(""), m["source"].as_str().unwrap_or("")));
            let r = mats.entry(path).or_insert(Row { kinds: BTreeSet::new(), placements: 0, slots: BTreeSet::new() });
            r.kinds.insert(id.clone()); r.placements += n; r.slots.insert(format!("{id}#{}", m["slot"]));
        }
        println!("| {id} | {n} | `{}` | {} |", mesh.rsplit('/').next().unwrap_or(mesh), slots.join("; "));
    }
    println!("\n## static prop materials ({} unique), vertex factory FLocalVertexFactory\n", mats.len());
    println!("| material | kinds | placements | root | shading | blend | two-sided | clip | shader map package | VS / PS / PS sky / depth-only PS / shadow VS | scene-lit assembly | programs |\n|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (path, r) in &mats {
        println!("| `{}` | {} | {} | {} |", path.rsplit('.').next().unwrap_or(path), r.kinds.len(), r.placements, check(&set, path, Vf::Local.type_name()));
    }
    // terrain / backdrop
    let bd = json(&dir.join("backdrop/backdrop.json"));
    println!("\n## backdrop (landscape / skybox / ocean)\n");
    if let Ok(bd) = bd {
        let mut seen = BTreeSet::new();
        for i in bd["instances"].as_array().cloned().unwrap_or_default() {
            let p = i["material"]["path"].as_str().unwrap_or("").to_owned();
            if seen.insert(p.clone()) { println!("| `{}` (backdrop static mesh) | {} |", p.rsplit('.').next().unwrap_or(""), check(&set, &p, Vf::Local.type_name())); }
        }
        let lm = bd["terrain"]["landscape_material"].as_str().unwrap_or("").to_owned();
        let lm = if lm.contains('.') { lm } else { format!("{lm}.{}", lm.rsplit('/').next().unwrap_or("")) };
        println!("| `{}` (landscape material, FLocalVertexFactory) | {} |", lm.rsplit('.').next().unwrap_or(""), check(&set, &lm, Vf::Local.type_name()));
        println!("| `{}` (landscape material, FLandscapeVertexFactory) | {} |", lm.rsplit('.').next().unwrap_or(""), check(&set, &lm, "FLandscapeVertexFactory"));
    }
    // the landscape components' own MICs (LandscapeMaterialInstanceConstant) live in the level packages: their programs per vertex factory
    // the persistent level + every streamed level it references (`LevelStreaming*` `WorldAsset`, as fn-export props `layout::levels`)
    let mut levels = vec!["/Game/Athena/Maps/Athena_Terrain".to_owned()];
    if let Ok(pd) = fn_uasset::PackageData::load(&set, "/Game/Athena/Maps/Athena_Terrain") { if let Ok(pkg) = pd.package() {
        for i in 0..pkg.exports.len() {
            if !pkg.export_class_name(i).starts_with("LevelStreaming") { continue; }
            let Ok(o) = pkg.read_export(i) else { continue };
            if let Some(wa) = o.props.to_json(&pkg).get("WorldAsset").and_then(|v| v.as_str()) { levels.push(wa.split('.').next().unwrap_or(wa).to_owned()); }
        }
    } }
    let (mut total_comps, mut total_mics, mut with_landscape) = (0usize, 0usize, 0usize);
    let mut agg: BTreeMap<(String, String), usize> = BTreeMap::new();
    for lv in levels.iter().map(|s| s.as_str()) {
        let Some(ap) = fn_pak::game_path_to_pak(lv, "umap") else { continue };
        let Ok(a) = set.read(&ap) else { println!("\n{lv}: not in the paks"); continue };
        let x = set.read(&ap.replace(".umap", ".uexp")).map(|c| c.into_owned()).unwrap_or_default();
        let b = set.read(&ap.replace(".umap", ".ubulk")).map(|c| c.into_owned()).unwrap_or_default();
        let pk = match fn_ueio::Package::parse(&a, &x, &b) { Ok(p) => p, Err(e) => { println!("\n{lv}: {e}"); continue } };
        let mics: Vec<usize> = (0..pk.exports.len()).filter(|&i| pk.export_class(i) == "LandscapeMaterialInstanceConstant").collect();
        let comps = (0..pk.exports.len()).filter(|&i| pk.export_class(i) == "LandscapeComponent").count();
        let cooked = fn_fx::cooked::Cooked::parse(&x, &pk.names, pk.summary.total_header_size as usize);
        let mut by: BTreeMap<(String, String), usize> = BTreeMap::new();
        if let Ok(c) = &cooked { for p in &c.programs { if p.shader.major >= 5 { *by.entry((p.vertex_factory.clone(), p.shader_type.clone())).or_default() += 1; } } }
        if comps > 0 || !mics.is_empty() { println!("{lv}: {comps} LandscapeComponents, {} LandscapeMaterialInstanceConstants, cooked programs {}", mics.len(), cooked.as_ref().map(|c| c.programs.len().to_string()).unwrap_or_else(|e| format!("parse failed: {e}"))); }
        total_comps += comps; total_mics += mics.len(); if comps > 0 { with_landscape += 1; }
        for (k, n) in by { *agg.entry(k).or_default() += n; }
    }
    println!("
landscape total: {total_comps} LandscapeComponents in {with_landscape} of {} levels, {total_mics} LandscapeMaterialInstanceConstants; SM5 programs (all levels):", levels.len());
    for ((vf, ty), n) in agg.iter().filter(|((v, t), _)| v.starts_with("FLandscape") && (t.starts_with("TBasePassVSFNoLightMapPolicy") || t.starts_with("TBasePassPSFNoLightMapPolicy") || t.starts_with("FDepthOnly") || *t == fn_fx::gpu::SHADOW_VS || *t == fn_fx::gpu::SHADOW_PS)) { println!("  {n:>4} x {vf} / {ty}"); }
    Ok(())
}
