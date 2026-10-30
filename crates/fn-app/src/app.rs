//! The application: winit event loop handler, the startup state machine and the per-frame game loop.
//!
//! ```text
//! Boot (window hidden, saved mode/size applied, GPU) -> Splash (Splash.bmp, 1.5 s, in-window)
//!   -> StartupLoading (key art + EARLY ACCESS + LOADING + progress line, the real start-up work on a worker thread, no minimum, 0.3 s fade out)
//!   -> GamePicker (Battle Royale / Quit: mouse, keyboard, gamepad)
//!   -> Loading (level-travel screen, real progress from worker threads, no minimum) -> Finishing (100 %, 0.6 s hold, 0.3 s fade to black)
//!   -> Playing (world, black fade-in)        any failure -> Failed (error page, Esc closes)
//! ```
//! The timing rules live in [`startup::Flow`] (pure, fake-clock tested); this file owns the windows, the GPU and the input. `--skip-loading` (and every scripted mode)
//! starts directly in `Loading`; `--startup-capture` runs the whole flow on a fake 60 Hz clock with synthetic input and writes PNGs of every screen.
//! Frame in `Playing` (the old `App::update` + `render`): real dt (clamped to UE's 0.1 s `MaxDeltaTime`; fixed 1/fps in scripted runs) ->
//! input (`InputState::gather`, gamepad, quickbar) -> `GameSim::step` -> pose evaluation -> scene render -> HUD / menu / fade -> present -> frame limiter.
pub const GRENADE_RETICLE_IMAGE: &str = "HUD_Xhair_Launcher_comp.png";
use crate::audio::{AudioSink, GameplayAudio, SoundEvent};
use crate::arms::{Arms, ArmsData, ArmsInput, Policy};
use crate::backend::AppBackend;
use crate::cli::Options;
use crate::gfx::{Acquire, Gfx};
use crate::input::InputState;
use crate::keys;
use crate::loader::{Loader, Source, State};
use crate::pad::{self, DynPad};
use crate::platform::{WinitPlatform, MIN_CLIENT};
use crate::run::{self, ScriptRun};
use crate::scene::{camera_for, pose_hash, Scene};
use crate::stats::FrameStats;
use crate::ui::{self, events, UiLayer};
use fn_game::inputdev::pad as gpad;
use fn_game::{FrameInput, GameSim};
use fn_hud_adapter::{hud_state, FrameClock, HudInputs};
use fn_settings::{FrameLimiter, PlatformHost, Settings, Value, WindowMode};
use fn_ui::screens::hud::HudLoading;
use fn_ui::input::{Event as UiEvent, MouseButton as UiMouseButton};
use fn_ui::menu::pages::UiContext as MenuContext;
use fn_ui::menu::HostRequest;
use fn_ui::screens::picker::{self, Outcome as PickerOutcome, Picker};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};

/// UE `MaxDeltaTime` stand-in of the old runtime (`time_audit::MAX_DELTA_TIME`): longer frames lose the excess.
pub const MAX_DELTA_TIME: f32 = 0.1;
pub const TITLE: &str = "Fortnite 1.8 - standalone Rust runtime";
const FADE_IN_SECONDS: f32 = 0.5;

/// Fixed step of the start-up capture (`--startup-capture`): the flow clock advances 1/60 s per drawn frame.
pub const CAPTURE_DT: f32 = 1. / 60.;
/// A frame interval longer than this on a start-up screen counts as a UI stall (the screens must keep animating).
pub const STALL_MS: f32 = 50.;

