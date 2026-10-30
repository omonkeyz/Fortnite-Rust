//! Which rows / sub tabs of the original 1.8 OptionsMenu are shown, as the Kismet visibility bytecode decides.
//!
//! RECOVERED (reports/menu-layout-settings.json `visibilityLogic`, decoded by tools/menu-kismet.cjs; the unit test
//! `rules_match_the_decoded_bytecode` re-evaluates that JSON for every platform x sub-game combination and compares it with
//! [`rule`], so this table cannot drift from the evidence):
//! * `IsXboxPlatform()` / `IsPS4Platform()` collapse the PC-only rows (Mouse Sensitivity, Language, Toggle Sprint) on consoles and
//!   (Xbox Safe Zone is gated by the NATIVE `ShouldShowSafeZoneOption()` in 1.8: shown on consoles only, ASSUMED; 1.8 has no Uncap Framerate row);
//! * `IsAthena()` (Battle Royale sub-game) collapses Show Grass, First-Person Camera, Show Head Accessories and Show Backpack
//!   (they are Save the World rows), shows the Controller ADS/Scope sliders only in BR, and the Mouse ADS/Scope sliders in BR on
//!   non-Xbox;
//! * Voice Chat is always shown in 1.8 (no sub-game gate any more); Push To Talk collapses on consoles only;
//! * the 1.8 Game tab's Gamepad ADS Sensitivity is never collapsed (the bytecode's else branch collapses Gamepad Scope Sensitivity twice and never
//!   touches Gamepad ADS Sensitivity, so it stays Visible outside Battle Royale too: a quirk of the original, reproduced);
//! * the sub tab bar of the Game tab (Game | HUD) is collapsed while `IsInZone()` (in a match): only the Game page is built;
//! * Display Resolution is only HitTestInvisible (inert) in Windowed Fullscreen: that is a [`super::Gate`], not a visibility rule.
//!
//! NOT recoverable (native `ShowChatVolume()` / `ShowQuality()` and the tab-level `Show*Options()` checks): those rows/tabs are
//! shown. INFERRED: sub-game value 0 = Save the World ("Voice chat only available in Battle Royale" hover text).
//!
//! The runtime's context (what the original gets from the platform and `FortGlobalUIContext`) defaults to PC / Battle Royale /
//! not in a match (the runtime has no lobby, so the full front-end screen is shown). It can be changed for testing and for
//! reaching the Save the World rows with `[Menu] Platform=Pc|Xbox|PS4`, `SubGame=Athena|Campaign`, `InZone=True` in
//! FortniteSettings.ini (read from the working settings every frame).
use super::rows::{RowDef, SubTab};
use crate::settings::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform { Pc, Xbox, Ps4 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubGame { Campaign, Athena }
impl SubGame {
    /// Numeric value `GetSubGame()` returns (0 collapses the voice rows; INFERRED: 0 = Save the World, 1 = Battle Royale).
    pub fn number(self) -> u8 { match self { SubGame::Campaign => 0, SubGame::Athena => 1 } }
}
static IN_ZONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_in_zone(on: bool) { IN_ZONE.store(on, std::sync::atomic::Ordering::Relaxed); }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuContext { pub platform: Platform, pub subgame: SubGame, pub in_zone: bool }
impl Default for MenuContext { fn default() -> Self { Self { platform: Platform::Pc, subgame: SubGame::Athena, in_zone: false } } }
impl MenuContext {
    pub fn is_athena(&self) -> bool { self.subgame == SubGame::Athena }
    pub fn is_xbox(&self) -> bool { self.platform == Platform::Xbox }
    pub fn is_ps4(&self) -> bool { self.platform == Platform::Ps4 }
    pub fn is_console(&self) -> bool { self.is_xbox() || self.is_ps4() }
    /// Context from the `[Menu]` keys of the settings file (anything unknown keeps the default).
    pub fn from_settings(s: &Settings) -> Self {
        let mut c = Self::default();
        if let Some(v) = s.extra("Menu", "Platform") { c.platform = match v.trim().to_ascii_lowercase().as_str() { "xbox" | "xboxone" => Platform::Xbox, "ps4" | "playstation" => Platform::Ps4, _ => Platform::Pc }; }
        if let Some(v) = s.extra("Menu", "SubGame") { c.subgame = match v.trim().to_ascii_lowercase().as_str() { "campaign" | "stw" | "savetheworld" | "0" => SubGame::Campaign, _ => SubGame::Athena }; }
        c.in_zone = IN_ZONE.load(std::sync::atomic::Ordering::Relaxed);
        if let Some(v) = s.extra("Menu", "InZone") { c.in_zone = matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"); }
        c
    }
}

/// One visibility rule of the original (what the bytecode tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// No visibility logic in the tab's bytecode.
    Always,
    /// Collapsed when `IsPS4Platform() || IsXboxPlatform()`.
    PcOnly,
    /// Visible when `IsPS4Platform() || IsXboxPlatform()`.
    ConsoleOnly,
    /// Visible when `IsXboxPlatform()`.
    XboxOnly,
    /// Decided by the native `ShouldShowSafeZoneOption()`: ASSUMED true on consoles only.
    NativeConsole,
    /// Collapsed when `IsAthena()`.
    NotAthena,
    /// Visible when `IsAthena()` and not Xbox.
    AthenaNotXbox,
    /// Visible when `IsAthena()`.
    AthenaOnly,
    /// Collapsed when `GetSubGame() == 0`.
    VoiceSubGame,
    /// Collapsed when `IsXbox || IsPS4 || GetSubGame() == 0`.
    PushToTalk,
    /// Decided by a native check (`ShowChatVolume()`, `ShowQuality()`) that is not in the pak: shown.
    Native,
}

/// The rule of row `key` on tab `tab` (ids of `layout_table`).
pub fn rule(tab: &str, key: &str) -> Rule {
    match (tab, key) {
        ("video", "ShowGrass") => Rule::NotAthena,
        ("game", "MouseSensitivity") | ("game", "Language") | ("game", "ToggleSprint") => Rule::PcOnly,
        ("game", "MouseTargetingMultiplier") | ("game", "MouseScopedMultiplier") => Rule::AthenaNotXbox,
        ("game", "GamepadScopedMultiplier") => Rule::AthenaOnly,
        ("game", "SafeZone") => Rule::NativeConsole,
        ("game", "First-PersonCamera") | ("game", "ShowHeadAccessories") | ("game", "ShowBackpack") => Rule::NotAthena,
        ("audio", "ChatVolume") | ("audio", "Quality") => Rule::Native,
        ("audio", "VoiceChatPushToTalk") => Rule::PcOnly,
        _ => Rule::Always,
    }
}

pub fn rule_shows(r: Rule, c: &MenuContext) -> bool {
    match r {
        Rule::Always | Rule::Native => true,
        Rule::PcOnly => !c.is_console(),
        Rule::ConsoleOnly => c.is_console(),
        Rule::XboxOnly => c.is_xbox(),
        Rule::NativeConsole => c.is_console(),
        Rule::NotAthena => !c.is_athena(),
        Rule::AthenaNotXbox => c.is_athena() && !c.is_xbox(),
        Rule::AthenaOnly => c.is_athena(),
        Rule::VoiceSubGame => c.subgame.number() != 0,
        Rule::PushToTalk => !c.is_console() && c.subgame.number() != 0,
    }
}
pub fn row_visible(tab: &str, key: &str, c: &MenuContext) -> bool { rule_shows(rule(tab, key), c) }

/// Rows of `sub` (on tab `tab`) that the original shows in context `c`, in the original order.
pub fn visible_rows(tab: &str, sub: &'static SubTab, c: &MenuContext) -> Vec<&'static RowDef> {
    sub.rows.iter().filter(|r| !r.extension && row_visible(tab, r.key, c)).collect()
}
/// Sub tabs of `tab` shown in context `c`: the sub tab bar is collapsed in a match and only the first (Game) page exists.
pub fn visible_subs(tab: &'static super::rows::TabDef, c: &MenuContext) -> Vec<&'static SubTab> {
    if tab.subs.len() > 1 && c.in_zone { tab.subs.iter().take(1).collect() } else { tab.subs.iter().collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::pages::layout_table::TABS;
    use serde_json::Value as J;

    fn all_contexts() -> Vec<MenuContext> {
        let mut v = Vec::new();
        for platform in [Platform::Pc, Platform::Xbox, Platform::Ps4] { for subgame in [SubGame::Campaign, SubGame::Athena] { for in_zone in [false, true] { v.push(MenuContext { platform, subgame, in_zone }); } } }
        v
    }

    // ---- a tiny evaluator for the decoded bytecode expressions of `visibilityLogic` --------------------------------
    #[derive(Debug, Clone, PartialEq)]
    enum Val { B(bool), N(i64) }
    fn split_args(s: &str) -> Vec<&str> {
        let (mut depth, mut start, mut out) = (0i32, 0usize, Vec::new());
        for (i, ch) in s.char_indices() {
            match ch { '(' | '[' | '{' => depth += 1, ')' | ']' | '}' => depth -= 1, ',' if depth == 0 => { out.push(s[start..i].trim()); start = i + 1; } _ => {} }
        }
        if !s[start..].trim().is_empty() { out.push(s[start..].trim()); }
        out
    }
    /// None = the expression involves something the context does not define (native call): the caller skips the row.
    fn eval(e: &str, c: &MenuContext) -> Option<Val> {
        let e = e.trim();
        if let Some(rest) = e.strip_prefix("!(") { return match eval(rest.strip_suffix(')')?, c)? { Val::B(b) => Some(Val::B(!b)), _ => None }; }
        if e.contains("@GetSubGame#") && !e.starts_with("@EqualEqual_ByteByte") && !e.starts_with("@BooleanOR") { return Some(Val::N(c.subgame.number() as i64)); }
        if let Ok(n) = e.parse::<i64>() { return Some(Val::N(n)); }
        if e.starts_with('@') {
            let name_end = e.find('#')?;
            let name = &e[1..name_end];
            let open = e.find('(')?;
            let inner = &e[open + 1..e.rfind(')')?];
            let args = split_args(inner);
            let b = |i: usize| -> Option<bool> { match eval(args.get(i)?, c)? { Val::B(b) => Some(b), _ => None } };
            return match name {
                "IsAthena" => Some(Val::B(c.is_athena())),
                "IsXboxPlatform" => Some(Val::B(c.is_xbox())),
                "IsPS4Platform" => Some(Val::B(c.is_ps4())),
                "BooleanOR" => Some(Val::B(b(0)? || b(1)?)),
                "BooleanAND" => Some(Val::B(b(0)? && b(1)?)),
                "Not_PreBool" => Some(Val::B(!b(0)?)),
                "EqualEqual_ByteByte" => { let (x, y) = (eval(args.get(0)?, c)?, eval(args.get(1)?, c)?); Some(Val::B(x == y)) }
                _ => None,
            };
        }
        None
    }
    fn shown(vis: &str) -> bool { vis != "Collapsed" }

    /// What the decoded bytecode says about row `r` in context `c`: Some(shown) or None when it is decided natively / not a visibility rule.
    fn decoded_shown(r: &J, c: &MenuContext) -> Option<bool> {
        let logic = r.get("visibilityLogic")?.as_array()?;
        let entries: Vec<&J> = logic.iter().collect();
        // inert-only rows (Display Resolution): HitTestInvisible / SelfHitTestInvisible never collapse
        if entries.iter().all(|e| e.get("when_true").and_then(|v| v.as_str()).map_or(false, |s| s.contains("HitTest")) && e.get("when_false").and_then(|v| v.as_str()).map_or(false, |s| s.contains("HitTest"))) { return Some(true); }
        // two-entry shape: [{conditions:[..], ...}, {visibility: Collapsed}] = if conditions { first } else { second }; a single entry has no else branch (stays Visible)
        let first = entries[0];
        if let Some(conds) = first.get("conditions").and_then(|v| v.as_array()) {
            let mut all = true;
            for cd in conds { match eval(cd.as_str()?, c)? { Val::B(b) => all &= b, _ => return None } }
            if all {
                if let Some(on) = first.get("on").and_then(|v| v.as_str()) {
                    let Val::B(v) = eval(on, c)? else { return None };
                    let vis = first.get(if v { "when_true" } else { "when_false" })?.as_str()?;
                    return Some(shown(vis));
                }
                return Some(shown(first.get("visibility")?.as_str()?));
            }
            return match entries.get(1) { Some(e) => Some(shown(e.get("visibility")?.as_str()?)), None => Some(true) };
        }
        // single `on` select by bool or by number
        let on = first.get("on")?.as_str()?;
        let v = eval(on, c)?;
        match v {
            Val::B(b) => Some(shown(first.get(if b { "when_true" } else { "when_false" })?.as_str()?)),
            Val::N(n) => Some(shown(first.get(format!("when_{n}"))?.as_str()?)),
        }
    }

    #[test]
    fn rules_match_the_decoded_bytecode() {
        let j: J = crate::testutil::report_json("menu-layout-settings.json");
        let mut checked = 0;
        let mut native: Vec<String> = Vec::new();
        for t in j["tabs"].as_array().unwrap() {
            let tab = match t["id"].as_str().unwrap() { "Video" => "video", "Game" => "game", "Audio" => "audio", _ => continue };
            let mut rows: Vec<&J> = Vec::new();
            if let Some(r) = t["rows"].as_array() { rows.extend(r.iter()); }
            if let Some(subs) = t["subTabs"].as_array() { for s in subs { if let Some(r) = s["rows"].as_array() { rows.extend(r.iter()); } } }
            for r in rows {
                let key = r["widget"].as_str().unwrap();
                for c in all_contexts() {
                    match decoded_shown(r, &c) {
                        Some(want) => { assert_eq!(row_visible(tab, key, &c), want, "{tab}/{key} in {c:?}"); checked += 1; }
                        None => { if r.get("visibilityLogic").is_some() { if !native.contains(&format!("{tab}/{key}")) { native.push(format!("{tab}/{key}")); } assert!(matches!(rule(tab, key), Rule::Native | Rule::NativeConsole), "{tab}/{key} is decided natively"); } else { assert_eq!(rule(tab, key), Rule::Always, "{tab}/{key} has no logic"); } }
                    }
                }
            }
        }
        assert!(checked > 100, "only {checked} evaluations: the evaluator is not reaching the rows");
        // the only natively decided rows of the three tabs: Voice Chat Volume, the audio Quality row and the Xbox Safe Zone slider
        native.sort();
        assert_eq!(native, vec!["audio/ChatVolume".to_string(), "audio/Quality".to_string(), "game/SafeZone".to_string()]);
    }

    #[test]
    fn pc_battle_royale_context_hides_exactly_the_save_the_world_and_console_rows() {
        let c = MenuContext::default();
        let hidden = |tab: &str, sub: &SubTab| -> Vec<&str> { sub.rows.iter().filter(|r| !row_visible(tab, r.key, &c)).map(|r| r.key).collect() };
        let video = &TABS.iter().find(|t| t.id == "video").unwrap().subs[0];
        assert_eq!(hidden("video", video), vec!["ShowGrass"]);
        let game = &TABS.iter().find(|t| t.id == "game").unwrap().subs[0];
        assert_eq!(hidden("game", game), vec!["SafeZone", "First-PersonCamera", "ShowHeadAccessories", "ShowBackpack"]);
        assert_eq!(visible_rows("game", game, &c).len(), 17);
        let audio = &TABS.iter().find(|t| t.id == "audio").unwrap().subs[0];
        assert!(hidden("audio", audio).is_empty());
        // Save the World shows the grass / first-person / cosmetics rows and drops the controller ADS rows, voice chat and push to talk
        let stw = MenuContext { subgame: SubGame::Campaign, ..c };
        assert!(row_visible("video", "ShowGrass", &stw) && row_visible("game", "First-PersonCamera", &stw) && row_visible("game", "ShowBackpack", &stw));
        assert!(row_visible("game", "GamepadTargetingMultiplier", &stw), "1.8 bytecode quirk: Gamepad ADS Sensitivity is never collapsed");
        assert!(!row_visible("game", "GamepadScopedMultiplier", &stw) && !row_visible("game", "MouseScopedMultiplier", &stw));
        assert!(row_visible("audio", "VoiceChatEnable", &stw) && row_visible("audio", "VoiceChatPushToTalk", &stw), "1.8: voice rows are not sub-game gated");
        assert!(!row_visible("audio", "VoiceChatPushToTalk", &MenuContext { platform: Platform::Xbox, ..c }));
        // a match hides the sub tab bar: only the Game page remains
        let game_tab = TABS.iter().find(|t| t.id == "game").unwrap();
        assert_eq!(visible_subs(game_tab, &c).len(), 2);
        assert_eq!(visible_subs(game_tab, &MenuContext { in_zone: true, ..c }).len(), 1);
    }

    #[test]
    fn context_comes_from_the_menu_section_of_the_settings_file() {
        let mut s = Settings::default();
        assert_eq!(MenuContext::from_settings(&s), MenuContext::default());
        s.set_extra("Menu", "SubGame", "Campaign"); s.set_extra("Menu", "Platform", "Xbox"); s.set_extra("Menu", "InZone", "True");
        assert_eq!(MenuContext::from_settings(&s), MenuContext { platform: Platform::Xbox, subgame: SubGame::Campaign, in_zone: true });
    }
}
