//! Paint pass: the arranged tree -> [`RenderList`]. Brushes (textures: stretch / nine-patch; materials: baked on the CPU through
//! [`SlateMat`](super::slate_mat::SlateMat) into a texture), text (font, size, colour, shadow, justification), borders and buttons.
use super::layout::{Ctx, R};
use crate::screens::athena_lobby::slate_mat::SlateMat; // SHOP: fork of athena_lobby::paint (see mod.rs)
use crate::screens::athena_lobby::store::{jf, jvec};
use crate::screens::athena_lobby::styles::{Brush, BtnState, DrawAs};
use crate::screens::athena_lobby::umg::Vis;
use crate::assets::{Sampler, Texture};
use crate::draw::{RenderList, TextureId};
use crate::layout::Rect;
use crate::screens::{put_text, srgb, Snap};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug)]
pub struct Tf { pub sx: f32, pub sy: f32, pub tx: f32, pub ty: f32 }
impl Tf {
    pub const ID: Tf = Tf { sx: 1.0, sy: 1.0, tx: 0.0, ty: 0.0 };
    pub fn rect(&self, r: Rect) -> (Rect, bool, bool) {
        let (x0, x1) = (r.x * self.sx + self.tx, (r.x + r.w) * self.sx + self.tx);
        let (y0, y1) = (r.y * self.sy + self.ty, (r.y + r.h) * self.sy + self.ty);
        (Rect::new(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs()), self.sx < 0.0, self.sy < 0.0)
    }
}

/// the lobby layer's blur region type, so one renderer path handles both
pub use crate::screens::athena_lobby::paint::BlurRegion;

pub struct Painter<'a> {
    pub c: Ctx<'a>,
    pub out: RenderList,
    pub mats: &'a mut HashMap<String, Option<Rc<SlateMat>>>,
    pub blur: Vec<BlurRegion>,
    pub missing: Vec<String>,
    pub scale: f32,
    /// an ancestor (or the widget itself) is disabled: Slate `ShouldBeEnabled(bParentEnabled)` is false for the whole subtree, which draws with the disabled effect
    pub disabled: bool,
    /// SHOP: runtime material parameters of widgets (`GetDynamicMaterial().Set*ParameterValue`), font-material colours and native item cards
    pub dynm: DynMats,
}

fn srgb4(c: [f32; 4]) -> [f32; 4] { [srgb(c[0]), srgb(c[1]), srgb(c[2]), c[3]] }
/// Slate `DrawDisabledEffect` as compiled in the cooked Slate pixel shaders (`dp3 lum, (0.3, 0.59, 0.11), C; C = lerp(C, lum, 0.8)`, read from the
/// disabled permutation of M-Border-1pxOutline, shader_7136): the same colour transform applied to the (linear) text colour.
pub fn disabled_effect(c: [f32; 4]) -> [f32; 4] {
    let lum = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
    [c[0] + (lum - c[0]) * 0.8, c[1] + (lum - c[1]) * 0.8, c[2] + (lum - c[2]) * 0.8, c[3]]
}
fn mul4(a: [f32; 4], b: [f32; 4]) -> [f32; 4] { [a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]] }

impl<'a> Painter<'a> {
    fn px_rect(&self, r: R) -> Rect { Rect::new(r[0] * self.scale, r[1] * self.scale, r[2] * self.scale, r[3] * self.scale) }

    pub fn paint(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let w = &self.c.ui.w[id];
        if !w.vis.draws() { return; }
        let prev_disabled = self.disabled;
        self.disabled |= !w.enabled;
        self.paint_inner(id, tf, color);
        self.disabled = prev_disabled;
    }

