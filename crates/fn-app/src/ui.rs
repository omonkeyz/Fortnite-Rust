//! The UI of the app: `fn-ui` screens (splash, loading screen, HUD, Esc menu + Settings) built into one [`RenderList`] per frame and drawn by
//! `fn-ui-wgpu` on top of the 3D scene (or over black before the world exists).
//!
//! Draw order of the old runtime: HUD (unless the menu covers it), F8 help card, FPS counter, menu, black fade.
use crate::assets::AssetRoots;
use fn_ui::screens::hud::HudState;
use fn_ui::draw::RenderList;
use fn_ui::input::{Event, PadButton, PadState};
use fn_ui::layout::Rect;
use fn_ui::menu::pages::{self, UiContext as MenuContext};
use fn_ui::menu::{lower, HostRequest, MenuSystem};
use fn_ui::scene::UiContext;
use fn_ui::screens::hud::Hud;
use fn_ui::screens::picker::{self, Picker};
use fn_ui::screens::startup_loading::StartupLoadingScreen;
use fn_ui::screens::{put_text, Snap};
use fn_ui::text::FontId;
use fn_ui_wgpu::UiRenderer;

pub struct UiLayer {
    pub ctx: UiContext,
    pub renderer: UiRenderer,
    pub menu: MenuSystem,
    pub hud: Hud,
    pub list: RenderList,
    pad_nav: PadState,
    /// Start-up screens (state-less) and the GPU objects needed to pre-upload art.
    startup: StartupLoadingScreen,
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    prewarm_target: Option<fn_render::OffscreenTarget>,
    /// WIDGETS3 HOOK: the Athena inventory screen (panel state + item texts + action host).
    pub inv: crate::inventory::InventoryUi,
    /// FRONT END: the cooked "Logging In..." screen and game picker (`frontend_cooked`), composed once by [`UiLayer::load_front_end`]; the screens fall back
    /// to the older ones when a tree could not be composed.
    pub front: Option<fn_ui::screens::frontend_cooked::FrontEnd>,
}

pub const WHITE: [f32; 4] = [1., 1., 1., 1.];
/// LOAD COUNTER: text size, left inset and baseline height above the bottom edge (Slate units; clear of the start-up screen's LOADING row and bar).
const COUNTER_SIZE: f32 = 18.;
const COUNTER_X: f32 = 16.;
const COUNTER_BOTTOM: f32 = 96.;
pub fn pad_status(available: bool, connected: bool, last: Option<fn_game::inputdev::pad::State>) -> fn_ui::inputdev::pad::PadStatus {
    fn_ui::inputdev::pad::PadStatus { available, connected, last: last.map(|s| fn_ui::inputdev::pad::State { buttons: s.buttons, lt: s.lt, rt: s.rt, lx: s.lx, ly: s.ly, rx: s.rx, ry: s.ry }) }
}

impl UiLayer {
    /// overlay = images the start-up worker decodes from the paks (front-end art); it is the first layer of the UI's asset source, the asset roots follow.
    pub fn new(roots: &AssetRoots, device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat, overlay: crate::app::startup::Overlay) -> Result<UiLayer, String> {
        let source = fn_ui::assets::LayeredAssets::new(vec![Box::new(overlay), roots.ui_source()]);
        let ctx = UiContext::new(Box::new(source))?;
        let renderer = UiRenderer::with_device(device.clone(), queue, format);
        let mut menu = MenuSystem::new(ctx.fonts.clone());
        pages::register_all_into(&mut menu);
        Ok(UiLayer { ctx, renderer, menu, hud: Hud::new(), list: RenderList::new(), pad_nav: PadState::default(), startup: StartupLoadingScreen, device, format, prewarm_target: None, inv: Default::default(), front: None })
    }

    pub fn begin(&mut self) { self.list.clear(); }

