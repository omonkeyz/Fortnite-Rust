//! Paint pass: the arranged tree -> [`RenderList`]. Brushes (textures: stretch / nine-patch; materials: baked on the CPU through
//! [`SlateMat`](super::slate_mat::SlateMat) into a texture), text (font, size, colour, shadow, justification), borders and buttons.
use super::layout::{Ctx, R};
use super::slate_mat::SlateMat;
use super::store::{jf, jvec};
use super::styles::{Brush, BtnState, DrawAs};
use super::umg::Vis;
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

#[derive(Clone, Debug)]
pub struct BlurRegion { pub rect: Rect, pub strength: f32 }

thread_local! {
    static RASTER_DEADLINE: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
    static RASTER_PARTIAL: std::cell::RefCell<std::collections::HashMap<String, std::sync::mpsc::Receiver<Vec<u8>>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}
thread_local! {
    static RASTER_DEFERRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RASTER_LAST: std::cell::RefCell<HashMap<String, TextureId>> = std::cell::RefCell::new(HashMap::new());
}
pub fn with_raster_budget<R>(ms: u64, f: impl FnOnce() -> R) -> R {
    RASTER_DEFERRED.with(|d| d.set(false));
    RASTER_DEADLINE.with(|d| d.set(Some(std::time::Instant::now() + std::time::Duration::from_millis(ms))));
    let r = f();
    RASTER_DEADLINE.with(|d| d.set(None));
    r
}
pub fn raster_pending() -> bool { RASTER_DEFERRED.with(|d| d.get()) || RASTER_PARTIAL.with(|p| !p.borrow().is_empty()) }

