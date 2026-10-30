//! In-game HUD + loading screen as pure state -> [`RenderList`] (no GL, no globals, no thread-locals).
//!
//! Every screen is a plain struct owned by the caller: `tick(dt)` advances animations, `build(ctx, w, h, &mut RenderList)` appends draw commands.
//! * [`quickbar`]  AthenaQuickbarPrimary rail (item cards through `MaterialKind::ItemCard`).
//! * [`vitals`]    AthenaLocalPlayerHitPointInfo health/shield bars (`MaterialKind::HudBar`, `PanelLine`).
//! * [`hud_extra`] compass, minimap, players-alive/kills counters, resource packs, backpack row.
//! * [`hud`]       combines the three above (draw order of the old `Hud::draw` + `hud_extra::draw`) with the world feedback pieces below.
//! * [`reticle`]   pickaxe reticle (`HUD_Xhair_Dot`) + harvest `+N` popups; [`reticle_state`] is the plain-data contract of all world feedback pieces.
//! * [`target_info`] / [`interaction`] / [`equip_progress`]  Target Info health bar (BuildingInfoIndicator), interaction prompt, equip timer.
//! * [`loading`]   loading screen (cover scaling, wordmark, progress bar, tips).
//! * [`scenes`]    deterministic capture scenes named after the old runtime's BMP captures.
//!
//! TEXT: the old runtime drew HUD text with fontdue atlases (HUD) or GDI Segoe UI bitmap atlases (loading screen). Everything here goes through
//! [`crate::text::TextAtlases`] (fontdue, NotoSans); the loading screen's GDI metrics are therefore replaced by fontdue advances (APPROXIMATION).
pub mod equip_progress;
pub mod equipped_item;
pub mod hud;
pub mod hud_extra;
pub mod game_phase;
pub mod interaction;
pub mod kill_feed;
pub mod loading;
pub mod picker;
pub mod quickbar;
pub mod reticle;
pub mod reticle_state;
mod scenes;
mod scenes_world;
pub mod startup_layout;
pub mod startup_loading;
pub mod target_info;
pub mod team_info;
pub mod vitals;
pub mod xform; // WIDGETS3 HOOK: RenderTransform (rotation) of emitted glyph quads
pub mod slot_cooldown; // WIDGETS3 HOOK: QuickbarSlotCooldown
#[cfg(feature = "pak")]
pub mod athena_shop; // SHOP UI (Item Shop screen)
pub mod athena_lobby; // LOBBY UI: the BR lobby screen decoded from the 1.8 widget blueprints
#[cfg(feature = "pak")]
pub mod athena_settings; // SETTINGS: the BR Settings screen (OptionsMenu) composed from the widget blueprints
#[cfg(feature = "pak")]
pub mod athena_locker; // LOCKER: the BR Locker tab (AthenaCustomizationTab) + the lobby pointer controller
pub mod inventory_panel; // WIDGETS3 HOOK: AthenaInventoryPanel
pub mod inventory_cooked; // INVENTORY FIX2: AthenaInventoryPanel / QuantitySelector composed from the cooked widget trees (generic UMG runtime of athena_lobby)
pub mod item_header; // PICKUP CARD: ItemDetailsHeader_C runtime fill shared by the inventory details and the pickup card
pub mod team_indicator_cooked;
pub mod interaction_cooked; // PICKUP CARD: InteractionIndicator_C (+ HUD-PickupItemWidget) composed from the cooked widget trees
pub mod frontend_cooked; // FRONT END: LoginScreen_C ("Logging In...") + SubgameSelectScreen_C (game picker) composed from the cooked widget trees
pub mod build_quickbar; // BUILD HUD: AthenaQuickbarSecondary (building quickbar)
pub mod aerial_feedback;
pub mod radial_picker_cooked;
pub mod hud_cooked;
pub mod pickup_stream;
pub mod health_bar;
pub mod hold_ring; // CONTAINERS: KeybindWidget hold ring (HollowCircularProgressBar) of a hold interaction

use crate::assets::{Sampler, TexInfo};
use crate::draw::RenderList;
use crate::scene::{SceneDef, UiContext};
use crate::text::FontId;

pub const WHITE: [f32; 4] = [1., 1., 1., 1.];

/// HUD/loading scenes (see [`scenes`] for the names and the old capture each one reproduces).
pub fn scenes() -> Vec<SceneDef> { let mut v = scenes::all(); v.extend(scenes_world::all()); v }

/// Mip-mapped, clamped UI art texture by file name (`TexInfo::NONE` when missing; recorded in `ctx.assets.missing`).
pub(crate) fn art(ctx: &mut UiContext, name: &str) -> TexInfo { ctx.assets.texture(name) }
/// Texture sampled INSIDE a material shader (bilinear, no mips, clamp or wrap) - pixels are read by `material::hud::bake`.
pub(crate) fn mat_tex(ctx: &mut UiContext, name: &str, wrap: bool) -> TexInfo { ctx.assets.texture_with(name, Sampler { filter: crate::assets::Filter::Linear, mips: false, wrap }) }

