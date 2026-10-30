//! `--audio-test [out.wav]`: a scripted gameplay run (idle, jog, sprint + jump, crouch walk, walk up to a tree, 12 pickaxe hits until it falls, walk to a
//! rock, 3 hits) driven through the REAL `GameSim` + `GameplayAudio` + `EngineSink` and rendered offline (no device, deterministic, 48 kHz stereo) into a
//! WAV, followed by probes (listener rotation vs a fixed source, Settings volume sliders, mute-when-unfocused, menu sounds). The run is analysed:
//! every logged event against the game data (swing = TriggerAbility notify, 12 hits to fell the tree, jump / land frames, foot-plant intervals), peak /
//! RMS / clipping, pan against the listener geometry, volume ratios, the frame cost of the audio code on the main thread. A second render without
//! the ambience bed (`<out>-dry.wav`) is what the per-event measurements read. Exit code 0 only when every check passes.
use super::*;
use fn_audio::library::Library;
use fn_audio::mixer::AudioEngine;
use fn_audio_sink::EngineSink;
use fn_game::{FrameInput, GameSim};
use glam::Vec3;
use std::time::Instant;

const RATE: u32 = 48_000;
const FPS: f32 = 60.;
const FRAME: usize = (RATE as f32 / FPS) as usize; // 800 audio frames per game frame

#[derive(Clone, Debug)]
struct Logged { t: f32, ev: SoundEvent, eye: Vec3, fwd: Vec3, up: Vec3 }

/// Forwards to the real sink and logs what the game asked for, with the audio clock and the listener at that moment.
struct Tee { sink: EngineSink, log: Vec<Logged>, now: f32, eye: Vec3, fwd: Vec3, up: Vec3 }
impl AudioSink for Tee {
    fn event(&mut self, e: SoundEvent) { self.log.push(Logged { t: self.now, ev: e, eye: self.eye, fwd: self.fwd, up: self.up }); self.sink.event(e); }
    fn listener(&mut self, p: Vec3, f: Vec3, u: Vec3) { (self.eye, self.fwd, self.up) = (p, f, u); self.sink.listener(p, f, u); }
    fn set_volumes(&mut self, m: f32, mu: f32, s: f32, d: f32, v: f32) { self.sink.set_volumes(m, mu, s, d, v); }
    fn set_suspended(&mut self, s: bool) { self.sink.set_suspended(s); }
    fn set_ambience(&mut self, on: bool) { self.sink.set_ambience(on); }
    fn set_quality(&mut self, h: bool) { self.sink.set_quality(h); }
}

struct Rig { eng: AudioEngine, tee: Tee, pcm: Vec<f32>, seed: u64 }
impl Rig {
    fn new(lib: &mut Library) -> Rig {
        let eng = AudioEngine::new(RATE);
        let sink = EngineSink::with_engine(eng.handle(), lib);
        Rig { eng, tee: Tee { sink, log: vec![], now: 0., eye: Vec3::ZERO, fwd: Vec3::NEG_Z, up: Vec3::Y }, pcm: vec![], seed: 9 }
    }
    fn now(&self) -> f32 { self.pcm.len() as f32 / 2. / RATE as f32 }
    /// Renders `frames` audio frames (the seed changes per block so the cues' Random nodes vary, deterministically).
    fn render(&mut self, frames: usize) { let o = self.eng.render_offline(frames, self.seed); self.seed += 1; self.pcm.extend(o); self.tee.now = self.now(); }
    fn render_secs(&mut self, s: f32) { let mut n = (s * RATE as f32) as usize; while n > 0 { let k = n.min(FRAME); self.render(k); n -= k; } }
}

