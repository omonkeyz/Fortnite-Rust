//! Loading screen as pure state -> [`RenderList`] (the old `loading.rs` minus the Win32 splash window, message pump, GL fonts and loader driver).
//!
//! ORIGINAL DATA: `Splash.bmp` (Unreal splash window artwork), `FORT_Load_BattleRoyale` (the
//! `BackgroundImage` of `LSID_DefaultBG`), the `ProgressBar_*` textures and the
//! `AthenaLoadingScreenTips` strings (see `tools/loading-evidence.cjs`, `reports/loading-screen.json`).
//! APPROXIMATE: all placement/sizing. No loading-screen widget blueprint exists in the pak index, so the
//! cover scaling, bar position, fonts (Segoe UI stands in for Burbank) and fade timings are provisional.
//!
//! PORT NOTES (differences from the old runtime):
//! * TEXT: the old screen rasterised Segoe UI with GDI (`CreateFontW` weights 700/600, integer `GetCharWidth32W` advances). This port draws the Segoe UI system faces (NotoSans-Bold when absent) through the
//!   shared fontdue atlases ([`crate::text::TextAtlases`]); `wrap`/`fit` take a width function (fontdue float advances) instead of the `[i32; 128]` GDI table.
//!   Glyph shapes, advances and therefore wrap points differ slightly from the old captures (APPROXIMATION). The user's reference draws Segoe UI (700 tip / 600 label), so the same system faces are used here (`FontId::SegoeBold` / `SegoeSemibold`); fontdue vs GDI rasterisation differs slightly.
//! * Dropped as OS code: `Splash` window (+`capture`), `pump`, `enter_down`/`foreground_is_ours`, GL `Font`/`Screen::draw`, `Loader`, `hold_splash`, `gallery`.
//!   The pure parts (`splash_rect`, hold/fade timing) are kept as helpers.
//! * The screen has no marquee/indeterminate mode: like the old one it is driven by [`Progress`] (completed load steps), never by wall-clock time.
use crate::assets::TexInfo;
use crate::draw::{RenderList, TextureId};
use crate::layout::Rect;
use crate::scene::UiContext;
use crate::screens::{art, put_text, Snap};
use crate::text::FontId;

/// Reference size of FORT_Load_BattleRoyale (1920x1080).
pub const REF_W: f32 = 1920.;
pub const REF_H: f32 = 1080.;
/// Native size of Content/Splash/Splash.bmp.
pub const SPLASH_W: i32 = 640;
pub const SPLASH_H: i32 = 738;
pub const SPLASH_MIN_SECONDS: f32 = 1.5;
pub const COMPLETE_HOLD_SECONDS: f32 = 0.6;
pub const FADE_OUT_SECONDS: f32 = 0.3;
/// Reference fonts (`Font::new(dc, px, 700)` for the tip, weight 600 for the label / percent, family "Segoe UI").
pub const BIG_FONT: FontId = FontId::SegoeBold;
pub const SMALL_FONT: FontId = FontId::SegoeSemibold;

// ---------------------------------------------------------------- pure layout/logic (unit tested)

