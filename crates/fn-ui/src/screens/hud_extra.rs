//! Remaining Athena gameplay HUD elements (compass, minimap frame, players-alive / eliminations
//! counters, wood-stone-metal resource counters, backpack row).
//!
//! SOURCES (all recovered from the 1.8 paks; see reports/hud-extra-widgets.json and
//! tools/hud-extra-evidence.cjs):
//! * Placement: AthenaHUD "[Overlay] Persistent HUD Content" slots (padding/alignment, Slate units).
//! * AthenaCompass: SizeBox 768x96, M_Compass samples T_Compass (2048x128 strip) with CompassPan,
//!   centre mark Compass_CenterMark 24x24 at Left=372 Top=8.
//! * AthenaMinimapContainer: Border (padding 5) around a 280x280 SizeBox, background brush
//!   minimap_Art_square 264x264. The map itself is a legacy Slate widget (no baseplate map data), so the
//!   interior is a neutral placeholder.
//! * AthenaPlayersLeftWidget / AthenaKillsWidget: 34x34 AliveIco / KillsIco + text (Kills: NotoSans-Bold 18,
//!   MinDesiredWidth 28). Players-left text default is "100" (designer default in the widget).
//! * AthenaPickupManager: three PackResource widgets (54x80 SizeBox, 64x64 icon ScaleToFitX, count text
//!   TextStyle-Base-XS-B-S = NotoSans-Bold 16, shadow (1.6,1.6) black 0.75). Icons come from the resource item
//!   definitions: WoodItemData -> T-Icon-Logs, StoneItemData -> T-Icon-S-Loot-Stone, MetalItemData -> T-Icon-IBeam.
//!   MaxStackSize 999 each.
//! * TYPEFACE: every recovered 1.8 Athena HUD widget uses the NotoSans font asset (Regular/Bold faces from
//!   FortressArtSource/Fonts/NotoSans), NOT Burbank. The Burbank .otf/.ufont files exist in the paks and were
//!   extracted to assets/fonts, but no inspected Athena gameplay widget references them.
//!
//! APPROXIMATIONS (not verified 1:1): Slate font point size -> pixel mapping (1 px per unit * DPI scale),
//! glyph rasterisation (fontdue, no hinting/kerning/outline material), the gradient/line brush materials
//! (M_Border_HUD_VGradient / M_Border_HUD_Line_Vert are material instances that were not evaluated), the
//! compass edge fade (M_Compass SmoothStep parameters not evaluated). (The DPI curve is no longer an approximation: layout.rs, CookedIniVersion.txt L531-532.)
//!
//! PORT NOTES: the compass / pack background / divider are `MaterialKind::{Compass, PanelVGrad, PanelLine}` commands over their layout rects (the backend bakes them at
//! the rect's pixel size via `material::hud::bake`); the compass pan is quantised to 1/1536 exactly as the old compass re-bake key.
use crate::assets::TexInfo;
use crate::draw::{MaterialKind, RenderList, TextureId};
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::{art, atlas_px, centred_baseline, mat_tex, put_text, text_width, Snap, WHITE};
use crate::text::FontId;

// ---- recovered constants (Slate units) -- reports/hud-1.8-evidence.json (tools/hud18-evidence.cjs) -------------------------------
pub const COMPASS_SIZE: (f32, f32) = (768., 128.); // AthenaCompass SizeBox_124 (1.10: 768x96)
pub const COMPASS_MARK: (f32, f32, f32, f32) = (372., 20., 24., 24.); // Compass_CenterMark: overlay slot Padding Left 372 / Top 20, image 24x24 (1.10: top 8)
pub const COMPASS_PAN_BIAS: f32 = 0.25; // AthenaCompass.UpdateCompassMaterial: CompassPan = yaw/360 + 0.25
pub const MINIMAP_BORDER: f32 = 5.; // AthenaMinimapContainer Border_0 Padding
pub const MINIMAP_INNER: f32 = 280.; // SizeBox_0 around the FortLegacySlateBridgeWidget (AthenaMiniMap)
pub const MINIMAP_OUTER: f32 = MINIMAP_INNER + 2. * MINIMAP_BORDER;
pub const MINIMAP_FRAME_MARGIN: [f32; 4] = [0.5, 0.59734, 0.50442, 0.5]; // Border_0 Background brush margin (minimap_Art_square 264x264)
pub const MINIMAP_MARGIN_RIGHT: f32 = 20.; // slot padding Right of MinimapContainer in InvalidationBoxTopRight
pub const MINIMAP_MARGIN_TOP: f32 = 20.; // InvalidationBoxTopRight slot Padding Top
pub const ROW_TOP: f32 = 320.; // HorizontalBox_2 padding Top
pub const ROW_RIGHT: f32 = 60.; // HorizontalBox_2 padding Right
pub const ROW_GAP: f32 = 10.; // padding Right of the phase / players-left slots
pub const ICON: f32 = 34.;
pub const ALIVE_TEXT_PAD: f32 = 4.; // AthenaPlayersLeftWidget HorizontalBoxSlot_1 Padding.Left
pub const KILLS_TEXT_PAD: f32 = 40.; // AthenaKillsWidget: overlay slot Padding.Left of KillsText (1.10 used a 2-unit gap + MinDesiredWidth 28)
pub const PICKUP_RIGHT: f32 = 40.; // AthenaPickupManager slot padding Right
pub const PICKUP_BOTTOM: f32 = 340.; // AthenaPickupManager slot padding Bottom
pub const PACK_SIZE: (f32, f32) = (54., 80.);
pub const PACK_DIVIDER: f32 = 4.;
pub const PACK_PAD_V: f32 = 4.;
pub const STREAM_PAD: f32 = 8.; // "Pickup Message Stream" slot padding Top (its text is Collapsed)
pub const BACKPACK_ICON: f32 = 40.; // T-Icon-Backpack-64 brush ImageSize
pub const BACKPACK_SHADOW: f32 = 2.; // Image_2 RenderTransform translation (black)
pub const BACKPACK_ROW_GAP: f32 = 6.; // VerticalBoxSlot_93 Padding.Bottom
pub const BACKPACK_DIVIDER: (f32, f32) = (4., 28.); // SizeBox_3
pub const BACKPACK_DIVIDER_PAD: f32 = 8.; // HorizontalBoxSlot_9 Padding Left/Right
pub const BACKPACK_COUNT_PAD: f32 = 8.; // HorizontalBoxSlot_26 Padding.Left
pub const KEYCAP_SMALL: f32 = 24.; // KeybindWidget Brush Size Small min size (see quickbar.rs)
pub const KEYCAP_FONT: f32 = 12.;
pub const MAX_STACK: u32 = 999;
pub const FONT_KILLS: f32 = 18.; // KillsText / PlayersLeftText / TimeText Size
pub const FONT_PACK: f32 = 16.; // TextStyle-Base-XS-B-S Size
pub const SHADOW_OFFSET: (f32, f32) = (1.6, 1.6);
pub const SHADOW_COLOR: [f32; 4] = [0., 0., 0., 0.75];
/// ASSUMED (native `FortInventoryContext.GetBackpackItemCounts`, not serialised): maximum backpack items shown next to the backpack icon.
pub const BACKPACK_MAX_ASSUMED: u32 = 5;
/// INVENTORY FIX2: the backpack row's `KeybindWidget` shows the key bound to `ToggleInventory`; this is the cooked default (`Input:ConfigD_Athena
/// FortPlayerInputSettings:ActionMappings:113=(ActionName="ToggleInventory", Key=I)`, CookedIniVersion.txt line 2740; Tab is `ToggleFullScreenMap`).
pub const BACKPACK_KEY_DEFAULT: &str = "I";

