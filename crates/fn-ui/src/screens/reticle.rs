//! 1.8 pickaxe reticle and harvest feedback (`+N` resource popups). Port of the old prototype's `reticle.rs` onto the fn-ui draw list.
//!
//! EVIDENCE: `testdata/reticle-1.8-evidence.json` (copy of the old `reports/reticle-1.8-evidence.json`: data + native findings, popup widget numbers, the three
//! decoded MovieScene animations), the Kismet dumps of `ResourceAggregationWidget` / `PotentialResourceWidget` / `PackResource`, `AthenaMeleeWeapons` row.
//!
//! RETICLE (what 1.8 actually does)
//! * `Reticle_C` (AthenaHUD, Fill/Fill in OverlayIndicatorContent -> SafeZone) is an EMPTY widget blueprint on the native `FortReticle`; the picture is painted by
//!   the native `SFortReticle` from WEAPON properties. The pickaxe chain (`B_Melee_Impact_Pickaxe_Athena` -> `B_Melee_Impact_Generic` -> `B_Melee_Generic`) sets
//!   `ReticleImage = HUD_Xhair_Dot` (58x54 texture: two brackets and a dot) and `ReticleBuildingColor = FColor(255,221,0,255)`; it has no `HitNotifyReticleImage`,
//!   no `ReticleCenterImage`, no corner angles.
//! * `SFortReticle::Construct` (exe 0x1430227a0): for a non-ranged weapon the centre image takes the `ReticleImage` brush at the TEXTURE size, Center/Center;
//!   `Tick` (0x14305c0f0) only offsets the image for ranged weapons, so the pickaxe reticle is at the centre of the safe zone = the screen centre.
//! * Colour: `FLinearColor::White`, switching to `ReticleBuildingColor` when the weapon's aim trace finds an actor within range (`DetermineReticleColor`).
//!   No melee hit marker exists in 1.8.
//!
//! HARVEST FEEDBACK: `ResourceAggregationWidget` (tag `HUD.Center.ResourceAggregation`, no Settings row -> no toggle can hide it) + `PotentialResourceWidget`,
//! driven by `FortHUDContext.OnDamagedResourceBuilding`; layout, styles, bytecode state machine and the three MovieScene animations are exact. Weak-point hits use
//! font size 20 and `FortUIStylesheet.Color-HarvestWeakPoint` (cyan) instead of 16 / white.
//!
//! NOT RECOVERED (explicit): the C++ default of `ReticleDefaultColor` (white ASSUMED), the aim trace's collision channel (the producer decides what is a hit), the
//! melee range of the native colour test (`RangeVSBuildings2D` 288 cm / Z 384 cm from the AthenaMeleeWeapons row are used by [`aim_in_reach`]), the per-hit
//! resource amount (the producer's number), weak-point geometry (the producer sets `weakpoint`), the icon size at runtime (40 = Standard-Icon-Size[3]).
use crate::draw::{RenderList, TextureId};
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::reticle_state::{FirearmReticle, HarvestHit, HarvestResource};
use crate::screens::{art, atlas_px};
use crate::text::FontId;

// ---- reticle (recovered) -----------------------------------------------------------------------------------------------------
/// `HUD_Xhair_Dot` texture size = the FSlateImageBrush size.
pub const RETICLE_IMAGE: (f32, f32) = (58., 54.);
/// File name of the reticle texture (`/Game/UI/Fort_HUD/HUD_Xhair_Dot`).
pub const RETICLE_TEXTURE: &str = "HUD_Xhair_Dot.png";
/// `B_Ranged_Generic.ReticleImage` decoded from the 1.8 packages (the fallback of `GetReticleImage` when the WID sets none). The native Slate brush size is 10x10.
pub const RANGED_RETICLE_TEXTURE: &str = "HUD_Xhair_2x10_line.png";
/// `B_Ranged_Generic.HitNotifyReticleImage`, drawn after a confirmed ranged impact.
pub const RANGED_HIT_TEXTURE: &str = "HUD_XHair_HitIndicator_full.png";
/// Native Slate brush size, read from the original texture.
pub const RANGED_HIT_IMAGE: (f32, f32) = (30., 30.);
/// `UFortWeaponItemDefinition::UFortWeaponItemDefinition` (RVA 0x9164b0, store at 0x916619) sets `HitNotifyDuration` (+0x61c) = 1.0. The weapon ACTOR's `HitNotifyDisplayDuration`
/// (0.4 on `B_Ranged_Generic`, +0x410) is not read by `SFortReticle` or by `AFortPlayerController` vfunc 651 (RVA 0x140829250, the opacity source): only the WID value counts.
pub const RANGED_HIT_DURATION: f32 = 1.0;
/// `AFortPlayerController::vfunc_651` (RVA 0x140829250, called as vtable +0x1458 by `SFortReticle::Tick`): the hit-notify opacity. Without a `HitNotifyOpacityCurve` it is
/// `1 - (World->TimeSeconds - LastHitNotifyTime) / HitNotifyDuration` clamped to 0..1 (the starter WIDs set no curve), and the image is shown while it is above 0; the image alpha
/// is exactly this value (`FUN_143039500`, the colour binding, copies the reticle colour and overwrites alpha with `SFortReticle+0x7bc`).
pub fn hit_opacity(elapsed: f32, duration: f32) -> f32 { if duration > 0. { (1. - elapsed / duration).clamp(0., 1.) } else { 0. } }
/// `SFortReticle::Tick`: the muzzle-blocked image fades by `dt / 0.2` per frame (`DAT_144008e28` = 0.2) and hides at <= 1e-4; its offset follows the projected blocked location with
/// `Vector2DInterpTo` speed 14 (`DAT_1440b2898`) and is zeroed the moment the weapon is no longer blocked.
pub const BLOCKED_FADE_SECONDS: f32 = 0.2;
pub const BLOCKED_OFFSET_INTERP_SPEED: f32 = 14.;
/// `FMath::Vector2DInterpTo` (UE4.16): `current + (target - current) * clamp(dt * speed, 0, 1)`, snapping when the remaining distance squared is below `SMALL_NUMBER`.
pub fn vec2_interp_to(cur: [f32; 2], target: [f32; 2], dt: f32, speed: f32) -> [f32; 2] {
    if speed <= 0. { return target; }
    let d = [target[0] - cur[0], target[1] - cur[1]];
    if d[0] * d[0] + d[1] * d[1] < 1.0e-8 { return target; }
    let k = (dt * speed).clamp(0., 1.);
    [cur[0] + d[0] * k, cur[1] + d[1] * k]
}
/// `SFortReticle::UpdateColor` (RVA 0x143063040) multiplies the colour alpha of the centre and corner images by this (native constant `0.333` @0x14401ee20), but ONLY while
/// the weapon `IsReloading()` (vtable +0x7c0), or its remaining-ammo getter (vtable +0x798) returns <= 0, or the muzzle-blocked image is shown (0x6313a..0x6317b, 0x6318c).
pub const RANGED_DIM_ALPHA: f32 = 0.333;
/// `SFortReticle::Tick` (RVA 0x14305c0f0): `0.5 * heat * multiplier` degrees -> `tan` over `tan(0.5 * FOV)` -> times the widget's allotted width * 0.5 (screen pixels).
pub fn spread_radius_px(width_px: f32, spread_deg: f32, fov_deg: f32) -> f32 {
    const DEG: f32 = 0.017453292;
    let t_spread = (0.5 * spread_deg * DEG).tan();
    let t_fov = (fov_deg * 0.5 * DEG).tan();
    let ratio = if t_fov != 0. { t_spread / t_fov } else { 0. };
    ratio * width_px * 0.5
}
/// `SFortReticle` corner image placement (RVA 0x143038f90, the bound render transform; `GetCornerTransform`): image `i` of `n` is rotated `i * 360 / n` degrees about its
/// own centre and translated by `(rx * sin a, -ry * cos a)` with `a = ReticleCornerAngles[i]`, `rx = radius (+ half image width if bReticleCornerOutsideSpreadRadius)`,
/// `ry` likewise with the half height. x uses `sinf`, y `-cosf` of the corner angle (named in the decompile: 0x143ed8c92 = sinf, 0x143ed8c8c = cosf).
pub fn corner_placement(i: usize, n: usize, angle_deg: f32, radius: f32, half_img: (f32, f32), outside: bool) -> ((f32, f32), f32) {
    const DEG: f32 = 0.017453292;
    let rot = i as f32 * 360. / n as f32 * DEG;
    let a = angle_deg * DEG;
    let (rx, ry) = if outside { (radius + half_img.0, radius + half_img.1) } else { (radius, radius) };
    ((rx * a.sin(), -(ry * a.cos())), rot)
}
/// `B_Melee_Generic.ReticleBuildingColor` FColor r,g,b,a.
pub const COLOR_BUILDING: [u8; 4] = [255, 221, 0, 255];
/// `FLinearColor::White` (native default colour of SFortReticle). `ReticleDefaultColor` itself is a native ctor default that is not recoverable: ASSUMED white.
pub const COLOR_DEFAULT: [u8; 4] = [255, 255, 255, 255];
/// Pickaxe `RangeVSBuildings2D` / `RangeVSBuildingsZ` in metres (AthenaMeleeWeapons row Harvest_Pickaxe_Athena_C_T01, 288 / 384 cm).
pub const RANGE_2D: f32 = 2.88;
pub const RANGE_Z: f32 = 3.84;
/// Length of the aim trace in the native colour test (start + dir * 8192 cm), metres.
pub const AIM_TRACE: f32 = 81.92;

