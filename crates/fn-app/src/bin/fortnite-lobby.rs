//! The live Fortnite 1.8 Battle Royale lobby (offline, pure Rust): the recovered 3D lobby scene (`lobby_scene`) with the UMG screens on top (`lobby_ui`),
//! clickable tabs, the Locker with outfit / harvesting tool / glider swapping, and the hero on the pad rebuilt from the equipped outfit.
//!
//! Never starts the original client, no account / commerce / network code. Everything shown is read from the 1.8 paks (see `parity/lobby/*.md`); what the
//! backend would own (V-Bucks, owned and equipped items) is the offline profile (`lobby_profile`), written next to the executable.
//!
//! * pointer: hover / press / click through the UMG hit test (`fn_ui::screens::athena_locker::controller`), button sounds are the widgets' serialized slate sounds;
//! * Escape: ends picking in the Locker (`AthenaCustomizationTab::HandleCancel`), otherwise closes the lobby;
//! * startup: the 1.8 loading screen while `lobby_preload` loads everything (scene, every outfit, UI, Locker icons, shop art, audio), then the hand-off;
//! * Item Shop tab: `lobby_shop::ShopScreen` (real widgets, offline catalog + rotation); F2: the command bar (`lobby_console`);
//! * Play: there is no matchmaking offline; it starts `fortnite-rs` (the Rust gameplay executable next to this one) with the equipped outfit.

#[path = "../gfx.rs"]
mod gfx;
#[path = "../lobby_console.rs"]
mod lobby_console;
#[path = "../lobby_locker.rs"]
mod lobby_locker;
#[path = "../lobby_preload.rs"]
mod lobby_preload;
#[path = "../lobby_shop.rs"]
mod lobby_shop;
#[path = "../lobby_profile.rs"]
mod lobby_profile;
#[path = "../lobby_scene.rs"]
mod lobby_scene;
#[path = "../lobby_ui.rs"]
mod lobby_ui;
#[path = "../lobby_world.rs"]
mod lobby_world;
#[path = "../platform.rs"]
mod platform;
#[path = "../sys.rs"]
mod sys;

use fn_audio_sink::{AudioSink, SoundEvent};
use fn_ui::draw::{RenderList, TextureId};
use fn_ui::layout::Rect;
use fn_ui::screens::athena_shop::{Dialog, ShopState};
use fn_ui_wgpu::UiRenderer;
use gfx::{Acquire, Gfx};
use lobby_console::{Console, Env, Key};
use lobby_preload::{counters, Counters, HeroCache, Preloader, TaskCtx};
use lobby_shop::{ShopEvent, ShopScreen};
use lobby_ui::{Controller, Frame, LobbyState, LobbyUi, UiEvent};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

const TITLE: &str = "Fortnite 1.8 - offline Athena lobby (Rust)";

fn install_path() -> PathBuf { fn_live::var_os("FN_ROOT").map(PathBuf::from).unwrap_or_else(fn_ui::pak_assets::default_install) }

/// `HoveredSlateSoundOverride` / `PressedSlateSoundOverride` object path of a widget -> the sink's event for that cue (the sink knows the cues of the lobby screens).
pub fn sound_event(cue: &str, hover: bool) -> Option<SoundEvent> {
    use fn_audio_sink as s;
    if hover {
        match cue { s::TOP_BAR_HOVER_CUE => Some(SoundEvent::TopBarHover), s::LOCKER_ITEM_HOVER_CUE => Some(SoundEvent::LockerItemHover), _ => None }
    } else {
        match cue {
            s::TOP_BAR_PRESS_CUE => Some(SoundEvent::TopBarPress), s::LOBBY_PLAY_PRESS_CUE => Some(SoundEvent::LobbyPlayPress), s::LOCKER_ITEM_PRESS_CUE => Some(SoundEvent::LockerItemPress),
            _ => Some(SoundEvent::MenuClick),
        }
    }
}

/// What the lobby does with the equipped outfit when Play is pressed: start the Rust game next to this executable. The game reads the equipped outfit from the same
/// offline profile (`br_character::selected_hero`).
fn launch_game() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = me.with_file_name(if cfg!(windows) { "fortnite-rs.exe" } else { "fortnite-rs" });
    if !exe.exists() { return Err(format!("{} not found next to the lobby", exe.display())); }
    std::process::Command::new(&exe).spawn().map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(exe)
}

/// The castle backdrop the storefront was captured over (`T_LobbyCastle_Matte`, cover-fitted).
// SCREEN BACKGROUND HOOK (see `App::set_tab`): the Locker / Item Shop backgrounds are being reworked in `lobby_scene.rs`; this sprite is the interim one.
fn backdrop(uic: &mut fn_ui::scene::UiContext, list: &mut RenderList, w: u32, h: u32) {
    let t = uic.assets.texture("T_LobbyCastle_Matte.png");
    if t.id != TextureId::NONE {
        let (fw, fh) = (w as f32, h as f32);
        let s = (fw / t.w as f32).max(fh / t.h as f32);
        let (dw, dh) = (t.w as f32 * s, t.h as f32 * s);
        list.sprite(t.id, Rect::new((fw - dw) * 0.5, (fh - dh) * 0.5, dw, dh), [1.0; 4]);
    }
}