    /// The splash artwork (`Splash.bmp`, 640x738) centred and shrunk to fit; black elsewhere.
    pub fn splash(&mut self, w: f32, h: f32, alpha: f32) {
        let t = self.ctx.assets.texture("Splash.bmp");
        if t.id == fn_ui::draw::TextureId::NONE { return; }
        let k = (w / t.w as f32).min(h / t.h as f32).min(1.);
        let (dw, dh) = (t.w as f32 * k, t.h as f32 * k);
        self.list.sprite(t.id, Rect::new(((w - dw) * 0.5).round(), ((h - dh) * 0.5).round(), dw, dh), [1., 1., 1., alpha]);
    }
    /// The start-up loading screen (	 = seconds since it appeared; rt_ready = the pak art was delivered AND uploaded).
    /// FRONT END: `fade` fades only the plate / EARLY ACCESS / LOADING layer (`bFlag == 0`); the art stays (the exe's two layers, as the live client shows).
    pub fn startup_loading(&mut self, w: f32, h: f32, t: f32, progress: f32, art_ready: bool, fade: f32) { self.startup.build_layers(&mut self.ctx, w, h, t, progress, art_ready, 1. - fade.clamp(0., 1.), 1., &mut self.list); }
    /// FRONT END: composes the cooked login screen + game picker from the install (one-shot, UI thread; returns the composition note when one failed).
    pub fn load_front_end(&mut self, install: &std::path::Path, display_name: &str) -> Option<String> {
        let mut fe = fn_ui::screens::frontend_cooked::FrontEnd::load(install);
        if let Some(p) = fe.picker.as_mut() { p.set_display_name(display_name); }
        let note = fe.note.clone();
        self.front = Some(fe);
        note
    }
    /// FRONT END: the cooked `LoginScreen_C` ("Logging In..." + the running login step's status text), `t` = seconds on screen. Behind its empty
    /// `LoginBackground` the 1.8 client shows the 3D front-end level (`Frontend_BG_Main`, not rendered yet: black). Without the cooked tree: the start-up screen.
    /// `art_alpha`: the start-up screen's art layer (`bFlag == 1`) fading out over it when the login screen appears (the live client: ~3.6 s).
    pub fn login(&mut self, w: f32, h: f32, t: f32, art_ready: bool, step: usize, art_alpha: f32) {
        match self.front.as_mut().and_then(|f| f.login.as_mut()) {
            Some(l) => { let list = l.draw(&mut self.ctx, w, h, t, step); self.list.extend(list); }
            None => self.startup.build(&mut self.ctx, w, h, t, 1., art_ready, 0., &mut self.list),
        }
        if art_alpha > 0. { self.startup.build_layers(&mut self.ctx, w, h, t, 1., art_ready, 0., art_alpha.min(1.), &mut self.list); }
    }
    pub fn sign_in(&mut self, w: f32, h: f32, st: &fn_ui::screens::frontend_cooked::SignInState) -> bool {
        match self.front.as_mut().and_then(|f| f.sign_in.as_mut()) {
            Some(c) => { let list = c.draw(&mut self.ctx, w, h, st); self.list.extend(list); true }
            None => false,
        }
    }
    pub fn sign_in_hit(&self, x: f32, y: f32) -> Option<fn_ui::screens::frontend_cooked::SignInHit> { self.front.as_ref().and_then(|f| f.sign_in.as_ref()).and_then(|c| c.hit(x, y)) }
    pub fn has_sign_in(&self) -> bool { self.front.as_ref().map_or(false, |f| f.sign_in.is_some()) }
    pub fn set_display_name(&mut self, name: &str) { if let Some(p) = self.front.as_mut().and_then(|f| f.picker.as_mut()) { p.set_display_name(name); } }
    /// The game picker: the cooked `SubgameSelectScreen_C` (FRONT END), else the older picker.
    pub fn picker(&mut self, p: &Picker, w: f32, h: f32, art_ready: bool) {
        match self.front.as_mut().and_then(|f| f.picker.as_mut()) {
            Some(c) => { let list = c.draw(&mut self.ctx, w, h, p); self.list.extend(list); }
            None => p.build(&mut self.ctx, w, h, art_ready, &mut self.list),
        }
    }
    /// FRONT END: lays out and paints the cooked picker once off screen (its full-window background material is rastered on the CPU: done behind the login
    /// screen, not on the picker's first frame).
    pub fn warm_picker_step(&mut self, w: f32, h: f32, k: usize) -> Option<bool> { match self.front.as_mut().and_then(|f| f.picker.as_mut()) { Some(c) => c.warm_step(&mut self.ctx, w, h, k), None => Some(true) } }
    pub fn warm_picker(&mut self, w: f32, h: f32) { if let Some(c) = self.front.as_mut().and_then(|f| f.picker.as_mut()) { c.warm(&mut self.ctx, w, h); } }
    /// Pixel layout of the picker's current page (for hit-testing; same text metrics as the drawing).
    pub fn picker_layout(&self, page: picker::Page, w: f32, h: f32) -> picker::Layout {
        match self.front.as_ref().and_then(|f| f.picker.as_ref()).filter(|c| c.laid_out()) {
            Some(c) => c.layout(w, h),
            None => picker::layout(page, w, h, &|t, px| self.ctx.fonts.measure(FontId::Title, t, px)),
        }
    }
    /// LOAD COUNTER (custom, not a 1.8 widget): "done/total" load units, bottom left.
    pub fn load_counter(&mut self, w: f32, h: f32, done: usize, total: usize) {
        if total == 0 { return; }
        let s = fn_ui::layout::dpi_scale(w, h);
        let px = fn_ui::screens::atlas_px(COUNTER_SIZE, s);
        let t = format!("{done}/{total}");
        put_text(&mut self.ctx, &mut self.list, FontId::Bold, px, (COUNTER_X * s).round(), (h - COUNTER_BOTTOM * s).round(), &t, [1., 1., 1., 0.85], Some(([1.5 * s, 1.5 * s], [0., 0., 0., 0.6])), Snap::Pen);
    }
    /// Uploads one texture to the GPU now (a 1x1 offscreen draw), so showing it later costs no hitch. Returns false when the texture is missing.
    pub fn prewarm_texture(&mut self, name: &str) -> bool {
        let t = self.ctx.assets.texture(name);
        if t.id == fn_ui::draw::TextureId::NONE { return false; }
        let target = self.prewarm_target.get_or_insert_with(|| fn_render::OffscreenTarget::new(&self.device, 4, 4, self.format));
        let mut l = RenderList::new(); l.sprite(t.id, Rect::new(0., 0., 4., 4.), WHITE);
        self.renderer.render(&l, &mut self.ctx.assets, &target.view, 4, 4, Some([0., 0., 0., 1.]));
        true
    }

