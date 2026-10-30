//! PICKUP MESH (fn-app): the displayed mesh of a world item (`PickupSkeletalMesh` / `PickupStaticMesh` of its item definition) as renderer primitives, read from the 1.8 paks.
//! Worker-thread code: no GPU objects are created here (the hook uploads the primitives).
//!
//! * geometry: LOD 0, UE cm (x, y, z) -> glTF metres (x, z, y) like every other mesh of the port (`fn_mesh::gltf::ue_to_gltf_position`), the vertices re-centred on the bounds
//!   origin: `B_Pickups` (`UserConstructionScript` ... `K2_SetWorldLocation(ActorLocation + (ComponentLocation - BoundsOrigin))`) puts the mesh's bounds centre on the actor location.
//! * textures: the material slot's instance chain (`MaterialInstanceConstant` -> parent -> ...), `TextureParameterValues` only (the cooked packages keep no defaults of the
//!   parent material graph): the first of Diffuse / BaseColor / Albedo ... and Normals; a slot without a diffuse texture is drawn with the material kind's neutral values (reported).
//!   These primitives use the renderer's `Prop` shading with the original textures, exactly like the held-gun stand-in; when the held gun runs its original material
//!   (`--original-gun-material`), weapon items do too (`pickup_orig.rs`: `WeaponMat_Base` in the gun pass) and these primitives are not drawn for them.
use fn_game::pickup_items::ItemDef;
use fn_mesh::gltf::{ue_to_gltf_direction, ue_to_gltf_position};
use fn_pose::ClipSourceDir;
use fn_render::{MaterialImages, MaterialKind, SkinnedPrimitive, SkinnedVertex};
use fn_ueio::props::{read_props, Ctx, PropValue};
use fn_ueio::Reader;
use image::RgbaImage;
use std::collections::HashMap;

/// A loaded pickup mesh (CPU side).
pub struct MeshAsset {
    pub prims: Vec<SkinnedPrimitive>,
    /// `GetComponentBounds` SphereRadius of the mesh (cm): `B_Pickups` derives `Component Radius (Scaled)` and `Sphere Radius` from it.
    pub radius_cm: f32,
    /// Bounds half extents (cm) and origin (cm, UE axes), as cooked.
    pub extent_cm: [f32; 3],
    pub origin_cm: [f32; 3],
    /// What could not be read (missing textures, ...), reported once.
    pub notes: Vec<String>,
    /// The mesh package that was drawn.
    pub source: String,
}

fn long_of(path: &str) -> &str { path.split('.').next().unwrap_or(path) }

/// `(Parent, TextureParameterValues)` of one material export.
fn material_level(src: &ClipSourceDir, path: &str) -> Result<(Option<String>, Vec<(String, String)>), String> {
    let long = long_of(path);
    let obj = path.split_once('.').map(|x| x.1).unwrap_or(long.rsplit('/').next().unwrap_or(long));
    let pkg = fn_pose::loader::package(src, long)?;
    let idx = pkg.exports.iter().position(|e| e.object_name == obj).ok_or_else(|| format!("{path}: no such export"))?;
    let class = pkg.export_class(idx);
    if !matches!(class.as_str(), "Material" | "MaterialInstanceConstant" | "MaterialInstance") { return Err(format!("{path}: unsupported material class {class}")); }
    let (start, end) = pkg.export_range(idx);
    let props = read_props(&mut Reader::at(&pkg.data[..end], start), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| format!("{path}: {e}"))?;
    let parent = match props.get("Parent") { Some(PropValue::Object(i)) if *i < 0 => Some(pkg.import_path(*i)), _ => None };
    let mut tex = vec![];
    if let Some(values) = props.array("TextureParameterValues") {
        for v in values {
            let Some(f) = v.as_fields() else { continue };
            let name = f.text("ParameterName").or_else(|| f.fields("ParameterInfo").and_then(|p| p.text("Name"))).unwrap_or("").to_owned();
            if let Some(i) = f.object("ParameterValue").filter(|i| *i < 0) { tex.push((name, pkg.import_path(i))); }
        }
    }
    Ok((parent, tex))
}

/// The merged texture parameters of an instance chain (a child overrides its parents).
fn merged_textures(src: &ClipSourceDir, material: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = vec![];
    let mut cur = Some(material.to_owned());
    for _ in 0..16 {
        let Some(p) = cur else { break };
        if !p.starts_with("/Game/") { break; }
        let (parent, tex) = material_level(src, &p)?;
        for (n, t) in tex { if !out.iter().any(|(on, _)| *on == n) { out.push((n, t)); } }
        cur = parent;
    }
    Ok(out)
}