pub fn reticle_tint(on_target: bool) -> [f32; 4] { let c = if on_target { COLOR_BUILDING } else { COLOR_DEFAULT }; [c[0] as f32 / 255., c[1] as f32 / 255., c[2] as f32 / 255., c[3] as f32 / 255.] }
/// Pixel rectangle of the reticle image: centred in the (zero-margin) safe zone, snapped to whole pixels like Slate box/image elements.
pub fn reticle_rect(w: f32, h: f32) -> Rect {
    let s = dpi_scale(w, h); let (iw, ih) = (RETICLE_IMAGE.0 * s, RETICLE_IMAGE.1 * s);
    Rect::new(((w - iw) * 0.5).round(), ((h - ih) * 0.5).round(), iw, ih)
}
/// A centred image of `size` Slate units (texture size) at DPI scale `s`.
pub fn centred_image_rect(w: f32, h: f32, size: (f32, f32)) -> Rect {
    let s = dpi_scale(w, h); let (iw, ih) = (size.0 * s, size.1 * s);
    Rect::new(((w - iw) * 0.5).round(), ((h - ih) * 0.5).round(), iw, ih)
}
/// Native hit-notify brush, centered in the same `SFortReticle` overlay as the ranged crosshair.
pub fn ranged_hit_rect(w: f32, h: f32) -> Rect {
    let s = dpi_scale(w, h); let (iw, ih) = (RANGED_HIT_IMAGE.0 * s, RANGED_HIT_IMAGE.1 * s);
    Rect::new(((w - iw) * 0.5).round(), ((h - ih) * 0.5).round(), iw, ih)
}
/// The reach half of the native colour test for a producer that already traced the aim ray: is the hit point within the pickaxe's reach of the pawn?
/// Metres, y up (the horizontal plane is x/z). APPROXIMATE (see module docs): 2D range 288 cm, Z range 384 cm measured from the pawn.
pub fn aim_in_reach(hit: [f32; 3], pawn: [f32; 3]) -> bool {
    let flat = ((hit[0] - pawn[0]).powi(2) + (hit[2] - pawn[2]).powi(2)).sqrt();
    flat <= RANGE_2D && (hit[1] - pawn[1]).abs() <= RANGE_Z
}

// ---- animation curves (MovieScene RichCurves, cubic keys, decoded by the old tools/reticle-evidence.cjs) ---------------------------
/// One cooked RichCurve key (interp 2 = cubic; tangents in value / second).
#[derive(Clone, Copy, Debug)]
pub struct Key { pub t: f32, pub v: f32, pub arrive: f32, pub leave: f32 }
const fn k(t: f32, v: f32) -> Key { Key { t, v, arrive: 0., leave: 0. } }
/// UE FRichCurve::Eval for cubic keys with constant extrapolation (Hermite on each interval).
pub fn eval(keys: &[Key], t: f32) -> f32 {
    let (first, last) = (keys[0], keys[keys.len() - 1]);
    if t <= first.t { return first.v; } if t >= last.t { return last.v; }
    for w in keys.windows(2) { let (a, b) = (w[0], w[1]); if t >= a.t && t <= b.t {
        let dt = b.t - a.t; if dt <= 0. { return b.v; } let s = ((t - a.t) / dt).clamp(0., 1.); let (s2, s3) = (s * s, s * s * s);
        return (2. * s3 - 3. * s2 + 1.) * a.v + (s3 - 2. * s2 + s) * (a.leave * dt) + (-2. * s3 + 3. * s2) * b.v + (s3 - s2) * (b.arrive * dt);
    } }
    last.v
}
/// ResourceAggregationWidget.AggregateOutroAnim (1.0 s): HBox translation Y, HBox scale (X = Y), aggregate text / icon / shadow alpha.
pub const OUTRO_LEN: f32 = 1.0;
pub const OUTRO_HBOX_TY: [Key; 2] = [k(0., 0.), k(1., -120.)];
pub const OUTRO_HBOX_SCALE: [Key; 4] = [k(0., 1.), k(0.05, 1.3), k(0.15, 1.), k(1., 1.)];
pub const OUTRO_TEXT_ALPHA: [Key; 4] = [k(0., 1.), k(0.05, 1.), Key { t: 0.45, v: 1., arrive: -1.05263, leave: -1.05263 }, k(1., 0.)];
pub const OUTRO_ICON_ALPHA: [Key; 3] = [k(0., 1.), Key { t: 0.45, v: 1., arrive: -1., leave: -1. }, k(1., 0.)];
pub const OUTRO_SHADOW_ALPHA: [Key; 4] = [k(0., 0.5), k(0.05, 0.5), Key { t: 0.45, v: 0.5, arrive: -0.52632, leave: -0.52632 }, k(1., 0.)];
/// ResourceIconPulseAnim (0.35 s): icon scale peaks 1.5 at 0.1 s, texts 1.3.
pub const PULSE_LEN: f32 = 0.35;
pub const PULSE_ICON_SCALE: [Key; 3] = [k(0., 1.), k(0.1, 1.5), k(0.35, 1.)];
pub const PULSE_TEXT_SCALE: [Key; 3] = [k(0., 1.), k(0.1, 1.3), k(0.35, 1.)];
/// PotentialResourceWidget.TranslateAndFadeUp (1.0 s): text rises 100 units and fades; the shadow (black, alpha 0.5 -> 0) starts at (1, 2) and rises with it.
pub const POTENTIAL_LEN: f32 = 1.0;
pub const POTENTIAL_TY: [Key; 2] = [k(0., 0.), k(1., -100.)];
pub const POTENTIAL_ALPHA: [Key; 2] = [k(0., 1.), k(1., 0.)];
pub const POTENTIAL_SHADOW_TY: [Key; 2] = [k(0., 2.), k(1., -98.)];
pub const POTENTIAL_SHADOW_ALPHA: [Key; 2] = [k(0., 0.5), k(1., 0.)];
pub const POTENTIAL_SHADOW_TX: f32 = 1.;

