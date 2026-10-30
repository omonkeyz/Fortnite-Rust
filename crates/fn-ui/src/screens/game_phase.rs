use crate::draw::{Cmd, MaterialKind, RenderList};
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::{atlas_px, centred_baseline, put_text, text_width, Snap, WHITE};
use crate::text::FontId;

pub const SMALL_ICON: f32 = 32.;
pub const SMALL_TEXT_PAD: f32 = 4.;
pub const SMALL_FONT: f32 = 18.;
pub const BANNER_TOP: f32 = 200.;
pub const BANNER_ICON: f32 = 64.;
pub const BANNER_MESSAGE_FONT: f32 = 18.;
pub const BANNER_MESSAGE_PAD_RIGHT: f32 = 4.;
pub const BANNER_MESSAGE_SHADOW: ([f32; 2], [f32; 4]) = ([0., 2.], [0., 0., 0., 0.34]);
pub const BANNER_TIME_FONT: f32 = 24.;
pub const BANNER_TIME_PAD_TOP: f32 = 1.5;
pub const BANNER_TIME_COLOR_LINEAR: [f32; 3] = [1.696782, 0., 1.];
pub const BANNER_TIME_SHADOW: ([f32; 2], [f32; 4]) = ([1., 3.], [0., 0., 0., 0.34]);
pub const MODE_CHANGED_LENGTH: f32 = 6.;
pub const ICON_PULSE_LENGTH: f32 = 0.65;
pub const ICON_PIVOT: [f32; 2] = [0., 1.];
pub const ICON_SLIDE_LENGTH: f32 = 1.5;

pub fn message(step: u8) -> &'static str {
    match step {
        1 => "WAITING FOR PLAYERS...",
        2 => "WAITING FOR PLAYERS",
        3 => "BATTLE BUS LAUNCHING IN",
        4 => "DOORS WILL OPEN IN",
        5 => "EVERYBODY OFF, LAST STOP IN",
        6 => "STORM FORMING IN",
        7 => "STORM EYE SHRINKS IN",
        8 => "STORM EYE SHRINKING",
        9 => "GAME OVER, RETURNING TO LOBBY IN",
        _ => "",
    }
}

pub const LOC_NAMESPACE: &str = "GamePhaseChanging";
pub const LOCRES_NAME: &str = "Game.locres";
pub const LOCRES_DIR: &str = "Localization/Game/en/";

pub fn message_key(step: u8) -> Option<&'static str> {
    match step {
        1 => Some("WaitingForContendersWarmup"),
        2 => Some("WaitingForPlayers"),
        3 => Some("AircraftStartingIn"),
        4 => Some("AircraftUnlockingIn"),
        5 => Some("AircraftEndingTime"),
        6 => Some("StormFormingIn"),
        7 => Some("StormShrinkingIn"),
        8 => Some("StormEyeShrinking"),
        9 => Some("GameOverReturning"),
        _ => None,
    }
}

pub const SECONDS_FORMAT: (&str, &str) = ("SecondsRemainingFormat", "{0} Seconds");
pub const MINUTES_SECONDS_FORMAT: (&str, &str) = ("MinutesSecondsRemainingFormat", "{0} Minutes {1} Seconds");

#[derive(Clone, Debug, PartialEq)]
pub struct PhaseTexts { pub messages: [String; 10], pub seconds: String, pub minutes_seconds: String }

impl Default for PhaseTexts {
    fn default() -> Self { PhaseTexts { messages: std::array::from_fn(|i| message(i as u8).to_owned()), seconds: SECONDS_FORMAT.1.to_owned(), minutes_seconds: MINUTES_SECONDS_FORMAT.1.to_owned() } }
}

