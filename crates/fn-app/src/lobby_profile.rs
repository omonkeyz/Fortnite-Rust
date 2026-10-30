//! OFFLINE PROFILE (substitute): the account data the 1.8 client got from Epic's servers and that is NOT in the paks.
//!
//! In the 1.8 client the wallet (`FortPrivateAccountInfo.MtxBalance`, shown by `TopBar.MTXButton.Amount`), the owned cosmetics (`FortAccountItem`s of the
//! profile) and the equipped loadout (`AthenaCharacter` / `AthenaPickaxe` / `AthenaGlider` slots of the Locker) live in the Athena profile on the backend.
//! Offline there is no backend and no real commerce, so this file is the single local stand-in shared by the Item Shop and the Locker:
//! * V-Bucks balance (default [`DEFAULT_VBUCKS`] = 1,000,000), freely editable (console `addvbucks` / `setvbucks`);
//! * owned item ids (asset names of `/Game/Athena/Items/Cosmetics/**`, e.g. `CID_005_Athena_Commando_M_Default`, `HalloweenScythe`, `Glider_Voyager`).
//!   DEFAULT = EVERYTHING: a fresh profile (and `reset`) owns every Battle Royale cosmetic definition of the 1.8 paks (all categories `fn_pose::cosmetics` enumerates:
//!   outfits, harvesting tools, gliders, battle buses, loading screens, emotes; development assets excluded), enumerated from the pak data by
//!   [`starting_items_from_paks`] (nothing typed in, nothing from Save the World). `clear_items` / `removeitem` give back the empty-ish state (default items only);
//! * the equipped item per Locker slot;
//! * `shop_day_offset`: days added to the UTC date that seeds the offline shop rotation (console `rotateshop`).
//!
//! It is stored as a small JSON file written by Rust (default: `fortnite-offline-profile.json` next to the executable). No network, no account, no real money.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The starting wallet of the offline profile.
pub const DEFAULT_VBUCKS: i64 = 1_000_000;
/// Hard cap of the wallet (keeps `i64` arithmetic and the TopBar number sane).
pub const MAX_VBUCKS: i64 = 999_999_999_999;

/// Locker slots of the 1.8 `AthenaCustomizationTab` that take a cosmetic item (`CharacterSlot`, `PickaxeSlot`, `GliderSlot`; the `BannerSlot` has no item).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot { Outfit, Pickaxe, Glider }

impl Slot {
    pub const ALL: [Slot; 3] = [Slot::Outfit, Slot::Pickaxe, Slot::Glider];
    pub fn key(self) -> &'static str { match self { Slot::Outfit => "outfit", Slot::Pickaxe => "pickaxe", Slot::Glider => "glider" } }
    pub fn title(self) -> &'static str { match self { Slot::Outfit => "Outfit", Slot::Pickaxe => "Harvesting Tool", Slot::Glider => "Glider" } }
    /// Accepts the slot spellings a console user would type.
    pub fn parse(s: &str) -> Option<Slot> {
        match s.trim().to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "outfit" | "skin" | "character" | "char" | "cid" => Some(Slot::Outfit),
            "pickaxe" | "harvestingtool" | "harvest" | "axe" | "tool" => Some(Slot::Pickaxe),
            "glider" | "umbrella" | "glide" => Some(Slot::Glider),
            _ => None,
        }
    }
}

/// What the shop asks the profile to buy: an offer id (offline-substitute offers are `offer:<item id>`), the item it grants and its V-Bucks price.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurchaseRequest { pub offer_id: String, pub item_id: String, pub price: i64 }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt { pub offer_id: String, pub item_id: String, pub price: i64, pub balance_after: i64 }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurchaseError {
    /// `AthenaDirectAcquisitionOfferDetailsWidget.SetupPrice`: `CanPurchaseOffer` result 2 = "Cannot Afford"
    InsufficientFunds { need: i64, have: i64 },
    /// the offer widgets show the `OwnedSwitcher` "OWNED" state instead of the price
    AlreadyOwned,
    InvalidPrice,
}
impl std::fmt::Display for PurchaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PurchaseError::InsufficientFunds { need, have } => write!(f, "Cannot Afford: needs {need} V-Bucks, balance is {have}"),
            PurchaseError::AlreadyOwned => write!(f, "already owned"),
            PurchaseError::InvalidPrice => write!(f, "invalid price"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EquipError { NotOwned(String) }
impl std::fmt::Display for EquipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { match self { EquipError::NotOwned(i) => write!(f, "{i} is not owned") } }
}

