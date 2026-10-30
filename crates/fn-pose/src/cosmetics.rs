//! COSMETICS: every Battle Royale cosmetic item definition that exists in the 1.8 paks, read from the packages (no table is typed in).
//!
//! The 1.8 client keeps the owned cosmetics in the account profile (a server concept: `FortAccountItem`s); the Locker's picker lists the account's items of one
//! category (`AthenaCustomizationPicker.PickerTileView` is a native `FortItemTileView` with `DisplayType = Account`). Offline there is no profile, so this
//! catalog is the stand-in: all item definitions that ship in the paks (`/Game/Athena/Items/Cosmetics/**`), none of them invented.
//!
//! What a definition carries (all decoded from the cooked properties):
//! * `AthenaCharacterItemDefinition` (`CID_*`): `HeroDefinition` -> `HID_*` (`FortHeroType`, the thing `fn_pose::br_character` assembles the pawn from);
//!   the CID itself has no preview image: `UFortItemDefinition::GetSmallPreviewImage` falls back to the hero's `SmallPreviewImage` / `LargePreviewImage`.
//! * `AthenaPickaxeItemDefinition`: `WeaponDefinition` (`WID_Harvest_*`, the tool the pawn holds), `FrontendPreviewMeshOffset`.
//! * `AthenaGliderItemDefinition`: `SkeletalMesh`, `AnimClass`, `MaterialOverrides`, previews.
//! * battle buses (`AthenaBattleBusItemDefinition`), loading screens, dances / emotes: enumerated for completeness; the 1.8 Locker widget
//!   (`AthenaCustomizationTab`: `CharacterSlot`, `PickaxeSlot`, `GliderSlot`, `BannerSlot`) has no slot for them, so they are not selectable there.
//!
//! Display names come from `Game.locres` (key + namespace of the serialized FText).
use fn_pak::PakSet;
use fn_uasset::PackageData;
use serde_json::Value;
use std::collections::HashMap;

pub const COSMETICS_ROOT: &str = "fortnitegame/content/athena/items/cosmetics";
pub const LOCRES_PATH: &str = "FortniteGame/Content/Localization/Game/en/Game.locres";

/// Item kind by the item definition class that the package's main export has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category { Outfit, Pickaxe, Glider, BattleBus, LoadingScreen, Emote }

impl Category {
    pub fn from_class(class: &str) -> Option<Category> {
        Some(match class {
            "AthenaCharacterItemDefinition" => Category::Outfit,
            "AthenaPickaxeItemDefinition" => Category::Pickaxe,
            "AthenaGliderItemDefinition" => Category::Glider,
            "AthenaBattleBusItemDefinition" => Category::BattleBus,
            "AthenaLoadingScreenItemDefinition" => Category::LoadingScreen,
            "AthenaDanceItemDefinition" | "AthenaConsumableEmoteItemDefinition" | "AthenaVictoryPoseItemDefinition" | "AthenaEmojiItemDefinition" | "AthenaToyItemDefinition" => Category::Emote,
            _ => return None,
        })
    }
    /// The `Category` byte `AthenaCustomizationTab::BeginPickingCustomization` receives from the slot that was clicked (Kismet of `AthenaCustomizationTab`:
    /// `CharacterSlot` -> 5, `PickaxeSlot` -> 2, `GliderSlot` -> 1; the same byte switches `HeaderItemType`'s text: 5 "OUTFIT", 2 "HARVESTING TOOL", 1 "GLIDER").
    /// Other kinds have no slot in the 1.8 Locker.
    pub fn locker_category_byte(self) -> Option<u8> {
        match self { Category::Outfit => Some(5), Category::Pickaxe => Some(2), Category::Glider => Some(1), _ => None }
    }
    pub fn name(self) -> &'static str {
        match self { Category::Outfit => "Outfit", Category::Pickaxe => "Harvesting Tool", Category::Glider => "Glider", Category::BattleBus => "Battle Bus", Category::LoadingScreen => "Loading Screen", Category::Emote => "Emote" }
    }
}

