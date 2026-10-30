//! Parse + dispatch. Commands that act on the world (movement tuning, teleport, resources, cosmetics) run directly on the [`GameSim`]; commands that act on the
//! player's inventory / vitals (which live in the front-end's `Arms`) come back as [`Request`]s for the front-end to apply, so this module stays UI- and
//! window-free and every command is unit-testable on a headless sim.
use crate::catalog::{rarity_name, Catalog, ItemInfo, ItemKind, Resource};
use crate::registry::{self, Command, COMMANDS, LIST_TOPICS};
use fn_game::cosmetic_swap::{self, CosmeticEntry};
pub use fn_game::cosmetic_swap::Kind as CosmeticKind;
use fn_game::GameSim;
use fn_game::movement::cmc::Params;
use fn_game::sim::{EYE, Z_OFFSET};
use glam::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind { Input, Ok, Error, Info }

/// Where the cosmetics come from (the 1.8 paks via `fn_game::cosmetic_swap`; a fake in tests).
pub trait CosmeticSource {
    fn list(&self, kind: CosmeticKind) -> Vec<CosmeticEntry>;
    fn set(&self, sim: &mut GameSim, kind: CosmeticKind, query: &str) -> Result<String, String>;
}
/// The real source: every BR cosmetic definition of the paks, selected through `fn_game::cosmetic_swap`.
pub struct PakCosmetics;
impl CosmeticSource for PakCosmetics {
    fn list(&self, kind: CosmeticKind) -> Vec<CosmeticEntry> {
        match kind { CosmeticKind::Skin => cosmetic_swap::list_skins(), CosmeticKind::Pickaxe => cosmetic_swap::list_pickaxes(), CosmeticKind::Backbling => cosmetic_swap::list_backblings(), CosmeticKind::Glider => cosmetic_swap::list_gliders() }
    }
    fn set(&self, sim: &mut GameSim, kind: CosmeticKind, query: &str) -> Result<String, String> {
        match kind { CosmeticKind::Skin => cosmetic_swap::set_skin(sim, query), CosmeticKind::Pickaxe => cosmetic_swap::set_pickaxe(sim, query), CosmeticKind::Backbling => cosmetic_swap::set_backbling(sim, query), CosmeticKind::Glider => cosmetic_swap::set_glider(sim, query) }
    }
}

/// Health / shield as the front-end's `Arms` holds them (read-only for the commands; changes come back as requests).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vitals { pub health: f32, pub shield: f32, pub max_health: f32, pub max_shield: f32 }
impl Default for Vitals { fn default() -> Self { Vitals { health: 100., shield: 0., max_health: 100., max_shield: 100. } } }

/// Toggles that live as long as the console: the front-end applies them every frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags { pub god: bool, pub infinite_ammo: bool }

/// What the commands operate on.
pub struct Env<'a> { pub sim: &'a mut GameSim, pub catalog: &'a Catalog, pub vitals: Vitals, pub cosmetics: &'a dyn CosmeticSource }

/// Player-state changes the front-end applies to its live `Arms` / HUD.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    SetHealth(f32),
    SetShield(f32),
    /// Firearm `gun` (starter loadout index): put it in the quickbar if it is not there, refill its magazine and reserve, select it when `select`.
    GiveGun { gun: usize, select: bool },
    /// +`count` of healing consumable `idx` (capped at its MaxStackSize); `to_slot` makes it the consumable of the quickbar's consumable slot.
    GiveHealable { idx: usize, count: u32, to_slot: bool },
    /// Ammo of firearm `gun` (None = every firearm): +`count` rounds, or a full stack when None.
    GiveAmmo { gun: Option<usize>, count: Option<u32> },
    SelectPickaxe,
    /// Any BR weapon through the front-end's weapon runtime (`WeaponRuntime::request_give`): `wid` is the WID package path, `equip` selects it once loaded.
    GiveWeapon { wid: String, equip: bool },
    /// `WeaponRuntime::request_equip`: select the weapon (given first when it is not on the quickbar).
    EquipWeapon { wid: String },
    /// `spawn <item> [count]`: drop a real, pickupable world item (`AFortPickupAthena`) in front of the player. `query` = asset name / display name; the front-end resolves it (every BR item definition, not only the catalog) and reports errors.
    SpawnPickup { query: String, count: u32 },
    AddKillFeedMessage,
    Die { cause: String },
    StartBus,
    Win,
    PlayEmote { query: String },
}

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Effects { pub lines: Vec<(LineKind, String)>, pub requests: Vec<Request>, pub clear: bool }
impl Effects {
    fn say(&mut self, k: LineKind, s: impl Into<String>) { self.lines.push((k, s.into())); }
    fn ok(&mut self, s: impl Into<String>) { self.say(LineKind::Ok, s) }
    fn err(&mut self, s: impl Into<String>) { self.say(LineKind::Error, s) }
    fn info(&mut self, s: impl Into<String>) { self.say(LineKind::Info, s) }
}

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// parsing

/// Whitespace separated words; `"double quotes"` keep spaces inside one word.
pub fn tokenize(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut any) = (vec![], String::new(), false, false);
    for c in line.chars() {
        match c {
            '"' => { quoted = !quoted; any = true; }
            c if c.is_whitespace() && !quoted => { if any { out.push(std::mem::take(&mut cur)); any = false; } }
            c => { cur.push(c); any = true; }
        }
    }
    if any { out.push(cur); }
    out
}

/// `(command word, argument words)` of a line.
pub fn parse(line: &str) -> Option<(String, Vec<String>)> {
    let mut t = tokenize(line);
    if t.is_empty() { return None; }
    let cmd = t.remove(0);
    Some((cmd, t))
}

/// A trailing integer word is the count; everything before it is the item query (`give pump shotgun 2`).
pub fn split_count(args: &[String]) -> (String, Option<u32>) {
    if args.len() >= 2 { if let Ok(n) = args[args.len() - 1].parse::<u32>() { return (args[..args.len() - 1].join(" "), Some(n)); } }
    (args.join(" "), None)
}

