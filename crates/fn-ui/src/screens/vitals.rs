//! HUD health + shield bars (AthenaLocalPlayerHitPointInfo / AthenaHitPointBar).
//!
//! SOURCES (recovered from the 1.8 paks; see reports/hud-vitals.json and tools/widget-evidence.cjs):
//! * Placement: AthenaHUD `VerticalBoxCenterBottom` (HAlign Center, VAlign Bottom) holds
//!   InvalidationBoxPlayerHitpointinfo with slot padding Bottom=90. Group = VerticalBox of two AthenaHitPointBar
//!   widgets, Shield row on top, Health row below, VerticalBoxSlot padding Bottom=4 between them.
//! * Each bar row: `[Size Box] Bar` WidthOverride 513.63544 x HeightOverride 25. Artwork overlay (back to front):
//!   Baseplate (HUD-ShieldMeter-Baseplate, Box brush margin L/R 0.4, ImageSize 360x60, tint 0,0.0012,0.0103,0.6),
//!   SizeBoxHighlight (LeadEdge bevel), Delta ProgressBar, Fill ProgressBar. Both ProgressBars use the material
//!   HUD-Bar_Material = lerp(ColorA, ColorB, LinearGradient(UV)). Stat text HorizontalBox at overlay Left=16:
//!   [current stat, Fill/right] [CommonBorder(Border_HUD_Line_Vert) around a 24-wide icon cell, padding 4,2,4,2]
//!   [max stat]; icon 24x24 with a 24x24 shadow (black 0.5).
//! * Colours: Initialize Bar bytecode contains five Select-by-ProgressBarType tables (enum 0=Health, 1=Shield,
//!   2=third type). They feed HUD-Bar_Material ColorB/ColorA (two sets) and the max-stat text style colours
//!   (which match TextStyle-Base-XS-HUD-{Health,Shield}BarMax). Text: NotoSans-Bold, Size 16.
//! * Damage trail: AthenaHitPointBar FadeUpdateRate = 0.05; Delta bar lags the Fill bar ("Update Delta Bar").
//!
//! NOT VERIFIED / APPROXIMATE (never claim 1:1): which colour set is Fill vs Delta (inferred from the bytecode
//! order of the ColorB/ColorA writes), gradient direction (assumed ColorA left -> ColorB right), Slate's
//! linear->sRGB conversion of tints/materials (assumed), the lead-edge placement/tint (drawn at the fill's
//! right edge in the bright colour; original SizeBoxHighlight width logic not decoded), heal animation
//! (fill jumps up, delta follows instantly), critical-health colour pulse (Critical Color A/B/C exist but their
//! trigger threshold and timing were not decoded), the M_Border_HUD_Line_Vert gradient-panel material (drawn as
//! a plain 0.2-alpha box), font point->pixel mapping (1 px per unit * DPI scale like hud_extra), hinting/kerning.
//!
//! PORT NOTES: the HUD-Bar_Material is emitted as `MaterialKind::HudBar` over the FILLED rect (the old runtime baked a 128x1 strip and stretched it;
//! the per-pixel gradient is the same function of u). The divider is `MaterialKind::PanelLine` over its rect.
use crate::assets::TexInfo;
use crate::draw::{MaterialKind, RenderList, TextureId};
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::{art, atlas_px, line_height, put_text, srgb, srgb3, Snap, WHITE};
use crate::text::FontId;