/// FRONT END: `GetPlatformDisplayName` for "Signed in as:": the signed-in account's name (offline: `FN_DISPLAY_NAME`, else the OS user name).
static WORLD_STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
static PLAY_WARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static WARM_GPU_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LAST_FINISH_FADE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
thread_local! { static WARM_T0: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) }; }
const FINISH_STEP_MS: f32 = if cfg!(target_arch = "wasm32") { 6. } else { 25. };
const WORLD_STEPS: usize = 11;
thread_local! { static WORLD_PROPS: std::cell::RefCell<Option<Option<crate::loader::PropsJob>>> = const { std::cell::RefCell::new(None) }; }
fn menu_first() -> bool { static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_MENU_FIRST").map_or(cfg!(target_arch = "wasm32"), |v| v != "0")) }
fn display_name() -> String {
    if cfg!(target_arch = "wasm32") { return fn_live::var("FN_DISPLAY_NAME").ok().filter(|s| !s.is_empty()).or_else(crate::name_store::saved).unwrap_or_else(|| "Player".into()); }
    fn_live::var("FN_DISPLAY_NAME").ok().filter(|s| !s.is_empty()).or_else(|| crate::lobby_profile::stored_display_name(&crate::lobby_profile::default_profile_path())).or_else(|| std::env::var("USERNAME").ok()).unwrap_or_else(|| "Player".into())
}
#[derive(Clone, Debug, PartialEq)]
pub enum Phase { Boot, Splash, StartupLoading, LoggingIn, GamePicker, Lobby, Loading, Finishing, Playing, Failed(String) }
impl Phase {
    fn of(s: Stage) -> Phase { match s { Stage::Splash => Phase::Splash, Stage::StartupLoading => Phase::StartupLoading, Stage::LoggingIn => Phase::LoggingIn, Stage::GamePicker => Phase::GamePicker, Stage::Lobby => Phase::Lobby, Stage::Loading => Phase::Loading, Stage::Finishing => Phase::Finishing, Stage::Playing => Phase::Playing } }
}

/// Longest frame intervals seen on the start-up screens (the UI thread must never block for long while they animate).
#[derive(Clone, Debug, Default)]
pub struct Stalls { pub frames: u32, pub max_ms: f32, pub over_stall: u32, pub worst: Vec<(String, f32)>, skip_next: bool }
impl Stalls {
    pub fn record(&mut self, stage: &str, ms: f32) {
        if std::mem::take(&mut self.skip_next) { return; }
        self.frames += 1; self.max_ms = self.max_ms.max(ms);
        if ms > STALL_MS { self.over_stall += 1; if self.worst.len() < 12 { self.worst.push((stage.to_owned(), ms)); } }
    }
}

/// Front-end art the worker decodes; uploaded to the GPU one texture per frame before it is shown.
#[derive(Default)]
struct ArtState { startup_queue: Vec<&'static str>, picker_queue: Vec<&'static str>, startup_uploaded: bool, picker_uploaded: bool, queued: (bool, bool) }

/// `--startup-capture` state: the script, the shots requested for the next drawn frame, results.
struct CaptureState { script: startup::capture::Script, out: PathBuf, pending: Vec<String>, shots: Vec<PathBuf>, failures: Vec<String>, started: Instant, size: (u32, u32), timeline: Vec<(f32, String)> }

pub struct App {
    pub opts: Options,
    roots: Arc<AssetRoots>,
    source: Source,
    skip_flow: bool,
    flow: Flow,
    overlay: Overlay,
    work: StartupWork,
    picker: Picker,
    art: ArtState,
    cap: Option<CaptureState>,
    stalls: Stalls,
    quit_requested: bool,
    tip: usize,
    gfx: Option<Gfx>,
    platform: Option<WinitPlatform>,
    ui: Option<UiLayer>,
    scene: Option<Box<Scene>>,
    loader: Option<Loader>,
    /// FRONT END: the world finished loading behind the start-up / login screens; applied when the Loading stage starts
    early_scene: Option<Box<Scene>>,
    /// LOAD COUNTER: units of the world load once it started (kept after the loader is dropped)
    loader_units: usize,
    /// FRONT END: 0 = not composed, 1 = cooked screens composed, 2 = picker painted once off screen
    front_step: u8,
    /// FRONT END BACKDROP: the 3D front-end level behind the login screen and the picker (frontend_bg.rs), its material clock
    backdrop: Option<crate::frontend_bg::FrontendBackdrop>,
    backdrop_t: f32,
    /// FRONT END BACKDROP: the CPU half loading on a worker (web: the main thread may not block on the loader's threads)
    backdrop_job: Option<std::sync::mpsc::Receiver<Result<crate::lobby_scene::LobbyAssets, String>>>,
    lobby: Option<crate::lobby_host::LobbyHost>,
    lobby_warmed: bool,
    lobby_shot_done: usize,
    name_entry: Option<crate::login_name::NameEntry>,
    name_known: bool,
    sim: GameSim,
    input: InputState,
    pad: gpad::Gamepad<DynPad>,
    pad_state: Option<gpad::State>,
    /// GAMEPAD LOOK: right-stick look state + the cooked AthenaGamepadSettings (pad.rs `PadLook`, fn_game::gamepad_look).
    pad_look: crate::pad::PadLook,
    pub settings: Settings,
    settings_note: String,
    backend: AppBackend,
    gp_audio: GameplayAudio,
    phase: Phase,
    phase_t: f32,
    startup_elapsed: f32,
    start: Instant,
    /// World time (UE `CurrentWorldTime`): advances by the frame delta only while the simulation steps (pauses with the menu / inventory / lost focus); the material time of the FX programs.
    world_time: f32,
    last: Instant,
    limiter: FrameLimiter,
    clock: FrameClock,
    hud_inputs: HudInputs,
    /// BUILD INPUT HOOK: build mode / building piece / material / edit keys (build_input.rs); intents of the last frame: `build_input.last_intents()`.
    pub build_input: crate::build_input::BuildInput,
    /// The player's quickbar / held firearm / bandage state (rms.rs); None until the original loadout resources were loaded (pak mode).
    arms: Option<Arms>,
    /// CONSOLE HOOK: the F2 command bar (fn-console state + the item catalog built from the loaded game).
    console: crate::console_hook::ConsoleHost,
    ads: Option<crate::ads::AdsDriver>, // ADS HOOK
    /// WEAPON RUNTIME: lazy loading / install of any BR firearm (`weapon_runtime.rs`; API for the console: `request_give` / `request_equip` / `resolve` / `catalog`).
    pub weapons: crate::weapon_runtime::WeaponRuntime,
    storm: crate::storm_hook::StormHook, // STORM HOOK: safe-zone wall + storm clouds (storm_hook.rs)
    fx: Option<fn_fx::bridge::FxWorld>, fx_tried: bool, // RENDER FX HOOK: weak-point ring + damage numbers
    arms_policy: Policy,
    run: Option<ScriptRun>,
    minimized: bool,
    window_created: bool,
    pub exit_code: i32,
    pub load_log: String,
    live_stats: FrameStats,
    live: Option<LiveTest>,
    /// FN_TRACE=1 prints every window event.
    trace: bool,
}
use crate::assets::AssetRoots;
// The start-up flow module sits beside this file (main.rs declares the modules; this keeps the new file self-registering).
#[path = "startup.rs"]
pub mod startup;
use startup::{Flow, Overlay, Stage, StartupWork, Timing};
// BROWSER PORT: automatic quality of the browser build (resolution / scalability ladder driven by the frame rate)
#[cfg(target_arch = "wasm32")]
#[path = "../../../tools/web/fn-web/src/quality_web.rs"]
mod web_quality;

impl App {
    pub fn new(opts: Options, roots: AssetRoots, source: Source, run: Option<ScriptRun>) -> App {
        let scripted = opts.scripted();
        let automated = scripted || opts.startup_capture;
        let (mut settings, note, path) = if opts.uses_user_settings() {
            let path = fn_settings::default_path();
            let (s, r) = Settings::load(&path);
            #[cfg(target_arch = "wasm32")]
            let s = { let mut s = s; if matches!(r.status, fn_settings::LoadStatus::Missing) { let _ = s.set("video.quality", Value::Index(fn_settings::Preset::Medium as usize)); } if let Some(n) = std::env::var("fnweb_view_distance").ok().and_then(|v| v.parse::<usize>().ok()) { let _ = s.set("video.view_distance", Value::Index(n.min(3))); } s };
            (s, format!("{} -> {:?}, file version {:?}, backup {:?}{}", r.path.display(), r.status, r.file_version, r.backup, if r.warnings.is_empty() { String::new() } else { format!("\n{}", r.warnings.join("\n")) }), Some(path))
        } else if let Some(p) = fn_live::var_os("FN_SETTINGS_READONLY").filter(|_| automated) { // PERF: an automated run at a given settings file (read, never written)
            let (s, r) = Settings::load(std::path::Path::new(&p));
            (s, format!("automated run: {} read only -> {:?}", r.path.display(), r.status), None)
        } else { (Settings::default(), "automated run: defaults, the user's settings file is neither read nor written".to_owned(), None) };
        if automated {
            // the run's window: the script's client size, windowed, no vsync (frame times measure the renderer, not the monitor) unless asked
            let (w, h) = opts.size.or(run.as_ref().and_then(|r| r.driver.size())).unwrap_or((1280, 720));
            let _ = settings.set("video.window_mode", Value::Index(2));
            let _ = settings.set("video.resolution", Value::Size(w, h));
            let _ = settings.set("video.vsync", Value::Bool(opts.vsync));
        }
        if opts.live_test {
            let _ = settings.set("video.window_mode", Value::Index(2));
            let _ = settings.set("video.resolution", Value::Size(1280, 720));
            let _ = settings.set("video.vsync", Value::Bool(true));
        }
        if let Some(m) = opts.window_mode { let _ = settings.set("video.window_mode", Value::Index(match m { WindowMode::Fullscreen => 0, WindowMode::WindowedFullscreen => 1, WindowMode::Windowed => 2 })); }
        if opts.no_vsync { let _ = settings.set("video.vsync", Value::Bool(false)); }
        if let Some(q) = opts.video_quality { for id in ["video.resolution_quality", "video.view_distance", "video.shadows", "video.anti_aliasing", "video.textures", "video.effects", "video.post_processing"] { let _ = settings.set(id, Value::Index(q as usize)); } } // SCALABILITY
        if let Some(m) = opts.motion_blur { let _ = settings.set("video.motion_blur", Value::Bool(m)); } // POSTFX
        // This standalone runtime loads the Athena/Battle Royale map, HUD, weapons, inventory and time-of-day assets only.
        // Pin the local front end to Athena, regardless of inherited settings.
        settings.set_extra("Menu", "SubGame", "Athena");
        let mut input = InputState::new();
        input.scripted = automated;
        let skip_flow = opts.skips_flow();
        // Startup capture's synthetic script is calibrated to the original reference timing (including the 0.7 s splash capture).
        // Do not let `--quick` skip past a scripted capture condition before it can be recorded.
        let timing = if skip_flow { Timing::skipped() } else if opts.quick && !opts.startup_capture { Timing::quick() } else { Timing::default() };
        let mut flow = Flow::new(timing, if skip_flow { Stage::Loading } else { Stage::Splash }).with_picker(opts.game_picker && !skip_flow);
        flow.lobby = flow.picker && !opts.startup_capture && run.is_none() && fn_live::var("FN_LOBBY").ok().filter(|v| !v.is_empty()).map_or(cfg!(target_arch = "wasm32"), |v| v == "1");
        flow.echo = true;
        let backend = AppBackend::new(path, crate::audio::open_for_app(&opts, &source)); // AUDIO HOOK: the real sink (NullAudio for --no-audio / no install / automated runs)
        let live = if opts.live_test { Some(LiveTest::default()) } else { None };
        App {
            pad: gpad::Gamepad::new(pad::open(!opts.no_gamepad && !automated)), opts, roots: Arc::new(roots), source, skip_flow, flow, overlay: Overlay::default(), work: StartupWork::none(), picker: Picker::new(), art: ArtState::default(), cap: None, stalls: Stalls::default(), quit_requested: false, tip: 0, gfx: None, platform: None, ui: None, scene: None, loader: None, early_scene: None, loader_units: 0, front_step: 0, backdrop: None, backdrop_t: 0., backdrop_job: None, lobby: None, lobby_warmed: false, lobby_shot_done: 0, name_entry: None, name_known: false, sim: GameSim::new(), input, pad_state: None, pad_look: Default::default(),
            settings, settings_note: note, backend, gp_audio: GameplayAudio::default(), phase: Phase::Boot, phase_t: 0., startup_elapsed: 0.,
            start: Instant::now(), world_time: 0.0, last: Instant::now(), limiter: FrameLimiter::new(), clock: FrameClock::default(), hud_inputs: HudInputs { player_name: display_name(), ..crate::loadout_preview::inputs() }, build_input: Default::default(), arms: None, weapons: Default::default(), console: Default::default(), ads: None, storm: Default::default(), fx: None, fx_tried: false, arms_policy: Policy::default(), run, minimized: false, window_created: false, exit_code: 0, load_log: String::new(), live_stats: FrameStats::default(), live, trace: fn_live::var_os("FN_TRACE").is_some(),
        }
    }
    pub fn set_audio(&mut self, sink: Box<dyn AudioSink>) { self.backend.audio = sink; }
    pub fn phase(&self) -> &Phase { &self.phase }

    // ------------------------------------------------------------------------------------------------------------------------------
    // start-up
    // ------------------------------------------------------------------------------------------------------------------------------
    fn init_window(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let (w, h) = if self.settings.video.width >= MIN_CLIENT.0 && self.settings.video.height >= MIN_CLIENT.1 { (self.settings.video.width, self.settings.video.height) } else { fn_settings::FALLBACK_WINDOWED };
        // the capture window has its own title: other tools that drive "the game window" by its title must not mistake an automated capture for the game
        let title = if self.opts.startup_capture { "fortnite-rs start-up capture (automated run, do not touch)" } else { TITLE };
        let attrs = Window::default_attributes().with_title(title).with_inner_size(PhysicalSize::new(w, h)).with_min_inner_size(PhysicalSize::new(MIN_CLIENT.0, MIN_CLIENT.1)).with_visible(false);
        #[cfg(target_arch = "wasm32")]
        let attrs = crate::web::window_attributes(attrs); // BROWSER PORT: the page's canvas, sized by the page
        let window = Arc::new(el.create_window(attrs).map_err(|e| format!("Could not create the window: {e}"))?);
        let mut gfx = Gfx::new(window.clone(), true)?;
        let mut platform = WinitPlatform::new(window.clone());
        // Saved window mode / size before the window is first shown (no size jump after loading), then every other setting.
        let report = self.backend.apply_all(&mut self.settings, &mut platform);
        if !report.ok() { eprintln!("[settings] apply failures: {:?}", report.failed); }
        self.settings_note.push_str(&format!("\napplied: {:?}\nfailed: {:?}\nstored-only (no runtime effect yet): {:?}", report.applied, report.failed, report.stored_only));
        if platform.vsync_dirty { gfx.set_vsync(platform.vsync); platform.vsync_dirty = false; }
        let (sw, sh) = (window.inner_size().width, window.inner_size().height);
        if sw > 0 && sh > 0 { gfx.resize(sw, sh); }
        // Automated runs (script / screenshot / smoke / start-up capture) and `--headless` keep the window hidden and never take focus (they render and capture all the
        // same); `FN_SHOW_WINDOW=1` shows an automated run's window for debugging.
        if !crate::cli::headless(&self.opts) {
            window.set_visible(true);
            window.focus_window();
        } else {
            window.set_visible(false); // winit's window-mode calls in `apply_all` (set_fullscreen / set_decorations) can show a hidden window again
        }
        let ui = UiLayer::new(&self.roots, gfx.gpu.device.clone(), gfx.gpu.queue.clone(), gfx.format(), self.overlay.clone())?;
        eprintln!("[gpu] {}  [window] {}x{} {:?}  [vsync] {}", gfx.describe(), sw, sh, self.settings.video.window_mode, gfx.vsync);
        self.input.focused = window.has_focus() || self.opts.scripted();
        self.gfx = Some(gfx); self.platform = Some(platform); self.ui = Some(ui);
        self.last = Instant::now();
        if self.opts.startup_capture {
            let size = (sw.max(1), sh.max(1));
            let out = self.opts.out.clone().unwrap_or_else(|| crate::assets::exe_dir().join("verification"));
            self.cap = Some(CaptureState { script: startup::capture::Script::standard(size, self.flow.picker), out, pending: vec![], shots: vec![], failures: vec![], started: Instant::now(), size, timeline: vec![] });
        }
        if self.skip_flow {
            crate::audio::stage_audio(self.backend.audio.as_mut(), Stage::Loading); // MUSIC HOOK: the flow starts at the level-travel loading screen
            self.phase = self.start_loading();
        } else {
            // the real start-up work (open the game files, decode the front-end art) starts now, behind the splash
            self.work = StartupWork::spawn(self.install_dir(), self.roots.clone(), self.overlay.clone(), self.flow.picker);
            self.phase = Phase::Splash;
        }
        Ok(())
    }
    /// Where the game files are for the start-up art: the `--paks` folder, else a known Paks folder / install.
    fn install_dir(&self) -> Option<PathBuf> {
        match &self.source {
            Source::Paks(d) => Some(d.clone()),
            Source::Dir => crate::assets::candidate_paks(None).into_iter().next().or_else(|| { let d = fn_ui::pak_assets::default_install(); d.exists().then_some(d) }),
        }
    }
    /// Starts the in-match load (worker threads) and enters the Loading phase.
    /// Enters the Loading phase; the world load itself started earlier with the picker flow (behind the start-up and login screens).
    fn start_loading(&mut self) -> Phase {
        if cfg!(target_arch = "wasm32") && self.run.is_none() && self.cap.is_none() && !self.skip_flow {
            if self.loader.is_none() && self.early_scene.is_none() && self.scene.is_none() && WORLD_STEP.load(std::sync::atomic::Ordering::Relaxed) >= WORLD_STEPS { WORLD_STEP.store(0, std::sync::atomic::Ordering::Relaxed); }
            self.phase_t = 0.;
            return Phase::Loading;
        }
        if WORLD_STEP.load(std::sync::atomic::Ordering::Relaxed) < WORLD_STEPS { self.finish_world_steps(); }
        if self.loader.is_none() && self.early_scene.is_none() && self.scene.is_none() { self.start_world_load(); }
        self.phase_t = 0.;
        Phase::Loading
    }
    /// Starts the in-match load (worker threads). FRONT END: with the picker flow it starts when the start-up screen appears, so the files load while the
    /// start-up, login and picker screens are up (split loading); otherwise when the Loading stage starts.
    fn start_world_load(&mut self) { WORLD_STEP.store(0, std::sync::atomic::Ordering::Relaxed); self.finish_world_steps(); }
    fn finish_world_steps(&mut self) { while self.next_world_step() {} }
    fn next_world_step(&mut self) -> bool {
        let i = WORLD_STEP.load(std::sync::atomic::Ordering::Relaxed);
        if i >= WORLD_STEPS { return false; }
        WORLD_STEP.store(i + 1, std::sync::atomic::Ordering::Relaxed);
        let t0 = Instant::now();
        match i {
            0 => {
            self.tip = if self.cap.is_some() { 0 } else { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as usize / 1000) };
            #[cfg(target_arch = "wasm32")]
            fn_pose::loader::set_cooked_texture_source(|p| fn_fx::webcook::load_raw(p).filter(|(f, _)| f.is_bc()).map(|(_, l)| l.into_iter().map(|l| (l.width, l.height, l.rgba8)).collect()));
            crate::pickups_fx::init(&self.roots.roots); // PICKUPS FX: <assets>/particles-pickup (P_Pickup / P_PickupRibbon_01 of the pickup effect Blueprint)
            }
            1 => {
            if !crate::building_render::is_ready() { if let Some(dir) = self.install_dir() { crate::building_render::init(&dir, &self.roots.roots); } } // BUILDING RENDER HOOK: the piece visuals (opens the paks; GPU resources are made on the first frame)
            }
            2 => {
            crate::backdrop_real::init(self.install_dir(), &self.roots.roots); // BACKDROP REAL (backdrop_real.rs)
            }
            3 => {
            crate::statics_render::init(self.install_dir(), &self.roots.roots); // STATIC MATERIALS: the layout props with their real cooked material shaders (statics_render.rs)
            }
            4 => {
            crate::spline_mesh::init(self.install_dir(), &self.roots.roots); // ROADS / SPLINES (spline_mesh.rs)
            }
            5 => {
            crate::landscape_render::init(self.install_dir()); // LANDSCAPE MATERIAL (landscape_render.rs)
            crate::landscape_render::prewarm(); // LOAD TIME: its packages read and its materials parsed on a worker while the match loads
            }
            6 => {
            crate::hurt_hook::init(&self.roots.roots); // HURT HOOK: <assets>/particles-hurt (damage direction lens effects)
            crate::foot_fx::init(self.install_dir(), &self.roots.roots); // FOOT FX: <assets>/particles-footfx (P_Walk_DustKickup_01 / P_Run_DustKickup_01 / P_Player_Land_Effect)
            crate::heal_fx::init(self.install_dir(), &self.roots.roots); // HEAL FX: <assets>/particles-healfx (P_Consume_Item, the consumable montages' particle notify)
            crate::enhanced_anim_load::ensure_loading(self.install_dir(), &self.roots.roots);
            crate::enhanced_sound::ensure_loading(&self.roots.roots);
            }
            7 => {
            crate::floor_loot_hook::init();
            crate::supply_drop_hook::init(self.install_dir(), &self.roots.roots);
            crate::bots_hook::init(&self.roots.roots);
            crate::chest_hook::init(self.install_dir(), &self.roots.roots); // CHEST HOOK: <assets>/particles-chests + the Athena loot tables (chest_hook.rs)
            crate::trap_hook::init(self.install_dir(), &self.roots.roots);
            }
            8 => {
            crate::bounce_hook::init(self.install_dir(), &self.roots.roots);
            crate::rocket_hook::init(self.install_dir(), &self.roots.roots);
            crate::gunfx_hook::init(self.install_dir(), &self.roots.roots);
            crate::bus_hook::init(self.install_dir());
            crate::elim_fx::init(self.install_dir(), &self.roots.roots);
            crate::muzzle_hook::init(&self.roots.roots); // MUZZLE HOOK: the firearms' muzzle flash assets (<assets>/particles-muzzle, independent of the props / harvest particles)
            }
            9 => {
            let mode = self.real_mode();
            let props = if mode.needs_real() {
                crate::props::discover(&self.roots.roots).map(|cat| {
                    let lib = std::sync::Arc::new(crate::props::Library::from_catalog(&cat, self.opts.prop_radius.unwrap_or(crate::props::default_radius_m())));
                    crate::audio::set_prop_ids(lib.protos.iter().map(|p| p.id.clone()).collect()); // CRACK AUDIO HOOK: proto -> prop id for the props.json hit / death sounds
                    crate::audio::set_ambient_world(lib.center_cm, crate::props::ANCHOR); // AMBIENCE HOOK: the level's ambient emitters are placed with the props' mapping
                    crate::particles::init(&self.roots.roots, lib.protos.iter().map(|p| cat.get(&p.id).and_then(|k| k.destruction.death_particles.clone())).collect()); // PARTICLE HOOK: proto -> DeathParticles
                    crate::particles::init_surfaces(self.install_dir(), lib.protos.iter().map(|p| cat.get(&p.id).map_or_else(Default::default, |k| (k.blueprint.clone(), k.mesh_ue_path.clone()))).collect()); // SURFACE HOOK: proto -> EPhysicalSurface (fn_hitreg::surface)
                    let kinds = lib.kinds_needed(mode);
                    crate::loader::PropsJob { cat, lib, kinds, poi: mode == crate::props::Mode::Layout }
                })
            } else { None };
            WORLD_PROPS.with(|w| *w.borrow_mut() = Some(props));
            }
            10 => {
            let props = WORLD_PROPS.with(|w| w.borrow_mut().take()).flatten();
            if let Some(j) = props.as_ref() { crate::statics_render::prewarm(j.kinds.clone()); } // LOAD TIME: the real-shader props' materials parse during the loading screen, not on the first frame
            crate::building_render::prewarm(); // LOAD TIME: the standard building pieces' materials too (no stutter at the first build-mode equip / first piece of a type)
            if let Some(g) = self.gfx.as_ref() { self.loader = Some(Loader::start(self.source.clone(), self.roots.clone(), g.gpu.device.clone(), g.gpu.queue.clone(), g.format(), props)); }
            self.loader_units = self.loader.as_ref().map_or(0, |l| l.progress().counts().1);
            }
            _ => {}
        }
        let ms = t0.elapsed().as_secs_f32() * 1000.;
        if ms > 30. { eprintln!("[load] world setup step {i}: {ms:.0} ms"); }
        i + 1 < WORLD_STEPS
    }
    /// FRONT END: the world load's main-thread steps behind a loading-type screen (budgeted); a scene that finishes early waits for the Loading stage.
    fn poll_world_early(&mut self, el: &ActiveEventLoop, budget: Duration) {
        if self.early_scene.is_some() { return; }
        let (Some(ui), Some(loader)) = (self.ui.as_mut(), self.loader.as_mut()) else { return };
        match loader.poll(ui, budget) {
            Ok(State::Done(scene)) => { self.flow.note("world load finished before the Loading stage"); self.early_scene = Some(scene); }
            Ok(State::Running) => {}
            Err(e) => self.fail(el, e),
        }
    }
    /// LOAD COUNTER: (finished, all) units of the start-up work + the world load.
    fn load_counts(&self) -> (usize, usize) {
        let (wd, wt) = self.work.counts();
        let (ld, lt) = match &self.loader { Some(l) if self.early_scene.is_none() => l.progress().counts(), _ if self.loader_units > 0 && (self.scene.is_some() || self.early_scene.is_some()) => (self.loader_units, self.loader_units), _ => (0, self.loader_units) };
        (wd + ld, wt + lt)
    }

    /// What fills the world with the real 1.8 props: `--props`, else the script's `props` line, else (the live game) the real layout.
    fn real_mode(&self) -> crate::props::Mode {
        use crate::cli::PropsChoice as C;
        use crate::props::Mode;
        match self.opts.props {
            Some(C::Real) => Mode::Layout, Some(C::Sandbox) => Mode::Off, Some(C::None) => Mode::Empty,
            None => match &self.run { Some(r) => r.driver.real_props(), None => Mode::Layout },
        }
    }
    /// Replaces the sandbox placeholders by the real props once the scene (which holds the uploaded meshes + the library) exists; without the assets the placeholders stay.
    fn populate_real_props(&mut self, scene: &Scene) {
        use crate::props::Mode;
        match self.real_mode() {
            Mode::Off => {}
            Mode::Empty => self.sim.world.props.clear(),
            m => match &scene.real {
                Some(real) => {
                    let n = real.lib.populate(&mut self.sim.world, m);
                    self.sim.stand_on_floor(); // ALL POIS: the spawn on top of the POI geometry now in the props (not inside it)
                    let drawn = real.meshes.iter().filter(|x| x.is_some()).count();
                    eprintln!("[props] {n} real props placed ({:?}; {} kinds uploaded of {} in the catalog, {} failed, {} placements within {} m of the anchor)", m, drawn, real.meshes.len(), real.skipped.len(), real.lib.placement_count(), real.lib.radius_m);
                    for note in &real.lib.notes { eprintln!("[props] note: {note}"); }
                    for (id, why) in &real.skipped { eprintln!("[props] {id} not loaded: {why} (drawn as the procedural placeholder)"); }
                    let summary = real.lib.summary(&self.sim.world.props);
                    if !summary.is_empty() { eprintln!("[props] placed:\n{summary}"); }
                }
                None => eprintln!("[props] the exported props are not available (no `props` folder with a catalog.json in the asset roots, or it is unreadable): the procedural trees / rocks stay"),
            },
        }
    }

    fn fail(&mut self, el: &ActiveEventLoop, message: String) {
        eprintln!("error: {message}");
        self.exit_code = 1;
        if let Some(cap) = self.cap.as_mut() { cap.failures.push(message.clone()); self.finish_capture(el); return; }
        if self.run.is_some() || self.gfx.is_none() { el.exit(); } else { self.phase = Phase::Failed(message); }
    }

    // ------------------------------------------------------------------------------------------------------------------------------
    // mouse capture / menu
    // ------------------------------------------------------------------------------------------------------------------------------
    fn menu_open(&self) -> bool { self.ui.as_ref().map_or(false, |u| u.menu.is_open()) }
    /// Capture the mouse when policy allows (focused, gameplay started, not scripted).
    fn try_capture(&mut self) {
        if self.input.captured || !self.input.may_capture() || self.minimized { return; }
        let Some(p) = self.platform.as_mut() else { return };
        if !p.window_focused() { return; }
        p.set_cursor_captured(true);
        if p.cursor_captured() { self.input.mark_captured(); }
    }
    fn release_mouse(&mut self) {
        if let Some(p) = self.platform.as_mut() { if p.cursor_captured() { p.set_cursor_captured(false); } }
        self.input.mark_released();
    }
    fn open_menu(&mut self) {
        let Some(ui) = self.ui.as_mut() else { return };
        if ui.menu.is_open() || self.phase != Phase::Playing { return; }
        ui.menu.open();
        self.backend.audio.event(SoundEvent::MenuOpen);
        self.release_mouse();
        gpad::menu_opened();
        let (x, y) = self.input.cursor;
        if let Some(ui) = self.ui.as_mut() { ui.menu.event(UiEvent::MouseMove(x, y)); }
    }
    fn handle_menu_requests(&mut self, el: &ActiveEventLoop, reqs: Vec<HostRequest>) {
        for r in reqs {
            match r {
                HostRequest::Closed => { self.backend.audio.event(SoundEvent::MenuClose); self.input.user_released = false; self.try_capture(); }
                HostRequest::QuitToDesktop => { self.release_mouse(); el.exit(); }
            }
        }
    }

    // ------------------------------------------------------------------------------------------------------------------------------
    // the frame
    // ------------------------------------------------------------------------------------------------------------------------------
    fn frame(&mut self, el: &ActiveEventLoop) {
        let mut now = Instant::now();
        let mut raw_dt = now.duration_since(self.last).as_secs_f32();
        if self.cap.is_some() && raw_dt < CAPTURE_DT {
            // the capture paces itself at the real 60 Hz of its fake clock, so background work (art decode, the load) takes the same share of the timeline as in a real run
            std::thread::sleep(Duration::from_secs_f32(CAPTURE_DT - raw_dt));
            now = Instant::now(); raw_dt = now.duration_since(self.last).as_secs_f32();
        }
        self.last = now;
        fn_live::poll();
        let minimized =self.platform.as_ref().map_or(false, |p| p.window.is_minimized() == Some(true)) || self.gfx.as_ref().map_or(true, |g| g.size().0 == 0);
        if minimized != self.minimized { self.minimized = minimized; if minimized { self.release_mouse(); } }
        if self.minimized { return; }
        if self.quit_requested { self.release_mouse(); el.exit(); return; }
        // the start-up capture runs on a fake 60 Hz clock; everything else on the real frame time
        let dt = if self.cap.is_some() { CAPTURE_DT } else { raw_dt };
        if matches!(self.phase, Phase::StartupLoading | Phase::LoggingIn | Phase::GamePicker | Phase::Lobby | Phase::Loading | Phase::Finishing) { let name = self.flow.stage.name(); self.stalls.record(name, raw_dt * 1000.); }
        if self.cap.is_some() { self.capture_poll(el, dt); if self.cap.is_none() { return; } }
        let pre_t = Instant::now(); let pre_name = (!matches!(self.phase, Phase::Playing)).then(|| self.flow.stage.name());
        #[cfg(target_arch = "wasm32")] let c0 = web_quality::pre_counters();
        #[cfg(not(target_arch = "wasm32"))] let c0 = (0f32, 0f32, 0f32);
        self.phase_frame(el, dt, now);
        #[cfg(target_arch = "wasm32")] let c1 = web_quality::pre_counters();
        #[cfg(not(target_arch = "wasm32"))] let c1 = (0f32, 0f32, 0f32);
        let secs = crate::platform::take_sections();
        if let Some(name) = pre_name { let ms = pre_t.elapsed().as_secs_f32() * 1000.; if ms > 40. { let mut s = secs; s.sort_by(|a, b| b.1.total_cmp(&a.1)); eprintln!("[hitchtag] {name} cpu {ms:.0}: [{}] lock {:.0} filewait {:.0} grow {:.0}", s.iter().take(8).map(|(k, v)| format!("{k} {v:.0}")).collect::<Vec<_>>().join(", "), c1.0 - c0.0, c1.1 - c0.1, c1.2 - c0.2); } }
    }

    fn phase_frame(&mut self, el: &ActiveEventLoop, dt: f32, now: Instant) {
        match self.phase.clone() {
            Phase::Boot => {}
            Phase::Splash => self.splash_frame(dt),
            Phase::StartupLoading => self.startup_frame(el, dt),
            Phase::LoggingIn => self.login_frame(el, dt),
            Phase::GamePicker => self.picker_frame(dt),
            Phase::Lobby => self.lobby_frame(el, dt),
            Phase::Loading => self.loading_frame(el, dt),
            Phase::Finishing => self.finishing_frame(dt),
            Phase::Playing => self.play_frame(el, dt, now),
            Phase::Failed(msg) => self.error_frame(&msg),
        }
    }

    /// Acquire, let `build` fill the UI list and draw it over black / the cleared target, present. Captures requested by the start-up capture script are taken from the
    /// same list (offscreen, same format).
    fn simple_frame(&mut self, build: impl FnOnce(&mut UiLayer, f32, f32)) {
        let want = self.cap.as_mut().map(|c| std::mem::take(&mut c.pending)).unwrap_or_default();
        let (Some(gfx), Some(ui)) = (self.gfx.as_mut(), self.ui.as_mut()) else { return };
        let (w, h) = gfx.size();
        ui.begin();
        crate::platform::timed("ui build", || build(ui, w as f32, h as f32));
        if let Acquire::Frame(f) = crate::platform::timed("acquire", || gfx.acquire()) {
            let view = f.texture.create_view(&Default::default());
            crate::platform::timed("ui render", || ui.render(&view, w, h, Some([0., 0., 0., 1.])));
            crate::platform::timed("present", || gfx.present(f));
        }
        if !want.is_empty() {
            let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
            ui.render(&target.view, w, h, Some([0., 0., 0., 1.]));
            let px = target.read_rgba8(&gfx.gpu);
            self.save_captures(want, w, h, &px);
        }
    }

    /// FRONT END BACKDROP: builds the GPU half once the worker delivered the scene data. True while it is still loading.
    fn poll_backdrop(&mut self) -> bool {
        let Some(rx) = self.backdrop_job.as_ref() else { return false };
        match rx.try_recv() {
            Ok(r) => {
                self.backdrop_job = None;
                let at = Instant::now();
                match r.and_then(|a| match self.gfx.as_ref() { Some(g) => crate::frontend_bg::FrontendBackdrop::from_assets(a, &g.gpu.device, &g.gpu.queue, g.format()), None => Err("no window".into()) }) {
                    Ok(b) => { for n in &b.notes { eprintln!("[front-end bg] {n}"); } self.backdrop = Some(b); }
                    Err(e) => eprintln!("[front-end bg] unavailable: {e}"),
                }
                self.flow.note(format!("front-end backdrop ready ({:.0} ms on the render thread)", at.elapsed().as_secs_f32() * 1000.));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => { self.backdrop_job = None; false }
        }
    }

    /// FRONT END BACKDROP: like [`Self::simple_frame`], with the 3D front-end level rendered under the UI (black without it).
    fn frontend_frame(&mut self, dt: f32, build: impl FnOnce(&mut UiLayer, f32, f32)) {
        if self.backdrop.is_none() { return self.simple_frame(build); }
        self.backdrop_t += dt;
        let t = self.backdrop_t;
        let want = self.cap.as_mut().map(|c| std::mem::take(&mut c.pending)).unwrap_or_default();
        let (Some(gfx), Some(ui), Some(bg)) = (self.gfx.as_mut(), self.ui.as_mut(), self.backdrop.as_mut()) else { return };
        let (w, h) = gfx.size();
        ui.begin();
        crate::platform::timed("ui build", || build(ui, w as f32, h as f32));
        if let Acquire::Frame(f) = crate::platform::timed("acquire", || gfx.acquire()) {
            let view = f.texture.create_view(&Default::default());
            crate::platform::timed("backdrop render", || bg.render(&view, w, h, t, dt));
            crate::platform::timed("ui render", || ui.render(&view, w, h, None));
            crate::platform::timed("present", || gfx.present(f));
        }
        if !want.is_empty() {
            let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
            bg.render(&target.view, w, h, t, 0.);
            ui.render(&target.view, w, h, None);
            let px = target.read_rgba8(&gfx.gpu);
            self.save_captures(want, w, h, &px);
        }
    }

    /// Writes the requested captures (`NN-name-WxH.png`) and records them in the timeline.
    fn save_captures(&mut self, names: Vec<String>, w: u32, h: u32, px: &[u8]) {
        self.stalls.skip_next = true; // the encode / read-back time is the capture's, not a stall of the screen
        let Some(cap) = self.cap.as_mut() else { return };
        let notes: Vec<String> = names.iter().map(|n| write_capture(cap, n, w, h, px, self.flow.now)).collect();
        for n in notes { self.flow.note(n); }
    }

    /// The loading screen, through the HUD contract (`HudInputs::loading` -> `Hud::present`). `fraction` 0..1 drives the bar and the percent.
    fn loading_page(&mut self, fraction: f32, label: &str, fade: f32) {
        const UNITS: usize = 1000;
        let done = ((fraction.clamp(0., 1.) * UNITS as f32).floor() as usize).min(UNITS);
        let inputs = HudInputs { loading: Some(HudLoading { done, total: UNITS, label: label.to_owned(), tip: self.tip, fade }), ..Default::default() };
        let st = hud_state(&self.sim, &self.settings, &inputs);
        let (cd, ct) = self.load_counts();
        self.simple_frame(move |ui, w, h| { ui.present_hud(&st, w, h, None); ui.load_counter(w, h, cd, ct); });
    }

    /// Applies a stage the flow just entered: phase, side effects (start the load), log.
    fn entered(&mut self, stage: Option<Stage>) {
        let Some(s) = stage else { return };
        #[cfg(target_arch = "wasm32")]
        crate::web::stage(s.name()); // BROWSER PORT: the page's loading indicator follows the flow
        crate::audio::stage_audio(self.backend.audio.as_mut(), s); // MUSIC HOOK: loading-screen mix / front-end music per flow stage
        self.phase_t = 0.;
        match s {
            Stage::Loading => { crate::bots_hook::reset(); self.release_front_end_art("loading"); self.backdrop = None; self.backdrop_job = None; if let Some(l) = self.lobby.as_mut() { l.flush(); if cfg!(target_arch = "wasm32") { l.release_gpu(); } } if self.lobby.take().is_some() { crate::lobby_scene::clear_hero_mips(); crate::lobby_scene::clear_hero_views(); } self.phase = self.start_loading(); }
            Stage::StartupLoading => {
                self.work.begin_front_end();
                if self.flow.picker && self.loader.is_none() && !menu_first() { self.start_world_load(); } // FRONT END: split loading (see start_world_load)
                self.phase = Phase::StartupLoading;
            }
            Stage::GamePicker => { fn_ui::menu::pages::visibility::set_in_zone(false); fn_pak::read_log_mark("picker"); self.picker = Picker::new(); self.phase = Phase::GamePicker; if menu_first() && self.loader.is_none() && self.early_scene.is_none() && self.scene.is_none() && WORLD_STEP.load(std::sync::atomic::Ordering::Relaxed) == usize::MAX { WORLD_STEP.store(0, std::sync::atomic::Ordering::Relaxed); } }
            Stage::Lobby => { self.phase = Phase::Lobby; self.release_mouse(); }
            Stage::Playing => {
                fn_ui::menu::pages::visibility::set_in_zone(true);
                self.release_front_end_art("play");
                self.phase = self.begin_play();
            }
            other => { self.phase = Phase::of(other); }
        }
    }

    fn release_front_end_art(&mut self, at: &str) {
        if !cfg!(target_arch = "wasm32") { return; }
        let o = self.overlay.release_large(1 << 20);
        let lm = self.lobby.as_mut().map_or(0, |l| l.release_ui_materials());
        if let Some((n, b)) = self.ui.as_mut().and_then(|u| u.front.as_mut()).map(|f| f.release_materials()) { if n > 0 { eprintln!("[mem] front-end UI materials released at {at}: {n} ({:.0} MB of material textures)", b as f64 / 1048576.); } }
        if lm > 0 { eprintln!("[mem] lobby UI materials released at {at}: {lm}"); }
        if let Some(ui) = self.ui.as_mut() { let keep = ui.hud.extra.kept_textures(); let (n, b) = ui.ctx.assets.release_large_keep(1 << 20, &keep); eprintln!("[mem] front-end art released at {at}: {n} UI textures ({:.0} MB), {o} overlay images", b as f64 / 1048576.); }
    }

    fn splash_frame(&mut self, dt: f32) {
        self.simple_frame(|ui, w, h| ui.splash(w, h, 1.));
        let s = self.flow.tick(dt);
        crate::platform::timed("entered", || self.entered(s));
    }

    /// Start-up loading screen: the key art is shown once the worker delivered it AND it was uploaded (one texture per frame, so showing it costs no hitch).
    fn startup_frame(&mut self, el: &ActiveEventLoop, dt: f32) {
        crate::platform::timed("upload_art", || self.upload_art());
        let (t, fade, ready, progress) = (self.flow.stage_t, self.flow.startup_fade(), self.art.startup_uploaded, self.work.fraction());
        let (done, total) = self.load_counts();
        self.simple_frame(move |ui, w, h| { ui.startup_loading(w, h, t, progress, ready, fade); ui.load_counter(w, h, done, total); });
        // FRONT END: compose the cooked login screen + picker once the start-up screen is up (UI thread, one-shot), then the world load's main-thread steps
        if self.flow.picker && self.front_step == 0 && t > 0. {
            let at = Instant::now();
            let name = display_name();
            let note = match (self.install_dir(), self.ui.as_mut()) { (Some(dir), Some(ui)) => crate::platform::timed("load front end", || ui.load_front_end(&dir, &name)), _ => Some("no game files".into()) };
            self.flow.note(format!("front end composed in {:.0} ms{}", at.elapsed().as_secs_f32() * 1000., note.map(|n| format!(" (fallback: {n})")).unwrap_or_default()));
            // FRONT END BACKDROP: the front-end level's scene data on a worker (frontend_bg.rs); its GPU half follows in poll_backdrop
            if let Some(dir) = self.install_dir() {
                let (tx, rx) = std::sync::mpsc::channel();
                let _ = std::thread::Builder::new().name("front-end backdrop".into()).spawn(move || { let _ = tx.send(crate::frontend_bg::FrontendBackdrop::load_assets(&dir)); });
                self.backdrop_job = Some(rx);
            }
            crate::lobby_world::set_shadow_quality(self.settings.video.shadows as u8);
            if self.flow.lobby { if let Some(dir) = self.install_dir() { self.lobby = Some(crate::lobby_host::LobbyHost::start(&dir, name.clone())); } }
            self.front_step = 1;
        }
        if self.flow.picker { crate::platform::timed("poll_world_early", || self.poll_world_early(el, Duration::from_millis(8))); }
        if self.work.done() && (self.art.picker_uploaded || !self.flow.picker) && (self.front_step >= 1 || !self.flow.picker) { self.flow.set_work_done(); }
        // FRONT END: the art-only hold = the world load's worker units (paks, loadout, models, clips)
        let bg_loading = crate::platform::timed("poll_backdrop", || self.poll_backdrop());
        if self.flow.work_done() && !bg_loading && (menu_first() || self.early_scene.is_some() || self.loader.as_ref().map_or(true, |l| l.login_share_done())) { self.flow.set_art_hold_done(); }
        let s = self.flow.tick(dt);
        crate::platform::timed("entered", || self.entered(s));
    }

    /// One art texture per call: first the start-up screen's, then the picker's.
    fn upload_art(&mut self) {
        let Some(ui) = self.ui.as_mut() else { return };
        if !self.art.queued.0 && self.work.startup_art_ready() {
            self.art.queued.0 = true;
            self.art.startup_queue = fn_ui::screens::startup_loading::PAK_TEXTURES.to_vec(); self.art.startup_queue.reverse();
        }
        if !self.flow.picker { self.art.picker_uploaded = true; }
        if self.flow.picker && !self.art.queued.1 && self.work.front_end_art_ready() {
            self.art.queued.1 = true;
            self.art.picker_queue = picker::PAK_TEXTURES.iter().copied().filter(|n| !fn_ui::screens::startup_loading::PAK_TEXTURES.contains(n)).collect(); self.art.picker_queue.reverse();
        }
        if let Some(n) = self.art.startup_queue.pop() {
            let t = Instant::now(); let ok = ui.prewarm_texture(n);
            self.flow.note(format!("art uploaded: {n} ({}, {:.1} ms)", if ok { "ok" } else { "missing" }, t.elapsed().as_secs_f32() * 1000.));
        } else if self.art.queued.0 && !self.art.startup_uploaded { self.art.startup_uploaded = true; }
        else if let Some(n) = self.art.picker_queue.pop() {
            let t = Instant::now(); let ok = ui.prewarm_texture(n);
            self.flow.note(format!("art uploaded: {n} ({}, {:.1} ms)", if ok { "ok" } else { "missing" }, t.elapsed().as_secs_f32() * 1000.));
        } else if self.art.queued.1 && !self.art.picker_uploaded { self.art.picker_uploaded = true; }
    }

    /// FRONT END: the cooked "Logging In..." screen. Its share of the load: the world load's worker units (`Loader::login_share_done`) and the picker's
    /// background raster; then the game picker.
    fn apply_display_name(&mut self, name: &str) {
        if let Err(e) = crate::lobby_profile::store_display_name(&crate::lobby_profile::default_profile_path(), name) { eprintln!("[login] cannot store the display name: {e}"); }
        crate::name_store::save(name);
        crate::chat_hook::on_name_applied(name);
        self.hud_inputs.player_name = name.to_owned();
        if let Some(ui) = self.ui.as_mut() { ui.set_display_name(name); }
        if let Some(l) = self.lobby.as_mut() { l.set_name(name); }
        self.flow.note(format!("login: display name {name:?}"));
    }

    fn poll_lobby(&mut self) {
        let (Some(lobby), Some(g)) = (self.lobby.as_mut(), self.gfx.as_ref()) else { return };
        if lobby.ready() {
            if !self.lobby_warmed { let (w, h) = g.size(); self.lobby_warmed = crate::platform::timed("lobby warm", || lobby.warm(w, h)); }
            return;
        }
        let at = Instant::now();
        lobby.poll_load(&g.gpu.device, &g.gpu.queue, g.format());
        let ms = at.elapsed().as_secs_f32() * 1000.;
        if ms > 30. { self.flow.note(format!("lobby load step on the UI thread: {ms:.0} ms")); }
        if let Some(e) = lobby.failed.take() { eprintln!("[lobby] unavailable: {e}"); self.flow.note(format!("lobby unavailable: {e}")); self.lobby = None; self.flow.lobby = false; }
    }

    fn login_frame(&mut self, el: &ActiveEventLoop, dt: f32) {
        crate::platform::timed("upload_art", || self.upload_art());
        crate::platform::timed("poll_backdrop", || self.poll_backdrop());
        let lobby_t = Instant::now();
        crate::platform::timed("poll_lobby", || self.poll_lobby());
        let lobby_busy = lobby_t.elapsed().as_secs_f32() * 1000. > 20.;
        let (t, ready) = (self.flow.stage_t, self.art.startup_uploaded);
        let (done, total) = self.load_counts();
        // GetLoginStatusText: the running login step, stepped by the rest of the world load
        let frac = if self.early_scene.is_some() { 1. } else { self.loader.as_ref().map_or(1., |l| l.fraction()) };
        let n = fn_ui::screens::frontend_cooked::LOGIN_STEPS.len();
        let status = ((frac * n as f32) as usize).min(n - 1);
        // the start-up screen's art layer fades out over the login screen with the exe's 0.2 s curve (`startup_loading` module notes)
        let art_alpha = 1. - (t / startup::STARTUP_FADE_SECONDS.max(1e-3)).clamp(0., 1.);
        if self.name_entry.is_none() && fn_live::var("FN_ASK_NAME").map_or(false, |v| v == "1") && crate::name_store::saved().is_none() && self.flow.lobby && self.cap.is_none() && self.front_step >= 1 && fn_live::var("FN_DISPLAY_NAME").map_or(true, |v| v.is_empty())
            && crate::lobby_profile::stored_display_name(&crate::lobby_profile::default_profile_path()).is_none() && self.ui.as_ref().map_or(false, |u| u.has_sign_in()) && !self.name_known {
            self.name_entry = Some(crate::login_name::NameEntry::new());
        }
        if let Some(e) = self.name_entry.as_mut() { e.st.t = t; }
        if let Some(st) = self.name_entry.as_ref().map(|e| e.st.clone()) {
            self.frontend_frame(dt, move |ui, w, h| { if !ui.sign_in(w, h, &st) { ui.login(w, h, t, ready, status, 0.); } });
        } else {
            self.frontend_frame(dt, move |ui, w, h| { ui.login(w, h, t, ready, status, art_alpha); ui.load_counter(w, h, done, total); });
        }
        if let Some(name) = self.name_entry.as_ref().and_then(|e| e.done.clone()) {
            self.name_entry = None;
            self.name_known = true;
            self.apply_display_name(&name);
        }
        if self.front_step < 2 && t > 0. && !lobby_busy {
            let at = Instant::now();
            static STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let k = STEP.load(std::sync::atomic::Ordering::Relaxed);
            let r = match (self.gfx.as_ref(), self.ui.as_mut()) { (Some(g), Some(ui)) => { let (w, h) = g.size(); crate::platform::timed("picker raster", || ui.warm_picker_step(w as f32, h as f32, k)) } _ => Some(true) };
            if r.is_some() { STEP.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
            let done = r.unwrap_or(false);
            self.flow.note(format!("picker background step {k} rastered in {:.0} ms", at.elapsed().as_secs_f32() * 1000.));
            if done { self.front_step = 2; }
        }
        crate::platform::timed("poll_world_early", || self.poll_world_early(el, Duration::from_millis(8)));
        // its share: the rest of the world load (renderer, uploads, UI warm-up), so the level-travel screen is short
        let share = menu_first() || self.early_scene.is_some() || self.loader.is_none();
        if share && self.front_step >= 2 && t >= startup::STARTUP_FADE_SECONDS && self.name_entry.is_none() { self.flow.set_login_done(); }
        let s = self.flow.tick(dt);
        crate::platform::timed("entered", || self.entered(s));
    }

    fn picker_frame(&mut self, dt: f32) {
        if !self.flow.lobby && WORLD_STEP.load(std::sync::atomic::Ordering::Relaxed) < WORLD_STEPS { self.next_world_step(); }
        // gamepad navigation (edge + repeat)
        if self.input.focused && self.cap.is_none() {
            self.pad_state = self.pad.poll(dt);
            let evs = self.ui.as_mut().map(|u| u.pad_events(self.pad_state.as_ref(), dt)).unwrap_or_default();
            for e in evs { self.picker_event(e); }
        }
        self.picker.tick(dt);
        crate::platform::timed("poll_lobby", || self.poll_lobby());
        if self.flow.lobby && self.cap.is_none() && fn_live::var_os("FN_LOBBY_SHOTS").is_some() && self.flow.pick_battle_royale() { let s = self.flow.stage; self.entered(Some(s)); return; }
        if crate::mp_hook::take_autoplay() && self.phase == Phase::GamePicker { self.flow.lobby = false; if self.flow.pick_battle_royale() { self.entered(Some(Stage::Loading)); } }
        let (p, ready) = (self.picker.clone(), self.art.picker_uploaded);
        let (done, total) = self.load_counts();
        // LOAD COUNTER: while the world keeps loading behind the picker (worker threads only: the picker is interactive)
        self.frontend_frame(dt, move |ui, w, h| { ui.picker(&p, w, h, ready); if done < total { ui.load_counter(w, h, done, total); } });
        // LOBBY PREPARE (2026-10-07): once the world data has loaded behind the front end, the match's first-use work (the POI groups around the spawn,
        // the building pieces, the pickup effects) runs while the player is in the lobby, a few ms per frame (the lobby stays smooth); the loading screen
        // after Play only finishes what is left (`finishing_frame`).
        if self.cap.is_none() && self.run.is_none() { if let Some(scene) = self.early_scene.as_mut() {
            crate::statics_render::set_view_distance_scale(self.backend.render.options.view_distance_scale);
            let _ = crate::statics_render::poi_prebuild_step(scene.renderer.fx(), 4.) && crate::building_render::prebuild_step(scene.renderer.fx(), 4.) && crate::pickups_fx::prebuild_step(&mut scene.renderer, 4.) && scene.renderer.fx().pso_warm_step(4.);
            crate::rocket_hook::start_prefetch();
        } }
        let s = self.flow.tick(dt);
        crate::platform::timed("entered", || self.entered(s));
    }

    fn lobby_frame(&mut self, _el: &ActiveEventLoop, dt: f32) {
        crate::platform::timed("poll_lobby", || self.poll_lobby());
        if crate::mp_hook::take_autoplay() && self.flow.lobby_play() { self.entered(Some(Stage::Loading)); return; }
        let load_fraction = 1.;
        if self.lobby.is_none() {
            if self.flow.lobby_play() { self.entered(Some(Stage::Loading)); }
            return;
        }
        if !self.lobby.as_ref().map_or(false, |l| l.ready()) {
            let t = self.flow.stage_t;
            self.simple_frame(move |ui, w, h| ui.startup_loading(w, h, t, 0.0, true, 0.));
            let s = self.flow.tick(dt); crate::platform::timed("entered", || self.entered(s));
            return;
        }
        let mut out = crate::lobby_host::LobbyOut::None;
        if let (Some(gfx), Some(lobby)) = (self.gfx.as_mut(), self.lobby.as_mut()) {
            let (w, h) = gfx.size();
            let (device, queue, format) = (gfx.gpu.device.clone(), gfx.gpu.queue.clone(), gfx.format());
            if let Acquire::Frame(f) = gfx.acquire() {
                out = crate::platform::timed("lobby frame", || lobby.frame(&device, &queue, format, &f.texture, w, h, dt, load_fraction));
                gfx.present(f);
            }
            for e in lobby.take_sounds() { eprintln!("[lobby] ui sound {e:?}"); self.backend.audio.event(e); }
        }
        if let Some(dir) = fn_live::var_os("FN_LOBBY_SHOTS") { self.lobby_shots(std::path::PathBuf::from(dir)); }
        if out == crate::lobby_host::LobbyOut::MatchReady {
            if let Some(l) = self.lobby.as_mut() { crate::platform::timed("lobby flush", || l.flush()); }
            if self.flow.lobby_play() { crate::platform::timed("enter loading", || self.entered(Some(Stage::Loading))); return; }
        }
        let s = self.flow.tick(dt);
        crate::platform::timed("entered", || self.entered(s));
    }

    fn lobby_shots(&mut self, dir: PathBuf) {
        let t = self.flow.stage_t;
        let plan: [(f32, &str); 4] = [(12.0, "lobby-idle"), (20.0, "play"), (21.7, "lobby-mm"), (24.0, "lobby-mm2")];
        let Some(i) = plan.iter().position(|(at, _)| t >= *at && self.lobby_shot_done < plan.len() && plan[self.lobby_shot_done].0 == *at) else { return };
        self.lobby_shot_done = i + 1;
        let name = plan[i].1;
        let (Some(gfx), Some(lobby)) = (self.gfx.as_ref(), self.lobby.as_mut()) else { return };
        if name == "play" { lobby.debug_play(); return; }
        if cfg!(target_arch = "wasm32") { eprintln!("[lobby] capture point {name}"); return; }
        let (w, h) = (1920, 1080);
        let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
        let _ = lobby.frame(&gfx.gpu.device, &gfx.gpu.queue, gfx.format(), &target.texture, w, h, 0.0, 1.0);
        let px = target.read_rgba8(&gfx.gpu);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}-{w}x{h}.png"));
        match run::save_png(&path, w, h, &px) { Ok(()) => eprintln!("[lobby] capture {}", path.display()), Err(e) => eprintln!("[lobby] capture failed: {e}") }
    }

    /// One input event for the Battle Royale picker (pointer positions in window pixels).
    fn picker_event(&mut self, ev: UiEvent) {
        if self.phase != Phase::GamePicker { return; }
        let (Some(gfx), Some(ui)) = (self.gfx.as_ref(), self.ui.as_ref()) else { return };
        let (w, h) = gfx.size();
        let l = ui.picker_layout(self.picker.page, w as f32, h as f32);
        let (focus, page) = (self.picker.focus, self.picker.page);
        let out = self.picker.event(ev, &l);
        if self.picker.focus != focus || self.picker.page != page { self.flow.note(format!("picker: focus {:?} ({:?} page)", self.picker.focus, self.picker.page)); }
        match out {
            PickerOutcome::StartBattleRoyale => { if self.flow.pick_battle_royale() { let s = self.flow.stage; self.entered(Some(s)); } }
            PickerOutcome::Quit => { self.flow.note("picker: Quit chosen"); self.quit_requested = true; }
            PickerOutcome::None => {}
        }
    }

    fn loading_frame(&mut self, el: &ActiveEventLoop, dt: f32) {
        self.phase_t += dt;
        #[cfg(target_arch = "wasm32")]
        crate::web::heap_reserve_step();
        if WORLD_STEP.load(std::sync::atomic::Ordering::Relaxed) < WORLD_STEPS {
            let i = WORLD_STEP.load(std::sync::atomic::Ordering::Relaxed);
            crate::platform::timed(["world step 0", "world step 1", "world step 2", "world step 3", "world step 4", "world step 5", "world step 6", "world step 7", "world step 8", "world step 9", "world step 10"].get(i).copied().unwrap_or("world step"), || self.next_world_step());
            self.loading_page(0., "Starting", 0.);
            let s = self.flow.tick(dt); self.entered(s);
            return;
        }
        let (Some(ui), Some(loader)) = (self.ui.as_mut(), self.loader.as_mut()) else {
            // load finished earlier: keep showing 100 % until the flow leaves the stage
            self.loading_page(1., "Starting", 0.);
            let s = self.flow.tick(dt); crate::platform::timed("entered", || self.entered(s));
            return;
        };
        static POST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        let poll_t = Instant::now();
        let state = match self.early_scene.take() { Some(s) => Ok(State::Done(s)), None => crate::platform::timed("loader poll", || loader.poll(ui, Duration::from_millis(12))) };
        let (fraction, label) = (loader.fraction(), loader.label());
        match state {
            Err(e) => { self.fail(el, e); return; }
            Ok(State::Done(scene)) => 'post: {
                let post = POST.load(std::sync::atomic::Ordering::Relaxed);
                if post == 0 && self.run.is_none() && poll_t.elapsed() > Duration::from_millis(20) { POST.store(1, std::sync::atomic::Ordering::Relaxed); self.early_scene = Some(scene); break 'post; }
                let t_done = Instant::now(); let mut marks: Vec<(&str, f32)> = vec![]; // LOAD TIME: the post-load setup runs on the UI thread with the screen frozen at 100 %
                let mut scene = scene;
                if post < 2 {
                if let Some(loadout) = &scene.loadout {
                    if let Some(ui) = self.ui.as_mut() { ui.inv.set_catalog(loadout.inventory.clone()); } // WIDGETS3 HOOK: inventory screen item texts
                    let mut items = std::array::from_fn(|_| None);
                    items[0] = Some(fn_ui::screens::hud::HudItem::pickaxe());
                    for (i, item) in loadout.items.iter().enumerate() {
                        items[i + 1] = Some(fn_ui::screens::hud::HudItem::new(item.rarity, &item.icon, (item.quantity > 1).then_some(item.quantity)));
                    }
                    self.hud_inputs.items = Some(items);
                    self.hud_inputs.backpack = Some((loadout.items.len() as u32, 5));
                    // HUD art straight from the packages (item icons, ammo icons): into the UI's asset overlay under the names the widgets ask for
                    for (name, w, h, rgba) in &loadout.ui_images { self.overlay.insert(name, fn_ui::assets::Image { w: *w, h: *h, rgba: rgba.clone() }); }
                    match ArmsData::from_resources(loadout) {
                        Ok(d) => { eprintln!("[arms] quickbar: 1 pickaxe, 2 {}, 3 {}, 4 {}, 5 bandage x{}", d.guns[0].def.stat_row, d.guns[1].def.stat_row, d.guns[2].def.stat_row, d.start_bandages); self.arms_policy.assume_unrecovered |= self.opts.assume_unrecovered || self.run.as_ref().map_or(false, |r| r.driver.assume_unrecovered()); if self.arms_policy.assume_unrecovered { for a in crate::arms::ASSUMPTIONS { eprintln!("[arms] ASSUMED (dev switch on): {a}"); } } else { for m in crate::arms::MISSING_BY_DEFAULT { eprintln!("[arms] MISSING (not recovered): {m}"); } } let dev = self.opts.dev_loadout || fn_live::var("FN_DEV_LOADOUT").map_or(false, |v| v == "1") || self.run.as_ref().map_or(false, |r| !r.driver.br_loadout()); let arms = if dev { eprintln!("[inventory] DEV loadout (--dev-loadout / FN_DEV_LOADOUT=1 / scripted run): AR, pump, sniper, bandages"); Arms::new(d, self.arms_policy) } else { eprintln!("[inventory] 1.8 BR spawn inventory: pickaxe only, 0 materials, no ammo (Athena_GameMode StartingItems)"); Arms::new_br(d, self.arms_policy) }; /* INVENTORY HOOK */ if let Some(ui) = self.ui.as_mut() { ui.inv.sync_quickbar(&arms.quickbar); } self.arms = Some(arms); }
                        Err(e) => eprintln!("[arms] unavailable: {e}"),
                    }
                    self.ads = crate::ads::AdsDriver::from_resources(loadout); // ADS HOOK: aim-down-sights profiles of the three firearms
                    crate::emote_hook::init(loadout);
                }
                marks.push(("arms + ads", t_done.elapsed().as_secs_f32() * 1000.));
                self.load_log = loader.timings_text();
                crate::hitreg_hook::set_install(self.install_dir()); // HITREG HOOK
                let install = self.install_dir(); crate::pickups_hook::init(install.clone()); self.weapons.set_install(install); // WEAPON RUNTIME HOOK (+ PICKUPS HOOK: the item catalog scan)
                if let (Some(dir), Some(arms), Some(ui)) = (self.install_dir(), self.arms.as_ref(), self.ui.as_mut()) { ui.inv.panel.set_install(dir.clone()); ui.hud.pickup.set_install(dir.clone()); /* PICKUP CARD: the cooked InteractionIndicator */ for n in ui.inv.load_extras(&dir, arms) { eprintln!("{n}"); } } // INVENTORY HOOK: consumable / ammo item texts + icons, the panel's background material
                marks.push(("hooks + inventory extras", t_done.elapsed().as_secs_f32() * 1000.));
                // SCOPE HOOK: the post-process material + mesh of the first scoped weapon (data: its ADS profile's `ScopeData`)
                if let (Some(dir), Some(sd)) = (self.install_dir(), self.ads.as_ref().and_then(|a| a.profiles.iter().find_map(|p| p.scope.clone()))) { crate::scope_hook::install(&mut scene.renderer, &dir, &sd); }
                if let Some(dir) = self.install_dir() { crate::outline_hook::start(&dir); } // OUTLINE HOOK: decode the outline post-process material
                if let Some(dir) = self.install_dir() { crate::decals_hook::start(&dir); }
                marks.push(("scope", t_done.elapsed().as_secs_f32() * 1000.));
                if self.run.is_none() { eprintln!("[load] post-load setup (UI thread), part 1: {}", marks.iter().map(|(n, ms)| format!("{n} -> {ms:.0} ms")).collect::<Vec<_>>().join(", ")); POST.store(2, std::sync::atomic::Ordering::Relaxed); self.early_scene = Some(scene); break 'post; }
                }
                POST.store(0, std::sync::atomic::Ordering::Relaxed);
                let stage_note = format!("load steps:\n{}", self.load_log);
                // GUN MATERIAL HOOK: the held firearms run their cooked WeaponMat_Base material through the gun pass by default (as 1.8; the gun pass now casts CSM shadows and
                // gets the DFAO + sky-light specular, docs/spec/pickup_visual_gaps.md section 2). If a material cannot be built the renderer Prop model is the logged
                // fallback; `--standin-gun-material` forces that model.
                let orig_gun_ok = match self.install_dir() {
                    Some(dir) if self.opts.original_gun_material && scene.loadout.is_some() =>match scene.install_original_guns(&dir) {
                        Ok(()) => { eprintln!("[arms] ORIGINAL gun material ON: the base-pass vertex + pixel programs of the cooked WeaponMat_Base shader map (SM5, TGPUSkinVertexFactory, FNoLightMapPolicy + skylight), original instance parameters and textures"); true }
                        Err(e) => { eprintln!("[arms] original gun material NOT available, FALLBACK to the renderer Prop model (original meshes + Diffuse / Normals): {e}"); false }
                    },
                    _ => false,
                };
                if !orig_gun_ok {
                    match scene.install_guns() { Ok(()) => eprintln!("[arms] recovered 1.8 weapon meshes ON: original meshes + original Diffuse/Normals textures; {}", if self.opts.original_gun_material { "FALLBACK, the cooked material failed (see above)" } else { "--standin-gun-material" }), Err(e) => eprintln!("[arms] gun meshes unavailable: {e}") }
                }
                marks.push(("guns", t_done.elapsed().as_secs_f32() * 1000.));
                self.populate_real_props(&scene); // REAL PROPS HOOK
                crate::mpc_hook::begin_play(self.settings.video.shadows); // MATERIAL PARAMETERS: TODM_A construction script + ReceiveBeginPlay (GetShadowQuality)
                marks.push(("props + mpc", t_done.elapsed().as_secs_f32() * 1000.));
                eprintln!("[load] post-load setup (UI thread): {}", marks.iter().map(|(n, ms)| format!("{n} -> {ms:.0} ms")).collect::<Vec<_>>().join(", "));
                self.scene = Some(scene);
                self.loader = None;
                self.flow.set_loader_done();
                self.flow.note(stage_note.trim_end());
                if self.run.is_some() {
                    // FIRST USE: `FN_SCRIPT_WARM=1` runs the loading screen's building warm-up before frame 0, as a normal launch does (verification/first_use)
                    if fn_live::var("FN_SCRIPT_WARM").map_or(cfg!(target_arch = "wasm32"), |v| v != "0") { if let Some(scene) = self.scene.as_mut() {
                        let t = Instant::now();
                        while !(crate::building_render::prebuild_step(scene.renderer.fx(), 1000.) && crate::pickups_fx::prebuild_step(&mut scene.renderer, 1000.)) && t.elapsed().as_secs() < 60 {}
                        crate::pickups_hook::warm_pickaxe(scene);
                        while !crate::particles::prebuild(&mut scene.renderer) {}
                        while !crate::muzzle_hook::prebuild(&mut scene.renderer) {}
                        while !crate::foot_fx::prebuild(scene.renderer.fx()) {} // FOOT FX
                        crate::heal_fx::prebuild(scene.renderer.fx()); // HEAL FX
                        crate::chest_hook::prebuild(scene.renderer.fx()); // CHEST HOOK
                        crate::rocket_hook::prebuild(scene.renderer.fx());
                        while !crate::gunfx_hook::prebuild(scene.renderer.fx()) {}
                        while !crate::elim_fx::prebuild(scene.renderer.fx()) {}
                        while !crate::supply_drop_hook::prebuild(scene.renderer.fx()) {}
                    } }
                    self.flow.force(Stage::Playing); self.phase = self.begin_play(); return;
                }
            }
            Ok(State::Running) => {}
        }
        let (fraction, label) = if self.loader.is_none() { (1., "Starting".to_owned()) } else { (fraction, label) };
        self.loading_page(fraction, &label, 0.);
        let s = self.flow.tick(dt); crate::platform::timed("entered", || self.entered(s));
    }

    fn finishing_frame(&mut self, dt: f32) {
        #[cfg(target_arch = "wasm32")]
        crate::web::heap_reserve_step();
        // LOAD TIME (2026-10-06): the real-shader props' materials build ~25 ms per frame under the live loading screen. The flow holds here until they
        // are done (capped at 30 s), so the first in-game frame no longer stalls on them with the screen frozen on "Starting".
        self.phase_t += dt;
        // FIRST USE (2026-10-06): then the building pieces / their effects (building_render::prebuild_step), so the first piece of a type does not freeze
        crate::statics_render::set_view_distance_scale(self.backend.render.options.view_distance_scale);
        static UI_WARM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let ui_pending = self.ui.as_mut().map_or(0, |u| { if !UI_WARM.swap(true, std::sync::atomic::Ordering::Relaxed) { let n = u.renderer.prewarm_materials(); eprintln!("[load] ui material pipelines: {n} started"); } u.renderer.prewarm_pending() });
        { let dir = self.install_dir(); crate::skydive_anim::ensure_loading(|| dir); }
        crate::enhanced_anim_load::ensure_loading(self.install_dir(), &self.roots.roots);
        crate::enhanced_sound::ensure_loading(&self.roots.roots);
        let (chain, pso_done) = match self.scene.as_mut() {
            Some(scene) => { let pso = crate::platform::timed("finishing: pso warm", || scene.renderer.fx().pso_warm_step(FINISH_STEP_MS)); (crate::platform::timed("finishing: guns", || scene.step_original_guns()) && crate::platform::timed("finishing: landscape", || crate::landscape_render::prebuild_step(scene.renderer.fx())) // LOAD TIME: the landscape's GPU data under the loading screen, not in the first frame of play
                && crate::platform::timed("finishing: statics", || crate::statics_render::prebuild_step(scene.renderer.fx(), &self.sim.world.props, scene.real.as_ref(), FINISH_STEP_MS)) && crate::platform::timed("finishing: building", || crate::building_render::prebuild_step(scene.renderer.fx(), FINISH_STEP_MS))
                && crate::platform::timed("finishing: splines", || crate::spline_mesh::prebuild_step(scene.renderer.fx())) // ROADS / SPLINES: the spline meshes under the loading screen (spline_mesh.rs)
                && crate::platform::timed("finishing: pickups", || crate::pickups_fx::prebuild_step(&mut scene.renderer, FINISH_STEP_MS)) // FIRST USE: the pickup effects (and with FN_WARM_ITEMS the item holograms / static materials)
                && crate::platform::timed("finishing: skydive", || crate::skydive_anim::prebuild_step(scene)) && crate::enhanced_anim_load::prebuild_step(scene) && crate::enhanced_sound::ready(), pso) }
            None => (true, true),
        };
        let warm = chain && self.finish_warm_step();
        static UPGRADES_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !UPGRADES_ON.swap(true, std::sync::atomic::Ordering::Relaxed) { fn_fx::gpu::enable_upgrades(); }
        if let Some(scene) = self.scene.as_mut() { crate::decals_hook::prewarm(&mut scene.renderer); }
        if let Some(scene) = self.scene.as_mut() { crate::platform::timed("finishing: texture upgrades", || fn_fx::gpu::upgrade_step(scene.renderer.fx(), 8 << 20)); }
        let textures = !warm || fn_fx::gpu::upgrades_idle();
        let pending = self.scene.as_mut().map_or(0, |s| s.renderer.fx().pending_pipelines());
        let ready = chain && warm && textures && pso_done && pending == 0 && ui_pending == 0 || self.phase_t > if cfg!(target_arch = "wasm32") { 240. } else { 30. }; // BROWSER PORT: remote file reads can take minutes on a cold cache
        // PIPELINE DEDUP: the FX pipelines the load made and the identical requests that reused one (once, when the loading screen is done)
        static DEDUP_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if ready && !DEDUP_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) { if let Some(scene) = self.scene.as_mut() { eprintln!("{}", scene.renderer.fx().pipeline_cache_report()); } }
        static FIN_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if ready && !FIN_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) { eprintln!("[load] finishing stage: {:.1} s (POI kinds {:?}, done or capped)", self.phase_t, crate::statics_render::poi_prepare_progress()); }
        let fade = self.flow.finish_fade();
        LAST_FINISH_FADE.store(fade.to_bits(), std::sync::atomic::Ordering::Relaxed);
        // LOADING SCREEN: real progress of the world preparation (the POI kinds of the groups around the spawn)
        let (pd, pt) = crate::statics_render::poi_prepare_progress();
        let (sd, st, sdone) = self.scene.as_mut().map_or((0, 0, true), |s| s.renderer.fx().pso_warm_progress());
        if pt > 0 && pd < pt { self.loading_page(pd as f32 / pt as f32, &format!("Preparing the world ({pd} / {pt})"), fade); }
        else if !sdone && st > 0 { self.loading_page(sd as f32 / st as f32, &format!("Compiling shaders ({sd} / {st})"), fade); }
        else { self.loading_page(1., "Starting", fade); }
        if ready { let s = self.flow.tick(dt); crate::platform::timed("entered", || self.entered(s)); }
    }

    fn load_fx_world(&mut self) {
        if !self.fx_tried && self.scene.is_some() {
            self.fx_tried = true;
            if let Some(dir) = self.install_dir() {
                match fn_pak::PakSet::open_install(&dir).map_err(|e| e.to_string()).and_then(|set| fn_fx::bridge::FxWorld::load(&set, self.scene.as_mut().unwrap().renderer.fx())) {
                    Ok(w) => { for n in &w.notes { eprintln!("[fx] {n}"); } self.fx = Some(w); }
                    Err(e) => eprintln!("[fx] weak-point ring / damage numbers unavailable: {e}"),
                }
            }
        }
    }

    fn finish_warm_step(&mut self) -> bool {
        static STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static TRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let step = STEP.load(std::sync::atomic::Ordering::Relaxed);
        if self.scene.is_none() { return true; }
        let t = Instant::now();
        let dir = self.install_dir();
        let mut advance = {
            let scene = self.scene.as_mut().unwrap();
            match step {
                0 => { crate::pickups_hook::warm_pickaxe(scene); true }
                1 => crate::particles::prebuild(&mut scene.renderer),
                2 => crate::muzzle_hook::prebuild(&mut scene.renderer),
                3 => crate::foot_fx::prebuild(scene.renderer.fx()),
                4 => { crate::heal_fx::prebuild(scene.renderer.fx()); true }
                5 => { crate::chest_hook::prebuild(scene.renderer.fx()); true }
                6 => crate::bus_hook::prebuild(scene.renderer.fx()),
                7 => crate::rocket_hook::prebuild(scene.renderer.fx()),
                20 => crate::gunfx_hook::prebuild(scene.renderer.fx()),
                21 => crate::elim_fx::prebuild(scene.renderer.fx()),
                22 | 23 | 24 => true,
                25 => crate::supply_drop_hook::prebuild(scene.renderer.fx()),
                8 => true,
                9 => { if crate::df_hook::pending() { crate::df_hook::frame(scene, dir.clone(), &self.roots.roots); } !crate::df_hook::pending() || TRIES.load(std::sync::atomic::Ordering::Relaxed) > 1200 }
                10 => { if crate::light_fn_hook::pending() { crate::light_fn_hook::frame(&mut scene.renderer, dir.clone()); } true }
                11 => { if crate::local_lights_hook::pending() { crate::local_lights_hook::frame(&mut scene.renderer, dir.clone(), &self.sim.world.props); } true }
                12 => { if crate::sky_refl_hook::pending() { crate::sky_refl_hook::frame(&mut scene.renderer, dir.clone()); } !crate::sky_refl_hook::pending() || TRIES.load(std::sync::atomic::Ordering::Relaxed) > 600 }
                13 => { if self.storm.wants_load(&self.sim.storm, true) { self.storm.load(dir.clone(), scene.renderer.fx(), &self.sim.storm); } true }
                14 => { crate::backdrop_real::prebuild(scene.renderer.fx()); true }
                15 => { crate::statics_render::grass_prebuild(scene.renderer.fx(), scene.grass.as_ref()); true }
                16 => { crate::trap_hook::prebuild_catalog(); crate::trap_hook::warm_ribbons(scene.renderer.fx()); true }
                17 => crate::building_render::prebuild_doors(scene.renderer.fx(), FINISH_STEP_MS),
                18 => crate::bots_hook::prewarm(scene),
                19 => true,
                _ => return true,
            }
        };
        if step == 8 { self.load_fx_world(); }
        if let (22..=24, Some(u), Some(g)) = (step, self.ui.as_mut(), self.gfx.as_ref()) {
            let (w, h) = g.size();
            match step { 22 => u.hud.prewarm_aerial(&mut u.ctx, w as f32, h as f32), 23 => { if !u.hud.prewarm_game_over(&mut u.ctx, w as f32, h as f32) { advance = false; } } _ => { let map = u.hud.prewarm(&mut u.ctx); u.renderer.prewarm_straight(map, &u.ctx.assets); } }
        }
        if step == 19 { crate::demolition_hook::tick(&mut self.sim); }
        let ms = t.elapsed().as_secs_f32() * 1000.;
        crate::platform::section(&format!("finish warm {step}"), ms);
        if ms > 100. { eprintln!("[load] finishing warm step {step}: {ms:.0} ms"); }
        if advance { STEP.fetch_add(1, std::sync::atomic::Ordering::Relaxed); TRIES.store(0, std::sync::atomic::Ordering::Relaxed); } else { TRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        false
    }

    fn begin_play(&mut self) -> Phase {
        crate::audio::stage_audio(self.backend.audio.as_mut(), Stage::Playing); // MUSIC HOOK (also the scripted path that forces Playing)
        self.input.started = true;
        if self.flow.lobby {
            for (slot, id) in crate::lobby_profile::stored_loadout(&crate::lobby_profile::default_profile_path()) {
                let r = match slot.as_str() { "outfit" => Some(fn_game::cosmetic_swap::set_skin(&mut self.sim, &id)), "pickaxe" => Some(fn_game::cosmetic_swap::set_pickaxe(&mut self.sim, &id)), _ => None };
                if let Some(r) = r { eprintln!("[lobby] locker {slot} {id} -> match pawn: {}", r.unwrap_or_else(|e| e)); }
            }
        }
        self.startup_elapsed = 0.;
        self.last = Instant::now();
        eprintln!("[load] done:\n{}", self.load_log);
        // SPAWN CHECK (2026-10-07): where the pawn starts against the loaded landscape and collision (the browser build was reported spawning under the ground)
        {
            let (x, z) = (self.sim.x, self.sim.z);
            let g = self.sim.ground_y(x, z);
            let f = self.sim.floor_below(glam::Vec3::new(x, g + fn_game::SPAWN_PROBE_M, z));
            let feet = self.sim.feet().y;
            eprintln!("[spawn] feet {feet:.2} m, landscape {g:.2} m, collision floor {f:.2} m at ({x:.1}, {z:.1}){}", if self.sim.terrain().is_none() { " (NO LANDSCAPE LOADED)" } else if feet + 0.05 < f { " (BELOW THE FLOOR)" } else { "" });
        }
        // RANDOM START HOUR: AFortGameModeAthena::HandleMatchHasStarted @1403f78a0 (bUseRandomTimeOfDay = true, cooked ini) sets the clock to
        // rand() * (1/32767) * 23.99. Scripted runs keep the deterministic start (StartTimeOfDayInGame 7.0, or FN_TOD_HOUR), like their speed-0 clock.
        let deterministic = self.run.is_some() || self.opts.scripted();
        match fn_render::time_of_day::start_match(deterministic) {
            Some(h) => eprintln!("[tod] match start: random time of day {h:.3} h (bUseRandomTimeOfDay)"),
            None => eprintln!("[tod] match start: time of day {:.3} h ({})", fn_render::time_of_day::hour(), if deterministic { "scripted run: no random hour" } else { "FN_TOD_HOUR" }),
        }
        crate::bus_hook::begin_match(&mut self.sim, deterministic);
        Phase::Playing
    }

    fn error_frame(&mut self, msg: &str) { let m = msg.to_owned(); self.simple_frame(|ui, w, h| ui.error_screen(w, h, &m)); }

    // ------------------------------------------------------------------------------------------------------------------------------
    // `--startup-capture`: the whole flow on a fake clock, synthetic picker input, PNGs of every screen
    // ------------------------------------------------------------------------------------------------------------------------------
    /// Feeds the capture script this frame's state and carries out its action (a capture is taken by the frame drawn right after this call).
    fn capture_poll(&mut self, el: &ActiveEventLoop, dt: f32) {
        use startup::capture::Act;
        let (w, h) = self.gfx.as_ref().map_or((0, 0), |g| g.size());
        let fraction = self.loader.as_ref().map_or(if self.flow.loader_done() { 1. } else { 0. }, |l| l.fraction());
        let view = startup::capture::View { stage: self.flow.stage, stage_t: self.flow.stage_t, fraction, loader_done: self.flow.loader_done(), startup_fade: self.flow.startup_fade(), finish_fade: self.flow.finish_fade(), size: (w, h), page: self.picker.page, art_mix: self.picker.art_mix, art_ready: self.art.startup_uploaded };
        let stage = self.flow.stage.name();
        let act = {
            let Some(cap) = self.cap.as_mut() else { return };
            if cap.started.elapsed().as_secs() > 240 { cap.failures.push(format!("timed out in stage {stage} waiting for {:?}", cap.script.next_cond())); Some(Act::Finish) } else { cap.script.poll(&view, dt) }
        };
        let Some(act) = act else { return };
        match act {
            Act::Shot(n) => {
                self.flow.note(format!("capture trigger {n}: stage {stage} t={:.2}s progress={:.1}% picker page {:?} art_mix {:.2}", view.stage_t, view.fraction * 100., view.page, view.art_mix));
                if let Some(cap) = self.cap.as_mut() { cap.pending.push(n); }
            }
            Act::Hover(item) | Act::Click(item) => {
                let click = matches!(act, Act::Click(_));
                let Some(ui) = self.ui.as_ref() else { return };
                let l = ui.picker_layout(self.picker.page, w as f32, h as f32);
                match l.rect(item) {
                    Some(r) => {
                        let (x, y) = r.center();
                        self.flow.note(format!("synthetic pointer: {} {:?} at ({x:.0}, {y:.0})", if click { "click" } else { "hover" }, item));
                        self.input.cursor = (x, y);
                        self.picker_event(UiEvent::MouseMove(x, y));
                        if click { self.picker_event(UiEvent::MouseDown(UiMouseButton::Left, x, y)); self.picker_event(UiEvent::MouseUp(UiMouseButton::Left, x, y)); }
                    }
                    None => { let m = format!("capture script: {item:?} is not on the {:?} page", self.picker.page); self.flow.note(m.clone()); if let Some(cap) = self.cap.as_mut() { cap.failures.push(m); } }
                }
            }
            Act::Resize(nw, nh) => {
                self.flow.note(format!("synthetic resize request {nw}x{nh} (window is {w}x{h})"));
                if let Some(p) = self.platform.as_ref() { let _ = p.window.request_inner_size(PhysicalSize::new(nw, nh)); }
            }
            Act::Finish => self.finish_capture(el),
        }
    }

    fn finish_capture(&mut self, el: &ActiveEventLoop) {
        let Some(cap) = self.cap.take() else { return };
        let (w, h) = cap.size;
        let reached_world = self.flow.stage == Stage::Playing;
        let mut ok = cap.failures.is_empty() && reached_world && cap.script.done();
        let mut r = format!("fortnite-rs start-up capture {w}x{h} (fake clock {:.5} s per frame, real run {:.1} s)\ngpu: {}\n", CAPTURE_DT, cap.started.elapsed().as_secs_f32(), self.gfx.as_ref().map(|g| g.describe()).unwrap_or_default());
        r.push_str(&format!("game picker: {}  (normal Battle Royale flow: Splash -> StartupLoading -> GamePicker -> Loading -> Finishing -> Playing)\n", if self.flow.picker { "on" } else { "off (only diagnostic/custom flow)" }));
        r.push_str("\n== state log (flow clock = fake time) ==\n");
        r.push_str(&self.flow.log_text());
        r.push_str("\n== start-up worker steps ==\n");
        for (l, s, at) in self.work.steps() { r.push_str(&format!("{s:8.3} s  {l}  (started at {at:.3} s)\n")); }
        if let Some(n) = self.work.note() { r.push_str(&format!("worker note: {n}\n")); }
        r.push_str("\n== load steps (parallel; start offsets) ==\n");
        r.push_str(&self.load_log);
        r.push_str(&format!("\n== UI-thread stalls on the start-up screens (frame intervals, real time, capture frames excluded) ==\nframes {}  longest {:.1} ms  over {STALL_MS:.0} ms: {}  worst {:?}\n", self.stalls.frames, self.stalls.max_ms, self.stalls.over_stall, self.stalls.worst));
        r.push_str(&format!("\n== captures ({}) ==\n", cap.shots.len()));
        for s in &cap.shots { r.push_str(&format!("  {}\n", s.display())); }
        for f in &cap.failures { r.push_str(&format!("FAILURE: {f}\n")); }
        if !reached_world { r.push_str(&format!("FAILURE: stopped in stage {}\n", self.flow.stage.name())); }
        if !cap.script.done() { r.push_str(&format!("FAILURE: the script stopped at step {} of {} (waiting for {:?})\n", cap.script.idx, cap.script.steps.len(), cap.script.next_cond())); ok = false; }
        r.push_str(if ok { "RESULT: ok\n" } else { "RESULT: FAILED\n" });
        let _ = std::fs::create_dir_all(&cap.out);
        let path = cap.out.join(format!("startup-capture-{w}x{h}-log.txt"));
        let _ = std::fs::write(&path, &r);
        println!("{r}log: {}", path.display());
        self.exit_code = if ok { 0 } else { 1 };
        el.exit();
    }

    /// PICKUPS HOOK: world items step (see pickups_hook.rs): the `spawn` queue, toss physics, the Use key / touch pickup, and the delivery of arrived items to the inventory.
    fn step_pickups(&mut self, dt: f32) {
        let use_down = !(self.console.console.is_open() || crate::chat_hook::is_open()) && self.backend.input.bindings.down(fn_settings::A_USE, |k| self.input.poll(k));
        crate::platform::timed("pickups update", || crate::pickups_hook::update(&mut self.sim, self.arms.as_mut(), &mut self.weapons, self.console.built.as_ref(), self.hud_inputs.items.as_mut(), self.backend.audio.as_mut(), dt, use_down)); // FIRST USE: [slow] when a spawn / pickup stalls the UI thread
        for (err, l) in crate::pickups_hook::take_lines() { self.console.console.say(if err { fn_console::LineKind::Error } else { fn_console::LineKind::Info }, l); }
        // DOOR HOOK: map / player doors (Use on the focused leaf, swing, timers, collision into the sim, open / close cues); a focused pickup keeps the key
        let pickup_focused = crate::pickups_hook::has_focus();
        crate::mp_hook::set_use_held(use_down && !pickup_focused);
        for e in crate::platform::timed("doors", || crate::door_hook::frame(&mut self.sim, use_down, pickup_focused, dt)) { self.backend.audio.event(e); }
        // CHEST HOOK: chests / ammo boxes (hold Use to search, open, loot); a focused pickup or door keeps the key
        let door_focused = crate::door_hook::prompt("").is_some();
        if self.phase == Phase::Playing { crate::platform::timed("floor loot", || crate::floor_loot_hook::frame(&mut self.sim)); }
        let equip = (self.hud_inputs.selected, self.build_input.is_building());
        for e in crate::platform::timed("chests", || crate::chest_hook::frame(&mut self.sim, use_down, pickup_focused || door_focused, equip, dt)) { if fn_live::var("FN_BUILD_SOUND_LOG").is_ok_and(|v| v == "1") { eprintln!("[chest-sound] {e:?}"); } self.backend.audio.event(e); }
        let chest_focused = crate::chest_hook::prompt("").is_some();
        let zones = self.storm.game.zones_view();
        for e in crate::platform::timed("supply drops", || crate::supply_drop_hook::frame(&mut self.sim, zones, use_down, pickup_focused || door_focused || chest_focused, dt)) { self.backend.audio.event(e); }
        if let Some(a) = self.arms.as_mut() { a.set_interact_search(crate::chest_hook::searching()); if crate::interact::take_tap() { a.play_interact_use(); } } // CHEST HOOK: the pawn's Search montage while a search hold runs
    }
    /// WEAPON RUNTIME HOOK: the runtime's log lines into the F2 console log.
    fn flush_weapon_events(&mut self) { for (is_err, line) in self.weapons.take_events() { self.console.console.say(if is_err { fn_console::LineKind::Error } else { fn_console::LineKind::Info }, line); } }
    /// WEAPON RUNTIME HOOK: starts / installs requested weapons (`weapon_runtime::WeaponRuntime::poll`) and applies their HUD changes. The resources are lent out of the scene for the install (the GPU step needs both).
    fn poll_weapons(&mut self) {
        if self.arms.is_none() || self.scene.as_ref().map_or(true, |s| s.loadout.is_none()) { self.weapons.fail_all("no live loadout (the original item resources were not loaded)"); self.flush_weapon_events(); return; }
        for w in crate::rocket_hook::take_give_aid(self.sim.frames) { if let Err(e) = self.weapons.request_give(&w, true) { eprintln!("[rockets] give {w}: {e}"); } }
        let install = self.install_dir(); // INVENTORY HOOK: for the ammo item texts of a newly installed weapon
        let (Some(arms), Some(scene)) = (self.arms.as_mut(), self.scene.as_mut()) else { return };
        let Some(mut res) = scene.loadout.take() else { return };
        let installed = crate::platform::timed("weapon poll", || self.weapons.poll(&mut res, arms, self.ads.as_mut(), &mut |r, i| scene.install_extra_gun(r, i.gun, &i.new_sequences, &i.new_cores))); // FIRST USE
        scene.loadout = Some(res);
        for (is_err, line) in self.weapons.take_events() { self.console.console.say(if is_err { fn_console::LineKind::Error } else { fn_console::LineKind::Info }, line); }
        for inst in installed {
            for (name, w, h, rgba) in &inst.ui_images { self.overlay.insert(name, fn_ui::assets::Image { w: *w, h: *h, rgba: rgba.clone() }); }
            if let Some(items) = self.hud_inputs.items.as_mut() {
                items[inst.slot] = Some(fn_ui::screens::hud::HudItem::new(inst.rarity, &inst.icon, None));
                self.hud_inputs.backpack = Some((items[1..].iter().flatten().count() as u32, 5));
            }
            if let Some(ui) = self.ui.as_mut() { if let Some(c) = inst.catalog { ui.inv.add_catalog_item(c); } ui.inv.sync_quickbar(&arms.quickbar); if let Some(dir) = &install { for n in ui.inv.load_extras(dir, arms) { eprintln!("{n}"); } } }
        }
    }
    /// Feeds the arms (quickbar / fire / reload / heal) for this frame and hands their results to the simulation: the held core animation set, the weapon montages
    /// and the non-melee flag (the pickaxe swing is off while anything else is selected). Returns the HUD selection.
    fn drive_arms(&mut self, dt: f32, inp: &mut FrameInput, ai: ArmsInput) {
        self.poll_weapons(); // WEAPON RUNTIME HOOK: weapons requested (console give / equip) install here, before the arms step
        crate::build_anim::poll(&self.build_input, || self.install_dir()); // BUILD ANIM HOOK: build / edit mode edges of the build input + the one-time background load
        let Some(arms) = self.arms.as_mut() else { return };
        let before = arms.selected;
        let fire = inp.attack;
        arms.update(dt, &ArmsInput { fire_held: fire, ..ai });
        for e in arms.take_events() { eprintln!("{e}"); }
        // WEAPON AUDIO HOOK: the arms' sound moments -> sink events (weapon sounds at the weapon's attach point, montage-notify sounds at the pawn mesh)
        crate::heal_fx::on_notifies(arms.take_particles()); // HEAL FX: the montages' AnimNotify_PlayParticleEffect (P_Consume_Item at the heal trigger)
        let sounds = arms.take_sounds();
        crate::muzzle_hook::on_arms_sounds(&sounds);
        if !sounds.is_empty() {
            let weapon = self.scene.as_ref().and_then(|s| s.weapon_origin).unwrap_or(self.sim.cam_view.eye);
            let pawn = crate::scene::pawn_matrix(&self.sim).w_axis.truncate();
            let bps: Vec<Option<String>> = arms.data.guns.iter().map(|g| g.def.assets.actor.clone()).collect(); // WEAPON BP TABLE: each firearm's own Blueprint sounds
            let low = arms.low_ammo(); // FIRE LOOP / LAYERS: the LowAmmo fire channel layers on the normal one
            for s in &sounds { for e in crate::audio::arms_to_sounds(s, &bps, low, weapon, pawn) { self.backend.audio.event(e); } }
        }
        let selected = arms.selected;
        inp.selection_changed = selected != before;
        inp.equip = selected == 0 && (before != 0 || self.sim.frames == 0);
        inp.non_melee = selected != 0; inp.attack = fire && selected == 0;
        if selected != 0 { inp.reset = false; } // R is Reload while a firearm is held (the pickaxe keeps R = back to spawn)
        // ADS HOOK: the Target binding (only with the mouse captured: a free mouse's right-button drag is look) drives the targeting state of the held firearm
        let mut aiming = false;
        let mut ads_weight = 0.0f32;
        let mut reticle_hidden = false; // the sniper scope timeline's `ReticleHUDElementTags` hide (recovered rule, `AdsMachine::reticle_hidden`)
        crate::scope_hook::set(0., false); // SCOPE HOOK: no scope picture unless the ADS step below says otherwise
        if let Some(ads) = self.ads.as_mut() {
            let held = (self.input.captured && self.backend.input.bindings.down(fn_settings::A_TARGET, |k| self.input.poll(k))) || self.input.pad_action(fn_settings::A_TARGET) || crate::ads::script_target();
            let (can_aim, i) = (arms.gun_index().is_some() && !arms.is_healing(), &self.settings.input);
            // GAMEPAD LOOK: vfunc_71 @1404392c0 keeps separate multipliers: MouseX/MouseY get TargetingMultiplier / ScopedMultiplier (+0x4f4 / +0x4f8), every other
            // axis (the right stick) GamepadTargetingMultiplier / GamepadScopedMultiplier (+0x4fc / +0x500). The look in `inp` here is the mouse's (and scripted
            // turn=); the stick gets its own pair before its curve in `PadLook` (added to `inp` after this step).
            let pad_look = self.pad_state.as_ref().map_or(false, |s| { let (x, y) = gpad::radial_deadzone(s.rx, s.ry, gpad::RIGHT_DEADZONE); x != 0. || y != 0. });
            let (ads_mul, scope_mul) = (i.mouse_ads_multiplier, i.mouse_scope_multiplier);
            let f = ads.step(dt, held, i.toggle_targeting, arms.gun_index(), can_aim, ads_mul, scope_mul);
            crate::scope_hook::set(f.scope_amount, f.scope_mesh); // SCOPE HOOK: DownScopePostProcessAmount + ScopeMesh1P visibility -> the renderer's scope pass (scope_hook.rs)
            ads.apply(&f, &mut self.sim, inp, !pad_look);
            aiming = f.aiming; ads_weight = f.weight; reticle_hidden = f.reticle_hidden;
            crate::muzzle_hook::set_scope(f.scope_mesh); // MUZZLE HOOK: IsUsingScope stand-in (the muzzle flash is skipped while looking through the scope)
        }
        // Select the authored player/weapon fire montage after the ADS state is
        // resolved.  Sampling before this point made every aimed shot use the
        // hip-fire local transform.
        arms.set_aiming(aiming);
        self.sim.set_held(arms.held_core().map(|(core, _)| (core, aiming)));
        if std::mem::take(&mut self.sim.sprint_cancelled_reload) { arms.cancel_reload_by_sprint(); }
        self.sim.reloading = arms.is_reloading();
        if let Some(why) = self.sim.movement_cancels_action(arms.is_healing()) { arms.cancel_heal(why); }
        self.sim.sprint_cancels_reload = self.settings.input.sprint_cancels_reload;
        // RELAXED HOOK: the held firearm's LastFireTime / reload / equip state -> the 1.8 native relaxed timers (bIsRelaxedLevel1 after 5 s, Level2 after 15 s, bRecentlyFired 0.2 s)
        self.sim.set_weapon_clock(arms.weapon_clock());
        // RECOIL HOOK: IsUsingGamepad proxy = the right stick is the looking device (same predicate as the ADS look rate above); the recoil step is added to this frame's look delta
        let gamepad = self.pad_state.as_ref().map_or(false, |s| { let (x, y) = gpad::radial_deadzone(s.rx, s.ry, gpad::RIGHT_DEADZONE); x != 0. || y != 0. });
        crate::hitreg_hook::set_aim_inputs(ads_weight, self.scene.as_ref().and_then(|s| s.weapon_origin)); // SPREAD HOOK: targeting camera weight + muzzle
        crate::building_hud::set_ranged_targeting(aiming); // BUILDING HUD: HandlePlayerTargetingChanged collapses the Target Info indicator
        crate::hitreg_hook::on_arms(&mut self.sim, arms, aiming, dt, inp, gamepad); // HITREG HOOK: shots -> trace / damage / prop health; RECOIL HOOK: recovered recoil -> look delta
        // Only confirmed hits restart `B_Ranged_Generic.HitNotifyReticleImage` (0.4 s); misses and melee never do.
        self.hud_inputs.firearm_hit = crate::hitreg_hook::take_impacts().iter().any(|(_, i)| i.surface == fn_hitreg::shoot::SURFACE_PLAYER && i.damage > 0.);
        self.sim.arms = arms.montages();
        { let pawn = crate::scene::pawn_matrix(&self.sim).w_axis.truncate() + glam::Vec3::Y * 0.9; let dead = crate::kill_feed_hook::local_death().is_some() || crate::mp_hook::local_dead(); let dm = crate::death_hook::local_frame(arms.data.death.as_ref(), dead, dt, pawn); self.sim.arms.extend(dm); let km = crate::dbno_hook::local_frame(arms.data.death.as_ref(), crate::mp_hook::local_knocked(), dead, dt, pawn); self.sim.arms.extend(km); }
        {
            let dead = crate::kill_feed_hook::local_death().is_some() || crate::mp_hook::local_dead();
            let weapon_ability = self.sim.swing.stage != 0 || (fire && arms.gun_index().is_some());
            let building = self.build_input.is_building() || self.build_input.is_editing();
            let typing = self.console.console.is_open() || crate::chat_hook::is_open();
            let pad_emote = crate::emote_hook::edge(23, self.pad_state.as_ref().map_or(false, |s| s.buttons & fn_game::pad_layout::DPAD_DOWN != 0));
            let key = crate::emote_hook::edge(24, self.backend.input.bindings.down(fn_settings::A_CHAT_PICKER, |k| self.input.poll(k)));
            let press = (key || pad_emote) && !typing && !crate::emote_hook::input_owned();
            let f = crate::emote_hook::Frame { dt, press, feet: self.sim.feet(), airborne: self.sim.controller.airborne(), crouched: self.sim.controller.crouched, jump_press: crate::emote_hook::edge(25, inp.jump),
                weapon_ability, weapon_start: crate::emote_hook::edge(26, weapon_ability), equip_changed: inp.selection_changed || crate::emote_hook::changed(27, building), dbno: crate::mp_hook::local_knocked(), dead, skydiving: self.sim.items_blocked() };
            let em = crate::emote_hook::local_frame(&f, self.pad_state.is_some());
            self.sim.arms.extend(em);
            if crate::mp_hook::players_alive().is_none() { let _ = crate::emote_hook::take_net(); }
        }
        crate::elim_fx::step(dt);
        for e in crate::death_hook::take_sounds() { self.backend.audio.event(e); }
        if crate::death_hook::take_killed_mix() { self.backend.audio.push_player_killed_mix(); }
        self.sim.anim.set_mask_right_arm(arms.mask_right_arm());
        self.sim.anim.set_additive_layer_curve(arms.interact_curve("AdditiveLayer").map_or(0., |(s, _)| s));
        crate::interact::set_montage_curves(["BothHandFK", "LeftHandFK"].into_iter().filter_map(|n| arms.interact_curve(n).map(|(s, w)| (n, s, w))).collect());
        crate::build_anim::drive(&mut self.sim, arms, inp, dt); // BUILD ANIM HOOK: blueprint in the hands -> PaperBlueprint set, BuildingState, Blueprint_* montages
        // BUILD ANIM HOOK: the blueprint montages' PlaySound notifies (Blueprint_Equip -> Player_Equip_Blueprint_Cue, bFollow: at the pawn mesh)
        for s in crate::build_anim::take_sounds() { let pawn = crate::scene::pawn_matrix(&self.sim).w_axis.truncate(); for e in crate::audio::arms_to_sounds(&s, &[], false, pawn, pawn) { self.backend.audio.event(e); } }
        self.hud_inputs.selected = selected;
        self.hud_inputs.ammo = arms.ammo_state();
        // RETICLE HOOK: `SFortReticle` of the held firearm (weapon data from `AdsProfile::reticle`, spread from the hitreg spread state, FOV of the blended camera)
        // Test aid (never set in play, like `FNRT_ADS`): `FNRT_MUZZLE_BLOCKED=1` reports the muzzle as blocked at a fixed point so the muzzle-blocked image can be captured;
        // the real producer (the weapon trace, `AFortWeaponRanged` vfunc 267 RVA 0x140942c80, writes `+0xaa0`) needs world collision this runtime has not got for the baseplate.
        let test_blocked = std::env::var_os("FNRT_MUZZLE_BLOCKED").map(|_| [80., 50.]);
        self.hud_inputs.firearm_reticle = if reticle_hidden { None } else {
            arms.gun_index().and_then(|g| {
                let rd = &self.ads.as_ref()?.profiles.get(g)?.reticle;
                let remaining = self.hud_inputs.ammo.as_ref().map_or(1, |a| a.remaining);
                Some(fn_ui::screens::reticle_state::FirearmReticle {
                    image: rd.image.clone(), center: rd.center_image.clone(), corner_angles: rd.corner_angles.clone(), corner_outside: rd.corner_outside_spread_radius,
                    spread_deg: crate::hitreg_hook::reticle_spread_deg(), fov_deg: if self.sim.ads.fov > 0. { self.sim.ads.fov } else { 80. },
                    dim: arms.is_reloading() || remaining <= 0,
                    hit: rd.hit_image.clone(), muzzle_blocked: rd.muzzle_blocked_image.clone(), muzzle_blocked_px: test_blocked,
                })
            }).or_else(|| arms.throwable_selected().then(|| fn_ui::screens::reticle_state::FirearmReticle {
                image: None, center: Some(GRENADE_RETICLE_IMAGE.to_owned()), corner_angles: vec![], corner_outside: false, spread_deg: 0., fov_deg: 80., dim: false, hit: None, muzzle_blocked: None, muzzle_blocked_px: None,
            }))
        };
        self.hud_inputs.equip = arms.equip_progress().map(|(elapsed, duration)| fn_ui::screens::reticle_state::EquipProgress { elapsed, duration });
        self.hud_inputs.slot_cooldowns = arms.slot_cooldowns(); // WIDGETS3 HOOK: QuickbarSlotCooldown
        crate::hurt_hook::on_frame(&mut self.sim, arms, dt); // FALL DAMAGE + HURT HOOK: landings -> shield / health, lens effect, camera shake
        self.hud_inputs.vitals = (arms.health.round() as u16, arms.shield.round() as u16);
        // INVENTORY HOOK: the HUD cards and the backpack counter follow the live inventory (stacks, drops, swaps, used-up consumables)
        if let (Some(ui), Some(items)) = (self.ui.as_ref(), self.hud_inputs.items.as_mut()) { ui.inv.sync_cards(arms, items); }
        self.hud_inputs.backpack = Some(arms.backpack_counts());
        for (item, count) in crate::inventory::drain_drops(arms) { crate::pickups_hook::queue_drop(&item, count); } // INVENTORY HOOK: drops and replaced items become world pickups
        if let Some(ui) = self.ui.as_mut() { for (n, w, h, rgba) in ui.inv.take_images() { self.overlay.insert(&n, fn_ui::assets::Image { w, h, rgba }); } }
        // QUICKBAR AMMO: every card's number (firearm loaded + inventory ammo, consumable stack) and ammo icon follow the live arms, None included
        if let Some(items) = self.hud_inputs.items.as_mut() { let (counts, ammo) = (arms.slot_counts(), arms.slot_ammo_brushes()); for (i, it) in items.iter_mut().enumerate() { if let Some(it) = it.as_mut() { it.count = counts[i]; it.ammo = ammo[i].clone(); } } }
    }

    /// GAMEPAD LOOK: one `FUN_140615dd0` tick for the raw right stick -> (yaw right, pitch up) degrees (fn_game::gamepad_look). The targeting / scope
    /// state is the previous step's (UE processes the player input before the pawn ticks); `fov` = the last view's FOV (`GetFOVAngle` in vfunc_71).
    fn pad_actions(&mut self, dt: f32, cursor_mode: bool) -> fn_game::pad_layout::Actions {
        use fn_game::pad_layout::{Actions, Layout, Mode};
        if self.console.console.is_open() || crate::chat_hook::is_open() { self.input.set_pad_actions(&self.backend.input.bindings, &[]); return Actions::default(); }
        let mode = if self.build_input.is_editing() { Mode::Edit } else if self.build_input.is_building() { Mode::Build } else { Mode::Combat };
        let interact = crate::pickups_hook::has_focus() || crate::door_hook::prompt("").is_some() || crate::chest_hook::prompt("").is_some() || crate::supply_drop_hook::has_focus();
        let was_connected = self.input.pad_connected;
        self.input.pad_connected = self.pad_state.is_some();
        if was_connected != self.input.pad_connected { eprintln!("[pad] controller {}", if self.input.pad_connected { "connected" } else { "disconnected: every held pad action released" }); }
        let mut a = self.input.pad_mapper.update(self.pad_state.as_ref(), Layout::from_settings(&self.settings), mode, interact, dt);
        let map_open = self.full_map_open();
        if a.map && !cursor_mode { if let Some(ui) = self.ui.as_mut() { ui.hud.full_map = !ui.hud.full_map; eprintln!("[map] full screen map {} (controller)", if ui.hud.full_map { "opened" } else { "closed" }); } }
        if a.inventory && !map_open { self.toggle_inventory(); }
        if cursor_mode || map_open { a.held.clear(); a.toggle_harvest = false; a.crouch_press = false; }
        self.input.set_pad_actions(&self.backend.input.bindings, &a.held);
        a
    }
    fn pad_look_step(&mut self, rx: i16, ry: i16, dt: f32, fov: f32, moving: bool) -> (f32, f32) {
        if !self.pad_look.tried { let dir = self.install_dir(); self.pad_look.ensure(dir); }
        let (targeting, scope_weapon) = self.ads.as_ref().map_or((false, false), |a| (a.machine.is_targeting(), a.profile().map_or(false, |p| p.use_scope)));
        let i = &self.settings.input;
        let ctx = fn_game::gamepad_look::LookContext { fov, targeting, scope_weapon, editing: self.build_input.is_editing(), moving,
            targeting_mult: i.controller_ads_multiplier, scoped_mult: i.controller_scope_multiplier,
            sensitivity: [gpad::slider_to_sensitivity(i.controller_sensitivity_x), gpad::slider_to_sensitivity(i.controller_sensitivity_y)], invert: self.backend.input.invert_y };
        self.pad_look.tick(rx, ry, &ctx, dt)
    }

    /// One gameplay input + simulation step. Returns true when the simulation advanced.
    fn update_sim(&mut self, dt: f32) -> bool {
        let Some(ui) = self.ui.as_mut() else { return false };
        if let Some(run) = self.run.as_mut() {
            let f = run.frame;
            run.driver.prepare(f, &mut self.sim);
            let mut inp = run.driver.input(f);
            let (mdx, mdy) = run.driver.mouse(f); // MOUSE LOOK: scripted raw counts through the live 1.8 chain (InputLive::look_delta at the last view's FOV)
            if (mdx, mdy) != (0, 0) {
                let fov = if self.sim.ads.fov > 0. { self.sim.ads.fov } else { self.sim.ads.base_mode().fov };
                let (y, p) = self.backend.input.look_delta(mdx, mdy, fov);
                inp.yaw_delta += y; inp.pitch_delta += p;
                eprintln!("[mouse] frame {f}: counts ({mdx}, {mdy}) at FOV {fov:.2} -> yaw {y:+.4} pitch {p:+.4} deg (yaw before {:.4}, pitch before {:.4})", self.sim.yaw, self.sim.pitch);
            }
            let pad_axes = run.driver.pad(f); // GAMEPAD LOOK: `pad=RX:RY` (right stick -1..1, held for the range)
            let mut ai = run.driver.arms(f);
            let inv_steps = run.driver.inventory(f); // INVENTORY HOOK
            // INVENTORY FIX3 (inventory_gaps G4): scripts go through the same cursor mode as live play; the panel state is the one after this frame's
            // `inv=open` / `inv=close` (live play toggles in the key handler, before the frame)
            let cursor_mode = inv_steps.iter().rev().find_map(|s| match s { crate::script::InvCmd::Open => Some(true), crate::script::InvCmd::Close => Some(false), _ => None }).unwrap_or(ui.inv.panel.open);
            let map_open = inv_steps.iter().rev().find_map(|s| match s { crate::script::InvCmd::MapOpen => Some(true), crate::script::InvCmd::MapClose => Some(false), _ => None }).unwrap_or(ui.hud.full_map);
            cursor_mode_mask(cursor_mode || map_open, &mut inp, &mut ai);
            // SCRIPT BUILD KEYS: the same BUILD INPUT HOOK as live play below, with the script's held key-binding rows answering the key poll
            // (no `key=` rows = no edges and no intents, so scripts without them run exactly as before). `aimtile=` / `aimarrow=` aim the view
            // (absolute yaw / pitch, the `yaw=` / `pitch=` path) from the current eye at the edit tile's box centre, the box the edit trace tests.
            let (keys, aim) = (run.driver.build_keys(f), run.driver.edit_aim(f));
            crate::interact::set_script_use(keys.contains(&fn_settings::A_USE)); // INTERACT: `key=use` for every interactable hook
            crate::ads::set_script_target(keys.contains(&fn_settings::A_TARGET));
            if let Some(c) = aim.and_then(crate::building_hook::edit_target_center) {
                let d = crate::building_hook::world_to_sim(c) - self.sim.cam_view.eye;
                if d.length() > 1e-3 { let d = d.normalize(); inp.set_yaw = Some(d.x.atan2(-d.z).to_degrees()); inp.set_pitch = Some(d.y.clamp(-1., 1.).asin().to_degrees()); }
            }
            // HUD FIX: `key=1..6` (HotbarKey1..6) through the live path below: `Input::quickbar_press` over the script's held rows -> HudState::select -> select
            { let b = &self.backend.input.bindings; let codes: Vec<u16> = keys.iter().flat_map(|&a| b.get(a)).filter_map(|k| k.poll_code()).collect();
              if let Some(i) = (0..6).filter(|i| b.down(fn_settings::A_PRIMARY_QUICKBAR_SLOT1 + i, |k| codes.contains(&k))).last() {
                  // an empty slot changes nothing (FUN_140698b20 returns -1 for a slot without items: no focus change, build mode stays; see the live path)
                  let mut st = hud_state(&self.sim, &self.settings, &self.hud_inputs);
                  if st.occupied()[i] { st.select(i); self.hud_inputs.selected = st.selected; ai.select = Some(st.selected); }
              } }
            if ai.select.is_some() { self.build_input.on_primary_slot_pressed(); } // a quickbar slot key focuses the Primary bar (as live)
            { let b = &self.backend.input.bindings; let codes: Vec<u16> = keys.iter().flat_map(|&a| b.get(a)).filter_map(|k| k.poll_code()).collect(); self.build_input.update(b, |k| codes.contains(&k), self.settings.game.reset_building_choice, dt); }
            self.build_input.end_frame();
            if !self.build_input.last_intents().is_empty() { eprintln!("[build-keys] frame {f}: {:?} (building {}, editing {})", self.build_input.last_intents(), self.build_input.is_building(), self.build_input.is_editing()); }
            if self.build_input.is_building() || self.build_input.is_editing() { inp.attack = false; } // in build mode Fire places a piece, in edit mode it selects tiles (EditModeInputComponent0): it never swings
            // GAMEPAD LOOK: the scripted stick through the same PadLook tick as live play (targeting state of the previous step, like UE's input-before-tick)
            let pad_turn = pad_axes.filter(|_| !cursor_mode).map(|(x, y)| {
                let fov = if self.sim.ads.fov > 0. { self.sim.ads.fov } else { self.sim.ads.base_mode().fov };
                let t = self.pad_look_step(crate::pad::axis_i16(x), crate::pad::axis_i16(y), dt, fov, inp.forward != 0. || inp.side != 0.);
                eprintln!("[pad] frame {f}: stick ({x:+.3}, {y:+.3}) at FOV {fov:.2} accel {:?} -> yaw {:+.4} pitch {:+.4} deg (yaw before {:.4}, pitch before {:.4})", self.pad_look.look.accel, t.0, t.1, self.sim.yaw, self.sim.pitch);
                t
            }).unwrap_or((0., 0.));
            self.drive_arms(dt, &mut inp, ai);
            inp.yaw_delta += pad_turn.0; inp.pitch_delta += pad_turn.1;
            if !inv_steps.is_empty() { self.script_inventory(inv_steps); }
            crate::statics_render::prof("sim step", || self.sim.step(&inp, dt)); // PERF PROFILE (FN_PERF_PROF=1)
            crate::demolition_hook::tick(&mut self.sim);
            crate::foot_fx::on_frame(&self.sim, dt); // FOOT FX: walk / run / land dust of PlayerPawn_Athena_Generic (foot_fx.rs)
            self.gp_audio.update(self.backend.audio.as_mut(), &self.sim, dt);
            crate::statics_render::prof("pickups", || self.step_pickups(dt)); // PICKUPS HOOK
            return true;
        }
        self.input.begin_frame(); // wheel notches since the last frame become one-frame presses
        let (dz_left, dz_right) = fn_settings::extras::deadzones(&self.settings);
        self.pad_state = self.pad.poll(dt).map(|s| fn_game::pad_layout::apply_deadzones(s, dz_left, dz_right));
        if ui.menu.is_open() || (self.console.console.is_open() || crate::chat_hook::is_open()) { self.input.pad_mapper.suppress(self.pad_state.as_ref()); self.input.set_pad_actions(&self.backend.input.bindings, &[]); }
        if ui.menu.is_open() { gpad::menu_opened(); return false; } // the menu pauses the simulation (no input, animation, combat, camera or HUD ticks)
        // INVENTORY FIX2: no pause. In 1.8 nothing pauses for the inventory: opening it only enters cursor mode (`EnterCursorMode` / `HandleCursorModeChanged`,
        // closing = `HandleClose` -> `EnterCameraMode`); the world keeps running and only the mouse belongs to the panel (see `cursor_mode` below).
        let cursor_mode = ui.inv.panel.open;
        if !self.input.focused { return false; }
        self.input.update_help();
        self.clock.tick(dt);
        // In 1.8 AFortPawn::Tick runs once per rendered frame, so UFortMovementComp_Character::DoJump's frame-delta JumpZ scale sees the
        // display frame time, not the port's fixed 30 Hz sim tick (fn-movement FALLING.md).
        self.sim.controller.component_mut().frame_dt_override = Some(dt);
        let before = self.hud_inputs.selected;
        let mut ai = ArmsInput::default();
        let pad_act = self.pad_actions(dt, cursor_mode);
        let emote_open = crate::emote_hook::picker_open();
        crate::emote_hook::set_input_owned(emote_open);
        self.sim.ext_block_app = if crate::emote_hook::with_local(|l| l.playing.is_some()) { Some("emoting") } else if self.build_input.is_editing() { Some("entered edit mode") } else { None };
        if emote_open {
            let (vw, vh) = self.gfx.as_ref().map_or((1280, 720), |g| g.size());
            let (mx, my) = self.input.take_mouse();
            crate::emote_hook::picker_mouse(mx as f32, my as f32, vw as f32 * 0.5, vh as f32 * 0.5);
            let pad = self.pad_state.as_ref().map(|s| { let (x, y) = gpad::radial_deadzone(s.rx, s.ry, gpad::RIGHT_DEADZONE); (s.buttons & gpad::A != 0, s.rt >= gpad::TRIGGER_ON, s.buttons & gpad::B != 0, s.buttons & fn_game::pad_layout::DPAD_DOWN != 0, (x, y)) });
            let shoulder = self.pad_state.as_ref().map_or(0, |s| crate::emote_hook::edge(20, s.buttons & gpad::RB != 0) as i32 - crate::emote_hook::edge(21, s.buttons & gpad::LB != 0) as i32);
            let k = crate::emote_hook::picker_keys(|c| self.input.raw_down(c), self.input.slot_wheel(&self.backend.input.bindings) + shoulder, pad);
            crate::emote_hook::apply_picker_keys(&k, self.sim.feet(), fn_ui::layout::dpi_scale(vw as f32, vh as f32));
        } else { crate::emote_hook::picker_keys(|c| self.input.raw_down(c), 0, None); }
        if emote_open {
        } else if let Some(i) = self.input.quickbar_press(&self.backend.input.bindings) {
            // only occupied slots can be selected (HudState::select)
            let mut st = hud_state(&self.sim, &self.settings, &self.hud_inputs); st.select(i); self.hud_inputs.selected = st.selected;
            ai.select = Some(st.selected);
            // BUILD INPUT HOOK: HotbarKey1..N focus the Primary bar (leaves build mode). HUD FIX: only a slot holding an item: HotbarKeyN (e.g. FUN_14083a270)
            // -> FUN_14067ba90 -> FUN_140698930 -> FUN_140698b20, which returns -1 without touching either bar's focus when the slot has no item.
            // FUN_140698b20 also needs the slot item's vfunc 0x2f8 (`UFortWorldItem::vfunc_95` @1406bc4f0) true: the item entry is live (vfunc_84 @1406bc5d0 ->
            // FUN_14068e9e0, the owning controller's inventory check: always true for the local player's own items) and its definition is a
            // UFortWorldItemDefinition whose vfunc_126 (FUN_1406c4d50) returns `bSupportsQuickbarFocus` (+0x284 bit 0, set by the UFortWorldItemDefinition ctor
            // @1406a5bc0). Every cooked /Game/Athena/Items definition keeps it true except the six AmmoData items, which never sit on the quickbar: not ported.
            if st.occupied()[i] { self.build_input.on_primary_slot_pressed(); }
        } else if let Some(i) = fn_weapons::inventory_model::cycle_slot(&hud_state(&self.sim, &self.settings, &self.hud_inputs).occupied(), self.hud_inputs.selected, if self.build_input.is_building() { 0 } else { self.input.slot_wheel(&self.backend.input.bindings) }) { // BUILD INPUT HOOK: in build mode SlotUp/SlotDown cycle the focused (building) bar
            self.hud_inputs.selected = i; ai.select = Some(i); if self.build_input.is_editing() { self.build_input.on_primary_slot_pressed(); } // INVENTORY HOOK: SlotUp / SlotDown (mouse wheel) = the next / previous occupied slot, wrapping
        } else if pad_act.toggle_harvest {
            let target = if self.hud_inputs.selected != 0 || self.build_input.is_building() { 0 } else { self.input.pad_last_weapon };
            if self.hud_inputs.selected != 0 { self.input.pad_last_weapon = self.hud_inputs.selected; }
            let mut st = hud_state(&self.sim, &self.settings, &self.hud_inputs);
            if st.occupied().get(target).copied().unwrap_or(false) { st.select(target); self.hud_inputs.selected = st.selected; ai.select = Some(st.selected); self.build_input.on_primary_slot_pressed(); }
        }
        let equip = (self.hud_inputs.selected != before && self.hud_inputs.selected == 0) || self.sim.frames == 0;
        let pf = self.pad_state.as_ref().map(gpad::map).unwrap_or_default();
        let open_menu = gpad::start_released(pf.menu);
        // mouse look uses the FOV of the last composed view (`GetFOVAngle()` in UFortPlayerInputAthena vfunc_71 @1404392c0); the base mode's before the first compose
        let look_fov = if self.sim.ads.fov > 0. { self.sim.ads.fov } else { self.sim.ads.base_mode().fov };
        self.sim.crouch_toggle = self.backend.input.crouch_mode == fn_settings::CrouchMode::Toggle;
        self.sim.sprint_toggle = self.backend.input.sprint_toggle;
        let mut inp = self.input.gather(&self.backend.input.bindings, &self.backend.input, &pf, dt, look_fov);
        if if self.sim.crouch_toggle { pad_act.crouch_press } else { pad_act.crouch } { inp.crouch_key = true; }
        if inp.reset && !self.input.raw_down(keys::VK_R) { inp.reset = false; }
        ai.reload_press = self.input.reload_edge(&self.backend.input.bindings);
        inp.equip = equip;
        if self.console.console.is_open() || crate::chat_hook::is_open() || crate::mp_hook::local_dead() || crate::death_hook::local_dying() { inp = FrameInput::default(); ai = ArmsInput::default(); }
        if crate::mp_hook::local_knocked() { ai = ArmsInput::default(); inp.attack = false; } // CONSOLE HOOK: typing never moves / fires / selects
        // INVENTORY FIX2: cursor mode: the mouse buttons / wheel / look go to the panel (Slate handles the pointer), the keyboard still drives the pawn
        let map_open = self.full_map_open();
        cursor_mode_mask(cursor_mode || map_open, &mut inp, &mut ai);
        if emote_open { inp.attack = false; inp.yaw_delta = 0.; inp.pitch_delta = 0.; ai.reload_press = false; ai.fire_held = false; }
        // GAMEPAD LOOK: the right stick through FUN_140615dd0 (pad.rs PadLook); not while typing in the console or in cursor mode (like the mouse look)
        let pad_turn = if emote_open { (0., 0.) } else if self.pad_state.is_some() && !(self.console.console.is_open() || crate::chat_hook::is_open()) && !cursor_mode && !map_open { let m = inp.forward != 0. || inp.side != 0.; self.pad_look_step(pf.look.0, pf.look.1, dt, look_fov, m) } else { (0., 0.) };
        if let Some(arms) = self.arms.as_mut() { crate::console_hook::tick(&self.console.console.flags, arms); } // CONSOLE HOOK: god / infinite ammo
        if !(self.console.console.is_open() || crate::chat_hook::is_open()) && !self.sim.items_blocked() { let (b, inp_state) = (&self.backend.input.bindings, &self.input); self.build_input.update(b, |k| inp_state.poll(k), self.settings.game.reset_building_choice, dt); } // BUILD INPUT HOOK (build_input.rs)
        if self.sim.items_blocked() && (self.build_input.is_building() || self.build_input.is_editing()) { self.build_input.exit_build_mode(); }
        self.build_input.end_frame();
        if self.build_input.is_building() || self.build_input.is_editing() { inp.attack = false; } // BUILD INPUT HOOK: in build mode Fire belongs to BuildPreviewModeInputComponent (places a piece), in edit mode to EditModeInputComponent0 (tile select): it never swings
        if inp.attack && self.sim.ext_block_app.is_none() { self.sim.ext_block_app = Some("firing"); }
        if self.arms.is_some() { self.drive_arms(dt, &mut inp, ai); }
        else if self.hud_inputs.selected != 0 { inp.attack = false; } // preview items (no loadout resources) must never secretly execute pickaxe combat
        inp.yaw_delta += pad_turn.0; inp.pitch_delta += pad_turn.1; // GAMEPAD LOOK (after the mouse-only ADS multiplier of drive_arms)
        crate::statics_render::prof("sim step", || self.sim.step(&inp, dt)); // PERF PROFILE (FN_PERF_PROF=1)
        crate::demolition_hook::tick(&mut self.sim);
        crate::foot_fx::on_frame(&self.sim, dt); // FOOT FX: walk / run / land dust of PlayerPawn_Athena_Generic (foot_fx.rs)
        self.gp_audio.update(self.backend.audio.as_mut(), &self.sim, dt);
        crate::statics_render::prof("pickups", || self.step_pickups(dt)); // PICKUPS HOOK: world items (spawn / toss / Use / touch / delivery)
        if open_menu && !(self.console.console.is_open() || crate::chat_hook::is_open()) { self.open_menu(); }
        true
    }
    fn play_frame(&mut self, el: &ActiveEventLoop, raw_dt: f32, frame_start: Instant) {
        if let Some(t0) = WARM_T0.with(|t| t.get()) {
            let pending = self.scene.as_mut().map_or(0, |s| s.renderer.fx().pending_pipelines());
            let gpu_done = WARM_GPU_DONE.load(std::sync::atomic::Ordering::Relaxed);
            if (!gpu_done || pending > 0) && t0.elapsed().as_secs_f32() < 4. {
                let fade = f32::from_bits(LAST_FINISH_FADE.load(std::sync::atomic::Ordering::Relaxed));
                self.loading_page(1., "Starting", fade);
                return;
            }
            WARM_T0.with(|t| t.set(None));
            eprintln!("[load] warm-up GPU work done after {:.0} ms (pipelines pending {pending})", t0.elapsed().as_secs_f32() * 1000.);
        }
        if self.cap.is_some() { self.flow.tick(raw_dt); } // the start-up capture's `StageTime(Playing, ..)` conditions need the flow clock to keep running in the world
        for n in crate::chat_hook::take_renames() { self.apply_display_name(&n); }
        let scripted = self.run.is_some();
        let dt = if let Some(r) = &self.run { 1. / r.driver.fps() } else { raw_dt.min(MAX_DELTA_TIME) };
        if !scripted {
            // gameplay starts captured; capture again after focus returns / the menu closed, unless the menu is open
            if self.input.started && self.input.focused && !self.input.captured && !self.input.user_released && !self.menu_open() && !self.inventory_open() && !self.storm.game.cursor_mode() { self.try_capture(); } // INVENTORY HOOK: never re-grab the mouse under the open inventory screen
            if self.storm.game.cursor_mode() && self.input.captured { self.release_mouse(); }
            if self.input.captured { if let Some(p) = self.platform.as_ref() { p.recenter(); } }
        }
        self.startup_elapsed += dt;
        let work = Instant::now();
        // scripted window resizes happen before the frame is built
        if let (Some(run), Some(p)) = (self.run.as_ref(), self.platform.as_ref()) {
            if let Some((w, h)) = run.driver.resize(run.frame) { let _ = p.window.request_inner_size(PhysicalSize::new(w, h)); }
        }
        // RENDER FX HOOK: the FX world (ring + damage numbers) is loaded once from the original paks, after the scene exists
        self.load_fx_world();
        // DISTANCE FIELD HOOK: the cooked mesh distance fields of the real props, once (df_hook.rs)
        if self.scene.is_some() && crate::df_hook::pending() { let dir = self.install_dir(); crate::df_hook::frame(self.scene.as_mut().unwrap(), dir, &self.roots.roots); }
        if self.scene.is_some() { let dir = if crate::light_fn_hook::pending() { self.install_dir() } else { None }; crate::light_fn_hook::frame(&mut self.scene.as_mut().unwrap().renderer, dir); } // LIGHT FUNCTION HOOK
        if self.scene.is_some() { let dir = if crate::local_lights_hook::pending() { self.install_dir() } else { None }; crate::local_lights_hook::frame(&mut self.scene.as_mut().unwrap().renderer, dir, &self.sim.world.props); }
        if self.scene.is_some() && crate::sky_refl_hook::pending() { let dir = self.install_dir(); crate::sky_refl_hook::frame(&mut self.scene.as_mut().unwrap().renderer, dir); } // SKY REFLECTION HOOK: the sky light's processed specular cube
        // STORM HOOK: the wall + cloud assets are loaded once from the original paks (when the storm is on), after the scene exists
        if self.scene.is_some() && self.storm.wants_load(&self.sim.storm, self.sim.drop.is_some()) { let dir = self.install_dir(); self.storm.load(dir, self.scene.as_mut().unwrap().renderer.fx(), &self.sim.storm); }
        let stepped = self.update_sim(dt);
        let world_dt = if stepped { dt } else { 0.0 }; self.world_time += world_dt; // RENDER FX HOOK: world time (UE CurrentWorldTime pauses / dilates; no time dilation in the port)
        if self.run.is_some() && fn_live::var_os("FN_TOD_SPEED").is_none() { fn_render::time_of_day::set_speed(0.0); } // scripted runs keep a fixed clock (deterministic captures) unless FN_TOD_SPEED is set
        fn_render::time_of_day::tick(world_dt); // TOD LIVE: the TOD manager's clock advance (exe default speed 80; FN_TOD_SPEED overrides), read by every renderer upload this frame
        if stepped { let dir = self.install_dir(); self.build_input.set_interact_build_blocked(self.sim.swing.blocks_interact_build()); crate::building_hook::frame(&mut self.sim, &mut self.build_input, dir, dt); } // EDIT GATE: the pickaxe combo blocks InteractBuild (build_input::try_interact_build, vfunc_669 @14083e780). BUILDING HOOK (core): build intents, preview, placing, build-up, piece collision (building_hook.rs)
        if stepped { for e in crate::trap_hook::frame(&mut self.sim, &mut self.build_input, self.arms.as_mut(), dt) { self.backend.audio.event(e); } }
        if stepped { for e in crate::bounce_hook::frame(&mut self.sim, dt) { self.backend.audio.event(e); } }
        if stepped { for e in crate::rocket_hook::frame(&mut self.sim, self.arms.as_mut(), dt) { self.backend.audio.event(e); } }
        crate::gunfx_hook::set_listener(crate::scene::pawn_matrix(&self.sim).w_axis.truncate() + glam::Vec3::Y * 0.9, self.sim.cam_view.forward);
        for e in crate::gunfx_hook::take_sounds() { self.backend.audio.event(e); }
        for e in crate::bus_hook::frame(&mut self.sim, dt) { self.backend.audio.event(e); }
        for e in self.storm.game.frame(&mut self.sim, self.arms.as_mut()) { self.backend.audio.event(e); }
        if self.sim.drop.is_some() || crate::mp_hook::squad_play() { let dir = self.install_dir(); crate::skydive_anim::ensure_loading(|| dir); }
        for (n, im) in crate::trap_hook::take_ui_images() { self.overlay.insert(&n, im); }
        if self.live.is_some() { self.live_sample(raw_dt); }
        let pad_available = self.pad.available();
        let (pad_connected, pad_last) = (self.pad.connected, self.pad.last);
        let (Some(gfx), Some(ui), Some(scene), Some(platform)) = (self.gfx.as_mut(), self.ui.as_mut(), self.scene.as_mut(), self.platform.as_mut()) else { return };
        if self.run.is_none() && self.input.focused { ui.pad_to_menu(self.pad_state.as_ref(), dt); }
        let (w, h) = gfx.size();
        let fade_len = if scripted { 0. } else { FADE_IN_SECONDS };
        let fade = if fade_len > 0. { (1. - self.startup_elapsed / fade_len).clamp(0., 1.) } else { 0. };
        // pose + camera
        scene.show_pickaxe = self.hud_inputs.selected == 0;
        scene.held = self.arms.as_ref().and_then(|a| a.gun_index().map(|g| (g, a.weapon_clip().map(|(_, c, t)| (c, t)))));
        if self.sim.items_blocked() || self.sim.controller.enhanced.as_ref().is_some_and(|e| e.holster_weapon()) { scene.show_pickaxe = false; scene.held = None; }
        if crate::emote_hook::local_holstered() || crate::death_hook::local_holstered(self.arms.as_ref().and_then(|a| a.data.death.as_ref())) { scene.show_pickaxe = false; scene.held = None; }
        if crate::death_hook::local_hidden() { self.sim.cam_view.hide_pawn = true; }
        crate::loader::cosmetic::tick(&mut self.sim, scene); // COSMETIC SWAP HOOK: pending skin / pickaxe changes (background rebuild, atomic install before the pose update)
        crate::build_anim::pre_scene(scene); // BUILD ANIM HOOK: the blueprint replaces the pickaxe / firearm in the hands
        crate::statics_render::prof("scene update", || scene.update(&mut self.sim));
        crate::build_anim::frame(scene, &self.sim);
        crate::enhanced_anim_load::frame(scene);
        crate::skydive_anim::frame(scene, &self.sim, world_dt); // BUILD ANIM HOOK: paper on BluePrintPaper, pencil on BluePrintPen (this frame's final pose)
        crate::mp_hook::frame(&mut self.sim, scene, self.arms.as_mut(), dt);
        for e in crate::bots_hook::frame(&mut self.sim, scene, self.arms.as_mut(), &self.storm.game, world_dt) { self.backend.audio.event(e); }
        crate::enhanced_sound::frame(self.backend.audio.as_mut(), &self.sim, world_dt);
        crate::platform::timed("pickups frame", || crate::pickups_hook::frame(scene)); // PICKUPS HOOK: world items' renderer models (FIRST USE: [slow] when a first draw builds)
        let mut camera = camera_for(&self.sim, w, h);
        crate::cam_pose::apply(&mut camera, self.run.as_ref().map(|r| r.frame)); // UE POSE CAMERA (dev): FN_CAM_UE
        crate::enhanced_anim_load::orbit(&mut camera, &self.sim);
        let hash = pose_hash(scene.character.body_local());
        if let Some(run) = self.run.as_mut() { let f = run.frame; run.log_frame(f, &self.sim, Some(hash)); }
        // ui
        ui.begin();
        let menu_open = ui.menu.is_open();
        self.hud_inputs.frame = self.clock.stats();
        { let o = crate::storm_hook::offset_cm(); self.hud_inputs.level_offset_cm = [o[0], o[1]]; } // MINIMAP FIX: the minimap maps the Athena_Terrain level point (fn_hud_adapter::minimap_uv)
        self.hud_inputs.teammates = crate::mp_hook::team_hud();
        self.hud_inputs.team_map = crate::mp_hook::team_map();
        let mut st = hud_state(&self.sim, &self.settings, &self.hud_inputs);
        if let Some(me) = st.team.first_mut() { me.dbno = crate::mp_hook::local_knocked(); me.reviving = crate::mp_hook::local_being_revived(); me.dead = crate::mp_hook::local_dead(); }
        crate::bus_hook::hud(&self.sim, &mut st, self.backend.input.bindings.get(fn_settings::A_JUMP)[0].name());
        st.world.radial = crate::emote_hook::picker_view(self.pad_state.is_some());
        self.storm.game.hud(&self.sim, &mut st);
        st.game_over = self.storm.game.game_over(self.arms.as_ref().map_or(true, |a| a.health > 0.));
        st.spectating = crate::mp_hook::spectating_for();
        st.spectate_vitals = crate::mp_hook::spectate_vitals();
        self.storm.game.set_cursor_mode(st.game_over.is_some());
        st.action_alert = self.storm.game.take_alert().map(|a| (a, String::new()));
        st.reticle_status = self.build_input.take_status();
        crate::kill_feed_hook::set_local_name(&self.hud_inputs.player_name);
        if crate::kill_feed_hook::take_zero_vitals() { if let Some(a) = self.arms.as_mut() { a.health = 0.; a.shield = 0.; } }
        st.kill_feed = crate::kill_feed_hook::frame(self.world_time, self.arms.as_ref().map(|a| a.health));
        crate::chat_hook::decorate(&mut st.kill_feed, self.start.elapsed().as_secs_f32());
        st.kills = st.kills.max(crate::kill_feed_hook::kills());
        st.world.current_resource = self.build_input.hud_resource(); ui.hud.build.state = self.build_input.hud(&self.backend.input.bindings); ui.hud.build.shown = true; // BUILD HUD HOOK: PackResource highlight + AthenaQuickbarSecondary
        let harvest_frame = crate::harvest_fx::feed(&mut self.sim, &mut st, &camera, w as f32, h as f32); // HARVEST HOOK: strike events, reticle colour, Target Info
        st.world.building_prompt = st.world.target.and_then(|t| crate::building_hud::prompt(&self.sim, &self.backend.input.bindings, t.interaction)); st.world.ranged_targeting = crate::building_hud::ranged_targeting(); // BUILDING HUD: BuildingInfoIndicator interaction prompts
        { let key = self.backend.input.bindings.get(fn_settings::A_USE)[0].name().to_owned(); let full = st.backpack.0 >= st.backpack.1; if let Some((key, text, at, card)) = crate::pickups_hook::prompt(self.arms.as_ref(), &self.sim, &key, full) { st.world.interaction = Some(fn_ui::screens::reticle_state::InteractionPrompt { key, text, screen: crate::harvest_fx::project(&camera, at, w as f32, h as f32), pickup: Some(card) }); } } // PICKUPS HOOK: "Pick up" / "Swap equipped" of the pickup in front of the player
        st.world.picked_up = crate::pickups_hook::take_picked_up();
        if st.world.interaction.is_none() { let key = self.backend.input.bindings.get(fn_settings::A_USE)[0].name().to_owned(); if let Some((key, text, at)) = crate::door_hook::prompt(&key) { st.world.interaction = Some(fn_ui::screens::reticle_state::InteractionPrompt { key, text, screen: crate::harvest_fx::project(&camera, at, w as f32, h as f32), pickup: None }); } } // DOOR HOOK: "Open" / "Close" of the focused door (Fort.Building.Door.Defaults)
        if st.world.interaction.is_none() { let key = self.backend.input.bindings.get(fn_settings::A_USE)[0].name().to_owned(); if let Some((key, text, at, pct)) = crate::chest_hook::prompt(&key) { st.world.interaction = Some(fn_ui::screens::reticle_state::InteractionPrompt { key, text, screen: crate::harvest_fx::project(&camera, at, w as f32, h as f32), pickup: None }); st.world.interaction_hold = Some(pct); } } // CHEST HOOK: "Search" + the KeybindWidget hold ring
        if st.world.interaction.is_none() { let key = self.backend.input.bindings.get(fn_settings::A_USE)[0].name().to_owned(); if let Some((key, text, at)) = crate::supply_drop_hook::prompt(&key) { st.world.interaction = Some(fn_ui::screens::reticle_state::InteractionPrompt { key, text, screen: crate::harvest_fx::project(&camera, at, w as f32, h as f32), pickup: None }); } }
        st.world.health_bars = crate::supply_drop_hook::health_bars().into_iter().filter_map(|(at, hp, max)| crate::harvest_fx::project(&camera, at, w as f32, h as f32).map(|screen| fn_ui::screens::health_bar::HealthBarView { screen, health: hp, max_health: max, display_text: String::new() })).collect();
        st.world.team_indicators = crate::mp_hook::team_indicator_points().into_iter().filter_map(|(name, at, dbno, reviving)| crate::harvest_fx::project(&camera, at, w as f32, h as f32).map(|anchor| fn_ui::screens::team_indicator_cooked::TeamIndicator { name, anchor, dbno, reviving })).collect();
        if st.world.interaction.is_none() { let key = self.backend.input.bindings.get(fn_settings::A_USE)[0].name().to_owned(); if let Some((key, text, at, pct)) = crate::mp_hook::revive_prompt(&key) { st.world.interaction = Some(fn_ui::screens::reticle_state::InteractionPrompt { key, text, screen: crate::harvest_fx::project(&camera, at, w as f32, h as f32), pickup: None }); st.world.interaction_hold = Some(pct); } }
        st.slot_keys = std::array::from_fn(|i| fn_settings::Bindings::action_index(&format!("PrimaryQuickbarSlot{}", i + 1)).map(|a| self.backend.input.bindings.get(a)[0]).filter(|k| *k != fn_settings::Key::NONE).map_or(String::new(), |k| k.name().to_owned()));
        st.backpack_key = toggle_inventory_keys(&self.backend.input.bindings)[0].name().to_owned(); // INVENTORY FIX2: the backpack KeybindWidget shows the bound ToggleInventory key
        if !menu_open && fn_live::var_os("FN_HUD_OFF").is_none() { ui.present_hud(&st, w as f32, h as f32, if stepped { Some(dt) } else { None }); } else { ui.hud.set_state(&st); }
        crate::harvest_fx::observe_hud(self.sim.frames, &ui.hud); // HARVEST HOOK: run log of the Target Info bar
        if ui.inv.panel.open && !menu_open { let v = match self.arms.as_ref() { Some(arms) => ui.inv.view_live(arms, [self.sim.world.wood, self.sim.world.stone, self.sim.world.metal.max(self.hud_inputs.metal)]), None => ui.inv.view(&st) }; ui.inv.panel.set_view(v); ui.present_inventory(w as f32, h as f32); } // WIDGETS3 HOOK: AthenaInventoryPanel over the HUD
        if self.console.console.is_open() && !menu_open { let v = self.console.console.view(crate::console_ui::LOG_LINES); ui.present_console(&v, w as f32, h as f32, self.start.elapsed().as_secs_f32()); } // CONSOLE HOOK: the F2 command bar
        if self.input.help_visible && !menu_open { ui.help(w as f32, h as f32); }
        let reqs = {
            let mut cx = MenuContext::new(&mut self.settings, platform, &mut self.backend).with_pad(ui::pad_status(pad_available, pad_connected, pad_connected.then_some(pad_last)));
            ui.menu_frame(&mut cx, w as f32, h as f32, raw_dt)
        };
        ui.black(w as f32, h as f32, fade);
        if platform.vsync_dirty { gfx.set_vsync(platform.vsync); platform.vsync_dirty = false; }
        // material / FX time = WORLD time (UE View.Family->CurrentWorldTime, ShaderBaseClasses.cpp:287), not the wall clock; RealTime = time since start
        let real_time = if let Some(r) = &self.run { r.frame as f32 / r.driver.fps() } else { self.start.elapsed().as_secs_f32() };
        let time = real_time; // (the scene renderer keeps this clock)
        let fx_time = self.world_time;
        let mat_time = fn_fx::uniform::MaterialTime { world: fx_time, real: real_time, delta_world: world_dt };
        let mut options = self.backend.render.options.clone();
        // RENDER FX HOOK: strike events -> ring effects, new damage numbers, then this frame's FX draws for the renderer
        let (tx, ty) = camera.tan_half();
        let view = fn_fx::view::ViewParams { eye: camera.eye, forward: camera.forward, up: camera.up, tan_half: (tx, ty), near_m: camera.near, far_m: camera.far, width: w, height: h, time: fx_time, real_time, delta_world: world_dt }; // SORTFIX: View.GameTime = world, RealTime = real, PrevFrame* = each - DeltaWorldTime (SceneView.cpp:2235-2277)
        crate::pickups_fx::set_world_clock(fx_time, real_time, world_dt); // SORTFIX: the pickup effects tick on world time (paused with the game)
        let mut fx_frame = fn_render::fx::FxFrame::default();
        if let Some(fx) = self.fx.as_mut() {
            fx.events(&harvest_frame.fx_events);
            fx.numbers_from(&self.sim.world.fx, &view, fx_time);
            for (pos, amount, critical, kind) in crate::mp_hook::take_numbers().into_iter().chain(crate::bots_hook::take_numbers()) { fx.push_number(pos, amount, critical, kind, &view, fx_time); }
            fx_frame = fx.frame(&view, world_dt, fx_time); // SORTFIX: the ring advances with WORLD time (UE ticks it with the world delta; paused = frozen)
        }
        let feet = self.sim.feet(); self.storm.frame(&mut self.sim.storm, feet, &view, world_dt, mat_time, &mut fx_frame); // STORM HOOK: the safe-zone wall + storm clouds join the FX frame (storm_hook.rs)
        options.storm_pp_weight = self.storm.pp_weight(); // POSTFX: OutsideSafeZonePP0 BlendWeight (storm_hook.rs)
        options.scope_pp_weight = crate::scope_hook::amount(); // POSTFX: DownScopePostProcess BlendWeight (scope_hook.rs)
        for e in crate::building_render::demo_frame(scene.renderer.fx(), feet, &view, world_dt, &mut fx_frame) { self.backend.audio.event(e); } // BUILDING RENDER HOOK: verification scene (FNRT_BUILDING_DEMO only)
        if std::env::var_os("FNRT_BUILDING_DEMO").is_none() { // BUILDING RENDER HOOK: the core's world (building_hook) + its preview marker + the edit session -> FX draws + sounds
            let pv = crate::building_hook::preview().map(|p| crate::building_render::Preview { class_path: p.class_path, location: p.location, yaw_deg: p.yaw_deg, mirrored: p.mirrored, valid: p.valid });
            let ev = crate::building_hook::edit_visuals();
            let off = crate::building_hook::world_to_fx(glam::Vec3::ZERO);
            let snd = crate::building_hook::with_world(|w| crate::platform::timed("building frame", || crate::building_render::frame(scene.renderer.fx(), w, off, pv.as_ref(), ev.as_ref(), &view, world_dt, &mut fx_frame)));
            let log_snd = fn_live::var("FN_BUILD_SOUND_LOG").map_or(false, |v| v == "1");
            for e in snd { if log_snd { eprintln!("[build-sound] {e:?}"); } self.backend.audio.event(e); }
            for c in crate::building_hook::drain_build_sound_cues().into_iter().chain(crate::building_hook::drain_edit_sound_cues()) { if let Some(e) = crate::building_render::edit_cue_event(c) { if log_snd { eprintln!("[build-sound] {e:?}"); } self.backend.audio.event(e); } }
        }
        crate::foot_fx::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame); // FOOT FX: walk / run / land dust on their cooked materials (foot_fx.rs)
        crate::heal_fx::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame); // HEAL FX: P_Consume_Item bursts on their cooked materials (heal_fx.rs)
        crate::chest_hook::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame);
        crate::supply_drop_hook::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame); // CHEST HOOK: closed-chest mist / open bursts on their cooked materials
        crate::trap_hook::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame);
        crate::bounce_hook::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame);
        crate::rocket_hook::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame);
        crate::gunfx_hook::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame);
        crate::bus_hook::fx_frame(scene.renderer.fx(), &view, &self.sim, &mut fx_frame);
        crate::elim_fx::fx_frame(scene.renderer.fx(), &view, world_dt, &mut fx_frame);
        crate::backdrop_real::frame(scene.renderer.fx(), &view, &mut fx_frame); // BACKDROP REAL: far terraces / rocks, real cooked material, backdrop pass
        scene.renderer.occlusion_pickup(); // OCCLUSION: readbacks completed during this frame's CPU work
        crate::statics_render::set_lod_distance_factor(fn_render::quality::lod_distance_factor(camera.fov_deg)); crate::statics_render::set_view_distance_scale(options.view_distance_scale); // LOD 1:1: View.LODDistanceFactor, r.ViewDistanceScale
        crate::statics_render::prof("statics frame", || crate::statics_render::frame(scene.renderer.fx(), &self.sim.world.props, scene.real.as_ref(), &view, &mut fx_frame)); // STATIC MATERIALS: real-shader props (statics_render.rs)
        crate::statics_render::prof("splines frame", || crate::spline_mesh::frame(scene.renderer.fx(), &view, &mut fx_frame)); // ROADS / SPLINES: the spline meshes (spline_mesh.rs)
        crate::statics_render::prof("grass frame", || crate::statics_render::grass_frame(scene.renderer.fx(), scene.grass.as_ref(), &view, options.view_distance_scale, &mut fx_frame)); // GRASS: the ground cover on its real cooked material (grass_render.rs)
        { let lb = scene.renderer.landscape_terrain_bounds(); let on = crate::statics_render::prof("landscape frame", || crate::landscape_render::frame(scene.renderer.fx(), &view, &lb, &mut fx_frame)); scene.renderer.set_landscape_main_suppressed(on); } // LANDSCAPE MATERIAL: the main-pass landscape on its cooked material (landscape_render.rs)
        crate::mpc_hook::frame(scene.renderer.fx(), real_time, &mut fx_frame); // MATERIAL PARAMETERS: FortniteMaterialParameters (mpc_hook.rs)
        scene.renderer.set_fx_frame(fx_frame);
        let cpu_ms = work.elapsed().as_secs_f32() * 1000.;
        let mut shots: Vec<(PathBuf, Vec<u8>, (u32, u32))> = vec![];
        let had_shot_names = self.run.as_mut().map(|r| { let f = r.frame; r.driver.shots(f, &self.sim).len() }).unwrap_or(0);
        let had_shot = had_shot_names > 0;
        { let (vw, vh) = fn_render::quality::scaled_view_size(camera.width, camera.height, options.screen_percentage); crate::outline_hook::submit(&mut scene.renderer, scene.real.as_ref(), &self.sim.world.props, &fn_render::CameraParams { width: vw, height: vh, ..camera }, time); crate::decals_hook::submit(&mut scene.renderer, &fn_render::CameraParams { width: vw, height: vh, ..camera }, self.world_time); } // OUTLINE HOOK: the build preview highlight (custom depth + SkinAndMeshOutlinePostProcess)
        if cfg!(target_arch = "wasm32") && self.run.is_none() && !PLAY_WARMED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let t_warm = Instant::now();
            let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
            scene.draw(&camera, time, dt, &options, &target.view);
            ui.render_texture(&target.texture, w, h, None);
            gfx.gpu.queue.on_submitted_work_done(|| WARM_GPU_DONE.store(true, std::sync::atomic::Ordering::Relaxed));
            WARM_T0.with(|t| t.set(Some(Instant::now())));
            eprintln!("[load] first play frame drawn offscreen as a warm-up: {:.0} ms", t_warm.elapsed().as_secs_f32() * 1000.);
            return;
        }
        match gfx.acquire() {
            Acquire::Frame(frame) => {
                let view = frame.texture.create_view(&Default::default());
                // SHOT CAPTURE (2026-10-07): a scripted shot is the FIRST render of its frame (offscreen, real dt), the window frame is the repeat (dt 0). Was: the
                // window first, then the same frame again for the shot, so every temporal effect of the shot (TAA ClipToPrevClip / object velocity, DFAO / SSR
                // previous view, light-shaft history) saw a static camera and an already-resolved history: motion artefacts never showed in a screenshot.
                let shot_names: Vec<String> = self.run.as_mut().map(|r| { let f = r.frame; r.driver.shots(f, &self.sim) }).unwrap_or_default();
                let t_draw = Instant::now();
                if let (Some(name), Some(run)) = (shot_names.first(), self.run.as_ref()) {
                    let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
                    scene.draw(&camera, time, dt, &options, &target.view);
                    ui.render_texture(&target.texture, w, h, None); // INVENTORY FIX3
                    shots.push((run.shot_path(name, (w, h)), target.read_rgba8(&gfx.gpu), (w, h)));
                    if fn_live::var_os("FN_EXPOSURE_LOG").is_some() { let e = scene.renderer.read_exposure(); eprintln!("[exposure] shot {name}: exposure {:.4} avg_luminance {:.4} ev {:.4} target {:.4}", e.exposure, e.avg_luminance, e.ev, e.target_ev); } // verification aid
                }
                let batch = cfg!(target_arch = "wasm32");
                if batch { scene.renderer.begin_batch(); }
                scene.draw(&camera, time, if shot_names.is_empty() { dt } else { 0. }, &options, &view);
                if batch { ui.renderer.submit_before_next(scene.renderer.take_batch()); }
                let t_ui = Instant::now();
                ui.render_texture(&frame.texture, w, h, None); // INVENTORY FIX3: the texture itself, for the SBackgroundBlur pass
                if batch { ui.renderer.flush_pending(); scene.renderer.after_batch_submitted(); }
                gfx.present(frame);
                if let Some(run) = self.run.as_mut() { run.draw_ms.push((t_ui - t_draw).as_secs_f32() * 1000.); run.ui_present_ms.push(t_ui.elapsed().as_secs_f32() * 1000.); run.push_gpu(scene.renderer.take_pass_timings()); if let Some(s) = scene.last_stats.as_ref() { run.push_cpu_passes(s); } } // PERF
                if let Some(cap) = self.cap.as_mut() {
                    // start-up capture: the world shot (same frame again into an offscreen target of the swapchain format; dt 0)
                    for name in std::mem::take(&mut cap.pending) {
                        let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
                        scene.draw(&camera, time, 0., &options, &target.view);
                        ui.render(&target.view, w, h, None);
                        let px = target.read_rgba8(&gfx.gpu);
                        let note = write_capture(cap, &name, w, h, &px, self.flow.now);
                        self.flow.note(note);
                        self.stalls.skip_next = true;
                    }
                }
                if let Some(run) = self.run.as_mut() {
                    for name in shot_names.iter().skip(1) {
                        // the same frame again into an offscreen target of the swapchain format (dt 0: the exposure adaptation does not advance twice)
                        let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
                        scene.draw(&camera, time, 0., &options, &target.view);
                        ui.render_texture(&target.texture, w, h, None); // INVENTORY FIX3
                        shots.push((run.shot_path(&name, (w, h)), target.read_rgba8(&gfx.gpu), (w, h)));
                    }
                }
            }
            Acquire::Skip => {}
            Acquire::Fatal(e) => { self.exit_code = 1; eprintln!("error: {e}"); el.exit(); return; }
        }
        let mut finished = false;
        if let Some(run) = self.run.as_mut() {
            for (path, px, (sw, sh)) in shots {
                match run::save_png(&path, sw, sh, &px) { Ok(()) => run.shots.push(path), Err(e) => run.failures.push(e) }
            }
            if run.sizes_seen.last() != Some(&(w, h)) { run.sizes_seen.push((w, h)); }
            let interval = frame_start.duration_since(run.started).as_secs_f32() * 1000.;
            let _ = interval;
            run.cpu.push(cpu_ms);
            run.captured_frame = had_shot;
            run.frame += 1;
            finished = run.done();
        } else {
            self.live_stats.push(raw_dt * 1000.);
        }
        if let Some(run) = self.run.as_mut() { let ms = frame_start.elapsed().as_secs_f32() * 1000.; if run.captured_frame { run.capture_ms.push(ms); } else { run.interval.push(ms); } }
        self.handle_menu_requests(el, reqs);
        if finished { self.finish_run(el); return; }
        #[cfg(target_arch = "wasm32")]
        web_quality::after_frame(self, raw_dt); // BROWSER PORT: auto quality
        // BROWSER PORT: requestAnimationFrame paces the browser build; a limiter there would spin the page's only UI thread
        if self.run.is_none() && !cfg!(target_arch = "wasm32") { let (cap, vsync) = (self.backend.runtime.frame_cap, self.backend.runtime.vsync); self.limiter.throttle(cap, vsync); }
    }

    // ------------------------------------------------------------------------------------------------------------------------------
    // scripted run epilogue
    // ------------------------------------------------------------------------------------------------------------------------------
    fn finish_run(&mut self, el: &ActiveEventLoop) {
        let Some(run) = self.run.take() else { return };
        let out = run.out.clone();
        let _ = std::fs::create_dir_all(&out);
        let name = run_output_name(&run.spec);
        let mut report = format!("fortnite-rs scripted run: {}\nframes {} in {:.2} s, fixed dt {:.5} s, client sizes seen {:?}\nsettings: {}\n", run.spec, run.frame, run.started.elapsed().as_secs_f32(), 1. / run.driver.fps(), run.sizes_seen, self.settings_note.lines().next().unwrap_or(""));
        report.push_str(&format!("gpu: {}\n", self.gfx.as_ref().map(|g| g.describe()).unwrap_or_default()));
        report.push_str(&run.interval.report("frame interval (wall, incl. present)", 5));
        report.push_str(&run.cpu.report("cpu work per frame (sim + pose + ui + encode)", 5));
        report.push_str(&run.draw_ms.report("cpu scene draw (renderer encode + submit)", 5));
        report.push_str(&run.ui_present_ms.report("cpu ui render + present", 5));
        report.push_str(&run.gpu_report()); // PERF
        if !run.capture_ms.ms.is_empty() { report.push_str(&run.capture_ms.report("capture frames (offscreen re-render + read-back, excluded above)", 0)); }
        report.push_str(&format!("captures ({}):\n", run.shots.len())); for s in &run.shots { report.push_str(&format!("  {}\n", s.display())); }
        report.push_str(&format!("load steps:\n{}", self.load_log));
        report.push_str(&crate::harvest_fx::run_report(&out, &name)); // HARVEST HOOK
        report.push_str(&crate::particles::run_report(&out, &name)); // PARTICLE HOOK
        report.push_str(&crate::pickups_fx::report()); report.push_str(&crate::pickups_hook::run_report()); report.push_str(&crate::door_hook::report()); report.push_str(&crate::chest_hook::report()); report.push_str(&crate::floor_loot_hook::report()); report.push_str(&crate::supply_drop_hook::report()); report.push_str(&crate::demolition_hook::report()); // DOOR HOOK // PICKUPS FX / HOOK
        report.push_str(&crate::building_hook::run_report()); // BUILDING HOOK (core)
        report.push_str(&crate::trap_hook::report());
        report.push_str(&crate::bounce_hook::report());
        report.push_str(&crate::rocket_hook::report());
        report.push_str(&crate::gunfx_hook::report());
        report.push_str(&crate::statics_render::report()); // STATIC MATERIALS
        report.push_str(&crate::spline_mesh::report()); // ROADS / SPLINES
        report.push_str(&fn_render::backdrop::run_report()); report.push_str(&crate::backdrop_real::run_report()); // PERF (2026-10-08): backdrop / landscape draw averages
        report.push_str(&crate::mpc_hook::report()); // MATERIAL PARAMETERS
        let mut ok = run.failures.is_empty();
        for f in &run.failures { report.push_str(&format!("FAILURE: {f}\n")); }
        if !run.log.is_empty() && run.log.lines().any(|l| l.contains("NaN") || l.contains("inf")) { ok = false; report.push_str("FAILURE: non-finite values in the frame log\n"); }
        let frames_path = out.join(format!("{name}-frames.txt"));
        let _ = std::fs::write(&frames_path, &run.log);
        report.push_str(&format!("frame log: {}\n", frames_path.display()));
        if self.opts.compare_sim || self.opts.smoke {
            let d = self.compare_with_sim(&run);
            report.push_str(&d.text);
            if self.opts.compare_sim && !d.sim_ok { ok = false; }
        }
        if run.shots.is_empty() && run.driver.name() != "none" && !self.opts.compare_sim { report.push_str("note: the script has no captures\n"); }
        report.push_str(if ok { "RESULT: ok\n" } else { "RESULT: FAILED\n" });
        let rpath = out.join(format!("{name}-report.txt"));
        let _ = std::fs::write(&rpath, &report);
        println!("{report}report: {}", rpath.display());
        self.exit_code = if ok { 0 } else { 1 };
        el.exit();
    }

    fn compare_with_sim(&self, run: &ScriptRun) -> run::Diff {
        #[cfg(feature = "sim-compare")]
        {
            let assets_dir = self.roots.roots.iter().find(|r| r.join("animation").is_dir() || r.join("exported").is_dir()).cloned();
            let pak_mode = matches!(self.source, Source::Paks(_));
            if run.spec == "builtin:pickaxe" {
                // the authoritative fn-sim scenario (with its own rig when the old tree is available)
                let rig = if pak_mode { None } else { assets_dir.as_ref().and_then(|a| fn_sim::rig::Rig::load(a).ok()) };
                fn_sim::pick_script::set_active(true);
                let out = fn_sim::scenarios::pick_run(fn_sim::scenarios::PickMode::Pickaxe, rig.as_ref(), Some(run.end));
                let reference = out.files.iter().find(|(n, _)| n == "pickaxe-test-frames.txt").map(|(_, t)| t.clone()).unwrap_or_default();
                fn_sim::pick_script::set_active(false);
                return run::compare_logs(&run.log, &reference, "fn-sim scenarios::pick_run (PickMode::Pickaxe)");
            }
            let Some(driver) = (if run.spec == "builtin:smoke" || run.spec == "builtin:walk" || run.spec.starts_with("builtin:") { crate::script::load(&run.spec).ok() } else { crate::script::load(&run.spec).ok() }) else { return run::Diff { text: "compare: cannot reload the script\n".into(), sim_ok: false, full_ok: false } };
            let reference = run::reference_log(driver, if pak_mode { None } else { assets_dir.as_deref() }, Some(run.end));
            run::compare_logs(&run.log, &reference, "headless GameSim replay of the same script (fn-sim frame_line format)")
        }
        #[cfg(not(feature = "sim-compare"))]
        { let _ = run; run::Diff { text: "compare: built without the sim-compare feature\n".into(), sim_ok: false, full_ok: false } }
    }
}