/// Cover-scale the 1920x1080 artwork to the viewport, anchored bottom-right so the character art and the
/// blue tip plate (the artwork's own text area) always stay on screen. Returns (scale, x, y).
pub fn cover(w: f32, h: f32) -> (f32, f32, f32) { let s = (w / REF_W).max(h / REF_H); (s, w - REF_W * s, h - REF_H * s) }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout { pub scale: f32, pub bg: [f32; 4], pub text_x0: f32, pub text_x1: f32, pub label_y: f32, pub bar: [f32; 4], pub tip_y: f32, pub tip_step: f32, pub big_px: i32, pub small_px: i32, pub logo: [f32; 4] }
/// Native size of T-Logo-Wordmark-1024 (1.8: 1024x320; the 1.10 file was 1024x512). The file is kept under the old name `T-Logo-Wordmark-1024x512.png`
/// as an unmodified byte copy (alias recorded in reports/version-diff-1.8.json); the white wordmark fills most of the new, tighter canvas.
pub const LOGO_W: f32 = 1024.;
pub const LOGO_H: f32 = 320.;
/// Bottom margin kept below the last of three tip lines so descenders are never clipped.
const TIP_LINES: f32 = 3.;
/// Positions are authored in artwork pixels (blue plate spans x>=715, y>=880) then mapped through `cover`.
/// The original texture's left ~40% is a flat black panel (DXT1, RGB 0,0,0 - see reports/loading-screen.json), the
/// designed negative space the native screen draws over; the wordmark is placed there (a runtime choice).
/// `label_y`, `tip_y` are text BASELINES.
pub fn layout(w: f32, h: f32) -> Layout {
    let (s, ox, oy) = cover(w, h);
    let x0 = (ox + 780. * s).max(w * 0.04); let x1 = ox + 1860. * s;
    let big = ((30. * s).round() as i32).clamp(14, 96); let small = ((24. * s).round() as i32).clamp(12, 80);
    let step = (34. * s).max(big as f32 * 1.25);
    let (mut label_y, mut bar_y, mut tip_y) = (oy + 920. * s, oy + 934. * s, oy + 980. * s);
    // Keep three tip lines (baseline of the last + descender + margin) on screen, shifting the whole block up.
    let bottom = tip_y + (TIP_LINES - 1.) * step + big as f32 * 0.3 + (h * 0.012).max(4.);
    let shift = (bottom - h).max(0.); label_y -= shift; bar_y -= shift; tip_y -= shift;
    // Logo: authored 560x280 at (90,50) in artwork pixels, inside the black panel (x<=730 even at the bottom edge).
    // The bottom-right cover anchor crops the panel on tall/narrow windows, so it is fitted to the visible part
    // of the panel and dropped entirely when almost none of it is on screen (it must never overlap the art).
    let (vis_l, vis_r) = (ox.max(0.), ox + 730. * s); let margin = (vis_r - vis_l) * 0.08;
    let lw = (560. * s).min(vis_r - vis_l - 2. * margin).max(0.);
    let logo = if lw < 64. { [0., 0., 0., 0.] } else { [(ox + 90. * s).max(vis_l + margin).min(vis_r - margin - lw), (oy + 50. * s).max(h * 0.04), lw, lw * LOGO_H / LOGO_W] };
    Layout { scale: s, bg: [ox, oy, REF_W * s, REF_H * s], text_x0: x0, text_x1: x1, label_y, bar: [x0, bar_y, (x1 - x0).max(1.), (12. * s).max(4.)], tip_y, tip_step: step, big_px: big, small_px: small, logo }
}

/// Native Unreal-style splash window rectangle: centered at native size, shrunk only if the screen is too short (pure helper kept from the old Win32 splash).
pub fn splash_rect(screen_w: i32, screen_h: i32) -> (i32, i32, i32, i32) {
    let scale = ((screen_h as f32 * 0.9) / SPLASH_H as f32).min((screen_w as f32 * 0.9) / SPLASH_W as f32).min(1.);
    let (w, h) = (((SPLASH_W as f32 * scale).round() as i32).max(1), ((SPLASH_H as f32 * scale).round() as i32).max(1));
    ((screen_w - w) / 2, (screen_h - h) / 2, w, h)
}

/// Progress driven by completed load steps (never by wall-clock time).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress { pub total: usize, pub done: usize }
impl Progress {
    pub fn fraction(&self) -> f32 { if self.total == 0 { 1. } else { (self.done as f32 / self.total as f32).min(1.) } }
    pub fn percent(&self) -> i32 { (self.fraction() * 100.).floor() as i32 }
}

/// The text atlases only cover ASCII, so typographic punctuation in the original tips is folded.
pub fn ascii_fold(text: &str) -> String { text.chars().map(|c| match c { '\u{2018}' | '\u{2019}' => '\'', '\u{201c}' | '\u{201d}' => '"', '\u{2013}' | '\u{2014}' => '-', c if c.is_ascii() && !c.is_control() => c, _ => '?' }).collect() }
pub fn parse_tips(text: &str) -> Vec<String> { text.lines().map(str::trim).filter(|l| !l.is_empty()).map(ascii_fold).collect() }
/// Index of the longest tip by byte length; ties go to the LAST one (`Iterator::max_by_key`, exactly the reference gallery's choice).
pub fn longest_tip(tips: &[String]) -> usize { tips.iter().enumerate().max_by_key(|(_, t)| t.len()).map(|(i, _)| i).unwrap_or(0) }

