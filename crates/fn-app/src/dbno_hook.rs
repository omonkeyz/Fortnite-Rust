use crate::arms::MontageData;
use crate::death_hook::{cues_between, montage_at, push_sound, DeathData};
use fn_animgraph::Montage;
use fn_audio_sink::SoundEvent;
use fn_net::modes::dbno;
use glam::Vec3;
use std::sync::Mutex;

pub const REVIVE_TEXT: &str = "Revive";
pub const REVIVE_CUE: &str = "/Game/Sounds/Fort_GamePlay_Sounds/Player/Revive_Player_Athena_Cue";
pub const CAMERA_VIEW_OFFSET_CM: [f32; 3] = [0., 10., 15.];
pub const CAMERA_TRANSITION_S: f32 = 2.0;
pub const REVIVE_CONE_COS: f32 = 0.5;
pub const CRAWL_LENGTH_S: f32 = 0.8;
pub const TEAMMATE_DBNO_CUE: &str = "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_DBNOTeammate_Message_Athena_Cue";
pub const CRAWL_NOTIFIES: [(f32, &str); 3] = [(0.134, "/Game/Athena/Sounds/Character/Athena_Character_DBNO_HandPlace_Cue"), (0.178_5, "/Game/Athena/Sounds/Character/Athena_Character_DBNO_Shuffle_Left_Cue"), (0.483_8, "/Game/Athena/Sounds/Character/Athena_Character_DBNO_Shuffle_Right_Cue")];

pub fn crawl_cues(crawl: &[(&str, f32, f32)]) -> Vec<&'static str> {
    let mut out = vec![];
    for (_, a, b) in crawl {
        if a == b { continue; }
        let hit = |t: f32| if b > a { t > *a && t <= *b } else { t > *a || t <= *b };
        for (t, cue) in CRAWL_NOTIFIES { if hit(t) { out.push(cue); } }
    }
    out
}
pub fn crawl_sounds(crawl: &[(&str, f32, f32)], at: Vec3) { for cue in crawl_cues(crawl) { if let Some(c) = fn_audio_sink::death_cue(cue) { push_sound(SoundEvent::BuildingCue { cue: c, pos: Some(at) }); } } }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase { Intro(f32), Knocked, Outro(f32) }

#[derive(Default)]
struct Local { phase: Option<Phase>, was: bool, cam: f32 }
static LOCAL: Mutex<Local> = Mutex::new(Local { phase: None, was: false, cam: 0. });

fn advance(m: Option<&MontageData>, t0: f32, dt: f32, at: Vec3) -> (f32, Option<Montage>) {
    let t = t0 + dt.max(0.);
    let Some(m) = m else { return (t, None) };
    for cue in cues_between(m, if t0 == 0. { -1. } else { t0 }, t) { push_sound(SoundEvent::BuildingCue { cue, pos: Some(at) }); }
    (t, montage_at(m, t))
}

pub fn local_frame(data: Option<&DeathData>, knocked: bool, dead: bool, dt: f32, pawn: Vec3) -> Vec<Montage> {
    let mut l = LOCAL.lock().unwrap_or_else(|p| p.into_inner());
    let target = if knocked { 1. } else { 0. };
    l.cam += (target - l.cam).signum() * (dt.max(0.) / CAMERA_TRANSITION_S);
    l.cam = l.cam.clamp(0., 1.);
    if dead { l.phase = None; l.was = false; l.cam = 0.; return vec![]; }
    if knocked && !l.was { l.phase = Some(Phase::Intro(0.)); eprintln!("[dbno] local pawn knocked: DBNO_Athena_Intro, FullBody DBNO states"); }
    if !knocked && l.was { l.phase = Some(Phase::Outro(0.)); eprintln!("[dbno] local pawn revived: DBNO_Athena_Outro"); }
    l.was = knocked;
    let mut out = vec![];
    match l.phase {
        Some(Phase::Intro(t)) => { let m = data.and_then(|d| d.dbno_intro.as_ref()); let (t, mo) = advance(m, t, dt, pawn); out.extend(mo.clone()); l.phase = if mo.is_some() { Some(Phase::Intro(t)) } else { Some(Phase::Knocked) }; }
        Some(Phase::Outro(t)) => { let m = data.and_then(|d| d.dbno_outro.as_ref()); let (t, mo) = advance(m, t, dt, pawn); out.extend(mo.clone()); l.phase = mo.is_some().then_some(Phase::Outro(t)); }
        _ => {}
    }
    out
}

