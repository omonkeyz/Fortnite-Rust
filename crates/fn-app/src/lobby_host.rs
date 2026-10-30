use crate::audio::SoundEvent;
use crate::lobby_locker::LockerGlue;
use crate::lobby_mm::Matchmaking;
use crate::lobby_profile::Profile;
use crate::lobby_scene::{HeroReady, LobbyAssets, LobbyScene, LobbyScreen, ScreenBackdrops};
use crate::lobby_shop::{ShopEvent, ShopScreen};
use crate::lobby_ui::{Controller, Frame, LobbyState, LobbyUi, Pad, PadKind, UiEvent};
use fn_ui::draw::RenderList;
use fn_ui::layout::Rect;
use fn_ui_wgpu::UiRenderer;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

struct CpuReady { set: Arc<fn_pak::PakSet>, glue: LockerGlue, profile: Profile, assets: LobbyAssets }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LobbyOut { None, MatchReady }

pub fn sound_event(cue: &str, hover: bool) -> Option<SoundEvent> {
    use fn_audio_sink as s;
    if hover {
        match cue { s::TOP_BAR_HOVER_CUE => Some(SoundEvent::TopBarHover), s::LOCKER_ITEM_HOVER_CUE => Some(SoundEvent::LockerItemHover), _ => None }
    } else {
        match cue { s::TOP_BAR_PRESS_CUE => Some(SoundEvent::TopBarPress), s::LOBBY_PLAY_PRESS_CUE => Some(SoundEvent::LobbyPlayPress), s::LOCKER_ITEM_PRESS_CUE => Some(SoundEvent::LockerItemPress), _ => None }
    }
}

pub fn season_xp_for_level(level: i32) -> i32 {
    match level {
        1 => 100, 2 => 250, 3 => 300, 4 => 350,
        5..=14 => 700,
        15..=25 => 1700,
        l => 1800 + (l - 26).max(0) * 100,
    }
}

pub const SEASON_MAX_LEVEL: i32 = 100;
pub const SEASON_CHASE_REWARD_LEVELS: [i32; 2] = [10, 25];

pub struct LobbyHost {
    install: PathBuf,
    cpu_rx: Option<Receiver<Result<CpuReady, String>>>,
    set: Option<Arc<fn_pak::PakSet>>,
    glue: Option<LockerGlue>,
    profile: Option<Profile>,
    assets: Option<LobbyAssets>,
    ui: Option<LobbyUi>,
    renderer: Option<UiRenderer>,
    scene: Option<LobbyScene>,
    shop: Option<ShopScreen>,
    shop_tried: bool,
    ui_ctx: Option<fn_ui::scene::UiContext>,
    shop_catalog: Option<(crate::lobby_shop::Shop, std::rc::Rc<fn_ui::screens::athena_lobby::store::Store>)>,
    backdrops: ScreenBackdrops,
    backdrop_job: Option<(&'static str, Receiver<Result<LobbyAssets, String>>)>,
    pub failed: Option<String>,
    controller: Controller,
    pub state: LobbyState,
    frame: Option<Frame>,
    mm: Option<Matchmaking>,
    hero_job: Option<Receiver<Result<crate::lobby_scene::HeroParts, String>>>,
    hero_queued: bool,
    locker_job: Option<(String, Receiver<Result<crate::lobby_scene::HeroParts, String>>)>,
    t: f32,
    cursor: (f32, f32),
    name: String,
    sounds: Vec<SoundEvent>,
    quests: Vec<fn_ui::screens::athena_lobby::QuestRow>,
    fx: Option<crate::lobby_fx::LobbyFx>,
    quests_loaded: bool,
    fx_job: Option<Receiver<crate::lobby_fx::LobbyFxAssets>>,
    party_jobs: Vec<(i32, String, Receiver<Result<crate::lobby_scene::HeroParts, String>>)>,
    party_sent: Option<(bool, String, usize)>,
    start_sent: bool,
    party_fits: bool,
    party_ui: crate::lobby_party::PartyUi,
    chat_base: Option<(String, String)>,
    mm_error: Option<String>,
    size: (f32, f32),
    party_size_seen: usize,
}

pub const PARTY_PADS: [i32; 15] = [1, 2, 3, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111];
pub const MAX_PARTY: usize = 16;
pub const FAKE_HEROES: [&str; 4] = ["/Game/Athena/Heroes/HID_001_Athena_Commando_F.HID_001_Athena_Commando_F", "/Game/Athena/Heroes/HID_005_Athena_Commando_M.HID_005_Athena_Commando_M",
    "/Game/Athena/Heroes/HID_017_Athena_Commando_M.HID_017_Athena_Commando_M", "/Game/Athena/Heroes/HID_013_Athena_Commando_F.HID_013_Athena_Commando_F"];

pub fn grid_shown(members: usize) -> usize { if members < 5 { 0 } else { (members - 4 + usize::from(members < MAX_PARTY)).min(crate::lobby_world::GRID_SLOTS) } }

pub const PLATE_W_1080: f32 = 115.0;
pub const PLATE_H_1080: f32 = 52.0;
pub const PLATE_TOP_1080: f32 = 150.0;
pub fn destagger_plates(pads: &mut [Pad], view_h: f32) {
    let k = view_h / 1080.0;
    let (pw, ph) = (PLATE_W_1080 * k, PLATE_H_1080 * k);
    for i in 0..pads.len() {
        if !matches!(pads[i].kind, PadKind::Player { .. }) { continue; }
        for _ in 0..12 {
            let hit = (0..i).filter(|&j| matches!(pads[j].kind, PadKind::Player { .. })).find(|&j| (pads[j].x - pads[i].x).abs() < pw && (pads[j].y - pads[i].y).abs() < ph).map(|j| pads[j].y);
            match hit { Some(y) if y - ph >= PLATE_TOP_1080 * k => pads[i].y = y - ph, _ => break }
        }
    }
}

pub fn fake_party(n: usize, name: &str) -> crate::mp_hook::PartyView {
    let n = n.clamp(1, MAX_PARTY);
    let me = crate::mp_hook::PartyMember { slot: 0, name: name.to_owned(), host: true, ready: false, outfit: String::new(), playlist: 0 };
    let members = std::iter::once(me).chain((1..n).map(|i| crate::mp_hook::PartyMember { slot: i as u8, name: format!("Member{i:02}"), host: false, ready: true, outfit: FAKE_HEROES[(i - 1) % FAKE_HEROES.len()].to_owned(), playlist: 0 })).collect();
    crate::mp_hook::PartyView { me: Some(0), code: "TEST16".into(), members }
}

pub fn party_hero(outfit: &str) -> String {
    if outfit.starts_with("/Game/Athena/Heroes/") { outfit.to_owned() } else { fn_pose::br_character::DEFAULT_HERO.to_owned() }
}

pub fn party_pads(pv: &crate::mp_hook::PartyView) -> Vec<(i32, crate::mp_hook::PartyMember)> {
    pv.others().into_iter().zip(PARTY_PADS).map(|(m, id)| (id, m.clone())).collect()
}

pub fn party_can_matchmake(members: usize, _playlist: usize) -> bool { members.max(1) <= MAX_PARTY }

impl LobbyHost {
    pub fn start(install: &Path, name: String) -> LobbyHost {
        let (tx, rx) = channel();
        let inst = install.to_path_buf();
        let spawned = std::thread::Builder::new().name("lobby-load".into()).spawn(move || {
            let r = (|| -> Result<CpuReady, String> {
                let set = Arc::new(fn_pak::PakSet::open_install(&inst).map_err(|e| format!("cannot open the paks under {}: {e}", inst.display()))?);
                let glue = LockerGlue::load(&set);
                let profile = Profile::load_or_new(&crate::lobby_profile::default_profile_path(), glue.default_loadout());
                let hero = glue.equipped_hero(&profile);
                let assets = LobbyAssets::load(&inst, Some(&hero))?;
                Ok(CpuReady { set, glue, profile, assets })
            })();
            let _ = tx.send(r);
        });
        let failed = spawned.err().map(|e| e.to_string());
        LobbyHost {
            install: install.to_path_buf(), cpu_rx: Some(rx), set: None, glue: None, profile: None, assets: None, ui: None, renderer: None, scene: None, shop: None, shop_tried: false, ui_ctx: None, shop_catalog: None,
            backdrops: ScreenBackdrops::new(), backdrop_job: None, failed, controller: Controller::new(), state: LobbyState::default(), frame: None, mm: None, hero_job: None, hero_queued: false, locker_job: None,
            t: 0.0, cursor: (0.0, 0.0), name, sounds: vec![], quests: vec![], fx: None, fx_job: None, quests_loaded: false,
            party_jobs: vec![], party_sent: None, start_sent: false, party_fits: true,
            party_ui: Default::default(), chat_base: None, mm_error: None, size: (0.0, 0.0), party_size_seen: 1,
        }
    }

