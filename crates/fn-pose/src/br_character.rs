//! BR CHARACTER: the real Battle Royale (Athena) pawn's character parts, read from the 1.8 packages.
//!
//! What the data says (all recovered, see the tests):
//! * `/Game/Athena/PlayerPawn_Athena_Generic` (`PlayerPawn_Athena`'s parent) has ONE skeletal-mesh component, `CharacterMesh0`, with mesh
//!   `Fortnite_M_Avg_Player` (the master / IK skeleton) and `Fortnite_M_Avg_Player_AnimBlueprint`. The visible body / head / hat are
//!   `CustomCharacterPart`s that the pawn follows onto that master (`OnCharacterPartsReinitialized`, `SlaveAMeshToTheBody`).
//! * A hero (`HID_*_Athena_Commando_*`, class `FortHeroType`) lists `Specializations`; each `HS_*` (`FortHeroSpecialization`) lists its
//!   `CharacterParts`; each part is a `CustomCharacterPart` with `SkeletalMesh`, `MasterSkeletalMeshes`, `MaterialOverrides` (slot index ->
//!   material instance) and `AdditionalData` (`CustomCharacterBodyPartData` / `CustomCharacterHeadData`: `AnimClass`, hair / skin swatches,
//!   hat / helmet morph targets).
//! * Which hero a fresh BR player wears is a per-player runtime choice (it is NOT stored on the pawn / game mode / controller in the paks):
//!   [`DEFAULT_HERO`] is a caller's choice, not recovered data.
//!
//! Everything here is renderer independent: a part is a [`Part`] (skeleton + skinned primitives, master IK bones appended) and the
//! per-section material comes back as decoded RGBA textures keyed by the material's own parameter names (`Diffuse`, `Normals`,
//! `SpecularMasks`, `M`), inherited down the material-instance parent chain.
use crate::{loader, ClipSourceDir, Part};
use fn_ueio::{props::{read_props, Ctx, PropList, PropValue}, Package, Reader};
use std::collections::HashMap;

/// A hero that exists in the 1.8 paks and whose parts follow the master skeleton `Fortnite_M_Avg_Player`: the male Medium soldier
/// ("Rescue Trooper Ramirez", `HS_005` = head `M_Med_Soldier_Head_01_ATH` + body `CP_005_Athena_Body`). Its body mesh is the Athena
/// variant `SK_M_Med_Soldier_04_ATH`; the current (pre-BR) port used the non-Athena `SK_M_Med_Soldier_04`.
pub const DEFAULT_HERO: &str = "/Game/Athena/Heroes/HID_005_Athena_Commando_M";
pub const MASTER_MESH_PKG: &str = loader::MASTER_PKG;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind { Head, Body, Hat, Other }

#[derive(Clone, Debug)]
pub struct PartDef {
    /// `/Game/...` package of the `CustomCharacterPart`.
    pub package: String,
    pub kind: PartKind,
    /// `CharacterPartType` enum text when serialized (default-valued enums are not).
    pub part_type: Option<String>,
    /// `/Game/..Pkg.Obj` of the skeletal mesh.
    pub skeletal_mesh: String,
    pub master_meshes: Vec<String>,
    pub gender: Option<String>,
    /// `(material slot index, override material path)`.
    pub material_overrides: Vec<(usize, String)>,
    pub additional_data_class: Option<String>,
    pub anim_class: Option<String>,
    pub skin_swatch: Option<String>,
    pub hair_swatch: Option<String>,
    pub hat_morphs: Vec<String>,
    pub helmet_morphs: Vec<String>,
}

fn export_props(pkg: &Package, idx: usize) -> Result<PropList, String> {
    let (start, end) = pkg.export_range(idx);
    read_props(&mut Reader::at(&pkg.data[..end], start), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())
}
fn obj_path(pkg: &Package, v: &PropValue) -> Option<String> {
    match v { PropValue::Object(i) if *i < 0 => Some(pkg.import_path(*i)), PropValue::Str(s) if s.starts_with("/Game/") => Some(s.clone()), _ => None } // soft references are strings
}
fn obj_list(pkg: &Package, p: &PropList, key: &str) -> Vec<String> {
    p.array(key).map(|a| a.iter().filter_map(|v| obj_path(pkg, v)).collect()).unwrap_or_default()
}
fn long_of(path: &str) -> &str { path.split_once('.').map_or(path, |s| s.0) }
fn name_list(p: &PropList, key: &str) -> Vec<String> {
    p.array(key).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}
