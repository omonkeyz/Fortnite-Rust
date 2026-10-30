//! F2 COMMAND BAR for the offline lobby: a text line overlay (drawn with the 1.8 UI fonts through `fn_ui`) where the user types commands that edit the
//! offline profile (`lobby_profile`) and the offline shop (`lobby_shop`). It is a developer / sandbox convenience of this port, NOT a recovered 1.8 feature
//! (the 1.8 client has no such command bar); nothing here talks to a server.
//!
//! * typing, Backspace / Delete / Left / Right / Home / End, history with Up / Down, Tab autocomplete (command names, item ids / names, slots, amounts) with a
//!   suggestion list and an argument hint, Enter executes, Esc / ~ closes;
//! * commands: `help`, `addvbucks <n>`, `setvbucks <n>`, `giveitem <id|name>`, `giveall [outfit|pickaxe|glider]`, `removeitem <id|name>`, `clearitems`, `rotateshop [days|reset]`,
//!   `setskin <id|name>`, `setpickaxe <id|name>`, `setglider <id|name>`, `equip <slot> <id|name>`, `resetprofile`, `balance`, `owned [category]`, `shop`.
//!
//! Integration: keep one [`Console`]; route F2 to [`Console::toggle`]; while [`Console::is_open`] send every key to [`Console::key`] (build [`Key`] values with
//! [`keys_from_winit`]) and do not forward it to the lobby; after a key that returned [`Effects`] with `profile_changed` write the profile (`Profile::flush`), with
//! `equip_changed` rebuild the lobby pawn / Locker lists, with `shop_changed` nothing else is needed (the shop view is rebuilt every frame from the profile);
//! draw [`Console::draw`] last, over everything.
use crate::lobby_profile::{Profile, Slot};
use crate::lobby_shop::{rarity_title, Resolve, Shop, ShopItem};
use fn_pose::cosmetics::Category;

/// V-Bucks pack sizes of the 1.8 paks (`/Game/Catalog/DisplayAssets/DA_MtxPack{1000,2800,4200,7500,13500}`): the amount presets of `addvbucks` / `setvbucks`.
pub const VBUCKS_PACKS: [i64; 5] = [1000, 2800, 4200, 7500, 13500];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind { None, Amount, Item, OutfitItem, PickaxeItem, GliderItem, OwnedItem, Category, Slot, SlotItem, Days }

#[derive(Clone, Copy, Debug)]
pub struct CommandSpec { pub name: &'static str, pub args: &'static str, pub help: &'static str, pub kind: ArgKind }

pub const COMMANDS: [CommandSpec; 16] = [
    CommandSpec { name: "help", args: "[command]", help: "list the commands, or describe one", kind: ArgKind::None },
    CommandSpec { name: "addvbucks", args: "<amount>", help: "add V-Bucks to the balance (negative subtracts); 10k / 2m accepted", kind: ArgKind::Amount },
    CommandSpec { name: "setvbucks", args: "<amount>", help: "set the V-Bucks balance", kind: ArgKind::Amount },
    CommandSpec { name: "giveitem", args: "<id|name>", help: "grant an outfit / harvesting tool / glider to the profile", kind: ArgKind::Item },
    CommandSpec { name: "giveall", args: "[outfit|pickaxe|glider]", help: "grant every cosmetic of the paks (or of one category)", kind: ArgKind::Category },
    CommandSpec { name: "removeitem", args: "<id|name>", help: "remove an owned item (default items stay)", kind: ArgKind::OwnedItem },
    CommandSpec { name: "rotateshop", args: "[days|reset]", help: "advance the offline shop rotation by days (default 1), or reset it to today", kind: ArgKind::Days },
    CommandSpec { name: "setskin", args: "<id|name>", help: "grant (if needed) and equip an outfit", kind: ArgKind::OutfitItem },
    CommandSpec { name: "setpickaxe", args: "<id|name>", help: "grant (if needed) and equip a harvesting tool", kind: ArgKind::PickaxeItem },
    CommandSpec { name: "setglider", args: "<id|name>", help: "grant (if needed) and equip a glider", kind: ArgKind::GliderItem },
    CommandSpec { name: "equip", args: "<slot> <id|name>", help: "equip an OWNED item into a Locker slot (outfit | pickaxe | glider)", kind: ArgKind::SlotItem },
    CommandSpec { name: "resetprofile", args: "", help: "back to the default profile: 1,000,000 V-Bucks, owns every cosmetic of the paks, shop rotation reset", kind: ArgKind::None },
    CommandSpec { name: "clearitems", args: "", help: "remove every owned item except the default ones (V-Bucks unchanged) so every shop offer can be bought", kind: ArgKind::None },
    CommandSpec { name: "balance", args: "", help: "show the V-Bucks balance and the loadout", kind: ArgKind::None },
    CommandSpec { name: "owned", args: "[outfit|pickaxe|glider]", help: "list the owned items", kind: ArgKind::Category },
    CommandSpec { name: "shop", args: "", help: "list today's offers with their prices", kind: ArgKind::None },
];

pub fn spec(name: &str) -> Option<&'static CommandSpec> { COMMANDS.iter().find(|c| c.name.eq_ignore_ascii_case(name)) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind { Input, Ok, Error, Info }

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Effects {
    /// the profile changed (write it with `Profile::flush`)
    pub profile_changed: bool,
    /// an equipped item changed (rebuild the lobby pawn / refresh the Locker)
    pub equip_changed: bool,
    pub shop_changed: bool,
    pub lines: Vec<(LineKind, String)>,
}
impl Effects {
    fn say(&mut self, k: LineKind, s: impl Into<String>) { self.lines.push((k, s.into())); }
}

/// What the commands operate on.
pub struct Env<'a> { pub profile: &'a mut Profile, pub shop: &'a Shop, pub now: i64 }

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// parsing helpers

/// Amount with optional sign, thousands separators and k / m suffix: `1500`, `+1,500`, `-20k`, `2m`.
pub fn parse_amount(s: &str) -> Option<i64> {
    let t: String = s.trim().chars().filter(|c| *c != ',' && *c != '_').collect();
    if t.is_empty() { return None; }
    let (neg, t) = match t.strip_prefix('-') { Some(r) => (true, r.to_owned()), None => (false, t.strip_prefix('+').unwrap_or(&t).to_owned()) };
    let (num, mult) = match t.chars().last()? { 'k' | 'K' => (&t[..t.len() - 1], 1_000i64), 'm' | 'M' => (&t[..t.len() - 1], 1_000_000), _ => (&t[..], 1) };
    let v: f64 = num.parse().ok()?;
    if !v.is_finite() || v < 0.0 { return None; }
    let n = (v * mult as f64).round();
    if n > 9.0e15 { return None; }
    Some(if neg { -(n as i64) } else { n as i64 })
}

fn category_of_word(w: &str) -> Option<Category> {
    match Slot::parse(w)? { Slot::Outfit => Some(Category::Outfit), Slot::Pickaxe => Some(Category::Pickaxe), Slot::Glider => Some(Category::Glider) }
}
fn slot_cat(s: Slot) -> Category { match s { Slot::Outfit => Category::Outfit, Slot::Pickaxe => Category::Pickaxe, Slot::Glider => Category::Glider } }

/// `(command, rest)` of a line; the rest has surrounding quotes removed.
fn split_command(line: &str) -> (&str, &str) {
    let l = line.trim_start();
    match l.find(char::is_whitespace) { Some(i) => (&l[..i], l[i..].trim()), None => (l, "") }
}
fn unquote(s: &str) -> &str { s.trim().trim_matches('"') }

fn describe(i: &ShopItem) -> String { format!("{} ({}, {})", i.name, i.category.name(), rarity_title(i.rarity)) }

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// execution

fn resolve_in<'a>(shop: &'a Shop, query: &str, filter: impl Fn(&ShopItem) -> bool, fx: &mut Effects) -> Option<&'a ShopItem> {
    let q = unquote(query);
    if q.is_empty() { fx.say(LineKind::Error, "missing item: give an id or a name (Tab completes)"); return None; }
    // resolve inside the filtered set first so "Paratrooper" for setskin does not hit a glider
    let cat = crate::lobby_shop::ShopCatalog { items: shop.catalog.items.iter().filter(|i| filter(i)).cloned().collect() };
    match cat.resolve(q) {
        Resolve::One(i) => shop.catalog.get(&i.id),
        Resolve::Many(v) => {
            fx.say(LineKind::Error, format!("'{q}' matches {} items, be more specific (use the id):", v.len()));
            for i in v.iter().take(8) { fx.say(LineKind::Info, format!("  {}  {}", i.id, describe(i))); }
            if v.len() > 8 { fx.say(LineKind::Info, format!("  ... and {} more", v.len() - 8)); }
            None
        }
        Resolve::None => { fx.say(LineKind::Error, format!("no item matches '{q}'")); None }
    }
}

