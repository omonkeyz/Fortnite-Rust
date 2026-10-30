//! CONSOLE HOOK: the in-game F2 command bar (`fn-console` = registry / parser / completion / history; this file = the front-end half).
//!
//! * [`keys_from_winit`]  window key event -> the console's abstract keys (F2 toggles, printable text becomes characters)
//! * [`build`]            the console's item catalog from what the sandbox can really hold: the three firearms of `Arms`, the four healing consumables, the pickaxe and the
//!                        wood / stone / metal items, with display names / rarities / stack sizes read from the 1.8 item definitions (`fn_pose::loadout_inventory::entry`)
//! * [`apply`]            the console's player-state requests (health, shield, give gun / consumable / ammo) applied to the live [`Arms`] and the quickbar HUD cards
//! * [`tick`]             god / infinite ammo, applied every frame
//!
//! The overlay is drawn by `console_ui.rs` (the 1.8 UI fonts through `fn_ui`); the App (`app.rs`, "CONSOLE HOOK" block) owns the key capture, so while the bar is open no
//! movement / fire / quickbar input reaches the sim.
use crate::arms::{Arms, QuickbarItem, MAX_HEALTH, MAX_SHIELD};
use fn_console::{Catalog, Console, Flags, ItemInfo, ItemKind, LineKind, Request, Resource};
use fn_ui::screens::hud::HudItem;

/// Everything the console needs from the loaded game that is not in the sim.
pub struct ConsoleHost {
    pub console: Console,
    pub built: Option<Built>,
}
impl Default for ConsoleHost { fn default() -> Self { ConsoleHost { console: Console::new(), built: None } } }

pub struct Built {
    pub catalog: Catalog,
    /// HUD icon (PNG name, RGBA) of each healing consumable, decoded from its `SmallPreviewImage`; None when it could not be read.
    pub icons: Vec<Option<(String, u32, u32, Vec<u8>)>>,
    /// The quickbar card the consumable slot started with (the bandage), used when the bandage is put back into the slot.
    pub base_card: Option<HudItem>,
    /// What could not be read from the paks (shown in the log when the bar first opens).
    pub notes: Vec<String>,
}

pub fn keys_from_winit(event: &winit::event::KeyEvent, shift: bool) -> Vec<fn_console::Key> {
    use fn_console::Key;
    use winit::keyboard::{Key as WKey, KeyCode, NamedKey, PhysicalKey};
    let text = || event.text.as_ref().map(|t| t.chars().filter(|c| (' '..='~').contains(c)).map(Key::Char).collect()).unwrap_or_default();
    if let PhysicalKey::Code(KeyCode::Backquote) = event.physical_key { return vec![Key::ToggleConsole]; }
    if let WKey::Named(n) = &event.logical_key {
        let k = match n {
            NamedKey::Backspace => Key::Backspace, NamedKey::Delete => Key::Delete, NamedKey::ArrowLeft => Key::Left, NamedKey::ArrowRight => Key::Right,
            NamedKey::Home => Key::Home, NamedKey::End => Key::End, NamedKey::ArrowUp => Key::Up, NamedKey::ArrowDown => Key::Down,
            NamedKey::PageUp => Key::PageUp, NamedKey::PageDown => Key::PageDown,
            NamedKey::Tab => if shift { Key::ShiftTab } else { Key::Tab }, NamedKey::Enter => Key::Enter, NamedKey::Escape => Key::Escape,
            NamedKey::Space => Key::Char(' '),
            _ => return text(),
        };
        return vec![k];
    }
    text()
}

fn short(path: &str) -> String { path.rsplit('/').next().unwrap_or(path).split('.').next().unwrap_or(path).to_owned() }