fn first_export(pkg: &Package, class: &str) -> Result<usize, String> {
    (0..pkg.exports.len()).find(|&i| pkg.export_class(i) == class).ok_or_else(|| format!("no {class} export"))
}

/// `Specializations` -> `CharacterParts` -> parts of a hero, in the order the data lists them.
pub fn hero_part_packages(src: &ClipSourceDir, hid: &str) -> Result<Vec<String>, String> {
    let pkg = loader::package(src, long_of(hid))?;
    let i = first_export(&pkg, "FortHeroType")?;
    let props = export_props(&pkg, i)?;
    let specs = obj_list(&pkg, &props, "Specializations");
    if specs.is_empty() { return Err(format!("{hid}: no Specializations")); }
    let mut out = vec![];
    for s in specs {
        let sp = loader::package(src, long_of(&s))?;
        let j = first_export(&sp, "FortHeroSpecialization")?;
        let sprops = export_props(&sp, j)?;
        for part in obj_list(&sp, &sprops, "CharacterParts") { if !out.contains(&part) { out.push(part); } }
    }
    if out.is_empty() { return Err(format!("{hid}: its specializations list no CharacterParts")); }
    Ok(out)
}

/// Reads one `CustomCharacterPart` package.
pub fn read_part_def(src: &ClipSourceDir, part_path: &str) -> Result<PartDef, String> {
    let long = long_of(part_path);
    let pkg = loader::package(src, long)?;
    let i = first_export(&pkg, "CustomCharacterPart")?;
    let p = export_props(&pkg, i)?;
    let skeletal_mesh = p.get("SkeletalMesh").and_then(|v| obj_path(&pkg, v)).ok_or_else(|| format!("{long}: part has no SkeletalMesh"))?;
    let mut material_overrides = vec![];
    for v in p.array("MaterialOverrides").map(|a| a.as_slice()).unwrap_or(&[]) {
        let f = v.as_fields().ok_or("bad MaterialOverrides entry")?;
        let slot = f.int("MaterialOverrideIndex").unwrap_or(0).max(0) as usize; // an int property equal to its default (0) is not serialized
        let mat = f.get("OverrideMaterial").and_then(|v| obj_path(&pkg, v)).ok_or_else(|| format!("{long}: override of slot {slot} has no material"))?;
        material_overrides.push((slot, mat));
    }
    // AdditionalData is an object inside this package ("Pkg:CustomCharacterXData_n")
    let (mut add_class, mut anim, mut skin, mut hair, mut hat, mut helmet) = (None, None, None, None, vec![], vec![]);
    if let Some(PropValue::Object(ai)) = p.get("AdditionalData") {
        if *ai > 0 {
            let k = *ai as usize - 1;
            add_class = Some(pkg.export_class(k));
            let a = export_props(&pkg, k)?;
            anim = a.get("AnimClass").and_then(|v| obj_path(&pkg, v));
            skin = a.get("SkinColorSwatch").and_then(|v| obj_path(&pkg, v));
            hair = a.get("HairColorSwatch").and_then(|v| obj_path(&pkg, v));
            hat = name_list(&a, "HatMorphTargets");
            helmet = name_list(&a, "HelmetMorphTargets");
        }
    }
    let part_type = p.text("CharacterPartType").map(str::to_owned);
    let kind = match (part_type.as_deref(), add_class.as_deref()) {
        (Some(t), _) if t.ends_with("::Body") => PartKind::Body,
        (Some(t), _) if t.ends_with("::Head") => PartKind::Head,
        (Some(t), _) if t.ends_with("::Hat") => PartKind::Hat,
        (Some(_), _) => PartKind::Other,
        // the Head value is the enum's default (0), so a head part does not serialize CharacterPartType; its AdditionalData class says it
        (None, Some("CustomCharacterHeadData")) => PartKind::Head,
        (None, _) => PartKind::Other,
    };
    Ok(PartDef { package: long.to_owned(), kind, part_type, skeletal_mesh, master_meshes: obj_list(&pkg, &p, "MasterSkeletalMeshes"), gender: p.text("GenderPermitted").map(str::to_owned), material_overrides, additional_data_class: add_class, anim_class: anim, skin_swatch: skin, hair_swatch: hair, hat_morphs: hat, helmet_morphs: helmet })
}

