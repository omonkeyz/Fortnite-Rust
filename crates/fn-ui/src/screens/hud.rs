//! The in-game HUD: quickbar + vitals (old `hud.rs`) and the extra widgets (old `hud_extra::draw`) combined into one draw order, driven by
//! one plain [`HudState`] value.
//!
//! # The HUD contract
//! The game side produces a [`HudState`] once per frame (`fn-hud-adapter` does it from `fn_game::GameSim` + the settings; any other producer
//! works as well, the struct is plain data with no behaviour); the front-end only hands it through: `hud.set_state(&state); hud.tick(dt);
//! hud.present(ctx, w, h, &mut list)`. Nothing in here reads the game, the settings or the clock.
//!
//! Original texture assets with provisional layout for what is not recovered; Slate widget layout of each piece lives in its own module with the
//! provenance notes (`quickbar`, `vitals`, `hud_extra`).
//! Draw order = the old `render()`: `Hud::draw` (quickbar, vitals) then `hud_extra::draw` (compass, minimap, counters, pickup manager).
//! The caller skips the HUD while a full-screen overlay (MainMenu) covers it (`menu::host::covers_hud()` in the old runtime).
use crate::draw::RenderList;
use crate::scene::UiContext;
use crate::screens::hud_extra::{HudExtra, HudExtraState};
use crate::screens::loading::{LoadingScreen, Progress};
use crate::screens::quickbar::{Quickbar, PICKAXE_ICON};
use crate::screens::reticle::ReticleHud;
use crate::screens::reticle_state::HudWorld;
use crate::screens::target_info::TargetInfoHud;
use crate::screens::team_info::{TeamInfo, TeamMember};
use crate::screens::vitals::Vitals;

/// One filled quickbar slot.
#[derive(Clone, Debug, PartialEq)]
pub struct HudItem {
    /// 0 Common, 1 Uncommon, 2 Rare, 3 Epic, 4 Legendary (`RarityData` order).
    pub rarity: u8,
    /// File name of the exported icon texture (e.g. `"T-Icon-Weapons-SK-Pick.png"`); empty = no icon.
    pub icon: String,
    /// QUICKBAR AMMO: the card's stack-count number (a firearm's loaded + inventory ammo, a stackable item's count, see `quickbar` module docs);
    /// None = no number (a pickaxe has none, nothing is invented).
    pub count: Option<u32>,
    /// QUICKBAR AMMO: the firearm's ammo item `AmmoIconBrush`, drawn after the number; None = no icon (items without an ammo item).
    pub ammo: Option<crate::screens::quickbar::MultiSizeBrush>,
}
impl HudItem {
    pub fn new(rarity: u8, icon: &str, count: Option<u32>) -> Self { HudItem { rarity, icon: icon.to_owned(), count, ammo: None } }
    pub fn pickaxe() -> Self { HudItem::new(0, PICKAXE_ICON, None) }
}

/// Minimap view: the centre in map-texture UV (0..1, north up) and the player arrow's rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudMinimap { pub pos: (f32, f32), pub rotation_deg: f32 }
impl Default for HudMinimap { fn default() -> Self { HudMinimap { pos: (0.5, 0.5), rotation_deg: 0. } } }

/// Frame statistics for the Show FPS counter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HudFrameStats { pub fps: f32, pub frame_ms: f32 }