    fn paint_inner(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let w = &self.c.ui.w[id];
        let rect = self.c.lay.rect[id];
        let mut tf = tf;
        let mut color = color;
        // RenderTransform (pivot 0.5, 0.5): scale / translate about the centre of the widget
        let t = w.transform.clone();
        if !t.is_identity() {
            let (cx, cy) = ((rect[0] + rect[2] * 0.5) * self.scale, (rect[1] + rect[3] * 0.5) * self.scale);
            let (sx, sy) = (t.scale[0], t.scale[1]);
            tf = Tf { sx: tf.sx * sx, sy: tf.sy * sy, tx: tf.sx * (cx - sx * cx + t.translation[0] * self.scale) + tf.tx, ty: tf.sy * (cy - sy * cy + t.translation[1] * self.scale) + tf.ty };
        }
        // user widget colour and opacity
        if w.user.is_some() { if let Some(c) = jvec::<4>(w.props.get("ColorAndOpacity")) { color = mul4(color, c); } }
        let class = w.class.clone();
        let user = w.user.is_some();
        let kids = w.kids.clone();
        match class.as_str() {
            "FortMultiSizeItemCard" => self.paint_card(id, tf, color), // SHOP
            "Border" | "CommonBorder" => self.paint_border(id, tf, color),
            "Image" | "FortSZAwareImage" | "FortItemIcon" => self.paint_image(id, tf, color),
            "TextBlock" | "CommonTextBlock" | "CommonNumericTextBlock" | "CommonDateTimeTextBlock" | "FortItemCountTextBlock" => self.paint_text(id, tf, color),
            "BackgroundBlur" => {
                let s = self.c.ui.w[id].props.get("BlurStrength").and_then(jf).unwrap_or(0.0);
                let (r, _, _) = tf.rect(self.px_rect(rect));
                self.blur.push(BlurRegion { rect: r, strength: s });
            }
            _ => {}
        }
        if user && self.c.is_button(id) { self.paint_button(id, tf, color); }
        // content colour of borders tints the children
        let mut child_color = color;
        if matches!(class.as_str(), "Border" | "CommonBorder") { if let Some(c) = jvec::<4>(self.c.ui.w[id].props.get("ContentColorAndOpacity")) { child_color = mul4(child_color, c); } }
        let active: Vec<usize> = match class.as_str() {
            "WidgetSwitcher" | "CommonWidgetSwitcher" => {
                let idx = self.c.ui.w[id].props.get("ActiveWidgetIndex").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize;
                kids.get(idx).copied().into_iter().collect()
            }
            _ => kids,
        };
        // `SBackgroundBlur` paints its content inside its own geometry (the closed top-bar menu is folded away by its zero height)
        let clip = class == "BackgroundBlur" || class == "ScrollBox"; // SHOP: SScrollBox clips its content to the viewport
        if clip { let (r, _, _) = tf.rect(self.px_rect(rect)); self.out.push_clip(r); }
        // widgets that do not take space are never painted
        for k in active { if self.c.ui.w[k].vis != Vis::Collapsed { self.paint(k, tf, child_color); } }
        if clip { self.out.pop_clip(); }
    }

