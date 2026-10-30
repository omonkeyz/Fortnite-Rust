//! PICKUP ITEMS (fn-game): the Battle Royale item definitions a world pickup can carry, read from the 1.8 paks (no GPU, no invented data).
//!
//! `FortWorldItemDefinition` (SDK `FN_FortniteGame_classes.hpp`, class at 0x240..0x440) is what the pickup actor reads: `PickupStaticMesh` / `PickupSkeletalMesh` (the displayed mesh;
//! exactly one is set), `PickupSound` (played when it is picked up), `DropSound` (when it is dropped), `PickupMeshTransform` + `bOverridePickupMeshTransform`, `Rarity`, `MaxStackSize`,
//! `DropCount`, `DisplayName`. Scanned: `/Game/Athena/Items/{Weapons,Consumables,Ammo,Traps}` and the three build resources `/Game/Items/ResourcePickups/{Wood,Stone,Metal}ItemData`.
use fn_pak::PakSet;
use fn_uasset::PackageData;
use serde_json::Value;
use std::sync::Arc;

/// What the sandbox does with the item when the pawn takes it (the delivery side is the front-end's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gameplay {
    /// A firearm / melee weapon definition (`WID_*`).
    Weapon,
    /// One of `fn_pose::loadout_stats::HEALABLE_ITEMS` (index in that order).
    Healable(usize),
    /// An ammo definition (`AthenaAmmoData*`).
    Ammo,
    /// Wood / stone / metal.
    Resource(Resource),
    Trap,
    /// A trap, a grenade, a bush ...: no gameplay in the sandbox (shown, but it cannot be held).
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource { Wood, Stone, Metal }

/// `PickupMeshTransform` (a cooked `FTransform`: quaternion, translation (cm), scale).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshTransform { pub rotation: [f32; 4], pub translation: [f32; 3], pub scale: [f32; 3] }

#[derive(Clone, Debug)]
pub struct ItemDef {
    /// Asset name (`WID_Assault_Auto_Athena_R_Ore_T03`, `Athena_Bandage`, `AthenaAmmoDataBulletsMedium`, `WoodItemData`).
    pub id: String,
    /// `/Game/...` package path.
    pub package: String,
    pub name: String,
    /// `EFortRarity` byte as the SDK enum numbers it (Handmade 0, Ordinary 1, Sturdy 2, Quality 3, Fine 4, Elegant 5, Masterwork 6, Epic 7, Badass 8, Legendary 9).
    pub rarity: u8,
    /// `EFortItemType` (1 Ammo, 14 Trap, 16 WeaponMelee, 17 WeaponRanged, 18 WeaponHarvest, 19 WorldResource, 0 otherwise / unknown).
    pub item_type: u8,
    pub class: String,
    pub max_stack: u32,
    pub drop_count: u32,
    pub static_mesh: Option<String>,
    pub skeletal_mesh: Option<String>,
    pub pickup_sound: Option<String>,
    pub drop_sound: Option<String>,
    pub mesh_transform: Option<MeshTransform>,
    pub gameplay: Gameplay,
    pub small_icon: Option<String>,
}

pub const RARITY_NAMES: [&str; 10] = ["Handmade", "Ordinary", "Sturdy", "Quality", "Fine", "Elegant", "Masterwork", "Epic", "Badass", "Legendary"];
/// Rarity names of the HUD / the console (Common .. Legendary for the five BR tiers).
pub fn rarity_label(r: u8) -> &'static str { match r { 0 => "Common", 1 => "Uncommon", 2 => "Rare", 3 => "Epic", 4 => "Legendary", 5 => "Mythic", 6 => "Transcendent", _ => "Unknown" } }
pub fn rarity_of(v: &str) -> u8 { let t = v.rsplit("::").next().unwrap_or(v); RARITY_NAMES.iter().position(|n| *n == t).unwrap_or(0) as u8 }

const HEALABLES: [&str; 5] = ["Athena_Bandage", "Athena_Medkit", "Athena_Shields", "Athena_PurpleStuff", "Athena_Grenade"];
const ROOTS: [&str; 6] = [
    "FortniteGame/Content/Athena/Items/Weapons/WID_*.uasset",
    "FortniteGame/Content/Athena/Items/Consumables/**/*.uasset",
    "FortniteGame/Content/Athena/Items/Ammo/*.uasset",
    "FortniteGame/Content/Athena/Items/Traps/*.uasset",
    "FortniteGame/Content/Items/ResourcePickups/WoodItemData.uasset",
    "FortniteGame/Content/Items/ResourcePickups/*ItemData.uasset",
];