// ----------------------------------------------------------------------------------------------------------------------------------
// input events as methods: the winit handler below calls them, and the live self-test (`--live-test`) calls them directly to imitate the events
// (Esc, focus loss / gain, clicks) without any OS-level input injection.
// ----------------------------------------------------------------------------------------------------------------------------------
impl App {
    /// `WindowEvent::Focused`: losing focus drops every held key and the mouse capture, regaining it re-captures (per-frame policy).
    pub fn on_focus(&mut self, focused: bool) {
        if focused { self.input.focus_gained(); self.backend.audio.set_suspended(false); }
        else {
            self.input.focus_lost();
            self.release_mouse();
            if let Some(ui) = self.ui.as_mut() { ui.menu.event(UiEvent::FocusLost); }
            self.picker_event(UiEvent::FocusLost);
            self.backend.audio.set_suspended(true);
        }
    }
    /// A key by virtual-key code. Held keys are always tracked (keys still down when the menu closes keep working); with the menu open the press goes to the
    /// menu instead; Esc opens the menu in gameplay and closes the window during splash / loading.
    pub fn on_key(&mut self, el: &ActiveEventLoop, vk: u16, pressed: bool, repeat: bool) {
        if (self.opts.scripted() || self.opts.startup_capture) && !(vk == keys::VK_ESCAPE && pressed) { return; } // automated runs ignore real keys (stray input cannot leak in)
        match vk { keys::VK_SHIFT_L | keys::VK_SHIFT_R => self.input.shift = pressed, keys::VK_CTRL_L | keys::VK_CTRL_R => self.input.ctrl = pressed, _ => {} }
        self.input.set_key(vk, pressed);
        if self.map_key(vk, pressed, repeat) { return; }
        if self.inventory_key(vk, pressed, repeat) { return; } // WIDGETS3 HOOK: Toggle Inventory / the open inventory screen's keys
        if self.menu_open() && pressed {
            let evs = events::key_down(vk, repeat, self.input.shift, self.input.ctrl);
            if let Some(ui) = self.ui.as_mut() { for e in evs { ui.menu.event(e); } }
            return;
        }
        if self.phase == Phase::GamePicker && pressed {
            // Esc closes the picker; everything else is BR picker navigation (arrows, Enter / Space).
            if vk == keys::VK_ESCAPE { if !repeat { if self.picker.back() { self.flow.note("picker: back to the game list (Esc)"); } else { self.flow.note("Esc on the picker: closing"); el.exit(); } } return; }
            for e in events::key_down(vk, repeat, self.input.shift, self.input.ctrl) { self.picker_event(e); }
            return;
        }
        if self.phase == Phase::Lobby {
            if pressed && !repeat { if let Some(l) = self.lobby.as_mut() { if vk == keys::VK_ESCAPE { l.escape(); } else if vk == keys::VK_RETURN { l.enter(); } } }
            return;
        }
        if vk == keys::VK_RETURN && pressed && !repeat { self.flow.request_skip(); } // skips a fixed hold (splash, start-up minimum, loading minimum, 100 % hold), never real work
        if vk == keys::VK_ESCAPE && pressed && !repeat {
            if self.run.is_some() { el.exit(); return; }
            match self.phase { Phase::Playing => if crate::emote_hook::picker_open() { crate::emote_hook::picker_cancel(); } else { self.open_menu() }, Phase::Splash | Phase::StartupLoading | Phase::LoggingIn | Phase::GamePicker | Phase::Loading | Phase::Finishing | Phase::Failed(_) => { self.flow.note("Esc: closing"); el.exit() }, Phase::Lobby | Phase::Boot => {} }
        }
    }
    /// F11 in the match, like the lobby: `UGameViewportClient::InputKey` @1421a0a70 toggles fullscreen on an F11 press because `bF11TogglesFullscreen=true`
    /// (CookedIniVersion.txt L1978); `bAltEnterTogglesFullscreen=false` (L1977) leaves Alt+Enter an ordinary key. `HandleToggleFullscreenCommand`
    /// (UE 4.16 GameViewportClient.cpp:2795): fullscreen -> Windowed, otherwise the `r.FullScreenMode` mode (cvar default 1 = WindowedFullscreen,
    /// ConsoleManager.cpp:1948; not set in the cooked ini), and the user settings' window mode follows (SetFullscreenMode + ConfirmVideoMode).
    fn toggle_fullscreen(&mut self) {
        let Some(p) = self.platform.as_mut() else { return };
        let windowed = p.window.fullscreen().is_some();
        let mode = if windowed { WindowMode::Windowed } else { WindowMode::WindowedFullscreen };
        let (w, h) = (self.settings.video.width, self.settings.video.height);
        match p.apply_mode(mode, w, h) {
            Ok(()) => { let _ = self.settings.set("video.window_mode", Value::Index(if windowed { 2 } else { 1 })); eprintln!("[window] F11: {mode:?}"); }
            Err(e) => eprintln!("[window] F11 toggle failed: {e}"),
        }
    }
    pub fn on_mouse_button(&mut self, button: winit::event::MouseButton, pressed: bool) {
        if self.opts.scripted() || self.opts.startup_capture { return; }
        if self.console.console.is_open() || crate::chat_hook::is_open() { if let (Some(vk), false) = (keys::mouse_vk(button), pressed) { self.input.set_key(vk, false); } return; } // CONSOLE HOOK: the open bar swallows clicks
        if self.phase == Phase::GamePicker {
            if button == winit::event::MouseButton::Left { let (x, y) = self.input.cursor; self.picker_event(if pressed { UiEvent::MouseDown(UiMouseButton::Left, x, y) } else { UiEvent::MouseUp(UiMouseButton::Left, x, y) }); }
            return;
        }
        if self.phase == Phase::LoggingIn && self.name_entry.is_some() {
            if button == winit::event::MouseButton::Left {
                let (x, y) = self.input.cursor;
                let hit = self.ui.as_ref().and_then(|u| u.sign_in_hit(x, y));
                if let Some(e) = self.name_entry.as_mut() { e.pointer(hit, pressed); }
            }
            return;
        }
        if self.phase == Phase::Lobby {
            if button == winit::event::MouseButton::Left { if let Some(l) = self.lobby.as_mut() { l.pointer_button(pressed); } }
            return;
        }
        if let Some(vk) = keys::mouse_vk(button) { self.input.set_key(vk, pressed); if vk == keys::VK_RBUTTON { self.input.right_button(pressed); } }
        if self.inventory_mouse(button, pressed) { return; } // WIDGETS3 HOOK: clicks / drags on the open inventory screen
        if self.menu_open() {
            let (x, y) = self.input.cursor;
            if pressed { self.backend.audio.event(SoundEvent::MenuClick); } // AUDIO HOOK: a mouse press in the open menu (any button / slider / tab) clicks
            if let (Some(b), Some(ui)) = (events::mouse_button(button), self.ui.as_mut()) { ui.menu.event(if pressed { UiEvent::MouseDown(b, x, y) } else { UiEvent::MouseUp(b, x, y) }); }
        } else if pressed && button == winit::event::MouseButton::Left && self.phase == Phase::Playing && !self.input.captured {
            // click recaptures; the capture click is held back from the fire binding (`gated_attack`)
            self.input.user_released = false;
            self.try_capture();
        }
    }
    fn on_cursor(&mut self, x: f32, y: f32) {
        if self.input.captured { return; } // our own re-centring
        let open = self.menu_open();
        self.input.cursor_moved(x, y, open);
        if let Some(ui) = self.ui.as_mut() { if ui.inv.panel.open { ui.inv.panel.mouse_move(x, y); } } // WIDGETS3 HOOK
        if open { if let Some(ui) = self.ui.as_mut() { ui.menu.event(UiEvent::MouseMove(x, y)); } }
        else if self.phase == Phase::GamePicker && !self.opts.startup_capture { self.picker_event(UiEvent::MouseMove(x, y)); }
        else if self.phase == Phase::Lobby { if let Some(l) = self.lobby.as_mut() { l.pointer_move(x, y); } }
        else if self.phase == Phase::LoggingIn { let hit = self.ui.as_ref().and_then(|u| u.sign_in_hit(x, y)); if let Some(e) = self.name_entry.as_mut() { e.hover(hit); } }
    }
    fn on_wheel(&mut self, notches: f32) {
        if self.opts.scripted() { return; }
        if self.phase == Phase::Lobby { if let Some(l) = self.lobby.as_mut() { l.wheel(notches); } return; }
        if crate::chat_hook::is_open() { return; }
        if self.console.console.is_open() { self.console.console.scroll_by((notches.signum() * 3.) as i32); return; } // CONSOLE HOOK: the wheel scrolls the log
        if let Some(ui) = self.ui.as_mut() { if ui.inv.panel.open { ui.inv.panel.wheel(notches); return; } } // WIDGETS3 HOOK
        if self.menu_open() { if let Some(ui) = self.ui.as_mut() { ui.menu.event(UiEvent::Wheel(notches)); } } else { self.input.push_wheel(notches); }
    }
}

