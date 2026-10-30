//! BR (Athena) consumables / utility items (Bandage, Med Kit, Shield Potion, Slurp Juice, Bush, Grenade, M80): the 1.8 data path from the item definition to the
//! core animation set, equip montage, use montage (+ notifies, use time) and held prop, resolved by FULL package path. Battle Royale only.
//!
//! Chain (every link is a serialized 1.8 property):
//!  * item definition `/Game/Athena/Items/Consumables/<Item>/Athena_<Item>` (`FortWeaponRangedItemDefinition`) `.WeaponActorClass`
//!    = `/Game/Abilities/Player/Generic/UtilityItems/B_UtilityItem_Generic_Athena` -> `B_UtilityItem_Generic` -> native `/Script/FortniteGame.FortWeapon`.
//!  * `WeaponCoreAnimation`: the `B_UtilityItem_Generic` CDO serializes `EFortWeaponCoreAnimation::TrapPlacement` (EXACT, data). 1.8 has NO `Unarmed` key in
//!    `CoreAnims_Male`; `TrapPlacement` IS the unarmed hold: targeting pose `M_NoWep_Relaxed`, aim offset `M_NoWep_Relaxed_AO`, `M_Jog_NoWep_Additive`,
//!    `Sprint_Default`, `Crouch_NoWep_CMM`, `Crouch_NoWep_ADD`, `CrouchSprint_NoWeapon` (the same set the trap tools use). Resolved by [`crate::loadout_weapon`].
//!  * Equip montage: neither `B_UtilityItem_Generic_Athena` nor `B_UtilityItem_Generic` serializes `EquipAnimation` / `WeaponEquipMontage` and the item
//!    definitions carry none, so the data has NO equip montage for any consumable ([`ConsumableSet::equip_montage`] = `None`, listed in [`ConsumableSet::missing`]).
//!  * Use montage: item `.PrimaryFireAbility` (`GA_Athena_*`, `GA_Weapon_M80`) CDO `.MontageInfo.MontageToPlay` (the struct the ability bytecode hands
//!    `FortAbilityTask_PlayMontageWaitTarget::PlayMontageInfoAndWaitForTrigger`; `AnimMontageInfo` with its 0.75 rate is a dead variable, as in `loadout_stats`).
//!    `MontageInfo` serializes only the montage, so the play rate is the struct default 1.0 (INFERRED: the `FortGameplayAbilityMontageInfo` constructor was not traced).
//!  * Use time: ability CDO `TriggerDuration` (heals) = the `GameplayCue.Athena.Equipping` ring; the montage's `TriggerAbility` notify time is when the effect is applied.
//!    Throwables have no `TriggerDuration`: their montage's notifies drive them.
//!  * Held prop: the generic actor's `WeaponMesh0` component serializes no mesh and the definitions have no `WeaponMeshOverride`; the montages carry a
//!    `HolsterWeapon` AnimNotifyState (the previous weapon is hidden while using). `held_prop` is `None` when no mesh is serialized anywhere (reported, never invented).
use crate::{loader, loadout_weapon, ClipSourceDir};
use fn_anim::{montage::AnimMontage, Notify};
use fn_ueio::{props::{read_props, Ctx, PropList, PropValue}, Package, Reader};

/// Roots of Save the World content (see `loadout_melee::STW_ROOTS`; `/Game/Abilities/Player/Generic/` is shared by BR and is NOT STW).
pub use crate::loadout_melee::{is_stw, STW_ROOTS};

#[derive(Debug, Clone)]
pub struct UseNotify { pub name: String, pub time: f32, pub duration: f32, pub class: Option<String>, pub sound: Option<String> }
/// An `AnimNotifyState_AttachAsset_C` of the use montage: a static mesh attached to `socket` of the pawn for `duration` seconds from `start` (the held prop, exact data).
#[derive(Debug, Clone)]
pub struct AttachAsset { pub socket: String, pub mesh: String, pub start: f32, pub duration: f32 }