/// Greedy word wrap using a measured width function (pixels). At most `max_lines` lines; overflow ends in "...".
pub fn wrap(text: &str, width: &dyn Fn(&str) -> f32, max_px: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![]; let mut cur = String::new();
    for word in text.split_whitespace() {
        let candidate = if cur.is_empty() { word.to_owned() } else { format!("{cur} {word}") };
        if width(&candidate) <= max_px || cur.is_empty() { cur = candidate; } else { lines.push(std::mem::take(&mut cur)); cur = word.to_owned(); }
    }
    if !cur.is_empty() { lines.push(cur); }
    if lines.len() > max_lines.max(1) { lines.truncate(max_lines.max(1)); if let Some(last) = lines.last_mut() { last.push_str("..."); } }
    lines
}

/// Single-line fit: ellipsizes `text` so its measured width never exceeds `max_px`.
pub fn fit(text: &str, width: &dyn Fn(&str) -> f32, max_px: f32) -> String {
    if width(text) <= max_px { return text.to_owned(); }
    let ends: Vec<usize> = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
    let (mut lo, mut hi) = (0usize, ends.len() - 1);
    while lo < hi { let mid = (lo + hi + 1) / 2; if width(&format!("{}...", &text[..ends[mid]])) <= max_px { lo = mid; } else { hi = mid - 1; } }
    format!("{}...", &text[..ends[lo]])
}

/// Width function over an integer per-character table (the old GDI `widths: [i32; 128]`), for callers/tests that still have one.
pub fn table_width(widths: &[i32; 128]) -> impl Fn(&str) -> f32 + '_ { move |s: &str| s.bytes().map(|b| widths[(b & 127) as usize]).sum::<i32>() as f32 }

/// "Loading complete" hold, then a fade to black before the world appears (pure timing of the old `Loader::finish`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FinishTimeline { pub hold: f32 }
impl FinishTimeline {
    /// `quick` = the old automated-run shortening (0.1 s hold).
    pub fn new(quick: bool) -> Self { Self { hold: if quick { 0.1 } else { COMPLETE_HOLD_SECONDS } } }
    /// Black-overlay alpha `elapsed` seconds after the 100% frame was first shown.
    pub fn fade_at(&self, elapsed: f32) -> f32 { ((elapsed - self.hold) / FADE_OUT_SECONDS).clamp(0., 1.) }
    pub fn finished(&self, elapsed: f32) -> bool { elapsed >= self.hold + FADE_OUT_SECONDS }
}

// ---------------------------------------------------------------- draw

#[derive(Clone, Copy)]
struct Tex { bg: TexInfo, logo: TexInfo, bar_back: TexInfo, bar_fill: TexInfo }

/// Loading screen state: the tips list and lazily resolved textures. Everything else (progress, label, tip index, fade) is passed to `build`.
#[derive(Clone, Default)]
pub struct LoadingScreen { pub tips: Vec<String>, tips_loaded: bool, tex: Option<Tex> }