pub fn is_female(defs: &[PartDef]) -> bool {
    defs.iter().any(|d| d.kind == PartKind::Body && d.gender.as_deref().is_some_and(|g| g.ends_with("::Female")))
}

pub fn master_mesh_for(defs: &[PartDef]) -> String {
    defs.iter().find(|d| d.kind == PartKind::Body)
        .and_then(|d| d.master_meshes.first().cloned())
        .unwrap_or_else(|| MASTER_MESH_PKG.to_owned())
}

/// Effective texture parameters of a material instance: the instance's own `TextureParameterValues` over its parents' (stops at the first
/// non-instance parent: a base `Material`'s expression defaults are not read, see the report).
pub fn effective_texture_params(src: &ClipSourceDir, material: &str) -> Result<MaterialChain, String> {
    let mut chain: Vec<HashMap<String, String>> = vec![];
    let mut base: Option<(String, String)> = None;
    let mut cur = Some(material.to_owned());
    while let Some(path) = cur.take() {
        if chain.len() > 16 { return Err(format!("{material}: material parent chain too deep")); }
        let (long, object) = path.split_once('.').ok_or_else(|| format!("{path}: material needs a full object path"))?;
        let pkg = loader::package(src, long)?;
        let idx = pkg.exports.iter().position(|e| e.object_name == object).ok_or_else(|| format!("{path}: missing material export"))?;
        let class = pkg.export_class(idx);
        if class != "MaterialInstanceConstant" {
            // reached the base material: only its BlendMode is read (expression defaults are not evaluated)
            let props = export_props(&pkg, idx)?;
            base = Some((path.clone(), props.text("BlendMode").unwrap_or("BLEND_Opaque").to_owned()));
            break;
        }
        let props = export_props(&pkg, idx)?;
        let mut here = HashMap::new();
        for v in props.array("TextureParameterValues").map(|a| a.as_slice()).unwrap_or(&[]) {
            let f = v.as_fields().ok_or("bad texture parameter")?;
            let name = f.text("ParameterName").or_else(|| f.fields("ParameterInfo").and_then(|i| i.text("Name"))).ok_or("texture parameter without a name")?;
            if let Some(tex) = f.get("ParameterValue").and_then(|v| obj_path(&pkg, v)) { here.insert(name.to_owned(), tex); }
        }
        chain.push(here);
        cur = props.get("Parent").and_then(|v| obj_path(&pkg, v));
    }
    let mut out = HashMap::new();
    for level in chain.into_iter().rev() { out.extend(level); } // parents first, children overwrite
    Ok(MaterialChain { textures: out, base_material: base.as_ref().map(|b| b.0.clone()), blend_mode: base.map(|b| b.1) })
}

/// The material's effective `TwoSided` flag: the root `UMaterial::TwoSided`, replaced by a `MaterialInstanceConstant`'s `BasePropertyOverrides.TwoSided` only
/// when its `bOverride_TwoSided` is set (UE4.16 `UMaterialInstance::IsTwoSided` / `FMaterialInstanceBasePropertyOverrides`; the same rule as
/// `fn_matshader::material::resolve_flags`). `material` is an object path (`/Game/X/M.M`, or the package path when the object is named like the package).
pub fn material_two_sided(src: &ClipSourceDir, material: &str) -> Result<bool, String> {
    let mut overrides: Vec<Option<bool>> = vec![];
    let mut cur = Some(material.to_owned());
    while let Some(path) = cur.take() {
        if overrides.len() > 16 { return Err(format!("{material}: material parent chain too deep")); }
        let (long, object) = match path.split_once('.') { Some((l, o)) => (l.to_owned(), o.to_owned()), None => (path.clone(), path.rsplit('/').next().unwrap_or(&path).to_owned()) };
        let pkg = loader::package(src, &long)?;
        let idx = pkg.exports.iter().position(|e| e.object_name == object).ok_or_else(|| format!("{path}: missing material export"))?;
        let props = export_props(&pkg, idx)?;
        if pkg.export_class(idx) != "MaterialInstanceConstant" {
            let mut two = props.bool("TwoSided").unwrap_or(false);
            for o in overrides.iter().rev().flatten() { two = *o; } // parent instances first, the asked-for instance last
            return Ok(two);
        }
        let o = props.fields("BasePropertyOverrides");
        overrides.push(o.filter(|o| o.bool("bOverride_TwoSided") == Some(true)).map(|o| o.bool("TwoSided").unwrap_or(false)));
        cur = props.get("Parent").and_then(|v| obj_path(&pkg, v));
    }
    Err(format!("{material}: material chain has no root Material"))
}

