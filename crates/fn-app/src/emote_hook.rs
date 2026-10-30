use crate::arms::{Fx, MontageData};
use fn_animgraph::Montage;
use fn_audio_sink::SoundEvent;
use glam::Vec3;
use std::sync::Mutex;

pub const EMOTE_COOLDOWN_S: f32 = 3.0;
pub const MAX_PICKER_OPTIONS: usize = 8;
pub const AUDIO_FADE_OUT_S: f32 = 0.2;
pub const MOVEMENT_LEASH_M: f32 = 0.75;
pub const PICKER_MODE_EMOTE: u8 = 9;
pub const OPTION_RADIUS: f32 = 230.;
pub const POINTER_RADIUS: f32 = 130.;
pub const PAD_DEAD_ZONE_SQ: f32 = 0.25;
pub const ATHENA_GAME_DATA: &str = "/Game/Balance/AthenaGameData";
pub const DANCES_ROOT: &str = "/Game/Athena/Items/Cosmetics/Dances";
pub const CONSUMABLE_EMOTES_ROOT: &str = "/Game/Athena/Items/Cosmetics/ConsumableEmotes";
pub const VICTORY_POSES_ROOT: &str = "/Game/Athena/Items/Cosmetics/VictoryPoses";
pub const HOLSTER_STATE: &str = "HolsterWeapon";
pub const SOUND_ON_SELECT: &str = "/Game/Sounds/Fort_UI_Sounds/FrontEnd/New_FrontEnd/Fortnite_UI_MainMenu_Tab_Hover_02_Cue";
pub const SOUND_ON_ACCEPT: &str = "/Game/Sounds/Fort_UI_Sounds/FrontEnd/New_FrontEnd/Fortnite_UI_MainMenu_Generic_03_Cue";
pub const SOUND_ON_CANCEL: &str = "/Game/Sounds/Fort_UI_Sounds/FrontEnd/fort_ui_dismiss_03_Cue";
pub const GLOBAL_SINGLE_MUSIC: &str = "/Game/Sounds/Fort_Music/Dance/fort_dance_01_Cue";
pub const GLOBAL_SINGLE_MUSIC_S: f32 = 7.811_917;
pub const MUSIC_KEY_BASE: u32 = 0x5400_0000;
pub const LOCAL_KEY: u8 = 0xff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Dance, Consumable, VictoryPose }

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteDef { pub id: String, pub package: String, pub name: String, pub icon: Option<String>, pub kind: Kind, pub montage: String }

#[derive(Clone, Debug)]
pub struct EmoteData { pub def: EmoteDef, pub montage: MontageData, pub holster: Option<(f32, f32)>, pub music: Option<(f32, &'static str)>, pub cues: Vec<(f32, &'static str)>, pub steps: Vec<f32> }

#[derive(Clone, Debug, PartialEq)]
pub struct Option_ { pub label: String, pub icon: Option<String>, pub emote: usize, pub enabled: bool }

#[derive(Clone, Debug, PartialEq)]
pub struct Picker { pub options: Vec<Option_>, pub active: Option<usize>, pub pointer_deg: f32, pub pointer_visible: bool, pub cursor: (f32, f32), pub gamepad: bool }

#[derive(Clone, Debug, PartialEq)]
pub struct Playing { pub emote: usize, pub t: f32, pub stop: Option<(f32, f32)> }

