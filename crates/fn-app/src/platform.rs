//! `fn_settings::PlatformHost` over winit: window mode, resolution (video modes), vsync (present mode), cursor capture, focus.
//! The behaviour list is the one in `fn_settings::platform` (documented there from the old Win32/WGL code):
//!
//! * display modes = the monitor's real video modes, >= 640x360, >= 24-bit colour;
//! * **Windowed**: leave any exclusive/borderless state, decorated window, client size `fit_windowed_client(w, h, work area, frame)` centred in the
//!   monitor's real work area (`GetMonitorInfoW`, not a taskbar guess);
//! * **WindowedFullscreen** = winit borderless fullscreen on the window's monitor;
//! * **Fullscreen** = winit exclusive fullscreen with the video mode `pick_refresh` chooses (current refresh when the size offers it, else the
//!   highest); no matching mode -> `Err`, the window is left untouched;
//! * vsync is recorded here and applied by `Gfx` (present mode) after the settings apply;
//! * **cursor capture = the old runtime's policy** (`lock_mouse` / `release_mouse` of the prototype's `main.rs`), see [`capture`] and [`release`]:
//!   never unless the window is focused AND the foreground window and not minimised; move the cursor to the client centre FIRST; then grab (a 1x1 clip at
//!   the centre); hide the cursor only when a grab succeeded; verify the real clip rectangle lies inside the client area (else undo); release is
//!   unconditional (`ClipCursor(NULL)`), also while unfocused. Look comes from raw `DeviceEvent::MouseMotion`, not from the pointer.
use crate::sys::{self, Rect};
use fn_settings::{fit_windowed_client, pick_refresh, DisplayMode, PlatformHost, WindowMode, FALLBACK_WINDOWED};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::monitor::{MonitorHandle, VideoModeHandle};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{CursorGrabMode, Fullscreen, Window};

pub const MIN_CLIENT: (u32, u32) = (640, 360);
/// Largest client size per axis: wgpu's default `max_texture_dimension_2d` (8192), the limit every adapter we support offers. A swap chain or HDR target
/// beyond the DEVICE's limit is a validation error (the old crash), so every size is clamped here and again to the real device limit in `gfx.rs`.
/// Kept equal to `fn_settings::MAX_RES`, which clamps the saved value when the settings are loaded.
pub const MAX_CLIENT: (u32, u32) = (8192, 8192);
/// Height (px) assumed for the taskbar ONLY when the OS gives no work area (never on Windows).
pub const TASKBAR_ESTIMATE: u32 = 48;
/// After a failed capture attempt the next one waits this long (the app retries every frame; each attempt moves the cursor to the centre).
pub const CAPTURE_RETRY: Duration = Duration::from_millis(750);