// ---- widget numbers (recovered) --------------------------------------------------------------------------------------------------
pub const SLOT_PADDING_RIGHT: f32 = 400.; // OverlaySlot Padding (L0, T0, R400, B300), HAlign Center, VAlign Bottom
pub const SLOT_PADDING_BOTTOM: f32 = 300.;
pub const MAX_WIDTH: f32 = 200.; // root SizeBox MaxDesiredWidth (MinDesiredHeight 300 is not enabled; MaxDesiredHeight 300)
pub const MAX_HEIGHT: f32 = 300.;
pub const ICON_SIZE: f32 = 40.; // ImageResourceIcon brush ImageSize / Standard-Icon-Size[3]
pub const AGG_FONT: f32 = 32.; // TextStyle-Base-L-B (size from TextStyle-Base-L)
pub const AGG_SHADOW: (f32, f32) = (3., 5.); // TextAggregateResourceCollectedShadow RenderTransform translation
pub const POTENTIAL_SCALE: f32 = 1.2; // PotentialResourceWidget RenderTransform scale in the VerticalBox slot
pub const NORMAL_FONT: f32 = 16.; // PotentialResourceWidget NormalFontSize
pub const WEAKPOINT_FONT: f32 = 20.; // WeakpointFontSize
pub const DESIGNER_FONT: f32 = 18.; // TextStyle-Base-S size used until SetupFont runs
pub const HIDE_DELAY: f32 = 3.; // HandleDamagedResourceObject: SetTimer(ForceHideWidget, 3, false)
/// `FortUIStylesheet.Color-HarvestWeakPoint` (linear).
pub const WEAKPOINT_LINEAR: [f32; 3] = [0.127563, 0.847657, 0.875];
pub fn weakpoint_color() -> [f32; 3] { WEAKPOINT_LINEAR.map(crate::screens::srgb) }
/// Icon texture file of each resource (item definition icons: WoodItemData -> T-Icon-Logs, StoneItemData -> T-Icon-S-Loot-Stone, MetalItemData -> T-Icon-IBeam).
pub const RESOURCE_ICONS: [&str; 3] = ["T-Icon-Logs.png", "T-Icon-S-Loot-Stone.png", "T-Icon-IBeam.png"];