fn num(s: &str) -> Option<f32> { s.trim().parse::<f32>().ok().filter(|v| v.is_finite()) }
fn on_off(args: &[String], current: bool) -> Result<bool, String> {
    match args.first().map(|s| s.to_ascii_lowercase()).as_deref() { None => Ok(!current), Some("on" | "1" | "true") => Ok(true), Some("off" | "0" | "false") => Ok(false), Some(o) => Err(format!("expected on or off, got '{o}'")) }
}

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// dispatch

fn usage_err(fx: &mut Effects, c: &Command) { fx.err(format!("usage: {} {}", c.name, c.usage).trim_end().to_owned()); }

pub fn execute(line: &str, env: &mut Env, flags: &mut Flags) -> Effects {
    let mut fx = Effects::default();
    let Some((word, args)) = parse(line) else { return fx };
    let Some(c) = registry::find(&word) else {
        let w = word.to_ascii_lowercase();
        let near: Vec<&str> = COMMANDS.iter().filter(|c| c.name.starts_with(&w) || c.name.contains(&w)).map(|c| c.name).collect();
        if near.is_empty() { fx.err(format!("unknown command '{word}' (help lists the commands)")); } else { fx.err(format!("unknown command '{word}', did you mean: {}", near.join(", "))); }
        return fx;
    };
    match c.name {
        "help" => help(&mut fx, &args),
        "clear" => fx.clear = true,
        "list" => match args.first() { Some(t) => list(&mut fx, env, t, &args[1..].join(" ")), None => usage_err(&mut fx, c) },
        "listweapons" => list(&mut fx, env, "weapons", ""),
        "give" => if !give_weapon(&mut fx, env, c, &args) { give(&mut fx, env, c, &args) },
        "equip" => equip_weapon(&mut fx, env, c, &args),
        "spawn" => spawn_pickup(&mut fx, c, &args), // PICKUPS HOOK (pickups_hook.rs)
        "ammo" => ammo(&mut fx, env, c, &args),
        "giveathenaweapons" => give_weapons(&mut fx, env),
        "giveathenaconsumables" => give_consumables(&mut fx, env),
        "giveathenaammo" => { fx.requests.push(Request::GiveAmmo { gun: None, count: None }); fx.ok("a full ammo stack for every firearm"); }
        "giveathenafullkit" => { give_weapons(&mut fx, env); give_consumables(&mut fx, env); fx.requests.push(Request::GiveAmmo { gun: None, count: None }); fx.ok("a full ammo stack for every firearm"); }
        "givewood" | "givestone" | "givemetal" | "giveresources" => resources(&mut fx, env, c, &args),
        "heal" | "shield" => vital(&mut fx, env, c, &args),
        "sethealthpercent" => match args.first().and_then(|a| num(a)).filter(|p| (0. ..=100.).contains(p)) {
            Some(p) => { let h = env.vitals.max_health * p / 100.; fx.requests.push(Request::SetHealth(h)); fx.ok(format!("health -> {h:.0} ({p:.0} % of {:.0})", env.vitals.max_health)); }
            None => usage_err(&mut fx, c),
        },
        "god" => match on_off(&args, flags.god) { Ok(v) => { flags.god = v; fx.ok(format!("god mode {} (health held at the maximum)", if v { "on" } else { "off" })); } Err(e) => fx.err(e) },
        "infiniteammo" => match on_off(&args, flags.infinite_ammo) { Ok(v) => { flags.infinite_ammo = v; fx.ok(format!("infinite ammo {}", if v { "on" } else { "off" })); } Err(e) => fx.err(e) },
        "tp" => tp(&mut fx, env, c, &args),
        "face" => { let v: Vec<f32> = args.iter().filter_map(|a| num(a)).collect(); if v.is_empty() { usage_err(&mut fx, c); } else { env.sim.yaw = v[0]; if let Some(p) = v.get(1) { env.sim.pitch = *p; } fx.ok(format!("view yaw {:.1} pitch {:.1}", env.sim.yaw, env.sim.pitch)); } }
        "pos" => pos(&mut fx, env.sim),
        "respawn" => { env.sim.respawn(); fx.ok("back at the spawn (fresh movement controller)"); }
        "speed" => speed(&mut fx, env.sim, c, &args),
        "storm" => storm(&mut fx, env.sim, c, &args),
        "gravity" => tuning(&mut fx, env.sim, c, &args, true),
        "jump" => tuning(&mut fx, env.sim, c, &args, false),
        "skin" | "pickaxe" | "backbling" | "glider" => {
            let kind = match c.name { "skin" => CosmeticKind::Skin, "pickaxe" => CosmeticKind::Pickaxe, "backbling" => CosmeticKind::Backbling, _ => CosmeticKind::Glider };
            let q = args.join(" ");
            if q.trim().is_empty() { usage_err(&mut fx, c); } else { match env.cosmetics.set(env.sim, kind, &q) { Ok(m) => fx.ok(m), Err(e) => fx.err(e) } }
        }
        "fly" => { env.sim.fly = fn_game::sim::FlyMode::Fly; fx.ok("flying: WASD along the view, jump = up, crouch = down, sprint = fast (walk to land)"); }
        "ghost" | "noclip" => { env.sim.fly = fn_game::sim::FlyMode::Ghost; fx.ok("ghost: flying through everything (walk to land)"); }
        "walk" => { env.sim.fly = fn_game::sim::FlyMode::Walk; fx.ok("walking (falls from where you are)"); }
        "addkillfeedmessage" => { fx.requests.push(Request::AddKillFeedMessage); fx.ok("five test lines added to the elimination feed"); }
        "startbus" => { fx.ok("a fresh Battle Bus takes off".to_owned()); fx.requests.push(Request::StartBus); }
        "playemote" => { let q = args.join(" "); if q.is_empty() { fx.err("usage: playemote <emote>"); } else { fx.ok(format!("ServerPlayEmoteItem({q})")); fx.requests.push(Request::PlayEmote { query: q }); } }
        "win" => { fx.ok("Victory Royale".to_owned()); fx.requests.push(Request::Win); }
        "die" => match args.first().map(|a| a.to_ascii_lowercase()).filter(|a| registry::DIE_ARGS.contains(&a.as_str())) { Some(a) => { fx.ok(format!("eliminated ({a})")); fx.requests.push(Request::Die { cause: a }); } None => usage_err(&mut fx, c) },
        other => fx.err(format!("'{other}' is registered but has no handler")),
    }
    fx
}

