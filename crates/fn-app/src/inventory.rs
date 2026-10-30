//! WIDGETS3 / INVENTORY HOOK: the live inventory of the app as the inventory screen's view (`fn_ui::screens::inventory_panel`) and the host side of the panel's actions.
//!
//! The inventory is `Arms` (`arms/inventory_ops.rs`): the six primary slots (the pickaxe and five weapon slots, each holding one stack), the stash (owned items that are not on the bar = the
//! backpack side of the screen), the ammo stacks (one per ammo type) and, in the sim, the wood / stone / metal counters. The rules (slot acceptance, auto-slotting, stack merge, backpack
//! size, swap, drop) are `fn_weapons::inventory_model` and are each marked there as data / exe / inference. This file shows that state ([`InventoryUi::view_live`]), derives the HUD quickbar
//! cards from it ([`InventoryUi::sync_cards`]) and turns the panel's actions into operations on it ([`InventoryUi::execute`]).
//! Item ids: see [`item_id`] (0 pickaxe, firearms `g + 1` / `g + 5`, 5..=7 resources, healable stacks `100 + ...`, ammo `200 + ...`).
//! The legacy HUD-state view ([`InventoryUi::view`]) is the fallback while no `Arms` is loaded.
//!
//! The panel's decisions are recovered from the original bytecode (`fn_weapons::recovered_panel`, `recovered_inventory`); what leaves the inventory (drops, the item a full-bar pickup
//! replaced) is queued in `Arms::drops` for the world-item system (`pickups_hook::queue_drop`, see [`drain_drops`]).
use fn_ui::screens::hud::{HudItem, HudState};
use fn_pose::loadout_inventory::CatalogItem;
use fn_ui::screens::inventory_panel::{InvItem, InventoryPanel, InventoryView, PanelAction};
use fn_weapons::{recovered_inventory, recovered_panel};
use crate::arms::{Arms, QuickbarItem};
use crate::arms::inventory_ops::{Dropped, Item, Loc};

const CATALOG_OF_ID: [usize; 8] = [4, 0, 1, 2, 3, 5, 6, 7];
/// `EFortItemType` of the build resources when no catalog is loaded (WorldResource).
const RESOURCE_TYPE: u8 = 19;

pub struct InventoryUi { pub panel: InventoryPanel, catalog: Vec<CatalogItem>, quickbar: [Option<u32>; 6], pub log: Vec<String>, heal_cat: Vec<Option<CatalogItem>>, ammo_cat: Vec<(String, CatalogItem)>, images: Vec<(String, u32, u32, Vec<u8>)>, extras_loaded: bool }

impl Default for InventoryUi {
    fn default() -> Self { Self { panel: InventoryPanel::default(), catalog: vec![], quickbar: [Some(0), Some(1), Some(2), Some(3), Some(4), None], log: vec![], heal_cat: vec![], ammo_cat: vec![], images: vec![], extras_loaded: false } }
}

/// What applying the panel's actions asks of the app.
#[derive(Default, Debug, PartialEq)]
pub struct Applied { pub close: bool, pub lines: Vec<String>, pub quickbar_moves: Vec<(u32, usize)>, /// INVENTORY HOOK: the operations to run on the live inventory (`InventoryUi::execute`).
    pub ops: Vec<Op> }

impl InventoryUi {
    pub fn set_catalog(&mut self, c: Vec<CatalogItem>) { self.catalog = c; }
    /// Appends the texts of a firearm installed after start-up: catalog index 8 + k = item id 8 + k = gun index k + 3 (`QuickbarItem::inventory_id`). Call in gun-index order.
    pub fn add_catalog_item(&mut self, c: CatalogItem) { self.catalog.push(c); }
    pub fn sync_quickbar(&mut self, quickbar: &[Option<QuickbarItem>; 6]) {
        self.quickbar = std::array::from_fn(|slot| quickbar[slot].map(QuickbarItem::inventory_id));
    }
    pub fn has_catalog(&self) -> bool { self.catalog.len() >= 8 }