    /// A plain error page (the old runtime showed a MessageBox): centred title + wrapped message.
    pub fn error_screen(&mut self, w: f32, h: f32, message: &str) {
        self.list.fill(Rect::new(0., 0., w, h), [0.02, 0.03, 0.05, 1.]);
        let px = ((h / 36.).round() as u32).clamp(14, 28);
        let lines = wrap(message, ((w - 80.) / (px as f32 * 0.55)).max(20.) as usize);
        put_text(&mut self.ctx, &mut self.list, FontId::Bold, px + 6, 40., 60., "Fortnite could not start", [1., 0.45, 0.4, 1.], None, Snap::Pen);
        for (i, l) in lines.iter().enumerate() { put_text(&mut self.ctx, &mut self.list, FontId::Regular, px, 40., 60. + (i as f32 + 2.) * px as f32 * 1.4, l, WHITE, None, Snap::Pen); }
        put_text(&mut self.ctx, &mut self.list, FontId::Regular, px, 40., h - 40., "Press Esc to close.", [0.7, 0.8, 0.8, 1.], None, Snap::Pen);
    }
    /// The one HUD call of a frame: hand over the producer's state (`fn_hud_adapter::hud_state`), optionally step the animations, draw (HUD, or the loading screen while `state.loading` is set).
    pub fn present_hud(&mut self, st: &HudState, w: f32, h: f32, tick: Option<f32>) {
        self.hud.set_state(st);
        if let Some(dt) = tick { self.hud.tick(dt); }
        self.hud.present(&mut self.ctx, w, h, &mut self.list);
    }
    /// CONSOLE HOOK: the F2 command bar over the HUD.
    pub fn present_console(&mut self, v: &fn_console::View, w: f32, h: f32, time: f32) { let l = crate::console_ui::draw(&mut self.ctx, v, w, h, time); self.list.extend(l); }
    /// WIDGETS3 HOOK: draw the open inventory screen (`AthenaInventoryPanel`) over the HUD.
    pub fn present_inventory(&mut self, w: f32, h: f32) { self.inv.panel.build(&mut self.ctx, w, h, &mut self.list); }
    /// Engineering help card (F8; was F1, which 1.8 binds to HotbarKeyF1 = wall, CookedIniVersion.txt L2677), hidden by default like the old runtime's.
    pub fn help(&mut self, w: f32, h: f32) {
        let _ = h;
        self.list.fill(Rect::new(18., 18., 592f32.min(w - 36.), 82.), [0.025, 0.065, 0.07, 0.83]);
        let c = |l: &mut Self, y: f32, col: [f32; 4], t: &str| put_text(&mut l.ctx, &mut l.list, FontId::Regular, 15, 34., y, t, col, None, Snap::Pen);
        c(self, 43., [0.92, 0.97, 0.95, 1.], &format!("FORTNITE {} ASSETS / STANDALONE RUST RUNTIME", fn_settings::version::FORTNITE_VERSION));
        c(self, 65., [0.72, 0.84, 0.79, 1.], "WASD | Click: mouse-look | Shift: run | Space: jump | Ctrl: crouch | F8: hide this");
        c(self, 86., [0.72, 0.84, 0.79, 1.], "Not yet verified 1:1 | Esc: pause menu (Resume / Settings / Exit)");
    }
    pub fn black(&mut self, w: f32, h: f32, alpha: f32) { if alpha > 0. { self.list.fill(Rect::new(0., 0., w, h), [0., 0., 0., alpha.min(1.)]); } }