impl PhaseTexts {
    pub fn from_locres(get: impl Fn(&str, &str) -> Option<String>) -> PhaseTexts {
        let d = PhaseTexts::default();
        PhaseTexts {
            messages: std::array::from_fn(|i| message_key(i as u8).and_then(|k| get(LOC_NAMESPACE, k)).unwrap_or_else(|| d.messages[i].clone())),
            seconds: get(LOC_NAMESPACE, SECONDS_FORMAT.0).unwrap_or(d.seconds),
            minutes_seconds: get(LOC_NAMESPACE, MINUTES_SECONDS_FORMAT.0).unwrap_or(d.minutes_seconds),
        }
    }
    pub fn message(&self, step: u8) -> &str { self.messages.get(step as usize).map_or("", |s| s.as_str()) }
    pub fn banner_time(&self, step: u8, t: f32) -> String {
        if step == 1 { return String::new(); }
        let secs = floor_to_int(t);
        if secs < 60 { self.seconds.replace("{0}", &(secs % 60).to_string()) } else { self.minutes_seconds.replace("{0}", &(secs / 60).to_string()).replace("{1}", &(secs % 60).to_string()) }
    }
}

pub fn floor_to_int(t: f32) -> i32 { ((t + t - 0.5).round() as i32) >> 1 }

pub fn banner_time(step: u8, t: f32) -> String { PhaseTexts::default().banner_time(step, t) }

pub fn small_time(t: f32) -> String {
    let s = t.trunc() as i32;
    format!("{}:{:02}", s / 60, s % 60)
}

pub fn alert_sound(step: u8) -> bool { (1..=8).contains(&step) }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key { pub t: f32, pub v: f32, pub interp: u8, pub arrive: f32, pub leave: f32 }
const fn k(t: f32, v: f32, interp: u8, arrive: f32) -> Key { Key { t, v, interp, arrive, leave: arrive } }

pub fn eval_curve(keys: &[Key], t: f32) -> f32 {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else { return 0. };
    if t <= first.t { return first.v; }
    if t >= last.t { return last.v; }
    for w in keys.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t < b.t {
            let dt = b.t - a.t;
            let u = (t - a.t) / dt;
            return match a.interp {
                1 => a.v,
                2 => {
                    let (p0, p3) = (a.v, b.v);
                    let p1 = p0 + a.leave * dt / 3.;
                    let p2 = p3 - b.arrive * dt / 3.;
                    let v = 1. - u;
                    v * v * v * p0 + 3. * v * v * u * p1 + 3. * v * u * u * p2 + u * u * u * p3
                }
                _ => a.v + (b.v - a.v) * u,
            };
        }
    }
    last.v
}

pub const MODE_ALPHA: [Key; 4] = [k(0., 0., 2, 0.), k(0.3, 1., 0, 0.333333), k(5.7, 1., 2, -0.175439), k(6., 0., 2, 0.)];
pub const MODE_TX: [Key; 4] = [k(0., 700., 2, 0.), k(0.3, 0., 0, -233.333328), k(5.7, 0., 2, 122.807022), k(6., 700., 2, 0.)];
pub const MODE_TY: [Key; 4] = [k(0., 130., 2, 0.), k(0.3, 0., 0, -33.333332), k(5.7, 0., 2, 17.543859), k(6., 130., 2, 0.)];
pub const MODE_SCALE: [Key; 4] = [k(0., 0., 2, 0.), Key { t: 0.3, v: 1., interp: 1, arrive: 0.3, leave: 0. }, k(5.7, 1., 2, -0.175439), k(6., 0., 2, 0.)];
pub const PULSE_TY: [Key; 4] = [k(0., 0., 1, 0.), k(0.4, 0., 2, 11.111112), k(0.45, 5., 2, 0.), k(0.55, 0., 2, 0.)];
pub const PULSE_SCALE: [Key; 5] = [k(0., 1., 1, 0.), k(0.4, 1., 2, -0.222222), k(0.45, 0.9, 2, 3.333333), k(0.55, 1.5, 2, 0.5), k(0.65, 1., 2, 0.)];
pub const SLIDE_TX: [Key; 3] = [k(0., -150., 0, 0.), k(0.45, -150., 2, 100.), k(1.5, 0., 2, 0.)];
pub const SLIDE_SCALE_X: [Key; 3] = [k(0.36, 1., 2, 0.), k(0.39, 0.5, 2, 0.), k(0.5, 1., 2, 0.)];