/// PRELOAD TASK of the Item Shop and the F2 console (main thread, sliced): loads the shop (catalog + widgets), then draws through the real UI renderer into the
/// off-screen target: every item's tile art (all sellable items, 6 per draw), every item's details widget (hovered purchase button) and a few purchased widgets, the
/// hover state, the castle backdrop, and the console (help output, suggestion list) so every texture, font glyph and pipeline is resident before the hand-off.
fn shop_task(install: PathBuf, cell: Rc<RefCell<Option<ShopScreen>>>) -> Box<dyn FnMut(&mut TaskCtx) -> bool> {
    enum Job { Tiles(usize), Details(usize), Purchased(usize), Console }
    let mut jobs: Vec<Job> = vec![];
    let mut offers: Vec<fn_ui::screens::athena_shop::OfferView> = vec![];
    let mut next = 0usize;
    let mut loaded = false;
    Box::new(move |cx: &mut TaskCtx| {
        let (w, h) = cx.size;
        if !loaded {
            loaded = true;
            let store = match fn_ui::screens::athena_lobby::store::Store::open(&install) { Ok(s) => Rc::new(s), Err(e) => { eprintln!("[lobby] shop: {e}"); return true } };
            let screen = match ShopScreen::load(store) { Ok(s) => s, Err(e) => { eprintln!("[lobby] shop: {e}"); return true } };
            offers = screen.shop.all_offer_views(cx.profile);
            let chunks = offers.len().div_ceil(lobby_shop::WEEKLY_SLOTS + lobby_shop::DAILY_SLOTS);
            jobs.extend((0..chunks).map(Job::Tiles));
            jobs.extend((0..offers.len()).map(Job::Details));
            jobs.extend((0..offers.len().min(4)).map(Job::Purchased));
            jobs.push(Job::Console);
            *cell.borrow_mut() = Some(screen);
            eprintln!("[lobby] shop loaded: {} offers, {} jobs", offers.len(), jobs.len());
            return false;
        }
        let started = Instant::now();
        let mut guard = cell.borrow_mut();
        let Some(screen) = guard.as_mut() else { return true };
        let now = lobby_shop::now_unix();
        while next < jobs.len() {
            let job = &jobs[next];
            next += 1;
            let mut st = ShopState { selected_tab: screen.ui.item_shop_tab, mtx_balance: cx.profile.balance(), view: screen.shop.view(cx.profile, now), ..Default::default() };
            let mut list = RenderList::new();
            match job {
                Job::Tiles(c) => {
                    let per = lobby_shop::WEEKLY_SLOTS + lobby_shop::DAILY_SLOTS;
                    let chunk: Vec<_> = offers.iter().skip(c * per).take(per).cloned().collect();
                    let (weekly, daily) = chunk.split_at(chunk.len().min(lobby_shop::WEEKLY_SLOTS));
                    st.view.weekly = weekly.to_vec(); st.view.daily = daily.to_vec();
                    if *c == 0 { st.hovered_tab = Some(0); st.hovered_offer = chunk.first().map(|o| o.offer_id.clone()); st.back_hovered = true; backdrop(cx.ui.ctx_mut(), &mut list, w, h); }
                }
                Job::Details(i) => st.dialog = Dialog::Details { offer: offers[*i].clone(), purchase_hovered: true },
                Job::Purchased(i) => { let mut o = offers[*i].clone(); o.owned = true; st.dialog = Dialog::Purchased { offer: o }; }
                Job::Console => {
                    let mut p = lobby_profile::Profile::new(cx.glue.default_loadout());
                    let mut console = Console::new();
                    console.open();
                    for line in ["help", "addvbucks 1000", "giveitem sk"] {
                        console.set_input(line);
                        if line != "giveitem sk" { console.key(Key::Enter, &mut Env { profile: &mut p, shop: &screen.shop, now }); }
                    }
                    let env = Env { profile: &mut p, shop: &screen.shop, now };
                    let l = console.draw(cx.ui.ctx_mut(), w as f32, h as f32, &env, 0.0);
                    cx.renderer.render(&l, &mut cx.ui.ctx_mut().assets, cx.target, w, h, Some([0., 0., 0., 0.]));
                    continue;
                }
            }
            let frame = screen.ui.draw(cx.ui.ctx_mut(), w as f32, h as f32, &st);
            list.extend(frame.list);
            cx.renderer.render(&list, &mut cx.ui.ctx_mut().assets, cx.target, w, h, Some([0., 0., 0., 0.]));
            if next % 20 == 0 { eprintln!("[lobby] shop streaming: {next}/{} jobs", jobs.len()); }
            if started.elapsed().as_millis() > 15 { return false; }
        }
        true
    })
}

