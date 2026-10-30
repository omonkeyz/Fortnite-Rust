//! Text: fontdue glyph atlases (printable ASCII 32..126, no kerning) turned into textured quads in a [`RenderList`].
//! Port of the atlas / measure / font loading half of the old `hud_extra.rs` and the `Fonts` of `menu/mod.rs`.
//!
//! The atlas is stored in the [`TextureStore`] as STRAIGHT white + coverage alpha (premultiplied = (cov,cov,cov,cov)), exactly the old GL upload.
//! Glyph quads are snapped to whole pixels like the old runtime (`round()` of the glyph origin).
use crate::assets::{AssetSource, Sampler, Texture, TextureStore};
use crate::draw::{RenderList, TextureId};
use crate::layout::Rect;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FontId {
    Regular, Bold,
    /// `Burbank.Burbank` typeface "Burbank-Black" = BurbankBigCondensed-Black (TextStyle-Header-*, dialog titles). Falls back to NotoSans Bold when absent.
    Title,
    /// Roboto Bold (`Content/Slate/Fonts/Roboto-Bold.ttf`): engine default font of a `CommonTextBlock` without a text style. Falls back to NotoSans Bold.
    RobotoBold,
    /// `Burbank.Burbank` typeface "Burbank-PlayerPower" = BurbankBigRegular-Bold: the font of the start-up loading screen (`FSlateFontInfo(Burbank, 48, "Burbank-PlayerPower")` in
    /// the exe). Falls back to the Title face, then NotoSans Bold.
    BurbankBold,
    /// Windows' Segoe UI Bold (weight 700) and Semibold (600): the faces the user's reference level-travel loading screen draws (GDI `CreateFontW("Segoe UI")`, weights 700 for the tip,
    /// 600 for the label / percent). System fonts of the machine (`%WINDIR%\Fonts`); fall back to NotoSans Bold when absent.
    SegoeBold,
    SegoeSemibold,
}

/// NotoSans Regular/Bold (the face of the recovered widget text styles) + Burbank title + Roboto Bold.
#[derive(Clone)]
/// Faces are shared (`Arc`), so cloning `Fonts` does not copy the font data.
pub struct Fonts { pub regular: Arc<fontdue::Font>, pub bold: Arc<fontdue::Font>, pub title: Option<Arc<fontdue::Font>>, pub roboto_bold: Option<Arc<fontdue::Font>>, pub burbank_bold: Option<Arc<fontdue::Font>>, pub segoe_bold: Option<Arc<fontdue::Font>>, pub segoe_semibold: Option<Arc<fontdue::Font>> }

/// A Windows system font by file name (`%WINDIR%\Fonts\<file>`).
fn system_font(file: &str) -> Option<fontdue::Font> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_owned());
    try_load_font(&std::path::PathBuf::from(windir).join("Fonts").join(file)).ok()
}

pub fn parse_font(bytes: Vec<u8>) -> Result<fontdue::Font, String> { fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).map_err(|e| e.to_string()) }
pub fn try_load_font(path: &std::path::Path) -> Result<fontdue::Font, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read font {}: {e}", path.display()))?;
    parse_font(bytes).map_err(|e| format!("Cannot parse font {}: {e}", path.display()))
}
/// Font asset by file name; if missing, a Windows system font of the same weight; None if neither exists.
pub fn font_or_system(src: &dyn AssetSource, name: &str) -> Option<fontdue::Font> {
    if let Some(f) = src.read(name).and_then(|b| parse_font(b).ok()) { return Some(f); }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_owned());
    let candidates: &[&str] = if name.to_ascii_lowercase().contains("bold") { &["segoeuib.ttf", "arialbd.ttf"] } else { &["segoeui.ttf", "arial.ttf"] };
    candidates.iter().find_map(|f| try_load_font(&std::path::PathBuf::from(&windir).join("Fonts").join(f)).ok())
}