#[derive(Debug)]
pub struct ConsumableSet {
    /// Item definition package (`/Game/Athena/Items/Consumables/Bandage/Athena_Bandage`) and short name.
    pub item: String, pub name: String,
    /// `WeaponActorClass` object path and the Blueprint chain (most derived first) ending with the native class.
    pub weapon_actor_class: String, pub actor_chain: Vec<String>,
    /// `EFortWeaponCoreAnimation` key, its provenance, and the `CoreAnims_Male[core]` assets (field -> full path, None = explicit null in the data).
    pub core: String, pub core_source: String, pub core_assets: Vec<(String, Option<String>)>,
    /// `EquipAnimation` / `WeaponEquipMontage` of the actor chain: `None` = absent in the data.
    pub equip_montage: Option<String>,
    /// `PrimaryFireAbility` package, use montage (full package path), its parsed montage and `MontageInfo.AnimPlayRate` (None = not serialized -> 1.0, inferred).
    pub ability: String, pub use_montage: String, pub use_montage_data: AnimMontage, pub use_rate: Option<f32>,
    /// Ability CDO `TriggerDuration` (None for throwables).
    pub trigger_duration: Option<f32>,
    /// Every sequence the use montage plays (full paths) and its notifies (montage time = seconds on the montage's own timeline).
    pub use_sequences: Vec<String>, pub notifies: Vec<UseNotify>,
    /// `TriggerAbility` notify time (when the ability effect is applied), if the montage has one.
    pub trigger_ability_time: Option<f32>,
    /// A serialized held mesh (`WeaponMeshOverride` of the item or a mesh on the generic actor's `WeaponMesh0`): `None` = the data has none.
    pub held_prop: Option<String>,
    /// `AnimNotifyState_AttachAsset_C` exports of the use montage (socket + static mesh + window); the grenade carries one, the heals none.
    pub attach_assets: Vec<AttachAsset>,
    /// `PickupStaticMesh` (world pickup only, NOT a held prop).
    pub pickup_mesh: Option<String>,
}
impl ConsumableSet {
    /// Montage play length in seconds at the effective rate.
    pub fn use_seconds(&self) -> f32 { self.use_montage_data.length / (self.use_rate.unwrap_or(1.0) * self.use_montage_data.rate_scale) }
    /// What the 1.8 data does not provide for this item (reported, never substituted).
    pub fn missing(&self) -> Vec<String> {
        let mut v = vec![];
        if self.equip_montage.is_none() { v.push("equip montage (EquipAnimation/WeaponEquipMontage not serialized in the actor chain nor item)".to_owned()); }
        if self.held_prop.is_none() { v.push("held prop mesh (no WeaponMeshOverride / WeaponMesh0 mesh / AttachAsset notify in the data)".to_owned()); }
        if self.use_rate.is_none() { v.push("MontageInfo.AnimPlayRate not serialized (1.0 struct default inferred)".to_owned()); }
        v
    }
    /// Every package path of the set (for the STW check and the loader queue).
    pub fn all_paths(&self) -> Vec<String> {
        let mut v = vec![self.item.clone(), self.ability.clone(), self.use_montage.clone()];
        v.push(self.weapon_actor_class.split('.').next().unwrap_or_default().to_owned());
        v.extend(self.actor_chain.iter().filter(|c| c.starts_with("/Game/")).cloned());
        v.extend(self.core_assets.iter().filter_map(|(_, p)| p.clone()));
        v.extend(self.use_sequences.iter().cloned());
        v.extend(self.held_prop.iter().cloned());
        v.extend(self.equip_montage.iter().cloned());
        v.sort(); v.dedup(); v
    }
    pub fn stw_paths(&self) -> Vec<String> { self.all_paths().into_iter().filter(|p| is_stw(p)).collect() }
}

fn pkg_of(r: &str) -> &str { r.split('.').next().unwrap_or(r) }
fn export_props(pkg: &Package, idx: usize) -> Result<PropList, String> {
    let (s, e) = pkg.export_range(idx);
    read_props(&mut Reader::at(&pkg.data[..e], s), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())
}
fn cdo(pkg: &Package) -> Result<PropList, String> {
    export_props(pkg, pkg.exports.iter().position(|e| e.object_name.starts_with("Default__")).ok_or("missing class default object")?)
}
fn obj(pkg: &Package, p: &PropList, key: &str) -> Option<String> {
    if let Some(t) = p.text(key).filter(|s| s.starts_with("/Game/")) { return Some(t.to_owned()); }
    p.object(key).filter(|i| *i < 0).map(|i| pkg.import_path(i))
}
fn item_props(pkg: &Package) -> Result<PropList, String> {
    let class = (0..pkg.exports.len()).map(|i| pkg.export_class(i)).find(|c| c.contains("ItemDefinition")).ok_or("no item definition export")?;
    export_props(pkg, pkg.find_export(&class).ok_or("item export")?)
}

/// Item definition packages of every Athena consumable: `/Game/Athena/Items/Consumables/<Dir>/Athena_<Name>` (data-driven pak glob).
pub fn consumable_items(src: &ClipSourceDir) -> Result<Vec<String>, String> {
    let ClipSourceDir::Pak(set) = src else { return Err("consumable_items needs the pak source".into()) };
    let mut v: Vec<String> = set.glob("fortnitegame/content/athena/items/consumables/*/athena_*.uasset").into_iter().filter_map(|(_, e)| {
        let p = e.path.trim_end_matches(".uasset");
        let lower = p.to_lowercase();
        let at = lower.find("fortnitegame/content/")? + "fortnitegame/content/".len();
        Some(format!("/Game/{}", &p[at..]))
    }).collect();
    v.sort(); v.dedup();
    if v.is_empty() { return Err("no Athena consumable item definitions in the paks".into()); }
    Ok(v)
}