/// The item definition at `path` as the inventory screen reads it (display name, rarity, stack size, icon path).
fn read_item(set: &fn_pak::PakSet, path: &str) -> Result<fn_pose::loadout_inventory::CatalogItem, String> {
    let src = fn_pose::ClipSourceDir::Pak(set);
    let pkg = fn_pose::loader::package(&src, path)?;
    let (idx, class) = (0..pkg.exports.len()).find_map(|n| { let c = pkg.export_class(n); c.contains("ItemDefinition").then(|| (n, c)) }).ok_or_else(|| format!("{path}: no item definition export"))?;
    let (start, end) = pkg.export_range(idx);
    let p = fn_ueio::props::read_props(&mut fn_ueio::Reader::at(&pkg.data[..end], start), &fn_ueio::props::Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())?;
    let icon_path = p.object("SmallPreviewImage").filter(|i| *i < 0).map(|i| pkg.import_path(i)).unwrap_or_default();
    Ok(fn_pose::loadout_inventory::entry(path, &class, &p, &icon_path))
}

/// Builds the catalog once. Firearms come from the loaded definitions; the others are read from their item definitions in the paks (the cosmetic catalog's pak set is reused).
pub fn build(arms: &Arms, base_card: Option<HudItem>) -> Built {
    let mut notes = vec![];
    let mut items = vec![];
    let set = match fn_game::cosmetic_swap::paks() { Ok(s) => Some(s), Err(e) => { notes.push(format!("item definitions unavailable ({e}): items are listed by asset name without display name / rarity")); None } };
    for (g, gun) in arms.data.guns.iter().enumerate() {
        let id = short(&gun.def.item);
        // the WID's own DisplayName / Rarity (the loaded firearm definition keeps only the stat row); the definition's strings are the fallback
        let read = set.as_deref().ok_or_else(|| "no pak set".to_owned()).and_then(|s| read_item(s, &gun.def.item)).ok();
        let name = read.as_ref().map(|c| c.name.clone()).filter(|n| !n.is_empty()).or_else(|| gun.def.display_name.clone()).unwrap_or_else(|| id.clone());
        let rarity = read.as_ref().map(|c| c.rarity).or_else(|| gun.def.rarity.as_deref().and_then(fn_console::catalog::rarity_of));
        items.push(ItemInfo { name, id, rarity, kind: ItemKind::Gun(g), max_stack: None });
    }
    let mut icons = vec![];
    for (idx, h) in arms.data.healables.iter().enumerate() {
        let path = fn_pose::loadout_stats::HEALABLE_ITEMS.get(idx).copied().unwrap_or("");
        let read = set.as_deref().ok_or_else(|| "no pak set".to_owned()).and_then(|s| read_item(s, path));
        match read {
            Ok(c) => {
                let icon = (!c.icon_path.is_empty()).then(|| fn_pose::loadout_stats::decode_ui_texture(&fn_pose::ClipSourceDir::Pak(set.as_deref().unwrap()), &c.icon_path)).and_then(|r| r.map_err(|e| notes.push(format!("{}: icon not decoded ({e})", c.name))).ok());
                icons.push(icon);
                items.push(ItemInfo { id: short(path), name: if c.name.is_empty() { h.name.clone() } else { c.name.clone() }, rarity: Some(c.rarity), kind: ItemKind::Healable(idx), max_stack: c.max_stack.or(Some(h.max_stack)) });
            }
            Err(e) => {
                if set.is_some() { notes.push(format!("{path}: {e}")); }
                icons.push(None);
                items.push(ItemInfo { id: short(path), name: h.name.clone(), rarity: None, kind: ItemKind::Healable(idx), max_stack: Some(h.max_stack) });
            }
        }
    }
    for (i, path) in fn_pose::loadout_inventory::EXTRA_ITEMS.iter().enumerate() {
        let kind = match i { 0 => ItemKind::Pickaxe, 1 => ItemKind::Resource(Resource::Wood), 2 => ItemKind::Resource(Resource::Stone), _ => ItemKind::Resource(Resource::Metal) };
        match set.as_deref().ok_or_else(|| "no pak set".to_owned()).and_then(|s| read_item(s, path)) {
            Ok(c) => items.push(ItemInfo { id: short(path), name: if c.name.is_empty() { short(path) } else { c.name }, rarity: Some(c.rarity), kind, max_stack: c.max_stack }),
            Err(e) => { if set.is_some() { notes.push(format!("{path}: {e}")); } items.push(ItemInfo { id: short(path), name: short(path), rarity: None, kind, max_stack: None }); }
        }
    }
    for i in &mut items { i.name = i.name.trim().to_owned(); } // some display names carry trailing spaces in the data
    Built { catalog: Catalog::new(items), icons, base_card, notes }
}

