//! fn-hud-adapter: the HUD wiring contract between the game and the UI.
//!
//! * [`fn_ui::screens::hud::HudState`] is a plain struct (health, shield, selected slot, items with rarity / icon / count, resources, kills,
//!   players left, compass yaw, minimap position and rotation, Show FPS and frame stats, element toggles, loading progress).
//! * [`hud_state`] produces it from what the game owns: `fn_game::GameSim` (pawn pose, harvested resources), `fn_settings::Settings` (Show FPS,
//!   the `[HUD]` element toggles) and a [`HudInputs`] for the parts the game logic does not model yet (vitals, inventory, match info, loading).
//! * `fn-app` only passes it through: `hud.set_state(&hud_state(&sim, &settings, &inputs)); hud.tick(dt); hud.present(ctx, w, h, &mut list)`.
//!
//! The adapter has no state of its own except [`FrameClock`] (a smoothed fps meter); it never reads a clock, a window or a file.
pub mod feedback;
pub use feedback::{AggregateView, HarvestPopups, HudFeedback, HudTagVisibility, PackView, PopupView, PotentialView, ReticleState};
use fn_game::{settings::{FpsMeter, Settings}, GameSim};
use fn_ui::screens::hud::{HudFrameStats, HudItem, HudLoading, HudMinimap, HudState, HudVisibility};

/// What the HUD shows that `fn-game` does not model yet. `Default` = a fresh spawn: full health, no shield, the pickaxe in slot 1, no kills, no
/// match data (players left shows the widget's designer default), nothing in the backpack, no frame numbers, no loading screen.
#[derive(Clone, Debug, PartialEq)]
pub struct HudInputs {
    /// (health, shield), 0..=100 each.
    pub vitals: (u16, u16),
    /// Quickbar contents; None entries are empty slots. `None` for the whole value = only the pickaxe.
    pub items: Option<[Option<HudItem>; 6]>,
    pub selected: usize,
    pub kills: u32,
    pub players_left: Option<u32>,
    /// (items, max) shown next to the backpack icon; None = the widget default (empty).
    pub backpack: Option<(u32, u32)>,
    /// Metal count (the sandbox world only knows wood and stone).
    pub metal: u32,
    pub frame: HudFrameStats,
    pub loading: Option<HudLoading>,
    /// The held firearm's AthenaEquippedItemWidget numbers (None = nothing ranged is held).
    pub ammo: Option<fn_ui::screens::equipped_item::EquippedAmmo>,
    /// `GameplayCue.Athena.Equipping` as the use-progress ring (`AthenaEquipProgress`) shows it while a consumable is being used.
    pub equip: Option<fn_ui::screens::reticle_state::EquipProgress>,
    /// WIDGETS3: the running cooldown of each primary quickbar slot's item (`QuickbarSlotCooldown`).
    pub slot_cooldowns: [Option<fn_ui::screens::slot_cooldown::SlotCooldown>; 6],
    /// Original native ranged reticle texture selected by the held weapon. None for melee/consumables.
    pub firearm_reticle: Option<fn_ui::screens::reticle_state::FirearmReticle>,
    /// A ranged hit was confirmed by the local hit-registration result this frame. This is an event, not persistent state.
    pub firearm_hit: bool,
    /// MINIMAP FIX: the baseplate-to-level translation (UE cm, X / Y) of the loaded props, `fn-app storm_hook::offset_cm()[..2]`: a port point (x, z) m is the
    /// Athena_Terrain level point (x * 100 - off[0], z * 100 - off[1]) cm. [0, 0] without the real props (port metres = UE level metres).
    pub level_offset_cm: [f32; 2],
    pub player_name: String,
    pub teammates: Vec<fn_ui::screens::team_info::TeamMember>,
    pub team_map: Vec<(u8, f32, f32, f32, bool)>,
}
impl Default for HudInputs {
    fn default() -> Self { HudInputs { vitals: (100, 0), items: None, selected: 0, kills: 0, players_left: None, backpack: None, metal: 0, frame: HudFrameStats::default(), loading: None, ammo: None, equip: None, slot_cooldowns: [None; 6], firearm_reticle: None, firearm_hit: false, level_offset_cm: [0.; 2], player_name: String::new(), teammates: Vec::new(), team_map: Vec::new() } }
}