/// Result of walking a material's parent chain.
#[derive(Clone, Debug, Default)]
pub struct MaterialChain {
    /// Parameter name -> texture object path, children over parents.
    pub textures: HashMap<String, String>,
    /// The base `Material` the chain ended at (its expression graph is not evaluated) and that material's `BlendMode`.
    pub base_material: Option<String>,
    pub blend_mode: Option<String>,
}

/// A decoded texture (raw colour values of the largest available mip, like `fn_pose::loadout_material`). `cooked`: the package's stored mip chain as it is
/// (TEXTURE MEMORY, 2026-10-06): what UE uploads, for renderers that take it instead of generating mips from `rgba` (`fn_render::mesh::CookedTexture`).
#[derive(Clone, Debug)]
pub struct RgbaTexture { pub width: usize, pub height: usize, pub rgba: Vec<u8>, pub cooked: Option<std::sync::Arc<CookedChain>> }

/// TEXTURE MEMORY (2026-10-06): a texture's stored mips as cooked: the pixel format and, per available mip from the largest, (width, height, raw payload
/// cut to the mip's size: whole 4x4 blocks for a block format).
#[derive(Clone, Debug)]
pub struct CookedChain { pub format: fn_texture::PixelFormat, pub levels: Vec<(u32, u32, Vec<u8>)> }
impl CookedChain {
    /// The stored mips of `t` (slice 0); `None` when a mip's payload is shorter than its size or the format has no byte size.
    pub fn of(t: &fn_texture::Texture2D) -> Option<CookedChain> {
        let mut levels = vec![];
        for i in t.available_mips() {
            let m = &t.mips[i];
            let n = t.format.mip_bytes(m.width, m.height)?;
            levels.push((m.width as u32, m.height as u32, m.data()?.get(..n)?.to_vec()));
        }
        if levels.is_empty() { None } else { Some(CookedChain { format: t.format.clone(), levels }) }
    }
}

pub fn decode_texture(src: &ClipSourceDir, path: &str) -> Result<RgbaTexture, String> {
    let tex = loader::texture_export(src, path)?;
    if tex.num_slices != 1 { return Err(format!("{path}: not a plain 2D texture")); }
    let mip = tex.best_mip().ok_or_else(|| format!("{path}: no available mip"))?;
    let cooked = CookedChain::of(&tex).map(std::sync::Arc::new);
    if cooked.is_some() && cooked_only() { return Ok(RgbaTexture { width: tex.mips[mip].width as usize, height: tex.mips[mip].height as usize, rgba: Vec::new(), cooked }); }
    let img = tex.decode(mip, fn_texture::DecodeOptions::default()).map_err(|e| format!("{path}: {e}"))?;
    Ok(RgbaTexture { width: img.width, height: img.height, rgba: img.to_rgba8(), cooked })
}

