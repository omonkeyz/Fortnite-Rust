//! LOCKER glue of the live lobby: the cosmetic catalog (`fn_pose::cosmetics`, every Athena item definition of the 1.8 paks) <-> the Locker tab widgets
//! (`fn_ui::screens::athena_locker`) <-> the offline profile (`lobby_profile::Profile`: owned items + equipped loadout, the stand-in for the account profile).
//!
//! The Locker's picker lists the items the profile owns (the 1.8 picker lists the account's `FortAccountItem`s of the category); equipping writes the
//! profile; the lobby pawn is rebuilt from the equipped outfit (`lobby_scene::load_character_asset` + `LobbyScene::set_character`).
use crate::lobby_profile::{Profile, Slot as PSlot};
use fn_pose::cosmetics::{Catalog, Category, Cosmetic};
use fn_ui::screens::athena_locker::{LockerItem, LockerView, Slot as USlot};

pub fn to_ui(s: PSlot) -> USlot { match s { PSlot::Outfit => USlot::Outfit, PSlot::Pickaxe => USlot::Pickaxe, PSlot::Glider => USlot::Glider } }
pub fn to_profile(s: USlot) -> PSlot { match s { USlot::Outfit => PSlot::Outfit, USlot::Pickaxe => PSlot::Pickaxe, USlot::Glider => PSlot::Glider } }
/// The Locker slot an item definition is equipped into (outfits -> `CharacterSlot`, pickaxes -> `PickaxeSlot`, gliders -> `GliderSlot`).
pub fn to_profile_slot_of(c: &Cosmetic) -> PSlot { match c.category { Category::Pickaxe => PSlot::Pickaxe, Category::Glider => PSlot::Glider, _ => PSlot::Outfit } }
pub fn category(s: PSlot) -> Category { match s { PSlot::Outfit => Category::Outfit, PSlot::Pickaxe => Category::Pickaxe, PSlot::Glider => Category::Glider } }

pub const RANDOM_CHARACTERS: [&str; 8] = [
    "CID_001_Athena_Commando_F_Default", "CID_002_Athena_Commando_F_Default", "CID_003_Athena_Commando_F_Default", "CID_004_Athena_Commando_F_Default",
    "CID_005_Athena_Commando_M_Default", "CID_006_Athena_Commando_M_Default", "CID_007_Athena_Commando_M_Default", "CID_008_Athena_Commando_M_Default",
];

static SESSION_RANDOM_HERO: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub struct LockerGlue { pub catalog: Catalog, pub random_hero: String }