pub fn execute(line: &str, env: &mut Env) -> Effects {
    let mut fx = Effects::default();
    let (cmd, rest) = split_command(line);
    if cmd.is_empty() { return fx; }
    let Some(sp) = spec(cmd) else {
        let near: Vec<&str> = COMMANDS.iter().filter(|c| c.name.starts_with(&cmd.to_ascii_lowercase())).map(|c| c.name).collect();
        if near.is_empty() { fx.say(LineKind::Error, format!("unknown command '{cmd}' (type help)")); } else { fx.say(LineKind::Error, format!("unknown command '{cmd}', did you mean: {}", near.join(", "))); }
        return fx;
    };
    match sp.name {
        "help" => {
            if rest.is_empty() {
                fx.say(LineKind::Info, "commands (Tab completes, Up / Down = history, Esc / ~ closes):");
                for c in COMMANDS.iter() { fx.say(LineKind::Info, format!("  {} {}  - {}", c.name, c.args, c.help)); }
            } else if let Some(c) = spec(rest) {
                fx.say(LineKind::Info, format!("{} {}", c.name, c.args));
                fx.say(LineKind::Info, format!("  {}", c.help));
            } else { fx.say(LineKind::Error, format!("no command '{rest}'")); }
        }
        "addvbucks" | "setvbucks" => {
            let Some(n) = parse_amount(rest) else { fx.say(LineKind::Error, format!("usage: {} {}   (e.g. {} 1000, 10k, 2m)", sp.name, sp.args, sp.name)); return fx };
            if sp.name == "addvbucks" {
                let before = env.profile.balance();
                let after = env.profile.add_vbucks(n);
                fx.say(LineKind::Ok, format!("V-Bucks {:+} -> balance {}{}", after - before, group(after), if after - before != n { " (clamped)" } else { "" }));
            } else {
                if n < 0 { fx.say(LineKind::Error, "the balance cannot be negative"); return fx; }
                env.profile.set_vbucks(n);
                fx.say(LineKind::Ok, format!("V-Bucks balance set to {}", group(env.profile.balance())));
            }
            fx.profile_changed = true;
        }
        "giveitem" => {
            let Some(i) = resolve_in(env.shop, rest, |_| true, &mut fx) else { return fx };
            if env.profile.grant(&i.id) { fx.say(LineKind::Ok, format!("granted {} [{}]", describe(i), i.id)); fx.profile_changed = true; fx.shop_changed = true; }
            else { fx.say(LineKind::Info, format!("already owned: {}", describe(i))); }
        }
        "giveall" => {
            let cat = if rest.is_empty() { None } else {
                match category_of_word(rest) { Some(c) => Some(c), None => { fx.say(LineKind::Error, "usage: giveall [outfit|pickaxe|glider]"); return fx } }
            };
            let mut n = 0;
            for i in env.shop.catalog.items.iter().filter(|i| cat.map_or(true, |c| i.category == c)) { if env.profile.grant(&i.id) { n += 1; } }
            fx.say(LineKind::Ok, format!("granted {n} new item(s); the profile owns {} items", env.profile.owned_count()));
            if n > 0 { fx.profile_changed = true; fx.shop_changed = true; }
        }
        "removeitem" => {
            let Some(i) = resolve_in(env.shop, rest, |_| true, &mut fx) else { return fx };
            if env.profile.is_default(&i.id) { fx.say(LineKind::Error, format!("{} is a default item and cannot be removed", describe(i))); }
            else if env.profile.revoke(&i.id) { fx.say(LineKind::Ok, format!("removed {} [{}]", describe(i), i.id)); fx.profile_changed = true; fx.equip_changed = true; fx.shop_changed = true; }
            else { fx.say(LineKind::Info, format!("not owned: {}", describe(i))); }
        }
        "rotateshop" => {
            let days = match rest.to_ascii_lowercase().as_str() { "" => 1, "reset" | "today" => { env.profile.shop_day_offset = 0; fx.profile_changed = true; fx.shop_changed = true; fx.say(LineKind::Ok, "shop rotation reset to today (UTC)"); return fx } s => match s.parse::<i64>() { Ok(d) if d.abs() <= 3650 => d, _ => { fx.say(LineKind::Error, "usage: rotateshop [days|reset]   (days between -3650 and 3650)"); return fx } } };
            env.profile.shop_day_offset += days;
            let rot = env.shop.rotation(env.profile, env.now);
            fx.say(LineKind::Ok, format!("shop rotated by {days:+} day(s): UTC day {} (offset {:+}); {} weekly + {} daily offers", rot.day, env.profile.shop_day_offset, rot.weekly.len(), rot.daily.len()));
            fx.profile_changed = true; fx.shop_changed = true;
        }
        "setskin" | "setpickaxe" | "setglider" => {
            let (slot, cat) = match sp.name { "setskin" => (Slot::Outfit, Category::Outfit), "setpickaxe" => (Slot::Pickaxe, Category::Pickaxe), _ => (Slot::Glider, Category::Glider) };
            let Some(i) = resolve_in(env.shop, rest, |i| i.category == cat, &mut fx) else { return fx };
            let new = env.profile.grant(&i.id);
            let _ = env.profile.equip(slot, &i.id);
            fx.say(LineKind::Ok, format!("{}equipped {} [{}] as {}", if new { "granted and " } else { "" }, describe(i), i.id, slot.title()));
            fx.profile_changed = true; fx.equip_changed = true; fx.shop_changed = new;
        }
        "equip" => {
            let (slot_word, item) = split_command(rest);
            let Some(slot) = Slot::parse(slot_word) else { fx.say(LineKind::Error, "usage: equip <outfit|pickaxe|glider> <id|name>"); return fx };
            let cat = slot_cat(slot);
            let Some(i) = resolve_in(env.shop, item, |i| i.category == cat, &mut fx) else { return fx };
            match env.profile.equip(slot, &i.id) {
                Ok(()) => { fx.say(LineKind::Ok, format!("equipped {} as {}", describe(i), slot.title())); fx.profile_changed = true; fx.equip_changed = true; }
                Err(e) => fx.say(LineKind::Error, format!("{e}: use giveitem {} first (or setskin / setpickaxe / setglider)", i.id)),
            }
        }
        "resetprofile" => {
            env.profile.reset();
            fx.say(LineKind::Ok, format!("profile reset: {} V-Bucks, {} owned items (everything), shop rotation reset", group(env.profile.balance()), env.profile.owned_count()));
            fx.profile_changed = true; fx.equip_changed = true; fx.shop_changed = true;
        }
        "clearitems" => {
            let before = env.profile.owned_count();
            env.profile.clear_items();
            fx.say(LineKind::Ok, format!("removed {} item(s): the profile owns only its {} default items now, V-Bucks {} (buy from the Item Shop, or giveitem / giveall / resetprofile)", before - env.profile.owned_count().min(before), env.profile.owned_count(), group(env.profile.balance())));
            fx.profile_changed = true; fx.equip_changed = true; fx.shop_changed = true;
        }
        "balance" => {
            fx.say(LineKind::Info, format!("V-Bucks: {}   owned items: {}", group(env.profile.balance()), env.profile.owned_count()));
            for (s, id) in env.profile.loadout().map(|(s, i)| (s, i.to_owned())).collect::<Vec<_>>() {
                let name = env.shop.catalog.get(&id).map(describe).unwrap_or_else(|| id.clone());
                fx.say(LineKind::Info, format!("  {}: {name}", s.title()));
            }
        }
        "owned" => {
            let cat = if rest.is_empty() { None } else { match category_of_word(rest) { Some(c) => Some(c), None => { fx.say(LineKind::Error, "usage: owned [outfit|pickaxe|glider]"); return fx } } };
            let items: Vec<&ShopItem> = env.shop.catalog.items.iter().filter(|i| env.profile.owns(&i.id) && cat.map_or(true, |c| i.category == c)).collect();
            fx.say(LineKind::Info, format!("{} owned item(s){}", items.len(), if items.len() > 12 { " (first 12)" } else { "" }));
            for i in items.iter().take(12) { fx.say(LineKind::Info, format!("  {}  {}", i.id, describe(i))); }
        }
        "shop" => {
            let v = env.shop.view(env.profile, env.now);
            for (title, list) in [("WEEKLY", &v.weekly), ("DAILY", &v.daily)] {
                fx.say(LineKind::Info, format!("{title} ({} offers)", list.len()));
                for o in list { fx.say(LineKind::Info, format!("  {}  {} V-Bucks{}", o.name, group(o.price), if o.owned { "  [owned]" } else { "" })); }
            }
        }
        _ => {}
    }
    fx
}