// ---- the widget state machine (ResourceAggregationWidget + PotentialResourceWidget bytecode) ------------------------------------
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Potential { pub t: f32, pub amount: u32, pub weak: bool }
#[derive(Clone, Debug)]
pub struct Popup {
    /// `ResourceHoldingObject` (object identity of the prop being harvested).
    pub holding: Option<u64>, pub resource: HarvestResource, pub aggregate: u32,
    /// Widget visibility: false = Collapsed.
    pub visible: bool, pub activate: bool, pub ignore_hide: bool,
    /// `HideWidgetTimer` seconds left.
    pub hide_timer: Option<f32>,
    pub pulse_t: Option<f32>, pub outro_t: Option<f32>,
    /// TranslateAndFadeUp playing (PotentialResourceWidget Visible); None = Hidden.
    pub potential: Option<Potential>,
    /// Last text of the PotentialResourceWidget (a Hidden widget keeps its layout size).
    pub last_potential: Option<(u32, bool)>,
}
impl Default for Popup { fn default() -> Self { Self { holding: None, resource: HarvestResource::Wood, aggregate: 0, visible: false, activate: false, ignore_hide: false, hide_timer: None, pulse_t: None, outro_t: None, potential: None, last_potential: None } } }
impl Popup {
    /// `HandleDamagedResourceObject(DamagedResourceObject, PotentialResource, Destroyed, WeakpointDamage)`.
    pub fn on_event(&mut self, e: HarvestHit) {
        if self.holding != Some(e.object) {
            // ResetSourceOfResource: new holder, icon from the item, aggregate := stack, PlayAnimationTo(AggregateOutroAnim, 0, 1e-6) jumps the outro to t=0
            // (its OnAnimationFinished -> HideWidget only clears bIgnoreHide, so the flag is consumed right away).
            self.holding = Some(e.object); self.resource = e.resource; self.aggregate = e.amount; self.activate = false; self.ignore_hide = false; self.outro_t = None;
        } else { self.aggregate += e.amount; } // UpdateAggregateResourceCollected
        self.pulse_t = Some(0.); // ResourceIconPulseAnim restarts
        self.potential = Some(Potential { t: 0., amount: e.amount, weak: e.weakpoint }); self.last_potential = Some((e.amount, e.weakpoint)); // ShowPotentialResource
        self.visible = true; self.hide_timer = None; // SetVisibility(4); HBox visible; ClearAndInvalidateTimerHandle
        if e.destroyed { self.activate = true; } else { self.hide_timer = Some(HIDE_DELAY); }
    }
    fn trigger_aggregate_animation(&mut self) { if self.activate { self.activate = false; self.ignore_hide = false; self.outro_t = Some(0.); } }
    fn hide_widget(&mut self) { if self.ignore_hide { self.ignore_hide = false; } else { self.visible = false; self.holding = None; } }
    pub fn tick(&mut self, dt: f32) {
        let dt = dt.max(0.);
        if let Some(t) = self.hide_timer { let t = t - dt; if t <= 0. { self.hide_timer = None; self.activate = true; self.trigger_aggregate_animation(); } else { self.hide_timer = Some(t); } } // ForceHideWidget
        if let Some(t) = self.pulse_t { let t = t + dt; self.pulse_t = if t >= PULSE_LEN { None } else { Some(t) }; }
        if let Some(mut p) = self.potential { p.t += dt; if p.t >= POTENTIAL_LEN { self.potential = None; self.trigger_aggregate_animation(); } else { self.potential = Some(p); } } // FinishedShowing
        if let Some(t) = self.outro_t { let t = t + dt; if t >= OUTRO_LEN { self.outro_t = None; self.hide_widget(); } else { self.outro_t = Some(t); } }
    }
    /// Advance by `dt` in steps of at most 1/60 s (like the running game: one big step would collapse the timer + outro chain into a single tick).
    pub fn advance(&mut self, dt: f32) { let mut left = dt; while left > 1e-6 { let h = left.min(1. / 60.); self.tick(h); left -= h; } }
    pub fn active(&self) -> bool { self.visible || self.potential.is_some() }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AggParams { pub hbox_ty: f32, pub hbox_scale: f32, pub text_alpha: f32, pub shadow_alpha: f32, pub icon_alpha: f32, pub icon_scale: f32, pub text_scale: f32 }
/// Animated values of the aggregate row. `outro_t` None = AggregateOutroAnim parked at t=0; `pulse_t` None = ResourceIconPulseAnim at rest.
pub fn agg_params(outro_t: Option<f32>, pulse_t: Option<f32>) -> AggParams {
    let o = outro_t.unwrap_or(0.); let p = pulse_t.unwrap_or(PULSE_LEN);
    AggParams { hbox_ty: eval(&OUTRO_HBOX_TY, o), hbox_scale: eval(&OUTRO_HBOX_SCALE, o), text_alpha: eval(&OUTRO_TEXT_ALPHA, o).clamp(0., 1.), shadow_alpha: eval(&OUTRO_SHADOW_ALPHA, o).clamp(0., 1.), icon_alpha: eval(&OUTRO_ICON_ALPHA, o).clamp(0., 1.),
        icon_scale: eval(&PULSE_ICON_SCALE, p), text_scale: eval(&PULSE_TEXT_SCALE, p) }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PotParams { pub ty: f32, pub shadow_ty: f32, pub alpha: f32, pub shadow_alpha: f32 }
pub fn potential_params(t: f32) -> PotParams { PotParams { ty: eval(&POTENTIAL_TY, t), shadow_ty: eval(&POTENTIAL_SHADOW_TY, t), alpha: eval(&POTENTIAL_ALPHA, t).clamp(0., 1.), shadow_alpha: eval(&POTENTIAL_SHADOW_ALPHA, t).clamp(0., 1.) } }

// ---- layout ---------------------------------------------------------------------------------------------------------------------
/// Uniform scale + translation (RenderTransform compositions of scale about the pivot and translation: UMG applies scale, then translation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xf { pub k: f32, pub bx: f32, pub by: f32 }
impl Xf {
    pub const ID: Xf = Xf { k: 1., bx: 0., by: 0. };
    /// Scale `k` about pivot `c`, then translate by `t`.
    pub fn about(c: (f32, f32), k: f32, t: (f32, f32)) -> Xf { Xf { k, bx: c.0 - k * c.0 + t.0, by: c.1 - k * c.1 + t.1 } }
    /// Apply `self`, then `o`.
    pub fn then(self, o: Xf) -> Xf { Xf { k: o.k * self.k, bx: o.k * self.bx + o.bx, by: o.k * self.by + o.by } }
    pub fn rect(self, r: Rect) -> Rect { Rect::new(self.k * r.x + self.bx, self.k * r.y + self.by, self.k * r.w, self.k * r.h) }
}
pub fn centre(r: Rect) -> (f32, f32) { (r.x + r.w * 0.5, r.y + r.h * 0.5) }

/// Pixel layout of the popup at rest (before any render transform).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupLayout { pub scale: f32, pub widget: Rect, pub potential: Rect, pub hbox: Rect, pub icon: Rect, pub agg_text: Rect }
/// `*_w/_h` are measured text sizes in pixels. Overlay slot: area = screen minus Padding (R400, B300); HAlign Center, VAlign Bottom. Widget = SizeBox(max 200 x 300)
/// around a VerticalBox [Potential (HAlign Right)] [HBox: icon 40 | text].
pub fn popup_layout(w: f32, h: f32, agg_w: f32, agg_h: f32, pot_w: f32, pot_h: f32) -> PopupLayout {
    let s = dpi_scale(w, h); let icon = ICON_SIZE * s;
    let (hbox_w, hbox_h) = (icon + agg_w, icon.max(agg_h));
    let (ww, hh) = (hbox_w.max(pot_w).min(MAX_WIDTH * s), (pot_h + hbox_h).min(MAX_HEIGHT * s));
    let (area_w, area_h) = (w - SLOT_PADDING_RIGHT * s, h - SLOT_PADDING_BOTTOM * s);
    let (x0, y0) = ((area_w - ww) * 0.5, area_h - hh);
    let hbox = Rect::new(x0, y0 + pot_h, ww, hbox_h);
    PopupLayout { scale: s, widget: Rect::new(x0, y0, ww, hh), potential: Rect::new(x0 + ww - pot_w, y0, pot_w, pot_h), hbox,
        icon: Rect::new(x0, hbox.y + (hbox_h - icon) * 0.5, icon, icon), agg_text: Rect::new(x0 + icon, hbox.y + (hbox_h - agg_h) * 0.5, agg_w, agg_h) }
}

/// One laid-out single line of the HUD typeface (NotoSans-Bold): glyph quads relative to the text origin (x = pen start, y = top of the line box).
#[derive(Clone, Debug, Default)]
pub struct TextRun { pub tex: TextureId, pub quads: Vec<(Rect, [f32; 4])>, pub width: f32, pub height: f32 }
/// Lay out `text` at `size` Slate units x `scale` (pixels = size * scale, the mapping every HUD text uses).
pub fn layout_run(ctx: &mut UiContext, text: &str, size: f32, scale: f32) -> TextRun {
    let px = atlas_px(size, scale);
    let (tex, a) = ctx.atlases.atlas(&mut ctx.assets.store, &ctx.fonts, FontId::Bold, px);
    let lh = a.ascent + a.descent.abs(); let baseline = a.ascent;
    let (mut cx, mut quads) = (0f32, Vec::new());
    for ch in text.chars() {
        let Some(g) = a.glyphs.get(&ch) else { continue };
        if g.w > 0. { quads.push((Rect::new(cx + g.xmin, baseline - g.ymin - g.h, g.w, g.h), [g.u0, g.v0, g.u1, g.v1])); }
        cx += g.adv;
    }
    TextRun { tex, quads, width: cx, height: lh }
}
fn push_run(out: &mut RenderList, run: &TextRun, origin: (f32, f32), xf: Xf, tint: [f32; 4]) {
    let snap = (xf.k - 1.).abs() < 1e-3;
    for (rect, uv) in &run.quads {
        let mut r = xf.rect(Rect::new(origin.0 + rect.x, origin.1 + rect.y, rect.w, rect.h));
        if snap { r.x = r.x.round(); r.y = r.y.round(); }
        out.quad(run.tex, r, *uv, tint);
    }
}

// ---- the HUD piece ------------------------------------------------------------------------------------------------------------------
#[derive(Clone, Copy)]
struct Tex { dot: TextureId, icons: [TextureId; 3] }

/// Reticle + harvest popup widget: owns the popup state machine, draws both.
#[derive(Clone, Default)]
pub struct ReticleHud {
    pub popup: Popup,
    tex: Option<Tex>,
    /// Seconds since the last confirmed local firearm impact (None = never / expired); restarts only from an actual impact.
    hit_elapsed: Option<f32>,
    /// `SFortReticle+0x7c0`: muzzle-blocked image alpha, `+0x7a4`: its offset from the centre (pixels), and the offset it is heading to (None = weapon not blocked).
    blocked_alpha: f32, blocked_off: [f32; 2], blocked_target: Option<[f32; 2]>,
}
impl ReticleHud {
    pub fn new() -> Self { Self::default() }
    fn ensure_tex(&mut self, ctx: &mut UiContext) -> Tex {
        if let Some(t) = self.tex { return t; }
        // optional: a missing texture is skipped (TextureId::NONE quads are dropped by the RenderList), never fatal
        let t = Tex { dot: art(ctx, RETICLE_TEXTURE).id, icons: RESOURCE_ICONS.map(|n| art(ctx, n).id) };
        self.tex = Some(t); t
    }
    pub fn on_hit(&mut self, e: HarvestHit) { self.popup.on_event(e); }
    pub fn on_firearm_hit(&mut self) { self.hit_elapsed = Some(0.); }
    /// The muzzle-blocked point of this frame (pixels from the screen centre) or None; the producer is the weapon trace of `AFortWeaponRanged` vfunc 267 (RVA 0x140942c80).
    pub fn set_muzzle_blocked(&mut self, p: Option<[f32; 2]>) { self.blocked_target = p; }
    pub fn tick(&mut self, dt: f32) {
        let dt = dt.max(0.);
        self.popup.tick(dt);
        if let Some(e) = self.hit_elapsed { let e = e + dt; self.hit_elapsed = if e < RANGED_HIT_DURATION { Some(e) } else { None }; }
        // SFortReticle::Tick: blocked -> offset eases to the target and the alpha rises; unblocked -> offset zero at once and the alpha falls
        match self.blocked_target {
            Some(t) => { self.blocked_off = vec2_interp_to(self.blocked_off, t, dt, BLOCKED_OFFSET_INTERP_SPEED); if self.blocked_alpha < 1. { self.blocked_alpha = (self.blocked_alpha + dt / BLOCKED_FADE_SECONDS).clamp(0., 1.); } }
            None => { self.blocked_off = [0., 0.]; if self.blocked_alpha > 0. { self.blocked_alpha = (self.blocked_alpha - dt / BLOCKED_FADE_SECONDS).clamp(0., 1.); } }
        }
        if self.blocked_alpha <= 1.0e-4 && self.blocked_target.is_none() { self.blocked_alpha = 0.; }
    }
    /// Is the muzzle-blocked image visible (the dimming condition of `UpdateColor`)?
    pub fn muzzle_blocked_visible(&self) -> bool { self.blocked_alpha > 1.0e-4 }

    /// The reticle: `HUD_Xhair_Dot` at texture size x DPI scale, centred, white or building yellow.
    pub fn build_reticle(&mut self, ctx: &mut UiContext, w: f32, h: f32, on_target: bool, out: &mut RenderList) {
        let t = self.ensure_tex(ctx);
        out.sprite(t.dot, reticle_rect(w, h), reticle_tint(on_target));
    }
    /// `SFortReticle` for a ranged weapon: the centre image (`ReticleCenterImage`, centred, texture size), the hit-notify image while its timer runs, then one corner image per
    /// `ReticleCornerAngles` entry (`ReticleImage` brush) pushed out by the spread radius (overlay slot order of `Construct`: centre, hit notify, muzzle blocked, corners).
    /// Colour white (`ReticleDefaultColor` / `ReticleBuildingColor` default to white in the weapon ctor 0x914b60, `ReticleEnemyColor` red is never the final value of the ranged
    /// `DetermineReticleColor`), alpha x [`RANGED_DIM_ALPHA`] when [`FirearmReticle::dim`]. A missing texture is a recorded non-fatal asset error (the quad is dropped).
    pub fn build_firearm_reticle(&mut self, ctx: &mut UiContext, w: f32, h: f32, r: &FirearmReticle, out: &mut RenderList) {
        let s = dpi_scale(w, h);
        let tint = [1., 1., 1., if r.dim || self.muzzle_blocked_visible() { RANGED_DIM_ALPHA } else { 1. }];
        if let Some(name) = r.center.as_deref() { let t = art(ctx, name); if t.w > 0 { out.sprite(t.id, centred_image_rect(w, h, (t.w as f32, t.h as f32)), tint); } }
        self.build_firearm_hit(ctx, w, h, r.hit.as_deref().unwrap_or(RANGED_HIT_TEXTURE), out);
        if self.muzzle_blocked_visible() { if let Some(name) = r.muzzle_blocked.as_deref() { let t = art(ctx, name); if t.w > 0 { let mut rc = centred_image_rect(w, h, (t.w as f32, t.h as f32)); rc.x += self.blocked_off[0]; rc.y += self.blocked_off[1]; out.sprite(t.id, rc, [1., 1., 1., self.blocked_alpha]); } } }
        let Some(name) = r.image.as_deref() else { return };
        let t = art(ctx, name);
        if t.w == 0 || r.corner_angles.is_empty() { return; }
        let radius = spread_radius_px(w, r.spread_deg, r.fov_deg);
        let size = (t.w as f32 * s, t.h as f32 * s);
        let n = r.corner_angles.len();
        for (i, a) in r.corner_angles.iter().enumerate() {
            let ((dx, dy), rot) = corner_placement(i, n, *a, radius, (size.0 * 0.5, size.1 * 0.5), r.corner_outside);
            out.rotated(t.id, (w * 0.5 + dx, h * 0.5 + dy), size, rot, tint);
        }
    }
    /// `SFortReticle`'s hit-notify image: texture size, centred, alpha = [`hit_opacity`] (never dimmed: it is not in `UpdateColor`'s list).
    pub fn build_firearm_hit(&mut self, ctx: &mut UiContext, w: f32, h: f32, name: &str, out: &mut RenderList) {
        let Some(e) = self.hit_elapsed else { return };
        let a = hit_opacity(e, RANGED_HIT_DURATION); if a <= 0. { return; }
        let t = art(ctx, name); if t.w == 0 { return; }
        out.sprite(t.id, centred_image_rect(w, h, (t.w as f32, t.h as f32)), [1., 1., 1., a]);
    }

    /// The `+N` popup (aggregate row + potential text), nothing while the widget is Collapsed.
    pub fn build_popup(&mut self, ctx: &mut UiContext, w: f32, h: f32, out: &mut RenderList) {
        let st = self.popup.clone();
        if !st.active() { return; }
        let t = self.ensure_tex(ctx);
        let s = dpi_scale(w, h);
        let (pot_amount, pot_weak) = st.potential.map(|p| (p.amount, p.weak)).or(st.last_potential).unwrap_or((0, false));
        let pot_size = if st.last_potential.is_none() && st.potential.is_none() { DESIGNER_FONT } else if pot_weak { WEAKPOINT_FONT } else { NORMAL_FONT };
        let pot = layout_run(ctx, &format!("+{pot_amount}"), pot_size, s);
        let agg = layout_run(ctx, &format!("+{}", st.aggregate), AGG_FONT, s);
        let l = popup_layout(w, h, agg.width, agg.height, pot.width, pot.height);
        if st.visible {
            let a = agg_params(st.outro_t, st.pulse_t);
            let hbox_xf = Xf::about(centre(l.hbox), a.hbox_scale, (0., a.hbox_ty * s));
            // icon: Image 40x40, ColorAndOpacity alpha from the outro, scale from the pulse (pivot = icon centre)
            let ic = Xf::about(centre(l.icon), a.icon_scale, (0., 0.)).then(hbox_xf);
            out.sprite(t.icons[st.resource.index()], ic.rect(l.icon), [1., 1., 1., a.icon_alpha]);
            let c = centre(l.agg_text);
            let shadow = Xf::about(c, a.text_scale, (AGG_SHADOW.0 * s, AGG_SHADOW.1 * s)).then(hbox_xf);
            let main = Xf::about(c, a.text_scale, (0., 0.)).then(hbox_xf);
            push_run(out, &agg, (l.agg_text.x, l.agg_text.y), shadow, [0., 0., 0., a.shadow_alpha]);
            push_run(out, &agg, (l.agg_text.x, l.agg_text.y), main, [1., 1., 1., a.text_alpha]);
        }
        if let Some(p) = st.potential {
            let pp = potential_params(p.t);
            let c = centre(l.potential); let parent = Xf::about(c, POTENTIAL_SCALE, (0., 0.));
            let main = Xf::about(c, 1., (0., pp.ty * s)).then(parent);
            let shadow = Xf::about(c, 1., (POTENTIAL_SHADOW_TX * s, pp.shadow_ty * s)).then(parent);
            let col = if p.weak { weakpoint_color() } else { [1., 1., 1.] };
            push_run(out, &pot, (l.potential.x, l.potential.y), shadow, [0., 0., 0., pp.shadow_alpha]);
            push_run(out, &pot, (l.potential.x, l.potential.y), main, [col[0], col[1], col[2], pp.alpha]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::testutil::{ctx, evidence};
    fn f(v: &serde_json::Value) -> f32 { v.as_f64().unwrap() as f32 }
    fn keys_of(v: &serde_json::Value) -> Vec<Key> { v["keys"].as_array().unwrap().iter().map(|q| Key { t: f(&q["t"]), v: f(&q["v"]), arrive: f(&q["arrive"]), leave: f(&q["leave"]) }).collect() }
    fn same(a: &[Key], b: &[Key], what: &str) { assert_eq!(a.len(), b.len(), "{what}: key count"); for (x, y) in a.iter().zip(b) { assert!((x.t - y.t).abs() < 1e-4 && (x.v - y.v).abs() < 2e-4 && (x.arrive - y.arrive).abs() < 2e-4 && (x.leave - y.leave).abs() < 2e-4, "{what}: {x:?} vs {y:?}"); } }
    fn ev(obj: u64, amount: u32, destroyed: bool, weak: bool) -> HarvestHit { HarvestHit { object: obj, resource: HarvestResource::Wood, amount, destroyed, weakpoint: weak } }

    // ---- evidence ---------------------------------------------------------------------------------------------------
    #[test] fn reticle_constants_match_the_1_8_data() {
        let e = evidence("reticle-1.8-evidence.json");
        assert_eq!((f(&e["textures"]["HUD_Xhair_Dot"][0]), f(&e["textures"]["HUD_Xhair_Dot"][1])), RETICLE_IMAGE, "FSlateImageBrush size = texture size");
        assert_eq!((f(&e["textures"]["HUD_XHair_HitIndicator_full"][0]), f(&e["textures"]["HUD_XHair_HitIndicator_full"][1])), RANGED_HIT_IMAGE);
        assert!((f(&e["weapon"]["reticle_props_by_blueprint"]["B_Ranged_Generic"]["HitNotifyDisplayDuration"]) - 0.4).abs() < 1e-6, "the actor Blueprint value; the native fade uses the WID default RANGED_HIT_DURATION instead");
        assert!((RANGED_DIM_ALPHA - 0.333).abs() < f32::EPSILON, "native UpdateColor constant 0.333");
        let c = &e["weapon"]["effective_pickaxe"]["ReticleBuildingColor_FColor_rgba"]; for i in 0..4 { assert_eq!(c[i].as_u64().unwrap() as u8, COLOR_BUILDING[i]); }
        assert_eq!(e["weapon"]["reticle_props_by_blueprint"]["B_Melee_Generic"]["ReticleImage"], "HUD_Xhair_Dot#212");
        let b = &e["weapon"]["reticle_props_by_blueprint"]["B_Melee_Generic"]["ReticleBuildingColor"]; assert_eq!((b["r"].as_u64(), b["g"].as_u64(), b["b"].as_u64(), b["a"].as_u64()), (Some(255), Some(221), Some(0), Some(255)));
        assert!(e["weapon"]["weapon_properties_with_reticle"].as_array().unwrap().is_empty(), "the pickaxe item definition has no reticle properties; everything comes from the weapon class");
        for bp in ["B_Melee_Impact_Pickaxe_Athena", "B_Melee_Impact_Generic", "B_Melee_Generic"] { assert!(e["weapon"]["reticle_props_by_blueprint"][bp].get("HitNotifyReticleImage").is_none(), "{bp}: melee has no hit-marker"); }
        assert!(e["weapon"]["reticle_props_by_blueprint"]["B_Ranged_Generic"].get("HitNotifyReticleImage").is_some(), "only the ranged base class has one");
        assert_eq!(e["reticle_widget"]["widget_tree_widgets"], 0, "Reticle_C is an empty widget blueprint (native SFortReticle paints)");
        assert_eq!(e["reticle_widget"]["hud_element_tags"][0], "HUD.Center.Reticle");
        assert_eq!((e["reticle_widget"]["slot"][0]["hAlign"].as_str(), e["reticle_widget"]["slot"][0]["vAlign"].as_str()), (Some("HAlign_Fill"), Some("VAlign_Fill")));
        let s = &e["pickaxe_stats"]["stats"]; assert_eq!(f(&s["RangeVSBuildings2D"]) / 100., RANGE_2D); assert_eq!(f(&s["RangeVSBuildingsZ"]) / 100., RANGE_Z);
        assert!(WEAKPOINT_LINEAR.iter().zip([0.127563f32, 0.847657, 0.875]).all(|(a, b)| (a - b).abs() < 1e-6));
    }
    #[test] fn spread_radius_follows_the_native_tick_formula() {
        assert_eq!(spread_radius_px(1920., 0., 80.), 0.);
        // 3.3 deg full cone, FOV 80, 1920 wide: 960 * tan(1.65 deg) / tan(40 deg)
        let r = spread_radius_px(1920., 3.3, 80.); assert!((r - 960. * (1.65f32.to_radians()).tan() / (40f32.to_radians()).tan()).abs() < 0.05, "{r}");
        assert!(spread_radius_px(1920., 3.3, 15.) > 4. * r, "a narrower FOV magnifies the bloom");
    }
    #[test] fn corner_images_sit_up_right_down_left_and_rotate_with_their_index() {
        let ((x, y), rot) = corner_placement(0, 4, 0., 20., (5., 5.), false); assert!(x.abs() < 1e-4 && (y + 20.).abs() < 1e-4 && rot == 0.);
        let ((x, y), rot) = corner_placement(1, 4, 90., 20., (5., 5.), false); assert!((x - 20.).abs() < 1e-4 && y.abs() < 1e-3 && (rot - 90f32.to_radians()).abs() < 1e-5);
        let ((_, y), _) = corner_placement(2, 4, 180., 20., (5., 5.), true); assert!((y - 25.).abs() < 1e-3, "outside the spread radius adds half the image height");
    }
    #[test] fn ranged_hit_notify_fades_linearly_over_the_wid_duration_and_restarts() {
        assert_eq!(RANGED_HIT_DURATION, 1.0, "UFortWeaponItemDefinition ctor default, not the actor's 0.4");
        assert_eq!(hit_opacity(0., 1.), 1.); assert!((hit_opacity(0.25, 1.) - 0.75).abs() < 1e-6); assert_eq!(hit_opacity(1., 1.), 0.); assert_eq!(hit_opacity(2., 1.), 0.); assert_eq!(hit_opacity(0.1, 0.), 0.);
        let mut h = ReticleHud::new();
        h.on_firearm_hit(); assert_eq!(h.hit_elapsed, Some(0.));
        h.tick(0.9); assert!(h.hit_elapsed.is_some());
        h.on_firearm_hit(); h.tick(0.9); assert!(h.hit_elapsed.is_some(), "a second confirmed hit restarts the timer");
        h.tick(0.2); assert_eq!(h.hit_elapsed, None);
    }
    #[test] fn muzzle_blocked_image_fades_in_over_0_2_s_follows_the_point_and_snaps_its_offset_when_unblocked() {
        let mut h = ReticleHud::new();
        h.set_muzzle_blocked(Some([100., -50.]));
        h.tick(0.1); assert!((h.blocked_alpha - 0.5).abs() < 1e-5 && h.muzzle_blocked_visible());
        assert!((h.blocked_off[0] - 100. * 1.0f32.min(0.1 * 14.)).abs() < 1e-3, "Vector2DInterpTo speed 14: {:?}", h.blocked_off);
        h.tick(0.1); h.tick(0.1); assert_eq!(h.blocked_alpha, 1.);
        h.set_muzzle_blocked(None); h.tick(0.05); assert_eq!(h.blocked_off, [0., 0.]); assert!((h.blocked_alpha - 0.75).abs() < 1e-5);
        h.tick(0.2); assert!(!h.muzzle_blocked_visible());
        assert_eq!(vec2_interp_to([0., 0.], [3., 4.], 1., 14.), [3., 4.]);
    }
    #[test] fn popup_constants_match_the_1_8_widgets() {
        let e = evidence("reticle-1.8-evidence.json"); let p = &e["resource_popup"];
        assert_eq!(p["aggregation_tag"], "HUD.Center.ResourceAggregation");
        assert_eq!((f(&p["aggregation_slot"]["padding"][2]), f(&p["aggregation_slot"]["padding"][3])), (SLOT_PADDING_RIGHT, SLOT_PADDING_BOTTOM));
        assert_eq!((p["aggregation_slot"]["hAlign"].as_str(), p["aggregation_slot"]["vAlign"].as_str()), (Some("HAlign_Center"), Some("VAlign_Bottom")));
        assert_eq!((f(&p["root_sizebox"]["MaxDesiredWidth"]), f(&p["root_sizebox"]["MaxDesiredHeight"])), (MAX_WIDTH, MAX_HEIGHT));
        assert_eq!(p["root_sizebox"]["bOverride_MinDesiredHeight"], false, "MinDesiredHeight 300 is serialised but not enabled");
        assert_eq!(f(&p["icon_designer_brush"]["ImageSize"][0]), ICON_SIZE);
        assert_eq!((f(&p["potential_text"]["NormalFontSize"]), f(&p["potential_text"]["WeakpointFontSize"])), (NORMAL_FONT, WEAKPOINT_FONT));
        assert_eq!(f(&p["aggregate_text"]["shadow_translation"][0]), AGG_SHADOW.0); assert_eq!(f(&p["aggregate_text"]["shadow_translation"][1]), AGG_SHADOW.1);
        assert_eq!(f(&p["potential_text"]["shadow_translation"][0]), POTENTIAL_SHADOW_TX);
        assert!(p["aggregate_text"]["style"].as_str().unwrap().contains("size 32"));
        for (a, b) in p["weakpoint_colour"]["linear_rgb"].as_array().unwrap().iter().zip(WEAKPOINT_LINEAR) { assert!((f(a) - b).abs() < 1e-5); }
    }
    #[test] fn animation_keys_match_the_decoded_movie_scenes() {
        let e = evidence("reticle-1.8-evidence.json"); let a = &e["resource_popup"]["animations"];
        let o = &a["AggregateOutroAnim"]["bindings"];
        same(&OUTRO_HBOX_TY, &keys_of(&o["HBoxTotalAggregation"]["TranslationY"]), "outro hbox ty");
        same(&OUTRO_HBOX_SCALE, &keys_of(&o["HBoxTotalAggregation"]["ScaleX"]), "outro hbox scale x"); same(&OUTRO_HBOX_SCALE, &keys_of(&o["HBoxTotalAggregation"]["ScaleY"]), "outro hbox scale y");
        same(&OUTRO_TEXT_ALPHA, &keys_of(&o["TextAggregateResourceCollected"]["AlphaCurve"]), "outro text alpha");
        same(&OUTRO_ICON_ALPHA, &keys_of(&o["ImageResourceIcon"]["AlphaCurve"]), "outro icon alpha");
        same(&OUTRO_SHADOW_ALPHA, &keys_of(&o["TextAggregateResourceCollectedShadow"]["AlphaCurve"]), "outro shadow alpha");
        let p = &a["ResourceIconPulseAnim"]["bindings"];
        same(&PULSE_ICON_SCALE, &keys_of(&p["ImageResourceIcon"]["ScaleX"]), "pulse icon"); same(&PULSE_TEXT_SCALE, &keys_of(&p["TextAggregateResourceCollected"]["ScaleX"]), "pulse text");
        same(&PULSE_TEXT_SCALE, &keys_of(&p["TextAggregateResourceCollectedShadow"]["ScaleY"]), "pulse shadow");
        let t = &a["TranslateAndFadeUp"]["bindings"];
        same(&POTENTIAL_TY, &keys_of(&t["TextResourceCount"]["TranslationY"]), "potential ty"); same(&POTENTIAL_ALPHA, &keys_of(&t["TextResourceCount"]["AlphaCurve"]), "potential alpha");
        same(&POTENTIAL_SHADOW_TY, &keys_of(&t["TextResourceCountShadow"]["TranslationY"]), "shadow ty"); same(&POTENTIAL_SHADOW_ALPHA, &keys_of(&t["TextResourceCountShadow"]["AlphaCurve"]), "shadow alpha");
        assert_eq!(f(&t["TextResourceCountShadow"]["TranslationX"]["keys"][0]["v"]), POTENTIAL_SHADOW_TX);
        for (len, v) in [(OUTRO_LEN, &o["HBoxTotalAggregation"]["TranslationY"]), (PULSE_LEN, &p["ImageResourceIcon"]["ScaleX"]), (POTENTIAL_LEN, &t["TextResourceCount"]["TranslationY"])] { assert!((len - f(&v["end"])).abs() < 1e-5, "animation length {len}"); }
        assert_eq!(a["AggregateOutroAnim"]["bindings"]["HBoxTotalAggregation"]["TranslationY"]["keys"][0]["interp"], 2, "all keys are cubic");
    }

    // ---- curve evaluation ---------------------------------------------------------------------------------------------
    #[test] fn cubic_curves_are_smoothstep_with_zero_tangents_and_use_the_arrive_tangent() {
        assert!((eval(&POTENTIAL_TY, 0.5) - -50.).abs() < 1e-4 && (eval(&POTENTIAL_TY, 0.25) - -15.625).abs() < 1e-3, "smoothstep: 3s^2-2s^3 at .25 = .15625");
        assert_eq!(eval(&POTENTIAL_TY, 2.), -100.); assert_eq!(eval(&POTENTIAL_TY, -1.), 0.);
        assert!((eval(&PULSE_ICON_SCALE, 0.1) - 1.5).abs() < 1e-6 && (eval(&PULSE_ICON_SCALE, 0.35) - 1.).abs() < 1e-6 && eval(&PULSE_ICON_SCALE, 0.225) > 1.2);
        assert!((eval(&OUTRO_TEXT_ALPHA, 0.45) - 1.).abs() < 1e-6 && eval(&OUTRO_TEXT_ALPHA, 1.).abs() < 1e-6 && (eval(&OUTRO_TEXT_ALPHA, 0.725) - 0.42763).abs() < 1e-3, "0.45..1.0 leaves 0.45 with the cooked tangent -1.05263");
        assert!(eval(&OUTRO_TEXT_ALPHA, 0.3) >= 1., "the arrive tangent overshoots slightly above 1 (clamped when applied)");
        assert!((eval(&OUTRO_HBOX_SCALE, 0.05) - 1.3).abs() < 1e-6 && (eval(&OUTRO_HBOX_SCALE, 0.15) - 1.).abs() < 1e-6 && (eval(&OUTRO_HBOX_TY, 1.) + 120.).abs() < 1e-5);
    }
    #[test] fn animated_parameters_at_rest_and_at_key_times() {
        let r = agg_params(None, None); assert_eq!((r.hbox_ty, r.hbox_scale, r.text_alpha, r.shadow_alpha, r.icon_alpha, r.icon_scale, r.text_scale), (0., 1., 1., 0.5, 1., 1., 1.));
        let o = agg_params(Some(1.), None); assert!(o.text_alpha == 0. && o.icon_alpha == 0. && o.shadow_alpha == 0. && (o.hbox_ty + 120.).abs() < 1e-4);
        let p = agg_params(None, Some(0.1)); assert!((p.icon_scale - 1.5).abs() < 1e-6 && (p.text_scale - 1.3).abs() < 1e-6);
        let q = potential_params(0.); assert_eq!((q.ty, q.shadow_ty, q.alpha, q.shadow_alpha), (0., 2., 1., 0.5));
        let q = potential_params(1.); assert!((q.ty + 100.).abs() < 1e-4 && (q.shadow_ty + 98.).abs() < 1e-4 && q.alpha == 0. && q.shadow_alpha == 0.);
    }

    // ---- state machine (bytecode) -----------------------------------------------------------------------------------
    #[test] fn consecutive_hits_on_one_object_aggregate_and_hide_three_seconds_after_the_last() {
        let mut p = Popup::default(); assert!(!p.active());
        p.on_event(ev(7, 20, false, false)); assert!(p.visible && p.aggregate == 20 && p.holding == Some(7) && p.hide_timer == Some(3.) && p.pulse_t == Some(0.) && p.potential.unwrap().amount == 20);
        p.tick(0.5); p.on_event(ev(7, 20, false, false)); assert_eq!(p.aggregate, 40); assert_eq!(p.hide_timer, Some(3.), "timer restarted by the second hit");
        p.tick(2.9); assert!(p.outro_t.is_none() && p.visible, "{p:?}");
        p.tick(0.2); assert_eq!(p.outro_t.map(|t| t > 0.), Some(true)); assert!(!p.activate, "TriggerAggregateAnimation consumed bActivateAggregateAnim");
        p.tick(1.01); assert!(!p.visible && p.holding.is_none() && p.outro_t.is_none(), "AggregateOutroAnim finished -> HideWidget");
    }
    #[test] fn destroying_hit_waits_for_the_popup_then_plays_the_outro_without_a_timer() {
        let mut p = Popup::default(); p.on_event(ev(1, 20, true, false)); assert!(p.activate && p.hide_timer.is_none());
        p.tick(0.9); assert!(p.outro_t.is_none() && p.potential.is_some(), "outro waits for PotentialResourceWidget.FinishedShowing (1.0 s)");
        p.tick(0.2); assert!(p.potential.is_none() && p.outro_t.is_some() && !p.activate);
        p.tick(1.1); assert!(!p.visible);
    }
    #[test] fn a_different_object_resets_the_aggregate() {
        let mut p = Popup::default(); p.on_event(ev(1, 20, false, false)); p.tick(0.1); p.on_event(ev(1, 20, false, false)); assert_eq!(p.aggregate, 40);
        p.on_event(HarvestHit { object: 2, resource: HarvestResource::Stone, amount: 20, destroyed: false, weakpoint: false }); assert_eq!((p.aggregate, p.holding, p.resource), (20, Some(2), HarvestResource::Stone));
        p.on_event(ev(2, 0, false, true)); assert!(p.potential.unwrap().weak && p.last_potential == Some((0, true)));
    }
    #[test] fn pulse_restarts_per_hit_and_lasts_035_seconds() {
        let mut p = Popup::default(); p.on_event(ev(1, 20, false, false)); p.tick(0.3); assert!(p.pulse_t.is_some()); p.on_event(ev(1, 20, false, false)); assert_eq!(p.pulse_t, Some(0.)); p.tick(0.36); assert!(p.pulse_t.is_none());
    }
    #[test] fn advance_steps_in_sixtieths_like_the_running_game() {
        let mut a = Popup::default(); a.on_event(ev(1, 20, false, false)); a.advance(3.2); assert!(a.outro_t.is_some(), "hide timer expired in small steps: {a:?}");
        let mut b = Popup::default(); b.on_event(ev(1, 20, false, false)); b.tick(3.2); assert!(b.outro_t.is_none() && !b.visible, "one big step collapses timer + outro + hide into a single tick: {b:?}");
    }

    // ---- layout ---------------------------------------------------------------------------------------------------------
    #[test] fn reticle_is_centred_and_texture_sized_at_every_aspect() {
        for (w, h) in [(1920., 1080.), (1280., 720.), (2560., 1080.), (1024., 768.), (640., 480.), (800., 1280.), (3440., 1440.)] {
            let r = reticle_rect(w, h); let s = dpi_scale(w, h);
            assert!((r.w - 58. * s).abs() < 1e-3 && (r.h - 54. * s).abs() < 1e-3, "{w}x{h}");
            assert!((r.x + r.w * 0.5 - w * 0.5).abs() <= 0.5 + 1e-3 && (r.y + r.h * 0.5 - h * 0.5).abs() <= 0.5 + 1e-3, "{w}x{h} centre {:?}", r);
            assert_eq!(r.x, r.x.round()); assert_eq!(r.y, r.y.round());
        }
        assert_eq!(reticle_rect(1920., 1080.), Rect::new(931., 513., 58., 54.));
    }
    #[test] fn reticle_colour_white_until_the_aim_ray_finds_a_prop_within_pickaxe_reach() {
        assert_eq!(reticle_tint(false), [1., 1., 1., 1.]); let y = reticle_tint(true); assert_eq!((y[0], y[2], y[3]), (1., 0., 1.)); assert!((y[1] - 221. / 255.).abs() < 1e-6);
        let pawn = [0., 1.6, 12.];
        assert!(aim_in_reach([0., 1.5, 10.2], pawn), "tree 1.8 m ahead of the pawn");
        assert!(!aim_in_reach([0., 1.5, 4.], pawn), "8 m away is out of the 2.88 m reach");
        assert!(!aim_in_reach([0., 6., 10.2], pawn), "4.4 m above is out of the 3.84 m Z reach");
        assert!(aim_in_reach([2.88, 1.6, 12.], pawn) && !aim_in_reach([2.89, 1.6, 12.], pawn), "the 2D range is inclusive at 288 cm");
    }
    #[test] fn popup_sits_left_of_centre_and_300_units_above_the_bottom() {
        let l = popup_layout(1920., 1080., 80., 44., 40., 22.);
        assert_eq!(l.scale, 1.); assert_eq!(l.widget.y + l.widget.h, 1080. - 300.); assert!((l.widget.x + l.widget.w * 0.5 - (1920. - 400.) * 0.5).abs() < 1e-3);
        assert_eq!(l.widget.w, 120.); assert_eq!(l.widget.h, 22. + 44.);
        assert_eq!(l.potential.x + l.potential.w, l.widget.x + l.widget.w, "Potential is right aligned"); assert_eq!(l.potential.y, l.widget.y);
        assert_eq!(l.hbox.y, l.potential.y + 22.); assert_eq!((l.icon.w, l.icon.h), (40., 40.)); assert_eq!(l.agg_text.x, l.icon.x + 40.);
        assert!((l.icon.y + 20. - (l.hbox.y + 22.)).abs() < 1e-3, "icon vertically centred in the row");
        let c = popup_layout(1920., 1080., 400., 44., 40., 22.); assert_eq!(c.widget.w, 200., "SizeBox MaxDesiredWidth 200");
        let s = popup_layout(1280., 720., 80., 44., 40., 22.); assert!((s.widget.y + s.widget.h - (720. - 300. * (2. / 3.))).abs() < 1e-2 && s.scale < 0.7);
    }
    #[test] fn transforms_compose_scale_about_pivot_then_translation() {
        let r = Rect::new(10., 10., 20., 10.); let c = centre(r); let x = Xf::about(c, 2., (5., 0.)).rect(r);
        assert!((x.x - (c.0 - 20. + 5.)).abs() < 1e-4 && (x.w - 40.).abs() < 1e-4 && ((x.x + x.w * 0.5) - (c.0 + 5.)).abs() < 1e-4, "{x:?}");
        let both = Xf::about(c, 1., (0., -10.)).then(Xf::about(c, 1.2, (0., 0.))).rect(r); assert!((both.y + both.h * 0.5 - (c.1 + 1.2 * -10.)).abs() < 1e-3, "child translation is scaled by the parent's 1.2: {both:?}");
        assert_eq!(Xf::ID.rect(r), r);
    }
    #[test] fn weakpoint_colour_is_cyan() { let c = weakpoint_color(); assert!(c[1] > 0.9 && c[2] > 0.9 && c[0] < 0.5 && c[0] > 0.3, "{c:?}"); assert_eq!(crate::screens::srgb(0.), 0.); assert!((crate::screens::srgb(1.) - 1.).abs() < 1e-6); }

    // ---- drawing ------------------------------------------------------------------------------------------------------------
    fn quads_of(l: &RenderList) -> Vec<crate::draw::Quad> { l.cmds.iter().filter_map(|c| if let crate::draw::Cmd::Quad(q) = c { Some(*q) } else { None }).collect() }
    #[test] fn the_reticle_is_one_centred_quad_that_turns_yellow() {
        let Some(mut c) = ctx() else { return };
        let mut r = ReticleHud::new();
        for (on, want) in [(false, [1., 1., 1., 1.]), (true, reticle_tint(true))] {
            let mut out = RenderList::new(); r.build_reticle(&mut c, 1920., 1080., on, &mut out);
            let q = quads_of(&out); assert_eq!(q.len(), 1); assert_eq!(q[0].tint, want); assert_eq!(q[0].bounds(), Rect::new(931., 513., 58., 54.));
        }
        assert_eq!(c.assets.texture(RETICLE_TEXTURE).w, 58); assert_eq!(c.assets.texture(RETICLE_TEXTURE).h, 54);
    }
    #[test] fn the_popup_draws_nothing_when_idle_and_text_plus_icon_after_a_hit() {
        let Some(mut c) = ctx() else { return };
        let mut r = ReticleHud::new(); let mut out = RenderList::new(); r.build_popup(&mut c, 1920., 1080., &mut out); assert!(out.is_empty());
        r.on_hit(ev(1, 20, false, false)); r.popup.advance(0.25); r.build_popup(&mut c, 1920., 1080., &mut out);
        let q = quads_of(&out); assert!(q.len() >= 1 + 2 * 3 + 2 * 3, "icon + '+20' x2 (shadow+main) + '+20' potential x2: {}", q.len());
        // weak point: the potential text is cyan
        let mut r = ReticleHud::new(); r.on_hit(ev(1, 20, false, true)); r.popup.advance(0.1); let mut out = RenderList::new(); r.build_popup(&mut c, 1920., 1080., &mut out);
        let w = weakpoint_color(); assert!(quads_of(&out).iter().any(|q| (q.tint[0] - w[0]).abs() < 1e-5 && (q.tint[1] - w[1]).abs() < 1e-5), "cyan glyphs present");
    }
}