// ---- pure layout ----------------------------------------------------------------------------------------
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub scale: f32, pub compass: Rect, pub compass_mark: Rect, pub minimap: Rect, pub minimap_inner: Rect,
    pub alive_icon: Rect, pub alive_text_x: f32, pub kills_icon: Rect, pub kills_text_x: f32,
    pub pack_panel: Rect, pub packs: [Rect; 3], pub pack_dividers: [Rect; 2],
    /// Backpack row: [keycap][icon][count text x][divider][max text x], all vertically centred on `bp_row`.
    pub bp_row: Rect, pub bp_key: Rect, pub bp_icon: Rect, pub bp_count_x: f32, pub bp_divider: Rect, pub bp_max_x: f32,
}

/// `*_w` are measured text widths in PIXELS (kills: 40 + text; players-left: icon + 4 + text; backpack row: key text, count text, max text).
pub fn layout(w: f32, h: f32, alive_text_w: f32, kills_text_w: f32, key_text_w: f32, count_w: f32, max_w: f32) -> Layout { layout_with_stream(w, h, alive_text_w, kills_text_w, key_text_w, count_w, max_w, 0.) }
pub fn layout_with_stream(w: f32, h: f32, alive_text_w: f32, kills_text_w: f32, key_text_w: f32, count_w: f32, max_w: f32, stream_h: f32) -> Layout {
    let s = dpi_scale(w, h);
    let compass = Rect::new((w - COMPASS_SIZE.0 * s) * 0.5, 0., COMPASS_SIZE.0 * s, COMPASS_SIZE.1 * s);
    let compass_mark = Rect::new(compass.x + COMPASS_MARK.0 * s, compass.y + COMPASS_MARK.1 * s, COMPASS_MARK.2 * s, COMPASS_MARK.3 * s);
    let minimap = Rect::new(w - (MINIMAP_MARGIN_RIGHT + MINIMAP_OUTER) * s, MINIMAP_MARGIN_TOP * s, MINIMAP_OUTER * s, MINIMAP_OUTER * s);
    let minimap_inner = Rect::new(minimap.x + MINIMAP_BORDER * s, minimap.y + MINIMAP_BORDER * s, MINIMAP_INNER * s, MINIMAP_INNER * s);
    // Row (right aligned, padding Right 60): [phase (not drawn)] [PlayersLeft (padding right 10)] [Kills]. Kills = Overlay(icon 34 | text at Left 40).
    let kills_w = (KILLS_TEXT_PAD * s + kills_text_w).max(ICON * s);
    let kills_x = w - ROW_RIGHT * s - kills_w;
    let alive_w = ICON * s + ALIVE_TEXT_PAD * s + alive_text_w;
    let alive_x = kills_x - ROW_GAP * s - alive_w;
    let y = ROW_TOP * s;
    let alive_icon = Rect::new(alive_x, y, ICON * s, ICON * s);
    let kills_icon = Rect::new(kills_x, y, ICON * s, ICON * s);
    // Pickup manager (right 40 / bottom 340, HAlign right, VAlign bottom): VerticalBox [backpack row (+6)] [packs panel] [stream (+8, empty)].
    let panel_w = 3. * PACK_SIZE.0 + 2. * PACK_DIVIDER;
    let panel_h = PACK_SIZE.1 + 2. * PACK_PAD_V;
    let right = w - PICKUP_RIGHT * s;
    let bottom = h - (PICKUP_BOTTOM + STREAM_PAD + stream_h) * s;
    let pack_panel = Rect::new(right - panel_w * s, bottom - panel_h * s, panel_w * s, panel_h * s);
    let mut packs = [Rect::ZERO; 3];
    for (i, p) in packs.iter_mut().enumerate() { *p = Rect::new(pack_panel.x + i as f32 * (PACK_SIZE.0 + PACK_DIVIDER) * s, pack_panel.y + PACK_PAD_V * s, PACK_SIZE.0 * s, PACK_SIZE.1 * s); }
    let mut pack_dividers = [Rect::ZERO; 2];
    for (i, d) in pack_dividers.iter_mut().enumerate() { *d = Rect::new(pack_panel.x + (i as f32 * (PACK_SIZE.0 + PACK_DIVIDER) + PACK_SIZE.0) * s, pack_panel.y, PACK_DIVIDER * s, pack_panel.h); }
    let row_h = BACKPACK_ICON * s;
    let key_w = (KEYCAP_SMALL * s).max(key_text_w + 2. * 0.1 * KEYCAP_SMALL * s);
    let row_w = key_w + BACKPACK_ICON * s + BACKPACK_COUNT_PAD * s + count_w + (BACKPACK_DIVIDER_PAD * 2. + BACKPACK_DIVIDER.0) * s + max_w;
    let bp_row = Rect::new(right - row_w, pack_panel.y - BACKPACK_ROW_GAP * s - row_h, row_w, row_h);
    let cy = bp_row.y + row_h * 0.5;
    let bp_key = Rect::new(bp_row.x, cy - KEYCAP_SMALL * s * 0.5, key_w, KEYCAP_SMALL * s);
    let bp_icon = Rect::new(bp_key.x + key_w, bp_row.y, BACKPACK_ICON * s, BACKPACK_ICON * s);
    let bp_count_x = bp_icon.x + bp_icon.w + BACKPACK_COUNT_PAD * s;
    let bp_divider = Rect::new(bp_count_x + count_w + BACKPACK_DIVIDER_PAD * s, cy - BACKPACK_DIVIDER.1 * s * 0.5, BACKPACK_DIVIDER.0 * s, BACKPACK_DIVIDER.1 * s);
    let bp_max_x = bp_divider.x + bp_divider.w + BACKPACK_DIVIDER_PAD * s;
    Layout { scale: s, compass, compass_mark, minimap, minimap_inner, alive_icon, alive_text_x: alive_icon.x + (ICON + ALIVE_TEXT_PAD) * s, kills_icon, kills_text_x: kills_icon.x + KILLS_TEXT_PAD * s, pack_panel, packs, pack_dividers, bp_row, bp_key, bp_icon, bp_count_x, bp_divider, bp_max_x }
}

/// CompassPan for a view yaw in degrees: AthenaCompass.UpdateCompassMaterial = yaw/360 + 0.25 (no wrap: the T_Compass sampler wraps).
pub fn compass_pan(yaw_deg: f32) -> f32 { yaw_deg / 360. + COMPASS_PAN_BIAS }
/// The pan the compass material is baked at: quantised to one texel of the 1536-wide strip (the old re-bake key).
pub fn quantised_pan(pan: f32) -> f32 { (pan * 1536.).round() / 1536. }