fn group(v: i64) -> String {
    let s = v.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() { if i > 0 && (s.len() - i) % 3 == 0 { out.push(','); } out.push(c); }
    if v < 0 { format!("-{out}") } else { out }
}

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// autocomplete

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// text that replaces the token under the cursor
    pub insert: String,
    /// shown in the list
    pub label: String,
    pub hint: String,
}

/// Suggestions for the line (`token_start` = byte offset where the replaced token starts) and the ghost hint for the arguments still to type.
pub struct Completion { pub token_start: usize, pub items: Vec<Suggestion>, pub hint: String }

fn matches_query(i: &ShopItem, q: &str) -> Option<u8> {
    if q.is_empty() { return Some(3); }
    let id = i.id.to_ascii_lowercase();
    let name = i.name.to_ascii_lowercase();
    let nq: String = q.chars().filter(|c| !matches!(c, '_' | '-' | ' ' | '\'')).collect();
    let nname: String = name.chars().filter(|c| !matches!(c, '_' | '-' | ' ' | '\'')).collect();
    let nid: String = id.chars().filter(|c| !matches!(c, '_' | '-' | ' ' | '\'')).collect();
    if id.starts_with(q) { Some(0) } else if name.starts_with(q) || nname.starts_with(&nq) { Some(1) } else if id.contains(q) || name.contains(q) || nid.contains(&nq) || nname.contains(&nq) { Some(2) } else { None }
}

fn item_suggestions(env: &Env, q: &str, filter: impl Fn(&ShopItem) -> bool) -> Vec<Suggestion> {
    let q = unquote(q).to_ascii_lowercase();
    let mut v: Vec<(u8, &ShopItem)> = env.shop.catalog.items.iter().filter(|i| filter(i)).filter_map(|i| matches_query(i, &q).map(|r| (r, i))).collect();
    v.sort_by(|a, b| (a.0, &a.1.id).cmp(&(b.0, &b.1.id)));
    v.into_iter().map(|(_, i)| Suggestion { insert: i.id.clone(), label: i.id.clone(), hint: format!("{}{}", describe(i), if env.profile.owns(&i.id) { "  [owned]" } else { "" }) }).collect()
}