#[derive(Clone, Debug)]
pub struct Profile {
    vbucks: i64,
    owned: BTreeSet<String>,
    loadout: BTreeMap<Slot, String>,
    /// days added to the UTC date that seeds the shop rotation (console `rotateshop`)
    pub shop_day_offset: i64,
    display_name: Option<String>,
    season_level: i32,
    season_xp: i32,
    banner: Option<(String, String)>,
    match_options: BTreeMap<String, usize>,
    /// items every fresh profile owns (the 1.8 default outfits / harvesting tool / glider); re-applied on load and `reset`
    defaults: Vec<(Slot, String)>,
    /// items a fresh / reset profile owns on top of the defaults (default: every Athena cosmetic of the paks)
    starting: Vec<String>,
    path: Option<PathBuf>,
    dirty: bool,
}

/// `fortnite-offline-profile.json` next to the executable (falls back to the working directory).
pub fn default_profile_path() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("fortnite-offline-profile.json"))).unwrap_or_else(|| PathBuf::from("fortnite-offline-profile.json"))
}

pub fn stored_display_name(path: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("display_name").and_then(|x| x.as_str()).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

pub fn stored_loadout(path: &Path) -> Vec<(String, String)> {
    let Some(v) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return vec![] };
    v.get("loadout").and_then(|l| l.as_object()).map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned()))).collect()).unwrap_or_default()
}

pub fn store_display_name(path: &Path, name: &str) -> std::io::Result<()> {
    let mut v: Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
    if !v.is_object() { v = json!({}); }
    v["display_name"] = json!(name.trim());
    if let Some(dir) = path.parent() { if !dir.as_os_str().is_empty() { std::fs::create_dir_all(dir)?; } }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}

impl Profile {
    /// A fresh in-memory profile: 1,000,000 V-Bucks, the `defaults` owned, the first default of each slot equipped.
    pub fn new(defaults: Vec<(Slot, String)>) -> Profile {
        let mut p = Profile { vbucks: DEFAULT_VBUCKS, owned: BTreeSet::new(), loadout: BTreeMap::new(), shop_day_offset: 0, display_name: None, season_level: 1, season_xp: 0, banner: None, match_options: BTreeMap::new(), defaults, starting: vec![], path: None, dirty: true };
        p.apply_defaults();
        p
    }

    /// Sets the starting items (granted now, and again by `reset`).
    pub fn with_starting_items(mut self, items: Vec<String>) -> Profile {
        self.starting = items;
        for i in self.starting.clone() { self.owned.insert(i); }
        self
    }
    pub fn starting_items(&self) -> &[String] { &self.starting }

    fn apply_defaults(&mut self) {
        for (slot, id) in self.defaults.clone() {
            self.owned.insert(id.clone());
            self.loadout.entry(slot).or_insert(id);
        }
    }

    /// Load `path` (missing / unreadable / corrupt file = a fresh profile bound to `path`, the old file is left alone until the next save). A fresh profile owns
    /// everything ([`starting_items_from_paks`]); a file written before the starting items existed gets them granted once.
    pub fn load_or_new(path: &Path, defaults: Vec<(Slot, String)>) -> Profile { Profile::load_or_new_with(path, defaults, starting_items_from_paks()) }