/// Smoothed frames-per-second counter that feeds `HudInputs::frame` (the old runtime's `FpsMeter`).
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameClock { meter: FpsMeter, last_dt: f32 }
impl FrameClock {
    pub fn tick(&mut self, dt: f32) { self.meter.tick(dt); if dt.is_finite() && dt > 0. { self.last_dt = dt; } }
    pub fn stats(&self) -> HudFrameStats { HudFrameStats { fps: self.meter.fps, frame_ms: self.last_dt * 1000. } }
}

/// Is the `[HUD]` element switched on? Missing key = on; `False` / `0` / `Off` (any case) = off (the Settings screen's rule).
pub fn hud_flag(settings: &Settings, tag: &str) -> bool {
    match settings.extra("HUD", tag) { Some(v) => !(v.eq_ignore_ascii_case("false") || v == "0" || v.eq_ignore_ascii_case("off")), None => true }
}
/// The element toggles from the settings.
pub fn hud_visibility(settings: &Settings) -> HudVisibility { HudVisibility::from_tags(|tag| hud_flag(settings, tag)) }

/// MINIMAP FIX (2026-10-05): `AFortAthenaMapInfo` location = the map centre. `AFortInGameMapManagerAthena`'s WorldToMap (exe vfunc 191 @ 0x1404490d0) subtracts
/// `GameState.MapInfo->RootComponent->RelativeLocation` (XY) from the world point; Athena_Terrain's `DefaultMapInfo_4:RootComponent0.RelativeLocation` =
/// (32224, -25744, 1312) cm (`fn-inspect props /Game/Athena/Maps/Athena_Terrain.umap`). Its `ScreenshotCropGuide` box (extent 130000 cm) spans the same 2600 m.
pub const MAP_INFO_LOCATION_CM: [f32; 2] = [32224., -25744.];
/// `FortWorldSettings.MapWorldScale` of Athena_Terrain (tagged 290.0): cm per map unit. `AFortInGameMapManagerAthena::BeginPlay` (exe 0x14041d560) copies it to the
/// manager (+0x4e4), the WorldToMap divides by it.
pub const MAP_WORLD_SCALE_CM: f32 = 290.;

/// MINIMAP FIX: UE level point (cm) -> minimap UV on the map texture, the exe's WorldToMap (vfunc 191 @ 0x1404490d0) over `MapLayerSize`:
/// `map = ((p.y - c.y) / MapWorldScale + S/2, S/2 - (p.x - c.x) / MapWorldScale)`, S = `MapLayerSize` (896), and the paint (`FUN_1430b6a40`) stretches the
/// `AthenaMapImage` brush (MiniMapAthena, 2048 px) over the S x S map, so uv = map / S. UE +X is north (up), +Y east (right). Not clamped.
pub fn minimap_uv_ue(x_cm: f32, y_cm: f32) -> (f32, f32) {
    let s = fn_ui::screens::hud_extra::MAP_LAYER_SIZE;
    let (lx, ly) = (x_cm - MAP_INFO_LOCATION_CM[0], y_cm - MAP_INFO_LOCATION_CM[1]);
    ((ly / MAP_WORLD_SCALE_CM + s * 0.5) / s, (s * 0.5 - lx / MAP_WORLD_SCALE_CM) / s)
}