fn path_of(v: Option<&Value>) -> Option<String> { v.and_then(|x| x.as_str()).filter(|s| s.starts_with('/') && !s.is_empty()).map(str::to_owned) }
fn text_of(v: Option<&Value>) -> Option<String> { v?.as_object()?.get("text")?.as_str().map(|s| s.trim().to_owned()) }
fn f(v: Option<&Value>) -> f32 { v.and_then(|x| x.as_f64()).unwrap_or(0.) as f32 }
fn arr3(v: Option<&Value>, d: [f32; 3]) -> [f32; 3] { v.and_then(|x| x.as_array()).filter(|a| a.len() >= 3).map(|a| [f(a.first()), f(a.get(1)), f(a.get(2))]).unwrap_or(d) }

fn read_one(set: &PakSet, long: &str) -> Option<ItemDef> {
    let data = PackageData::load(set, long).ok()?;
    let pkg = data.package().ok()?;
    let file = long.rsplit('/').next().unwrap_or(long);
    let idx = (0..pkg.exports.len()).find(|&i| pkg.exports[i].name == file)?;
    let class = pkg.export_class_name(idx);
    if !class.contains("ItemDefinition") { return None; }
    let p = pkg.read_export(idx).ok()?.to_json(&pkg).get("properties").cloned().unwrap_or(Value::Null);
    let rarity = p.get("Rarity").and_then(|v| v.as_str()).map(rarity_of).unwrap_or(0);
    let item_type = match p.get("ItemType").and_then(|v| v.as_str()).map(|s| s.rsplit("::").next().unwrap_or(s)) {
        Some(t) => ["WorldItem", "Ammo", "Badge", "BackpackPickup", "BuildingPiece", "CharacterPart", "Deco", "EditTool", "Ingredient", "Food", "Gadget", "HomebaseGadget", "HeroAbility", "MissionItem", "Trap", "Weapon", "WeaponMelee", "WeaponRanged", "WeaponHarvest", "WorldResource"].iter().position(|n| *n == t).unwrap_or(0) as u8,
        None => if class.contains("Ranged") { 17 } else if class.contains("Harvest") { 18 } else if class.contains("Melee") { 16 } else if class.contains("Ammo") { 1 } else if class.contains("Resource") { 19 } else if class.contains("Trap") || file.starts_with("TID_") { 14 } else { 0 },
    };
    let gameplay = if file.starts_with("WID_") { Gameplay::Weapon }
        else if let Some(i) = HEALABLES.iter().position(|h| *h == file) { Gameplay::Healable(i) }
        else if class.contains("Ammo") { Gameplay::Ammo }
        else if file.starts_with("TID_") && class.contains("Trap") { Gameplay::Trap }
        else if file == "WoodItemData" { Gameplay::Resource(Resource::Wood) } else if file == "StoneItemData" { Gameplay::Resource(Resource::Stone) } else if file == "MetalItemData" { Gameplay::Resource(Resource::Metal) }
        else { Gameplay::Other };
    let mesh_transform = p.get("PickupMeshTransform").and_then(|t| t.as_object()).map(|t| {
        let r = t.get("Rotation").and_then(|x| x.as_array()).filter(|a| a.len() >= 4).map(|a| [f(a.first()), f(a.get(1)), f(a.get(2)), f(a.get(3))]).unwrap_or([0., 0., 0., 1.]);
        MeshTransform { rotation: r, translation: arr3(t.get("Translation"), [0.; 3]), scale: arr3(t.get("Scale3D"), [1.; 3]) }
    });
    let overridden = p.get("bOverridePickupMeshTransform").and_then(|v| v.as_bool()).unwrap_or(false);
    Some(ItemDef {
        id: file.to_owned(), package: long.to_owned(),
        name: text_of(p.get("DisplayName")).filter(|s| !s.is_empty()).unwrap_or_else(|| file.to_owned()),
        rarity, item_type, class,
        max_stack: p.get("MaxStackSize").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
        drop_count: p.get("DropCount").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
        static_mesh: path_of(p.get("PickupStaticMesh")), skeletal_mesh: path_of(p.get("PickupSkeletalMesh")),
        pickup_sound: path_of(p.get("PickupSound")), drop_sound: path_of(p.get("DropSound")),
        mesh_transform: if overridden { mesh_transform } else { None },
        gameplay,
        small_icon: path_of(p.get("SmallPreviewImage")),
    })
}

