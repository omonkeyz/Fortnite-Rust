//! INVENTORY HOOK: the inventory operations of the player (pickup, drop, swap, equip / unequip, stack bookkeeping) on the live [`Arms`] quickbar.
//!
//! The rules are `fn_weapons::inventory_model` (slot acceptance from the Athena game mode data, `ServerAddItemInternal` auto-slotting, `SwapItems`, stack merge / overflow, the backpack count);
//! this file applies them to the sandbox's items: the pickaxe, the firearms (`QuickbarItem::Gun`) and the healing consumables (`QuickbarItem::Healable`, one STACK per slot, several slots
//! may hold stacks of the same or different consumables). Items the player owns but that are not on the bar live in [`Arms::stash`] (the backpack side of the inventory screen).
//! Ammo reserves are one stack per ammo type (`Arms::reserve`), build resources live in the sim (`fn_combat::World`), neither takes a slot or a backpack entry.
//!
//! NOT RECOVERED (stripped from the 1.8 client exe; reported, inferred as noted): where the item replaced by a full-bar pickup goes (here: dropped on the ground = returned to the caller,
//! which has no pickup actors to spawn), which slot is focused after the focused item is removed (previous slot, else the pickaxe).
use super::{Arms, QuickbarItem, SLOT_COUNT};
use fn_weapons::inventory_model::{self as m, item_type, AutoSlot, ItemRules, Refusal};

/// An owned item that is not on the quickbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stash { Gun(usize), Heal { idx: usize, count: u32 } }

/// One inventory stack: what it is and how many (firearms and the pickaxe are always 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item { Pickaxe, Gun(usize), Heal { idx: usize, count: u32 } }
impl Item {
    pub fn count(self) -> u32 { match self { Item::Heal { count, .. } => count, _ => 1 } }
}
/// Where a stack currently is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loc { Slot(usize), Stash(usize) }

/// Something the player lost to the world (for the world-item system).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dropped { Stack(Item), Ammo { gun: usize, count: u32 }, Resource { kind: usize, count: u32 }, LooseAmmo { item: String, count: u32 } }

pub fn ammo_key(path: &str) -> &str { let p = path.split('.').next().unwrap_or(path); p.rsplit('/').next().unwrap_or(p) }

/// What a pickup did.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PickupReport {
    /// Slot the (first) new stack went to; None when everything merged / went to the stash / was refused.
    pub slot: Option<usize>,
    /// Amount added to existing stacks.
    pub merged: u32,
    /// New stacks that were put in the stash (no slot accepted them).
    pub stashed: u32,
    /// The slotted stack the pickup replaced (dropped; the caller owns what happens to it).
    pub replaced: Option<Item>,
    /// What did not fit.
    pub leftover: u32,
    pub refusal: Option<Refusal>,
}

impl Arms {
    /// The 1.8 BR spawn inventory: the pickaxe in slot 0 and nothing else. DATA: `/Game/Athena/Athena_GameMode` `StartingItems` = the four build pieces, the edit tool and
    /// `WoodItemData x0` (no weapon, no consumable, no ammo, no materials); the pickaxe is the player's cosmetic `WID_Harvest_Pickaxe_Athena_*`, which only slot 0 accepts.
    /// The firearms stay loaded in `data` (pickups / the console `give` put them on the bar), they are just not owned. [`Arms::new`] is the dev loadout (AR, pump, sniper,
    /// bandages x5), kept for `--dev-loadout` / `FN_DEV_LOADOUT=1` and the scripted weapon tests.
    pub fn new_br(data: super::ArmsData, policy: super::Policy) -> Arms {
        let mut a = Arms::new(data, policy);
        for s in 1..SLOT_COUNT { a.set_slot(s, None); }
        for r in a.reserve.iter_mut() { *r = 0; }
        a.restock();
        a.selected = 0;
        a
    }

    // ----------------------------------------------------------------------------------------------------------------------------------- ammo API (see crates/fn-weapons/INVENTORY_API.md)

    /// The ammo stack of firearm `g`'s ammo type (0 for an unknown firearm).
    pub fn ammo_for_gun(&self, g: usize) -> u32 { self.reserve.get(g).copied().unwrap_or(0) }
    /// (ammo item path, count) of every ammo type the loaded firearms use, first-use order.
    pub fn ammo_stacks(&self) -> Vec<(String, u32)> {
        let mut out: Vec<(String, u32)> = vec![];
        for (g, gun) in self.data.guns.iter().enumerate() { if !out.iter().any(|(a, _)| *a == gun.def.ammo) { out.push((gun.def.ammo.clone(), self.reserve[g])); } }
        out
    }
    /// Removes up to `n` from firearm `g`'s ammo stack (shared by its ammo type); returns what was removed.
    pub fn spend_ammo(&mut self, g: usize, n: u32) -> u32 {
        if g >= self.reserve.len() { return 0; }
        let d = n.min(self.reserve[g]);
        let v = self.reserve[g] - d; self.set_reserve(g, v);
        d
    }

    // ----------------------------------------------------------------------------------------------------------------------------------- queries