pub fn complete(line: &str, env: &Env) -> Completion {
    let lead = line.len() - line.trim_start().len();
    let body = &line[lead..];
    // first token: the command
    let Some(sp_at) = body.find(char::is_whitespace) else {
        let q = body.to_ascii_lowercase();
        let mut items: Vec<Suggestion> = COMMANDS.iter().filter(|c| c.name.starts_with(&q)).map(|c| Suggestion { insert: c.name.to_owned(), label: format!("{} {}", c.name, c.args).trim_end().to_owned(), hint: c.help.to_owned() }).collect();
        if items.is_empty() { items = COMMANDS.iter().filter(|c| c.name.contains(&q)).map(|c| Suggestion { insert: c.name.to_owned(), label: format!("{} {}", c.name, c.args).trim_end().to_owned(), hint: c.help.to_owned() }).collect(); }
        let hint = if items.len() == 1 && items[0].insert == q { items[0].label[items[0].insert.len()..].trim().to_owned() } else { String::new() };
        return Completion { token_start: lead, items, hint };
    };
    let cmd = &body[..sp_at];
    let rest_start = lead + sp_at + body[sp_at..].len() - body[sp_at..].trim_start().len();
    let rest = &line[rest_start..];
    let Some(sp) = spec(cmd) else { return Completion { token_start: rest_start, items: vec![], hint: String::new() } };
    let hint = |typed_args: usize| -> String { sp.args.split_whitespace().skip(typed_args).collect::<Vec<_>>().join(" ") };
    let cat_items = |q: &str| -> Vec<Suggestion> {
        ["outfit", "pickaxe", "glider"].iter().filter(|c| c.starts_with(&q.to_ascii_lowercase())).map(|c| Suggestion { insert: (*c).to_owned(), label: (*c).to_owned(), hint: match *c { "outfit" => "skins (CID_*)", "pickaxe" => "harvesting tools", _ => "gliders and umbrellas" }.to_owned() }).collect()
    };
    match sp.kind {
        ArgKind::None => Completion { token_start: rest_start, items: if sp.name == "help" { COMMANDS.iter().filter(|c| c.name.starts_with(&rest.to_ascii_lowercase())).map(|c| Suggestion { insert: c.name.to_owned(), label: c.name.to_owned(), hint: c.help.to_owned() }).collect() } else { vec![] }, hint: String::new() },
        ArgKind::Amount => {
            let items = VBUCKS_PACKS.iter().filter(|p| p.to_string().starts_with(rest.trim())).map(|p| Suggestion { insert: p.to_string(), label: group(*p), hint: "V-Bucks (pack size of the 1.8 store)".to_owned() }).chain(["100000", "1000000"].iter().filter(|p| p.starts_with(rest.trim())).map(|p| Suggestion { insert: (*p).to_owned(), label: group(p.parse().unwrap()), hint: "V-Bucks".to_owned() })).collect();
            Completion { token_start: rest_start, items, hint: if rest.is_empty() { hint(0) } else { String::new() } }
        }
        ArgKind::Days => {
            let mut items: Vec<Suggestion> = ["1", "7", "-1"].iter().filter(|d| d.starts_with(rest.trim())).map(|d| Suggestion { insert: (*d).to_owned(), label: (*d).to_owned(), hint: "days".to_owned() }).collect();
            if "reset".starts_with(&rest.trim().to_ascii_lowercase()) { items.push(Suggestion { insert: "reset".into(), label: "reset".into(), hint: "back to today's rotation".into() }); }
            Completion { token_start: rest_start, items, hint: if rest.is_empty() { hint(0) } else { String::new() } }
        }
        ArgKind::Category => Completion { token_start: rest_start, items: cat_items(rest.trim()), hint: if rest.is_empty() { hint(0) } else { String::new() } },
        ArgKind::Slot => Completion { token_start: rest_start, items: cat_items(rest.trim()), hint: String::new() },
        ArgKind::Item | ArgKind::OutfitItem | ArgKind::PickaxeItem | ArgKind::GliderItem | ArgKind::OwnedItem => {
            let cat = match sp.kind { ArgKind::OutfitItem => Some(Category::Outfit), ArgKind::PickaxeItem => Some(Category::Pickaxe), ArgKind::GliderItem => Some(Category::Glider), _ => None };
            let owned_only = sp.kind == ArgKind::OwnedItem;
            let items = item_suggestions(env, rest, |i| cat.map_or(true, |c| i.category == c) && (!owned_only || (env.profile.owns(&i.id) && !env.profile.is_default(&i.id))));
            Completion { token_start: rest_start, items, hint: if rest.is_empty() { hint(0) } else { String::new() } }
        }
        ArgKind::SlotItem => {
            let (slot_word, after) = match rest.find(char::is_whitespace) { Some(i) => (&rest[..i], Some((i, rest[i..].trim_start()))), None => (rest, None) };
            match after {
                None => Completion { token_start: rest_start, items: cat_items(slot_word), hint: if rest.is_empty() { hint(0) } else { String::new() } },
                Some((_, item_q)) => {
                    let off = rest.len() - item_q.len();
                    let Some(slot) = Slot::parse(slot_word) else { return Completion { token_start: rest_start + off, items: vec![], hint: String::new() } };
                    let cat = slot_cat(slot);
                    let items = item_suggestions(env, item_q, |i| i.category == cat && env.profile.owns(&i.id));
                    Completion { token_start: rest_start + off, items, hint: if item_q.is_empty() { hint(2) } else { String::new() } }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------------------------
// the console widget

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key { Char(char), Backspace, Delete, Left, Right, Home, End, Up, Down, Tab, ShiftTab, Enter, Escape, ToggleConsole }

/// Keys of a winit key press: F2 toggles; named editing keys; printable text (`event.text`) becomes `Char`s.
pub fn keys_from_winit(event: &winit::event::KeyEvent, shift: bool) -> Vec<Key> {
    use winit::keyboard::{Key as WKey, KeyCode, NamedKey, PhysicalKey};
    if let PhysicalKey::Code(KeyCode::Backquote) = event.physical_key { return vec![Key::ToggleConsole]; }
    if let WKey::Named(n) = &event.logical_key {
        let k = match n {
            NamedKey::Backspace => Key::Backspace, NamedKey::Delete => Key::Delete, NamedKey::ArrowLeft => Key::Left, NamedKey::ArrowRight => Key::Right,
            NamedKey::Home => Key::Home, NamedKey::End => Key::End, NamedKey::ArrowUp => Key::Up, NamedKey::ArrowDown => Key::Down,
            NamedKey::Tab => if shift { Key::ShiftTab } else { Key::Tab }, NamedKey::Enter => Key::Enter, NamedKey::Escape => Key::Escape,
            NamedKey::Space => Key::Char(' '),
            _ => return event.text.as_ref().map(|t| t.chars().filter(|c| (' '..='~').contains(c)).map(Key::Char).collect()).unwrap_or_default(),
        };
        return vec![k];
    }
    event.text.as_ref().map(|t| t.chars().filter(|c| (' '..='~').contains(c)).map(Key::Char).collect()).unwrap_or_default()
}

#[derive(Default)]
struct TabCycle { token_start: usize, original: String, items: Vec<Suggestion>, idx: usize }

#[derive(Default)]
pub struct Console {
    open: bool,
    input: String,
    /// caret position in characters
    cursor: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    draft: String,
    log: Vec<(LineKind, String)>,
    cycle: Option<TabCycle>,
    /// suggestion highlighted by the last Tab (index into the last completion)
    selected: Option<usize>,
}

impl Console {
    pub fn new() -> Console {
        let mut c = Console::default();
        c.log.push((LineKind::Info, "Offline command bar (~ closes). Type help, or press Tab to complete.".into()));
        c
    }
    pub fn is_open(&self) -> bool { self.open }
    pub fn open(&mut self) { self.open = true; }
    pub fn close(&mut self) { self.open = false; self.cycle = None; }
    pub fn toggle(&mut self) { if self.open { self.close() } else { self.open() } }
    pub fn input(&self) -> &str { &self.input }
    pub fn cursor(&self) -> usize { self.cursor }
    pub fn log(&self) -> &[(LineKind, String)] { &self.log }
    pub fn history(&self) -> &[String] { &self.history }
    /// Replace the input (used by tests / scripted use).
    pub fn set_input(&mut self, s: &str) { self.input = s.to_owned(); self.cursor = s.chars().count(); self.cycle = None; self.selected = None; }

    fn byte_at(&self, chars: usize) -> usize { self.input.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(self.input.len()) }
    fn edited(&mut self) { self.cycle = None; self.selected = None; self.hist_pos = None; }

    /// Suggestions for the current input (for drawing).
    pub fn completion(&self, env: &Env) -> Completion { complete(&self.input, env) }

    /// Handle one key while the console is open; returns the effects of an executed command.
    pub fn key(&mut self, k: Key, env: &mut Env) -> Effects {
        let mut fx = Effects::default();
        match k {
            Key::ToggleConsole | Key::Escape => self.close(),
            Key::Char(c) if (' '..='~').contains(&c) => {
                let at = self.byte_at(self.cursor);
                self.input.insert(at, c);
                self.cursor += 1;
                self.edited();
            }
            Key::Char(_) => {}
            Key::Backspace => if self.cursor > 0 { let a = self.byte_at(self.cursor - 1); let b = self.byte_at(self.cursor); self.input.replace_range(a..b, ""); self.cursor -= 1; self.edited(); },
            Key::Delete => if self.cursor < self.input.chars().count() { let a = self.byte_at(self.cursor); let b = self.byte_at(self.cursor + 1); self.input.replace_range(a..b, ""); self.edited(); },
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.input.chars().count(),
            Key::Up => {
                if self.history.is_empty() { return fx; }
                let pos = match self.hist_pos { None => { self.draft = self.input.clone(); self.history.len() - 1 } Some(p) => p.saturating_sub(1) };
                self.hist_pos = Some(pos);
                self.input = self.history[pos].clone();
                self.cursor = self.input.chars().count();
                self.cycle = None; self.selected = None;
            }
            Key::Down => {
                let Some(p) = self.hist_pos else { return fx };
                if p + 1 < self.history.len() { self.hist_pos = Some(p + 1); self.input = self.history[p + 1].clone(); }
                else { self.hist_pos = None; self.input = std::mem::take(&mut self.draft); }
                self.cursor = self.input.chars().count();
                self.cycle = None; self.selected = None;
            }
            Key::Tab | Key::ShiftTab => self.tab(k == Key::ShiftTab, env),
            Key::Enter => {
                let line = self.input.trim().to_owned();
                if line.is_empty() { return fx; }
                if self.history.last() != Some(&line) { self.history.push(line.clone()); }
                self.input.clear(); self.cursor = 0; self.hist_pos = None; self.cycle = None; self.selected = None;
                self.log.push((LineKind::Input, format!("> {line}")));
                fx = execute(&line, env);
                self.log.extend(fx.lines.iter().cloned());
                let n = self.log.len();
                if n > 300 { self.log.drain(0..n - 300); }
            }
        }
        fx
    }

    /// Tab: complete the token under the caret. One match = insert it (a command gets a trailing space and its argument hint); several = insert the common
    /// prefix first, then cycle through the matches on each further Tab (Shift+Tab backwards).
    fn tab(&mut self, back: bool, env: &Env) {
        if let Some(c) = &mut self.cycle {
            let n = c.items.len();
            if n > 0 {
                c.idx = if back { (c.idx + n - 1) % n } else { (c.idx + 1) % n };
                let ins = c.items[c.idx].insert.clone();
                let start = c.token_start;
                self.input.replace_range(start.., &ins);
                self.cursor = self.input.chars().count();
                self.selected = Some(c.idx);
            }
            return;
        }
        let comp = complete(&self.input, env);
        if comp.items.is_empty() { return; }
        let start = comp.token_start.min(self.input.len());
        let typed = self.input[start..].to_owned();
        // a command word (the only token of the line) is followed by a space when it takes arguments, so the argument hint / list shows next
        let in_command = !self.input.trim_start().contains(char::is_whitespace);
        if comp.items.len() == 1 {
            let ins = comp.items[0].insert.clone();
            let mut text = ins.clone();
            if in_command && spec(&ins).map_or(false, |c| !c.args.is_empty()) { text.push(' '); }
            self.input.replace_range(start.., &text);
            self.cursor = self.input.chars().count();
            self.selected = Some(0);
            return;
        }        // several: common prefix of the inserts (case-insensitive match against what was typed)
        let first = comp.items[0].insert.clone();
        let mut common = first.clone();
        for s in &comp.items[1..] { let l = common.chars().zip(s.insert.chars()).take_while(|(a, b)| a.eq_ignore_ascii_case(b)).count(); common = common.chars().take(l).collect(); }
        if common.len() > typed.trim().len() && comp.items.iter().all(|s| s.insert.to_ascii_lowercase().starts_with(&common.to_ascii_lowercase())) {
            self.input.replace_range(start.., &common);
            self.cursor = self.input.chars().count();
            self.selected = None;
            return;
        }
        let items = comp.items.clone();
        self.cycle = Some(TabCycle { token_start: start, original: typed, items, idx: 0 });
        let ins = comp.items[0].insert.clone();
        self.input.replace_range(start.., &ins);
        self.cursor = self.input.chars().count();
        self.selected = Some(0);
    }

    // ---- drawing ------------------------------------------------------------------------------------------------------------------------------

    /// The overlay as a draw list (pixels; transparent outside the panel). `time` drives the caret blink. Nothing is drawn while closed.
    pub fn draw(&self, uic: &mut fn_ui::scene::UiContext, w: f32, h: f32, env: &Env, time: f32) -> fn_ui::draw::RenderList {
        use fn_ui::layout::Rect;
        use fn_ui::screens::{ascent, atlas_px, put_text, Snap};
        use fn_ui::text::FontId;
        let mut out = fn_ui::draw::RenderList::new();
        if !self.open { return out; }
        let scale = fn_ui::layout::dpi_scale(w, h);
        let px = atlas_px(18.0, scale);
        let line_h = (uic.fonts.line_height(FontId::Regular, px as f32) * 1.15).ceil();
        let pad = 10.0 * scale;
        let comp = self.completion(env);
        let max_sugg = 9usize;
        let shown: Vec<&Suggestion> = comp.items.iter().take(max_sugg).collect();
        let log_lines = 9usize;
        let log_start = self.log.len().saturating_sub(log_lines);
        let rows_log = self.log.len() - log_start;
        let height = pad * 2.0 + line_h * (rows_log as f32 + 1.0) + if shown.is_empty() { 0.0 } else { line_h * (shown.len() as f32 + if comp.items.len() > max_sugg { 1.0 } else { 0.0 }) + pad * 0.5 };
        // panel: dark translucent backing + accent line (the 1.8 menu navy)
        out.fill(Rect::new(0.0, 0.0, w, height), [0.012, 0.016, 0.045, 0.93]);
        out.fill(Rect::new(0.0, height - 2.0, w, 2.0), [0.35, 0.55, 0.85, 1.0]);
        let colour = |k: LineKind| match k { LineKind::Input => [0.75, 0.85, 1.0, 1.0], LineKind::Ok => [0.55, 0.95, 0.55, 1.0], LineKind::Error => [1.0, 0.45, 0.45, 1.0], LineKind::Info => [0.85, 0.85, 0.85, 1.0] };
        let asc = ascent(uic, FontId::Regular, px);
        let mut y = pad;
        let shadow = Some(([1.0, 1.0], [0.0, 0.0, 0.0, 0.8]));
        for (k, line) in &self.log[log_start..] {
            put_text(uic, &mut out, FontId::Regular, px, pad, y + asc, line, colour(*k), shadow, Snap::Pen);
            y += line_h;
        }
        // input row
        let prompt = "> ";
        let pw = uic.fonts.measure(FontId::Bold, prompt, px as f32);
        put_text(uic, &mut out, FontId::Bold, px, pad, y + asc, prompt, [1.0, 0.85, 0.2, 1.0], shadow, Snap::Pen);
        put_text(uic, &mut out, FontId::Regular, px, pad + pw, y + asc, &self.input, [1.0, 1.0, 1.0, 1.0], shadow, Snap::Pen);
        let before: String = self.input.chars().take(self.cursor).collect();
        let cx = pad + pw + uic.fonts.measure(FontId::Regular, &before, px as f32);
        if (time * 2.0).fract() < 0.6 { out.fill(Rect::new(cx.round(), y + 2.0, (2.0 * scale).max(1.0), line_h - 4.0), [1.0, 1.0, 1.0, 1.0]); }
        // ghost hint for the remaining arguments
        if !comp.hint.is_empty() {
            let iw = uic.fonts.measure(FontId::Regular, &self.input, px as f32);
            let gap = if self.input.ends_with(' ') || self.input.is_empty() { 0.0 } else { uic.fonts.measure(FontId::Regular, " ", px as f32) };
            put_text(uic, &mut out, FontId::Regular, px, pad + pw + iw + gap + 4.0, y + asc, &comp.hint, [0.55, 0.6, 0.7, 1.0], None, Snap::Pen);
        }
        y += line_h + pad * 0.5;
        // suggestion list
        let label_w = shown.iter().map(|s| uic.fonts.measure(FontId::Bold, &s.label, px as f32)).fold(0.0f32, f32::max).min(w * 0.5);
        for (i, s) in shown.iter().enumerate() {
            let sel = self.selected == Some(i);
            if sel { out.fill(Rect::new(pad * 0.5, y, w - pad, line_h), [0.25, 0.45, 0.8, 0.55]); }
            put_text(uic, &mut out, FontId::Bold, px, pad, y + asc + 1.0, &s.label, if sel { [1.0, 1.0, 1.0, 1.0] } else { [0.8, 0.9, 1.0, 1.0] }, None, Snap::Pen);
            put_text(uic, &mut out, FontId::Regular, px, pad + label_w + 18.0 * scale, y + asc + 1.0, &s.hint, [0.62, 0.68, 0.78, 1.0], None, Snap::Pen);
            y += line_h;
        }
        if comp.items.len() > max_sugg { put_text(uic, &mut out, FontId::Regular, px, pad, y + asc, &format!("... {} more (keep typing to narrow)", comp.items.len() - max_sugg), [0.55, 0.6, 0.7, 1.0], None, Snap::Pen); }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lobby_shop::tests::synthetic;

    fn env_parts() -> (Profile, Shop) {
        let shop = Shop::new(synthetic());
        let p = Profile::new(shop.catalog.default_grants());
        (p, shop)
    }
    fn run(c: &mut Console, p: &mut Profile, s: &Shop, line: &str) -> Effects {
        c.set_input(line);
        let mut env = Env { profile: p, shop: s, now: 20_000 * 86_400 + 3_600 };
        c.key(Key::Enter, &mut env)
    }
    fn text(fx: &Effects) -> String { fx.lines.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join(" | ") }

    #[test]
    fn amounts_parse_with_signs_separators_and_suffixes() {
        assert_eq!(parse_amount("1500"), Some(1500));
        assert_eq!(parse_amount("+1,500"), Some(1500));
        assert_eq!(parse_amount("-20k"), Some(-20_000));
        assert_eq!(parse_amount("2m"), Some(2_000_000));
        assert_eq!(parse_amount("1.5k"), Some(1500));
        assert_eq!(parse_amount("abc"), None);
        assert_eq!(parse_amount(""), None);
        assert_eq!(parse_amount("-"), None);
    }

    #[test]
    fn vbucks_commands() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        let fx = run(&mut c, &mut p, &s, "addvbucks 500");
        assert_eq!(p.balance(), 1_000_500);
        assert!(fx.profile_changed && text(&fx).contains("1,000,500"));
        run(&mut c, &mut p, &s, "setvbucks 25");
        assert_eq!(p.balance(), 25);
        let fx = run(&mut c, &mut p, &s, "setvbucks -4");
        assert_eq!(p.balance(), 25);
        assert!(text(&fx).contains("negative"));
        let fx = run(&mut c, &mut p, &s, "addvbucks");
        assert!(text(&fx).contains("usage"));
        run(&mut c, &mut p, &s, "addvbucks 2m");
        assert_eq!(p.balance(), 2_000_025);
    }

    #[test]
    fn item_commands_grant_remove_and_resolve_names() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        assert!(!p.owns("Item_05"));
        let fx = run(&mut c, &mut p, &s, "giveitem item_05");
        assert!(p.owns("Item_05") && fx.profile_changed && fx.shop_changed, "{}", text(&fx));
        let fx = run(&mut c, &mut p, &s, "giveitem Item_05");
        assert!(text(&fx).contains("already owned"));
        let fx = run(&mut c, &mut p, &s, "giveitem Name Item_07");
        assert!(p.owns("Item_07"), "{}", text(&fx));
        let fx = run(&mut c, &mut p, &s, "giveitem Item_1");
        assert!(text(&fx).contains("matches"), "ambiguous: {}", text(&fx));
        let fx = run(&mut c, &mut p, &s, "giveitem nothinglikethis");
        assert!(text(&fx).contains("no item"));
        let fx = run(&mut c, &mut p, &s, "removeitem Item_05");
        assert!(!p.owns("Item_05") && fx.profile_changed, "{}", text(&fx));
        let fx = run(&mut c, &mut p, &s, "removeitem DefaultPickaxe");
        assert!(p.owns("DefaultPickaxe") && text(&fx).contains("default"));
        let before = p.owned_count();
        let fx = run(&mut c, &mut p, &s, "giveall");
        assert!(p.owned_count() > before && fx.profile_changed);
        assert!(s.catalog.items.iter().all(|i| p.owns(&i.id)), "giveall grants everything incl. unsellable items");
        let fx = run(&mut c, &mut p, &s, "giveall banner");
        assert!(text(&fx).contains("usage"));
    }

    #[test]
    fn giveall_by_category() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        run(&mut c, &mut p, &s, "giveall pickaxe");
        assert!(s.catalog.items.iter().filter(|i| i.category == Category::Pickaxe).all(|i| p.owns(&i.id)));
        assert!(s.catalog.items.iter().filter(|i| i.category == Category::Outfit && !p.is_default(&i.id)).all(|i| !p.owns(&i.id)));
    }

    #[test]
    fn setskin_and_equip_follow_the_slot_rules() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        // an outfit id, not owned: setskin grants and equips
        let outfit = s.catalog.items.iter().find(|i| i.category == Category::Outfit && i.sellable).unwrap().id.clone();
        let fx = run(&mut c, &mut p, &s, &format!("setskin {outfit}"));
        assert_eq!(p.equipped(Slot::Outfit), Some(outfit.as_str()), "{}", text(&fx));
        assert!(fx.equip_changed && fx.profile_changed);
        // a glider is not an outfit
        let glider = s.catalog.items.iter().find(|i| i.category == Category::Glider && i.sellable).unwrap().id.clone();
        let fx = run(&mut c, &mut p, &s, &format!("setskin {glider}"));
        assert!(text(&fx).contains("no item"), "{}", text(&fx));
        // equip needs ownership
        let fx = run(&mut c, &mut p, &s, &format!("equip glider {glider}"));
        assert!(text(&fx).contains("not owned"), "{}", text(&fx));
        p.grant(&glider);
        let fx = run(&mut c, &mut p, &s, &format!("equip glider {glider}"));
        assert_eq!(p.equipped(Slot::Glider), Some(glider.as_str()), "{}", text(&fx));
        let fx = run(&mut c, &mut p, &s, "equip banner x");
        assert!(text(&fx).contains("usage"));
    }

    #[test]
    fn rotateshop_shifts_and_resets_the_rotation() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        let env_now = 20_000 * 86_400 + 3_600;
        let before = s.view(&p, env_now);
        let fx = run(&mut c, &mut p, &s, "rotateshop");
        assert_eq!(p.shop_day_offset, 1);
        assert!(fx.shop_changed);
        assert_ne!(s.view(&p, env_now).daily, before.daily);
        run(&mut c, &mut p, &s, "rotateshop 6");
        assert_eq!(p.shop_day_offset, 7);
        run(&mut c, &mut p, &s, "rotateshop -2");
        assert_eq!(p.shop_day_offset, 5);
        run(&mut c, &mut p, &s, "rotateshop reset");
        assert_eq!(p.shop_day_offset, 0);
        assert_eq!(s.view(&p, env_now).daily, before.daily);
        let fx = run(&mut c, &mut p, &s, "rotateshop soon");
        assert!(text(&fx).contains("usage"));
    }

    #[test]
    fn resetprofile_restores_the_defaults() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        run(&mut c, &mut p, &s, "giveall");
        run(&mut c, &mut p, &s, "setvbucks 5");
        run(&mut c, &mut p, &s, "rotateshop 3");
        let fx = run(&mut c, &mut p, &s, "resetprofile");
        assert_eq!((p.balance(), p.shop_day_offset), (1_000_000, 0));
        assert!(fx.equip_changed);
        assert_eq!(p.owned_count(), s.catalog.default_grants().len());
    }

    #[test]
    fn help_unknown_commands_and_info_commands() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        let fx = run(&mut c, &mut p, &s, "help");
        for cmd in COMMANDS.iter() { assert!(text(&fx).contains(cmd.name), "help lists {}", cmd.name); }
        for name in ["help", "addvbucks", "setvbucks", "giveitem", "giveall", "removeitem", "clearitems", "rotateshop", "setskin", "equip", "resetprofile"] { assert!(spec(name).is_some(), "{name} exists"); }
        let fx = run(&mut c, &mut p, &s, "help addvbucks");
        assert!(text(&fx).contains("<amount>"));
        let fx = run(&mut c, &mut p, &s, "addvbuck 5");
        assert!(text(&fx).contains("unknown command") && text(&fx).contains("addvbucks"), "{}", text(&fx));
        let fx = run(&mut c, &mut p, &s, "balance");
        assert!(text(&fx).contains("1,000,000"));
        let fx = run(&mut c, &mut p, &s, "shop");
        assert!(text(&fx).contains("WEEKLY") && text(&fx).contains("DAILY"));
        let fx = run(&mut c, &mut p, &s, "owned outfit");
        assert!(text(&fx).contains("owned item"));
    }

    fn env<'a>(p: &'a mut Profile, s: &'a Shop) -> Env<'a> { Env { profile: p, shop: s, now: 20_000 * 86_400 } }

    #[test]
    fn typing_editing_and_history() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        c.open();
        for ch in "addvbucks 10".chars() { c.key(Key::Char(ch), &mut env(&mut p, &s)); }
        assert_eq!(c.input(), "addvbucks 10");
        c.key(Key::Backspace, &mut env(&mut p, &s));
        assert_eq!(c.input(), "addvbucks 1");
        c.key(Key::Left, &mut env(&mut p, &s));
        c.key(Key::Char('5'), &mut env(&mut p, &s));
        assert_eq!(c.input(), "addvbucks 51");
        c.key(Key::Home, &mut env(&mut p, &s));
        assert_eq!(c.cursor(), 0);
        c.key(Key::Delete, &mut env(&mut p, &s));
        assert_eq!(c.input(), "ddvbucks 51");
        c.key(Key::End, &mut env(&mut p, &s));
        assert_eq!(c.cursor(), c.input().chars().count());
        // non-ASCII input is ignored (the UI atlases are printable ASCII)
        c.key(Key::Char('\u{e9}'), &mut env(&mut p, &s));
        assert_eq!(c.input(), "ddvbucks 51");
        // execute two commands, then walk the history
        c.set_input("addvbucks 1");
        c.key(Key::Enter, &mut env(&mut p, &s));
        assert_eq!(c.input(), "");
        c.set_input("balance");
        c.key(Key::Enter, &mut env(&mut p, &s));
        c.key(Key::Up, &mut env(&mut p, &s));
        assert_eq!(c.input(), "balance");
        c.key(Key::Up, &mut env(&mut p, &s));
        assert_eq!(c.input(), "addvbucks 1");
        c.key(Key::Up, &mut env(&mut p, &s));
        assert_eq!(c.input(), "addvbucks 1", "stays at the oldest entry");
        c.key(Key::Down, &mut env(&mut p, &s));
        assert_eq!(c.input(), "balance");
        c.key(Key::Down, &mut env(&mut p, &s));
        assert_eq!(c.input(), "", "past the newest entry the draft comes back");
        assert!(p.balance() > 1_000_000);
        // Esc closes, F2 toggles
        c.key(Key::Escape, &mut env(&mut p, &s));
        assert!(!c.is_open());
        c.toggle();
        assert!(c.is_open());
        c.key(Key::ToggleConsole, &mut env(&mut p, &s));
        assert!(!c.is_open());
    }

    #[test]
    fn tab_completes_commands_with_arguments_hints() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        c.set_input("addv");
        c.key(Key::Tab, &mut env(&mut p, &s));
        assert_eq!(c.input(), "addvbucks ", "a unique command is completed and prefilled with a space");
        let comp = c.completion(&env(&mut p, &s));
        assert_eq!(comp.hint, "<amount>", "the argument hint is offered");
        assert!(comp.items.iter().any(|i| i.insert == "13500"), "amount presets are the V-Bucks pack sizes");
        // several matches: common prefix, then cycling
        c.set_input("s");
        c.key(Key::Tab, &mut env(&mut p, &s));
        let first = c.input().to_owned();
        assert!(first.starts_with('s') && spec(first.trim()).is_some(), "{first}");
        c.key(Key::Tab, &mut env(&mut p, &s));
        assert_ne!(c.input(), first, "Tab cycles through the matches");
        c.key(Key::ShiftTab, &mut env(&mut p, &s));
        assert_eq!(c.input(), first);
        // a full command word with no arguments typed shows the hint without changing the text
        c.set_input("giveitem");
        let comp = c.completion(&env(&mut p, &s));
        assert_eq!(comp.hint, "<id|name>");
    }

    #[test]
    fn tab_completes_item_ids_and_names() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        c.set_input("giveitem Item_05");
        c.key(Key::Tab, &mut env(&mut p, &s));
        assert_eq!(c.input(), "giveitem Item_05");
        c.set_input("giveitem voy");
        c.key(Key::Tab, &mut env(&mut p, &s));
        assert_eq!(c.input(), "giveitem Glider_Voyager", "a unique name prefix completes to the id");
        // setskin only offers outfits
        c.set_input("setskin ");
        let comp = c.completion(&env(&mut p, &s));
        assert!(!comp.items.is_empty() && comp.items.iter().all(|i| s.catalog.get(&i.insert).unwrap().category == Category::Outfit));
        // the list carries the name / category / rarity hint
        assert!(comp.items[0].hint.contains("Outfit"));
        // equip <slot> <item>: slot first, then owned items of that slot only
        c.set_input("equip gl");
        let comp = c.completion(&env(&mut p, &s));
        assert!(comp.items.iter().any(|i| i.insert == "glider"));
        c.set_input("equip glider ");
        let comp = c.completion(&env(&mut p, &s));
        assert!(comp.items.iter().all(|i| p.owns(&i.insert) && s.catalog.get(&i.insert).unwrap().category == Category::Glider));
        assert!(comp.items.iter().any(|i| i.insert == "DefaultGlider"));
        // removeitem offers owned, non-default items only
        p.grant("Item_03");
        c.set_input("removeitem ");
        let comp = c.completion(&env(&mut p, &s));
        assert_eq!(comp.items.iter().map(|i| i.insert.as_str()).collect::<Vec<_>>(), vec!["Item_03"]);
        // unknown command: nothing to suggest
        c.set_input("frobnicate x");
        assert!(c.completion(&env(&mut p, &s)).items.is_empty());
    }

    #[test]
    fn enter_logs_input_and_result_lines() {
        let (mut p, s) = env_parts();
        let mut c = Console::new();
        c.open();
        c.set_input("addvbucks 7");
        let fx = c.key(Key::Enter, &mut env(&mut p, &s));
        assert!(fx.profile_changed);
        let log: Vec<&str> = c.log().iter().map(|(_, l)| l.as_str()).collect();
        assert!(log.iter().any(|l| *l == "> addvbucks 7"));
        assert!(log.last().unwrap().contains("balance"));
    }

    #[test]
    fn clearitems_leaves_only_the_defaults_so_offers_can_be_bought() {
        let shop = Shop::new(synthetic());
        let mut p = Profile::new(shop.catalog.default_grants());
        let mut c = Console::new();
        run(&mut c, &mut p, &shop, "giveall");
        run(&mut c, &mut p, &shop, "setvbucks 4242");
        let now = 20_000 * 86_400 + 3_600;
        let offer = shop.view(&p, now).daily[0].clone();
        assert!(offer.owned, "everything is owned after giveall: the tile shows the owned state");
        assert!(shop.buy(&mut p, &offer.offer_id, now).is_err());
        let fx = run(&mut c, &mut p, &shop, "clearitems");
        assert!(fx.profile_changed && fx.equip_changed && fx.shop_changed, "{}", text(&fx));
        assert_eq!(p.owned_count(), shop.catalog.default_grants().len());
        assert_eq!(p.balance(), 4242, "V-Bucks untouched");
        let offer = shop.view(&p, now).daily[0].clone();
        assert!(!offer.owned);
        let r = shop.buy(&mut p, &offer.offer_id, now).expect("buyable again");
        assert_eq!(r.balance_after, 4242 - offer.price);
        // removeitem frees a single offer
        run(&mut c, &mut p, &shop, "giveall");
        let id = offer.offer_id.strip_prefix("offer:").unwrap().to_owned();
        run(&mut c, &mut p, &shop, &format!("removeitem {id}"));
        assert!(!p.owns(&id));
    }}
