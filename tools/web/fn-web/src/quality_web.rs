//! BROWSER PORT: automatic quality of the browser build (a child module of `app.rs`, compiled only for wasm32).
//!
//! The browser build chooses and adapts its own quality to hold a smooth frame rate: the target is 60 fps, or the display's refresh when that is
//! lower. Each frame records the presented-frame interval (requestAnimationFrame pacing) and, when the adapter has timestamp queries, the GPU time
//! of the frame (`Renderer::last_pass_timings().span_ms`). Every window (>= 1 s and >= 30 frames) is judged on its 90th-percentile interval:
//! * too slow (p90 > target x 1.15, and when the GPU time is known it is > target x 0.75: a slow window with an idle GPU is the main thread or a
//!   first-use hitch, which a lower rung would not fix) twice in a row -> one rung down the [`LADDER`];
//! * comfortably fast (p90 < target x 0.85 and, when known, GPU time < target x 0.70) three windows in a row -> one rung up.
//! A change is followed by one settle window that is not judged. A rung that had to be left downward within 15 s of being reached upward is
//! blocked from going up for 30 s, doubling each time (no oscillation). The rungs lower the 3D resolution (`r.ScreenPercentage`) first, then the
//! scalability groups (shadows, effects, post processing, view distance), then the resolution again. The user's saved settings are not
//! touched: the controller drives the renderer-facing values (`AppBackend::render`) directly. `?fnweb_quality=off` disables it; `?fnweb_rung=N`
//! starts at a rung. The chosen rung is remembered in `/fn/user/fnweb-quality.txt` as the next session's starting point.
use super::*;
use fn_settings::VideoHooks;
use std::cell::RefCell;

/// One quality state: (3D resolution %, shadows, effects, post processing, view distance), levels 0 = low .. 3 = epic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rung { pub res: f32, pub shadows: u8, pub effects: u8, pub post: u8, pub view: u8 }

const fn r(res: f32, shadows: u8, effects: u8, post: u8, view: u8) -> Rung { Rung { res, shadows, effects, post, view } }

/// Best first. WEB 60 (2026-10-06, the user: "turn render distance down to low"): view distance is Low on every rung in the browser.
pub(super) const LADDER: [Rung; 12] = [
    r(100., 3, 3, 3, 0), r(90., 3, 3, 3, 0), r(80., 3, 3, 3, 0), r(80., 2, 3, 3, 0), r(75., 2, 2, 3, 0), r(70., 2, 2, 2, 0),
    r(70., 1, 2, 2, 0), r(62., 1, 1, 2, 0), r(55., 1, 1, 1, 0), r(50., 1, 1, 1, 0), r(50., 0, 1, 1, 0), r(42., 0, 0, 0, 0),
];

/// WEB 60 (2026-10-06): the browser build presents at most `fnweb_fps` frames per second (default 60; 0 = every animation frame). requestAnimationFrame
/// fires at the display refresh (144 Hz on the user's monitor): a frame is rendered when the next 1/60 s slot is due (half a refresh of slack), the slots
/// advance on a fixed grid so the average is exactly the cap, and a late frame never makes the following ones bunch up.
pub(super) static FRAME_CAP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub(super) fn frame_due(cap_fps: u32) -> bool {
    thread_local! { static NEXT: RefCell<Option<Instant>> = const { RefCell::new(None) }; static MIN_IV: RefCell<f32> = const { RefCell::new(f32::MAX) }; static LAST_RAF: RefCell<Option<Instant>> = const { RefCell::new(None) }; }
    FRAME_CAP.store(cap_fps, std::sync::atomic::Ordering::Relaxed);
    let cap = cap_fps as f32;
    if cap <= 0. { FRAME_T0.with(|t| t.set(Some(Instant::now()))); return true; }
    let now = Instant::now();
    // the display refresh: the shortest interval between animation frames seen
    let iv = LAST_RAF.with(|l| l.borrow_mut().replace(now)).map(|p| now.duration_since(p).as_secs_f32()).filter(|v| *v > 0.002);
    let refresh = MIN_IV.with(|m| { let mut m = m.borrow_mut(); if let Some(v) = iv { *m = m.min(v); } *m });
    let period = Duration::from_secs_f32(1. / cap);
    let slack = Duration::from_secs_f32(if refresh < 1. { refresh * 0.5 } else { 0.002 });
    NEXT.with(|n| {
        let mut n = n.borrow_mut();
        let due = n.unwrap_or(now);
        if now + slack < due { return false; }
        let mut next = due + period;
        if next + period < now { next = now + period; } // fell behind (a hitch / a hidden tab): restart the grid
        *n = Some(next);
        FRAME_T0.with(|t| t.set(Some(now)));
        true
    })
}
thread_local! { static FRAME_T0: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) }; }