/// Port world position (metres) -> minimap UV (north up), clamped to the texture (NaN -> centre).
/// SUN-AXIS FIX (2026-10-05): the port's world is UE `(x, y, z) -> (x, z, y)` (`fn_render::lighting::ue_to_world`), so port +x is UE +X (north) and +z UE +Y (east).
/// MINIMAP FIX (2026-10-05): the port point is moved back into the Athena_Terrain level by `level_offset_cm` (see [`HudInputs::level_offset_cm`]) and mapped with
/// the exe's WorldToMap ([`minimap_uv_ue`]). Was: the whole 2048 px texture stretched over the 512 m baseplate centred on the map centre (APPROXIMATE).
pub fn minimap_uv(x: f32, z: f32, level_offset_cm: [f32; 2]) -> (f32, f32) {
    let (u, v) = minimap_uv_ue(x * 100. - level_offset_cm[0], z * 100. - level_offset_cm[1]);
    (if u.is_finite() { u.clamp(0., 1.) } else { 0.5 }, if v.is_finite() { v.clamp(0., 1.) } else { 0.5 })
}

/// The UE yaw (degrees, 0 = north = UE +X, 90 = east) of the view for the port's yaw. The port's yaw 0 looks along -z (`fn_game::horizontal_step`:
/// forward = (sin yaw, 0, -cos yaw)), which is UE -Y (west) under the canonical `(x, z, y)` map, so UE yaw = port yaw - 90.
/// SUN-AXIS FIX (2026-10-05): the compass / minimap arrow used the port yaw directly, i.e. assumed the old `(y, z, -x)` sun frame; `AthenaCompass.UpdateCompassMaterial`
/// feeds the camera's UE yaw (T_Compass: N at yaw 0, E at 90).
pub fn ue_yaw_deg(port_yaw_deg: f32) -> f32 { (port_yaw_deg - 90.).rem_euclid(360.) }

pub fn team_rows(player_name: &str, vitals: (u16, u16), teammates: &[fn_ui::screens::team_info::TeamMember]) -> Vec<fn_ui::screens::team_info::TeamMember> {
    use fn_ui::screens::team_info::TeamMember;
    let mut v: Vec<TeamMember> = Vec::new();
    if !player_name.is_empty() { v.push(TeamMember::new(player_name, 0, vitals.0.min(100) as f32 / 100., vitals.1.min(100) as f32 / 100.)); }
    v.extend(teammates.iter().cloned());
    for (i, m) in v.iter_mut().enumerate() { m.player_index = (i as u8).min(3); }
    v
}