    /// Like [`Profile::load_or_new`] with an explicit starting-item list (tests, tools).
    pub fn load_or_new_with(path: &Path, defaults: Vec<(Slot, String)>, starting: Vec<String>) -> Profile {
        let mut p = Profile::new(defaults).with_starting_items(starting);
        p.path = Some(path.to_path_buf());
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                // a profile saved by this version already decided what it owns (a removed item stays removed); an older one gets the starting items once
                let seeded = v.get("starting_items_granted").and_then(|x| x.as_bool()).unwrap_or(false);
                if seeded { p.owned.clear(); p.apply_defaults(); }
                p.apply_json(&v);
                if !seeded { for i in p.starting.clone() { p.owned.insert(i); } p.dirty = true; } else { p.dirty = false; }
            }
        }
        p
    }
    fn apply_json(&mut self, v: &Value) {
        if let Some(n) = v.get("vbucks").and_then(|x| x.as_i64()) { self.vbucks = n.clamp(0, MAX_VBUCKS); }
        if let Some(a) = v.get("owned").and_then(|x| x.as_array()) { for i in a.iter().filter_map(|x| x.as_str()) { self.owned.insert(i.to_owned()); } }
        if let Some(o) = v.get("loadout").and_then(|x| x.as_object()) {
            for slot in Slot::ALL { if let Some(id) = o.get(slot.key()).and_then(|x| x.as_str()) { self.loadout.insert(slot, id.to_owned()); } }
        }
        if let Some(n) = v.get("shop_day_offset").and_then(|x| x.as_i64()) { self.shop_day_offset = n; }
        if let Some(n) = v.get("season_level").and_then(|x| x.as_i64()) { self.season_level = (n as i32).max(1); }
        if let Some(n) = v.get("season_xp").and_then(|x| x.as_i64()) { self.season_xp = (n as i32).max(0); }
        if let Some(n) = v.get("display_name").and_then(|x| x.as_str()).filter(|s| !s.trim().is_empty()) { self.display_name = Some(n.to_owned()); }
        if let (Some(i), Some(c)) = (v.get("banner").and_then(|b| b.get("icon")).and_then(|x| x.as_str()), v.get("banner").and_then(|b| b.get("color")).and_then(|x| x.as_str())) { self.banner = Some((i.to_owned(), c.to_owned())); }
        if let Some(o) = v.get("match_options").and_then(|x| x.as_object()) { for (k, n) in o { if let Some(n) = n.as_u64() { self.match_options.insert(k.clone(), n as usize); } } }
        self.apply_defaults();
        // the loadout may only hold owned items
        let bad: Vec<Slot> = self.loadout.iter().filter(|(_, id)| !self.owned.contains(*id)).map(|(s, _)| *s).collect();
        for s in bad { self.loadout.remove(&s); }
        self.apply_defaults();
    }

    pub fn to_json(&self) -> Value {
        let mut lo = serde_json::Map::new();
        for (s, id) in &self.loadout { lo.insert(s.key().to_owned(), json!(id)); }
        json!({ "_note": "OFFLINE substitute for the account profile (no server, no real money)", "vbucks": self.vbucks, "owned": self.owned.iter().collect::<Vec<_>>(), "loadout": Value::Object(lo), "shop_day_offset": self.shop_day_offset, "display_name": self.display_name, "season_level": self.season_level, "season_xp": self.season_xp, "banner": self.banner.as_ref().map(|(i, c)| json!({ "icon": i, "color": c })), "match_options": self.match_options, "starting_items_granted": true })
    }

    /// Write the profile when it changed since the last save (no-op for a profile without a path). Returns the io error, if any.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if !self.dirty { return Ok(()); }
        let Some(path) = self.path.clone() else { return Ok(()) };
        self.save_to(&path)?;
        self.dirty = false;
        Ok(())
    }
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() { if !dir.as_os_str().is_empty() { std::fs::create_dir_all(dir)?; } }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.to_json()).unwrap_or_default())?;
        std::fs::rename(&tmp, path)
    }
    pub fn is_dirty(&self) -> bool { self.dirty }
    pub fn display_name(&self) -> Option<&str> { self.display_name.as_deref() }
    pub fn season_level(&self) -> i32 { self.season_level }
    pub fn season_xp(&self) -> i32 { self.season_xp }
    pub fn set_display_name(&mut self, name: &str) { let n = name.trim(); self.display_name = (!n.is_empty()).then(|| n.to_owned()); self.dirty = true; }

    // ---- wallet -----------------------------------------------------------------------------------------------------------------------------
    /// The number `TopBar.MTXButton.Amount` shows (`LobbyState::mtx_balance`).
    pub fn balance(&self) -> i64 { self.vbucks }
    pub fn set_vbucks(&mut self, n: i64) { self.vbucks = n.clamp(0, MAX_VBUCKS); self.dirty = true; }
    /// Adds (or subtracts, for negative `n`) and returns the new balance (clamped to 0..=MAX_VBUCKS).
    pub fn add_vbucks(&mut self, n: i64) -> i64 { self.vbucks = self.vbucks.saturating_add(n).clamp(0, MAX_VBUCKS); self.dirty = true; self.vbucks }

    // ---- items ------------------------------------------------------------------------------------------------------------------------------
    pub fn owns(&self, item: &str) -> bool { self.owned.contains(item) }
    pub fn owned_items(&self) -> impl Iterator<Item = &String> { self.owned.iter() }
    pub fn owned_count(&self) -> usize { self.owned.len() }
    /// Grants an item; true when it was not owned before.
    pub fn grant(&mut self, item: &str) -> bool { let new = self.owned.insert(item.to_owned()); if new { self.dirty = true; } new }
    /// Removes an item (default items stay owned) and un-equips it; true when something was removed.
    pub fn revoke(&mut self, item: &str) -> bool {
        if self.defaults.iter().any(|(_, d)| d == item) { return false; }
        let had = self.owned.remove(item);
        if had {
            let slots: Vec<Slot> = self.loadout.iter().filter(|(_, i)| i.as_str() == item).map(|(s, _)| *s).collect();
            for s in slots { self.loadout.remove(&s); }
            self.apply_defaults();
            self.dirty = true;
        }
        had
    }
    pub fn is_default(&self, item: &str) -> bool { self.defaults.iter().any(|(_, d)| d == item) }

    /// Buys an offer: checks ownership and funds, deducts the price, grants the item. Nothing changes on error.
    pub fn purchase(&mut self, req: &PurchaseRequest) -> Result<Receipt, PurchaseError> {
        if req.price < 0 { return Err(PurchaseError::InvalidPrice); }
        if self.owns(&req.item_id) { return Err(PurchaseError::AlreadyOwned); }
        if self.vbucks < req.price { return Err(PurchaseError::InsufficientFunds { need: req.price, have: self.vbucks }); }
        self.vbucks -= req.price;
        self.owned.insert(req.item_id.clone());
        self.dirty = true;
        Ok(Receipt { offer_id: req.offer_id.clone(), item_id: req.item_id.clone(), price: req.price, balance_after: self.vbucks })
    }

    // ---- loadout ----------------------------------------------------------------------------------------------------------------------------
    pub fn equipped(&self, slot: Slot) -> Option<&str> { self.loadout.get(&slot).map(|s| s.as_str()) }
    /// Equips an owned item into a slot. (The caller checks that the item's category fits the slot.)
    pub fn equip(&mut self, slot: Slot, item: &str) -> Result<(), EquipError> {
        if !self.owns(item) { return Err(EquipError::NotOwned(item.to_owned())); }
        self.loadout.insert(slot, item.to_owned());
        self.dirty = true;
        Ok(())
    }
    pub fn banner(&self) -> Option<(&str, &str)> { self.banner.as_ref().map(|(i, c)| (i.as_str(), c.as_str())) }
    pub fn match_option(&self, id: &str) -> Option<usize> { self.match_options.get(id).copied() }
    pub fn set_match_option(&mut self, id: &str, v: usize) { if self.match_options.insert(id.to_owned(), v) != Some(v) { self.dirty = true; } }
    pub fn set_banner(&mut self, icon: &str, color: &str) { self.banner = Some((icon.to_owned(), color.to_owned())); self.dirty = true; }
    pub fn unequip(&mut self, slot: Slot) { if self.loadout.remove(&slot).is_some() { self.dirty = true; } }
    pub fn loadout(&self) -> impl Iterator<Item = (Slot, &str)> { self.loadout.iter().map(|(s, i)| (*s, i.as_str())) }

    /// Back to a fresh profile (1,000,000 V-Bucks, the default items plus the starting items = everything, shop offset 0); keeps the file binding.
    pub fn reset(&mut self) {
        self.vbucks = DEFAULT_VBUCKS;
        self.owned.clear();
        self.loadout.clear();
        self.shop_day_offset = 0;
        self.apply_defaults();
        for i in self.starting.clone() { self.owned.insert(i); }
        self.dirty = true;
    }

    /// Removes every owned item except the default items (outfits / harvesting tool / glider of a new account) and equips the defaults; V-Bucks and the shop
    /// rotation are untouched. This is the state in which every shop offer can be bought.
    pub fn clear_items(&mut self) {
        self.owned.clear();
        self.loadout.clear();
        self.apply_defaults();
        self.dirty = true;
    }
}

