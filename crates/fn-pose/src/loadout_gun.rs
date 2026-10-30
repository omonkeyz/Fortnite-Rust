//! GUN PACKS: everything ONE Battle Royale firearm needs at runtime, read from the 1.8 data only and built WITHOUT touching a live [`Resources`] (so it can run on a worker thread);
//! [`Resources::install_gun`] then merges the pack on the thread that owns the resources. The three starter firearms use the same item reader ([`read_gun_item`]) and the same
//! montage roles ([`ROLES`]); every other WID goes through [`load_gun_pack`].
//!
//! Gun index space (`Resources::gun_*`): 0..[`STARTER_GUNS`] = the starter firearms (`loadout::ITEMS[0..3]`, item index = gun index), then one index per installed pack in install order.
//!
//! What the data decides, and what it does not (never substituted): the montage roles come from the weapon's own Blueprint chain (`loadout_weapon::weapon_anims`); a role the chain leaves
//! unset (or explicitly nulls) stays `None` in [`fn_weapons::Assets`] and the pack's [`GunPack::gaps`] names it. `Assets::actor` is the weapon actor Blueprint package (the audio tables
//! key on it).
use crate::loadout::{object_path, package_name, props, rarity, Item, Resources};
use crate::loadout_ads;
use crate::loadout_ammo::AmmoSetup;
use crate::loadout_inventory::{self, CatalogItem};
use crate::loadout_material::{Material, Texture};
use crate::loadout_stats::{self, StatExtras};
use crate::loadout_weapon::{self, WeaponAnims};
use crate::{loader, ClipSourceDir, Part};
use fn_weapons::recovered_ads::AdsProfile;
use fn_weapons::{Assets, Definition};
use std::collections::{HashMap, HashSet, VecDeque};

/// Firearms that `Resources::load` always loads (`loadout::ITEMS[0..3]`).
pub const STARTER_GUNS: usize = 3;
/// (binding role name used by `Resources::bindings`, montage property of the weapon actor chain, setter of `fn_weapons::Assets`).
pub const ROLES: [(&str, &str); 7] = [
    ("playerEquip", "EquipAnimation"), ("playerFire", "FireAnimation"), ("playerFireADS", "FireDownsightsAnimation"), ("playerReload", "ReloadAnimation"),
    ("weaponFire", "WeaponFireMontage"), ("weaponFireADS", "WeaponFireDownsightsMontage"), ("weaponReload", "WeaponReloadMontage"),
];

/// A firearm installed after start-up (gun index `STARTER_GUNS + position`).
pub struct ExtraGun {
    pub item: Item,
    pub definition: Definition,
    pub extras: StatExtras,
    pub ads: AdsProfile,
    pub anims: WeaponAnims,
}