    fn fake_members(&self) -> Option<usize> {
        let n: usize = fn_live::var("FN_LOBBY_FAKE_PARTY").ok()?.parse().ok()?;
        let step: f32 = fn_live::var("FN_LOBBY_FAKE_PARTY_STEP").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let delay: f32 = fn_live::var("FN_LOBBY_FAKE_PARTY_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let n = if step > 0.0 { 1 + (((self.t - delay).max(0.0) / step) as usize).min(n.saturating_sub(1)) } else { n };
        Some(n.clamp(1, MAX_PARTY))
    }
    pub fn ready(&self) -> bool { self.ui.is_some() && self.renderer.is_some() && self.scene.as_ref().is_some_and(|s| !s.deferred_pending()) && self.profile.is_some() && self.quests_loaded && self.shop_tried && crate::lobby_scene::uploads_pending() == 0 }
    pub fn release_ui_materials(&mut self) -> usize { self.ui.as_mut().map_or(0, |u| u.inner_mut().release_material_cache()) }
    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_owned();
        if let Some(p) = self.profile.as_mut() { if p.display_name() != Some(name) && !name.is_empty() { p.set_display_name(name); let _ = p.flush(); } }
    }
    pub fn take_sounds(&mut self) -> Vec<SoundEvent> { std::mem::take(&mut self.sounds) }
    pub fn equipped_hero(&self) -> Option<String> { match (self.glue.as_ref(), self.profile.as_ref()) { (Some(g), Some(p)) => Some(g.equipped_hero(p)), _ => None } }