enum Stage {
    /// the 1.8 loading screen: everything is loaded behind it (`lobby_preload`)
    Preload(Box<Preloader>),
    Ready,
    Failed(String),
}

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    ui: Option<LobbyUi>,
    renderer: Option<UiRenderer>,
    scene: Option<lobby_scene::LobbyScene>,
    audio: Option<Box<dyn AudioSink>>,
    stage: Option<Stage>,
    controller: Controller,
    state: LobbyState,
    frame: Option<Frame>,
    profile: Option<lobby_profile::Profile>,
    glue: Option<lobby_locker::LockerGlue>,
    set: Option<Arc<fn_pak::PakSet>>,
    shop: Option<ShopScreen>,
    shop_cell: Rc<RefCell<Option<ShopScreen>>>,
    console: Console,
    heroes: HeroCache,
    overlay: Option<lobby_preload::AtlasOverlay>,
    counters0: Counters,
    counters_logged: Counters,
    lazy_hero_loads: u32,
    mods: ModifiersState,
    /// outfit swap in flight (character assets decode on a worker thread) and the hero queued behind it
    hero_job: Option<Receiver<Result<lobby_scene::HeroReady, String>>>,
    hero_queued: Option<String>,
    cursor: (f32, f32),
    started: Instant,
    last: Instant,
    scene_t0: Instant,
    frames: u64,
    /// `FN_LOBBY_SCRIPT=1`: a scripted session (shop tab, hover, details, purchase, console, outfit swaps) that prints the loads after the hand-off, then exits
    script: bool,
    shop_rects: (Vec<(String, Rect)>, Option<Rect>),
    /// post-hand-off streaming on the render thread, a few ms per frame (the UMG store is `Rc`): the shop (catalog + art), then the other tabs and Locker pages
    shop_task: Option<Box<dyn FnMut(&mut TaskCtx) -> bool>>,
    shop_done: bool,
    warm_jobs: Vec<(usize, Option<lobby_ui::LockerSlot>)>,
    idle_target: Option<((u32, u32), wgpu::Texture, wgpu::TextureView)>,
    idle_t0: Option<Instant>,
    script_base: Option<(u64, Counters)>,
}

use std::sync::mpsc::{channel, Receiver};

impl App {
    fn new() -> App {
        let now = Instant::now();
        App { window: None, gfx: None, ui: None, renderer: None, scene: None, audio: None, stage: None, controller: Controller::new(), state: LobbyState::default(), frame: None, profile: None, glue: None, set: None,
            shop: None, shop_cell: Rc::new(RefCell::new(None)), console: Console::new(), heroes: HeroCache::default(), overlay: None, counters0: Counters::default(), counters_logged: Counters::default(), lazy_hero_loads: 0,
            mods: ModifiersState::empty(), hero_job: None, hero_queued: None, cursor: (0., 0.), started: now, last: now, scene_t0: now, frames: 0,
            script: fn_live::var_os("FN_LOBBY_SCRIPT").is_some(), shop_rects: (vec![], None), shop_task: None, shop_done: false, warm_jobs: vec![], idle_target: None, idle_t0: None, script_base: None }
    }

    fn play(&mut self, ev: SoundEvent) { if let Some(a) = self.audio.as_mut() { a.event(ev); } }

    fn shop_tab(&self) -> Option<usize> { self.shop.as_ref().map(|s| s.ui.item_shop_tab) }
    fn on_shop(&self) -> bool { self.shop_tab() == Some(self.state.selected_tab) }

    /// The one place where the visible tab changes (clicks on the lobby tab bar, clicks on the shop's tab list).
    fn set_tab(&mut self, i: usize) {
        self.state.selected_tab = i;
        self.state.locker.picking = None;
        self.state.locker.picker_t = 0.0;
        if self.shop_tab() == Some(i) { if let Some(s) = self.shop.as_mut() { s.open(); } }
        // SCREEN BACKGROUND HOOK: the other agent's `LobbyScene::set_screen(<Lobby | Locker | ItemShop>)` call goes here, with the tab `i` mapped to its screen
        // (`i == shop tab` -> ItemShop, `lobby_ui::LobbyUi::tab_index_of(athena_locker::TAB_CLASS)` -> Locker, else Lobby).
        eprintln!("[lobby] tab {i} ({})", self.ui.as_ref().and_then(|u| u.tab_labels().get(i).cloned()).unwrap_or_default());
    }

