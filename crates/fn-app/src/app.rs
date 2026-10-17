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

#[derive(Clone, Debug, PartialEq)]
pub enum Phase { Boot, Splash, StartupLoading, GamePicker, Loading, Finishing, Playing, Failed(String) }
impl Phase {
    fn of(s: Stage) -> Phase { match s { Stage::Splash => Phase::Splash, Stage::StartupLoading => Phase::StartupLoading, Stage::GamePicker => Phase::GamePicker, Stage::Loading => Phase::Loading, Stage::Finishing => Phase::Finishing, Stage::Playing => Phase::Playing } }
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
    sim: GameSim,
    input: InputState,
    pad: gpad::Gamepad<DynPad>,
    pad_state: Option<gpad::State>,
    pub settings: Settings,
    settings_note: String,
    backend: AppBackend,
    gp_audio: GameplayAudio,
    phase: Phase,
    phase_t: f32,
    startup_elapsed: f32,
    start: Instant,
    last: Instant,
    limiter: FrameLimiter,
    clock: FrameClock,
    hud_inputs: HudInputs,
    /// The player's quickbar / held firearm / bandage state (rms.rs); None until the original loadout resources were loaded (pak mode).
    arms: Option<Arms>,
    ads: Option<crate::ads::AdsDriver>, // ADS HOOK
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

impl App {
    pub fn new(opts: Options, roots: AssetRoots, source: Source, run: Option<ScriptRun>) -> App {
        let scripted = opts.scripted();
        let automated = scripted || opts.startup_capture;
        let (mut settings, note, path) = if opts.uses_user_settings() {
            let path = fn_settings::default_path();
            let (s, r) = Settings::load(&path);
            (s, format!("{} -> {:?}, file version {:?}, backup {:?}{}", r.path.display(), r.status, r.file_version, r.backup, if r.warnings.is_empty() { String::new() } else { format!("\n{}", r.warnings.join("\n")) }), Some(path))
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
        flow.echo = true;
        let backend = AppBackend::new(path, crate::audio::open_for_app(&opts, &source)); // AUDIO HOOK: the real sink (NullAudio for --no-audio / no install / automated runs)
        let live = if opts.live_test { Some(LiveTest::default()) } else { None };
        App {
            pad: gpad::Gamepad::new(pad::open(!opts.no_gamepad && !automated)), opts, roots: Arc::new(roots), source, skip_flow, flow, overlay: Overlay::default(), work: StartupWork::none(), picker: Picker::new(), art: ArtState::default(), cap: None, stalls: Stalls::default(), quit_requested: false, tip: 0, gfx: None, platform: None, ui: None, scene: None, loader: None, sim: GameSim::new(), input, pad_state: None,
            settings, settings_note: note, backend, gp_audio: GameplayAudio::default(), phase: Phase::Boot, phase_t: 0., startup_elapsed: 0.,
            start: Instant::now(), last: Instant::now(), limiter: FrameLimiter::new(), clock: FrameClock::default(), hud_inputs: crate::loadout_preview::inputs(), arms: None, ads: None, fx: None, fx_tried: false, arms_policy: Policy::default(), run, minimized: false, window_created: false, exit_code: 0, load_log: String::new(), live_stats: FrameStats::default(), live, trace: std::env::var_os("FN_TRACE").is_some(),
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
        window.set_visible(true);
        window.focus_window();
        let ui = UiLayer::new(&self.roots, gfx.gpu.device.clone(), gfx.gpu.queue.clone(), gfx.format(), self.overlay.clone(), self.install_dir())?;
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
    fn start_loading(&mut self) -> Phase {
        self.tip = if self.cap.is_some() { 0 } else { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as usize / 1000) };
        crate::muzzle_hook::init(&self.roots.roots); // MUZZLE HOOK: the firearms' muzzle flash assets (<assets>/particles-muzzle, independent of the props / harvest particles)
        // REAL PROPS HOOK: load the exported 1.8 trees / rocks only when this run will place them (the parity scripts keep the sandbox and their timings)
        let mode = self.real_mode();
        let props = if mode.needs_real() {
            crate::props::discover(&self.roots.roots).map(|cat| {
                let lib = std::sync::Arc::new(crate::props::Library::from_catalog(&cat, self.opts.prop_radius.unwrap_or(crate::props::DEFAULT_RADIUS_M)));
                crate::audio::set_prop_ids(lib.protos.iter().map(|p| p.id.clone()).collect()); // CRACK AUDIO HOOK: proto -> prop id for the props.json hit / death sounds
                crate::audio::set_ambient_world(lib.center_cm, crate::props::ANCHOR); // AMBIENCE HOOK: the level's ambient emitters are placed with the props' mapping
                crate::particles::init(&self.roots.roots, lib.protos.iter().map(|p| cat.get(&p.id).and_then(|k| k.destruction.death_particles.clone())).collect()); // PARTICLE HOOK: proto -> DeathParticles
                let kinds = lib.kinds_needed(mode);
                crate::loader::PropsJob { cat, lib, kinds }
            })
        } else { None };
        if let Some(g) = self.gfx.as_ref() { self.loader = Some(Loader::start(self.source.clone(), self.roots.clone(), g.gpu.device.clone(), g.gpu.queue.clone(), g.format(), props)); }
        self.phase_t = 0.;
        Phase::Loading
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
        let minimized = self.platform.as_ref().map_or(false, |p| p.window.is_minimized() == Some(true)) || self.gfx.as_ref().map_or(true, |g| g.size().0 == 0);
        if minimized != self.minimized { self.minimized = minimized; if minimized { self.release_mouse(); } }
        if self.minimized { return; }
        if self.quit_requested { self.release_mouse(); el.exit(); return; }
        // the start-up capture runs on a fake 60 Hz clock; everything else on the real frame time
        let dt = if self.cap.is_some() { CAPTURE_DT } else { raw_dt };
        if matches!(self.phase, Phase::StartupLoading | Phase::GamePicker | Phase::Loading | Phase::Finishing) { let name = self.flow.stage.name(); self.stalls.record(name, raw_dt * 1000.); }
        if self.cap.is_some() { self.capture_poll(el, dt); if self.cap.is_none() { return; } }
        match self.phase.clone() {
            Phase::Boot => {}
            Phase::Splash => self.splash_frame(dt),
            Phase::StartupLoading => self.startup_frame(dt),
            Phase::GamePicker => self.picker_frame(dt),
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
        build(ui, w as f32, h as f32);
        if let Acquire::Frame(f) = gfx.acquire() {
            let view = f.texture.create_view(&Default::default());
            ui.render(&view, w, h, Some([0., 0., 0., 1.]));
            gfx.present(f);
        }
        if !want.is_empty() {
            let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
            ui.render(&target.view, w, h, Some([0., 0., 0., 1.]));
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
        self.simple_frame(move |ui, w, h| ui.present_hud(&st, w, h, None));
    }

    /// Applies a stage the flow just entered: phase, side effects (start the load), log.
    fn entered(&mut self, stage: Option<Stage>) {
        let Some(s) = stage else { return };
        crate::audio::stage_audio(self.backend.audio.as_mut(), s); // MUSIC HOOK: loading-screen mix / front-end music per flow stage
        self.phase_t = 0.;
        match s {
            Stage::Loading => { self.phase = self.start_loading(); }
            Stage::StartupLoading => { self.work.begin_front_end(); self.phase = Phase::StartupLoading; }
            Stage::GamePicker => { self.picker = Picker::new(); self.phase = Phase::GamePicker; }
            Stage::Playing => { self.phase = self.begin_play(); }
            other => { self.phase = Phase::of(other); }
        }
    }

    fn splash_frame(&mut self, dt: f32) {
        self.simple_frame(|ui, w, h| ui.splash(w, h, 1.));
        let s = self.flow.tick(dt);
        self.entered(s);
    }

    /// Start-up loading screen: the key art is shown once the worker delivered it AND it was uploaded (one texture per frame, so showing it costs no hitch).
    fn startup_frame(&mut self, dt: f32) {
        self.upload_art();
        let (t, fade, ready, progress) = (self.flow.stage_t, self.flow.startup_fade(), self.art.startup_uploaded, self.work.fraction());
        self.simple_frame(move |ui, w, h| ui.startup_loading(w, h, t, progress, ready, fade));
        if self.work.done() && (self.art.picker_uploaded || !self.flow.picker) { self.flow.set_work_done(); }
        let s = self.flow.tick(dt);
        self.entered(s);
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

    fn picker_frame(&mut self, dt: f32) {
        // gamepad navigation (edge + repeat)
        if self.input.focused && self.cap.is_none() {
            self.pad_state = self.pad.poll(dt);
            let evs = self.ui.as_mut().map(|u| u.pad_events(self.pad_state.as_ref(), dt)).unwrap_or_default();
            for e in evs { self.picker_event(e); }
        }
        self.picker.tick(dt);
        let (p, ready) = (self.picker.clone(), self.art.picker_uploaded);
        self.simple_frame(move |ui, w, h| ui.picker(&p, w, h, ready));
        let s = self.flow.tick(dt);
        self.entered(s);
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
            PickerOutcome::StartBattleRoyale => { if self.flow.pick_battle_royale() { self.entered(Some(Stage::Loading)); } }
            PickerOutcome::Quit => { self.flow.note("picker: Quit chosen"); self.quit_requested = true; }
            PickerOutcome::None => {}
        }
    }

    fn loading_frame(&mut self, el: &ActiveEventLoop, dt: f32) {
        self.phase_t += dt;
        let (Some(ui), Some(loader)) = (self.ui.as_mut(), self.loader.as_mut()) else {
            // load finished earlier: keep showing 100 % until the flow leaves the stage
            self.loading_page(1., "Starting", 0.);
            let s = self.flow.tick(dt); self.entered(s);
            return;
        };
        let state = loader.poll(ui, Duration::from_millis(12));
        let (fraction, label) = (loader.fraction(), loader.label());
        match state {
            Err(e) => { self.fail(el, e); return; }
            Ok(State::Done(scene)) => {
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
                        Ok(d) => { eprintln!("[arms] quickbar: 1 pickaxe, 2 {}, 3 {}, 4 {}, 5 bandage x{}", d.guns[0].def.stat_row, d.guns[1].def.stat_row, d.guns[2].def.stat_row, d.start_bandages); self.arms_policy.assume_unrecovered |= self.opts.assume_unrecovered || self.run.as_ref().map_or(false, |r| r.driver.assume_unrecovered()); if self.arms_policy.assume_unrecovered { for a in crate::arms::ASSUMPTIONS { eprintln!("[arms] ASSUMED (dev switch on): {a}"); } } else { for m in crate::arms::MISSING_BY_DEFAULT { eprintln!("[arms] MISSING (not recovered): {m}"); } } let arms = Arms::new(d, self.arms_policy); if let Some(ui) = self.ui.as_mut() { ui.inv.sync_quickbar(&arms.quickbar); } self.arms = Some(arms); }
                        Err(e) => eprintln!("[arms] unavailable: {e}"),
                    }
                    self.ads = crate::ads::AdsDriver::from_resources(loadout); // ADS HOOK: aim-down-sights profiles of the three firearms
                }
                self.load_log = loader.timings_text();
                crate::hitreg_hook::set_install(self.install_dir()); // HITREG HOOK
                let stage_note = format!("load steps:\n{}", self.load_log);
                let mut scene = scene;
                // The recovered original mesh path is the normal visual route.
                // The translated WeaponMat_Base pass remains an opt-in
                // diagnostic until a capture proves that it produces pixels.
                let orig_gun_ok = match self.install_dir() {
                    Some(dir) if self.opts.original_gun_material && scene.loadout.is_some() =>match scene.install_original_guns(&dir) {
                        Ok(()) => { eprintln!("[arms] ORIGINAL gun material ON: the base-pass vertex + pixel programs of the cooked WeaponMat_Base shader map (SM5, TGPUSkinVertexFactory, FNoLightMapPolicy + skylight), original instance parameters and textures"); true }
                        Err(e) => { eprintln!("[arms] original gun material NOT available, the held gun stays hidden: {e}"); false }
                    },
                    _ => false,
                };
                if !orig_gun_ok {
                    match scene.install_guns() { Ok(()) => eprintln!("[arms] recovered 1.8 weapon meshes ON: original meshes + original Diffuse/Normals textures; translated WeaponMat_Base remains opt-in"), Err(e) => eprintln!("[arms] gun meshes unavailable: {e}") }
                }
                self.populate_real_props(&scene); // REAL PROPS HOOK
                self.scene = Some(scene);
                self.loader = None;
                self.flow.set_loader_done();
                self.flow.note(stage_note.trim_end());
                if self.run.is_some() { self.flow.force(Stage::Playing); self.phase = self.begin_play(); return; }
            }
            Ok(State::Running) => {}
        }
        let (fraction, label) = if self.loader.is_none() { (1., "Starting".to_owned()) } else { (fraction, label) };
        self.loading_page(fraction, &label, 0.);
        let s = self.flow.tick(dt); self.entered(s);
    }

    fn finishing_frame(&mut self, dt: f32) {
        let fade = self.flow.finish_fade();
        self.loading_page(1., "Starting", fade);
        let s = self.flow.tick(dt); self.entered(s);
    }

    fn begin_play(&mut self) -> Phase {
        crate::audio::stage_audio(self.backend.audio.as_mut(), Stage::Playing); // MUSIC HOOK (also the scripted path that forces Playing)
        self.input.started = true;
        self.startup_elapsed = 0.;
        self.last = Instant::now();
        eprintln!("[load] done:\n{}", self.load_log);
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

    /// Feeds the arms (quickbar / fire / reload / heal) for this frame and hands their results to the simulation: the held core animation set, the weapon montages
    /// and the non-melee flag (the pickaxe swing is off while anything else is selected). Returns the HUD selection.
    fn drive_arms(&mut self, dt: f32, inp: &mut FrameInput, ai: ArmsInput) {
        let Some(arms) = self.arms.as_mut() else { return };
        let before = arms.selected;
        let fire = inp.attack;
        arms.update(dt, &ArmsInput { fire_held: fire, ..ai });
        for e in arms.take_events() { eprintln!("{e}"); }
        // WEAPON AUDIO HOOK: the arms' sound moments -> sink events (weapon sounds at the weapon's attach point, montage-notify sounds at the pawn mesh)
        let sounds = arms.take_sounds();
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
        self.hud_inputs.scope_overlay = None;
        if let Some(ads) = self.ads.as_mut() {
            let held = self.input.captured && self.backend.input.bindings.down(fn_settings::A_TARGET, |k| self.input.poll(k));
            let (can_aim, i) = (arms.gun_index().is_some() && !arms.is_healing(), &self.settings.input);
            // the look rate while targeting follows the device that is looking: the right stick (Controller ADS / Scope Sensitivity) or the mouse
            let pad_look = self.pad_state.as_ref().map_or(false, |s| { let (x, y) = gpad::radial_deadzone(s.rx, s.ry, gpad::RIGHT_DEADZONE); x != 0. || y != 0. });
            let (ads_mul, scope_mul) = if pad_look { (i.controller_ads_multiplier, i.controller_scope_multiplier) } else { (i.mouse_ads_multiplier, i.mouse_scope_multiplier) };
            let f = ads.step(dt, held, i.toggle_targeting, arms.gun_index(), can_aim, ads_mul, scope_mul);
            self.hud_inputs.scope_overlay = ads.scope_overlay(&f);
            ads.apply(&f, &mut self.sim, inp, !pad_look);
            aiming = f.aiming; ads_weight = f.weight; reticle_hidden = f.reticle_hidden;
            crate::muzzle_hook::set_scope(f.scope_mesh); // MUZZLE HOOK: IsUsingScope stand-in (the muzzle flash is skipped while looking through the scope)
        }
        // Select the authored player/weapon fire montage after the ADS state is
        // resolved.  Sampling before this point made every aimed shot use the
        // hip-fire local transform.
        arms.set_aiming(aiming);
        self.sim.set_held(arms.held_core().map(|(core, _)| (core, aiming)));
        // RELAXED HOOK: the held firearm's LastFireTime / reload / equip state -> the 1.8 native relaxed timers (bIsRelaxedLevel1 after 5 s, Level2 after 15 s, bRecentlyFired 0.2 s)
        self.sim.set_weapon_clock(arms.weapon_clock());
        // RECOIL HOOK: IsUsingGamepad proxy = the right stick is the looking device (same predicate as the ADS look rate above); the recoil step is added to this frame's look delta
        let gamepad = self.pad_state.as_ref().map_or(false, |s| { let (x, y) = gpad::radial_deadzone(s.rx, s.ry, gpad::RIGHT_DEADZONE); x != 0. || y != 0. });
        crate::hitreg_hook::set_aim_inputs(ads_weight, self.scene.as_ref().and_then(|s| s.weapon_origin)); // SPREAD HOOK: targeting camera weight + muzzle
        crate::hitreg_hook::on_arms(&mut self.sim, arms, aiming, dt, inp, gamepad); // HITREG HOOK: shots -> trace / damage / prop health; RECOIL HOOK: recovered recoil -> look delta
        // Only confirmed hits restart `B_Ranged_Generic.HitNotifyReticleImage` (0.4 s); misses and melee never do.
        self.hud_inputs.firearm_hit = !crate::hitreg_hook::take_impacts().is_empty();
        self.sim.arms = arms.montages();
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
            })
        };
        self.hud_inputs.equip = arms.equip_progress().map(|(elapsed, duration)| fn_ui::screens::reticle_state::EquipProgress { elapsed, duration });
        self.hud_inputs.slot_cooldowns = arms.slot_cooldowns(); // WIDGETS3 HOOK: QuickbarSlotCooldown
        self.hud_inputs.vitals = (arms.health.round() as u16, arms.shield.round() as u16);
        if let Some(items) = self.hud_inputs.items.as_mut() { let counts = arms.slot_counts(); for (i, it) in items.iter_mut().enumerate() { if let (Some(it), Some(c)) = (it.as_mut(), counts[i]) { it.count = Some(c); } } }
    }