/// Refreshes the console's weapon list from the weapon runtime (display names / rarities arrive from its background read).
pub fn sync_weapons(built: &mut Built, w: &crate::weapon_runtime::WeaponRuntime) {
    built.catalog.weapons = w.catalog().iter().map(|i| fn_console::WeaponEntry { wid: i.wid.clone(), id: i.short.clone(), name: i.display_name.as_ref().map(|n| n.trim().to_owned()), rarity: i.rarity }).collect();
}

/// Sends the `give <weapon>` / `equip <weapon>` requests to the weapon runtime; returns the error lines (the runtime logs the rest through `take_events`).
pub fn weapon_requests(reqs: &[Request], w: &mut crate::weapon_runtime::WeaponRuntime) -> Vec<(LineKind, String)> {
    let mut out = vec![];
    for r in reqs {
        let res = match r { Request::GiveWeapon { wid, equip } => w.request_give(wid, *equip), Request::EquipWeapon { wid } => w.request_equip(wid), _ => continue };
        if let Err(e) = res { out.push((LineKind::Error, e)); }
    }
    out
}

pub fn vitals(arms: Option<&Arms>) -> fn_console::Vitals {
    arms.map_or_else(Default::default, |a| fn_console::Vitals { health: a.health, shield: a.shield, max_health: MAX_HEALTH, max_shield: MAX_SHIELD })
}

/// Applies the requests of one command to the live arms. Returns the log lines (what really happened, including what the sandbox cannot do).
pub fn apply(reqs: &[Request], arms: &mut Arms, cards: Option<&mut [Option<HudItem>; 6]>, built: &Built) -> Vec<(LineKind, String)> {
    let mut out = vec![];
    let _ = (cards, built); // INVENTORY HOOK: the HUD cards are derived from the live inventory (`InventoryUi::sync_cards`), the console only changes the inventory
    for r in reqs {
        match *r {
            Request::SetHealth(h) => arms.health = h.clamp(0., MAX_HEALTH),
            Request::SetShield(s) => arms.shield = s.clamp(0., MAX_SHIELD),
            Request::SelectPickaxe => match arms.quickbar.iter().position(|q| *q == Some(QuickbarItem::Pickaxe)) { Some(s) => arms.select(s), None => out.push((LineKind::Error, "the pickaxe is not on the quickbar".into())) },
            Request::GiveWeapon { .. } | Request::EquipWeapon { .. } => {} // handled by the app through `WeaponRuntime` (see `weapon_requests`)
            Request::SpawnPickup { .. } => {} // PICKUPS HOOK: handled by `pickups_hook::requests`
            Request::StartBus => {}
            Request::Win => crate::kill_feed_hook::set_won(true),
            Request::PlayEmote { ref query } => { if !crate::emote_hook::force_play(query, glam::Vec3::ZERO) { eprintln!("[emote] playemote: no emote item {query}"); } }
            Request::AddKillFeedMessage => crate::kill_feed_hook::add_cheat_messages(),
            Request::Die { ref cause } => match fn_ui::screens::kill_feed::DeathCause::parse(cause) { Some(c) => { crate::kill_feed_hook::force_local_death(c); } None => out.push((LineKind::Error, format!("unknown death cause {cause}"))) },
            Request::GiveGun { gun, select } => {
                // INVENTORY HOOK: a pickup through the inventory rules (slot / backpack / replace-the-focused-slot), not a bare slot lookup
                match arms.pickup_gun(gun, select) {
                    Ok(rep) => {
                        if let Some(r) = rep.refusal { out.push((LineKind::Error, r.text().to_owned())); continue; }
                        if let Some(old) = rep.replaced { out.push((LineKind::Info, format!("the backpack was full: {old:?} was dropped for the new firearm"))); }
                    }
                    Err(e) => { out.push((LineKind::Error, e)); continue; }
                }
                fill_reserve(arms, gun, None, &mut out);
            }
            Request::GiveAmmo { gun, count } => {
                let guns: Vec<usize> = match gun { Some(g) => vec![g], None => (0..arms.data.guns.len()).collect() };
                for g in guns { fill_reserve(arms, g, count, &mut out); }
            }
            Request::GiveHealable { idx, count, to_slot } => {
                // INVENTORY HOOK: stacking / slotting / backpack rules (`Arms::pickup_heal`); `to_slot` = false never swaps out a held item
                match arms.pickup_heal(idx, count, to_slot) {
                    Ok(rep) => {
                        let name = arms.data.healables[idx].name.clone();
                        if let Some(r) = rep.refusal { out.push((LineKind::Error, format!("{name}: {} ({} not picked up)", r.text(), rep.leftover))); }
                        if let Some(old) = rep.replaced { out.push((LineKind::Info, format!("the backpack was full: {old:?} was dropped for the {name}"))); }
                    }
                    Err(e) => out.push((LineKind::Error, e)),
                }
            }
        }
    }
    out
}