const LEVEL: [&str; 4] = ["low", "medium", "high", "epic"];

struct Ctl {
    rung: usize,
    applied: Option<usize>,
    samples: Vec<(f32, Option<f32>)>,
    window_start: Instant,
    settle: bool,
    slow_windows: u32,
    fast_windows: u32,
    /// rung -> (blocked-until, backoff seconds) for upward moves into it
    blocked: Vec<(Instant, f32)>,
    /// when each rung was last reached by an upward move
    reached_up: Vec<Option<Instant>>,
    last_gpu_frame: u64,
    shortest_interval: f32,
    note_at: Instant,
    /// first play frame: the first seconds are not judged (the first frames build the props' materials and stream the rest)
    started: Instant,
    last_note: String,
    window_fps: f32,
    window_gpu: Option<f32>,
}

thread_local! { static CTL: RefCell<Option<Ctl>> = const { RefCell::new(None) }; }

fn env_rung() -> Option<usize> { std::env::var("fnweb_rung").ok().and_then(|v| v.parse().ok()).map(|n: usize| n.min(LADDER.len() - 1)) }
fn enabled() -> bool { std::env::var("fnweb_quality").map_or(true, |v| v != "off") }
const SAVE: &str = "/fn/user/fnweb-quality.txt";

/// A first guess from the canvas size: big canvases start lower (a 2560x1369 window is 3.5 MP; the RTX 2060 holds ~55 fps there natively).
fn initial_rung(w: u32, h: u32) -> usize {
    if let Some(n) = env_rung() { return n; }
    let mp = (w as f32 * h as f32) / 1.0e6;
    // the rung the last session settled on, when its canvas was about this size (the file holds "rung megapixels")
    if let Some((n, smp)) = std::fs::read_to_string(SAVE).ok().and_then(|t| { let mut it = t.split_whitespace(); Some((it.next()?.parse::<usize>().ok()?, it.next()?.parse::<f32>().ok()?)) }) {
        if (mp - smp).abs() <= 0.2 * smp.max(0.1) { return n.min(LADDER.len() - 1); }
    }
    // measured on an RTX 2060 (2026-10-06, after the renderer's GPU work got cheaper): a maximized 2560x1369 canvas (3.5 MP) settles between
    // rungs 1 and 3 (GPU 10-16 ms; verification/browser/launch3, launch4), so start there; the ladder climbs or drops from it
    if mp >= 3.0 { 3 } else if mp >= 2.0 { 2 } else if mp >= 1.2 { 1 } else { 0 }
}

fn note(text: &str) {
    let f = js_sys::Reflect::get(&js_sys::global(), &"__fnweb_note".into()).ok().and_then(|f| wasm_bindgen::JsCast::dyn_into::<js_sys::Function>(f).ok());
    if let Some(f) = f { let _ = f.call1(&wasm_bindgen::JsValue::NULL, &text.into()); }
}

fn describe(r: &Rung) -> String {
    format!("3D {:.0}% · shadows {} · effects {} · post {}", r.res, LEVEL[r.shadows as usize], LEVEL[r.effects as usize], LEVEL[r.post as usize])
}

fn apply(app: &mut App, r: &Rung) {
    let rs = &mut app.backend.render;
    let _ = rs.set_screen_percentage(r.res);
    let _ = rs.set_shadow_quality(r.shadows as fn_settings::Level);
    let _ = rs.set_effects_quality(r.effects as fn_settings::Level);
    let _ = rs.set_post_quality(r.post as fn_settings::Level);
}

fn restore_user_settings(app: &mut App) {
    let v = app.settings.video.clone();
    let rs = &mut app.backend.render;
    let _ = rs.set_screen_percentage(fn_settings::screen_percentage(v.resolution_quality));
    let _ = rs.set_shadow_quality(v.shadows);
    let _ = rs.set_effects_quality(v.effects);
    let _ = rs.set_post_quality(v.post_processing);
    let _ = rs.set_view_distance_scale(fn_settings::view_distance_scale(v.view_distance));
}

thread_local! { static FRAMES: RefCell<(u64, Vec<f32>)> = const { RefCell::new((0, Vec::new())) }; }
/// WEB 60: rendered frames so far and the frame intervals (ms) since the last call: [frames, worst, p99, p50, hitches > 25 ms] (the page's stats line).
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_frame_stats() -> Vec<f32> {
    FRAMES.with(|f| {
        let mut f = f.borrow_mut();
        let mut v = std::mem::take(&mut f.1);
        v.sort_by(|a, b| a.total_cmp(b));
        let q = |p: f32| if v.is_empty() { 0. } else { v[((v.len() as f32 * p) as usize).min(v.len() - 1)] };
        vec![f.0 as f32, v.last().copied().unwrap_or(0.), q(0.99), q(0.5), v.iter().filter(|x| **x > 25.).count() as f32]
    })
}

