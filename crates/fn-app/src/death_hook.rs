use crate::arms::{Fx, MontageData, Seg};
use fn_animgraph::Montage;
use fn_audio_sink::SoundEvent;
use glam::Vec3;
use std::sync::Mutex;

pub const HIDE_AFTER_S: f32 = 2.5;
pub const PLAYER_KILLED_CUE: &str = "/Game/Athena/Sounds/Character/PlayerKilled_Cue";
pub const REACT_SECTIONS: [&str; 4] = ["None", "InFrontv2", "FrontLeft", "FrontRight"];
pub const REACT_PICK_RANGE: u32 = 3;
pub const HOLSTER_NOTIFY: &str = "HolsterWeapon";

#[derive(Clone, Debug)]
pub struct DeathData { pub death: MontageData, pub death_dbno: Option<MontageData>, pub dbno_intro: Option<MontageData>, pub dbno_outro: Option<MontageData>, pub reacts: Vec<MontageData>, pub holster: Option<(f32, f32)> }

pub fn section(m: &MontageData, start: f32, end: f32) -> MontageData {
    let inside = |t: f32| t >= start - 1e-4 && t < end - 1e-4;
    MontageData {
        length: end - start,
        segs: m.segs.iter().filter(|s| inside(s.start)).map(|s| Seg { start: s.start - start, ..s.clone() }).collect(),
        notifies: m.notifies.iter().filter(|n| inside(n.1)).map(|n| (n.0.clone(), n.1 - start)).collect(),
        fx: m.fx.iter().filter(|n| inside(n.time)).map(|n| crate::arms::FxNotify { time: n.time - start, fx: n.fx.clone() }).collect(),
        ..m.clone()
    }
}

impl DeathData {
    pub fn from_resources(r: &fn_pose::loadout::Resources) -> Result<DeathData, String> {
        use fn_pose::loadout::{DBNO_INTRO_MONTAGE, DBNO_OUTRO_MONTAGE, DEATH_DBNO_MONTAGE, DEATH_MONTAGE, HIT_REACTS_MONTAGE};
        let raw = |p: &str| r.montages.get(p).ok_or_else(|| format!("montage {p} was not loaded"));
        let get = |p: &str| raw(p).and_then(|m| MontageData::from_anim_in(m, &r.sequences));
        let death_anim = raw(DEATH_MONTAGE)?;
        let death = get(DEATH_MONTAGE)?;
        let holster = death_anim.notifies.iter().find(|n| n.notify_name == HOLSTER_NOTIFY).map(|n| (n.link_value + n.trigger_time_offset, n.link_value + n.trigger_time_offset + n.duration));
        let death_dbno = get(DEATH_DBNO_MONTAGE).ok();
        let dbno_intro = get(DBNO_INTRO_MONTAGE).ok();
        let dbno_outro = get(DBNO_OUTRO_MONTAGE).ok();
        let hr = raw(HIT_REACTS_MONTAGE)?;
        let full = MontageData::from_anim_in(hr, &r.sequences)?;
        let mut starts: Vec<(String, f32)> = hr.sections.iter().map(|s| (s.name.clone(), s.start_time)).collect();
        starts.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut reacts = vec![];
        for name in REACT_SECTIONS {
            let i = if name == "None" { 0 } else { starts.iter().position(|s| s.0 == name).ok_or_else(|| format!("{HIT_REACTS_MONTAGE}: no section {name}"))? };
            let s = starts.get(i).map_or(0., |x| x.1);
            let e = starts.get(i + 1).map_or(full.length, |x| x.1);
            reacts.push(section(&full, s, e));
        }
        Ok(DeathData { death, death_dbno, dbno_intro, dbno_outro, reacts, holster })
    }
    pub fn holstered(&self, t: f32) -> bool { self.holster.map_or(false, |(a, b)| t >= a && t <= b) }
}

pub fn montage_at(m: &MontageData, t: f32) -> Option<Montage> {
    if t < 0. || t >= m.length { return None; }
    let (clip, time) = m.sample(t)?;
    let w = m.weight(t);
    (w > 0.).then_some(Montage { slot: m.slot, clip, time, weight: w, blend: m.blend })
}

pub fn cues_between(m: &MontageData, t0: f32, t1: f32) -> Vec<&'static str> {
    m.fx.iter().filter(|n| n.time > t0 && n.time <= t1 && n.time <= m.length).filter_map(|n| match &n.fx { Fx::Sound { cue, .. } => fn_audio_sink::death_cue(cue), _ => None }).collect()
}

pub fn pick_react(seed: &mut u64) -> u32 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*seed >> 33) % REACT_PICK_RANGE as u64) as u32
}

struct Local { t: Option<f32> }
static LOCAL: Mutex<Local> = Mutex::new(Local { t: None });
static LOCAL_DBNO_DEATH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_local_dbno_death(v: bool) { LOCAL_DBNO_DEATH.store(v, std::sync::atomic::Ordering::Relaxed); }
static SOUNDS: Mutex<Vec<SoundEvent>> = Mutex::new(Vec::new());

pub fn push_sound(e: SoundEvent) { if let Ok(mut q) = SOUNDS.lock() { q.push(e); } }
pub fn take_sounds() -> Vec<SoundEvent> { SOUNDS.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default() }
static KILLED_MIX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn killed_by_local() { push_sound(SoundEvent::BuildingCue { cue: PLAYER_KILLED_CUE, pos: None }); KILLED_MIX.store(true, std::sync::atomic::Ordering::Relaxed); }
pub fn take_killed_mix() -> bool { KILLED_MIX.swap(false, std::sync::atomic::Ordering::Relaxed) }