// ---------------------------------------------------------------------------------------------------------------------------------
// cursor capture policy (pure, driven through a trait so the order of operations is unit-tested)
// ---------------------------------------------------------------------------------------------------------------------------------
/// What the capture policy needs from the window system.
pub trait CursorOps {
    /// The window has keyboard focus AND is the foreground window.
    fn focused_foreground(&self) -> bool;
    fn minimized(&self) -> bool;
    /// Client centre in the window's own client coordinates (physical pixels); None for an empty client.
    fn client_center(&self) -> Option<(f64, f64)>;
    /// Moves the pointer to a client coordinate. False when the OS refused.
    fn set_position(&mut self, x: f64, y: f64) -> bool;
    fn grab(&mut self, mode: CursorGrabMode) -> bool;
    fn set_visible(&mut self, visible: bool);
    /// Unconditional OS-level `ClipCursor(NULL)` (winit skips its own release while the window is unfocused).
    fn release_clip(&mut self);
    /// After a grab: `Some(true)` when the real clip rectangle is a tiny box inside the client area holding the pointer, `Some(false)` when it is
    /// not (pinned somewhere else, or unclipped), None when it cannot be read.
    fn clip_ok(&self) -> Option<bool>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureOutcome { Captured(CursorGrabMode), Refused(&'static str), GrabFailed }

/// The grab modes in the order they are tried. Windows/winit 0.30: `Confined` while the cursor is hidden clips to the 1x1 client centre computed by winit
/// itself, `Locked` clips to wherever the pointer is at that instant; Confined is the deterministic one, Locked the fallback.
pub const GRAB_ORDER: [CursorGrabMode; 2] = [CursorGrabMode::Confined, CursorGrabMode::Locked];

/// Old `lock_mouse`: refuse unless focused + foreground + not minimised + a non-empty client; centre first; grab; hide only after a verified grab.
pub fn capture(ops: &mut dyn CursorOps) -> CaptureOutcome {
    if !ops.focused_foreground() { return CaptureOutcome::Refused("window is not focused / foreground"); }
    if ops.minimized() { return CaptureOutcome::Refused("window is minimised"); }
    let Some((cx, cy)) = ops.client_center() else { return CaptureOutcome::Refused("empty client area") };
    if !ops.set_position(cx, cy) { return CaptureOutcome::Refused("cannot move the pointer to the client centre"); }
    for mode in GRAB_ORDER {
        if !ops.grab(mode) { continue; }
        ops.set_visible(false);
        if ops.clip_ok() == Some(false) { release(ops); continue; } // grabbed, but the clip is not where the policy says: undo and try the next mode
        return CaptureOutcome::Captured(mode);
    }
    ops.release_clip();
    CaptureOutcome::GrabFailed
}
/// Old `release_mouse`: always safe to call, in any state.
pub fn release(ops: &mut dyn CursorOps) {
    let _ = ops.grab(CursorGrabMode::None);
    ops.release_clip();
    ops.set_visible(true);
}

/// Verdict on a capture: `clip` / `client` in screen pixels. Captured -> the clip is a tiny box inside the client area and holds the pointer;
/// released -> the clip is not a tiny pinned box.
pub fn audit_capture(captured: bool, clip: Option<Rect>, client: Rect, cursor: Option<(i32, i32)>) -> (bool, String) {
    let tiny = |c: Rect| c.2 - c.0 <= 4 && c.3 - c.1 <= 4;
    let inside = |c: Rect| c.0 >= client.0 && c.1 >= client.1 && c.2 <= client.2 && c.3 <= client.3;
    let holds = |c: Rect, p: (i32, i32)| p.0 >= c.0 && p.0 < c.2.max(c.0 + 1) && p.1 >= c.1 && p.1 < c.3.max(c.1 + 1);
    let (ok, why) = match (captured, clip) {
        (_, None) => (false, "clip rectangle unreadable"),
        (true, Some(c)) => if !tiny(c) { (false, "clip is not a pinned box") } else if !inside(c) { (false, "clip lies outside the client area") } else if cursor.map_or(false, |p| !holds(c, p)) { (false, "pointer is outside the clip") } else { (true, "pinned inside the client area") },
        (false, Some(c)) => if tiny(c) { (false, "pointer is still pinned after the release") } else { (true, "free") },
    };
    (ok, format!("{} clip={clip:?} client={client:?} cursor={cursor:?}: {why}", if captured { "captured" } else { "released" }))
}

/// Where to put the hidden pointer, or `None` to leave it alone: there must be a capture, a known client centre and an OS
/// that reports the pointer's position (Windows). Off Windows the position is unreadable, so the pointer is never warped
/// (the grab pins it anyway).
pub fn recenter_target(captured: bool, center: Option<(f64, f64)>, cursor: Option<(i32, i32)>, client: Option<Rect>) -> Option<(f64, f64)> {
    if !captured { return None; }
    let c = center?;
    let (p, cl) = (cursor?, client?);
    let want = (cl.0 + c.0 as i32, cl.1 + c.1 as i32);
    (p != want).then_some(c)
}

// ---------------------------------------------------------------------------------------------------------------------------------
// winit
// ---------------------------------------------------------------------------------------------------------------------------------

/// [`CursorOps`] over a real window.
struct WindowCursor<'a> { window: &'a Window }
impl WindowCursor<'_> {
    fn client_screen_rect(&self) -> Option<Rect> { client_screen_rect(self.window) }
}
impl CursorOps for WindowCursor<'_> {
    fn focused_foreground(&self) -> bool { self.window.has_focus() && is_foreground(self.window) }
    fn minimized(&self) -> bool { self.window.is_minimized() == Some(true) }
    fn client_center(&self) -> Option<(f64, f64)> {
        let s = self.window.inner_size();
        fn_game::host::capture_center((0, 0, s.width as i32, s.height as i32)).map(|(x, y)| (x as f64, y as f64))
    }
    fn set_position(&mut self, x: f64, y: f64) -> bool { self.window.set_cursor_position(PhysicalPosition::new(x, y)).is_ok() }
    fn grab(&mut self, mode: CursorGrabMode) -> bool { self.window.set_cursor_grab(mode).is_ok() }
    fn set_visible(&mut self, visible: bool) { self.window.set_cursor_visible(visible); }
    fn release_clip(&mut self) { sys::release_cursor_clip(); }
    fn clip_ok(&self) -> Option<bool> {
        let (clip, client) = (sys::clip_rect()?, self.client_screen_rect()?);
        Some(audit_capture(true, Some(clip), client, sys::cursor_pos()).0)
    }
}