    /// One gameplay input + simulation step. Returns true when the simulation advanced.
    fn update_sim(&mut self, dt: f32) -> bool {
        let Some(ui) = self.ui.as_mut() else { return false };
        if let Some(run) = self.run.as_mut() {
            let f = run.frame;
            run.driver.prepare(f, &mut self.sim);
            let mut inp = run.driver.input(f);
            let ai = run.driver.arms(f);
            self.drive_arms(dt, &mut inp, ai);
            self.sim.step(&inp, dt);
            self.gp_audio.update(self.backend.audio.as_mut(), &self.sim, dt);
            return true;
        }
        self.input.begin_frame(); // wheel notches since the last frame become one-frame presses
        if ui.menu.is_open() { gpad::menu_opened(); return false; } // the menu pauses the simulation (no input, animation, combat, camera or HUD ticks)
        if ui.inv.panel.open { return false; } // WIDGETS3 HOOK: the inventory screen takes the input (the sim waits like under the menu)
        if !self.input.focused { return false; }
        self.input.update_help();
        self.clock.tick(dt);
        // In 1.8 AFortPawn::Tick runs once per rendered frame, so UFortMovementComp_Character::DoJump's frame-delta JumpZ scale sees the
        // display frame time, not the port's fixed 30 Hz sim tick (fn-movement FALLING.md).
        self.sim.controller.component_mut().frame_dt_override = Some(dt);
        let before = self.hud_inputs.selected;
        let mut ai = ArmsInput::default();
        if let Some(i) = self.input.quickbar_press(&self.backend.input.bindings) {
            // only occupied slots can be selected (HudState::select)
            let mut st = hud_state(&self.sim, &self.settings, &self.hud_inputs); st.select(i); self.hud_inputs.selected = st.selected;
            ai.select = Some(st.selected);
        }
        let equip = (self.hud_inputs.selected != before && self.hud_inputs.selected == 0) || self.sim.frames == 0;
        // controller 0
        let cfg = gpad::Config { sens_x: self.settings.input.controller_sensitivity_x * gpad::SENS_UNITS_PER_SLIDER, sens_y: self.settings.input.controller_sensitivity_y * gpad::SENS_UNITS_PER_SLIDER, invert_y: self.backend.input.invert_y };
        self.pad_state = self.pad.poll(dt);
        let pf = self.pad_state.as_ref().map(|s| gpad::map(s, &cfg)).unwrap_or_default();
        let open_menu = gpad::start_released(pf.menu);
        let mut inp = self.input.gather(&self.backend.input.bindings, &self.backend.input, &pf, dt);
        ai.reload_press = self.input.reload_edge(&self.backend.input.bindings);
        inp.equip = equip;
        if self.arms.is_some() { self.drive_arms(dt, &mut inp, ai); }
        else if self.hud_inputs.selected != 0 { inp.attack = false; } // preview items (no loadout resources) must never secretly execute pickaxe combat
        self.sim.step(&inp, dt);
        self.gp_audio.update(self.backend.audio.as_mut(), &self.sim, dt);
        if open_menu { self.open_menu(); }
        true
    }
    fn play_frame(&mut self, el: &ActiveEventLoop, raw_dt: f32, frame_start: Instant) {
        if self.cap.is_some() { self.flow.tick(raw_dt); } // the start-up capture's `StageTime(Playing, ..)` conditions need the flow clock to keep running in the world
        let scripted = self.run.is_some();
        let dt = if let Some(r) = &self.run { 1. / r.driver.fps() } else { raw_dt.min(MAX_DELTA_TIME) };
        if !scripted {
            // gameplay starts captured; capture again after focus returns / the menu closed, unless the menu is open
            if self.input.started && self.input.focused && !self.input.captured && !self.input.user_released && !self.menu_open() { self.try_capture(); }
            if self.input.captured { if let Some(p) = self.platform.as_ref() { p.recenter(); } }
        }
        self.startup_elapsed += dt;
        let work = Instant::now();
        // scripted window resizes happen before the frame is built
        if let (Some(run), Some(p)) = (self.run.as_ref(), self.platform.as_ref()) {
            if let Some((w, h)) = run.driver.resize(run.frame) { let _ = p.window.request_inner_size(PhysicalSize::new(w, h)); }
        }
        // RENDER FX HOOK: the FX world (ring + damage numbers) is loaded once from the original paks, after the scene exists
        if !self.fx_tried && self.scene.is_some() {
            self.fx_tried = true;
            if let Some(dir) = self.install_dir() {
                match fn_pak::PakSet::open_install(&dir).map_err(|e| e.to_string()).and_then(|set| fn_fx::bridge::FxWorld::load(&set, self.scene.as_mut().unwrap().renderer.fx())) {
                    Ok(w) => { for n in &w.notes { eprintln!("[fx] {n}"); } self.fx = Some(w); }
                    Err(e) => eprintln!("[fx] weak-point ring / damage numbers unavailable: {e}"),
                }
            }
        }
        let stepped = self.update_sim(dt);
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
        scene.update(&mut self.sim);
        let camera = camera_for(&self.sim, w, h);
        let hash = pose_hash(scene.character.body_local());
        if let Some(run) = self.run.as_mut() { let f = run.frame; run.log_frame(f, &self.sim, Some(hash)); }
        // ui
        ui.begin();
        let menu_open = ui.menu.is_open();
        self.hud_inputs.frame = self.clock.stats();
        let mut st = hud_state(&self.sim, &self.settings, &self.hud_inputs);
        let harvest_frame = crate::harvest_fx::feed(&mut self.sim, &mut st, &camera, w as f32, h as f32); // HARVEST HOOK: strike events, reticle colour, Target Info
        if !menu_open { ui.present_hud(&st, w as f32, h as f32, if stepped { Some(dt) } else { None }); } else { ui.hud.set_state(&st); }
        crate::harvest_fx::observe_hud(self.sim.frames, &ui.hud); // HARVEST HOOK: run log of the Target Info bar
        if ui.inv.panel.open && !menu_open { let v = ui.inv.view(&st); ui.inv.panel.set_view(v); ui.present_inventory(w as f32, h as f32); } // WIDGETS3 HOOK: AthenaInventoryPanel over the HUD
        if self.input.help_visible && !menu_open { ui.help(w as f32, h as f32); }
        let reqs = {
            let mut cx = MenuContext::new(&mut self.settings, platform, &mut self.backend).with_pad(ui::pad_status(pad_available, pad_connected, pad_connected.then_some(pad_last)));
            ui.menu_frame(&mut cx, w as f32, h as f32, raw_dt)
        };
        ui.black(w as f32, h as f32, fade);
        if platform.vsync_dirty { gfx.set_vsync(platform.vsync); platform.vsync_dirty = false; }
        let time = if let Some(r) = &self.run { r.frame as f32 / r.driver.fps() } else { self.start.elapsed().as_secs_f32() };
        let options = self.backend.render.options.clone();
        // RENDER FX HOOK: strike events -> ring effects, new damage numbers, then this frame's FX draws for the renderer
        if let Some(fx) = self.fx.as_mut() {
            let (tx, ty) = camera.tan_half();
            let view = fn_fx::view::ViewParams { eye: camera.eye, forward: camera.forward, up: camera.up, tan_half: (tx, ty), near_m: camera.near, far_m: camera.far, width: w, height: h, time };
            fx.events(&harvest_frame.fx_events);
            fx.numbers_from(&self.sim.world.fx, &view, time);
            scene.renderer.set_fx_frame(fx.frame(&view, dt, time));
        }
        let cpu_ms = work.elapsed().as_secs_f32() * 1000.;
        let mut shots: Vec<(PathBuf, Vec<u8>, (u32, u32))> = vec![];
        let had_shot_names = self.run.as_mut().map(|r| { let f = r.frame; r.driver.shots(f, &self.sim).len() }).unwrap_or(0);
        let had_shot = had_shot_names > 0;
        match gfx.acquire() {
            Acquire::Frame(frame) => {
                let view = frame.texture.create_view(&Default::default());
                scene.draw(&camera, time, dt, &options, &view);
                ui.render(&view, w, h, None);
                gfx.present(frame);
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
                    for name in run.driver.shots(run.frame, &self.sim) {
                        // the same frame again into an offscreen target of the swapchain format (dt 0: the exposure adaptation does not advance twice)
                        let target = fn_render::OffscreenTarget::new(&gfx.gpu.device, w, h, gfx.format());
                        scene.draw(&camera, time, 0., &options, &target.view);
                        ui.render(&target.view, w, h, None);
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
        if self.run.is_none() { let (cap, vsync) = (self.backend.runtime.frame_cap, self.backend.runtime.vsync); self.limiter.throttle(cap, vsync); }
    }

    // ------------------------------------------------------------------------------------------------------------------------------
    // scripted run epilogue
    // ------------------------------------------------------------------------------------------------------------------------------
    fn finish_run(&mut self, el: &ActiveEventLoop) {
        let Some(run) = self.run.take() else { return };
        let out = run.out.clone();
        let _ = std::fs::create_dir_all(&out);
        let name = run.spec.replace([':', '/', '\\', '.'], "_");
        let mut report = format!("fortnite-rs scripted run: {}\nframes {} in {:.2} s, fixed dt {:.5} s, client sizes seen {:?}\nsettings: {}\n", run.spec, run.frame, run.started.elapsed().as_secs_f32(), 1. / run.driver.fps(), run.sizes_seen, self.settings_note.lines().next().unwrap_or(""));
        report.push_str(&format!("gpu: {}\n", self.gfx.as_ref().map(|g| g.describe()).unwrap_or_default()));
        report.push_str(&run.interval.report("frame interval (wall, incl. present)", 5));
        report.push_str(&run.cpu.report("cpu work per frame (sim + pose + ui + encode)", 5));
        if !run.capture_ms.ms.is_empty() { report.push_str(&run.capture_ms.report("capture frames (offscreen re-render + read-back, excluded above)", 0)); }
        report.push_str(&format!("captures ({}):\n", run.shots.len())); for s in &run.shots { report.push_str(&format!("  {}\n", s.display())); }
        report.push_str(&format!("load steps:\n{}", self.load_log));
        report.push_str(&crate::harvest_fx::run_report(&out, &name)); // HARVEST HOOK
        report.push_str(&crate::particles::run_report(&out, &name)); // PARTICLE HOOK
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
        if vk == keys::VK_RETURN && pressed && !repeat { self.flow.request_skip(); } // skips a fixed hold (splash, start-up minimum, loading minimum, 100 % hold), never real work
        if vk == keys::VK_ESCAPE && pressed && !repeat {
            if self.run.is_some() { el.exit(); return; }
            match self.phase { Phase::Playing => self.open_menu(), Phase::Splash | Phase::StartupLoading | Phase::GamePicker | Phase::Loading | Phase::Finishing | Phase::Failed(_) => { self.flow.note("Esc: closing"); el.exit() }, Phase::Boot => {} }
        }
    }
    pub fn on_mouse_button(&mut self, button: winit::event::MouseButton, pressed: bool) {
        if self.opts.scripted() || self.opts.startup_capture { return; }
        if self.phase == Phase::GamePicker {
            if button == winit::event::MouseButton::Left { let (x, y) = self.input.cursor; self.picker_event(if pressed { UiEvent::MouseDown(UiMouseButton::Left, x, y) } else { UiEvent::MouseUp(UiMouseButton::Left, x, y) }); }
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
    }
    fn on_wheel(&mut self, notches: f32) {
        if self.opts.scripted() { return; }
        if let Some(ui) = self.ui.as_mut() { if ui.inv.panel.open { ui.inv.panel.wheel(notches); return; } } // WIDGETS3 HOOK
        if self.menu_open() { if let Some(ui) = self.ui.as_mut() { ui.menu.event(UiEvent::Wheel(notches)); } } else { self.input.push_wheel(notches); }
    }
}

// ----------------------------------------------------------------------------------------------------------------------------------
// WIDGETS3 HOOK: the Athena inventory screen (`AthenaInventoryPanel`, `crate::inventory`)
// ----------------------------------------------------------------------------------------------------------------------------------
impl App {
    fn inventory_open(&self) -> bool { self.ui.as_ref().map_or(false, |u| u.inv.panel.open) }
    /// Open / close the screen: the mouse is released for it (the cursor drives the panel) and recaptured when it closes, like the menu.
    fn toggle_inventory(&mut self) {
        if self.phase != Phase::Playing || self.menu_open() || self.run.is_some() { return; }
        let Some(ui) = self.ui.as_mut() else { return };
        ui.inv.panel.toggle();
        let open = ui.inv.panel.open;
        if open { let (x, y) = self.input.cursor; ui.inv.panel.mouse_move(x, y); self.release_mouse(); } else { self.input.user_released = false; self.try_capture(); }
    }
    /// Keys while playing: the `ToggleInventory` binding (default Tab, plus I) opens / closes the screen; with it open Esc closes it (`HandleClose`) and Enter is the `Equip` input action.
    /// Returns true when the key was consumed.
    fn inventory_key(&mut self, vk: u16, pressed: bool, repeat: bool) -> bool {
        if self.phase != Phase::Playing || self.menu_open() || self.run.is_some() { return false; }
        let bound = fn_settings::Bindings::action_index("ToggleInventory").map_or(false, |a| self.backend.input.bindings.down(a, |k| k == vk));
        if pressed && !repeat && (bound || vk == 0x49) { self.toggle_inventory(); return true; }
        if !self.inventory_open() { return false; }
        if pressed && !repeat {
            if let Some(ui) = self.ui.as_mut() { match vk { keys::VK_ESCAPE => ui.inv.panel.key_close(), keys::VK_RETURN => ui.inv.panel.key_equip(), _ => {} } }
            self.inventory_apply();
        }
        true
    }
    fn inventory_mouse(&mut self, button: winit::event::MouseButton, pressed: bool) -> bool {
        if !self.inventory_open() || self.menu_open() { return false; }
        if button == winit::event::MouseButton::Left {
            let ((x, y), shift) = (self.input.cursor, self.input.shift);
            if let Some(ui) = self.ui.as_mut() { if pressed { ui.inv.panel.mouse_down(x, y, shift) } else { ui.inv.panel.mouse_up(x, y, shift) } }
            self.inventory_apply();
        }
        true
    }
    /// Resolve the panel's pending actions.  The original panel owns selection;
    /// primary-quickbar drag/drop reaches the same live arms/HUD state it draws.
    fn inventory_apply(&mut self) {
        let a = match self.ui.as_mut() { Some(ui) => ui.inv.apply(), None => return };
        for l in &a.lines { eprintln!("{l}"); }
        for (item, slot) in &a.quickbar_moves {
            let moved = if let Some(arms) = self.arms.as_mut() {
                let from = arms.slot_of_inventory_item(*item);
                match arms.move_inventory_item_to_slot(*item, *slot) {
                    Ok(()) => {
                        if let Some(from) = from { if let Some(items) = self.hud_inputs.items.as_mut() { items.swap(from, *slot); } }
                        true
                    }
                    Err(e) => { eprintln!("[inventory] move item {item} refused: {e}"); false }
                }
            } else { false };
            if !moved { eprintln!("[inventory] move item {item} has no loaded live quickbar"); }
        }
        if let (Some(ui), Some(arms)) = (self.ui.as_mut(), self.arms.as_ref()) { ui.inv.sync_quickbar(&arms.quickbar); }
        if a.close && self.ui.as_ref().is_some_and(|ui| ui.inv.panel.open) { self.toggle_inventory(); }
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
        el.set_control_flow(ControlFlow::Poll);
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
            WindowEvent::KeyboardInput { event, .. } => {
                let vk = match event.physical_key { PhysicalKey::Code(c) => keys::vk(c), PhysicalKey::Unidentified(_) => None };
                if let Some(vk) = vk { self.on_key(el, vk, event.state == ElementState::Pressed, event.repeat); }
            }
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_button(button, state == ElementState::Pressed),
            WindowEvent::CursorMoved { position, .. } => self.on_cursor(position.x as f32, position.y as f32),
            WindowEvent::CursorLeft { .. } => { if let Some(ui) = self.ui.as_mut() { ui.menu.event(UiEvent::MouseLeave); } self.picker_event(UiEvent::MouseLeave); }
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(match delta { MouseScrollDelta::LineDelta(_, y) => y, MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40. }),
            WindowEvent::RedrawRequested => self.frame(el),
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
        if self.minimized { el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50))); } else { el.set_control_flow(ControlFlow::Poll); p.window.request_redraw(); }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        self.release_mouse();
        if let Some(p) = self.platform.as_mut() { if p.is_exclusive() { p.restore_display(); } }
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


