//! The UI of the app: `fn-ui` screens (splash, loading screen, HUD, Esc menu + Settings) built into one [`RenderList`] per frame and drawn by
//! `fn-ui-wgpu` on top of the 3D scene (or over black before the world exists).
//!
//! Draw order of the old runtime: HUD (unless the menu covers it), F1 help card, FPS counter, menu, black fade.
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
use std::path::PathBuf;

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
}

pub const WHITE: [f32; 4] = [1., 1., 1., 1.];
pub fn pad_status(available: bool, connected: bool, last: Option<fn_game::inputdev::pad::State>) -> fn_ui::inputdev::pad::PadStatus {
    fn_ui::inputdev::pad::PadStatus { available, connected, last: last.map(|s| fn_ui::inputdev::pad::State { buttons: s.buttons, lt: s.lt, rt: s.rt, lx: s.lx, ly: s.ly, rx: s.rx, ry: s.ry }) }
}

impl UiLayer {
    /// overlay = images the start-up worker decodes from the paks (front-end art); it is the first layer of the UI's asset source, then the Fortnite
    /// install itself when running from a `--paks` folder (fonts are only in the paks: `NotoSans-*.ufont`), then the asset roots.
    pub fn new(roots: &AssetRoots, device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat, overlay: crate::app::startup::Overlay, install: Option<PathBuf>) -> Result<UiLayer, String> {
        let mut layers: Vec<Box<dyn fn_ui::assets::AssetSource>> = vec![Box::new(overlay)];
        if let Some(dir) = install {
            // the install may be missing/broken: the UI then falls back to the asset folders (and its message names the font it needs)
            match fn_ui::pak_assets::PakAssets::open(&dir) {
                Ok(pak) => layers.push(Box::new(pak)),
                Err(e) => eprintln!("[ui] the Fortnite install is not usable for UI assets ({e}); using the asset folders only"),
            }
        }
        layers.push(roots.ui_source());
        let ctx = UiContext::new(Box::new(fn_ui::assets::LayeredAssets::new(layers)))?;
        let renderer = UiRenderer::with_device(device.clone(), queue, format);
        let mut menu = MenuSystem::new(ctx.fonts.clone());
        pages::register_all_into(&mut menu);
        Ok(UiLayer { ctx, renderer, menu, hud: Hud::new(), list: RenderList::new(), pad_nav: PadState::default(), startup: StartupLoadingScreen, device, format, prewarm_target: None, inv: Default::default() })
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
    pub fn startup_loading(&mut self, w: f32, h: f32, t: f32, progress: f32, art_ready: bool, fade: f32) { self.startup.build(&mut self.ctx, w, h, t, progress, art_ready, fade, &mut self.list); }
    /// The game picker.
    pub fn picker(&mut self, p: &Picker, w: f32, h: f32, art_ready: bool) { p.build(&mut self.ctx, w, h, art_ready, &mut self.list); }
    /// Pixel layout of the picker's current page (for hit-testing; same text metrics as the drawing).
    pub fn picker_layout(&self, page: picker::Page, w: f32, h: f32) -> picker::Layout { picker::layout(page, w, h, &|t, px| self.ctx.fonts.measure(FontId::Title, t, px)) }
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
    /// WIDGETS3 HOOK: draw the open inventory screen (`AthenaInventoryPanel`) over the HUD.
    pub fn present_inventory(&mut self, w: f32, h: f32) { self.inv.panel.build(&mut self.ctx, w, h, &mut self.list); }
    /// Engineering help card (F1), hidden by default like the old runtime's.
    pub fn help(&mut self, w: f32, h: f32) {
        let _ = h;
        self.list.fill(Rect::new(18., 18., 592f32.min(w - 36.), 82.), [0.025, 0.065, 0.07, 0.83]);
        let c = |l: &mut Self, y: f32, col: [f32; 4], t: &str| put_text(&mut l.ctx, &mut l.list, FontId::Regular, 15, 34., y, t, col, None, Snap::Pen);
        c(self, 43., [0.92, 0.97, 0.95, 1.], &format!("FORTNITE {} ASSETS / STANDALONE RUST RUNTIME", fn_settings::version::FORTNITE_VERSION));
        c(self, 65., [0.72, 0.84, 0.79, 1.], "WASD | Click: mouse-look | Shift: run | Space: jump | Ctrl: crouch | F1: hide this");
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