/// Counts of the starting items by category, in catalog order (for reports): (category name, count).
pub fn starting_items_report() -> Vec<(&'static str, usize)> {
    use fn_pose::cosmetics::Category;
    let Some(cat) = pak_catalog() else { return vec![] };
    [Category::Outfit, Category::Pickaxe, Category::Glider, Category::BattleBus, Category::LoadingScreen, Category::Emote].iter().map(|c| (c.name(), cat.count(*c, false))).collect()
}

fn pak_catalog() -> Option<&'static fn_pose::cosmetics::Catalog> {
    static CATALOG: std::sync::OnceLock<Option<fn_pose::cosmetics::Catalog>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        let install = fn_ui::pak_assets::default_install();
        match fn_pak::PakSet::open_install(&install) {
            Ok(set) => Some(fn_pose::cosmetics::Catalog::load(&set)),
            Err(e) => { eprintln!("[profile] cannot open the 1.8 install at {}: {e} (a fresh profile then owns only its default items)", install.display()); None }
        }
    }).as_ref()
}

/// EVERY Battle Royale OUTFIT definition (`AthenaCharacterItemDefinition`) of the 1.8 paks (`/Game/Athena/Items/Cosmetics/**`, development assets
/// under `00_TestAssets` excluded; harvesting tools and gliders are bought in the shop or granted from the console), by asset name. Read from the pak data once and cached; empty when the install cannot be opened. This is the default content of
/// a fresh profile.
pub fn starting_items_from_paks() -> Vec<String> {
    let Some(cat) = pak_catalog() else { return vec![] };
    cat.items.iter().filter(|i| !i.test_asset && i.category == fn_pose::cosmetics::Category::Outfit).map(|i| i.id.clone()).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn defs() -> Vec<(Slot, String)> { vec![(Slot::Outfit, "CID_005".into()), (Slot::Pickaxe, "DefaultPickaxe".into()), (Slot::Glider, "DefaultGlider".into())] }
    fn tmp(name: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("fn-profile-test-{}-{name}", std::process::id())); let _ = std::fs::remove_file(&d); d }

    #[test]
    fn fresh_profile_has_a_million_vbucks_and_the_defaults() {
        let p = Profile::new(defs());
        assert_eq!(p.balance(), 1_000_000);
        assert!(p.owns("CID_005") && p.owns("DefaultPickaxe") && p.owns("DefaultGlider"));
        assert_eq!(p.equipped(Slot::Outfit), Some("CID_005"));
        assert_eq!(p.equipped(Slot::Pickaxe), Some("DefaultPickaxe"));
    }

    #[test]
    fn purchase_deducts_grants_and_rejects_bad_requests() {
        let mut p = Profile::new(defs());
        let r = p.purchase(&PurchaseRequest { offer_id: "offer:A".into(), item_id: "A".into(), price: 1500 }).unwrap();
        assert_eq!((r.price, r.balance_after), (1500, 998_500));
        assert!(p.owns("A") && p.balance() == 998_500);
        // owned: refused, nothing charged
        assert_eq!(p.purchase(&PurchaseRequest { offer_id: "offer:A".into(), item_id: "A".into(), price: 1500 }), Err(PurchaseError::AlreadyOwned));
        assert_eq!(p.balance(), 998_500);
        // insufficient funds: nothing changes
        p.set_vbucks(100);
        let e = p.purchase(&PurchaseRequest { offer_id: "offer:B".into(), item_id: "B".into(), price: 800 });
        assert_eq!(e, Err(PurchaseError::InsufficientFunds { need: 800, have: 100 }));
        assert!(!p.owns("B") && p.balance() == 100);
        // exactly enough works and leaves 0
        let r = p.purchase(&PurchaseRequest { offer_id: "offer:B".into(), item_id: "B".into(), price: 100 }).unwrap();
        assert_eq!(r.balance_after, 0);
        assert!(p.owns("B"));
    }

    #[test]
    fn equip_requires_ownership() {
        let mut p = Profile::new(defs());
        assert_eq!(p.equip(Slot::Outfit, "X"), Err(EquipError::NotOwned("X".into())));
        p.grant("X");
        p.equip(Slot::Outfit, "X").unwrap();
        assert_eq!(p.equipped(Slot::Outfit), Some("X"));
        // revoking an equipped item falls back to the default
        assert!(p.revoke("X"));
        assert_eq!(p.equipped(Slot::Outfit), Some("CID_005"));
        assert!(!p.revoke("CID_005"), "default items cannot be removed");
    }

    #[test]
    fn wallet_edits_clamp() {
        let mut p = Profile::new(defs());
        assert_eq!(p.add_vbucks(500), 1_000_500);
        assert_eq!(p.add_vbucks(-5_000_000), 0);
        p.set_vbucks(i64::MAX);
        assert_eq!(p.balance(), MAX_VBUCKS);
        p.set_vbucks(-4);
        assert_eq!(p.balance(), 0);
    }

    #[test]
    fn persistence_roundtrip() {
        let path = tmp("roundtrip.json");
        {
            let mut p = Profile::load_or_new_with(&path, defs(), vec![]);
            assert_eq!(p.balance(), DEFAULT_VBUCKS, "missing file = fresh profile");
            p.purchase(&PurchaseRequest { offer_id: "offer:Glider_Voyager".into(), item_id: "Glider_Voyager".into(), price: 1200 }).unwrap();
            p.equip(Slot::Glider, "Glider_Voyager").unwrap();
            p.shop_day_offset = 3;
            assert!(p.is_dirty());
            p.flush().unwrap();
            assert!(!p.is_dirty());
        }
        let q = Profile::load_or_new_with(&path, defs(), vec![]);
        assert_eq!(q.balance(), 998_800);
        assert!(q.owns("Glider_Voyager"));
        assert_eq!(q.equipped(Slot::Glider), Some("Glider_Voyager"));
        assert_eq!(q.shop_day_offset, 3);
        assert!(!q.is_dirty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_and_bad_loadout_are_survivable() {
        let path = tmp("corrupt.json");
        std::fs::write(&path, "{ not json").unwrap();
        let p = Profile::load_or_new_with(&path, defs(), vec![]);
        assert_eq!(p.balance(), DEFAULT_VBUCKS);
        std::fs::write(&path, r#"{"vbucks": 42, "owned": ["Z"], "loadout": {"outfit": "NotOwned", "glider": "Z"}}"#).unwrap();
        let p = Profile::load_or_new_with(&path, defs(), vec![]);
        assert_eq!(p.balance(), 42);
        assert_eq!(p.equipped(Slot::Outfit), Some("CID_005"), "an unowned loadout entry falls back to the default");
        assert_eq!(p.equipped(Slot::Glider), Some("Z"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reset_restores_a_fresh_profile() {
        let mut p = Profile::new(defs());
        p.grant("Q"); p.set_vbucks(7); p.shop_day_offset = 9;
        p.reset();
        assert_eq!((p.balance(), p.shop_day_offset), (DEFAULT_VBUCKS, 0));
        assert!(!p.owns("Q") && p.owns("CID_005"));
    }

    #[test]
    fn slot_parsing() {
        assert_eq!(Slot::parse("Skin"), Some(Slot::Outfit));
        assert_eq!(Slot::parse("harvesting-tool"), Some(Slot::Pickaxe));
        assert_eq!(Slot::parse("GLIDER"), Some(Slot::Glider));
        assert_eq!(Slot::parse("banner"), None);
    }

    #[test]
    fn everything_is_the_default_content_and_reset_restores_it() {
        let start: Vec<String> = ["CID_030", "HalloweenScythe", "Glider_Voyager", "Bus_X", "Emote_Y"].iter().map(|s| s.to_string()).collect();
        let mut p = Profile::new(defs()).with_starting_items(start.clone());
        assert!(start.iter().all(|i| p.owns(i)) && p.owns("CID_005"), "defaults + starting items");
        p.set_vbucks(3);
        p.revoke("Bus_X");
        assert!(!p.owns("Bus_X"));
        p.reset();
        assert_eq!(p.balance(), DEFAULT_VBUCKS);
        assert!(start.iter().all(|i| p.owns(i)), "reset owns everything again");
        // clear_items: only the default items, V-Bucks untouched, defaults equipped
        p.equip(Slot::Outfit, "CID_030").unwrap();
        p.set_vbucks(77);
        p.clear_items();
        assert_eq!(p.owned_count(), 3, "just the three defaults");
        assert!(!p.owns("CID_030") && p.owns("CID_005") && p.owns("DefaultPickaxe"));
        assert_eq!(p.equipped(Slot::Outfit), Some("CID_005"));
        assert_eq!(p.balance(), 77);
        // and purchases work again
        let r = p.purchase(&PurchaseRequest { offer_id: "offer:CID_030".into(), item_id: "CID_030".into(), price: 70 }).unwrap();
        assert_eq!(r.balance_after, 7);
    }

    #[test]
    fn load_grants_the_starting_items_once_and_keeps_later_removals() {
        let start: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
        // an older profile file (no marker) gets them once
        let path = tmp("start.json");
        std::fs::write(&path, r#"{"vbucks": 5, "owned": ["Z"], "loadout": {}}"#).unwrap();
        let mut p = Profile::load_or_new_with(&path, defs(), start.clone());
        assert!(["A", "B", "C", "Z"].iter().all(|i| p.owns(i)) && p.balance() == 5);
        assert!(p.is_dirty(), "migrated: will be saved with the marker");
        // the user removes one and the next load keeps it removed
        assert!(p.revoke("B"));
        p.flush().unwrap();
        let q = Profile::load_or_new_with(&path, defs(), start.clone());
        assert!(q.owns("A") && q.owns("C") && q.owns("Z") && !q.owns("B"), "a removal made by this version persists");
        assert!(!q.is_dirty());
        // a missing file is a fresh profile that owns everything
        let fresh = Profile::load_or_new_with(&tmp("none.json"), defs(), start.clone());
        assert!(start.iter().all(|i| fresh.owns(i)) && fresh.balance() == DEFAULT_VBUCKS);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn starting_items_enumerate_the_pak_data_when_the_install_exists() {
        let install = fn_ui::pak_assets::default_install();
        if !install.exists() { eprintln!("1.8 install not found: skipped"); return; }
        let items = starting_items_from_paks();
        let rep = starting_items_report();
        eprintln!("starting items: {} {:?}", items.len(), rep);
        assert!(items.contains(&"CID_030_Athena_Commando_M_Halloween".to_string()) && !items.contains(&"HalloweenScythe".to_string()));
        assert_eq!(items.len(), rep[0].1);
        // 30 shipped outfits, no development assets (CID_Test_*), nothing from Save the World
        assert_eq!(rep[0], ("Outfit", 30));
        assert!(!items.iter().any(|i| i.contains("CID_Test")), "no development assets");
        let mut dedup = items.clone(); dedup.sort(); dedup.dedup();
        assert_eq!(dedup.len(), items.len(), "ids are unique");
    }}