/// Resource count text as Slate shows it, clamped to the item's MaxStackSize.
pub fn clamp_resource(n: u32) -> u32 { n.min(MAX_STACK) }

// ---- state ---------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct HudExtraState {
    /// Players alive. None = no match data; shows the widget's original designer default ("100").
    pub players_alive: Option<u32>,
    pub kills: u32,
    /// Wood, Stone, Metal (all 0 until gathering exists).
    pub resources: [u32; 3],
    pub heading_override: Option<f32>,
    /// Backpack (items, max) from FortInventoryContext (native). Default: nothing in the backpack; max is ASSUMED (`BACKPACK_MAX_ASSUMED`).
    pub backpack: (u32, u32),
    /// INVENTORY FIX2: key text of the backpack row (the `ToggleInventory` binding; the host fills it).
    pub backpack_key: String,
    /// Minimap view centre in map-texture UV (0..1) and the fraction of the map texture shown across the 280-unit box. APPROXIMATE (native SFortMiniMap).
    pub map_centre: (f32, f32),
    /// Minimap player arrow rotation in degrees; None = the view yaw (the default: the arrow shows where the camera looks).
    pub arrow_deg: Option<f32>,
    /// HUD element toggles (settings HUD.TopRight.Minimap / HUD.TopLeft.PickupManager); the compass and the counters are always shown.
    pub show_minimap: bool, pub show_pickups: bool,
    /// Index (0 wood, 1 stone, 2 metal) of `FortInventoryContext.GetCurrentResourceItemDefinition()`: its pack uses the Highlighted border (`PackResource.UpdateCount`:
    /// `Border_HUD_ResourceBg_Highlighted` = `M_Border_LightNavy_VGrad`). ASSUMED wood (the native building-material default is not serialised).
    pub current_resource: usize,
    pub storm_map: Option<StormMap>,
    pub bus: Option<MapIcon>,
    pub teammates: Vec<TeamMapIcon>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapIcon { pub uv: (f32, f32), pub yaw_deg: f32 }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TeamMapIcon { pub uv: (f32, f32), pub yaw_deg: f32, pub tint: [f32; 3], pub dbno: bool }
pub const TEAM_ICON_SIZE: f32 = 20.;
pub const BUS_ICON_TEXTURE: &str = "T_AthenaBus_Top.png";
pub const BUS_ICON_IMAGE_SIZE: f32 = 256.;
pub const BUS_ICON_SCALE: f32 = 0.5;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StormMap { pub current: [f32; 3], pub next: [f32; 3] }
impl Default for HudExtraState { fn default() -> Self { Self { players_alive: None, kills: 0, resources: [0; 3], heading_override: None, backpack: (0, BACKPACK_MAX_ASSUMED), backpack_key: BACKPACK_KEY_DEFAULT.into(), map_centre: (0.5, 0.5), arrow_deg: None, show_minimap: true, show_pickups: true, current_resource: 0, storm_map: None, bus: None, teammates: vec![] } } }
pub fn team_tint(c: [f32; 3]) -> [f32; 4] { let t = crate::screens::srgb3(c); [t[0], t[1], t[2], 1.] }
pub const PLAYERS_LEFT_DEFAULT: &str = "100";
/// MINIMAP FIX (2026-10-05): `AFortInGameMapManager::MapLayerSize` of the Athena map manager = the side of the map in map units (one unit =
/// `FortWorldSettings.MapWorldScale` cm). `AFortGameStateAthena::BeginPlay` (exe 0x14041d400) spawns the NATIVE `AFortInGameMapManagerAthena` (no BP), so the
/// value is the `AFortInGameMapManager` ctor default (exe 0x1405f60c0: `*(this + 0x3d0) = 0x380`); the minimap paint reads it through `FUN_14060c8c0`.
pub const MAP_LAYER_SIZE: f32 = 896.;
pub const STORM_RING_THICKNESS: f32 = 4.;
pub const STORM_RING_SEGMENTS: usize = 100;
pub const STORM_CURRENT_RING_COLOR: [f32; 4] = [0., 0., 1., 1.];
pub const STORM_NEXT_RING_COLOR: [f32; 4] = [1., 1., 1., 1.];
pub const PLAYER_ARROW_TEXTURE: &str = "UI_Radar_AthenaPlayerArrow.png";
/// `AFortWorldSettings::MiniMapZoom` (Slate units per map unit at zoom 1): `AFortWorldSettings` ctor default 2.5 (exe 0x1406535b0: `+0x684 = 0x40200000`),
/// not overridden by `Athena_Terrain`'s FortWorldSettings (its tagged properties: MapWorldScale 290, MapZOffset 42000, AthenaMapImage, ...).
pub const MINIMAP_ZOOM: f32 = 2.5;
pub const FULL_MAP_PADDING: f32 = 20.;
pub const FULL_MAP_BACKING: [f32; 4] = [0., 0., 0., 0.6];
/// `SFortMiniMapAthena` zoom attribute: FArguments default 1.0 (exe `FUN_142e630b0`: `+0x290 = 1.0`), not set by `UFortLegacySlateBridgeWidget` (exe `FUN_142e72620`),
/// clamped to [0.1, 2] in the paint (exe `FUN_1430b6a40`). The bridge's `DPIScaleFactor` is its ctor default 1.0 (exe 0x142e67da0), so no extra scale.
pub const MINIMAP_WIDGET_ZOOM: f32 = 1.;
/// Fraction of the map texture shown across the minimap box. MINIMAP FIX: from the exe paint (`FUN_1430b6a40`): the whole map (MapLayerSize units) is drawn
/// `MapLayerSize * clamp(zoom, 0.1, 2) * MiniMapZoom` Slate units wide and offset so the player's map point sits at the widget centre, i.e. the
/// `MINIMAP_INNER` (280) box shows 280 / (896 * 2.5) = 0.125 of the texture. Was 0.33 (a guess).
pub const MAP_VIEW_FRACTION: f32 = MINIMAP_INNER / (MAP_LAYER_SIZE * MINIMAP_WIDGET_ZOOM * MINIMAP_ZOOM);

/// Test-capture variants (old `hud_extra::set_test_variant`, used by `--hud-extra-test`; values are TEST inputs, not game data).
pub fn test_variant(v: u8) -> HudExtraState {
    match v {
        1 => HudExtraState { players_alive: Some(57), kills: 3, resources: [123, 45, 999], heading_override: Some(90.), backpack: (3, BACKPACK_MAX_ASSUMED), map_centre: (0.52, 0.43), ..Default::default() },
        2 => HudExtraState { players_alive: Some(7), kills: 12, resources: [1000, 0, 7], heading_override: Some(225.), backpack: (5, BACKPACK_MAX_ASSUMED), map_centre: (0.40, 0.62), ..Default::default() },
        3 => HudExtraState { players_alive: Some(100), kills: 0, resources: [0; 3], heading_override: Some(350.), ..Default::default() },
        _ => HudExtraState::default(),
    }
}