// ----------------------------------------------------------------------------------------------------------------------------------
// WIDGETS3 HOOK: the Athena inventory screen (`AthenaInventoryPanel`, `crate::inventory`)
// ----------------------------------------------------------------------------------------------------------------------------------
/// INVENTORY FIX2: the keys of `ToggleInventory`. Source: `Input:ConfigD_Athena FortPlayerInputSettings:ActionMappings:113=(ActionName="ToggleInventory", Key=I)`
/// (CookedIniVersion.txt line 2740; `ToggleFullScreenMap` = M and Tab; `InputActionDataTable` row `Inventory` PC key I). The bindings are read through
/// `fn_settings::Bindings`; while the settings table still ships its earlier ASSUMED default (Tab, being corrected by the settings owner), an untouched
/// default binding resolves to the cooked I instead of Tab.
/// INVENTORY FIX2 / FIX3: cursor mode, every frame the panel is open (live and scripted runs): the mouse buttons / wheel / look go to the panel (Slate handles
/// the pointer), the keyboard still drives the pawn. The one-time entry effects are `App::enter_cursor_mode`.
fn cursor_mode_mask(cursor_mode: bool, inp: &mut FrameInput, ai: &mut ArmsInput) {
    if cursor_mode { inp.attack = false; inp.yaw_delta = 0.; inp.pitch_delta = 0.; ai.fire_held = false; }
}
pub(crate) fn toggle_inventory_keys(b: &fn_settings::Bindings) -> [fn_settings::Key; 2] {
    use fn_settings::{Bindings, Key};
    let cooked = [Key::from_name("I").unwrap_or(Key::NONE), Key::NONE];
    let Some(a) = Bindings::action_index("ToggleInventory") else { return cooked };
    let k = b.get(a);
    let stale_default = k == Bindings::default().get(a) && k.contains(&Key::from_name("Tab").unwrap_or(Key::NONE));
    if stale_default || k.iter().all(|x| *x == Key::NONE) { cooked } else { k }
}
#[cfg(test)]
mod inventory_key_tests {
    // INVENTORY FIX2: ToggleInventory resolves to the cooked I while the table still carries its old Tab default; an explicit user binding is kept.
    #[test] fn toggle_inventory_is_i_not_tab() {
        use fn_settings::{Bindings, Key};
        let mut b = Bindings::default();
        assert_eq!(super::toggle_inventory_keys(&b)[0], Key::from_name("I").unwrap());
        let a = Bindings::action_index("ToggleInventory").unwrap();
        b.keys[a] = [Key::from_name("K").unwrap(), Key::NONE];
        assert_eq!(super::toggle_inventory_keys(&b)[0], Key::from_name("K").unwrap());
    }
}

