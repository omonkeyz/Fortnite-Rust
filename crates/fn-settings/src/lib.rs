//! Settings model, persistence and apply layer.
//!
//! What is RECOVERED vs ASSUMED
//! * The *set of settings*, their labels, tooltips, option lists, slider ranges and the key-binding action names come from the
//!   1.8 pak (settings widget blueprints, Game.locres namespaces Fort.InputOptions / Fortnite.VideoOptions, SoundClass volumes).
//!   `tools/settings-evidence.cjs` verifies every such claim against the pak and generates `src/settings_table.rs`
//!   (this module's descriptor table) and `reports/settings-catalog.json`.
//! * Defaults: the key defaults are RECOVERED from `FortniteGame/CookedIniVersion.txt` (unencrypted, pakchunk0: the ini values the cooker
//!   saw): `FortPlayerInput:DefaultPresetNameAthena=ConfigD_Athena` (L2206) and that preset's KBM ActionMappings (L2627-2761) plus the
//!   `InputSettings:AxisMappings` (L1987-1995); the generator reads them from the pak. Mouse sensitivity and the ADS/scope multipliers are
//!   RECOVERED from the exe constructors (see the entries). Every other default is ASSUMED (DefaultGameUserSettings is not in that list);
//!   quality defaults are Epic because that is what the runtime already rendered.
//! * A setting is `Support::Wired` only when it changes runtime behaviour today; everything else is `Support::StoredOnly`
//!   (persisted and validated, no effect yet): anti-aliasing, textures, effects, 3D resolution, motion blur, grass, audio, etc.
//!
//! Layers: (1) typed [`Settings`] + generic by-id access with validation/clamping and change events ([`Change`]);
//! (2) UI model: [`entries`] gives every setting's label/kind/range/step/default/live-vs-restart/support so menu tabs can be
//! generated; (3) INI persistence ([`Settings::load`]/[`Settings::save`]); (4) apply layer ([`Settings::apply_video`] ...) that diffs
//! against what was last applied and calls hook traits, implemented for the live renderer by [`LiveVideo`].
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub mod version;
pub mod extras;
mod platform;
pub use platform::*;
mod table;
pub use table::*;

pub const VERSION: u32 = 3;
pub const FILE_NAME: &str = "FortniteSettings.ini";
const MAX_FILE_BYTES: usize = 1 << 20;

// ------------------------------------------------------------------------------------------------------------------
// Descriptor model (UI-facing)
// ------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Apply { Live, Restart }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support { Wired, StoredOnly }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KindDef { Toggle, Slider { min: f32, max: f32, step: f32, decimals: u8 }, Enum(&'static [(&'static str, &'static str)]), Resolution, Keybind }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Def { Bool(bool), Float(f32), Index(usize), Size(u32, u32), Keys(&'static str, &'static str) }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value { Bool(bool), Float(f32), Index(usize), Size(u32, u32), Keys([Key; 2]) }

/// One setting as the menu needs to see it. `recovered` = the setting and its label exist in the 1.8 data;
/// `options_recovered` / `range_recovered` say the same for the option list / slider range; `default_recovered` = the default value is
/// recovered (key bindings from CookedIniVersion.txt ConfigD_Athena, mouse values from the exe constructors).
#[derive(Debug)]
pub struct Entry {
    pub id: &'static str, pub section: &'static str, pub tab: &'static str, pub ini_key: &'static str,
    pub label: &'static str, pub tooltip: &'static str, pub kind: KindDef, pub default: Def,
    pub apply: Apply, pub support: Support, pub wired_to: &'static str,
    pub recovered: bool, pub options_recovered: bool, pub range_recovered: bool, pub default_recovered: bool, pub note: &'static str,
}
/// One key-binding row. `name` is the Fort.InputOptions key (the row label's id), `input_action` the 1.8 input action / axis it binds
/// (`ConfigD_Athena` ActionMappings / `InputSettings` AxisMappings name), `group` its `EFortInputActionGroup` from the FIRST
/// `FortPlayerInput:InputActionGroupContexts` entry naming it (CookedIniVersion.txt L2216-2240; 1 Combat, 2 Building, 3 Edit, 4 Death,
/// 5 Cinematic; 0 = no entry).
#[derive(Debug)]
pub struct ActionDef { pub name: &'static str, pub label: &'static str, pub primary: &'static str, pub secondary: &'static str, pub wired: bool, pub input_action: &'static str, pub group: u8 }

#[derive(Clone, Debug, PartialEq)]
pub enum SetError { UnknownId(String), TypeMismatch { id: &'static str }, NotFinite { id: &'static str }, OutOfRange { id: &'static str }, Parse { id: &'static str, text: String, why: String } }
impl std::fmt::Display for SetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetError::UnknownId(i) => write!(f, "unknown setting '{i}'"),
            SetError::TypeMismatch { id } => write!(f, "wrong value type for '{id}'"),
            SetError::NotFinite { id } => write!(f, "'{id}' must be a finite number"),
            SetError::OutOfRange { id } => write!(f, "option index out of range for '{id}'"),
            SetError::Parse { id, text, why } => write!(f, "cannot parse '{text}' for '{id}': {why}"),
        }
    }
}

pub fn entries() -> &'static [Entry] { table::ENTRIES }
pub fn actions() -> &'static [ActionDef] { table::ACTIONS }
pub fn entry(id: &str) -> Option<&'static Entry> {
    static MAP: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    MAP.get_or_init(|| entries().iter().enumerate().map(|(i, e)| (e.id, i)).collect()).get(id).map(|&i| &entries()[i])
}
/// Tab order of the original options screen (OptionsMenu strings: Video, Game, Brightness, Audio, Input, Controller).
pub const TABS: [&str; 6] = ["Video", "Game", "Brightness", "Audio", "Input", "Controller"];
pub fn entries_for_tab(tab: &str) -> impl Iterator<Item = &'static Entry> + '_ { entries().iter().filter(move |e| e.tab == tab) }
fn ini_section(e: &Entry) -> &'static str { if e.id.starts_with("bind.") { "Bindings" } else { e.section } }

impl Def {
    pub fn value(&self) -> Value {
        match *self {
            Def::Bool(b) => Value::Bool(b), Def::Float(f) => Value::Float(f), Def::Index(i) => Value::Index(i), Def::Size(w, h) => Value::Size(w, h),
            Def::Keys(a, b) => Value::Keys([Key::from_name(a).expect("table key"), Key::from_name(b).expect("table key")]),
        }
    }
}
/// Smallest accepted resolution (client size).
pub const MIN_RES: (u32, u32) = (640, 360);
/// Largest accepted resolution per axis. 8192 is the `max_texture_dimension_2d` every wgpu adapter in the supported set offers (wgpu's default
/// limit): a swap chain / HDR target larger than that is a validation error, so anything above (an ini `Resolution=9000x720`, a script `resize`)
/// is clamped here, at load time; `fn-app` clamps again to the real adapter limit (`gfx.rs`) and repairs the ini (`settings_guard.rs`).
pub const MAX_RES: (u32, u32) = (8192, 8192);