#[derive(Clone, Copy)]
struct Tex {
    alive: TextureId, kills: TextureId, compass_mark: TextureId, minimap: TexInfo, map: TextureId, arrow: TextureId,
    wood: TextureId, stone: TextureId, metal: TextureId, backpack: TextureId, keycap: TexInfo,
    compass_strip: TextureId, compass_feather: TextureId, line_mask: TextureId,
}

/// `PackResource.UpdateCount`: `NumericTextCount.InterpolateToValue(ResourceCount, 1, 0.5, 0)` (old `reports/hud-bytecode/PackResource.txt`). The roles of the first and last
/// arguments (exponent / delay) are ASSUMED; the 0.5 s duration and the target are bytecode literals.
pub const COUNT_INTERP_SECS: f32 = 0.5;
/// `PackResource.UpdateCount`: count text alpha 1.0 while ResourceCount < MaxStackSize, 0.6 at the cap (Select between the literals 1 and 0.6).
pub const COUNT_ALPHA_AT_CAP: f32 = 0.6;
/// Linear count-up of the three pack numbers (see [`COUNT_INTERP_SECS`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CountAnim { from: [f32; 3], to: [u32; 3], t: [f32; 3], started: bool }
impl Default for CountAnim { fn default() -> Self { Self { from: [0.; 3], to: [0; 3], t: [COUNT_INTERP_SECS; 3], started: false } } }
impl CountAnim {
    /// Advance by `dt`; a changed target restarts the interpolation from the number currently shown. The first call adopts the target without animating.
    pub fn step(&mut self, target: [u32; 3], dt: f32) {
        if !self.started { self.started = true; self.to = target; self.from = target.map(|v| v as f32); self.t = [COUNT_INTERP_SECS; 3]; return; }
        for i in 0..3 { if target[i] != self.to[i] { self.from[i] = self.value(i, target); self.to[i] = target[i]; self.t[i] = 0.; } self.t[i] = (self.t[i] + dt.max(0.)).min(COUNT_INTERP_SECS); }
    }
    /// Number shown for pack `i` (before any `step` call: the target itself, so captures that never tick show the state as is).
    pub fn value(&self, i: usize, target: [u32; 3]) -> f32 {
        if !self.started { return target[i] as f32; }
        let k = (self.t[i] / COUNT_INTERP_SECS).clamp(0., 1.); self.from[i] + (self.to[i] as f32 - self.from[i]) * k
    }
    pub fn shown(&self, i: usize, target: [u32; 3]) -> u32 { self.value(i, target).round().max(0.) as u32 }
}

#[derive(Clone, Default)]
pub struct HudExtra { pub state: HudExtraState, pub count: CountAnim, tex: Option<Tex>, pub alive_row: Option<Rect>, pub stream: (f32, f32), pub stream_anchor: Option<(f32, f32)> }

impl HudExtra {
    pub fn new() -> Self { Self::default() }
    pub fn with_state(state: HudExtraState) -> Self { Self { state, count: CountAnim::default(), tex: None, alive_row: None, stream: (0., 0.), stream_anchor: None } }
    /// Game-time step: the pack numbers count up to the new value over [`COUNT_INTERP_SECS`] (not calling it shows the numbers as they are).
    pub fn tick(&mut self, dt: f32) { self.count.step(self.state.resources, dt); }

    pub fn prewarm(&mut self, ctx: &mut UiContext) -> TextureId { self.ensure_tex(ctx).map }
    pub fn kept_textures(&self) -> Vec<TextureId> { self.tex.map(|t| vec![t.map, t.minimap.id]).unwrap_or_default() }
    fn ensure_tex(&mut self, ctx: &mut UiContext) -> Tex {
        if let Some(t) = self.tex {
            let live = |id: TextureId| id == TextureId::NONE || ctx.assets.store.get(id).is_some();
            if [t.alive, t.kills, t.compass_mark, t.minimap.id, t.map, t.arrow, t.wood, t.stone, t.metal, t.backpack, t.keycap.id, t.compass_strip, t.compass_feather, t.line_mask].into_iter().all(live) { return t; }
        }
        // optional: a missing icon is skipped (TextureId::NONE quads are dropped by the RenderList), not fatal
        let t = Tex {
            alive: art(ctx, "AliveIco.png").id, kills: art(ctx, "KillsIco.png").id, compass_mark: art(ctx, "Compass_CenterMark.png").id, minimap: art(ctx, "minimap_Art_square.png"),
            map: art(ctx, "MiniMapAthena.png").id, arrow: art(ctx, PLAYER_ARROW_TEXTURE).id,
            wood: art(ctx, "T-Icon-Logs.png").id, stone: art(ctx, "T-Icon-S-Loot-Stone.png").id, metal: art(ctx, "T-Icon-IBeam.png").id, backpack: art(ctx, "T-Icon-Backpack-64.png").id,
            keycap: art(ctx, "Keybind-Frame-S.png"),
            compass_strip: mat_tex(ctx, "T_Compass.png", true).id, compass_feather: mat_tex(ctx, "T_CompassFeather.png", false).id, line_mask: mat_tex(ctx, "T-Border-HUD-Line-Vert.png", false).id,
        };
        self.tex = Some(t); t
    }

    /// One text line (old `HudExtra::text`): glyph origin x, vertically centred in (`top`, `box_h`), optional shadow pass first.
    #[allow(clippy::too_many_arguments)]
    fn text(ctx: &mut UiContext, out: &mut RenderList, px: u32, x: f32, top: f32, box_h: f32, text: &str, color: [f32; 4], shadow: Option<([f32; 2], [f32; 4])>) {
        let base = centred_baseline(ctx, FontId::Bold, px, top, box_h);
        put_text(ctx, out, FontId::Bold, px, x, base, text, color, shadow, Snap::PerGlyph);
    }