// ---- recovered constants (Slate units) --------------------------------------------------------------
// 1.8 (AthenaHitPointBar bytecode, Initialize Bar): the bar's baseplate brush is replaced by T-Border-HUD-HitPointBar-Bg (288x24, SetBrushFromTexture
// with bMatchSize) and the bar is sized from it: WidthOverride = SizeX * 1.5 = 432, HeightOverride = SizeY = 24. The tree's 513.6354 x 25 and the
// 360x60 HUD-ShieldMeter-Baseplate are only designer defaults that PreConstruct overwrites.
pub const BG_TEX: (f32, f32) = (288., 24.); // T-Border-HUD-HitPointBar-Bg
pub const BAR_W: f32 = BG_TEX.0 * 1.5; // Initialize Bar: [Size Box] Bar.SetWidthOverride(BaseplateTexture.SizeX * 1.5)
pub const BAR_H: f32 = BG_TEX.1; // Initialize Bar: [Size Box] Bar.SetHeightOverride(BaseplateTexture.SizeY)
pub const ICON_CELL_W: f32 = 24.; // SizeBox_12774 MinDesiredWidth (stat icon cell left of the bar in HorizontalBox_66)
pub const ROW_H: f32 = 26.; // icon overlay: 24 icon + shadow slot Padding.Top 2 -> row height 26 (bar is VAlign_Top, 24 tall)
pub const ROW_GAP: f32 = 4.; // VerticalBoxSlot_46 padding Bottom
pub const BOTTOM_PAD: f32 = 90.; // AthenaHUD VerticalBoxSlot_5 (hit point info) padding Bottom
pub const TEXT_LEFT: f32 = 16.; // OverlaySlot_1 padding Left for the stat text row
pub const ICON: f32 = 24.; // Stat Icon ImageSize
pub const DIVIDER: f32 = 4.; // CommonBorder_6 (Border_HUD_Line_Vert) SizeBox WidthOverride
pub const DIVIDER_PAD: (f32, f32) = (4., 2.); // HorizontalBoxSlot_8 padding (x, y): cell = 4 + 4 + 4 wide
pub const TEXT_TOP: f32 = -2.5; // stat text slots Padding.Top
pub const BEVEL_H: f32 = 24.; // T-Border-HUD-HitPointBar-LeadEdge ImageSize (VAlign_Center in the bar)
pub const BEVEL_FADE: f32 = 1.2; // Update Fill Bar: bevel alpha = min(1, X / (ImageSize.X * 1.2)) for X = fill width
pub const FONT_SIZE: f32 = 16.; // TextStyle-Base-XS-B / -HUD-*BarMax Size
pub const MAX_VALUE: f32 = 100.; // normal player max health / max shield
pub const FADE_UPDATE_RATE: f32 = 0.05; // AthenaHitPointBar FadeUpdateRate
pub const PLATE_MARGIN: [f32; 4] = [0.4, 0., 0.4, 0.]; // Baseplate brush Margin (L,T,R,B)
pub const PLATE_TINT: [f32; 4] = [0., 0.0012, 0.0103, 0.6]; // Baseplate ColorAndOpacity (linear)
pub const EDGE_MARGIN: [f32; 4] = [0., 0., 0.9, 0.]; // Bevel brush Margin
pub const SHADOW_TINT: [f32; 4] = [0., 0., 0., 0.5]; // ImageStatIconShadow ColorAndOpacity
pub const LINE_VERT_ALPHA: f32 = 0.2; // M_Border_HUD_Line_Vert "Global Alpha"
/// Old fallback gradient step count (kept for reference; the material path evaluates the gradient per pixel).
pub const GRADIENT_STEPS: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Health, Shield, Energy }
/// All colours are LINEAR (as stored in the assets).
#[derive(Clone, Copy, Debug)]
pub struct Style { pub fill_a: [f32; 3], pub fill_b: [f32; 3], pub delta_a: [f32; 3], pub delta_b: [f32; 3], pub text: [f32; 3] }
pub const HEALTH: Style = Style { fill_a: [0.07619, 0.50289, 0.0356], fill_b: [0.17789, 0.73046, 0.08022], delta_a: [0.78354, 0.05448, 0.05448], delta_b: [1., 0.16513, 0.16513], text: [0.47932, 0.87962, 0.23074] };
pub const SHIELD: Style = Style { fill_a: [0.0382, 0.18116, 0.83077], fill_b: [0.0865, 0.43966, 0.9131], delta_a: [0.0865, 0.43966, 0.9131], delta_b: [0.20508, 0.68669, 0.95597], text: [0.20508, 0.68669, 0.95597] };
pub const THIRD: Style = Style { fill_a: [0.66539, 0.2462, 0.02956], fill_b: [0.82279, 0.55201, 0.0648], delta_a: [0.82279, 0.55201, 0.0648], delta_b: [0.9131, 0.76052, 0.14996], text: [0.9131, 0.76052, 0.14996] };
pub fn style(k: Kind) -> Style { match k { Kind::Health => HEALTH, Kind::Shield => SHIELD, Kind::Energy => THIRD } }
pub fn energy_rows(l: &Layout) -> (Rect, Rect) {
    let top = l.shield.y - ROW_GAP * l.scale - ROW_H * l.scale;
    (Rect::new(l.shield_icon.x, top, l.shield_icon.w, l.shield_icon.h), Rect::new(l.shield.x, top, l.shield.w, l.shield.h))
}

#[allow(dead_code)] // the gradient itself is evaluated by the HudBar material; kept for the endpoint test
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] { [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t] }

