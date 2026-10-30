//! Read-only original starter assets. No replacement firing or montage scheduling.
//! References are recovered bindings; bytes and metadata are read from the install.
use crate::{loader, ClipSourceDir, FnClip, Part, SampleMode};
use fn_anim::{AnimSequence, SkeletonInfo, BlendSpace, BlendSpaceKind, montage::{AnimComposite, AnimMontage}};
use fn_ueio::{Package, Reader, props::{read_props, Ctx, PropList, PropValue}};
use std::collections::{HashMap, HashSet, VecDeque};

pub const ITEMS: [&str; 4] = [
    "/Game/Athena/Items/Weapons/WID_Assault_Auto_Athena_C_Ore_T02",
    "/Game/Athena/Items/Weapons/WID_Shotgun_Standard_Athena_C_Ore_T03",
    "/Game/Athena/Items/Weapons/WID_Sniper_BoltAction_Scope_Athena_R_Ore_T03",
    "/Game/Athena/Items/Consumables/Bandage/Athena_Bandage",
];
pub const PLAYER_SKELETON: &str = "/Game/Characters/Player/Male/Male_Avg_Base/Fortnite_M_Avg_Player_Skeleton";
/// INVENTORY FIX4: the four building items of the secondary quickbar F1..F4 (`FortBuildingItemDefinition`; the same list as `fn_building::pieces`).
pub const BUILD_PIECE_ITEMS: [&str; 4] = [
    "/Game/Items/Weapons/BuildingTools/BuildingItemData_Wall",
    "/Game/Items/Weapons/BuildingTools/BuildingItemData_Floor",
    "/Game/Items/Weapons/BuildingTools/BuildingItemData_Stair_W",
    "/Game/Items/Weapons/BuildingTools/BuildingItemData_RoofS",
];
#[derive(Debug)]
pub struct Item {
    pub definition: String,
    pub icon: String,
    /// Full object path of the item's SmallPreviewImage texture (icon is its PNG file name).
    pub icon_path: String,
    pub rarity: u8,
    pub quantity: u32,
    pub mesh: Option<Part>,
    /// Exact Skeleton property of the original weapon mesh. Never substitute
    /// the player skeleton or infer compatibility from matching bone names.
    pub mesh_skeleton: Option<String>,
    /// Full material identities in LOD0 section order, after original remaps.
    pub mesh_materials: Vec<Option<String>>,
    /// Inherited original WeaponCoreAnimation enum name, not a UI category.
    pub core_animation: Option<String>,
    /// Exact serialized item properties, including stat-table row handle.
    pub properties: PropList,
}
pub struct Sequence {
    pub source: AnimSequence,
    pub skeleton: String,
    pub clip: FnClip,
}
/// CONTAINERS: `GA_DefaultPlayer_InteractSearch.MontageInfo.MontageToPlay` (slot UpperBody: Default = Search_Start -> Loop = Search_Loop, looping; End = Search_End).
pub const SEARCH_MONTAGE: &str = "/Game/Animation/Game/MainPlayer/Montages/General/Search";
pub const DOOR_OPEN_MONTAGE: &str = "/Game/Animation/Game/MainPlayer/Montages/General/DoorOpen";
pub const DEATH_MONTAGE: &str = "/Game/Animation/Game/MainPlayer/Reacts/Deaths/PlayerDeath_Athena";
pub const DEATH_DBNO_MONTAGE: &str = "/Game/Animation/Game/MainPlayer/Reacts/Deaths/PlayerDeath_DBNO_Athena";
pub const HIT_REACTS_MONTAGE: &str = "/Game/Animation/Game/MainPlayer/Montages/General/AdditiveHitReacts";
pub const DBNO_INTRO_MONTAGE: &str = "/Game/Animation/Game/MainPlayer/Montages/General/DBNO_Athena_Intro";
pub const DBNO_OUTRO_MONTAGE: &str = "/Game/Animation/Game/MainPlayer/Montages/General/DBNO_Athena_Outro";
pub struct Resources {
    /// Serialized player skeleton socket used by the native pawn default.
    pub player_hand_socket: fn_mesh::Socket,
    pub items: Vec<Item>,
    pub bindings: Vec<(String, String, String)>,
    pub montages: HashMap<String, AnimMontage>,
    pub composites: HashMap<String, AnimComposite>,
    pub sequences: HashMap<String, Sequence>,
    pub blendspaces: HashMap<String, BlendSpace>,
    pub core_anims: HashMap<String, PropList>,
    /// Package indices only have meaning inside the owning AnimBlueprint.
    /// Resolve them before discarding that package; preserve field array index
    /// and explicit nulls rather than selecting clips by ambiguous basename.
    pub core_asset_paths: HashMap<String, HashMap<(String, i32), Option<String>>>,
    pub materials: HashMap<String, crate::loadout_material::Material>,
    pub material_textures: HashMap<String, crate::loadout_material::Texture>,
    /// Original firearm numbers (index = item index; None for the bandage), read from the WID + its WeaponStatHandle stat row.
    pub definitions: Vec<Option<fn_weapons::Definition>>,
    pub stat_extras: Vec<Option<crate::loadout_stats::StatExtras>>,
    /// Every healing consumable of the 1.8 data (Bandage, Medkit, Shields, PurpleStuff), read from the item / ability / effect assets (index = HEALABLE_ITEMS).
    pub healables: Vec<crate::loadout_stats::HealableStats>,
    /// HUD textures decoded from the packages (item icons, ammo icons): (PNG name the UI asks for, width, height, RGBA8). Original pixels, no re-drawn art.
    pub ui_images: Vec<(String, u32, u32, Vec<u8>)>,
    /// AMMO HOOK: the ammo item definitions of the firearms' ammo types and the Athena game mode's StartingItems (`loadout_ammo`).
    pub ammo: crate::loadout_ammo::AmmoSetup,
    /// ADS HOOK: aim-down-sights profiles of the three firearms (`loadout_ads`); empty when they could not be read (reported at load).
    pub ads: Vec<fn_weapons::recovered_ads::AdsProfile>,
    /// WIDGETS3 HOOK: inventory screen data (name / description / rarity / type / icon) of the four starter items followed by the pickaxe and wood / stone / metal (`loadout_inventory`).
    pub inventory: Vec<crate::loadout_inventory::CatalogItem>,
    /// Firearms installed after start-up (`loadout_gun`): gun index `STARTER_GUNS + position`. `inventory` gets one entry per pack, in the same order (after the starters / pickaxe / resources).
    pub extra_guns: Vec<crate::loadout_gun::ExtraGun>,
    pub montage_curves: HashMap<String, Vec<(String, Vec<(f32, f32)>)>>,
}
pub(crate) fn package_name(reference: &str) -> &str { reference.split('.').next().unwrap_or(reference) }
pub(crate) fn props(pkg: &Package, class: &str) -> Result<PropList, String> {
    let idx = pkg.find_export(class).ok_or_else(|| format!("missing {class}"))?;
    let (start, end) = pkg.export_range(idx);
    read_props(&mut Reader::at(&pkg.data[..end], start), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())
}
pub(crate) fn object_path(pkg: &Package, p: &PropList, key: &str) -> Result<String, String> {
    if let Some(path) = p.text(key).filter(|s| s.starts_with("/Game/")) { return Ok(path.to_owned()); }
    let index = p.object(key).filter(|i| *i < 0).ok_or_else(|| format!("{}: missing external {key}", pkg.exports[0].object_name))?;
    Ok(pkg.import_path(index))
}
pub(crate) fn rarity(value: &str) -> Result<u8, String> {
    match value.rsplit("::").next().unwrap_or(value) {
        "Handmade" => Ok(0), "Ordinary" => Ok(1), "Sturdy" => Ok(2), "Quality" => Ok(3), "Fine" => Ok(4),
        v => Err(format!("unsupported original rarity {v}")),
    }
}
/// Original map payload of `CoreAnims_Male` (see `loadout_weapon::core_map`). Duplicate keys are refused.
fn core_map(pkg:&Package) -> Result<HashMap<String,PropList>,String> {
    let mut out=HashMap::new();
    for (k,v) in crate::loadout_weapon::core_map(pkg,"CoreAnims_Male")? {if out.insert(k,v).is_some() {return Err("duplicate original core enum".into());}}
    Ok(out)
}
impl Resources {
    /// Full package paths in the recovered 1.8 dependency closure whose
    /// original `UAnimSequence` contains `SnapWeapon_LHand`.
    pub fn snap_weapon_lhand_sequences(&self) -> Vec<String> {
        let mut out: Vec<String> = self.sequences.iter()
            .filter(|(_, sequence)| sequence.source.curves.iter().any(|curve| curve.name == "SnapWeapon_LHand"))
            .map(|(path, _)| path.clone()).collect();
        out.sort(); out
    }
    /// Library for a held mesh, deliberately separate from player animation.
    /// Full package identities only: no ambiguous basename aliases and no
    /// invented equip/fire/reload scheduling. An empty library means the
    /// recovered closure has no sequences for this skeleton, not fallback.
    pub fn weapon_clips(&self, item_index: usize) -> Result<crate::ClipStore, String> {
        let item=self.items.get(item_index).ok_or("weapon item index out of range")?;
        self.clips_for_item(item)
    }
    pub(crate) fn clips_for_item(&self, item: &Item) -> Result<crate::ClipStore, String> {
        if item.mesh.is_none() {return Err("item has no recovered weapon mesh".into());}
        let skeleton=item.mesh_skeleton.as_deref().ok_or("weapon mesh has no original skeleton identity")?;
        let skeleton=package_name(skeleton);
        if skeleton==PLAYER_SKELETON {return Err("weapon library cannot use the player skeleton".into());}
        let mut store=crate::ClipStore::new();
        for (path,sequence) in &self.sequences {
            if package_name(&sequence.skeleton)==skeleton {store.insert(path,sequence.clip.clone());}
        }
        Ok(store)
    }
    /// Supply only original PLAYER clips to the character pose library.
    /// Weapon-skeleton clips remain separate for the held mesh evaluator.
    pub fn install_player_clips(&self, store:&mut crate::ClipStore) -> usize {
        let mut added=0;
        let mut name_counts=HashMap::<&str,usize>::new();
        for sequence in self.sequences.values().filter(|s|s.skeleton==PLAYER_SKELETON) {
            *name_counts.entry(&sequence.clip.name).or_default()+=1;
        }
        for (path,sequence) in &self.sequences {
            if sequence.skeleton!=PLAYER_SKELETON {continue;}
            // Original assets can have the SAME filename in targeted and
            // non-targeted directories. Full package identity is mandatory.
            if !store.clips.contains_key(path) {store.insert(path,sequence.clip.clone());added+=1;}
            // Legacy graph names can only be supplied when unambiguous.
            // Preserve existing legacy aliases; never randomly pick a duplicate.
            if name_counts[sequence.clip.name.as_str()]==1 && !store.clips.contains_key(&sequence.clip.name) {
                store.insert(&sequence.clip.name,sequence.clip.clone());added+=1;
            }
        }
        added
    }
    /// CONTAINERS: `GA_DefaultPlayer_InteractSearch.MontageInfo.MontageToPlay` (the search hold's upper-body montage).
    pub fn load(src: &ClipSourceDir) -> Result<Self, String> {
        let skeleton_pkg=loader::package(src,PLAYER_SKELETON)?;
        let skeleton=fn_mesh::Skeleton::parse(&skeleton_pkg).map_err(|e|format!("player socket skeleton: {e}"))?;
        let mut hands=skeleton.sockets.into_iter().filter(|s|s.name=="RightHand");
        let player_hand_socket=hands.next().ok_or("original player skeleton has no RightHand socket")?;
        if hands.next().is_some(){return Err("duplicate original RightHand socket".into());}
        let mut resources = Self { player_hand_socket, items: vec![], bindings: vec![], montages: HashMap::new(), composites: HashMap::new(), sequences: HashMap::new(), blendspaces: HashMap::new(), core_anims: HashMap::new(), core_asset_paths: HashMap::new(), materials:HashMap::new(), material_textures:HashMap::new(), definitions: vec![], stat_extras: vec![], healables: vec![], inventory: vec![], ui_images: vec![], ammo: Default::default(), extra_guns: vec![], montage_curves: HashMap::new(), ads: match crate::loadout_ads::load(src, &ITEMS[..crate::loadout_gun::STARTER_GUNS]) { Ok(a) => a, Err(e) => { eprintln!("[ads] aim-down-sights data unavailable: {e}"); vec![] } } }; // ADS HOOK
        for (i, path) in ITEMS.iter().enumerate() {
            if i < crate::loadout_gun::STARTER_GUNS {
                // the starter firearms go through the same item reader as every other BR weapon (loadout_gun)
                let (item, definition, extras, _actor) = crate::loadout_gun::read_gun_item(src, path)?;
                resources.definitions.push(Some(definition)); resources.stat_extras.push(Some(extras));
                resources.items.push(item);
                continue;
            }
            let pkg = loader::package(src, path)?;
            // Bandage's concrete class is checked below, not silently replaced by defaults.
            let class = pkg.exports.iter().enumerate().find_map(|(n, _)| {
                let c = pkg.export_class(n);
                c.contains("ItemDefinition").then_some(c)
            }).ok_or("bandage has no item definition")?;
            let properties = props(&pkg, &class)?;
            let icon = object_path(&pkg, &properties, "SmallPreviewImage")?;
            let icon_object = icon.clone();
            let icon = format!("{}.png", package_name(&icon).rsplit('/').next().ok_or("invalid icon reference")?);
            let r = rarity(properties.text("Rarity").ok_or("item missing rarity")?)?;
            let quantity = 5;
            if properties.int("MaxStackSize").is_none_or(|max| max < quantity as i64) { return Err("bandage grant exceeds original stack limit or limit is missing".into()); }
            resources.definitions.push(None); resources.stat_extras.push(None);
            let core_animation=Some(crate::loadout_consumable::core_for_item(src,path)?); // CONSUMABLE HOOK: bandage = utility item, unarmed NoWep set
            resources.items.push(Item { definition: (*path).to_owned(), icon, icon_path: icon_object.clone(), rarity: r, quantity, mesh: None, mesh_skeleton: None, mesh_materials: vec![], core_animation, properties });
        }
        for it in &resources.items { resources.ui_images.push(crate::loadout_stats::decode_ui_texture(src, &it.icon_path)?); }
        // Native sniper scope material's exact Reticle parameter. It belongs in the normal UI overlay so the runtime can draw the recovered pixels.
        for profile in &resources.ads {
            if let Some(scope) = profile.scope.as_ref() {
                // The exact scope Reticle parameter belongs to the original
                // post-process material.  Preserve its black mask as decoded:
                // erasing those pixels made the scoped view transparent rather
                // than reproducing the material's blackout outside the optic.
                let image = crate::loadout_stats::decode_ui_texture(src, &scope.reticle_texture)?;
                if !resources.ui_images.iter().any(|(name, ..)| *name == image.0) { resources.ui_images.push(image); }
            }
        }
        for e in resources.stat_extras.iter().flatten() { if !resources.ui_images.iter().any(|(n, ..)| *n == e.ammo_icon) { resources.ui_images.push(crate::loadout_stats::decode_ui_texture(src, &e.ammo_icon_path)?); } }
        // QUICKBAR AMMO: the item card's ammo icon is a sub-brush of the ammo item's AmmoIconBrush (T-Icon-Ammo-*-32 / -64 ...), pak-only like the rest
        for e in resources.stat_extras.iter().flatten() { for (_, name, path) in e.ammo_icon_brush.iter().flatten() { if !resources.ui_images.iter().any(|(n, ..)| n == name) { resources.ui_images.push(crate::loadout_stats::decode_ui_texture(src, path)?); } } }
        // AMMO HOOK: ammo item definitions + the game mode's starting items (original data; `loadout_ammo`)
        { let paths: Vec<String> = resources.definitions.iter().flatten().map(|d| d.ammo.clone()).collect(); resources.ammo = crate::loadout_ammo::read(src, &paths)?; }
        // the use-progress ring (AthenaEquipProgress: ImageTimerBackground / ImageTimer brush textures behind M_HealProgress) is not in the exported UI folder: decode them from the packages too
        for path in crate::loadout_stats::EQUIP_PROGRESS_TEXTURES { resources.ui_images.push(crate::loadout_stats::decode_ui_texture(src, path)?); }
        // HUD FIX: the building bar's mode icons + dotted separator and the interaction prompt's key base (loadout_stats::HUD_PAK_TEXTURES), also pak-only
        for path in crate::loadout_stats::HUD_PAK_TEXTURES { resources.ui_images.push(crate::loadout_stats::decode_ui_texture(src, path)?); }
        // INVENTORY FIX4 (inventory_gaps verify3 V7): the building quickbar's F1..F4 cards show each building item's `SmallPreviewImage`
        // (`T_BuildMode_Prefab_*_Large-64`, /Game/UI/Foundation/Textures/Icons/HUD), which is not in the exported UI folder either: without this the HUD's
        // lookup by name missed (and `UiAssets` keeps the miss), so the slots drew their frames with no piece icon.
        for path in BUILD_PIECE_ITEMS {
            let pkg = loader::package(src, path)?;
            let class = pkg.exports.iter().enumerate().find_map(|(n, _)| { let c = pkg.export_class(n); c.contains("ItemDefinition").then_some(c) }).ok_or("building item has no item definition")?;
            let icon = object_path(&pkg, &props(&pkg, &class)?, "SmallPreviewImage")?;
            let image = crate::loadout_stats::decode_ui_texture(src, &icon)?;
            if !resources.ui_images.iter().any(|(n, ..)| *n == image.0) { resources.ui_images.push(image); }
        }
        for path in crate::loadout_stats::HEALABLE_ITEMS {
            let pkg = loader::package(src, path)?;
            let class = pkg.exports.iter().enumerate().find_map(|(n, _)| { let c = pkg.export_class(n); c.contains("ItemDefinition").then_some(c) }).ok_or("consumable has no item definition")?;
            let item = props(&pkg, &class)?;
            resources.healables.push(crate::loadout_stats::healable_stats(src, path, &pkg, &item)?);
        }
        resources.materials=crate::loadout_material::load_closure(src,resources.items.iter().flat_map(|item|item.mesh_materials.iter().flatten().cloned()))?;
        resources.material_textures=crate::loadout_material::load_parameter_textures(src,&resources.materials)?;
        let mut queue = VecDeque::new();
        let graph=loader::package(src,"/Game/Characters/Player/Male/Male_Avg_Base/Fortnite_M_Avg_Player_AnimBlueprint")?;
        let mut core=core_map(&graph)?;
        for item in &resources.items {
            let Some(key)=&item.core_animation else {continue};
            if resources.core_anims.contains_key(key) {continue;}
            let entry=core.remove(key).ok_or_else(||format!("missing original CoreAnims_Male[{key}]"))?;
            let mut paths=HashMap::new();
            for field in &entry.0 {
                if let PropValue::Object(index)=field.value {
                    if index==0 {
                        if paths.insert((field.name.clone(),field.array_index),None).is_some() {return Err("duplicate core asset field".into());}
                        continue;
                    }
                    if index>=0 {return Err("unsupported internal core animation reference".into());}
                    let path=graph.import_path(index);
                    if !path.starts_with("/Game/") {return Err("core animation outside game packages".into());}
                    queue.push_back(package_name(&path).to_owned());
                    if paths.insert((field.name.clone(),field.array_index),Some(path)).is_some() {return Err("duplicate core asset field".into());}
                }
            }
            resources.core_asset_paths.insert(key.clone(),paths);
            resources.core_anims.insert(key.clone(),entry);
        }
        // Montage roles of the starter firearms: each weapon's OWN actor Blueprint chain (loadout_weapon::weapon_anims), same reader as every other BR weapon.
        // A role the chain does not set stays unbound (reported by the caller), never substituted.
        for (i, path) in ITEMS.iter().enumerate().take(crate::loadout_gun::STARTER_GUNS) {
            let anims = crate::loadout_weapon::weapon_anims(src, path, false)?;
            if let Some(Some(d)) = resources.definitions.get_mut(i) { crate::loadout_gun::assign_assets(&mut d.assets, &anims); }
            for (item, role, package) in crate::loadout_gun::bindings(path, &anims) { queue.push_back(package.clone()); resources.bindings.push((item, role, package)); }
        }
        // Every healing ability plays its MontageInfo montage.
        for h in &resources.healables { queue.push_back(h.montage.clone()); }
        // CONTAINERS: GA_DefaultPlayer_InteractSearch (GAS_DefaultPlayer = DefaultGameData.GenericPlayerAbilitySet) plays MontageInfo = Search during a search hold
        queue.push_back(SEARCH_MONTAGE.to_owned());
        for m in [DEATH_MONTAGE, DEATH_DBNO_MONTAGE, HIT_REACTS_MONTAGE, DBNO_INTRO_MONTAGE, DBNO_OUTRO_MONTAGE] { queue.push_back(m.to_owned()); }
        resources.absorb(src, queue)?;
        if let Err(e) = resources.absorb(src, VecDeque::from([DOOR_OPEN_MONTAGE.to_owned()])) { eprintln!("[interact] {DOOR_OPEN_MONTAGE}: not loaded ({e})"); }
        for m in [SEARCH_MONTAGE, DOOR_OPEN_MONTAGE] { match crate::loadout_build::montage_curves(src, m) { Ok(c) => { resources.montage_curves.insert(m.to_owned(), c); } Err(e) => eprintln!("[interact] {m}: montage curves unavailable ({e})") } }
        // WIDGETS3 HOOK: the inventory screen's item texts (starter items from their serialized properties, pickaxe + resources read from their definitions)
        for it in &resources.items { resources.inventory.push(crate::loadout_inventory::entry(&it.definition, "FortWeaponRangedItemDefinition", &it.properties, &it.icon_path)); }
        match crate::loadout_inventory::read_extras(src) { Ok(v) => resources.inventory.extend(v), Err(e) => eprintln!("[inventory] item texts unavailable for the pickaxe / resources: {e}") }
        Ok(resources)
    }
    /// Load the animation closure (montage / composite / blendspace / sequence packages and their skeletons) of the queued packages into
    /// this resource set; packages already present are not re-read.
    pub fn absorb(&mut self, src: &ClipSourceDir, queue: VecDeque<String>) -> Result<(), String> { self.absorb_skipping(src, queue, &HashSet::new()) }
    /// [`Self::absorb`] that also treats the packages in `skip` as already loaded (a worker's scratch set skips what the live resources hold; their closures are complete by construction).
    pub fn absorb_skipping(&mut self, src: &ClipSourceDir, mut queue: VecDeque<String>, skip: &HashSet<String>) -> Result<(), String> {
        let mut seen: HashSet<String> = self.montages.keys().chain(self.composites.keys()).chain(self.blendspaces.keys()).chain(self.sequences.keys()).chain(skip.iter()).cloned().collect();
        let mut skeletons = HashMap::<String, SkeletonInfo>::new();
        while let Some(path) = queue.pop_front() {
            let path = package_name(&path).to_owned();
            if !seen.insert(path.clone()) { continue; }
            let pkg = loader::package(src, &path)?;
            if pkg.find_export("AnimMontage").is_some() {
                let montage = AnimMontage::parse(&pkg).map_err(|e| format!("{path}: {e}"))?;
                for slot in &montage.slots { for segment in &slot.segments {
                    queue.push_back(segment.animation_path.clone().ok_or_else(|| format!("{path}: segment lacks external animation reference"))?);
                } }
                self.montages.insert(path, montage);
            } else if pkg.find_export("AnimComposite").is_some() {
                let composite = AnimComposite::parse(&pkg).map_err(|e| format!("{path}: {e}"))?;
                for segment in &composite.segments { queue.push_back(segment.animation_path.clone().ok_or("composite segment missing reference")?); }
                self.composites.insert(path, composite);
            } else if pkg.exports.iter().enumerate().any(|(i,_)|BlendSpaceKind::from_class(&pkg.export_class(i)).is_some()) {
                let space=BlendSpace::parse(&pkg).map_err(|e|format!("{path}: {e}"))?;
                for sample in &space.samples {
                    if let Some(reference)=&sample.animation_path {queue.push_back(package_name(reference).to_owned());}
                    else if sample.animation.is_some() {return Err(format!("{path}: sample lacks external sequence reference"));}
                }
                self.blendspaces.insert(path,space);
            } else if pkg.find_export("AnimSequence").is_some() {
                let source = AnimSequence::parse(&pkg).map_err(|e| format!("{path}: {e}"))?;
                if let Some(reference) = &source.additive.ref_pose_seq_path { queue.push_back(reference.clone()); }
                let skeleton = package_name(source.skeleton_path.as_deref().ok_or("sequence missing skeleton")?).to_owned();
                if !skeletons.contains_key(&skeleton) {
                    skeletons.insert(skeleton.clone(), SkeletonInfo::from_package(&loader::package(src, &skeleton)?).map_err(|e| e.to_string())?);
                }
                let clip = FnClip::from_sequence(&source, &skeletons[&skeleton], SampleMode::Ue);
                self.sequences.insert(path, Sequence { source, skeleton, clip });
            } else { return Err(format!("unsupported loadout animation class: {path}")); }
        }
        Ok(())
    }
    /// Make ANY BR weapon's animation set available (generic, data-driven; see `loadout_weapon`): its `CoreAnims_{Male|Female}[core]` entry
    /// is stored under `core_anims` / `core_asset_paths` (key = core enum; the first set stored for a key wins, male and female maps are not mixed)
    /// and every referenced core asset and weapon / player montage is loaded with its sequence closure.
    pub fn add_weapon_anims(&mut self, src: &ClipSourceDir, w: &crate::loadout_weapon::WeaponAnims) -> Result<(), String> {
        let mut queue = VecDeque::new();
        if !self.core_anims.contains_key(&w.core) {
            let graph = loader::package(src, crate::loadout_weapon::PLAYER_ANIM_BP)?;
            let map = crate::loadout_weapon::core_map(&graph, if w.female { "CoreAnims_Female" } else { "CoreAnims_Male" })?;
            let (_, entry) = map.into_iter().find(|(k, _)| *k == w.core).ok_or_else(|| format!("missing core entry {}", w.core))?;
            let mut paths = HashMap::new();
            for a in &w.core_assets { paths.insert((a.raw.clone(), a.index), a.path.clone()); }
            self.core_asset_paths.insert(w.core.clone(), paths);
            self.core_anims.insert(w.core.clone(), entry);
        }
        for p in w.packages() { queue.push_back(p); }
        self.absorb(src, queue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_starter_resources() {
        // Explicit fixture: this test never skips missing original data silently.
        let root = fn_live::var_os("FN_TEST_ROOT").expect("set FN_TEST_ROOT to the local 1.8 install");
        let set = fn_pak::PakSet::open_install(root).unwrap();
        let r = Resources::load(&ClipSourceDir::Pak(&set)).unwrap();
        assert_eq!(r.items.len(), 4);
        assert_eq!(r.player_hand_socket.bone,"weapon_r");
        assert_eq!(r.player_hand_socket.location,[0.;3]);
        assert_eq!(r.player_hand_socket.rotation,[0.,-90.,0.]);
        assert_eq!(r.player_hand_socket.scale,[1.;3]);
        let original_hand=crate::SocketDef::from_original_yaw_socket(0,&r.player_hand_socket).unwrap();
        let legacy_hand=crate::SocketDef::right_hand(0);
        assert_eq!(original_hand.name,legacy_hand.name);
        assert_eq!(original_hand.bone,legacy_hand.bone);
        assert_eq!(original_hand.local.to_cols_array(),legacy_hand.local.to_cols_array(),"actual recovered socket keeps previous pick attachment unchanged");
        assert_eq!(r.items.iter().map(|i| i.rarity).collect::<Vec<_>>(), [0, 0, 2, 0]);
        assert_eq!(r.items[3].quantity, 5);
        assert_eq!(r.bindings.len(), 21);
        assert_eq!(r.sequences.len(),181,"original locomotion/aim dependency fixture (138 starter + the Medkit / Shield / PurpleStuff / Grenade use montages + the 36 TrapPlacement (unarmed NoWep) consumable core-set assets + Search_Start / Search_Loop / Search_End)");
        assert_eq!(r.sequences.values().map(|s|s.source.num_frames).sum::<usize>(),3839);
        // The held-firearm idle poses must be available under the exact names
        // the recovered player AnimBlueprint requests.  Without these, the
        // graph silently falls back to its unarmed reference pose and leaves
        // the supporting hand away from the weapon.
        for required in ["M_AssaultRifle_NonTargeted", "M_AssaultRifle_Targeted", "Rifle_NonTargeted_CMM", "Rifle_Targeted_CMM"] {
            assert!(r.sequences.values().any(|s| s.clip.name == required), "missing held-firearm player pose {required}; available names: {:?}", r.sequences.values().map(|s| s.clip.name.as_str()).collect::<Vec<_>>());
        }
        // The graph asks for these legacy animation names.  It is not enough
        // for a package to contain them: they must reach the player's runtime
        // clip map, otherwise evaluation silently returns the unarmed bind
        // pose and leaves the support hand away from the rifle.
        let mut player_clips=crate::ClipStore::new();
        r.install_player_clips(&mut player_clips);
        for required in ["M_AssaultRifle_NonTargeted", "M_AssaultRifle_Targeted", "Rifle_NonTargeted_CMM", "Rifle_Targeted_CMM"] {
            assert!(player_clips.clips.contains_key(required), "held-firearm runtime clip alias missing: {required}");
        }
        assert_eq!(r.items[..3].iter().map(|i|i.core_animation.as_deref()).collect::<Vec<_>>(),[Some("AssaultRifle"),Some("Rifle"),Some("AssaultRifle")]);
        assert_eq!(r.core_anims.len(),3); // AssaultRifle, Rifle + TrapPlacement (consumables)
        assert_eq!(r.core_asset_paths.len(),3);
        for (core,entry) in &r.core_anims {
            let paths=&r.core_asset_paths[core];
            assert_eq!(paths.len(),entry.0.iter().filter(|f|matches!(f.value,PropValue::Object(_))).count());
            for field in &entry.0 {if let PropValue::Object(index)=field.value {
                let path=&paths[&(field.name.clone(),field.array_index)];
                assert_eq!(path.is_none(),index==0,"original explicit null {core}.{}",field.name);
                if let Some(path)=path {
                    let package=package_name(path);
                    assert!(r.sequences.contains_key(package)||r.blendspaces.contains_key(package)||r.montages.contains_key(package)||r.composites.contains_key(package),"unresolved original core field {core}.{}: {path}",field.name);
                }
            }}
        }
        assert_eq!(r.blendspaces.len(),9); // +M_Jog_NoWep_Additive, Crouch_NoWep_ADD (TrapPlacement)
        for space in r.blendspaces.values() {for sample in &space.samples {if let Some(path)=&sample.animation_path {assert!(r.sequences.contains_key(package_name(path)),"missing {path}");}}}
        eprintln!("starter closure: {} sequences / {} frames, {} blendspaces",r.sequences.len(),r.sequences.values().map(|s|s.source.num_frames).sum::<usize>(),r.blendspaces.len());
        for item in &r.items[..3] {
            assert!(!item.mesh.as_ref().unwrap().prims.is_empty());
            assert_eq!(item.mesh_materials.len(),item.mesh.as_ref().unwrap().prims.len());
            for path in item.mesh_materials.iter().flatten() {assert!(r.materials.contains_key(path),"missing original mesh material {path}");}
            assert!(item.properties.fields("WeaponStatHandle").is_some());
        }
        assert!(r.weapon_clips(3).is_err(),"bandages have no recovered gun mesh");
        assert!(r.weapon_clips(usize::MAX).is_err(),"invalid inventory index");
        for index in 0..3 {
            let item=&r.items[index];
            let skeleton=package_name(item.mesh_skeleton.as_deref().unwrap());
            assert_ne!(skeleton,PLAYER_SKELETON);
            let library=r.weapon_clips(index).unwrap();
            let expected=r.sequences.iter().filter(|(_,s)|package_name(&s.skeleton)==skeleton).count();
            assert_eq!(library.clips.len(),expected);
            for (path,sequence) in &r.sequences {
                assert_eq!(library.clips.contains_key(path),package_name(&sequence.skeleton)==skeleton,"weapon/player skeleton isolation: {path}");
                assert!(!library.clips.contains_key(&sequence.clip.name),"weapon library must not add basename aliases");
            }
        }
        for (path,material) in &r.materials {
            if let Some(parent)=&material.parent {assert!(r.materials.contains_key(parent),"missing parent {parent}");}
            for texture in material.texture_parameters.values().flatten() {assert!(r.material_textures.contains_key(texture),"{path}: missing original texture {texture}");}
        }
        assert_eq!(r.materials.len(),7);
        assert_eq!(r.material_textures.len(),20);
        let weapon_base=&r.materials["/Game/Packages/Fortnite_UniqueMaterials_1/Weapons/WeaponMat_Base.WeaponMat_Base"];
        let ammo_base=&r.materials["/Game/Packages/Fortnite_UniqueMaterials_1/BackPackShrinkingEffect.BackPackShrinkingEffect"];
        assert_eq!(weapon_base.shaders.len(),594);
        assert_eq!(ammo_base.shaders.len(),777);
        {
            let export=weapon_base.package.exports.iter().position(|e|e.object_name=="WeaponMat_Base").unwrap();
            let bytes=weapon_base.package.export_bytes(export).unwrap();let shader=&weapon_base.shaders[0];let stream=&shader.serialized_stream;
            let mut bad=bytes.to_vec();bad[stream.start+8..stream.start+12].copy_from_slice(&99u32.to_le_bytes());
            assert!(fn_dxbc::extract::serialized_stream(&bad,&shader.blob,&shader.program).is_err(),"reject conflicting original stage");
            let mut bad=bytes.to_vec();let count=(stream.end-shader.blob.id+1) as u32;bad[shader.blob.id-4..shader.blob.id].copy_from_slice(&count.to_le_bytes());
            assert!(fn_dxbc::extract::serialized_stream(&bad,&shader.blob,&shader.program).is_err(),"reject extra compressed input bytes");
            assert!(fn_dxbc::extract::serialized_stream(&bytes[..stream.end-1],&shader.blob,&shader.program).is_err(),"reject truncated compressed array");
            let mut bad=bytes.to_vec();bad[shader.blob.id]^=1;
            assert!(fn_dxbc::extract::serialized_stream(&bad,&shader.blob,&shader.program).is_err(),"reject damaged compressed packet");
            let mut bad=bytes.to_vec();bad[stream.end+28..stream.end+32].copy_from_slice(&0u32.to_le_bytes());
            assert!(fn_dxbc::extract::serialized_stream(&bad,&shader.blob,&shader.program).is_err(),"reject incorrect trailing output size");
            let mut bad=bytes.to_vec();bad[stream.end+24..stream.end+28].copy_from_slice(&99u32.to_le_bytes());
            assert!(fn_dxbc::extract::serialized_stream(&bad,&shader.blob,&shader.program).is_err(),"reject incorrect trailing sampler count");
            assert!(fn_dxbc::extract::serialized_stream(&bytes[..stream.tail.end-1],&shader.blob,&shader.program).is_err(),"reject truncated shader tail");
        }
        assert_eq!(r.materials.values().map(|m|m.shaders.len()).sum::<usize>(),1371);
        for material in r.materials.values(){for shader in &material.shaders {
            assert!(fn_dxbc::analysis::unknown_opcodes(&shader.program).is_empty(),"unsupported original shader opcode");
            assert_ne!(shader.program_hash,0);
            let container=fn_dxbc::Container::parse(&shader.blob.bytes).unwrap();
            assert!(container.program().is_some());
            assert_eq!(shader.blob.header.len(),shader.blob.prefix);
            assert_eq!(shader.resource_table.encode(),shader.blob.header,"resource table must round-trip exactly");
            assert_eq!(shader.serialized_stream.start+20,shader.blob.id);
            assert!(shader.serialized_stream.end>shader.blob.id);
            assert_eq!(shader.serialized_stream.tail.end,shader.serialized_stream.end+32);
            assert_eq!(shader.serialized_stream.tail.inflated_size as usize,shader.blob.header.len()+shader.blob.bytes.len()+shader.blob.trailer.len());
            assert_eq!(shader.serialized_stream.tail.sampler_count as usize,shader.program.decls.samplers.len());
            assert!(matches!(shader.serialized_stream.platform,0|5),"unexpected original D3D platform");
            for (kind,map) in [("texture",&shader.resource_table.texture_map),("srv",&shader.resource_table.srv_map),("sampler",&shader.resource_table.sampler_map),("uav",&shader.resource_table.uav_map)] {
                for binding in shader.resource_table.bindings(map).unwrap() {
                    let register=binding.register as u32;
                    let declared=match kind {
                        "sampler"=>shader.program.decls.samplers.iter().any(|d|d.slot==register),
                        "uav"=>shader.program.decls.uavs.iter().any(|d|d.slot==register),
                        _=>shader.program.decls.resources.iter().any(|d|d.slot==register),
                    };
                    assert!(declared,"{} shader {}: {kind} register {register} absent from DXBC declarations",material.package.exports[0].object_name,shader.blob.id);
                    let names=shader.uniform_buffer_names.as_ref().expect("bound resources require named uniform-buffer slots");
                    assert!(!names.get(binding.buffer as usize).expect("binding buffer slot outside names").is_empty(),"bound uniform buffer has empty name");
                    assert!(shader.resource_table.layout_hashes.get(binding.buffer as usize).is_some(),"bound buffer lacks layout hash");
                }
            }
        }}
        for (path,texture) in &r.material_textures {
            assert!(texture.image.width>0&&texture.image.height>0,"empty {path}");
            match &texture.image.pixels {
                fn_texture::Pixels::Rgba8(bytes)=>assert_eq!(bytes.len(),texture.image.width*texture.image.height*4),
                fn_texture::Pixels::RgbaF16(values)=>assert_eq!(values.len(),texture.image.width*texture.image.height*4),
            }
        }
        let bandage = &r.montages["/Game/Animation/Game/MainPlayer/Montages/General/WrapBandages_Athena"];
        assert_eq!(bandage.length, 3.8784313201904297_f32);
        assert!(bandage.slots.iter().any(|s| s.segments.iter().any(|s| s.looping_count == 2)));
        assert!(r.sequences.values().any(|s| s.skeleton.contains("Asault_Rifle")));
        let mut store=crate::ClipStore::new();
        let expected=r.sequences.values().filter(|s|s.skeleton==PLAYER_SKELETON).count();
        let mut names=HashMap::<String,Vec<String>>::new();
        for (path,sequence) in &r.sequences {if sequence.skeleton==PLAYER_SKELETON {names.entry(sequence.clip.name.clone()).or_default().push(path.clone());}}
        let aliases=names.values().filter(|paths|paths.len()==1).count();
        assert_eq!(r.install_player_clips(&mut store),expected+aliases);
        assert_eq!(r.install_player_clips(&mut store),0,"do not replace loaded character clips");
        assert_eq!(store.clips.len(),expected+aliases);
        for (path,sequence) in &r.sequences {
            if sequence.skeleton!=PLAYER_SKELETON {assert!(!store.clips.contains_key(path),"weapon clip leaked into player skeleton");}
            else {assert!(store.clips.contains_key(path));}
        }
        for (name,paths) in &names {assert_eq!(store.clips.contains_key(name),paths.len()==1,"ambiguous basename alias {name}");}
    }
    /// The runtime reads these numbers from the install (WID + `WeaponStatHandle` row of `AthenaRangedWeapons`, `GA_Athena_Bandage` CDO); the test pins what the 1.8 files say.
    #[test]
    fn original_weapon_numbers_come_from_the_stat_rows_and_the_bandage_ability() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT") else { eprintln!("FN_TEST_ROOT not set, skipping"); return };
        let paks = fn_pak::PakSet::open_install(&root).unwrap();
        let r = Resources::load(&ClipSourceDir::Pak(&paks)).unwrap();
        let d: Vec<_> = r.definitions.iter().flatten().collect();
        assert_eq!(d.len(), 3);
        assert_eq!((d[0].stat_row.as_str(), d[0].clip_size, d[0].firing_rate, d[0].reload_time, d[0].damage_pb, d[0].individual_reload, d[0].trigger.clone()), ("Assault_Auto_Athena_C_Ore_T03", 30, 5.5, 2.3, 32., false, fn_weapons::Trigger::Automatic));
        assert_eq!((d[1].clip_size, d[1].bullets_per_cartridge, d[1].firing_rate, d[1].damage_pb, d[1].individual_reload, d[1].trigger.clone()), (5, 10, 0.7, 9., true, fn_weapons::Trigger::OnPress));
        assert_eq!((d[2].clip_size, d[2].firing_rate, d[2].reload_time, d[2].damage_pb, d[2].trigger.clone()), (1, 0.33, 3., 105., fn_weapons::Trigger::OnPress));
        use crate::loadout_stats::HealRule;
        assert_eq!(r.healables.len(), 5);
        let b = &r.healables[0];
        assert_eq!((b.rule.clone(), b.trigger_duration, b.montage_rate, b.cost), (HealRule::Bandage { healing_cap: 75., max_healing_amount: 15. }, 3.5, 1., 1.));
        assert!(b.montage.ends_with("WrapBandages_Athena") && b.effect.mods.len() == 1 && b.effect.mods[0].attribute == "Healing" && b.effect.mods[0].magnitude == 1.);
        assert_eq!((r.healables[1].rule.clone(), r.healables[1].trigger_duration), (HealRule::Medkit { healing_amount: 100. }, 10.));
        assert_eq!((r.healables[2].rule.clone(), r.healables[2].trigger_duration, r.healables[2].effect.mods[0].attribute.as_str()), (HealRule::Shield { shielding_amount: 50. }, 5., "CurrentShield"));
        let pu = &r.healables[3]; assert_eq!((pu.rule.clone(), pu.trigger_duration, pu.effect.duration, pu.effect.period, pu.cooldown, pu.effect.mods.len()), (HealRule::Purple, 2., Some(24.), Some(1.), Some(26.), 2));
        assert!(r.ui_images.iter().any(|(n, w, h, px)| n == "T-Items-AmmoMedium.png" && *w > 0 && px.len() == (*w * *h * 4) as usize), "ammo icon decoded from the packages");
        // QUICKBAR AMMO: AthenaAmmoDataBulletsMedium / Shells / BulletsHeavy AmmoIconBrush (no XXS / XL; Brush_M has no serialized ImageSize -> ctor 32)
        let brush = |g: usize| r.stat_extras.iter().flatten().nth(g).unwrap().ammo_icon_brush.clone().map(|b| b.map(|(s, n, _)| (s, n)));
        let sub = |s: f32, n: &str| Some(([s, s], format!("{n}.png")));
        assert_eq!(brush(0), [None, sub(16., "T-Icon-Ammo-Medium-32"), sub(24., "T-Icon-Ammo-Medium-32"), sub(32., "T-Icon-Ammo-Medium-32"), sub(40., "T-Icon-Ammo-Medium-64"), None]);
        assert_eq!(brush(1)[2], sub(24., "T-Icon-Shell-32")); assert_eq!(brush(2)[2], sub(24., "T-Icon-Ammo-Heavy-32"));
        assert!(r.ui_images.iter().any(|(n, w, ..)| n == "T-Icon-Ammo-Medium-32.png" && *w > 0), "the card's ammo icon is decoded from the packages");
        assert!(r.ui_images.iter().any(|(n, ..)| n == "T-Icon-Weapons-SK-MachineGun-01.png"));
    }    #[test]
    fn rarity_requires_known_original_value() {
        assert_eq!(rarity("EFortRarity::Sturdy"), Ok(2));
        assert!(rarity("new-custom-rarity").is_err());
    }
}