/// Which HUD elements are shown (the `[HUD]` toggles of the Settings screen, tags `HUD.*`). All on by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HudVisibility { pub reticle: bool, pub player_health: bool, pub pickup_info: bool, pub minimap: bool, pub quick_bar: bool, pub target_info: bool, pub interaction_info: bool }
impl Default for HudVisibility { fn default() -> Self { HudVisibility { reticle: true, player_health: true, pickup_info: true, minimap: true, quick_bar: true, target_info: true, interaction_info: true } } }
impl HudVisibility {
    /// The `[HUD]` settings tags in the order of the Settings screen's HUD tab (`pages::hud_tags`).
    pub const TAGS: [&'static str; 7] = ["HUD.Center.Reticle", "HUD.TopLeft.PlayerHitPoints", "HUD.TopLeft.PickupManager", "HUD.TopRight.Minimap", "HUD.Quickbar", "HUD.InWorld.BuildingInfoIndicator", "HUD.TopLeft.PlayerInfo"];
    /// Build from a per-tag switch (`on("HUD.TopRight.Minimap")` = the element is shown).
    pub fn from_tags(on: impl Fn(&str) -> bool) -> Self {
        let t = Self::TAGS;
        HudVisibility { reticle: on(t[0]), player_health: on(t[1]), pickup_info: on(t[2]), minimap: on(t[3]), quick_bar: on(t[4]), target_info: on(t[5]), interaction_info: on(t[6]) }
    }
}

/// Loading screen state: shown instead of the HUD while `Some`.
#[derive(Clone, Debug, PartialEq)]
pub struct HudLoading { pub done: usize, pub total: usize, pub label: String, pub tip: usize, /// 0 = fully shown .. 1 = faded out.
    pub fade: f32 }

/// Everything the HUD shows, as plain data (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct HudState {
    /// 0..=100 each.
    pub health: u16, pub shield: u16,
    pub energy: Option<f32>,
    /// Selected quickbar slot 0..=5 (an empty slot cannot be selected through [`HudState::select`]).
    pub selected: usize,
    /// The six primary quickbar slots; None = empty.
    pub items: [Option<HudItem>; 6],
    /// Wood, stone, metal (clamped to the 999 stack limit when drawn).
    pub resources: [u32; 3],
    /// Backpack (items, max).
    pub backpack: (u32, u32),
    /// INVENTORY FIX2: `KeybindWidget` text of the backpack row = the key bound to `ToggleInventory` (the host fills it from the bindings; default = the cooked
    /// `ConfigD_Athena` mapping `ToggleInventory Key=I`, CookedIniVersion.txt line 2740).
    pub backpack_key: String,
    pub slot_keys: [String; 6],
    /// Eliminations, and players still alive (None = no match data: the widget's designer default is shown).
    pub kills: u32, pub players_left: Option<u32>,
    /// Camera yaw in degrees for the compass.
    pub compass_yaw_deg: f32,
    pub minimap: HudMinimap,
    /// Show the FPS counter (settings `video.show_fps`) and the numbers it shows.
    pub show_fps: bool, pub frame: HudFrameStats,
    pub visible: HudVisibility,
    pub loading: Option<HudLoading>,
    /// World feedback inputs: reticle colour, harvest hits, Target Info, interaction prompt, equip progress (see `screens::reticle_state`).
    pub world: HudWorld,
    pub player_name: String,
    pub team: Vec<TeamMember>,
    pub kill_feed: Vec<String>,
    pub game_phase: Option<(u8, f32)>,
    pub storm_map: Option<crate::screens::hud_extra::StormMap>,
    pub bus_map: Option<crate::screens::hud_extra::MapIcon>,
    pub game_over: Option<crate::screens::hud_cooked::GameOver>,
    pub spectating: Option<(String, f32)>,
    pub spectate_vitals: Option<(f32, f32)>,
    pub team_map: Vec<crate::screens::hud_extra::TeamMapIcon>,
    pub action_alert: Option<(crate::screens::hud_cooked::ActionAlert, String)>,
    pub reticle_status: Option<crate::screens::hud_cooked::StatusEvent>,
}
impl Default for HudState {
    fn default() -> Self {
        let mut items: [Option<HudItem>; 6] = Default::default();
        items[0] = Some(HudItem::pickaxe());
        HudState {
            health: 100, shield: 0, energy: None, selected: 0, items, resources: [0; 3], backpack: (0, crate::screens::hud_extra::BACKPACK_MAX_ASSUMED), backpack_key: crate::screens::hud_extra::BACKPACK_KEY_DEFAULT.into(), slot_keys: crate::screens::quickbar::default_slot_keys(), kills: 0, players_left: None,
            compass_yaw_deg: 0., minimap: HudMinimap::default(), show_fps: false, frame: HudFrameStats::default(), visible: HudVisibility::default(), loading: None, world: HudWorld::default(), spectating: None, spectate_vitals: None, team_map: Vec::new(), player_name: String::new(), team: Vec::new(), kill_feed: Vec::new(), game_phase: None, storm_map: None, bus_map: None, game_over: None, action_alert: None, reticle_status: None,
        }
    }
}
impl HudState {
    pub fn select(&mut self, index: usize) { if index < 6 && self.items[index].is_some() { self.selected = index; } }
    /// Which slots hold an item.
    pub fn occupied(&self) -> [bool; 6] { std::array::from_fn(|i| self.items[i].is_some()) }
    /// Out-of-range values clamped / repaired (a producer bug must never poison the draw list): health and shield to 0..=100, selected slot to a valid one,
    /// rarity to 0..=4, non-finite angles / positions / timings to 0.
    pub fn sanitized(&self) -> HudState {
        let mut s = self.clone();
        s.health = s.health.min(100); s.shield = s.shield.min(100);
        s.selected = s.selected.min(5);
        for it in s.items.iter_mut().flatten() { it.rarity = it.rarity.min(4); }
        let fin = |v: f32| if v.is_finite() { v } else { 0. };
        s.compass_yaw_deg = fin(s.compass_yaw_deg);
        s.minimap = HudMinimap { pos: (fin(s.minimap.pos.0).clamp(0., 1.), fin(s.minimap.pos.1).clamp(0., 1.)), rotation_deg: fin(s.minimap.rotation_deg) };
        s.frame = HudFrameStats { fps: fin(s.frame.fps).max(0.), frame_ms: fin(s.frame.frame_ms).max(0.) };
        if let Some(l) = s.loading.as_mut() { l.fade = fin(l.fade).clamp(0., 1.); l.done = l.done.min(l.total); }
        s.world = s.world.sanitized();
        s.team = s.team.iter().map(TeamMember::sanitized).collect();
        let n = s.kill_feed.len();
        if n > crate::screens::kill_feed::MAX_ENTRIES { s.kill_feed.drain(..n - crate::screens::kill_feed::MAX_ENTRIES); }
        s
    }
    /// Is the equipped item the pickaxe (the weapon whose class sets `ReticleImage = HUD_Xhair_Dot`)? Drives the reticle.
    pub fn pickaxe_equipped(&self) -> bool { self.items.get(self.selected).and_then(|i| i.as_ref()).map_or(false, |i| i.icon == PICKAXE_ICON) }
}