// ---- layout ----------------------------------------------------------------------------------------------
/// NOT ORIGINAL DATA (invented fallback, labelled): AthenaHUD anchors the bars HAlign_Center and the quickbar
/// HAlign_Right with fixed widths (513.6 vs 496 Slate units) and the recovered widgets contain no rule that separates them,
/// so on narrow aspects (4:3, portrait, i.e. Slate width < ~1000) the data-faithful placement overlaps slot 1.
/// Fallback: keep the recovered Bottom=90 padding and widths, and slide the group left just far enough to clear the
/// quickbar's left edge by this many Slate units (never less than screen x=0). Identical to the original whenever it fits.
pub const CLEARANCE: f32 = 8.;
/// Left edge for a bar of width `bw` centred at `centred_x`, kept `clearance` px left of `limit` (quickbar left edge).
pub fn avoid_quickbar(centred_x: f32, bw: f32, limit: f32, clearance: f32) -> f32 {
    if centred_x + bw <= limit - clearance { centred_x } else { (limit - clearance - bw).max(0.) }
}
/// `group` = both rows (icon cell + bar); `shield`/`health` = the BAR rectangles (432x24); `*_icon` = the 24-wide icon cells left of the bars.
#[derive(Clone, Copy, Debug)]
pub struct Layout { pub scale: f32, pub group: Rect, pub shield: Rect, pub health: Rect, pub shield_icon: Rect, pub health_icon: Rect }
pub fn layout(w: f32, h: f32) -> Layout {
    let s = dpi_scale(w, h);
    let (bw, bh, gap, rh, cw) = (BAR_W * s, BAR_H * s, ROW_GAP * s, ROW_H * s, ICON_CELL_W * s);
    let gw = cw + bw; // HorizontalBox_66 desired width (icon cell + bar), both rows equal
    let x = avoid_quickbar((w - gw) * 0.5, gw, crate::screens::quickbar::layout(w, h)[0].item.x, CLEARANCE * s);
    let health_top = h - BOTTOM_PAD * s - rh;
    let shield_top = health_top - gap - rh;
    let mk = |top: f32| (Rect::new(x, top, cw, rh), Rect::new(x + cw, top, bw, bh)); // bar is VAlign_Top
    let (si, sb) = mk(shield_top); let (hi, hb) = mk(health_top);
    Layout { scale: s, group: Rect::new(x, shield_top, gw, 2. * rh + gap), shield: sb, health: hb, shield_icon: si, health_icon: hi }
}
/// Fraction of the bar covered by `value` (clamped 0..1).
pub fn percent(value: f32, max: f32) -> f32 { if max <= 0. { 0. } else { (value / max).clamp(0., 1.) } }
/// Filled part of a row for a percentage.
pub fn fill_rect(row: Rect, pct: f32) -> Rect { Rect::new(row.x, row.y, row.w * pct.clamp(0., 1.), row.h) }

// ---- animation (damage trail) ----------------------------------------------------------------------------
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anim { pub fill: f32, pub delta: f32 }
impl Anim {
    pub fn new(p: f32) -> Self { Self { fill: p, delta: p } }
    /// Fill follows the value immediately; the Delta bar trails behind on damage (FadeUpdateRate per 1/60 s tick,
    /// frame-rate compensated) and snaps up on heal. Rate model is an approximation of "Update Delta Bar".
    pub fn step(&mut self, target: f32, dt: f32) {
        self.fill = target;
        if self.delta <= target { self.delta = target; return; }
        let k = 1. - (1. - FADE_UPDATE_RATE).powf((dt * 60.).max(0.));
        self.delta += (target - self.delta) * k;
        if (self.delta - target).abs() < 0.0005 { self.delta = target; }
    }
}

// ---- text helpers ------------------------------------------------------------------------------------------
pub fn stat_text(v: u16) -> String { v.min(9999).to_string() }
/// Horizontal layout of the stat row in pixels: returns (current_x, icon_cell_x, max_x) given text widths.
/// [Horizontal Box] Stat Text sits in an overlay slot (HAlign Left, Padding.Left 16): current stat | divider cell (padding 4, border 4, padding 4) | max stat.
pub fn text_row(row: Rect, scale: f32, cur_w: f32) -> (f32, f32, f32) {
    let cur_x = row.x + TEXT_LEFT * scale;
    let cell_x = cur_x + cur_w;
    let max_x = cell_x + (DIVIDER + 2. * DIVIDER_PAD.0) * scale;
    (cur_x, cell_x, max_x)
}