/// What a worker read for one weapon. `scratch` holds only the animation / material packages the live resources did not have yet.
pub struct GunPack {
    pub wid: String,
    pub gun: ExtraGun,
    /// Inventory-screen texts of the weapon (`loadout_inventory::entry`).
    pub inventory: CatalogItem,
    /// HUD textures (item icon, ammo icon, scope reticle) not yet in the resources: (PNG name, w, h, RGBA8).
    pub ui_images: Vec<(String, u32, u32, Vec<u8>)>,
    /// The weapon's ammo item (+ the game mode's starting items): the inventory side of its ammo type.
    pub ammo: AmmoSetup,
    scratch: Resources,
    /// Per-step wall time (seconds), for load-time reports.
    pub timings: Vec<(&'static str, f32)>,
    /// What the weapon's data lacks (montage roles / core assets), straight from `WeaponAnims::missing`.
    pub gaps: Vec<String>,
}

/// What [`Resources::install_gun`] added.
#[derive(Debug, Clone)]
pub struct GunInstalled { pub gun: usize, /** package paths of the sequences that were not loaded before */ pub new_sequences: Vec<String>, /** `CoreAnims_Male` keys that were not loaded before */ pub new_cores: Vec<String> }

/// What the live resources already hold (a snapshot taken on the owning thread before a worker starts): packages are not read twice.
#[derive(Default, Clone, Debug)]
pub struct Known { pub anim_packages: HashSet<String>, pub materials: HashSet<String>, pub textures: HashSet<String>, pub ui_images: HashSet<String>, pub cores: HashSet<String> }

/// Montage binding of one role as an `Assets` package path (object suffix removed): `None` = the data leaves the role unset / null.
pub fn role_package(w: &WeaponAnims, prop: &str) -> Option<String> { w.montage(prop).map(|p| package_name(p).to_owned()) }
/// Fills the montage roles and the actor Blueprint of `a` from the weapon's resolved data.
pub fn assign_assets(a: &mut Assets, w: &WeaponAnims) {
    a.actor = Some(package_name(&w.actor).to_owned());
    for (role, prop) in ROLES {
        let p = role_package(w, prop);
        match role { "playerEquip" => a.player_equip = p, "playerFire" => a.player_fire = p, "playerFireADS" => a.player_fire_ads = p, "playerReload" => a.player_reload = p, "weaponFire" => a.weapon_fire = p, "weaponFireADS" => a.weapon_fire_ads = p, "weaponReload" => a.weapon_reload = p, _ => {} }
    }
}
/// The `Resources::bindings` rows (item, role, package) of a weapon: only the roles the data sets.
pub fn bindings(item: &str, w: &WeaponAnims) -> Vec<(String, String, String)> {
    ROLES.iter().filter_map(|(role, prop)| role_package(w, prop).map(|p| (item.to_owned(), (*role).to_owned(), p))).collect()
}

/// (display name, rarity 0..4) of a ranged WID without reading anything else (the catalog read of the console); the rarity follows the same default rule as [`read_gun_item`].
pub fn weapon_texts(src: &ClipSourceDir, wid: &str) -> Result<(Option<String>, u8), String> {
    let pkg = loader::package(src, wid)?;
    let class = (0..pkg.exports.len()).map(|i| pkg.export_class(i)).find(|c| c.contains("ItemDefinition")).ok_or_else(|| format!("{wid}: no item definition export"))?;
    let p = props(&pkg, &class)?;
    let icon = p.object("SmallPreviewImage").filter(|i| *i < 0).map(|i| pkg.import_path(i)).unwrap_or_default();
    let e = loadout_inventory::entry(wid, &class, &p, &icon);
    let name = e.name.trim().to_owned();
    Ok(((!name.is_empty()).then_some(name), rarity(p.text("Rarity").unwrap_or(DEFAULT_RARITY))?))
}
/// The tier a WID without a serialized `Rarity` has (see [`read_gun_item`]: INFERENCE).
pub const DEFAULT_RARITY: &str = "EFortRarity::Ordinary";

/// Reads one ranged WID: item data, mesh, materials identities, stat-row definition. `Definition::assets` is left empty (see [`assign_assets`]).
/// Returns the item, the definition + stat extras and the weapon actor Blueprint package.
pub fn read_gun_item(src: &ClipSourceDir, path: &str) -> Result<(Item, Definition, StatExtras, String), String> {
    let pkg = loader::package(src, path)?;
    let class = (0..pkg.exports.len()).map(|i| pkg.export_class(i)).find(|c| c.contains("ItemDefinition")).ok_or_else(|| format!("{path}: no item definition export"))?;
    if class != "FortWeaponRangedItemDefinition" { return Err(format!("{path}: {class} is not a ranged weapon definition")); }
    let properties = props(&pkg, &class)?;
    let icon_object = object_path(&pkg, &properties, "SmallPreviewImage")?;
    let icon = format!("{}.png", package_name(&icon_object).rsplit('/').next().ok_or("invalid icon reference")?);
    let mesh_ref = object_path(&pkg, &properties, "WeaponMeshOverride")?;
    let mesh_pkg = loader::package(src, package_name(&mesh_ref))?;
    let decoded = fn_mesh::SkeletalMesh::parse(&mesh_pkg).map_err(|e| e.to_string())?;
    let materials = decoded.lods.first().ok_or("gun mesh lacks LOD0")?.sections.iter().enumerate()
        .map(|(section, _)| decoded.materials.get(decoded.section_material_index(0, section)).map(|m| m.material.clone()).ok_or("gun section material index out of range"))
        .collect::<Result<Vec<_>, _>>()?;
    let skeleton = decoded.skeleton_path.clone().filter(|p| p.starts_with("/Game/")).ok_or("original gun mesh lacks game skeleton identity")?;
    let mesh = Part::from_skeletal_mesh(&decoded.name, &decoded, 0)?;
    // INFERENCE (labelled): every `_UC_` weapon of the 1.8 data omits `Rarity` while `_C_` serializes Handmade, `_R_` Sturdy, `_VR_` Quality and `_SR_` Fine. A property equal to the class default is not
    // serialized, and Handmade is serialized, so the default is not Handmade; on the 5-step ladder the omitted tier is Ordinary (= uncommon, what the `UC` in the asset name says).
    let rarity_text = properties.text("Rarity").unwrap_or(DEFAULT_RARITY);
    let r = rarity(rarity_text)?;
    let (definition, extras) = loadout_stats::ranged_definition(src, path, &pkg, &properties, None, rarity_text)?;
    let actor = package_name(&object_path(&pkg, &properties, "WeaponActorClass")?).to_owned();
    let core = loadout_weapon::actor_chain(src, &actor)?.core()?.0;
    let item = Item { definition: path.to_owned(), icon, icon_path: icon_object, rarity: r, quantity: 1, mesh: Some(mesh), mesh_skeleton: Some(skeleton), mesh_materials: materials, core_animation: Some(core), properties };
    Ok((item, definition, extras, actor))
}

/// `loadout_material::load_closure` that stops at materials already known.
fn materials_skipping(src: &ClipSourceDir, roots: impl IntoIterator<Item = String>, known: &HashSet<String>) -> Result<HashMap<String, Material>, String> {
    let mut out = HashMap::<String, Material>::new();
    for root in roots {
        let mut path = Some(root);
        let mut chain = HashSet::new();
        while let Some(current) = path {
            if !chain.insert(current.clone()) { return Err(format!("original material parent cycle: {current}")); }
            if chain.len() > 64 { return Err("material parent depth limit".into()); }
            if known.contains(&current) { break; }
            if let Some(existing) = out.get(&current) { path = existing.parent.clone(); continue; }
            let m = Material::load(src, &current)?;
            path = m.parent.clone();
            out.insert(current, m);
        }
    }
    Ok(out)
}
/// `loadout_material::load_parameter_textures` that skips textures already known.
fn textures_skipping(src: &ClipSourceDir, materials: &HashMap<String, Material>, known: &HashSet<String>) -> Result<HashMap<String, Texture>, String> {
    let wanted: Vec<&String> = materials.values().flat_map(|m| m.texture_parameters.values().flatten()).filter(|t| !known.contains(*t)).collect();
    let mut out = HashMap::new();
    for path in wanted {
        if out.contains_key(path) { continue; }
        let one = crate::loadout_material::load_texture(src, path)?;
        out.insert(path.clone(), one);
    }
    Ok(out)
}

/// Reads everything the runtime needs for `wid` (a `/Game/...` WID package path). Pure reads of the paks; nothing is shared with a live `Resources` except the `known` snapshot.
pub fn load_gun_pack(src: &ClipSourceDir, wid: &str, known: &Known, hand_socket: &fn_mesh::Socket) -> Result<GunPack, String> {
    let mut timings = vec![];
    let t = std::time::Instant::now();
    let mut lap = |name: &'static str, t: &mut std::time::Instant| { timings.push((name, t.elapsed().as_secs_f32())); *t = std::time::Instant::now(); };
    let mut t = t;
    let (item, mut definition, extras, actor) = read_gun_item(src, wid)?;
    lap("item+mesh+stat row", &mut t);
    let anims = loadout_weapon::weapon_anims(src, wid, false)?;
    if anims.actor.is_empty() || package_name(&anims.actor) != actor { return Err(format!("{wid}: actor mismatch {} / {actor}", anims.actor)); }
    assign_assets(&mut definition.assets, &anims);
    lap("actor chain + montage roles", &mut t);
    let ads = loadout_ads::profile(src, wid).map_err(|e| format!("{wid}: ADS profile: {e}"))?;
    lap("ads profile", &mut t);
    let mut scratch = Resources::scratch(hand_socket.clone());
    // animations: the core entry (once per core), then every referenced montage / core asset package that is not loaded yet
    let mut queue = VecDeque::new();
    if !known.cores.contains(&anims.core) {
        let graph = loader::package(src, loadout_weapon::PLAYER_ANIM_BP)?;
        let map = loadout_weapon::core_map(&graph, "CoreAnims_Male")?;
        let (_, entry) = map.into_iter().find(|(k, _)| *k == anims.core).ok_or_else(|| format!("CoreAnims_Male has no entry {}", anims.core))?;
        let mut paths = HashMap::new();
        for a in &anims.core_assets { paths.insert((a.raw.clone(), a.index), a.path.clone()); }
        scratch.core_asset_paths.insert(anims.core.clone(), paths);
        scratch.core_anims.insert(anims.core.clone(), entry);
    }
    for p in anims.packages() { queue.push_back(p); }
    scratch.absorb_skipping(src, queue, &known.anim_packages)?;
    lap("animation closure", &mut t);
    // materials of the mesh (stand-in / original gun draw read their textures from here)
    let mats = materials_skipping(src, item.mesh_materials.iter().flatten().cloned(), &known.materials)?;
    let tex = textures_skipping(src, &mats, &known.textures)?;
    scratch.materials = mats; scratch.material_textures = tex;
    lap("materials + textures", &mut t);
    // HUD art
    let mut ui_images = vec![];
    let want = |name: String, path: String, ui: &mut Vec<(String, u32, u32, Vec<u8>)>| -> Result<(), String> {
        if known.ui_images.contains(&name) || ui.iter().any(|(n, ..)| *n == name) { return Ok(()); }
        ui.push(loadout_stats::decode_ui_texture(src, &path)?); Ok(())
    };
    want(item.icon.clone(), item.icon_path.clone(), &mut ui_images)?;
    want(extras.ammo_icon.clone(), extras.ammo_icon_path.clone(), &mut ui_images)?;
    for (_, name, path) in extras.ammo_icon_brush.iter().flatten() { want(name.clone(), path.clone(), &mut ui_images)?; } // QUICKBAR AMMO: the card's ammo icon
    if let Some(scope) = ads.scope.as_ref() {
        let name = format!("{}.png", package_name(&scope.reticle_texture).rsplit('/').next().unwrap_or_default());
        want(name, scope.reticle_texture.clone(), &mut ui_images)?;
    }
    lap("hud textures", &mut t);
    let ammo = crate::loadout_ammo::read(src, &[definition.ammo.clone()])?;
    lap("ammo", &mut t);
    let mut inventory = loadout_inventory::entry(&item.definition, "FortWeaponRangedItemDefinition", &item.properties, &item.icon_path);
    inventory.rarity = item.rarity; // the entry reads the serialized property only (see the `_UC_` default in read_gun_item)
    let gaps = anims.missing();
    Ok(GunPack { wid: wid.to_owned(), gun: ExtraGun { item, definition, extras, ads, anims }, inventory, ui_images, ammo, scratch, timings, gaps })
}

impl Resources {
    /// An empty resource set (a worker's scratch space): only the player's hand socket is carried.
    pub(crate) fn scratch(player_hand_socket: fn_mesh::Socket) -> Resources {
        Resources { player_hand_socket, items: vec![], bindings: vec![], montages: HashMap::new(), composites: HashMap::new(), sequences: HashMap::new(), blendspaces: HashMap::new(), core_anims: HashMap::new(), core_asset_paths: HashMap::new(),
            materials: HashMap::new(), material_textures: HashMap::new(), definitions: vec![], stat_extras: vec![], healables: vec![], ui_images: vec![], ammo: Default::default(), ads: vec![], inventory: vec![], extra_guns: vec![], montage_curves: HashMap::new() }
    }
    /// What is loaded already (see [`Known`]).
    pub fn known(&self) -> Known {
        Known {
            anim_packages: self.montages.keys().chain(self.composites.keys()).chain(self.blendspaces.keys()).chain(self.sequences.keys()).cloned().collect(),
            materials: self.materials.keys().cloned().collect(), textures: self.material_textures.keys().cloned().collect(),
            ui_images: self.ui_images.iter().map(|(n, ..)| n.clone()).collect(), cores: self.core_anims.keys().cloned().collect(),
        }
    }
    /// Number of firearms (starters + installed packs).
    pub fn gun_count(&self) -> usize { STARTER_GUNS.min(self.items.len()) + self.extra_guns.len() }
    pub fn gun_item(&self, g: usize) -> Option<&Item> { if g < STARTER_GUNS { self.items.get(g) } else { self.extra_guns.get(g - STARTER_GUNS).map(|e| &e.item) } }
    pub fn gun_definition(&self, g: usize) -> Option<&Definition> { if g < STARTER_GUNS { self.definitions.get(g)?.as_ref() } else { self.extra_guns.get(g - STARTER_GUNS).map(|e| &e.definition) } }
    pub fn gun_extras(&self, g: usize) -> Option<&StatExtras> { if g < STARTER_GUNS { self.stat_extras.get(g)?.as_ref() } else { self.extra_guns.get(g - STARTER_GUNS).map(|e| &e.extras) } }
    pub fn gun_ads(&self, g: usize) -> Option<&AdsProfile> { if g < STARTER_GUNS { self.ads.get(g) } else { self.extra_guns.get(g - STARTER_GUNS).map(|e| &e.ads) } }
    /// Gun index of an installed weapon by WID package path (with or without the object suffix).
    pub fn gun_index_of(&self, wid: &str) -> Option<usize> {
        let w = package_name(wid);
        (0..self.gun_count()).find(|g| self.gun_item(*g).is_some_and(|i| package_name(&i.definition) == w))
    }
    /// Library for a firearm's own mesh skeleton (`weapon_clips` by gun index; installed packs included).
    pub fn gun_clips(&self, g: usize) -> Result<crate::ClipStore, String> {
        let item = self.gun_item(g).ok_or("gun index out of range")?;
        self.clips_for_item(item)
    }
    /// Merges a pack; returns the new gun index and what it newly added. Existing entries are never replaced.
    pub fn install_gun(&mut self, pack: GunPack) -> Result<GunInstalled, String> {
        if self.gun_index_of(&pack.wid).is_some() { return Err(format!("{} is already installed", pack.wid)); }
        let GunPack { gun, inventory, ui_images, ammo, scratch, .. } = pack;
        let Resources { montages, composites, sequences, blendspaces, core_anims, core_asset_paths, materials, material_textures, .. } = scratch;
        for (k, v) in montages { self.montages.entry(k).or_insert(v); }
        for (k, v) in composites { self.composites.entry(k).or_insert(v); }
        let mut new_sequences = vec![];
        for (k, v) in sequences { if !self.sequences.contains_key(&k) { new_sequences.push(k.clone()); self.sequences.insert(k, v); } }
        let new_core = core_anims.keys().filter(|k| !self.core_anims.contains_key(*k)).cloned().collect::<Vec<_>>();
        for (k, v) in blendspaces { self.blendspaces.entry(k).or_insert(v); }
        for (k, v) in core_anims { self.core_anims.entry(k).or_insert(v); }
        for (k, v) in core_asset_paths { self.core_asset_paths.entry(k).or_insert(v); }
        for (k, v) in materials { self.materials.entry(k).or_insert(v); }
        for (k, v) in material_textures { self.material_textures.entry(k).or_insert(v); }
        for im in ui_images { if !self.ui_images.iter().any(|(n, ..)| *n == im.0) { self.ui_images.push(im); } }
        for d in ammo.defs { if self.ammo.def_index(&d.item).is_none() { self.ammo.defs.push(d); } }
        for r in bindings(&gun.item.definition, &gun.anims) { self.bindings.push(r); }
        self.inventory.push(inventory);
        self.extra_guns.push(gun);
        Ok(GunInstalled { gun: STARTER_GUNS + self.extra_guns.len() - 1, new_sequences, new_cores: new_core })
    }
    /// Undoes the LAST [`Resources::install_gun`] (the animation / material packages it added stay: they are harmless shared data). Used when a later install step fails so gun indices stay aligned.
    pub fn uninstall_last_gun(&mut self) {
        if let Some(g) = self.extra_guns.pop() { self.inventory.pop(); self.bindings.retain(|(item, ..)| *item != g.item.definition); }
    }
    /// The player-skeleton clips of `new_sequences` (the paths `install_gun` reported) as the character's clip library names them: the full package path always, the short name when it is unambiguous among ALL
    /// player sequences held (the animation graph asks for short names), plus, for every core set in `new_cores`, the short name of each of its core assets bound to that asset's exact path (so a short name two directories
    /// share still resolves for the weapon whose core set names that exact asset; `first wins` when two newly installed cores disagree, reported in the returned conflicts).
    pub fn new_player_clips(&self, new_sequences: &[String], new_cores: &[String]) -> (crate::ClipStore, Vec<String>) {
        let mut store = crate::ClipStore::new();
        let mut conflicts = vec![];
        let mut counts = HashMap::<&str, usize>::new();
        for s in self.sequences.values().filter(|s| s.skeleton == crate::loadout::PLAYER_SKELETON) { *counts.entry(&s.clip.name).or_default() += 1; }
        for path in new_sequences {
            let Some(s) = self.sequences.get(path).filter(|s| s.skeleton == crate::loadout::PLAYER_SKELETON) else { continue };
            store.insert(path, s.clip.clone());
            if counts.get(s.clip.name.as_str()) == Some(&1) { store.insert(&s.clip.name, s.clip.clone()); }
        }
        for core in new_cores {
            let Some(paths) = self.core_asset_paths.get(core) else { continue };
            let mut fields: Vec<_> = paths.iter().collect();
            fields.sort_by(|a, b| a.0.cmp(b.0));
            for (_, p) in fields {
                let Some(p) = p else { continue };
                let pkg = package_name(p);
                let Some(seq) = self.sequences.get(pkg).filter(|s| s.skeleton == crate::loadout::PLAYER_SKELETON) else { continue };
                let short = pkg.rsplit('/').next().unwrap_or(pkg);
                match store.clips.get(short) { None => store.insert(short, seq.clip.clone()), Some(_) if counts.get(short) == Some(&1) => {} Some(_) => conflicts.push(format!("core {core}: short name {short} is claimed by two different assets, the first stays")) }
            }
        }
        (store, conflicts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loadout::ITEMS;
    fn install() -> fn_pak::PakSet { fn_pak::PakSet::open_install(fn_live::var_os("FN_TEST_ROOT").expect("set FN_TEST_ROOT to the local 1.8 install")).unwrap() }

    /// The data-driven montage roles of the three starters are exactly the 21 bindings the old hand-recovered JSON held (so replacing the JSON changed nothing).
    #[test]
    fn starter_roles_from_the_actor_chain_equal_the_recovered_bindings() {
        let set = install();
        let src = ClipSourceDir::Pak(&set);
        let json: serde_json::Value = serde_json::from_str(include_str!("../data/loadout-bindings.json")).unwrap();
        let mut want: Vec<(String, String, String)> = json.as_array().unwrap().iter().map(|b| (b["item"].as_str().unwrap().to_owned(), b["role"].as_str().unwrap().to_owned(), b["package"].as_str().unwrap().to_owned())).collect();
        let mut got = vec![];
        for wid in &ITEMS[..STARTER_GUNS] { got.extend(bindings(wid, &loadout_weapon::weapon_anims(&src, wid, false).unwrap())); }
        want.sort(); got.sort();
        assert_eq!(got, want);
    }

    /// Every one of the 45 BR firearms loads as a pack and installs; timings are printed (a worker thread pays them, not the frame).
    #[test]
    fn every_br_weapon_loads_as_a_pack_and_installs() {
        let set = install();
        let src = ClipSourceDir::Pak(&set);
        let mut res = Resources::load(&src).unwrap();
        let t0 = std::time::Instant::now();
        let mut slowest = (0f32, String::new());
        for w in fn_animgraph::weapon_assets::BR_WEAPON_ASSETS {
            let wid = format!("/Game/Athena/Items/Weapons/{}", w.wid);
            if res.gun_index_of(&wid).is_some() { continue; }
            let t = std::time::Instant::now();
            let pack = load_gun_pack(&src, &wid, &res.known(), &res.player_hand_socket).unwrap_or_else(|e| panic!("{}: {e}", w.wid));
            let secs = t.elapsed().as_secs_f32();
            if secs > slowest.0 { slowest = (secs, w.wid.to_owned()); }
            eprintln!("{:55} {:.2}s [{}] gaps {:?}", w.wid, secs, pack.timings.iter().map(|(n, s)| format!("{n} {s:.2}")).collect::<Vec<_>>().join(", "), pack.gaps);
            assert_eq!(pack.gun.definition.assets.actor.as_deref().map(|a| a.rsplit('/').next().unwrap()), Some(w.actor), "{}", w.wid);
            let g = res.install_gun(pack).unwrap().gun;
            assert_eq!(res.gun_definition(g).unwrap().item, wid);
            assert!(res.gun_clips(g).is_ok());
        }
        eprintln!("45 packs in {:.1}s, slowest {:?}; resources now: {} sequences, {} montages, {} materials", t0.elapsed().as_secs_f32(), slowest, res.sequences.len(), res.montages.len(), res.materials.len());
        assert!(res.gun_count() >= 45);
    }
}