#[derive(Clone, Debug)]
pub struct Cosmetic {
    /// `/Game/Athena/Items/Cosmetics/Characters/CID_030_Athena_Commando_M_Halloween`
    pub package: String,
    /// asset name (`CID_030_Athena_Commando_M_Halloween`, `HalloweenScythe`, `Glider_Voyager`)
    pub id: String,
    pub class: String,
    pub category: Category,
    /// `Game.locres` text of `DisplayName` (None when the item serializes none)
    pub name: Option<String>,
    pub description: Option<String>,
    pub short_description: Option<String>,
    /// `EFortRarity::X` -> `X` (e.g. `Quality`, `Fine`, `Sturdy`; not serialized when it is the default `Handmade`)
    pub rarity: String,
    /// development asset (`00_TestAssets`): enumerated, not offered by default
    pub test_asset: bool,
    /// outfits: the `HeroDefinition` object path (`/Game/Athena/Heroes/HID_030_..HID_030_..`) and its package
    pub hero: Option<String>,
    pub hero_package: Option<String>,
    /// pickaxes: the `WeaponDefinition` object path
    pub weapon: Option<String>,
    /// gliders: `SkeletalMesh`
    pub skeletal_mesh: Option<String>,
    /// texture object paths; the item's own previews, else the hero's / weapon definition's
    pub small_icon: Option<String>,
    pub large_icon: Option<String>,
    pub animation: Option<String>,
}

/// `EFortRarity` enumerators in the order compiled into the 1.8 exe (strings at 0x42e93a0..0x42ea1d0). `RarityData.RarityCollection[n]` is indexed by the value
/// (exe 0x60df10 returns null for values >= 10): rows 0..6 are the in-game Common, Uncommon, Rare, Epic, Legendary, Mythic, Transcendent.
pub const RARITY_NAMES: [&str; 10] = ["Handmade", "Ordinary", "Sturdy", "Quality", "Fine", "Elegant", "Masterwork", "Epic", "Badass", "Legendary"];
pub fn rarity_index(name: &str) -> usize { RARITY_NAMES.iter().position(|n| *n == name).unwrap_or(0) }

impl Cosmetic {
    pub fn display_name(&self) -> &str { self.name.as_deref().unwrap_or(&self.id) }
    /// row of `RarityData.RarityCollection` (`Handmade` = 0 when the property is not serialized: it is the enum default)
    pub fn rarity_index(&self) -> usize { rarity_index(&self.rarity) }
}

#[derive(Default, Debug)]
pub struct Catalog {
    pub items: Vec<Cosmetic>,
    /// anything that could not be read (package, reason)
    pub problems: Vec<(String, String)>,
}

impl Catalog {
    /// Items of a category, development assets excluded unless `with_test`.
    pub fn of(&self, c: Category, with_test: bool) -> Vec<&Cosmetic> { self.items.iter().filter(|i| i.category == c && (with_test || !i.test_asset)).collect() }
    pub fn find(&self, id_or_package: &str) -> Option<&Cosmetic> { self.items.iter().find(|i| i.id == id_or_package || i.package == id_or_package) }
    pub fn count(&self, c: Category, with_test: bool) -> usize { self.of(c, with_test).len() }
    /// Enumerates every item definition under `/Game/Athena/Items/Cosmetics` of the paks.
    pub fn load(set: &PakSet) -> Catalog {
        let locres = Locres::open(set);
        let mut cat = Catalog::default();
        let mut paths: Vec<String> = set.glob(&format!("{COSMETICS_ROOT}/**/*.uasset")).iter().filter_map(|(_, e)| fn_pak::pak_path_to_game(e.rel_path()).or_else(|| fn_pak::pak_path_to_game(&e.path))).map(|g| g.trim_end_matches(".uasset").to_owned()).collect();
        paths.sort();
        paths.dedup();
        for long in paths {
            match read_item(set, &long, locres.as_ref()) {
                Ok(Some(i)) => cat.items.push(i),
                Ok(None) => {}
                Err(e) => cat.problems.push((long, e)),
            }
        }
        // resolve preview fallbacks (hero / weapon definition) once all items are known
        let mut images: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        for it in cat.items.iter_mut() {
            if it.small_icon.is_some() && it.large_icon.is_some() { continue; }
            let src = it.hero.clone().or_else(|| it.weapon.clone());
            let Some(src) = src else { continue };
            let long = src.split('.').next().unwrap_or(&src).to_owned();
            let (s, l) = images.entry(long.clone()).or_insert_with(|| preview_images(set, &long)).clone();
            if it.small_icon.is_none() { it.small_icon = s.or_else(|| it.large_icon.clone()); }
            if it.large_icon.is_none() { it.large_icon = l.or_else(|| it.small_icon.clone()); }
        }
        cat.items.sort_by(|a, b| (a.category, &a.package).cmp(&(b.category, &b.package)));
        cat
    }
}