impl LockerGlue {
    pub fn load(set: &fn_pak::PakSet) -> LockerGlue {
        let catalog = Catalog::load(set);
        let random_hero = SESSION_RANDOM_HERO.get_or_init(|| {
            let recruits: Vec<&str> = RANDOM_CHARACTERS.iter().copied().filter(|id| catalog.find(id).is_some()).collect();
            let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| (d.as_nanos() / 1000) as usize ^ (d.as_secs() as usize));
            recruits.get(if recruits.is_empty() { 0 } else { seed % recruits.len() }).and_then(|id| catalog.find(id)).and_then(|c| c.hero.clone()).unwrap_or_else(|| fn_pose::br_character::DEFAULT_HERO.to_owned())
        }).clone();
        LockerGlue { catalog, random_hero }
    }

    /// The items a fresh offline profile owns: the Athena default outfits (`CID_*_Default`), `DefaultPickaxe`, `DefaultGlider` (the 1.8 account starts with the
    /// default items). The first outfit is `CID_005_Athena_Commando_M_Default` (the hero the lobby has always used).
    pub fn default_loadout(&self) -> Vec<(PSlot, String)> {
        let mut out = vec![];
        for (slot, id) in [(PSlot::Pickaxe, "DefaultPickaxe"), (PSlot::Glider, "DefaultGlider")] { if self.catalog.find(id).is_some() { out.push((slot, id.to_owned())); } }
        out
    }

    fn item(c: &Cosmetic) -> LockerItem {
        LockerItem {
            id: c.id.clone(), name: c.display_name().trim().to_owned(), rarity: c.rarity_index(),
            // the L card shows the large preview (small for sizes <= S, exe lambda 0x2f6b150), falling back to the other one
            icon: c.large_icon.clone().or_else(|| c.small_icon.clone()),
        }
    }

    /// The owned items of a slot in catalog order (the 1.8 picker order comes from the profile's item iteration and cannot be recovered: this is the asset-name
    /// order, i.e. the item number).
    pub fn owned(&self, profile: &Profile, slot: PSlot) -> Vec<&Cosmetic> {
        let mut v: Vec<&Cosmetic> = self.catalog.of(category(slot), false).into_iter().filter(|c| profile.owns(&c.id)).collect();
        v.sort_by_key(|c| c.rarity_index());
        v
    }

    /// Refresh the picker lists and the equipped markers of `view` from the profile.
    pub fn sync(&self, profile: &Profile, view: &mut LockerView) {
        for s in PSlot::ALL {
            let owned = self.owned(profile, s);
            let i = to_ui(s).index();
            view.items[i] = owned.iter().map(|c| Self::item(c)).collect();
            view.equipped[i] = profile.equipped(s).and_then(|e| owned.iter().position(|c| c.id == e));
            view.null_tile[i] = s == PSlot::Outfit;
        }
        if let Some((icon, color)) = profile.banner() { view.banner_icon = icon.to_owned(); view.banner_color = color.to_owned(); }
    }

    /// A picker tile was clicked: equip the item (the profile checks ownership). Returns the equipped cosmetic.
    pub fn equip(&self, profile: &mut Profile, slot: USlot, view: &LockerView, index: usize) -> Option<&Cosmetic> {
        if index == usize::MAX { profile.unequip(to_profile(slot)); return None; }
        let item = view.items[slot.index()].get(index)?;
        let ps = to_profile(slot);
        profile.equip(ps, &item.id).ok()?;
        self.catalog.find(&item.id)
    }

    /// `HID_*` path of the outfit equipped in `profile` (falls back to the default hero).
    pub fn equipped_hero(&self, profile: &Profile) -> String {
        fn_live::var("FN_LOBBY_OUTFIT").ok().and_then(|id| self.catalog.find(&id)).and_then(|c| c.hero.clone())
            .or_else(|| profile.equipped(PSlot::Outfit).and_then(|id| self.catalog.find(id)).and_then(|c| c.hero.clone())).unwrap_or_else(|| self.random_hero.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn glue() -> Option<LockerGlue> {
        let root = fn_live::var("FN_ROOT").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        let set = fn_pak::PakSet::open_install(root).ok()?;
        Some(LockerGlue::load(&set))
    }
    #[test]
    fn the_fresh_profile_owns_only_the_defaults_and_equipping_swaps_the_loadout() {
        let Some(g) = glue() else { return };
        let defs = g.default_loadout();
        assert!(defs.iter().all(|d| d.0 != PSlot::Outfit));
        let mut p = Profile::new(defs).with_starting_items(RANDOM_CHARACTERS.iter().map(|s| s.to_string()).collect());
        let mut view = LockerView::default();
        g.sync(&p, &mut view);
        let outfits = &view.items[USlot::Outfit.index()];
        assert_eq!(outfits.len(), 8, "{:?}", outfits.iter().map(|i| &i.id).collect::<Vec<_>>());
        assert_eq!(view.items[USlot::Pickaxe.index()].len(), 1);
        assert_eq!(view.items[USlot::Glider.index()].len(), 1);
        assert_eq!(view.equipped[0], None);
        assert!(view.null_tile[0]);
        assert!(RANDOM_CHARACTERS.iter().any(|id| g.catalog.find(id).and_then(|c| c.hero.as_deref()) == Some(g.equipped_hero(&p).as_str())));
        // buy Skull Trooper (what the shop does) and equip it from the picker
        p.grant("CID_030_Athena_Commando_M_Halloween");
        g.sync(&p, &mut view);
        let idx = view.items[0].iter().position(|i| i.id == "CID_030_Athena_Commando_M_Halloween").expect("owned item is listed");
        assert_eq!(view.items[0][idx].name, "Skull Trooper");
        assert_eq!(view.items[0][idx].rarity, 3, "Quality = Epic row of RarityData");
        let c = g.equip(&mut p, USlot::Outfit, &view, idx).unwrap();
        assert_eq!(c.hero.as_deref().map(|h| h.rsplit('/').next().unwrap()), Some("HID_030_Athena_Commando_M_Halloween.HID_030_Athena_Commando_M_Halloween"));
        assert_eq!(p.equipped(PSlot::Outfit), Some("CID_030_Athena_Commando_M_Halloween"));
        assert!(g.equipped_hero(&p).contains("HID_030"));
        g.sync(&p, &mut view);
        assert_eq!(view.equipped[0], Some(idx));
        // an item that is not owned cannot be listed (and the profile refuses to equip it)
        assert!(p.equip(PSlot::Outfit, "CID_029_Athena_Commando_F_Halloween").is_err());
    }
}