/// Client area in screen pixels (left, top, right, bottom).
pub fn client_screen_rect(w: &Window) -> Option<Rect> {
    let (p, s) = (w.inner_position().ok()?, w.inner_size());
    Some((p.x, p.y, p.x + s.width as i32, p.y + s.height as i32))
}
fn hwnd_of(w: &Window) -> Option<isize> { match w.window_handle().ok()?.as_raw() { RawWindowHandle::Win32(h) => Some(h.hwnd.get()), _ => None } }
/// The window is the OS foreground window (always true where the OS cannot say).
pub fn is_foreground(w: &Window) -> bool { match hwnd_of(w) { Some(h) if cfg!(windows) => sys::foreground_window() == h, _ => true } }

#[derive(Clone, Debug, PartialEq)]
pub struct CaptureAudit { pub ok: bool, pub text: String }

pub struct WinitPlatform {
    pub window: Arc<Window>,
    captured: bool,
    /// Requested swap interval (applied to the surface by `Gfx::apply_vsync`).
    pub vsync: bool,
    pub vsync_dirty: bool,
    pub mode: WindowMode,
    /// How the cursor is currently grabbed (None = capture failed or not captured).
    pub grab: Option<CursorGrabMode>,
    /// Why the last capture attempt did not capture (None after a success / before any attempt).
    pub last_refusal: Option<String>,
    retry_at: Option<Instant>,
    /// Verdict of the real OS clip rectangle after the last capture / release (also printed as a `[capture]` line): the live test reads it.
    pub last_audit: Option<CaptureAudit>,
    /// OS video modes of the current monitor, reused for [`MODE_CACHE_TTL`].
    pub modes: ModeCache,
}

pub fn monitor_of(w: &Window) -> Option<MonitorHandle> { w.current_monitor().or_else(|| w.primary_monitor()).or_else(|| w.available_monitors().next()) }

/// The usable display modes (>= 640x360, >= 24-bit) of a list of video-mode handles, sorted and de-duplicated.
pub fn modes_from(handles: &[VideoModeHandle]) -> Vec<DisplayMode> {
    let mut v: Vec<DisplayMode> = handles.iter().filter(|vm| vm.bit_depth() >= 24 && vm.size().width >= MIN_CLIENT.0 && vm.size().height >= MIN_CLIENT.1)
        .map(|vm| DisplayMode { width: vm.size().width, height: vm.size().height, hz: (vm.refresh_rate_millihertz() + 500) / 1000 }).collect();
    v.sort_by_key(|m| (m.width, m.height, m.hz)); v.dedup(); v
}
/// Enumerates the monitor's video modes through the OS (`EnumDisplaySettings` for every mode: 20-50 ms on this machine, so never in a loop; see [`ModeCache`]).
pub fn enumerate_modes(m: &MonitorHandle) -> Vec<VideoModeHandle> {
    let t = Instant::now();
    let v: Vec<VideoModeHandle> = m.video_modes().collect();
    let ms = t.elapsed().as_secs_f32() * 1000.;
    if ms > crate::gfx::SLOW_MS { eprintln!("[slow] enumerating {} video modes took {ms:.1} ms", v.len()); }
    v
}
pub fn modes_of(m: &MonitorHandle) -> Vec<DisplayMode> { modes_from(&enumerate_modes(m)) }

/// How long an enumeration is reused (the set of modes a monitor offers does not change while the game runs; a different monitor or an exclusive mode
/// switch changes the cache key, and `WinitPlatform::new` fills the cache while the window is still hidden so the first Esc menu finds it hot).
/// The Settings screen asks "is this size supported?" once per listed resolution (~20 calls per frame it builds the
/// Video page); each used to enumerate every mode again: ~500 ms in one frame, the first-Esc-menu / window-mode-switch hitch.
pub const MODE_CACHE_TTL: Duration = Duration::from_secs(600);
/// Per-monitor cache of the OS video-mode list. Keyed by the monitor's position and current size, so a monitor change or an exclusive mode switch misses.
#[derive(Default)]
pub struct ModeCache { entry: Option<((i32, i32, u32, u32), Instant, Vec<VideoModeHandle>)>, pub enumerations: u32 }
impl ModeCache {
    pub fn key(m: &MonitorHandle) -> (i32, i32, u32, u32) { let (p, s) = (m.position(), m.size()); (p.x, p.y, s.width, s.height) }
    pub fn get(&mut self, m: &MonitorHandle, now: Instant) -> &[VideoModeHandle] { self.get_with(Self::key(m), now, || enumerate_modes(m)) }
    /// [`ModeCache::get`] with the key and the enumeration supplied (unit-testable without a monitor).
    pub fn get_with(&mut self, key: (i32, i32, u32, u32), now: Instant, enumerate: impl FnOnce() -> Vec<VideoModeHandle>) -> &[VideoModeHandle] {
        let fresh = matches!(&self.entry, Some((k, at, _)) if *k == key && now.duration_since(*at) < MODE_CACHE_TTL);
        if !fresh { self.entry = Some((key, now, enumerate())); self.enumerations += 1; }
        &self.entry.as_ref().expect("just filled").2
    }
    pub fn invalidate(&mut self) { self.entry = None; }
}
/// The monitor's usable area (minus taskbar) in virtual-screen pixels: the OS work area, else the monitor rectangle minus [`TASKBAR_ESTIMATE`].
pub fn work_area_of(m: &MonitorHandle) -> Rect {
    #[cfg(windows)]
    { use winit::platform::windows::MonitorHandleExtWindows; if let Some(r) = sys::monitor_work_area(m.hmonitor()) { if r.2 > r.0 && r.3 > r.1 { return r; } } }
    let (p, s) = (m.position(), m.size());
    (p.x, p.y, p.x + s.width as i32, p.y + s.height.saturating_sub(TASKBAR_ESTIMATE).max(1) as i32)
}