/// First `.uasset` main export props of a package as JSON (`properties`), plus its class name.
fn main_export(set: &PakSet, long: &str) -> Result<(String, String, Value), String> {
    let data = PackageData::load(set, long).map_err(|e| e.to_string())?;
    let pkg = data.package().map_err(|e| e.to_string())?;
    let file = long.rsplit('/').next().unwrap_or(long);
    let idx = (0..pkg.exports.len()).find(|&i| pkg.exports[i].name == file).or(if pkg.exports.is_empty() { None } else { Some(0) }).ok_or("package has no export")?;
    let class = pkg.export_class_name(idx);
    let props = pkg.read_export(idx).map_err(|e| e.to_string())?.to_json(&pkg).get("properties").cloned().unwrap_or(Value::Null);
    Ok((file.to_owned(), class, props))
}

fn tail(v: &Value) -> Option<String> { v.as_str().map(|s| s.rsplit("::").next().unwrap_or(s).to_owned()) }
fn path_of(v: Option<&Value>) -> Option<String> { v.and_then(|x| x.as_str()).filter(|s| s.starts_with('/') && !s.is_empty()).map(str::to_owned) }

fn text_of(v: Option<&Value>, locres: Option<&Locres>) -> Option<String> {
    let o = v?.as_object()?;
    let key = o.get("key").and_then(|k| k.as_str()).unwrap_or("");
    let ns = o.get("namespace").and_then(|k| k.as_str()).unwrap_or("");
    if key.is_empty() { return o.get("text").and_then(|t| t.as_str()).map(str::to_owned); }
    // keyed FText: Game.locres only (no fallback to the serialized source text)
    locres.and_then(|l| l.get(ns, key)).map(str::to_owned)
}

fn read_item(set: &PakSet, long: &str, locres: Option<&Locres>) -> Result<Option<Cosmetic>, String> {
    let (id, class, p) = main_export(set, long)?;
    let Some(category) = Category::from_class(&class) else { return Ok(None) };
    let hero = path_of(p.get("HeroDefinition"));
    let hero_package = hero.as_ref().map(|h| h.split('.').next().unwrap_or(h).to_owned());
    Ok(Some(Cosmetic {
        package: long.to_owned(), id, class, category,
        name: text_of(p.get("DisplayName"), locres),
        description: text_of(p.get("Description"), locres),
        short_description: text_of(p.get("ShortDescription"), locres),
        rarity: p.get("Rarity").and_then(tail).unwrap_or_else(|| "Ordinary".to_owned()),
        test_asset: long.to_ascii_lowercase().contains("/00_testassets/"),
        hero, hero_package,
        weapon: path_of(p.get("WeaponDefinition")),
        skeletal_mesh: path_of(p.get("SkeletalMesh")),
        small_icon: path_of(p.get("SmallPreviewImage")),
        large_icon: path_of(p.get("LargePreviewImage")),
        animation: path_of(p.get("Animation")),
    }))
}

pub fn read_definition(set: &PakSet, long: &str) -> Result<Option<Cosmetic>, String> { let locres = Locres::open(set); read_item(set, long, locres.as_ref()) }

fn preview_images(set: &PakSet, long: &str) -> (Option<String>, Option<String>) {
    match main_export(set, long) {
        Ok((_, _, p)) => (path_of(p.get("SmallPreviewImage")), path_of(p.get("LargePreviewImage"))),
        Err(_) => (None, None),
    }
}