impl Entry {
    pub fn default_value(&self) -> Value { self.default.value() }
    /// Validates the type and clamps numbers into range. Enum indices out of range are an error (never silently remapped).
    pub fn sanitize(&self, v: Value) -> Result<Value, SetError> {
        Ok(match (self.kind, v) {
            (KindDef::Toggle, Value::Bool(_)) => v,
            (KindDef::Slider { min, max, .. }, Value::Float(f)) => { if !f.is_finite() { return Err(SetError::NotFinite { id: self.id }); } Value::Float(f.clamp(min, max)) }
            (KindDef::Enum(o), Value::Index(i)) => { if i >= o.len() { return Err(SetError::OutOfRange { id: self.id }); } v }
            (KindDef::Resolution, Value::Size(w, h)) => Value::Size(w.clamp(MIN_RES.0, MAX_RES.0), h.clamp(MIN_RES.1, MAX_RES.1)),
            (KindDef::Keybind, Value::Keys(_)) => v,
            _ => return Err(SetError::TypeMismatch { id: self.id }),
        })
    }
    pub fn text(&self, v: &Value) -> String {
        match (self.kind, v) {
            (_, Value::Bool(b)) => if *b { "True".into() } else { "False".into() },
            (_, Value::Float(f)) => format!("{f}"),
            (KindDef::Enum(o), Value::Index(i)) => o.get(*i).map(|x| x.0.to_string()).unwrap_or_else(|| i.to_string()),
            (_, Value::Index(i)) => i.to_string(),
            (_, Value::Size(w, h)) => format!("{w}x{h}"),
            (_, Value::Keys(k)) => format!("{},{}", k[0].name(), k[1].name()),
        }
    }
    /// Human display text for menus (slider honours `decimals`, enums show their recovered label).
    pub fn display(&self, v: &Value) -> String {
        match (self.kind, v) {
            (KindDef::Slider { decimals, .. }, Value::Float(f)) => format!("{f:.*}", decimals as usize),
            (KindDef::Enum(o), Value::Index(i)) => o.get(*i).map(|x| x.1.to_string()).unwrap_or_default(),
            (KindDef::Toggle, Value::Bool(b)) => if *b { "On".into() } else { "Off".into() },
            (KindDef::Resolution, Value::Size(w, h)) => format!("{w}x{h}"),
            (KindDef::Keybind, Value::Keys(k)) => if k[1] == Key::NONE { k[0].name().into() } else { format!("{} / {}", k[0].name(), k[1].name()) },
            _ => self.text(v),
        }
    }
    pub fn parse(&self, text: &str) -> Result<Value, String> {
        let t = text.trim();
        match self.kind {
            KindDef::Toggle => match t.to_ascii_lowercase().as_str() { "true" | "1" | "on" | "yes" => Ok(Value::Bool(true)), "false" | "0" | "off" | "no" => Ok(Value::Bool(false)), _ => Err("expected True/False".into()) },
            KindDef::Slider { .. } => t.parse::<f32>().map_err(|e| e.to_string()).and_then(|f| if f.is_finite() { Ok(Value::Float(f)) } else { Err("not finite".into()) }),
            // "low" is accepted for Anti-Aliasing: the saved files of earlier builds used that id for what the original calls "off"
            KindDef::Enum(o) => o.iter().position(|(id, label)| id.eq_ignore_ascii_case(t) || label.eq_ignore_ascii_case(t) || (self.id == "video.anti_aliasing" && *id == "off" && t.eq_ignore_ascii_case("low"))).map(Value::Index).ok_or_else(|| format!("expected one of {}", o.iter().map(|x| x.0).collect::<Vec<_>>().join("/"))),
            KindDef::Resolution => {
                let (a, b) = t.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT")?;
                Ok(Value::Size(a.trim().parse::<u32>().map_err(|e| e.to_string())?, b.trim().parse::<u32>().map_err(|e| e.to_string())?))
            }
            KindDef::Keybind => {
                let mut it = t.splitn(2, ',');
                let k = |s: Option<&str>| -> Result<Key, String> { match s.map(str::trim) { None | Some("") => Ok(Key::NONE), Some(n) => Key::from_name(n).ok_or_else(|| format!("unknown key '{n}'")) } };
                Ok(Value::Keys([k(it.next())?, k(it.next())?]))
            }
        }
    }
    /// Alternate legal value, used by round-trip tests and the self-test.
    pub fn sample_alternative(&self) -> Value {
        let cur = self.default_value();
        match (self.kind, cur) {
            (_, Value::Bool(b)) => Value::Bool(!b),
            (KindDef::Slider { min, max, .. }, Value::Float(f)) => Value::Float(if (f - max).abs() > (f - min).abs() { max } else { min }),
            (KindDef::Enum(o), Value::Index(i)) => Value::Index((i + 1) % o.len()),
            (_, Value::Size(w, h)) => Value::Size(w + 160, h + 90),
            (_, Value::Keys(k)) => Value::Keys([if k[0] == Key::from_name("F12").unwrap() { Key::from_name("F11").unwrap() } else { Key::from_name("F12").unwrap() }, Key::from_name("Insert").unwrap()]),
            (_, v) => v,
        }
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Keys
// ------------------------------------------------------------------------------------------------------------------

/// Unreal-style key name (FKey naming: `LeftMouseButton`, `SpaceBar`, `One`, `F1` ...) as an index; 0 = unbound.
/// Names follow UE4 conventions (the pak's InputActionDataTable confirms `LeftMouseButton`, `Escape`, `F10` etc.);
/// the rest of the table is ASSUMED to follow the same convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key(pub u8);
impl Key {
    pub const NONE: Key = Key(0);
    fn table() -> &'static Vec<(String, u16)> {
        static T: OnceLock<Vec<(String, u16)>> = OnceLock::new();
        T.get_or_init(|| {
            let mut t: Vec<(String, u16)> = Vec::new();
            let mut p = |n: &str, v: u16| t.push((n.to_string(), v));
            for (n, v) in [("LeftMouseButton", 0x01), ("RightMouseButton", 0x02), ("MiddleMouseButton", 0x04), ("ThumbMouseButton", 0x05), ("ThumbMouseButton2", 0x06), ("MouseScrollUp", 0), ("MouseScrollDown", 0),
                ("SpaceBar", 0x20), ("Tab", 0x09), ("Enter", 0x0D), ("Escape", 0x1B), ("BackSpace", 0x08), ("CapsLock", 0x14), ("LeftShift", 0xA0), ("RightShift", 0xA1), ("LeftControl", 0xA2), ("RightControl", 0xA3), ("LeftAlt", 0xA4), ("RightAlt", 0xA5),
                ("Up", 0x26), ("Down", 0x28), ("Left", 0x25), ("Right", 0x27), ("PageUp", 0x21), ("PageDown", 0x22), ("End", 0x23), ("Home", 0x24), ("Insert", 0x2D), ("Delete", 0x2E),
                ("Semicolon", 0xBA), ("Equals", 0xBB), ("Comma", 0xBC), ("Hyphen", 0xBD), ("Period", 0xBE), ("Slash", 0xBF), ("Tilde", 0xC0), ("LeftBracket", 0xDB), ("Backslash", 0xDC), ("RightBracket", 0xDD), ("Quote", 0xDE),
                ("Multiply", 0x6A), ("Add", 0x6B), ("Subtract", 0x6D), ("Decimal", 0x6E), ("Divide", 0x6F),
                // Appended (never reorder: key indices are only used in memory, names are what is persisted).
                ("Pause", 0x13), ("NumLock", 0x90), ("ScrollLock", 0x91)] { p(n, v); }
            for c in b'A'..=b'Z' { p(&(c as char).to_string(), c as u16); }
            for (i, n) in ["Zero", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine"].iter().enumerate() { p(n, 0x30 + i as u16); p(&format!("NumPad{n}"), 0x60 + i as u16); }
            for i in 1..=12u16 { p(&format!("F{i}"), 0x6F + i); }
            t
        })
    }
    pub fn from_name(n: &str) -> Option<Key> {
        static M: OnceLock<HashMap<String, u8>> = OnceLock::new();
        let t = n.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("none") { return Some(Key::NONE); }
        M.get_or_init(|| Key::table().iter().enumerate().map(|(i, (n, _))| (n.to_ascii_lowercase(), i as u8 + 1)).collect()).get(&t.to_ascii_lowercase()).map(|&i| Key(i))
    }
    pub fn name(&self) -> &'static str { if self.0 == 0 { "None" } else { Key::table().get(self.0 as usize - 1).map(|x| x.0.as_str()).unwrap_or("None") } }
    /// Windows virtual-key code for polling, None for unbound keys and the mouse wheel (not pollable).
    pub fn vk(&self) -> Option<u16> { if self.0 == 0 { None } else { Key::table().get(self.0 as usize - 1).map(|x| x.1).filter(|&v| v != 0) } }
    pub fn all_names() -> impl Iterator<Item = &'static str> { Key::table().iter().map(|x| x.0.as_str()) }
    /// Code the input poller asks about: the Windows virtual key, or a pseudo code for the mouse wheel (which has no key state;
    /// main.rs answers `WHEEL_UP_CODE`/`WHEEL_DOWN_CODE` from the wheel notches seen since the previous frame). None when unbound.
    pub fn poll_code(&self) -> Option<u16> {
        match self.name() { "MouseScrollUp" => Some(WHEEL_UP_CODE), "MouseScrollDown" => Some(WHEEL_DOWN_CODE), _ => self.vk() }
    }
    /// The bindable key a Windows virtual-key code stands for (left/right modifier codes, mouse buttons, digits, letters, F1-F12 ...).
    /// None for keys the binding table does not know (Windows keys, media keys ...).
    pub fn from_vk(vk: u16) -> Option<Key> {
        static M: OnceLock<HashMap<u16, u8>> = OnceLock::new();
        M.get_or_init(|| { let mut m = HashMap::new(); for (i, (_, v)) in Key::table().iter().enumerate() { if *v != 0 { m.entry(*v).or_insert(i as u8 + 1); } } m }).get(&vk).map(|&i| Key(i))
    }
}
/// Pseudo virtual-key codes (0x0E/0x0F are unassigned by Windows) used for wheel bindings; see [`Key::poll_code`].
pub const WHEEL_UP_CODE: u16 = 0x0E;
pub const WHEEL_DOWN_CODE: u16 = 0x0F;

#[derive(Clone, Debug, PartialEq)]
pub struct Bindings { pub keys: Vec<[Key; 2]> }
impl Default for Bindings {
    fn default() -> Self { Bindings { keys: actions().iter().map(|a| [Key::from_name(a.primary).unwrap(), Key::from_name(a.secondary).unwrap()]).collect() } }
}
impl Bindings {
    pub fn get(&self, action: usize) -> [Key; 2] { self.keys[action] }
    /// True if either binding of `action` is currently down according to `poll(virtual_key)`.
    pub fn down(&self, action: usize, poll: impl Fn(u16) -> bool) -> bool { self.keys[action].iter().filter_map(|k| k.poll_code()).any(poll) }
    pub fn action_index(name: &str) -> Option<usize> { actions().iter().position(|a| a.name.eq_ignore_ascii_case(name)) }
    /// 1.8 `FUN_1405fc190` (UFortPlayerInput, the predicate of the keybind screen): may actions `a` and `b` NOT share a key? They may share
    /// one only when both have an `InputActionGroupContexts` group and the groups differ (R = Reload [Combat] + Rotate [Building],
    /// RightMouseButton = Target / ChangeMaterial / BuildingEditReset / Respawn, LeftMouseButton = Fire / Resurrect, SpaceBar = Jump / Skip Cutscene).
    pub fn exclusive(a: usize, b: usize) -> bool {
        let (ga, gb) = (actions()[a].group, actions()[b].group);
        !(ga != 0 && gb != 0 && ga != gb)
    }
    /// Keys shared by actions that 1.8 keeps exclusive ([`Bindings::exclusive`]). The 1.8 keybind screen never shows duplicates: when a key is
    /// bound (`UFortInputOptions::ChangeBinding` @142edfa80 -> `FUN_140542480`), every OTHER row holding that key in its primary or secondary
    /// slot is cleared when the pair is exclusive, and kept when the groups differ (see [`Bindings::change_binding`]). Defaults share keys on purpose.
    pub fn conflicts(&self) -> Vec<(Key, Vec<usize>)> {
        let mut m: HashMap<Key, Vec<usize>> = HashMap::new();
        for (i, ks) in self.keys.iter().enumerate() { for k in ks { if *k != Key::NONE { let v = m.entry(*k).or_default(); if !v.contains(&i) { v.push(i); } } } }
        let mut out: Vec<_> = m.into_iter().filter(|(_, v)| v.iter().enumerate().any(|(i, &a)| v[i + 1..].iter().any(|&b| Bindings::exclusive(a, b)))).collect();
        out.sort_by_key(|(k, _)| k.0);
        out
    }
    /// `UFortInputOptions::ChangeBinding` @142edfa80 + `FUN_140542480` @140542480: put `key` on `slot` (0 = KeyBind1, 1 = KeyBind2) of
    /// `action`; a valid key is then removed (set to None) from both slots of every other row for which [`Bindings::exclusive`] holds.
    /// No swap, no message. Returns the rows that lost the key.
    pub fn change_binding(&mut self, action: usize, slot: usize, key: Key) -> Vec<usize> {
        self.keys[action][slot.min(1)] = key;
        let mut cleared = vec![];
        if key == Key::NONE { return cleared; }
        for b in 0..self.keys.len() {
            if b == action || !Bindings::exclusive(action, b) { continue; }
            let mut hit = false;
            for s in 0..2 { if self.keys[b][s] == key { self.keys[b][s] = Key::NONE; hit = true; } }
            if hit { cleared.push(b); }
        }
        cleared
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Typed settings
// ------------------------------------------------------------------------------------------------------------------

pub trait IndexEnum: Sized { fn index(&self) -> usize; fn from_index(i: usize) -> Self; }
macro_rules! index_enum { ($t:ident { $($v:ident),+ }) => { impl IndexEnum for $t {
    fn index(&self) -> usize { *self as usize }
    fn from_index(i: usize) -> Self { const ALL: &[$t] = &[$($t::$v),+]; ALL[i.min(ALL.len() - 1)] } } } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum WindowMode { Fullscreen, WindowedFullscreen, Windowed }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum FrameRateLimit { Fps30, Fps60, Fps120, Fps144, Fps165, Fps240, Unlimited }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Preset { Low, Medium, High, Epic, Auto, Custom }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum CrouchMode { Hold, Toggle }
index_enum!(WindowMode { Fullscreen, WindowedFullscreen, Windowed });
index_enum!(FrameRateLimit { Fps30, Fps60, Fps120, Fps144, Fps165, Fps240, Unlimited });
index_enum!(Preset { Low, Medium, High, Epic, Auto, Custom });
index_enum!(CrouchMode { Hold, Toggle });
impl FrameRateLimit { /// 0 = unlimited.
    pub fn fps(self) -> u32 { match self { FrameRateLimit::Fps30 => 30, FrameRateLimit::Fps60 => 60, FrameRateLimit::Fps120 => 120, FrameRateLimit::Fps144 => 144, FrameRateLimit::Fps165 => 165, FrameRateLimit::Fps240 => 240, FrameRateLimit::Unlimited => 0 } } }

/// Quality group level 0..=3 (off/near/low .. epic; the label per group is in the descriptor options).
pub type Level = u8;

#[derive(Clone, Debug, PartialEq)]
pub struct Video {
    pub window_mode: WindowMode, pub width: u32, pub height: u32, pub vsync: bool, pub frame_rate_limit: FrameRateLimit,
    pub preset: Preset, pub resolution_quality: Level, pub view_distance: Level, pub shadows: Level, pub anti_aliasing: Level, pub textures: Level, pub effects: Level, pub post_processing: Level,
    pub motion_blur: bool, pub show_grass: bool, pub show_fps: bool, pub brightness: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub mouse_sensitivity: f32, pub mouse_sensitivity_y: f32, pub mouse_ads_multiplier: f32, pub mouse_scope_multiplier: f32, pub invert_y: bool, pub crouch_mode: CrouchMode, pub toggle_sprint: bool,
    pub sprint_cancels_reload: bool, pub tap_to_interact: bool, pub toggle_targeting: bool, pub vibration: bool,
    pub controller_sensitivity_x: f32, pub controller_sensitivity_y: f32, pub controller_ads_multiplier: f32, pub controller_scope_multiplier: f32, pub bindings: Bindings,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Game { pub auto_equip_better_items: bool, pub first_person_camera: bool, pub show_head_accessories: bool, pub show_backpack: bool, pub reset_building_choice: bool, pub language: usize }
#[derive(Clone, Debug, PartialEq)]
pub struct Audio { pub master_volume: f32, pub music_volume: f32, pub sfx_volume: f32, pub dialog_volume: f32, pub voice_chat_volume: f32, pub voice_chat_enabled: bool, pub push_to_talk: bool, pub quality: usize }
#[derive(Clone, Debug, PartialEq)]
pub struct Accessibility { pub subtitles: bool }

/// A recorded change (also the diff element between two snapshots).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Change { pub id: &'static str, pub old: Value, pub new: Value }

#[derive(Clone, Debug)]
pub struct Settings {
    pub video: Video, pub input: Input, pub game: Game, pub audio: Audio, pub accessibility: Accessibility,
    pending: Vec<Change>,
    /// Snapshot of what the apply layer last applied (per domain), None until the first apply.
    applied: Option<Box<Settings>>,
    /// Sections (Video/Input/Game/Audio/Accessibility) that have been applied at least once.
    applied_sections: Vec<&'static str>,
    /// Ids whose hook failed last time; re-applied on the next apply call.
    retry: Vec<&'static str>,
    /// (section, key, value) lines this version does not know; preserved on save.
    unknown: Vec<(String, String, String)>,
    pub loaded_version: Option<u32>,
}

const GROUPS: [&str; 6] = ["video.view_distance", "video.shadows", "video.anti_aliasing", "video.textures", "video.effects", "video.post_processing"];

macro_rules! fields {
    ($s:ident; $($id:literal => $t:ident [$($p:tt)+]),+ $(,)?) => {
        impl Settings {
            fn get_field(&self, id: &str) -> Option<Value> { let $s = self; Some(match id { $($id => fields!(@get $t $($p)+),)+ _ => return None }) }
            fn set_field(&mut self, id: &str, v: Value) -> bool { let $s = self; match id { $($id => fields!(@set $t v $($p)+),)+ _ => return false } true }
        }
    };
    (@get b $($p:tt)+) => { Value::Bool($($p)+) };
    (@get f $($p:tt)+) => { Value::Float($($p)+) };
    (@get l $($p:tt)+) => { Value::Index($($p)+ as usize) };
    (@get e $($p:tt)+) => { Value::Index(IndexEnum::index(&$($p)+)) };
    (@get z $a:expr, $b:expr) => { Value::Size($a, $b) };
    (@set b $v:ident $($p:tt)+) => { if let Value::Bool(x) = $v { $($p)+ = x; } };
    (@set f $v:ident $($p:tt)+) => { if let Value::Float(x) = $v { $($p)+ = x; } };
    (@set l $v:ident $($p:tt)+) => { if let Value::Index(x) = $v { $($p)+ = x as _; } };
    (@set e $v:ident $($p:tt)+) => { if let Value::Index(x) = $v { $($p)+ = IndexEnum::from_index(x); } };
    (@set z $v:ident $a:expr, $b:expr) => { if let Value::Size(w, h) = $v { $a = w; $b = h; } };
}
fields!(s;
    "video.window_mode" => e [s.video.window_mode], "video.resolution" => z [s.video.width, s.video.height], "video.vsync" => b [s.video.vsync],
    "video.frame_rate_limit" => e [s.video.frame_rate_limit], "video.quality" => e [s.video.preset],
    "video.resolution_quality" => l [s.video.resolution_quality], "video.view_distance" => l [s.video.view_distance], "video.shadows" => l [s.video.shadows],
    "video.anti_aliasing" => l [s.video.anti_aliasing], "video.textures" => l [s.video.textures], "video.effects" => l [s.video.effects], "video.post_processing" => l [s.video.post_processing],
    "video.motion_blur" => b [s.video.motion_blur], "video.show_grass" => b [s.video.show_grass], "video.show_fps" => b [s.video.show_fps], "video.brightness" => f [s.video.brightness],
    "input.mouse_sensitivity" => f [s.input.mouse_sensitivity], "input.mouse_sensitivity_y" => f [s.input.mouse_sensitivity_y], "input.mouse_ads_multiplier" => f [s.input.mouse_ads_multiplier],
    "input.mouse_scope_multiplier" => f [s.input.mouse_scope_multiplier], "input.invert_y" => b [s.input.invert_y], "input.crouch_mode" => e [s.input.crouch_mode], "input.toggle_sprint" => b [s.input.toggle_sprint],
    "input.sprint_cancels_reload" => b [s.input.sprint_cancels_reload], "input.tap_to_interact" => b [s.input.tap_to_interact], "input.toggle_targeting" => b [s.input.toggle_targeting], "input.vibration" => b [s.input.vibration],
    "input.controller_sensitivity_x" => f [s.input.controller_sensitivity_x], "input.controller_sensitivity_y" => f [s.input.controller_sensitivity_y],
    "input.controller_ads_multiplier" => f [s.input.controller_ads_multiplier], "input.controller_scope_multiplier" => f [s.input.controller_scope_multiplier],
    "game.auto_equip_better_items" => b [s.game.auto_equip_better_items], "game.first_person_camera" => b [s.game.first_person_camera], "game.show_head_accessories" => b [s.game.show_head_accessories],
    "game.show_backpack" => b [s.game.show_backpack], "game.reset_building_choice" => b [s.game.reset_building_choice], "game.language" => l [s.game.language],
    "audio.master_volume" => f [s.audio.master_volume], "audio.music_volume" => f [s.audio.music_volume], "audio.sfx_volume" => f [s.audio.sfx_volume], "audio.dialog_volume" => f [s.audio.dialog_volume],
    "audio.voice_chat_volume" => f [s.audio.voice_chat_volume], "audio.voice_chat_enabled" => b [s.audio.voice_chat_enabled], "audio.push_to_talk" => b [s.audio.push_to_talk], "audio.quality" => l [s.audio.quality],
    "accessibility.subtitles" => b [s.accessibility.subtitles],
);

impl Default for Settings {
    /// Built from the generated descriptor table, so defaults and descriptors can never disagree.
    fn default() -> Self {
        let mut s = Settings {
            video: Video { window_mode: WindowMode::Windowed, width: 0, height: 0, vsync: false, frame_rate_limit: FrameRateLimit::Unlimited, preset: Preset::Custom, resolution_quality: 0, view_distance: 0, shadows: 0, anti_aliasing: 0, textures: 0, effects: 0, post_processing: 0, motion_blur: false, show_grass: false, show_fps: false, brightness: 0. },
            input: Input { mouse_sensitivity: 0., mouse_sensitivity_y: 0., mouse_ads_multiplier: 0., mouse_scope_multiplier: 0., invert_y: false, crouch_mode: CrouchMode::Hold, toggle_sprint: false, sprint_cancels_reload: false, tap_to_interact: false, toggle_targeting: false, vibration: false, controller_sensitivity_x: 0., controller_sensitivity_y: 0., controller_ads_multiplier: 0., controller_scope_multiplier: 0., bindings: Bindings::default() },
            game: Game { auto_equip_better_items: false, first_person_camera: false, show_head_accessories: false, show_backpack: false, reset_building_choice: false, language: 0 },
            audio: Audio { master_volume: 0., music_volume: 0., sfx_volume: 0., dialog_volume: 0., voice_chat_volume: 0., voice_chat_enabled: false, push_to_talk: false, quality: 0 },
            accessibility: Accessibility { subtitles: false },
            pending: Vec::new(), applied: None, applied_sections: Vec::new(), retry: Vec::new(), unknown: Vec::new(), loaded_version: None,
        };
        for e in entries() { s.set_raw(e.id, e.default_value()); }
        s.video.view_distance = DEFAULT_VIEW_DISTANCE;
        s.refresh_preset(false);
        s
    }
}

impl Settings {
    fn get_raw(&self, id: &str) -> Option<Value> {
        if let Some(n) = id.strip_prefix("bind.") { return Bindings::action_index(n).map(|i| Value::Keys(self.input.bindings.keys[i])); }
        self.get_field(id)
    }
    fn set_raw(&mut self, id: &str, v: Value) -> bool {
        if let Some(n) = id.strip_prefix("bind.") {
            return match (Bindings::action_index(n), v) { (Some(i), Value::Keys(k)) => { self.input.bindings.keys[i] = k; true } _ => false };
        }
        self.set_field(id, v)
    }
    pub fn get(&self, id: &str) -> Option<Value> { self.get_raw(id) }
    pub fn get_bool(&self, id: &str) -> Option<bool> { match self.get_raw(id) { Some(Value::Bool(b)) => Some(b), _ => None } }
    pub fn get_float(&self, id: &str) -> Option<f32> { match self.get_raw(id) { Some(Value::Float(f)) => Some(f), _ => None } }

    /// Validated set: type-checks, clamps, records a [`Change`]. Returns whether the value changed.
    pub fn set(&mut self, id: &str, v: Value) -> Result<bool, SetError> {
        let e = entry(id).ok_or_else(|| SetError::UnknownId(id.to_string()))?;
        let v = e.sanitize(v)?;
        let old = self.get_raw(id).ok_or_else(|| SetError::UnknownId(id.to_string()))?;
        if old == v { return Ok(false); }
        self.set_raw(id, v);
        self.pending.push(Change { id: e.id, old, new: v });
        if id == "video.quality" { self.cascade_preset(); } else if GROUPS.contains(&e.id) { self.refresh_preset(true); } else if id == "video.resolution_quality" && self.video.preset == Preset::Auto { self.video.preset = Preset::Custom; self.pending.push(Change { id: "video.quality", old: Value::Index(Preset::Auto.index()), new: Value::Index(Preset::Custom.index()) }); }
        Ok(true)
    }
    pub fn set_text(&mut self, id: &str, text: &str) -> Result<bool, SetError> {
        let e = entry(id).ok_or_else(|| SetError::UnknownId(id.to_string()))?;
        let v = e.parse(text).map_err(|why| SetError::Parse { id: e.id, text: text.to_string(), why })?;
        self.set(id, v)
    }
    pub fn reset(&mut self, id: &str) -> Result<bool, SetError> { let e = entry(id).ok_or_else(|| SetError::UnknownId(id.to_string()))?; self.set(id, e.default_value()) }
    /// Resets every setting (optionally only one tab, e.g. "Video") and records the changes.
    pub fn reset_all(&mut self, tab: Option<&str>) { for e in entries().iter().filter(|e| tab.map_or(true, |t| e.tab == t)) { let _ = self.set(e.id, e.default_value()); } }
    pub fn drain_changes(&mut self) -> Vec<Change> { std::mem::take(&mut self.pending) }
    pub fn pending_changes(&self) -> &[Change] { &self.pending }
    /// Free-form `[section] key=value` pair kept outside the descriptor table and preserved in the INI (used by the menu for
    /// values the typed model has no field for, e.g. the HUD element visibility list in `[HUD]`). Case-insensitive lookup.
    pub fn extra(&self, section: &str, key: &str) -> Option<&str> {
        self.unknown.iter().find(|(s, k, _)| s.eq_ignore_ascii_case(section) && k.eq_ignore_ascii_case(key)).map(|(_, _, v)| v.as_str())
    }
    /// Sets (or adds) an extra key; see [`Settings::extra`]. Not part of `diff`/`same_values` (compare with `extra`).
    pub fn set_extra(&mut self, section: &str, key: &str, value: &str) {
        if let Some(e) = self.unknown.iter_mut().find(|(s, k, _)| s.eq_ignore_ascii_case(section) && k.eq_ignore_ascii_case(key)) { e.2 = value.to_string(); }
        else { self.unknown.push((section.to_string(), key.to_string(), value.to_string())); }
    }
    /// Removes an extra key (back to "unset").
    pub fn clear_extra(&mut self, section: &str, key: &str) { self.unknown.retain(|(s, k, _)| !(s.eq_ignore_ascii_case(section) && k.eq_ignore_ascii_case(key))); }

    /// Changes needed to go from `self` to `other`.
    pub fn diff(&self, other: &Settings) -> Vec<Change> {
        entries().iter().filter_map(|e| { let (a, b) = (self.get_raw(e.id)?, other.get_raw(e.id)?); if a != b { Some(Change { id: e.id, old: a, new: b }) } else { None } }).collect()
    }
    pub fn same_values(&self, other: &Settings) -> bool { self.diff(other).is_empty() }

    fn group_levels(&self) -> [Level; 6] { let v = &self.video; [v.view_distance, v.shadows, v.anti_aliasing, v.textures, v.effects, v.post_processing] }
    fn cascade_preset(&mut self) {
        let level = match self.video.preset { Preset::Low => 0, Preset::Medium => 1, Preset::High => 2, Preset::Epic | Preset::Auto => 3, Preset::Custom => return };
        for id in GROUPS {
            if self.video.preset == Preset::Auto && id == "video.view_distance" { continue; } let old = self.get_raw(id).unwrap(); let new = Value::Index(level); if old != new { self.set_raw(id, new); self.pending.push(Change { id: entry(id).unwrap().id, old, new }); } }
    }
    /// Recomputes the preset label from the six groups (Auto stays Auto while everything is Epic).
    fn refresh_preset(&mut self, record: bool) {
        let l = self.group_levels();
        let want = if self.video.preset == Preset::Auto && l[1..].iter().all(|&x| x == 3) { Preset::Auto } else if l.iter().all(|&x| x == l[0]) { Preset::from_index(l[0] as usize) } else { Preset::Custom };
        if want != self.video.preset {
            let old = Value::Index(self.video.preset.index());
            self.video.preset = want;
            if record { self.pending.push(Change { id: "video.quality", old, new: Value::Index(want.index()) }); }
        }
    }
    /// Frame cap in FPS actually applied (0 = unlimited): Uncap Framerate overrides the limit.
    /// Frame cap in fps (0 = unlimited). 1.8 has no separate "Uncap Framerate" row: "Unlimited" is a Frame Rate Limit option.
    pub fn effective_frame_cap(&self) -> u32 { self.video.frame_rate_limit.fps() }
    /// Every value is inside its descriptor's range (used by the fuzz test and the self-test).
    pub fn validate(&self) -> Result<(), String> {
        for e in entries() { let v = self.get_raw(e.id).ok_or(format!("{} missing", e.id))?; if e.sanitize(v).map_err(|x| x.to_string())? != v { return Err(format!("{} out of range: {:?}", e.id, v)); } }
        Ok(())
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Recovered / approximated mappings from setting values to runtime behaviour
// ------------------------------------------------------------------------------------------------------------------

/// `input.mouse_sensitivity` is the Game-tab slider position (0..1, what 1.8 shows). RECOVERED mapping: `UFortGameOptions::MouseSensitivityChanged`
/// @142f1d380 stores `slider * this+0x278` into `UFortGameUserSettings.MouseSensitivity` (+0x108), `GetMouseSensitivityValue` @142f0cbd0 shows
/// `MouseSensitivity / +0x278` clamped to 0..1, and the `UFortGameOptions` ctor @1430fd3f0 sets +0x278 (a native field, not a UPROPERTY) to 0.25.
pub const MOUSE_SENSITIVITY_SLIDER_SCALE: f32 = 0.25;
/// The slider position of the default `MouseSensitivity` 0.07 (ctor @1405b4930): 0.07 / 0.25.
pub const MOUSE_SENSITIVITY_DEFAULT_SLIDER: f32 = 0.28;
/// Native `MouseSensitivity` (the MouseX/MouseY `AxisProperties.Sensitivity`, `UPlayerInput::SetMouseSensitivity` @1425095f0) for a slider value.
pub fn native_mouse_sensitivity(slider: f32) -> f32 { slider * MOUSE_SENSITIVITY_SLIDER_SCALE }
/// Yaw degrees per raw count BEFORE the FOV term (`InputLive::deg_x`): MouseSensitivity x `InputYawScale` 2.5. The exe multiplies by
/// `FOV x FOVScale` afterwards (`fn_movement::mouse_fov_factor`, applied by [`InputLive::look`] / the app's input gathering).
pub fn mouse_deg_per_count(slider: f32) -> f32 { fn_movement::mouse_degrees_per_count(native_mouse_sensitivity(slider), 0.).0 }
/// Pitch counterpart (`InputPitchScale` magnitude 1.75; mouse up = look up).
pub fn mouse_pitch_deg_per_count(slider: f32) -> f32 { fn_movement::mouse_degrees_per_count(native_mouse_sensitivity(slider), 0.).1 }
/// Shadow-map resolution per Shadows level; None = shadows off.
/// RECOVERED: Fortnite's `Scalability:ShadowQuality@N` (FortniteGame/CookedIniVersion.txt, lines 5103-5158 of the extracted copy): `r.ShadowQuality` 0 at @0
/// (dynamic shadows off), `r.Shadow.MaxCSMResolution` 1024 / 2048 / 2048 at @1..@3. The table is `fn_render::quality::ShadowScalability` (one copy).
pub fn shadow_map_size(level: Level) -> Option<u32> { fn_render::quality::shadow_map_size(level as u32) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostFlags { pub ao: bool, pub bloom: bool, pub vignette: bool }
/// Post Processing level -> (SSAO, bloom, vignette). RECOVERED: `Scalability:PostProcessQuality@N` (CookedIniVersion.txt L5174-5241): r.BloomQuality 4/4/5/5
/// (bloom at every level), r.AmbientOcclusionLevels 0/-1/-1/-1 (SSAO from level 1), r.Tonemapper.Quality 0/2/5/5 (vignette needs >= 2). The whole cvar
/// group is `fn_render::quality::PostProcessScalability`.
pub fn post_flags(level: Level) -> PostFlags { let (ao, bloom, vignette) = fn_render::quality::post_flags(level as u32); PostFlags { ao, bloom, vignette } }
/// View Distance level -> camera far plane: the far plane does NOT change with the level (`fn_camera::FAR` at every level); View Distance is
/// `r.ViewDistanceScale`, see [`view_distance_scale`].
pub fn far_plane_m(_level: Level) -> f32 { fn_camera::FAR }
pub const DEFAULT_VIEW_DISTANCE: Level = 0;
/// View Distance level -> `r.ViewDistanceScale` 0.5 / 0.67 / 0.83 / 1.0 (`ViewDistanceQuality@N`, CookedIniVersion.txt L5079 / 5084 / 5089 / 5094), the
/// multiplier of every cull distance.
pub fn view_distance_scale(level: Level) -> f32 { fn_render::quality::ViewDistanceScalability::level(level as u32).view_distance_scale }
/// 3D Resolution level -> `r.ScreenPercentage` 50 / 71 / 87 / 100 (`PerfIndexValues_ResolutionQuality`, CookedIniVersion.txt L5066).
pub fn screen_percentage(level: Level) -> f32 { fn_render::quality::screen_percentage(level as u32) }
/// Gamma range of the Brightness tab. RECOVERED: `MinGammaValue` = 1.7 / `MaxGammaValue` = 2.7 are set in TabBrightnessOptions::Initialize Data
/// (reports/menu-widgets.json, calls "Initialize Data").
pub const GAMMA_MIN: f32 = 1.7;
pub const GAMMA_MAX: f32 = 2.7;
/// Gamma the output is authored for (UE default display gamma). Sits exactly at slider 0.50 of the range above (matches the "Default Value 0.50" callout).
pub const GAMMA_REFERENCE: f32 = 2.2;
/// Brightness slider 0..1 -> display gamma = lerp(1.7, 2.7, value), 2.2 at the assumed default 0.5. The range is RECOVERED; that the slider is the
/// normalised position between the two (and linear) is INFERRED (the pak holds no value mapping).
pub fn display_gamma(brightness: f32) -> f32 { GAMMA_MIN + (GAMMA_MAX - GAMMA_MIN) * brightness.clamp(0., 1.) }
/// Exponent applied to the sRGB-encoded output in post.frag: 1.0 at gamma 2.2, < 1 brightens, > 1 darkens.
pub fn display_exponent(gamma: f32) -> f32 { GAMMA_REFERENCE / gamma.max(0.1) }

/// Anti-Aliasing level -> FXAA parameters of the OLD FXAA-only runtime (debug only). The real Anti-Aliasing level is `AntiAliasingQuality@N`
/// (CookedIniVersion.txt L5068-5075: none / FXAA / temporal AA / temporal AA, `fn_render::quality::AaScalability`), applied by `set_anti_aliasing`.
/// Old note: APPROXIMATION (the original AA is UE4 post-process AA selected by an encrypted scalability
/// table; this runtime only has FXAA). Levels: 0 Low = off, 1 Medium = light, 2 High = standard, 3 Epic = full quality.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxaaParams { /// Sub-pixel aliasing removal (0..1).
    pub subpix: f32, /// Minimum local contrast that counts as an edge.
    pub edge_threshold: f32, /// Contrast floor for dark areas.
    pub edge_threshold_min: f32, /// Edge-end search steps in each direction.
    pub search_steps: i32 }
pub fn aa_params(level: Level) -> Option<FxaaParams> {
    match level { 0 => None,
        1 => Some(FxaaParams { subpix: 0.5, edge_threshold: 0.25, edge_threshold_min: 0.0625, search_steps: 4 }),
        2 => Some(FxaaParams { subpix: 0.75, edge_threshold: 0.166, edge_threshold_min: 0.0833, search_steps: 8 }),
        _ => Some(FxaaParams { subpix: 1.0, edge_threshold: 0.125, edge_threshold_min: 0.0312, search_steps: 12 }) }
}
/// Textures level -> (top mip levels skipped, anisotropy). RECOVERED: `TextureQuality@N` (CookedIniVersion.txt L5259-5275) r.MaxAnisotropy 0/2/4/8 (0 -> 1) and
/// r.Streaming.MipBias 1.5/1/0/0, which only allows the streamer to drop mips when over the pool budget (`fn_render::quality::TextureScalability`): 0 here.
pub fn texture_params(level: Level) -> (u32, f32) { let (skip, aniso) = fn_render::quality::texture_params(level as u32); (skip, aniso as f32) }

// ------------------------------------------------------------------------------------------------------------------
// Persistence
// ------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadStatus { /// No file: defaults.
    Missing, /// Parsed cleanly.
    Loaded, /// Parsed, but some lines/values were repaired (see warnings).
    Repaired, /// Unreadable: defaults used, original moved aside.
    Corrupt }
#[derive(Clone, Debug)]
pub struct LoadReport { pub status: LoadStatus, pub path: PathBuf, pub file_version: Option<u32>, pub warnings: Vec<String>, pub backup: Option<PathBuf> }

/// `FortniteSettings.ini` next to the executable.
pub fn default_path() -> PathBuf { std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join(FILE_NAME))).unwrap_or_else(|| PathBuf::from(FILE_NAME)) }

fn ini_lookup() -> &'static HashMap<(String, String), usize> {
    static M: OnceLock<HashMap<(String, String), usize>> = OnceLock::new();
    M.get_or_init(|| entries().iter().enumerate().map(|(i, e)| ((ini_section(e).to_ascii_lowercase(), e.ini_key.to_ascii_lowercase()), i)).collect())
}
const SECTION_ORDER: [&str; 6] = ["Video", "Input", "Game", "Audio", "Accessibility", "Bindings"];

impl Settings {
    pub fn to_ini(&self) -> String {
        let mut o = format!("; Fortnite {} standalone runtime settings. Edit freely while the game is closed; invalid values fall back to defaults.\n; Keys are case-insensitive. Unknown keys are preserved. Key names follow Unreal FKey names (SpaceBar, LeftControl, One, F1, MouseScrollUp, ...).\n", version::FORTNITE_VERSION);
        o.push_str(&format!("\n[Settings]\nVersion={VERSION}\n"));
        let mut extra_done = vec![false; self.unknown.len()];
        for sec in SECTION_ORDER {
            o.push_str(&format!("\n[{sec}]\n"));
            for e in entries().iter().filter(|e| ini_section(e) == sec) {
                if let Some(v) = self.get_raw(e.id) { o.push_str(&format!("{}={}\n", e.ini_key, e.text(&v))); }
            }
            for (i, (s, k, v)) in self.unknown.iter().enumerate() { if s.eq_ignore_ascii_case(sec) { o.push_str(&format!("{k}={v}\n")); extra_done[i] = true; } }
        }
        let mut last = String::new();
        for (i, (s, k, v)) in self.unknown.iter().enumerate() {
            if extra_done[i] { continue; }
            if !s.eq_ignore_ascii_case(&last) { o.push_str(&format!("\n[{s}]\n")); last = s.clone(); }
            o.push_str(&format!("{k}={v}\n"));
        }
        o
    }

    /// Parses INI bytes. Err = the file is unusable (binary, not UTF-8, oversized, no recognised section); otherwise settings
    /// with every bad value repaired (default or clamped) and one warning per repair.
    pub fn from_ini_bytes(bytes: &[u8]) -> Result<(Settings, Vec<String>), String> {
        if bytes.len() > MAX_FILE_BYTES { return Err(format!("file is {} bytes (limit {MAX_FILE_BYTES})", bytes.len())); }
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        let text = std::str::from_utf8(bytes).map_err(|e| format!("not valid UTF-8 ({e})"))?;
        if text.contains('\0') { return Err("contains NUL bytes".into()); }
        let mut s = Settings::default();
        let mut warnings = Vec::new();
        let (mut section, mut known_sections, mut content_lines, mut applied_keys) = (String::new(), 0usize, 0usize, 0usize);
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') { continue; }
            content_lines += 1;
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_string();
                if section.eq_ignore_ascii_case("Settings") || SECTION_ORDER.iter().any(|x| x.eq_ignore_ascii_case(&section)) { known_sections += 1; }
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { warnings.push(format!("line {}: not a key=value pair, ignored", n + 1)); continue; };
            let (k, v) = (k.trim(), v.trim());
            if section.is_empty() { warnings.push(format!("line {}: '{k}' appears before any [section], ignored", n + 1)); continue; }
            if section.eq_ignore_ascii_case("Settings") {
                if k.eq_ignore_ascii_case("Version") { match v.parse::<u32>() { Ok(x) => s.loaded_version = Some(x), Err(_) => warnings.push(format!("line {}: bad Version '{v}'", n + 1)) } }
                else { s.unknown.push((section.clone(), k.to_string(), v.to_string())); }
                continue;
            }
            match ini_lookup().get(&(section.to_ascii_lowercase(), k.to_ascii_lowercase())) {
                None => s.unknown.push((section.clone(), k.to_string(), v.to_string())),
                Some(&i) => {
                    let e = &entries()[i];
                    match e.parse(v) {
                        Err(why) => warnings.push(format!("{}.{k}: {why} (got '{v}'); using default", section)),
                        Ok(val) => match e.sanitize(val) {
                            Err(x) => warnings.push(format!("{}.{k}: {x}; using default", section)),
                            Ok(clean) => { if clean != val { warnings.push(format!("{}.{k}: value '{v}' out of range, clamped to {}", section, e.text(&clean))); } s.set_raw(e.id, clean); applied_keys += 1; }
                        },
                    }
                }
            }
        }
        if content_lines > 0 && known_sections == 0 { return Err("no recognised [section] headers".into()); }
        match s.loaded_version {
            None => { s.video.view_distance = DEFAULT_VIEW_DISTANCE; warnings.push("no Version key; treated as version 1".into()) }
            Some(v) if v > VERSION => warnings.push(format!("file version {v} is newer than this build ({VERSION}); known keys were read best-effort, unknown keys are preserved")),
            Some(v) if v < VERSION => { if v < 3 { s.video.view_distance = DEFAULT_VIEW_DISTANCE; } warnings.push(format!("file version {v} migrated to {VERSION}")) }
            _ => {}
        }
        let _ = applied_keys;
        let before = s.video.preset;
        s.refresh_preset(false);
        if s.video.preset != before { warnings.push(format!("Video.Quality '{}' disagrees with the individual quality groups; now '{}'", Preset::to_name(before), Preset::to_name(s.video.preset))); }
        Ok((s, warnings))
    }

    /// Loads `path`; never fails: missing -> defaults, corrupt -> defaults + the bad file moved to `<name>.corrupt`.
    pub fn load(path: &Path) -> (Settings, LoadReport) {
        let mut report = LoadReport { status: LoadStatus::Missing, path: path.to_path_buf(), file_version: None, warnings: Vec::new(), backup: None };
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Settings::default(), report),
            Err(e) => { report.status = LoadStatus::Corrupt; report.warnings.push(format!("cannot read file: {e}")); return (Settings::default(), report); }
        };
        if bytes.iter().all(|b| b.is_ascii_whitespace()) { report.warnings.push("file is empty; defaults used".into()); return (Settings::default(), report); }
        match Settings::from_ini_bytes(&bytes) {
            Ok((s, w)) => { report.status = if w.is_empty() { LoadStatus::Loaded } else { LoadStatus::Repaired }; report.file_version = s.loaded_version; report.warnings = w; (s, report) }
            Err(why) => {
                report.status = LoadStatus::Corrupt; report.warnings.push(format!("settings file unusable: {why}"));
                let backup = path.with_extension("ini.corrupt");
                match std::fs::rename(path, &backup) { Ok(()) => report.backup = Some(backup), Err(e) => report.warnings.push(format!("could not move the bad file aside: {e}")) }
                (Settings::default(), report)
            }
        }
    }

    /// Atomic write: temp file in the same directory, flushed to disk, then renamed over the target.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) { std::fs::create_dir_all(dir)?; }
        let tmp = path.with_extension("ini.tmp");
        let result = (|| { let mut f = std::fs::File::create(&tmp)?; f.write_all(self.to_ini().as_bytes())?; f.sync_all()?; drop(f); std::fs::rename(&tmp, path) })();
        if result.is_err() { let _ = std::fs::remove_file(&tmp); }
        result
    }
}
impl Preset { fn to_name(p: Preset) -> &'static str { match p { Preset::Low => "low", Preset::Medium => "medium", Preset::High => "high", Preset::Epic => "epic", Preset::Auto => "auto", Preset::Custom => "custom" } } }

// ------------------------------------------------------------------------------------------------------------------
// Apply layer
// ------------------------------------------------------------------------------------------------------------------

/// Renderer / window hooks. Default methods succeed without doing anything so partial implementations are fine.
pub trait VideoHooks {
    fn set_display(&mut self, _mode: WindowMode, _width: u32, _height: u32) -> Result<(), String> { Ok(()) }
    fn set_vsync(&mut self, _on: bool) -> Result<(), String> { Ok(()) }
    /// FPS cap, 0 = unlimited (already combines Frame Rate Limit and Uncap Framerate).
    fn set_frame_cap(&mut self, _fps: u32) -> Result<(), String> { Ok(()) }
    /// None = shadows off, otherwise cascade shadow-map resolution.
    fn set_shadow_map_size(&mut self, _size: Option<u32>) -> Result<(), String> { Ok(()) }
    /// Shadows level 0..=3 = `sg.ShadowQuality` (the whole cvar group); the default only forwards the map size.
    fn set_shadow_quality(&mut self, level: Level) -> Result<(), String> { self.set_shadow_map_size(shadow_map_size(level)) }
    fn set_post(&mut self, _flags: PostFlags) -> Result<(), String> { Ok(()) }
    /// Post Processing level 0..=3 = `sg.PostProcessQuality` (the whole cvar group, `fn_render::quality::PostProcessScalability`); the default only
    /// forwards the on/off flags.
    fn set_post_quality(&mut self, level: Level) -> Result<(), String> { self.set_post(post_flags(level)) }
    fn set_display_gamma(&mut self, _gamma: f32) -> Result<(), String> { Ok(()) }
    fn set_far_plane(&mut self, _metres: f32) -> Result<(), String> { Ok(()) }
    /// View Distance: `r.ViewDistanceScale` (see [`view_distance_scale`]).
    fn set_view_distance_scale(&mut self, _scale: f32) -> Result<(), String> { Ok(()) }
    /// Effects level 0..=3 = `sg.EffectsQuality` (CookedIniVersion.txt L5284-5339: r.SSR.Quality, the translucency lighting volume, ...).
    fn set_effects_quality(&mut self, _level: Level) -> Result<(), String> { Ok(()) }
    /// 3D Resolution: `r.ScreenPercentage` (see [`screen_percentage`]).
    fn set_screen_percentage(&mut self, _percent: f32) -> Result<(), String> { Ok(()) }
    fn set_show_fps(&mut self, _on: bool) -> Result<(), String> { Ok(()) }
    /// Motion Blur: `UFortGameUserSettings::bMotionBlur` -> `EngineShowFlags.MotionBlur` of the game viewport (ApplyNonResolutionSettings @1405bcb10).
    fn set_motion_blur(&mut self, _on: bool) -> Result<(), String> { Ok(()) }
    /// Anti-Aliasing level 0..=3 (see [`aa_params`]).
    fn set_anti_aliasing(&mut self, _level: Level) -> Result<(), String> { Ok(()) }
    /// Textures level 0..=3 (see [`texture_params`]).
    fn set_texture_quality(&mut self, _level: Level) -> Result<(), String> { Ok(()) }
}
pub trait InputHooks {
    fn set_look(&mut self, _deg_per_count_x: f32, _deg_per_count_y: f32, _invert_y: bool) {}
    fn set_crouch_mode(&mut self, _mode: CrouchMode) {}
    fn set_sprint_toggle(&mut self, _on: bool) {}
    fn set_bindings(&mut self, _bindings: &Bindings) {}
}
/// There is no audio engine; this trait exists so one can be attached later. Nothing in the runtime implements it for real.
pub trait AudioHooks {
    fn set_volumes(&mut self, _master: f32, _music: f32, _sfx: f32, _dialog: f32, _voice_chat: f32) {}
    fn set_voice_chat(&mut self, _enabled: bool, _push_to_talk: bool) {}
    fn set_quality(&mut self, _high: bool) {}
    fn set_subtitles(&mut self, _on: bool) {}
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApplyReport {
    /// Hook groups that ran successfully.
    pub applied: Vec<&'static str>,
    /// (group, error) for hooks that failed; they are retried on the next apply.
    pub failed: Vec<(&'static str, String)>,
    /// Changed setting ids that are Support::StoredOnly (persisted, but have no runtime effect).
    pub stored_only: Vec<&'static str>,
}
impl ApplyReport {
    pub fn ok(&self) -> bool { self.failed.is_empty() }
    pub fn merge(&mut self, o: ApplyReport) { self.applied.extend(o.applied); self.failed.extend(o.failed); self.stored_only.extend(o.stored_only); }
}
/// Audio sink for a runtime without an audio engine: accepts every value and does nothing (STORED ONLY).
pub struct StoredOnlyAudio;
impl AudioHooks for StoredOnlyAudio {}

const VIDEO_GROUPS: &[(&str, &[&str])] = &[
    ("display", &["video.window_mode", "video.resolution"]), ("vsync", &["video.vsync"]), ("frame_cap", &["video.frame_rate_limit"]),
    ("shadows", &["video.shadows"]), ("post", &["video.post_processing"]), ("gamma", &["video.brightness"]), ("view_distance", &["video.view_distance"]), ("show_fps", &["video.show_fps"]),
    ("anti_aliasing", &["video.anti_aliasing"]), ("textures", &["video.textures"]), ("resolution_quality", &["video.resolution_quality"]), ("effects", &["video.effects"]),
    ("motion_blur", &["video.motion_blur"]),
];

impl Settings {
    /// Ids of `domain` entries (by `section`) that differ from what was last applied (or all of them before the first apply),
    /// plus ids whose hook failed last time.
    fn dirty(&self, sections: &[&'static str]) -> Vec<&'static str> {
        entries().iter().filter(|e| sections.contains(&e.section)).filter(|e| {
            self.retry.contains(&e.id) || !self.applied_sections.contains(&e.section) || self.applied.as_ref().map_or(true, |a| a.get_raw(e.id) != self.get_raw(e.id))
        }).map(|e| e.id).collect()
    }
    fn commit(&mut self, sections: &[&'static str], ids: &[&'static str], failed: &[&'static str]) {
        let values: Vec<(&'static str, Value)> = ids.iter().filter(|id| !failed.contains(id)).filter_map(|&id| self.get_raw(id).map(|v| (id, v))).collect();
        let snapshot = self.applied.get_or_insert_with(|| Box::new(Settings::default()));
        for (id, v) in values { snapshot.set_raw(id, v); }
        for s in sections { if !self.applied_sections.contains(s) { self.applied_sections.push(s); } }
        self.retry.retain(|r| !ids.contains(r));
        self.retry.extend(failed.iter().copied());
        self.retry.sort_unstable(); self.retry.dedup();
    }
    fn finish_report(&self, ids: &[&'static str], report: &mut ApplyReport) {
        for &id in ids { if entry(id).map_or(false, |e| e.support == Support::StoredOnly) { report.stored_only.push(id); } }
    }

    /// Applies changed video settings through `hooks`. The first call applies everything.
    pub fn apply_video(&mut self, hooks: &mut dyn VideoHooks) -> ApplyReport {
        let dirty = self.dirty(&["Video"]);
        let mut report = ApplyReport::default();
        let mut failed_ids: Vec<&'static str> = Vec::new();
        let v = self.video.clone();
        for (group, ids) in VIDEO_GROUPS {
            if !ids.iter().any(|i| dirty.contains(i)) { continue; }
            let r = match *group {
                "display" => hooks.set_display(v.window_mode, v.width, v.height),
                "vsync" => hooks.set_vsync(v.vsync),
                "frame_cap" => hooks.set_frame_cap(self.effective_frame_cap()),
                "shadows" => hooks.set_shadow_quality(v.shadows),
                "post" => hooks.set_post_quality(v.post_processing),
                "gamma" => hooks.set_display_gamma(display_gamma(v.brightness)),
                "view_distance" => hooks.set_far_plane(far_plane_m(v.view_distance)).and_then(|_| hooks.set_view_distance_scale(view_distance_scale(v.view_distance))),
                "resolution_quality" => hooks.set_screen_percentage(screen_percentage(v.resolution_quality)),
                "effects" => hooks.set_effects_quality(v.effects),
                "anti_aliasing" => hooks.set_anti_aliasing(v.anti_aliasing),
                "textures" => hooks.set_texture_quality(v.textures),
                "motion_blur" => hooks.set_motion_blur(v.motion_blur),
                _ => hooks.set_show_fps(v.show_fps),
            };
            match r { Ok(()) => report.applied.push(group), Err(e) => { report.failed.push((group, e)); failed_ids.extend(ids.iter().copied()); } }
        }
        self.finish_report(&dirty, &mut report);
        self.commit(&["Video"], &dirty, &failed_ids);
        report
    }
    pub fn apply_input(&mut self, hooks: &mut dyn InputHooks) -> ApplyReport {
        let dirty = self.dirty(&["Input"]);
        let mut report = ApplyReport::default();
        let any = |names: &[&str]| dirty.iter().any(|d| names.contains(d));
        let i = self.input.clone();
        if any(&["input.mouse_sensitivity", "input.mouse_sensitivity_y", "input.invert_y"]) { hooks.set_look(mouse_deg_per_count(i.mouse_sensitivity), mouse_pitch_deg_per_count(i.mouse_sensitivity_y), i.invert_y); report.applied.push("look"); }
        if any(&["input.crouch_mode"]) { hooks.set_crouch_mode(i.crouch_mode); report.applied.push("crouch_mode"); }
        if any(&["input.toggle_sprint"]) { hooks.set_sprint_toggle(i.toggle_sprint); report.applied.push("sprint_toggle"); }
        if dirty.iter().any(|d| d.starts_with("bind.")) { hooks.set_bindings(&i.bindings); report.applied.push("bindings"); }
        self.finish_report(&dirty, &mut report);
        self.commit(&["Input"], &dirty, &[]);
        report
    }
    /// Audio and Accessibility are STORED ONLY today; the hooks fire so a future audio engine gets the values.
    pub fn apply_audio(&mut self, hooks: &mut dyn AudioHooks) -> ApplyReport {
        let dirty = self.dirty(&["Audio", "Accessibility"]);
        let mut report = ApplyReport::default();
        let any = |names: &[&str]| dirty.iter().any(|d| names.contains(d));
        let a = self.audio.clone();
        if any(&["audio.master_volume", "audio.music_volume", "audio.sfx_volume", "audio.dialog_volume", "audio.voice_chat_volume"]) { hooks.set_volumes(a.master_volume, a.music_volume, a.sfx_volume, a.dialog_volume, a.voice_chat_volume); report.applied.push("volumes"); }
        if any(&["audio.voice_chat_enabled", "audio.push_to_talk"]) { hooks.set_voice_chat(a.voice_chat_enabled, a.push_to_talk); report.applied.push("voice_chat"); }
        if any(&["audio.quality"]) { hooks.set_quality(a.quality == 0); report.applied.push("quality"); }
        if any(&["accessibility.subtitles"]) { hooks.set_subtitles(self.accessibility.subtitles); report.applied.push("subtitles"); }
        self.finish_report(&dirty, &mut report);
        self.commit(&["Audio", "Accessibility"], &dirty, &[]);
        report
    }
    /// Game-tab options have no runtime consumers yet: marks them applied and reports them as stored-only.
    pub fn apply_game(&mut self) -> ApplyReport {
        let dirty = self.dirty(&["Game"]);
        let mut report = ApplyReport::default();
        self.finish_report(&dirty, &mut report);
        self.commit(&["Game"], &dirty, &[]);
        report
    }
}

// ------------------------------------------------------------------------------------------------------------------
// Runtime helpers (input latches, frame limiter, live input snapshot)
// ------------------------------------------------------------------------------------------------------------------

/// Hold-vs-toggle logic for crouch/sprint. Hold mode follows the key; toggle mode flips on each fresh press and can be cancelled
/// (sprint: when movement stops; crouch: on jump).
#[derive(Clone, Copy, Debug, Default)]
pub struct ModeLatch { prev: bool, on: bool, held_cleared: bool }
impl ModeLatch {
    pub fn update(&mut self, down: bool, toggle: bool, cancel: bool) -> bool {
        // INVENTORY FIX3: after `clear_held` a key that is still down is not a hold (the press event already happened); it counts again after a release
        let down = if self.held_cleared { if down { false } else { self.held_cleared = false; false } } else { down };
        if !toggle { self.on = false; self.prev = down; return down; }
        if down && !self.prev { self.on = !self.on; }
        self.prev = down;
        if cancel { self.on = false; }
        self.on
    }
    pub fn active(&self) -> bool { self.on }
    pub fn cancel(&mut self) { self.on = false; }
    /// INVENTORY FIX3 (inventory_gaps G3): the sprint part of the controller's cursor-mode entry `FUN_140824b40` (called by `UFortHUDContext::EnterCursorMode`
    /// 0x142e18200): `bHoldingSprint` (+0xF16) = 0, and `bWantsToSprint` (+0xF15) = 0 unless `bSprintToggleable` (+0xF17). Offsets: 1.8 SDK
    /// `AFortPlayerController`. The held key is ignored until it is released and pressed again; a toggled sprint stays on.
    pub fn clear_held(&mut self, toggleable: bool) {
        self.held_cleared = true;
        if !toggleable { self.on = false; }
    }
}

/// What input code reads every frame; updated through [`InputHooks`]. `deg_x` / `deg_y`: yaw / pitch degrees per raw mouse count before the
/// exe's FOV term (`fn_movement::mouse_fov_factor`), see [`mouse_deg_per_count`].
#[derive(Clone, Debug)]
pub struct InputLive { pub deg_x: f32, pub deg_y: f32, pub invert_y: bool, pub crouch_mode: CrouchMode, pub sprint_toggle: bool, pub bindings: Bindings }
impl InputLive {
    pub fn from_settings(s: &Settings) -> Self {
        InputLive { deg_x: mouse_deg_per_count(s.input.mouse_sensitivity), deg_y: mouse_pitch_deg_per_count(s.input.mouse_sensitivity_y), invert_y: s.input.invert_y, crouch_mode: s.input.crouch_mode, sprint_toggle: s.input.toggle_sprint, bindings: s.input.bindings.clone() }
    }
    /// Look degrees (yaw, pitch-up) for raw counts (dy > 0 = mouse moved down, Windows convention) at camera FOV `fov` (degrees; 0 = unknown,
    /// no FOV term). Invert View negates the vertical axis (`InvertAxis("LookUp")`).
    pub fn look_delta(&self, dx: i32, dy: i32, fov: f32) -> (f32, f32) {
        let (sign, k) = (if self.invert_y { -1. } else { 1. }, fn_movement::mouse_fov_factor(fov));
        (dx as f32 * self.deg_x * k, -sign * dy as f32 * self.deg_y * k)
    }
    /// Raw-mouse look: yaw wraps to [0,360), pitch is clamped by the camera limits; Invert flips the vertical direction.
    pub fn look(&self, yaw: f32, pitch: f32, dx: i32, dy: i32, fov: f32) -> (f32, f32) {
        let (y, p) = self.look_delta(dx, dy, fov);
        ((yaw + y).rem_euclid(360.), fn_camera::clamp_pitch(pitch + p))
    }
}
impl Default for InputLive { fn default() -> Self { InputLive::from_settings(&Settings::default()) } }
impl InputHooks for InputLive {
    fn set_look(&mut self, x: f32, y: f32, invert: bool) { self.deg_x = x; self.deg_y = y; self.invert_y = invert; }
    fn set_crouch_mode(&mut self, m: CrouchMode) { self.crouch_mode = m; }
    fn set_sprint_toggle(&mut self, on: bool) { self.sprint_toggle = on; }
    fn set_bindings(&mut self, b: &Bindings) { self.bindings = b.clone(); }
}

/// Values the main loop reads each frame; written by [`LiveVideo`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Runtime { pub frame_cap: u32, pub show_fps: bool, pub vsync: bool }

/// Refresh rate (Hz) of the monitor the game window is on; 0 = unknown. Written by [`LiveVideo`] after display changes.
pub static DISPLAY_HZ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Size used when a saved windowed resolution is nonsense (a zero axis).
pub const FALLBACK_WINDOWED: (u32, u32) = (1280, 720);
/// Client size to use for a windowed window: `(w, h)` as saved, clamped so that client + `frame` (non-client overhead: borders and caption)
/// fits inside the monitor `work` area (width, height). A zero axis falls back to [`FALLBACK_WINDOWED`]; sizes below the minimum resolution are
/// raised to it unless the work area itself is smaller, in which case the work area wins. Always returns at least 1x1.
pub fn fit_windowed_client(w: u32, h: u32, work: (i32, i32), frame: (i32, i32)) -> (u32, u32) {
    let (w, h) = if w == 0 || h == 0 { FALLBACK_WINDOWED } else { (w.clamp(MIN_RES.0, MAX_RES.0), h.clamp(MIN_RES.1, MAX_RES.1)) };
    let avail = |work: i32, frame: i32| (work.saturating_sub(frame.max(0))).max(1) as u32;
    (w.min(avail(work.0, frame.0)), h.min(avail(work.1, frame.1)))
}

/// Time to wait so that frames start `period` apart.
pub fn frame_wait(elapsed: Duration, period: Duration) -> Duration { period.saturating_sub(elapsed) }
pub struct FrameLimiter { last: Instant }
impl FrameLimiter {
    pub fn new() -> Self { FrameLimiter { last: Instant::now() } }
    /// Minimum frame period when not paced by vsync: the configured cap, or 2 ms (the runtime's old no-vsync fallback) when unlimited.
    pub fn period(cap_fps: u32) -> Duration { if cap_fps > 0 { Duration::from_secs_f64(1. / cap_fps as f64) } else { Duration::from_millis(2) } }
    /// Whether the limiter paces the loop: always without vsync; with vsync only when the cap is clearly below the display refresh (vsync alone then
    /// would run faster than the cap). `display_hz` 0 = unknown (vsync is trusted).
    pub fn paced(cap_fps: u32, vsync: bool, display_hz: u32) -> bool { !vsync || (cap_fps > 0 && display_hz > 0 && cap_fps + 1 < display_hz) }
    pub fn throttle(&mut self, cap_fps: u32, vsync: bool) {
        if Self::paced(cap_fps, vsync, DISPLAY_HZ.load(std::sync::atomic::Ordering::Relaxed)) {
            let wait = frame_wait(self.last.elapsed(), Self::period(cap_fps));
            if wait > Duration::from_micros(2500) { std::thread::sleep(wait - Duration::from_micros(1500)); }
            while self.last.elapsed() < Self::period(cap_fps) { std::hint::spin_loop(); }
        }
        self.last = Instant::now();
    }
}

/// Idle-CPU fix for vsync-on windowed play. With vsync the driver lets the CPU run a frame ahead, then BLOCKS inside the next GL call that needs a
/// free back buffer, and the NVIDIA driver burns that wait in a spin loop (measured: ~3.4 of 5.8 CPU-ms per 6.94 ms frame at 144 Hz, ~83% of a core
/// for the render thread). Waiting for the compositor's next refresh with `DwmFlush` (a kernel wait, no spinning) right after the swap means the
/// next frame starts just after a vblank with the queue empty, so the driver never has to wait. Frame rate, vsync and the image are unchanged
/// (still swap interval 1). Not used when the limiter already paces below the refresh, in exclusive fullscreen (the compositor is bypassed), or after
/// `DwmFlush` fails/stalls; `FNRT_NO_DWMFLUSH=1` turns it off.
pub struct VblankWait { off: bool }
impl VblankWait {
    pub fn new() -> Self { VblankWait { off: std::env::var_os("FNRT_NO_DWMFLUSH").is_some() } }
    /// Pure policy: only for vsync on, the limiter not pacing (see [`FrameLimiter::paced`]), and not exclusive fullscreen.
    pub fn applies(vsync: bool, limiter_paced: bool, exclusive_fullscreen: bool) -> bool { vsync && !limiter_paced && !exclusive_fullscreen }
    /// Pure health check of one `DwmFlush` call: it must succeed and return within a few frames, otherwise the wait is abandoned for the session.
    pub fn healthy(hresult: i32, waited: Duration) -> bool { hresult >= 0 && waited < Duration::from_millis(250) }
    pub fn wanted(&self, vsync: bool, limiter_paced: bool, exclusive_fullscreen: bool) -> bool { !self.off && Self::applies(vsync, limiter_paced, exclusive_fullscreen) }
    pub fn disabled(&self) -> bool { self.off }
    /// Blocks until the compositor's next refresh through the platform host (`DwmFlush` on Windows); a failing or stalling flush disables the wait for the session.
    pub fn wait(&mut self, host: &mut dyn PlatformHost) { let t = Instant::now(); let hr = host.wait_vblank(); if !Self::healthy(hr, t.elapsed()) { self.off = true; } }
}

/// Smoothed frames-per-second readout for the Show FPS option.
#[derive(Clone, Copy, Debug, Default)]
pub struct FpsMeter { pub fps: f32 }
impl FpsMeter { pub fn tick(&mut self, dt: f32) { if dt > 0. { let inst = 1. / dt; self.fps = if self.fps == 0. { inst } else { self.fps + (inst - self.fps) * (1. - (-3. * dt).exp()) }; } } }

// ------------------------------------------------------------------------------------------------------------------
// Self-test (shared by `cargo test` and `FortniteRuntime.exe --settings-test`)
// ------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Check { pub name: String, pub ok: bool, pub detail: String }

#[derive(Default)]
struct RecordingVideo { calls: Vec<String>, fail_display: bool }
impl VideoHooks for RecordingVideo {
    fn set_display(&mut self, m: WindowMode, w: u32, h: u32) -> Result<(), String> { self.calls.push(format!("display {m:?} {w}x{h}")); if self.fail_display { Err("refused".into()) } else { Ok(()) } }
    fn set_vsync(&mut self, on: bool) -> Result<(), String> { self.calls.push(format!("vsync {on}")); Ok(()) }
    fn set_frame_cap(&mut self, f: u32) -> Result<(), String> { self.calls.push(format!("cap {f}")); Ok(()) }
    fn set_shadow_map_size(&mut self, s: Option<u32>) -> Result<(), String> { self.calls.push(format!("shadows {s:?}")); Ok(()) }
    fn set_post(&mut self, f: PostFlags) -> Result<(), String> { self.calls.push(format!("post {f:?}")); Ok(()) }
    fn set_display_gamma(&mut self, g: f32) -> Result<(), String> { self.calls.push(format!("gamma {g:.3}")); Ok(()) }
    fn set_far_plane(&mut self, m: f32) -> Result<(), String> { self.calls.push(format!("far {m}")); Ok(()) }
    fn set_show_fps(&mut self, on: bool) -> Result<(), String> { self.calls.push(format!("fps {on}")); Ok(()) }
    fn set_anti_aliasing(&mut self, l: Level) -> Result<(), String> { self.calls.push(format!("aa {l}")); Ok(()) }
    fn set_texture_quality(&mut self, l: Level) -> Result<(), String> { self.calls.push(format!("textures {l}")); Ok(()) }
}

pub fn self_test(work_dir: &Path) -> Vec<Check> {
    let dir = work_dir.join("settings-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    type T = fn(&Path) -> Result<String, String>;
    let tests: [(&str, T); 13] = [
        ("defaults match descriptor table and validate", t_defaults), ("every descriptor round-trips set/get/text", t_descriptor_roundtrip), ("INI round-trip of all non-default values", t_ini_roundtrip),
        ("out-of-range / malformed values are repaired with warnings", t_repair), ("unknown keys and sections tolerated and preserved", t_unknown), ("corrupt files fall back to defaults and are moved aside", t_corrupt),
        ("every truncated prefix of a saved file loads safely", t_prefixes), ("version handling", t_version), ("atomic write", t_atomic), ("apply layer diffs and retries", t_apply),
        ("quality preset cascade", t_preset), ("key bindings", t_bindings), ("input mapping (sensitivity, invert, latches, limiter)", t_input),
    ];
    let mut out = Vec::new();
    for (i, (name, f)) in tests.iter().enumerate() {
        let sub = dir.join(format!("t{i}")); let _ = std::fs::create_dir_all(&sub);
        let r = std::panic::catch_unwind(|| f(&sub)).unwrap_or_else(|_| Err("panicked".into()));
        out.push(match r { Ok(d) => Check { name: name.to_string(), ok: true, detail: d }, Err(d) => Check { name: name.to_string(), ok: false, detail: d } });
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}
fn t_defaults(_: &Path) -> Result<String, String> {
    let s = Settings::default(); s.validate()?;
    for e in entries() { if s.get(e.id) != Some(e.default_value()) { return Err(format!("{} default mismatch", e.id)); } }
    let (mut ids, mut dups) = (std::collections::HashSet::new(), 0);
    for e in entries() { if !ids.insert(e.id) { dups += 1; } }
    if dups > 0 { return Err("duplicate ids".into()); }
    if s.video.preset != Preset::Epic { return Err("default preset should be epic".into()); }
    Ok(format!("{} descriptors, {} wired, {} stored-only", entries().len(), entries().iter().filter(|e| e.support == Support::Wired).count(), entries().iter().filter(|e| e.support == Support::StoredOnly).count()))
}
fn t_descriptor_roundtrip(_: &Path) -> Result<String, String> {
    let mut s = Settings::default();
    for e in entries() {
        let alt = e.sample_alternative();
        s.set(e.id, alt).map_err(|x| x.to_string())?;
        let got = s.get(e.id).unwrap();
        if got != e.sanitize(alt).unwrap() { return Err(format!("{}: set/get mismatch {alt:?} vs {got:?}", e.id)); }
        if e.parse(&e.text(&got)) != Ok(got) { return Err(format!("{}: text round trip failed ('{}')", e.id, e.text(&got))); }
        if e.sanitize(Value::Bool(true)).is_ok() && !matches!(e.kind, KindDef::Toggle) { return Err(format!("{}: accepted a wrong-typed value", e.id)); }
    }
    for bad in [Value::Float(f32::NAN), Value::Float(f32::INFINITY)] { if s.set("input.mouse_sensitivity", bad).is_ok() { return Err("NaN/inf accepted".into()); } }
    if s.set("video.shadows", Value::Index(9)).is_ok() { return Err("enum index out of range accepted".into()); }
    if !matches!(s.set("no.such", Value::Bool(true)), Err(SetError::UnknownId(_))) { return Err("unknown id accepted".into()); }
    s.set("input.mouse_sensitivity", Value::Float(99.)).map_err(|x| x.to_string())?;
    if s.input.mouse_sensitivity != 1.0 { return Err("slider not clamped to max".into()); }
    Ok("all descriptors set/get/parse/sanitize correctly; NaN, inf, bad index and unknown id rejected; sliders clamp".into())
}
fn all_alternatives() -> Settings { let mut s = Settings::default(); for e in entries() { let _ = s.set(e.id, e.sample_alternative()); } s.drain_changes(); s }
fn t_ini_roundtrip(dir: &Path) -> Result<String, String> {
    let s = all_alternatives(); let p = dir.join(FILE_NAME);
    s.save(&p).map_err(|e| e.to_string())?;
    let (l, r) = Settings::load(&p);
    if r.status != LoadStatus::Loaded { return Err(format!("status {:?} warnings {:?}", r.status, r.warnings)); }
    let d = s.diff(&l);
    if !d.is_empty() { return Err(format!("{} differences, first {:?}", d.len(), d[0])); }
    let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    for needle in ["[Video]", "WindowMode=", "[Bindings]", "MoveForward=", "Version=1"] { if !text.contains(needle) { return Err(format!("file lacks {needle}")); } }
    let d0 = Settings::default(); d0.save(&p).map_err(|e| e.to_string())?;
    let (l0, r0) = Settings::load(&p);
    if r0.status != LoadStatus::Loaded || !l0.same_values(&d0) { return Err("defaults round trip failed".into()); }
    Ok(format!("{} settings, {} bytes, human-readable sections present", entries().len(), text.len()))
}
fn t_repair(_: &Path) -> Result<String, String> {
    let ini = "[Settings]\nVersion=1\n[Video]\nResolution=999999x1\nVsync=maybe\nShadows=ultra\nBrightness=7.5\nWindowMode=WindowedFullscreen\nFrameRateLimit=60\n[Input]\nMouseSensitivity=-4\nMouseSensitivityY=abc\nInvertY=on\n[Audio]\nMusicVolume=nan\n[Bindings]\nJump=Banana,F2\nCrouch=C\n";
    let (s, w) = Settings::from_ini_bytes(ini.as_bytes()).map_err(|e| e)?;
    s.validate()?;
    let checks = [
        (s.video.width == MAX_RES.0 && s.video.height == 360, "resolution clamped"), (s.video.vsync, "bad bool -> default (true)"), (s.video.shadows == 3, "bad enum -> default epic"), (s.video.brightness == 1.0, "brightness clamped"),
        (s.video.window_mode == WindowMode::WindowedFullscreen, "label alias accepted"), (s.video.frame_rate_limit == FrameRateLimit::Fps60, "enum parsed"), (s.input.mouse_sensitivity == 0.0, "sensitivity clamped to min (1.8: the bare AnalogSlider range 0..1)"),
        (s.input.mouse_sensitivity_y == MOUSE_SENSITIVITY_DEFAULT_SLIDER, "garbage float -> default"), (s.input.invert_y, "on = true"),
        // The default of Music Volume is the recovered SoundClass "Music" base volume (0.5), NOT 1.0: this test used to hard-code 1.0
        // (a stale expectation from before the sound-class defaults were recovered), which is why self_test_all_pass failed on 1.10 too.
        (entry("audio.music_volume").map(|e| e.default_value()) == Some(Value::Float(s.audio.music_volume)), "nan -> the descriptor default (SoundClass Music base volume)"),
        (s.input.bindings.get(A_JUMP) == [Key::from_name("SpaceBar").unwrap(), Key::NONE], "unknown key -> default binding"), (s.input.bindings.get(A_CROUCH)[0] == Key::from_name("C").unwrap(), "valid rebind kept"),
    ];
    for (ok, what) in checks { if !ok { return Err(format!("failed: {what}; warnings: {w:?}")); } }
    if w.len() < 7 { return Err(format!("expected >= 7 warnings, got {w:?}")); }
    Ok(format!("{} repairs reported", w.len()))
}
fn t_unknown(dir: &Path) -> Result<String, String> {
    let ini = "[Settings]\nVersion=1\nFutureThing=3\n[Video]\nVsync=false\nHologram=on\n[Mystery]\nA=1\nB=2\n[Input]\nMouseSensitivity=0.5\n";
    let (s, w) = Settings::from_ini_bytes(ini.as_bytes())?;
    if s.video.vsync || s.input.mouse_sensitivity != 0.5 { return Err("known keys beside unknown ones were not read".into()); }
    let p = dir.join(FILE_NAME); s.save(&p).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    for n in ["FutureThing=3", "Hologram=on", "[Mystery]", "A=1", "B=2"] { if !text.contains(n) { return Err(format!("unknown entry lost on save: {n}")); } }
    let (s2, _) = Settings::from_ini_bytes(text.as_bytes())?;
    if s2.to_ini() != text { return Err("save is not stable across a load/save cycle".into()); }
    Ok(format!("{} warnings, unknown entries preserved and save/load/save is byte-stable", w.len()))
}
fn t_corrupt(dir: &Path) -> Result<String, String> {
    let p = dir.join(FILE_NAME);
    let mut rng = 0x9E3779B9u32; let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    cases.push(("binary garbage", (0..4096).map(|_| { rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5; rng as u8 }).collect()));
    cases.push(("invalid utf-8", vec![b'[', b'V', 0xFF, 0xFE, b']', b'\n']));
    cases.push(("text without sections", b"hello world\nthis is not an ini\n".to_vec()));
    cases.push(("NUL bytes", b"[Video]\0\nVsync=true\n".to_vec()));
    cases.push(("oversized", vec![b'a'; MAX_FILE_BYTES + 1]));
    for (name, bytes) in &cases {
        std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
        let (s, r) = Settings::load(&p);
        if r.status != LoadStatus::Corrupt { return Err(format!("{name}: status {:?}", r.status)); }
        if !s.same_values(&Settings::default()) { return Err(format!("{name}: not defaults")); }
        let b = r.backup.ok_or(format!("{name}: no backup"))?;
        if std::fs::read(&b).map_err(|e| e.to_string())? != *bytes { return Err(format!("{name}: backup differs")); }
        if p.exists() { return Err(format!("{name}: bad file left in place")); }
    }
    let (s, r) = Settings::load(&dir.join("missing.ini")); if r.status != LoadStatus::Missing || !s.same_values(&Settings::default()) { return Err("missing file".into()); }
    std::fs::write(&p, "   \n\n").map_err(|e| e.to_string())?; let (_, r) = Settings::load(&p); if r.status != LoadStatus::Missing { return Err("blank file".into()); }
    Ok(format!("{} corrupt shapes + missing + blank handled", cases.len()))
}
fn t_prefixes(_: &Path) -> Result<String, String> {
    let text = all_alternatives().to_ini(); let bytes = text.as_bytes(); let (mut loaded, mut rejected) = (0, 0);
    for n in 0..=bytes.len() {
        match Settings::from_ini_bytes(&bytes[..n]) { Ok((s, _)) => { s.validate().map_err(|e| format!("prefix {n}: {e}"))?; loaded += 1; } Err(_) => rejected += 1 }
    }
    Ok(format!("{} prefixes: {loaded} loaded valid, {rejected} rejected, none panicked", bytes.len() + 1))
}
fn t_version(_: &Path) -> Result<String, String> {
    let (_, w) = Settings::from_ini_bytes(b"[Video]\nVsync=false\n")?; if !w.iter().any(|x| x.contains("no Version")) { return Err("missing Version not reported".into()); }
    let (s, w) = Settings::from_ini_bytes(b"[Settings]\nVersion=99\n[Video]\nVsync=false\nNewKnob=1\n")?; if s.video.vsync || !w.iter().any(|x| x.contains("newer")) { return Err("newer version not handled".into()); }
    let (s, _) = Settings::from_ini_bytes(s.to_ini().as_bytes())?; if s.loaded_version != Some(VERSION) { return Err("saved version should be current".into()); }
    let (_, w) = Settings::from_ini_bytes(b"[Settings]\nVersion=0\n[Video]\nVsync=false\n")?; if !w.iter().any(|x| x.contains("migrated")) { return Err("old version not reported".into()); }
    Ok("missing/newer/older versions reported; saves are written at the current version".into())
}
fn t_atomic(dir: &Path) -> Result<String, String> {
    let p = dir.join(FILE_NAME); let a = Settings::default(); a.save(&p).map_err(|e| e.to_string())?;
    let mut b = Settings::default(); b.set("video.vsync", Value::Bool(false)).unwrap(); b.save(&p).map_err(|e| e.to_string())?;
    if !p.exists() || p.with_extension("ini.tmp").exists() { return Err("temp file left behind / target missing".into()); }
    if Settings::load(&p).0.video.vsync { return Err("overwrite not visible".into()); }
    let blocked = dir.join("blocked"); std::fs::create_dir_all(&blocked).map_err(|e| e.to_string())?;
    if a.save(&blocked).is_ok() { return Err("saving over a directory should fail".into()); }
    if blocked.with_extension("ini.tmp").exists() { return Err("temp file leaked on failure".into()); }
    let nested = dir.join("a").join("b").join(FILE_NAME); a.save(&nested).map_err(|e| e.to_string())?;
    Ok("replace-in-place, no temp leftovers (also on failure), parent directories created".into())
}
fn t_apply(_: &Path) -> Result<String, String> {
    let mut s = Settings::default(); let mut h = RecordingVideo::default();
    let r = s.apply_video(&mut h);
    if r.applied.len() != VIDEO_GROUPS.len() || !r.ok() { return Err(format!("first apply should run every group: {:?}", r)); }
    if !h.calls.contains(&"cap 0".to_string()) || !h.calls.contains(&"shadows Some(2048)".to_string()) || !h.calls.iter().any(|c| c == "gamma 2.200") { return Err(format!("unexpected first-apply calls {:?}", h.calls)); }
    h.calls.clear(); let r = s.apply_video(&mut h); if !h.calls.is_empty() || !r.applied.is_empty() { return Err(format!("no-op apply called hooks: {:?}", h.calls)); }
    s.set("video.shadows", Value::Index(0)).unwrap(); s.set("video.brightness", Value::Float(0.8)).unwrap(); s.set("video.anti_aliasing", Value::Index(1)).unwrap();
    let r = s.apply_video(&mut h);
    if h.calls != vec!["shadows None".to_string(), "gamma 2.500".to_string(), "aa 1".to_string()] { return Err(format!("incremental apply wrong: {:?}", h.calls)); }
    if r.stored_only.contains(&"video.anti_aliasing") || r.stored_only.contains(&"video.shadows") { return Err(format!("anti-aliasing/shadows are wired, not stored-only: {:?}", r.stored_only)); }
    h.calls.clear(); s.set("video.effects", Value::Index(0)).unwrap(); let r = s.apply_video(&mut h);
    if r.stored_only.contains(&"video.effects") || r.applied != vec!["effects"] { return Err(format!("effects is wired (sg.EffectsQuality): {:?} {:?}", r.stored_only, r.applied)); }
    h.calls.clear(); s.set("video.window_mode", Value::Index(0)).unwrap(); h.fail_display = true;
    let r = s.apply_video(&mut h); if r.failed.len() != 1 || r.failed[0].0 != "display" { return Err(format!("failure not reported: {:?}", r)); }
    h.calls.clear(); h.fail_display = false; let r = s.apply_video(&mut h);
    if h.calls != vec!["display Fullscreen 1280x720".to_string()] || !r.ok() { return Err(format!("failed hook not retried: {:?}", h.calls)); }
    // the descriptor default is already Unlimited (1.8 has no separate Uncap Framerate row), so move away from it first, then back
    s.set("video.frame_rate_limit", Value::Index(FrameRateLimit::Fps60.index())).unwrap(); s.apply_video(&mut h);
    h.calls.clear(); s.set("video.frame_rate_limit", Value::Index(FrameRateLimit::Unlimited.index())).unwrap(); s.apply_video(&mut h);
    if h.calls != vec!["cap 0".to_string()] { return Err(format!("Unlimited frame rate limit should uncap: {:?}", h.calls)); }
    struct In(Vec<String>); impl InputHooks for In { fn set_look(&mut self, x: f32, y: f32, i: bool) { self.0.push(format!("look {x:.4} {y:.4} {i}")); } fn set_crouch_mode(&mut self, m: CrouchMode) { self.0.push(format!("crouch {m:?}")); } fn set_sprint_toggle(&mut self, o: bool) { self.0.push(format!("sprint {o}")); } fn set_bindings(&mut self, _: &Bindings) { self.0.push("bindings".into()); } }
    let mut i = In(vec![]); s.apply_input(&mut i); if i.0.len() != 4 { return Err(format!("first input apply: {:?}", i.0)); }
    i.0.clear(); s.set("input.invert_y", Value::Bool(true)).unwrap(); s.apply_input(&mut i); if i.0 != vec!["look 0.1750 0.1225 true".to_string()] { return Err(format!("input incremental: {:?}", i.0)); }
    struct A(u32); impl AudioHooks for A { fn set_volumes(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32) { self.0 += 1; } } let mut a = A(0);
    s.apply_audio(&mut a); s.apply_audio(&mut a); s.set("audio.music_volume", Value::Float(0.3)).unwrap(); let r = s.apply_audio(&mut a);
    if a.0 != 2 || !r.stored_only.contains(&"audio.music_volume") { return Err(format!("audio apply: calls {} report {:?}", a.0, r)); }
    Ok("first apply = all groups, no-op = none, incremental = only changed groups, failures retried, stored-only reported".into())
}
fn t_preset(_: &Path) -> Result<String, String> {
    let mut s = Settings::default();
    s.set("video.quality", Value::Index(Preset::Low.index())).unwrap();
    if s.group_levels() != [0; 6] || s.video.preset != Preset::Low { return Err("Low preset did not cascade".into()); }
    let ch = s.drain_changes(); if !ch.iter().any(|c| c.id == "video.shadows") { return Err("cascade changes not recorded".into()); }
    s.set("video.shadows", Value::Index(2)).unwrap(); if s.video.preset != Preset::Custom { return Err("single change should make preset Custom".into()); }
    s.set("video.shadows", Value::Index(0)).unwrap(); if s.video.preset != Preset::Low { return Err("matching groups should restore the preset label".into()); }
    s.set("video.quality", Value::Index(Preset::Auto.index())).unwrap(); if s.group_levels() != [0, 3, 3, 3, 3, 3] || s.video.preset != Preset::Auto { return Err("Auto should resolve to epic groups, keep the view distance and stay Auto".into()); }
    s.set("video.view_distance", Value::Index(2)).unwrap(); if s.video.preset != Preset::Auto { return Err("a view distance change should keep Auto".into()); }
    s.set("video.quality", Value::Index(Preset::Custom.index())).unwrap(); if s.group_levels() != [2, 3, 3, 3, 3, 3] { return Err("Custom must not touch groups".into()); }
    s.set("video.quality", Value::Index(Preset::Auto.index())).unwrap(); s.set("video.resolution_quality", Value::Index(1)).unwrap(); if s.video.preset != Preset::Custom { return Err("a 3D resolution change should leave Auto".into()); }
    s.set("video.quality", Value::Index(Preset::Auto.index())).unwrap(); s.set("video.shadows", Value::Index(1)).unwrap(); if s.video.preset != Preset::Custom { return Err("a group change should leave Auto".into()); }
    Ok("Low/Medium/High/Epic/Auto cascade; Custom is computed".into())
}
fn t_bindings(_: &Path) -> Result<String, String> {
    let mut s = Settings::default();
    let w = Key::from_name("w").unwrap(); if w.name() != "W" || w.vk() != Some(0x57) { return Err("key lookup".into()); }
    if Key::from_name("SPACEBAR").and_then(|k| k.vk()) != Some(0x20) || Key::from_name("MouseScrollUp").unwrap().vk().is_some() || Key::from_name("none") != Some(Key::NONE) || Key::from_name("Banana").is_some() { return Err("key table".into()); }
    if !s.input.bindings.conflicts().is_empty() { return Err(format!("default bindings conflict: {:?}", s.input.bindings.conflicts())); }
    s.set("bind.Jump", Value::Keys([Key::from_name("W").unwrap(), Key::NONE])).unwrap();
    let c = s.input.bindings.conflicts(); if c.len() != 1 || c[0].1 != vec![A_MOVE_FORWARD, A_JUMP] { return Err(format!("conflict detection {c:?}")); }
    let b = &s.input.bindings; if !b.down(A_JUMP, |vk| vk == 0x57) || b.down(A_JUMP, |vk| vk == 0x20) { return Err("poll mapping".into()); }
    // 1.8 ChangeBinding (FUN_140542480 + FUN_1405fc190): binding R to Target (Combat) clears it from Reload (Combat), keeps it on Rotate (Building)
    let mut b = Bindings::default(); let r = Key::from_name("R").unwrap();
    if b.get(A_RELOAD)[0] != r || b.get(A_ROTATE)[0] != r { return Err("ConfigD_Athena shares R between Reload and Rotate".into()); }
    let cleared = b.change_binding(A_TARGET, 0, r);
    if cleared != vec![A_RELOAD] || b.get(A_RELOAD)[0] != Key::NONE || b.get(A_ROTATE)[0] != r || b.get(A_TARGET)[0] != r { return Err(format!("group-aware rebind {cleared:?}")); }
    let cleared = b.change_binding(A_USE, 1, r); // Use has no group: exclusive with everything
    if cleared != vec![A_TARGET, A_ROTATE] { return Err(format!("ungrouped rebind {cleared:?}")); }
    let mut names: Vec<&str> = Key::all_names().collect(); names.sort_unstable(); let n = names.len(); names.dedup(); if names.len() != n { return Err("duplicate key names".into()); }
    for e in entries().iter().filter(|e| e.id.starts_with("bind.")) { if Bindings::action_index(&e.id[5..]).is_none() { return Err(format!("{} has no action", e.id)); } }
    Ok(format!("{} keys, {} actions, conflicts detected, unbound/wheel keys not pollable", n, actions().len()))
}
fn t_input(_: &Path) -> Result<String, String> {
    let live = InputLive::default();
    // 1.8: MouseSensitivity 0.07 x InputYawScale 2.5 (x FOV x 0.01111 at look time); pitch x 1.75
    if (live.deg_x - 0.175).abs() > 1e-6 || (live.deg_y - 0.1225).abs() > 1e-6 { return Err(format!("default scale {} / {} != 0.175 / 0.1225", live.deg_x, live.deg_y)); }
    let k = 80. * 0.01111;
    let (y, p) = live.look(10., 0., 100, 50, 80.); if (y - (10. + 100. * 0.175 * k)).abs() > 1e-3 || (p + 50. * 0.1225 * k).abs() > 1e-3 { return Err(format!("look {y} {p}")); }
    let mut inv = live.clone(); inv.invert_y = true; let (_, p2) = inv.look(10., 0., 100, 50, 80.); if (p2 - 50. * 0.1225 * k).abs() > 1e-3 { return Err("invert failed".into()); }
    let mut x2 = live.clone(); x2.deg_y = live.deg_y * 2.; let (_, p3) = x2.look(0., 0., 0, 10, 80.); let (_, p4) = live.look(0., 0., 0, 10, 80.); if (p3 / p4 - 2.).abs() > 1e-4 { return Err("separate Y scale failed".into()); }
    let (ys, _) = live.look(0., 0., 100, 0, 15.); if (ys - 100. * 0.175 * 15. * 0.01111).abs() > 1e-3 { return Err(format!("scope FOV term {ys}")); }
    let mut l = ModeLatch::default();
    let hold: Vec<bool> = [true, true, false, true].iter().map(|&d| l.update(d, false, false)).collect(); if hold != [true, true, false, true] { return Err("hold mode".into()); }
    let mut l = ModeLatch::default();
    let tog: Vec<bool> = [true, true, false, false, true, false].iter().map(|&d| l.update(d, true, false)).collect(); if tog != [true, true, true, true, false, false] { return Err(format!("toggle mode {tog:?}")); }
    if l.update(true, true, true) { return Err("cancel should clear the latch".into()); }
    // INVENTORY FIX3: cursor-mode entry (`clear_held`): hold sprint stops and the held key does nothing until re-pressed; a toggled sprint stays on
    let mut l = ModeLatch::default(); l.update(true, false, false); l.clear_held(false);
    let held: Vec<bool> = [true, true, false, true].iter().map(|&d| l.update(d, false, false)).collect(); if held != [false, false, false, true] { return Err(format!("clear_held hold {held:?}")); }
    let mut l = ModeLatch::default(); l.update(true, true, false); l.clear_held(true);
    let tog: Vec<bool> = [true, false, true].iter().map(|&d| l.update(d, true, false)).collect(); if tog != [true, true, false] { return Err(format!("clear_held toggle {tog:?}")); }
    if (display_exponent(display_gamma(0.5)) - 1.).abs() > 1e-5 { return Err("default brightness must be identity".into()); }
    if !(display_exponent(display_gamma(0.9)) < 1. && display_exponent(display_gamma(0.1)) > 1.) { return Err("brightness direction".into()); }
    if frame_wait(Duration::from_millis(5), FrameLimiter::period(60)) < Duration::from_millis(11) || frame_wait(Duration::from_millis(20), FrameLimiter::period(60)) != Duration::ZERO { return Err("frame_wait".into()); }
    let mut fl = FrameLimiter::new(); let t = Instant::now(); for _ in 0..10 { fl.throttle(200, false); } let el = t.elapsed().as_secs_f32();
    if !(0.045..0.25).contains(&el) { return Err(format!("10 frames at a 200 fps cap took {el:.3}s (expected ~0.05)")); }
    Ok(format!("0.1556 deg/count yaw at defaults and 80 deg FOV, invert + Y scale, hold/toggle latches, gamma identity, limiter 10 frames @200fps = {el:.3}s"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(name: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("fnrt-settings-{name}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    #[test] fn oversized_resolutions_are_clamped_to_the_texture_limit_on_load() {
        for (ini, want) in [("9000x720", (8192, 720)), ("1280x9000", (1280, 8192)), ("99999x1", (8192, MIN_RES.1)), ("4294967295x4294967295", (8192, 8192)), ("8192x8192", (8192, 8192)), ("1920x1080", (1920, 1080))] {
            let text = format!("[Settings]\nVersion=1\n[Video]\nResolution={ini}\n");
            let (s, w) = Settings::from_ini_bytes(text.as_bytes()).unwrap();
            assert_eq!((s.video.width, s.video.height), want, "Resolution={ini}: {w:?}");
        }
        let e = entry("video.resolution").unwrap();
        assert_eq!(e.sanitize(Value::Size(9000, 500)).unwrap(), Value::Size(8192, 500), "menu / script writes are clamped too");
        assert_eq!(fit_windowed_client(9000, 720, (20000, 20000), (0, 0)), (8192, 720), "even a huge work area cannot exceed the limit");
    }
    #[test] fn windowed_size_is_clamped_to_the_work_area() {
        let frame = (16, 39); let work = (2560, 1400); // 2560x1440 monitor with a 40 px taskbar
        assert_eq!(fit_windowed_client(1280, 720, work, frame), (1280, 720), "sane sizes untouched");
        assert_eq!(fit_windowed_client(2544, 1361, work, frame), (2544, 1361), "exactly filling the work area is allowed");
        assert_eq!(fit_windowed_client(99999, 99999, work, frame), (2544, 1361), "ini Resolution=99999x99999 (clamped to MAX_RES on load) fits");
        assert_eq!(fit_windowed_client(16384, 16384, work, frame), (2544, 1361));
        assert_eq!(fit_windowed_client(3840, 720, work, frame), (2544, 720), "each axis clamps on its own");
        for (w, h) in [(99999u32, 99999u32), (3840, 2160), (2560, 1400), (u32::MAX, u32::MAX), (640, 360)] {
            let (cw, ch) = fit_windowed_client(w, h, work, frame);
            assert!(cw as i32 + frame.0 <= work.0 && ch as i32 + frame.1 <= work.1, "{w}x{h} -> {cw}x{ch} outer window must fit {work:?}");
        }
    }
    #[test] fn vblank_wait_policy() {
        assert!(VblankWait::applies(true, false, false), "vsync on, windowed, limiter idle: kernel-wait for the refresh");
        assert!(!VblankWait::applies(false, false, false), "no vsync: the limiter owns pacing");
        assert!(!VblankWait::applies(true, true, false), "a cap below the refresh is paced by the limiter, never double-waited");
        assert!(!VblankWait::applies(true, false, true), "exclusive fullscreen bypasses the compositor");
        assert!(VblankWait::healthy(0, Duration::from_millis(7)) && VblankWait::healthy(0, Duration::from_millis(40)));
        assert!(!VblankWait::healthy(-2147467259, Duration::from_millis(1)), "failed HRESULT abandons the wait");
        assert!(!VblankWait::healthy(0, Duration::from_millis(900)), "a stall (display asleep, DWM restart) abandons the wait");
        let mut w = VblankWait { off: false }; assert!(w.wanted(true, false, false)); w.off = true; assert!(!w.wanted(true, false, false) && w.disabled());
    }
    #[test] fn nonsense_windowed_sizes_fall_back_sanely() {
        let (frame, work) = ((16, 39), (1920, 1040));
        assert_eq!(fit_windowed_client(0, 0, work, frame), FALLBACK_WINDOWED);
        assert_eq!(fit_windowed_client(0, 900, work, frame), FALLBACK_WINDOWED, "a zero axis is nonsense for both");
        assert_eq!(fit_windowed_client(5, 5, work, frame), (MIN_RES.0, MIN_RES.1), "tiny sizes rise to the minimum resolution");
        assert_eq!(fit_windowed_client(1280, 720, (800, 600), frame), (784, 561), "small monitor: the work area wins over the minimum");
        assert_eq!(fit_windowed_client(1280, 720, (0, 0), (50, 50)), (1, 1), "degenerate work area never yields a zero size");
        assert_eq!(fit_windowed_client(1280, 720, (1920, 1040), (-5, -5)), (1280, 720), "negative frame is treated as none");
    }
    #[test] fn self_test_all_pass() { let d = tmp("self"); let r = self_test(&d); let _ = std::fs::remove_dir_all(&d); for c in &r { assert!(c.ok, "{}: {}", c.name, c.detail); } assert_eq!(r.len(), 13); }
    #[test] fn catalog_json_matches_descriptor_table() {
        let text = fn_testdata::report_text("settings-catalog.json");
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        let list = j["entries"].as_array().unwrap();
        assert_eq!(list.len(), entries().len(), "catalog and generated table have different entry counts (regenerate)");
        for (c, e) in list.iter().zip(entries()) {
            assert_eq!(c["id"], e.id); assert_eq!(c["label"], e.label); assert_eq!(c["section"], e.section); assert_eq!(c["tab"], e.tab);
            // SCALABILITY: the port wires these two since the old tree's catalog (read-only, FortniteRuntime/reports) was generated: 3D Resolution
            // (r.ScreenPercentage) and Effects (sg.EffectsQuality: SSR + translucency lighting volume). Every other row must still match.
            // POSTFX: Motion Blur is wired (ShowFlag.MotionBlur) and its default (true) was recovered from the exe since then
            let ported_since = ["video.resolution_quality", "video.effects", "video.motion_blur", "input.sprint_cancels_reload"];
            let default_recovered_since = ["video.motion_blur", "input.sprint_cancels_reload"];
            if ported_since.contains(&e.id) { assert_eq!((c["support"].as_str(), e.support), (Some("stored-only"), Support::Wired), "{}", e.id); }
            else { assert_eq!(c["support"], if e.support == Support::Wired { "wired" } else { "stored-only" }, "{}", e.id); }
            assert_eq!(c["apply"], if e.apply == Apply::Live { "live" } else { "restart" });
            assert_eq!(c["status"], if e.recovered { "recovered" } else { "assumed" });
            if e.id == "input.crouch_mode" { assert_eq!(e.default_value(), Value::Index(1)); } else if default_recovered_since.contains(&e.id) { assert!(e.default_recovered, "{}", e.id); } else {
            assert!(c["default"]["value"].as_str().unwrap().eq_ignore_ascii_case(&e.text(&e.default_value())), "{}", e.id);
            assert_eq!(c["default"]["recovered"], e.default_recovered, "{}", e.id);
            }
            if let KindDef::Slider { min, max, step, decimals } = e.kind { let r = &c["range"]; assert!((r["min"].as_f64().unwrap() - min as f64).abs() < 1e-6 && (r["max"].as_f64().unwrap() - max as f64).abs() < 1e-6 && (r["step"].as_f64().unwrap() - step as f64).abs() < 1e-6 && r["decimals"] == decimals, "{}", e.id); }
            if let KindDef::Enum(o) = e.kind { let a = c["options"].as_array().unwrap(); assert_eq!(a.len(), o.len(), "{}", e.id); for (x, y) in a.iter().zip(o) { assert_eq!(x["id"], y.0); assert_eq!(x["label"], y.1); } }
        }
        assert_eq!(j["actions"].as_array().unwrap().len(), actions().len());
        assert_eq!(j["soundClasses"]["Master"]["volume"].as_f64().unwrap(), 0.7);
    }
    #[test] fn every_wired_entry_documents_what_it_drives_and_stored_only_says_so() {
        for e in entries() {
            match e.support { Support::Wired => assert!(!e.wired_to.is_empty(), "{} wired without wired_to", e.id), Support::StoredOnly => { assert!(e.wired_to.is_empty(), "{}", e.id); if !e.id.starts_with("bind.") { assert!(e.note.contains("STORED ONLY"), "{} note must say STORED ONLY", e.id); } } }
            if let KindDef::Slider { min, max, step, .. } = e.kind { assert!(min < max && step > 0.); let d = e.default_value(); assert!(matches!(d, Value::Float(f) if f >= min && f <= max), "{}", e.id); }
        }
        assert!(entries().iter().filter(|e| e.support == Support::Wired).count() >= 25);
    }
    #[test] fn tabs_cover_all_entries_and_are_known() { for e in entries() { assert!(TABS.contains(&e.tab), "{} tab {}", e.id, e.tab); } assert!(entries_for_tab("Video").count() >= 14); }
    #[test] fn recovered_sound_class_volumes_are_exposed_in_the_catalog_notes() { let e = entry("audio.music_volume").unwrap(); assert!(e.note.contains("0.5")); assert!(entry("audio.master_volume").unwrap().note.contains("0.7")); }
    #[test] fn wired_actions_cover_the_keys_the_runtime_polls() { for n in ["MoveForward", "MoveBackward", "MoveLeft", "MoveRight", "Jump", "Crouch", "Sprint", "Fire", "PrimaryQuickbarSlot1", "PrimaryQuickbarSlot6"] { assert!(actions()[Bindings::action_index(n).unwrap()].wired, "{n}"); } assert_eq!(actions()[A_FIRE].name, "Fire"); assert_eq!(A_SLOT1_CHECK, 14); }
    const A_SLOT1_CHECK: usize = A_PRIMARY_QUICKBAR_SLOT1;
    #[test] fn default_bindings_reproduce_the_runtimes_original_keys() {
        let b = Bindings::default(); let key = |a: usize| b.get(a)[0].vk();
        assert_eq!(key(A_MOVE_FORWARD), Some(b'W' as u16)); assert_eq!(key(A_MOVE_LEFT), Some(b'A' as u16)); assert_eq!(key(A_JUMP), Some(0x20)); assert_eq!(key(A_CROUCH), Some(0xA2)); assert_eq!(key(A_SPRINT), Some(0xA0)); assert_eq!(key(A_FIRE), Some(0x01));
        for i in 0..6 { assert_eq!(b.get(A_PRIMARY_QUICKBAR_SLOT1 + i)[0].vk(), Some(b'1' as u16 + i as u16)); }
    }
    #[test] fn mappings_match_documented_values() {
        assert_eq!(shadow_map_size(0), None); assert_eq!(shadow_map_size(3), Some(2048)); assert_eq!(post_flags(3), PostFlags { ao: true, bloom: true, vignette: true });
        assert_eq!(far_plane_m(3), fn_camera::FAR); assert_eq!(fn_camera::far_plane(), fn_camera::FAR); assert!((0..4).all(|l| far_plane_m(l) == fn_camera::FAR), "View Distance scales cull distances, not the far plane");
        assert_eq!(shadow_map_size(1), Some(1024)); assert_eq!(post_flags(1), PostFlags { ao: true, bloom: true, vignette: true }); assert_eq!(post_flags(0), PostFlags { ao: false, bloom: true, vignette: false });
        assert_eq!((0..4).map(view_distance_scale).collect::<Vec<_>>(), vec![0.5, 0.67, 0.83, 1.0]); assert_eq!((0..4).map(screen_percentage).collect::<Vec<_>>(), vec![50., 71., 87., 100.]);
        assert!((native_mouse_sensitivity(MOUSE_SENSITIVITY_DEFAULT_SLIDER) - fn_movement::MOUSE_SENSITIVITY_DEFAULT).abs() < 1e-6);
        assert!((mouse_deg_per_count(MOUSE_SENSITIVITY_DEFAULT_SLIDER) * fn_movement::mouse_fov_factor(80.) - 0.15554).abs() < 1e-4);
        assert!((mouse_pitch_deg_per_count(MOUSE_SENSITIVITY_DEFAULT_SLIDER) * fn_movement::mouse_fov_factor(80.) - 0.10888).abs() < 1e-4);
        // the defaults come from the entries: slider 0.28 = MouseSensitivity 0.07, multipliers 1.0
        let s = Settings::default(); assert_eq!(s.input.mouse_sensitivity, MOUSE_SENSITIVITY_DEFAULT_SLIDER); assert_eq!((s.input.mouse_ads_multiplier, s.input.mouse_scope_multiplier), (1., 1.));
    }
    #[test] fn changes_are_recorded_and_drained() {
        let mut s = Settings::default(); assert!(s.set("video.vsync", Value::Bool(false)).unwrap()); assert!(!s.set("video.vsync", Value::Bool(false)).unwrap());
        let c = s.drain_changes(); assert_eq!(c.len(), 1); assert_eq!(c[0], Change { id: "video.vsync", old: Value::Bool(true), new: Value::Bool(false) }); assert!(s.drain_changes().is_empty());
        let a = Settings::default(); let b = all_alternatives(); assert_eq!(a.diff(&b).len(), entries().len()); assert!(a.diff(&a).is_empty());
        s.set_text("input.mouse_sensitivity", " 0.25 ").unwrap(); assert_eq!(s.input.mouse_sensitivity, 0.25); assert!(s.set_text("video.vsync", "perhaps").is_err());
        s.reset_all(Some("Video")); assert!(s.video.vsync);
    }
    #[test] fn load_report_variants_on_disk() {
        let d = tmp("load"); let p = d.join(FILE_NAME);
        std::fs::write(&p, "[Video]\nVsync=false\n[Settings]\nVersion=1\n").unwrap(); let (s, r) = Settings::load(&p); assert_eq!(r.status, LoadStatus::Loaded); assert!(!s.video.vsync); assert_eq!(r.file_version, Some(1));
        std::fs::write(&p, "[Settings]\nVersion=1\n[Video]\nVsync=oops\n").unwrap(); let (_, r) = Settings::load(&p); assert_eq!(r.status, LoadStatus::Repaired); assert!(r.warnings[0].contains("Vsync"));
        std::fs::write(&p, "\u{feff}[Settings]\nVersion=1\n[Video]\nvsync = FALSE  \n").unwrap(); let (s, _) = Settings::load(&p); assert!(!s.video.vsync, "BOM, case, spaces tolerated");
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn hand_edited_quality_conflict_is_reconciled() {
        let (s, w) = Settings::from_ini_bytes(b"[Settings]\nVersion=1\n[Video]\nQuality=low\nShadows=medium\n").unwrap(); assert_eq!(s.video.preset, Preset::Custom); assert!(w.iter().any(|x| x.contains("disagrees")));
    }

    // ---- Video / Brightness tabs ----
    #[test] fn frame_rate_options_are_30_60_120_144_165_240_unlimited_and_persist_by_id() {
        let e = entry("video.frame_rate_limit").unwrap();
        let KindDef::Enum(o) = e.kind else { panic!() };
        assert_eq!(o.iter().map(|x| x.0).collect::<Vec<_>>(), ["30", "60", "120", "144", "165", "240", "unlimited"]);
        assert_eq!(o.iter().map(|x| x.1).collect::<Vec<_>>(), ["30 FPS", "60 FPS", "120 FPS", "144 FPS", "165 FPS", "240 FPS", "Unlimited"], "recovered FrameRateLimitText format");
        let caps: Vec<u32> = (0..7).map(|i| FrameRateLimit::from_index(i).fps()).collect(); assert_eq!(caps, [30, 60, 120, 144, 165, 240, 0]);
        assert_eq!(e.default_value(), Value::Index(FrameRateLimit::Unlimited.index()));
        let mut s = Settings::default(); s.set("video.frame_rate_limit", Value::Index(4)).unwrap(); assert_eq!(s.effective_frame_cap(), 165);
        let (l, _) = Settings::from_ini_bytes(s.to_ini().as_bytes()).unwrap(); assert_eq!(l.video.frame_rate_limit, FrameRateLimit::Fps165);
        let (old, _) = Settings::from_ini_bytes(b"[Settings]\nVersion=1\n[Video]\nFrameRateLimit=unlimited\n").unwrap(); assert_eq!(old.video.frame_rate_limit, FrameRateLimit::Unlimited, "files written before 144/165/240 existed still load");
    }
    #[test] fn brightness_maps_onto_the_recovered_gamma_range() {
        assert_eq!((GAMMA_MIN, GAMMA_MAX, GAMMA_REFERENCE), (1.7, 2.7, 2.2));
        assert!((display_gamma(0.) - 1.7).abs() < 1e-6 && (display_gamma(1.) - 2.7).abs() < 1e-6 && (display_gamma(0.5) - 2.2).abs() < 1e-6);
        assert!((display_exponent(display_gamma(0.5)) - 1.).abs() < 1e-6, "default is identity");
        assert!(display_exponent(display_gamma(0.)) > 1. && display_exponent(display_gamma(1.)) < 1.);
        assert_eq!(display_gamma(-3.), 1.7); assert_eq!(display_gamma(9.), 2.7);
        assert_eq!(entry("video.brightness").unwrap().default_value(), Value::Float(0.5));
    }
    #[test] fn anti_aliasing_and_texture_levels_are_wired_and_monotonic() {
        for id in ["video.anti_aliasing", "video.textures"] { assert_eq!(entry(id).unwrap().support, Support::Wired, "{id}"); }
        assert_eq!(entry("video.effects").unwrap().support, Support::Wired, "Effects = sg.EffectsQuality (SSR, translucency lighting volume)");
        assert_eq!(aa_params(0), None);
        let p: Vec<FxaaParams> = (1..=3).map(|l| aa_params(l).unwrap()).collect();
        assert!(p[0].subpix < p[1].subpix && p[1].subpix <= p[2].subpix && p[0].edge_threshold > p[1].edge_threshold && p[1].edge_threshold > p[2].edge_threshold && p[0].search_steps < p[1].search_steps && p[1].search_steps < p[2].search_steps && p[2].search_steps <= 12);
        assert_eq!(aa_params(9), aa_params(3));
        let t: Vec<(u32, f32)> = (0..4).map(texture_params).collect();
        assert_eq!(t, vec![(0, 1.), (0, 2.), (0, 4.), (0, 8.)]);
        assert_eq!(base_level_for(2, 10), 2); assert_eq!(base_level_for(2, 2), 1, "never past the last mip"); assert_eq!(base_level_for(5, 0), 0);
    }
    #[test] fn display_mode_helpers_dedupe_filter_and_pick_refresh() {
        let m = |w, h, hz| DisplayMode { width: w, height: h, hz };
        let modes = [m(1920, 1080, 60), m(1920, 1080, 144), m(1280, 720, 60), m(2560, 1440, 165), m(1920, 1080, 165), m(2560, 1440, 60)];
        assert_eq!(distinct_sizes(&modes, |_, _| true), vec![(1280, 720), (1920, 1080), (2560, 1440)]);
        assert_eq!(distinct_sizes(&modes, |w, _| w != 1920), vec![(1280, 720), (2560, 1440)], "invalid modes are dropped");
        assert_eq!(pick_refresh(&modes, 1920, 1080, 144), 144, "keeps the current refresh when offered");
        assert_eq!(pick_refresh(&modes, 1280, 720, 165), 60, "else the highest the size offers");
        assert_eq!(pick_refresh(&modes, 2560, 1440, 0), 165); assert_eq!(pick_refresh(&modes, 800, 600, 60), 0, "unknown size: let Windows choose");
    }
    #[test] fn frame_limiter_paces_with_vsync_only_below_the_refresh_rate() {
        assert!(FrameLimiter::paced(60, false, 165) && FrameLimiter::paced(0, false, 0), "vsync off always paces");
        assert!(!FrameLimiter::paced(165, true, 165) && !FrameLimiter::paced(0, true, 165) && !FrameLimiter::paced(240, true, 165), "at or above the refresh the display is the limit");
        assert!(FrameLimiter::paced(30, true, 165) && FrameLimiter::paced(60, true, 144) && FrameLimiter::paced(120, true, 144));
        assert!(!FrameLimiter::paced(30, true, 0), "unknown refresh: trust vsync");
    }
}