pub fn local_frame(data: Option<&DeathData>, dead: bool, dt: f32, pawn: Vec3) -> Vec<Montage> {
    let mut l = LOCAL.lock().unwrap_or_else(|p| p.into_inner());
    if !dead { l.t = None; LOCAL_DBNO_DEATH.store(false, std::sync::atomic::Ordering::Relaxed); return vec![]; }
    let t0 = l.t;
    let t = t0.map_or(0., |t| t + dt.max(0.));
    l.t = Some(t);
    if t0.is_none() { eprintln!("[death] local pawn died: PlayerDeath_Athena, hidden after {HIDE_AFTER_S} s"); }
    let Some(d) = data else { return vec![] };
    let m = if LOCAL_DBNO_DEATH.load(std::sync::atomic::Ordering::Relaxed) { d.death_dbno.as_ref().unwrap_or(&d.death) } else { &d.death };
    for cue in cues_between(m, t0.unwrap_or(-1.), t) { push_sound(SoundEvent::BuildingCue { cue, pos: Some(pawn) }); }
    montage_at(m, t).into_iter().collect()
}

pub fn local_time() -> Option<f32> { LOCAL.lock().ok().and_then(|l| l.t) }
pub fn local_dying() -> bool { local_time().is_some() }
pub fn local_hidden() -> bool { local_time().map_or(false, |t| t >= HIDE_AFTER_S) }
pub fn local_holstered(data: Option<&DeathData>) -> bool { local_time().map_or(false, |t| data.map_or(true, |d| d.holstered(t))) }

#[cfg(test)]
mod tests {
    use super::*;
    fn res() -> Option<fn_pose::loadout::Resources> {
        let dir = crate::hitreg_hook::install_dir().or_else(|| std::env::var_os("FN_ROOT").map(std::path::PathBuf::from))?;
        let paks = fn_pak::PakSet::open_install(&dir).ok()?;
        fn_pose::loadout::Resources::load(&fn_pose::ClipSourceDir::Pak(&paks)).ok()
    }
    #[test] fn death_and_hit_react_montages_match_the_cooked_assets() {
        let Some(r) = res() else { eprintln!("no install (FN_ROOT): skipped"); return };
        let d = DeathData::from_resources(&r).unwrap();
        assert!((d.death.length - 2.633_333_4).abs() < 1e-4 && d.death.slot == "FullBody");
        assert!((d.death.blend_in.0 - 0.03).abs() < 1e-6 && (d.death.blend_out.0 - 0.2).abs() < 1e-6);
        assert_eq!(d.death.segs[0].clip, "/Game/Animation/Game/MainPlayer/Reacts/Deaths/Death_Fwd");
        let falls = cues_between(&d.death, -1., 3.);
        assert_eq!(falls, vec![fn_audio_sink::DEATH_CUES[0]; 2]);
        let (a, b) = d.holster.unwrap(); assert!(a <= 0. && b >= 2.63);
        assert_eq!(d.reacts.len(), 4);
        let lens: Vec<f32> = d.reacts.iter().map(|m| m.length).collect();
        for (l, e) in lens.iter().zip([1.0, 1.366_666_7, 1.666_666_7, 1.033_333_3]) { assert!((l - e).abs() < 1e-3, "{lens:?}"); }
        for m in &d.reacts { assert_eq!((m.slot, m.segs.len()), ("AdditiveFullBody", 1)); assert!(m.segs[0].start.abs() < 1e-4); }
        assert!(d.reacts[1].segs[0].clip.ends_with("FromStanding_InFront_CMM"));
    }
    #[test] fn react_pick_never_reaches_front_right() {
        let mut s = 7u64;
        let picks: Vec<u32> = (0..300).map(|_| pick_react(&mut s)).collect();
        assert!(picks.iter().all(|p| *p < 3) && (0..3).all(|k| picks.contains(&k)));
    }
    #[test] fn local_death_runs_the_montage_then_hides_the_pawn() {
        let m = MontageData { length: 2.0, blend_in: (0.03, 1), blend_out: (0.2, 1), slot: "FullBody", segs: vec![Seg { clip: "d".into(), start: 0., anim_start: 0., anim_end: 2., rate: 1., loops: 1 }], notifies: vec![], fx: vec![crate::arms::FxNotify { time: 0.5, fx: Fx::Sound { cue: format!("{0}.x", fn_audio_sink::DEATH_CUES[0]), pitch: 1., volume: 1. } }], blend: fn_animgraph::SlotBlend::Pose };
        let d = DeathData { death: m, death_dbno: None, dbno_intro: None, dbno_outro: None, reacts: vec![], holster: Some((0., 2.)) };
        take_sounds();
        assert!(local_frame(Some(&d), false, 0.1, Vec3::ZERO).is_empty() && !local_dying());
        let mut seen = 0;
        for _ in 0..30 { if !local_frame(Some(&d), true, 0.1, Vec3::ZERO).is_empty() { seen += 1; } }
        assert!(seen >= 18 && local_hidden() && local_holstered(Some(&d)) == false);
        assert_eq!(take_sounds().len(), 1);
        local_frame(Some(&d), false, 0.1, Vec3::ZERO);
        assert!(!local_dying());
    }
}