fn help(fx: &mut Effects, args: &[String]) {
    match args.first() {
        None => {
            fx.info("commands (Tab completes, Up / Down = history, PageUp / PageDown = scroll, Esc / ~ closes; help <command> for details):");
            for c in COMMANDS { fx.info(format!("  {} {}  - {}", c.name, c.usage, c.help).replace("  -", " -")); }
        }
        Some(n) => match registry::find(n) {
            Some(c) => {
                fx.info(format!("{} {}", c.name, c.usage).trim_end().to_owned());
                fx.info(format!("  {}", c.help));
                if !c.aliases.is_empty() { fx.info(format!("  aliases: {}", c.aliases.join(", "))); }
                if !c.cite.is_empty() { fx.info(format!("  1.8 counterpart: {}", c.cite)); }
            }
            None => fx.err(format!("no command '{n}'")),
        },
    }
}

fn show(fx: &mut Effects, rows: Vec<String>, what: &str, filter: &str) {
    const MAX: usize = 40;
    if rows.is_empty() { fx.err(format!("no {what}{}", if filter.is_empty() { String::new() } else { format!(" matching '{filter}'") })); return; }
    fx.info(format!("{} {what}{}:", rows.len(), if filter.is_empty() { String::new() } else { format!(" matching '{filter}'") }));
    for r in rows.iter().take(MAX) { fx.info(format!("  {r}")); }
    if rows.len() > MAX { fx.info(format!("  ... {} more (add a filter: list {what} <text>)", rows.len() - MAX)); }
}

fn list(fx: &mut Effects, env: &Env, topic: &str, filter: &str) {
    let t = topic.to_ascii_lowercase();
    let f = crate::catalog::norm(filter);
    let cosmetic = |kind: CosmeticKind| -> Vec<String> {
        env.cosmetics.list(kind).into_iter().filter(|e| f.is_empty() || crate::catalog::norm(&e.id).contains(&f) || crate::catalog::norm(&e.display_name).contains(&f))
            .map(|e| format!("{}  {} ({})", e.id, e.display_name, e.rarity)).collect()
    };
    let items = |pick: &dyn Fn(&ItemInfo) -> bool| -> Vec<String> {
        env.catalog.items.iter().filter(|i| pick(i)).filter(|i| f.is_empty() || Catalog::rank(i, &f).is_some()).map(|i| format!("{}  {}", i.id, Catalog::describe(i))).collect()
    };
    match t.as_str() {
        "skins" | "skin" | "outfits" => show(fx, cosmetic(CosmeticKind::Skin), "skins", filter),
        "pickaxes" | "pickaxe" => show(fx, cosmetic(CosmeticKind::Pickaxe), "pickaxes", filter),
        "backblings" | "backbling" => show(fx, cosmetic(CosmeticKind::Backbling), "backblings", filter),
        "gliders" | "glider" => show(fx, cosmetic(CosmeticKind::Glider), "gliders", filter),
        "weapons" | "guns" if !env.catalog.weapons.is_empty() => {
            let rows = env.catalog.weapon_search(filter).into_iter().map(|w| format!("{}  {}", w.id, w.describe())).collect();
            show(fx, rows, "weapons", filter);
        }
        "weapons" | "guns" => show(fx, items(&|i| matches!(i.kind, ItemKind::Gun(_))), "weapons", filter),
        "consumables" | "healing" => show(fx, items(&|i| matches!(i.kind, ItemKind::Healable(_))), "consumables", filter),
        "resources" => show(fx, items(&|i| matches!(i.kind, ItemKind::Resource(_))), "resources", filter),
        "items" => show(fx, items(&|_| true), "items", filter),
        "commands" => show(fx, COMMANDS.iter().map(|c| c.name.to_owned()).collect(), "commands", filter),
        _ => fx.err(format!("unknown list '{topic}': {}", LIST_TOPICS.join(", "))),
    }
}

/// `give <weapon> [count] [equip]` for the weapon runtime's catalog (any BR weapon by display name / WID). Returns false when the command is not about a runtime weapon (the item path handles it).
fn give_weapon(fx: &mut Effects, env: &Env, _c: &Command, args: &[String]) -> bool {
    if env.catalog.weapons.is_empty() || args.is_empty() { return false; }
    let equip = args.len() >= 2 && args[args.len() - 1].eq_ignore_ascii_case("equip");
    let args = if equip { &args[..args.len() - 1] } else { args };
    let (q, count) = split_count(args);
    // consumables / resources / the pickaxe keep their own path
    if env.catalog.resolve(&q, |_| true).is_ok_and(|i| !matches!(i.kind, ItemKind::Gun(_))) { return false; }
    match env.catalog.weapon_resolve(&q) {
        Ok(w) => {
            if count == Some(0) { fx.err("count must be at least 1"); return true; }
            fx.requests.push(Request::GiveWeapon { wid: w.wid.clone(), equip });
            fx.ok(format!("{} [{}]: loading, then on the quickbar with a full magazine{}{}", w.name.as_deref().unwrap_or(&w.id), w.rarity.map(rarity_name).unwrap_or("no rarity"), if equip { ", equipped" } else { " (add `equip` to select it)" }, if count.unwrap_or(1) > 1 { " (count ignored: one weapon per quickbar slot)" } else { "" }));
            true
        }
        // nothing named: a starter item by a looser name still works through the item path
        Err(_) if env.catalog.weapon_matches(&q).is_empty() && env.catalog.resolve(&q, |_| true).is_ok() => false,
        Err(e) => { fx.err(e); true }
    }
}

fn equip_weapon(fx: &mut Effects, env: &Env, c: &Command, args: &[String]) {
    if args.is_empty() { usage_err(fx, c); return; }
    if env.catalog.weapons.is_empty() { fx.err("the weapon catalog is not available (no live loadout)"); return; }
    match env.catalog.weapon_resolve(&args.join(" ")) {
        Ok(w) => { fx.requests.push(Request::EquipWeapon { wid: w.wid.clone() }); fx.ok(format!("{} [{}]: equipping (given first when it is not on the quickbar)", w.name.as_deref().unwrap_or(&w.id), w.rarity.map(rarity_name).unwrap_or("no rarity"))); }
        Err(e) => fx.err(e),
    }
}