impl App {
    fn inventory_open(&self) -> bool { self.ui.as_ref().map_or(false, |u| u.inv.panel.open) }
    /// Open / close the screen: the mouse is released for it (the cursor drives the panel) and recaptured when it closes, like the menu.
    fn toggle_inventory(&mut self) {
        if self.phase != Phase::Playing || self.menu_open() || self.run.is_some() { return; }
        let Some(ui) = self.ui.as_mut() else { return };
        ui.inv.panel.toggle();
        let open = ui.inv.panel.open;
        // INVENTORY FIX: `user_released` keeps `play_frame`'s auto-capture from grabbing the mouse back on the next frame (it did, so the cursor vanished and the panel never saw
        // a move: nothing could be dragged). The panel is a cursor-mode panel (`HandleCursorModeChanged`; `HandleClose` = `FortHUDContext.EnterCameraMode`).
        if open { let (x, y) = self.input.cursor; ui.inv.panel.mouse_move(x, y); self.release_mouse(); self.input.user_released = true; } else { self.input.user_released = false; self.try_capture(); }
        if open { self.enter_cursor_mode(); } // INVENTORY FIX3 (G3)
    }
    /// INVENTORY FIX3 (inventory_gaps G3): the pawn side of the controller's cursor-mode entry `FUN_140824b40` (`UFortHUDContext::EnterCursorMode` 0x142e18200
    /// calls it): `bHoldingSprint` (+0xF16) = 0 and `bWantsToSprint` (+0xF15) = 0 unless `bSprintToggleable` (+0xF17) (`FUN_14084ea60` then removes a leftover
    /// `Gameplay.Action.Player.Sprint` tag: no ability-system tags in the port, the latch is the sprint state); `MyFortPawn` (+0x770) vfunc +0x980 (0) =
    /// `AFortPawn::PawnStopFire` (0x1407f8b80: `bPrimaryInputHeld` = 0, `CurrentWeapon->StopFire`) and vfunc +0x938 (0) = `AFortPawn` vfunc 295 (0x1407f8be0:
    /// `bSecondaryInputHeld` = 0, weapon vfunc 212 -> `AFortWeaponRanged::SetTargeting(false)` while `bIsTargeting`). Vtable slots read from
    /// `AFortPlayerPawnAthena::vftable` (0x144072328) in the 1.8 exe. Stop fire is [`cursor_mode_mask`] (fire / attack are masked every cursor-mode frame,
    /// and the recapture after closing gates the held click, `InputState::mark_captured`). The controller's own calls (vtable +0x14a0 / +0x1380, UI-zone
    /// and input-stack bookkeeping) have no port counterpart.
    fn enter_cursor_mode(&mut self) {
        let toggleable = self.sim.sprint_toggle;
        self.sim.sprint_latch.clear_held(toggleable);
        if let Some(ads) = self.ads.as_mut() { ads.machine.release_targeting(); }
        eprintln!("[inventory] cursor mode: sprint {} (toggle {toggleable}), fire stopped, targeting released", if self.sim.sprint_latch.active() && toggleable { "kept" } else { "cancelled" });
    }
    /// Keys while playing: the `ToggleInventory` binding ([`toggle_inventory_keys`], cooked default I) opens / closes the screen; with it open Esc closes it
    /// (`HandleClose`, or the quantity selector's `Cancel`), E is `Equip` / `Unequip` and X is `Drop` (the panel's input-action rows).
    /// Returns true when the key was consumed.
    fn full_map_open(&self) -> bool { self.ui.as_ref().map_or(false, |u| u.hud.full_map) }
    fn map_key(&mut self, vk: u16, pressed: bool, repeat: bool) -> bool {
        if self.phase != Phase::Playing || self.menu_open() || (self.console.console.is_open() || crate::chat_hook::is_open()) || self.run.is_some() || !pressed || repeat { return false; }
        let Some(a) = fn_settings::Bindings::action_index("ToggleFullScreenMap") else { return false };
        if !self.backend.input.bindings.get(a).iter().filter_map(|k| k.poll_code()).any(|k| k == vk) { return false; }
        if let Some(ui) = self.ui.as_mut() { ui.hud.full_map = !ui.hud.full_map; eprintln!("[map] full screen map {}", if ui.hud.full_map { "opened" } else { "closed" }); }
        true
    }
    fn inventory_key(&mut self, vk: u16, pressed: bool, repeat: bool) -> bool {
        if self.phase != Phase::Playing || self.menu_open() || (self.console.console.is_open() || crate::chat_hook::is_open()) || self.run.is_some() { return false; }
        // INVENTORY FIX2: only the binding toggles the screen (no hardcoded extra key; Tab is ToggleFullScreenMap in the cooked ConfigD_Athena)
        let bound = toggle_inventory_keys(&self.backend.input.bindings).iter().filter_map(|k| k.poll_code()).any(|k| k == vk);
        if pressed && !repeat && bound { self.toggle_inventory(); return true; }
        if !self.inventory_open() { return false; }
        if pressed && !repeat {
            // `TrySetupInputHandling` rows of `InputActionDataTable` (PC keys): Back = Escape, Equip / Unequip = E, Drop = X (fn_ui inventory_panel::KEY_*)
            if let Some(ui) = self.ui.as_mut() { match vk { keys::VK_ESCAPE => ui.inv.panel.key_close(), 0x45 => ui.inv.panel.key_equip(), 0x58 => ui.inv.panel.key_drop(), _ => {} } }
            self.inventory_apply();
        }
        true
    }
    fn inventory_mouse(&mut self, button: winit::event::MouseButton, pressed: bool) -> bool {
        if !self.inventory_open() || self.menu_open() { return false; }
        if button == winit::event::MouseButton::Left {
            let ((x, y), shift) = (self.input.cursor, self.input.shift);
            let now = self.start.elapsed().as_secs_f64();
            if let Some(ui) = self.ui.as_mut() { if pressed { ui.inv.panel.mouse_down_at(x, y, shift, now) } else { ui.inv.panel.mouse_up(x, y, shift) } }
            self.inventory_apply();
        }
        true
    }
    /// INVENTORY HOOK: scripted inventory steps (`--script` `inv=` / `invdrag=` / ...): the same panel input path as the mouse, at the targets' centres.
    fn script_inventory(&mut self, steps: Vec<crate::script::InvCmd>) {
        use crate::script::{InvCmd, InvTarget};
        use fn_ui::screens::inventory_panel::PanelTarget;
        let at = |ui: &crate::ui::UiLayer, t: InvTarget| ui.inv.panel.target_center(match t { InvTarget::Slot(n) => PanelTarget::EquipSlot(n), InvTarget::Tile(k) => PanelTarget::Tile(k), InvTarget::Drop => PanelTarget::DropSlot });
        // INVENTORY FIX2: presses carry the script clock (frame / fps). Every press used to be stamped -10 s, so a click followed by a drag on the same tile
        // read as a double click; now only presses inside the real 500 ms window are one (`invdbl` presses twice 0.2 s apart).
        let now = self.run.as_ref().map_or(0., |r| r.frame as f64 / r.driver.fps() as f64);
        for s in steps {
            eprintln!("[inventory] script step {s:?}");
            if matches!(s, InvCmd::MapOpen | InvCmd::MapClose) { if let Some(ui) = self.ui.as_mut() { ui.hud.full_map = matches!(s, InvCmd::MapOpen); eprintln!("[map] script: full screen map {}", ui.hud.full_map); } continue; }
            if let InvCmd::Mats(m) = s { for (t, n) in m.iter().enumerate() { if let Some(r) = fn_combat::Resource::from_tier(t) { self.sim.world.set_mats(r, *n); } } continue; }
            let Some(ui) = self.ui.as_mut() else { return };
            let opens = matches!(s, InvCmd::Open) && !ui.inv.panel.open; // INVENTORY FIX3 (G4): the scripted open enters cursor mode like the key
            match s {
                InvCmd::Open => { ui.inv.panel.set_open(true); if let Some(a) = self.arms.as_ref() { let v = ui.inv.view_live(a, self.sim.world.all_mats()); ui.inv.panel.set_view(v); } }
                InvCmd::Close => ui.inv.panel.set_open(false),
                InvCmd::EquipKey => ui.inv.panel.key_equip(),
                InvCmd::DropKey => ui.inv.panel.key_drop(),
                InvCmd::Click(t) => if let Some((x, y)) = at(ui, t) { ui.inv.panel.mouse_move(x, y); ui.inv.panel.mouse_down_at(x, y, false, now); ui.inv.panel.mouse_up(x, y, false); },
                InvCmd::DoubleClick(t) => if let Some((x, y)) = at(ui, t) { ui.inv.panel.mouse_down_at(x, y, false, now); ui.inv.panel.mouse_up(x, y, false); ui.inv.panel.mouse_down_at(x, y, false, now + 0.2); ui.inv.panel.mouse_up(x, y, false); },
                // INVENTORY FIX2: the quantity selector through the same pointer path (press + release on the button, a slider press at the value)
                InvCmd::Qsel(q) => {
                    use crate::script::QselCmd;
                    let p = &mut ui.inv.panel;
                    let pt = match q { QselCmd::Confirm => p.target_center(PanelTarget::QselConfirm), QselCmd::Cancel => p.target_center(PanelTarget::QselCancel), QselCmd::Max => p.target_center(PanelTarget::QselMax), QselCmd::Slider(v) => p.qsel_slider_point(v) };
                    match pt { Some((x, y)) => { p.mouse_move(x, y); p.mouse_down_at(x, y, false, now); p.mouse_up(x, y, false); } None => eprintln!("[inventory] script: the quantity selector is not on screen") }
                }
                InvCmd::Drag { from, to, shift } => if let (Some(a), Some(b)) = (at(ui, from), at(ui, to)) {
                    let p = &mut ui.inv.panel; p.mouse_move(a.0, a.1); p.mouse_down_at(a.0, a.1, shift, now);
                    p.mouse_move((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5); p.mouse_move(b.0, b.1); p.mouse_up(b.0, b.1, false);
                } else { eprintln!("[inventory] script drag: target not on screen (open the inventory and render a frame first)"); },
                InvCmd::Mats(_) | InvCmd::MapOpen | InvCmd::MapClose => {}
                // INVENTORY FIX2: wheel notches with the cursor over the tile view (the panel scrolls only there)
                InvCmd::Wheel(n) => { let c = ui.inv.panel.tile_view_center(); ui.inv.panel.mouse_move(c.0, c.1); ui.inv.panel.wheel(n); }
            }
            if opens { self.enter_cursor_mode(); }
            self.inventory_apply();
        }
    }
    /// Resolve the panel's pending actions.  The original panel owns selection;
    /// primary-quickbar drag/drop reaches the same live arms/HUD state it draws.
    fn inventory_apply(&mut self) {
        let a = match self.ui.as_mut() { Some(ui) => ui.inv.apply(), None => return };
        for l in &a.lines { eprintln!("{l}"); }
        // INVENTORY HOOK: the panel's operations run on the live inventory (stacks, stash, ammo, resources)
        if !a.ops.is_empty() {
            if let (Some(ui), Some(arms)) = (self.ui.as_mut(), self.arms.as_mut()) {
                let w = &mut self.sim.world;
                let mut res = [w.wood, w.stone, w.metal];
                for l in ui.inv.execute(&a, arms, &mut res) { eprintln!("{l}"); }
                (w.wood, w.stone, w.metal) = (res[0], res[1], res[2]);
            } else { eprintln!("[inventory] {} operation(s) have no loaded live inventory", a.ops.len()); }
        }
        if let (Some(ui), Some(arms)) = (self.ui.as_mut(), self.arms.as_ref()) { ui.inv.sync_quickbar(&arms.quickbar); }
        if a.close && self.ui.as_ref().is_some_and(|ui| ui.inv.panel.open) { self.toggle_inventory(); }
    }
}

// ----------------------------------------------------------------------------------------------------------------------------------
// CONSOLE HOOK: the F2 command bar (`fn-console`, `console_hook.rs`, `console_ui.rs`)
// ----------------------------------------------------------------------------------------------------------------------------------
impl App {
    /// May the bar open now? Only in the running game: not in a scripted / capture run, not under the Esc menu or the inventory screen.
    fn console_may_open(&self) -> bool { !crate::chat_hook::is_open() && self.phase == Phase::Playing && self.run.is_none() && !self.opts.startup_capture && self.ui.is_some() && !self.menu_open() && !self.inventory_open() }
    /// Window key events: F2 opens the bar; while it is open it takes every key (nothing reaches the game's bindings, only key RELEASES update the held-key table so
    /// nothing sticks). Returns true when the event was consumed.
    fn console_event(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{KeyCode, PhysicalKey};
        let pressed = event.state == ElementState::Pressed;
        let vk = match event.physical_key { PhysicalKey::Code(c) => keys::vk(c), PhysicalKey::Unidentified(_) => None };
        if !self.console.console.is_open() {
            if pressed && !event.repeat && event.physical_key == PhysicalKey::Code(KeyCode::Backquote) && self.console_may_open() { self.console_open(); return true; }
            return false;
        }
        match vk { Some(keys::VK_SHIFT_L | keys::VK_SHIFT_R) => self.input.shift = pressed, Some(keys::VK_CTRL_L | keys::VK_CTRL_R) => self.input.ctrl = pressed, _ => {} }
        if !pressed { if let Some(vk) = vk { self.input.set_key(vk, false); } return true; }
        if event.physical_key == PhysicalKey::Code(KeyCode::Backquote) && event.repeat { return true; }
        for k in crate::console_hook::keys_from_winit(event, self.input.shift) { self.console_key(k); }
        true
    }
    fn chat_event(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{KeyCode, PhysicalKey};
        let pressed = event.state == ElementState::Pressed;
        let vk = match event.physical_key { PhysicalKey::Code(c) => keys::vk(c), PhysicalKey::Unidentified(_) => None };
        if !crate::chat_hook::is_open() {
            if pressed && !event.repeat && event.physical_key == PhysicalKey::Code(KeyCode::KeyT) && !self.console.console.is_open() && self.console_may_open() {
                crate::chat_hook::open();
                self.release_mouse();
                self.input.user_released = true;
                return true;
            }
            return false;
        }
        match vk { Some(keys::VK_SHIFT_L | keys::VK_SHIFT_R) => self.input.shift = pressed, Some(keys::VK_CTRL_L | keys::VK_CTRL_R) => self.input.ctrl = pressed, _ => {} }
        if !pressed { if let Some(vk) = vk { self.input.set_key(vk, false); } return true; }
        for k in crate::console_hook::keys_from_winit(event, self.input.shift) { if k != fn_console::Key::ToggleConsole { crate::chat_hook::key(k); } }
        if !crate::chat_hook::is_open() { self.input.user_released = false; self.try_capture(); }
        true
    }
    fn console_open(&mut self) {
        if self.console.built.is_none() {
            let base = self.arms.as_ref().and_then(|a| a.quickbar.iter().position(|q| *q == Some(crate::arms::QuickbarItem::Healable))).and_then(|s| self.hud_inputs.items.as_ref().and_then(|i| i[s].clone()));
            let built = match self.arms.as_ref() { Some(a) => crate::console_hook::build(a, base), None => crate::console_hook::Built::empty() };
            // the consumables' HUD icons go into the UI's asset overlay under the names the widgets ask for (like the starter items' art at load)
            for (name, w, h, rgba) in built.icons.iter().flatten() { self.overlay.insert(name, fn_ui::assets::Image { w: *w, h: *h, rgba: rgba.clone() }); }
            for n in &built.notes { eprintln!("[console] {n}"); self.console.console.say(fn_console::LineKind::Info, n.clone()); }
            if self.arms.is_none() { self.console.console.say(fn_console::LineKind::Info, "no live loadout (the original item resources were not loaded): item and vitals commands are unavailable, world commands work".to_owned()); }
            self.console.built = Some(built);
        }
        self.console.console.open();
        if let Some(b) = self.console.built.as_mut() { crate::console_hook::sync_weapons(b, &self.weapons); }
        if let Some(b) = self.console.built.as_ref() { self.console.console.refresh(&b.catalog, &fn_console::PakCosmetics); }
        self.release_mouse();
        self.input.user_released = true; // no automatic re-capture while the bar is open
    }
    fn console_key(&mut self, k: fn_console::Key) {
        let vit = crate::console_hook::vitals(self.arms.as_ref());
        let console = &mut self.console;
        if let Some(b) = console.built.as_mut() { crate::console_hook::sync_weapons(b, &self.weapons); } // F2 `give <weapon>` / `equip` / `list weapons`: the runtime's 45-weapon catalog
        let Some(built) = console.built.as_ref() else { return };
        let fx = { let mut env = fn_console::Env { sim: &mut self.sim, catalog: &built.catalog, vitals: vit, cosmetics: &fn_console::PakCosmetics }; console.console.key(k, &mut env) };
        for (_, l) in &fx.lines { eprintln!("[console] {l}"); }
        for (kind, l) in crate::console_hook::weapon_requests(&fx.requests, &mut self.weapons) { eprintln!("[console] {l}"); console.console.say(kind, l); }
        crate::pickups_hook::requests(&fx.requests); // PICKUPS HOOK: `spawn <item> [count]`
        if fx.requests.contains(&fn_console::Request::StartBus) { crate::bus_hook::start_bus_now(&mut self.sim); }
        if fx.requests.iter().any(|r| !matches!(r, fn_console::Request::GiveWeapon { .. } | fn_console::Request::EquipWeapon { .. })) {
            match self.arms.as_mut() {
                Some(arms) => for (kind, l) in crate::console_hook::apply(&fx.requests, arms, self.hud_inputs.items.as_mut(), built) { eprintln!("[console] {l}"); console.console.say(kind, l); },
                None => console.console.say(fn_console::LineKind::Error, "no live loadout: the item / vitals request was not applied"),
            }
        }
        if !console.console.is_open() { self.input.user_released = false; self.try_capture(); }
    }
}

/// Output file prefix of a scripted run: `builtin:NAME` -> `builtin_NAME`; a script file (relative or absolute) -> its file stem only.
fn run_output_name(spec: &str) -> String {
    if spec.starts_with("builtin:") { return spec.replace([':', '/', '\\', '.'], "_"); }
    let stem = std::path::Path::new(spec).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = stem.replace([':', '/', '\\', '.'], "_");
    if stem.is_empty() { "script".into() } else { stem }
}

#[cfg(test)]
mod run_output_name_tests {
    use super::run_output_name;
    #[test] fn script_files_use_only_their_stem() {
        assert_eq!(run_output_name(r"C:\Users\x\verification\building\place.txt"), "place");
        assert_eq!(run_output_name("verification/building/place.txt"), "place");
        assert_eq!(run_output_name("place.txt"), "place");
        assert_eq!(run_output_name("builtin:smoke"), "builtin_smoke");
    }
}