#[derive(Clone, Default)]
pub struct Hud { pub qb: Quickbar, pub vitals: Vitals, pub extra: HudExtra, pub state: HudState, pub loading: LoadingScreen, pub reticle: ReticleHud, pub target: TargetInfoHud,
    /// BUILD HUD: AthenaQuickbarSecondary (the building quickbar above the primary bar); the game side sets `build.state` (build_quickbar.rs).
    pub build: crate::screens::build_quickbar::BuildQuickbar,
    /// PICKUP CARD: the cooked `InteractionIndicator_C` of a pickup (set its install with `pickup.set_install`).
    pub pickup: crate::screens::interaction_cooked::PickupPrompt,
    pub team_indicators: crate::screens::team_indicator_cooked::TeamIndicators,
    /// CONTAINERS: the KeybindWidget hold ring of a hold interaction (material read from the pickup prompt's install).
    pub hold_ring: crate::screens::hold_ring::HoldRing,
    pub stream: crate::screens::pickup_stream::PickupStream,
    pub aerial: crate::screens::aerial_feedback::AerialFeedback,
    pub radial: crate::screens::radial_picker_cooked::RadialPicker,
    pub team: TeamInfo,
    pub phase: crate::screens::game_phase::GamePhaseHud,
    pub cooked: crate::screens::hud_cooked::HudCooked,
    pub full_map: bool }