#[derive(Clone, Debug, Default)]
pub struct GamePhaseHud { last_step: Option<u8>, mode_t: Option<f32>, icon_t: f32, slide: bool, texts: Option<PhaseTexts>, fallback: PhaseTexts, missed: bool }

impl GamePhaseHud {
    pub fn texts(&mut self, ctx: &UiContext) -> &PhaseTexts {
        if self.texts.is_none() {
            match ctx.assets.source.read_in(LOCRES_NAME, LOCRES_DIR).and_then(|b| crate::screens::athena_lobby::store::Locres::parse(&b)) {
                Some(l) => self.texts = Some(PhaseTexts::from_locres(|ns, k| l.get(ns, k).map(str::to_owned))),
                None => if !self.missed { self.missed = true; eprintln!("[game phase] no {LOCRES_DIR}{LOCRES_NAME} yet: the banner uses the source texts"); },
            }
        }
        self.texts.as_ref().unwrap_or(&self.fallback)
    }
    pub fn tick(&mut self, phase: Option<(u8, f32)>, dt: f32) -> bool {
        let mut changed = false;
        if let Some((step, _)) = phase {
            if self.last_step != Some(step) {
                self.last_step = Some(step);
                if step != 0 { self.mode_t = Some(0.); self.icon_t = 0.; self.slide = step == 8; changed = true; }
            } else if let Some(t) = self.mode_t.as_mut() {
                *t += dt.max(0.);
                self.icon_t += dt.max(0.);
                if *t >= MODE_CHANGED_LENGTH { self.mode_t = None; }
            }
        } else { self.last_step = None; self.mode_t = None; }
        changed
    }
    pub fn banner_visible(&self) -> bool { self.mode_t.is_some() }

    pub fn build_small(&self, ctx: &mut UiContext, w: f32, h: f32, right_x: f32, row_y: f32, row_h: f32, phase: (u8, f32), icon: Option<MaterialKind>, out: &mut RenderList) -> f32 {
        let s = dpi_scale(w, h);
        let px = atlas_px(SMALL_FONT, s);
        let txt = small_time(phase.1);
        let tw = text_width(ctx, FontId::Bold, &txt, px);
        let width = SMALL_ICON * s + SMALL_TEXT_PAD * s + tw;
        let x = right_x - width;
        let cy = row_y + row_h * 0.5;
        if let Some(m) = icon { out.material(m, Rect::new(x, cy - SMALL_ICON * s * 0.5, SMALL_ICON * s, SMALL_ICON * s), WHITE, false); }
        let lh = crate::screens::line_height(ctx, FontId::Bold, px).max(1.);
        let base = centred_baseline(ctx, FontId::Bold, px, cy - lh * 0.5, lh);
        put_text(ctx, out, FontId::Bold, px, x + (SMALL_ICON + SMALL_TEXT_PAD) * s, base, &txt, WHITE, None, Snap::PerGlyph);
        width
    }