pub fn camera_alpha() -> f32 { LOCAL.lock().map(|l| l.cam).unwrap_or(0.) }

#[derive(Clone, Debug, Default)]
pub struct RemoteDbno { pub phase: Option<Phase>, pub was: bool }

impl RemoteDbno {
    pub fn step(&mut self, data: Option<&DeathData>, knocked: bool, dt: f32, at: Vec3) -> Option<Montage> {
        if knocked && !self.was { self.phase = Some(Phase::Intro(0.)); }
        if !knocked && self.was { self.phase = Some(Phase::Outro(0.)); }
        self.was = knocked;
        match self.phase {
            Some(Phase::Intro(t)) => { let (t, mo) = advance(data.and_then(|d| d.dbno_intro.as_ref()), t, dt, at); self.phase = Some(if mo.is_some() { Phase::Intro(t) } else { Phase::Knocked }); mo }
            Some(Phase::Outro(t)) => { let (t, mo) = advance(data.and_then(|d| d.dbno_outro.as_ref()), t, dt, at); self.phase = mo.is_some().then_some(Phase::Outro(t)); mo }
            _ => None,
        }
    }
}

pub fn revive_focus(pawn: Vec3, view_fwd: Vec3, mates: &[(u8, Vec3)]) -> Option<u8> {
    let f = Vec3::new(view_fwd.x, 0., view_fwd.z).normalize_or_zero();
    mates.iter().filter_map(|(s, feet)| {
        let d = *feet - Vec3::new(pawn.x, feet.y, pawn.z);
        let dist = d.length();
        let facing = if dist > 0.3 { d.normalize().dot(f) } else { 1. };
        (dist <= fn_game::pickups::INTERACT_DISTANCE && facing >= REVIVE_CONE_COS && (feet.y - pawn.y).abs() < 2.).then_some((*s, dist))
    }).min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Revive { pub target: Option<u8>, pub sent: bool, pub started: Option<f64> }

impl Revive {
    pub fn update(&mut self, use_down: bool, focus: Option<u8>, able: bool) -> Vec<(u8, bool)> {
        let mut out = vec![];
        let want = if use_down && able { focus.or(self.target.filter(|t| Some(*t) == focus)) } else { None };
        if self.sent && want != self.target { if let Some(t) = self.target { out.push((t, false)); } self.sent = false; self.started = None; self.target = None; }
        if !self.sent { if let Some(t) = want { out.push((t, true)); self.sent = true; self.target = Some(t); } }
        out
    }
    pub fn progress(&self, now_ms: f64) -> Option<f32> { self.started.map(|s| (((now_ms - s) / 1000.) as f32 / dbno::REVIVE_S).clamp(0., 1.)) }
    pub fn clear(&mut self) { *self = Revive::default(); }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick { Teammate(u8), Killer(u8), Any(u8) }

pub fn spectate_pick(squad: bool, mates_alive: &[u8], killer: Option<u8>, killer_alive: bool, any_alive: &[u8]) -> Option<Pick> {
    if squad { if let Some(m) = mates_alive.first() { return Some(Pick::Teammate(*m)); } }
    if let (Some(k), true) = (killer, killer_alive) { return Some(Pick::Killer(k)); }
    any_alive.first().map(|s| Pick::Any(*s))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paks() -> Option<fn_pak::PakSet> {
        let dir = crate::hitreg_hook::install_dir().or_else(|| std::env::var_os("FN_ROOT").map(std::path::PathBuf::from))?;
        fn_pak::PakSet::open_install(&dir).ok()
    }
    #[test] fn dbno_values_match_athena_game_data() {
        let Some(p) = paks() else { eprintln!("no install (FN_ROOT): skipped"); return };
        let row = |r: &str| fn_game::containers::eval_keys(&fn_game::containers::curve_row(&p, "/Game/Athena/Balance/DataTables/AthenaGameData", r).unwrap(), 0.);
        assert_eq!(row("Default.DBNO.InitialHealth"), dbno::INITIAL_HEALTH);
        assert_eq!(row("Default.DBNO.BleedDamage"), dbno::BLEED_DAMAGE);
        assert_eq!(row("Default.DBNO.BleedTick"), dbno::BLEED_TICK_S);
        assert_eq!(row("Default.DBNO.ReviveHealth"), dbno::REVIVE_HEALTH);
        let props = |pkg: &str| { let pd = fn_uasset::PackageData::load(&p, pkg).unwrap(); let pk = pd.package().unwrap(); let i = pk.default_object_export().or_else(|| pk.exports.iter().position(|e| e.name.starts_with("Default__"))).unwrap(); pk.read_export(i).unwrap().to_json(&pk)["properties"].clone() };
        let bleed = props("/Game/Athena/MappedEffects/GE_Athena_DBNO_Bleed");
        assert_eq!(bleed["Period"]["Curve"]["RowName"], "Default.DBNO.BleedTick");
        assert_eq!(bleed["OngoingTagRequirements"]["IgnoreTags"]["GameplayTags"][0], "Gameplay.Status.DBNOBeingRevived");
        let gab = props("/Game/Abilities/NPC/Generic/GAB_AthenaDBNO");
        assert_eq!(gab["AbilityTriggers"][0]["TriggerTag"]["TagName"], "Event.Damage.DiedAthena");
        assert!(gab["DeathMontage"].as_str().unwrap().ends_with("DBNO_Athena_Intro"));
        let rev = props("/Game/Abilities/NPC/Generic/GAB_AthenaDBNORevive");
        assert!(rev["MontageInfo"]["MontageToPlay"].as_str().unwrap().ends_with("DBNO_Athena_Outro"));
        let cam = props("/Game/Blueprints/Camera/Athena/Athena_PlayerCamera_DBNO");
        assert_eq!(cam["TransitionTime"].as_f64().unwrap() as f32, CAMERA_TRANSITION_S);
        let off: Vec<f32> = cam["ViewTargetSpaceViewOffset"]["SamplePoints"][0]["ViewOffset"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
        assert_eq!(off, CAMERA_VIEW_OFFSET_CM.to_vec());
        let pawn = props("/Game/Athena/PlayerPawn_Athena");
        assert!(pawn["OnReviveSound"].as_str().unwrap().starts_with(REVIVE_CUE));
    }
    #[test] fn dbno_montages_are_the_cooked_ones() {
        let dir = crate::hitreg_hook::install_dir().or_else(|| std::env::var_os("FN_ROOT").map(std::path::PathBuf::from));
        let Some(dir) = dir else { eprintln!("no install: skipped"); return };
        let paks = fn_pak::PakSet::open_install(&dir).unwrap();
        let r = fn_pose::loadout::Resources::load(&fn_pose::ClipSourceDir::Pak(&paks)).unwrap();
        let d = DeathData::from_resources(&r).unwrap();
        let (i, o, k) = (d.dbno_intro.unwrap(), d.dbno_outro.unwrap(), d.death_dbno.unwrap());
        assert!((i.length - 1.766_284).abs() < 1e-3 && i.slot == "FullBody" && i.segs[0].clip.ends_with("AthenaDBNO_Intro_CMM"));
        assert!((o.length - 1.733_333).abs() < 1e-3 && o.segs[0].clip.ends_with("AthenaDBNO_Outro_CMM"));
        assert!((k.length - 1.766_667).abs() < 1e-3 && k.segs[0].clip.ends_with("Death_DBNO"));
        assert_eq!(cues_between(&i, -1., 2.), vec![fn_audio_sink::DEATH_CUES[0]]);
    }
    #[test] fn crawl_notifies_fire_once_per_crossing_and_per_distinct_clip() {
        assert_eq!(crawl_cues(&[("a", 0.1, 0.2)]), vec![CRAWL_NOTIFIES[0].1, CRAWL_NOTIFIES[1].1]);
        assert_eq!(crawl_cues(&[("a", 0.7, 0.14)]).len(), 1);
        assert_eq!(crawl_cues(&[("a", 0.45, 0.5), ("b", 0.45, 0.5)]).len(), 2);
        assert!(crawl_cues(&[("a", 0.2, 0.2)]).is_empty());
        let Some(p) = paks() else { return };
        for c in ["N", "S", "E", "W"] {
            let pd = fn_uasset::PackageData::load(&p, &format!("/Game/Animation/Game/MainPlayer/Locomotion/Medium/Male/DBNO/DBNOCrawl_{c}_CMM")).unwrap();
            let pk = pd.package().unwrap();
            let mut times = vec![]; let mut len = 0.;
            for i in 0..pk.exports.len() { let j = pk.read_export(i).map(|o| o.to_json(&pk)["properties"].clone()).unwrap_or_default(); if let Some(n) = j["Notifies"].as_array() { for e in n { if e["NotifyName"] == "PlaySound" { times.push((e["LinkValue"].as_f64().unwrap() as f32 * 1e4).round() / 1e4); } } len = j["SequenceLength"].as_f64().unwrap_or(0.) as f32; } }
            assert_eq!(times, CRAWL_NOTIFIES.iter().map(|n| n.0).collect::<Vec<_>>(), "{c}");
            assert!((len - CRAWL_LENGTH_S).abs() < 1e-6);
        }
    }
    #[test] fn revive_hold_sends_start_and_stop_once() {
        let mut r = Revive::default();
        assert!(r.update(false, Some(2), true).is_empty());
        assert_eq!(r.update(true, Some(2), true), vec![(2, true)]);
        assert!(r.update(true, Some(2), true).is_empty());
        r.started = Some(1000.);
        assert_eq!(r.progress(6000.), Some(0.5));
        assert_eq!(r.update(false, Some(2), true), vec![(2, false)]);
        assert_eq!(r.update(true, Some(2), false), vec![]);
        assert_eq!(r.update(true, Some(2), true), vec![(2, true)]);
        assert_eq!(r.update(true, None, true), vec![(2, false)]);
    }
    #[test] fn revive_focus_needs_reach_and_facing() {
        let mates = [(1u8, Vec3::new(0., 0., -2.)), (2, Vec3::new(0., 0., -5.))];
        assert_eq!(revive_focus(Vec3::ZERO, Vec3::NEG_Z, &mates), Some(1));
        assert_eq!(revive_focus(Vec3::ZERO, Vec3::Z, &mates), None);
        assert_eq!(revive_focus(Vec3::new(0., 0., 3.), Vec3::NEG_Z, &mates), None);
    }
    #[test] fn spectate_prefers_a_teammate_in_squad_play_then_the_killer() {
        assert_eq!(spectate_pick(true, &[3], Some(5), true, &[3, 5]), Some(Pick::Teammate(3)));
        assert_eq!(spectate_pick(false, &[3], Some(5), true, &[3, 5]), Some(Pick::Killer(5)));
        assert_eq!(spectate_pick(true, &[], Some(5), false, &[7]), Some(Pick::Any(7)));
        assert_eq!(spectate_pick(true, &[], None, false, &[]), None);
    }
}