// ---- state -----------------------------------------------------------------------------------------------------
#[derive(Clone, Copy)]
struct Tex { plate: TexInfo, edge: TexInfo, shield_icon: TextureId, health_icon: TextureId, line: TextureId }

/// Health/shield bar state: the animated (fill, delta) fractions plus lazily resolved textures.
#[derive(Clone)]
pub struct Vitals { health: Anim, shield: Anim, primed: bool, tex: Option<Tex>, energy: Option<Anim> }
impl Default for Vitals { fn default() -> Self { Self { health: Anim::new(1.), shield: Anim::new(0.), primed: false, tex: None, energy: None } } }

impl Vitals {
    pub fn new() -> Self { Self::default() }
    /// Advance the damage-trail animation toward the current values. The first call snaps (no trail on spawn).
    pub fn tick(&mut self, dt: f32, health: u16, shield: u16) {
        let (h, s) = (percent(health as f32, MAX_VALUE), percent(shield as f32, MAX_VALUE));
        if !self.primed { self.health = Anim::new(h); self.shield = Anim::new(s); self.primed = true; }
        self.health.step(h, dt); self.shield.step(s, dt);
    }
    pub fn tick_energy(&mut self, dt: f32, energy: Option<f32>) {
        match energy {
            Some(e) => { let p = percent(e, MAX_VALUE); match self.energy.as_mut() { Some(a) => a.step(p, dt), None => self.energy = Some(Anim::new(p)) } }
            None => self.energy = None,
        }
    }
    pub fn build_energy(&mut self, ctx: &mut UiContext, w: f32, h: f32, energy: Option<f32>, out: &mut RenderList) {
        let (Some(e), Some(anim)) = (energy, self.energy) else { return };
        let tex = self.ensure_tex(ctx);
        let l = layout(w, h);
        let (icon, row) = energy_rows(&l);
        Self::row(&tex, ctx, out, Kind::Energy, row, icon, anim, e.round().clamp(0., MAX_VALUE) as u16, l.scale);
    }
    /// Snap both bars (no trail); used by the capture scenes to set a known state.
    pub fn snap(&mut self, health: u16, shield: u16) { self.health = Anim::new(percent(health as f32, MAX_VALUE)); self.shield = Anim::new(percent(shield as f32, MAX_VALUE)); self.primed = true; }
    /// Current animated (fill, delta) fractions: ([health], [shield]).
    pub fn anim_state(&self) -> ([f32; 2], [f32; 2]) { ([self.health.fill, self.health.delta], [self.shield.fill, self.shield.delta]) }

    fn ensure_tex(&mut self, ctx: &mut UiContext) -> Tex {
        if let Some(t) = self.tex { return t; }
        let t = Tex { plate: art(ctx, "T-Border-HUD-HitPointBar-Bg.png"), edge: art(ctx, "T-Border-HUD-HitPointBar-LeadEdge.png"), shield_icon: art(ctx, "Icon-Shieldbar-M.png").id, health_icon: art(ctx, "Icon-Healthbar-S.png").id,
            line: crate::screens::mat_tex(ctx, "T-Border-HUD-Line-Vert.png", false).id };
        self.tex = Some(t); t
    }