/// Client size + outer position of a windowed window inside `work` (virtual-screen rectangle), with `frame` non-client overhead. The client is clamped
/// to the work area and to `fn_settings::MAX_RES`.
pub fn windowed_geometry_in(w: u32, h: u32, work: Rect, frame: (i32, i32)) -> ((u32, u32), (i32, i32)) {
    let (ww, wh) = ((work.2 - work.0).max(1), (work.3 - work.1).max(1));
    let (cw, ch) = fit_windowed_client(w.min(MAX_CLIENT.0), h.min(MAX_CLIENT.1), (ww, wh), frame);
    let outer = (cw as i32 + frame.0, ch as i32 + frame.1);
    ((cw, ch), (work.0 + ((ww - outer.0) / 2).max(0), work.1 + ((wh - outer.1) / 2).max(0)))
}
/// A client size a run may ask for (`--size`, script `size` / `resize`): at least [`MIN_CLIENT`], at most the `fn_settings::MAX_RES` texture limit.
/// Out-of-range values are an error with a clear message instead of being silently raised / crashing the swap chain.
pub fn check_client_size(w: u32, h: u32, what: &str) -> Result<(u32, u32), String> {
    if w < MIN_CLIENT.0 || h < MIN_CLIENT.1 { return Err(format!("{what} {w}x{h} is below the minimum window client size {}x{}", MIN_CLIENT.0, MIN_CLIENT.1)); }
    if w > MAX_CLIENT.0 || h > MAX_CLIENT.1 { return Err(format!("{what} {w}x{h} exceeds the {}x{} limit (the largest swap chain / texture every supported GPU accepts)", MAX_CLIENT.0, MAX_CLIENT.1)); }
    Ok((w, h))
}
/// [`windowed_geometry_in`] on a monitor of `mon` pixels at the origin with the estimated taskbar.
pub fn windowed_geometry(w: u32, h: u32, mon: (u32, u32), frame: (i32, i32)) -> ((u32, u32), (i32, i32)) {
    windowed_geometry_in(w, h, (0, 0, mon.0 as i32, mon.1.saturating_sub(TASKBAR_ESTIMATE).max(1) as i32), frame)
}
/// Client size to create the window with: the saved size, repaired (zero / tiny -> fallback or minimum, above the limits -> the limits) and fitted to the
/// primary monitor's work area (guessing the usual 16 x 39 px frame). `app.rs` should call this instead of using the raw saved size (see README, Handoff).
pub fn initial_client_size(w: u32, h: u32, primary: Option<&MonitorHandle>) -> (u32, u32) {
    let work = primary.map(work_area_of).unwrap_or((0, 0, 1920, 1080 - TASKBAR_ESTIMATE as i32));
    windowed_geometry_in(w, h, work, (16, 39)).0
}