    /// Append the compass, minimap, counters and pickup manager. `yaw_deg` = camera yaw (ignored when `state.heading_override` is set).
    pub fn build_full_map(&mut self, ctx: &mut UiContext, w: f32, h: f32, yaw_deg: f32, out: &mut RenderList) {
        let tex = self.ensure_tex(ctx);
        let s = dpi_scale(w, h);
        let (side, pad) = (MAP_LAYER_SIZE * s, FULL_MAP_PADDING * s);
        let outer = Rect::new(((w - side) * 0.5 - pad).round(), ((h - side) * 0.5 - pad).round(), side + 2. * pad, side + 2. * pad);
        out.fill(outer, FULL_MAP_BACKING);
        let r = Rect::new(outer.x + pad, outer.y + pad, side, side);
        match self.state.storm_map { Some(sm) => out.material(MaterialKind::MinimapStorms { map: tex.map, current: sm.current }, r, WHITE, false), None => out.quad(tex.map, r, [0., 0., 1., 1.], WHITE) }
        if let Some(sm) = self.state.storm_map { out.push_clip(r); Self::storm_rings(out, &sm, self.state.map_centre, |u, v| (r.x + u * r.w, r.y + v * r.h), r.w, s); out.pop_clip(); }
        if let Some(b) = self.state.bus { out.push_clip(r); Self::bus_icon(ctx, out, (r.x + r.w * b.uv.0, r.y + r.h * b.uv.1), b.yaw_deg, s); out.pop_clip(); }
        let c = self.state.map_centre; let a = 20. * s;
        out.push_clip(r);
        for t in &self.state.teammates { let d = TEAM_ICON_SIZE * s; out.rotated(tex.arrow, (r.x + r.w * t.uv.0, r.y + r.h * t.uv.1), (d, d), t.yaw_deg.to_radians(), team_tint(t.tint)); }
        out.pop_clip();
        out.rotated(tex.arrow, (r.x + r.w * c.0.clamp(0., 1.), r.y + r.h * c.1.clamp(0., 1.)), (a, a), self.state.arrow_deg.unwrap_or(yaw_deg).to_radians(), WHITE);
    }
    fn segment(out: &mut RenderList, a: (f32, f32), b: (f32, f32), thickness: f32, color: [f32; 4]) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 0. { return; }
        out.rotated(TextureId::WHITE, ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5), (len + thickness, thickness), dy.atan2(dx), color);
    }
    fn ring(out: &mut RenderList, c: (f32, f32), r: f32, thickness: f32, color: [f32; 4]) {
        let pt = |i: usize| { let a = i as f32 * std::f32::consts::TAU / STORM_RING_SEGMENTS as f32; (c.0 + a.cos() * r, c.1 + a.sin() * r) };
        for i in 0..STORM_RING_SEGMENTS { Self::segment(out, pt(i), pt(i + 1), thickness, color); }
    }
    fn line_to_circle(out: &mut RenderList, c: (f32, f32), r: f32, player: (f32, f32), thickness: f32, color: [f32; 4]) {
        let (dx, dy) = (c.0 - player.0, c.1 - player.1);
        let dist = (dx * dx + dy * dy).sqrt();
        if dist <= r { return; }
        let (nx, ny) = if dist * dist <= 1e-8 { (0., 0.) } else { (dx / dist, dy / dist) };
        let edge = (player.0 + nx * (dist - r), player.1 + ny * (dist - r));
        Self::segment(out, edge, player, thickness, color);
    }
    pub fn storm_rings(out: &mut RenderList, sm: &StormMap, player_uv: (f32, f32), to_px: impl Fn(f32, f32) -> (f32, f32), px_per_uv: f32, s: f32) {
        let t = STORM_RING_THICKNESS * s;
        let (cur, next) = (to_px(sm.current[0], sm.current[1]), to_px(sm.next[0], sm.next[1]));
        let (rc, rn) = (sm.current[2] * px_per_uv, sm.next[2] * px_per_uv);
        let p = to_px(player_uv.0, player_uv.1);
        Self::ring(out, cur, rc, t, STORM_CURRENT_RING_COLOR);
        Self::ring(out, next, rn, t, STORM_NEXT_RING_COLOR);
        Self::line_to_circle(out, next, rn, p, t, STORM_NEXT_RING_COLOR);
        Self::line_to_circle(out, cur, rc, p, t, STORM_CURRENT_RING_COLOR);
    }
    fn bus_icon(ctx: &mut UiContext, out: &mut RenderList, at: (f32, f32), yaw_deg: f32, s: f32) {
        let t = art(ctx, BUS_ICON_TEXTURE).id;
        let d = BUS_ICON_IMAGE_SIZE * BUS_ICON_SCALE * s;
        out.rotated(t, at, (d, d), yaw_deg.to_radians(), WHITE);
    }
    pub fn draw_minimap_interior(&mut self, ctx: &mut UiContext, r: Rect, yaw_deg: f32, s: f32, out: &mut RenderList) {
        let tex = self.ensure_tex(ctx);
        let st = self.state.clone();
        let heading = st.heading_override.unwrap_or(yaw_deg);
        Self::minimap_interior(ctx, &tex, &st, r, heading, s, out);
    }
    fn minimap_interior(ctx: &mut UiContext, tex: &Tex, st: &HudExtraState, r: Rect, heading: f32, s: f32, out: &mut RenderList) {
        let c = st.map_centre; let f = MAP_VIEW_FRACTION * 0.5;
        // MINIMAP FIX: the exe draws the whole map offset by the player's point and clips it to the widget, so near the map edge the texture
        // stops short of the box (nothing is drawn past it) instead of being squeezed: crop the box with the uv window.
        let (u0, v0, u1, v1) = (c.0 - f, c.1 - f, c.0 + f, c.1 + f);
        let (cu0, cv0, cu1, cv1) = (u0.max(0.), v0.max(0.), u1.min(1.), v1.min(1.));
        let (sx, sy) = (r.w / (u1 - u0), r.h / (v1 - v0));
        let vis = Rect::new(r.x + (cu0 - u0) * sx, r.y + (cv0 - v0) * sy, (cu1 - cu0).max(0.) * sx, (cv1 - cv0).max(0.) * sy);
        match st.storm_map {
            Some(sm) => { out.push_clip(r); out.material(MaterialKind::MinimapStorms { map: tex.map, current: sm.current }, Rect::new(r.x - u0 * sx, r.y - v0 * sy, sx, sy), WHITE, false); Self::storm_rings(out, &sm, c, |u, v| (r.x + (u - u0) * sx, r.y + (v - v0) * sy), sx, s); out.pop_clip(); }
            None => out.quad(tex.map, vis, [cu0, cv0, cu1, cv1], WHITE),
        }
        if let Some(b) = st.bus { out.push_clip(r); Self::bus_icon(ctx, out, (r.x + (b.uv.0 - u0) * sx, r.y + (b.uv.1 - v0) * sy), b.yaw_deg, s); out.pop_clip(); }
        out.push_clip(r);
        for t in &st.teammates { let d = TEAM_ICON_SIZE * s; out.rotated(tex.arrow, (r.x + (t.uv.0 - u0) * sx, r.y + (t.uv.1 - v0) * sy), (d, d), t.yaw_deg.to_radians(), team_tint(t.tint)); }
        out.pop_clip();
        let a = 20. * s; let centre = (r.x + r.w * 0.5, r.y + r.h * 0.5);
        out.rotated(tex.arrow, centre, (a, a), st.arrow_deg.unwrap_or(heading).to_radians(), WHITE);
    }
    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, yaw_deg: f32, out: &mut RenderList) {
        let tex = self.ensure_tex(ctx);
        let st = self.state.clone();
        let s = dpi_scale(w, h);
        let alive_txt = st.players_alive.map(|n| n.to_string()).unwrap_or_else(|| PLAYERS_LEFT_DEFAULT.to_string());
        let kills_txt = st.kills.to_string();
        let kpx = atlas_px(FONT_KILLS, s);
        let ppx = atlas_px(FONT_PACK, s);
        let kcpx = atlas_px(KEYCAP_FONT, s);
        let key_icon = crate::screens::target_info::key_icon(&st.backpack_key);
        let key_owned = if key_icon.is_some() { String::new() } else { crate::screens::target_info::hud_key_text(&st.backpack_key) };
        let (count_txt, max_txt, key_txt) = (st.backpack.0.to_string(), st.backpack.1.to_string(), key_owned.as_str()); // INVENTORY FIX2: the bound ToggleInventory key, not a hardcoded "TAB"
        let tw = |ctx: &UiContext, t: &str, px: u32| text_width(ctx, FontId::Bold, t, px);
        let l = layout_with_stream(w, h, tw(ctx, &alive_txt, kpx), tw(ctx, &kills_txt, kpx), tw(ctx, key_txt, kcpx), tw(ctx, &count_txt, ppx), tw(ctx, &max_txt, ppx), self.stream.0);
        self.alive_row = Some(l.alive_icon);
        let right = l.pack_panel.x + l.pack_panel.w;
        let box_w = (l.pack_panel.w).max(l.bp_row.w).max(self.stream.1 * s);
        self.stream_anchor = if st.show_pickups { Some((right - box_w, l.pack_panel.y + l.pack_panel.h + STREAM_PAD * s)) } else { None };
        let shadow = Some(([SHADOW_OFFSET.0 * s, SHADOW_OFFSET.1 * s], SHADOW_COLOR));
        // Compass (AthenaCompass): centre mark first (Overlay_2 slot order), then the M_Compass image (translated pixel shader, baked at pixel size).
        let heading = st.heading_override.unwrap_or(yaw_deg);
        out.sprite(tex.compass_mark, l.compass_mark, WHITE);
        if tex.compass_strip != TextureId::NONE && tex.compass_feather != TextureId::NONE {
            out.material(MaterialKind::Compass { pan: quantised_pan(compass_pan(heading)), strip: tex.compass_strip, feather: tex.compass_feather }, l.compass, WHITE, false);
        }
        // Minimap (AthenaMinimapContainer): Border background (minimap_Art_square, Box margins from the widget), then the native map widget interior.
        if st.show_minimap { out.nine_slice(tex.minimap, l.minimap, MINIMAP_FRAME_MARGIN, s, WHITE); }
        // Native SFortMiniMapAthena interior: the 1.8 island texture, `MAP_VIEW_FRACTION` of it across the box, centred on `map_centre`, north up;
        // the local player arrow (UI_Radar_PlayerArrow) at the centre rotated by the view yaw (or `arrow_deg`).
        if st.show_minimap { Self::minimap_interior(ctx, &tex, &st, l.minimap_inner, heading, s, out); }
        // Players alive / eliminations (text top-aligned: default slot alignment is Fill and STextBlock paints from the top of its geometry).
        let line_h = crate::screens::line_height(ctx, FontId::Bold, kpx).max(1.);
        out.sprite(tex.alive, l.alive_icon, WHITE);
        out.sprite(tex.kills, l.kills_icon, WHITE);
        Self::text(ctx, out, kpx, l.alive_text_x, l.alive_icon.y, line_h, &alive_txt, WHITE, None);
        Self::text(ctx, out, kpx, l.kills_text_x, l.kills_icon.y, line_h, &kills_txt, WHITE, None);
        // Pickup manager: backpack row, then Wood | Stone | Metal packs with Border_HUD_ResourceBg backgrounds and Border_HUD_Line_Vert dividers.
        if !st.show_pickups { return; }
        let keycap_h = KEYCAP_SMALL * s;
        if let Some(icon) = key_icon { crate::screens::target_info::draw_key_icon(ctx, out, icon, l.bp_key, s); } else {
        out.nine_slice(tex.keycap, l.bp_key, [0.25; 4], s, [1., 1., 1., 0.6]);
        let kw = tw(ctx, key_txt, kcpx);
        Self::text(ctx, out, kcpx, l.bp_key.x + (l.bp_key.w - kw) * 0.5, l.bp_key.y, keycap_h, key_txt, WHITE, None);
        }
        out.sprite(tex.backpack, Rect::new(l.bp_icon.x + BACKPACK_SHADOW * s, l.bp_icon.y + BACKPACK_SHADOW * s, l.bp_icon.w, l.bp_icon.h), [0., 0., 0., 1.]);
        out.sprite(tex.backpack, l.bp_icon, WHITE);
        // BackpackColor is white while BackpackFullPct < 0.6 (RefreshCounts bytecode); the fuller-backpack theme colours are not applied.
        Self::text(ctx, out, ppx, l.bp_count_x, l.bp_row.y, l.bp_row.h, &count_txt, WHITE, shadow);
        let line = MaterialKind::PanelLine { mask: tex.line_mask, global_alpha: 0.2 };
        if tex.line_mask != TextureId::NONE { out.material(line.clone(), l.bp_divider, WHITE, false); }
        Self::text(ctx, out, ppx, l.bp_max_x, l.bp_row.y, l.bp_row.h, &max_txt, WHITE, shadow);
        for (i, icon) in [tex.wood, tex.stone, tex.metal].into_iter().enumerate() {
            let p = l.packs[i];
            // PackResource.UpdateCount: Border_HUD_ResourceBg_Highlighted (M_Border_LightNavy_VGrad) for the current resource, else Border_HUD_ResourceBg.
            out.material(if i == st.current_resource { MaterialKind::PanelVGradHighlighted } else { MaterialKind::PanelVGrad }, p, WHITE, false);
            out.sprite(icon, Rect::new(p.x, p.y, p.w, p.w), WHITE); // 64x64 icon ScaleToFitX -> 54x54, VAlign top
            let txt = clamp_resource(self.count.shown(i, st.resources)).to_string(); // NumericTextCount interpolates to the new value over 0.5 s
            let twd = tw(ctx, &txt, ppx);
            let lh = crate::screens::line_height(ctx, FontId::Bold, ppx);
            let a = if st.resources[i] >= MAX_STACK { COUNT_ALPHA_AT_CAP } else { 1. }; // alpha 0.6 at MaxStackSize
            let sh = shadow.map(|(o, c)| (o, [c[0], c[1], c[2], c[3] * a]));
            Self::text(ctx, out, ppx, p.x + (p.w - twd) * 0.5, p.y + p.h - lh, lh, &txt, [1., 1., 1., a], sh);
            if i < 2 && tex.line_mask != TextureId::NONE { out.material(line.clone(), l.pack_dividers[i], WHITE, false); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Cmd;
    use crate::screens::testutil::{ctx, evidence, testdata};
    fn f(v: &serde_json::Value) -> f32 { v.as_f64().unwrap() as f32 }
    #[test] fn constants_match_1_8_widget_evidence() {
        let e = evidence("hud-1.8-evidence.json");
        let c = &e["compass"];
        assert_eq!((f(&c["size"][0]), f(&c["size"][1])), COMPASS_SIZE, "1.8 compass SizeBox is 768x128");
        assert_eq!(f(&c["centre_mark"]["slot_padding"][0]), COMPASS_MARK.0); assert_eq!(f(&c["centre_mark"]["slot_padding"][1]), COMPASS_MARK.1);
        assert_eq!(c["pan_formula"], "yaw/360 + 0.25"); assert_eq!(f(&c["textures"]["strip"][0]), 1536., "T_Compass is 1536x128 in 1.8");
        let m = &e["minimap"];
        assert_eq!(f(&m["border_padding"][0]), MINIMAP_BORDER); assert_eq!(f(&m["interior_size"][0]), MINIMAP_INNER);
        for k in 0..4 { assert!((f(&m["frame_margin"][k]) - MINIMAP_FRAME_MARGIN[k]).abs() < 1e-4); }
        let cn = &e["counters"];
        assert_eq!(f(&cn["players_left"]["icon"][0]), ICON); assert_eq!(f(&cn["players_left"]["text_padding_left"]), ALIVE_TEXT_PAD); assert_eq!(f(&cn["kills"]["text_padding_left"]), KILLS_TEXT_PAD);
        assert_eq!(f(&cn["kills"]["size"]), FONT_KILLS); assert_eq!(cn["players_left"]["designer_text"], PLAYERS_LEFT_DEFAULT);
        assert_eq!(f(&cn["row_slot"]["Padding"][1]), ROW_TOP); assert_eq!(f(&cn["row_slot"]["Padding"][2]), ROW_RIGHT);
        let p = &e["pickup"];
        assert_eq!(f(&p["manager_slot"]["Padding"][2]), PICKUP_RIGHT); assert_eq!(f(&p["manager_slot"]["Padding"][3]), PICKUP_BOTTOM);
        assert_eq!((f(&p["pack"]["size"][0]), f(&p["pack"]["size"][1])), PACK_SIZE); assert_eq!(f(&p["pack"]["divider_width"]), PACK_DIVIDER);
        assert_eq!(f(&p["pack"]["row_slot_padding"][1]), PACK_PAD_V); assert_eq!(f(&p["stream_padding_top"]), STREAM_PAD);
        let b = &p["backpack_row"];
        assert_eq!(f(&b["icon_image_size"][0]), BACKPACK_ICON); assert_eq!(f(&b["row_bottom_padding"]), BACKPACK_ROW_GAP); assert_eq!(f(&b["divider"][1]), BACKPACK_DIVIDER.1);
        assert_eq!(f(&b["count_padding_left"]), BACKPACK_COUNT_PAD); assert_eq!(f(&b["divider_padding"][0]), BACKPACK_DIVIDER_PAD);
        assert_eq!(e["removedIn18"].as_array().unwrap().iter().filter(|x| x["inPaks"] == true).count(), 0, "AthenaHUDBottomBar/AthenaFPS/AthenaHUDMenu do not exist in 1.8");
    }
    #[test] fn compass_pan_follows_update_compass_material() {
        assert!((compass_pan(0.) - 0.25).abs() < 1e-6 && (compass_pan(90.) - 0.5).abs() < 1e-6 && (compass_pan(-90.) - 0.).abs() < 1e-6);
        assert!((compass_pan(360.) - compass_pan(0.) - 1.).abs() < 1e-5, "no wrap in the pan value: the sampler wraps");
        assert!((quantised_pan(compass_pan(90.)) - 0.5).abs() < 1e-6 && (quantised_pan(0.2501) - 0.25).abs() < 1.0 / 1536.);
    }
    #[test] fn layout_matches_recovered_slots_at_1080p() {
        let l = layout(1920., 1080., 60., 20., 24., 8., 8.);
        assert_eq!(l.scale, 1.);
        assert_eq!(l.compass, Rect::new(576., 0., 768., 128.));
        assert_eq!(l.compass_mark, Rect::new(948., 20., 24., 24.)); // centred: 960-12
        assert_eq!(l.minimap, Rect::new(1920. - 310., 20., 290., 290.));
        assert_eq!(l.minimap_inner, Rect::new(1920. - 305., 25., 280., 280.));
        assert!(l.minimap.y + l.minimap.h <= ROW_TOP, "row sits below minimap (310 < 320)");
        assert_eq!(l.kills_icon.y, 320.);
        assert!((l.kills_icon.x + 40. + 20. - (1920. - 60.)).abs() < 1e-3, "kills: text at Left 40");
        assert!(l.alive_icon.x + 34. + 4. + 60. + 10. <= l.kills_icon.x + 1e-3);
        assert_eq!(l.pack_panel.w, 3. * 54. + 2. * 4.);
        assert_eq!(l.pack_panel.x + l.pack_panel.w, 1920. - 40.);
        assert_eq!(l.pack_panel.y + l.pack_panel.h, 1080. - 348.);
        assert_eq!(l.packs[1].x - l.packs[0].x, 58.);
        assert_eq!(l.bp_row.y + l.bp_row.h + 6., l.pack_panel.y, "backpack row sits 6 above the packs");
        assert_eq!(l.bp_row.x + l.bp_row.w, 1920. - 40.);
    }
    #[test] fn layout_scales_and_stays_on_screen_for_all_aspects() {
        // landscape only: the Vertical DPI rule (CookedIniVersion.txt L531) makes an 800x1280 client 675 Slate units wide, narrower than the compass
        for (w, h) in [(1920., 1080.), (1280., 720.), (2560., 1080.), (3440., 1440.), (1024., 768.), (640., 480.), (3840., 2160.)] {
            let l = layout(w, h, 50., 20., 24., 8., 8.);
            for r in [l.compass, l.minimap, l.alive_icon, l.kills_icon, l.pack_panel, l.bp_row] { assert!(r.x >= -0.5 && r.y >= -0.5 && r.x + r.w <= w + 0.5 && r.y + r.h <= h + 0.5, "{w}x{h} {r:?}"); }
            assert!((l.compass.x + l.compass.w * 0.5 - w * 0.5).abs() < 0.01);
        }
    }
    #[test] fn resource_counts_clamp_to_max_stack() { assert_eq!(clamp_resource(0), 0); assert_eq!(clamp_resource(999), 999); assert_eq!(clamp_resource(5000), 999); }
    #[test] fn default_state_uses_only_original_defaults() { let s = HudExtraState::default(); assert_eq!(s.resources, [0; 3]); assert_eq!(s.kills, 0); assert!(s.players_alive.is_none()); assert_eq!(PLAYERS_LEFT_DEFAULT, "100"); }
    #[test] fn test_variants_are_the_old_capture_inputs() {
        let v1 = test_variant(1); assert_eq!((v1.players_alive, v1.kills, v1.resources, v1.heading_override, v1.map_centre), (Some(57), 3, [123, 45, 999], Some(90.), (0.52, 0.43)));
        let v2 = test_variant(2); assert_eq!((v2.players_alive, v2.kills, v2.resources, v2.backpack.0), (Some(7), 12, [1000, 0, 7], 5));
        let v3 = test_variant(3); assert_eq!((v3.players_alive, v3.heading_override), (Some(100), Some(350.)));
        assert!(test_variant(0).players_alive.is_none() && test_variant(9).heading_override.is_none());
    }
    #[test] fn original_noto_sans_bold_rasterises_and_measures() {
        let Some(c) = ctx() else { return };
        let font = &c.fonts.bold;
        let atlas = crate::text::build_atlas(font, 18);
        assert!(atlas.glyphs.len() >= 95);
        for ch in "0123456789 SWNE".chars() { assert!(atlas.glyphs.contains_key(&ch)); }
        let g = atlas.glyphs[&'8']; assert!(g.w > 4. && g.h > 8., "{g:?}");
        let ink: usize = atlas.pixels.iter().filter(|&&p| p > 128).count(); assert!(ink > 300);
        assert!(crate::text::measure(font, "100", 18.) > crate::text::measure(font, "1", 18.) * 2.5);
        assert!(crate::text::measure(font, "999", 36.) > crate::text::measure(font, "999", 18.) * 1.9);
        // atlas UVs inside [0,1] and glyph bitmaps do not overlap neighbours' regions
        assert!(atlas.glyphs.values().all(|g| g.u0 >= 0. && g.v0 >= 0. && g.u1 <= 1. && g.v1 <= 1.));
        let mut seen = vec![false; atlas.w * atlas.h]; let mut overlap = false;
        for g in atlas.glyphs.values() { for y in (g.v0 * atlas.h as f32).round() as usize..(g.v1 * atlas.h as f32).round() as usize { for x in (g.u0 * atlas.w as f32).round() as usize..(g.u1 * atlas.w as f32).round() as usize { if seen[y * atlas.w + x] { overlap = true; } seen[y * atlas.w + x] = true; } } }
        assert!(!overlap);
    }
    #[test] fn burbank_fonts_extracted_but_noto_is_the_hud_face() {
        let Some(c) = ctx() else { return };
        let root = crate::assets::DirAssets::new(crate::assets::DirAssets::default_root());
        for n in ["BurbankBigRegular-Bold.otf", "BurbankSmall-Medium.otf", "NotoSans-Regular.ufont"] {
            use crate::assets::AssetSource;
            let f = crate::text::parse_font(root.read(n).unwrap_or_else(|| panic!("missing {n}"))).unwrap(); assert!(f.horizontal_line_metrics(20.).is_some(), "{n}");
        }
        let _ = c;
        let report = testdata("hud-extra-widgets.json");
        assert!(report.contains("NotoSans-Bold") && !report.contains("Burbank"), "recovered Athena widgets reference NotoSans only");
    }
    #[test] fn recovered_textures_have_widget_brush_sizes() {
        // widget ImageSize: icons 34x34 (textures are 64x64), compass mark 24x24, minimap art 264x264, compass strip 2048x128.
        let Some(mut c) = ctx() else { return };
        for (n, wh) in [("AliveIco.png", (64, 64)), ("KillsIco.png", (64, 64)), ("Compass_CenterMark.png", (24, 24)), ("minimap_Art_square.png", (264, 264)), ("T_Compass.png", (1536, 128)), ("T_CompassFeather.png", (768, 128)), ("MiniMapAthena.png", (2048, 2048)), ("T-Icon-Backpack-64.png", (64, 64)), ("T-Icon-Logs.png", (128, 128)), ("T-Icon-S-Loot-Stone.png", (128, 128))] {
            let t = c.assets.texture(n); assert_eq!((t.w, t.h), wh, "{n}");
        }
        let m = c.assets.texture("T-Icon-IBeam.png"); assert_eq!(m.w, m.h);
    }
    #[test] fn report_records_recovered_widget_numbers() {
        let r = testdata("hud-extra-widgets.json");
        for k in ["AthenaCompass", "AthenaMinimapContainer", "AthenaPlayersLeftWidget", "AthenaKillsWidget", "WidthOverride", "minimap_Art_square", "KillsIco"] { assert!(r.contains(k), "{k}"); }
    }
    fn finite(l: &RenderList) -> bool { l.cmds.iter().all(|c| match c { Cmd::Quad(q) => q.pos.iter().flatten().chain(q.uv.iter()).chain(q.tint.iter()).all(|v| v.is_finite()), Cmd::Material(m) => [m.rect.x, m.rect.y, m.rect.w, m.rect.h].iter().all(|v| v.is_finite()), _ => true }) }
    #[test] fn build_emits_compass_minimap_packs_at_layout_rects() {
        let Some(mut c) = ctx() else { return };
        let mut x = HudExtra::with_state(test_variant(1)); let mut out = RenderList::new();
        x.build(&mut c, 1920., 1080., 0., &mut out);
        assert!(finite(&out));
        let mats: Vec<_> = out.cmds.iter().filter_map(|c| if let Cmd::Material(m) = c { Some(m) } else { None }).collect();
        let compass = mats.iter().find(|m| matches!(m.kind, MaterialKind::Compass { .. })).expect("compass material");
        assert_eq!(compass.rect, Rect::new(576., 0., 768., 128.));
        assert!(matches!(compass.kind, MaterialKind::Compass { pan, .. } if (pan - 0.5).abs() < 1e-6), "variant 1 heading override 90 -> pan 0.5");
        assert_eq!(mats.iter().filter(|m| matches!(m.kind, MaterialKind::PanelVGrad | MaterialKind::PanelVGradHighlighted)).count(), 3, "three resource packs (one of them highlighted)");
        assert_eq!(mats.iter().filter(|m| matches!(m.kind, MaterialKind::PanelLine { .. })).count(), 3, "backpack divider + two pack dividers");
        // the player arrow is the one rotated (non axis-aligned) quad
        let rotated = out.cmds.iter().filter(|c| matches!(c, Cmd::Quad(q) if q.pos[0][1] != q.pos[1][1])).count(); assert_eq!(rotated, 1);
        // heading 90 degrees: arrow quad extents are still 20x20 (square rotated a quarter turn)
        // text: more glyphs with variant 1 than with the default state
        let mut base = RenderList::new(); HudExtra::new().build(&mut c, 1920., 1080., 0., &mut base);
        assert!(out.quad_count() > base.quad_count());
    }
    #[test] fn heading_override_wins_over_camera_yaw_and_pan_wraps_by_one_per_turn() {
        let Some(mut c) = ctx() else { return };
        let pan_of = |x: &mut HudExtra, c: &mut crate::scene::UiContext, yaw: f32| { let mut o = RenderList::new(); x.build(c, 1280., 720., yaw, &mut o); o.cmds.iter().find_map(|m| if let Cmd::Material(MaterialCmd { kind: MaterialKind::Compass { pan, .. }, .. }) = m { Some(*pan) } else { None }).unwrap() };
        use crate::draw::MaterialCmd;
        let mut a = HudExtra::new(); let p0 = pan_of(&mut a, &mut c, 0.); let p360 = pan_of(&mut a, &mut c, 360.);
        assert!((p360 - p0 - 1.).abs() < 1e-5);
        let mut b = HudExtra::with_state(test_variant(2)); assert!((pan_of(&mut b, &mut c, 12.) - (225. / 360. + 0.25)).abs() < 1.0 / 1536.);
    }
}