    #[allow(clippy::too_many_arguments)]
    fn row(tex: &Tex, ctx: &mut UiContext, out: &mut RenderList, kind: Kind, row: Rect, icon_cell: Rect, anim: Anim, value: u16, scale: f32) {
        let st = style(kind);
        // 1. Baseplate: T-Border-HUD-HitPointBar-Bg as a Box brush (margin L/R 0.4), tint (0,0.00121,0.01033,0.6).
        out.nine_slice(tex.plate, row, PLATE_MARGIN, scale, [srgb(PLATE_TINT[0]), srgb(PLATE_TINT[1]), srgb(PLATE_TINT[2]), PLATE_TINT[3]]);
        // 2. Delta then Fill ProgressBars (HUD-Bar_Material, ColorA left -> ColorB right over the filled quad).
        for (pct, delta) in [(anim.delta, true), (anim.fill, false)] {
            let f = fill_rect(row, pct);
            if f.w > 0.5 { let (a, b) = if delta { (st.delta_a, st.delta_b) } else { (st.fill_a, st.fill_b) }; out.material(MaterialKind::HudBar { a, b }, f, WHITE, false); }
        }
        // 3. Lead-edge bevel: SizeBoxHighlight (left aligned) width = fill width; Bevel = Box brush margin R 0.9, 24 tall, VAlign centre,
        //    tint = bar text colour, alpha = min(1, X / (24 * 1.2)) (Update Fill Bar bytecode).
        let f = fill_rect(row, anim.fill);
        if f.w > 0.5 {
            let alpha = (f.w / scale / (tex.edge.w as f32 * BEVEL_FADE)).min(1.);
            let bright = srgb3(st.text);
            out.nine_slice(tex.edge, Rect::new(f.x, row.y + (row.h - BEVEL_H * scale) * 0.5, f.w, BEVEL_H * scale), EDGE_MARGIN, scale, [bright[0], bright[1], bright[2], alpha]);
        }
        // 4. Stat row: current (TextStyle-Base-XS-B, white) | divider cell | max (TextStyle-Base-XS-HUD-*BarMax colour).
        let px = atlas_px(FONT_SIZE, scale);
        let (cur, max) = (stat_text(value), stat_text(MAX_VALUE as u16));
        let cur_w = ctx.fonts.measure(FontId::Bold, &cur, px as f32);
        let (cur_x, cell_x, max_x) = text_row(row, scale, cur_w);
        let lh = line_height(ctx, FontId::Bold, px);
        let asc = crate::screens::ascent(ctx, FontId::Bold, px);
        // HorizontalBoxSlot Top=-2.5 on a centred child
        let base = (row.y + (row.h - lh) * 0.5 + asc + (-1.25 * px as f32 / FONT_SIZE)).round();
        put_text(ctx, out, FontId::Bold, px, cur_x, base, &cur, WHITE, None, Snap::Pen);
        let line_h = ((lh + TEXT_TOP * scale) - 2. * DIVIDER_PAD.1 * scale).max(2.);
        let div = Rect::new(cell_x + DIVIDER_PAD.0 * scale, row.y + (row.h - (line_h + 2. * DIVIDER_PAD.1 * scale)) * 0.5 + DIVIDER_PAD.1 * scale, DIVIDER * scale, line_h);
        if tex.line != TextureId::NONE { out.material(MaterialKind::PanelLine { mask: tex.line, global_alpha: LINE_VERT_ALPHA }, div, WHITE, false); }
        let t = srgb3(st.text);
        put_text(ctx, out, FontId::Bold, px, max_x, base, &max, [t[0], t[1], t[2], 1.], None, Snap::Pen);
        // 5. Stat icon cell left of the bar: 24x24 icon (+ black 0.5 shadow 2 units lower; the 26-tall overlay centres the icon 1 unit down).
        let icon = match kind { Kind::Health => tex.health_icon, Kind::Shield => tex.shield_icon, Kind::Energy => return };
        let ic = Rect::new(icon_cell.x + (icon_cell.w - ICON * scale) * 0.5, icon_cell.y + scale, ICON * scale, ICON * scale);
        out.sprite(icon, Rect::new(ic.x, ic.y + scale, ic.w, ic.h), SHADOW_TINT);
        out.sprite(icon, ic, WHITE);
    }