    fn paint_border(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let store = self.c.ui.store.clone();
        let style_cls = self.c.ui.w[id].props.get("Style").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_owned());
        let brush = match style_cls {
            Some(c) => self.c.ui.styles.border(&store, &c).background,
            None => self.c.ui.w[id].props.get("Background").and_then(Brush::from_json),
        };
        // SHOP: a border whose material the Blueprint parametrises (`GetDynamicMaterial()`) keeps its own serialized `Background` brush when that is a material
        // (`BottomBorder`: M-FoundersPanel; the style's brush is a plain colour), otherwise the style's
        let own = if self.dynm.mats.contains_key(&id) { self.c.ui.w[id].props.get("Background").and_then(Brush::from_json).filter(|b| b.resource.is_some()) } else { None };
        let Some(b) = own.or(brush) else { return };
        let bc = jvec::<4>(self.c.ui.w[id].props.get("BrushColor")).unwrap_or([1.0; 4]);
        let rect = self.c.lay.rect[id];
        if self.draw_dyn(id, &b, rect, mul4(bc, color), tf) { return; } // SHOP
        self.draw_brush(&b, rect, mul4(bc, color), tf, false);
    }

    fn paint_image(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let Some(b) = self.c.brush_of(id) else { return };
        let ci = jvec::<4>(self.c.ui.w[id].props.get("ColorAndOpacity")).unwrap_or([1.0; 4]);
        let rect = self.c.lay.rect[id];
        if self.draw_dyn(id, &b, rect, mul4(ci, color), tf) { return; } // SHOP
        self.draw_brush(&b, rect, mul4(ci, color), tf, false);
    }

    fn paint_button(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let Some(st) = self.c.ui.button_style(id) else { return };
        let state = self.c.ui.w[id].btn_state;
        let Some(b) = st.brush(state).or(st.normal_base.as_ref()).cloned() else { return };
        let rect = self.c.lay.rect[id];
        self.draw_brush(&b, rect, color, tf, state == BtnState::Disabled);
    }

    fn paint_text(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let rect = self.c.lay.rect[id];
        let ww = {
            let p = &self.c.ui.w[id].props;
            p.get("WrapTextAt").and_then(jf).filter(|w| *w > 0.0).or_else(|| if p.get("AutoWrapText").and_then(|b| b.as_bool()).unwrap_or(false) { Some(rect[2]) } else { None })
        };
        let (lines, st, px, lh) = self.c.text_lines(id, ww);
        let m = crate::screens::athena_lobby::store::jmargin(self.c.ui.w[id].props.get("Margin"));
        let cp = self.c.ui.w[id].props.get("ColorAndOpacity");
        let ci = jvec::<4>(cp).or_else(|| jvec::<4>(cp.filter(|c| !c.get("ColorUseRule").and_then(|r| r.as_str()).map_or(false, |r| r.contains("Foreground"))).and_then(|c| c.get("SpecifiedColor")))).unwrap_or([1.0; 4]);
        let style_col = if cp.is_some() && crate::screens::athena_lobby::umg::is_styled_common_text(&self.c.ui.w[id].class, &self.c.ui.w[id].props) { [1.0; 4] } else { st.color };
        let base = self.dynm.text_color.get(&id).copied().unwrap_or(style_col); // SHOP: font material colour (`GetDynamicFontMaterial().SetVectorParameterValue(RGB_1, ..)`)
        let mut lin = mul4(mul4(base, ci), color);
        if self.disabled { lin = disabled_effect(lin); }
        let col = srgb4(lin);
        if col[3] <= 0.0 { return; }
        let just = self.c.ui.w[id].props.get("Justification").and_then(|v| v.as_str()).map(|s| s.rsplit("::").next().unwrap_or(s).to_owned()).unwrap_or_else(|| "Left".into());
        let font = st.font;
        let scale = self.scale;
        let shadow = if st.uses_shadow && st.shadow_color[3] > 0.0 { Some(([st.shadow_offset[0] * scale * tf.sx.signum(), st.shadow_offset[1] * scale * tf.sy.signum()], [st.shadow_color[0], st.shadow_color[1], st.shadow_color[2], st.shadow_color[3] * color[3]])) } else { None };
        let ascent = crate::screens::ascent(self.c.uic, font, px);
        // `UCommonTextBlock::RebuildWidget` (exe 0x394db60, CommonTextBlock.cpp) wraps the STextBlock in an SScrollBox: the text is placed inside the scroll box
        // geometry, centred vertically (reference: the V-Bucks count / "+" in Fill slots sit at the centre of their 40 / 48 unit boxes). Plain TextBlocks stay top aligned.
        let vcentre = if matches!(self.c.ui.w[id].class.as_str(), "CommonTextBlock" | "CommonNumericTextBlock") { ((rect[3] - m[1] - m[3] - lines.len() as f32 * lh) * 0.5).max(0.0) } else { 0.0 };
        for (i, line) in lines.iter().enumerate() {
            let lw = self.c.uic.fonts.measure(font, line, px as f32);
            let inner_w = (rect[2] - m[0] - m[2]) * scale;
            let x_off = match just.as_str() { "Center" => (inner_w - lw) * 0.5, "Right" => inner_w - lw, _ => 0.0 };
            let x = (rect[0] + m[0]) * scale + x_off.max(if just == "Left" { 0.0 } else { f32::MIN });
            let top = (rect[1] + m[1] + vcentre + i as f32 * lh) * scale;
            let (rr, _, _) = tf.rect(Rect::new(x, top + ascent, lw, 0.0));
            put_text(self.c.uic, &mut self.out, font, px, rr.x, rr.y, line, col, shadow, Snap::Pen);
        }
    }

    /// Draw a brush over `rect` (Slate units).
    pub fn draw_brush(&mut self, b: &Brush, rect: R, tint_lin: [f32; 4], tf: Tf, disabled: bool) {
        let disabled = disabled || self.disabled;
        if b.draw == DrawAs::NoDraw { return; }
        let (r, fx, _fy) = tf.rect(self.px_rect(rect));
        if r.w <= 0.0 || r.h <= 0.0 { return; }
        let tint = mul4(b.tint, tint_lin);
        if tint[3] <= 0.0 { return; }
        let flip = fx ^ b.flip_x;
        let Some(res) = b.resource.clone() else {
            // no resource: a plain tinted rectangle
            self.out.fill(r, srgb4(tint));
            return;
        };
        let store = self.c.ui.store.clone();
        match store.object_class(&res).as_deref() {
            Some("Texture2D") => {
                let name = crate::assets::texture_name_of(&res); // INVENTORY FIX3 (inventory_gaps G2): by long package path, not stem
                let t = self.c.uic.assets.texture(&name);
                if t.id == TextureId::NONE { if !self.missing.contains(&name) { self.missing.push(name); } return; }
                match b.draw {
                    DrawAs::Box | DrawAs::Border => {
                        if flip { // mirrored nine-patch: mirror the destination cells
                            let cells = crate::layout::nine_slice_cells(t.w, t.h, r, b.margin, self.scale);
                            for (cr, uv) in cells { let mx = Rect::new(r.x + (r.x + r.w - (cr.x + cr.w)), cr.y, cr.w, cr.h); self.out.quad(t.id, mx, [uv[2], uv[1], uv[0], uv[3]], srgb4(tint)); }
                        } else { self.out.nine_slice(t, r, b.margin, self.scale, srgb4(tint)); }
                    }
                    _ => {
                        let uv = b.uv_region.unwrap_or([0.0, 0.0, 1.0, 1.0]);
                        let uvq = if flip { [uv[2], uv[1], uv[0], uv[3]] } else { uv };
                        self.out.quad(t.id, r, uvq, srgb4(tint));
                    }
                }
            }
            Some("Material") | Some("MaterialInstanceConstant") => self.draw_material(b, &res, r, tint, flip, disabled),
            other => { let m = format!("{res} ({other:?})"); if !self.missing.contains(&m) { self.missing.push(m); } }
        }
    }

    fn draw_material(&mut self, b: &Brush, res: &str, r: Rect, tint: [f32; 4], flip: bool, disabled: bool) {
        let (w, h) = (r.w.round().max(1.0) as u32, r.h.round().max(1.0) as u32);
        let boxed = b.draw == DrawAs::Box && b.margin.iter().any(|m| *m != 0.0);
        let key = format!("lobbymat:{res}:{w}x{h}:{}:{:?}:{:?}:{:?}:{:?}:{}", boxed, b.margin, b.image_size, tint.map(f32::to_bits), self.scale.to_bits(), disabled);
        let tid = match self.c.uic.assets.store.named(&key) {
            Some(t) => t,
            None => {
                if !self.mats.contains_key(res) {
                    let m = SlateMat::load(&self.c.ui.store.clone(), &mut self.c.uic.assets, res);
                    let m = match m { Ok(m) => Some(Rc::new(m)), Err(e) => { self.missing.push(format!("material {res}: {e}")); None } };
                    if let Some(m) = &m { for wmsg in &m.warnings { self.missing.push(format!("material {res}: {wmsg}")); } }
                    self.mats.insert(res.to_owned(), m);
                }
                let Some(m) = self.mats.get(res).cloned().flatten() else { return };
                if !m.is_ready() { return; }
                let size = (w as f32 / self.scale, h as f32 / self.scale);
                let px = m.raster(w, h, size, (b.image_size[0], b.image_size[1]), b.margin, boxed, tint, disabled, 0.0);
                self.c.uic.assets.store.insert_named(&key, Texture::new(w, h, px, Sampler::LINEAR))
            }
        };
        self.out.quad(tid, r, if flip { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 1.0, 1.0] }, [1.0, 1.0, 1.0, 1.0]);
    }
}