fn pick<'a>(tex: &'a [(String, String)], exact: &[&str], contains: &[&str]) -> Option<&'a (String, String)> {
    for e in exact { if let Some(t) = tex.iter().find(|(n, _)| n.eq_ignore_ascii_case(e)) { return Some(t); } }
    tex.iter().find(|(n, _)| { let l = n.to_ascii_lowercase(); contains.iter().any(|c| l.contains(c)) })
}

type Cooked = Option<std::sync::Arc<fn_render::mesh::CookedTexture>>;
fn load_image(src: &ClipSourceDir, path: &str) -> Result<(RgbaImage, Cooked), String> {
    let t = fn_pose::loadout_material::load_texture(src, path)?;
    let img = RgbaImage::from_raw(t.image.width as u32, t.image.height as u32, t.image.to_rgba8()).ok_or_else(|| format!("{path}: bad decoded size"))?;
    Ok((img, crate::br_character::cooked_texture(&t.source)))
}

/// Loads the mesh of `def` (skeletal first, then static) with its material textures.
pub fn load(set: &fn_pak::PakSet, def: &ItemDef) -> Result<MeshAsset, String> {
    let src = ClipSourceDir::Pak(set);
    let (path, skeletal) = match (&def.skeletal_mesh, &def.static_mesh) { (Some(p), _) => (p.clone(), true), (None, Some(p)) => (p.clone(), false), _ => return Err(format!("{}: the item definition names no pickup mesh", def.id)) };
    let pkg = fn_pose::loader::package(&src, long_of(&path))?;
    // geometry: one (positions, normals, uvs, indices, material) per section
    struct Sec { pos: Vec<[f32; 3]>, nrm: Vec<[f32; 3]>, uv: Vec<[f32; 2]>, idx: Vec<u32>, material: Option<String> }
    let (secs, bounds): (Vec<Sec>, fn_mesh::Bounds) = if skeletal {
        let m = fn_mesh::SkeletalMesh::parse(&pkg).map_err(|e| format!("{path}: {e}"))?;
        let lod = m.lods.first().ok_or_else(|| format!("{path}: no LOD"))?;
        let mut v = vec![];
        for s in lod.sections.iter().filter(|s| !s.disabled) {
            let base = s.base_index as usize; let n = s.num_triangles as usize * 3;
            let Some(ix) = lod.indices.get(base..base + n) else { continue };
            let mut map: HashMap<u32, u32> = HashMap::new();
            let mut sec = Sec { pos: vec![], nrm: vec![], uv: vec![], idx: vec![], material: m.materials.get(s.material_index.max(0) as usize).and_then(|x| x.material.clone()) };
            for &i in ix {
                let l = *map.entry(i).or_insert_with(|| {
                    let n = sec.pos.len() as u32;
                    sec.pos.push(ue_to_gltf_position(lod.positions[i as usize])); sec.nrm.push(ue_to_gltf_direction(lod.normal(i as usize)));
                    sec.uv.push(lod.uvs.first().and_then(|u| u.get(i as usize)).copied().unwrap_or([0., 0.]));
                    n
                });
                sec.idx.push(l);
            }
            v.push(sec);
        }
        (v, m.bounds)
    } else {
        let m = fn_mesh::StaticMesh::parse(&pkg).map_err(|e| format!("{path}: {e}"))?;
        let lod = m.lods.first().ok_or_else(|| format!("{path}: no LOD"))?;
        let mut v = vec![];
        for s in &lod.sections {
            let base = s.first_index as usize; let n = s.num_triangles as usize * 3;
            let Some(ix) = lod.indices.get(base..base + n) else { continue };
            let mut map: HashMap<u32, u32> = HashMap::new();
            let mut sec = Sec { pos: vec![], nrm: vec![], uv: vec![], idx: vec![], material: m.materials.get(s.material_index.max(0) as usize).and_then(|x| x.material.clone()) };
            for &i in ix {
                let l = *map.entry(i).or_insert_with(|| {
                    let n = sec.pos.len() as u32;
                    sec.pos.push(ue_to_gltf_position(lod.positions[i as usize])); sec.nrm.push(ue_to_gltf_direction(lod.normal(i as usize)));
                    sec.uv.push(lod.uvs.first().and_then(|u| u.get(i as usize)).copied().unwrap_or([0., 0.]));
                    n
                });
                sec.idx.push(l);
            }
            v.push(sec);
        }
        (v, m.bounds)
    };
    if secs.is_empty() { return Err(format!("{path}: no drawable section")); }
    let centre = ue_to_gltf_position(bounds.origin);
    let mut notes = vec![];
    let mut cache: HashMap<String, (Option<(RgbaImage, Cooked)>, Option<(RgbaImage, Cooked)>)> = HashMap::new();
    let mut prims = vec![];
    for s in secs {
        let (diffuse, normal) = match &s.material {
            Some(mat) => cache.entry(mat.clone()).or_insert_with(|| {
                let tex = match merged_textures(&src, mat) { Ok(t) => t, Err(e) => { notes.push(format!("material {mat}: {e}")); vec![] } };
                let d = pick(&tex, &["Diffuse", "DiffuseTexture", "BaseColor", "Albedo", "Color", "Diffuse_Tex", "Texture"], &["diffuse", "albedo", "basecolor", "base_color"]);
                let n = pick(&tex, &["Normals", "Normal", "NormalMap", "Normal_Map"], &["normal"]);
                let load = |t: Option<&(String, String)>, what: &str, notes: &mut Vec<String>| t.and_then(|(name, p)| match load_image(&src, p) { Ok(i) => Some(i), Err(e) => { notes.push(format!("{what} `{name}` of {mat}: {e}")); None } });
                let (di, ni) = (load(d, "diffuse", &mut notes), load(n, "normal", &mut notes));
                if di.is_none() { notes.push(format!("material {mat}: no diffuse texture parameter found (parameters: {})", tex.iter().map(|t| t.0.as_str()).collect::<Vec<_>>().join(", "))); }
                (di, ni)
            }).clone(),
            None => { notes.push("a section has no material".into()); (None, None) }
        };
        let vertices = (0..s.pos.len()).map(|i| {
            let p = [s.pos[i][0] - centre[0], s.pos[i][1] - centre[1], s.pos[i][2] - centre[2]];
            SkinnedVertex { pos: p, normal: s.nrm[i], uv: s.uv[i], joints: [0; 4], weights: [1., 0., 0., 0.] }
        }).collect();
        let two_sided = s.material.as_deref().is_some_and(|m| fn_pose::br_character::material_two_sided(&src, m).unwrap_or(false)); // shadow-pass culling (UE4.16 ShadowDepthRendering.cpp:1059)
        prims.push(SkinnedPrimitive { vertices, indices: s.idx, material: MaterialImages { cooked: [diffuse.as_ref().and_then(|d| d.1.clone()), normal.as_ref().and_then(|n| n.1.clone()), None, None], diffuse: diffuse.map(|d| d.0), normal: normal.map(|n| n.0), spec: None, mask: None, ue: None, two_sided }, kind: MaterialKind::Prop });
    }
    Ok(MeshAsset { prims, radius_cm: bounds.radius, extent_cm: bounds.extent, origin_cm: bounds.origin, notes, source: long_of(&path).to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The real meshes of the items the console spawns load with textures (needs FN_TEST_ROOT).
    #[test] fn the_real_pickup_meshes_load() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT").map(std::path::PathBuf::from) else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let dir = fn_pak::find_paks_dir(&root).expect("paks");
        let set = fn_pak::PakSet::open_install(&dir).unwrap();
        let items = fn_game::pickup_items::scan(&set);
        for id in ["WID_Assault_Auto_Athena_R_Ore_T03", "WID_Shotgun_Standard_Athena_C_Ore_T03", "WID_Pistol_SixShooter_Athena_C_Ore_T02", "Athena_Bandage", "Athena_Medkit", "Athena_Shields", "Athena_PurpleStuff", "AthenaAmmoDataBulletsMedium", "AthenaAmmoDataShells", "WoodItemData", "StoneItemData", "MetalItemData", "TID_Floor_Spikes_Athena_R_T03"] {
            let def = items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{id} missing"));
            let t = std::time::Instant::now();
            match load(&set, def) {
                Ok(a) => eprintln!("{id}: {} prims, {} verts, radius {:.1} cm, extent {:?}, diffuse {} normal {} ({:.0} ms) notes {:?}", a.prims.len(), a.prims.iter().map(|p| p.vertices.len()).sum::<usize>(), a.radius_cm, a.extent_cm, a.prims.iter().filter(|p| p.material.diffuse.is_some()).count(), a.prims.iter().filter(|p| p.material.normal.is_some()).count(), t.elapsed().as_secs_f32() * 1000., a.notes),
                Err(e) => eprintln!("{id}: ERROR {e}"),
            }
        }
    }
}
