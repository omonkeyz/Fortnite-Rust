//! Audio of the game: the real engine (`fn-audio` decoding the Athena cues straight from the 1.8 paks, mixing on a cpal callback) behind the
//! [`AudioSink`] seam, the game-state event sources, the Settings > Audio apply layer and the offline / device test modes.
//!
//! * [`AudioSink`] / [`SoundEvent`] are `fn-audio-sink`'s (one definition; this module re-exports them).
//! * [`open_for_app`] opens the device at once and lets a worker thread mount the install, decode the gameplay / UI cues and then the long ambience
//!   loops (`fn_audio_sink::DeviceSink::open_streaming`); the game thread only ever pushes lock-free commands. Audio ALWAYS reads the SoundWaves /
//!   SoundCues from the 1.8 paks (`--paks` / `FN_PAKS` / `FN_ROOT` / the install the run uses / the developer's default install), whatever the
//!   graphics / animation source is. No install (an assets-only run) or no output device: the null sink and one log line, never a failure.
//! * [`GameplayAudio`] turns the sim state into sound events (jump, landing, pickaxe fire, impacts, equip, foot plants from the clips'
//!   `AnimNotify_FootStep` notifies) and feeds the listener (the player camera view) every frame; see `fn_game::audio_events`.
//! * Settings > Audio (Master / Music / Sound FX / Dialog / Voice Chat volumes, Quality) reach the sink through [`SinkHooks`]
//!   (`fn_settings::AudioHooks`, driven by `AppBackend::apply_all`, i.e. live from the Settings screen's Apply / Preview). The settings table has no
//!   mute-when-unfocused option; the app mutes on focus loss / minimise unconditionally (`AudioSink::set_suspended`, see `app.rs`).
//! * Modes handled before the window exists ([`run_mode`]): `--audio-test [out.wav]` (scripted gameplay rendered offline, analysed, no device) and
//!   `--audio-device-smoke` (0.3 s of the pickaxe swing once, low volume). `--no-audio` disables sound; scripted / self-test runs stay silent unless
//!   `--audio` is given (a harness must not play through the user's speakers).
pub use fn_audio_sink::{AudioSink, Gait, MusicCue, SoundEvent, Surface};
use fn_game::audio_events::{self as moments, AudioMoment, MomentDetector, StepTable};
use fn_settings::AudioHooks;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

#[path = "audio_test.rs"]
pub mod offline;

pub const USAGE: &str = "audio: [--no-audio] [--audio] [--audio-test [OUT.wav]] [--audio-device-smoke]   (--audio-test renders a scripted run offline into a WAV + report; --audio-device-smoke plays 0.3 s once; --audio enables sound in scripted runs)";

/// The developer machine's install (last resort, like `fn-audio-cli`'s default).
const DEV_INSTALL: &str = r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame";

// ---------------------------------------------------------------------------------------------------------------------------------
// sinks
// ---------------------------------------------------------------------------------------------------------------------------------
/// Drops everything.
#[derive(Default)]
pub struct NullAudio;
impl AudioSink for NullAudio {}

/// Records events (tests, `--script` runs that assert on gameplay moments).
#[derive(Default)]
pub struct RecordingAudio { pub events: Vec<SoundEvent>, pub volumes: Option<[f32; 5]>, pub suspended: bool, pub ambience: Option<bool>, pub quality_high: Option<bool>, pub listener: Option<(glam::Vec3, glam::Vec3, glam::Vec3)>, /* MUSIC HOOK: */ pub music: Vec<Option<MusicCue>>, pub loading_screen: Vec<bool> }
impl AudioSink for RecordingAudio {
    fn set_music(&mut self, t: Option<MusicCue>) { self.music.push(t); } // MUSIC HOOK
    fn set_loading_screen(&mut self, on: bool) { self.loading_screen.push(on); } // MUSIC HOOK
    fn event(&mut self, e: SoundEvent) { self.events.push(e); }
    fn listener(&mut self, p: glam::Vec3, f: glam::Vec3, u: glam::Vec3) { self.listener = Some((p, f, u)); }
    fn set_volumes(&mut self, m: f32, mu: f32, s: f32, d: f32, v: f32) { self.volumes = Some([m, mu, s, d, v]); }
    fn set_suspended(&mut self, s: bool) { self.suspended = s; }
    fn set_ambience(&mut self, on: bool) { self.ambience = Some(on); }
    fn set_quality(&mut self, high: bool) { self.quality_high = Some(high); }
}

/// `fn_settings::AudioHooks` -> a sink (what `Settings::apply_audio` drives): the five volume sliders and the Quality rotator.
/// Voice chat / push-to-talk / subtitles have no engine behind them (the settings table marks them STORED ONLY).
pub struct SinkHooks<'a>(pub &'a mut dyn AudioSink);
impl AudioHooks for SinkHooks<'_> {
    fn set_volumes(&mut self, master: f32, music: f32, sfx: f32, dialog: f32, voice_chat: f32) { self.0.set_volumes(master, music, sfx, dialog, voice_chat); }
    fn set_quality(&mut self, high: bool) { self.0.set_quality(high); }
}