/// Slate font sizes are POINTS: UE4.16 `FontCacheFreeType.cpp:64` sets the FreeType char size to the font size at `FreeTypeConstants::HorizontalDPI` 96
/// (`FontCacheFreeType.h:41`), so the em is `size * 96 / 72` pixels (fontdue's px is the em).
pub const SLATE_PT_TO_PX: f32 = 96. / 72.;
/// Atlas pixel size of a text style: Slate size (points) * 96/72 * DPI scale, rounded, never below 6 px.
pub fn atlas_px(size: f32, scale: f32) -> u32 { (size * SLATE_PT_TO_PX * scale).round().max(6.) as u32 }

/// How glyph origins are snapped to the pixel grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Snap {
    /// hud_extra `text()`: every glyph quad's x and y are `round()`ed (shadow pass included).
    PerGlyph,
    /// vitals `text()`: the pen start is rounded, glyph x = pen + xmin (fractional advances accumulate), baseline already rounded by the caller.
    Pen,
}

/// Line height (ascent + |descent|) of an atlas line.
pub fn line_height(ctx: &mut UiContext, font: FontId, px: u32) -> f32 {
    let (_, a) = ctx.atlases.atlas(&mut ctx.assets.store, &ctx.fonts, font, px);
    a.ascent + a.descent.abs()
}
/// Baseline of a line vertically centred in (`top`, `box_h`) using atlas ascent/descent (old `text()` rule).
pub fn centred_baseline(ctx: &mut UiContext, font: FontId, px: u32, top: f32, box_h: f32) -> f32 {
    let (_, a) = ctx.atlases.atlas(&mut ctx.assets.store, &ctx.fonts, font, px);
    top + (box_h - (a.ascent + a.descent.abs())) * 0.5 + a.ascent
}
/// Atlas ascent of a font at a pixel size.
pub fn ascent(ctx: &mut UiContext, font: FontId, px: u32) -> f32 { ctx.atlases.atlas(&mut ctx.assets.store, &ctx.fonts, font, px).1.ascent }

/// Text with its first glyph origin at `x` on `baseline`. `shadow` = optional (offset px, colour) pass drawn FIRST (old `text()` rule).
#[allow(clippy::too_many_arguments)]
pub fn put_text(ctx: &mut UiContext, out: &mut RenderList, font: FontId, px: u32, x: f32, baseline: f32, text: &str, color: [f32; 4], shadow: Option<([f32; 2], [f32; 4])>, snap: Snap) {
    use crate::layout::Rect;
    let (tex, a) = ctx.atlases.atlas(&mut ctx.assets.store, &ctx.fonts, font, px);
    let passes: Vec<([f32; 2], [f32; 4])> = shadow.into_iter().chain(std::iter::once(([0., 0.], color))).collect();
    for (off, col) in passes {
        let mut pen = match snap { Snap::PerGlyph => x, Snap::Pen => x.round() };
        for ch in text.chars() {
            let Some(g) = a.glyphs.get(&ch) else { continue };
            if g.w > 0. {
                let r = match snap {
                    Snap::PerGlyph => Rect::new((pen + g.xmin + off[0]).round(), (baseline - g.ymin - g.h + off[1]).round(), g.w, g.h),
                    Snap::Pen => Rect::new(pen + g.xmin + off[0], baseline - g.ymin - g.h + off[1], g.w, g.h),
                };
                out.quad(tex, r, [g.u0, g.v0, g.u1, g.v1], col);
            }
            pen += g.adv;
        }
    }
}
/// Advance width of `text` (fontdue, no kerning).
pub fn text_width(ctx: &UiContext, font: FontId, text: &str, px: u32) -> f32 { ctx.fonts.measure(font, text, px as f32) }

/// Linear -> sRGB transfer (Slate composites tints in gamma space).
pub fn srgb(c: f32) -> f32 { let c = c.clamp(0., 1.); if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 } }
pub fn srgb3(c: [f32; 3]) -> [f32; 3] { [srgb(c[0]), srgb(c[1]), srgb(c[2])] }

/// Test helpers: crate-local copies of the old evidence reports and access to the old asset tree.
#[cfg(test)]
pub(crate) mod testutil {
    use crate::scene::UiContext;
    use std::path::PathBuf;
    /// Old runtime tree root (`FN_UI_OLD_ROOT`, default the prototype's location).
    pub fn old_root() -> PathBuf { crate::assets::DirAssets::old_root() }
    /// Evidence JSON copied into `crates/fn-ui/testdata/` (never moved from the old tree).
    pub fn testdata(name: &str) -> String { std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata").join(name)).unwrap_or_else(|e| panic!("testdata/{name}: {e}")) }
    pub fn evidence(name: &str) -> serde_json::Value { serde_json::from_str(&testdata(name)).unwrap() }
    /// UiContext over the old asset tree, or None (with a note) when the tree / fonts are absent.
    pub fn ctx() -> Option<UiContext> {
        let root = crate::assets::DirAssets::default_root();
        if !root.is_dir() { eprintln!("asset tree {} not found (set FN_UI_ASSETS / FN_UI_OLD_ROOT), skipping", root.display()); return None; }
        match UiContext::from_default_assets() { Ok(c) => Some(c), Err(e) => { eprintln!("no UI context ({e}), skipping"); None } }
    }
}