impl Hud {
    pub fn new() -> Self { Self::default() }
    /// Take the state of this frame (the front-end passes the producer's value straight through). The harvest hits of `world.harvest_hits` are fed to the `+N` popup
    /// state machine exactly once here and removed from the stored state (calling `present` again, or `set_state` with an empty queue, never replays them).
    pub fn set_state(&mut self, s: &HudState) {
        self.state = s.sanitized();
        for hit in std::mem::take(&mut self.state.world.harvest_hits) { self.reticle.on_hit(hit); }
        for p in std::mem::take(&mut self.state.world.picked_up) { self.stream.push(&p); }
        if self.state.world.firearm_hit { self.reticle.on_firearm_hit(); }
        self.reticle.set_muzzle_blocked(self.state.world.firearm_reticle.as_ref().and_then(|r| r.muzzle_blocked_px));
    }
    /// Per-frame HUD animation step (quickbar Selected animation, vitals damage trail, resource count-up, harvest popups, Target Info timer).
    pub fn tick(&mut self, dt: f32) {
        self.qb.primary_focused = !self.build.state.focused_secondary; // HUD FIX: AthenaHUD.SetQuickbarSizes minimises the weapon bar in build mode (quickbar.rs layout_box)
        self.qb.tick(self.state.selected, dt);
        self.build.tick(dt); // BUILD HUD
        self.vitals.tick(dt, self.state.health, self.state.shield);
        self.vitals.tick_energy(dt, self.state.energy);
        self.extra.count.step(self.state.resources, dt);
        self.reticle.tick(dt);
        self.target.tick(dt);
        self.phase.tick(self.state.game_phase, dt);
        self.cooked.tick(dt);
        self.stream.tick(dt);
    }
    /// Copy the quickbar / counters / minimap part of the state into the widgets (`present` does this; `build` leaves the widgets as the caller set them).
    fn sync_widgets(&mut self) {
        let st = &self.state;
        for i in 0..6 {
            match &st.items[i] {
                Some(it) => { self.qb.rarity[i] = it.rarity as usize; self.qb.count[i] = it.count; self.qb.icon[i] = it.icon.clone(); if self.qb.ammo[i] != it.ammo { self.qb.ammo[i] = it.ammo.clone(); } }
                None => { self.qb.rarity[i] = 0; self.qb.count[i] = None; self.qb.icon[i].clear(); self.qb.ammo[i] = None; }
            }
        }
        self.qb.slot_keys = st.slot_keys.clone();
        self.qb.cooldown = st.world.slot_cooldowns; // WIDGETS3: QuickbarSlotCooldown
        self.extra.state = HudExtraState {
            players_alive: st.players_left, kills: st.kills, resources: st.resources, backpack: st.backpack, backpack_key: st.backpack_key.clone(), map_centre: st.minimap.pos, arrow_deg: Some(st.minimap.rotation_deg),
            show_minimap: st.visible.minimap, show_pickups: st.visible.pickup_info, heading_override: None, current_resource: st.world.current_resource.index(), storm_map: st.storm_map, bus: st.bus_map, teammates: st.team_map.clone(),
        };
    }
    /// Append the whole HUD from the widgets' current state. `yaw_deg` = camera yaw for the compass and the minimap arrow.
    /// No fabricated ammo count: the current runtime has no firearm/ammo inventory yet.
    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, yaw_deg: f32, out: &mut RenderList) {
        // Source widget units: SizeBoxBackplate=80x80; inspected rail slots Right=8. DPI: `layout::dpi_scale` (Vertical rule, cooked UIScaleCurve; CookedIniVersion.txt L531-532).
        // QUICKBAR: all layout/draw lives in quickbar.rs (recovered AthenaQuickbarPrimary/AthenaQuickBarSlot data).
        self.qb.build(ctx, w, h, self.state.selected, self.state.occupied(), out);
        self.build.build(ctx, w, h, out); // BUILD HUD: AthenaQuickbarSecondary
        // VITALS (health/shield bars): all layout/draw lives in vitals.rs (AthenaLocalPlayerHitPointInfo / AthenaHitPointBar data).
        self.vitals.build(ctx, w, h, self.state.health, self.state.shield, out);
        self.vitals.build_energy(ctx, w, h, self.state.energy, out);
        self.team.build(ctx, w, h, &self.state.player_name, true, &self.state.team, out);
        self.extra.build(ctx, w, h, yaw_deg, out);
        self.build_game_phase(ctx, w, h, out);
    }
    fn build_game_phase(&mut self, ctx: &mut UiContext, w: f32, h: f32, out: &mut RenderList) {
        let Some(p) = self.state.game_phase else { return };
        let icon = crate::screens::game_phase::icon(ctx, p.0);
        if let Some(r) = self.extra.alive_row { let s = crate::layout::dpi_scale(w, h); self.phase.build_small(ctx, w, h, r.x - crate::screens::hud_extra::ROW_GAP * s, r.y, r.h, p, icon.clone(), out); }
        self.phase.build_banner(ctx, w, h, p, icon, out);
    }
    /// Draw the state set with [`Hud::set_state`]: the loading screen alone while `loading` is set, else the HUD with its element toggles applied
    /// plus the Show FPS counter. This is the one call a front-end makes per frame.
    pub fn prewarm(&mut self, ctx: &mut UiContext) -> crate::draw::TextureId {
        self.team.prewarm(ctx);
        let map = self.extra.prewarm(ctx);
        let _ = self.phase.texts(ctx);
        map
    }
    pub fn prewarm_game_over(&mut self, ctx: &mut UiContext, w: f32, h: f32) -> bool {
        let Some(i) = self.pickup.install() else { return true };
        self.cooked.set_install(i);
        self.cooked.share_store(self.aerial.store());
        self.cooked.prewarm_game_over(ctx, w, h)
    }
    pub fn prewarm_aerial(&mut self, ctx: &mut UiContext, w: f32, h: f32) {
        let Some(i) = self.pickup.install() else { return };
        self.aerial.set_install(i);
        let mut scratch = RenderList::new();
        for action in [crate::screens::aerial_feedback::AerialAction::Jump, crate::screens::aerial_feedback::AerialAction::DeployGlider, crate::screens::aerial_feedback::AerialAction::Skydive] {
            self.aerial.build(ctx, w, h, &crate::screens::aerial_feedback::AerialPrompt { key: "SpaceBar".into(), action }, "Glider", &mut scratch);
        }
    }
    pub fn present(&mut self, ctx: &mut UiContext, w: f32, h: f32, out: &mut RenderList) {
        if let Some(l) = self.state.loading.clone() {
            self.loading.build(ctx, w, h, Progress { total: l.total, done: l.done }, &l.label, l.tip, l.fade, out);
            return;
        }
        self.sync_widgets();
        if let Some(i) = self.pickup.install() { self.cooked.set_install(i); }
        self.cooked.share_store(self.aerial.store());
        self.cooked.set_game_over(self.state.game_over.as_ref());
        self.cooked.set_spectating(self.state.spectating.as_ref().map(|(n, t)| (n.as_str(), *t)));
        self.cooked.set_view_target_vitals(self.state.spectate_vitals);
        if let Some((a, d)) = self.state.action_alert.clone() { self.cooked.alert_player(a, &d); }
        if let Some(e) = self.state.reticle_status { self.cooked.status_event(e); }
        if self.cooked.game_over().is_some() {
            let facts = crate::screens::hud_cooked::HudFacts { players_left: self.state.players_left, kills: self.state.kills, phase: self.state.game_phase };
            if let Some(r) = self.cooked.build_game_over(ctx, w, h, &facts, out) {
                if let (Some(ir), Some(p)) = (r.phase_icon, self.state.game_phase) { if let Some(m) = crate::screens::game_phase::icon(ctx, p.0) { out.material(m, crate::layout::Rect::new(ir[0], ir[1], ir[2], ir[3]), [1.; 4], false); } }
                if let Some(m) = r.minimap { self.extra.draw_minimap_interior(ctx, crate::layout::Rect::new(m[0], m[1], m[2], m[3]), self.state.compass_yaw_deg, r.scale, out); }
                crate::screens::kill_feed::build(ctx, w, h, &self.state.kill_feed, out);
                return;
            }
        }
        let vis = self.state.visible;
        if vis.quick_bar { self.qb.build(ctx, w, h, self.state.selected, self.state.occupied(), out); self.build.build(ctx, w, h, out); } // BUILD HUD: both bars carry HUD.Quickbar.Build
        if vis.player_health { self.vitals.build(ctx, w, h, self.state.health, self.state.shield, out); self.vitals.build_energy(ctx, w, h, self.state.energy, out); }
        self.team.build(ctx, w, h, &self.state.player_name, vis.interaction_info, &self.state.team, out);
        let yaw = self.state.compass_yaw_deg;
        if let Some(i) = self.pickup.install() { self.stream.set_install(i); }
        let sh = if self.extra.state.show_pickups { self.stream.prepare(ctx, w, h) } else { 0. };
        self.extra.stream = (sh, self.stream.width());
        self.extra.build(ctx, w, h, yaw, out);
        if let Some((x, y)) = self.extra.stream_anchor { if sh > 0. { self.stream.draw(ctx, w, h, x, y, out); } }
        self.build_game_phase(ctx, w, h, out);
        crate::screens::kill_feed::build(ctx, w, h, &self.state.kill_feed, out);
        // World feedback (old `render()` order: reticle then the harvest popup after hud_extra). The reticle shows while the pickaxe is equipped and `HUD.Center.Reticle` is on;
        // the `+N` popup (`HUD.Center.ResourceAggregation`) has no Settings row, so no toggle hides it.
        if vis.reticle {
            if self.state.pickaxe_equipped() { self.reticle.build_reticle(ctx, w, h, self.state.world.reticle.on_target, out); }
            else if let Some(r) = self.state.world.firearm_reticle.clone() { self.reticle.build_firearm_reticle(ctx, w, h, &r, out); }
        }
        self.reticle.build_popup(ctx, w, h, out);
        // Target Info (`HUD.InWorld.BuildingInfoIndicator`): the widget keeps following the focus while hidden by its toggle, so it does not pop up stale when re-enabled.
        let target = self.state.world.target;
        self.target.update(target.as_ref(), w, h);
        self.target.prompt = self.state.world.building_prompt.clone(); self.target.ranged_targeting = self.state.world.ranged_targeting;
        if vis.target_info { self.target.build(ctx, w, h, out); }
        crate::screens::health_bar::build(ctx, w, h, &self.state.world.health_bars, out);
        if !self.state.world.team_indicators.is_empty() { if let Some(i) = self.pickup.install() { self.team_indicators.set_install(i); } let list = self.state.world.team_indicators.clone(); self.team_indicators.build(ctx, w, h, &list, out); }
        if vis.interaction_info { if let Some(p) = self.state.world.interaction.clone() {
            // PICKUP CARD: a pickup draws the cooked indicator (+ HUD-PickupItemWidget); the basic prompt otherwise or when the trees are unavailable
            let cooked = p.pickup.as_ref().map_or(false, |c| self.pickup.build(ctx, w, h, &p.key, c, p.screen, out));
            if !cooked {
                // CONTAINERS: a hold interaction (chest / ammo box search) draws the KeybindWidget hold ring (cooked HollowCircularProgressBar)
                match self.state.world.interaction_hold {
                    Some(pct) => { if let Some(i) = self.pickup.install() { self.hold_ring.set_install(i); } crate::screens::interaction::build_hold(ctx, w, h, &p, Some((pct.clamp(0., 1.), &mut self.hold_ring)), out); }
                    None => crate::screens::interaction::build(ctx, w, h, &p, out),
                }
            }
        } }
        if let Some(a) = self.state.world.aerial.clone() { if let Some(i) = self.pickup.install() { self.aerial.set_install(i); } self.aerial.build(ctx, w, h, &a, "Glider", out); }
        if let Some(r) = self.state.world.radial.clone() { if let Some(i) = self.pickup.install() { self.radial.set_install(i); } self.radial.build(ctx, w, h, &r, out); }
        if let Some(a) = self.state.world.ammo.clone() { crate::screens::equipped_item::build(ctx, w, h, &a, out); }
        if let Some(e) = self.state.world.equip { crate::screens::equip_progress::build(ctx, w, h, &e, out); }
        if self.state.show_fps { self.fps_counter(ctx, w, h, out); }
        self.cooked.build_overlays(ctx, w, h, out);
        if self.full_map { self.extra.build_full_map(ctx, w, h, yaw, out); }
    }
    /// "{fps} FPS" in the top-right corner (old `Video.ShowFPS`), right aligned, above the minimap frame.
    fn fps_counter(&mut self, ctx: &mut UiContext, w: f32, h: f32, out: &mut RenderList) {
        use crate::screens::{atlas_px, centred_baseline, put_text, text_width, Snap};
        use crate::text::FontId;
        let s = crate::layout::dpi_scale(w, h);
        let px = atlas_px(16., s);
        let txt = format!("{:.0} FPS", self.state.frame.fps);
        let tw = text_width(ctx, FontId::Bold, &txt, px);
        let base = centred_baseline(ctx, FontId::Bold, px, 2. * s, 16. * s);
        put_text(ctx, out, FontId::Bold, px, w - 10. * s - tw, base, &txt, [0.92, 0.97, 0.95, 1.], Some(([1.6 * s, 1.6 * s], [0., 0., 0., 0.75])), Snap::PerGlyph);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Cmd;
    use crate::screens::testutil::ctx;
    #[test] fn empty_slots_cannot_be_equipped() { let mut h = HudState::default(); h.select(4); assert_eq!(h.selected, 0); h.items[4] = Some(HudItem::new(2, "", Some(3))); h.select(4); assert_eq!(h.selected, 4); h.select(9); assert_eq!(h.selected, 4); }
    #[test] fn initial_vitals() { let h = HudState::default(); assert_eq!((h.health, h.shield), (100, 0)); assert_eq!(h.occupied(), [true, false, false, false, false, false]); }
    #[test] fn sanitizing_repairs_a_bad_producer() {
        let mut s = HudState::default();
        s.health = 5000; s.shield = 101; s.selected = 99; s.items[1] = Some(HudItem::new(9, "x.png", None));
        s.compass_yaw_deg = f32::NAN; s.minimap = HudMinimap { pos: (f32::INFINITY, -3.), rotation_deg: f32::NAN }; s.frame.fps = f32::NAN;
        s.loading = Some(HudLoading { done: 50, total: 10, label: String::new(), tip: 0, fade: 7. });
        let c = s.sanitized();
        assert_eq!((c.health, c.shield, c.selected), (100, 100, 5));
        assert_eq!(c.items[1].as_ref().unwrap().rarity, 4);
        assert_eq!((c.compass_yaw_deg, c.minimap.pos, c.minimap.rotation_deg, c.frame.fps), (0., (0., 0.), 0., 0.));
        let l = c.loading.unwrap(); assert_eq!((l.done, l.fade), (10, 1.));
    }
    #[test] fn tick_primes_vitals_and_animates_selection() {
        let mut h = Hud::new(); for i in 0..6 { h.state.items[i] = Some(HudItem::default_for_test()); } h.state.select(2);
        h.tick(0.05); assert!((h.qb.anim()[2] - 0.5).abs() < 1e-5);
        assert_eq!(h.vitals.anim_state().0, [1., 1.], "first tick snaps (no trail on spawn)");
        h.state.health = 40; h.tick(1. / 60.); let (hp, _) = h.vitals.anim_state(); assert_eq!(hp[0], 0.4); assert!(hp[1] > 0.4);
    }
    impl HudItem { fn default_for_test() -> Self { HudItem::new(0, "", None) } }
    #[test] fn full_hud_builds_deterministically_for_all_aspects() {
        let Some(mut c) = ctx() else { return };
        for (w, hh) in [(1920., 1080.), (1280., 720.), (2560., 1080.), (1024., 768.), (640., 480.), (800., 1280.), (3440., 1440.), (3840., 2160.)] {
            let (mut a, mut b) = (RenderList::new(), RenderList::new());
            let mut h = Hud::new(); h.build(&mut c, w, hh, 0., &mut a);
            let mut h2 = Hud::new(); h2.build(&mut c, w, hh, 0., &mut b);
            assert_eq!(a.cmds, b.cmds, "{w}x{hh}");
            assert!(!a.is_empty());
            for cmd in &a.cmds { match cmd {
                Cmd::Quad(q) => { assert!(q.pos.iter().flatten().chain(q.uv.iter()).chain(q.tint.iter()).all(|v| v.is_finite())); let r = q.bounds(); assert!(r.w > 0. && r.h > 0.); }
                Cmd::Material(m) => assert!(m.rect.w > 0. && m.rect.h > 0. && m.rect.x.is_finite()),
                _ => {}
            } }
        }
    }
    fn quads(l: &RenderList) -> usize { l.cmds.iter().filter(|c| matches!(c, Cmd::Quad(_))).count() }
    fn mats(l: &RenderList) -> usize { l.cmds.iter().filter(|c| matches!(c, Cmd::Material(_))).count() }
    fn present(c: &mut UiContext, s: &HudState, w: f32, h: f32) -> RenderList { let mut hud = Hud::new(); hud.set_state(s); hud.tick(1. / 60.); let mut out = RenderList::new(); hud.present(c, w, h, &mut out); out }
    #[test] fn present_with_the_default_state_equals_the_raw_build_and_state_drives_the_widgets() {
        let Some(mut c) = ctx() else { return };
        let mut raw = Hud::new(); raw.tick(1. / 60.); let mut a = RenderList::new(); raw.build(&mut c, 1920., 1080., 0., &mut a);
        let b = present(&mut c, &HudState::default(), 1920., 1080.);
        // (the raw build leaves the minimap arrow at the view yaw and no override: with the default state both agree)
        // (the default state holds the pickaxe, so `present` adds exactly one quad on top of the raw build: the reticle)
        assert_eq!(b.cmds[..a.cmds.len()], a.cmds[..], "the default state is the default HUD");
        assert_eq!(b.cmds.len(), a.cmds.len() + 1, "plus the pickaxe reticle");
        // items / counts / resources / kills change the output
        let mut s = HudState::default();
        s.items[1] = Some(HudItem::new(3, "", Some(120))); s.resources = [100, 20, 3]; s.kills = 7; s.players_left = Some(42);
        let full = present(&mut c, &s, 1920., 1080.);
        assert_ne!(full.cmds, b.cmds);
        assert!(mats(&full) > mats(&b), "the second item card is a material");
    }
    #[test] fn element_toggles_hide_their_widgets() {
        let Some(mut c) = ctx() else { return };
        let all = present(&mut c, &HudState::default(), 1920., 1080.);
        for (name, f) in [
            ("quick bar", (|v: &mut HudVisibility| v.quick_bar = false) as fn(&mut HudVisibility)),
            ("player health", |v| v.player_health = false), ("minimap", |v| v.minimap = false), ("pickup info", |v| v.pickup_info = false),
        ] {
            let mut s = HudState::default(); f(&mut s.visible);
            let hidden = present(&mut c, &s, 1920., 1080.);
            assert!(hidden.cmds.len() < all.cmds.len(), "{name} hidden removes commands: {} vs {}", hidden.cmds.len(), all.cmds.len());
        }
        let mut s = HudState::default(); s.visible = HudVisibility { reticle: false, quick_bar: false, player_health: false, minimap: false, pickup_info: false, target_info: false, interaction_info: false };
        let none = present(&mut c, &s, 1920., 1080.);
        assert!(none.cmds.len() < all.cmds.len() / 2 + 6 && !none.is_empty(), "compass and counters remain: {} vs {}", none.cmds.len(), all.cmds.len());
    }
    #[test] fn show_fps_adds_the_counter_and_loading_replaces_the_hud() {
        let Some(mut c) = ctx() else { return };
        let off = present(&mut c, &HudState::default(), 1920., 1080.);
        let mut s = HudState::default(); s.show_fps = true; s.frame = HudFrameStats { fps: 144., frame_ms: 6.9 };
        let on = present(&mut c, &s, 1920., 1080.);
        assert!(quads(&on) > quads(&off), "glyph quads of '144 FPS'");
        let mut l = HudState::default(); l.loading = Some(HudLoading { done: 5, total: 10, label: "Loading HUD".into(), tip: 0, fade: 0. });
        let ld = present(&mut c, &l, 1920., 1080.);
        assert!(!ld.is_empty() && ld.cmds != off.cmds, "the loading screen is drawn instead");
        assert_eq!(mats(&ld), 0, "no HUD materials under the loading screen");
    }
}