pub struct Painter<'a> {
    pub c: Ctx<'a>,
    pub out: RenderList,
    pub mats: &'a mut HashMap<String, Option<Rc<SlateMat>>>,
    pub blur: Vec<BlurRegion>,
    pub missing: Vec<String>,
    pub scale: f32,
    /// an ancestor (or the widget itself) is disabled: Slate `ShouldBeEnabled(bParentEnabled)` is false for the whole subtree, which draws with the disabled effect
    pub disabled: bool,
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
        let pv = jvec::<2>(w.props.get("RenderTransformPivot")).unwrap_or([0.5, 0.5]);
        let outer = tf;
        let start = self.out.cmds.len();
        if !t.is_identity() {
            let (cx, cy) = ((rect[0] + rect[2] * pv[0]) * self.scale, (rect[1] + rect[3] * pv[1]) * self.scale);
            let (sx, sy) = (t.scale[0], t.scale[1]);
            tf = Tf { sx: tf.sx * sx, sy: tf.sy * sy, tx: tf.sx * (cx - sx * cx + t.translation[0] * self.scale) + tf.tx, ty: tf.sy * (cy - sy * cy + t.translation[1] * self.scale) + tf.ty };
        }
        // user widget colour and opacity
        if w.user.is_some() { if let Some(c) = jvec::<4>(w.props.get("ColorAndOpacity")) { color = mul4(color, c); } }
        let class = w.class.clone();
        let user = w.user.is_some();
        let kids = w.kids.clone();
        match class.as_str() {
            "Border" | "CommonBorder" => self.paint_border(id, tf, color),
            "Image" | "FortSZAwareImage" | "CommonLazyImage" => self.paint_image(id, tf, color),
            "Button" => self.paint_plain_button(id, tf, color),
            "TextBlock" | "CommonTextBlock" | "CommonNumericTextBlock" | "FortItemCountTextBlock" | "EditableText" => self.paint_text(id, tf, color),
            "BackgroundBlur" => {
                let s = self.c.ui.w[id].props.get("BlurStrength").and_then(jf).unwrap_or(0.0);
                let (r, _, _) = tf.rect(self.px_rect(rect));
                self.blur.push(BlurRegion { rect: r, strength: s });
                // INVENTORY FIX3 (inventory_gaps G5): the blur pass itself, in paint order (before the children); `bApplyAlphaToBlur` defaults to true
                // (UE 4.16 `UBackgroundBlur` constructor), so the strength is scaled by the inherited opacity (`SBackgroundBlur::OnPaint`)
                let a = if self.c.ui.w[id].props.get("bApplyAlphaToBlur").and_then(|v| v.as_bool()).unwrap_or(true) { color[3] } else { 1. };
                let p = &self.c.ui.w[id].props;
                let radius = if p.get("bOverrideAutoRadiusCalculation").and_then(|v| v.as_bool()).unwrap_or(false) { Some(p.get("BlurRadius").and_then(|v| v.as_i64()).unwrap_or(0) as i32) } else { None };
                self.out.blur(r, s * a, radius);
            }
            _ => {}
        }
        if user && self.c.ui.is_common_button(id) { self.paint_button(id, tf, color); }
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
        let clip = class == "BackgroundBlur";
        if clip { let (r, _, _) = tf.rect(self.px_rect(rect)); self.out.push_clip(r); }
        // widgets that do not take space are never painted
        for k in active { if self.c.ui.w[k].vis != Vis::Collapsed { self.paint(k, tf, child_color); } }
        if clip { self.out.pop_clip(); }
        if t.angle != 0.0 {
            let (px, py) = ((rect[0] + rect[2] * pv[0] + t.translation[0]) * self.scale, (rect[1] + rect[3] * pv[1] + t.translation[1]) * self.scale);
            crate::screens::xform::rotate_quads_since(&mut self.out, start, (outer.sx * px + outer.tx, outer.sy * py + outer.ty), t.angle);
        }
    }

    fn paint_border(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let store = self.c.ui.store.clone();
        let style_cls = self.c.ui.w[id].props.get("Style").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_owned());
        let brush = match style_cls {
            Some(c) => self.c.ui.styles.border(&store, &c).background,
            None => self.c.ui.w[id].props.get("Background").and_then(Brush::from_json),
        };
        let Some(b) = brush else { return };
        let bc = jvec::<4>(self.c.ui.w[id].props.get("BrushColor")).unwrap_or([1.0; 4]);
        let rect = self.c.lay.rect[id];
        // INVENTORY FIX3 (inventory_gaps G1): the border's dynamic-material scalar parameters as well, the same as `paint_image`
        // (`ItemDetailsHeader.UpdateItemsForWidgets`: `RarityBorderMID.SetScalarParameterValue(bIsSchematic, 0)`; the MIC default is 1)
        self.c.ui.cur_scalars = self.c.ui.w[id].mat_scalars.clone();
        self.c.ui.cur_vectors = self.c.ui.w[id].mat_vectors.clone(); // INVENTORY FIX2: the border's dynamic-material vector parameters
        self.draw_brush(&b, rect, mul4(bc, color), tf, false);
        self.c.ui.cur_scalars.clear();
        self.c.ui.cur_vectors.clear();
    }

    fn paint_image(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let Some(b) = self.c.brush_of(id) else { return };
        let cp = self.c.ui.w[id].props.get("ColorAndOpacity");
        let ci = jvec::<4>(cp).or_else(|| jvec::<4>(cp.filter(|c| !c.get("ColorUseRule").and_then(|r| r.as_str()).map_or(false, |r| r.contains("Foreground"))).and_then(|c| c.get("SpecifiedColor")))).unwrap_or([1.0; 4]);
        let rect = self.c.lay.rect[id];
        self.c.ui.cur_scalars = self.c.ui.w[id].mat_scalars.clone();
        self.c.ui.cur_vectors = self.c.ui.w[id].mat_vectors.clone(); // INVENTORY FIX2
        self.c.ui.cur_textures = self.c.ui.w[id].mat_textures.clone();
        self.draw_brush(&b, rect, mul4(ci, color), tf, false);
        self.c.ui.cur_scalars.clear();
        self.c.ui.cur_vectors.clear();
        self.c.ui.cur_textures.clear();
    }

    /// `UButton` (`SButton`): the `WidgetStyle` brush of the current state (Normal / Hovered / Pressed / Disabled).
    fn paint_plain_button(&mut self, id: usize, tf: Tf, color: [f32; 4]) {
        let state = self.c.ui.w[id].btn_state;
        let key = match state { BtnState::Hovered | BtnState::SelectedHovered => "Hovered", BtnState::Pressed | BtnState::SelectedPressed => "Pressed", BtnState::Disabled => "Disabled", _ => "Normal" };
        let Some(b) = self.c.ui.w[id].props.get("WidgetStyle").and_then(|s| s.get(key)).and_then(Brush::from_json) else { return };
        let rect = self.c.lay.rect[id];
        self.draw_brush(&b, rect, color, tf, state == BtnState::Disabled);
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
        let m = super::store::jmargin(self.c.ui.w[id].props.get("Margin"));
        let cp = self.c.ui.w[id].props.get("ColorAndOpacity");
        let ci = jvec::<4>(cp).or_else(|| jvec::<4>(cp.filter(|c| !c.get("ColorUseRule").and_then(|r| r.as_str()).map_or(false, |r| r.contains("Foreground"))).and_then(|c| c.get("SpecifiedColor")))).unwrap_or([1.0; 4]);
        let base = if cp.is_some() && super::umg::is_styled_common_text(&self.c.ui.w[id].class, &self.c.ui.w[id].props) { [1.0; 4] } else { st.color };
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
        let deadline = RASTER_DEADLINE.with(|d| d.get());
        let rs = self.c.ui.mat_res.iter().find(|(m, _)| m == res).map_or(1.0, |(_, k)| *k);
        let (w, h) = ((r.w * rs).round().max(1.0) as u32, (r.h * rs).round().max(1.0) as u32);
        let boxed = b.draw == DrawAs::Box && b.margin.iter().any(|m| *m != 0.0);
        let sc: Vec<(String, f32)> = self.c.ui.cur_scalars.clone();
        let vc: Vec<(String, [f32; 4])> = self.c.ui.cur_vectors.clone(); // INVENTORY FIX2: MID vector parameters
        let mut vkey: Vec<(String, [u32; 4])> = vc.iter().map(|(k, v)| (k.clone(), v.map(f32::to_bits))).collect();
        let tc: Vec<(String, String)> = self.c.ui.cur_textures.clone();
        for (k, v) in &tc { vkey.push((format!("{k}={v}"), [0; 4])); }
        let key = format!("lobbymat:{res}:{w}x{h}:{}:{:?}:{:?}:{:?}:{:?}:{}:{:?}{}", boxed, b.margin, b.image_size, tint.map(f32::to_bits), self.scale.to_bits(), disabled, sc.iter().map(|(k, v)| (k.clone(), v.to_bits())).collect::<Vec<_>>(), if vkey.is_empty() { String::new() } else { format!(":{vkey:?}") });
        let timed = self.c.ui.timed_mats.iter().any(|m| m == res);
        let tid = match self.c.uic.assets.store.named(&key) {
            // FRONT END: an animated material brush is re-rastered at the current material time into the same texture
            Some(t) if timed => {
                if let Some(m) = self.mats.values().flatten().find(|m| m.path == res).cloned() {
                    let size = (r.w / self.scale, r.h / self.scale);
                    let px = m.raster(w, h, size, (b.image_size[0], b.image_size[1]), b.margin, boxed, tint, disabled, self.c.ui.mat_time);
                    self.c.uic.assets.store.replace(t, w, h, px);
                }
                t
            }
            Some(t) => t,
            None => {
                // a material with runtime scalar parameters (a dynamic material instance) is its own material object
                let mkey = if sc.is_empty() && vkey.is_empty() { res.to_owned() } else { format!("{res}|{:?}|{vkey:?}", sc.iter().map(|(k, v)| (k.clone(), v.to_bits())).collect::<Vec<_>>()) };
                if !self.mats.contains_key(&mkey) {
                    if deadline.is_some_and(|dl| std::time::Instant::now() >= dl) { RASTER_DEFERRED.with(|d| d.set(true)); return; }
                    let ov = fn_dxbc_cli::uniform::Overrides { vectors: vc.iter().cloned().collect(), scalars: sc.iter().cloned().collect() };
                    let mut subst = self.c.ui.tex_subst.clone();
                    for (k, v) in &tc { subst.insert(k.clone(), v.clone()); }
                    let m = SlateMat::load_with(&self.c.ui.store.clone(), &mut self.c.uic.assets, res, &ov, &subst);
                    let m = match m { Ok(m) => Some(Rc::new(m)), Err(e) => { self.missing.push(format!("material {res}: {e}")); None } };
                    if let Some(m) = &m { for wmsg in &m.warnings { self.missing.push(format!("material {res}: {wmsg}")); } }
                    self.mats.insert(mkey.clone(), m);
                }
                let Some(m) = self.mats.get(&mkey).cloned().flatten() else { return };
                if !m.is_ready() { return; }
                let size = (r.w / self.scale, r.h / self.scale);
                let time = if timed { self.c.ui.mat_time } else { 0.0 };
                let px = match deadline {
                    None => m.raster(w, h, size, (b.image_size[0], b.image_size[1]), b.margin, boxed, tint, disabled, time),
                    Some(_) => {
                        let mut done = None;
                        RASTER_PARTIAL.with(|p| {
                            let mut p = p.borrow_mut();
                            let rx = p.entry(key.clone()).or_insert_with(|| {
                                let (tx, rx) = std::sync::mpsc::channel();
                                let (job, native, margin) = (m.job(), (b.image_size[0], b.image_size[1]), b.margin);
                                let _ = std::thread::Builder::new().name("ui raster".into()).spawn(move || { let _ = tx.send(job.raster_rows(w, h, size, native, margin, boxed, tint, disabled, time, 0, h)); });
                                rx
                            });
                            match rx.try_recv() {
                                Ok(px) => { p.remove(&key); done = Some(px); }
                                Err(std::sync::mpsc::TryRecvError::Disconnected) => { p.remove(&key); }
                                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                            }
                        });
                        match done {
                            Some(px) => px,
                            None => {
                                if let Some(t) = RASTER_LAST.with(|l| l.borrow().get(&format!("{res}:{w}x{h}")).copied()) { self.out.quad(t, r, if flip { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 1.0, 1.0] }, [1.0, 1.0, 1.0, 1.0]); }
                                return;
                            }
                        }
                    }
                };
                self.c.uic.assets.store.insert_named(&key, Texture::new(w, h, px, Sampler::LINEAR))
            }
        };
        if deadline.is_some() { RASTER_LAST.with(|l| { l.borrow_mut().insert(format!("{res}:{w}x{h}"), tid); }); }
        self.out.quad(tid, r, if flip { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 1.0, 1.0] }, [1.0, 1.0, 1.0, 1.0]);
    }
}