    /// The inventory of this frame from the HUD state.
    pub fn view(&self, st: &HudState) -> InventoryView {
        let cat = |id: usize| self.catalog.get(CATALOG_OF_ID.get(id).copied().unwrap_or(id));
        let mut items = vec![];
        for slot in 0..6usize {
            let Some(id) = self.quickbar[slot] else { continue };
            let Some(h) = st.items[slot].as_ref() else { continue };
            let c = cat(id as usize);
            items.push(InvItem {
                id, name: c.map(|c| c.name.trim().to_owned()).unwrap_or_default(), description: c.map(|c| c.description.clone()).unwrap_or_default(),
                rarity: h.rarity, icon: h.icon.clone(), count: h.count.unwrap_or(1), item_type: c.map_or(17, |c| c.item_type),
                equipable: true, is_resource: false, can_be_dropped: true, slot: (slot >= 1).then_some(slot as u8), def_path: c.map(|c| c.path.clone()).unwrap_or_default(), // INVENTORY FIX2: definition for GetUserFriendlyTags
            });
        }
        for (k, n) in st.resources.iter().enumerate() {
            if *n == 0 { continue; }
            let id = 5 + k; let c = cat(id);
            items.push(InvItem {
                id: id as u32, name: c.map(|c| c.name.clone()).unwrap_or_default(), description: c.map(|c| c.description.clone()).unwrap_or_default(), rarity: c.map_or(0, |c| c.rarity),
                icon: c.map(|c| c.icon.clone()).unwrap_or_default(), count: *n, item_type: c.map_or(RESOURCE_TYPE, |c| c.item_type), equipable: false, is_resource: true, can_be_dropped: true, slot: None, def_path: c.map(|c| c.path.clone()).unwrap_or_default(),
            });
        }
        InventoryView { items, backpack: st.backpack }
    }

