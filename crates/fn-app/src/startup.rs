//! The start-up flow of the game, independent of winit and the GPU so it can be tested with a fake clock and driven by the capture mode.
//!
//! ```text
//! Splash (Splash.bmp, min 1.5 s) -> StartupLoading (key art + EARLY ACCESS + LOADING + progress line, real work on a worker thread, no minimum, 0.3 s fade to black)
//!   -> [GamePicker: Battle Royale only] -> Loading (level-travel screen, real progress)
//!   -> Finishing (100 % hold 0.6 s, 0.3 s fade to black) -> Playing (0.5 s fade-in from black)
//! ```
//! Timing values are the user's reference implementation (old prototype `loading.rs`): `SPLASH_MIN_SECONDS` 1.5, `COMPLETE_HOLD_SECONDS` 0.6, `FADE_OUT_SECONDS` 0.3, the world's
//! 0.5 s fade-in; automated (`--quick`) runs use 0.3 s for the splash and 0.1 s for the 100 % hold. The reference has NO other minimum display time (the 1.0 s start-up
//! minimum and the 2.5 s loading minimum of the earlier rebuild were inventions and are gone).
//! Skip rules (Enter, as the reference's `enter_down`): a skip waives a FIXED hold (the splash hold, the start-up fade, the loading fade, the 100 % hold AND the fade after it -
//! the reference `break`s out of the whole hold+fade loop) but never real work: a skip pressed while the start-up work or the load is still running is remembered and
//! applies the moment that work completes. Enter does nothing on the picker.
//! [`StartupWork`] is the real start-up work (opening the game files, decoding the front-end art, pre-decoding the menu art) on a worker thread.
use crate::assets::AssetRoots;
use crate::loader::Progress;
use fn_ui::assets::{AssetSource, Image};
use fn_ui::screens::loading::{FinishTimeline, FADE_OUT_SECONDS, SPLASH_MIN_SECONDS};
use fn_ui::screens::picker::Item;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const SPLASH_SECONDS: f32 = SPLASH_MIN_SECONDS;
/// The start-up screen has no minimum display time in the reference (it is shown while the real work runs).
pub const STARTUP_MIN_SECONDS: f32 = 0.0;
/// Fade to black when the start-up screen leaves (reference `FADE_OUT_SECONDS`).
pub const STARTUP_FADE_SECONDS: f32 = FADE_OUT_SECONDS;
/// No minimum for the level-travel screen either (reference).
pub const LOADING_MIN_SECONDS: f32 = 0.0;
/// Splash hold of automated runs (reference `hold_splash(.., quick)`).
pub const QUICK_SPLASH_SECONDS: f32 = 0.3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing { pub splash: f32, pub startup_min: f32, pub startup_fade: f32, pub loading_min: f32, pub finish: FinishTimeline }
impl Default for Timing { fn default() -> Self { Timing { splash: SPLASH_SECONDS, startup_min: STARTUP_MIN_SECONDS, startup_fade: STARTUP_FADE_SECONDS, loading_min: LOADING_MIN_SECONDS, finish: FinishTimeline::new(false) } } }
impl Timing {
    /// Runs that skip the flow (`--skip-loading`, scripted modes): no holds at all (the real work is still waited for) and a short 100 % hold.
    pub fn skipped() -> Self { Timing { splash: 0., startup_min: 0., startup_fade: 0., loading_min: 0., finish: FinishTimeline::new(true) } }
    /// `--quick`: the reference's automated-run holds (0.3 s splash, 0.1 s 100 % hold); the real work is still waited for.
    pub fn quick() -> Self { Timing { splash: QUICK_SPLASH_SECONDS, startup_min: 0., startup_fade: 0.05, loading_min: 0., finish: FinishTimeline::new(true) } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// FRONT END: `LoggingIn` = the cooked `LoginScreen_C` ("Logging In...") between the start-up screen and the game picker (picker flow only).
pub enum Stage { Splash, StartupLoading, LoggingIn, GamePicker, Lobby, Loading, Finishing, Playing }
impl Stage { pub fn name(self) -> &'static str { match self { Stage::Splash => "Splash", Stage::StartupLoading => "StartupLoading", Stage::LoggingIn => "LoggingIn", Stage::GamePicker => "GamePicker", Stage::Lobby => "Lobby", Stage::Loading => "Loading", Stage::Finishing => "Finishing", Stage::Playing => "Playing" } } }

#[derive(Clone, Debug)]
pub struct Flow {
    pub timing: Timing,
    pub stage: Stage,
    /// Seconds in the current stage / since the flow started (sums of the `tick` deltas: a fake clock in tests and captures).
    pub stage_t: f32,
    pub now: f32,
    /// The reusable flow supports an optional picker; the normal application enables it for the Battle Royale launch path.
    pub picker: bool,
    pub lobby: bool,
    work_done: bool,
    loader_done: bool,
    /// FRONT END: the login screen's share of the load is done (`LoggingIn` -> `GamePicker`)
    login_done: bool,
    /// FRONT END: the art-only hold after the start-up screen's fade (its share of the load) is done (`StartupLoading` -> `LoggingIn`)
    art_hold_done: bool,
    skip: bool,
    fade_from: Option<f32>,
    log: Vec<(f32, String)>,
    /// Print every log line to stderr as it is added.
    pub echo: bool,
}

impl Flow {
    pub fn new(timing: Timing, first: Stage) -> Flow {
        let mut f = Flow { timing, stage: first, stage_t: 0., now: 0., picker: false, lobby: false, work_done: false, loader_done: false, login_done: false, art_hold_done: false, skip: false, fade_from: None, log: vec![], echo: false };
        f.note(format!("-> {}", first.name()));
        f
    }
    /// Enables the optional game picker between the start-up screen and the level-travel screen.
    pub fn with_picker(mut self, on: bool) -> Flow { self.picker = on; self }
    /// Timestamped log line (`now` = seconds on the flow clock).
    pub fn note(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.echo { eprintln!("[startup {:8.3}] {msg}", self.now); }
        self.log.push((self.now, msg));
    }
    pub fn log(&self) -> &[(f32, String)] { &self.log }
    pub fn log_text(&self) -> String { self.log.iter().map(|(t, m)| format!("[{t:8.3}] {m}\n")).collect() }

    pub fn set_work_done(&mut self) { if !self.work_done { self.work_done = true; self.note("start-up work finished"); } }
    pub fn set_loader_done(&mut self) { if !self.loader_done { self.loader_done = true; self.note("load finished"); } }
    pub fn set_art_hold_done(&mut self) { if !self.art_hold_done { self.art_hold_done = true; self.note("art-only hold: its share of the load finished"); } }
    pub fn set_login_done(&mut self) { if !self.login_done { self.login_done = true; self.note("login share of the load finished"); } }
    pub fn work_done(&self) -> bool { self.work_done }
    pub fn loader_done(&self) -> bool { self.loader_done }

    /// Enter: waives the fixed hold of the current stage (never real work; remembered until the work is done). No effect on the picker.
    pub fn request_skip(&mut self) {
        if matches!(self.stage, Stage::LoggingIn | Stage::GamePicker | Stage::Lobby | Stage::Playing) { return; }
        if !self.skip { self.skip = true; self.note(format!("skip requested in {}", self.stage.name())); }
    }
    /// The player chose Battle Royale on the picker.
    pub fn pick_battle_royale(&mut self) -> bool {
        if self.stage != Stage::GamePicker { return false; }
        self.note("picker: Battle Royale chosen");
        self.go(if self.lobby { Stage::Lobby } else { Stage::Loading });
        true
    }
    pub fn lobby_play(&mut self) -> bool {
        if self.stage != Stage::Lobby { return false; }
        self.note("lobby: matchmaking finished");
        self.go(Stage::Loading);
        true
    }
    /// Jumps straight to `s` (scripted runs leave the loading stage without the finishing hold).
    pub fn force(&mut self, s: Stage) { if self.stage != s { self.go(s); } }
    fn go(&mut self, s: Stage) { self.stage = s; self.stage_t = 0.; self.skip = false; self.fade_from = None; self.note(format!("-> {}", s.name())); }

    /// Advances the clock by `dt` and runs the transition rules. Returns the stage entered, if any.
    pub fn tick(&mut self, dt: f32) -> Option<Stage> {
        let dt = dt.max(0.);
        self.now += dt; self.stage_t += dt;
        let t = self.timing;
        let after_startup = if self.picker { Stage::LoggingIn } else { Stage::Loading };
        let next = match self.stage {
            Stage::Splash => (self.skip || self.stage_t >= t.splash).then_some(Stage::StartupLoading),
            Stage::StartupLoading => {
                if self.work_done && (self.skip || self.stage_t >= t.startup_min) { self.fade_from.get_or_insert(self.stage_t); }
                let done = self.fade_from.map_or(false, |f| self.skip || self.stage_t - f >= t.startup_fade);
                (done && (self.art_hold_done || !self.picker)).then_some(after_startup)
            }
            Stage::LoggingIn => self.login_done.then_some(Stage::GamePicker),
            Stage::GamePicker | Stage::Lobby | Stage::Playing => None,
            Stage::Loading => (self.loader_done && (self.skip || self.stage_t >= t.loading_min)).then_some(Stage::Finishing),
            // Enter breaks out of the whole hold + fade (reference `finish`: `if enter_down() { break; }`)
            Stage::Finishing => (self.skip || t.finish.finished(self.stage_t)).then_some(Stage::Playing),
        };
        if let Some(s) = next { self.go(s); }
        next
    }
    /// Black overlay of the start-up screen while it fades out (0 until the work finished).
    pub fn startup_fade(&self) -> f32 {
        if self.stage != Stage::StartupLoading { return 0.; }
        if self.timing.startup_fade <= 0. { return 0.; }
        self.fade_from.map_or(0., |f| ((self.stage_t - f) / self.timing.startup_fade).clamp(0., 1.))
    }
    /// Black overlay of the loading screen in `Finishing` (hold, then the fade to black).
    pub fn finish_fade(&self) -> f32 { if self.stage == Stage::Finishing { self.timing.finish.fade_at(self.stage_t) } else { 0. } }
}

// ------------------------------------------------------------------------------------------------------------------------------------------
// the real start-up work
// ------------------------------------------------------------------------------------------------------------------------------------------

/// Images decoded by the worker, served to the UI as the first layer of its asset source (file-name keyed, case-insensitive). FRONT END: once the worker
/// handed over its pak reader, a name the map does not hold is read / decoded from the paks (the cooked login / picker screens' brushes and material
/// textures, by long package path).
#[derive(Clone, Default)]
pub struct Overlay(Arc<Mutex<HashMap<String, Image>>>, Arc<Mutex<Option<fn_ui::pak_assets::PakAssets>>>);
impl Overlay {
    pub fn insert(&self, name: &str, im: Image) { if let Ok(mut m) = self.0.lock() { m.insert(name.to_ascii_lowercase(), im); } }
    pub fn contains(&self, name: &str) -> bool { self.0.lock().map_or(false, |m| m.contains_key(&name.to_ascii_lowercase())) }
    pub fn release_large(&self, min_px: u64) -> usize { self.0.lock().map(|mut m| { let n = m.len(); m.retain(|_, i| (i.w as u64) * (i.h as u64) < min_px); n - m.len() }).unwrap_or(0) }
    pub fn set_paks(&self, p: fn_ui::pak_assets::PakAssets) { if let Ok(mut s) = self.1.lock() { *s = Some(p); } }
}
impl AssetSource for Overlay {
    fn read(&self, name: &str) -> Option<Vec<u8>> { self.1.lock().ok().and_then(|p| p.as_ref().and_then(|p| p.read(name))) }
    fn read_in(&self, name: &str, dir_hint: &str) -> Option<Vec<u8>> { self.1.lock().ok().and_then(|p| p.as_ref().and_then(|p| p.read_in(name, dir_hint))) }
    fn image(&self, name: &str) -> Option<Image> {
        if let Some(i) = self.0.lock().ok().and_then(|m| m.get(&name.to_ascii_lowercase()).cloned()) { return Some(i); }
        self.1.lock().ok().and_then(|p| p.as_ref().and_then(|p| p.image(name)))
    }
}

#[derive(Default)]
struct Shared {
    /// The start-up screen's own art (key art, plate) is in the overlay.
    startup_art: AtomicBool,
    /// The front-end art (picker key art + wordmark) is in the overlay (always true when the picker is off: nothing to decode).
    front_end_art: AtomicBool,
    /// The main thread started the front-end phase (the start-up screen is up): the worker waits for this before the work the bar shows.
    go: AtomicBool,
    done: AtomicBool,
    note: Mutex<Option<String>>,
}

/// Brush textures decoded per progress unit.
const BRUSH_CHUNK: usize = 8;
const W_OPEN: f32 = 0.14;
const W_STARTUP_ART: f32 = 0.05;
const W_FRONT_ART: f32 = 0.16;
const W_BRUSHES: f32 = 0.30;

/// Start-up work on a worker thread: (1) open the game files and decode the start-up screen's own art (behind the splash), then, once the start-up screen is
/// up (`begin_front_end`), (2) with the picker on: decode the front-end art (the picker's key art and wordmark) and (3) decode every menu / HUD brush texture of the asset
/// roots, so the in-match load finds them ready. Progress (units with weights, see `loader::Core`) drives the start-up screen's line: it moves when a unit COMPLETES.
pub struct StartupWork { shared: Arc<Shared>, progress: Arc<Progress>, started: Instant }
impl StartupWork {
    /// `install`: a Paks directory or the install folder (None / missing: the art stays absent and the screens draw their text-only fallback).
    /// `picker_art`: also decode the game picker's art (only needed with `--game-picker`).
    pub fn spawn(install: Option<PathBuf>, roots: Arc<AssetRoots>, overlay: Overlay, picker_art: bool) -> StartupWork {
        let brushes = fn_ui::menu::lower::brush_files();
        let chunks: Vec<Vec<String>> = brushes.chunks(BRUSH_CHUNK).map(|c| c.to_vec()).collect();
        let mut units: Vec<(String, f32)> = vec![("Opening the game files".into(), W_OPEN), ("Decoding the start-up art".into(), W_STARTUP_ART)];
        if picker_art { units.push(("Decoding the front-end art".into(), W_FRONT_ART)); }
        let base = units.len();
        let n = chunks.len();
        for i in 0..n { units.push((format!("Loading menu art ({}/{n})", i + 1), W_BRUSHES / n.max(1) as f32)); }
        let progress = Progress::new(units);
        let shared = Arc::new(Shared::default());
        let (sh, pr) = (shared.clone(), progress.clone());
        std::thread::Builder::new().name("startup-work".into()).spawn(move || {
            let note = |m: String| { if let Ok(mut n) = sh.note.lock() { n.get_or_insert(m); } };
            pr.begin(0);
            let assets = install.as_ref().and_then(|d| match fn_ui::pak_assets::PakAssets::open(d) { Ok(a) => Some(a), Err(e) => { note(e); None } });
            if assets.is_none() { note("the game files were not found: front-end art unavailable".into()); }
            pr.end(0);
            // the start-up screen's own art
            pr.begin(1);
            if let Some(a) = assets.as_ref() {
                for name in fn_ui::screens::startup_loading::PAK_TEXTURES { match a.decode_texture(name) { Ok(im) => overlay.insert(name, im), Err(e) => note(format!("{name}: {e}")) } }
            }
            pr.end(1);
            sh.startup_art.store(true, Ordering::Release);
            while !sh.go.load(Ordering::Acquire) { std::thread::sleep(std::time::Duration::from_millis(2)); }
            if picker_art {
                pr.begin(2);
                if let Some(a) = assets.as_ref() {
                    for name in fn_ui::screens::picker::PAK_TEXTURES { if !overlay.contains(name) { match a.decode_texture(name) { Ok(im) => overlay.insert(name, im), Err(e) => note(format!("{name}: {e}")) } } }
                }
                pr.end(2);
            }
            if let Some(a) = assets { overlay.set_paks(a); } // FRONT END: the pak reader serves the cooked front end's textures from now on
            sh.front_end_art.store(true, Ordering::Release);
            for (i, chunk) in chunks.iter().enumerate() {
                pr.begin(base + i);
                for name in chunk { if let Ok(img) = roots.load_rgba(name) { let (w, h) = (img.width(), img.height()); overlay.insert(name, Image { w, h, rgba: img.into_raw() }); } }
                pr.end(base + i);
            }
            sh.done.store(true, Ordering::Release);
        }).ok();
        StartupWork { shared, progress, started: Instant::now() }
    }
    /// A finished work object, for runs that skip the start-up flow.
    pub fn none() -> StartupWork {
        let s = Shared::default(); s.done.store(true, Ordering::Release); s.startup_art.store(true, Ordering::Release); s.front_end_art.store(true, Ordering::Release); s.go.store(true, Ordering::Release);
        StartupWork { shared: Arc::new(s), progress: Progress::new(vec![]), started: Instant::now() }
    }
    /// The start-up screen is up: run the front-end phase (the part the bar shows).
    pub fn begin_front_end(&self) { self.shared.go.store(true, Ordering::Release); }
    pub fn startup_art_ready(&self) -> bool { self.shared.startup_art.load(Ordering::Acquire) }
    pub fn front_end_art_ready(&self) -> bool { self.shared.front_end_art.load(Ordering::Acquire) }
    pub fn done(&self) -> bool { self.shared.done.load(Ordering::Acquire) }
    /// LOAD COUNTER: (finished units, all units) of the start-up work.
    pub fn counts(&self) -> (usize, usize) { self.progress.counts() }
    /// 0..1 weighted progress of the worker's COMPLETED units.
    pub fn fraction(&self) -> f32 { if self.done() { 1. } else { self.progress.fraction() } }
    /// (label, seconds, start offset) of the finished units.
    pub fn steps(&self) -> Vec<(String, f32, f32)> { self.progress.timings() }
    pub fn note(&self) -> Option<String> { self.shared.note.lock().ok().and_then(|n| n.clone()) }
    pub fn age(&self) -> f32 { self.started.elapsed().as_secs_f32() }
}
// ------------------------------------------------------------------------------------------------------------------------------------------
// deterministic capture of the flow (`--startup-capture`)
// ------------------------------------------------------------------------------------------------------------------------------------------
pub mod capture {
    use super::*;
    use fn_ui::screens::picker::Page;

    /// What the capture script looks at each frame.
    #[derive(Clone, Debug)]
    pub struct View { pub stage: Stage, pub stage_t: f32, pub fraction: f32, pub loader_done: bool, pub startup_fade: f32, pub finish_fade: f32, pub size: (u32, u32), pub page: Page, pub art_mix: f32, /// The start-up screen's key art is uploaded (the screen shows it).
    pub art_ready: bool }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Cond {
        /// In `stage` for at least this many (fake) seconds.
        StageTime(Stage, f32),
        /// In the start-up screen for this long AND its key art is on screen (the art arrives from the worker in real time).
        StartupArt(f32),
        /// The loading progress fraction reached this value (Loading stage).
        Fraction(f32),
        /// The load finished while the loading screen is still up (100 %; the Loading screen or its 100 % hold).
        LoaderDone,
        StartupFade(f32),
        FinishFade(f32),
        /// This many seconds since the previous action (lets hover animations settle).
        Settled(f32),
        /// The picker is on this page and has settled.
        PageSettled(Page, f32),
        SizeIs(u32, u32),
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum Act { Shot(String), Hover(Item), Click(Item), Resize(u32, u32), Finish }
    #[derive(Clone, Debug)]
    pub struct Step { pub cond: Cond, pub act: Act }

    #[derive(Clone, Debug)]
    pub struct Script { pub steps: Vec<Step>, pub idx: usize, since: f32 }

    fn st(cond: Cond, act: Act) -> Step { Step { cond, act } }
    fn shot(n: &str) -> Act { Act::Shot(n.to_owned()) }

    impl Script {
        /// The standard capture: splash, Athena start-up loading, then the BR-only
        /// picker (idle and both buttons), level-travel loading, fade, and world.
        pub fn standard(_size: (u32, u32), picker: bool) -> Script {
            use Cond::*;
            let mut steps = vec![
                st(StageTime(Stage::Splash, 0.7), shot("01-splash")),
                st(StartupArt(0.0), shot("02-startup-loading-art")),
                st(StartupFade(0.5), shot("03-startup-loading-fading")),
            ];
            if picker {
                steps.extend([
                    st(StageTime(Stage::LoggingIn, 0.3), shot("035-logging-in")),
                    st(StageTime(Stage::GamePicker, 0.9), shot("04-picker-idle")),
                    st(Settled(0.1), Act::Hover(Item::BattleRoyale)),
                    st(Settled(0.5), shot("05-picker-hover-battle-royale")),
                    st(Settled(0.1), Act::Hover(Item::Quit)),
                    st(Settled(0.5), shot("06-picker-hover-quit")),
                    st(Settled(0.1), Act::Hover(Item::BattleRoyale)),
                    st(Settled(0.2), Act::Click(Item::BattleRoyale)),
                ]);
            }
            steps.extend([
                st(Fraction(0.10), shot("13-loading-10pct")),
                st(Fraction(0.50), shot("14-loading-50pct")),
                st(LoaderDone, shot("15-loading-100pct")),
                st(FinishFade(0.5), shot("16-loading-fade-mid")),
                st(StageTime(Stage::Playing, 0.9), shot("17-world")),
                st(Settled(0.0), Act::Finish),
            ]);
            Script { steps, idx: 0, since: 0. }
        }        pub fn done(&self) -> bool { self.idx >= self.steps.len() }
        pub fn next_cond(&self) -> Option<&Cond> { self.steps.get(self.idx).map(|s| &s.cond) }
        /// Advances the "since last action" clock by `dt`; returns the next action when its condition holds (at most one per call).
        pub fn poll(&mut self, v: &View, dt: f32) -> Option<Act> {
            self.since += dt.max(0.);
            let step = self.steps.get(self.idx)?;
            let ok = match &step.cond {
                Cond::StageTime(s, t) => v.stage == *s && v.stage_t >= *t,
                Cond::StartupArt(t) => v.stage == Stage::StartupLoading && v.stage_t >= *t && v.art_ready,
                Cond::Fraction(f) => matches!(v.stage, Stage::Loading | Stage::Finishing) && v.fraction >= *f, // FRONT END: with the split load the level-travel screen can open already at 100 %
                Cond::LoaderDone => matches!(v.stage, Stage::Loading | Stage::Finishing) && v.loader_done, // no minimum display time: the stage can already be Finishing
                Cond::StartupFade(f) => v.stage == Stage::StartupLoading && v.startup_fade >= *f,
                Cond::FinishFade(f) => v.stage == Stage::Finishing && v.finish_fade >= *f,
                Cond::Settled(t) => self.since >= *t,
                Cond::PageSettled(p, t) => v.page == *p && self.since >= *t,
                Cond::SizeIs(w, h) => v.size == (*w, *h) && self.since >= 0.2,
            };
            if !ok { return None; }
            let act = step.act.clone();
            self.idx += 1; self.since = 0.;
            Some(act)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(f: &mut Flow, secs: f32, dt: f32) -> Vec<(f32, Stage)> {
        let mut out = vec![]; let n = (secs / dt).round() as usize;
        for _ in 0..n { if let Some(s) = f.tick(dt) { out.push((f.now, s)); } }
        out
    }
    fn at(v: &[(f32, Stage)], s: Stage) -> f32 { v.iter().find(|(_, x)| *x == s).map(|(t, _)| *t).unwrap_or(f32::NAN) }
    const DT: f32 = 1. / 60.;

    #[test] fn timing_constants_are_the_reference_values() {
        let t = Timing::default();
        assert_eq!((t.splash, t.startup_min, t.startup_fade, t.loading_min), (1.5, 0., 0.3, 0.));
        assert_eq!((t.finish.hold, fn_ui::screens::loading::FADE_OUT_SECONDS), (0.6, 0.3));
        let q = Timing::quick(); assert_eq!((q.splash, q.finish.hold), (0.3, 0.1));
    }
    #[test] fn raw_flow_without_the_application_picker_option_goes_startup_to_the_travel_screen() {
        let mut f = Flow::new(Timing::default(), Stage::Splash);
        f.set_work_done();
        let tr = run(&mut f, 3.2, DT);
        assert!((at(&tr, Stage::StartupLoading) - 1.5).abs() < 0.02, "splash 1.5 s: {tr:?}");
        assert!((at(&tr, Stage::Loading) - (1.5 + STARTUP_FADE_SECONDS)).abs() < 0.05, "no start-up minimum: only the 0.3 s fade after the work: {tr:?}");
        assert!(tr.iter().all(|(_, s)| *s != Stage::GamePicker), "the raw Flow option is off until App::new supplies its normal picker setting: {tr:?}");
        assert_eq!(f.stage, Stage::Loading);
    }
    #[test] fn with_the_picker_flag_the_picker_sits_between_start_up_and_travel_and_waits_for_the_player() {
        let mut f = Flow::new(Timing::default(), Stage::Splash).with_picker(true);
        f.set_work_done();
        f.set_art_hold_done();
        let tr = run(&mut f, 3.2, DT);
        // FRONT END: the login screen holds until its share of the load is done
        assert!((at(&tr, Stage::LoggingIn) - (1.5 + STARTUP_FADE_SECONDS)).abs() < 0.05, "{tr:?}");
        assert_eq!(f.stage, Stage::LoggingIn);
        f.set_login_done();
        let tr = run(&mut f, 0.1, DT);
        assert_eq!(tr.first().map(|t| t.1), Some(Stage::GamePicker), "{tr:?}");
        assert_eq!(f.stage, Stage::GamePicker);
        assert!(run(&mut f, 120., 0.1).is_empty(), "the picker waits for the player");
        assert!(f.pick_battle_royale()); assert_eq!(f.stage, Stage::Loading);
    }
    #[test] fn slow_start_up_work_is_never_skipped_by_time() {
        let mut f = Flow::new(Timing::default(), Stage::Splash);
        run(&mut f, 6., DT);
        assert_eq!(f.stage, Stage::StartupLoading, "work still running after 6 s");
        f.set_work_done();
        let tr = run(&mut f, 1., DT);
        assert!((at(&tr, Stage::Loading) - (6. + STARTUP_FADE_SECONDS)).abs() < 0.04, "leaves one fade after the work finished: {tr:?}");
    }
    #[test] fn enter_skips_the_splash_hold_at_once() {
        let mut f = Flow::new(Timing::default(), Stage::Splash);
        run(&mut f, 0.3, DT); f.request_skip(); let tr = run(&mut f, 0.05, DT);
        assert_eq!(f.stage, Stage::StartupLoading); assert!(at(&tr, Stage::StartupLoading) < 0.4, "{tr:?}");
    }
    #[test] fn enter_in_the_start_up_screen_waits_for_the_work_then_leaves_immediately() {
        let mut f = Flow::new(Timing::default(), Stage::StartupLoading);
        run(&mut f, 0.2, DT); f.request_skip(); run(&mut f, 1.0, DT);
        assert_eq!(f.stage, Stage::StartupLoading, "the real work is not done: no skip");
        f.set_work_done(); run(&mut f, 0.05, DT);
        assert_eq!(f.stage, Stage::Loading, "the remembered skip applies once the work is done (no fade)");
    }
    #[test] fn enter_does_nothing_on_the_picker_and_a_stale_skip_does_not_leak_into_the_next_stage() {
        let mut f = Flow::new(Timing::default(), Stage::GamePicker).with_picker(true);
        f.request_skip(); run(&mut f, 1., DT); assert_eq!(f.stage, Stage::GamePicker);
        assert!(f.pick_battle_royale()); assert_eq!(f.stage, Stage::Loading);
        run(&mut f, 1., DT); assert_eq!(f.stage, Stage::Loading, "no stale skip: still waiting for the loader");
        assert!(!f.pick_battle_royale(), "only valid on the picker");
    }
    #[test] fn the_travel_screen_has_no_minimum_time_then_holds_the_hundred_percent_and_fades() {
        let mut f = Flow::new(Timing::default(), Stage::Loading); run(&mut f, 0.5, DT); assert_eq!(f.stage, Stage::Loading, "leaves only when the load finished");
        f.set_loader_done();
        let tr = run(&mut f, 2., DT);
        assert!((at(&tr, Stage::Finishing) - 0.5).abs() < 0.03, "{tr:?}");
        assert!((at(&tr, Stage::Playing) - at(&tr, Stage::Finishing) - 0.6 - 0.3).abs() < 0.04, "0.6 s hold + 0.3 s fade: {tr:?}");
    }
    #[test] fn slow_load_is_waited_for_and_enter_never_skips_it() {
        let mut f = Flow::new(Timing::default(), Stage::Loading);
        f.request_skip(); run(&mut f, 5., DT); assert_eq!(f.stage, Stage::Loading, "loader not done: stays");
        f.set_loader_done(); run(&mut f, 0.03, DT); assert_eq!(f.stage, Stage::Finishing, "the remembered skip waives only a hold, never the work");
    }
    #[test] fn enter_breaks_out_of_the_hundred_percent_hold_and_the_fade_like_the_reference() {
        let mut f = Flow::new(Timing::default(), Stage::Finishing);
        run(&mut f, 0.2, DT); f.request_skip(); let tr = run(&mut f, 0.1, DT);
        assert_eq!(f.stage, Stage::Playing); assert!(at(&tr, Stage::Playing) < 0.2 + 0.0167 * 2., "the whole hold + fade is waived: {tr:?}");
        assert!(f.finish_fade() == 0.);
        // pressed during the fade: also leaves at once
        let mut g = Flow::new(Timing::default(), Stage::Finishing); run(&mut g, 0.75, DT); assert!(g.finish_fade() > 0.); g.request_skip(); run(&mut g, 0.03, DT); assert_eq!(g.stage, Stage::Playing);
    }
    #[test] fn fades_are_zero_outside_their_stage_and_monotonic_inside() {
        let mut f = Flow::new(Timing::default(), Stage::StartupLoading); f.set_work_done();
        let mut last = -1.; let mut saw_mid = false;
        for _ in 0..400 { f.tick(DT); let v = f.startup_fade(); assert!(v >= last - 1e-6 || f.stage != Stage::StartupLoading); last = v; if v > 0.3 && v < 0.7 { saw_mid = true; } }
        assert!(saw_mid); assert_eq!(f.startup_fade(), 0.);
        let mut g = Flow::new(Timing::default(), Stage::Finishing); assert_eq!(g.finish_fade(), 0.); run(&mut g, 0.75, DT); let m = g.finish_fade(); assert!(m > 0. && m < 1.);
    }
    #[test] fn quick_mode_shrinks_the_holds_but_still_waits_for_work() {
        let mut f = Flow::new(Timing::quick(), Stage::Splash);
        run(&mut f, 0.5, DT); assert_eq!(f.stage, Stage::StartupLoading); f.set_work_done(); run(&mut f, 0.3, DT); assert_eq!(f.stage, Stage::Loading);
    }
    #[test] fn skipped_timing_waives_every_hold_but_not_the_work() {
        let mut f = Flow::new(Timing::skipped(), Stage::StartupLoading);
        run(&mut f, 1., DT); assert_eq!(f.stage, Stage::StartupLoading); f.set_work_done(); run(&mut f, 0.05, DT); assert_eq!(f.stage, Stage::Loading); assert_eq!(f.startup_fade(), 0.);
    }
    #[test] fn the_log_is_timestamped_in_order_and_names_every_stage() {
        let mut f = Flow::new(Timing::default(), Stage::Splash).with_picker(true); f.set_work_done(); f.set_art_hold_done();
        run(&mut f, 3., DT); f.set_login_done(); run(&mut f, 0.1, DT); f.pick_battle_royale(); f.set_loader_done(); run(&mut f, 5., DT);
        let ts: Vec<f32> = f.log().iter().map(|l| l.0).collect(); assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{ts:?}");
        let text = f.log_text();
        for s in ["-> Splash", "-> StartupLoading", "-> LoggingIn", "-> GamePicker", "-> Loading", "-> Finishing", "-> Playing", "start-up work finished", "load finished"] { assert!(text.contains(s), "{s} missing in\n{text}"); }
    }
    #[test] fn negative_and_zero_steps_do_not_move_the_clock_backwards() { let mut f = Flow::new(Timing::default(), Stage::Splash); f.tick(-1.); f.tick(0.); assert_eq!(f.now, 0.); }

    #[test] fn overlay_serves_lowercase_names_and_nothing_else() {
        let o = Overlay::default(); assert!(o.image("A.png").is_none());
        o.insert("A.PNG", Image { w: 1, h: 1, rgba: vec![1, 2, 3, 4] });
        assert_eq!(o.image("a.png").unwrap().rgba, vec![1, 2, 3, 4]); assert!(o.contains("A.png")); assert!(o.read("a.png").is_none());
    }
    fn wait(w: &StartupWork, secs: u64) { let t = Instant::now(); while !w.done() && t.elapsed().as_secs() < secs { std::thread::sleep(std::time::Duration::from_millis(5)); } }
    #[test] fn startup_work_without_an_install_finishes_and_leaves_the_art_absent() {
        let o = Overlay::default();
        let w = StartupWork::spawn(Some(PathBuf::from(r"Z:\definitely\not\here")), Arc::new(AssetRoots::open(vec![])), o.clone(), false);
        let t = Instant::now(); while !w.startup_art_ready() && t.elapsed().as_secs() < 20 { std::thread::sleep(std::time::Duration::from_millis(5)); }
        assert!(w.startup_art_ready()); assert!(!w.done(), "the front-end phase waits for begin_front_end");
        w.begin_front_end(); wait(&w, 20);
        assert!(w.done() && w.front_end_art_ready() && w.fraction() == 1.); assert!(!o.contains("T_BattleRoyaleKeyArt.png")); assert!(w.note().is_some());
    }
    #[test] fn startup_work_decodes_the_art_from_the_install_and_the_menu_brushes_from_the_roots() {
        let install = fn_ui::pak_assets::default_install();
        if !install.exists() { eprintln!("skipping: no install"); return; }
        let roots_dir = fn_ui::assets::DirAssets::default_root();
        let roots = Arc::new(AssetRoots::open(if roots_dir.is_dir() { vec![roots_dir.clone()] } else { vec![] }));
        let o = Overlay::default(); let w = StartupWork::spawn(Some(install), roots, o.clone(), true);
        let t = Instant::now(); while !w.startup_art_ready() && t.elapsed().as_secs() < 60 { std::thread::sleep(std::time::Duration::from_millis(5)); }
        for n in fn_ui::screens::startup_loading::PAK_TEXTURES { assert!(o.contains(n), "{n} decoded before the screen shows"); }
        assert!(!w.done(), "the front-end phase has not started");
        w.begin_front_end(); wait(&w, 120);
        assert!(w.done(), "worker finished"); assert!(w.note().is_none(), "{:?}", w.note());
        for n in fn_ui::screens::picker::PAK_TEXTURES { assert!(o.contains(n), "{n} decoded"); }
        assert!(o.image("T_BattleRoyaleKeyArt.png").is_some());
        let steps = w.steps(); assert!(steps.len() >= 4 && steps.iter().all(|s| s.1 >= 0.0), "{steps:?}");
        if roots_dir.is_dir() { assert!(fn_ui::menu::lower::brush_files().iter().filter(|f| o.contains(f)).count() > 10, "menu brushes were pre-decoded from the roots"); }
    }
    #[test] fn the_none_work_object_is_finished_and_full() { let w = StartupWork::none(); assert!(w.done() && w.startup_art_ready() && w.front_end_art_ready() && w.fraction() == 1.); }
    mod capture_script {
        use super::*; use crate::app::startup::capture::*; use fn_ui::screens::picker::Page;
        fn view(stage: Stage, t: f32) -> View { View { stage, stage_t: t, fraction: 0., loader_done: false, startup_fade: 0., finish_fade: 0., size: (1280, 720), page: Page::Main, art_mix: 0., art_ready: true } }
        fn shots(s: &Script) -> Vec<&str> { s.steps.iter().filter_map(|x| if let Act::Shot(n) = &x.act { Some(n.as_str()) } else { None }).collect() }
        #[test] fn the_standard_script_names_every_required_capture_once_in_order() {
            for picker in [false, true] {
                let s = Script::standard((1280, 720), picker);
                let shots = shots(&s);
                let mut want = vec!["splash", "startup-loading-art", "startup-loading-fading", "loading-10pct", "loading-50pct", "loading-100pct", "loading-fade-mid", "world"];
                if picker { want.extend(["picker-idle", "picker-hover-battle-royale", "picker-hover-quit"]); }
                for w in want { assert!(shots.iter().any(|n| n.ends_with(w)), "{w} missing: {shots:?}"); }
                assert_eq!(shots.iter().any(|n| n.contains("picker")), picker);
                let mut sorted = shots.clone(); sorted.sort(); assert_eq!(sorted, shots, "numbered in order"); sorted.dedup(); assert_eq!(sorted.len(), shots.len());
                assert!(matches!(s.steps.last().unwrap().act, Act::Finish));
            }
        }
        #[test] fn steps_fire_only_when_their_condition_holds_and_one_per_poll() {
            let mut s = Script::standard((1280, 720), false);
            assert_eq!(s.poll(&view(Stage::Splash, 0.2), 0.016), None);
            assert_eq!(s.poll(&view(Stage::Splash, 0.8), 0.016), Some(Act::Shot("01-splash".into())));
            assert_eq!(s.poll(&view(Stage::Splash, 0.9), 0.016), None, "next step waits for the start-up screen");
            assert_eq!(s.idx, 1);
        }
        #[test] fn loading_shots_wait_for_progress_and_the_finish() {
            let mut s = Script::standard((1920, 1080), false);
            s.idx = s.steps.iter().position(|x| x.cond == Cond::Fraction(0.10)).unwrap();
            let mut v = view(Stage::Loading, 1.); v.fraction = 0.05; assert_eq!(s.poll(&v, 0.016), None);
            v.fraction = 0.11; assert!(matches!(s.poll(&v, 0.016), Some(Act::Shot(n)) if n.contains("10pct")));
            v.fraction = 0.4; assert_eq!(s.poll(&v, 0.016), None); v.fraction = 0.5; assert!(matches!(s.poll(&v, 0.016), Some(Act::Shot(n)) if n.contains("50pct")));
            assert_eq!(s.poll(&v, 0.016), None);
            // no minimum display time: the flow may already be in Finishing when the script sees the finished load
            v.stage = Stage::Finishing; v.loader_done = true; assert!(matches!(s.poll(&v, 0.016), Some(Act::Shot(n)) if n.contains("100pct")));
        }
        #[test] fn br_only_picker_capture_never_resizes_or_visits_a_campaign_page() {
            for size in [(1280, 720), (1920, 1080)] {
                assert!(Script::standard(size, false).steps.iter().all(|x| !matches!(x.act, Act::Resize(..))));
                let s = Script::standard(size, true);
                let rs: Vec<(u32, u32)> = s.steps.iter().filter_map(|x| if let Act::Resize(w, h) = x.act { Some((w, h)) } else { None }).collect();
                assert!(rs.is_empty());
            }
        }
    }
}