/// Every BR world item definition of the install (sorted by id). Takes a second or two on a cold pak set: call it from a worker thread.
pub fn scan(set: &PakSet) -> Vec<ItemDef> {
    let mut longs: Vec<String> = vec![];
    for pat in ROOTS {
        for (_, e) in set.glob(pat) {
            if let Some(g) = fn_pak::pak_path_to_game(e.rel_path()).or_else(|| fn_pak::pak_path_to_game(&e.path)) { longs.push(g.trim_end_matches(".uasset").to_owned()); }
        }
    }
    longs.sort(); longs.dedup();
    let mut out: Vec<ItemDef> = longs.iter().filter_map(|l| read_one(set, l)).collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn norm(s: &str) -> String { s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect() }

/// The one item a query names: the asset name (any case, `WID_` optional, or a full package path), else the display name; ambiguity is an error that lists the candidates.
pub fn resolve<'a>(items: &'a [ItemDef], query: &str) -> Result<&'a ItemDef, String> {
    let q = query.trim().trim_matches('"');
    let q = q.rsplit('/').next().unwrap_or(q);
    let q = q.split('.').next().unwrap_or(q);
    let nq = norm(q);
    if nq.is_empty() { return Err("missing item".into()); }
    let pick = |hits: Vec<&'a ItemDef>, what: &str| -> Option<Result<&'a ItemDef, String>> {
        match hits.len() { 0 => None, 1 => Some(Ok(hits[0])), n => Some(Err(format!("'{}' matches {n} items by {what}: {}", query.trim(), hits.iter().take(10).map(|i| format!("{} [{}]", i.id, rarity_label(i.rarity))).collect::<Vec<_>>().join(", ")))) }
    };
    if let Some(r) = pick(items.iter().filter(|i| { let n = norm(&i.id); n == nq || n.strip_prefix("wid") == Some(&nq) }).collect(), "asset name") { return r; }
    if let Some(r) = pick(items.iter().filter(|i| norm(&i.name) == nq).collect(), "display name") { return r; }
    if let Some(r) = pick(items.iter().filter(|i| norm(&i.id).contains(&nq) || norm(&i.name).contains(&nq)).collect(), "name") { return r; }
    Err(format!("no item matches '{}' (try a weapon / consumable / ammo / resource name or its asset name)", query.trim()))
}

/// A shared, lazily scanned catalog of the process (needs the cosmetic catalog's pak set).
pub fn catalog() -> Result<Arc<Vec<ItemDef>>, String> {
    static C: std::sync::OnceLock<Result<Arc<Vec<ItemDef>>, String>> = std::sync::OnceLock::new();
    C.get_or_init(|| { let set = crate::cosmetic_swap::paks()?; Ok(Arc::new(scan(&set))) }).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn set() -> Option<PakSet> {
        let root = fn_live::var_os("FN_TEST_ROOT").map(std::path::PathBuf::from)?;
        let dir = fn_pak::find_paks_dir(&root)?;
        PakSet::open_install(&dir).ok()
    }
    /// The real definitions: every sandbox-relevant item has a pickup mesh and sound, rarities / types come from the data.
    #[test] fn the_real_item_definitions() {
        let Some(set) = set() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let items = scan(&set);
        eprintln!("{} items", items.len());
        for i in &items { eprintln!("{:48} {:12} type {:2} stack {:4} drop {:3} sm={:?} sk={:?} snd={:?}", i.id, rarity_label(i.rarity), i.item_type, i.max_stack, i.drop_count, i.static_mesh.as_deref().map(|s| s.rsplit('/').next().unwrap_or(s)), i.skeletal_mesh.as_deref().map(|s| s.rsplit('/').next().unwrap_or(s)), i.pickup_sound.as_deref().map(|s| s.rsplit('/').next().unwrap_or(s))); }
        { let mut snd: Vec<String> = items.iter().flat_map(|i| [i.pickup_sound.clone(), i.drop_sound.clone()]).flatten().collect(); snd.sort(); snd.dedup(); for s in snd { eprintln!("SOUND {s}"); } for i in items.iter().filter(|i| i.mesh_transform.is_some()) { eprintln!("XFORM {} {:?}", i.id, i.mesh_transform); } }
        let ar = resolve(&items, "WID_Assault_Auto_Athena_R_Ore_T03").unwrap();
        assert_eq!(ar.name, "Assault Rifle"); assert_eq!(ar.rarity, 2); assert_eq!(ar.item_type, 17);
        assert_eq!(ar.skeletal_mesh.as_deref(), Some("/Game/Weapons/FORT_Rifles/Mesh/SK_MachineGun_01.SK_MachineGun_01"));
        assert!(ar.pickup_sound.as_deref().unwrap().ends_with("AR_Pickup_Cue"));
        let b = resolve(&items, "bandage").unwrap();
        assert_eq!(b.gameplay, Gameplay::Healable(0)); assert_eq!(b.static_mesh.as_deref(), Some("/Game/Items/Art_noLOD/Meshes/MedBandage.MedBandage"));
        let w = resolve(&items, "wood").unwrap(); assert_eq!(w.gameplay, Gameplay::Resource(Resource::Wood)); assert_eq!(w.item_type, 19);
        let a = resolve(&items, "AthenaAmmoDataBulletsMedium").unwrap(); assert_eq!(a.item_type, 1); assert_eq!(a.drop_count, 12);
        assert!(resolve(&items, "assault rifle").is_err(), "several rarities share the display name");
        assert!(items.iter().filter(|i| i.item_type == 17).all(|i| i.skeletal_mesh.is_some() || i.static_mesh.is_some()), "every ranged weapon has a pickup mesh");
    }
}