/// The HUD state of this frame.
pub fn hud_state(sim: &GameSim, settings: &Settings, inputs: &HudInputs) -> HudState {
    let mut items: [Option<HudItem>; 6] = inputs.items.clone().unwrap_or_else(|| { let mut i: [Option<HudItem>; 6] = Default::default(); i[0] = Some(HudItem::pickaxe()); i });
    if items.iter().all(|i| i.is_none()) { items[0] = Some(HudItem::pickaxe()); } // the pickaxe cannot be dropped
    let mut s = HudState {
        health: inputs.vitals.0, shield: inputs.vitals.1,
        selected: inputs.selected, items,
        resources: [sim.world.wood, sim.world.stone, sim.world.metal.max(inputs.metal)], // INVENTORY HOOK: metal is the world counter too (F2 givemetal / harvest), not only the input
        backpack: inputs.backpack.unwrap_or(HudState::default().backpack),
        kills: inputs.kills, players_left: inputs.players_left,
        compass_yaw_deg: ue_yaw_deg(sim.yaw), // SUN-AXIS FIX: UE heading, see ue_yaw_deg
        minimap: HudMinimap { pos: minimap_uv(sim.x, sim.z, inputs.level_offset_cm), rotation_deg: ue_yaw_deg(sim.yaw) },
        show_fps: settings.video.show_fps, frame: inputs.frame,
        visible: hud_visibility(settings),
        loading: inputs.loading.clone(),
        // `HudState::world` (reticle / harvest hits / Target Info / interaction / equip progress, `fn_ui::screens::reticle_state`): filled by the world-feedback
        // producer, empty until then (the `..` keeps this literal compiling when the contract grows).
        ..HudState::default()
    };
    s.world.ammo = inputs.ammo.clone();
    s.energy = sim.controller.enhanced.as_ref().filter(|e| e.tac.active() || e.tac.energy < fn_game::movement::enhanced::tac::ENERGY_MAX).map(|e| e.tac.energy);
    s.world.equip = inputs.equip;
    s.world.slot_cooldowns = inputs.slot_cooldowns; // WIDGETS3
    s.world.firearm_reticle = inputs.firearm_reticle.clone();
    s.world.firearm_hit = inputs.firearm_hit;
    s.player_name = inputs.player_name.clone();
    s.team = team_rows(&inputs.player_name, inputs.vitals, &inputs.teammates);
    s.team_map = inputs.team_map.iter().map(|(i, x, z, yaw, dbno)| fn_ui::screens::hud_extra::TeamMapIcon { uv: minimap_uv(*x, *z, inputs.level_offset_cm), yaw_deg: ue_yaw_deg(*yaw), tint: fn_ui::screens::team_info::PLAYER_TINTS[(*i as usize).min(3)], dbno: *dbno }).collect();
    // an empty or missing selection falls back to the first filled slot (HudState::select never lands on an empty one)
    if s.items.get(s.selected).map_or(true, |i| i.is_none()) { s.selected = s.items.iter().position(|i| i.is_some()).unwrap_or(0); }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use fn_game::settings::Settings;
    use fn_ui::draw::RenderList;
    use fn_ui::screens::hud::Hud;

    #[test] fn the_held_firearms_ammo_reaches_the_equipped_item_widget_state() {
        let sim = GameSim::new(); let ammo = fn_ui::screens::equipped_item::EquippedAmmo { magazine: 30, remaining: 150, icon: "T-Items-AmmoMedium.png".into() };
        let st = hud_state(&sim, &Settings::default(), &HudInputs { ammo: Some(ammo.clone()), ..Default::default() }); assert_eq!(st.world.ammo, Some(ammo));
        assert!(hud_state(&sim, &Settings::default(), &HudInputs::default()).world.ammo.is_none());
    }
    #[test] fn the_local_player_is_team_row_zero() {
        let sim = GameSim::new();
        let st = hud_state(&sim, &Settings::default(), &HudInputs { player_name: "Claude".into(), vitals: (75, 50), ..Default::default() });
        assert_eq!(st.team.len(), 1);
        assert_eq!((st.team[0].name.as_str(), st.team[0].player_index, st.team[0].health, st.team[0].shield), ("Claude", 0, 0.75, 0.5));
        let mate = fn_ui::screens::team_info::TeamMember::new("Bot", 3, 1., 0.);
        let rows = team_rows("Claude", (100, 0), &[mate]);
        assert_eq!(rows.iter().map(|r| r.player_index).collect::<Vec<_>>(), vec![0, 1]);
        assert!(hud_state(&sim, &Settings::default(), &HudInputs::default()).team.is_empty());
    }
    #[test] fn a_fresh_spawn_is_the_default_hud() {
        let sim = GameSim::new();
        let st = hud_state(&sim, &Settings::default(), &HudInputs::default());
        assert_eq!((st.health, st.shield, st.selected), (100, 0, 0));
        assert_eq!(st.occupied(), [true, false, false, false, false, false], "just the pickaxe");
        assert_eq!(st.items[0], Some(HudItem::pickaxe()));
        assert_eq!(st.resources, [0, 0, 0]);
        assert!(st.visible == HudVisibility::default() && !st.show_fps && st.loading.is_none());
        // SUN-AXIS FIX: the spawn looks along -z = UE -Y = west, so the compass reads 270 (W); z = 12 is 12 m east of the centre
        assert_eq!(st.compass_yaw_deg, ue_yaw_deg(sim.yaw));
        assert_eq!(st.minimap.rotation_deg, ue_yaw_deg(sim.yaw));
        assert_eq!(ue_yaw_deg(0.), 270.);
        // MINIMAP FIX: without a level offset the port metres are UE level metres: (0, 12 m) -> uv of UE (0, 1200) cm, south-east of the map centre (MapInfo)
        assert_eq!(st.minimap.pos, minimap_uv_ue(0., 1200.));
        let (u, v) = st.minimap.pos; assert!((u - 0.6037).abs() < 1e-3 && (v - 0.6240).abs() < 1e-3, "{u} {v}");
    }
    #[test] fn world_to_map_is_the_exe_transform_and_the_level_offset_is_undone() {
        // MINIMAP FIX: the MapInfo actor is the map centre; 290 cm per map unit, 896 units across (2598.4 m), +X up, +Y right
        assert_eq!(minimap_uv_ue(MAP_INFO_LOCATION_CM[0], MAP_INFO_LOCATION_CM[1]), (0.5, 0.5));
        let half = 448. * MAP_WORLD_SCALE_CM;
        let (u, v) = minimap_uv_ue(MAP_INFO_LOCATION_CM[0] + half, MAP_INFO_LOCATION_CM[1] + half);
        assert!((u - 1.).abs() < 1e-6 && v.abs() < 1e-6, "north-east corner = top right");
        // a port point maps like its level point: level = port * 100 - offset
        let off = [-31000., 27000.];
        assert_eq!(minimap_uv(5., -7., off), minimap_uv_ue(500. + 31000., -700. - 27000.));
        let sim = GameSim::new();
        let st = hud_state(&sim, &Settings::default(), &HudInputs { level_offset_cm: off, ..Default::default() });
        assert_eq!(st.minimap.pos, minimap_uv(sim.x, sim.z, off));
    }
    #[test] fn placed_pois_land_on_their_minimap_labels() {
        // MINIMAP FIX: the Athena_Terrain POI sublevel offsets (UE cm, props/layout/stats.json) through the exe mapping land within ~60 px (of 2048) of the
        // MiniMapAthena.png labels (label text is drawn near, not on, the sublevel origin: these are a sanity bound, not the source of the mapping)
        for (ue, px, what) in [((3584., -8192.), (1110., 1240.), "Salty Springs"), ((45056., -4096.), (1140., 926.), "Dusty Depot"), ((66048., -43008.), (857., 752.), "Loot Lake"),
                               ((-62976., -64512.), (702., 1726.), "Flush Factory"), ((-40960., 57344.), (1636., 1536.), "Moisty Mire")] {
            let (u, v) = minimap_uv_ue(ue.0, ue.1);
            let d = ((u * 2048. - px.0).powi(2) + (v * 2048. - px.1).powi(2)).sqrt();
            assert!(d < 80., "{what}: ({:.0}, {:.0}) px vs label {px:?}", u * 2048., v * 2048.);
        }
    }
    #[test] fn game_state_flows_into_the_hud() {
        let mut sim = GameSim::new();
        sim.world.wood = 123; sim.world.stone = 45; sim.yaw = 270.; sim.x = -256.; sim.z = 256.;
        let st = hud_state(&sim, &Settings::default(), &HudInputs { metal: 9, kills: 3, players_left: Some(57), ..Default::default() });
        assert_eq!(st.resources, [123, 45, 9]);
        assert_eq!((st.kills, st.players_left), (3, Some(57)));
        assert_eq!(st.compass_yaw_deg, 180., "port yaw 270 looks along -x = UE -X = south");
        assert_eq!(st.minimap.pos, minimap_uv_ue(-25600., 25600.), "x = -256 m (south), z = +256 m (east) of the UE origin");
        sim.x = f32::NAN; sim.z = 1e9;
        let st = hud_state(&sim, &Settings::default(), &HudInputs::default());
        assert_eq!(st.minimap.pos, (1., 0.5), "NaN falls back to the centre, far away clamps");
    }
    #[test] fn compass_and_minimap_follow_the_canonical_ue_axes() {
        // SUN-AXIS FIX: looking along +x (UE +X, north) reads N; along +z (UE +Y, east) reads E; north is up on the map, east is right
        let fwd = |yaw: f32| { let (s, c) = yaw.to_radians().sin_cos(); (s, -c) }; // fn_game::horizontal_step forward (x, z)
        let (x, z) = fwd(90.); assert!((x - 1.).abs() < 1e-6 && z.abs() < 1e-6); assert_eq!(ue_yaw_deg(90.), 0.);
        let (x, z) = fwd(180.); assert!(x.abs() < 1e-6 && (z - 1.).abs() < 1e-6); assert_eq!(ue_yaw_deg(180.), 90.);
        let c = minimap_uv(0., 0., [0.; 2]);
        assert!(minimap_uv(100., 0., [0.; 2]).1 < c.1 && minimap_uv(0., 100., [0.; 2]).0 > c.0);
    }
    #[test] fn the_compass_points_at_1_8_north_of_the_minimap_when_looking_between_placed_pois() {
        // SUN-AXIS FIX: ground truth = MiniMapAthena.png (2048 px, north up) label pixels; the POIs are placed in the game with the props' map
        // (fn_propdata::ue_to_game_position) from the Athena_Terrain POI sublevel offsets (UE cm, props/layout/stats.json). Looking from one POI to
        // another, the compass (ue_yaw_deg of the port yaw) must read the bearing the minimap shows between the two labels.
        let poi = |ue_x: f32, ue_y: f32| { let p = fn_propdata::ue_to_game_position([ue_x, ue_y, 0.]); (p[0], p[2]) };
        let (salty, dusty, loot, flush, moisty) = (poi(3584., -8192.), poi(45056., -4096.), poi(66048., -43008.), poi(-62976., -64512.), poi(-40960., 57344.));
        let (salty_px, dusty_px, loot_px, flush_px, moisty_px) = ((1110., 1240.), (1140., 926.), (857., 752.), (702., 1726.), (1636., 1536.));
        let port_yaw = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0).atan2(-(b.1 - a.1)).to_degrees(); // forward = (sin, -cos) in (x, z)
        let map_bearing = |a: (f32, f32), b: (f32, f32)| ((b.0 - a.0).atan2(-(b.1 - a.1)).to_degrees()).rem_euclid(360.); // image x east, y south
        for (a, b, ap, bp, what) in [(salty, dusty, salty_px, dusty_px, "Salty -> Dusty (N)"), (dusty, loot, dusty_px, loot_px, "Dusty -> Loot Lake (NW)"),
                                     (flush, moisty, flush_px, moisty_px, "Flush -> Moisty (E)"), (moisty, loot, moisty_px, loot_px, "Moisty -> Loot Lake (NW)")] {
            let heading = ue_yaw_deg(port_yaw(a, b));
            let d = (heading - map_bearing(ap, bp) + 540.).rem_euclid(360.) - 180.;
            assert!(d.abs() < 12., "{what}: compass {heading:.1} vs minimap bearing {:.1}", map_bearing(ap, bp));
        }
    }
    #[test] fn settings_drive_show_fps_and_the_element_toggles() {
        let sim = GameSim::new();
        let mut s = Settings::default();
        s.set("video.show_fps", fn_game::settings::Value::Bool(true)).unwrap();
        s.set_extra("HUD", "HUD.TopRight.Minimap", "False");
        s.set_extra("HUD", "HUD.Quickbar", "off");
        s.set_extra("HUD", "HUD.TopLeft.PickupManager", "0");
        s.set_extra("HUD", "HUD.Center.Reticle", "True");
        let st = hud_state(&sim, &s, &HudInputs::default());
        assert!(st.show_fps);
        let v = st.visible;
        assert!(!v.minimap && !v.quick_bar && !v.pickup_info);
        assert!(v.reticle && v.player_health && v.target_info && v.interaction_info, "unset and True stay on");
        assert!(hud_flag(&s, "HUD.Unknown") && !hud_flag(&s, "HUD.Quickbar"));
    }
    #[test] fn tags_match_the_settings_screens_hud_tab() {
        assert_eq!(HudVisibility::TAGS.len(), 7);
        let all_off = HudVisibility::from_tags(|_| false);
        assert!(!all_off.reticle && !all_off.player_health && !all_off.pickup_info && !all_off.minimap && !all_off.quick_bar && !all_off.target_info && !all_off.interaction_info);
        for (i, tag) in HudVisibility::TAGS.iter().enumerate() {
            let one = HudVisibility::from_tags(|t| t == *tag);
            let n = [one.reticle, one.player_health, one.pickup_info, one.minimap, one.quick_bar, one.target_info, one.interaction_info];
            assert_eq!(n.iter().filter(|b| **b).count(), 1, "{tag}");
            assert!(n[i], "{tag} is toggle #{i}");
        }
    }
    #[test] fn inventory_rules() {
        let sim = GameSim::new();
        // custom items, selection on an empty slot falls back to a filled one, an emptied bar keeps the pickaxe
        let mut items: [Option<HudItem>; 6] = Default::default();
        items[2] = Some(HudItem::new(3, "T-Icon-Test.png", Some(7)));
        let st = hud_state(&sim, &Settings::default(), &HudInputs { items: Some(items), selected: 0, ..Default::default() });
        assert_eq!(st.selected, 2);
        assert_eq!(st.items[2].as_ref().unwrap().count, Some(7));
        let st = hud_state(&sim, &Settings::default(), &HudInputs { items: Some(Default::default()), selected: 4, ..Default::default() });
        assert_eq!((st.selected, st.items[0].clone()), (0, Some(HudItem::pickaxe())));
    }
    #[test] fn frame_clock_smooths_and_reports_milliseconds() {
        let mut c = FrameClock::default();
        assert_eq!(c.stats(), HudFrameStats::default());
        for _ in 0..600 { c.tick(1. / 120.); }
        let s = c.stats();
        assert!((s.fps - 120.).abs() < 1., "{}", s.fps);
        assert!((s.frame_ms - 1000. / 120.).abs() < 1e-3);
        c.tick(f32::NAN); c.tick(0.); c.tick(-1.);
        assert!(c.stats().fps.is_finite());
    }
    #[test] fn loading_passes_through_and_the_state_draws_through_fn_ui() {
        let sim = GameSim::new();
        let mut inputs = HudInputs::default();
        inputs.loading = Some(HudLoading { done: 3, total: 10, label: "Loading HUD".into(), tip: 0, fade: 0. });
        assert_eq!(hud_state(&sim, &Settings::default(), &inputs).loading.unwrap().done, 3);
        // the contract end to end (skipped without the exported UI assets): produce -> set_state -> present
        let Ok(mut ctx) = fn_ui::scene::UiContext::from_default_assets() else { eprintln!("skipping: no UI assets"); return };
        let mut hud = Hud::new();
        hud.set_state(&hud_state(&sim, &Settings::default(), &HudInputs::default()));
        hud.tick(1. / 60.);
        let mut out = RenderList::new();
        hud.present(&mut ctx, 1920., 1080., &mut out);
        assert!(!out.is_empty());
        let mut out2 = RenderList::new();
        let mut hud2 = Hud::new(); hud2.set_state(&hud_state(&sim, &Settings::default(), &HudInputs::default())); hud2.tick(1. / 60.); hud2.present(&mut ctx, 1920., 1080., &mut out2);
        assert_eq!(out.cmds, out2.cmds, "same state, same draw list");
    }
}