/// MUSIC HOOK: what the sink does when the flow enters `stage` (called by `App::entered` / `begin_play`; idempotent). The data behind it is `fn_audio::music`:
/// * `StartupLoading`, `Loading` (and `Finishing`, which keeps the loading screen up): the loading screen is silent: `DefaultGameData.LoadingScreenSoundMix`
///   (`Fort_Loading_Mix`: SFX / Voice / Music to 0, 0.5 s in) is on; it holds no music of its own.
/// * `LoggingIn` (FRONT END: the cooked `LoginScreen_C`): the loading mix is popped (1.0 s fade out) and the login track starts (`LoginScreen` enters its state
///   -> `FrontEnd_Music_Controller.Music_Login_FadeIn`, `ExecuteUbergraph_LoginScreen` 0x18d).
/// * `GamePicker` (`SubgameSelectScreen_C`): nothing changes: neither `SubgameSelect` nor `SubgameSelectScreen` touches the music, so the login track keeps
///   playing (the Battle Royale lobby track belongs to `AthenaLobby`, which this port does not show).
/// * `Loading`: the level travel fades the front-end music out (`Fort_Entry_Music_Controller_BP` `SubGameChanged` handler, `FadeOutTime` 1.0 s).
/// * `Playing`: the loading mix is popped (1.0 s). There is no music in the match itself: the BR data binds none (the bus / pre-game island sounds are SFX-class
///   and the port has no pre-game island or bus).
/// * `Splash`: the engine's native splash (no sound in the data).
pub fn stage_audio(sink: &mut dyn AudioSink, stage: crate::app::startup::Stage) {
    use crate::app::startup::Stage;
    match stage {
        Stage::StartupLoading => sink.set_loading_screen(true),
        Stage::LoggingIn => { sink.set_loading_screen(false); sink.set_music(Some(MusicCue::Login)); }
        Stage::GamePicker => {}
        Stage::Lobby => { sink.set_loading_screen(false); sink.set_music(Some(MusicCue::LobbyAthena)); }
        Stage::Loading => { sink.set_music(None); sink.set_loading_screen(true); }
        Stage::Playing => sink.set_loading_screen(false),
        Stage::Splash | Stage::Finishing => {}
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// command line
// ---------------------------------------------------------------------------------------------------------------------------------
static NO_AUDIO: AtomicBool = AtomicBool::new(false);
static FORCE_AUDIO: AtomicBool = AtomicBool::new(false);
/// 0 = none, 1 = `--audio-test`, 2 = `--audio-device-smoke`.
static MODE: AtomicU8 = AtomicU8::new(0);
static TEST_OUT: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Removes the audio options from `args` (the main parser knows nothing about them) and remembers them.
pub fn strip_cli(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-audio" => NO_AUDIO.store(true, Ordering::Relaxed),
            "--audio" => FORCE_AUDIO.store(true, Ordering::Relaxed),
            "--audio-device-smoke" => MODE.store(2, Ordering::Relaxed),
            "--audio-test" => {
                MODE.store(1, Ordering::Relaxed);
                let v = args.get(i + 1).filter(|a| !a.starts_with("--")).cloned();
                if v.is_some() { i += 1; }
                let _ = TEST_OUT.set(v.map(PathBuf::from));
            }
            _ => out.push(args[i].clone()),
        }
        i += 1;
    }
    out
}
fn value_of(args: &[String], name: &str) -> Option<PathBuf> { args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(PathBuf::from) }

/// The pre-window modes (`--audio-test`, `--audio-device-smoke`); `None` = run the game.
pub fn run_mode(args: &[String]) -> Option<ExitCode> {
    let mode = MODE.load(Ordering::Relaxed);
    if mode == 0 { return None; }
    let explicit = value_of(args, "--paks");
    let Some(root) = find_install(explicit.as_deref()) else {
        eprintln!("error: no Fortnite 1.8 install found for the audio (give --paks <Paks or install folder>, or set FN_PAKS / FN_ROOT)");
        return Some(ExitCode::from(1));
    };
    Some(match mode {
        1 => {
            let out = TEST_OUT.get().cloned().flatten().unwrap_or_else(|| crate::assets::exe_dir().join("verification").join("audio-test.wav"));
            match offline::run(&root, &out) { Ok(true) => ExitCode::SUCCESS, Ok(false) => ExitCode::from(1), Err(e) => { eprintln!("error: audio test: {e}"); ExitCode::from(1) } }
        }
        _ => device_smoke(&root),
    })
}