    /// Feed the gamepad to the menu as navigation presses (edge + repeat), while it is open.
    pub fn pad_to_menu(&mut self, state: Option<&fn_game::inputdev::pad::State>, dt: f32) {
        if !self.menu.is_open() { return; }
        let (b, lx, ly) = state.map(|s| (s.buttons, s.lx, s.ly)).unwrap_or((0, 0, 0));
        for p in self.pad_nav.step(b, lx, ly, dt) { self.menu.event(Event::Pad(p)); }
    }
    pub fn pad_button_for_tests(&mut self, b: PadButton) { self.menu.event(Event::Pad(b)); }
    /// Gamepad navigation presses (edge + repeat) of this frame for screens that are not the menu (the picker).
    pub fn pad_events(&mut self, state: Option<&fn_game::inputdev::pad::State>, dt: f32) -> Vec<Event> {
        let (b, lx, ly) = state.map(|s| (s.buttons, s.lx, s.ly)).unwrap_or((0, 0, 0));
        self.pad_nav.step(b, lx, ly, dt).into_iter().map(Event::Pad).collect()
    }

    /// One menu pass (also while closed: fade-out, apply requests) lowered into the frame's list. The host context carries the settings, the platform and the apply layer.
    pub fn menu_frame(&mut self, cx: &mut MenuContext, w: f32, h: f32, dt: f32) -> Vec<HostRequest> {
        let draw = self.menu.frame_with(cx, w, h, dt);
        lower::lower(draw, &mut self.ctx, w, h, &mut self.list);
        self.menu.take_requests()
    }

    /// Draws the list into `target` (over the 3D scene already in it when `clear` is None).
    pub fn render(&mut self, target: &wgpu::TextureView, w: u32, h: u32, clear: Option<[f32; 4]>) { self.renderer.render(&self.list, &mut self.ctx.assets, target, w, h, clear); }
    /// INVENTORY FIX3: [`UiLayer::render`] into the texture itself, so `SBackgroundBlur` passes (`Cmd::Blur`) can read it (needs `COPY_SRC`, see `Gfx::new`).
    pub fn render_texture(&mut self, target: &wgpu::Texture, w: u32, h: u32, clear: Option<[f32; 4]>) { self.renderer.render_texture(&self.list, &mut self.ctx.assets, target, w, h, clear); }
}

/// Greedy word wrap to `cols` characters.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut out = vec![];
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.len() + 1 + word.len() > cols { out.push(std::mem::take(&mut line)); }
            if !line.is_empty() { line.push(' '); }
            line.push_str(word);
        }
        out.push(line);
    }
    out
}

/// winit -> menu events (the abstract `fn_ui::input` model).
pub mod events {
    use fn_ui::input::{self, Event, MouseButton as M};
    use winit::event::MouseButton;
    pub fn mouse_button(b: MouseButton) -> Option<M> {
        match b { MouseButton::Left => Some(M::Left), MouseButton::Right => Some(M::Right), MouseButton::Middle => Some(M::Middle), MouseButton::Back => Some(M::X1), MouseButton::Forward => Some(M::X2), MouseButton::Other(_) => None }
    }
    pub fn key_down(vk: u16, repeat: bool, shift: bool, ctrl: bool) -> Vec<Event> { input::translate_key_down(vk, repeat, shift, ctrl) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn wrap_breaks_on_words_and_keeps_paragraphs() {
        assert_eq!(wrap("aaa bbb ccc", 7), vec!["aaa bbb", "ccc"]); assert_eq!(wrap("a\nb", 10), vec!["a", "b"]); assert_eq!(wrap("toolongword x", 4), vec!["toolongword", "x"]);
    }
    #[test] fn pad_status_converts_state() { let s = pad_status(true, true, Some(fn_game::inputdev::pad::State { buttons: 3, lt: 1, rt: 2, lx: 4, ly: 5, rx: 6, ry: 7 })); assert!(s.available && s.connected); assert_eq!(s.last.unwrap().ry, 7); assert!(pad_status(false, false, None).last.is_none()); }
}