    /// Append both bars (Shield row on top, Health below) for the current `health`/`shield` values (the bars' fractions come from the animation state).
    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, health: u16, shield: u16, out: &mut RenderList) {
        let tex = self.ensure_tex(ctx);
        let l = layout(w, h);
        Self::row(&tex, ctx, out, Kind::Shield, l.shield, l.shield_icon, self.shield, shield, l.scale);
        Self::row(&tex, ctx, out, Kind::Health, l.health, l.health_icon, self.health, health, l.scale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Cmd;
    use crate::screens::testutil::{ctx, evidence};
    fn f(v: &serde_json::Value) -> f32 { v.as_f64().unwrap() as f32 }
    #[test] fn constants_match_1_8_bytecode_evidence() {
        let v = &evidence("hud-1.8-evidence.json")["vitals"];
        // Initialize Bar bytecode: width = Baseplate Texture SizeX * 1.5, height = SizeY, baseplate texture = T-Border-HUD-HitPointBar-Bg (288x24).
        assert_eq!(v["baseplate_texture"], "T-Border-HUD-HitPointBar-Bg"); assert_eq!(v["width_multiplier"].as_f64().unwrap() as f32, 1.5);
        assert_eq!(v["bar_width"].as_f64().unwrap() as f32, BAR_W); assert_eq!(v["bar_height"].as_f64().unwrap() as f32, BAR_H);
        assert!((f(&v["designer_default_bar"][0]) - 513.63544).abs() < 1e-3 && f(&v["designer_default_bar"][1]) == 25., "designer defaults are overwritten by Initialize Bar");
        assert_eq!(v["row_gap"].as_f64().unwrap() as f32, ROW_GAP); assert_eq!(v["bottom_padding"].as_f64().unwrap() as f32, BOTTOM_PAD);
        assert_eq!(v["text_left"].as_f64().unwrap() as f32, TEXT_LEFT); assert_eq!(v["icon_cell_min_width"].as_f64().unwrap() as f32, ICON_CELL_W);
        assert_eq!(v["divider_width"].as_f64().unwrap() as f32, DIVIDER); assert_eq!(v["fade_update_rate"].as_f64().unwrap() as f32, FADE_UPDATE_RATE);
        let lin = |k: &str| -> [f32; 3] { let a = v["colours"][k].as_array().unwrap(); [a[0].as_f64().unwrap() as f32, a[1].as_f64().unwrap() as f32, a[2].as_f64().unwrap() as f32] };
        // bytecode-proven: [ProgressBar] Fill receives ColorA/ColorB (...Temp_struct 9/6, 8/5), [ProgressBar] Delta the other set (13/10, 14/11).
        assert_eq!(lin("health_fill_a"), HEALTH.fill_a); assert_eq!(lin("health_fill_b"), HEALTH.fill_b); assert_eq!(lin("health_delta_a"), HEALTH.delta_a); assert_eq!(lin("health_delta_b"), HEALTH.delta_b);
        assert_eq!(lin("shield_fill_a"), SHIELD.fill_a); assert_eq!(lin("shield_fill_b"), SHIELD.fill_b); assert_eq!(lin("shield_delta_a"), SHIELD.delta_a); assert_eq!(lin("shield_delta_b"), SHIELD.delta_b);
        assert_eq!(lin("health_text"), HEALTH.text); assert_eq!(lin("shield_text"), SHIELD.text);
    }
    #[test] fn layout_1080p_matches_slate_units() {
        let l = layout(1920., 1080.); assert!((l.scale - 1.).abs() < 1e-6);
        assert!((l.health.y + ROW_H - (1080. - 90.)).abs() < 1e-3, "health row (26 tall) bottom edge is 90 above the screen bottom");
        assert!((l.shield.y + ROW_H + 4. - l.health.y).abs() < 1e-3, "shield row sits 4 above health");
        assert!((l.health.w - 432.).abs() < 1e-3 && (l.health.h - 24.).abs() < 1e-6);
        assert!((l.group.x + l.group.w * 0.5 - 960.).abs() < 1e-3, "icon cell + bar (456 wide) centred horizontally");
        assert!((l.health.x - l.health_icon.x - 24.).abs() < 1e-3, "icon cell is 24 wide, left of the bar");
    }
    #[test] fn layout_scales_with_dpi_and_stays_on_screen() {
        for (w, h) in [(1280., 720.), (2560., 1080.), (1024., 768.), (640., 480.), (800., 1280.), (3440., 1440.), (3840., 2160.)] {
            let l = layout(w, h); assert!((l.health.w - BAR_W * l.scale).abs() < 1e-3);
            assert!(l.group.x >= 0. && l.group.x + l.group.w <= w + 0.01, "{w}x{h} horizontal fit");
            assert!(l.group.y >= 0. && l.health.y + ROW_H * l.scale <= h - BOTTOM_PAD * l.scale + 0.01, "{w}x{h} vertical fit");
        }
    }
    #[test] fn bars_never_overlap_quickbar_and_stay_centred_when_they_fit() {
        // landscape only: the Vertical DPI rule (CookedIniVersion.txt L531) makes an 800x1280 client 675 Slate units wide, too narrow for both bars
        for (w, h) in [(1280., 720.), (1920., 1080.), (2560., 1080.), (1024., 768.), (640., 480.), (3440., 1440.), (3840., 2160.)] {
            let l = layout(w, h); let qb = crate::screens::quickbar::layout(w, h)[0].item;
            assert!(l.group.x + l.group.w <= qb.x - 0.5 * l.scale || l.group.y + l.group.h <= qb.y, "{w}x{h}: bars overlap quickbar slot 1");
            let centred = (w - l.group.w) * 0.5; let fits = centred + l.group.w <= qb.x - CLEARANCE * l.scale;
            if fits { assert!((l.group.x - centred).abs() < 1e-3, "{w}x{h}: must stay centred (original anchor) when it fits"); }
        }
        let l = layout(1920., 1080.); assert!((l.group.x + l.group.w * 0.5 - 960.).abs() < 1e-3);
        assert!(layout(1024., 768.).group.x < (1024. - layout(1024., 768.).group.w) * 0.5, "4:3 shifts left");
        assert_eq!(avoid_quickbar(100., 200., 1000., 8.), 100.); assert_eq!(avoid_quickbar(500., 600., 900., 8.), 292.); assert_eq!(avoid_quickbar(0., 600., 300., 8.), 0.);
    }
    #[test] fn percent_and_fill_rect() { assert_eq!(percent(50., 100.), 0.5); assert_eq!(percent(200., 100.), 1.); assert_eq!(percent(-5., 100.), 0.); assert_eq!(percent(5., 0.), 0.); let r = fill_rect(Rect::new(10., 20., 200., 25.), 0.25); assert_eq!(r, Rect::new(10., 20., 50., 25.)); assert_eq!(fill_rect(Rect::new(0., 0., 80., 8.), 2.).w, 80.); }
    #[test] fn damage_trail_lags_then_converges() {
        let mut a = Anim::new(1.); a.step(0.4, 1. / 60.);
        assert_eq!(a.fill, 0.4); assert!(a.delta > 0.4 && a.delta < 1., "delta trails after damage: {}", a.delta);
        let first = a.delta; a.step(0.4, 1. / 60.); assert!(a.delta < first, "delta keeps shrinking");
        for _ in 0..600 { a.step(0.4, 1. / 60.); } assert_eq!(a.delta, 0.4);
    }
    #[test] fn heal_raises_delta_with_fill() { let mut a = Anim::new(0.3); a.step(0.8, 1. / 60.); assert_eq!((a.fill, a.delta), (0.8, 0.8)); }
    #[test] fn trail_rate_is_frame_rate_independent() {
        let (mut a, mut b) = (Anim::new(1.), Anim::new(1.)); a.step(0., 1. / 30.); b.step(0., 1. / 60.); b.step(0., 1. / 60.);
        assert!((a.delta - b.delta).abs() < 1e-5, "{} vs {}", a.delta, b.delta);
        let mut c = Anim::new(1.); c.step(0., 1. / 60.); assert!((c.delta - (1. - FADE_UPDATE_RATE)).abs() < 1e-5, "one 60 Hz tick keeps 1-0.05");
    }
    #[test] fn srgb_conversion_known_points() { assert_eq!(srgb(0.), 0.); assert!((srgb(1.) - 1.).abs() < 1e-6); assert!((srgb(0.2140) - 0.5).abs() < 2e-3); assert!((srgb(0.0031308) - 0.04045).abs() < 1e-3); }
    #[test] fn recovered_palette_values() {
        // Values decoded from AthenaHitPointBar bytecode (see reports/hud-vitals.json).
        assert_eq!(HEALTH.text, [0.47932, 0.87962, 0.23074]); assert_eq!(SHIELD.text, [0.20508, 0.68669, 0.95597]);
        for k in [HEALTH, SHIELD] { for c in [k.fill_a, k.fill_b, k.delta_a, k.delta_b, k.text] { assert!(c.iter().all(|v| (0. ..=1.).contains(v))); } assert!(k.fill_b.iter().sum::<f32>() > k.fill_a.iter().sum::<f32>() - 0.4, "fill pair is a dark-to-bright ramp"); assert!(k.delta_b.iter().sum::<f32>() > k.delta_a.iter().sum::<f32>(), "delta pair ramps up"); }
        assert!(HEALTH.fill_b[1] > HEALTH.fill_b[0] && HEALTH.fill_b[1] > HEALTH.fill_b[2], "health fill is green");
        assert!(SHIELD.fill_b[2] > SHIELD.fill_b[0] && SHIELD.fill_b[2] > SHIELD.fill_b[1], "shield fill is blue");
        assert_eq!(PLATE_TINT, [0., 0.0012, 0.0103, 0.6]);
    }
    #[test] fn gradient_endpoints_are_fill_a_and_b() { let (a, b) = (HEALTH.fill_a, HEALTH.fill_b); assert_eq!(lerp3(a, b, 0.), a); assert_eq!(lerp3(a, b, 1.), b); let m = lerp3(a, b, 0.5); assert!((m[1] - (a[1] + b[1]) / 2.).abs() < 1e-6); }
    #[test] fn stat_text_formats_integers() { assert_eq!(stat_text(100), "100"); assert_eq!(stat_text(0), "0"); assert_eq!(stat_text(7), "7"); assert_eq!(stat_text(65535), "9999"); }
    #[test] fn text_row_order_and_spacing() {
        let (cx, cell, mx) = text_row(Rect::new(100., 0., 432., 24.), 2., 30.);
        assert_eq!(cx, 100. + 32.); assert_eq!(cell, cx + 30.); assert_eq!(mx, cell + 24.); assert!(cx < cell && cell < mx, "divider cell = 4 + 4 + 4 units");
    }
    #[test] fn exported_textures_match_recovered_sizes() {
        let Some(mut c) = ctx() else { return };
        for (name, w, h) in [("T-Border-HUD-HitPointBar-Bg.png", 288, 24), ("T-Border-HUD-HitPointBar-LeadEdge.png", 24, 24), ("Icon-Shieldbar-M.png", 40, 40), ("Icon-Healthbar-S.png", 24, 24), ("T-Border-HUD-Line-Vert.png", 4, 8)] {
            let t = c.assets.texture(name); assert_eq!((t.w, t.h), (w, h), "{name}");
        }
    }
    #[test] fn original_font_loads_and_digits_have_advance() {
        let Some(c) = ctx() else { return };
        let a = crate::text::build_atlas(&c.fonts.bold, 16);
        for ch in "0123456789".chars() { let g = a.glyphs.get(&ch).unwrap(); assert!(g.adv > 5. && g.w > 3. && g.h > 8., "digit {ch}"); }
        assert!(c.fonts.measure(FontId::Bold, "100", 16.) > c.fonts.measure(FontId::Bold, "10", 16.));
    }
    #[test] fn baseplate_texture_is_translucent_white_frame() {
        let Some(mut c) = ctx() else { return };
        let im = c.assets.image("HUD-ShieldMeter-Baseplate.png").expect("baseplate");
        let at = |x: u32, y: u32| { let i = ((y * im.w + x) * 4) as usize; [im.rgba[i], im.rgba[i + 1], im.rgba[i + 2], im.rgba[i + 3]] };
        let edge_a = (0..12).map(|x| at(x, 20)[3]).max().unwrap(); let mid = at(159, 20); assert!(edge_a > 200, "outline is opaque: {edge_a}"); assert!(mid[3] > 40 && mid[3] < 200, "interior translucent: {}", mid[3]);
    }
    fn mats(l: &RenderList) -> Vec<&crate::draw::MaterialCmd> { l.cmds.iter().filter_map(|c| if let Cmd::Material(m) = c { Some(m) } else { None }).collect() }
    #[test] fn build_full_health_emits_fill_materials_at_layout_rects() {
        let Some(mut c) = ctx() else { return };
        let mut v = Vitals::new(); v.snap(100, 0); let mut out = RenderList::new();
        v.build(&mut c, 1920., 1080., 100, 0, &mut out);
        let l = layout(1920., 1080.);
        let m = mats(&out);
        // shield empty: no shield bars; health: delta + fill HudBar; two PanelLine dividers
        let bars: Vec<_> = m.iter().filter(|m| matches!(m.kind, MaterialKind::HudBar { .. })).collect();
        assert_eq!(bars.len(), 2);
        assert!(bars.iter().all(|b| b.rect == Rect::new(l.health.x, l.health.y, l.health.w, l.health.h)));
        assert_eq!(m.iter().filter(|m| matches!(m.kind, MaterialKind::PanelLine { .. })).count(), 2);
        assert!(matches!(bars[1].kind, MaterialKind::HudBar { a, b } if a == HEALTH.fill_a && b == HEALTH.fill_b), "fill drawn after delta");
        assert!(out.cmds.iter().all(|c| match c { Cmd::Quad(q) => q.pos.iter().flatten().all(|v| v.is_finite()), _ => true }));
    }
    #[test] fn build_half_fill_halves_the_bar_and_delta_trails() {
        let Some(mut c) = ctx() else { return };
        let mut v = Vitals::new(); v.snap(100, 100); v.tick(1. / 60., 40, 20); // damage: fill jumps, delta lags
        let mut out = RenderList::new(); v.build(&mut c, 1920., 1080., 40, 20, &mut out);
        let l = layout(1920., 1080.);
        let hb: Vec<_> = mats(&out).into_iter().filter(|m| matches!(m.kind, MaterialKind::HudBar { a, .. } if a == HEALTH.fill_a || a == HEALTH.delta_a)).map(|m| m.rect).collect();
        assert_eq!(hb.len(), 2);
        assert!(hb[0].w > hb[1].w, "delta bar is longer than the fill after damage");
        assert!((hb[1].w - l.health.w * 0.4).abs() < 1e-2);
    }
}