struct FrameTrace { t: f32, airborne: bool, speed: f32, crouched: bool, running: bool }
struct Run { rig: Rig, trace: Vec<FrameTrace>, update_us: Vec<f32>, phases: Vec<(&'static str, f32, f32)>, game_end: f32, tree_hit_start: f32, rock_hit_start: f32 }

#[derive(Clone, Copy, PartialEq, Debug)]
enum St { Idle, Jog, Sprint, Crouch, ToTree, HitTree, AfterTree, ToRock, HitRock, Tail, Done }
impl St { fn name(self) -> &'static str { match self { St::Idle => "idle", St::Jog => "jog", St::Sprint => "sprint+jump", St::Crouch => "crouch walk", St::ToTree => "walk to tree", St::HitTree => "hit tree", St::AfterTree => "pause", St::ToRock => "walk to rock", St::HitRock => "hit rock", St::Tail => "tail", St::Done => "done" } } }

fn dist(g: &GameSim, p: Vec3) -> f32 { ((g.x - p.x).powi(2) + (g.z - p.z).powi(2)).sqrt() }

/// Plays the script through the game's own audio path. `ambience`: whether the island bed runs (the dry render has none).
fn play(lib: &mut Library, table: Arc<StepTable>, ambience: bool) -> Result<Run, String> {
    let mut rig = Rig::new(lib);
    let mut g = GameSim::new();
    let tree = Vec3::new(0., 0., -40.);
    let rock = Vec3::new(0., 0., -50.);
    g.world.props = vec![fn_combat::Harvestable::new(fn_combat::Kind::Tree, tree), fn_combat::Harvestable::new(fn_combat::Kind::Rock, rock)];
    let mut gp = GameplayAudio::with_table(table);
    let mut trace = vec![]; let mut update_us = vec![]; let mut phases: Vec<(&'static str, f32, f32)> = vec![];
    let (mut st, mut st_t, mut f) = (St::Idle, 0f32, 0u32);
    let (mut tree_hit_start, mut rock_hit_start) = (0f32, 0f32);
    let mut rock_hits0 = 0;
    let dt = 1. / FPS;
    let mut phase_start = 0f32;
    // the app applies the Settings once at start-up (default sliders: master 1.0, music 0.5, sfx 0.5, dialog 0.8): the run is mixed like the real game
    fn_settings::Settings::default().apply_audio(&mut SinkHooks(&mut rig.tee));
    while st != St::Done {
        let t = f as f32 * dt;
        if t > 90. { return Err(format!("the script did not finish in 90 s (stuck in {st:?})")); }
        let prev = st;
        let mut inp = FrameInput { equip: f == 0, ..Default::default() };
        match st {
            St::Idle => { if st_t >= 0.8 { st = St::Jog; } }
            St::Jog => { inp.forward = 1.; if st_t >= 1.5 { st = St::Sprint; } }
            St::Sprint => { inp.forward = 1.; inp.sprint_key = true; inp.jump = (1.8..1.87).contains(&st_t); if st_t >= 4.8 { st = St::Crouch; } }
            St::Crouch => { inp.forward = 1.; inp.crouch_key = true; if st_t >= 1.6 { st = St::ToTree; } }
            St::ToTree => { inp.forward = 1.; if dist(&g, tree) <= 2.4 { st = St::HitTree; } }
            St::HitTree => { inp.attack = true; if !g.world.props[0].alive() { st = St::AfterTree; } }
            St::AfterTree => { if st_t >= 1.2 { st = St::ToRock; } }
            St::ToRock => { inp.forward = 1.; if dist(&g, rock) <= 3.0 { st = St::HitRock; rock_hits0 = g.world.hits; } }
            St::HitRock => { inp.attack = true; if g.world.hits >= rock_hits0 + 3 { st = St::Tail; } }
            St::Tail => { if st_t >= 1.5 { st = St::Done; } }
            St::Done => {}
        }
        if st != prev {
            phases.push((prev.name(), phase_start, t)); phase_start = t; st_t = 0.;
            if st == St::HitTree { tree_hit_start = t; }
            if st == St::HitRock { rock_hit_start = t; }
        }
        // the app's order: sim step, then the audio update, then the frame is rendered
        g.step(&inp, dt);
        rig.tee.now = rig.now();
        let t0 = Instant::now();
        if ambience { gp.update(&mut rig.tee, &g, dt); } else { let mut quiet = Quiet(&mut rig.tee); gp.update(&mut quiet, &g, dt); }
        update_us.push(t0.elapsed().as_secs_f32() * 1e6);
        let c = &g.controller;
        trace.push(FrameTrace { t: rig.now(), airborne: c.airborne(), speed: c.velocity.length(), crouched: c.crouched, running: c.running });
        rig.render(FRAME);
        st_t += dt; f += 1;
    }
    let game_end = rig.now();
    Ok(Run { rig, trace, update_us, phases, game_end, tree_hit_start, rock_hit_start })
}
/// Same as the tee but swallows the ambience request (the dry render).
struct Quiet<'a>(&'a mut Tee);
impl AudioSink for Quiet<'_> {
    fn event(&mut self, e: SoundEvent) { self.0.event(e); }
    fn listener(&mut self, p: Vec3, f: Vec3, u: Vec3) { self.0.listener(p, f, u); }
    fn set_ambience(&mut self, _on: bool) {}
}

// ---------------------------------------------------------------------------------------------------------------------------------
// analysis helpers
// ---------------------------------------------------------------------------------------------------------------------------------
fn win(pcm: &[f32], t0: f32, t1: f32) -> &[f32] {
    let a = ((t0.max(0.) * RATE as f32) as usize * 2).min(pcm.len() & !1); let b = ((t1.max(0.) * RATE as f32) as usize * 2).min(pcm.len() & !1);
    &pcm[a..b.max(a)]
}
fn peak(s: &[f32]) -> f32 { s.iter().fold(0f32, |a, &b| a.max(b.abs())) }
fn rms(s: &[f32]) -> f32 { if s.is_empty() { 0. } else { (s.iter().map(|&x| x as f64 * x as f64).sum::<f64>() / s.len() as f64).sqrt() as f32 } }
fn lr_rms(s: &[f32]) -> (f32, f32) { let l: Vec<f32> = s.iter().step_by(2).copied().collect(); let r: Vec<f32> = s.iter().skip(1).step_by(2).copied().collect(); (rms(&l), rms(&r)) }
/// (R - L) / (R + L) of the windowed RMS: -1 hard left, +1 hard right.
fn pan_of(s: &[f32]) -> f32 { let (l, r) = lr_rms(s); if l + r < 1e-9 { 0. } else { (r - l) / (r + l) } }
fn pct(v: &[f32], p: f32) -> f32 { if v.is_empty() { return 0.; } let mut s = v.to_vec(); s.sort_by(|a, b| a.total_cmp(b)); s[(((s.len() - 1) as f32) * p).round() as usize] }
/// Attack time (s, relative to t0): the first 2 ms window in [t0 - 2 ms, t1) whose peak is above 3e-4 AND at least 1.5x the previous window's peak (a new
/// attack rises out of silence or out of a decaying tail). None when nothing rises (a masked or soft entry).
fn onset(pcm: &[f32], t0: f32, t1: f32) -> Option<f32> {
    let step = 0.002;
    let mut prev = peak(win(pcm, t0 - step, t0));
    let mut t = t0;
    while t < t1 { let p = peak(win(pcm, t, t + step)); if p > 3e-4 && p > 1.5 * prev { return Some(t - t0); } prev = p; t += step; }
    None
}
fn ev_name(e: &SoundEvent) -> String {
    match e {
        SoundEvent::Step { gait } => format!("step {gait:?}"), SoundEvent::Land { speed } => format!("land {speed:.2} m/s"), SoundEvent::SwingAt { .. } => "swing (PrimaryFireSound)".into(),
        SoundEvent::Harvest { surface, destroyed, .. } => format!("harvest {surface:?}{}", if *destroyed { " DESTROYED" } else { "" }), other => format!("{other:?}"),
    }
}
/// events::TABLE ids a sound event plays.
fn rows(e: &SoundEvent) -> Vec<&'static str> {
    match e {
        SoundEvent::Step { gait } => vec![match gait { Gait::Walk => "move.footstep.walk", Gait::Sprint => "move.footstep.sprint", Gait::Crouch => "move.footstep.crouch", Gait::CrouchSprint => "move.footstep.crouch_sprint" }],
        SoundEvent::Footstep { sprint } => vec![if *sprint { "move.footstep.sprint" } else { "move.footstep.walk" }],
        SoundEvent::Jump => vec!["move.jump"], SoundEvent::Land { speed } => vec![if *speed >= fn_audio_sink::HARD_LAND_SPEED { "move.land_hard" } else { "move.land" }],
        SoundEvent::Swing | SoundEvent::SwingAt { .. } => vec!["melee.swing"], SoundEvent::HarvestHit => vec!["melee.impact.default", "harvest.tree.hit"],
        SoundEvent::Harvest { surface, destroyed, .. } => { let mut v = vec!["melee.impact.default"]; match surface { Surface::Tree => v.push("harvest.tree.hit"), Surface::Rock => v.push("harvest.rock.hit"), Surface::Wood => v.push("harvest.wood.hit"), Surface::Metal => v.push("harvest.metal.hit"), Surface::Default => {} } if *destroyed && *surface == Surface::Tree { v.push("harvest.tree.destroyed"); } v }
        SoundEvent::Equip => vec!["melee.equip"], SoundEvent::MenuOpen => vec!["ui.menu.open"], SoundEvent::MenuClose => vec!["ui.menu.close"], SoundEvent::MenuClick => vec!["ui.menu.click"],
        // CRACK AUDIO HOOK
        SoundEvent::CrackSpawn { .. } => vec!["crack.spawn"], SoundEvent::CrackImpact { .. } => vec!["crack.impact"],
        SoundEvent::CrackLevel { level, .. } => vec![["crack.level.1", "crack.level.2", "crack.level.3", "crack.level.4", "crack.level.5", "crack.level.6", "crack.level.7", "crack.level.8", "crack.level.9"][(*level as usize).min(8)]],
        SoundEvent::PropHit { .. } => vec!["melee.impact.default"],
        SoundEvent::PropDeath { .. } => vec![], // BUILDING: the prop's own DeathSound path, no events::TABLE row
        SoundEvent::Weapon { id, .. } | SoundEvent::WeaponLoop { id, .. } => vec![*id], // WEAPON AUDIO HOOK
        SoundEvent::WeaponLoopStop { .. } | SoundEvent::PickupCue { .. } => vec![],
        SoundEvent::BuildingCue { .. } | SoundEvent::CueVolume { .. } | SoundEvent::BuildingLoop { .. } | SoundEvent::BuildingLoopStop { .. } => vec![], // BUILDING HOOK: direct cue paths, no events::TABLE row
        SoundEvent::ContainerLoop { .. } | SoundEvent::ContainerLoopStop { .. } | SoundEvent::ContainerLoopFadeIn { .. } | SoundEvent::LoopMove { .. } | SoundEvent::LoopGain { .. } | SoundEvent::ParamLoopFadeIn { .. } | SoundEvent::LoopParam { .. } => vec![], // CONTAINERS: direct cue paths (chest_hook.rs)
        // Front-end button sounds (lobby / top bar / locker): played from the widgets' own serialized overrides, no events::TABLE row.
        SoundEvent::TopBarHover | SoundEvent::TopBarPress | SoundEvent::LobbyPlayHover | SoundEvent::LobbyPlayPress | SoundEvent::LockerItemHover | SoundEvent::LockerItemPress => vec![],
    }
}

/// CRACK AUDIO HOOK: the scripted weak-spot sequence on the shared engine: a 1300 hp tree (the oak `Athena_Tree_Large_1` as `Harvestable::proto` 0) is hit once ordinarily (generates
/// the weak spot), then the camera is turned onto every weak spot (what a player does with the blue circle) until the tree breaks. Everything runs through the real `GameSim` +
/// `GameplayAudio` + `EngineSink`, rendered offline.
fn crack_section(rig: &mut Rig, table: Arc<StepTable>, rep: &mut Report) {
    rep.line("\n== weak-spot sequence: oak (1300 hp), first hit generates the weak spot, then every hit aims at it until the tree breaks ==");
    set_prop_ids(vec!["Athena_Tree_Large_1".to_owned()]);
    rig.tee.set_ambience(false);
    rig.render_secs(1.5);
    let first = rig.tee.log.len();
    let mut g = GameSim::new();
    g.world.props.clear();
    let mut tree = fn_combat::Harvestable::new(fn_combat::Kind::Tree, Vec3::new(0., 0., 12. - 1.8));
    tree.proto = 0; tree.health = 1300.; tree.max_hp = 1300.;
    g.world.props.push(tree);
    let mut gp = GameplayAudio::with_table(table);
    let dt = 1. / FPS;
    let mut f = 0u32;
    while g.world.props[0].alive() && f < 60 * 40 {
        if let Some(s) = g.world.fx.weak_spot(0, &g.world.props[0]) { let d = s.pos - g.cam_view.eye; g.yaw = d.x.atan2(-d.z).to_degrees().rem_euclid(360.); g.pitch = d.y.atan2((d.x * d.x + d.z * d.z).sqrt()).to_degrees(); }
        g.step(&FrameInput { attack: true, ..Default::default() }, dt);
        rig.tee.now = rig.now();
        gp.update(&mut rig.tee, &g, dt);
        g.world.fx.take_events(); // the presentation drains the queue once per presented frame
        rig.render(FRAME);
        f += 1;
    }
    rig.render_secs(3.0);
    let log: Vec<Logged> = rig.tee.log[first..].to_vec();
    rep.line("  time     event                                        cue row(s)                       peak@300ms   pan geometry");
    let crack_ev = |e: &SoundEvent| matches!(e, SoundEvent::CrackSpawn { .. } | SoundEvent::CrackLevel { .. } | SoundEvent::CrackImpact { .. });
    let mut silent = vec![];
    for l in log.iter().filter(|l| crack_ev(&l.ev)) {
        let pk = peak(win(&rig.pcm, l.t, l.t + 0.3));
        if pk < 1e-4 { silent.push(l.t); }
        let cues: Vec<&str> = rows(&l.ev).iter().filter_map(|r| fn_audio::events::lookup(r)).map(|e| e.cue.rsplit('/').next().unwrap_or(e.cue)).collect();
        let (pos, ex) = match l.ev { SoundEvent::CrackSpawn { pos } | SoundEvent::CrackLevel { pos, .. } | SoundEvent::CrackImpact { pos } => (pos, expected_pan(l, pos).0), _ => (Vec3::ZERO, 0.) };
        rep.line(format!("  {:7.3}  {:<44} {:<32} {:>8.4}   {:+.2} at ({:.2},{:.2},{:.2})", l.t, format!("{:?}", l.ev).chars().take(44).collect::<String>(), cues.join("+"), pk, ex, pos.x, pos.y, pos.z));
    }
    let spawns: Vec<&Logged> = log.iter().filter(|l| matches!(l.ev, SoundEvent::CrackSpawn { .. })).collect();
    let levels: Vec<u8> = log.iter().filter_map(|l| if let SoundEvent::CrackLevel { level, .. } = l.ev { Some(level) } else { None }).collect();
    let impacts = log.iter().filter(|l| matches!(l.ev, SoundEvent::CrackImpact { .. })).count();
    // a weak-point hit that breaks the prop is a break (death sound), not a crack hit: the effect state is dropped with the prop
    let crit = g.world.events.iter().filter(|e| e.weakpoint && !e.destroyed).count();
    let first_crack = log.iter().find(|l| crack_ev(&l.ev)).map(|l| l.ev);
    rep.check(spawns.len() == 1 && matches!(first_crack, Some(SoundEvent::CrackSpawn { .. })), format!("exactly one Crack_Spawn_Cue, before any other crack sound ({} spawns)", spawns.len()));
    rep.check(crit >= 3 && levels.len() == crit && impacts == crit, format!("one crack-level cue and one Crack_Impact_Cue per surviving weak-point hit ({crit} such hits, {} levels, {impacts} impacts)", levels.len()));
    rep.check(levels.windows(2).all(|w| w[1] == (w[0] + 1).min(8)) && levels.iter().all(|&l| l <= 8), format!("crack level rises with the hit count and clamps at 8: {levels:?}"));
    let on_trunk = log.iter().all(|l| match l.ev { SoundEvent::CrackSpawn { pos } | SoundEvent::CrackLevel { pos, .. } | SoundEvent::CrackImpact { pos } => { let r = ((pos.x).powi(2) + (pos.z - 10.2).powi(2)).sqrt(); (r - 0.42).abs() < 0.1 && pos.y > 0.5 && pos.y < 2.5 } _ => true });
    rep.check(on_trunk, "every crack sound is positioned on the trunk surface (the weak spot), not at the listener");
    rep.check(silent.is_empty(), format!("every crack event is audible in the render ({} silent)", silent.len()));
    let deaths: Vec<&Logged> = log.iter().filter(|l| matches!(l.ev, SoundEvent::PropHit { destroyed: true, .. })).collect();
    let plain = log.iter().filter(|l| matches!(l.ev, SoundEvent::Harvest { .. })).count();
    rep.check(deaths.len() == 1 && plain == 0, format!("the oak's own hit / death sounds (Demo_OakTree_Impact_Cue / Demo_OakTree_Destroyed_Cue) replace the generic tree cues: {} death, {plain} generic harvest events", deaths.len()));
    let end = rig.pcm.len() as f32 / 2. / RATE as f32;
    if let Some(d) = deaths.first() { rep.check(peak(win(&rig.pcm, d.t, (d.t + 1.0).min(end))) > 1e-3, "the break is audible"); }
}
/// Pan the listener geometry predicts (+1 = source on the listener's right).
fn expected_pan(l: &Logged, pos: Vec3) -> (f32, f32) {
    let rel = pos - l.eye; let d = rel.length();
    let right = l.fwd.cross(l.up).normalize_or_zero();
    (if d > 1e-3 { right.dot(rel / d) } else { 0. }, d)
}
fn pos_of(e: &SoundEvent) -> Option<Vec3> { match e { SoundEvent::SwingAt { pos } | SoundEvent::Harvest { pos, .. } => Some(*pos), _ => None } }

struct Report { text: String, failures: Vec<String> }
impl Report {
    fn line(&mut self, s: impl AsRef<str>) { self.text.push_str(s.as_ref()); self.text.push('\n'); }
    fn check(&mut self, ok: bool, what: impl Into<String>) { let w = what.into(); self.line(format!("  [{}] {w}", if ok { "ok" } else { "FAIL" })); if !ok { self.failures.push(w); } }
}

/// The whole run; `Ok(true)` when every check passed.
pub fn run(root: &Path, out: &Path) -> Result<bool, String> {
    let t_all = Instant::now();
    if let Some(d) = out.parent() { let _ = std::fs::create_dir_all(d); }
    let mut lib = Library::open(root).map_err(|e| format!("open {}: {e}", root.display()))?;
    let table = Arc::new(load_step_table(root)?);
    let mut rep = Report { text: String::new(), failures: vec![] };
    rep.line(format!("fortnite-rs --audio-test: install {}, {} Hz stereo, {} audio frames per game frame, game 60 Hz", root.display(), RATE, FRAME));
    rep.line(format!("footstep notify table: {} clips in {} families: {}", table.clips.len(), table.families().len(), table.families().join(", ")));
    let dry = play(&mut lib, table.clone(), false)?;
    let mut wet = play(&mut lib, table.clone(), true)?;
    let sink_missing = wet.rig.tee.sink.missing.clone();
    rep.check(sink_missing.is_empty(), format!("every cue loaded ({} missing {:?})", sink_missing.len(), sink_missing));
    let game_end = wet.game_end;
    rep.line(format!("scripted gameplay: {:.2} s; phases: {}", game_end, wet.phases.iter().map(|(n, a, b)| format!("{n} {a:.2}-{b:.2}")).collect::<Vec<_>>().join(", ")));

    // ---- the gameplay events
    let log: Vec<Logged> = wet.rig.tee.log.clone();
    let game_log: Vec<&Logged> = log.iter().filter(|l| l.t <= game_end + 1e-3).collect();
    let dpcm = &dry.rig.pcm;
    rep.line("\n== events (audio clock = game clock; dry render measurements) ==");
    rep.line("  time     event                              cue row(s) [recovered|assumed]                         onset    peak   pan(meas/expected)");
    for (i, l) in game_log.iter().enumerate() {
        let next = game_log.iter().skip(i + 1).map(|n| n.t).find(|&t| t > l.t + 0.004).unwrap_or(l.t + 0.3);
        let end = (l.t + 0.25).min(next - 0.001).max(l.t + 0.02);
        let w = win(dpcm, l.t, end);
        let on = onset(dpcm, l.t, end);
        let src: Vec<String> = rows(&l.ev).iter().map(|r| format!("{r}[{}]", match fn_audio::events::lookup(r).map(|e| e.source) { Some(fn_audio::events::Source::Recovered) => "R", Some(fn_audio::events::Source::Assumed) => "A", None => "?" })).collect();
        let pan = match pos_of(&l.ev) { Some(p) => { let (e, d) = expected_pan(l, p); format!("{:+.2}/{:+.2} ({d:.1} m)", pan_of(win(dpcm, l.t, l.t + 0.15)), e) } None => "2D".to_owned() };
        rep.line(format!("  {:7.3}  {:<34} {:<55} {:>7} {:>7.4}  {}", l.t, ev_name(&l.ev), src.join("+"), on.map_or("masked".to_owned(), |o| format!("{:.1} ms", o * 1000.)), peak(w), pan));
    }
    let t_of = |p: &dyn Fn(&SoundEvent) -> bool| -> Vec<f32> { game_log.iter().filter(|l| p(&l.ev)).map(|l| l.t).collect() };
    let in_phase = |name: &str| -> (f32, f32) { wet.phases.iter().find(|p| p.0 == name).map(|p| (p.1, p.2)).unwrap_or((0., 0.)) };

    rep.line("\n== checks vs the game data ==");
    let equips = t_of(&|e| matches!(e, SoundEvent::Equip));
    rep.check(equips.len() == 1 && equips[0] <= 2. * (1. / FPS), format!("equip sound once at the start of the world ({equips:?})"));
    // jump / land against the sim state
    let jumps = t_of(&|e| matches!(e, SoundEvent::Jump)); let lands: Vec<&Logged> = game_log.iter().filter(|l| matches!(l.ev, SoundEvent::Land { .. })).copied().collect();
    rep.check(jumps.len() == 1 && lands.len() == 1, format!("one jump ({}) and one landing ({})", jumps.len(), lands.len()));
    if let (Some(&j), Some(l)) = (jumps.first(), lands.first()) {
        let first_air = dry.trace.iter().find(|x| x.airborne).map(|x| x.t).unwrap_or(-1.);
        let first_ground = dry.trace.iter().skip_while(|x| !x.airborne).find(|x| !x.airborne).map(|x| x.t).unwrap_or(-1.);
        rep.check((j - first_air).abs() < 1.5 / FPS, format!("jump sound at liftoff: event {j:.3} s, first airborne frame {first_air:.3} s"));
        rep.check((l.t - first_ground).abs() < 1.5 / FPS, format!("landing sound at touch-down: event {:.3} s, first grounded frame {first_ground:.3} s", l.t));
        if let SoundEvent::Land { speed } = l.ev { rep.check(speed > 3. && speed < 12., format!("landing impact speed {speed:.2} m/s (hard-landing threshold {} m/s, ASSUMED) -> {}", fn_audio_sink::HARD_LAND_SPEED, if speed >= fn_audio_sink::HARD_LAND_SPEED { "LandHard" } else { "Land" })); }
    }
    // swings
    let swings = t_of(&|e| matches!(e, SoundEvent::SwingAt { .. }));
    let harvests: Vec<&Logged> = game_log.iter().filter(|l| matches!(l.ev, SoundEvent::Harvest { .. })).copied().collect();
    let tree_hits: Vec<&Logged> = harvests.iter().filter(|l| matches!(l.ev, SoundEvent::Harvest { surface: Surface::Tree, .. })).copied().collect();
    let rock_hits: Vec<&Logged> = harvests.iter().filter(|l| matches!(l.ev, SoundEvent::Harvest { surface: Surface::Rock, .. })).copied().collect();
    rep.check(tree_hits.len() == 12 && matches!(tree_hits.last().map(|l| l.ev), Some(SoundEvent::Harvest { destroyed: true, .. })) && tree_hits.iter().filter(|l| matches!(l.ev, SoundEvent::Harvest { destroyed: true, .. })).count() == 1,
        format!("12 tree impacts (600 hp / 50 per hit), only the 12th destroys it ({} hits)", tree_hits.len()));
    rep.check(rock_hits.len() == 3, format!("3 rock impacts ({})", rock_hits.len()));
    rep.check(swings.len() >= 15 && swings.len() <= 16, format!("a pickaxe-fire sound per swing: {} swings for {} impacts", swings.len(), harvests.len()));
    let swing_with_hit = harvests.iter().all(|h| swings.iter().any(|&s| (h.t - s).abs() < 1e-3));
    rep.check(swing_with_hit, "every impact comes with its swing in the same frame (strike = TriggerAbility notify)");
    let first_swing = swings.iter().copied().find(|&t| t >= wet.tree_hit_start).unwrap_or(-1.);
    let trig = fn_combat::TRIGGER_ABILITY[0];
    // the press happens on the frame the phase starts; its step runs in that frame, so the event lands at press + trigger rounded up to a frame
    let delay = first_swing - wet.tree_hit_start;
    rep.check(delay >= trig - 1.0 / FPS && delay <= trig + 2.5 / FPS, format!("first swing sound {delay:.4} s after the attack press (TriggerAbility notify at {trig:.4} s, frame {:.4} s)", 1. / FPS));
    let spacing: Vec<f32> = tree_hits.windows(2).map(|w| w[1].t - w[0].t).collect();
    // held attack on a damageable prop: Swing1 -> Swing2 -> Swing1 ... (no Bounce: a damaged prop is not "blocked", decomp AFortPawn vfunc 331 / ability fn 14029c670), hits at the TriggerAbility notifies
    let (s1, s2) = (0.5333f32, 0.633_333_3f32);
    let (d12, d21) = (s1 + fn_combat::TRIGGER_ABILITY[1] - fn_combat::TRIGGER_ABILITY[0], s2 + fn_combat::TRIGGER_ABILITY[0] - fn_combat::TRIGGER_ABILITY[1]);
    rep.check(!spacing.is_empty() && spacing.iter().enumerate().all(|(i, s)| (s - if i % 2 == 0 { d12 } else { d21 }).abs() < 2.5 / FPS), format!("hit spacing {:.4}..{:.4} s; alternating Swing1->Swing2 {d12:.4} s / Swing2->Swing1 {d21:.4} s from the montage data", spacing.iter().cloned().fold(9., f32::min), spacing.iter().cloned().fold(0., f32::max)));
    // footsteps per phase
    for (phase, gait, lo, hi, min_n) in [("jog", Gait::Walk, 0.20f32, 0.45f32, 3usize), ("sprint+jump", Gait::Sprint, 0.16, 0.40, 6), ("crouch walk", Gait::Crouch, 0.25, 0.80, 2)] {
        let (a, b) = in_phase(phase);
        // the foot plants of this phase once its state has taken over (a blend from the previous gait keeps the old cue for a moment)
        let steps: Vec<(f32, Gait)> = game_log.iter().filter_map(|l| if let SoundEvent::Step { gait } = l.ev { (l.t >= a + 0.6 && l.t < b).then_some((l.t, gait)) } else { None }).collect();
        let own: Vec<f32> = steps.iter().filter(|s| s.1 == gait).map(|s| s.0).collect();
        // intervals between consecutive steps on the ground (a jump leaves one long gap)
        let normal: Vec<f32> = own.windows(2).map(|w| w[1] - w[0]).filter(|&d| d < 0.9).collect();
        let mean = normal.iter().sum::<f32>() / normal.len().max(1) as f32;
        rep.check(own.len() >= min_n && !normal.is_empty() && normal.iter().all(|d| (lo..=hi).contains(d)) && own.len() == steps.len(),
            format!("{phase}: {} foot plants, all {gait:?} cue, intervals {:.3}..{:.3} s (mean {mean:.3} s; clip cycle / 2 before stride matching: jog 0.300, sprint 0.233, crouch 0.317)", steps.len(), normal.iter().cloned().fold(9., f32::min), normal.iter().cloned().fold(0., f32::max)));
    }
    let idle_steps = game_log.iter().filter(|l| matches!(l.ev, SoundEvent::Step { .. }) && l.t < in_phase("idle").1).count();
    let tree_steps = game_log.iter().filter(|l| matches!(l.ev, SoundEvent::Step { .. }) && l.t >= wet.tree_hit_start && l.t < wet.tree_hit_start + 6.).count();
    rep.check(idle_steps == 0 && tree_steps == 0, format!("no foot plants while standing (idle {idle_steps}, while hitting the tree {tree_steps})"));

    // ---- levels on the dry / wet renders
    let wpcm = &wet.rig.pcm;
    let mut diff_pcm: Vec<f32> = wpcm.iter().zip(dpcm.iter()).map(|(a, b)| a - b).collect();
    let n_game = (game_end * RATE as f32) as usize * 2;
    let amb = rms(&diff_pcm[RATE as usize..(RATE as usize * 2).min(diff_pcm.len())]);
    let amb_peak = peak(&diff_pcm[..n_game.min(diff_pcm.len())]);
    diff_pcm.clear();
    rep.line("\n== levels ==");
    // probes are appended to the wet render only; measure the game part first
    let gp = &wpcm[..n_game.min(wpcm.len())];
    let st = fn_audio::DecodedWave::new(gp.to_vec(), RATE, 2).stats();
    rep.line(format!("  game part, with ambience: peak {:.4} rms {:.4} nan {} clip-run {}; dry: peak {:.4} rms {:.4}; ambience bed rms {amb:.5} peak {amb_peak:.4}", st.peak, st.rms, st.non_finite, st.max_clip_run, peak(&dpcm[..n_game.min(dpcm.len())]), rms(&dpcm[..n_game.min(dpcm.len())])));
    rep.check(st.non_finite == 0 && st.max_clip_run == 0 && st.peak < 0.95, format!("no NaN, no clipping (peak {:.4} of full scale 1.0)", st.peak));
    rep.check(amb > 1e-4, format!("the ambience bed is audible under the game (rms {amb:.5})"));
    rep.check(st.peak > 0.02, "the run is audible (peak above 0.02)");

    // ---- pan probe: a fixed world source, the listener turns
    rep.line("\n== probes ==");
    let eye = wet.rig.tee.eye;
    wet.rig.render_secs(1.0);
    let mut pan_rows = vec![];
    for yaw in [0f32, 180., 90., 270.] {
        let a = yaw.to_radians(); let fwd = Vec3::new(a.sin(), 0., -a.cos());
        let tee = &mut wet.rig.tee;
        tee.listener(eye, fwd, Vec3::Y);
        wet.rig.render_secs(0.05);
        let pos = eye + Vec3::new(-6., 0., 0.);
        let t0 = wet.rig.now();
        wet.rig.tee.event(SoundEvent::Harvest { surface: Surface::Rock, destroyed: false, pos });
        let l = wet.rig.tee.log.last().cloned().unwrap();
        wet.rig.render_secs(1.4);
        let m = pan_of(win(&wet.rig.pcm, t0, t0 + 0.15)); let (e, _) = expected_pan(&l, pos);
        pan_rows.push((yaw, m, e));
    }
    // the dry copy of the probe is not rendered: the ambience is steady and low next to a rock impact, the sign test is robust to it
    for (yaw, m, e) in &pan_rows { rep.line(format!("  listener yaw {yaw:5.0} deg, source 6 m to the world -X: measured pan {m:+.3}, geometry {e:+.2}")); }
    rep.check(pan_rows[0].1 < -0.2 && pan_rows[1].1 > 0.2 && pan_rows[2].1.abs() < 0.2 && pan_rows[3].1.abs() < 0.2, "panning follows the listener: left when facing -Z, right when facing +Z, centred when the source is behind / ahead (folded)");
    // gameplay events: sign of the pan against the geometry where the geometry is clear
    let mut sign_checked = 0; let mut sign_ok = 0;
    for l in &game_log {
        if let Some(p) = pos_of(&l.ev) { let (e, d) = expected_pan(l, p); if e.abs() > 0.05 && d > 2. { sign_checked += 1; if (pan_of(win(dpcm, l.t, l.t + 0.15)) > 0.) == (e > 0.) { sign_ok += 1; } } }
    }
    rep.line(format!("  gameplay events off the listener's centre line (|geometric pan| > 0.05, > 2 m away): {sign_ok}/{sign_checked} agree in sign"));
    rep.check(sign_checked > 10 && sign_checked == sign_ok, "gameplay impacts / swings pan to the side the camera geometry says");

    // ---- mute when unfocused (the ambience bed is still running here: it is silenced too)
    wet.rig.tee.set_suspended(true);
    wet.rig.render_secs(0.1);
    let muted_t0 = wet.rig.now(); wet.rig.tee.event(SoundEvent::MenuOpen); wet.rig.render_secs(0.5);
    let muted = peak(win(&wet.rig.pcm, muted_t0 + 0.1, muted_t0 + 0.5));
    wet.rig.tee.set_suspended(false);
    wet.rig.render_secs(0.2);
    let open_t0 = wet.rig.now(); wet.rig.tee.event(SoundEvent::MenuOpen); wet.rig.render_secs(0.8);
    let opened = peak(win(&wet.rig.pcm, open_t0, open_t0 + 0.8));
    rep.line(format!("  focus lost (suspend): peak {muted:.5}; focus back: menu-open peak {opened:.4}"));
    rep.check(muted < 1e-5 && opened > 10. * muted.max(1e-6), "focus loss mutes everything (ambience too), focus regain restores it");
    // ---- Settings > Audio sliders drive the live mix (ambience off so the bed does not colour the peaks)
    wet.rig.tee.set_ambience(false);
    wet.rig.render_secs(0.4);
    let click_peak = |rig: &mut Rig| { let t0 = rig.now(); rig.tee.event(SoundEvent::MenuClick); rig.render_secs(0.5); peak(win(&rig.pcm, t0, t0 + 0.5)) };
    let apply = |rig: &mut Rig, set: &[(&str, f32)]| { let mut s = fn_settings::Settings::default(); for (k, v) in set { s.set(k, fn_settings::Value::Float(*v)).unwrap(); } s.apply_audio(&mut SinkHooks(&mut rig.tee)); rig.render_secs(0.1); };
    apply(&mut wet.rig, &[]);
    let full = click_peak(&mut wet.rig);
    apply(&mut wet.rig, &[("audio.sfx_volume", 0.25)]); // default sfx slider is 0.5: the menu click is under the SFX subtree
    let quarter = click_peak(&mut wet.rig);
    apply(&mut wet.rig, &[("audio.sfx_volume", 0.0)]);
    let zero = click_peak(&mut wet.rig);
    apply(&mut wet.rig, &[("audio.sfx_volume", 0.5), ("audio.music_volume", 0.0), ("audio.dialog_volume", 0.0)]); // music / dialog sliders leave the UI sound alone
    let music0 = click_peak(&mut wet.rig);
    apply(&mut wet.rig, &[("audio.master_volume", 0.0)]);
    let master0 = click_peak(&mut wet.rig);
    apply(&mut wet.rig, &[]);
    rep.line(format!("  UI click peak (ambience off): sfx 0.5 (default) {full:.4}, sfx 0.25 {quarter:.4}, sfx 0.0 {zero:.4}, music+dialog 0 {music0:.4}, master 0 {master0:.4}"));
    rep.check(quarter > 0.35 * full && quarter < 0.65 * full, format!("Sound FX slider scales the UI cue (0.25 vs 0.5 -> {:.2}x, expected 0.5x)", quarter / full.max(1e-9)));
    rep.check(zero < 0.2 * full && master0 < 0.2 * full, "Sound FX 0 and Master 0 silence it (only the ambience bed's own residue is left)");
    rep.check((music0 / full - 1.).abs() < 0.25, format!("Music / Dialog sliders do not change the UI cue ({:.2}x)", music0 / full.max(1e-9)));
    wet.rig.tee.event(SoundEvent::MenuClose); wet.rig.render_secs(0.8);
    let log = wet.rig.tee.log.clone();
    let ui: Vec<&Logged> = log.iter().filter(|l| matches!(l.ev, SoundEvent::MenuOpen | SoundEvent::MenuClose | SoundEvent::MenuClick)).collect();
    rep.check(!ui.is_empty(), format!("UI sounds played: {} menu events", ui.len()));

    // ---- the weak-spot sequence (CRACK AUDIO HOOK)
    crack_section(&mut wet.rig, table.clone(), &mut rep);

    // ---- frame cost on the main thread
    let us = &wet.update_us;
    let (p50, p99, mx, mean) = (pct(us, 0.5), pct(us, 0.99), pct(us, 1.0), us.iter().sum::<f32>() / us.len().max(1) as f32);
    rep.line(format!("\n== main-thread cost of GameplayAudio::update (event detection incl. the pose-tree walk for foot plants + sink commands), {} frames ==\n  mean {mean:.1} us, p50 {p50:.1} us, p99 {p99:.1} us, max {mx:.1} us (the mixing runs on the audio thread)", us.len()));
    rep.check(p99 <= 200., format!("audio adds {p99:.1} us p99 per frame to the main thread (budget 200 us)"));

    // ---- files
    let samples = wet.rig.pcm.clone();
    let total = fn_audio::DecodedWave::new(samples.clone(), RATE, 2);
    let ts = total.stats();
    fn_audio::write_wav(out, &total).map_err(|e| format!("write {}: {e}", out.display()))?;
    let dry_path = out.with_file_name(format!("{}-dry.wav", out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "audio-test".into())));
    fn_audio::write_wav(&dry_path, &fn_audio::DecodedWave::new(dry.rig.pcm.clone(), RATE, 2)).map_err(|e| format!("write {}: {e}", dry_path.display()))?;
    rep.line(format!("\nwrote {} ({:.1} s, peak {:.4}, rms {:.4}, nan {}, clip-run {}) and {} (events only)", out.display(), total.duration_seconds(), ts.peak, ts.rms, ts.non_finite, ts.max_clip_run, dry_path.display()));
    rep.check(ts.non_finite == 0 && ts.max_clip_run == 0, "whole file: no NaN, no clipping");
    // the event table: exact vs assumed
    rep.line("\n== event -> cue table (R = recovered from cooked data, A = assumed) ==");
    let mut seen: Vec<&'static str> = vec![];
    for l in &log { for r in rows(&l.ev) { if !seen.contains(&r) { seen.push(r); } } }
    for r in seen { if let Some(e) = fn_audio::events::lookup(r) { rep.line(format!("  {} {:<28} {}", if e.source == fn_audio::events::Source::Recovered { "R" } else { "A" }, e.event, e.cue.rsplit('/').next().unwrap_or(e.cue))); } }
    rep.line(format!("\nfootstep trigger: clip AnimNotifies (exact); swing: TriggerAbility notify (exact timing, which cue-moment is ASSUMED from the README); gait cue per clip family (exact names, jog -> walk cue ASSUMED); landing hard threshold {} m/s (ASSUMED)", fn_audio_sink::HARD_LAND_SPEED));
    rep.line(format!("total {:.1} s; {}", t_all.elapsed().as_secs_f32(), if rep.failures.is_empty() { "RESULT: ok".to_owned() } else { format!("RESULT: FAILED ({})", rep.failures.join("; ")) }));
    let report_path = out.with_extension("report.txt");
    let _ = std::fs::write(&report_path, &rep.text);
    print!("{}", rep.text);
    println!("report: {}", report_path.display());
    Ok(rep.failures.is_empty())
}