/// Saves one capture as `<out>/<name>-<w>x<h>.png`; returns the log line.
fn write_capture(cap: &mut CaptureState, name: &str, w: u32, h: u32, px: &[u8], now: f32) -> String {
    let path = cap.out.join(format!("{name}-{w}x{h}.png"));
    match run::save_png(&path, w, h, px) {
        Ok(()) => { cap.timeline.push((now, format!("capture {name}"))); cap.shots.push(path.clone()); format!("capture saved {}", path.display()) }
        Err(e) => { cap.failures.push(e.clone()); format!("capture FAILED {e}") }
    }
}

// ----------------------------------------------------------------------------------------------------------------------------------
// winit
// ----------------------------------------------------------------------------------------------------------------------------------
impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window_created { return; }
        self.window_created = true;
        el.set_control_flow(if cfg!(target_arch = "wasm32") { ControlFlow::Wait } else { ControlFlow::Poll }); // BROWSER PORT: frames are paced by requestAnimationFrame
        if let Err(e) = self.init_window(el) { self.fail(el, e); }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if self.trace && !matches!(event, WindowEvent::RedrawRequested | WindowEvent::CursorMoved { .. } | WindowEvent::AxisMotion { .. }) {
            eprintln!("[event {:7.3}] {}", self.start.elapsed().as_secs_f32(), match &event { WindowEvent::KeyboardInput { event, .. } => format!("Key {:?} {:?}", event.physical_key, event.state), e => format!("{e:?}") });
        }
        match event {
            WindowEvent::CloseRequested => {
                // an automated capture is not closed by a stray close request (Esc still stops it); it logs the request instead
                if self.cap.is_some() { self.flow.note("close request ignored (start-up capture running; Esc stops it)"); } else { self.release_mouse(); el.exit(); }
            }
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gfx.as_mut() { g.resize(size.width, size.height); }
                if size.width > 0 && size.height > 0 {
                    self.minimized = false;
                    if self.input.captured { if let Some(p) = self.platform.as_ref() { p.recenter(); } }
                }
            }
            WindowEvent::Focused(focused) => self.on_focus(focused),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && !event.repeat && matches!(event.physical_key, PhysicalKey::Code(winit::keyboard::KeyCode::F11))
                && !(self.opts.scripted() || self.opts.startup_capture) => self.toggle_fullscreen(),
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(e) = self.name_entry.as_mut() {
                    if event.state == ElementState::Pressed { for k in crate::console_hook::keys_from_winit(&event, self.input.shift) { e.key(k); } }
                    if let PhysicalKey::Code(c) = event.physical_key { if let Some(vk) = keys::vk(c) { match vk { keys::VK_SHIFT_L | keys::VK_SHIFT_R => self.input.shift = event.state == ElementState::Pressed, _ => {} } } }
                    return;
                }
                if self.phase == Phase::Lobby && self.lobby.as_ref().is_some_and(|l| l.chat_open()) {
                    if let PhysicalKey::Code(c) = event.physical_key { if let Some(vk) = keys::vk(c) { if matches!(vk, keys::VK_SHIFT_L | keys::VK_SHIFT_R) { self.input.shift = event.state == ElementState::Pressed; } } }
                    if event.state == ElementState::Pressed { let ks = crate::console_hook::keys_from_winit(&event, self.input.shift); if let Some(l) = self.lobby.as_mut() { for k in ks { l.chat_key(k); } } }
                    return;
                }
                if self.chat_event(&event) { return; }
                if self.console_event(&event) { return; } // CONSOLE HOOK: F2 / the open command bar
                let vk = match event.physical_key { PhysicalKey::Code(c) => keys::vk(c), PhysicalKey::Unidentified(_) => None };
                if let Some(vk) = vk { self.on_key(el, vk, event.state == ElementState::Pressed, event.repeat); }
            }
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_button(button, state == ElementState::Pressed),
            WindowEvent::CursorMoved { position, .. } => self.on_cursor(position.x as f32, position.y as f32),
            WindowEvent::CursorLeft { .. } => { if let Some(ui) = self.ui.as_mut() { ui.menu.event(UiEvent::MouseLeave); } self.picker_event(UiEvent::MouseLeave); }
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(match delta { MouseScrollDelta::LineDelta(_, y) => y, MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40. }),
            WindowEvent::RedrawRequested => {
                // WEB 60: the browser build renders at most 60 frames per second (`web_quality::frame_due`); a skipped animation frame only asks for the next
                #[cfg(target_arch = "wasm32")]
                if self.run.is_none() && !web_quality::frame_due(self.backend.runtime.frame_cap) { if let Some(p) = self.platform.as_ref() { p.window.request_redraw(); } return; }
                self.frame(el)
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event { self.input.raw_motion(delta.0, delta.1); }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.quit_requested { self.release_mouse(); el.exit(); return; }
        if self.live.is_some() { self.live_tick(el); }
        let Some(p) = self.platform.as_ref() else { return };
        // a hidden (headless) window never gets WM_PAINT, so `request_redraw` would never deliver `RedrawRequested`: drive the frame from here instead
        if crate::cli::headless(&self.opts) { el.set_control_flow(ControlFlow::Poll); self.frame(el); return; }
        // BROWSER PORT: request_redraw delivers RedrawRequested on the next animation frame; waiting in between leaves the page idle
        #[cfg(target_arch = "wasm32")]
        {
            el.set_control_flow(ControlFlow::Wait); p.window.request_redraw();
            // BROWSER PORT: the browser refused or dropped the pointer lock the game asked for: release the capture, so the next click asks again
            if self.input.captured && crate::web::lock_refused() { eprintln!("[capture] the browser did not grant the pointer lock: click the game to capture the mouse"); self.release_mouse(); }
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.minimized { el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50))); } else { el.set_control_flow(ControlFlow::Poll); p.window.request_redraw(); }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        self.release_mouse();
        if let Some(p) = self.platform.as_mut() { if p.is_exclusive() { p.restore_display(); } }
        #[cfg(target_arch = "wasm32")]
        crate::web::exited(self.exit_code); // BROWSER PORT: the page says the game was closed (a page cannot close itself)
    }
}