/// The `CoreAnims_Male` key of a consumable item (what `loadout.rs` stores in `Item::core_animation`): chain value, else native constructor default.
pub fn core_for_item(src: &ClipSourceDir, item: &str) -> Result<String, String> {
    let pkg = loader::package(src, pkg_of(item))?;
    let ip = item_props(&pkg)?;
    let actor = obj(&pkg, &ip, "WeaponActorClass").ok_or("item has no WeaponActorClass")?;
    Ok(loadout_weapon::actor_chain(src, &actor)?.core()?.0)
}

fn notify_of(n: &Notify) -> UseNotify { UseNotify { name: n.notify_name.clone(), time: n.link_value, duration: n.duration, class: n.notify_class.clone().or_else(|| n.notify_object.clone()), sound: n.sound.clone() } }

pub fn load_for(src: &ClipSourceDir, item: &str) -> Result<ConsumableSet, String> {
    let item = pkg_of(item).to_owned();
    let pkg = loader::package(src, &item)?;
    let ip = item_props(&pkg)?;
    let weapon_actor_class = obj(&pkg, &ip, "WeaponActorClass").ok_or("item has no WeaponActorClass")?;
    let ability = obj(&pkg, &ip, "PrimaryFireAbility").ok_or("item has no PrimaryFireAbility")?;
    let chain = loadout_weapon::actor_chain(src, &weapon_actor_class)?;
    let (core, core_source) = chain.core()?;
    let anims = loadout_weapon::weapon_anims(src, &item, false)?;
    let core_assets = anims.core_assets.iter().map(|a| (a.field.clone(), a.path.clone())).collect();
    let equip_montage = match chain.object("EquipAnimation")? { Some(m) => Some(m), None => chain.object("WeaponEquipMontage")? }.map(|(p, _)| p);
    // held mesh: item WeaponMeshOverride, else a mesh property on a WeaponMesh component export of any chain link
    let mut held_prop = obj(&pkg, &ip, "WeaponMeshOverride");
    if held_prop.is_none() {
        'outer: for l in &chain.links {
            for (i, e) in l.pkg.exports.iter().enumerate() {
                if !e.object_name.contains("WeaponMesh") { continue; }
                let Ok(p) = export_props(&l.pkg, i) else { continue };
                for k in ["SkeletalMesh", "StaticMesh"] { if let Some(m) = obj(&l.pkg, &p, k) { held_prop = Some(m); break 'outer; } }
            }
        }
    }
    let apkg = loader::package(src, pkg_of(&ability))?;
    let ap = cdo(&apkg)?;
    let info = ap.fields("MontageInfo").ok_or("ability has no MontageInfo")?;
    let use_montage = pkg_of(&obj(&apkg, info, "MontageToPlay").ok_or("MontageInfo has no MontageToPlay")?).to_owned();
    let use_rate = info.get("AnimPlayRate").and_then(PropValue::as_f64).map(|v| v as f32);
    let trigger_duration = ap.float("TriggerDuration").map(|v| v as f32);
    let mpkg = loader::package(src, &use_montage)?;
    let use_montage_data = AnimMontage::parse(&mpkg).map_err(|e| format!("{use_montage}: {e}"))?;
    let mut use_sequences = vec![];
    for s in &use_montage_data.slots {
        for g in &s.segments { use_sequences.push(pkg_of(g.animation_path.as_deref().ok_or("use montage segment lacks external animation")?).to_owned()); }
    }
    use_sequences.dedup();
    let notifies: Vec<UseNotify> = use_montage_data.notifies.iter().map(notify_of).collect();
    let mut attach_assets = vec![];
    for (i, _) in mpkg.exports.iter().enumerate().filter(|(i, _)| mpkg.export_class(*i) == "AnimNotifyState_AttachAsset_C") {
        let p = export_props(&mpkg, i)?;
        let a = p.fields("Attachment").ok_or("AttachAsset notify has no Attachment")?;
        let mesh = obj(&mpkg, a, "StaticMeshAsset").ok_or("AttachAsset notify has no external StaticMeshAsset")?;
        let name = &mpkg.exports[i].object_name;
        let n = use_montage_data.notifies.iter().find(|n| n.notify_state_object.as_deref() == Some(name.as_str())).ok_or("AttachAsset state is not on the montage notify track")?;
        attach_assets.push(AttachAsset { socket: a.text("SocketName").ok_or("AttachAsset has no SocketName")?.to_owned(), mesh: pkg_of(&mesh).to_owned(), start: n.link_value, duration: n.duration });
    }
    if held_prop.is_none() { held_prop = attach_assets.first().map(|a| a.mesh.clone()); }
    let trigger_ability_time = notifies.iter().find(|n| n.name == "TriggerAbility").map(|n| n.time);
    let mut actor_chain: Vec<String> = chain.links.iter().map(|l| l.path.clone()).collect();
    actor_chain.push(chain.native.clone());
    Ok(ConsumableSet {
        name: item.rsplit('/').next().unwrap_or_default().to_owned(), item, weapon_actor_class, actor_chain, core, core_source, core_assets, equip_montage,
        ability: pkg_of(&ability).to_owned(), use_montage, use_montage_data, use_rate, trigger_duration, use_sequences, notifies, trigger_ability_time, held_prop, attach_assets,
        pickup_mesh: obj(&pkg, &ip, "PickupStaticMesh"),
    })
}

/// Every Athena consumable's resolved set.
pub fn load_all(src: &ClipSourceDir) -> Result<Vec<ConsumableSet>, String> { consumable_items(src)?.iter().map(|i| load_for(src, i)).collect() }

#[cfg(test)]
mod tests {
    use super::*;
    fn paks() -> fn_pak::PakSet { fn_pak::PakSet::open_install(fn_live::var_os("FN_TEST_ROOT").expect("set FN_TEST_ROOT to the local 1.8 install")).unwrap() }

    /// Data-driven over EVERY Athena consumable: unarmed (NoWep) core set from Athena data, a use montage that resolves, no STW asset.
    #[test]
    fn every_br_consumable_resolves_unarmed_core_and_use_montage() {
        let p = paks();
        let src = ClipSourceDir::Pak(&p);
        let all = load_all(&src).unwrap();
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Athena_Bandage", "Athena_Bush", "Athena_Grenade", "Athena_M80", "Athena_Medkit", "Athena_PurpleStuff", "Athena_Shields"]);
        for s in &all {
            eprintln!("== {} core {} ({}) | ability {} | use {} len {:.3}s rate {:?} trig {:?} triggerAbility@{:?} | equip {:?} | prop {:?}", s.name, s.core, s.core_source, s.ability, s.use_montage, s.use_seconds(), s.use_rate, s.trigger_duration, s.trigger_ability_time, s.equip_montage, s.held_prop);
            for n in &s.notifies { eprintln!("     notify {} @{:.3} {:?} {:?}", n.name, n.time, n.class, n.sound); }
            eprintln!("     attach {:?}
     sequences {:?}\n     MISSING {:?}", s.attach_assets, s.use_sequences, s.missing());
            assert_eq!(s.core, "TrapPlacement", "{}: BR utility items use the NoWep (unarmed) core set", s.name);
            let get = |f: &str| s.core_assets.iter().find(|(n, _)| n == f).and_then(|(_, p)| p.as_deref()).unwrap_or_else(|| panic!("{}: core field {f} null", s.name)).to_owned();
            assert!(get("TargetingPose").ends_with("/M_NoWep_Relaxed.M_NoWep_Relaxed"));
            assert!(get("JogAdditiveBlendspace").contains("/NoWeapon/"));
            assert!(get("SprintAnimation").contains("/Sprint/Sprint_Default"));
            assert!(s.use_montage.starts_with("/Game/Animation/Game/MainPlayer/"), "{}: {}", s.name, s.use_montage);
            assert!(!s.use_sequences.is_empty() && s.use_montage_data.length > 0.0);
            assert!(s.weapon_actor_class.contains("B_UtilityItem_Generic_Athena"));
            assert!(s.stw_paths().is_empty(), "{}: STW path {:?}", s.name, s.stw_paths());
            for path in s.all_paths() { assert!(path.starts_with("/Game/Athena/") || (path == "/Game/CharClasses/Soldier/Gadgets/FragGrenade/S_Grenade_Single" && s.name == "Athena_Grenade") || path.starts_with("/Game/Abilities/Player/Generic/") || path.starts_with("/Game/Animation/Game/MainPlayer/"), "{}: unexpected root {path}", s.name); }
            assert_eq!(core_for_item(&src, &s.item).unwrap(), "TrapPlacement");
        }
        // the four healables share the montage / trigger-duration data `loadout_stats` already reads
        for h in crate::loadout_stats::HEALABLE_ITEMS {
            let s = all.iter().find(|s| h.ends_with(&s.name)).unwrap();
            assert!((s.trigger_duration.is_some() || s.name == "Athena_Grenade") && s.trigger_ability_time.is_some(), "{}", s.name);
        }
    }
}