    pub fn build_banner(&mut self, ctx: &mut UiContext, w: f32, h: f32, phase: (u8, f32), icon: Option<MaterialKind>, out: &mut RenderList) {
        let Some(t) = self.mode_t else { return };
        let s = dpi_scale(w, h);
        let (mpx, tpx) = (atlas_px(BANNER_MESSAGE_FONT, s), atlas_px(BANNER_TIME_FONT, s));
        let texts = self.texts(ctx);
        let msg = texts.message(phase.0).to_owned();
        let time = texts.banner_time(phase.0, phase.1);
        let msg = msg.as_str();
        let mw = text_width(ctx, FontId::Bold, msg, mpx);
        let tw = text_width(ctx, FontId::Title, &time, tpx);
        let mlh = crate::screens::line_height(ctx, FontId::Bold, mpx).max(1.);
        let tlh = crate::screens::line_height(ctx, FontId::Title, tpx).max(1.);
        let row_h = mlh.max(tlh + BANNER_TIME_PAD_TOP * s);
        let row_w = mw + BANNER_MESSAGE_PAD_RIGHT * s + tw;
        let icon_sz = BANNER_ICON * s;
        let total_w = row_w.max(icon_sz);
        let x0 = (w - total_w) * 0.5;
        let y0 = BANNER_TOP * s;
        let start = out.cmds.len();
        let icon_rect = Rect::new(x0 + (total_w - icon_sz) * 0.5, y0, icon_sz, icon_sz);
        if let Some(m) = icon {
            let it = self.icon_t;
            let (tx, ty, sx, sy) = if self.slide { (eval_curve(&SLIDE_TX, it), 0., eval_curve(&SLIDE_SCALE_X, it), 1.) } else { let sc = eval_curve(&PULSE_SCALE, it); (0., eval_curve(&PULSE_TY, it), sc, sc) };
            let (cw, ch) = (icon_rect.w * sx, icon_rect.h * sy);
            let r = Rect::new(icon_rect.x + tx * s, icon_rect.y + icon_rect.h - ch + ty * s, cw, ch);
            out.material(m, Rect::new(r.x + (icon_rect.w - cw) * ICON_PIVOT[0], r.y, r.w, r.h), WHITE, false);
        }
        let ry = y0 + icon_sz;
        let rx = x0 + (total_w - row_w) * 0.5;
        let mbase = centred_baseline(ctx, FontId::Bold, mpx, ry + (row_h - mlh) * 0.5, mlh);
        put_text(ctx, out, FontId::Bold, mpx, rx, mbase, msg, WHITE, Some(([BANNER_MESSAGE_SHADOW.0[0] * s, BANNER_MESSAGE_SHADOW.0[1] * s], BANNER_MESSAGE_SHADOW.1)), Snap::PerGlyph);
        let c = crate::screens::srgb3(BANNER_TIME_COLOR_LINEAR);
        let tbase = centred_baseline(ctx, FontId::Title, tpx, ry + BANNER_TIME_PAD_TOP * s + (row_h - BANNER_TIME_PAD_TOP * s - tlh) * 0.5, tlh);
        put_text(ctx, out, FontId::Title, tpx, rx + mw + BANNER_MESSAGE_PAD_RIGHT * s, tbase, &time, [c[0], c[1], c[2], 1.], Some(([BANNER_TIME_SHADOW.0[0] * s, BANNER_TIME_SHADOW.0[1] * s], BANNER_TIME_SHADOW.1)), Snap::PerGlyph);
        let alpha = eval_curve(&MODE_ALPHA, t).clamp(0., 1.);
        let scale = eval_curve(&MODE_SCALE, t);
        let (tx, ty) = (eval_curve(&MODE_TX, t) * s, eval_curve(&MODE_TY, t) * s);
        let pivot = (x0 + total_w * 0.5, y0 + (h - y0) * 0.5);
        transform_since(out, start, pivot, scale, (tx, ty), alpha);
    }
}

pub fn icon_parts(step: u8) -> Option<(u8, &'static str)> {
    match step {
        1 | 9 => Some((4, "T-Icon-Check-52.png")),
        2 => Some((0, "T_Clock.png")),
        3 | 4 => Some((0, "T_Bus.png")),
        5 => Some((1, "T_Jump.png")),
        6 | 7 => Some((2, "T_StormHolding.png")),
        8 => Some((3, "T_StormMoving.png")),
        _ => None,
    }
}

pub fn prewarm(ctx: &mut UiContext) { for s in 1..=9 { let _ = icon(ctx, s); } }

pub fn icon(ctx: &mut UiContext, step: u8) -> Option<MaterialKind> {
    let (shader, name) = icon_parts(step)?;
    let bg = crate::screens::mat_tex(ctx, "T_GamePhaseBackground.png", false).id;
    let icon = crate::screens::mat_tex(ctx, name, false).id;
    if bg == crate::draw::TextureId::NONE || icon == crate::draw::TextureId::NONE { return None; }
    Some(MaterialKind::GamePhaseIcon { shader, bg, icon })
}