type FontCell = Arc<std::sync::OnceLock<Option<Arc<fontdue::Font>>>>;
static FONT_CACHE: std::sync::Mutex<Option<HashMap<&'static str, FontCell>>> = std::sync::Mutex::new(None);
fn cached_font(key: &'static str, load: impl FnOnce() -> Option<fontdue::Font>) -> Option<Arc<fontdue::Font>> {
    let cell = FONT_CACHE.lock().ok().map(|mut g| g.get_or_insert_with(HashMap::new).entry(key).or_default().clone());
    let Some(c) = cell else { return load().map(Arc::new) };
    let r = c.get_or_init(|| load().map(Arc::new)).clone();
    if r.is_none() { if let Ok(mut g) = FONT_CACHE.lock() { if let Some(m) = g.as_mut() { if m.get(key).is_some_and(|x| Arc::ptr_eq(x, &c)) { m.remove(key); } } } }
    r
}
fn cached_or_system(src: &dyn AssetSource, name: &str, key: &'static str) -> Option<Arc<fontdue::Font>> {
    if let Some(f) = cached_font(key, || src.read(name).and_then(|b| parse_font(b).ok())) { return Some(f); }
    let bold = name.to_ascii_lowercase().contains("bold");
    let candidates: [(&str, &'static str); 2] = if bold { [("segoeuib.ttf", "s:segoeuib.ttf"), ("arialbd.ttf", "s:arialbd.ttf")] } else { [("segoeui.ttf", "s:segoeui.ttf"), ("arial.ttf", "s:arial.ttf")] };
    candidates.iter().find_map(|(f, k)| cached_font(k, || system_font(f)))
}
pub fn warm_fonts(src: &dyn AssetSource) { let _ = Fonts::load(src); }

impl Fonts {
    pub fn load(src: &dyn AssetSource) -> Option<Self> {
        let title = cached_font("a:BurbankBigCondensed-Black.ufont", || src.read("BurbankBigCondensed-Black.ufont").and_then(|b| parse_font(b).ok()));
        let roboto_bold = cached_font("a:/Slate/Fonts/Roboto-Bold.ttf", || src.read_in("Roboto-Bold.ttf", "/Slate/Fonts/").and_then(|b| parse_font(b).ok()));
        let burbank_bold = cached_font("a:BurbankBigRegular-Bold.ufont", || src.read("BurbankBigRegular-Bold.ufont").and_then(|b| parse_font(b).ok()));
        let regular = cached_or_system(src, "NotoSans-Regular.ufont", "a:NotoSans-Regular.ufont")?;
        let bold = cached_or_system(src, "NotoSans-Bold.ufont", "a:NotoSans-Bold.ufont")?;
        Some(Self { regular, bold, title, roboto_bold, burbank_bold, segoe_bold: cached_font("s:segoeuib.ttf", || system_font("segoeuib.ttf")), segoe_semibold: cached_font("s:seguisb.ttf", || system_font("seguisb.ttf")) })
    }
    pub fn font(&self, id: FontId) -> &fontdue::Font {
        let f: &Arc<fontdue::Font> = match id { FontId::Regular => &self.regular, FontId::Bold => &self.bold, FontId::Title => self.title.as_ref().unwrap_or(&self.bold), FontId::RobotoBold => self.roboto_bold.as_ref().unwrap_or(&self.bold), FontId::BurbankBold => self.burbank_bold.as_ref().or(self.title.as_ref()).unwrap_or(&self.bold), FontId::SegoeBold => self.segoe_bold.as_ref().unwrap_or(&self.bold), FontId::SegoeSemibold => self.segoe_semibold.as_ref().or(self.segoe_bold.as_ref()).unwrap_or(&self.bold) }; f
    }
    pub fn measure(&self, id: FontId, text: &str, px: f32) -> f32 { measure(self.font(id), text, px) }
    /// Ascent+|descent| of a line in pixels.
    pub fn line_height(&self, id: FontId, px: f32) -> f32 { self.font(id).horizontal_line_metrics(px).map(|l| l.ascent - l.descent).unwrap_or(px * 1.2) }
}

#[derive(Clone, Copy, Debug)]
pub struct Glyph { pub u0: f32, pub v0: f32, pub u1: f32, pub v1: f32, pub w: f32, pub h: f32, pub xmin: f32, pub ymin: f32, pub adv: f32 }
pub struct AtlasCpu { pub w: usize, pub h: usize, pub px: u32, pub pixels: Vec<u8>, pub glyphs: HashMap<char, Glyph>, pub ascent: f32, pub descent: f32 }

pub fn build_atlas(font: &fontdue::Font, px: u32) -> AtlasCpu {
    let pxf = px as f32;
    let lm = font.horizontal_line_metrics(pxf);
    let (ascent, descent) = lm.map(|l| (l.ascent, l.descent)).unwrap_or((pxf * 0.9, -pxf * 0.25));
    let aw = 512usize;
    let mut items = Vec::new();
    for c in (32u8..=126).map(|b| b as char) { let (m, bmp) = font.rasterize(c, pxf); items.push((c, m, bmp)); }
    let (mut x, mut y, mut row_h) = (1usize, 1usize, 0usize);
    let mut placed = Vec::new();
    for (_, m, _) in &items {
        if x + m.width + 1 > aw { x = 1; y += row_h + 1; row_h = 0; }
        placed.push((x, y)); x += m.width + 1; row_h = row_h.max(m.height);
    }
    let ah = y + row_h + 1;
    let mut pixels = vec![0u8; aw * ah];
    let mut glyphs = HashMap::new();
    for ((c, m, bmp), (px0, py0)) in items.into_iter().zip(placed) {
        for r in 0..m.height { for q in 0..m.width { pixels[(py0 + r) * aw + px0 + q] = bmp[r * m.width + q]; } }
        glyphs.insert(c, Glyph { u0: px0 as f32 / aw as f32, v0: py0 as f32 / ah as f32, u1: (px0 + m.width) as f32 / aw as f32, v1: (py0 + m.height) as f32 / ah as f32, w: m.width as f32, h: m.height as f32, xmin: m.xmin as f32, ymin: m.ymin as f32, adv: m.advance_width });
    }
    AtlasCpu { w: aw, h: ah, px, pixels, glyphs, ascent, descent }
}
/// Text width in pixels (advance sum, no kerning).
pub fn measure(font: &fontdue::Font, text: &str, px: f32) -> f32 { text.chars().map(|c| font.metrics(c, px).advance_width).sum() }

/// Drop shadow style of TextStyle-Base-XS-B-S: offset 1.6 Slate units, black 0.75.
pub const SHADOW_OFFSET: f32 = 1.6;
pub const SHADOW_COLOR: [f32; 4] = [0., 0., 0., 0.75];

struct GpuAtlas { tex: TextureId, cpu: AtlasCpu }

/// Glyph atlas cache (one atlas per (font, pixel size)) registered in a `TextureStore`.
#[derive(Default)]
pub struct TextAtlases { atlases: HashMap<(FontId, u32), GpuAtlas> }
impl TextAtlases {
    pub fn new() -> Self { Self::default() }
    fn ensure(&mut self, store: &mut TextureStore, fonts: &Fonts, font: FontId, px: u32) {
        if self.atlases.len() > 32 && !self.atlases.contains_key(&(font, px)) { for (_, a) in self.atlases.drain() { store.remove(a.tex); } }
        self.atlases.entry((font, px)).or_insert_with(|| {
            let cpu = build_atlas(fonts.font(font), px);
            let mut rgba = Vec::with_capacity(cpu.pixels.len() * 4);
            for &a in &cpu.pixels { rgba.extend_from_slice(&[255, 255, 255, a]); }
            let tex = store.insert(Texture::new(cpu.w as u32, cpu.h as u32, rgba, Sampler::LINEAR));
            GpuAtlas { tex, cpu }
        });
    }
    pub fn atlas(&mut self, store: &mut TextureStore, fonts: &Fonts, font: FontId, px: u32) -> (TextureId, &AtlasCpu) {
        self.ensure(store, fonts, font, px); let a = &self.atlases[&(font, px)]; (a.tex, &a.cpu)
    }
    /// One line of text. `x` = left of the first glyph; the line is vertically centred in the box (`top`, `box_h`) using atlas ascent/descent
    /// (old `menu::render` rule). `shadow` = optional (offset px, colour) pass drawn first.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line(&mut self, store: &mut TextureStore, fonts: &Fonts, out: &mut RenderList, font: FontId, px: u32, x: f32, top: f32, box_h: f32, text: &str, color: [f32; 4], shadow: Option<([f32; 2], [f32; 4])>) {
        let (tex, a) = self.atlas(store, fonts, font, px);
        let baseline = top + (box_h - (a.ascent + a.descent.abs())) * 0.5 + a.ascent;
        let passes: Vec<([f32; 2], [f32; 4])> = shadow.into_iter().chain(std::iter::once(([0., 0.], color))).collect();
        for (off, col) in passes {
            let mut cx = x;
            for ch in text.chars() {
                let Some(g) = a.glyphs.get(&ch) else { continue };
                if g.w > 0. { out.quad(tex, Rect::new((cx + g.xmin + off[0]).round(), (baseline - g.ymin - g.h + off[1]).round(), g.w, g.h), [g.u0, g.v0, g.u1, g.v1], col); }
                cx += g.adv;
            }
        }
    }
    /// Like `draw_line` but the box is the font's natural line (top-aligned text as `STextBlock` paints it), returning the line height used.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_top(&mut self, store: &mut TextureStore, fonts: &Fonts, out: &mut RenderList, font: FontId, px: u32, x: f32, top: f32, text: &str, color: [f32; 4], shadow: Option<([f32; 2], [f32; 4])>) -> f32 {
        let lh = fonts.line_height(font, px as f32).max(1.);
        self.draw_line(store, fonts, out, font, px, x, top, lh, text, color, shadow); lh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::DirAssets;
    fn fonts() -> Option<Fonts> { let d = DirAssets::new(DirAssets::default_root().join("fonts")); Fonts::load(&d) }
    #[test] fn atlas_has_printable_ascii_and_measure_matches_advances() {
        let Some(f) = fonts() else { eprintln!("assets not found, skipping"); return };
        let a = build_atlas(&f.bold, 24);
        assert_eq!(a.glyphs.len(), 95); assert!(a.ascent > 0.);
        let w = f.measure(FontId::Bold, "Hello", 24.);
        let s: f32 = "Hello".chars().map(|c| a.glyphs[&c].adv).sum();
        assert!((w - s).abs() < 1e-3);
    }
    #[test] fn draw_line_emits_one_quad_per_visible_glyph() {
        let Some(f) = fonts() else { return };
        let (mut st, mut at, mut out) = (TextureStore::new(), TextAtlases::new(), RenderList::new());
        at.draw_line(&mut st, &f, &mut out, FontId::Regular, 20, 10., 10., 30., "A B", [1.; 4], None);
        assert_eq!(out.quad_count(), 2); // space has no bitmap
        let mut out2 = RenderList::new();
        at.draw_line(&mut st, &f, &mut out2, FontId::Regular, 20, 10., 10., 30., "A B", [1.; 4], Some(([2., 2.], SHADOW_COLOR)));
        assert_eq!(out2.quad_count(), 4);
    }
}