    /// Resolve the panel's pending actions. Returns whether the screen asked to close plus the log lines (also kept in `self.log`).
    pub fn apply(&mut self) -> Applied {
        let mut a = Applied::default();
        for act in self.panel.take_actions() {
            let find = |id: u32| self.panel.view().items.iter().find(|i| i.id == id).cloned();
            match act {
                PanelAction::Select(id) => a.lines.push(format!("[inventory] select item {id} (AthenaHUDContext.SetSelectedInventoryItem, ItemDetails.SetItemToDetail)")),
                PanelAction::Close => { a.close = true; a.lines.push("[inventory] HandleClose (FortHUDContext.EnterCameraMode)".into()); }
                PanelAction::Equip(id) => {
                    let Some(it) = find(id) else { continue };
                    let d = recovered_panel::perform_equip(it.item_type, it.slot.is_some() || id == 0, false);
                    match d {
                        recovered_panel::EquipAction::CloseThenEquip => { a.close = true; a.ops.push(Op::Equip(id)); a.lines.push(format!("[inventory] Equip item {id}: HandleClose, then FortInventoryContext.EquipItem")); }
                        recovered_panel::EquipAction::RemoveFromQuickbar => a.ops.push(Op::Equip(id)),
                        recovered_panel::EquipAction::ProcessSlotAction => a.lines.push(format!("[inventory] Equip item {id}: ProcessSlotAction (gamepad path, not applied)")),
                        recovered_panel::EquipAction::Equip => a.ops.push(Op::Equip(id)),
                    }
                }
                PanelAction::DropOnSlot { item, slot } => a.ops.push(Op::Move { item, slot: slot as usize }),
                PanelAction::Drop { item, split } => {
                    let Some(it) = find(item) else { continue };
                    let q = recovered_inventory::drag_drop_quantity(true, true, it.can_be_dropped, it.count as i32, split, false);
                    match q { Some(q) => a.ops.push(Op::Drop { item, quantity: q.max(0) as u32 }), None => a.lines.push(format!("[inventory] drop of item {item} refused (CanBeDropped false)")) }
                }
                // INVENTORY FIX2: X on a single item, or the QuantitySelector_C confirm (CurrentValue) / cancel (0: `DropItem(item, 0)` drops nothing)
                PanelAction::DropQuantity { item, quantity: 0 } => a.lines.push(format!("[inventory] DropItem(item {item}, 0): quantity selector cancelled, nothing dropped")),
                PanelAction::DropQuantity { item, quantity } => a.ops.push(Op::Drop { item, quantity }),
            }
        }
        self.log.extend(a.lines.iter().cloned());
        a
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// INVENTORY HOOK: the live inventory (`Arms` stacks + ammo + resources) as the panel's view, the HUD cards, and the panel's actions applied to it
// ---------------------------------------------------------------------------------------------------------------------------------------------

/// What an inventory item id refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target { Stack(Loc), Ammo(usize), Resource(usize), LooseAmmo(usize) }

/// Item ids of the panel. 0 pickaxe, firearm g: `g + 1` for the three starters, `g + 5` after (the legacy ids, which are also the catalog indices of `set_catalog` / `add_catalog_item`),
/// 5..=7 wood / stone / metal, healable stack: `100 + idx * 10 + ordinal` (ordinal among the stacks of that healable, bar first), ammo of the type first used by firearm g: `200 + g`.
pub fn item_id(item: Item, heal_ordinal: usize) -> u32 {
    match item { Item::Pickaxe => 0, Item::Gun(g) => if g < 3 { g as u32 + 1 } else { g as u32 + 5 }, Item::Heal { idx, .. } => 100 + idx as u32 * 10 + heal_ordinal.min(9) as u32 }
}
pub const AMMO_ID_BASE: u32 = 200;
pub const LOOSE_AMMO_ID_BASE: u32 = 900;
pub const BR_AMMO_ITEMS: [&str; 5] = ["/Game/Athena/Items/Ammo/AthenaAmmoDataBulletsLight.AthenaAmmoDataBulletsLight", "/Game/Athena/Items/Ammo/AthenaAmmoDataBulletsMedium.AthenaAmmoDataBulletsMedium", "/Game/Athena/Items/Ammo/AthenaAmmoDataBulletsHeavy.AthenaAmmoDataBulletsHeavy", "/Game/Athena/Items/Ammo/AthenaAmmoDataShells.AthenaAmmoDataShells", "/Game/Athena/Items/Ammo/AmmoDataRockets.AmmoDataRockets"];
/// Textures the inventory panel draws by name (object paths from the widget data: `AthenaInventoryFortItemTileButton`, `AthenaInventoryDropSlot`, `AthenaInventoryEquipSlot`).
pub const INVENTORY_ART: [&str; 7] = [
    "/Game/Athena/HUD/Art/quickbar_s1.quickbar_s1", "/Game/Athena/HUD/Art/quickbar_s2.quickbar_s2", "/Game/Athena/HUD/Art/quickbar_s3.quickbar_s3",
    "/Game/Athena/HUD/Art/quickbar_s4.quickbar_s4", "/Game/Athena/HUD/Art/quickbar_s5.quickbar_s5",
    "/Game/Athena/HUD/Inventory/T_AthenaDropItem.T_AthenaDropItem", "/Game/UI/Quickbar/Icon-QuickbarSlot-Highlight.Icon-QuickbarSlot-Highlight",
];
pub const RESOURCE_ID_BASE: u32 = 5;

/// Every inventory entry the panel can show: (id, what it is, count), in display order: the bar and stash stacks, the ammo stacks, the resources (`res` = wood, stone, metal).
pub fn enumerate(arms: &Arms, res: [u32; 3]) -> Vec<(u32, Target, u32)> {
    let mut out = vec![];
    let mut ord = vec![0usize; arms.stock.len()];
    for (loc, item) in arms.all_stacks() {
        let o = if let Item::Heal { idx, .. } = item { let o = ord[idx]; ord[idx] += 1; o } else { 0 };
        out.push((item_id(item, o), Target::Stack(loc), item.count()));
    }
    let mut seen: Vec<&str> = vec![];
    for (g, gun) in arms.data.guns.iter().enumerate() {
        if seen.contains(&gun.def.ammo.as_str()) { continue; }
        seen.push(&gun.def.ammo);
        if arms.reserve[g] > 0 { out.push((AMMO_ID_BASE + g as u32, Target::Ammo(g), arms.reserve[g])); }
    }
    for (i, (_, n)) in arms.loose_ammo.iter().enumerate() { if *n > 0 { out.push((LOOSE_AMMO_ID_BASE + i as u32, Target::LooseAmmo(i), *n)); } }
    for (k, n) in res.iter().enumerate() { if *n > 0 { out.push((RESOURCE_ID_BASE + k as u32, Target::Resource(k), *n)); } }
    out
}

/// Texts / art of one inventory entry.
#[derive(Clone, Debug, Default)]
pub struct Info { pub name: String, pub description: String, pub rarity: u8, pub icon: String, pub item_type: u8, /// INVENTORY FIX2: item definition object path (GetUserFriendlyTags reads its cooked GameplayTags)
    pub path: String }
fn info_of(c: &CatalogItem) -> Info { Info { name: c.name.trim().to_owned(), description: c.description.clone(), rarity: c.rarity, icon: c.icon.clone(), item_type: c.item_type, path: c.path.clone() } }

/// One panel operation on the live inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op { Equip(u32), Move { item: u32, slot: usize }, Drop { item: u32, quantity: u32 } }

impl InventoryUi {
    /// Catalog entries of the healing consumables (HEALABLE_ITEMS order) and of the ammo types (keyed by the ammo item path of the firearms using it).
    pub fn set_extras(&mut self, heal: Vec<Option<CatalogItem>>, ammo: Vec<(String, CatalogItem)>) { self.heal_cat = heal; self.ammo_cat = ammo; self.extras_loaded = true; }
    pub fn extras_loaded(&self) -> bool { self.extras_loaded }
    /// HUD textures decoded for the extras (PNG name, w, h, RGBA8), drained by the app into its overlay image store.
    pub fn take_images(&mut self) -> Vec<(String, u32, u32, Vec<u8>)> { std::mem::take(&mut self.images) }
    pub fn push_images(&mut self, v: Vec<(String, u32, u32, Vec<u8>)>) { self.images.extend(v); }
    /// Reads the item definitions the starter catalog lacks from the install: every healing consumable and every ammo type the firearms use (names, descriptions, rarities, icons).
    /// The decoded icons are queued for `take_images`. Returns the notes of what could not be read.
    pub fn load_extras(&mut self, install: &std::path::Path, arms: &Arms) -> Vec<String> {
        let mut notes = vec![];
        let set = match crate::weapon_runtime::paks(install) { Ok(s) => s, Err(e) => return vec![format!("[inventory] item definitions unavailable: {e}")] };
        let src = fn_pose::ClipSourceDir::Pak(&set);
        let mut images = vec![];
        let mut icon = |c: &CatalogItem, notes: &mut Vec<String>| if !c.icon_path.is_empty() {
            match fn_pose::loadout_stats::decode_ui_texture(&src, &c.icon_path) { Ok(i) => images.push(i), Err(e) => notes.push(format!("[inventory] {}: icon not decoded ({e})", c.name)) }
        };
        let mut heal = vec![];
        for p in fn_pose::loadout_stats::HEALABLE_ITEMS.iter().take(arms.data.healables.len()) {
            match fn_pose::loadout_quickbar::read_catalog_item(&src, p) { Ok(c) => { icon(&c, &mut notes); heal.push(Some(c)); } Err(e) => { notes.push(format!("[inventory] {p}: {e}")); heal.push(None); } }
        }
        let mut ammo: Vec<(String, CatalogItem)> = vec![];
        for path in arms.data.guns.iter().map(|g| g.def.ammo.as_str()).chain(BR_AMMO_ITEMS) {
            if ammo.iter().any(|(a, _)| crate::arms::inventory_ops::ammo_key(a) == crate::arms::inventory_ops::ammo_key(path)) { continue; }
            match fn_pose::loadout_quickbar::read_catalog_item(&src, path) { Ok(c) => { icon(&c, &mut notes); ammo.push((path.to_owned(), c)); } Err(e) => notes.push(format!("[inventory] {path}: {e}")) }
        }
        // the panel's own art that the exported folder lacks: `EquipSlotOverlay` brushes of `AthenaInventoryFortItemTileButton` (`SetEquipSlotOverlay`: quickbar_s1..5), the drop
        // slot icon and the equip-slot drop highlight (straight from the paks; same decoder as the item icons)
        for p in INVENTORY_ART { match fn_pose::loadout_stats::decode_ui_texture(&src, p) { Ok(i) => images.push(i), Err(e) => notes.push(format!("[inventory] {p}: {e}")) } }
        self.images.extend(images);
        self.set_extras(heal, ammo);
        notes
    }

    fn catalog_of_gun(&self, g: usize) -> Option<&CatalogItem> { self.catalog.get(if g < 3 { g } else { g + 5 }) }
    /// Texts and art of a stack.
    pub fn info(&self, item: Item) -> Option<Info> {
        match item {
            Item::Pickaxe => self.catalog.get(4).map(info_of),
            Item::Gun(g) => self.catalog_of_gun(g).map(info_of),
            Item::Heal { idx, .. } => self.heal_cat.get(idx).and_then(|c| c.as_ref()).or_else(|| if idx == 0 { self.catalog.get(3) } else { None }).map(info_of),
        }
    }
    fn ammo_info(&self, arms: &Arms, g: usize) -> Option<Info> { self.ammo_info_of(&arms.data.guns.get(g)?.def.ammo) }
    fn ammo_info_of(&self, path: &str) -> Option<Info> {
        let p = crate::arms::inventory_ops::ammo_key(path);
        self.ammo_cat.iter().find(|(a, _)| crate::arms::inventory_ops::ammo_key(a) == p).map(|(_, c)| info_of(c))
    }

    /// The HUD quickbar cards of the live bar: rarity / icon from each item definition, the stack count of a consumable. A slot whose item has no texts yet keeps its current card.
    pub fn sync_cards(&self, arms: &Arms, items: &mut [Option<HudItem>; 6]) {
        for slot in 0..6 {
            match arms.slot_item(slot) {
                None => items[slot] = None,
                Some(Item::Pickaxe) => items[slot] = Some(HudItem::pickaxe()),
                Some(it) => {
                    let count = arms.slot_counts()[slot]; // QUICKBAR AMMO: the 1.8 card number rules (`Arms::slot_counts`: firearm ammo, a stackable item's count even at 1)
                    match self.info(it) {
                        Some(i) => items[slot] = Some(HudItem::new(i.rarity, &i.icon, count)),
                        None => if let Some(c) = items[slot].as_mut() { c.count = count; },
                    }
                }
            }
        }
    }

    /// The live inventory as the panel shows it: every stack (bar and stash), the ammo stacks and the resources, with `Backpack n / 5`.
    pub fn view_live(&self, arms: &Arms, res: [u32; 3]) -> InventoryView {
        let mut items = vec![];
        for (id, target, count) in enumerate(arms, res) {
            match target {
                Target::Stack(loc) => {
                    let Some(item) = arms.item_at(loc) else { continue };
                    let i = self.info(item).unwrap_or_default();
                    let rules = arms.rules_of_item(item);
                    let slot = match loc { Loc::Slot(s) if s >= 1 => Some(s as u8), _ => None };
                    items.push(InvItem { id, name: i.name, description: i.description, rarity: i.rarity, icon: i.icon, count, item_type: if i.item_type == 0 { Arms::item_type_of(item) } else { i.item_type },
                        equipable: true, is_resource: false, can_be_dropped: rules.can_be_dropped, slot, def_path: i.path });
                }
                Target::Ammo(g) => {
                    let i = self.ammo_info(arms, g).unwrap_or_default();
                    items.push(InvItem { id, name: i.name, description: i.description, rarity: i.rarity, icon: i.icon, count, item_type: if i.item_type == 0 { 1 } else { i.item_type }, equipable: false, is_resource: false, can_be_dropped: true, slot: None, def_path: i.path });
                }
                Target::LooseAmmo(i) => {
                    let i = arms.loose_ammo.get(i).and_then(|(p, _)| self.ammo_info_of(p)).unwrap_or_default();
                    items.push(InvItem { id, name: i.name, description: i.description, rarity: i.rarity, icon: i.icon, count, item_type: if i.item_type == 0 { 1 } else { i.item_type }, equipable: false, is_resource: false, can_be_dropped: true, slot: None, def_path: i.path });
                }
                Target::Resource(k) => {
                    let c = self.catalog.get(5 + k);
                    items.push(InvItem { id, name: c.map(|c| c.name.trim().to_owned()).unwrap_or_default(), description: c.map(|c| c.description.clone()).unwrap_or_default(), rarity: c.map_or(0, |c| c.rarity),
                        icon: c.map(|c| c.icon.clone()).unwrap_or_default(), count, item_type: c.map_or(RESOURCE_TYPE, |c| c.item_type), equipable: false, is_resource: true, can_be_dropped: true, slot: None, def_path: c.map(|c| c.path.clone()).unwrap_or_default() });
                }
            }
        }
        InventoryView { items, backpack: arms.backpack_counts() }
    }

    /// Runs the panel's pending operations on the live inventory. `res` = wood, stone, metal (dropped resources leave it). Returns the log lines.
    pub fn execute(&mut self, a: &Applied, arms: &mut Arms, res: &mut [u32; 3]) -> Vec<String> {
        let mut log = vec![];
        for op in &a.ops {
            let ents = enumerate(arms, *res);
            let find = |id: u32| ents.iter().find(|(i, ..)| *i == id).map(|(_, t, c)| (*t, *c));
            match *op {
                Op::Equip(id) => match find(id) {
                    Some((Target::Stack(Loc::Slot(s)), _)) => match arms.unequip_slot(s) { Ok(()) => log.push(format!("[inventory] Unequip item {id}: RemoveItemFromQuickBar, the item stays in the backpack")), Err(e) => log.push(format!("[inventory] Unequip item {id} refused: {e}")) },
                    Some((Target::Stack(Loc::Stash(i)), _)) => match arms.equip_from_stash(i) { Ok(s) => log.push(format!("[inventory] Equip item {id}: EquipItem -> quickbar slot {}", s + 1)), Err(e) => log.push(format!("[inventory] Equip item {id} refused: {e}")) },
                    _ => log.push(format!("[inventory] Equip item {id}: nothing to equip")),
                },
                Op::Move { item, slot } => match find(item) {
                    Some((Target::Stack(loc), _)) => match arms.put_on_slot(loc, slot) { Ok(()) => log.push(format!("[inventory] DropItemOnQuickBar(item {item}, bar 0, slot {slot}) applied")), Err(e) => log.push(format!("[inventory] DropItemOnQuickBar(item {item}, slot {slot}) refused: {e}")) },
                    _ => log.push(format!("[inventory] move of item {item}: not a slottable item")),
                },
                Op::Drop { item, quantity } => match find(item) {
                    Some((Target::Stack(loc), _)) => match arms.drop_stack(loc, quantity) {
                        Ok(d) => log.push(format!("[inventory] DropItem(item {item}, {quantity}): dropped {d:?}")),
                        Err(e) => log.push(format!("[inventory] drop of item {item} refused: {e}")),
                    },
                    Some((Target::Ammo(g), _)) => match arms.drop_ammo(g, quantity) { Ok(d) => log.push(format!("[inventory] dropped {d} ammo")), Err(e) => log.push(format!("[inventory] ammo drop refused: {e}")) },
                    Some((Target::LooseAmmo(i), _)) => match arms.drop_loose_ammo(i, quantity) { Ok(d) => log.push(format!("[inventory] dropped {d} ammo")), Err(e) => log.push(format!("[inventory] ammo drop refused: {e}")) },
                    Some((Target::Resource(k), c)) => { let d = quantity.min(c).min(res[k]); res[k] -= d; if d > 0 { arms.drops.push(Dropped::Resource { kind: k, count: d }); } log.push(format!("[inventory] dropped {d} of resource {k}")); }                    None => log.push(format!("[inventory] drop of item {item}: no such item")),
                },
            }
        }
        self.log.extend(log.iter().cloned());
        log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arms::Arms;
    use fn_ui::screens::hud::HudItem;
    fn cat(n: &str, ty: u8) -> CatalogItem { CatalogItem { path: format!("/{n}"), name: n.into(), description: format!("{n} description"), rarity: 0, item_type: ty, max_stack: None, icon: format!("{n}.png"), icon_path: String::new() } }
    fn ui() -> InventoryUi { let mut u = InventoryUi::default(); u.set_catalog(vec![cat("AR", 17), cat("Shotgun", 17), cat("Sniper", 17), cat("Bandage", 17), cat("Pickaxe", 18), cat("Wood", 19), cat("Stone", 19), cat("Metal", 19)]); u }
    fn state() -> HudState { let mut st = HudState::default(); st.items[1] = Some(HudItem::new(0, "ar.png", None)); st.items[4] = Some(HudItem::new(0, "bandage.png", Some(5))); st.resources = [120, 0, 7]; st.backpack = (4, 5); st }

    #[test] fn the_view_is_the_live_quickbar_plus_resources() {
        let u = ui(); let v = u.view(&state());
        let ids: Vec<u32> = v.items.iter().map(|i| i.id).collect(); assert_eq!(ids, [0, 1, 4, 5, 7], "pickaxe, the filled slots, wood and metal (stone is 0)");
        assert_eq!(v.backpack, (4, 5));
        let bandage = v.items.iter().find(|i| i.id == 4).unwrap(); assert_eq!((bandage.count, bandage.slot, bandage.name.as_str()), (5, Some(4), "Bandage"));
        let pick = &v.items[0]; assert_eq!((pick.slot, pick.item_type, pick.name.as_str()), (None, 18, "Pickaxe"), "the pickaxe is not an equip slot");
        let wood = v.items.iter().find(|i| i.id == 5).unwrap(); assert!(wood.is_resource && !wood.equipable && wood.count == 120 && wood.description == "Wood description");
        assert_eq!(v.items.iter().find(|i| i.id == 1).unwrap().slot, Some(1));
    }
    #[test] fn without_a_catalog_the_view_still_shows_the_state() { let v = InventoryUi::default().view(&state()); assert_eq!(v.items.len(), 5); assert!(v.items.iter().all(|i| i.name.is_empty())); }
    #[test] fn actions_resolve_to_operations_on_the_live_inventory() {
        let mut u = ui(); let v = u.view(&state()); u.panel.set_view(v); u.panel.set_open(true);
        u.panel.select(1); u.panel.key_equip(); let a = u.apply();
        assert!(!a.close && a.ops == vec![Op::Equip(1)], "a slotted item's Equip action is RemoveItemFromQuickBar (the host decides slotted vs not): {:?}", a.ops);
        u.panel.select(5); u.panel.key_equip(); assert!(u.apply().ops.is_empty(), "resources have no Equip action");
        u.panel.key_close(); assert!(u.apply().close);
    }
    fn live() -> (InventoryUi, Arms) {
        let mut u = ui(); u.set_extras(vec![Some(cat("Bandage", 17)), Some(cat("Medkit", 17)), None, None], vec![]);
        (u, Arms::new(crate::arms::tests::data(), crate::arms::Policy::default()))
    }
    #[test] fn the_live_view_lists_every_stack_with_the_backpack_counter() {
        let (u, mut a) = live();
        let v = u.view_live(&a, [120, 0, 7]);
        let ids: Vec<u32> = v.items.iter().map(|i| i.id).collect(); assert_eq!(ids, [0, 1, 2, 3, 100, 5, 7], "pickaxe, three firearms, the bandage stack, wood and metal");
        assert_eq!(v.backpack, (4, 5)); let b = v.items.iter().find(|i| i.id == 100).unwrap(); assert_eq!((b.count, b.slot, b.name.as_str()), (5, Some(4), "Bandage"));
        // a second stack of the same consumable and a medkit get their own ids; an unslotted firearm shows without an equip slot
        a.pickup_heal(0, 12, true).unwrap(); a.unequip_slot(2).unwrap();
        let v = u.view_live(&a, [0; 3]);
        let ids: Vec<u32> = v.items.iter().map(|i| i.id).collect(); assert_eq!(ids, [0, 1, 3, 100, 101, 2], "bar order, then the stash");
        assert_eq!(v.items.last().unwrap().slot, None); assert_eq!(v.backpack, (5, 5));
    }
    #[test] fn the_hud_cards_follow_the_live_bar() {
        let (u, mut a) = live();
        let mut items: [Option<HudItem>; 6] = Default::default();
        u.sync_cards(&a, &mut items);
        assert_eq!(items.iter().map(|i| i.is_some()).collect::<Vec<_>>(), [true, true, true, true, true, false]);
        assert_eq!(items[4].as_ref().unwrap().count, Some(5), "the consumable card shows its stack"); assert_eq!(items[1].as_ref().unwrap().icon, "AR.png");
        a.drop_stack(Loc::Slot(1), 1).unwrap(); u.sync_cards(&a, &mut items); assert!(items[1].is_none(), "a dropped item leaves the bar");
    }
    #[test] fn executing_the_panel_operations_changes_the_inventory_and_resources() {
        let (mut u, mut a) = live(); let mut res = [100, 0, 50];
        let run = |u: &mut InventoryUi, a: &mut Arms, res: &mut [u32; 3], ops: Vec<Op>| u.execute(&Applied { ops, ..Default::default() }, a, res);
        run(&mut u, &mut a, &mut res, vec![Op::Equip(2)]); assert_eq!((a.quickbar[2], a.stash.len()), (None, 1), "Unequip: the shotgun stays in the backpack");
        run(&mut u, &mut a, &mut res, vec![Op::Equip(2)]); assert_eq!((a.quickbar[2], a.stash.len()), (Some(QuickbarItem::Gun(1)), 0), "Equip puts it back");
        let l = run(&mut u, &mut a, &mut res, vec![Op::Move { item: 100, slot: 1 }]); assert_eq!((a.quickbar[1], a.slot_stack[1]), (Some(QuickbarItem::Healable), 5), "{l:?}");
        run(&mut u, &mut a, &mut res, vec![Op::Move { item: 100, slot: 0 }]); assert_eq!(a.quickbar[0], Some(QuickbarItem::Pickaxe), "slot 0 refuses a consumable");
        run(&mut u, &mut a, &mut res, vec![Op::Drop { item: 5, quantity: 30 }, Op::Drop { item: 7, quantity: 999 }]); assert_eq!(res, [70, 0, 0], "drops clamp to the stack");
        run(&mut u, &mut a, &mut res, vec![Op::Drop { item: 0, quantity: 1 }]); assert_eq!(a.quickbar[0], Some(QuickbarItem::Pickaxe), "the pickaxe cannot be dropped");
        run(&mut u, &mut a, &mut res, vec![Op::Drop { item: 100, quantity: 2 }]); assert_eq!((a.slot_stack[1], a.stock[0]), (3, 3));
    }
}