/// `spawn <item> [count]` (PICKUPS HOOK): the request goes to the front-end, which owns the pickup actors.
fn spawn_pickup(fx: &mut Effects, c: &Command, args: &[String]) {
    if args.is_empty() { usage_err(fx, c); return; }
    let (q, count) = split_count(args);
    if count == Some(0) { fx.err("count must be at least 1"); return; }
    fx.requests.push(Request::SpawnPickup { query: q.clone(), count: count.unwrap_or(1) });
    fx.info(format!("spawn {q}: looking the item up, it drops in front of the player"));
}

fn give(fx: &mut Effects, env: &mut Env, c: &Command, args: &[String]) {
    if args.is_empty() { usage_err(fx, c); return; }
    let (q, count) = split_count(args);
    if count == Some(0) { fx.err("count must be at least 1"); return; }
    let item = match env.catalog.resolve(&q, |_| true) { Ok(i) => i.clone(), Err(e) => { fx.err(e); return; } };
    let tag = format!("{} [{}]", item.name, item.rarity.map(rarity_name).unwrap_or("no rarity"));
    let via = if c.name == "spawn" { " (no pickup actors in this sandbox: added directly)" } else { "" };
    match item.kind {
        ItemKind::Gun(gun) => {
            fx.requests.push(Request::GiveGun { gun, select: true });
            fx.ok(format!("{tag}: in the quickbar, magazine and reserve refilled, selected{via}{}", if count.unwrap_or(1) > 1 { " (count ignored: one firearm per quickbar slot here)" } else { "" }));
        }
        ItemKind::Healable(idx) => { let n = count.unwrap_or(1); fx.requests.push(Request::GiveHealable { idx, count: n, to_slot: true }); fx.ok(format!("{n} x {tag}: in the consumable slot (capped at the item's stack size){via}")); }
        ItemKind::Pickaxe => { fx.requests.push(Request::SelectPickaxe); fx.ok(format!("{tag}: selected (always in slot 1){via}")); }
        ItemKind::Resource(r) => { let n = count.unwrap_or(1); let t = add_resource(env.sim, r, n); fx.ok(format!("{n} x {tag}: now {t}{via}")); }
    }
}

fn resource_ref(sim: &mut GameSim, r: Resource) -> &mut u32 { match r { Resource::Wood => &mut sim.world.wood, Resource::Stone => &mut sim.world.stone, Resource::Metal => &mut sim.world.metal } }
/// HUD pack counters show at most 999 (`fn_hud_adapter::feedback::clamp_resource`).
const RESOURCE_CAP: u32 = 999;
fn add_resource(sim: &mut GameSim, r: Resource, n: u32) -> u32 { let v = resource_ref(sim, r); *v = v.saturating_add(n).min(RESOURCE_CAP); *v }

fn resources(fx: &mut Effects, env: &mut Env, c: &Command, args: &[String]) {
    let Some(n) = args.first().and_then(|a| a.parse::<u32>().ok()) else { usage_err(fx, c); return };
    let which: &[(Resource, &str)] = match c.name { "givewood" => &[(Resource::Wood, "wood")], "givestone" => &[(Resource::Stone, "stone")], "givemetal" => &[(Resource::Metal, "metal")], _ => &[(Resource::Wood, "wood"), (Resource::Stone, "stone"), (Resource::Metal, "metal")] };
    let parts: Vec<String> = which.iter().map(|(r, name)| format!("{name} {}", add_resource(env.sim, *r, n))).collect();
    fx.ok(format!("+{n}: {} (capped at {RESOURCE_CAP})", parts.join(", ")));
}

fn ammo(fx: &mut Effects, env: &mut Env, _c: &Command, args: &[String]) {
    if args.is_empty() { fx.requests.push(Request::GiveAmmo { gun: None, count: None }); fx.ok("a full ammo stack for every firearm"); return; }
    let (q, count) = if args.len() == 1 && args[0].parse::<u32>().is_ok() { (String::new(), args[0].parse::<u32>().ok()) } else { split_count(args) };
    if count == Some(0) { fx.err("count must be at least 1"); return; }
    let gun = if q.is_empty() { None } else {
        match env.catalog.resolve(&q, |i| matches!(i.kind, ItemKind::Gun(_))) { Ok(ItemInfo { kind: ItemKind::Gun(g), .. }) => Some(*g), Ok(_) => None, Err(e) => { fx.err(e); return; } }
    };
    fx.requests.push(Request::GiveAmmo { gun, count });
    fx.ok(format!("ammo for {}: {}", if q.is_empty() { "every firearm".to_owned() } else { q }, count.map(|n| format!("+{n} rounds (capped at the ammo item's stack size)")).unwrap_or_else(|| "a full stack".into())));
}

fn give_weapons(fx: &mut Effects, env: &Env) {
    let guns: Vec<usize> = env.catalog.guns().filter_map(|i| if let ItemKind::Gun(g) = i.kind { Some(g) } else { None }).collect();
    for g in &guns { fx.requests.push(Request::GiveGun { gun: *g, select: false }); }
    fx.ok(format!("{} firearms refilled (magazine + reserve)", guns.len()));
}
fn give_consumables(fx: &mut Effects, env: &Env) {
    let mut n = 0;
    for i in env.catalog.items.iter() { if let ItemKind::Healable(idx) = i.kind { fx.requests.push(Request::GiveHealable { idx, count: i.max_stack.unwrap_or(1), to_slot: false }); n += 1; } }
    fx.ok(format!("{n} consumables at a full stack (the consumable slot keeps showing the one it had; give <name> puts another in it)"));
}

fn vital(fx: &mut Effects, env: &Env, c: &Command, args: &[String]) {
    let v = env.vitals;
    let (cur, max, name) = if c.name == "heal" { (v.health, v.max_health, "health") } else { (v.shield, v.max_shield, "shield") };
    let new = match args.first() { None => max, Some(a) => match num(a) { Some(x) => (cur + x).clamp(0., max), None => { usage_err(fx, c); return } } };
    fx.requests.push(if c.name == "heal" { Request::SetHealth(new) } else { Request::SetShield(new) });
    fx.ok(format!("{name} {cur:.0} -> {new:.0}"));
}