// ---------------------------------------------------------------------------------------------------------------------------------------------------
// SHOP additions: what the shop widgets' Blueprints / native widgets set at runtime and the generic painter has no field for.

/// Runtime parameters of one widget's material brush: the Blueprint did `GetDynamicMaterial()` + `SetVectorParameterValue` / `SetScalarParameterValue` on it.
#[derive(Clone, Debug, Default)]
pub struct DynMat {
    pub overrides: fn_dxbc_cli::uniform::Overrides,
    pub textures: HashMap<String, String>,
    /// cache discriminator (what makes two parameter sets differ)
    pub key: String,
}

/// A native `FortMultiSizeItemCard` (leaf widget without a tree): the cosmetic item card material of `size` with the item's rarity colours and icon.
#[derive(Clone, Debug)]
pub struct CardSpec { pub size: String, pub rarity_colors: [[f32; 4]; 5], pub rarity: usize, pub icon: Option<String>, pub hero: bool }

#[derive(Default)]
pub struct DynMats {
    pub mats: HashMap<usize, DynMat>,
    pub text_color: HashMap<usize, [f32; 4]>,
    pub cards: HashMap<usize, CardSpec>,
}

impl<'a> Painter<'a> {
    /// Draws the material brush of widget `id` with its runtime parameters; false when the widget has none (the generic path then draws it).
    fn draw_dyn(&mut self, id: usize, b: &Brush, rect: R, tint: [f32; 4], tf: Tf) -> bool {
        let Some(d) = self.dynm.mats.get(&id).cloned() else { return false };
        let Some(res) = b.resource.clone() else { return false };
        if !matches!(self.c.ui.store.object_class(&res).as_deref(), Some("Material") | Some("MaterialInstanceConstant")) { return false; }
        let (r, fx, _) = tf.rect(self.px_rect(rect));
        if r.w <= 0.0 || r.h <= 0.0 { return true; }
        let tint = mul4(b.tint, tint);
        if tint[3] <= 0.0 { return true; }
        let (w, h) = (r.w.round().max(1.0) as u32, r.h.round().max(1.0) as u32);
        let boxed = b.draw == DrawAs::Box && b.margin.iter().any(|m| *m != 0.0);
        let key = format!("shopmat:{res}:{}:{w}x{h}:{boxed}:{:?}:{:?}:{:?}:{:?}", d.key, b.margin, b.image_size, tint.map(f32::to_bits), self.scale.to_bits());
        let tid = match self.c.uic.assets.store.named(&key) {
            Some(t) => t,
            None => {
                let store = self.c.ui.store.clone();
                match SlateMat::load_with(&store, &mut self.c.uic.assets, &res, &d.overrides, &d.textures) {
                    Ok(m) if m.is_ready() => {
                        for wm in &m.warnings { let s = format!("material {res}: {wm}"); if !self.missing.contains(&s) { self.missing.push(s); } }
                        let size = (w as f32 / self.scale, h as f32 / self.scale);
                        let px = m.raster(w, h, size, (b.image_size[0], b.image_size[1]), b.margin, boxed, tint, self.disabled, 0.0);
                        self.c.uic.assets.store.insert_named(&key, Texture::new(w, h, px, Sampler::LINEAR))
                    }
                    Ok(_) => { let s = format!("material {res}: no pixel shader"); if !self.missing.contains(&s) { self.missing.push(s); } return true; }
                    Err(e) => { let s = format!("material {res}: {e}"); if !self.missing.contains(&s) { self.missing.push(s); } return true; }
                }
            }
        };
        self.out.quad(tid, r, if fx ^ b.flip_x { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 1.0, 1.0] }, [1.0, 1.0, 1.0, 1.0]);
        true
    }

    /// `UFortMultiSizeItemCard` stand-in (native, no widget tree): the cooked cosmetic card material `M-Item-Card-Cosmetic-Inst-<size>` evaluated with the
    /// item's parameters (`Icon` texture, `Rarity 1..5` = RarityData Color1..5, `bIsHero`, `bIsItem` = 0, `bBorderOn` = 1, `bAnimateBorder` = rarity >= 4),
    /// the values the native card sets (read from the exe by the Locker port: `fn-ui/src/screens/athena_locker/card.rs`).
    fn paint_card(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let Some(spec) = self.dynm.cards.get(&id).cloned() else { return };
        let rect = self.c.lay.rect[id];
        let (r, _, _) = tf.rect(self.px_rect(rect));
        if r.w <= 0.0 || r.h <= 0.0 { return; }
        let (w, h) = (r.w.round().max(1.0) as u32, r.h.round().max(1.0) as u32);
        let key = format!("shopcard:{}:{}:{}:{:?}:{w}x{h}", spec.size, spec.hero, spec.rarity, spec.icon);
        let tid = match self.c.uic.assets.store.named(&key) {
            Some(t) => t,
            None => {
                let res = format!("/Game/UI/Foundation/Items/Materials/M-Item-Card-Cosmetic-Inst-{0}.M-Item-Card-Cosmetic-Inst-{0}", spec.size);
                let mut ov = fn_dxbc_cli::uniform::Overrides::default();
                for (i, c) in spec.rarity_colors.iter().enumerate() { ov.vectors.insert(format!("Rarity {}", i + 1), *c); }
                for (k, v) in [("bIsHero", if spec.hero { 1.0 } else { 0.0 }), ("bIsItem", 0.0), ("bIsSchematic", 0.0), ("bIsLead", 0.0), ("sSchematicAnimate", 0.0), ("bAnimateBorder", if spec.rarity >= 4 { 1.0 } else { 0.0 }), ("bBorderOn", 1.0), ("bIsLoading", 0.0)] { ov.scalars.insert(k.into(), v); }
                let mut tex = HashMap::new();
                if let Some(i) = &spec.icon { tex.insert("Icon".to_owned(), i.clone()); }
                let store = self.c.ui.store.clone();
                match SlateMat::load_with(&store, &mut self.c.uic.assets, &res, &ov, &tex) {
                    Ok(m) if m.is_ready() => {
                        let px = m.raster(w, h, (rect[2], rect[3]), (rect[2], rect[3]), [0.0; 4], false, [1.0; 4], false, 0.0);
                        self.c.uic.assets.store.insert_named(&key, Texture::new(w, h, px, Sampler::LINEAR))
                    }
                    Ok(_) => { let s = format!("{res}: no pixel shader"); if !self.missing.contains(&s) { self.missing.push(s); } return; }
                    Err(e) => { let s = format!("{res}: {e}"); if !self.missing.contains(&s) { self.missing.push(s); } return; }
                }
            }
        };
        self.out.quad(tid, r, [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 1.0, color[3]]);
    }
}