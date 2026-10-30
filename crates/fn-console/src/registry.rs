//! The command table. `cite` names the real 1.8 function a command corresponds to: `UFortCheatManager` (the 1.8 SDK, `FN_FortniteGame_classes.hpp`, 352 native
//! functions) or the engine `UCheatManager` (UE 4.16 `GameFramework/CheatManager.h`). The argument conventions of the port's commands are the port's own (the real
//! ones take engine units / item paths this sandbox does not have); where they differ the `cite` says so. Commands without a `cite` are port-only conveniences.

/// What the argument(s) of a command are, for tab-completion and the usage hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    None,
    /// An item of the catalog (id or display name), optionally followed by a count.
    Item,
    /// Any BR weapon of the weapon runtime's catalog (id, WID path or display name).
    Weapon,
    /// A firearm of the catalog, optionally followed by a count.
    Gun,
    Skin, Pickaxe, Backbling, Glider,
    /// A `list` topic, then a free filter.
    Topic,
    /// A command name (`help`).
    Command,
    /// A fixed set of words.
    Values(&'static [&'static str]),
    /// A number (usage hint only).
    Number,
}

#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub help: &'static str,
    pub arg: Arg,
    pub cite: &'static str,
}

pub const LIST_TOPICS: &[&str] = &["skins", "pickaxes", "backblings", "gliders", "weapons", "consumables", "resources", "items", "commands"];
const ONOFF: &[&str] = &["on", "off"];
const STORM_ARGS: &[&str] = &["on", "off", "reset", "here"];
pub const DIE_ARGS: &[&str] = &["storm", "fall", "self", "banhammer"];

