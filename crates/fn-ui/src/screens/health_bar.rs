use crate::draw::RenderList;
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::{art, ascent, atlas_px, line_height, put_text, srgb, Snap, WHITE};
use crate::text::FontId;

pub const BAR_W: f32 = 180.;
pub const BAR_H: f32 = 14.;
pub const BORDER_PADDING: f32 = 2.;
pub const FONT_SIZE: f32 = 12.;
pub const TEXT_SHADOW: ([f32; 2], [f32; 4]) = ([-1., 1.], [0., 0., 0., 1.]);
pub const GRADIENT: [[(f32, f32); 4]; 3] = [
    [(0.000_408_887_86, 1.), (0.248_964_13, 0.830_000_04), (0.627_934_2, 0.952_534_2), (1.000_767_1, 0.)],
    [(0.000_408_887_86, 5.364_418e-7), (0.248_964_13, 3.957_748_6e-7), (0.627_934_2, 1.), (1.000_767_1, 1.)],
    [(0.000_408_887_86, 0.), (0.248_964_13, 0.), (0.627_934_2, 0.009_999_99), (1.000_767_1, 0.)],
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HealthBarView { pub screen: (f32, f32), pub health: f32, pub max_health: f32, pub display_text: String }

pub fn percent(health: f32, max: f32) -> f32 { if max > 0. { (health / max).clamp(0., 1.) } else { 0. } }

fn curve(keys: &[(f32, f32); 4], t: f32) -> f32 {
    if t <= keys[0].0 { return keys[0].1; }
    for w in keys.windows(2) { if t <= w[1].0 { let a = (t - w[0].0) / (w[1].0 - w[0].0).max(1e-9); return w[0].1 + (w[1].1 - w[0].1) * a; } }
    keys[3].1
}
pub fn fill_color(p: f32) -> [f32; 3] { [curve(&GRADIENT[0], p), curve(&GRADIENT[1], p), curve(&GRADIENT[2], p)] }
pub fn hp_text(health: f32, max: f32) -> String { format!("HP : {} / {}", health as i32, max as i32) }

pub fn build(ctx: &mut UiContext, w: f32, h: f32, bars: &[HealthBarView], out: &mut RenderList) {
    if bars.is_empty() { return; }
    let s = dpi_scale(w, h);
    let back = art(ctx, "ProgressBar_Background.png");
    let fill = art(ctx, "ProgressBar_Fill.png");
    let px = atlas_px(FONT_SIZE, s);
    let lh_n = line_height(ctx, FontId::Regular, px);
    let asc_n = ascent(ctx, FontId::Regular, px);
    let lh_b = line_height(ctx, FontId::Bold, px);
    let asc_b = ascent(ctx, FontId::Bold, px);
    for b in bars {
        let total = lh_b + BAR_H * s + lh_n;
        let top = b.screen.1 - total * 0.5;
        let cx = b.screen.0;
        if !b.display_text.is_empty() {
            let tw = ctx.fonts.measure(FontId::Bold, &b.display_text, px as f32);
            put_text(ctx, out, FontId::Bold, px, (cx - tw * 0.5).round(), (top + asc_b).round(), &b.display_text, WHITE, Some(TEXT_SHADOW), Snap::Pen);
        }
        let row = Rect::new((cx - BAR_W * s * 0.5).round(), (top + lh_b).round(), BAR_W * s, BAR_H * s);
        out.sprite(back.id, row, WHITE);
        let p = percent(b.health, b.max_health);
        let inner = Rect::new(row.x + BORDER_PADDING * s, row.y + BORDER_PADDING * s, (row.w - 2. * BORDER_PADDING * s).max(0.) * p, (row.h - 2. * BORDER_PADDING * s).max(0.));
        let c = fill_color(p);
        if inner.w > 0.5 { out.sprite(fill.id, inner, [srgb(c[0]), srgb(c[1]), srgb(c[2]), 1.]); }
        let text = hp_text(b.health, b.max_health);
        let tw = ctx.fonts.measure(FontId::Regular, &text, px as f32);
        put_text(ctx, out, FontId::Regular, px, (cx - tw * 0.5).round(), (row.y + row.h + asc_n).round(), &text, WHITE, Some(TEXT_SHADOW), Snap::Pen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_gradient_runs_red_to_green() {
        let r = fill_color(0.); assert!(r[0] > 0.99 && r[1] < 1e-3);
        let g = fill_color(1.); assert!(g[0] < 0.01 && g[1] > 0.99);
        let y = fill_color(0.6279342); assert!((y[0] - 0.9525342).abs() < 1e-4 && (y[1] - 1.).abs() < 1e-4);
    }
    #[test] fn the_text_and_percent_follow_the_widget() {
        assert_eq!(hp_text(1249.6, 1250.), "HP : 1249 / 1250");
        assert_eq!(percent(625., 1250.), 0.5); assert_eq!(percent(-3., 1250.), 0.); assert_eq!(percent(1., 0.), 0.);
    }
}