impl WinitPlatform {
    pub fn new(window: Arc<Window>) -> Self {
        let mut modes = ModeCache::default();
        if let Some(m) = monitor_of(&window) { modes.get(&m, Instant::now()); } // pre-warm: 20-50 ms of EnumDisplaySettings now instead of in the first Esc-menu frame
        WinitPlatform { window, captured: false, vsync: true, vsync_dirty: false, mode: WindowMode::Windowed, grab: None, last_refusal: None, retry_at: None, last_audit: None, modes }
    }
    fn frame_overhead(&self) -> (i32, i32) { let (o, i) = (self.window.outer_size(), self.window.inner_size()); (o.width as i32 - i.width as i32, o.height as i32 - i.height as i32) }
    pub fn is_exclusive(&self) -> bool { matches!(self.window.fullscreen(), Some(Fullscreen::Exclusive(_))) }
    /// Client centre in physical pixels (None for an empty / minimised client).
    pub fn client_center(&self) -> Option<PhysicalPosition<f64>> { WindowCursor { window: &self.window }.client_center().map(|(x, y)| PhysicalPosition::new(x, y)) }
    /// Keeps the hidden pointer on the client centre. The grab already pins it there, so this only acts when the OS can
    /// report the pointer's position and it is somewhere else — see [`recenter_target`]. Without a verifiable position the
    /// pointer is left alone: a blind warp every frame fights the device (on macOS `set_cursor_position` re-associates the
    /// mouse with the cursor, so the reported motion cancels the warp and the view snaps back to where it started).
    pub fn recenter(&self) {
        if !self.captured { return; }
        let target = recenter_target(true, self.client_center().map(|c| (c.x, c.y)), sys::cursor_pos(), client_screen_rect(&self.window));
        if let Some((x, y)) = target { let _ = self.window.set_cursor_position(PhysicalPosition::new(x, y)); }
    }
    /// Reads the real OS clip rectangle and judges it (see [`audit_capture`]).
    pub fn audit(&self) -> Option<CaptureAudit> {
        let (clip, client) = (sys::clip_rect()?, client_screen_rect(&self.window)?);
        let (ok, text) = audit_capture(self.captured, Some(clip), client, sys::cursor_pos());
        Some(CaptureAudit { ok, text })
    }
    fn note_audit(&mut self) {
        if let Some(a) = self.audit() { eprintln!("[capture] {} {}", if a.ok { "ok" } else { "VIOLATION" }, a.text); self.last_audit = Some(a); }
    }
    pub fn apply_mode(&mut self, mode: WindowMode, w: u32, h: u32) -> Result<(), String> {
        let win = &self.window;
        let mon = monitor_of(win);
        match mode {
            WindowMode::Windowed => {
                win.set_fullscreen(None);
                win.set_decorations(true);
                let work = mon.as_ref().map(work_area_of).unwrap_or((0, 0, 1920, 1080 - TASKBAR_ESTIMATE as i32));
                let ((cw, ch), (px, py)) = windowed_geometry_in(w, h, work, self.frame_overhead());
                let _ = win.request_inner_size(PhysicalSize::new(cw, ch));
                win.set_outer_position(PhysicalPosition::new(px, py));
            }
            WindowMode::WindowedFullscreen => { win.set_fullscreen(Some(Fullscreen::Borderless(mon))); }
            WindowMode::Fullscreen => {
                let mon = mon.ok_or("no monitor")?;
                let handles: Vec<VideoModeHandle> = self.modes.get(&mon, Instant::now()).to_vec();
                let modes = modes_from(&handles);
                let (w, h) = if w == 0 || h == 0 { FALLBACK_WINDOWED } else { (w, h) };
                let current_hz = (mon.refresh_rate_millihertz().unwrap_or(60_000) + 500) / 1000;
                let hz = pick_refresh(&modes, w, h, current_hz);
                if hz == 0 { return Err(format!("the display does not offer {w}x{h}")); }
                let vm = handles.iter().filter(|vm| vm.size() == PhysicalSize::new(w, h) && vm.bit_depth() >= 24).cloned()
                    .min_by_key(|vm| ((vm.refresh_rate_millihertz() as i64 - hz as i64 * 1000).abs(), std::cmp::Reverse(vm.bit_depth())))
                    .ok_or_else(|| format!("no video mode {w}x{h}@{hz}"))?;
                win.set_fullscreen(Some(Fullscreen::Exclusive(vm)));
            }
        }
        self.mode = mode;
        Ok(())
    }
}

/// Runs `f`, logging a `[slow]` line when it took longer than `gfx::SLOW_MS`.
pub fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now(); let r = f();
    let ms = t.elapsed().as_secs_f32() * 1000.;
    if ms > crate::gfx::SLOW_MS { eprintln!("[slow] {what} took {ms:.1} ms"); }
    r
}