impl LoadingScreen {
    pub fn new() -> Self { Self::default() }
    /// With an explicit tip list (no `tips.txt` lookup).
    pub fn with_tips(tips: Vec<String>) -> Self { Self { tips, tips_loaded: true, tex: None } }
    /// Loading-screen tips; optional: a missing/unreadable tips.txt yields no tips (and is noted in `ctx.assets.missing`).
    pub fn load_tips(ctx: &mut UiContext) -> Vec<String> {
        match ctx.assets.source.read("tips.txt") { Some(b) => parse_tips(&String::from_utf8_lossy(&b)), None => { ctx.assets.note_missing("Missing asset: tips.txt"); Vec::new() } }
    }
    fn ensure(&mut self, ctx: &mut UiContext) -> Tex {
        if !self.tips_loaded { self.tips = Self::load_tips(ctx); self.tips_loaded = true; }
        if let Some(t) = self.tex { if [t.bg, t.logo, t.bar_back, t.bar_fill].iter().all(|i| i.id == TextureId::NONE || ctx.assets.store.get(i.id).is_some()) { return t; } }
        // Everything here is optional: a missing background/bar texture is skipped (TextureId::NONE) and missing tips mean no tip text.
        let t = Tex { bg: art(ctx, "FORT_Load_BattleRoyale.png"), logo: art(ctx, "T-Logo-Wordmark-1024.png"), bar_back: art(ctx, "ProgressBar_Background.png"), bar_fill: art(ctx, "ProgressBar_Fill.png") };
        self.tex = Some(t); t
    }
    fn quad(out: &mut RenderList, r: [f32; 4], color: [f32; 4], tex: TextureId) { out.sprite(tex, Rect::new(r[0], r[1], r[2], r[3]), color); }
    /// Old `Font::draw`: (1,1) black shadow at alpha*0.75 first, then the text; `y` is the baseline.
    fn text(ctx: &mut UiContext, out: &mut RenderList, font: FontId, px: i32, x: f32, y: f32, color: [f32; 4], text: &str) {
        put_text(ctx, out, font, px as u32, x, y, text, color, Some(([1., 1.], [0., 0., 0., color[3] * 0.75])), Snap::PerGlyph);
    }
    /// Append one loading frame (the frame is opaque: background art covers the viewport). `fade` (0..1) is a black overlay used for transitions.
    #[allow(clippy::too_many_arguments)]
    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, progress: Progress, label: &str, tip: usize, fade: f32, out: &mut RenderList) {
        let tex = self.ensure(ctx);
        let l = layout(w, h);
        Self::quad(out, l.bg, [1., 1., 1., 1.], tex.bg.id);
        if l.logo[2] > 0. { Self::quad(out, l.logo, [1., 1., 1., 1.], tex.logo.id); }
        let measure = |ctx: &UiContext, px: i32, s: &str| ctx.fonts.measure(SMALL_FONT, s, px as f32);
        let percent = format!("{}%", progress.percent());
        let pw = measure(ctx, l.small_px, &percent).floor(); // old GDI widths were integers
        let label_w = (l.text_x1 - l.text_x0).floor() - pw - (l.small_px * 2) as f32;
        let fitted = { let width = |s: &str| ctx.fonts.measure(SMALL_FONT, s, l.small_px as f32).floor(); fit(&ascii_fold(label), &width, label_w) };
        Self::text(ctx, out, SMALL_FONT, l.small_px, l.text_x0, l.label_y, [1., 1., 1., 0.95], &fitted);
        Self::text(ctx, out, SMALL_FONT, l.small_px, l.text_x1 - pw, l.label_y, [1., 1., 1., 0.95], &percent);
        let [bx, by, bw, bh] = l.bar;
        Self::quad(out, [bx, by, bw, bh], [0.15, 0.15, 0.2, 0.85], tex.bar_back.id);
        Self::quad(out, [bx, by, bw * progress.fraction(), bh], [1., 1., 1., 1.], tex.bar_fill.id);
        if !self.tips.is_empty() {
            let text = self.tips[tip % self.tips.len()].clone();
            let lines = { let width = |s: &str| ctx.fonts.measure(BIG_FONT, s, l.big_px as f32).floor(); wrap(&text, &width, (l.text_x1 - l.text_x0).floor(), 3) };
            for (i, line) in lines.iter().enumerate() { Self::text(ctx, out, BIG_FONT, l.big_px, l.text_x0, l.tip_y + i as f32 * l.tip_step, [1., 1., 1., 1.], line); }
        }
        if fade > 0. { out.fill(Rect::new(0., 0., w, h), [0., 0., 0., fade.min(1.)]); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Cmd;
    use crate::screens::testutil::{ctx, testdata};
    fn flat() -> [i32; 128] { [10; 128] }
    #[test] fn cover_always_fills_viewport_and_keeps_bottom_right() {
        for (w, h) in [(1920., 1080.), (1280., 720.), (2560., 1080.), (1024., 768.), (640., 360.), (600., 900.), (3840., 2160.), (320., 1000.)] {
            let (s, x, y) = cover(w, h); assert!(x <= 0.001 && y <= 0.001, "{w}x{h}");
            assert!((x + REF_W * s - w).abs() < 0.01 && (y + REF_H * s - h).abs() < 0.01, "anchored bottom-right {w}x{h}");
            assert!(REF_W * s >= w - 0.01 && REF_H * s >= h - 0.01, "covers {w}x{h}");
        }
        assert_eq!(cover(1920., 1080.), (1., 0., 0.));
    }
    #[test] fn layout_text_and_bar_stay_inside_viewport() {
        for (w, h) in [(1920., 1080.), (1280., 720.), (2560., 1080.), (1024., 768.), (640., 360.), (600., 900.), (3840., 2160.), (400., 300.)] {
            let l = layout(w, h); assert!(l.text_x0 >= 0. && l.text_x1 <= w && l.text_x1 > l.text_x0, "{w}x{h} {l:?}");
            assert!(l.bar[0] >= 0. && l.bar[0] + l.bar[2] <= w + 0.01 && l.bar[1] + l.bar[3] < h, "{w}x{h} bar");
            assert!(l.label_y < l.bar[1] && l.bar[1] + l.bar[3] < l.tip_y, "{w}x{h} rows ordered");
            // Three tip lines must remain fully on screen: last baseline + descender (~0.3 em) + margin < h.
            let last = l.tip_y + 2. * l.tip_step; assert!(last + l.big_px as f32 * 0.3 + 3. <= h, "{w}x{h} third tip line clipped: baseline {last}");
            // The wordmark (when shown) stays inside the viewport and inside the dark panel, left of the plate.
            if l.logo[2] > 0. { let [x, y, lw, lh] = l.logo; assert!(x >= 0. && y >= 0. && x + lw <= w && y + lh <= h, "{w}x{h} logo {:?}", l.logo); assert!(x + lw <= l.bg[0] + 730. * l.scale + 0.01, "{w}x{h} logo leaves the black panel"); }
        }
        // At the reference size the layout is the authored artwork coordinates (rows shifted only when 3 lines would not fit).
        let l = layout(1920., 1080.); assert_eq!((l.text_x0, l.text_x1, l.label_y, l.tip_y), (780., 1860., 920., 980.));
        // 1.8 wordmark is 1024x320 (was 1024x512): the 560-wide runtime placement keeps the texture aspect, so it is 175 tall (was 280).
        assert_eq!(l.logo, [90., 50., 560., 560. * LOGO_H / LOGO_W]);
        // Tall window: the panel is cropped away, so no logo is drawn over the art.
        assert_eq!(layout(600., 900.).logo[2], 0.);
    }
    #[test] fn splash_window_is_native_size_centered_and_only_shrinks_on_small_screens() {
        assert_eq!(splash_rect(2560, 1440), ((2560 - 640) / 2, (1440 - 738) / 2, 640, 738));
        assert_eq!(splash_rect(1920, 1080), ((1920 - 640) / 2, (1080 - 738) / 2, 640, 738));
        for (sw, sh) in [(1366, 768), (1280, 720), (800, 600), (1024, 768)] { let (x, y, w, h) = splash_rect(sw, sh); assert!(x >= 0 && y >= 0 && x + w <= sw && y + h <= sh); assert!(((w as f32 / h as f32) - 640. / 738.).abs() < 0.01, "aspect {sw}x{sh}"); }
    }
    #[test] fn progress_is_step_driven_clamped_and_monotonic() {
        let mut p = Progress { total: 4, done: 0 }; let mut last = -1.; for i in 0..=6 { p.done = i; let f = p.fraction(); assert!(f >= last && f <= 1.); last = f; }
        assert_eq!(Progress { total: 4, done: 1 }.percent(), 25); assert_eq!(Progress { total: 3, done: 1 }.percent(), 33); assert_eq!(Progress { total: 4, done: 4 }.percent(), 100); assert_eq!(Progress::default().fraction(), 1.);
    }
    #[test] fn finish_timeline_holds_then_fades() {
        let t = FinishTimeline::new(false);
        assert_eq!(t.fade_at(0.), 0.); assert_eq!(t.fade_at(0.6), 0.); assert!((t.fade_at(0.75) - 0.5).abs() < 1e-5); assert_eq!(t.fade_at(5.), 1.);
        assert!(!t.finished(0.8) && t.finished(0.95)); assert_eq!(FinishTimeline::new(true).hold, 0.1);
    }
    #[test] fn wrap_respects_width_lines_and_truncates() {
        let t = flat(); let w = table_width(&t);
        assert_eq!(wrap("aaa bbb ccc", &w, 70., 3), vec!["aaa bbb", "ccc"]);
        assert_eq!(wrap("one", &w, 5., 3), vec!["one"]); // a word wider than the box still renders
        assert_eq!(wrap("aa bb cc dd ee", &w, 50., 2), vec!["aa bb", "cc dd..."]);
        for line in wrap("the quick brown fox jumps over the lazy dog", &w, 100., 9) { assert!(line.len() as i32 * 10 <= 100); }
        assert!(wrap("", &w, 100., 3).is_empty());
    }
    #[test] fn fit_ellipsizes_to_width() { let t = flat(); let w = table_width(&t); assert_eq!(fit("short", &w, 100.), "short"); let f = fit("a very long loading label", &w, 100.); assert!(f.ends_with("...") && f.len() as i32 * 10 <= 100, "{f}"); assert_eq!(fit("abc", &w, 10.), "..."); }
    #[test] fn ascii_fold_handles_original_typography() { assert_eq!(ascii_fold("player\u{2019}s \u{201c}x\u{201d} \u{4e2d}"), "player's \"x\" ?"); }
    #[test] fn parse_tips_skips_blank_lines() { assert_eq!(parse_tips("a\n\n  b \r\n"), vec!["a", "b"]); }
    #[test] fn longest_tip_picks_last_longest_like_the_reference() { let t = vec!["aa".to_owned(), "bbbb".to_owned(), "cccc".to_owned()]; assert_eq!(longest_tip(&t), 2); assert_eq!(longest_tip(&[]), 0); }
    fn tips_from_assets(c: &mut UiContext) -> Vec<String> { LoadingScreen::load_tips(c) }
    #[test] fn original_tips_are_recovered_and_fit_three_lines_everywhere() {
        let Some(mut c) = ctx() else { return };
        let tips = tips_from_assets(&mut c);
        assert_eq!(tips.len(), 32); assert!(tips.iter().any(|t| t == "Need cover? Build some.")); assert!(tips.iter().any(|t| t == "Listen carefully! You may hear an approaching player's footsteps."));
        assert!(tips.iter().all(|t| t.is_ascii() && t.len() > 10));
        // Average ~7px/char at 1080p-equivalent scale: every tip must wrap within 3 lines at 640x360 and 1024x768 without truncation (flat 0.6 em widths as before).
        for (w, h) in [(640., 360.), (1024., 768.), (1920., 1080.)] {
            let l = layout(w, h); let per = (l.big_px as f32 * 0.6).round(); let width = move |s: &str| s.len() as f32 * per;
            for t in &tips { let lines = wrap(t, &width, (l.text_x1 - l.text_x0).floor(), 3); assert!(!lines.last().unwrap().ends_with("..."), "{w}x{h} truncates: {t}"); }
        }
        // ... and with the real fontdue advances of the bold face
        for (w, h) in [(1024., 768.), (1920., 1080.)] {
            let l = layout(w, h); let width = |s: &str| c.fonts.measure(BIG_FONT, s, l.big_px as f32);
            for t in &tips { let lines = wrap(t, &width, (l.text_x1 - l.text_x0).floor(), 3); assert!(!lines.last().unwrap().ends_with("..."), "{w}x{h} fontdue truncates: {t}"); }
        }
    }
    #[test] fn tips_match_recovered_report() {
        let report = testdata("hud-loading-screen.json");
        assert!(report.contains("LSID_DefaultBG") && report.contains("FORT_Load_BattleRoyale") && report.contains("\"tipCount\": 32"));
    }
    #[test] fn loading_textures_have_original_dimensions() {
        let Some(mut c) = ctx() else { return };
        let bg = c.assets.texture("FORT_Load_BattleRoyale.png"); assert_eq!((bg.w, bg.h), (1920, 1080));
        let logo = c.assets.texture("T-Logo-Wordmark-1024.png"); assert_eq!((logo.w as f32, logo.h as f32), (LOGO_W, LOGO_H));
        // The wordmark is white with real alpha (transparent surround, opaque letters): it must not be an opaque box.
        let im = c.assets.image("T-Logo-Wordmark-1024.png").unwrap();
        assert_eq!(im.rgba[(2 * im.w as usize + 2) * 4 + 3], 0, "wordmark corner is transparent");
        assert!(im.rgba.chunks_exact(4).any(|p| p[3] == 255 && p[0] > 240), "wordmark has opaque white letters");
        for n in ["ProgressBar_Background.png", "ProgressBar_Fill.png"] { let t = c.assets.texture(n); assert_eq!((t.w, t.h), (12, 12)); }
    }
    #[test] fn splash_bmp_keeps_original_dimensions_and_size() {
        let p = crate::assets::DirAssets::default_root().join("Splash.bmp");
        if !p.is_file() { eprintln!("Splash.bmp not found, skipping"); return; }
        assert_eq!(image::image_dimensions(&p).unwrap(), (SPLASH_W as u32, SPLASH_H as u32));
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 1417016);
    }
    #[test] fn background_plate_geometry_matches_layout_assumptions() {
        // The text rows are authored against the artwork's blue plate (y>=880, x>=715). Verify against the real pixels.
        let Some(mut c) = ctx() else { return };
        let im = c.assets.image("FORT_Load_BattleRoyale.png").unwrap();
        let rgb = |x: u32, y: u32| { let i = ((y * im.w + x) * 4) as usize; [im.rgba[i] as i32, im.rgba[i + 1] as i32, im.rgba[i + 2] as i32] };
        let blueish = |x: u32, y: u32| { let p = rgb(x, y); p[2] > p[0] + 40 && p[2] > 90 };
        assert!(blueish(1000, 1000) && blueish(1500, 960) && blueish(1850, 1050), "tip plate present where text is placed");
        let black = rgb(300, 500); assert!(black[0] < 8 && black[1] < 8 && black[2] < 8, "left panel is black");
        for y in [930, 950, 1000, 1040, 1070] { assert!(blueish(780, y) && blueish(1860, y), "row y={y} lies on the plate"); }
    }
    #[test] fn build_structure_background_bar_tips_and_fade() {
        let Some(mut c) = ctx() else { return };
        let mut s = LoadingScreen::new();
        let mut out = RenderList::new();
        s.build(&mut c, 1920., 1080., Progress { total: 4, done: 2 }, "Loading HUD", 0, 0., &mut out);
        let l = layout(1920., 1080.);
        let Cmd::Quad(first) = &out.cmds[0] else { panic!() };
        assert_eq!(first.bounds(), Rect::new(l.bg[0], l.bg[1], l.bg[2], l.bg[3]), "first command is the cover-scaled background");
        let Cmd::Quad(logo) = &out.cmds[1] else { panic!() }; assert_eq!(logo.bounds().w, 560.);
        // progress fill width = half the bar
        let fill = out.cmds.iter().rev().find_map(|c| if let Cmd::Quad(q) = c { let b = q.bounds(); if b.h == l.bar[3] && b.x == l.bar[0] && b.w < l.bar[2] { Some(b) } else { None } } else { None }).expect("fill quad");
        assert!((fill.w - l.bar[2] * 0.5).abs() < 1e-3);
        assert!(!s.tips.is_empty());
        assert!(out.cmds.iter().all(|c| match c { Cmd::Quad(q) => q.pos.iter().flatten().chain(q.tint.iter()).all(|v| v.is_finite()), _ => true }));
        // fade adds exactly one full-screen black quad as the LAST command
        let n = out.cmds.len(); let mut f = RenderList::new();
        s.build(&mut c, 1920., 1080., Progress { total: 4, done: 2 }, "Loading HUD", 0, 0.5, &mut f);
        assert_eq!(f.cmds.len(), n + 1);
        let Cmd::Quad(q) = f.cmds.last().unwrap() else { panic!() }; assert_eq!(q.bounds(), Rect::new(0., 0., 1920., 1080.)); assert_eq!(q.tint, [0., 0., 0., 0.5]);
        // tip index wraps around the list; different tips give different glyph streams
        let mut a = RenderList::new(); s.build(&mut c, 1920., 1080., Progress::default(), "x", 0, 0., &mut a);
        let mut b = RenderList::new(); let len = s.tips.len(); s.build(&mut c, 1920., 1080., Progress::default(), "x", len, 0., &mut b);
        assert_eq!(a.cmds, b.cmds);
        let mut d = RenderList::new(); s.build(&mut c, 1920., 1080., Progress::default(), "x", 1, 0., &mut d);
        assert_ne!(a.cmds, d.cmds);
    }
    #[test] fn long_label_is_ellipsized_not_overflowing() {
        let Some(mut c) = ctx() else { return };
        let mut s = LoadingScreen::with_tips(vec![]);
        let l = layout(640., 360.);
        let mut out = RenderList::new();
        s.build(&mut c, 640., 360., Progress { total: 10, done: 3 }, &"Loading animation: M_Jog_Melee_Twohanded_NE ".repeat(4), 0, 0., &mut out);
        // every glyph quad that sits on the label baseline row stays inside [text_x0, text_x1 + 2]
        for cmd in &out.cmds { if let Cmd::Quad(q) = cmd { let b = q.bounds(); if b.y < l.label_y && b.y + b.h > l.label_y - l.small_px as f32 && b.w < 40. { assert!(b.x >= l.text_x0 - 4. && b.x + b.w <= l.text_x1 + 4., "{b:?}"); } } }
    }
}