    pub fn poll_load(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) {
        if self.failed.is_some() { return; }
        if let Some(rx) = self.cpu_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(c)) => {
                    self.cpu_rx = None;
                    self.set = Some(c.set); self.glue = Some(c.glue); self.profile = Some(c.profile); self.assets = Some(c.assets);
                    if let Some(n) = self.profile.as_ref().and_then(|p| p.display_name()) { self.name = n.to_owned(); }
                    self.load_match_options();
                }
                Ok(Err(e)) => { self.cpu_rx = None; self.failed = Some(e); return; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { self.cpu_rx = None; self.failed = Some("the lobby loader stopped".into()); return; }
            }
        }
        if !crate::platform::timed("lobby: uploads", || crate::lobby_scene::drain_uploads(queue, 6 << 20)) { return; }
        if self.renderer.is_none() { self.renderer = Some(UiRenderer::with_device(device.clone(), queue.clone(), format)); return; }
        if self.ui.is_none() {
            match self.ui_ctx.take() {
                None => match crate::platform::timed("lobby: ui context", || LobbyUi::load_context(&self.install)) { Ok(c) => self.ui_ctx = Some(c), Err(e) => self.failed = Some(format!("lobby UI: {e}")) },
                Some(c) => match crate::platform::timed("lobby: ui load", || LobbyUi::with_context(c, &self.install)) { Ok(u) => self.ui = Some(u), Err(e) => self.failed = Some(format!("lobby UI: {e}")) },
            }
            return;
        }
        if self.scene.is_none() {
            if let Some(a) = self.assets.take() {
                match crate::platform::timed("lobby: scene from assets", || crate::lobby_scene::with_deferred_uploads(|| LobbyScene::from_assets_deferred(a, device, queue, format))) {
                    Ok(s) => {
                        for (m, why) in &s.report.substituted { eprintln!("[lobby] material not original: {m}: {why}"); }
                        if fn_live::var("FN_LOBBY_FX").map_or(true, |v| v != "0") {
                            if let Some(set) = self.set.clone() {
                                let data = s.data.clone();
                                let (tx, rx) = channel();
                                let _ = std::thread::Builder::new().name("lobby-fx".into()).spawn(move || { let _ = tx.send(crate::lobby_fx::load_assets(&set, &data)); });
                                self.fx_job = Some(rx);
                            }
                        }
                        self.scene = Some(s);
                    }
                    Err(e) => self.failed = Some(format!("lobby scene: {e}")),
                }
            }
            return;
        }
        if let Some(s) = self.scene.as_mut().filter(|s| s.deferred_pending()) { crate::platform::timed("lobby: character", || crate::lobby_scene::with_deferred_uploads(|| s.step_deferred())); return; }
        if !self.quests_loaded && self.set.is_some() {
            self.quests_loaded = true;
            let locres = self.ui.as_ref().and_then(|u| u.locres());
            self.quests = crate::platform::timed("lobby: quests", || load_daily_quests(self.set.as_deref(), locres.clone()));
            if let Some(l) = locres.as_ref() {
                let g = l.get("FriendsList", "Global").unwrap_or("").to_owned();
                let hint = l.get("", "LostConnection").unwrap_or("").to_owned();
                if !g.is_empty() { self.state.chat = Some((g.clone(), hint.clone())); self.chat_base = Some((g, hint)); }
            }
            return;
        }
        if !self.shop_tried && self.shop_catalog.is_none() {
            let shared = self.ui.as_ref().map(|u| u.inner().ui.store.clone());
            match crate::platform::timed("lobby: shop catalog", || shared.map_or_else(|| fn_ui::screens::athena_lobby::store::Store::open(&self.install).map(std::rc::Rc::new), Ok).map(|st| (crate::lobby_shop::Shop::load(&st), st))) {
                Ok(c) => { self.shop_catalog = Some(c); return; }
                Err(e) => eprintln!("[lobby] item shop catalog unavailable: {e}"),
            }
        }
        if !self.shop_tried {
            self.shop_tried = true;
            let parts = self.shop_catalog.take();
            match crate::platform::timed("lobby: shop", || parts.ok_or_else(|| "no shop catalog".to_owned()).and_then(|(shop, st)| ShopScreen::from_parts(shop, st))) {
                Ok(s) => self.shop = Some(s),
                Err(e) => eprintln!("[lobby] item shop unavailable: {e}"),
            }
            if let (Some(glue), Some(profile)) = (self.glue.as_ref(), self.profile.as_ref()) {
                glue.sync(profile, &mut self.state.locker);
                self.state.mtx_balance = profile.balance();
            }
        }
    }

    pub fn warm(&mut self, w: u32, h: u32) -> bool {
        let (Some(scene), Some(ui), Some(renderer)) = (self.scene.as_mut(), self.ui.as_mut(), self.renderer.as_mut()) else { return true };
        let st = LobbyState { mtx_balance: self.state.mtx_balance, ..Default::default() };
        let frame = fn_ui::screens::athena_lobby::paint::with_raster_budget(8, || ui.draw(w as f32, h as f32, &st));
        if fn_ui::screens::athena_lobby::paint::raster_pending() { return false; }
        let device = scene_device(scene);
        let tex = device.create_texture(&wgpu::TextureDescriptor { label: Some("lobby warm target"), size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: scene_format(scene), usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        scene.render(&view, w, h, 0.0);
        renderer.render_texture(&frame.list, &mut ui.ctx_mut().assets, &tex, w, h, None);
        true
    }

    fn shop_tab(&self) -> Option<usize> { self.shop.as_ref().map(|s| s.ui.item_shop_tab) }
    fn on_shop(&self) -> bool { self.shop_tab() == Some(self.state.selected_tab) }

    fn set_tab(&mut self, i: usize) {
        self.state.selected_tab = i;
        self.state.locker.picking = None;
        self.state.locker.picker_t = 0.0;
        if self.shop_tab() == Some(i) { if let Some(s) = self.shop.as_mut() { s.open(); } }
        self.backdrops.set_screen(LobbyScreen::from_tab_index(i));
    }

    fn poll_fx(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let Some(rx) = self.fx_job.as_ref() else { return };
        match rx.try_recv() {
            Ok(a) => {
                self.fx_job = None;
                let eye = self.scene.as_ref().map_or(glam::Vec3::ZERO, |s| s.camera_xf().t);
                let fx = crate::lobby_fx::LobbyFx::new(device, queue, &self.install, a, eye);
                eprintln!("[lobby fx] {} particle systems in the lobby", fx.count());
                self.fx = Some(fx);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.fx_job = None,
        }
    }

    fn poll_backdrop(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) {
        if let Some((cam, rx)) = self.backdrop_job.as_ref() {
            match rx.try_recv() {
                Ok(r) => {
                    let cam = *cam;
                    self.backdrop_job = None;
                    match r.and_then(|a| LobbyScene::from_assets(a, device, queue, format)) {
                        Ok(s) => self.backdrops.insert(cam, s),
                        Err(e) => eprintln!("[lobby] backdrop {cam}: {e}"),
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.backdrop_job = None,
            }
            return;
        }
        if let Some(cam) = self.backdrops.pending() {
            let (tx, rx) = channel();
            let inst = self.install.clone();
            let ch = if cam == "Cosmetics" { self.scene.as_ref().and_then(|s| s.share_character()) } else { None };
            let _ = std::thread::Builder::new().name("lobby-backdrop".into()).spawn(move || { let _ = tx.send(LobbyAssets::load_backdrop(&inst, cam, ch)); });
            self.backdrop_job = Some((cam, rx));
        }
    }

    fn request_hero(&mut self) {
        let (Some(glue), Some(profile)) = (self.glue.as_ref(), self.profile.as_ref()) else { return };
        let hero = glue.equipped_hero(profile);
        if self.scene.as_ref().and_then(|s| s.hero()) == Some(hero.as_str()) && self.hero_job.is_none() { return; }
        if self.hero_job.is_some() { self.hero_queued = true; return; }
        let Some(set) = self.set.clone() else { return };
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new().name("lobby-hero".into()).spawn(move || { let _ = tx.send(crate::lobby_scene::load_hero_parts(&set, &hero)); });
        self.hero_job = Some(rx);
    }

    fn poll_hero(&mut self) {
        let Some(rx) = self.hero_job.as_ref() else { return };
        let res = match rx.try_recv() { Ok(r) => r, Err(TryRecvError::Empty) => return, Err(TryRecvError::Disconnected) => Err("the outfit loader stopped".into()) };
        self.hero_job = None;
        match res {
            Ok(parts) => {
                crate::lobby_scene::clear_hero_views();
                if let Some(s) = self.scene.as_mut() { if let Some(b) = s.hero_builder() { let ready: HeroReady = b.build(&parts); s.set_hero_ready(ready); } }
                eprintln!("[lobby] outfit applied to the lobby pawn: {}", parts.hero);
            }
            Err(e) => eprintln!("[lobby] cannot build the equipped outfit: {e}"),
        }
        crate::lobby_scene::clear_hero_mips();
        if std::mem::take(&mut self.hero_queued) { self.request_hero(); }
    }

    fn locker_preview_hero(&self) -> Option<String> {
        let l = &self.state.locker;
        if l.picking != Some(crate::lobby_ui::LockerSlot::Outfit) { return None; }
        let i = l.selected[crate::lobby_ui::LockerSlot::Outfit.index()]?;
        let item = l.items[crate::lobby_ui::LockerSlot::Outfit.index()].get(i)?;
        self.glue.as_ref()?.catalog.find(&item.id)?.hero.clone()
    }

    fn poll_locker_hero(&mut self) -> bool {
        let want = self.locker_preview_hero();
        if let Some((hero, rx)) = self.locker_job.as_ref() {
            match rx.try_recv() {
                Ok(Ok(parts)) => {
                    let hero = hero.clone();
                    self.locker_job = None;
                    if want.as_deref() == Some(hero.as_str()) {
                        let ok = self.backdrops.scene_for_mut("Cosmetics").and_then(|s| s.hero_builder().map(|b| (b.build(&parts), s))).map_or(false, |(r, s)| s.set_hero_ready(r));
                        eprintln!("[lobby] locker preview {hero}: {ok}");
                    }
                    crate::lobby_scene::clear_hero_mips();
                }
                Ok(Err(e)) => { eprintln!("[lobby] locker preview {hero}: {e}"); self.locker_job = None; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.locker_job = None,
            }
        }
        let shown = self.backdrops.scene_for_mut("Cosmetics").and_then(|s| s.hero().map(str::to_owned));
        let Some(want) = want else { return false };
        if shown.as_deref() == Some(want.as_str()) { return true; }
        if self.locker_job.is_none() && shown.is_some() {
            if let Some(set) = self.set.clone() {
                let (tx, rx) = channel();
                let h = want.clone();
                let _ = std::thread::Builder::new().name("lobby-locker-hero".into()).spawn(move || { let _ = tx.send(crate::lobby_scene::load_hero_parts(&set, &h)); });
                self.locker_job = Some((want, rx));
            }
        }
        false
    }

    fn sync_party(&mut self, ready: bool) -> Option<crate::mp_hook::PartyView> {
        let real = crate::mp_hook::party().filter(|p| p.me.is_some());
        let fake = if real.is_none() { self.fake_members() } else { None };
        let pv = real.clone().or_else(|| fake.map(|n| fake_party(n, &self.name)));
        if let Some(pv) = real.as_ref() {
            if !pv.is_leader() { if let Some(l) = pv.leader() { self.state.playlist = l.playlist; } }
            let want = (ready, self.equipped_hero().unwrap_or_default(), self.state.playlist);
            if self.party_sent.as_ref() != Some(&want) && crate::mp_hook::send_member(want.0, &want.1, want.2) {
                eprintln!("[lobby] party member state sent: ready {} outfit {} playlist {}", want.0, want.1, want.2);
                self.party_sent = Some(want);
            }

        }
        let desired: Vec<(i32, String)> = pv.as_ref().map(|p| party_pads(p).into_iter().map(|(id, m)| (id, party_hero(&m.outfit))).collect()).unwrap_or_default();
        let Some(scene) = self.scene.as_mut() else { return pv };
        let members = pv.as_ref().map_or(1, |p| p.members.len());
        if self.party_size_seen != members { eprintln!("[lobby] party size {} -> {members} (extra pads {})", self.party_size_seen, grid_shown(members)); self.party_size_seen = members; }
        scene.set_grid_shown(grid_shown(members));
        let mut done = vec![];
        self.party_jobs.retain(|(id, hero, rx)| match rx.try_recv() {
            Ok(r) => { done.push((*id, hero.clone(), r)); false }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        for (id, hero, r) in done {
            match r {
                Ok(parts) => {
                    let pads: Vec<i32> = desired.iter().filter(|(d, h)| *h == hero && (*d == id || scene.party_pawn_hero(*d) != Some(hero.as_str()))).map(|(d, _)| *d).collect();
                    for pad in pads {
                        if let Some(b) = scene.hero_builder_for(pad) {
                            scene.set_party_pawn(pad, b.build(&parts));
                            scene.set_pad_occupied(pad, true);
                            eprintln!("[lobby] party pad {pad}: {hero}");
                        }
                    }
                }
                Err(e) => eprintln!("[lobby] party pad {id}: cannot build {hero}: {e}"),
            }
        }
        for id in PARTY_PADS {
            if desired.iter().any(|(d, _)| *d == id) { continue; }
            if scene.pad_occupied(id) || scene.party_pawn_hero(id).is_some() { scene.remove_party_pawn(id); scene.set_pad_occupied(id, false); eprintln!("[lobby] party pad {id}: empty"); }
        }
        for (id, hero) in &desired {
            if scene.party_pawn_hero(*id) == Some(hero.as_str()) || self.party_jobs.iter().any(|(_, h, _)| h == hero) { continue; }
            let Some(set) = self.set.clone() else { continue };
            let (tx, rx) = channel();
            let h = hero.clone();
            let _ = std::thread::Builder::new().name("lobby-party-hero".into()).spawn(move || { let _ = tx.send(crate::lobby_scene::load_hero_parts(&set, &h)); });
            self.party_jobs.push((*id, hero.clone(), rx));
        }
        pv
    }

    fn handle_events(&mut self) {
        for ev in self.controller.take_events() {
            match ev {
                UiEvent::HoverSound(c) => { if let Some(e) = sound_event(&c, true) { self.sounds.push(e); } }
                UiEvent::PressSound(c) => { if let Some(e) = sound_event(&c, false) { self.sounds.push(e); } }
                UiEvent::TabSelected(i) => self.set_tab(i),
                UiEvent::Play => {
                    if self.mm.is_none() && self.state.can_matchmake {
                        self.mm = Some(Matchmaking::start());
                        self.mm_error = if self.party_fits { None } else { Some("PartySizeTooBig".to_owned()) };
                        #[cfg(target_arch = "wasm32")]
                        crate::web::stage("LobbyPlay");
                    }
                }
                UiEvent::Cancel => { if self.mm.as_ref().map_or(false, |m| m.can_cancel()) { if self.start_sent { let sent = crate::mp_hook::send_unqueue(); eprintln!("[lobby] matchmaking cancelled: unqueue sent to the relay: {sent}"); } self.mm = None; self.start_sent = false; crate::mp_hook::reset_queue(); } }
                UiEvent::Equip { slot, index } => {
                    let (Some(glue), Some(profile)) = (self.glue.as_ref(), self.profile.as_mut()) else { continue };
                    if glue.equip(profile, slot, &self.state.locker, index).is_some() || index == usize::MAX { let _ = profile.flush(); }
                    glue.sync(profile, &mut self.state.locker);
                    if slot == crate::lobby_ui::LockerSlot::Outfit { self.request_hero(); }
                }
                UiEvent::BannerApply { icon, color } => {
                    if let Some(p) = self.profile.as_mut() { p.set_banner(&icon, &color); let _ = p.flush(); eprintln!("[lobby] banner set: {icon} / {color}"); }
                }
                UiEvent::OptionChanged { id, selected } => self.store_match_option(id, selected),
                UiEvent::Preview { .. } | UiEvent::PickerOpened(_) | UiEvent::PickerClosed | UiEvent::PlaylistChanged(_) | UiEvent::FillChanged(_) => {}
            }
        }
    }

    fn load_match_options(&mut self) {
        for i in 0..self.state.options.len() {
            let id = self.state.options[i].id;
            let saved = crate::name_store::get(&format!("fnweb-opt-{id}")).and_then(|v| v.parse::<usize>().ok()).or_else(|| self.profile.as_ref().and_then(|p| p.match_option(id)));
            if let Some(v) = saved { let o = &mut self.state.options[i]; o.selected = v.min(o.values.len().saturating_sub(1)); }
        }
        self.apply_match_options();
    }
    fn store_match_option(&mut self, id: &'static str, selected: usize) {
        if let Some(p) = self.profile.as_mut() { p.set_match_option(id, selected); let _ = p.flush(); }
        crate::name_store::set(&format!("fnweb-opt-{id}"), &selected.to_string());
        eprintln!("[lobby] match option {id} = {selected}");
        self.apply_match_options();
    }
    fn apply_match_options(&self) {
        let on = self.state.option(fn_ui::screens::athena_lobby::ENHANCED_MODE).is_some_and(|o| o.is_on());
        fn_movement::enhanced::set_enabled(on);
        let bots = self.state.option(fn_ui::screens::athena_lobby::BOTS).map_or(0, |o| fn_ui::screens::athena_lobby::BOT_COUNTS.get(o.selected).copied().unwrap_or(0));
        crate::bots_hook::set_lobby_count(bots);
    }

    fn season_view(&self) -> fn_ui::screens::athena_lobby::SeasonView {
        let (level, xp) = self.profile.as_ref().map_or((1, 0), |p| (p.season_level(), p.season_xp()));
        let level = level.clamp(1, SEASON_MAX_LEVEL);
        let chase = SEASON_CHASE_REWARD_LEVELS.iter().copied().find(|&l| l > level);
        fn_ui::screens::athena_lobby::SeasonView { level, max_level: SEASON_MAX_LEVEL, level_xp: xp, level_xp_for_level: season_xp_for_level(level), chase_reward_level: chase }
    }

    pub fn frame(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, target: &wgpu::Texture, w: u32, h: u32, dt: f32, load_fraction: f32) -> LobbyOut {
        self.t += dt;
        self.size = (w as f32, h as f32);
        let t_frame = std::time::Instant::now();
        self.poll_hero();
        let t_hero = t_frame.elapsed().as_secs_f32() * 1000.0;
        self.poll_backdrop(device, queue, format);
        let t_backdrop = t_frame.elapsed().as_secs_f32() * 1000.0;
        self.poll_fx(device, queue);
        let t_fx = t_frame.elapsed().as_secs_f32() * 1000.0;
        let mut out = LobbyOut::None;
        let local_ready = self.mm.as_ref().map_or(false, |m| m.local_ready);
        let party = self.sync_party(local_ready);
        let in_party = party.as_ref().map_or(false, |p| p.members.len() > 1);
        let leader = party.as_ref().map_or(true, |p| p.is_leader());
        let others_ready = party.as_ref().map_or(true, |p| p.others().iter().all(|m| m.ready));
        self.state.party_leader = leader;
        self.party_fits = party_can_matchmake(party.as_ref().map_or(1, |p| p.members.len()), self.state.playlist);
        let connected = crate::mp_hook::party().map_or(false, |p| p.me.is_some()) && (in_party || crate::bots_hook::wanted() == 0);
        if let Some(m) = self.mm.as_mut() {
            let hold = in_party && (!leader || !others_ready);
            let before = m.step();
            let (q, q_err) = crate::mp_hook::queue_state();
            let hold_match = connected && self.mm_error.is_none() && q != crate::mp_hook::QUEUE_ERROR;
            m.tick_queue(dt, if self.mm_error.is_some() { 0.0 } else { load_fraction }, hold && self.mm_error.is_none(), hold_match);
            if connected && leader && before == crate::lobby_mm::Step::WaitingForReady && m.step() != before && !self.start_sent && self.mm_error.is_none() {
                let fill = fn_ui::screens::athena_lobby::team_fill_rule(self.state.playlist, self.state.team_fill, true).0 == 1;
                self.start_sent = crate::mp_hook::send_queue(self.state.playlist, fill);
                eprintln!("[lobby] party leader: everyone is ready, queued for {} (fill {fill}): {}", fn_net::modes::playlist(self.state.playlist).name, self.start_sent);
            }
            if q == crate::mp_hook::QUEUE_ERROR { eprintln!("[lobby] matchmaking error from the relay: {}", q_err.unwrap_or_default()); crate::mp_hook::reset_queue(); }
            if m.done() { out = LobbyOut::MatchReady; }
        }
        let locres = self.ui.as_ref().and_then(|u| u.locres());
        if self.mm_error.is_some() && self.mm.as_ref().is_some_and(|m| m.step() == crate::lobby_mm::Step::ConnectingToMatchmakingService) {
            let key = self.mm_error.take().unwrap_or_default();
            self.mm = None;
            self.start_sent = false;
            let msg = locres.as_ref().and_then(|l| l.get("Fortnite.FortMatchmakingV2", &key)).unwrap_or("").to_owned();
            eprintln!("[lobby] matchmaking error Fortnite.FortMatchmakingV2 {key}: {msg}");
            self.party_ui.show_error(msg);
        }
        self.party_ui.poll(party.as_ref());
        self.party_ui.apply(&mut self.state);
        self.state.chat = self.chat_base.clone().map(|(g, hint)| (g, self.party_ui.hint.clone().unwrap_or(hint)));
        self.state.matchmaking = self.mm.as_ref().map(|m| m.view(locres.as_deref(), load_fraction));
        let ready = self.mm.as_ref().map_or(false, |m| m.local_ready);
        self.state.mat_time = self.t;
        self.state.season = Some(self.season_view());
        self.state.quests = self.quests.clone();
        if let Some(p) = self.profile.as_ref() { self.state.mtx_balance = p.balance(); }
        let on_shop = self.on_shop();
        let preview_ready = self.poll_locker_hero();
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let (Some(ui), Some(renderer)) = (self.ui.as_mut(), self.renderer.as_mut()) else { return out };
        self.controller.tick(&mut self.state, dt);
        let lobby_cam = self.backdrops.camera() == "HeroSelect";
        if let Some(s) = self.backdrops.scene_for_mut("Cosmetics") { s.hide_character = !preview_ready; }
        let scene_drawn = match self.backdrops.scene_mut() {
            Some(bg) => { bg.render(&view, w, h, self.t); true }
            None => match self.scene.as_mut() {
                Some(scene) => {
                    scene.render_ext(&view, w, h, self.t, self.fx.as_mut().map(|f| f as &mut dyn crate::lobby_scene::LobbyTranslucent));
                    let mut pads = vec![];
                    if self.state.selected_tab == 0 && lobby_cam {
                        let members = party.as_ref().map(party_pads).unwrap_or_default();
                        for s in scene.party_slots().iter().filter(|s| !s.hidden) {
                            if s.occupied {
                                let kind = match members.iter().find(|(id, _)| *id == s.player_id) {
                                    Some((_, m)) => PadKind::Player { name: Some(m.name.clone()), leader: m.host, ready: m.ready },
                                    None => PadKind::Player { name: Some(party.as_ref().and_then(|p| p.local()).map_or_else(|| self.name.clone(), |m| m.name.clone())), leader, ready },
                                };
                                if let Some([x, y]) = scene.project(s.top_widget_cm, w, h) { pads.push(Pad { x, y, pivot: s.top_pivot, kind }); }
                            } else if let Some([x, y]) = scene.project(s.add_widget_cm, w, h) { pads.push(Pad { x, y, pivot: s.add_pivot, kind: PadKind::Empty }); }
                        }
                    }
                    destagger_plates(&mut pads, h as f32);
                    self.state.pads = pads;
                    true
                }
                None => false,
            },
        };
        if !scene_drawn { self.state.pads.clear(); }
        let t_scene = t_frame.elapsed().as_secs_f32() * 1000.0;
        let clear = if scene_drawn { None } else { Some([0.0, 0.0, 0.0, 1.0]) };
        if on_shop {
            let (Some(shop), Some(profile)) = (self.shop.as_mut(), self.profile.as_ref()) else { return out };
            let mut list = RenderList::new();
            if let Some(c) = self.backdrops.darken() { list.fill(Rect::new(0.0, 0.0, w as f32, h as f32), c); }
            let sf = shop.draw(ui.ctx_mut(), w as f32, h as f32, profile, crate::lobby_shop::now_unix());
            list.extend(sf.list);
            renderer.render_texture(&list, &mut ui.ctx_mut().assets, target, w, h, clear);
            self.frame = None;
        } else {
            let frame = fn_ui::screens::athena_lobby::paint::with_raster_budget(6, || ui.draw(w as f32, h as f32, &self.state));
            renderer.render_texture(&frame.list, &mut ui.ctx_mut().assets, target, w, h, clear);
            self.party_ui.log_layout(ui, &frame, target.usage().contains(wgpu::TextureUsages::COPY_SRC) && target.format() == format);
            self.frame = Some(frame);
        }
        let t_ui = t_frame.elapsed().as_secs_f32() * 1000.0;
        self.handle_events();
        let t_all = t_frame.elapsed().as_secs_f32() * 1000.0;
        if t_all > 50.0 { eprintln!("[lobby] slow frame {t_all:.0} ms: hero {t_hero:.0}, backdrop {:.0}, fx {:.0}, scene {:.0}, ui {:.0}, events {:.0}", t_backdrop - t_hero, t_fx - t_backdrop, t_scene - t_fx, t_ui - t_scene, t_all - t_ui); }
        out
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        self.cursor = (x, y);
        if self.on_shop() { if let Some(s) = self.shop.as_mut() { s.pointer_move(x, y); } return; }
        let (Some(ui), Some(frame)) = (self.ui.as_ref(), self.frame.as_ref()) else { return };
        if self.party_ui.modal_open() { self.party_ui.pointer_move(ui, frame, x, y); return; }
        self.controller.pointer_move(ui.inner(), frame, &mut self.state, x, y);
    }

    pub fn pointer_button(&mut self, down: bool) {
        let (x, y) = self.cursor;
        if self.on_shop() {
            if down { return; }
            let now = crate::lobby_shop::now_unix();
            let (Some(shop), Some(profile)) = (self.shop.as_mut(), self.profile.as_mut()) else { return };
            match shop.click(x, y, profile, now) {
                ShopEvent::TabSelected(i) => { self.sounds.push(SoundEvent::TopBarPress); self.set_tab(i); }
                ShopEvent::Bought { .. } => {
                    let _ = profile.flush();
                    if let Some(g) = self.glue.as_ref() { g.sync(profile, &mut self.state.locker); }
                    self.request_hero();
                }
                ShopEvent::Message(m) => eprintln!("[lobby] shop: {m}"),
                ShopEvent::None => {}
            }
            return;
        }
        let (Some(ui), Some(frame)) = (self.ui.as_ref(), self.frame.as_ref()) else { return };
        if self.party_ui.modal_open() {
            if let crate::lobby_party::PartyAction::Join(code) = self.party_ui.pointer_button(ui, frame, x, y, down) { self.join_party(code); }
            return;
        }
        let join_box = ui.inner().join_code_widget().and_then(|id| frame.id_rects.get(id).copied()).filter(|r| r.w > 0.0 && r.contains(x, y));
        if join_box.is_some() { if !down { self.party_ui.open_join(); } return; }
        if down { self.party_ui.join_input = None; }
        if !down && frame.rect("Social").is_some_and(|r| r.w > 0.0 && r.contains(x, y)) {
            self.sounds.push(SoundEvent::TopBarPress);
            self.open_party_finder();
            return;
        }
        if !down && self.state.selected_tab == 0 {
            let empty = frame.pads.iter().zip(&self.state.pads).any(|(r, p)| matches!(p.kind, PadKind::Empty) && r.contains(x, y));
            if empty { self.open_party_finder(); return; }
        }
        if !down && self.mm.is_none() && crate::lobby_party::PartyUi::chat_strip_hit(self.size.0, self.size.1, x, y) { self.party_ui.open_chat(); return; }
        self.controller.pointer_button(ui.inner(), frame, &mut self.state, x, y, down);
        self.handle_events();
    }

    fn open_party_finder(&mut self) {
        self.party_ui.open_finder();
        if crate::mp_hook::party().is_none() { let ok = crate::mp_hook::party_create(&self.name); eprintln!("[lobby] party finder opened, party create requested: {ok}"); }
    }

    fn join_party(&mut self, code: String) {
        if crate::mp_hook::party().is_some_and(|p| p.me.is_some() && p.code.eq_ignore_ascii_case(&code)) { eprintln!("[lobby] party finder: already in party {code}"); return; }
        if self.mm.is_some() {
            if self.start_sent { let sent = crate::mp_hook::send_unqueue(); eprintln!("[lobby] matchmaking cancelled to change party: unqueue sent {sent}"); }
            self.mm = None;
            self.start_sent = false;
            self.mm_error = None;
        }
        let from = crate::mp_hook::party().filter(|p| p.me.is_some()).map(|p| p.code);
        eprintln!("[lobby] party finder: join {code}{}", from.map_or(String::new(), |c| format!(" (leaving party {c})")));
        self.party_sent = None;
        crate::mp_hook::party_join(&code, &self.name);
    }

    pub fn chat_open(&self) -> bool { self.party_ui.chat_input.is_some() || self.party_ui.join_input.is_some() }
    pub fn chat_key(&mut self, k: fn_console::Key) {
        if self.party_ui.join_input.is_none() { self.party_ui.chat_key(k); return; }
        if let crate::lobby_party::PartyAction::Join(code) = self.party_ui.join_key(k) { self.join_party(code); }
    }

    pub fn wheel(&mut self, notches: f32) {
        if self.on_shop() { return; }
        let rows = if notches < 0.0 { 1 } else if notches > 0.0 { -1 } else { 0 };
        let (x, y) = self.cursor;
        if let (Some(ui), Some(frame)) = (self.ui.as_ref(), self.frame.as_ref()) { self.controller.wheel(ui.inner(), frame, &mut self.state, x, y, rows); }
    }

    pub fn escape(&mut self) {
        if self.party_ui.escape() { return; }
        if self.on_shop() && self.shop.as_mut().map_or(false, |s| s.back()) { return; }
        let _ = self.controller.cancel(&mut self.state);
    }

    pub fn enter(&mut self) {
        if !self.on_shop() { if !self.party_ui.modal_open() { self.party_ui.open_chat(); } return; }
        let now = crate::lobby_shop::now_unix();
        if let (Some(shop), Some(profile)) = (self.shop.as_mut(), self.profile.as_mut()) {
            if let ShopEvent::Bought { .. } = shop.confirm(profile, now) {
                let _ = profile.flush();
                if let Some(g) = self.glue.as_ref() { g.sync(profile, &mut self.state.locker); }
                self.request_hero();
            }
        }
    }

    pub fn debug_play(&mut self) { if self.mm.is_none() { self.mm = Some(Matchmaking::start()); } }

    pub fn release_gpu(&mut self) { if let Some(s) = self.scene.as_mut() { s.release_gpu(); } }
    pub fn flush(&mut self) { if let Some(p) = self.profile.as_mut() { let _ = p.flush(); } }
}

fn scene_device(scene: &LobbyScene) -> wgpu::Device { scene.device().clone() }
fn scene_format(scene: &LobbyScene) -> wgpu::TextureFormat { scene.output_format() }

pub const DAILY_QUESTS: [&str; 3] = [
    "/Game/Athena/Items/Quests/DailyQuests/AthenaDailyQuest_InteractAmmoCrate",
    "/Game/Athena/Items/Quests/DailyQuests/AthenaDailyQuest_PlayerElimination",
    "/Game/Athena/Items/Quests/DailyQuests/AthenaDaily_Solo_Top25",
];

fn load_daily_quests(set: Option<&fn_pak::PakSet>, locres: Option<std::rc::Rc<fn_ui::screens::athena_lobby::store::Locres>>) -> Vec<fn_ui::screens::athena_lobby::QuestRow> {
    let Some(set) = set else { return vec![] };
    let text = |v: &serde_json::Value| -> String {
        let key = v.get("key").and_then(|x| x.as_str()).unwrap_or("");
        let ns = v.get("namespace").and_then(|x| x.as_str()).unwrap_or("");
        if key.is_empty() { return v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_owned(); }
        locres.as_ref().and_then(|l| l.get(ns, key)).unwrap_or("").to_owned()
    };
    let mut out = vec![];
    for path in DAILY_QUESTS {
        let pd = match fn_uasset::PackageData::load(set, path) { Ok(p) => p, Err(e) => { eprintln!("[lobby] daily quest {path}: {e}"); continue } };
        let pkg = match pd.package() { Ok(p) => p, Err(e) => { eprintln!("[lobby] daily quest {path}: {e}"); continue } };
        let Some(idx) = (0..pkg.exports.len()).find(|&i| pkg.export_class_name(i) == "FortQuestItemDefinition") else { continue };
        let Ok(obj) = pkg.read_export(idx) else { continue };
        let props = obj.props.to_json(&pkg);
        let name = props.get("DisplayName").map(&text).unwrap_or_default();
        let obj0 = props.get("Objectives").and_then(|o| o.as_array()).and_then(|a| a.first()).cloned().unwrap_or(serde_json::Value::Null);
        let task = obj0.get("HudShortDescription").map(&text).unwrap_or_default();
        let required = obj0.get("Count").and_then(|c| c.as_i64()).unwrap_or(1) as i32;
        let reward_xp = props.get("Rewards").and_then(|r| r.as_array()).and_then(|a| a.first()).and_then(|r| r.get("Quantity")).and_then(|q| q.as_i64()).unwrap_or(0) as i32;
        out.push(fn_ui::screens::athena_lobby::QuestRow { name, task, achieved: 0, required, reward_xp });
    }
    eprintln!("[lobby] daily quests: {}", out.iter().map(|q| format!("{} ({}: 0/{}, {} XP)", q.name, q.task, q.required, q.reward_xp)).collect::<Vec<_>>().join(", "));
    out
}

#[cfg(test)]
mod party_tests {
    use super::*;
    use crate::mp_hook::{PartyMember, PartyView};
    fn m(slot: u8, host: bool, outfit: &str) -> PartyMember { PartyMember { slot, name: format!("P{slot}"), host, ready: false, outfit: outfit.into(), playlist: 0 } }
    #[test]
    fn remote_members_fill_pads_one_to_three_in_slot_order() {
        let pv = PartyView { me: Some(2), code: "ABCDEF".into(), members: vec![m(5, false, ""), m(0, true, "/Game/Athena/Heroes/HID_017_Athena_Commando_M.HID_017_Athena_Commando_M"), m(2, false, ""), m(3, false, "")] };
        let pads = party_pads(&pv);
        assert_eq!(pads.iter().map(|(id, p)| (*id, p.slot)).collect::<Vec<_>>(), vec![(1, 0), (2, 3), (3, 5)]);
        assert!(!pv.is_leader() && pv.leader().map(|l| l.slot) == Some(0));
        assert_eq!(party_hero(&pads[0].1.outfit), "/Game/Athena/Heroes/HID_017_Athena_Commando_M.HID_017_Athena_Commando_M");
        assert_eq!(party_hero("../../x"), fn_pose::br_character::DEFAULT_HERO);
    }
    #[test]
    fn party_size_limits_follow_the_playlist() {
        assert!(party_can_matchmake(1, 0) && party_can_matchmake(2, 0) && party_can_matchmake(16, 0) && !party_can_matchmake(17, 0));
        assert!(party_can_matchmake(16, 1) && party_can_matchmake(16, 2) && !party_can_matchmake(17, 2));
    }
    #[test]
    fn the_grid_shows_no_extra_pad_up_to_four_then_the_members_plus_one_free_slot() {
        assert_eq!((1..=16).map(grid_shown).collect::<Vec<_>>(), vec![0, 0, 0, 0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 12]);
        let plate = |x: f32, y: f32| Pad { x, y, pivot: [0.5, 1.0], kind: PadKind::Player { name: None, leader: false, ready: false } };
        let mut pads = vec![plate(900.0, 300.0), plate(1100.0, 300.0), plate(950.0, 290.0), plate(1400.0, 280.0)];
        destagger_plates(&mut pads, 1080.0);
        assert_eq!(pads.iter().map(|p| p.y).collect::<Vec<_>>(), vec![300.0, 300.0, 248.0, 280.0]);
        let pv = fake_party(16, "Me");
        assert_eq!(pv.members.len(), 16);
        assert_eq!(party_pads(&pv).iter().map(|(id, _)| *id).collect::<Vec<_>>(), PARTY_PADS.to_vec());
    }
}