fn tp(fx: &mut Effects, env: &mut Env, c: &Command, args: &[String]) {
    if args.len() == 1 && args[0].eq_ignore_ascii_case("spawn") { env.sim.respawn(); fx.ok("back at the spawn"); return; }
    let v: Vec<f32> = args.iter().filter_map(|a| num(a)).collect();
    if args.len() != 3 || v.len() != 3 { usage_err(fx, c); return; }
    // TERRAIN: x / z clamped to the world box (the landscape bounds minus fn_game::EDGE_MARGIN, or the plate); y = metres above the ground there
    let s = &mut *env.sim;
    let (lo, hi) = s.world_box();
    let (x, z) = (v[0].clamp(lo.x, hi.x), v[2].clamp(lo.y, hi.y));
    let up = v[1].max(0.);
    let y = s.ground_y(x, z) + up;
    s.controller.position = Vec2::new(x, z - Z_OFFSET);
    s.controller.velocity = Vec2::ZERO; s.controller.vertical_speed = 0.;
    s.controller.height = y;
    s.x = x; s.z = z; s.y = EYE + y;
    let clamped = (x, up, z) != (v[0], v[1], v[2]);
    fx.ok(format!("teleported to x {x:.2} y {y:.2} z {z:.2} ({up:.2} m above the ground){}", if clamped { format!(" (clamped: the world is x {:.0}..{:.0}, z {:.0}..{:.0} m and the height above the ground >= 0)", lo.x, hi.x, lo.y, hi.y) } else { String::new() }));
}

fn pos(fx: &mut Effects, s: &GameSim) {
    let f = s.feet();
    let (lo, hi) = s.world_box();
    fx.info(format!("feet x {:.2} y {:.2} z {:.2}, ground y {:.2} (metres, y up; the world is x {:.0}..{:.0}, z {:.0}..{:.0})", f.x, f.y, f.z, s.ground_y(f.x, f.z), lo.x, hi.x, lo.y, hi.y));
    fx.info(format!("camera eye {:.3} {:.3} {:.3} forward {:.4} {:.4} {:.4}", s.cam_view.eye.x, s.cam_view.eye.y, s.cam_view.eye.z, s.cam_view.forward.x, s.cam_view.forward.y, s.cam_view.forward.z));
    fx.info(format!("yaw {:.1} pitch {:.1}  velocity {:.2} {:.2} (m/s)  vertical {:.2}  {}", s.yaw, s.pitch, s.controller.velocity.x, s.controller.velocity.y, s.controller.vertical_speed, if s.controller.airborne() { "airborne" } else { "grounded" }));
}

/// `storm`: the static safe zone the storm visuals draw (`fn_game::storm_state`). A number is the radius in metres; `on` / `off` / `reset`.
fn storm(fx: &mut Effects, s: &mut GameSim, c: &Command, args: &[String]) {
    let st = &mut s.storm;
    let Some(a) = args.first() else {
        fx.info(format!("storm {}: radius {:.0} m, centre UE ({:.0}, {:.0}, {:.0}) cm, opacity {:.2} (first phase = 1500 m, `storm <metres>` sets it)", if st.enabled { "on" } else { "off" }, st.radius_m(), st.center_cm[0], st.center_cm[1], st.center_cm[2], st.master_opacity()));
        return;
    };
    match a.to_ascii_lowercase().as_str() {
        "on" => { st.turn_on(); fx.ok("storm on (the 25 s StormFadeIn timeline restarts from 0 if it was off)"); }
        "off" => { st.turn_off(); fx.ok("storm off"); }
        "reset" => { st.reset_zone(); fx.ok(format!("zone reset to the first phase: radius {:.0} m", st.radius_m())); }
        "here" => { st.here_requested = true; st.turn_on(); fx.ok(format!("zone centred on the player (radius {:.0} m)", st.radius_m())); }
        other => match num(other).filter(|v| *v >= 1. && *v <= 100_000.) {
            Some(m) => { st.set_radius_m(m); st.turn_on(); fx.ok(format!("storm radius {m:.0} m")); }
            None => { usage_err(fx, c); fx.err("the radius is a number of metres from 1 to 100000"); }
        },
    }
}

fn speed(fx: &mut Effects, s: &mut GameSim, c: &Command, args: &[String]) {
    let Some(a) = args.first() else { fx.info(format!("speed scale {} (run {:.0}, sprint {:.0} cm/s at 1)", s.speed_scale, Params::default().speed_run, Params::default().speed_sprint)); return };
    let v = if a.eq_ignore_ascii_case("reset") { Some(1.) } else { num(a) };
    match v.filter(|v| *v > 0. && *v <= 50.) {
        Some(v) => { s.speed_scale = v; fx.ok(format!("speed x{v} (applied to run, sprint, crouched run, crouched sprint; the targeting limit still caps it per tick)")); }
        None => { usage_err(fx, c); fx.err("the scale must be above 0 and at most 50"); }
    }
}