    /// Rebuild the lobby pawn from the outfit the profile has equipped. Every outfit was pre-built by the preload: the swap is a cache hit; a miss (over the memory
    /// budget / failed at preload) loads off the render thread and is counted as a lazy load.
    fn request_hero(&mut self) {
        let (Some(glue), Some(profile)) = (self.glue.as_ref(), self.profile.as_ref()) else { return };
        let hero = glue.equipped_hero(profile);
        if self.scene.as_ref().and_then(|s| s.hero()) == Some(hero.as_str()) && self.hero_job.is_none() { return; }
        if self.hero_job.is_some() { self.hero_queued = Some(hero); return; }
        if let Some(scene) = self.scene.as_mut() {
            if self.heroes.swap_in(scene, &hero) { eprintln!("[lobby] outfit on the pad (preloaded): {hero}"); return; }
        }
        let Some(set) = self.set.clone() else { return };
        let Some(builder) = self.scene.as_ref().and_then(|s| s.hero_builder()) else { return };
        self.lazy_hero_loads += 1;
        eprintln!("[lobby] LAZY LOAD: outfit {hero} was not in the preload cache");
        let (tx, rx) = channel();
        std::thread::spawn(move || { let _ = tx.send(lobby_scene::load_hero_parts(&set, &hero).map(|p| builder.build(&p))); });
        self.hero_job = Some(rx);
    }

    fn poll_hero(&mut self) {
        let Some(rx) = self.hero_job.as_ref() else { return };
        let Ok(res) = rx.try_recv() else { return };
        self.hero_job = None;
        match res {
            Ok(ready) => { if let Some(s) = self.scene.as_mut() { let hero = ready.hero().to_owned(); if s.set_hero_ready(ready) { eprintln!("[lobby] outfit on the pad: {hero}"); } } }
            Err(e) => eprintln!("[lobby] cannot build the equipped outfit: {e}"),
        }
        if self.hero_queued.take().is_some() { self.request_hero(); }
    }

    /// A profile change made by the console / the shop: persist it and refresh the Locker lists and the pawn.
    fn profile_changed(&mut self, equip: bool) {
        if let Some(p) = self.profile.as_mut() { let _ = p.flush(); }
        if let (Some(glue), Some(profile)) = (self.glue.as_ref(), self.profile.as_ref()) { glue.sync(profile, &mut self.state.locker); }
        if equip { self.request_hero(); }
    }

    fn handle_events(&mut self) {
        for ev in self.controller.take_events() {
            match ev {
                UiEvent::HoverSound(c) => { if let Some(e) = sound_event(&c, true) { self.play(e); } }
                UiEvent::PressSound(c) => { if let Some(e) = sound_event(&c, false) { self.play(e); } }
                UiEvent::TabSelected(i) => self.set_tab(i),
                UiEvent::Play => match launch_game() { Ok(p) => eprintln!("[lobby] Play: started {}", p.display()), Err(e) => eprintln!("[lobby] Play: no matchmaking offline ({e})") },
                UiEvent::Equip { slot, index } => {
                    let (Some(glue), Some(profile)) = (self.glue.as_ref(), self.profile.as_mut()) else { continue };
                    if let Some(c) = glue.equip(profile, slot, &self.state.locker, index) {
                        eprintln!("[lobby] equipped {} ({})", c.display_name().trim(), c.id);
                        let _ = profile.flush();
                    }
                    glue.sync(profile, &mut self.state.locker);
                    if slot == lobby_ui::LockerSlot::Outfit { self.request_hero(); }
                }
                UiEvent::BannerApply { icon, color } => { if let Some(p) = self.profile.as_mut() { p.set_banner(&icon, &color); let _ = p.flush(); } }
                UiEvent::Preview { .. } | UiEvent::PickerOpened(_) | UiEvent::PickerClosed | UiEvent::PlaylistChanged(_) | UiEvent::FillChanged(_) | UiEvent::OptionChanged { .. } | UiEvent::Cancel => {}
            }
        }
    }

    /// Hands the preload's products to the lobby.
    fn handoff(&mut self, p: lobby_preload::Products) {
        for l in p.report.lines() { eprintln!("{l}"); }
        self.ui = Some(p.ui); self.renderer = Some(p.renderer); self.scene = Some(p.scene); self.audio = p.audio; self.profile = Some(p.profile); self.glue = Some(p.glue); self.set = Some(p.set);
        self.heroes = p.heroes; self.overlay = p.overlay;
        self.idle_t0 = Some(Instant::now());
        { let n = self.ui.as_ref().map_or(0, |u| u.tab_labels().len());
          let locker = self.ui.as_ref().and_then(|u| u.tab_index_of(fn_ui::screens::athena_locker::TAB_CLASS));
          // after the shop: the other tabs, then each Locker picker for a few frames (its cards bake progressively)
          let mut jobs = vec![];
          if let Some(l) = locker { for s in lobby_ui::LockerSlot::ALL { for _ in 0..6 { jobs.push((l, Some(s))); } } }
          for t in 1..n { if Some(t) != locker { jobs.push((t, None)); } }
          jobs.reverse();
          self.warm_jobs = jobs; }
        let (glue, profile) = (self.glue.as_ref().unwrap(), self.profile.as_ref().unwrap());
        self.state.selected_tab = 0;
        self.state.mtx_balance = profile.balance();
        glue.sync(profile, &mut self.state.locker);
        eprintln!("[lobby] hand-off after {:.2} s: profile owns {} items ({} outfits listed in the Locker), {} skins cached (the rest build in the background)", self.started.elapsed().as_secs_f32(), profile.owned_count(), self.state.locker.items[0].len(), self.heroes.len());
        self.counters0 = p.counters; self.counters_logged = p.counters;
        self.scene_t0 = Instant::now();
        self.stage = Some(Stage::Ready);
    }