/// Reserve ammo of firearm `g`'s ammo item (shared by every firearm of that ammo type): +`count` or a full stack, capped at the item's MaxStackSize.
fn fill_reserve(arms: &mut Arms, g: usize, count: Option<u32>, out: &mut Vec<(LineKind, String)>) {
    let max = arms.data.ammo.max_stack.get(g).copied().unwrap_or(u32::MAX);
    if max == u32::MAX { if count.is_none() { out.push((LineKind::Error, format!("firearm {g}: its ammo item definition was not read, so there is no stack size to fill to (use ammo <gun> <count>)"))); } }
    let new = match count { Some(n) => arms.reserve[g].saturating_add(n).min(max), None => if max == u32::MAX { return } else { max } };
    let ammo = arms.data.guns[g].def.ammo.clone();
    for h in 0..arms.reserve.len() { if arms.data.guns[h].def.ammo == ammo { arms.reserve[h] = new; } }
}

/// Per-frame toggles: god holds health at the maximum; infinite ammo refills every magazine and reserve.
pub fn tick(flags: &Flags, arms: &mut Arms) {
    if flags.god { arms.health = MAX_HEALTH; }
    if flags.infinite_ammo {
        for g in 0..arms.data.guns.len() {
            arms.mags[g] = arms.data.guns[g].def.clip_size;
            let max = arms.data.ammo.max_stack.get(g).copied().unwrap_or(u32::MAX);
            if max != u32::MAX { arms.reserve[g] = max; }
        }
    }
}