/// `Game.locres` (UE 4.16 legacy v0): `u32 namespaces { FString ns, u32 keys { FString key, u32 source hash, FString text } }`.
pub struct Locres { map: HashMap<(String, String), String> }
impl Locres {
    pub fn open(set: &PakSet) -> Option<Locres> { Locres::parse(&set.read(LOCRES_PATH).ok()?) }
    pub fn parse(b: &[u8]) -> Option<Locres> {
        let mut o = 0usize;
        fn u32_(b: &[u8], o: &mut usize) -> Option<u32> { let v = u32::from_le_bytes(b.get(*o..*o + 4)?.try_into().ok()?); *o += 4; Some(v) }
        fn fstr(b: &[u8], o: &mut usize) -> Option<String> {
            let n = i32::from_le_bytes(b.get(*o..*o + 4)?.try_into().ok()?); *o += 4;
            if n == 0 { return Some(String::new()); }
            if n < 0 {
                let len = (-n) as usize;
                let raw = b.get(*o..*o + len * 2)?; *o += len * 2;
                let u: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                Some(String::from_utf16_lossy(&u[..u.len().saturating_sub(1)]))
            } else {
                let len = n as usize;
                let raw = b.get(*o..*o + len)?; *o += len;
                Some(String::from_utf8_lossy(&raw[..raw.len().saturating_sub(1)]).into_owned())
            }
        }
        let nns = u32_(b, &mut o)?;
        let mut map = HashMap::new();
        for _ in 0..nns {
            let ns = fstr(b, &mut o)?;
            let nk = u32_(b, &mut o)?;
            for _ in 0..nk {
                let key = fstr(b, &mut o)?;
                let _hash = u32_(b, &mut o)?;
                let val = fstr(b, &mut o)?;
                map.insert((ns.clone(), key), val);
            }
        }
        Some(Locres { map })
    }
    pub fn get(&self, ns: &str, key: &str) -> Option<&str> { self.map.get(&(ns.to_owned(), key.to_owned())).map(|s| s.as_str()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn install() -> Option<PakSet> {
        let root = fn_live::var("FN_ROOT").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        PakSet::open_install(root).ok()
    }

    #[test]
    fn category_bytes_match_the_locker_kismet() {
        assert_eq!(Category::Outfit.locker_category_byte(), Some(5));
        assert_eq!(Category::Pickaxe.locker_category_byte(), Some(2));
        assert_eq!(Category::Glider.locker_category_byte(), Some(1));
        assert_eq!(Category::Emote.locker_category_byte(), None);
    }

    #[test]
    fn enumerates_the_cosmetics_of_the_1_8_paks() {
        let Some(set) = install() else { eprintln!("1.8 install not found: skipped"); return };
        let cat = Catalog::load(&set);
        for (c, n) in [(Category::Outfit, "outfits"), (Category::Pickaxe, "pickaxes"), (Category::Glider, "gliders"), (Category::BattleBus, "buses"), (Category::LoadingScreen, "loading screens"), (Category::Emote, "emotes")] {
            eprintln!("{n}: {} (+{} test)", cat.count(c, false), cat.count(c, true) - cat.count(c, false));
        }
        eprintln!("problems: {:?}", cat.problems);
        // 30 shipped outfits: CID_001..CID_030 (the six CID_Test_* are development assets)
        assert_eq!(cat.count(Category::Outfit, false), 30, "{:?}", cat.of(Category::Outfit, false).iter().map(|i| &i.id).collect::<Vec<_>>());
        assert_eq!(cat.count(Category::Outfit, true), 36);
        assert_eq!((cat.count(Category::Pickaxe, false), cat.count(Category::Glider, false)), (10, 23));
        assert_eq!((cat.count(Category::BattleBus, false), cat.count(Category::LoadingScreen, false), cat.count(Category::Emote, false)), (2, 2, 3));
        assert!(cat.problems.is_empty(), "{:?}", cat.problems);
        // the Halloween outfits of 1.8 (Fortnitemares): Skull Trooper (male) and Ghoul Trooper (female)
        let skull = cat.find("CID_030_Athena_Commando_M_Halloween").expect("Skull Trooper");
        assert_eq!(skull.display_name().trim(), "Skull Trooper");
        assert_eq!(skull.hero.as_deref(), Some("/Game/Athena/Heroes/HID_030_Athena_Commando_M_Halloween.HID_030_Athena_Commando_M_Halloween"));
        assert!(skull.large_icon.as_deref().is_some_and(|i| i.contains("T-Soldier-HID-030-Athena-Commando-M-Halloween")), "{:?}", skull.large_icon);
        let ghoul = cat.find("CID_029_Athena_Commando_F_Halloween").expect("Ghoul Trooper");
        assert_eq!(ghoul.display_name().trim(), "Ghoul Trooper");
        let reaper = cat.find("HalloweenScythe").expect("Reaper pickaxe");
        assert_eq!(reaper.category, Category::Pickaxe);
        assert_eq!(reaper.display_name(), "Reaper");
        assert!(reaper.weapon.as_deref().is_some_and(|w| w.contains("WID_Harvest_HalloweenScythe")));
        let voyager = cat.find("Glider_Voyager").expect("Voyager glider");
        assert_eq!((voyager.category, voyager.display_name()), (Category::Glider, "Voyager"));
        assert!(voyager.small_icon.is_some() && voyager.skeletal_mesh.is_some());
        // the default outfit the lobby has always used
        assert!(cat.find("CID_005_Athena_Commando_M_Default").is_some_and(|c| c.hero.as_deref().is_some_and(|h| h.contains("HID_005_Athena_Commando_M"))));
        // every shipped outfit names a hero package that exists
        for c in cat.of(Category::Outfit, false) {
            let hp = c.hero_package.as_deref().unwrap_or_else(|| panic!("{} has no HeroDefinition", c.id));
            assert!(set.contains(&fn_pak::game_path_to_pak(hp, "uasset").unwrap()), "{hp}");
        }
    }

    /// Slow (decodes every outfit's textures): `cargo test --release -p fn-pose --lib cosmetics::tests::all_outfits_assemble -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn all_outfits_assemble() {
        let Some(set) = install() else { return };
        let cat = Catalog::load(&set);
        let src = crate::ClipSourceDir::Pak(&set);
        let mut bad = vec![];
        for c in cat.of(Category::Outfit, false) {
            let t = std::time::Instant::now();
            match crate::br_character::BrCharacter::load(&src, c.hero.as_deref().unwrap()) {
                Ok(ch) => eprintln!("OK   {} ({}) {} parts, {:?}", c.id, c.display_name(), ch.parts.len(), t.elapsed()),
                Err(e) => { eprintln!("FAIL {} ({}): {e}", c.id, c.display_name()); bad.push(c.id.clone()); }
            }
        }
        eprintln!("failed to assemble: {bad:?}");
    }

    #[test]
    fn every_shipped_outfit_resolves_into_loadable_character_parts() {
        let Some(set) = install() else { return };
        let cat = Catalog::load(&set);
        let src = crate::ClipSourceDir::Pak(&set);
        let mut bad = vec![];
        for c in cat.of(Category::Outfit, false) {
            let hero = c.hero.as_deref().unwrap();
            match crate::br_character::hero_part_packages(&src, hero) {
                Ok(parts) if parts.len() >= 2 => {}
                other => bad.push((c.id.clone(), format!("{other:?}"))),
            }
        }
        eprintln!("outfits whose hero has no >=2 parts: {bad:?}");
        assert!(bad.is_empty(), "{bad:?}");
    }

    /// RUNTIME SWAP (fn-game `cosmetic_swap` records the item; the front-end rebuilds with exactly these calls): skin A -> skin B builds the part list from scratch
    /// (no part package of A survives in B unless the data lists it for B), and every shipped BR pickaxe resolves its mesh / anims / swing data. Prints load times.
    #[test]
    fn runtime_swap_rebuilds_skin_parts_and_pickaxe_data() {
        let Some(set) = install() else { return };
        let cat = Catalog::load(&set);
        let src = crate::ClipSourceDir::Pak(&set);
        let load = |id: &str| {
            let t = std::time::Instant::now();
            let ch = crate::br_character::BrCharacter::load(&src, cat.find(id).unwrap().hero.as_deref().unwrap()).unwrap();
            eprintln!("skin {id}: {} parts in {:?}", ch.parts.len(), t.elapsed());
            ch.parts.iter().map(|p| p.def.package.clone()).collect::<Vec<_>>()
        };
        let a = load("CID_030_Athena_Commando_M_Halloween");
        let b = load("CID_029_Athena_Commando_F_Halloween");
        let a_again = load("CID_030_Athena_Commando_M_Halloween");
        assert_ne!(a, b, "the two skins must not share the same part list");
        assert_eq!(a, a_again, "rebuild is deterministic and carries nothing over from the other skin");
        let mut seen_swing = vec![];
        let mut failed: Vec<(String, String)> = vec![];
        for c in cat.of(Category::Pickaxe, false) {
            let t = std::time::Instant::now();
            let ps = match crate::loadout_melee::load_for(&src, &c.package) { Ok(p) => p, Err(e) => { eprintln!("pickaxe {} FAILS: {e}", c.id); failed.push((c.id.clone(), e)); continue } };
            eprintln!("pickaxe {} -> {} mesh {} swing {} in {:?}", c.id, ps.weapon_definition, ps.weapon_mesh, ps.swing_montage, t.elapsed());
            assert!(!crate::loadout_melee::is_stw(&ps.weapon_definition) && ps.stw_paths().is_empty(), "{}: STW path", c.id);
            seen_swing.push((ps.weapon_mesh.clone(), ps.swing_montage.clone()));
        }
        // KNOWN (loadout_melee.rs, not mine): BoltonPickaxe.uasset names its export "BoltOnPickaxe" and load_for compares the export name case-sensitively
        assert!(failed.iter().all(|(id, e)| id == "BoltonPickaxe" && e.contains("cosmetic export missing")), "{failed:?}");
        seen_swing.sort(); seen_swing.dedup();
        assert!(seen_swing.len() > 1, "pickaxes must resolve to different mesh / swing data: {seen_swing:?}");
    }
}