pub fn transform_since(out: &mut RenderList, start: usize, pivot: (f32, f32), scale: f32, translate: (f32, f32), alpha: f32) {
    let xf = |p: [f32; 2]| [pivot.0 + (p[0] - pivot.0) * scale + translate.0, pivot.1 + (p[1] - pivot.1) * scale + translate.1];
    for cmd in out.cmds.iter_mut().skip(start) {
        match cmd {
            Cmd::Quad(q) => { for p in q.pos.iter_mut() { *p = xf(*p); } q.tint[3] *= alpha; }
            Cmd::Material(m) => {
                let a = xf([m.rect.x, m.rect.y]);
                m.rect = Rect::new(a[0], a[1], m.rect.w * scale, m.rect.h * scale);
                m.tint[3] *= alpha;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn texts_follow_the_native_formats() {
        assert_eq!(small_time(200.9), "3:20");
        assert_eq!(small_time(59.99), "0:59");
        assert_eq!(small_time(5.), "0:05");
        assert_eq!(banner_time(7, 200.4), "3 Minutes 20 Seconds");
        assert_eq!(banner_time(6, 59.7), "59 Seconds");
        assert_eq!(banner_time(8, 60.2), "1 Minutes 0 Seconds");
        assert_eq!(banner_time(1, 10.), "");
        assert_eq!(message(6), "STORM FORMING IN");
        assert_eq!(message(8), "STORM EYE SHRINKING");
        assert!(alert_sound(8) && !alert_sound(9) && !alert_sound(0));
    }
    #[test] fn banner_texts_come_from_the_cooked_locres() {
        let p = crate::pak_assets::default_install();
        if !p.exists() { return; }
        use crate::assets::AssetSource;
        let src = crate::pak_assets::PakAssets::open(&p).unwrap();
        let l = crate::screens::athena_lobby::store::Locres::parse(&src.read_in(LOCRES_NAME, LOCRES_DIR).unwrap()).unwrap();
        for step in 1..=9u8 { assert!(l.get(LOC_NAMESPACE, message_key(step).unwrap()).is_some(), "step {step}"); }
        let t = PhaseTexts::from_locres(|ns, k| l.get(ns, k).map(str::to_owned));
        assert_eq!(t, PhaseTexts::default());
        assert_eq!(t.message(6), "STORM FORMING IN");
        assert_eq!(t.banner_time(7, 200.4), "3 Minutes 20 Seconds");
    }
    #[test] fn floor_to_int_matches_the_exe_rounding() {
        assert_eq!(floor_to_int(3.99), 3);
        assert_eq!(floor_to_int(4.0), 4);
        assert_eq!(floor_to_int(0.2), 0);
    }
    #[test] fn mode_changed_slides_in_over_0_3_s_and_out_over_the_last_0_3_s() {
        assert_eq!(eval_curve(&MODE_TX, 0.), 700.);
        assert_eq!(eval_curve(&MODE_TX, 0.3), 0.);
        assert_eq!(eval_curve(&MODE_TX, 3.), 0.);
        assert_eq!(eval_curve(&MODE_TX, 6.), 700.);
        assert_eq!(eval_curve(&MODE_ALPHA, 0.), 0.);
        assert!((eval_curve(&MODE_ALPHA, 2.) - 1.).abs() < 1e-6);
        assert!((eval_curve(&MODE_SCALE, 0.3) - 1.).abs() < 1e-6 && (eval_curve(&MODE_SCALE, 1.) - 1.).abs() < 1e-6);
    }
    #[test] fn a_step_change_shows_the_banner_for_six_seconds() {
        let mut g = GamePhaseHud::default();
        assert!(g.tick(Some((6, 60.)), 0.016));
        assert!(g.banner_visible());
        for _ in 0..380 { assert!(!g.tick(Some((6, 50.)), 0.016)); }
        assert!(!g.banner_visible());
        assert!(g.tick(Some((7, 200.)), 0.016));
        assert!(!GamePhaseHud::default().tick(Some((0, 0.)), 0.016));
    }
}