/// `gravity` (true) / `jump` (false): the two `Tuning` knobs, written to the live movement component too.
fn tuning(fx: &mut Effects, s: &mut GameSim, c: &Command, args: &[String], gravity: bool) {
    let d = Params::default();
    let cur = if gravity { s.controller.component().p.gravity_scale } else { s.controller.component().p.jump_z_velocity };
    let Some(a) = args.first() else { fx.info(format!("{} {cur} (recovered {})", if gravity { "gravity scale" } else { "jump velocity cm/s" }, if gravity { d.gravity_scale } else { d.jump_z_velocity })); return };
    let reset = a.eq_ignore_ascii_case("reset");
    let v = if reset { Some(if gravity { d.gravity_scale } else { d.jump_z_velocity }) } else { num(a).filter(|v| *v > 0.) };
    let Some(v) = v else { usage_err(fx, c); fx.err("must be a number above 0 (Tuning ignores values <= 0)"); return };
    if gravity { s.tuning.gravity_scale = if reset { None } else { Some(v) }; s.controller.component_mut().p.gravity_scale = v; }
    else { s.tuning.jump_z_velocity = if reset { None } else { Some(v) }; s.controller.component_mut().p.jump_z_velocity = v; }
    fx.ok(format!("{} {cur} -> {v}{}", if gravity { "gravity scale" } else { "jump velocity" }, if reset { " (recovered value)" } else { "" }));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::catalog::test_catalog;

    pub struct FakeCosmetics;
    impl CosmeticSource for FakeCosmetics {
        fn list(&self, kind: CosmeticKind) -> Vec<CosmeticEntry> {
            let e = |id: &str, n: &str, r: &str| CosmeticEntry { id: id.into(), display_name: n.into(), rarity: r.into() };
            match kind {
                CosmeticKind::Skin => vec![e("CID_001", "Alpha Skin", "Rare"), e("CID_002", "Beta Skin", "Epic"), e("CID_003", "Alphabet Skin", "Common")],
                CosmeticKind::Pickaxe => vec![e("Pickaxe_ID_001", "Axe One", "Common")],
                _ => vec![],
            }
        }
        fn set(&self, _sim: &mut GameSim, kind: CosmeticKind, query: &str) -> Result<String, String> {
            let list = self.list(kind);
            cosmetic_swap_resolve(&list, query).map(|e| format!("{} -> {}", kind.name(), e.display_name))
        }
    }
    fn cosmetic_swap_resolve<'a>(list: &'a [CosmeticEntry], q: &str) -> Result<&'a CosmeticEntry, String> {
        let q = q.to_lowercase();
        let hits: Vec<_> = list.iter().filter(|e| e.id.to_lowercase() == q || e.display_name.to_lowercase() == q).collect();
        if hits.len() == 1 { Ok(hits[0]) } else { Err(format!("no cosmetic matches '{q}'")) }
    }

    pub fn run(sim: &mut GameSim, cat: &Catalog, flags: &mut Flags, line: &str) -> Effects {
        execute(line, &mut Env { sim, catalog: cat, vitals: Vitals { health: 40., shield: 10., ..Default::default() }, cosmetics: &FakeCosmetics }, flags)
    }
    fn text(fx: &Effects) -> String { fx.lines.iter().map(|l| l.1.clone()).collect::<Vec<_>>().join("\n") }

    #[test] fn tokenizer_handles_quotes_and_counts() {
        assert_eq!(tokenize("give \"pump shotgun\" 2"), ["give", "pump shotgun", "2"]);
        assert_eq!(tokenize("  a   b "), ["a", "b"]); assert!(tokenize("   ").is_empty()); assert_eq!(tokenize("skin \"\""), ["skin", ""]);
        assert_eq!(split_count(&["pump".into(), "shotgun".into(), "3".into()]), ("pump shotgun".into(), Some(3)));
        assert_eq!(split_count(&["3".into()]), ("3".into(), None), "a lone number is the item query, not a count");
        assert_eq!(parse("  heal 5").unwrap(), ("heal".to_owned(), vec!["5".to_owned()]));
    }
    #[test] fn give_and_equip_any_br_weapon_through_the_runtime() {
        let (mut sim, cat, mut fl) = (GameSim::new(), crate::catalog::test_weapons(), Flags::default());
        let wid = |id: &str| format!("/Game/Athena/Items/Weapons/{id}");
        let fx = run(&mut sim, &cat, &mut fl, "give rocket launcher");
        assert_eq!(fx.requests, [Request::GiveWeapon { wid: wid("WID_Launcher_Rocket_Athena_R_Ore_T03"), equip: false }], "{}", text(&fx));
        let fx = run(&mut sim, &cat, &mut fl, "give WID_Pistol_SemiAuto_Athena_C_Ore_T02 equip");
        assert_eq!(fx.requests, [Request::GiveWeapon { wid: wid("WID_Pistol_SemiAuto_Athena_C_Ore_T02"), equip: true }]);
        let fx = run(&mut sim, &cat, &mut fl, "give \"Rocket Launcher\" 3 EQUIP"); assert!(matches!(fx.requests[..], [Request::GiveWeapon { equip: true, .. }]));
        let fx = run(&mut sim, &cat, &mut fl, "give Assault Rifle");
        assert!(fx.requests.is_empty() && fx.lines[0].0 == LineKind::Error && text(&fx).contains("matches 3 weapons") && text(&fx).contains("_UC_"), "{}", text(&fx));
        let fx = run(&mut sim, &cat, &mut fl, "give assault_auto_athena_uc_ore_t03"); assert_eq!(fx.requests.len(), 1);
        assert_eq!(run(&mut sim, &cat, &mut fl, "give bazooka").lines[0].0, LineKind::Error);
        // consumables keep their own path
        assert_eq!(run(&mut sim, &cat, &mut fl, "give med kit 2").requests, [Request::GiveHealable { idx: 1, count: 2, to_slot: true }]);
        let fx = run(&mut sim, &cat, &mut fl, "equip rocket launcher"); assert_eq!(fx.requests, [Request::EquipWeapon { wid: wid("WID_Launcher_Rocket_Athena_R_Ore_T03") }]);
        assert_eq!(run(&mut sim, &cat, &mut fl, "equip").lines[0].0, LineKind::Error);
        assert_eq!(run(&mut sim, &cat, &mut fl, "equip assault rifle").lines[0].0, LineKind::Error);
        let l = text(&run(&mut sim, &cat, &mut fl, "list weapons")); assert!(l.contains("5 weapons") && l.contains("Rocket Launcher (Rare)"), "{l}");
        assert!(text(&run(&mut sim, &cat, &mut fl, "list weapons rocket")).contains("1 weapons matching"));
        // no weapon catalog (preview): `equip` explains, give keeps the starter path
        let plain = crate::catalog::test_catalog();
        assert_eq!(run(&mut sim, &plain, &mut fl, "equip pistol").lines[0].0, LineKind::Error);
        assert_eq!(run(&mut sim, &plain, &mut fl, "give pump shotgun").requests, [Request::GiveGun { gun: 1, select: true }]);
    }
    #[test] fn give_resolves_names_and_emits_requests() {
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        let fx = run(&mut sim, &cat, &mut fl, "give pump shotgun"); assert_eq!(fx.requests, [Request::GiveGun { gun: 1, select: true }]); assert_eq!(fx.lines[0].0, LineKind::Ok);
        let fx = run(&mut sim, &cat, &mut fl, "give med kit 2"); assert_eq!(fx.requests, [Request::GiveHealable { idx: 1, count: 2, to_slot: true }], "{}", text(&fx));
        let fx = run(&mut sim, &cat, &mut fl, "spawn bandage 2"); assert_eq!(fx.requests, [Request::SpawnPickup { query: "bandage".into(), count: 2 }]); assert_eq!(run(&mut sim, &cat, &mut fl, "spawn").lines[0].0, LineKind::Error);
        let fx = run(&mut sim, &cat, &mut fl, "give wood 50"); assert!(fx.requests.is_empty()); assert_eq!(sim.world.wood, 50);
        run(&mut sim, &cat, &mut fl, "give wood 5000"); assert_eq!(sim.world.wood, 999, "the HUD counter cap");
        let fx = run(&mut sim, &cat, &mut fl, "give widgets"); assert_eq!(fx.lines[0].0, LineKind::Error);
        assert!(text(&run(&mut sim, &cat, &mut fl, "give wid")).contains("matches 4 items"));
        assert_eq!(run(&mut sim, &cat, &mut fl, "give").lines[0].0, LineKind::Error);
        assert_eq!(run(&mut sim, &cat, &mut fl, "give bandage 0").lines[0].0, LineKind::Error);
    }
    #[test] fn ammo_and_bulk_gives() {
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        assert_eq!(run(&mut sim, &cat, &mut fl, "ammo").requests, [Request::GiveAmmo { gun: None, count: None }]);
        assert_eq!(run(&mut sim, &cat, &mut fl, "ammo 30").requests, [Request::GiveAmmo { gun: None, count: Some(30) }]);
        assert_eq!(run(&mut sim, &cat, &mut fl, "ammo shotgun 12").requests, [Request::GiveAmmo { gun: Some(1), count: Some(12) }]);
        assert_eq!(run(&mut sim, &cat, &mut fl, "giveathenaweapons").requests.len(), 3);
        let fx = run(&mut sim, &cat, &mut fl, "giveathenaconsumables"); assert_eq!(fx.requests.len(), 3); assert!(fx.requests.contains(&Request::GiveHealable { idx: 1, count: 3, to_slot: false }));
        assert_eq!(run(&mut sim, &cat, &mut fl, "giveathenafullkit").requests.len(), 3 + 3 + 1);
    }
    #[test] fn vitals_commands_use_the_live_values() {
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        assert_eq!(run(&mut sim, &cat, &mut fl, "heal").requests, [Request::SetHealth(100.)]);
        assert_eq!(run(&mut sim, &cat, &mut fl, "heal 25").requests, [Request::SetHealth(65.)]);
        assert_eq!(run(&mut sim, &cat, &mut fl, "shield 500").requests, [Request::SetShield(100.)], "clamped to the maximum");
        assert_eq!(run(&mut sim, &cat, &mut fl, "sethealthpercent 50").requests, [Request::SetHealth(50.)]);
        assert_eq!(run(&mut sim, &cat, &mut fl, "sethealthpercent 150").lines[0].0, LineKind::Error);
        run(&mut sim, &cat, &mut fl, "god"); assert!(fl.god); run(&mut sim, &cat, &mut fl, "god off"); assert!(!fl.god); run(&mut sim, &cat, &mut fl, "ToggleUnlimitedHealth"); assert!(fl.god);
        assert_eq!(run(&mut sim, &cat, &mut fl, "god maybe").lines[0].0, LineKind::Error);
        run(&mut sim, &cat, &mut fl, "infiniteammo on"); assert!(fl.infinite_ammo);
    }
    #[test] fn movement_tuning_acts_on_the_headless_sim() {
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        run(&mut sim, &cat, &mut fl, "gravity 2.5");
        assert_eq!(sim.tuning.gravity_scale, Some(2.5)); assert_eq!(sim.controller.component().p.gravity_scale, 2.5);
        assert_eq!(sim.new_controller().component().p.gravity_scale, 2.5, "a respawn keeps the tuning");
        run(&mut sim, &cat, &mut fl, "gravity reset"); assert!(sim.tuning.gravity_scale.is_none()); assert_eq!(sim.controller.component().p.gravity_scale, Params::default().gravity_scale);
        assert_eq!(run(&mut sim, &cat, &mut fl, "gravity -1").lines.last().unwrap().0, LineKind::Error);
        run(&mut sim, &cat, &mut fl, "jump 800"); assert_eq!(sim.tuning.jump_z_velocity, Some(800.)); assert_eq!(sim.controller.component().p.jump_z_velocity, 800.);
        run(&mut sim, &cat, &mut fl, "speed 2"); assert_eq!(sim.speed_scale, 2.);
        assert_eq!(run(&mut sim, &cat, &mut fl, "speed 0").lines.last().unwrap().0, LineKind::Error); assert_eq!(sim.speed_scale, 2.);
        run(&mut sim, &cat, &mut fl, "speed reset"); assert_eq!(sim.speed_scale, 1.);
    }
    #[test] fn speed_scale_makes_the_pawn_cover_more_ground() {
        use fn_game::FrameInput;
        let walk = |scale: f32| { let mut s = GameSim::new(); s.speed_scale = scale; for _ in 0..90 { s.step(&FrameInput { forward: 1., ..Default::default() }, 1. / 60.); } s.controller.position.length() };
        let (a, b) = (walk(1.), walk(2.)); assert!(b > a * 1.5, "scale 2 walked {b} vs {a}");
    }
    #[test] fn storm_command_sets_radius_and_toggles() {
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        assert_eq!((sim.storm.radius_cm, sim.storm.master_opacity()), (150_000., 1.));
        assert_eq!(run(&mut sim, &cat, &mut fl, "storm 800").lines[0].0, LineKind::Ok); assert_eq!(sim.storm.radius_cm, 80_000.);
        run(&mut sim, &cat, &mut fl, "storm off"); assert_eq!(sim.storm.master_opacity(), 0.);
        run(&mut sim, &cat, &mut fl, "storm on"); assert_eq!(sim.storm.master_opacity(), 0., "the fade-in restarts from 0");
        run(&mut sim, &cat, &mut fl, "storm reset"); assert_eq!(sim.storm.radius_cm, 150_000.);
        assert_eq!(run(&mut sim, &cat, &mut fl, "storm banana").lines.iter().filter(|l| l.0 == LineKind::Error).count(), 2, "usage + reason");
        assert_eq!(run(&mut sim, &cat, &mut fl, "storm").lines[0].0, LineKind::Info);
    }
    #[test] fn gravity_scale_changes_the_fall() {
        use fn_game::FrameInput;
        let fall = |g: f32| { let mut s = GameSim::new(); s.controller.component_mut().p.gravity_scale = g; s.controller.height = 40.; for _ in 0..60 { s.step(&FrameInput::default(), 1. / 60.); } s.controller.height };
        assert!(fall(5.) < fall(0.2), "heavier gravity lands sooner: {} vs {}", fall(5.), fall(0.2));
    }
    #[test] fn teleport_moves_the_pawn_and_clamps_to_the_world() {
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        let fx = run(&mut sim, &cat, &mut fl, "tp 10 0 20"); assert_eq!(fx.lines[0].0, LineKind::Ok);
        assert_eq!((sim.x, sim.z), (10., 20.)); assert_eq!(sim.controller.position, Vec2::new(10., 20. - Z_OFFSET));
        let f = sim.feet(); assert_eq!((f.x, f.y, f.z), (10., 0., 20.));
        let fx = run(&mut sim, &cat, &mut fl, "tp 9999 5 -9999"); assert!(text(&fx).contains("clamped")); assert_eq!(sim.x, fn_game::sim::HALF - 0.5); assert_eq!(sim.x, sim.world_box().1.x); assert_eq!(sim.controller.height, 5.);
        assert_eq!(run(&mut sim, &cat, &mut fl, "tp 1 2").lines[0].0, LineKind::Error);
        run(&mut sim, &cat, &mut fl, "tp spawn"); let s = fn_game::sim::spawn(); assert_eq!((sim.x, sim.z), (s.0, s.2));
        // the step after a teleport keeps the pawn where it was put (the controller imports the external change)
        run(&mut sim, &cat, &mut fl, "tp 30 0 40"); sim.step(&fn_game::FrameInput::default(), 1. / 60.);
        assert!((sim.controller.position - Vec2::new(30., 40. - Z_OFFSET)).length() < 0.1, "{:?}", sim.controller.position);
    }
    /// TERRAIN: on a landscape, tp clamps to its bounds (minus EDGE_MARGIN) and y is the height above the ground there.
    #[test] fn teleport_on_the_landscape_stands_on_the_ground() {
        use fn_game::terrain as t;
        let hf = t::Heightfield::synthetic(glam::Vec2::splat(-5000.), 200., 2, 26, |x, y| 0.1 * x - 0.05 * y + 120.);
        let w = std::sync::Arc::new(t::World { hf, map: t::Mapping { centre_cm: [0.; 3], anchor: glam::Vec3::ZERO, ref_z_cm: 0. } });
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        sim.terrain = Some(w.clone());
        run(&mut sim, &cat, &mut fl, "tp 10 0 20");
        let g = w.ground_y(10., 20.).unwrap(); assert!((sim.feet().y - g).abs() < 1e-5 && g > 1.);
        sim.step(&fn_game::FrameInput::default(), 1. / 60.);
        assert!(sim.controller.grounded() && (sim.controller.height - g).abs() < 0.1, "{} vs {g}", sim.controller.height);
        run(&mut sim, &cat, &mut fl, "tp 9999 3 -9999"); assert_eq!((sim.x, sim.z), (49., -49.));
        assert!((sim.controller.height - w.ground_y(49., -49.).unwrap() - 3.).abs() < 1e-4);
        sim.step(&fn_game::FrameInput::default(), 1. / 60.); assert!(sim.controller.airborne());
    }
    #[test] fn cosmetics_go_through_the_source_and_unsupported_modes_say_so() {
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        assert!(text(&run(&mut sim, &cat, &mut fl, "skin Alpha Skin")).contains("Alpha Skin"));
        assert_eq!(run(&mut sim, &cat, &mut fl, "skin nobody").lines[0].0, LineKind::Error);
        assert_eq!(run(&mut sim, &cat, &mut fl, "skin").lines[0].0, LineKind::Error);
        assert!(text(&run(&mut sim, &cat, &mut fl, "fly")).contains("flying") && sim.fly == fn_game::sim::FlyMode::Fly); assert!(text(&run(&mut sim, &cat, &mut fl, "noclip")).contains("ghost") && sim.fly == fn_game::sim::FlyMode::Ghost); run(&mut sim, &cat, &mut fl, "walk"); assert_eq!(sim.fly, fn_game::sim::FlyMode::Walk);
    }
    #[test] fn list_help_clear_and_unknown() {
        let (mut sim, cat, mut fl) = (GameSim::new(), test_catalog(), Flags::default());
        let t = text(&run(&mut sim, &cat, &mut fl, "list skins alpha")); assert!(t.contains("CID_001") && t.contains("CID_003") && !t.contains("CID_002"), "{t}");
        assert!(text(&run(&mut sim, &cat, &mut fl, "list weapons")).contains("Pump Shotgun"));
        assert!(text(&run(&mut sim, &cat, &mut fl, "listweapons")).contains("Pump Shotgun"));
        assert!(text(&run(&mut sim, &cat, &mut fl, "list gliders")).contains("no gliders"));
        assert!(text(&run(&mut sim, &cat, &mut fl, "help god")).contains("UFortCheatManager::ToggleUnlimitedHealth"));
        assert!(text(&run(&mut sim, &cat, &mut fl, "help")).contains("giveathenafullkit"));
        assert!(run(&mut sim, &cat, &mut fl, "clear").clear);
        let t = text(&run(&mut sim, &cat, &mut fl, "gvie")); assert!(t.contains("unknown command"), "{t}");
        assert!(text(&run(&mut sim, &cat, &mut fl, "gi")).contains("did you mean: give"));
        assert!(run(&mut sim, &cat, &mut fl, "   ").lines.is_empty());
    }
}