pub const COMMANDS: &[Command] = &[
    Command { name: "help", aliases: &["listcheats", "?"], usage: "[command]", help: "list the commands, or describe one", arg: Arg::Command, cite: "UFortCheatManager::ListCheats" },
    Command { name: "clear", aliases: &["cls"], usage: "", help: "clear the log", arg: Arg::None, cite: "" },
    Command { name: "list", aliases: &["ls"], usage: "<skins|pickaxes|backblings|gliders|weapons|consumables|resources|items|commands> [filter]", help: "list what exists (cosmetics: every shipped BR item of the paks; items: what this sandbox can hold)", arg: Arg::Topic, cite: "list weapons = UFortCheatManager::ListWeapons" },
    Command { name: "listweapons", aliases: &[], usage: "", help: "same as list weapons", arg: Arg::None, cite: "UFortCheatManager::ListWeapons" },
    Command { name: "give", aliases: &["giveweapon", "givespecificitem"], usage: "<item|weapon> [count] [equip]", help: "put an item in the player's inventory (any of the BR weapons by display name / WID: on the quickbar with a full magazine, `equip` selects it; consumable / resource: +count)", arg: Arg::Item, cite: "UFortCheatManager::GiveWeapon(WeaponName, RequestedLevel, Count) / GiveSpecificItem(ItemPath, Amount, Level); here the argument is the id or display name and the level is dropped" },
    Command { name: "equip", aliases: &[], usage: "<weapon>", help: "select a weapon (display name or WID); one that is not on the quickbar is given first", arg: Arg::Weapon, cite: "port-only: the 1.8 quickbar select is client input" },
    Command { name: "spawn", aliases: &["summon"], usage: "<item> [count]", help: "drop a real pickup actor of any BR item (weapon, consumable, ammo, resource, trap) in front of the player: it is tossed, bounces, lies there with the rarity effects and is picked up with E (ammo / resources / traps also by walking into them)", arg: Arg::Item, cite: "UFortKismetLibrary::K2_SpawnPickupInWorld(ItemDefinition, NumberToSpawn, Position, Direction, bToss) (the client exe has only a stub: the spawn side is server code)" },
    Command { name: "ammo", aliases: &["giveammo"], usage: "[gun] [count]", help: "ammo for one firearm (or all): count rounds, or a full stack without a count", arg: Arg::Gun, cite: "UFortCheatManager::GiveAmmo(AmmoNameAndAmmount)" },
    Command { name: "giveathenaweapons", aliases: &[], usage: "", help: "refill and keep every firearm of the loadout", arg: Arg::None, cite: "UFortCheatManager::GiveAthenaWeapons" },
    Command { name: "giveathenaconsumables", aliases: &[], usage: "", help: "a full stack of every healing consumable", arg: Arg::None, cite: "UFortCheatManager::GiveAthenaConsumables" },
    Command { name: "giveathenaammo", aliases: &[], usage: "", help: "a full ammo stack for every firearm", arg: Arg::None, cite: "UFortCheatManager::GiveAthenaAmmo" },
    Command { name: "giveathenafullkit", aliases: &[], usage: "", help: "weapons + consumables + ammo", arg: Arg::None, cite: "UFortCheatManager::GiveAthenaFullKit" },
    Command { name: "givewood", aliases: &[], usage: "<amount>", help: "wood (the HUD counter caps at 999)", arg: Arg::Number, cite: "UFortCheatManager::GiveWood" },
    Command { name: "givestone", aliases: &[], usage: "<amount>", help: "stone", arg: Arg::Number, cite: "UFortCheatManager::GiveStone" },
    Command { name: "givemetal", aliases: &[], usage: "<amount>", help: "metal", arg: Arg::Number, cite: "UFortCheatManager::GiveMetal" },
    Command { name: "giveresources", aliases: &[], usage: "<amount>", help: "wood, stone and metal", arg: Arg::Number, cite: "UFortCheatManager::GiveResources" },
    Command { name: "heal", aliases: &[], usage: "[amount]", help: "health to the maximum, or +amount", arg: Arg::Number, cite: "" },
    Command { name: "shield", aliases: &[], usage: "[amount]", help: "shield to the maximum, or +amount", arg: Arg::Number, cite: "" },
    Command { name: "sethealthpercent", aliases: &[], usage: "<0-100>", help: "health as a percentage of the maximum", arg: Arg::Number, cite: "UFortCheatManager::SetHealthPercent" },
    Command { name: "god", aliases: &["toggleunlimitedhealth"], usage: "[on|off]", help: "health is held at the maximum (nothing in this sandbox deals damage yet; use sethealthpercent to test healing)", arg: Arg::Values(ONOFF), cite: "UFortCheatManager::ToggleUnlimitedHealth; engine UCheatManager::God" },
    Command { name: "infiniteammo", aliases: &["toggleinfiniteammo"], usage: "[on|off]", help: "magazines and reserves are refilled every frame", arg: Arg::Values(ONOFF), cite: "UFortCheatManager::ToggleInfiniteAmmo" },
    Command { name: "tp", aliases: &[], usage: "<x> <y> <z> | spawn", help: "teleport the feet to x y z (metres, the coordinates pos prints; y = height above the ground there, the landscape or the plate); spawn = back to the start", arg: Arg::Number, cite: "UFortCheatManager::TeleportToLocation takes engine centimetres (X, Y, Z-up); the sandbox axes are not mapped to those, so the units are the port's" },
    Command { name: "face", aliases: &[], usage: "<yaw> [pitch]", help: "set the view yaw / pitch in degrees (the angles pos prints)", arg: Arg::Number, cite: "" },
    Command { name: "pos", aliases: &["where"], usage: "", help: "print the feet position, view angles and velocity", arg: Arg::None, cite: "engine UCheatManager::LogLoc" },
    Command { name: "respawn", aliases: &["respawnplayer"], usage: "", help: "back to the spawn with a fresh movement controller", arg: Arg::None, cite: "UFortCheatManager::RespawnPlayer" },
    Command { name: "speed", aliases: &[], usage: "[scale]", help: "scale the run / sprint / crouch speeds (1 = recovered values); without an argument shows the scale", arg: Arg::Number, cite: "" },
    Command { name: "storm", aliases: &["safezone"], usage: "[radius_metres | on | off | reset | here]", help: "the BR storm: set the (static) safe-zone radius in metres, hide / show it (on replays the 25 s fade-in), here = centre the zone on the player, reset = the first phase (1500 m around the map centre); no argument shows the zone", arg: Arg::Values(STORM_ARGS), cite: "UFortCheatManager::SetSafeZoneRadius (the real one takes engine units; the port takes metres)" },
    Command { name: "gravity", aliases: &[], usage: "[scale|reset]", help: "gravity scale of the movement component (fn_movement Tuning.gravity_scale, 1 = the project gravity -2800 cm/s^2)", arg: Arg::Number, cite: "the FNRT_GRAVITY_SCALE experiment knob" },
    Command { name: "jump", aliases: &["jumpz"], usage: "[cm/s|reset]", help: "jump velocity (fn_movement Tuning.jump_z_velocity, recovered value 600 cm/s)", arg: Arg::Number, cite: "the FNRT_JUMPZ experiment knob" },
    Command { name: "skin", aliases: &["outfit"], usage: "<id|name>", help: "change the player's outfit to any BR skin of the paks", arg: Arg::Skin, cite: "" },
    Command { name: "pickaxe", aliases: &[], usage: "<id|name>", help: "change the harvesting tool", arg: Arg::Pickaxe, cite: "" },
    Command { name: "backbling", aliases: &[], usage: "<id|name>", help: "change the back bling", arg: Arg::Backbling, cite: "" },
    Command { name: "glider", aliases: &[], usage: "<id|name>", help: "change the glider", arg: Arg::Glider, cite: "" },
    Command { name: "fly", aliases: &[], usage: "", help: "fly along the view (jump up, crouch down, sprint fast), stays above the landscape; walk to land", arg: Arg::None, cite: "engine UCheatManager::Fly (port-only free flight)" },
    Command { name: "ghost", aliases: &["noclip"], usage: "", help: "fly through everything (no collision); walk to land", arg: Arg::None, cite: "engine UCheatManager::Ghost (port-only free flight)" },
    Command { name: "walk", aliases: &[], usage: "", help: "back to walking after fly / ghost", arg: Arg::None, cite: "engine UCheatManager::Walk" },
    Command { name: "addkillfeedmessage", aliases: &["killfeed"], usage: "", help: "five test lines in the elimination feed (every name style)", arg: Arg::None, cite: "UFortCheatManager::AddKillFeedMessage" },
    Command { name: "startbus", aliases: &["startaircraft"], usage: "", help: "end any warmup and put the player on a fresh Battle Bus at the start of its flight", arg: Arg::None, cite: "AFortGameModeAthena exec STARTAIRCRAFT -> StartAircraftPhase (FUN_140407e40)" },
    Command { name: "playemote", aliases: &[], usage: "<emote>", help: "play any emote item of the paks (dance, consumable emote, victory pose) by id or name, bypassing the picker", arg: Arg::None, cite: "port-only: AFortPlayerController::ServerPlayEmoteItem" },
    Command { name: "win", aliases: &[], usage: "", help: "the local player wins the match (Victory Royale screen)", arg: Arg::None, cite: "port-only: AthenaHUDContext.OnLocalPlayerWon -> AthenaGameOverScreen On Player Won" },
    Command { name: "die", aliases: &["eliminate"], usage: "<storm|fall|self|banhammer>", help: "eliminate the player now with that death cause (health 0 + the elimination feed line)", arg: Arg::Values(DIE_ARGS), cite: "port-only: forces AFortPlayerStateAthena::DeathInfo (OnRep_DeathInfo builds the feed line)" },
];

/// The command with this name or alias (case-insensitive).
pub fn find(name: &str) -> Option<&'static Command> {
    let n = name.to_ascii_lowercase();
    COMMANDS.iter().find(|c| c.name == n).or_else(|| COMMANDS.iter().find(|c| c.aliases.iter().any(|a| *a == n)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn names_and_aliases_are_unique_and_found() {
        let mut seen = std::collections::HashSet::new();
        for c in COMMANDS { assert!(seen.insert(c.name), "{}", c.name); for a in c.aliases { assert!(seen.insert(*a), "alias {a}"); } }
        assert_eq!(find("ToggleUnlimitedHealth").unwrap().name, "god"); assert_eq!(find("GIVE").unwrap().name, "give"); assert!(find("nope").is_none());
    }
}