impl Built {
    /// No loaded loadout (preview mode): world commands still work, item commands report that nothing can be given.
    pub fn empty() -> Built { Built { catalog: Catalog::default(), icons: vec![], base_card: None, notes: vec![] } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arms::{ArmsData, Policy};
    use fn_console::{Env, Key, PakCosmetics};

    /// The console against the REAL starter loadout (needs FN_TEST_ROOT; skipped otherwise): catalog names / rarities, give gun / consumable / ammo, vitals, god.
    #[test]
    fn console_drives_the_real_loadout() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT").map(std::path::PathBuf::from) else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let dir = crate::assets::find_paks_dir(&root).expect("paks dir");
        let set = fn_pak::PakSet::open_install(&dir).unwrap();
        let res = fn_pose::loadout::Resources::load(&fn_pose::ClipSourceDir::Pak(&set)).unwrap();
        let mut arms = Arms::new(ArmsData::from_resources(&res).unwrap(), Policy::default());
        let built = build(&arms, Some(HudItem::new(0, "T-Icon-MedBandage.png", Some(5))));
        eprintln!("catalog: {:?}\nnotes: {:?}", built.catalog.items.iter().map(|i| (&i.id, &i.name, i.rarity, i.max_stack)).collect::<Vec<_>>(), built.notes);
        assert_eq!(built.catalog.guns().count(), 3);
        assert!(built.catalog.items.iter().all(|i| !i.name.is_empty()));
        assert!(built.catalog.items.iter().filter(|i| matches!(i.kind, ItemKind::Healable(_))).count() == 5);
        let mut sim = fn_game::GameSim::new();
        let mut con = Console::new(); con.open();
        let mut cards: [Option<HudItem>; 6] = Default::default();
        cards[4] = Some(HudItem::new(0, "T-Icon-MedBandage.png", Some(5)));
        let mut run = |con: &mut Console, arms: &mut Arms, cards: &mut [Option<HudItem>; 6], line: &str| {
            for c in line.chars() { con.key(Key::Char(c), &mut Env { sim: &mut sim, catalog: &built.catalog, vitals: vitals(Some(arms)), cosmetics: &PakCosmetics }); }
            let fx = con.key(Key::Enter, &mut Env { sim: &mut sim, catalog: &built.catalog, vitals: vitals(Some(arms)), cosmetics: &PakCosmetics });
            let lines = apply(&fx.requests, arms, Some(cards), &built);
            (fx, lines)
        };
        // a firearm: refilled, selected
        arms.mags[1] = 0;
        let name = built.catalog.guns().nth(1).unwrap().name.clone();
        let (fx, lines) = run(&mut con, &mut arms, &mut cards, &format!("give {name}"));
        assert!(lines.is_empty(), "{lines:?} {:?}", fx.lines);
        assert_eq!(arms.mags[1], arms.data.guns[1].def.clip_size); assert_eq!(arms.selected, arms.quickbar.iter().position(|q| *q == Some(QuickbarItem::Gun(1))).unwrap());
        // vitals
        arms.health = 20.;
        run(&mut con, &mut arms, &mut cards, "heal"); assert_eq!(arms.health, MAX_HEALTH);
        run(&mut con, &mut arms, &mut cards, "shield 30"); assert_eq!(arms.shield, 30.);
        run(&mut con, &mut arms, &mut cards, "sethealthpercent 25"); assert_eq!(arms.health, 25.);
        run(&mut con, &mut arms, &mut cards, "god"); arms.health = 1.; tick(&con.flags, &mut arms); assert_eq!(arms.health, MAX_HEALTH);
        // a consumable into the consumable slot, with its own HUD card when the icon is readable
        let (_, lines) = run(&mut con, &mut arms, &mut cards, "give med kit 2");
        eprintln!("medkit lines {lines:?}");
        assert_eq!(arms.stock[1], 2.min(arms.data.healables[1].max_stack.max(1)));
        if lines.is_empty() { assert_eq!(arms.slot_item(5), Some(crate::arms::inventory_ops::Item::Heal { idx: 1, count: arms.stock[1] }), "a new stack goes to the first free slot (the backpack had one free entry)"); }
        let _ = &cards;
        // ammo
        arms.reserve = vec![0; 3];
        run(&mut con, &mut arms, &mut cards, "ammo");
        eprintln!("reserve after ammo: {:?} (max {:?})", arms.reserve, arms.data.ammo.max_stack);
        run(&mut con, &mut arms, &mut cards, "infiniteammo on"); arms.mags = vec![0; 3]; tick(&con.flags, &mut arms); assert_eq!(arms.mags[0], arms.data.guns[0].def.clip_size);
        // resources go straight to the sim
        run(&mut con, &mut arms, &mut cards, "givewood 120"); assert_eq!(sim.world.wood, 120);
    }
}