thread_local! {
    static BENCH: RefCell<Option<Vec<f32>>> = const { RefCell::new(None) };
    static BENCH_STATE: RefCell<[f32; 8]> = const { RefCell::new([0.; 8]) };
    static BENCH_YAW: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
    static BENCH_GPU_FRAME: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static BENCH_POI: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_bench_start() { BENCH.with(|b| *b.borrow_mut() = Some(Vec::with_capacity(10 * 8192))); }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_bench_take() -> Vec<f32> { BENCH.with(|b| b.borrow_mut().take().unwrap_or_default()) }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_bench_state() -> Vec<f32> { BENCH_STATE.with(|s| s.borrow().to_vec()) }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_bench_yaw(yaw: f32) { BENCH_YAW.with(|y| y.set(Some(yaw))); }
thread_local! { static BENCH_PITCH_SET: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) }; }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_bench_pitch(pitch: f32) { BENCH_PITCH_SET.with(|p| p.set(pitch.is_finite().then_some(pitch))); }

fn bench_frame(app: &mut App, raw_dt: f32) {
    if let Some(y) = BENCH_YAW.with(|y| y.take()) { app.sim.yaw = y.rem_euclid(360.); }
    static PITCH: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    if let Some(p) = *PITCH.get_or_init(|| std::env::var("FNWEB_BENCH_PITCH").ok().and_then(|v| v.parse().ok())) { app.sim.pitch = p; }
    if let Some(p) = BENCH_PITCH_SET.with(|p| p.get()) { app.sim.pitch = p; }
    let menu = app.menu_open() as u8 as f32 + 2. * app.inventory_open() as u8 as f32;
    let feet = app.sim.feet();
    let frames = FRAMES.with(|f| f.borrow().0) as f32;
    BENCH_STATE.with(|s| *s.borrow_mut() = [app.sim.x, feet.y, app.sim.z, app.sim.yaw, menu, app.input.captured as u8 as f32, frames, raw_dt * 1000.]);
    let cpu = FRAME_T0.with(|t| t.get()).map_or(0., |t| t.elapsed().as_secs_f32() * 1000.);
    if BENCH.with(|b| b.borrow().is_none()) {
        static TAGS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TAGS.get_or_init(|| std::env::var("FNWEB_HITCH_TAGS").is_ok_and(|v| v == "1") || std::env::var("FNWEB_SECTIONS_LOG").is_ok_and(|v| v == "1")) { hitch_tag(app, raw_dt * 1000., cpu, 0., 0, 0., fn_web_rt::vfs::timing().1 as f32); }
        return;
    }
    let last = BENCH_GPU_FRAME.with(|g| g.get());
    let gpu = app.scene.as_ref().map(|sc| sc.renderer.last_pass_timings()).filter(|t| t.supported && t.span_ms > 0. && t.frame != last).map_or(0., |t| { BENCH_GPU_FRAME.with(|g| g.set(t.frame)); t.span_ms });
    let poi = fn_web_rt::POI_KINDS_BUILT.load(std::sync::atomic::Ordering::Relaxed);
    let dpoi = poi.saturating_sub(BENCH_POI.with(|p| p.replace(poi)));
    let pending = app.scene.as_mut().map_or(0, |s| s.renderer.fx().pending_pipelines()) as f32;
    let wait = fn_web_rt::vfs::timing().1 as f32;
    hitch_tag(app, raw_dt * 1000., cpu, gpu, dpoi, pending, wait);
    BENCH.with(|b| if let Some(v) = b.borrow_mut().as_mut() { v.extend_from_slice(&[raw_dt * 1000., cpu, gpu, app.sim.x, feet.y, app.sim.z, menu, dpoi as f32, pending, wait]); });
}

pub(super) fn pre_counters() -> (f32, f32, f32) { (fn_web_rt::mem::MAIN_LOCK_WAIT_US.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000., fn_web_rt::vfs::timing().1 as f32, fn_web_rt::mem::GROW_US.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.) }