impl PlatformHost for WinitPlatform {
    fn display_modes(&mut self) -> Vec<DisplayMode> { match monitor_of(&self.window) { Some(m) => modes_from(self.modes.get(&m, Instant::now())), None => vec![] } }
    fn current_display_mode(&mut self) -> Option<DisplayMode> {
        let m = monitor_of(&self.window)?; let s = m.size();
        Some(DisplayMode { width: s.width, height: s.height, hz: (m.refresh_rate_millihertz().unwrap_or(0) + 500) / 1000 })
    }
    fn exclusive_mode_supported(&mut self, w: u32, h: u32) -> bool { match monitor_of(&self.window) { Some(m) => self.modes.get(&m, Instant::now()).iter().any(|vm| vm.size() == PhysicalSize::new(w, h) && vm.bit_depth() >= 24), None => false } }
    fn apply_display(&mut self, mode: WindowMode, w: u32, h: u32) -> Result<(), String> { timed(&format!("apply_display {mode:?} {w}x{h}"), || self.apply_mode(mode, w, h)) }
    fn restore_display(&mut self) { self.window.set_fullscreen(None); }
    fn set_swap_interval(&mut self, on: bool) -> bool { if self.vsync != on { self.vsync = on; self.vsync_dirty = true; } true }
    fn swap_interval(&self) -> Option<i32> { Some(self.vsync as i32) }
    fn set_cursor_captured(&mut self, captured: bool) {
        if captured {
            if self.captured { return; }
            if self.retry_at.map_or(false, |t| Instant::now() < t) { return; }
            let outcome = capture(&mut WindowCursor { window: &self.window });
            match outcome {
                CaptureOutcome::Captured(mode) => { self.captured = true; self.grab = Some(mode); self.last_refusal = None; self.retry_at = None; self.note_audit(); }
                CaptureOutcome::Refused(why) => { self.last_refusal = Some(why.to_owned()); }
                CaptureOutcome::GrabFailed => {
                    self.last_refusal = Some("the OS refused every cursor grab mode".to_owned());
                    self.retry_at = Some(Instant::now() + CAPTURE_RETRY);
                    eprintln!("[capture] the OS refused every cursor grab mode: the mouse stays free and visible");
                }
            }
        } else {
            release(&mut WindowCursor { window: &self.window });
            let was = self.captured;
            self.captured = false; self.grab = None; self.retry_at = None;
            if was { self.note_audit(); }
        }
    }
    fn cursor_captured(&self) -> bool { self.captured }
    fn window_focused(&self) -> bool { self.window.has_focus() && is_foreground(&self.window) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test] fn windowed_geometry_fits_and_centres() {
        let ((w, h), (x, y)) = windowed_geometry(1280, 720, (1920, 1080), (16, 39));
        assert_eq!((w, h), (1280, 720)); assert_eq!((x, y), ((1920 - 1296) / 2, (1080 - 48 - 759) / 2));
    }
    #[test] fn oversized_requests_shrink_to_the_work_area_and_zero_falls_back() {
        let ((w, h), _) = windowed_geometry(3840, 2160, (1920, 1080), (16, 39)); assert_eq!((w, h), (1904, 1080 - 48 - 39));
        let ((w, h), _) = windowed_geometry(0, 0, (1920, 1080), (16, 39)); assert_eq!((w, h), FALLBACK_WINDOWED);
        let ((w, h), (x, y)) = windowed_geometry(100, 100, (1920, 1080), (0, 0)); assert!(w >= 640 && h >= 360 && x >= 0 && y >= 0);
    }
    #[test] fn the_texture_limit_wins_even_on_a_huge_work_area() {
        let work = (0, 0, 20000, 20000);
        assert_eq!(windowed_geometry_in(9000, 720, work, (16, 39)).0, (8192, 720));
        assert_eq!(windowed_geometry_in(1280, 9000, work, (16, 39)).0, (1280, 8192));
        assert_eq!(windowed_geometry_in(99999, 1, work, (16, 39)).0, (8192, MIN_CLIENT.1));
        assert_eq!(windowed_geometry_in(u32::MAX, u32::MAX, work, (0, 0)).0, (8192, 8192));
    }
    #[test] fn a_work_area_on_a_second_monitor_keeps_absolute_coordinates() {
        // taskbar on top of a monitor to the left of the primary: work area (-1920, 40) .. (0, 1080)
        let ((w, h), (x, y)) = windowed_geometry_in(1280, 720, (-1920, 40, 0, 1080), (16, 39));
        assert_eq!((w, h), (1280, 720)); assert_eq!((x, y), (-1920 + (1920 - 1296) / 2, 40 + (1040 - 759) / 2));
        assert!(x >= -1920 && x + 1296 <= 0 && y >= 40 && y + 759 <= 1080);
    }
    #[test] fn the_mode_cache_enumerates_once_per_ttl_and_per_monitor_state() {
        let mut c = ModeCache::default(); let t0 = Instant::now(); let k = (0, 0, 2560, 1440);
        for _ in 0..40 { c.get_with(k, t0, Vec::new); }
        assert_eq!(c.enumerations, 1, "40 `is this size supported?` queries in one frame cost ONE OS enumeration");
        c.get_with(k, t0 + MODE_CACHE_TTL - Duration::from_millis(1), Vec::new); assert_eq!(c.enumerations, 1, "still fresh");
        c.get_with(k, t0 + MODE_CACHE_TTL, Vec::new); assert_eq!(c.enumerations, 2, "expired");
        c.get_with((0, 0, 1920, 1080), t0 + MODE_CACHE_TTL, Vec::new); assert_eq!(c.enumerations, 3, "a different monitor / exclusive mode is a different key");
        c.invalidate(); c.get_with((0, 0, 1920, 1080), t0 + MODE_CACHE_TTL, Vec::new); assert_eq!(c.enumerations, 4, "invalidate forces a refresh");
    }
    #[test] fn run_sizes_outside_the_window_limits_are_errors() {
        assert_eq!(check_client_size(640, 360, "--size"), Ok((640, 360))); assert_eq!(check_client_size(8192, 8192, "x"), Ok((8192, 8192)));
        for (w, h) in [(100, 100), (639, 360), (640, 359), (0, 0)] { let e = check_client_size(w, h, "--size").unwrap_err(); assert!(e.contains("minimum") && e.contains(&format!("{w}x{h}")), "{e}"); }
        for (w, h) in [(9000, 500), (1280, 9000), (99999, 1), (u32::MAX, 720)] { assert!(check_client_size(w, h, "resize").is_err(), "{w}x{h}"); }
        assert!(check_client_size(9000, 500, "resize").unwrap_err().contains("8192"));
    }
    #[test] fn initial_size_repairs_nonsense_without_a_monitor() {
        assert_eq!(initial_client_size(9000, 720, None).1, 720); assert!(initial_client_size(9000, 720, None).0 <= 1920);
        assert_eq!(initial_client_size(0, 0, None), FALLBACK_WINDOWED);
    }

    // ----- capture policy against a recording mock -----
    #[derive(Default)]
    struct Mock {
        focused: bool, minimized: bool, empty: bool, move_ok: bool, grab_ok: Vec<CursorGrabMode>, clip_ok: Option<bool>,
        log: RefCell<Vec<String>>,
    }
    impl Mock { fn ready() -> Mock { Mock { focused: true, move_ok: true, grab_ok: vec![CursorGrabMode::Confined, CursorGrabMode::Locked], clip_ok: Some(true), ..Default::default() } } fn log(&self) -> Vec<String> { self.log.borrow().clone() } }
    impl CursorOps for Mock {
        fn focused_foreground(&self) -> bool { self.focused }
        fn minimized(&self) -> bool { self.minimized }
        fn client_center(&self) -> Option<(f64, f64)> { (!self.empty).then_some((640., 360.)) }
        fn set_position(&mut self, x: f64, y: f64) -> bool { self.log.borrow_mut().push(format!("pos {x},{y}")); self.move_ok }
        fn grab(&mut self, mode: CursorGrabMode) -> bool { self.log.borrow_mut().push(format!("grab {mode:?}")); mode == CursorGrabMode::None || self.grab_ok.contains(&mode) }
        fn set_visible(&mut self, v: bool) { self.log.borrow_mut().push(format!("visible {v}")); }
        fn release_clip(&mut self) { self.log.borrow_mut().push("release_clip".into()); }
        fn clip_ok(&self) -> Option<bool> { self.clip_ok }
    }
    #[test] fn the_pointer_is_only_warped_when_its_position_is_known_and_off_centre() {
        let client = Some((100, 50, 1380, 770));
        // captured, pointer readable and elsewhere -> warp to the client centre (640,360 absolute 740,410)
        assert_eq!(recenter_target(true, Some((640., 360.)), Some((700, 400)), client), Some((640., 360.)));
        // already at the centre -> nothing to do
        assert_eq!(recenter_target(true, Some((640., 360.)), Some((740, 410)), client), None);
        // no capture / no client centre -> nothing to do
        assert_eq!(recenter_target(false, Some((640., 360.)), Some((0, 0)), client), None);
        assert_eq!(recenter_target(true, None, Some((0, 0)), client), None);
        // the OS cannot say where the pointer is (macOS, Linux): never warp blindly
        assert_eq!(recenter_target(true, Some((640., 360.)), None, client), None);
        assert_eq!(recenter_target(true, Some((640., 360.)), Some((0, 0)), None), None);
    }
    #[test] fn capture_centres_first_then_grabs_then_hides() {
        let mut m = Mock::ready();
        assert_eq!(capture(&mut m), CaptureOutcome::Captured(CursorGrabMode::Confined));
        assert_eq!(m.log(), ["pos 640,360", "grab Confined", "visible false"], "centre FIRST, hide only after the grab");
    }
    #[test] fn capture_falls_back_to_locked_and_never_hides_when_every_grab_fails() {
        let mut m = Mock::ready(); m.grab_ok = vec![CursorGrabMode::Locked];
        assert_eq!(capture(&mut m), CaptureOutcome::Captured(CursorGrabMode::Locked));
        assert_eq!(m.log(), ["pos 640,360", "grab Confined", "grab Locked", "visible false"]);
        let mut m = Mock::ready(); m.grab_ok = vec![];
        assert_eq!(capture(&mut m), CaptureOutcome::GrabFailed);
        assert!(!m.log().iter().any(|l| l == "visible false"), "no grab -> the cursor stays visible: {:?}", m.log());
        assert_eq!(m.log().last().unwrap(), "release_clip");
    }
    #[test] fn capture_is_refused_unfocused_minimised_empty_or_when_the_pointer_cannot_be_centred() {
        for (what, mut m, expect) in [
            ("unfocused", Mock { focused: false, ..Mock::ready() }, "focused"), ("minimised", Mock { minimized: true, ..Mock::ready() }, "minimised"),
            ("empty", Mock { empty: true, ..Mock::ready() }, "empty"), ("cannot centre", Mock { move_ok: false, ..Mock::ready() }, "centre"),
        ] {
            match capture(&mut m) { CaptureOutcome::Refused(why) => assert!(why.contains(expect), "{what}: {why}"), o => panic!("{what}: {o:?}") }
            assert!(!m.log().iter().any(|l| l.starts_with("grab") || l == "visible false"), "{what}: nothing may be grabbed or hidden: {:?}", m.log());
        }
        assert!(Mock { focused: false, ..Mock::ready() }.log().is_empty());
    }
    #[test] fn a_grab_that_is_not_pinned_inside_the_client_is_undone() {
        let mut m = Mock::ready(); m.clip_ok = Some(false);
        assert_eq!(capture(&mut m), CaptureOutcome::GrabFailed);
        let log = m.log(); assert!(log.iter().filter(|l| *l == "release_clip").count() >= 3, "{log:?}");
        assert_eq!(log.last().unwrap(), "release_clip");
        let mut m = Mock::ready(); m.clip_ok = None; assert!(matches!(capture(&mut m), CaptureOutcome::Captured(_)), "unreadable clip is accepted");
    }
    #[test] fn release_is_unconditional_and_shows_the_cursor() {
        let mut m = Mock::ready(); release(&mut m);
        assert_eq!(m.log(), ["grab None", "release_clip", "visible true"]);
    }
    #[test] fn audit_judges_the_real_clip_rectangle() {
        let client = (100, 50, 1380, 770); // 1280x720 client at (100, 50)
        let (cx, cy) = (100 + 640, 50 + 360);
        assert!(audit_capture(true, Some((cx, cy, cx + 1, cy + 1)), client, Some((cx, cy))).0, "the old runtime's 1x1 clip at the client centre");
        assert!(!audit_capture(true, Some((10, 10, 11, 11)), client, Some((10, 10))).0, "pinned OUTSIDE the window (the old Locked bug)");
        assert!(!audit_capture(true, Some(client), client, Some((cx, cy))).0, "a full-client clip is not a pinned capture");
        assert!(!audit_capture(true, Some((cx, cy, cx + 1, cy + 1)), client, Some((cx + 30, cy))).0, "pointer escaped the clip");
        assert!(!audit_capture(true, None, client, None).0);
        assert!(audit_capture(false, Some((-1920, 0, 3840, 1440)), client, None).0, "released = whole desktop");
        assert!(!audit_capture(false, Some((cx, cy, cx + 1, cy + 1)), client, None).0, "still pinned after the release");
    }

    // ----- real window -----
    /// A hidden, unfocused window must never capture: no clip, cursor visible, pointer untouched (no OS input is generated).
    #[cfg(windows)]
    #[test] fn a_real_unfocused_window_refuses_to_capture_and_leaves_the_pointer_alone() {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        let Ok(el) = winit::event_loop::EventLoop::builder().with_any_thread(true).build() else { eprintln!("no event loop: skipped"); return };
        #[allow(deprecated)]
        let Ok(window) = el.create_window(Window::default_attributes().with_visible(false).with_inner_size(PhysicalSize::new(800u32, 600u32))) else { eprintln!("no window: skipped"); return };
        let mut p = WinitPlatform::new(Arc::new(window));
        let (clip0, cur0) = (sys::clip_rect(), sys::cursor_pos());
        p.set_cursor_captured(true);
        assert!(!p.cursor_captured() && p.grab.is_none(), "{:?}", p.last_refusal);
        assert!(p.last_refusal.as_deref().map_or(false, |w| w.contains("focused")), "{:?}", p.last_refusal);
        assert_eq!(sys::clip_rect(), clip0, "no clip appeared"); assert_eq!(sys::cursor_pos(), cur0, "the pointer was not moved");
        p.set_cursor_captured(false); assert_eq!(sys::clip_rect(), clip0);
    }
}