thread_local! { static COOKED_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub fn cooked_only() -> bool { COOKED_ONLY.with(|c| c.get()) }
pub fn with_cooked_only<R>(f: impl FnOnce() -> R) -> R {
    let prev = COOKED_ONLY.with(|c| c.replace(true));
    let r = f();
    COOKED_ONLY.with(|c| c.set(prev));
    r
}

/// The four slots of the renderer's character material, filled from a material's parameters `Diffuse`, `Normals`, `SpecularMasks`, `M`.
#[derive(Clone, Debug, Default)]
pub struct SectionTextures {
    /// `Some(reason)`: do not draw this section (a textureless special material whose look is an expression graph / blend mode this port
    /// does not evaluate, e.g. a masked "invisible" material or a modulate eye-shadow overlay).
    pub hidden: Option<String>,
    pub material: Option<String>, pub diffuse: Option<RgbaTexture>, pub normal: Option<RgbaTexture>, pub spec: Option<RgbaTexture>, pub mask: Option<RgbaTexture>, pub missing: Vec<String>,
    /// BR CHARACTER MATERIAL HOOK: the original compiled material of this section (head / hair / body skin permutation) with its evaluated constant buffer.
    pub ue: Option<crate::br_material::CharSection>,
    /// BR CHARACTER MATERIAL HOOK: this section is the eye-region `Eyeshadow` modulate overlay (drawn with its original shader after the scene).
    pub eyeshadow: Option<crate::br_material::Eyeshadow>,
    /// The section material's effective `TwoSided` flag ([`material_two_sided`]): both faces cast shadows.
    pub two_sided: bool }

#[derive(Clone, Debug)]
pub struct BrPart {
    pub def: PartDef,
    pub part: Part,
    /// Effective material path per primitive (mesh slot, or the part's override of that slot).
    pub section_materials: Vec<Option<String>>,
    pub sections: Vec<SectionTextures>,
}

#[derive(Debug, Default)]
pub struct BrCharacter {
    pub hero: String,
    /// Body first, head second (what `Scene` draws), then any further parts (hats ...: not drawn, see `undrawn`).
    pub parts: Vec<BrPart>,
    pub master: Option<Part>,
    pub female: bool,
    /// Things the data has but the port does not use, and things that could not be recovered.
    pub notes: Vec<String>,
}

/// Bone-compatibility of a part's skeleton against the master it follows.
#[derive(Debug, Default, PartialEq)]
pub struct BoneCheck {
    pub bones: usize,
    /// Bones the master lacks (the head's face rig). Fine only when each is attached (through its parents) to a shared bone: they then ride
    /// on the master pose at their bind transform, which is what the animation graph does for bones no clip drives.
    pub extra_bones: Vec<String>,
    /// Extra bones whose whole parent chain never reaches a shared bone (not attached to the master at all).
    pub detached_extra: Vec<String>,
    pub parent_mismatch: Vec<String>,
    pub max_bind_translation_delta_m: f32,
    pub worst_bind_bone: String,
    pub max_bind_rotation_delta: f32,
}
impl BoneCheck { pub fn compatible(&self) -> bool { self.detached_extra.is_empty() && self.parent_mismatch.is_empty() } }

/// A part's bones by name against the master's: existence, parent name, bind pose deltas. Animation clips address bones by name, so this is
/// what decides whether a part can follow the master pose.
pub fn bone_check(part: &Part, master: &Part) -> BoneCheck {
    let mut c = BoneCheck { bones: part.tree.len(), ..Default::default() };
    for (i, name) in part.tree.names.iter().enumerate() {
        let Some(mi) = master.tree.bone_index(name) else {
            if i == 0 { continue } // the mesh node
            c.extra_bones.push(name.clone());
            let (mut cur, mut attached) = (part.tree.parents[i], false);
            while let Some(p) = cur { if p != 0 && master.tree.bone_index(&part.tree.names[p]).is_some() { attached = true; break; } cur = part.tree.parents[p]; }
            if !attached { c.detached_extra.push(name.clone()); }
            continue
        };
        let pn = part.tree.parents[i].map(|p| part.tree.names[p].as_str());
        let mn = master.tree.parents[mi].map(|p| master.tree.names[p].as_str());
        // the mesh node (index 0, no parent in the part) is the glTF root; compare only real bones
        // (a parent index of 0 is the mesh node on both sides: its name is the mesh's own)
        let p_mesh = part.tree.parents[i] == Some(0); let m_mesh = master.tree.parents[mi] == Some(0);
        if i != 0 && !(p_mesh && m_mesh) && pn != mn { c.parent_mismatch.push(format!("{name}: {pn:?} vs {mn:?}")); }
        let (a, b) = (part.tree.local[i], master.tree.local[mi]);
        let d = (a.0 - b.0).length();
        if d > c.max_bind_translation_delta_m { c.max_bind_translation_delta_m = d; c.worst_bind_bone = name.clone(); }
        c.max_bind_rotation_delta = c.max_bind_rotation_delta.max(1. - a.1.dot(b.1).abs());
    }
    c
}

impl BrCharacter {
    /// Loads the hero's parts (meshes, materials, textures) from raw packages / the pak. Any missing piece is an `Err` naming it; the
    /// caller keeps the current character then.
    pub fn load(src: &ClipSourceDir, hid: &str) -> Result<BrCharacter, String> {
        let load_mesh = |long: &str, name: &str| -> Result<(Part, fn_mesh::SkeletalMesh), String> {
            let pkg = loader::package(src, long)?;
            let mesh = fn_mesh::SkeletalMesh::parse(&pkg).map_err(|e| format!("{long}: {e}"))?;
            Ok((Part::from_skeletal_mesh(name, &mesh, 0)?, mesh))
        };
        let mut ch = BrCharacter { hero: hid.to_owned(), ..Default::default() };
        let mut tex_cache: HashMap<String, Option<RgbaTexture>> = HashMap::new();
        // BR CHARACTER MATERIAL HOOK: the parts' colour swatches are parameter overrides by name on every material (the engine sets them on the dynamic instances)
        let defs: Vec<PartDef> = hero_part_packages(src, hid)?.iter().map(|p| read_part_def(src, p)).collect::<Result<_, _>>()?;
        ch.female = is_female(&defs);
        let master_path = master_mesh_for(&defs);
        let (master, _) = load_mesh(long_of(&master_path), master_path.rsplit('.').next().unwrap_or("Fortnite_M_Avg_Player"))?;
        if master_path != MASTER_MESH_PKG { ch.notes.push(format!("master mesh swapped to the Body part's MasterSkeletalMeshes[0] {master_path} (FortPlayerPawn.cpp:0x1ea path, FUN_1407fccd0)")); }
        let swatches = crate::br_material::swatch_overrides(src, &defs.iter().collect::<Vec<_>>(), &mut ch.notes);
        for def in defs {
            // def.master_meshes names the mesh the part follows in the editor (the body lists SK_M_MALE_Base_Skeleton, the head the
            // Fortnite_M_Avg_Player master); whether it can be posed by the master's animation is decided by bone_check below.
            let long = long_of(&def.skeletal_mesh).to_owned();
            let obj = def.skeletal_mesh.rsplit('.').next().unwrap_or("part").to_owned();
            let (part, mesh) = load_mesh(&long, &obj)?;
            let part = part.with_master_ik_bones(&master.tree);
            if part.prims.len() != mesh.lods[0].sections.len() { return Err(format!("{long}: {} primitives for {} sections", part.prims.len(), mesh.lods[0].sections.len())); }
            let mut section_materials = vec![];
            for s in 0..part.prims.len() {
                let slot = mesh.section_material_index(0, s);
                let over = def.material_overrides.iter().find(|(i, _)| *i == slot).map(|(_, m)| m.clone());
                section_materials.push(over.or_else(|| mesh.materials.get(slot).and_then(|m| m.material.clone())));
            }
            let mut sections = vec![];
            for m in &section_materials {
                let mut st = SectionTextures { material: m.clone(), ..Default::default() };
                if let Some(mp) = m { match material_two_sided(src, mp) { Ok(t) => st.two_sided = t, Err(e) => ch.notes.push(format!("{mp}: TwoSided not resolved ({e}), one-sided assumed")) } }
                match m {
                    None => st.missing.push("section has no material".into()),
                    Some(mp) => match effective_texture_params(src, mp) {
                        Err(e) => st.missing.push(e),
                        Ok(chain) => {
                            let params = chain.textures.clone();
                            let mut get = |param: &str, st_missing: &mut Vec<String>| -> Option<RgbaTexture> {
                                let Some(path) = params.get(param) else { st_missing.push(format!("{mp}: no {param} texture parameter")); return None };
                                let e = tex_cache.entry(path.clone()).or_insert_with(|| match decode_texture(src, path) { Ok(t) => Some(t), Err(_) => None });
                                if e.is_none() { st_missing.push(format!("{path}: texture did not decode")); }
                                e.clone()
                            };
                            let mut miss = vec![];
                            st.diffuse = get("Diffuse", &mut miss);
                            st.normal = get("Normals", &mut miss);
                            st.spec = get("SpecularMasks", &mut miss);
                            st.mask = get("M", &mut miss);
                            if st.diffuse.is_none() && st.normal.is_none() && st.spec.is_none() && st.mask.is_none() && params.is_empty() {
                                // BR CHARACTER MATERIAL HOOK: a textureless plain material is judged by its compiled shaders, not by the missing textures
                                let base_long = chain.base_material.as_deref().map(|b| long_of(b).to_owned());
                                match base_long.as_deref() {
                                    Some(b) if crate::br_material::ALWAYS_DISCARD.contains(&b) => st.hidden = Some(format!("{mp}: every coverage shader of {b} (depth pre-pass, shadow depth, hit proxy, velocity, lightmap density) discards unconditionally (discard_nz l(-1)); its base-pass shaders carry no clip: nothing is drawn")),
                                    Some(crate::br_material::EYESHADOW_MATERIAL) => match crate::br_material::eyeshadow(src) {
                                        Ok(e) => st.eyeshadow = Some(e),
                                        Err(e) => st.hidden = Some(format!("{mp}: eyeshadow modulate overlay could not be loaded: {e}")),
                                    },
                                    _ => st.hidden = Some(format!("{mp}: no texture parameters anywhere in its chain; base {:?} BlendMode {:?} (not a recovered material: not drawn)", chain.base_material, chain.blend_mode)),
                                }
                                miss.clear();
                            } else {
                                match crate::br_material::resolve_char_material(src, mp, &swatches) {
                                    Ok(Some(c)) => st.ue = Some(c),
                                    Ok(None) => {}
                                    Err(e) => miss.push(format!("{mp}: original material evaluation unavailable: {e}")),
                                }
                            }
                            st.missing = miss;
                        }
                    },
                }
                sections.push(st);
            }
            let check = bone_check(&part, &master);
            if !check.compatible() { return Err(format!("{}: skeleton does not follow the master: {:?}", def.package, check)); }
            ch.parts.push(BrPart { def, part, section_materials, sections });
        }
        // body first, head second, the rest after (stable)
        ch.parts.sort_by_key(|p| match p.def.kind { PartKind::Body => 0, PartKind::Head => 1, _ => 2 });
        if ch.parts.first().map(|p| p.def.kind) != Some(PartKind::Body) { return Err(format!("{hid}: no Body part")); }
        if ch.parts.get(1).map(|p| p.def.kind) != Some(PartKind::Head) { return Err(format!("{hid}: no Head part")); }
        for p in ch.parts.iter().skip(2) { ch.notes.push(format!("{}: {:?} part recovered but not drawn (the scene draws body + head)", p.def.package, p.def.kind)); }
        for p in &ch.parts {
            if let Some(a) = &p.def.anim_class { ch.notes.push(format!("{}: AnimClass {a} (its face / physics AnimBP is not evaluated: the master pose drives the part)", p.def.package)); }
            if p.def.skin_swatch.is_some() || p.def.hair_swatch.is_some() { ch.notes.push(format!("{}: skin/hair colour swatches {:?} / {:?} are applied as parameter overrides by name (see the swatch lines above)", p.def.package, p.def.skin_swatch, p.def.hair_swatch)); }
            for (i, s) in p.sections.iter().enumerate() {
                if let Some(u) = &s.ue { ch.notes.push(format!("{} section {i}: original {} base-pass shader of {} ({} constant registers)", p.def.package, u.perm, u.owner, u.cb.len())); }
                if s.eyeshadow.is_some() { ch.notes.push(format!("{} section {i}: Eyeshadow modulate overlay (original shader)", p.def.package)); }
            }
            for s in &p.sections {
                if let Some(h) = &s.hidden { ch.notes.push(format!("{}: section not drawn: {h}", p.def.package)); }
                for m in &s.missing { ch.notes.push(format!("{}: MISSING {m}", p.def.package)); }
            }
        }
        ch.master = Some(master);
        Ok(ch)
    }

    /// The rig parts (`[body, head, ...]` skeletons).
    pub fn rig_parts(&self) -> Vec<Part> { self.parts.iter().take(2).map(|p| p.part.clone()).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn install() -> Option<fn_pak::PakSet> {
        let root = fn_live::var("FN_ROOT").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        fn_pak::PakSet::open_install(root).ok()
    }
    #[test]
    fn default_hero_parts_match_the_packages() {
        let Some(set) = install() else { eprintln!("1.8 install not found: skipped"); return };
        let src = ClipSourceDir::Pak(&set);
        let parts = hero_part_packages(&src, DEFAULT_HERO).unwrap();
        assert_eq!(parts, vec!["/Game/Athena/Heroes/Meshes/Heads/M_Med_Soldier_Head_01_ATH.M_Med_Soldier_Head_01_ATH", "/Game/Athena/Heroes/Meshes/Bodies/CP_005_Athena_Body.CP_005_Athena_Body"]);
        let ch = BrCharacter::load(&src, DEFAULT_HERO).unwrap();
        assert_eq!(ch.parts.len(), 2);
        let (body, head) = (&ch.parts[0], &ch.parts[1]);
        assert_eq!((body.def.kind, head.def.kind), (PartKind::Body, PartKind::Head));
        assert_eq!(body.def.skeletal_mesh, "/Game/Athena/Heroes/Meshes/Bodies/SK_M_Med_Soldier_04_ATH.SK_M_Med_Soldier_04_ATH");
        assert_eq!(head.def.skeletal_mesh, "/Game/Athena/Heroes/Meshes/Heads/SK_M_MED_Head_01_ATH.SK_M_MED_Head_01_ATH");
        assert_eq!(body.def.material_overrides, vec![(0, "/Game/Characters/Player/Male/Medium/Bodies/M_Med_Soldier_04/Materials/BR01/M_Med_Soldier_04_CAU_BR01.M_Med_Soldier_04_CAU_BR01".to_string())]);
        // section / material counts straight from the mesh packages
        for p in &ch.parts {
            let pkg = loader::package(&src, long_of(&p.def.skeletal_mesh)).unwrap();
            let mesh = fn_mesh::SkeletalMesh::parse(&pkg).unwrap();
            assert_eq!(p.part.prims.len(), mesh.lods[0].sections.len());
            assert_eq!(p.section_materials.len(), p.part.prims.len());
            assert_eq!(p.sections.len(), p.part.prims.len());
            eprintln!("{}: {} sections, {} material slots, {} bones, materials {:?}, missing {:?}", p.part.name, p.part.prims.len(), mesh.materials.len(), p.part.tree.len(), p.section_materials, p.sections.iter().map(|s| &s.missing).collect::<Vec<_>>());
        }
        // body uses the override for slot 0, textures inherited from M_Med_Soldier_04 (all four parameters present)
        let s0 = &body.sections[0];
        assert!(s0.material.as_deref().unwrap().contains("M_Med_Soldier_04_CAU_BR01"), "{:?}", s0.material);
        assert!(s0.diffuse.is_some() && s0.normal.is_some() && s0.spec.is_some() && s0.mask.is_some(), "{:?}", s0.missing);
    }
    #[test]
    fn parts_follow_the_master_skeleton() {
        let Some(set) = install() else { return };
        let src = ClipSourceDir::Pak(&set);
        let ch = BrCharacter::load(&src, DEFAULT_HERO).unwrap();
        let master = ch.master.as_ref().unwrap();
        for p in &ch.parts {
            let c = bone_check(&p.part, master);
            eprintln!("{}: {:?}", p.part.name, c);
            assert!(c.compatible(), "{:?}", c);
            // the master's IK bones were appended and are not skinned
            assert!(p.part.tree.bone_index("ik_hand_l").is_some());
            assert!(p.part.tree.bone_index("pelvis").is_some());
            // the bind pose of every shared bone: translations in metres agree to a millimetre-ish
            assert!(c.max_bind_translation_delta_m < 0.5 && c.max_bind_rotation_delta < 0.1, "{:?}", c);
        }
    }
}