    /// The stack in primary slot `slot`.
    pub fn slot_item(&self, slot: usize) -> Option<Item> {
        match self.quickbar.get(slot).copied().flatten()? {
            QuickbarItem::Pickaxe => Some(Item::Pickaxe),
            QuickbarItem::Gun(g) => Some(Item::Gun(g)),
            QuickbarItem::Healable => Some(Item::Heal { idx: self.slot_heal[slot], count: self.slot_stack[slot] }),
        }
    }
    pub fn stash_item(&self, i: usize) -> Option<Item> {
        match self.stash.get(i).copied()? { Stash::Gun(g) => Some(Item::Gun(g)), Stash::Heal { idx, count } => Some(Item::Heal { idx, count }) }
    }
    pub fn item_at(&self, loc: Loc) -> Option<Item> { match loc { Loc::Slot(s) => self.slot_item(s), Loc::Stash(i) => self.stash_item(i) } }
    /// Every stack the player owns with its location: the bar in slot order, then the stash.
    pub fn all_stacks(&self) -> Vec<(Loc, Item)> {
        let mut v: Vec<(Loc, Item)> = (0..SLOT_COUNT).filter_map(|s| self.slot_item(s).map(|i| (Loc::Slot(s), i))).collect();
        v.extend((0..self.stash.len()).filter_map(|i| self.stash_item(i).map(|it| (Loc::Stash(i), it))));
        v
    }
    /// `EFortItemType` of a stack (firearms and consumables are ranged weapon definitions).
    pub fn item_type_of(item: Item) -> u8 { match item { Item::Pickaxe => item_type::WEAPON_HARVEST, _ => item_type::WEAPON_RANGED } }
    pub fn rules_of_item(&self, item: Item) -> ItemRules {
        match item { Item::Pickaxe => ItemRules::pickaxe(), Item::Gun(_) => ItemRules::weapon(1), Item::Heal { idx, .. } => ItemRules::weapon(self.data.healables[idx].max_stack.max(1)) }
    }
    /// Item types per primary slot (None = empty), the input of the slot rules.
    pub fn bar_types(&self) -> [Option<u8>; SLOT_COUNT] { std::array::from_fn(|s| self.slot_item(s).map(Self::item_type_of)) }
    pub fn occupied(&self) -> [bool; SLOT_COUNT] { std::array::from_fn(|s| self.quickbar[s].is_some()) }
    /// `Backpack n / 5`: stacks of size-limited items, slotted or not.
    pub fn backpack_counts(&self) -> (u32, u32) {
        let n = self.all_stacks().iter().filter(|(_, it)| self.rules_of_item(*it).size_limited).count() as u32;
        (m::backpack_numerator(n, 0), m::BACKPACK_START_SIZE)
    }
    fn backpack_free(&self) -> u32 { let (n, max) = self.backpack_counts(); max.saturating_sub(n) }
    /// Counts of the stacks of healable `idx` (slot order, then the stash) with their locations.
    fn heal_stacks(&self, idx: usize) -> Vec<(Loc, u32)> {
        self.all_stacks().into_iter().filter_map(|(l, it)| match it { Item::Heal { idx: i, count } if i == idx => Some((l, count)), _ => None }).collect()
    }
    /// The primary slot holding a firearm / the stash index holding it.
    pub fn gun_loc(&self, g: usize) -> Option<Loc> {
        self.quickbar.iter().position(|q| *q == Some(QuickbarItem::Gun(g))).map(Loc::Slot).or_else(|| self.stash.iter().position(|s| *s == Stash::Gun(g)).map(Loc::Stash))
    }
    /// The focused slot as the quickbar rules see it.
    fn focus(&self) -> usize { if self.selected < SLOT_COUNT { self.selected } else { 0 } }

    // ----------------------------------------------------------------------------------------------------------------------------------- mutation helpers

    fn set_slot(&mut self, slot: usize, item: Option<Item>) {
        match item {
            None => { self.quickbar[slot] = None; self.slot_stack[slot] = 0; }
            Some(Item::Pickaxe) => { self.quickbar[slot] = Some(QuickbarItem::Pickaxe); self.slot_stack[slot] = 0; }
            Some(Item::Gun(g)) => { self.quickbar[slot] = Some(QuickbarItem::Gun(g)); self.slot_stack[slot] = 0; }
            Some(Item::Heal { idx, count }) => { self.quickbar[slot] = Some(QuickbarItem::Healable); self.slot_heal[slot] = idx; self.slot_stack[slot] = count; }
        }
    }
    /// The stash entry of an item (firearms and healable stacks).
    fn to_stash(item: Item) -> Option<Stash> { match item { Item::Pickaxe => None, Item::Gun(g) => Some(Stash::Gun(g)), Item::Heal { idx, count } => Some(Stash::Heal { idx, count }) } }
    /// `stock[idx]` = the total of every stack of healable `idx`.
    fn restock(&mut self) {
        for idx in 0..self.stock.len() { self.stock[idx] = self.heal_stacks(idx).iter().map(|(_, c)| *c).sum(); }
    }
    /// The held slot's stack pays `cost` of healable `idx` (called by the heal ability when it commits).
    pub(super) fn consume_from_held_stack(&mut self, idx: usize, cost: u32) {
        let s = self.focus();
        if self.quickbar[s] == Some(QuickbarItem::Healable) && self.slot_heal[s] == idx { self.slot_stack[s] = self.slot_stack[s].saturating_sub(cost); }
        else if let Some(l) = self.heal_stacks(idx).into_iter().find(|(l, c)| matches!(l, Loc::Slot(_)) && *c >= cost) { if let (Loc::Slot(sl), _) = l { self.slot_stack[sl] = self.slot_stack[sl].saturating_sub(cost); } }
    }
    /// Focus `slot` (the slot rules' fallback after a removal): the equip path runs like any slot change.
    fn refocus_after_removal(&mut self, removed: usize) {
        if self.focus() != removed { return; }
        let to = m::focus_after_removal(&self.occupied(), self.prev_selected.filter(|p| *p < SLOT_COUNT), removed);
        self.selected = removed; // `select` ignores the slot it is already on; the removed slot is empty now so the new focus is always another slot
        self.cancel_action_for_removal();
        let was = self.selected; self.select(to); if self.selected == was && to != was { self.selected = to; }
    }
    fn cancel_action_for_removal(&mut self) { self.cancel_action("by item removal"); self.fire_t = None; self.burst = None; self.auto_reload = None; self.refire = None; self.interrupt_retry = None; self.queued = false; }