// ----------------------------------------------------------------------------------------------------------------------------------
// `--live-test`: the interactive code path (real window, real frame times, mouse capture, Esc menu, Settings apply) exercised on a schedule, with
// the "events" supplied by this process (window operations through winit, handler calls) instead of the user: no global input injection.
// ----------------------------------------------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct Check { pub name: String, pub ok: bool, pub detail: String }

#[derive(Default)]
pub struct LiveTest {
    pub t: f32,
    pub next: usize,
    pub checks: Vec<Check>,
    pub frames_mark: u32,
    pub steady: FrameStats,
    pub all: FrameStats,
    pub started: Option<Instant>,
    /// The minimize probe saw the window minimised (the OS can restore it by itself; the state check then accepts that).
    pub saw_minimized: bool,
    /// (seconds into the test, interval ms) of every frame longer than 25 ms.
    pub hitches: Vec<(f32, f32)>,
}
/// (seconds after gameplay starts, step). The first 3 s are undisturbed gameplay: that is the steady-state frame-time sample.
pub const LIVE_SCHEDULE: &[(f32, &str)] = &[
    (0.6, "capture-at-start"), (3.0, "minimize"), (3.1, "probe"), (3.4, "probe"), (3.8, "probe"), (4.0, "minimized-state"), (4.1, "restore"), (4.3, "probe"), (5.4, "restored-state"),
    (5.5, "resize"), (6.3, "resized-state"), (6.4, "esc-opens-menu"), (6.8, "menu-state"),
    (6.9, "apply-borderless"), (7.8, "borderless-state"), (7.9, "apply-windowed-vsync"), (8.9, "windowed-state"),
    (9.0, "close-menu"), (9.6, "menu-closed-state"), (9.7, "focus-out"), (10.0, "focus-out-state"), (10.1, "focus-in"), (10.6, "focus-in-state"), (10.8, "finish"),
];