    /// Streams the rest of the lobby in after the hand-off, one slice per frame (never more than ~15 ms): the shop first (so the tab and F2 work), then the other tabs
    /// and the Locker pages. Everything goes through the same renderer and UI context the lobby draws with, into an off-screen target.
    fn idle_step(&mut self) {
        if self.idle_t0.map_or(true, |t| t.elapsed().as_secs_f32() < 1.0) { return; }
        if self.shop_done && self.warm_jobs.is_empty() { return; }
        let (Some(gfx), Some(ui), Some(renderer), Some(glue), Some(profile), Some(set)) = (self.gfx.as_ref(), self.ui.as_mut(), self.renderer.as_mut(), self.glue.as_ref(), self.profile.as_ref(), self.set.as_ref()) else { return };
        let size = gfx.size();
        if self.idle_target.as_ref().map_or(true, |(s, _, _)| *s != size) {
            let tex = gfx.gpu.device.create_texture(&wgpu::TextureDescriptor { label: Some("lobby idle target"), size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: gfx.format(), usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            self.idle_target = Some((size, tex, view));
        }
        let view = &self.idle_target.as_ref().unwrap().2;
        if !self.shop_done {
            let Some(task) = self.shop_task.as_mut() else { self.shop_done = true; return };
            let mut cx = TaskCtx { ui, renderer, glue, profile, set, size, target: view };
            if task(&mut cx) {
                self.shop_done = true;
                self.shop = self.shop_cell.borrow_mut().take();
                eprintln!("[lobby] streamed in: Item Shop + console ({:.1} s after the hand-off){}", self.idle_t0.map_or(0., |t| t.elapsed().as_secs_f32()), if self.shop.is_none() { ", FAILED" } else { "" });
            }
            return;
        }
        if let Some((tab, picking)) = self.warm_jobs.pop() {
            let mut st = LobbyState { selected_tab: tab, mtx_balance: profile.balance(), ..Default::default() };
            st.locker = self.state.locker.clone();
            if let Some(s) = picking { st.locker.picking = Some(s); st.locker.picker_t = fn_ui::screens::athena_locker::INTRO_PICKER_END; }
            let frame = ui.draw(size.0 as f32, size.1 as f32, &st);
            renderer.render(&frame.list, &mut ui.ctx_mut().assets, view, size.0, size.1, Some([0., 0., 0., 0.]));
            if self.warm_jobs.is_empty() { eprintln!("[lobby] streamed in: every tab and Locker page ({:.1} s after the hand-off)", self.idle_t0.map_or(0., |t| t.elapsed().as_secs_f32())); }
        }
    }
    fn draw(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;
        // loading screen
        if let Some(Stage::Preload(_)) = self.stage.as_ref() {
            let Some(gfx) = self.gfx.as_mut() else { return };
            let Acquire::Frame(frame_tex) = gfx.acquire() else {
                // minimized / occluded / mid-resize: no frame to draw, but the loading goes on (the hand-off needs a real frame)
                if let Some(Stage::Preload(pre)) = self.stage.as_mut() { if let Err(e) = pre.poll(Instant::now() + lobby_preload::FRAME_BUDGET) { eprintln!("[lobby] preload: {e}"); self.stage = Some(Stage::Failed(e)); } }
                return;
            };
            let (w, h) = gfx.size();
            let view = frame_tex.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let Some(Stage::Preload(pre)) = self.stage.as_mut() else { return };
            let res = pre.frame(&view, w, h);
            self.gfx.as_ref().unwrap().present(frame_tex);
            match res {
                Ok(Some(products)) => self.handoff(products),
                Ok(None) => {}
                Err(e) => { eprintln!("[lobby] preload: {e}"); self.stage = Some(Stage::Failed(e)); }
            }
            return;
        }
        if matches!(self.stage, Some(Stage::Failed(_)) | None) { return; }
        self.poll_hero();
        self.heroes.pump();
        self.idle_step();
        self.frames += 1;
        let on_shop = self.on_shop();
        let wall = lobby_shop::now_unix();
        let (Some(gfx), Some(ui), Some(renderer)) = (self.gfx.as_mut(), self.ui.as_mut(), self.renderer.as_mut()) else { return };
        let Acquire::Frame(frame_tex) = gfx.acquire() else { return };
        let (w, h) = gfx.size();
        let view = frame_tex.texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.controller.tick(&mut self.state, dt);
        if let Some(p) = self.profile.as_ref() { self.state.mtx_balance = p.balance(); }
        let t = self.scene_t0.elapsed().as_secs_f32();
        if on_shop {
            // the Item Shop screen (its own tab list and top bar, the V-Bucks number is the profile balance)
            let (shop, profile) = (self.shop.as_mut().unwrap(), self.profile.as_ref().unwrap());
            let mut list = RenderList::new();
            backdrop(ui.ctx_mut(), &mut list, w, h);
            let sf = shop.draw(ui.ctx_mut(), w as f32, h as f32, profile, wall);
            self.shop_rects = (sf.offers.clone(), sf.purchase_button);
            list.extend(sf.list);
            // INVENTORY FIX4 (verify3 V2): into the frame texture itself (the game's path), so `BackgroundBlur` passes run (the surface has COPY_SRC, `gfx.rs`)
            renderer.render_texture(&list, &mut ui.ctx_mut().assets, &frame_tex.texture, w, h, Some([0.0, 0.0, 0.0, 1.0]));
            self.frame = None;
        } else {
            match self.scene.as_mut() {
                Some(scene) => {
                    scene.render(&view, w, h, t);
                    // the party pad widgets (name plate over the local player, "+" over the empty slots) belong to the lobby camera: only the Lobby tab shows them
                    let mut pads = vec![];
                    if self.state.selected_tab == 0 {
                        for s in scene.party_slots().iter().filter(|s| !s.hidden) {
                            if s.occupied { if let Some([x, y]) = scene.project(s.top_widget_cm, w, h) { pads.push(lobby_ui::Pad { x, y, pivot: s.top_pivot, kind: lobby_ui::PadKind::Player { name: None, leader: true, ready: false } }); } }
                            else if let Some([x, y]) = scene.project(s.add_widget_cm, w, h) { pads.push(lobby_ui::Pad { x, y, pivot: s.add_pivot, kind: lobby_ui::PadKind::Empty }); }
                        }
                    }
                    self.state.pads = pads;
                }
                None => self.state.pads.clear(),
            }
            let frame = ui.draw(w as f32, h as f32, &self.state);
            renderer.render_texture(&frame.list, &mut ui.ctx_mut().assets, &frame_tex.texture, w, h, None); // INVENTORY FIX4 (verify3 V2): with the blur pass
            self.frame = Some(frame);
        }
        // F2 command bar over everything
        if self.console.is_open() {
            if let (Some(shop), Some(profile)) = (self.shop.as_ref(), self.profile.as_mut()) {
                let env = Env { profile, shop: &shop.shop, now: wall };
                let list = self.console.draw(ui.ctx_mut(), w as f32, h as f32, &env, self.started.elapsed().as_secs_f32());
                renderer.render_texture(&list, &mut ui.ctx_mut().assets, &frame_tex.texture, w, h, None); // INVENTORY FIX4 (verify3 V2)
            }
        }
        // the key art fades out over the first lobby frames
        if let Some(o) = self.overlay.as_mut() { if o.draw(&view, w, h) { self.overlay = None; } }
        gfx.present(frame_tex);
        // loads after the hand-off: anything that grew since the last look
        if self.frames % 30 == 0 || self.script {
            let c = counters(ui, renderer);
            let grew = c.grew_since(&self.counters_logged);
            if !grew.is_empty() { eprintln!("[lobby] LOADS AFTER HAND-OFF (frame {}): {}", self.frames, grew.join(", ")); self.counters_logged = c; }
        }
        if self.script { self.run_script(); }
    }

    fn pointer_move(&mut self) {
        if self.console.is_open() { return; }
        if self.on_shop() { if let Some(s) = self.shop.as_mut() { s.pointer_move(self.cursor.0, self.cursor.1); } return; }
        let (Some(ui), Some(frame)) = (self.ui.as_ref(), self.frame.as_ref()) else { return };
        self.controller.pointer_move(ui.inner(), frame, &mut self.state, self.cursor.0, self.cursor.1);
    }

    /// A left click on the shop screen: tab list, tiles, purchase, back.
    fn shop_click(&mut self) {
        let now = lobby_shop::now_unix();
        let (Some(shop), Some(profile)) = (self.shop.as_mut(), self.profile.as_mut()) else { return };
        let ev = shop.click(self.cursor.0, self.cursor.1, profile, now);
        match ev {
            ShopEvent::None => {}
            ShopEvent::TabSelected(i) => { self.play(SoundEvent::TopBarPress); self.set_tab(i); }
            ShopEvent::Bought { offer_id, item_id, price, balance } => {
                eprintln!("[lobby] shop: bought {offer_id} ({item_id}) for {price} V-Bucks, balance {balance}");
                self.profile_changed(true);
            }
            ShopEvent::Message(m) => eprintln!("[lobby] shop: {m}"),
        }
    }

    fn console_key(&mut self, k: Key) {
        let now = lobby_shop::now_unix();
        let (Some(shop), Some(profile)) = (self.shop.as_ref(), self.profile.as_mut()) else { return };
        let fx = self.console.key(k, &mut Env { profile, shop: &shop.shop, now });
        for (_, l) in &fx.lines { eprintln!("[console] {l}"); }
        if fx.profile_changed || fx.equip_changed { self.profile_changed(fx.equip_changed); }
    }

    fn keyboard(&mut self, event_loop: &ActiveEventLoop, event: &winit::event::KeyEvent) {
        let keys = lobby_console::keys_from_winit(event, self.mods.shift_key());
        if self.console.is_open() { for k in keys { self.console_key(k); } return; }
        if event.repeat { return; }
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Escape) => {
                // the shop's pushed widgets pop first (`HandleBack`), then `AthenaCustomizationTab::HandleCancel` ends picking; otherwise the lobby closes
                let popped = self.on_shop() && self.shop.as_mut().map_or(false, |s| s.back());
                if !popped { let _ = self.controller.cancel(&mut self.state); }
            }
            PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) if self.on_shop() => {
                let now = lobby_shop::now_unix();
                if let (Some(shop), Some(profile)) = (self.shop.as_mut(), self.profile.as_mut()) {
                    if let ShopEvent::Bought { offer_id, price, balance, .. } = shop.confirm(profile, now) { eprintln!("[lobby] shop: bought {offer_id} for {price}, balance {balance}"); self.profile_changed(true); }
                }
            }
            _ => {}
        }
    }

    /// `FN_LOBBY_SCRIPT=1` session: shop tab, hover, details, purchase, console commands, outfit swaps; prints what loaded after the hand-off, then exits.
    fn run_script(&mut self) {
        if !(self.shop_done && self.warm_jobs.is_empty()) { return; }
        if self.script_base.is_none() {
            let c = match (self.ui.as_mut(), self.renderer.as_ref()) { (Some(u), Some(r)) => lobby_preload::counters(u, r), _ => return };
            self.script_base = Some((self.frames, c));
        }
        let (base, base_counters) = self.script_base.unwrap();
        let f = self.frames - base;
        let shop_tab = self.shop_tab().unwrap_or(2);
        match f {
            20 => self.set_tab(shop_tab),
            40 => { if let Some((_, r)) = self.shop_rects.0.get(2).cloned() { self.cursor = (r.x + r.w * 0.5, r.y + r.h * 0.5); self.pointer_move(); } }
            60 => { eprintln!("[script] click the third tile"); self.shop_click(); }
            80 => { if let Some(r) = self.shop_rects.1 { self.cursor = (r.x + r.w * 0.5, r.y + r.h * 0.5); self.pointer_move(); } }
            100 => { eprintln!("[script] click purchase (balance {})", self.profile.as_ref().map_or(0, |p| p.balance())); self.shop_click(); }
            140 => { if let Some(s) = self.shop.as_mut() { s.back(); } eprintln!("[script] balance now {}", self.profile.as_ref().map_or(0, |p| p.balance())); }
            150 => { self.console.open(); for line in ["help", "rotateshop 1", "setvbucks 5000", "addvbucks 13500", "giveitem reaper", "setskin skull trooper"] { for c in line.chars() { self.console_key(Key::Char(c)); } self.console_key(Key::Enter); } }
            200 => { for c in "setskin gh".chars() { self.console_key(Key::Char(c)); } self.console_key(Key::Tab); self.console_key(Key::Enter); }
            220 => { for c in "resetprofile".chars() { self.console_key(Key::Char(c)); } self.console_key(Key::Enter); self.console.close(); }
            230 => self.set_tab(1),
            250 => { if let Some(w) = &self.window { w.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None))); eprintln!("[script] fullscreen on"); } }
            275 => { if let Some(w) = &self.window { let _ = w.request_inner_size(PhysicalSize::new(1280, 720)); w.set_fullscreen(None); eprintln!("[script] fullscreen off"); } }
            240 => self.set_tab(0),
            330 => {
                if let (Some(ui), Some(r)) = (self.ui.as_mut(), self.renderer.as_ref()) {
                    let c = lobby_preload::counters(ui, r);
                    eprintln!("[script] AFTER THE STREAMING FINISHED: UI textures +{}, unresolved +{}, renderer textures +{}, lazy skin loads {} (pak reads +{} include the background skin builder)",
                        c.ui_textures - base_counters.ui_textures, c.ui_missing - base_counters.ui_missing, c.renderer_textures - base_counters.renderer_textures, self.lazy_hero_loads, c.pak_reads - base_counters.pak_reads);
                }
                std::process::exit(0);
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() { return; }
        let attrs = Window::default_attributes().with_title(TITLE).with_inner_size(PhysicalSize::new(1280, 720)).with_min_inner_size(PhysicalSize::new(640, 360));
        let Ok(window) = event_loop.create_window(attrs) else { event_loop.exit(); return };
        let window = Arc::new(window);
        let Ok(gfx) = Gfx::new(window.clone(), true) else { eprintln!("[lobby] no GPU surface"); event_loop.exit(); return };
        let install = install_path();
        // everything loads behind the 1.8 loading screen (`lobby_preload`); the shop + console are one more preload unit
        let pre = match Preloader::start(&install, gfx.gpu.device.clone(), gfx.gpu.queue.clone(), gfx.format(), gfx.size()) { Ok(p) => p, Err(e) => { eprintln!("[lobby] preload: {e}"); event_loop.exit(); return } };
        self.shop_task = Some(shop_task(install, self.shop_cell.clone()));
        self.window = Some(window); self.gfx = Some(gfx);
        self.stage = Some(Stage::Preload(Box::new(pre)));
        self.started = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => { if let Some(p) = self.profile.as_mut() { let _ = p.flush(); } event_loop.exit() }
            WindowEvent::RedrawRequested => { self.draw(); self.handle_events(); if matches!(self.stage, Some(Stage::Failed(_))) { event_loop.exit(); } }
            WindowEvent::Resized(size) => { if let Some(g) = self.gfx.as_mut() { g.resize(size.width, size.height); } }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => { self.cursor = (position.x as f32, position.y as f32); self.pointer_move(); }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if self.console.is_open() || !matches!(self.stage, Some(Stage::Ready)) { return; }
                if self.on_shop() { if state == ElementState::Released { self.shop_click(); } return; }
                let (Some(ui), Some(frame)) = (self.ui.as_ref(), self.frame.as_ref()) else { return };
                self.controller.pointer_button(ui.inner(), frame, &mut self.state, self.cursor.0, self.cursor.1, state == ElementState::Pressed);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.console.is_open() || self.on_shop() { return; }
                let lines = match delta { MouseScrollDelta::LineDelta(_, y) => -y, MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / 40.0 };
                let rows = if lines > 0.0 { 1 } else if lines < 0.0 { -1 } else { 0 };
                if let (Some(ui), Some(frame)) = (self.ui.as_ref(), self.frame.as_ref()) { self.controller.wheel(ui.inner(), frame, &mut self.state, self.cursor.0, self.cursor.1, rows); }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && !event.repeat
                && matches!(event.physical_key, PhysicalKey::Code(KeyCode::F11)) => {
                // F11 only: `InputSettings:bF11TogglesFullscreen=true`, `bAltEnterTogglesFullscreen=false` (CookedIniVersion.txt L1978 / L1977), the two flags
                // UGameViewportClient::InputKey tests (UE 4.16 GameViewportClient.cpp:382-383); Alt+Enter falls through as an ordinary key.
                // Borderless fullscreen toggle (the surface is reconfigured by the Resized event / the next `acquire`).
                if let Some(w) = &self.window { let on = w.fullscreen().is_some(); w.set_fullscreen(if on { None } else { Some(winit::window::Fullscreen::Borderless(None)) }); eprintln!("[lobby] fullscreen {}", !on); }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if matches!(self.stage, Some(Stage::Ready)) { self.keyboard(event_loop, &event); }
            }
            WindowEvent::Focused(focused) => { if let Some(a) = self.audio.as_mut() { a.set_suspended(!focused); } }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) { if let Some(w) = &self.window { w.request_redraw(); } }
}

fn main() {
    let Ok(event_loop) = EventLoop::builder().build() else { eprintln!("could not create event loop"); return };
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    let _ = event_loop.run_app(&mut app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lobby_screens_sound_cues_map_to_sink_events() {
        use fn_audio_sink as s;
        assert_eq!(sound_event(s::LOCKER_ITEM_HOVER_CUE, true), Some(SoundEvent::LockerItemHover));
        assert_eq!(sound_event(s::LOCKER_ITEM_PRESS_CUE, false), Some(SoundEvent::LockerItemPress));
        assert_eq!(sound_event(s::LOBBY_PLAY_PRESS_CUE, false), Some(SoundEvent::LobbyPlayPress));
        assert_eq!(sound_event(s::TOP_BAR_PRESS_CUE, false), Some(SoundEvent::TopBarPress));
        assert_eq!(sound_event(s::TOP_BAR_HOVER_CUE, true), Some(SoundEvent::TopBarHover));
        assert_eq!(sound_event("/Game/Sounds/Unknown.Unknown", true), None);
    }
}