fn sections_avg(secs: &[(&'static str, f32)]) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var("FNWEB_SECTIONS_LOG").is_ok_and(|v| v == "1")) { return; }
    thread_local! { static ACC: RefCell<(u32, Vec<(&'static str, f32)>)> = const { RefCell::new((0, Vec::new())) }; }
    ACC.with(|a| {
        let mut a = a.borrow_mut();
        a.0 += 1;
        for &(k, v) in secs { match a.1.iter_mut().find(|e| e.0 == k) { Some(e) => e.1 += v, None => a.1.push((k, v)) } }
        if a.0 >= 600 {
            let n = a.0 as f32;
            let mut v: Vec<(&str, f32)> = a.1.iter().map(|&(k, t)| (k, t / n)).collect();
            v.sort_by(|x, y| y.1.total_cmp(&x.1));
            eprintln!("[sections] ms/frame {}", v.iter().take(14).map(|(k, t)| format!("{k} {t:.3}")).collect::<Vec<_>>().join(", "));
            a.0 = 0; a.1.clear();
        }
    });
}

fn hitch_tag(app: &mut App, iv: f32, cpu: f32, gpu: f32, dpoi: u64, pending: f32, wait: f32) {
    use std::sync::atomic::Ordering::Relaxed;
    thread_local! { static PREV: RefCell<(String, f32, i64, usize, i64, f32)> = const { RefCell::new((String::new(), 0., 0, 0, 0, 0.)) }; }
    let secs = crate::statics_render::take_frame_sections();
    sections_avg(&secs);
    let (lock, grows, grow_us) = (fn_web_rt::mem::MAIN_LOCK_WAIT_US.load(Relaxed), fn_web_rt::mem::GROWS.load(Relaxed), fn_web_rt::mem::GROW_US.load(Relaxed));
    let lr = fn_web_rt::mem::LOCK_REPORT.swap(0, Relaxed);
    if lr != 0 { eprintln!("[heaplock main] waited {} ms, holder changes {}, last holder worker {}", lr >> 32, (lr >> 16) & 0xffff, (lr & 0xffff) - 2); }
    thread_local! { static MH: std::cell::Cell<(u32, usize)> = const { std::cell::Cell::new((0, 0)) }; }
    static MH_ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *MH_ON.get_or_init(|| std::env::var("FNWEB_HITCH_TAGS").is_ok_and(|v| v == "1")) { MH.with(|m| { let (n, last) = m.get(); let st = fn_web_rt::mem::main_heap_stats(); if n >= 2400 || (st.1 != last && n >= 120) { eprintln!("[mainheap] region used {:.1} MB, fallback spins {}, allocs {}, deferred frees {}, main grows denied {}", st.0 as f64 / 1048576., st.1, st.2, st.3, fn_web_rt::mem::GROW_DENIED.load(std::sync::atomic::Ordering::Relaxed)); m.set((0, st.1)); } else { m.set((n + 1, last)); } }); }
    PREV.with(|p| {
        let mut p = p.borrow_mut();
        if p.1 == 0. && p.2 == 0 && p.3 == 0 { p.2 = lock; p.3 = grows; p.4 = grow_us; p.5 = wait; fn_web_rt::mem::GROW_MAX_US.store(0, Relaxed); }
        let (dl, dg, dgu, dw) = (lock - p.2, grows - p.3, grow_us - p.4, wait - p.5);
        p.2 = lock; p.3 = grows; p.4 = grow_us; p.5 = wait;
        if iv > 50. && p.1 < 30. && cpu < 30. { eprintln!("[hitchtag] gap iv {iv:.0} ms: prev cpu {:.0}, this cpu {cpu:.0}, gpu {gpu:.1}, pend {pending}, grows {dg} ({:.1} ms), prev: {}", p.1, dgu as f32 / 1000., p.0); }
        if cpu > 30. || iv > 50. || fn_render::fx::frame_upload_bytes() > (4 << 20) {
            let mut s: Vec<(&str, f32)> = secs.iter().filter(|e| e.1 >= 1.).map(|e| (e.0, e.1)).collect();
            s.sort_by(|a, b| b.1.total_cmp(&a.1));
            let mut r: Vec<(String, f32)> = app.scene.as_ref().and_then(|sc| sc.last_stats.as_ref()).map(|st| st.passes.iter().filter(|x| x.encode_ms >= 1.).map(|x| (x.name.clone(), x.encode_ms)).collect()).unwrap_or_default();
            r.sort_by(|a, b| b.1.total_cmp(&a.1));
            let gi = if dg > 0 { let m = &fn_web_rt::mem::GROW_MAX_US; format!(" [grow max {:.1} ms, main {}, want {} KB, free {} MB, largest {} KB]", m.swap(0, Relaxed) as f32 / 1000., fn_web_rt::mem::GROW_MAIN.load(Relaxed), fn_web_rt::mem::GROW_WANT_KB.load(Relaxed), fn_web_rt::mem::GROW_FREE_KB.load(Relaxed) >> 10, fn_web_rt::mem::GROW_LARGEST_KB.load(Relaxed)) } else { String::new() };
            let tag = format!("sections [{}] render [{}] lock {:.1} ms, grows {dg} ({:.1} ms){gi}, poi {dpoi}, pend {pending}, filewait {dw:.0} ms, upload {:.1} MB",
                s.iter().take(6).map(|(k, v)| format!("{k} {v:.0}")).collect::<Vec<_>>().join(", "), r.iter().take(4).map(|(k, v)| format!("{k} {v:.0}")).collect::<Vec<_>>().join(", "), dl as f32 / 1000., dgu as f32 / 1000., fn_render::fx::frame_upload_bytes() as f32 / 1048576.);
            if cpu > 30. || iv > 50. { eprintln!("[hitchtag] f={} iv {iv:.0} cpu {cpu:.0}: {tag}", FRAMES.with(|f| f.borrow().0)); }
            p.0 = tag;
        } else { p.0.clear(); }
        p.1 = cpu;
    });
}

/// Called once per `play_frame`, after the frame was presented.
pub(super) fn after_frame(app: &mut App, raw_dt: f32) {
    FRAMES.with(|f| { let mut f = f.borrow_mut(); f.0 += 1; if raw_dt > 0. && raw_dt < 5. && f.1.len() < 4096 { f.1.push(raw_dt * 1000.); } });
    // the page hides its loading indicator after the first in-game frame
    static PLAYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static FIRST_FRAME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    static CACHE_TRIMMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !PLAYING.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let f = js_sys::Reflect::get(&js_sys::global(), &"__fnweb_phase".into()).ok().and_then(|f| wasm_bindgen::JsCast::dyn_into::<js_sys::Function>(f).ok());
        if let Some(f) = f { let _ = f.call1(&wasm_bindgen::JsValue::NULL, &"playing".into()); }
        eprintln!("[web] first in-game frame presented");
        if let Some(w) = fn_fx::webtex::stats() { eprintln!("[webtex] at the first frame: {w}"); }
        { let (n, w, r) = fn_web_rt::webcook_files::stats(); if n > 0 { eprintln!("[webcook] files overlay at the first frame: {n} files, {w:.1} MB over the wire -> {r:.1} MB"); } }
        // PIPELINE DEDUP / async pipelines: what the load created (fn_render::fx)
        if let Some(scene) = app.scene.as_mut() { let fx = scene.renderer.fx(); let pending = fx.pending_pipelines(); eprintln!("{} | still compiling: {pending}", fx.pipeline_cache_report()); eprintln!("[props] procedural prop vertices {}, static instances {}", scene.renderer.prop_vertex_count(), scene.renderer.static_instance_count()); }
        let _ = FIRST_FRAME.set(Instant::now());
        eprintln!("[webtex] {}", fn_fx::webtex::low_stats());
        fn_fx::gpu::enable_upgrades();
        fn_fx::gpu::note_play_started();
        crate::landscape_render::prewarm();
    }
    if let Some(scene) = app.scene.as_mut() { fn_fx::gpu::upgrade_step(scene.renderer.fx(), 4 << 20); }
    hitch_log(app, raw_dt, FIRST_FRAME.get().copied());
    bench_frame(app, raw_dt);
    // SPAWN CHECK 2: the pawn 3 s into play (after the first ticks), against the landscape and the collision floor under it
    static SPAWN2: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if FIRST_FRAME.get().is_some_and(|t| t.elapsed().as_secs_f32() > 3.) && !SPAWN2.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let (x, z) = (app.sim.x, app.sim.z);
        let g = app.sim.ground_y(x, z);
        let f = app.sim.floor_below(glam::Vec3::new(x, g + fn_game::SPAWN_PROBE_M, z));
        let feet = app.sim.feet().y;
        eprintln!("[spawn] 3 s into play: feet {feet:.2} m, landscape {g:.2} m, collision floor {f:.2} m{}", if feet + 0.05 < f { " (BELOW THE FLOOR)" } else { "" });
    }
    perf_log(app, raw_dt, FIRST_FRAME.get().copied());
    // MEMORY: the file block cache serves the load and the first in-game frames' lazy loads; 15 s into play a small one is enough
    if FIRST_FRAME.get().is_some_and(|t| t.elapsed().as_secs_f32() > 15.) && !CACHE_TRIMMED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        fn_web_rt::vfs::set_cache_mb(16);
        fn_pak::webcook::set_cache_budget(16 << 20);
    }
    if !enabled() || app.run.is_some() { return; }
    if app.settings.video.preset != fn_settings::Preset::Auto {
        let was_driving = CTL.with(|c| c.borrow_mut().take().is_some_and(|ctl| ctl.applied.is_some()));
        if was_driving {
            restore_user_settings(app);
            note("");
            eprintln!("[auto-quality] off: Quality is not auto, the video settings apply");
        }
        return;
    }
    let (w, h) = app.gfx.as_ref().map_or((0, 0), |g| g.size());
    let gpu = app.scene.as_ref().map(|s| s.renderer.last_pass_timings()).filter(|t| t.supported && t.span_ms > 0.).map(|t| (t.frame, t.span_ms));
    CTL.with(|c| {
        let mut c = c.borrow_mut();
        let now = Instant::now();
        let ctl = c.get_or_insert_with(|| {
            let rung = initial_rung(w, h);
            Ctl { rung, applied: None, samples: vec![], window_start: now, settle: true, slow_windows: 0, fast_windows: 0, blocked: vec![(now, 0.); LADDER.len()], reached_up: vec![None; LADDER.len()], last_gpu_frame: 0, shortest_interval: f32::MAX, note_at: now, started: now, last_note: String::new(), window_fps: 0., window_gpu: None }
        });
        if ctl.applied != Some(ctl.rung) {
            apply(app, &LADDER[ctl.rung]);
            ctl.applied = Some(ctl.rung);
            eprintln!("[auto-quality] rung {} of {}: {} (canvas {w}x{h})", ctl.rung, LADDER.len() - 1, describe(&LADDER[ctl.rung]));
            let _ = std::fs::write(SAVE, format!("{} {:.2}\n", ctl.rung, (w as f32 * h as f32) / 1.0e6));
        }
        if std::env::var("fnweb_quality").is_ok_and(|v| v == "hold") { return; }
        let interval = raw_dt * 1000.;
        if interval > 0. && interval < 1000. { ctl.shortest_interval = ctl.shortest_interval.min(interval); }
        let gpu_ms = gpu.filter(|(f, _)| *f != ctl.last_gpu_frame).map(|(f, ms)| { ctl.last_gpu_frame = f; ms });
        if interval < 1000. { ctl.samples.push((interval, gpu_ms)); }
        let elapsed = now.duration_since(ctl.window_start).as_secs_f32();
        if now.duration_since(ctl.started).as_secs_f32() < 4.0 { ctl.samples.clear(); ctl.window_start = now; return; }
        if elapsed < 1.0 || ctl.samples.len() < 30 { return; }
        // the window
        let mut iv: Vec<f32> = ctl.samples.iter().map(|s| s.0).collect();
        iv.sort_by(|a, b| a.total_cmp(b));
        let p90 = iv[(iv.len() * 9 / 10).min(iv.len() - 1)];
        let mut gv: Vec<f32> = ctl.samples.iter().filter_map(|s| s.1).collect();
        gv.sort_by(|a, b| a.total_cmp(b));
        let gpu_med = (!gv.is_empty()).then(|| gv[gv.len() / 2]);
        ctl.window_fps = ctl.samples.len() as f32 / elapsed;
        ctl.window_gpu = gpu_med;
        ctl.samples.clear();
        ctl.window_start = now;
        // target: 60 fps, or the display refresh when it is lower (the shortest interval seen ~ one refresh)
        let refresh_ms = ctl.shortest_interval.max(1000. / 240.);
        let cap = FRAME_CAP.load(std::sync::atomic::Ordering::Relaxed);
        let target = if cap > 0 { (1000. / cap as f32).max(refresh_ms * 0.98) } else { refresh_ms * 0.98 };
        let txt = format!("auto quality: {}\n{:.0} fps{}", describe(&LADDER[ctl.rung]), ctl.window_fps, gpu_med.map_or(String::new(), |g| format!(" · GPU {g:.1} ms")));
        if txt != ctl.last_note || now.duration_since(ctl.note_at).as_secs_f32() > 5. { note(&txt); ctl.last_note = txt; ctl.note_at = now; }
        if std::mem::take(&mut ctl.settle) { return; }
        // a slow window whose GPU time is low is CPU-bound (main thread, first-use loads): a lower rung would only cost quality
        // WEB 60: under the 60 fps cap the presented intervals alternate whole refreshes (13.9 / 20.8 ms at 144 Hz), so a capped window is judged on
        // its rendered rate (slow below 95% of the target rate) and, for going up, on the GPU time
        let capped = cap > 0;
        let rate_ok = ctl.window_fps >= 0.95 * 1000. / target;
        let slow = if capped { !rate_ok } else { p90 > target * 1.15 } && gpu_med.map_or(true, |g| g > target * 0.75);
        let fast = if capped { rate_ok && p90 < target * 1.3 } else { p90 < target * 0.85 } && gpu_med.map_or(true, |g| g < target * 0.70);
        if slow { ctl.slow_windows += 1; ctl.fast_windows = 0; } else if fast { ctl.fast_windows += 1; ctl.slow_windows = 0; } else { ctl.slow_windows = 0; ctl.fast_windows = 0; }
        let from = ctl.rung;
        if ctl.slow_windows >= 2 && ctl.rung + 1 < LADDER.len() {
            // left soon after an upward move: block that upward move for a while (doubling)
            if let Some(t) = ctl.reached_up[ctl.rung] { if now.duration_since(t).as_secs_f32() < 15. { let b = &mut ctl.blocked[ctl.rung]; b.1 = if b.1 == 0. { 30. } else { (b.1 * 2.).min(600.) }; b.0 = now + Duration::from_secs_f32(b.1); } }
            ctl.rung += 1;
        } else if ctl.fast_windows >= 3 && ctl.rung > 0 && now >= ctl.blocked[ctl.rung - 1].0 {
            ctl.rung -= 1;
            ctl.reached_up[ctl.rung] = Some(now);
        }
        if ctl.rung != from {
            eprintln!("[auto-quality] {:.1} fps (p90 frame {:.1} ms, target {:.1} ms{}) -> rung {} {}: {}", ctl.window_fps, p90, target, gpu_med.map_or(String::new(), |g| format!(", GPU {g:.1} ms")), ctl.rung, if ctl.rung > from { "down" } else { "up" }, describe(&LADDER[ctl.rung]));
            ctl.slow_windows = 0; ctl.fast_windows = 0; ctl.settle = true;
        }
    });
}

/// Rung, frame rate and GPU time of the last window (for the page / tests).
pub(super) fn state() -> Option<(usize, f32, Option<f32>)> { CTL.with(|c| c.borrow().as_ref().map(|c| (c.rung, c.window_fps, c.window_gpu))) }

/// HITCHES (2026-10-07): every frame over 20 ms during the first 60 s of play, with what the frame did that can stall the main thread (file reads
/// it waited for, requests, POI kinds built, pipelines still compiling), and a summary at 60 s (`[hitch] first 60 s`, parsed by the test harness).
fn hitch_log(app: &mut App, raw_dt: f32, first: Option<Instant>) {
    struct H { last: (f64, u64, u64), over20: u32, over33: u32, worst: f32, done: bool }
    thread_local! { static S: std::cell::RefCell<H> = const { std::cell::RefCell::new(H { last: (0., 0, 0), over20: 0, over33: 0, worst: 0., done: false }) }; }
    let Some(first) = first else { return };
    let t = first.elapsed().as_secs_f32();
    let now = (fn_web_rt::vfs::timing().1, fn_web_rt::vfs::stats().0, fn_web_rt::POI_KINDS_BUILT.load(std::sync::atomic::Ordering::Relaxed));
    let pending = app.scene.as_mut().map_or(0, |s| s.renderer.fx().pending_pipelines());
    S.with(|s| {
        let mut s = s.borrow_mut();
        let (wait, reqs, poi) = (now.0 - s.last.0, now.1 - s.last.1, now.2 - s.last.2);
        s.last = now;
        if t < 0.05 { return; }
        if s.done { if raw_dt * 1000. > 80. { let mut why = vec![]; if wait > 1. { why.push(format!("waited {wait:.0} ms for files")); } if reqs > 0 { why.push(format!("{reqs} file requests")); } if poi > 0 { why.push(format!("{poi} POI kinds built")); } if pending > 0 { why.push(format!("{pending} pipelines compiling")); } eprintln!("[hitch] t={t:.1}s frame {:.0} ms: {}", raw_dt * 1000., if why.is_empty() { "no file / POI / pipeline work".to_owned() } else { why.join(", ") }); } return; }
        if t > 60. {
            s.done = true;
            eprintln!("[hitch] first 60 s of play: {} frames > 33 ms, {} frames > 20 ms, worst {:.0} ms", s.over33, s.over20, s.worst);
            if let Some(w) = fn_fx::webtex::stats() { eprintln!("[webtex] {w}"); }
            let (q, r, d, mb) = fn_fx::gpu::upgrade_stats();
            eprintln!("[webtex] {}; full-resolution upgrades: {d} swapped ({mb:.0} MB), {r} ready, {q} queued", fn_fx::webtex::low_stats());
            return;
        }
        let ms = raw_dt * 1000.;
        if ms <= 20. { return; }
        s.over20 += 1; if ms > 33. { s.over33 += 1; } s.worst = s.worst.max(ms);
        let mut why = vec![];
        if wait > 1. { why.push(format!("waited {wait:.0} ms for files")); }
        if reqs > 0 { why.push(format!("{reqs} file requests")); }
        if poi > 0 { why.push(format!("{poi} POI kinds built")); }
        if pending > 0 { why.push(format!("{pending} pipelines compiling")); }
        eprintln!("[hitch] t={t:.1}s frame {ms:.0} ms: {}", if why.is_empty() { "no file / POI / pipeline work (render or GC)".to_owned() } else { why.join(", ") });
    });
}

/// PERF (2026-10-07): every 10 s of play, the frame interval, the main thread's work per frame (from the animation frame's start to the end of the
/// game's frame) and the GPU time (timestamp queries: whole span + the four costliest passes): `[perf] ...` lines, parsed by the test harness.
fn perf_log(app: &mut App, raw_dt: f32, first: Option<Instant>) {
    struct P { start: Option<Instant>, iv: Vec<f32>, cpu: Vec<f32>, gpu: Vec<f32>, passes: std::collections::HashMap<String, (f32, u32)>, last_gpu_frame: u64, steps: std::collections::HashMap<String, f32>, frames: u32 }
    thread_local! { static S: std::cell::RefCell<Option<P>> = const { std::cell::RefCell::new(None) }; }
    if first.is_none() { return; }
    let cpu = FRAME_T0.with(|t| t.get()).map(|t| t.elapsed().as_secs_f32() * 1000.);
    S.with(|s| {
        let mut s = s.borrow_mut();
        let p = s.get_or_insert_with(|| P { start: Some(Instant::now()), iv: vec![], cpu: vec![], gpu: vec![], passes: Default::default(), last_gpu_frame: 0, steps: Default::default(), frames: 0 });
        if let Some(st) = app.scene.as_ref().and_then(|sc| sc.last_stats.as_ref()) { p.frames += 1; for x in &st.passes { *p.steps.entry(x.name.clone()).or_default() += x.encode_ms; } }
        if raw_dt > 0. && raw_dt < 1. { p.iv.push(raw_dt * 1000.); }
        if let Some(c) = cpu { p.cpu.push(c); }
        if let Some(t) = app.scene.as_ref().map(|sc| sc.renderer.last_pass_timings()).filter(|t| t.supported && t.frame != p.last_gpu_frame && t.span_ms > 0.) {
            p.last_gpu_frame = t.frame;
            p.gpu.push(t.span_ms);
            for x in &t.passes { let e = p.passes.entry(x.name.clone()).or_default(); e.0 += x.ms; e.1 += 1; }
        }
        if p.start.is_some_and(|t| t.elapsed().as_secs_f32() >= 10.) {
            let pct = |v: &mut Vec<f32>, q: f32| -> f32 { if v.is_empty() { return 0.; } v.sort_by(|a, b| a.total_cmp(b)); v[((v.len() - 1) as f32 * q) as usize] };
            let n = p.iv.len().max(1);
            let fps = n as f32 / (p.iv.iter().sum::<f32>().max(1.) / 1000.);
            let mut top: Vec<(String, f32)> = p.passes.iter().map(|(k, (ms, c))| (k.clone(), ms / (*c).max(1) as f32)).collect();
            top.sort_by(|a, b| b.1.total_cmp(&a.1));
            let tops = top.iter().take(4).map(|(k, ms)| format!("{k} {ms:.2}")).collect::<Vec<_>>().join(", ");
            let mut st: Vec<(String, f32)> = p.steps.iter().map(|(k, v)| (k.clone(), v / p.frames.max(1) as f32)).collect();
            st.sort_by(|a, b| b.1.total_cmp(&a.1));
            let cpu_steps = st.iter().take(8).map(|(k, ms)| format!("{k} {ms:.2}")).collect::<Vec<_>>().join(", ");
            eprintln!("[perf] {fps:.1} fps; interval p50 {:.1} p95 {:.1} ms; main thread p50 {:.1} p95 {:.1} ms; GPU p50 {:.1} p95 {:.1} ms ({tops}); render CPU per frame: {cpu_steps}",
                pct(&mut p.iv, 0.5), pct(&mut p.iv, 0.95), pct(&mut p.cpu, 0.5), pct(&mut p.cpu, 0.95), pct(&mut p.gpu, 0.5), pct(&mut p.gpu, 0.95));
            *p = P { start: Some(Instant::now()), iv: vec![], cpu: vec![], gpu: vec![], passes: Default::default(), last_gpu_frame: p.last_gpu_frame, steps: Default::default(), frames: 0 };
        }
    });
}
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_pso_dump() -> Vec<u8> { fn_render::fx::pso_recording() }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_pso_count() -> usize { fn_render::fx::pso_recorded() }