/// Plays 0.3 s of the pickaxe swing once through the default device at a low volume (master 0.15 on top of the cue's class chain).
fn device_smoke(root: &Path) -> ExitCode {
    use fn_audio::mixer::PlayParams;
    let mut lib = match fn_audio::library::Library::open(root) { Ok(l) => l, Err(e) => { eprintln!("error: {e}"); return ExitCode::from(1); } };
    let (handle, stream) = match fn_audio::device::open_default() { Ok(x) => x, Err(e) => { eprintln!("error: no usable output device: {e}"); return ExitCode::from(1); } };
    println!("output device: {} ({} Hz, {} ch)", stream.device_name, stream.sample_rate, stream.channels);
    if let Err(e) = lib.prepare_cue(&handle, fn_audio::events::SWING) { eprintln!("error: {e}"); return ExitCode::from(1); }
    handle.set_bus_volume(fn_audio::soundclass::Bus::Master, 0.15);
    let _ = handle.play_cue(fn_audio::events::SWING, PlayParams::default());
    std::thread::sleep(std::time::Duration::from_millis(300));
    handle.stop_all();
    std::thread::sleep(std::time::Duration::from_millis(40));
    println!("played 0.3 s of {} through the default device", fn_audio::events::SWING);
    drop(stream);
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the install and the footstep table
// ---------------------------------------------------------------------------------------------------------------------------------
/// Where the Fortnite 1.8 paks are for the audio: the run's own paks dir / `--paks`, `FN_PAKS`, `FN_ROOT`, `Paks` beside the exe, then the developer's install.
pub fn find_install(explicit: Option<&Path>) -> Option<PathBuf> {
    let mut c = crate::assets::candidate_paks(explicit);
    if let Some(e) = explicit { c.insert(0, e.to_path_buf()); }
    c.push(PathBuf::from(DEV_INSTALL));
    c.iter().find_map(|p| crate::assets::find_paks_dir(p))
}

/// The footstep notifies of every clip the animation graph can play, read from the paks (a one-off, ~1 s; runs on a worker thread in the game).
pub fn load_step_table(root: &Path) -> Result<StepTable, String> {
    let set = fn_pak::PakSet::open_install(root).map_err(|e| e.to_string())?;
    let mut names = fn_animgraph::required_clips();
    names.sort();
    names.dedup();
    let mut clips = vec![];
    for name in names {
        let hit = set.glob(&format!("fortnitegame/content/**/{}.uasset", name.to_lowercase()));
        let Some((_, e)) = hit.first() else { continue };
        let base = e.path.trim_end_matches(".uasset").to_owned();
        let rd = |ext: &str| set.read(&format!("{base}.{ext}")).map(|c| c.into_owned()).unwrap_or_default();
        let Ok(pkg) = fn_ueio::Package::parse(&rd("uasset"), &rd("uexp"), &rd("ubulk")) else { continue };
        let Ok(seq) = fn_anim::AnimSequence::parse(&pkg) else { continue };
        let steps: Vec<(f32, bool)> = seq.notifies.iter().filter_map(|n| {
            let id = format!("{} {}", n.notify_name, n.notify_class.as_deref().unwrap_or("")).to_ascii_lowercase();
            id.contains("footstep").then(|| (n.trigger_time(), id.contains("left")))
        }).collect();
        clips.push((name, seq.length, steps));
    }
    Ok(StepTable::from_clips(clips))
}

/// The table of the running game (filled by the worker thread of [`open_for_app`]).
static STEP_TABLE: OnceLock<StepTable> = OnceLock::new();

// ---------------------------------------------------------------------------------------------------------------------------------
// opening the sink for the game
// ---------------------------------------------------------------------------------------------------------------------------------
/// The sink the game starts with (`App::new`): the real engine, or [`NullAudio`] (with one log line saying why) for `--no-audio`, scripted runs
/// without `--audio`, a missing install or a missing output device. Returns at once: decoding happens on a worker thread.
pub fn open_for_app(opts: &crate::cli::Options, source: &crate::loader::Source) -> Box<dyn AudioSink> {
    if NO_AUDIO.load(Ordering::Relaxed) { eprintln!("[audio] off (--no-audio)"); return Box::new(NullAudio); }
    let automated = opts.scripted() || opts.live_test || opts.startup_capture;
    if automated && !FORCE_AUDIO.load(Ordering::Relaxed) { eprintln!("[audio] off for this automated run (add --audio to play it)"); return Box::new(NullAudio); }
    let explicit = match source { crate::loader::Source::Paks(p) => Some(p.clone()), crate::loader::Source::Dir => opts.paks.clone() };
    let Some(root) = find_install(explicit.as_deref()) else { eprintln!("[audio] off: no Fortnite 1.8 install (paks) to read the sounds from"); return Box::new(NullAudio); };
    let table_root = root.clone();
    match fn_audio_sink::DeviceSink::open_streaming(root.clone()) {
        Ok((sink, _log)) => {
            eprintln!("[audio] output {} ({} Hz, {} ch); sounds from {}", sink.device_name, sink.sample_rate, sink.channels, root.display());
            let _ = std::thread::Builder::new().name("fn-audio-steps".into()).spawn(move || match load_step_table(&table_root) {
                Ok(t) => { eprintln!("[audio] footstep notifies: {} clips in {} families", t.clips.len(), t.families().len()); let _ = STEP_TABLE.set(t); }
                Err(e) => eprintln!("[audio] footstep notifies unavailable: {e}"),
            });
            Box::new(sink)
        }
        Err(e) => { eprintln!("[audio] off: {e}"); Box::new(NullAudio) }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// game state -> sound events
// ---------------------------------------------------------------------------------------------------------------------------------
fn to_sound(m: &AudioMoment) -> SoundEvent {
    match *m {
        AudioMoment::Step { gait, .. } => SoundEvent::Step { gait: match gait { moments::Gait::Walk => Gait::Walk, moments::Gait::Sprint => Gait::Sprint, moments::Gait::Crouch => Gait::Crouch, moments::Gait::CrouchSprint => Gait::CrouchSprint } },
        AudioMoment::Jump => SoundEvent::Jump,
        AudioMoment::Land { speed } => SoundEvent::Land { speed },
        AudioMoment::Swing { pos } => SoundEvent::SwingAt { pos },
        // CRACK AUDIO HOOK: a prop whose `props.json` names its sounds plays those (PropHit); anything else keeps the surface cues
        AudioMoment::Harvest { surface, destroyed, pos, proto } => match prop_sound_of(proto) {
            Some(sound) if crate::demolition_hook::cascade_killed(proto) => SoundEvent::PropDeath { sound, pos },
            Some(sound) => SoundEvent::PropHit { sound, destroyed, pos },
            None => SoundEvent::Harvest { surface: match surface { moments::HitSurface::Tree => Surface::Tree, moments::HitSurface::Rock => Surface::Rock }, destroyed, pos },
        },
        // BUILDING: a prop a placement destroyed: its DeathSound only (placeholders: the surface's destroyed cue)
        AudioMoment::PropDestroyed { surface, pos, proto } => match prop_sound_of(proto) {
            Some(sound) => SoundEvent::PropDeath { sound, pos },
            None => SoundEvent::Harvest { surface: match surface { moments::HitSurface::Tree => Surface::Tree, moments::HitSurface::Rock => Surface::Rock }, destroyed: true, pos },
        },
        AudioMoment::Equip => SoundEvent::Equip,
        // CRACK AUDIO HOOK: weak-spot effect (positional at the spot)
        AudioMoment::CrackSpawn { pos } => SoundEvent::CrackSpawn { pos },
        AudioMoment::CrackLevel { level, pos } => SoundEvent::CrackLevel { level, pos },
        AudioMoment::CrackImpact { pos } => SoundEvent::CrackImpact { pos },
    }
}

/// CRACK AUDIO HOOK: catalog prop ids indexed like `Harvestable::proto` (`props::Library::protos`), set once when the real props are loaded; without them (placeholder props) the
/// surface cues play.
static PROP_IDS: OnceLock<Vec<String>> = OnceLock::new();
pub fn set_prop_ids(ids: Vec<String>) { let _ = PROP_IDS.set(ids); }
/// `props.json` hit / death sounds of the prop with this `Harvestable::proto`.
pub fn prop_sound_of(proto: u32) -> Option<&'static fn_audio::events::PropSound> {
    if fn_combat::is_poi_proto(proto) { return crate::demolition_hook::poi_sound(proto); }
    let id = PROP_IDS.get()?.get(proto as usize)?;
    match fn_audio::demolition::class(id) {
        Some(r) => Some(fn_audio::demolition::prop_sound(id, r.surface, r.resource)),
        None => fn_audio::events::prop_sound(id),
    }
}

/// WEAPON AUDIO HOOK: a sound moment of the arms (`crate::arms::Arms`) as a sink event. Firearm sounds (fire, empty, reload stages) are emitted at `weapon` (the weapon's
/// attach point, where its AudioComponents sit; world metres), `AnimNotify_PlaySound` sounds (equip, healing) at the pawn mesh (`pawn`: `PlaySoundAtLocation(MeshComp)`).
/// `None` for a notify whose cue is not in `fn_audio::weapon_events::TABLE` (e.g. the pickaxe equip, owned by the melee events).
pub fn arms_to_sound(s: &crate::arms::ArmsSound, weapon: glam::Vec3, pawn: glam::Vec3) -> Option<SoundEvent> { arms_to_sound_bp(s, &[], weapon, pawn) }

/// FIRE LOOP / LAYERS: every sink event of one arms sound moment (`low_ammo` = `Arms::low_ammo()`: magazine <= clip x `LowAmmoPercentage` 0.25, the exe's FireAudioChannelWantsToPlay[1] rule,
/// `AFortWeapon` vfunc_285 RVA 0x9454f0). Exe rules (`PlayWeaponFireFX` vfunc_201 RVA 0x9355d0, `StopWeaponFireFX` vfunc_202 RVA 0x93e020):
/// * every channel with WantsToPlay plays its `PrimaryFireSound[i]`: the normal channel always, the LowAmmo channel as an extra LAYER while `low_ammo`;
/// * a weapon with `bPersistentFireFX` (the Blueprint data: Gatling, burst rifle, hydraulic drum) starts ONE sound per hold / burst on `FireStart` (per-shot `Fire` is silent) and `FireStop`
///   fades the channels out over `PrimaryFireSoundFadeOutTime` and plays each non-null `PrimaryFireStopSound[i]` at the weapon;
/// * every other weapon (including Automatic ones such as the Striker AR, whose cue is a single shot) plays per shot, and `FireStart` / `FireStop` make no sound.
/// Weapons whose Blueprint is not in the generated table use the starter table by index (per shot + low-ammo layer, no loop).
pub fn arms_to_sounds(s: &crate::arms::ArmsSound, bps: &[Option<String>], low_ammo: bool, weapon: glam::Vec3, pawn: glam::Vec3) -> Vec<SoundEvent> {
    use crate::arms::ArmsSound as A;
    use fn_audio::weapon_events as we;
    let w = |id: &'static str| SoundEvent::Weapon { id, pos: Some(weapon), volume: 1. };
    let bp = |gun: usize| bps.get(gun).and_then(|b| b.as_deref()).filter(|b| we::bp_known(b));
    match s {
        A::Fire { gun } => match bp(*gun) {
            Some(b) if we::bp_persistent_fire(b) => vec![],
            Some(b) => { let mut v: Vec<_> = we::bp_event(b, "fire").map(w).into_iter().collect(); if low_ammo { v.extend(we::bp_event(b, "fire.lowammo").map(w)); } v }
            None => match we::Gun::from_item(*gun) {
                Some(g) => { let mut v = vec![w(we::fire_event(g))]; if low_ammo { v.extend(we::lookup(&format!("weapon.{}.fire.lowammo", g.key())).map(|r| w(r.event))); } v }
                None => vec![],
            },
        },
        A::FireStart { gun } => match bp(*gun) {
            Some(b) if we::bp_persistent_fire(b) => {
                let k = *gun as u32 * 3;
                let mut v: Vec<_> = we::bp_event(b, "fire").map(|id| SoundEvent::WeaponLoop { id, key: k, pos: Some(weapon), volume: 1. }).into_iter().collect();
                if low_ammo { v.extend(we::bp_event(b, "fire.lowammo").map(|id| SoundEvent::WeaponLoop { id, key: k + 1, pos: Some(weapon), volume: 1. })); }
                v
            }
            _ => vec![],
        },
        A::FireStop { gun } => match bp(*gun) {
            Some(b) if we::bp_persistent_fire(b) => {
                let fade = we::bp_fire_fx(b).map_or(0.25, |f| f.1);
                let mut v: Vec<_> = (0..3).map(|c| SoundEvent::WeaponLoopStop { key: *gun as u32 * 3 + c, fade }).collect();
                for kind in ["fire.stop", "fire.stop.lowammo", "fire.stop.degraded"] { v.extend(we::bp_event(b, kind).map(w)); }
                v
            }
            _ => vec![],
        },
        other => arms_to_sound_bp(other, bps, weapon, pawn).into_iter().collect(),
    }
}

/// As [`arms_to_sound`], with the weapon Blueprint (`WeaponActorClass`, any form `bp_name` accepts) of each firearm index in `bps`: a firearm whose Blueprint is one of the generated BR weapons
/// (`weapon_events::BPS`) plays that Blueprint's own `PrimaryFireSound[0]` / `OutOfAmmoSound` / `ReloadSounds[stage]` (no row = the CDO chain names no sound = silence); any other firearm
/// falls back to the starter-loadout table by index. Montage-notify cues (equip, rack, reload foley) resolve through the generated `notify.*` rows.
pub fn arms_to_sound_bp(s: &crate::arms::ArmsSound, bps: &[Option<String>], weapon: glam::Vec3, pawn: glam::Vec3) -> Option<SoundEvent> {
    use crate::arms::ArmsSound as A;
    use fn_audio::weapon_events as we;
    let bp = |gun: usize| bps.get(gun).and_then(|b| b.as_deref()).filter(|b| we::bp_known(b));
    if let A::Fire { gun } | A::Empty { gun } | A::ReloadStage { gun, .. } = s {
        if let Some(b) = bp(*gun) {
            let id = match s { A::Fire { .. } => we::bp_event(b, "fire"), A::Empty { .. } => we::bp_event(b, "empty"), A::ReloadStage { stage, .. } => we::bp_reload_event(b, *stage), _ => None }?;
            return Some(SoundEvent::Weapon { id, pos: Some(weapon), volume: 1. });
        }
    }
    Some(match s {
        A::Fire { gun } => SoundEvent::Weapon { id: we::fire_event(we::Gun::from_item(*gun)?), pos: Some(weapon), volume: 1. },
        A::Empty { gun } => SoundEvent::Weapon { id: we::empty_event(we::Gun::from_item(*gun)?), pos: Some(weapon), volume: 1. },
        A::ReloadStage { gun, stage } => SoundEvent::Weapon { id: we::reload_event(we::Gun::from_item(*gun)?, *stage), pos: Some(weapon), volume: 1. },
        A::Notify { cue, volume, .. } => SoundEvent::Weapon { id: we::by_cue_path(cue)?.event, pos: Some(pawn), volume: *volume },
        // FireStart / FireStop make their sounds in `arms_to_sounds` (loop start / stop events)
        A::FireStart { .. } | A::FireStop { .. } => return None,
    })
}

/// Turns the per-frame game state into sound events and the listener (called once per simulated frame by the app).
#[derive(Default)]
pub struct GameplayAudio { det: MomentDetector, table: Option<Arc<StepTable>>, buf: Vec<AudioMoment>, ambience_started: bool, ambient_mapped: bool /* AMBIENCE HOOK */ }

/// AMBIENCE HOOK: where the real layout is centred (UE world cm) and where that centre sits in game metres, set once when the real props are loaded; the level's ambient
/// emitters (`fn_audio::ambient`) are placed with the same mapping as the props. Without it the default mapping applies (UE `(x, y, z)` -> game `(x, z, y)`).
static AMBIENT_WORLD: OnceLock<([f32; 3], glam::Vec3)> = OnceLock::new();
pub fn set_ambient_world(center_cm: [f32; 3], anchor_m: glam::Vec3) { let _ = AMBIENT_WORLD.set((center_cm, anchor_m)); }

/// AMBIENCE HOOK: `LineTraceSingle` of the ambient controller's "surrounded" test: world geometry the pawn can hit (the live props' hulls / solids and the baseplate); pawns are ignored.
fn ambient_trace(g: &fn_game::GameSim, from: glam::Vec3, dir: glam::Vec3, len: f32) -> bool {
    let to = from + dir * len;
    fn_game::props::trace_in(&g.world.props, g.fresh_prop_index(), from, to, 0.).is_some() || fn_game::camera::trace_ground(from, to, 0.).is_some()
}
impl GameplayAudio {
    /// A tracker with a fixed footstep table (tests, the offline run); the game uses the table its worker thread loads.
    pub fn with_table(t: Arc<StepTable>) -> Self { GameplayAudio { table: Some(t), ..Default::default() } }
    pub fn update(&mut self, sink: &mut dyn AudioSink, g: &fn_game::GameSim, dt: f32) {
        if !self.ambience_started { self.ambience_started = true; sink.set_ambience(true); } // the island bed starts when the world is up (as soon as its loops are decoded)
        self.buf.clear();
        self.det.observe(g, self.table.as_deref().or_else(|| STEP_TABLE.get()), dt, &mut self.buf);
        let enhanced_steps = crate::enhanced_sound::replaces_steps(g);
        for m in &self.buf {
            if enhanced_steps && matches!(m, AudioMoment::Step { .. }) { continue; }
            let e = to_sound(m);
            if let SoundEvent::PropHit { sound, .. } | SoundEvent::PropDeath { sound, .. } = &e { if fn_live::var("FN_HARVEST_AUDIO_LOG").is_ok_and(|v| v == "1") { eprintln!("[harvest audio] {} {}: {:?}", if matches!(e, SoundEvent::PropDeath { .. }) { "death" } else { "hit" }, sound.id, fn_audio_sink::crack_event_cues(&e)); } }
            sink.event(e);
        }
        // the listener is the player camera view (UE: the PlayerCameraManager view point)
        sink.listener(g.cam_view.eye, g.cam_view.forward, g.cam_view.up);
        // AMBIENCE HOOK: the level's ambient system (bed phase loop + cross-fades, lakes, rivers, placed emitters) follows the pawn; the "surrounded" test traces world geometry
        if !self.ambient_mapped { if let Some((c, a)) = AMBIENT_WORLD.get() { self.ambient_mapped = true; sink.ambient_map(*c, *a); } }
        let pawn = g.feet() + glam::Vec3::Y * 0.75; // actor location = capsule centre (recovered half height 75 cm)
        let fwd = glam::Vec3::new(g.cam_view.forward.x, 0., g.cam_view.forward.z).try_normalize().unwrap_or(glam::Vec3::NEG_Z);
        sink.ambient_frame(dt, pawn, fwd, &mut |from, dir, len| ambient_trace(g, from, dir, len));
    }
    /// Moments of the last `update` (diagnostics).
    pub fn last_moments(&self) -> &[AudioMoment] { &self.buf }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fn_game::{FrameInput, GameSim};
    fn a(s: &str) -> Vec<String> { s.split_whitespace().map(String::from).collect() }

    #[test] fn null_audio_accepts_everything() { let mut n = NullAudio; n.event(SoundEvent::Jump); n.set_volumes(1., 1., 1., 1., 1.); n.set_suspended(true); n.set_ambience(true); n.set_quality(false); }
    #[test] fn audio_options_are_stripped_from_the_command_line_and_remembered() {
        let r = strip_cli(a("--smoke-test --no-audio --paks D --audio --audio-test out.wav --audio-device-smoke --frames 3"));
        assert_eq!(r, a("--smoke-test --paks D --frames 3"));
        assert!(NO_AUDIO.load(Ordering::Relaxed) && FORCE_AUDIO.load(Ordering::Relaxed));
        assert_eq!(TEST_OUT.get().cloned().flatten(), Some(PathBuf::from("out.wav")));
        assert_eq!(value_of(&a("--x --paks D:\\p"), "--paks"), Some(PathBuf::from("D:\\p")));
        NO_AUDIO.store(false, Ordering::Relaxed); FORCE_AUDIO.store(false, Ordering::Relaxed); MODE.store(0, Ordering::Relaxed);
    }
    #[test] fn no_install_means_no_sound_not_a_crash() { let p = std::env::temp_dir().join("fn-app-no-paks-here"); let _ = std::fs::create_dir_all(&p); assert!(crate::assets::find_paks_dir(&p).is_none()); }
    fn table() -> Arc<StepTable> {
        let mut v = vec![];
        for d in ["N", "NE", "E", "SE", "S", "SW", "W", "NW"] { v.push((format!("M_Jog_AssaultRifle_{d}_Core"), 0.6, vec![(0.2674, false), (0.5674, true)])); }
        v.push(("Sprint_TwoHanded".to_owned(), 0.4667, vec![(0.1973, true), (0.4305, false)]));
        Arc::new(StepTable::from_clips(v))
    }
    #[test] fn jump_land_swing_and_footsteps_become_sound_events_and_the_listener_follows_the_camera() {
        let mut g = GameSim::new(); let mut a = GameplayAudio::with_table(table()); let mut sink = RecordingAudio::default();
        for f in 0..240 { g.step(&FrameInput { forward: if f > 20 { 1. } else { 0. }, jump: f == 100, attack: (150..160).contains(&f), ..Default::default() }, 1. / 60.); a.update(&mut sink, &g, 1. / 60.); }
        let n = |p: fn(&SoundEvent) -> bool| sink.events.iter().filter(|e| p(e)).count();
        assert_eq!(n(|e| matches!(e, SoundEvent::Jump)), 1, "{:?}", sink.events);
        assert_eq!(n(|e| matches!(e, SoundEvent::Land { .. })), 1);
        assert!(n(|e| matches!(e, SoundEvent::Step { gait: Gait::Walk })) >= 3, "{:?}", sink.events);
        assert_eq!(n(|e| matches!(e, SoundEvent::SwingAt { .. })), 1);
        assert_eq!(n(|e| matches!(e, SoundEvent::Equip)), 0, "no equip was requested");
        assert_eq!(sink.ambience, Some(true), "the bed starts with the world");
        let (p, f, _) = sink.listener.unwrap(); assert_eq!((p, f), (g.cam_view.eye, g.cam_view.forward));
    }
    /// The sound notifies of the starter loadout's real montages (read from the 1.8 paks): cues, pitch multipliers and `PlayReloadFX` stages at the recovered times.
    #[test] fn the_weapon_montages_of_the_1_8_install_carry_the_recovered_sound_notifies() {
        use crate::arms::{montage_fx, Fx};
        use fn_audio::weapon_events::{self as we, ReloadStage as Rs};
        let Some(root) = find_install(None) else { eprintln!("SKIP: no install"); return };
        let set = fn_pak::PakSet::open_install(&root).unwrap();
        let fx = |name: &str| {
            let hit = set.glob(&format!("fortnitegame/content/**/{}.uasset", name.to_lowercase()));
            let (_, e) = hit.first().unwrap_or_else(|| panic!("{name} not in the install"));
            let base = e.path.trim_end_matches(".uasset").to_owned();
            let rd = |ext: &str| set.read(&format!("{base}.{ext}")).map(|c| c.into_owned()).unwrap_or_default();
            let pkg = fn_ueio::Package::parse(&rd("uasset"), &rd("uexp"), &rd("ubulk")).unwrap();
            montage_fx(&fn_anim::montage::AnimMontage::parse(&pkg).unwrap())
        };
        let near = |a: f32, b: f32| (a - b).abs() < 2e-3;
        let stages = |v: &[crate::arms::FxNotify]| v.iter().filter_map(|n| if let Fx::Reload(s) = &n.fx { Some((*s, n.time)) } else { None }).collect::<Vec<_>>();
        let equip = fx("Shotgun_Default_Equip_CMM");
        assert!(equip.iter().any(|n| matches!(&n.fx, Fx::Sound { cue, pitch, .. } if cue.ends_with("Player_Equip_Projectile_Equip_Cue.Player_Equip_Projectile_Equip_Cue") && *pitch == 1.) && near(n.time, 0.1803)), "{equip:?}");
        assert!(stages(&equip).iter().any(|(s, t)| *s == Rs::End && near(*t, 0.2410)), "{equip:?}");
        for (name, want) in [("AssaultRifle_FCBA_Reload_CMM", [0.4447, 1.3753, 1.9062]), ("Shotgun_RG_PABF_Reload_M", [0.0771, 0.7406, 1.9331]), ("SniperRifle_FCSA_Re3Sec_CMM_M", [0.4861, 1.6016, 2.1187])] {
            let s = stages(&fx(name));
            assert_eq!(s.iter().map(|x| x.0).collect::<Vec<_>>(), vec![Rs::Start, Rs::Cartridge, Rs::End], "{name}");
            for (got, w) in s.iter().zip(want) { assert!(near(got.1, w), "{name}: {got:?} vs {w}"); }
        }
        for (name, last_cue, last_t) in [("WrapBandages_Athena", "EatHealth_Restore_Athena_Cue", 3.5086), ("ApplyMedkit_Athena", "EatHealth_Restore_Athena_Cue", 10.0667), ("ApplyShield_Athena", "Sheild_Restore_Athena_Cue", 5.1223), ("ApplyPurple_Athena", "Sheild_Restore_Athena_Cue", 1.9968)] {
            let v = fx(name);
            let sounds: Vec<_> = v.iter().filter_map(|n| if let Fx::Sound { cue, pitch, .. } = &n.fx { Some((cue.clone(), *pitch, n.time)) } else { None }).collect();
            assert_eq!(sounds.len(), 2, "{name}: {sounds:?}");
            assert!(sounds[0].0.ends_with("Bandage_Apply_Athena_Cue.Bandage_Apply_Athena_Cue") && sounds[0].1 == 1., "{name}: {sounds:?}");
            assert!(sounds[1].0.ends_with(&format!("{last_cue}.{last_cue}")) && sounds[1].1 == 1.5 && near(sounds[1].2, last_t), "{name}: {sounds:?}");
            for (c, ..) in &sounds { assert!(we::by_cue_path(c).is_some(), "{c} has no weapon_events row"); }
            // HEAL FX: the green `P_Consume_Item` burst on `b_MF_Head`, at the TriggerAbility time; the shield potion's montage has none
            let parts: Vec<_> = v.iter().filter_map(|n| if let Fx::Particle(p) = &n.fx { Some((p.clone(), n.time)) } else { None }).collect();
            if name == "ApplyShield_Athena" { assert!(parts.is_empty(), "{name}: {parts:?}"); continue; }
            assert_eq!(parts.len(), 1, "{name}: {parts:?}");
            let (p, t) = &parts[0];
            assert_eq!((p.template.as_str(), p.socket.as_str(), p.attached, p.location_offset, p.rotation_offset), ("/Game/Effects/Fort_Effects/Gameplay/P_Consume_Item.P_Consume_Item", "b_MF_Head", true, [0.; 3], [0.; 3]), "{name}");
            assert!(near(*t, last_t), "{name}: particle at {t}, TriggerAbility / restore sound at {last_t}");
        }
    }
    /// PUMP FIX (install-checked): the Pump Shotgun's fire montage carries `PlayReloadFX(ReloadEnd)` (the rack = `ReloadSounds[2]` = Weapon_Shotgun_Cock2_Cue). Also prints
    /// (`--nocapture`) every weapon montage of the install that has sound notifies, to audit which ones the arms play.
    #[test] fn the_pump_shotguns_fire_montage_carries_the_rack_notify_and_every_weapon_montage_with_sounds_is_listed() {
        use crate::arms::{montage_fx, Fx};
        use fn_audio::weapon_events::ReloadStage as Rs;
        let Some(root) = find_install(None) else { eprintln!("SKIP: no install"); return };
        let set = fn_pak::PakSet::open_install(&root).unwrap();
        let mut pump = None;
        for (_, e) in set.glob("fortnitegame/content/animation/**/*.uasset") {
            let base = e.path.trim_end_matches(".uasset").to_owned();
            let rd = |ext: &str| set.read(&format!("{base}.{ext}")).map(|c| c.into_owned()).unwrap_or_default();
            let Ok(pkg) = fn_ueio::Package::parse(&rd("uasset"), &rd("uexp"), &rd("ubulk")) else { continue };
            let Ok(m) = fn_anim::montage::AnimMontage::parse(&pkg) else { continue };
            let fx = montage_fx(&m);
            if fx.is_empty() { continue; }
            let name = base.rsplit('/').next().unwrap().to_owned();
            if name.eq_ignore_ascii_case("Shotgun_Rifle_PABF_FireNPump02_M_Athena") { pump = Some(fx.clone()); }
            let l: Vec<String> = fx.iter().map(|n| match &n.fx { Fx::Sound { cue, .. } => format!("{:.4}s {}", n.time, cue.rsplit('/').next().unwrap_or(cue)), Fx::Reload(s) => format!("{:.4}s ReloadFX({s:?})", n.time), Fx::Particle(p) => format!("{:.4}s Particle({} @ {})", n.time, p.template, p.socket) }).collect();
            if base.to_lowercase().contains("/combat/") || base.to_lowercase().contains("/montages/") { eprintln!("MONTAGE-SOUND {name}: {}", l.join(", ")); }
        }
        let pump = pump.expect("pump fire montage not found");
        assert!(pump.iter().any(|n| matches!(n.fx, Fx::Reload(Rs::End)) && (n.time - 0.6818).abs() < 2e-3), "{pump:?}");
    }
    /// FIRE LOOP / LAYERS (exe vfunc_201 0x9355d0, vfunc_202 0x93e020, vfunc_285 0x9454f0): per-shot weapons layer the low-ammo sound, persistent-fire weapons start / stop one sound.
    #[test] fn fire_sounds_follow_the_persistent_fx_rule_and_layer_low_ammo() {
        use crate::arms::ArmsSound as A;
        let (w, p) = (glam::Vec3::new(1., 2., 3.), glam::Vec3::ZERO);
        let ids = |v: Vec<SoundEvent>| v.into_iter().map(|e| match e { SoundEvent::Weapon { id, .. } => id.to_owned(), SoundEvent::WeaponLoop { id, key, .. } => format!("loop {id} #{key}"), SoundEvent::WeaponLoopStop { key, fade } => format!("stop #{key} {fade}"), o => format!("{o:?}") }).collect::<Vec<_>>();
        let bps = |names: &[&str]| names.iter().map(|n| Some(format!("/Game/Weapons/X/{n}.{n}_C"))).collect::<Vec<_>>();
        // the Striker AR is Automatic but NOT persistent: per-shot sound, FireStart / FireStop silent, low ammo layers the LowAmmo channel
        let ar = bps(&["B_Assault_Auto_Athena"]);
        assert_eq!(ids(arms_to_sounds(&A::Fire { gun: 0 }, &ar, false, w, p)), vec!["bp.B_Assault_Auto_Athena.fire"]);
        assert_eq!(ids(arms_to_sounds(&A::Fire { gun: 0 }, &ar, true, w, p)), vec!["bp.B_Assault_Auto_Athena.fire", "bp.B_Assault_Auto_Athena.fire.lowammo"]);
        assert!(arms_to_sounds(&A::FireStart { gun: 0 }, &ar, false, w, p).is_empty() && arms_to_sounds(&A::FireStop { gun: 0 }, &ar, false, w, p).is_empty());
        // no Blueprint known (starter fallback by index): per shot + low-ammo layer from the starter table
        assert_eq!(ids(arms_to_sounds(&A::Fire { gun: 1 }, &[], true, w, p)), vec!["weapon.shotgun.fire", "weapon.shotgun.fire.lowammo"]);
        // the Hydraulic Drum is persistent: the shot is silent, FireStart starts one sound per channel that is on, FireStop fades all three and plays the stop sound
        let dr = bps(&["B_Assault_Hydraulic_Drum_Athena"]);
        assert!(arms_to_sounds(&A::Fire { gun: 0 }, &dr, false, w, p).is_empty());
        assert_eq!(ids(arms_to_sounds(&A::FireStart { gun: 0 }, &dr, false, w, p)), vec!["loop bp.B_Assault_Hydraulic_Drum_Athena.fire #0"]);
        assert_eq!(ids(arms_to_sounds(&A::FireStart { gun: 2 }, &bps(&["B_Rifle_Sniper_Athena", "B_Rifle_Sniper_Athena", "B_Assault_Burst_Athena"]), true, w, p)), vec!["loop bp.B_Assault_Burst_Athena.fire #6", "loop bp.B_Assault_Burst_Athena.fire.lowammo #7"]);
        let stop = ids(arms_to_sounds(&A::FireStop { gun: 0 }, &dr, false, w, p));
        assert_eq!(stop, vec!["stop #0 0.15", "stop #1 0.15", "stop #2 0.15", "bp.B_Assault_Hydraulic_Drum_Athena.fire.stop"]);
        // other moments keep their mapping
        assert_eq!(ids(arms_to_sounds(&A::Empty { gun: 0 }, &ar, false, w, p)), vec!["bp.B_Assault_Auto_Athena.empty"]);
    }
    #[test] fn arms_sounds_map_to_weapon_table_events_at_the_weapon_or_the_pawn() {
        use crate::arms::ArmsSound as A;
        use fn_audio::weapon_events::{self as we, ReloadStage as Rs};
        let (w, p) = (glam::Vec3::new(1., 2., 3.), glam::Vec3::new(4., 0., 6.));
        let e = |s: A| arms_to_sound(&s, w, p);
        assert_eq!(e(A::Fire { gun: 1 }), Some(SoundEvent::Weapon { id: "weapon.shotgun.fire", pos: Some(w), volume: 1. }));
        assert_eq!(e(A::Empty { gun: 2 }), Some(SoundEvent::Weapon { id: "weapon.sniper.empty", pos: Some(w), volume: 1. }));
        assert_eq!(e(A::ReloadStage { gun: 0, stage: Rs::Cartridge }), Some(SoundEvent::Weapon { id: "weapon.ar.reload.cartridge", pos: Some(w), volume: 1. }));
        assert_eq!(e(A::Notify { cue: "/Game/Sounds/Fort_GamePlay_Sounds/Player/Player_Equip_Projectile_Equip_Cue.Player_Equip_Projectile_Equip_Cue".into(), pitch: 1., volume: 1. }), Some(SoundEvent::Weapon { id: "weapon.equip", pos: Some(p), volume: 1. }));
        assert_eq!(e(A::Notify { cue: "/Game/Sounds/Fort_GamePlay_Sounds/Player/Player_Equip_Melee_Cue.X".into(), pitch: 1., volume: 1. }), None, "the pickaxe equip belongs to the melee events");
        assert_eq!(e(A::Fire { gun: 3 }), None);
        assert!(we::lookup("weapon.shotgun.fire").is_some());
    }
    /// MUSIC HOOK: the flow stages drive the loading mix (silent loading screens) and the login track.
    #[test] fn flow_stages_drive_the_loading_mix_and_the_lobby_music() {
        use crate::app::startup::Stage;
        let run =|stages: &[Stage]| { let mut s = RecordingAudio::default(); for st in stages { stage_audio(&mut s, *st); } s };
        // the default reference flow has no front-end screen: the loading mix is up from the start-up loading screen until Playing; no music is started
        let d = run(&[Stage::Splash, Stage::StartupLoading, Stage::Loading, Stage::Finishing, Stage::Playing]);
        assert_eq!(d.loading_screen, vec![true, true, false]);
        assert!(d.music.iter().all(|m| m.is_none()), "{:?}", d.music);
        // with the login screen + game picker: loading mix off + login music on at the login screen (kept on the picker), music fades out and the mix is back
        // for the level travel
        let p = run(&[Stage::Splash, Stage::StartupLoading, Stage::LoggingIn, Stage::GamePicker, Stage::Loading, Stage::Finishing, Stage::Playing]);
        assert_eq!(p.loading_screen, vec![true, false, true, false]);
        assert_eq!(p.music, vec![Some(MusicCue::Login), None]);
    }
    #[test] fn the_settings_apply_layer_drives_the_sink_volumes_and_quality() {
        let mut s = fn_settings::Settings::default(); let mut sink = RecordingAudio::default();
        s.set("audio.music_volume", fn_settings::Value::Float(0.3)).unwrap(); s.set("audio.sfx_volume", fn_settings::Value::Float(0.6)).unwrap(); s.set("audio.quality", fn_settings::Value::Index(1)).unwrap();
        s.apply_audio(&mut SinkHooks(&mut sink));
        let v = sink.volumes.unwrap(); assert_eq!((v[1], v[2]), (0.3, 0.6)); assert_eq!(sink.quality_high, Some(false));
    }
}