impl Playing {
    pub fn new(emote: usize) -> Playing { Playing { emote, t: 0., stop: None } }
    pub fn montage(&self, d: &EmoteData) -> Option<Montage> {
        let m = &d.montage;
        let mut mo = crate::death_hook::montage_at(m, self.t)?;
        if let Some((t0, w0)) = self.stop {
            let bo = m.blend_out.0.max(1e-4);
            let x = ((self.t - t0) / bo).clamp(0., 1.);
            let k = if m.blend_out.1 == 0 { 1. - x } else { 1. - x * x * (3. - 2. * x) };
            mo.weight = (w0 * k).min(mo.weight);
            if mo.weight <= 0. { return None; }
        }
        Some(mo)
    }
    pub fn stop_now(&mut self, d: &EmoteData) { if self.stop.is_none() { self.stop = Some((self.t, d.montage.weight(self.t))); } }
    pub fn done(&self, d: &EmoteData) -> bool { self.t >= d.montage.length || self.stop.map_or(false, |(t0, _)| self.t - t0 >= d.montage.blend_out.0) }
    pub fn holstered(&self, d: &EmoteData) -> bool { !self.done(d) && d.holster.map_or(false, |(a, b)| self.t >= a && (self.t <= b || self.stop.is_some())) }
    pub fn advance(&mut self, d: &EmoteData, dt: f32) -> Vec<Ev> {
        let t0 = if self.t == 0. { -1. } else { self.t };
        self.t += dt.max(0.);
        let t1 = self.t.min(d.montage.length);
        let mut out = vec![];
        if self.stop.is_some() { return out; }
        if let Some((at, cue)) = d.music { if at > t0 && at <= t1 { out.push(Ev::Music(cue)); } }
        for (at, cue) in &d.cues { if *at > t0 && *at <= t1 { out.push(Ev::Cue(cue)); } }
        for at in &d.steps { if *at > t0 && *at <= t1 { out.push(Ev::Step); } }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev { Music(&'static str), Cue(&'static str), Step }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame { pub dt: f32, pub press: bool, pub feet: Vec3, pub airborne: bool, pub crouched: bool, pub jump_press: bool, pub weapon_ability: bool, pub weapon_start: bool, pub equip_changed: bool, pub dbno: bool, pub dead: bool, pub skydiving: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cancel { Move, Jump, Fall, Crouch, ChangeEquipment, AbilityWeapon, Dbno, Death }
impl Cancel { pub fn tag(self) -> &'static str { match self { Cancel::Move => "Gameplay.Action.Player.Move (leash 75 cm)", Cancel::Jump => "Gameplay.Action.Player.Jump", Cancel::Fall => "Gameplay.Action.Player.Fall", Cancel::Crouch => "Gameplay.Action.Player.Crouch", Cancel::ChangeEquipment => "Gameplay.Action.Player.ChangeEquipment", Cancel::AbilityWeapon => "Gameplay.Action.Player.AbilityWeapon", Cancel::Dbno => "Gameplay.Action.Player.DBNOAthena", Cancel::Death => "Gameplay.Action.Death" } } }

#[derive(Clone, Debug, Default)]
pub struct Local { pub clock: f32, pub last_emote_time: f32, pub playing: Option<Playing>, pub picker: Option<Picker>, pub anchor: Vec3, pub prev_air: bool, pub prev_crouch: bool, pub net: Vec<(u8, u8)>, pub owned: Option<Vec<String>>, pub consumed: Vec<String>, pub spent: Vec<String> }

static DATA: Mutex<Vec<EmoteData>> = Mutex::new(Vec::new());
static DEFS: Mutex<Vec<EmoteDef>> = Mutex::new(Vec::new());
static LOCAL: Mutex<Option<Local>> = Mutex::new(None);
static MUSIC_UNTIL: Mutex<(f32, u8)> = Mutex::new((0., 0));

pub fn defs_from_paks(set: &fn_pak::PakSet) -> Vec<EmoteDef> {
    let read = |long: &str, kind: Kind| -> Option<EmoteDef> {
        let c = fn_pose::cosmetics::read_definition(set, long).ok().flatten()?;
        let montage = c.animation.as_deref()?.split('.').next()?.to_owned();
        Some(EmoteDef { id: c.id.clone(), package: c.package.clone(), name: c.display_name().trim().to_owned(), icon: c.small_icon.clone().or(c.large_icon.clone()), kind, montage })
    };
    let fallback = fn_uasset::PackageData::load(set, ATHENA_GAME_DATA).ok().and_then(|pd| {
        let pk = pd.package().ok()?;
        let i = pk.exports.iter().position(|e| e.name == "AthenaGameData").or(Some(0))?;
        let j = pk.read_export(i).ok()?.to_json(&pk);
        j["properties"]["FallbackDance"].as_str().map(|s| s.split('.').next().unwrap_or(s).to_owned())
    });
    let mut out = vec![];
    match fallback.as_deref().and_then(|p| read(p, Kind::Dance)) { Some(d) => out.push(d), None => eprintln!("[emote] AthenaGameData.FallbackDance unreadable: no dance") }
    let glob = |root: &str| -> Vec<String> {
        let mut v: Vec<String> = set.glob(&format!("FortniteGame/Content{}/*.uasset", root.trim_start_matches("/Game"))).iter().filter_map(|(_, e)| fn_pak::pak_path_to_game(e.rel_path()).or_else(|| fn_pak::pak_path_to_game(&e.path))).map(|g| g.trim_end_matches(".uasset").to_owned()).collect();
        v.sort(); v.dedup(); v
    };
    for p in glob(DANCES_ROOT) { if !out.iter().any(|d| d.package == p) { if let Some(d) = read(&p, Kind::Dance) { out.push(d); } } }
    for p in glob(CONSUMABLE_EMOTES_ROOT) { if let Some(d) = read(&p, Kind::Consumable) { out.push(d); } }
    for p in glob(VICTORY_POSES_ROOT) { if let Some(d) = read(&p, Kind::VictoryPose) { out.push(d); } }
    out
}

pub fn absorb(set: &fn_pak::PakSet, r: &mut fn_pose::loadout::Resources) {
    let defs = defs_from_paks(set);
    let q: std::collections::VecDeque<String> = defs.iter().map(|d| d.montage.clone()).collect();
    if let Err(e) = r.absorb(&fn_pose::ClipSourceDir::Pak(set), q) { eprintln!("[emote] emote montages not loaded: {e}"); }
    eprintln!("[emote] {} emote definitions: {}", defs.len(), defs.iter().map(|d| format!("{} ({:?})", d.id, d.kind)).collect::<Vec<_>>().join(", "));
    if let Ok(mut g) = DEFS.lock() { *g = defs; }
}

pub fn build(def: &EmoteDef, r: &fn_pose::loadout::Resources) -> Result<EmoteData, String> {
    let raw = r.montages.get(&def.montage).ok_or_else(|| format!("montage {} was not loaded", def.montage))?;
    let montage = MontageData::from_anim_in(raw, &r.sequences)?;
    let holster = raw.notifies.iter().find(|n| n.notify_name.contains(HOLSTER_STATE) || n.notify_class.as_deref().map_or(false, |c| c.contains(HOLSTER_STATE)) || n.notify_state_object.as_deref().map_or(false, |c| c.contains(HOLSTER_STATE)))
        .map(|n| (n.link_value + n.trigger_time_offset, n.link_value + n.trigger_time_offset + n.duration));
    let cues = montage.fx.iter().filter_map(|n| match &n.fx { Fx::Sound { cue, .. } => fn_audio_sink::emote_cue(cue).map(|c| (n.time, c)), _ => None }).collect();
    let steps = raw.notifies.iter().filter(|n| n.notify_name.to_ascii_lowercase().contains("footstep")).map(|n| n.link_value + n.trigger_time_offset).collect();
    let mut music = None;
    for seg in raw.slots.iter().flat_map(|s| s.segments.iter()) {
        let Some(path) = seg.animation_path.as_deref() else { continue };
        let Some(seq) = r.sequences.get(path.split('.').next().unwrap_or(path)) else { continue };
        for n in &seq.source.notifies {
            if n.notify_class.as_deref() != Some("FortAnimNotify_PlayEmoteMusic") { continue }
            let Some(cue) = n.sound.as_deref().and_then(fn_audio_sink::emote_cue) else { continue };
            let at = seg.start_pos + (n.link_value + n.trigger_time_offset - seg.anim_start_time) / seg.play_rate.max(1e-6);
            if music.is_none() { music = Some((at, cue)); }
        }
    }
    Ok(EmoteData { def: def.clone(), montage, holster, music, cues, steps })
}

pub fn init(r: &fn_pose::loadout::Resources) {
    let defs = DEFS.lock().map(|d| d.clone()).unwrap_or_default();
    let mut data = vec![];
    for d in &defs {
        match build(d, r) {
            Ok(e) => { eprintln!("[emote] {} \"{}\": {} ({:.4} s, blend {:.2}/{:.2}), holster {:?}, music {:?}, {} cues, {} footsteps", e.def.id, e.def.name, e.def.montage.rsplit('/').next().unwrap_or(""), e.montage.length, e.montage.blend_in.0, e.montage.blend_out.0, e.holster, e.music.map(|m| m.0), e.cues.len(), e.steps.len()); data.push(e); }
            Err(err) => eprintln!("[emote] {} unavailable: {err}", d.id),
        }
    }
    if let Ok(mut g) = DATA.lock() { *g = data; }
}

pub fn with_data<R>(f: impl FnOnce(&[EmoteData]) -> R) -> R { let g = DATA.lock().unwrap_or_else(|p| p.into_inner()); f(&g) }
pub fn index_of(id: &str) -> Option<usize> { with_data(|d| d.iter().position(|e| e.def.id.eq_ignore_ascii_case(id) || e.def.name.eq_ignore_ascii_case(id))) }

pub fn picker_list(data: &[EmoteData], loadout_dance: Option<&str>, owned: &[String]) -> Vec<Option_> {
    let mut out = vec![];
    let dance = loadout_dance.and_then(|id| data.iter().position(|e| e.def.id == id && e.def.kind == Kind::Dance)).or_else(|| data.iter().position(|e| e.def.kind == Kind::Dance));
    if let Some(i) = dance { out.push(Option_ { label: data[i].def.name.clone(), icon: data[i].def.icon.clone(), emote: i, enabled: true }); }
    for (i, e) in data.iter().enumerate() {
        if e.def.kind != Kind::Consumable || !owned.iter().any(|o| o == &e.def.id) { continue }
        out.push(Option_ { label: format!("{} (x{})", e.def.name, 1), icon: e.def.icon.clone(), emote: i, enabled: true });
    }
    out.truncate(MAX_PICKER_OPTIONS);
    out
}

pub fn option_angle(i: usize, n: usize) -> f32 { 90. - (360. / n.max(1) as f32) * i as f32 }
pub fn option_position(i: usize, n: usize) -> (f32, f32) { let a = option_angle(i, n).to_radians(); (a.cos() * OPTION_RADIUS, a.sin() * -OPTION_RADIUS) }
pub fn angle_difference(a: f32, b: f32) -> f32 { let d = (a % 360. - b % 360.).abs(); if d > 180. { 360. - d } else { d } }
pub fn pointer_offset(deg: f32) -> (f32, f32) { let a = deg.to_radians(); (a.cos() * POINTER_RADIUS, a.sin() * -POINTER_RADIUS) }
pub fn option_under(deg: f32, n: usize) -> Option<usize> { (0..n.min(MAX_PICKER_OPTIONS)).find(|&i| angle_difference(option_angle(i, n), deg) <= 180. / n as f32) }

impl Picker {
    pub fn new(options: Vec<Option_>, gamepad: bool) -> Picker { Picker { options, active: None, pointer_deg: -90., pointer_visible: false, cursor: (0., 0.), gamepad } }
    pub fn n(&self) -> usize { self.options.len() }
    pub fn point(&mut self, deg: f32) -> bool {
        self.pointer_deg = deg;
        let hit = option_under(deg, self.n());
        let changed = hit.is_some() && hit != self.active;
        if changed { self.active = hit; }
        self.pointer_visible = true;
        changed
    }
    pub fn mouse(&mut self, dx: f32, dy: f32, half_w: f32, half_h: f32) -> bool {
        self.cursor.0 = (self.cursor.0 + dx).clamp(-half_w, half_w);
        self.cursor.1 = (self.cursor.1 + dy).clamp(-half_h, half_h);
        self.point((-self.cursor.1).atan2(self.cursor.0).to_degrees())
    }
    pub fn stick(&mut self, x: f32, y: f32, y_mult: f32) -> bool { if x * x + y * y <= PAD_DEAD_ZONE_SQ { return false; } self.point((y * y_mult).atan2(x).to_degrees()) }
    pub fn step_option(&mut self, dir: i32, scale: f32) -> bool {
        let n = self.n() as i32; if n == 0 { return false; }
        let cur = self.active.map_or(if dir > 0 { -1 } else { 0 }, |a| a as i32);
        let next = (cur + dir).rem_euclid(n) as usize;
        self.active = Some(next);
        self.pointer_deg = option_angle(next, self.n());
        self.pointer_visible = true;
        if !self.gamepad { let p = option_position(next, self.n()); self.cursor = (p.0 * scale, p.1 * scale); }
        true
    }
    pub fn can_confirm(&self) -> Option<usize> { self.active.filter(|&a| self.options.get(a).map_or(false, |o| o.enabled)) }
}

fn sound(cue: &'static str, pos: Option<Vec3>) { if let Some(c) = fn_audio_sink::emote_cue(cue) { crate::death_hook::push_sound(SoundEvent::BuildingCue { cue: c, pos }); } }

pub fn emit(evs: &[Ev], key: u8, at: Vec3, local: bool, clock: f32) {
    for e in evs {
        match *e {
            Ev::Music(cue) => {
                if cue == GLOBAL_SINGLE_MUSIC {
                    let mut until = MUSIC_UNTIL.lock().unwrap_or_else(|p| p.into_inner());
                    if until.0 > clock && until.1 != key { eprintln!("[emote] {cue} not started: MaxCount 1, PreventNew"); continue; }
                    *until = (clock + GLOBAL_SINGLE_MUSIC_S, key);
                }
                crate::death_hook::push_sound(SoundEvent::ContainerLoop { cue, key: MUSIC_KEY_BASE + key as u32, pos: Some(at) });
            }
            Ev::Cue(cue) => crate::death_hook::push_sound(SoundEvent::BuildingCue { cue, pos: Some(at) }),
            Ev::Step => if local { crate::death_hook::push_sound(SoundEvent::Step { gait: fn_audio_sink::Gait::Walk }); },
        }
    }
}

pub fn end_audio(key: u8) {
    crate::death_hook::push_sound(SoundEvent::ContainerLoopStop { key: MUSIC_KEY_BASE + key as u32, fade: AUDIO_FADE_OUT_S });
    if let Ok(mut u) = MUSIC_UNTIL.lock() { if u.1 == key { u.0 = 0.; } }
}

fn owned_consumables(data: &[EmoteData]) -> Vec<String> {
    if let Ok(v) = fn_live::var("FN_EMOTE_OWNED") { return v.split(',').map(|x| x.trim().to_owned()).filter(|x| !x.is_empty()).collect(); }
    let path = crate::lobby_profile::default_profile_path();
    if !path.exists() { return vec![]; }
    let p = crate::lobby_profile::Profile::load_or_new_with(&path, vec![], vec![]);
    data.iter().filter(|e| e.def.kind == Kind::Consumable && p.owns(&e.def.id)).map(|e| e.def.id.clone()).collect()
}

fn consume(id: &str) {
    let path = crate::lobby_profile::default_profile_path();
    if !path.exists() { return; }
    let mut p = crate::lobby_profile::Profile::load_or_new_with(&path, vec![], vec![]);
    if p.revoke(id) { let _ = p.flush(); eprintln!("[emote] consumable {id} used: removed from the profile (count 1 -> 0)"); }
}

pub fn gate_ok(f: &Frame) -> bool { !(f.dead || f.dbno || f.skydiving || f.weapon_ability) }

pub fn cancel_reason(l: &Local, f: &Frame) -> Option<Cancel> {
    if f.dead { return Some(Cancel::Death); }
    if f.dbno { return Some(Cancel::Dbno); }
    if f.weapon_start { return Some(Cancel::AbilityWeapon); }
    if f.equip_changed { return Some(Cancel::ChangeEquipment); }
    if f.jump_press && !f.airborne { return Some(Cancel::Jump); }
    if f.airborne && !l.prev_air { return Some(Cancel::Fall); }
    if f.crouched && !l.prev_crouch { return Some(Cancel::Crouch); }
    if (f.feet - l.anchor).length() > MOVEMENT_LEASH_M { return Some(Cancel::Move); }
    None
}

impl Local {
    pub fn play(&mut self, data: &[EmoteData], emote: usize, feet: Vec3) {
        if let Some(p) = self.playing.as_mut() { if let Some(d) = data.get(p.emote) { p.stop_now(d); } end_audio(LOCAL_KEY); }
        let Some(d) = data.get(emote) else { return };
        eprintln!("[emote] ServerPlayEmoteItem({}) -> GAB_Emote_Generic plays {} on FullBody", d.def.id, d.def.montage.rsplit('/').next().unwrap_or(""));
        if d.def.kind == Kind::Consumable { self.consumed.push(d.def.id.clone()); }
        self.playing = Some(Playing::new(emote));
        self.anchor = feet;
        self.net.push((fn_net::proto::emote_op::PLAY, emote as u8));
    }
    pub fn press(&mut self, data: &[EmoteData], f: &Frame, gamepad: bool) {
        let since = self.clock - self.last_emote_time;
        if !(since > EMOTE_COOLDOWN_S) { eprintln!("[emote] SocialPicker ignored: {:.2} s since the last open (EmoteCooldown {EMOTE_COOLDOWN_S} s)", since); return; }
        if !gate_ok(f) { eprintln!("[emote] SocialPicker ignored: Gameplay.Action.Player.InteractUse is blocked"); return; }
        self.last_emote_time = self.clock;
        let owned = self.owned.clone().unwrap_or_else(|| owned_consumables(data));
        let owned: Vec<String> = owned.into_iter().filter(|o| !self.consumed.contains(o) && !self.spent.contains(o)).collect();
        let list = picker_list(data, None, &owned);
        match list.len() {
            0 => eprintln!("[emote] no emote to play"),
            1 => { eprintln!("[emote] one picker entry: playing it at once, no radial"); let e = list[0].emote; self.picker = None; self.play(data, e, f.feet); }
            n => { eprintln!("[emote] ShowPicker({PICKER_MODE_EMOTE}): {n} options {:?}", list.iter().map(|o| o.label.as_str()).collect::<Vec<_>>()); self.picker = Some(Picker::new(list, gamepad)); }
        }
    }
    pub fn accept(&mut self, data: &[EmoteData], feet: Vec3) -> bool {
        let Some(p) = self.picker.as_ref() else { return false };
        let Some(i) = p.can_confirm() else { return false };
        let e = p.options[i].emote;
        sound(SOUND_ON_ACCEPT, None);
        self.picker = None;
        self.play(data, e, feet);
        true
    }
    pub fn cancel_picker(&mut self) { if self.picker.take().is_some() { sound(SOUND_ON_CANCEL, None); eprintln!("[emote] picker cancelled"); } }
    pub fn frame(&mut self, data: &[EmoteData], f: &Frame) -> Vec<Montage> {
        self.clock += f.dt.max(0.);
        let mut out = vec![];
        if self.playing.as_ref().map_or(false, |p| p.t == 0. && p.stop.is_none()) { self.anchor = f.feet; }
        let reason = if self.playing.as_ref().map_or(false, |p| p.stop.is_none()) { cancel_reason(self, f) } else { None };
        if let (Some(r), Some(p)) = (reason, self.playing.as_mut()) {
            if let Some(d) = data.get(p.emote) { p.stop_now(d); eprintln!("[emote] {} cancelled by {} (AbilityTagsToCancel)", d.def.id, r.tag()); }
            end_audio(LOCAL_KEY);
            self.net.push((fn_net::proto::emote_op::STOP, p.emote as u8));
        }
        if f.dead || f.dbno { self.picker = None; }
        self.prev_air = f.airborne;
        self.prev_crouch = f.crouched;
        let mut finished = false;
        if let Some(p) = self.playing.as_mut() {
            if let Some(d) = data.get(p.emote) {
                let was_stopped = p.stop.is_some();
                let evs = p.advance(d, f.dt);
                emit(&evs, LOCAL_KEY, f.feet + Vec3::Y * 0.9, true, self.clock);
                out.extend(p.montage(d));
                if p.done(d) { finished = true; if !was_stopped { eprintln!("[emote] {} completed: K2_EndAbility", d.def.id); end_audio(LOCAL_KEY); } }
            } else { finished = true; }
        }
        if finished { self.playing = None; }
        for id in std::mem::take(&mut self.consumed) { consume(&id); self.consumed_done(&id); }
        out
    }
    fn consumed_done(&mut self, id: &str) { if let Some(o) = self.owned.as_mut() { o.retain(|x| x != id); } self.spent.push(id.to_owned()); }
    pub fn holstered(&self, data: &[EmoteData]) -> bool { self.playing.as_ref().map_or(false, |p| data.get(p.emote).map_or(false, |d| p.holstered(d))) }
}

pub fn with_local<R>(f: impl FnOnce(&mut Local) -> R) -> R { let mut g = LOCAL.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(Local::default)) }

pub fn local_frame(f: &Frame, gamepad: bool) -> Vec<Montage> {
    with_data(|data| with_local(|l| { if f.press { l.press(data, f, gamepad); } l.frame(data, f) }))
}
pub fn local_holstered() -> bool { with_data(|data| with_local(|l| l.holstered(data))) }
pub fn take_net() -> Vec<(u8, u8)> { with_local(|l| std::mem::take(&mut l.net)) }
pub fn picker() -> Option<Picker> { with_local(|l| l.picker.clone()) }
pub fn picker_view(gamepad: bool) -> Option<fn_ui::screens::radial_picker_cooked::RadialView> {
    use fn_ui::screens::radial_picker_cooked::{RadialOption, RadialView};
    let p = picker()?;
    Some(RadialView { options: p.options.iter().map(|o| RadialOption { label: o.label.clone(), icon: o.icon.clone(), enabled: o.enabled }).collect(), active: p.active, pointer_deg: p.pointer_deg, pointer_visible: p.pointer_visible,
        gamepad: gamepad && p.gamepad, confirm_key: "LeftMouseButton".into(), cancel_key: "RightMouseButton".into(), option_keys: (1..=MAX_PICKER_OPTIONS).map(|i| i.to_string()).collect() })
}
pub fn picker_open() -> bool { with_local(|l| l.picker.is_some()) }
pub fn picker_mouse(dx: f32, dy: f32, half_w: f32, half_h: f32) { let sel = with_local(|l| l.picker.as_mut().map_or(false, |p| p.mouse(dx, dy, half_w, half_h))); if sel { sound(SOUND_ON_SELECT, None); } }
pub fn picker_stick(x: f32, y: f32) { let sel = with_local(|l| l.picker.as_mut().map_or(false, |p| { p.gamepad = true; p.stick(x, y, -1.) })); if sel { sound(SOUND_ON_SELECT, None); } }
pub fn picker_step(dir: i32, scale: f32) { with_local(|l| { if let Some(p) = l.picker.as_mut() { p.step_option(dir, scale); } }); }
pub fn picker_confirm(feet: Vec3) -> bool { with_data(|data| with_local(|l| l.accept(data, feet))) }
pub fn picker_choose(option: usize, feet: Vec3) -> bool {
    with_data(|data| with_local(|l| { let Some(p) = l.picker.as_mut() else { return false }; if option >= p.n() { return false; } p.active = Some(option); l.accept(data, feet) }))
}
pub fn picker_cancel() { with_local(|l| l.cancel_picker()); }
pub fn set_owned(owned: Option<Vec<String>>) { with_local(|l| l.owned = owned); }
pub fn force_play(id: &str, feet: Vec3) -> bool { let Some(i) = index_of(id) else { return false }; with_data(|data| with_local(|l| { l.picker = None; l.play(data, i, feet); })); true }

pub const KEY_CONFIRM: [u16; 1] = [0x01];
pub const KEY_CANCEL: [u16; 4] = [0x02, 0x42, 0x4E, 0x54];
pub const KEY_OPTIONS: [u16; 8] = [0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38];
static PREV: Mutex<[bool; 32]> = Mutex::new([false; 32]);
pub fn changed(i: usize, v: bool) -> bool { let mut p = PREV.lock().unwrap_or_else(|e| e.into_inner()); let was = std::mem::replace(&mut p[i % 32], v); v != was }
static OWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_input_owned(v: bool) { OWNED.store(v, std::sync::atomic::Ordering::Relaxed); }
pub fn input_owned() -> bool { OWNED.load(std::sync::atomic::Ordering::Relaxed) }
pub fn edge(i: usize, down: bool) -> bool { let mut p = PREV.lock().unwrap_or_else(|e| e.into_inner()); let was = std::mem::replace(&mut p[i % 32], down); down && !was }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PickerKeys { pub confirm: bool, pub cancel: bool, pub option: Option<usize>, pub wheel: i32, pub stick: Option<(f32, f32)> }
pub fn picker_keys(down: impl Fn(u16) -> bool, wheel: i32, pad: Option<(bool, bool, bool, bool, (f32, f32))>) -> PickerKeys {
    let mut k = PickerKeys { wheel, ..Default::default() };
    if edge(0, KEY_CONFIRM.iter().any(|c| down(*c))) { k.confirm = true; }
    if edge(1, KEY_CANCEL.iter().any(|c| down(*c))) { k.cancel = true; }
    for (i, c) in KEY_OPTIONS.iter().enumerate() { if edge(2 + i, down(*c)) && k.option.is_none() { k.option = Some(i); } }
    if let Some((a, rt, b, dd, stick)) = pad {
        if edge(12, a || rt) { k.confirm = true; }
        if edge(13, b || dd) { k.cancel = true; }
        k.stick = Some(stick);
    }
    k
}

pub fn apply_picker_keys(k: &PickerKeys, feet: Vec3, scale: f32) {
    if !picker_open() { return; }
    if let Some((x, y)) = k.stick { if x * x + y * y > PAD_DEAD_ZONE_SQ { picker_stick(x, y); } }
    if k.wheel != 0 { picker_step(k.wheel.signum(), scale); }
    if let Some(i) = k.option { picker_choose(i, feet); return; }
    if k.confirm { picker_confirm(feet); return; }
    if k.cancel { picker_cancel(); }
}

#[derive(Clone, Debug, Default)]
pub struct Remote { pub playing: Option<Playing> }
impl Remote {
    pub fn on_net(&mut self, op: u8, item: u8, slot: u8) {
        with_data(|data| {
            if op == fn_net::proto::emote_op::PLAY {
                if let Some(d) = data.get(item as usize) {
                    if let Some(p) = self.playing.as_mut() { if let Some(od) = data.get(p.emote) { p.stop_now(od); } }
                    end_audio(slot);
                    eprintln!("[emote] remote {slot} plays {} ({})", d.def.id, d.def.name);
                    self.playing = Some(Playing::new(item as usize));
                }
            } else if let Some(p) = self.playing.as_mut() {
                if let Some(d) = data.get(p.emote) { p.stop_now(d); eprintln!("[emote] remote {slot} emote {} cancelled", d.def.id); }
                end_audio(slot);
            }
        })
    }
    pub fn step(&mut self, dt: f32, at: Vec3, slot: u8, alive: bool) -> (Option<Montage>, bool) {
        with_data(|data| {
            let Some(p) = self.playing.as_mut() else { return (None, false) };
            let Some(d) = data.get(p.emote) else { self.playing = None; return (None, false) };
            if !alive && p.stop.is_none() { p.stop_now(d); end_audio(slot); }
            let was_stopped = p.stop.is_some();
            let evs = p.advance(d, dt);
            emit(&evs, slot, at + Vec3::Y * 0.9, false, with_local(|l| l.clock));
            let m = p.montage(d);
            let h = p.holstered(d);
            if p.done(d) { if !was_stopped { end_audio(slot); } self.playing = None; }
            (m, h)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn res() -> Option<(fn_pak::PakSet, fn_pose::loadout::Resources)> {
        let dir = crate::hitreg_hook::install_dir().or_else(|| std::env::var_os("FN_ROOT").map(std::path::PathBuf::from)).unwrap_or_else(|| std::path::PathBuf::from(r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame"));
        let paks = fn_pak::PakSet::open_install(&dir).ok()?;
        let r = fn_pose::loadout::Resources::load(&fn_pose::ClipSourceDir::Pak(&paks)).ok()?;
        Some((paks, r))
    }
    fn data_from(paks: &fn_pak::PakSet, r: &mut fn_pose::loadout::Resources) -> Vec<EmoteData> {
        let defs = defs_from_paks(paks);
        let q = defs.iter().map(|d| d.montage.clone()).collect();
        r.absorb(&fn_pose::ClipSourceDir::Pak(paks), q).unwrap();
        defs.iter().map(|d| build(d, r).unwrap()).collect()
    }
    #[test] fn the_1_8_emotes_match_the_cooked_assets() {
        let Some((paks, mut r)) = res() else { eprintln!("no install: skipped"); return };
        let d = data_from(&paks, &mut r);
        let ids: Vec<(&str, Kind)> = d.iter().map(|e| (e.def.id.as_str(), e.def.kind)).collect();
        assert_eq!(ids, vec![("EID_DanceMoves", Kind::Dance), ("EID_GunShow", Kind::Consumable), ("EID_RideThePony", Kind::VictoryPose)]);
        let (dm, gs, rp) = (&d[0], &d[1], &d[2]);
        assert_eq!((dm.def.name.as_str(), gs.def.name.as_str(), rp.def.name.as_str()), ("Dance Moves", "GUN SHOW", "Ride the Pony"));
        assert!(dm.def.icon.as_deref().unwrap().ends_with("Icon-DanceMoves"));
        for e in &d { assert_eq!(e.montage.slot, "FullBody", "{}", e.def.id); assert_eq!(e.montage.blend, fn_animgraph::SlotBlend::Pose); }
        assert!((dm.montage.length - 6.8).abs() < 1e-3 && (rp.montage.length - 2.5667).abs() < 1e-3 && (gs.montage.length - 8.1333).abs() < 1e-3);
        assert_eq!((dm.montage.blend_in, dm.montage.blend_out), ((0.15, 0), (0.15, 0)));
        assert_eq!((rp.montage.blend_in.0, rp.montage.blend_out.0), (0.25, 0.25));
        assert_eq!((gs.montage.blend_in.0, gs.montage.blend_out.0), (0.25, 0.4));
        assert_ne!(gs.montage.blend_in.1, 0);
        for e in &d { let (a, b) = e.holster.expect("HolsterWeapon state"); assert!(a.abs() < 1e-3 && (b - e.montage.length).abs() < 1e-2, "{} holster {a}..{b}", e.def.id); }
        let m = dm.music.expect("PlayEmoteMusic on the DanceMoves sequence");
        assert!((m.0 - 0.0316).abs() < 2e-3 && m.1 == "/Game/Sounds/Fort_Music/Dance/fort_dance_01_Cue", "{m:?}");
        assert_eq!(dm.cues.iter().map(|c| c.1).collect::<Vec<_>>(), vec!["/Game/Sounds/Fort_Music/Dance/Dance_Clap_Cue"; 2]);
        assert!((dm.cues[0].0 - 0.3904).abs() < 1e-3 && (dm.cues[1].0 - 2.4145).abs() < 1e-3);
        assert_eq!(dm.steps.len(), 16);
        assert_eq!(rp.cues.iter().map(|c| c.1).collect::<Vec<_>>(), vec!["/Game/Sounds/Fort_Music/Dance/fort_dance_music_ridethepony_Cue"]);
        assert!(rp.music.is_none() && gs.music.is_none() && gs.cues.is_empty());
    }
    #[test] fn picker_list_is_the_dance_then_owned_consumables_capped_at_eight() {
        let Some((paks, mut r)) = res() else { return };
        let d = data_from(&paks, &mut r);
        let one = picker_list(&d, None, &[]);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].label, "Dance Moves");
        let two = picker_list(&d, None, &["EID_GunShow".into()]);
        assert_eq!(two.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), vec!["Dance Moves", "GUN SHOW (x1)"]);
        assert!(!two.iter().any(|o| d[o.emote].def.kind == Kind::VictoryPose));
    }
    fn fake() -> Vec<EmoteData> {
        let seg = crate::arms::Seg { clip: "c".into(), start: 0., anim_start: 0., anim_end: 2., rate: 1., loops: 1 };
        let m = MontageData { length: 2., blend_in: (0.15, 0), blend_out: (0.15, 0), slot: "FullBody", segs: vec![seg], notifies: vec![], fx: vec![], blend: fn_animgraph::SlotBlend::Pose };
        let def = |id: &str, kind| EmoteDef { id: id.into(), package: id.into(), name: id.into(), icon: None, kind, montage: "m".into() };
        vec![EmoteData { def: def("D", Kind::Dance), montage: m.clone(), holster: Some((0., 2.)), music: None, cues: vec![], steps: vec![0.5] }, EmoteData { def: def("C", Kind::Consumable), montage: m, holster: Some((0., 2.)), music: None, cues: vec![], steps: vec![] }]
    }
    fn fr(dt: f32) -> Frame { Frame { dt, ..Default::default() } }
    #[test] fn cooldown_is_strictly_greater_than_three_seconds_and_stamped_on_open() {
        let d = fake();
        let mut l = Local { owned: Some(vec![]), ..Default::default() };
        l.frame(&d, &fr(1.0)); l.press(&d, &fr(0.), false);
        assert!(l.playing.is_none(), "1 s into the match: now - 0 <= 3");
        l.frame(&d, &fr(2.0)); l.press(&d, &fr(0.), false);
        assert!(l.playing.is_none(), "exactly 3.0 s is not > 3.0");
        l.frame(&d, &fr(0.01)); l.press(&d, &fr(0.), false);
        assert!(l.playing.is_some(), "one entry plays at once");
        assert_eq!(l.net, vec![(fn_net::proto::emote_op::PLAY, 0)]);
        let stamp = l.last_emote_time;
        l.frame(&d, &fr(1.0)); l.press(&d, &fr(0.), false);
        assert_eq!(l.last_emote_time, stamp, "a press inside the cooldown changes nothing");
    }
    #[test] fn two_entries_open_the_radial_and_accept_plays_the_option() {
        let d = fake();
        let mut l = Local { owned: Some(vec!["C".into()]), clock: 10., ..Default::default() };
        l.press(&d, &fr(0.), false);
        let p = l.picker.clone().expect("radial open");
        assert_eq!(p.n(), 2);
        assert!(l.playing.is_none());
        assert!(l.picker.as_mut().unwrap().point(-90.));
        assert_eq!(l.picker.as_ref().unwrap().active, Some(1));
        assert!(l.accept(&d, Vec3::ZERO));
        assert_eq!(l.playing.as_ref().map(|p| p.emote), Some(1));
        assert!(l.picker.is_none());
    }
    #[test] fn a_consumable_emote_is_used_up_after_one_play() {
        let d = fake();
        let mut l = Local { owned: Some(vec!["C".into()]), clock: 10., ..Default::default() };
        l.press(&d, &fr(0.), false);
        assert!(l.accept(&d, Vec3::ZERO) || { l.picker.as_mut().unwrap().active = Some(1); l.accept(&d, Vec3::ZERO) });
        l.frame(&d, &fr(0.1));
        l.clock += 5.; l.playing = None;
        l.press(&d, &fr(0.), false);
        assert!(l.picker.is_none() && l.playing.as_ref().map(|p| p.emote) == Some(0), "only the dance is left: it plays at once");
    }
    #[test] fn radial_geometry_matches_the_blueprint() {
        assert_eq!(option_angle(0, 4), 90.);
        assert_eq!(option_angle(1, 4), 0.);
        let (x, y) = option_position(0, 4); assert!(x.abs() < 1e-3 && (y + 230.).abs() < 1e-3);
        assert_eq!(angle_difference(350., 10.), 20.);
        assert_eq!(option_under(0., 2), Some(0), "the pointer at 0 deg is exactly on the 90 deg sector edge of option 0 (<=)");
        assert_eq!(option_under(-100., 2), Some(1));
        let (px, py) = pointer_offset(90.); assert!(px.abs() < 1e-3 && (py + 130.).abs() < 1e-3);
        let mut p = Picker::new(vec![Option_ { label: "a".into(), icon: None, emote: 0, enabled: true }; 3], false);
        assert!(!p.stick(0.3, 0.3, -1.), "inside the 0.25 dead zone");
        p.step_option(1, 1.); assert_eq!(p.active, Some(0));
        p.step_option(-1, 1.); assert_eq!(p.active, Some(2));
    }
    #[test] fn the_ability_cancel_rules() {
        let d = fake();
        let start = |l: &mut Local| { l.playing = None; l.clock = 100.; l.last_emote_time = 0.; l.press(&d, &fr(0.), false); assert!(l.playing.is_some()); l.net.clear(); };
        let mut l = Local { owned: Some(vec![]), ..Default::default() };
        start(&mut l);
        l.frame(&d, &fr(0.05));
        l.frame(&d, &Frame { dt: 0.1, feet: Vec3::new(0.5, 0., 0.), ..Default::default() });
        assert!(l.playing.as_ref().unwrap().stop.is_none(), "50 cm of drift stays inside the 75 cm leash");
        l.frame(&d, &Frame { dt: 0.1, feet: Vec3::new(0.8, 0., 0.), ..Default::default() });
        assert!(l.playing.as_ref().unwrap().stop.is_some(), "past the leash: Move cancels");
        assert_eq!(l.net, vec![(fn_net::proto::emote_op::STOP, 0)]);
        for (f, what) in [(Frame { jump_press: true, ..Default::default() }, "jump"), (Frame { airborne: true, ..Default::default() }, "fall"), (Frame { crouched: true, ..Default::default() }, "crouch"),
            (Frame { equip_changed: true, ..Default::default() }, "slot change"), (Frame { weapon_start: true, ..Default::default() }, "fire"), (Frame { dbno: true, ..Default::default() }, "DBNO"), (Frame { dead: true, ..Default::default() }, "death")] {
            start(&mut l);
            l.frame(&d, &Frame { dt: 0.05, ..f });
            assert!(l.playing.as_ref().map_or(true, |p| p.stop.is_some()), "{what} cancels");
        }
        start(&mut l);
        l.frame(&d, &Frame { dt: 0.05, ..Default::default() });
        assert!(l.playing.as_ref().unwrap().stop.is_none(), "nothing happening: still emoting (taking damage is not in the cancel list)");
        let mut t = 0f32; while l.playing.is_some() && t < 3. { l.frame(&d, &fr(0.1)); t += 0.1; }
        assert!(l.playing.is_none() && (t - 2.0).abs() < 0.15, "ends on montage completion ({t})");
    }
    #[test] fn the_gate_blocks_weapon_dbno_death_and_skydive() {
        let d = fake();
        for f in [Frame { weapon_ability: true, ..Default::default() }, Frame { dbno: true, ..Default::default() }, Frame { dead: true, ..Default::default() }, Frame { skydiving: true, ..Default::default() }] {
            let mut l = Local { owned: Some(vec![]), clock: 10., ..Default::default() };
            l.press(&d, &f, false);
            assert!(l.playing.is_none() && l.last_emote_time == 0., "blocked press does not stamp the cooldown");
        }
    }
    #[test] fn a_stopped_emote_blends_out_and_holsters_until_it_is_gone() {
        let d = fake();
        let mut p = Playing::new(0);
        p.advance(&d[0], 0.5);
        assert!(p.holstered(&d[0]));
        let w = p.montage(&d[0]).unwrap().weight;
        p.stop_now(&d[0]);
        p.advance(&d[0], 0.075);
        let w2 = p.montage(&d[0]).unwrap().weight;
        assert!(w2 < w && w2 > 0.);
        p.advance(&d[0], 0.1);
        assert!(p.montage(&d[0]).is_none() && p.done(&d[0]) && !p.holstered(&d[0]));
    }
}