impl App {
    fn live_check(&mut self, name: &str, ok: bool, detail: String) {
        eprintln!("[live-test] {} {name}: {detail}", if ok { "ok  " } else { "FAIL" });
        if let Some(l) = self.live.as_mut() { l.checks.push(Check { name: name.to_owned(), ok, detail }); }
    }
    fn win_state(&self) -> String {
        let (Some(p), Some(g)) = (self.platform.as_ref(), self.gfx.as_ref()) else { return "no window".into() };
        format!("inner {:?} surface {:?} fullscreen {} focus {} captured {} cursor-hidden-by-capture {} minimized {:?} vsync {} menu {}",
            p.window.inner_size(), g.size(), p.window.fullscreen().is_some(), p.window.has_focus(), self.input.captured, p.cursor_captured(), p.window.is_minimized(), g.vsync, self.menu_open())
    }
    /// Advances the self-test clock and runs the steps that are due. Called once per gameplay frame.
    fn live_sample(&mut self, raw_dt: f32) {
        let Some(l) = self.live.as_mut() else { return };
        let t = l.started.map_or(0., |s| s.elapsed().as_secs_f32());
        l.all.push(raw_dt * 1000.);
        if raw_dt > 0.025 { l.hitches.push((t, raw_dt * 1000.)); }
        if t < 3.0 && t > 0.3 { l.steady.push(raw_dt * 1000.); }
    }
    /// Wall-clock driven (called from `about_to_wait`, so the schedule keeps running while the window is minimized and no frames are drawn).
    fn live_tick(&mut self, el: &ActiveEventLoop) {
        if self.phase != Phase::Playing { return; }
        let Some(l) = self.live.as_mut() else { return };
        if l.started.is_none() { l.started = Some(Instant::now()); }
        let t = l.started.map_or(0., |s| s.elapsed().as_secs_f32());
        l.t = t;
        while let Some(&(at, step)) = LIVE_SCHEDULE.get(self.live.as_ref().map_or(usize::MAX, |l| l.next)) {
            if t < at { break; }
            if let Some(l) = self.live.as_mut() { l.next += 1; }
            let focused = self.platform.as_ref().map_or(false, |p| p.window.has_focus());
            let skip_note = if focused { "" } else { " (the OS did not give the window focus: capture checks skipped)" };
            match step {
                "capture-at-start" => { let ok = !focused || (self.input.captured && self.platform.as_ref().map_or(false, |p| p.cursor_captured())); self.live_check(step, ok, format!("{}{skip_note}", self.win_state())); if let Some(l) = self.live.as_mut() { l.frames_mark = self.sim.frames; } }
                "probe" => { if self.minimized { if let Some(l) = self.live.as_mut() { l.saw_minimized = true; } } eprintln!("[live-test] probe t={t:.2}: {}  frames {} minimized-flag {}", self.win_state(), self.sim.frames, self.minimized); }
                "minimize" => { if let Some(p) = self.platform.as_ref() { p.window.set_minimized(true); } }
                "minimized-state" => {
                    let saw = self.live.as_ref().map_or(false, |l| l.saw_minimized);
                    let ok = (self.minimized && !self.input.captured) || (saw && !self.minimized);
                    self.live_check(step, ok, format!("{}{}", self.win_state(), if !self.minimized && saw { " (minimised earlier; the OS had already restored the window before this check)" } else { "" }));
                }
                "restore" => { if let Some(p) = self.platform.as_ref() { p.window.set_minimized(false); p.window.focus_window(); } }
                "restored-state" => { let ok = !self.minimized && (!focused || self.input.captured); self.live_check(step, ok, format!("{}{skip_note}", self.win_state())); }
                "resize" => { if let Some(p) = self.platform.as_ref() { let _ = p.window.request_inner_size(PhysicalSize::new(1024u32, 600u32)); } }
                "resized-state" => { let (ok, d) = match (self.platform.as_ref(), self.gfx.as_ref()) { (Some(p), Some(g)) => (g.size() == (1024, 600) && p.window.inner_size() == PhysicalSize::new(1024, 600), self.win_state()), _ => (false, "no window".into()) }; self.live_check(step, ok, d); }
                "esc-opens-menu" => { self.on_key(el, keys::VK_ESCAPE, true, false); self.on_key(el, keys::VK_ESCAPE, false, false); if let Some(l) = self.live.as_mut() { l.frames_mark = self.sim.frames; } }
                "menu-state" => { let frozen = self.live.as_ref().map_or(0, |l| l.frames_mark) == self.sim.frames; let ok = self.menu_open() && !self.input.captured && frozen; self.live_check(step, ok, format!("sim frames frozen while the menu is open: {frozen}; {}", self.win_state())); }
                "apply-borderless" | "apply-windowed-vsync" => {
                    let borderless = step == "apply-borderless";
                    if let Some(ui) = self.ui.as_mut() {
                        let s = ui.menu.session_mut();
                        s.with_settings(|st| {
                            let _ = st.set("video.window_mode", Value::Index(if borderless { 1 } else { 2 }));
                            if !borderless { let _ = st.set("video.resolution", Value::Size(1280, 720)); let _ = st.set("video.vsync", Value::Bool(true)); }
                        });
                        s.request_apply();
                    }
                }
                "borderless-state" => {
                    let mon = self.platform.as_ref().and_then(|p| crate::platform::monitor_of(&p.window)).map(|m| (m.size().width, m.size().height));
                    let ok = self.gfx.as_ref().map(|g| Some(g.size()) == mon).unwrap_or(false) && self.platform.as_ref().map_or(false, |p| p.window.fullscreen().is_some());
                    self.live_check(step, ok, format!("monitor {mon:?}; {}", self.win_state()));
                }
                "windowed-state" => {
                    let ok = self.gfx.as_ref().map_or(false, |g| g.size() == (1280, 720) && g.vsync) && self.platform.as_ref().map_or(false, |p| p.window.fullscreen().is_none());
                    self.live_check(step, ok, self.win_state());
                }
                "close-menu" => { if let Some(ui) = self.ui.as_mut() { ui.menu.close(); } let reqs = self.ui.as_mut().map(|u| u.menu.take_requests()).unwrap_or_default(); self.handle_menu_requests(el, reqs); if let Some(l) = self.live.as_mut() { l.frames_mark = self.sim.frames; } }
                "menu-closed-state" => { let adv = self.sim.frames > self.live.as_ref().map_or(0, |l| l.frames_mark); let ok = !self.menu_open() && adv && (!focused || self.input.captured); self.live_check(step, ok, format!("sim advancing again: {adv}; {}{skip_note}", self.win_state())); }
                "focus-out" => { self.on_focus(false); self.input.set_key(0x57, true); }
                "focus-out-state" => { let ok = !self.input.captured && !self.input.focused && !self.input.poll(0x57); self.live_check(step, ok, format!("{} (held W is not read while unfocused)", self.win_state())); }
                "focus-in" => { self.on_focus(true); }
                "focus-in-state" => { let ok = self.input.focused && (!focused || self.input.captured) && !self.input.poll(0x57); self.live_check(step, ok, format!("{} (the W held across the focus loss was dropped){skip_note}", self.win_state())); }
                "finish" => { self.finish_live(el); return; }
                _ => {}
            }
        }
    }

    fn finish_live(&mut self, el: &ActiveEventLoop) {
        let Some(l) = self.live.take() else { return };
        let mut report = format!("fortnite-rs live self-test (real window / frame loop / capture policy / Settings apply)\ngpu: {}\n", self.gfx.as_ref().map(|g| g.describe()).unwrap_or_default());
        report.push_str(&l.steady.report("steady-state gameplay (first 3 s, before any window operation; dt = real frame time)", 0));
        report.push_str(&l.all.report("whole test (includes minimize / resize / mode switch hitches)", 20));
        report.push_str(&format!("frames over 25 ms: {} {:?}\n", l.hitches.len(), l.hitches.iter().map(|(t, ms)| format!("{t:.2}s:{ms:.0}ms")).collect::<Vec<_>>()));
        for c in &l.checks { report.push_str(&format!("{} {}: {}\n", if c.ok { "ok  " } else { "FAIL" }, c.name, c.detail)); }
        let ok = !l.checks.is_empty() && l.checks.iter().all(|c| c.ok);
        report.push_str(if ok { "RESULT: ok\n" } else { "RESULT: FAILED\n" });
        let out = self.opts.out.clone().unwrap_or_else(|| crate::assets::exe_dir().join("verification"));
        let _ = std::fs::create_dir_all(&out);
        let p = out.join("live-test-report.txt");
        let _ = std::fs::write(&p, &report);
        println!("{report}report: {}", p.display());
        self.exit_code = if ok { 0 } else { 1 };
        el.exit();
    }
}