/// The pickup-item name and count of something the player lost to the world (`pickups_hook::queue_drop`); None for what cannot be dropped.
pub fn drop_request(arms: &Arms, d: &Dropped) -> Option<(String, u32)> {
    use fn_pose::loadout_inventory::EXTRA_ITEMS;
    match d {
        Dropped::Stack(Item::Pickaxe) => None,
        Dropped::Stack(Item::Gun(g)) => arms.data.guns.get(*g).map(|x| (x.def.item.clone(), 1)),
        Dropped::Stack(Item::Heal { idx, count }) => fn_pose::loadout_stats::HEALABLE_ITEMS.get(*idx).map(|p| ((*p).to_owned(), *count)),
        Dropped::Ammo { gun, count } => arms.data.guns.get(*gun).map(|x| (x.def.ammo.clone(), *count)),
        Dropped::Resource { kind, count } => EXTRA_ITEMS.get(1 + kind).map(|p| ((*p).to_owned(), *count)),
        Dropped::LooseAmmo { item, count } => Some((item.clone(), *count)),
    }
}
pub fn death_drops(arms: &mut Arms, res: &mut [u32; 3]) -> Vec<(String, u32)> {
    for (loc, item) in arms.all_stacks().into_iter().rev() { if !matches!(item, Item::Pickaxe) { let _ = arms.drop_stack(loc, item.count()); } }
    let mut seen: Vec<String> = vec![];
    for g in 0..arms.reserve.len().min(arms.data.guns.len()) { let a = arms.data.guns[g].def.ammo.clone(); let n = arms.reserve[g]; if n > 0 && !seen.contains(&a) { seen.push(a); let _ = arms.drop_ammo(g, n); } }
    while let Some(n) = arms.loose_ammo.first().map(|e| e.1) { if arms.drop_loose_ammo(0, n.max(1)).is_err() { arms.loose_ammo.remove(0); } }
    for (k, r) in res.iter_mut().enumerate() { if *r > 0 { arms.drops.push(Dropped::Resource { kind: k, count: *r }); *r = 0; } }
    drain_drops(arms)
}
/// Takes everything the player dropped since the last call as (item, count) world-item requests.
pub fn drain_drops(arms: &mut Arms) -> Vec<(String, u32)> {
    let drops = std::mem::take(&mut arms.drops);
    drops.iter().filter_map(|d| drop_request(arms, d)).filter(|(_, c)| *c > 0).collect()
}