    // ----------------------------------------------------------------------------------------------------------------------------------- pickup

    /// Puts a NEW stack on the bar by `ServerAddItemInternal`'s auto path; the replaced stack (pass 2) is returned. `None` slot = no slot accepts it (the stack must go to the stash).
    fn auto_place(&mut self, item: Item) -> Option<(usize, Option<Item>)> {
        match m::auto_slot(&self.bar_types(), self.focus(), Self::item_type_of(item), None)? {
            AutoSlot::Empty(s) => { self.set_slot(s, Some(item)); Some((s, None)) }
            AutoSlot::ReplaceFocusedScan(s) => { let old = self.slot_item(s); if s == self.focus() { self.cancel_action_for_removal(); } self.set_slot(s, Some(item)); Some((s, old)) }
        }
    }

    /// A firearm pickup. A firearm the player already has keeps its place (its magazine is refilled, like a fresh pickup); a new one needs backpack room, otherwise it replaces the slot the
    /// focus scan reaches first and the replaced stack is dropped.
    pub fn pickup_gun(&mut self, g: usize, select: bool) -> Result<PickupReport, String> {
        if g >= self.data.guns.len() { return Err(format!("no firearm {g}")); }
        let mut rep = PickupReport::default();
        match self.gun_loc(g) {
            Some(Loc::Slot(s)) => { rep.slot = Some(s); }
            Some(Loc::Stash(i)) => { // the owned weapon is picked up again: it is equipped into a slot like `EquipItem`
                self.stash.remove(i);
                let (s, old) = self.auto_place(Item::Gun(g)).ok_or("no quickbar slot accepts a firearm")?;
                if let Some(o) = old { if let Some(st) = Self::to_stash(o) { self.stash.push(st); } }
                rep.slot = Some(s);
            }
            None => {
                let plan = m::plan_pickup(&ItemRules::weapon(1), &[], 1, self.backpack_free());
                if plan.new_stacks.is_empty() {
                    // backpack full: a pickup swaps with the slot the focus scan reaches (pass 2); nothing replaceable = refused
                    let types = self.bar_types();
                    match m::auto_slot(&types, self.focus(), item_type::WEAPON_RANGED, None) {
                        Some(AutoSlot::ReplaceFocusedScan(s)) if self.slot_item(s).is_some_and(|it| self.rules_of_item(it).can_be_dropped) => {
                            let (s2, old) = self.auto_place(Item::Gun(g)).ok_or("no quickbar slot accepts a firearm")?; debug_assert_eq!(s, s2);
                            rep.slot = Some(s2); rep.replaced = old; if let Some(o) = old { self.drops.push(Dropped::Stack(o)); }
                        }
                        _ => { rep.refusal = plan.refusal; rep.leftover = 1; return Ok(rep); }
                    }
                } else {
                    match self.auto_place(Item::Gun(g)) { Some((s, _)) => rep.slot = Some(s), None => { self.stash.push(Stash::Gun(g)); rep.stashed = 1; } }
                }
            }
        }
        self.mags[g] = self.data.guns[g].def.clip_size;
        let name = self.data.guns[g].def.item.rsplit('/').next().unwrap_or_default().to_owned();
        self.log(format!("pickup firearm {name} (gun {g}) -> {}{}", rep.slot.map_or("stash".to_owned(), |s| format!("quickbar slot {}", s + 1)), rep.replaced.map_or(String::new(), |r| format!(" (replaced {r:?}, dropped)"))));
        if let (true, Some(s)) = (select, rep.slot) { self.equip_slot_fresh(s); }
        self.restock();
        Ok(rep)
    }
    /// Would a NEW item (a stack that needs a backpack entry) replace a slotted item instead of taking a free entry? (`GetInteractText` shows "Swap equipped" then.)
    pub fn pickup_swaps(&self) -> bool {
        self.backpack_free() == 0 && matches!(m::auto_slot(&self.bar_types(), self.focus(), item_type::WEAPON_RANGED, None), Some(AutoSlot::ReplaceFocusedScan(s)) if self.slot_item(s).is_some_and(|it| self.rules_of_item(it).can_be_dropped))
    }
    /// Why `count` of healable `idx` cannot be picked up at all (the `AFortPickup` error text), or None when at least part of it can be (merged, a new stack, or a swap).
    pub fn heal_pickup_refusal(&self, idx: usize, count: u32) -> Option<&'static str> {
        if idx >= self.stock.len() { return Some("Can't pickup this item!"); }
        let counts: Vec<u32> = self.heal_stacks(idx).iter().map(|(_, c)| *c).collect();
        let plan = m::plan_pickup(&self.rules_of_item(Item::Heal { idx, count: 0 }), &counts, count, self.backpack_free());
        if !plan.merged.is_empty() || !plan.new_stacks.is_empty() || self.pickup_swaps() { None } else { plan.refusal.map(Refusal::text) }
    }
    pub fn heal_pickup_swaps(&self, idx: usize, count: u32) -> bool {
        if idx >= self.stock.len() { return false; }
        let counts: Vec<u32> = self.heal_stacks(idx).iter().map(|(_, c)| *c).collect();
        let plan = m::plan_pickup(&self.rules_of_item(Item::Heal { idx, count: 0 }), &counts, count, self.backpack_free());
        plan.merged.is_empty() && plan.new_stacks.is_empty() && self.pickup_swaps()
    }
    /// Why a firearm pickup is refused (full backpack and nothing to swap with), or None.
    pub fn gun_pickup_refusal(&self, g: usize) -> Option<&'static str> {
        if self.gun_loc(g).is_some() || self.backpack_free() > 0 || self.pickup_swaps() { None } else { Some(Refusal::BackpackFull.text()) }
    }
    /// Selects `slot` and plays the equip even if it is the held one (a swapped-in weapon is equipped fresh).
    fn equip_slot_fresh(&mut self, slot: usize) {
        if self.selected == slot { self.selected = usize::MAX; }
        self.select(slot);
        if self.selected == usize::MAX { self.selected = slot; }
    }

    /// A pickup of `count` of healable `idx`: existing stacks are topped up to `MaxStackSize`, the rest becomes new stacks (each takes a slot by the auto rule, or the stash) while the backpack
    /// has room; with the backpack full the first new stack replaces the slot the focus scan reaches.
    pub fn pickup_heal(&mut self, idx: usize, count: u32, allow_swap: bool) -> Result<PickupReport, String> {
        if idx >= self.stock.len() { return Err(format!("no consumable {idx}")); }
        let rules = self.rules_of_item(Item::Heal { idx, count: 0 });
        let existing = self.heal_stacks(idx);
        let counts: Vec<u32> = existing.iter().map(|(_, c)| *c).collect();
        let mut plan = m::plan_pickup(&rules, &counts, count, self.backpack_free());
        let mut rep = PickupReport { leftover: plan.leftover, refusal: plan.refusal, ..Default::default() };
        // full backpack: the first new stack swaps with the focus-scan slot
        if allow_swap && plan.refusal == Some(Refusal::BackpackFull) && plan.new_stacks.is_empty() && plan.merged.is_empty() && plan.leftover > 0 { // nothing fit at all: swap (a partly merged pile leaves its rest on the ground)
            let types = self.bar_types();
            if let Some(AutoSlot::ReplaceFocusedScan(s)) = m::auto_slot(&types, self.focus(), item_type::WEAPON_RANGED, None) {
                if self.slot_item(s).is_some_and(|it| self.rules_of_item(it).can_be_dropped) { let first = plan.leftover.min(rules.max_stack); plan.new_stacks.push(first); plan.leftover -= first; rep.leftover = plan.leftover; rep.refusal = (plan.leftover > 0).then_some(Refusal::BackpackFull); }
            }
        }
        for (i, add) in &plan.merged {
            match existing[*i].0 { Loc::Slot(s) => self.slot_stack[s] += add, Loc::Stash(k) => if let Some(Stash::Heal { count, .. }) = self.stash.get_mut(k) { *count += add } }
            rep.merged += add;
        }
        let full = self.backpack_free() == 0;
        for (n, c) in plan.new_stacks.iter().enumerate() {
            let item = Item::Heal { idx, count: *c };
            if n == 0 && full { // swap with the focus-scan slot
                if let Some((s, old)) = self.auto_place(item) { rep.slot.get_or_insert(s); rep.replaced = old; if let Some(o) = old { self.drops.push(Dropped::Stack(o)); } continue; }
            }
            match self.auto_place(item) {
                Some((s, old)) => { rep.slot.get_or_insert(s); if let Some(o) = old { rep.replaced = Some(o); self.drops.push(Dropped::Stack(o)); } }
                None => { self.stash.push(Stash::Heal { idx, count: *c }); rep.stashed += 1; }
            }
        }
        self.restock();
        let name = self.data.healables[idx].name.clone();
        self.log(format!("pickup {count} {name}: merged {} new stacks {:?} slot {:?} replaced {:?} leftover {}{}", rep.merged, plan.new_stacks, rep.slot.map(|s| s + 1), rep.replaced, rep.leftover, rep.refusal.map_or(String::new(), |r| format!(" ({})", r.text()))));
        Ok(rep)
    }

    /// Ammo pickup: the one stack of firearm `g`'s ammo type (shared by every firearm of that type) is topped up to its `MaxStackSize`; returns (added, leftover).
    pub fn pickup_ammo(&mut self, g: usize, count: u32) -> Result<(u32, u32), String> {
        if g >= self.data.guns.len() { return Err(format!("no firearm {g}")); }
        let max = self.data.ammo.max_stack.get(g).copied().unwrap_or(u32::MAX);
        let rules = ItemRules::ammo(max);
        let have = self.reserve[g];
        let held = [have]; let plan = m::plan_pickup(&rules, if have > 0 { &held[..] } else { &[] }, count, 0);
        let added = count - plan.leftover;
        let new = have + added;
        let ammo = self.data.guns[g].def.ammo.clone();
        for h in 0..self.reserve.len() { if self.data.guns[h].def.ammo == ammo { self.reserve[h] = new; } }
        Ok((added, plan.leftover))
    }

    pub fn ammo_gun_of(&self, item: &str) -> Option<usize> { let k = ammo_key(item); self.data.guns.iter().position(|g| ammo_key(&g.def.ammo) == k) }
    pub fn ammo_count(&self, item: &str) -> u32 {
        match self.ammo_gun_of(item) { Some(g) => self.reserve[g], None => { let k = ammo_key(item); self.loose_ammo.iter().find(|(p, _)| ammo_key(p) == k).map_or(0, |e| e.1) } }
    }
    pub fn pickup_ammo_item(&mut self, item: &str, max_stack: u32, count: u32) -> (u32, u32) {
        if let Some(g) = self.ammo_gun_of(item) { return self.pickup_ammo(g, count).unwrap_or((0, count)); }
        let k = ammo_key(item).to_owned();
        let have = self.ammo_count(item);
        let held = [have]; let plan = m::plan_pickup(&ItemRules::ammo(max_stack), if have > 0 { &held[..] } else { &[] }, count, 0);
        let added = count - plan.leftover;
        if added > 0 {
            match self.loose_ammo.iter_mut().find(|(p, _)| ammo_key(p) == k) { Some(e) => e.1 += added, None => self.loose_ammo.push((item.to_owned(), added)) }
            self.log(format!("pickup {added} {k}: no firearm uses it, kept in the ammo pool ({} now)", have + added));
        }
        (added, plan.leftover)
    }
    pub fn take_loose_ammo(&mut self, item: &str) -> Option<u32> {
        let k = ammo_key(item);
        let i = self.loose_ammo.iter().position(|(p, _)| ammo_key(p) == k)?;
        Some(self.loose_ammo.remove(i).1)
    }
    pub fn drop_loose_ammo(&mut self, idx: usize, quantity: u32) -> Result<u32, String> {
        let (item, have) = self.loose_ammo.get(idx).cloned().ok_or("no such ammo stack")?;
        let (d, rem) = m::drop_amount(&ItemRules::ammo(u32::MAX), have, quantity).ok_or("nothing to drop")?;
        if rem == 0 { self.loose_ammo.remove(idx); } else { self.loose_ammo[idx].1 = rem; }
        self.drops.push(Dropped::LooseAmmo { item, count: d });
        Ok(d)
    }

    // ----------------------------------------------------------------------------------------------------------------------------------- drop / unequip / equip / move

    /// `DropItem(item, quantity)`: what is dropped is returned (the caller has no pickup actors in the sandbox, so it is gone unless the caller spawns one). The pickaxe cannot be dropped.
    pub fn drop_stack(&mut self, loc: Loc, quantity: u32) -> Result<Item, String> {
        let item = self.item_at(loc).ok_or("no such inventory item")?;
        let (dropped, remaining) = m::drop_amount(&self.rules_of_item(item), item.count(), quantity).ok_or("this item cannot be dropped")?;
        match (loc, item) {
            (Loc::Slot(s), Item::Heal { idx, .. }) if remaining > 0 => { self.slot_stack[s] = remaining; self.log(format!("drop {dropped} {} from slot {} ({remaining} left)", self.data.healables[idx].name, s + 1)); }
            (Loc::Slot(s), _) => { self.set_slot(s, None); self.log(format!("drop {item:?} from slot {}", s + 1)); self.refocus_after_removal(s); }
            (Loc::Stash(i), Item::Heal { idx, .. }) if remaining > 0 => { self.stash[i] = Stash::Heal { idx, count: remaining }; }
            (Loc::Stash(i), _) => { self.stash.remove(i); }
        }
        let out = match item { Item::Heal { idx, .. } => Item::Heal { idx, count: dropped }, other => other };
        self.drops.push(Dropped::Stack(out));
        self.restock();
        Ok(out)
    }
    /// Ammo drop: removes up to `quantity` of the ammo stack of firearm `g`'s type; returns what was removed.
    pub fn drop_ammo(&mut self, g: usize, quantity: u32) -> Result<u32, String> {
        if g >= self.data.guns.len() { return Err(format!("no firearm {g}")); }
        let (d, rem) = m::drop_amount(&ItemRules::ammo(u32::MAX), self.reserve[g], quantity).ok_or("nothing to drop")?;
        let ammo = self.data.guns[g].def.ammo.clone();
        for h in 0..self.reserve.len() { if self.data.guns[h].def.ammo == ammo { self.reserve[h] = rem; } }
        self.drops.push(Dropped::Ammo { gun: g, count: d });
        Ok(d)
    }

    /// `RemoveItemFromQuickBar`: the slot's item stays in the inventory (stash), the slot empties; the pickaxe stays.
    pub fn unequip_slot(&mut self, slot: usize) -> Result<(), String> {
        let item = self.slot_item(slot).ok_or("that slot is empty")?;
        let st = Self::to_stash(item).ok_or("the pickaxe cannot leave its slot")?;
        self.stash.push(st);
        self.set_slot(slot, None);
        self.refocus_after_removal(slot);
        self.restock();
        Ok(())
    }
    /// `EquipItem` for a stash stack: the auto path (empty slot first, else the focus-scan slot whose item goes back to the stash), then it is held.
    pub fn equip_from_stash(&mut self, i: usize) -> Result<usize, String> {
        let item = self.stash_item(i).ok_or("no such stash item")?;
        let target = m::auto_slot(&self.bar_types(), self.focus(), Self::item_type_of(item), None).ok_or("no quickbar slot accepts this item")?;
        self.stash.remove(i);
        let (s, old) = match target {
            AutoSlot::Empty(s) => { self.set_slot(s, Some(item)); (s, None) }
            AutoSlot::ReplaceFocusedScan(s) => { let old = self.slot_item(s); if s == self.focus() { self.cancel_action_for_removal(); } self.set_slot(s, Some(item)); (s, old) }
        };
        if let Some(o) = old { if let Some(st) = Self::to_stash(o) { self.stash.push(st); } }
        self.equip_slot_fresh(s);
        self.restock();
        Ok(s)
    }
    /// `DropItemOnQuickBar(item, 0, slot)` (the recovered flow of `fn_weapons::native_inventory`): the destination must accept the item's type; a slotted item swaps with the destination
    /// (`SwapItems`, the focus follows), an unslotted one takes the slot and the previous occupant goes back to the stash. Dropping on the item's own slot does nothing.
    pub fn put_on_slot(&mut self, loc: Loc, slot: usize) -> Result<(), String> {
        if slot >= SLOT_COUNT { return Err(format!("invalid primary quickbar slot {slot}")); }
        let item = self.item_at(loc).ok_or("no such inventory item")?;
        if !m::slot_accepts(slot, Self::item_type_of(item)) { return Err(format!("slot {} does not accept this item type", slot + 1)); }
        match loc {
            Loc::Slot(from) => {
                if from == slot { return Ok(()); }
                let focus = m::swap_slots(&self.bar_types(), self.focus(), from, slot).ok_or("the quickbar refuses this swap")?;
                let (a, b) = (self.slot_item(from), self.slot_item(slot));
                self.set_slot(from, b); self.set_slot(slot, a);
                if self.selected < SLOT_COUNT && focus != self.selected { self.selected = focus; }
                // the held consumable follows its slot
                if self.quickbar[self.focus()] == Some(QuickbarItem::Healable) { self.slot5 = self.slot_heal[self.focus()]; }
                self.log(format!("inventory move: slot {} <-> slot {}", from + 1, slot + 1));
            }
            Loc::Stash(i) => {
                let old = self.slot_item(slot);
                if slot == self.focus() { self.cancel_action_for_removal(); }
                self.stash.remove(i);
                self.set_slot(slot, Some(item));
                if let Some(o) = old { if let Some(st) = Self::to_stash(o) { self.stash.push(st); } }
                if slot == self.focus() { self.equip_slot_fresh(slot); }
                self.log(format!("inventory: stash item -> slot {}", slot + 1));
            }
        }
        self.restock();
        Ok(())
    }

    // ----------------------------------------------------------------------------------------------------------------------------------- per-frame

    /// A consumable stack that was used up leaves its slot (the item is gone from the inventory); the focus falls back like after a drop. Runs after the heal ability finished.
    pub fn reap_used_up_stacks(&mut self) {
        if self.is_healing() { return; }
        for s in 0..SLOT_COUNT {
            if self.quickbar[s] == Some(QuickbarItem::Healable) && self.slot_stack[s] == 0 {
                self.log(format!("slot {} stack used up: item removed", s + 1));
                self.set_slot(s, None);
                self.refocus_after_removal(s);
            }
        }
        self.stash.retain(|st| !matches!(st, Stash::Heal { count: 0, .. }));
        self.restock();
    }
    /// Development / console hook: makes slot `slot` (or the first consumable slot) hold `n` of healable `idx` (replaces what `ArmsInput::give` did with the single consumable slot).
    pub fn set_consumable_slot(&mut self, idx: usize, n: u32) {
        let slot = self.quickbar.iter().position(|q| *q == Some(QuickbarItem::Healable)).or_else(|| self.quickbar.iter().position(|q| q.is_none()));
        if let Some(s) = slot { self.set_slot(s, Some(Item::Heal { idx, count: n })); if self.selected == s { self.slot5 = idx; } }
        self.restock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arms::{tests::data, Policy};
    fn arms() -> Arms { Arms::new(data(), Policy::default()) }
    /// A new firearm (cloned from gun 0 with its own item name) so the bar can be filled past the starters.
    fn add_gun(a: &mut Arms, n: usize) -> usize { let mut g = a.data.guns[0].clone(); g.def.item = format!("/Game/Test/Gun{n}"); a.add_gun(g, 999, 12, 0) }

    #[test] fn the_starter_bar_is_four_backpack_items_with_one_free() {
        let a = arms();
        assert_eq!(a.backpack_counts(), (4, 5), "AR, shotgun, sniper, bandage stack; the pickaxe is not size limited");
        assert_eq!(a.bar_types(), [Some(item_type::WEAPON_HARVEST), Some(17), Some(17), Some(17), Some(17), None]);
    }
    #[test] fn a_new_firearm_takes_the_empty_slot_then_replaces_from_the_focus_scan() {
        let mut a = arms();
        let g3 = add_gun(&mut a, 3); let g4 = add_gun(&mut a, 4); let g5 = add_gun(&mut a, 5);
        let r = a.pickup_gun(g3, false).unwrap(); assert_eq!((r.slot, r.replaced), (Some(5), None)); assert_eq!(a.backpack_counts(), (5, 5));
        // full, pickaxe held: slot 0 rejects the weapon, the scan reaches slot 1 (the AR), which is dropped
        let r = a.pickup_gun(g4, false).unwrap(); assert_eq!((r.slot, r.replaced), (Some(1), Some(Item::Gun(0)))); assert_eq!(a.quickbar[1], Some(QuickbarItem::Gun(g4)));
        // full, slot 3 focused: that slot is replaced
        a.select(3); let r = a.pickup_gun(g5, false).unwrap(); assert_eq!((r.slot, r.replaced), (Some(3), Some(Item::Gun(2)))); assert_eq!(a.backpack_counts(), (5, 5));
        // the same weapon again keeps its place
        let r = a.pickup_gun(g5, false).unwrap(); assert_eq!((r.slot, r.replaced), (Some(3), None));
    }
    #[test] fn consumables_top_up_then_open_new_stacks_and_stop_at_the_backpack() {
        let mut a = arms(); // bandage stack of 5 in slot 5, free backpack 1
        let r = a.pickup_heal(0, 12, true).unwrap();
        assert_eq!((r.merged, r.slot, r.leftover), (10, Some(5), 0), "10 top the first stack up to 15, 2 open a second stack in slot 6");
        assert_eq!((a.slot_stack[4], a.slot_stack[5], a.stock[0]), (15, 2, 17));
        assert_eq!(a.backpack_counts(), (5, 5));
        let r = a.pickup_heal(0, 20, true).unwrap(); // second stack tops up to 15, the rest is refused: the backpack is full and the pile partly merged (no swap)
        assert_eq!(a.slot_stack[5], 15); assert_eq!((r.merged, r.leftover), (13, 7)); assert_eq!(a.stock[0], 30);
        let r = a.pickup_heal(1, 1, true).unwrap(); // a medkit with a full backpack swaps with the focus scan (pickaxe held: slot 1)
        assert_eq!((r.slot, r.replaced), (Some(1), Some(Item::Gun(0)))); assert_eq!((a.quickbar[1], a.slot_heal[1], a.slot_stack[1]), (Some(QuickbarItem::Healable), 1, 1));
    }
    #[test] fn a_used_up_stack_leaves_its_slot_and_focus_goes_back() {
        let mut a = arms(); a.select(2); a.select(4);
        a.slot_stack[4] = 0; a.stock[0] = 0;
        a.reap_used_up_stacks();
        assert_eq!(a.quickbar[4], None); assert_eq!(a.selected, 2, "back to the previously focused slot");
        assert_eq!(a.backpack_counts(), (3, 5));
    }
    #[test] fn drops_follow_can_be_dropped_and_clamp_the_stack() {
        let mut a = arms();
        assert!(a.drop_stack(Loc::Slot(0), 1).is_err(), "the pickaxe cannot be dropped");
        let d = a.drop_stack(Loc::Slot(4), 2).unwrap(); assert_eq!(d, Item::Heal { idx: 0, count: 2 }); assert_eq!((a.slot_stack[4], a.stock[0]), (3, 3));
        a.select(1); let d = a.drop_stack(Loc::Slot(1), 1).unwrap(); assert_eq!(d, Item::Gun(0)); assert_eq!(a.quickbar[1], None); assert_eq!(a.selected, 0, "no previous slot: the pickaxe");
        let d = a.drop_stack(Loc::Slot(4), 99).unwrap(); assert_eq!(d, Item::Heal { idx: 0, count: 3 }); assert_eq!((a.quickbar[4], a.stock[0]), (None, 0));
    }
    #[test] fn unequip_keeps_the_item_in_the_backpack_and_equip_puts_it_back() {
        let mut a = arms();
        a.unequip_slot(2).unwrap(); assert_eq!(a.quickbar[2], None); assert_eq!(a.stash, vec![Stash::Gun(1)]); assert_eq!(a.backpack_counts(), (4, 5), "still counted");
        assert!(a.unequip_slot(0).is_err());
        let s = a.equip_from_stash(0).unwrap(); assert_eq!((s, a.quickbar[2], a.stash.len(), a.selected), (2, Some(QuickbarItem::Gun(1)), 0, 2), "first empty accepting slot, then held");
    }
    #[test] fn slot_moves_use_the_swap_rules_and_the_focus_follows() {
        let mut a = arms(); a.select(1);
        a.put_on_slot(Loc::Slot(1), 4).unwrap(); assert_eq!((a.quickbar[1], a.quickbar[4]), (Some(QuickbarItem::Healable), Some(QuickbarItem::Gun(0)))); assert_eq!(a.selected, 4, "the held AR moved to slot 5");
        assert_eq!((a.slot_stack[1], a.slot_heal[1]), (5, 0), "the consumable stack moved with its slot");
        assert!(a.put_on_slot(Loc::Slot(1), 0).is_err(), "slot 0 only takes the pickaxe");
        assert!(a.put_on_slot(Loc::Slot(0), 3).is_err(), "the pickaxe only goes to slot 0");
        // an unslotted item dropped on an occupied slot replaces it; the old occupant goes to the backpack
        a.unequip_slot(3).unwrap(); a.put_on_slot(Loc::Stash(0), 2).unwrap();
        assert_eq!((a.quickbar[2], a.stash.clone()), (Some(QuickbarItem::Gun(2)), vec![Stash::Gun(1)]));
    }
    #[test] fn the_br_start_is_the_pickaxe_alone() {
        let a = Arms::new_br(data(), Policy::default());
        assert_eq!(a.quickbar, [Some(QuickbarItem::Pickaxe), None, None, None, None, None]);
        assert_eq!((a.backpack_counts(), a.stock.iter().sum::<u32>(), a.reserve.iter().sum::<u32>(), a.stash.len()), ((0, 5), 0, 0, 0));
        assert!(a.pickaxe_selected());
    }
    #[test] fn ammo_api_spends_the_shared_stack() {
        let mut a = arms(); a.reserve = vec![30, 0, 0];
        assert_eq!(a.spend_ammo(0, 50), 30); assert_eq!(a.ammo_for_gun(0), 0); assert_eq!(a.spend_ammo(9, 1), 0);
        assert_eq!(a.ammo_stacks().len(), {let mut v: Vec<&str> = a.data.guns.iter().map(|g| g.def.ammo.as_str()).collect(); v.sort(); v.dedup(); v.len()});
    }
    #[test] fn ammo_is_one_capped_stack_per_type() {
        let mut a = arms(); a.data.ammo.max_stack = vec![999; 3]; a.reserve = vec![990, 0, 0];
        assert_eq!(a.pickup_ammo(0, 20).unwrap(), (9, 11), "capped at MaxStackSize, the rest stays on the ground");
        assert_eq!(a.drop_ammo(0, 100).unwrap(), 100); assert_eq!(a.reserve[0], 899);
    }
    fn fill_backpack(a: &mut Arms) { let r = a.pickup_heal(1, 1, true).unwrap(); assert_eq!((r.slot, r.leftover), (Some(5), 0)); assert_eq!(a.backpack_counts(), (5, 5)); }
    fn owned_heal(a: &Arms, idx: usize) -> u32 { a.heal_stacks(idx).iter().map(|(_, c)| *c).sum() }
    fn dropped_heal(a: &Arms, idx: usize) -> u32 { a.drops.iter().map(|d| match d { Dropped::Stack(Item::Heal { idx: i, count }) if *i == idx => *count, _ => 0 }).sum() }
    #[test] fn a_partial_bandage_pile_takes_what_fits_and_leaves_the_rest() {
        let mut a = arms(); fill_backpack(&mut a);
        let r = a.pickup_heal(0, 15, true).unwrap();
        assert_eq!((r.merged, r.leftover, r.replaced), (10, 5, None), "the stack of 5 tops up to 15, the other 5 stay on the ground");
        assert_eq!(owned_heal(&a, 0) + r.leftover, 5 + 15, "nothing vanishes");
        assert!(a.drops.is_empty());
        assert!(a.heal_pickup_swaps(0, 5), "the rest no longer fits anywhere: Use swaps it in");
        let r = a.pickup_heal(0, 5, true).unwrap();
        assert_eq!((r.slot, r.leftover, r.replaced), (Some(1), 0, Some(Item::Gun(0))));
        assert_eq!(owned_heal(&a, 0), 20);
    }
    #[test] fn three_consumables_on_a_full_backpack_each_swap_the_focus_scan_slot_and_nothing_is_lost() {
        let mut a = arms(); fill_backpack(&mut a);
        let before: Vec<Item> = (0..SLOT_COUNT).filter_map(|s| a.slot_item(s)).collect();
        assert!(a.heal_pickup_swaps(2, 2) && !a.heal_pickup_swaps(0, 3), "a new consumable swaps, an owned one merges");
        for (idx, n) in [(2usize, 2u32), (3, 3), (2, 1)] {
            let r = a.pickup_heal(idx, n, true).unwrap();
            assert_eq!((r.slot, r.leftover), (Some(1), 0), "pickaxe held: pass 2 of ServerAddItemInternal replaces slot 1 every time");
        }
        assert_eq!((a.slot_heal[1], a.slot_stack[1]), (2, 1));
        assert_eq!(a.drops.first(), Some(&Dropped::Stack(Item::Gun(0))), "the AR that was in slot 1 is on the ground");
        assert_eq!((dropped_heal(&a, 2), dropped_heal(&a, 3)), (2, 3), "the swapped-out consumables are on the ground");
        assert_eq!(a.backpack_counts(), (5, 5));
        let after: u32 = (0..SLOT_COUNT).filter_map(|s| a.slot_item(s)).map(|i| i.count()).sum::<u32>() + a.drops.iter().map(|d| match d { Dropped::Stack(i) => i.count(), _ => 0 }).sum::<u32>();
        assert_eq!(after, before.iter().map(|i| i.count()).sum::<u32>() + 2 + 3 + 1, "every item is in the inventory or on the ground");
    }
    #[test] fn two_firearms_one_takes_the_free_slot_the_other_swaps_and_nothing_is_lost() {
        let mut a = arms();
        let g3 = add_gun(&mut a, 3); let g4 = add_gun(&mut a, 4);
        let r = a.pickup_gun(g3, false).unwrap(); assert_eq!((r.slot, r.replaced, r.refusal), (Some(5), None, None));
        let r = a.pickup_gun(g4, false).unwrap(); assert_eq!((r.slot, r.replaced, r.refusal), (Some(1), Some(Item::Gun(0)), None));
        assert_eq!((a.gun_loc(g3), a.gun_loc(g4)), (Some(Loc::Slot(5)), Some(Loc::Slot(1))));
        assert_eq!(a.drops, vec![Dropped::Stack(Item::Gun(0))], "the replaced AR is on the ground");
        assert_eq!(a.give_gun(g3, false).map(|(s, _)| s), Ok(5), "an owned firearm keeps its slot");
    }
    #[test] fn surplus_ammo_stays_on_the_ground() {
        let mut a = arms(); a.data.ammo.max_stack = vec![999; 3]; a.reserve = vec![989, 0, 0];
        let piles = [30u32, 30, 30, 30];
        let rests: Vec<u32> = piles.iter().map(|n| a.pickup_ammo(0, *n).unwrap()).map(|(added, rest)| { assert_eq!(added + rest, 30); rest }).collect();
        assert_eq!(rests, vec![20, 30, 30, 30]);
        assert_eq!(a.reserve[0], 999);
        assert_eq!(a.reserve[0] - 989 + rests.iter().sum::<u32>(), 120, "taken + left on the ground = picked up");
    }
    #[test] fn several_consumable_pickups_arriving_together_take_what_fits_in_order() {
        let mut a = arms();
        let mut ground = 0;
        let mut rests = vec![];
        for n in [12u32, 12, 12] { let r = a.pickup_heal(0, n, true).unwrap(); assert_eq!(r.replaced, None); rests.push(r.leftover); ground += r.leftover; }
        assert_eq!(rests, vec![0, 0, 11]);
        assert_eq!(a.backpack_counts(), (5, 5));
        assert_eq!(owned_heal(&a, 0), 30, "the stack of 5 tops up, the free backpack entry takes a second stack of 15");
        assert_eq!(owned_heal(&a, 0) + ground, 5 + 36, "the rest stays on the ground");
        assert!(a.drops.is_empty());
    }
}
