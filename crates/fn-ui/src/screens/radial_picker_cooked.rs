use super::athena_lobby::layout::{self as ul, Ctx};
use super::athena_lobby::paint::{Painter, Tf};
use super::athena_lobby::slate_mat::SlateMat;
use super::athena_lobby::store::Store;
use super::athena_lobby::styles::Brush;
use super::athena_lobby::umg::{Ui, Vis};
use super::interaction as basic;
use crate::draw::RenderList;
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::{art, ascent, atlas_px, line_height, put_text, text_width, Snap, WHITE};
use crate::text::FontId;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const CLASS: &str = "/Game/UI/InGame/HUD/RadialPicker.RadialPicker_C";
pub const OPTIONS: usize = 8;
pub const OPTION_RADIUS: f32 = 230.;
pub const POINTER_RADIUS: f32 = 130.;
pub const SELECTED_TINT: [f32; 4] = [1., 1., 0., 1.];
pub const IDLE_TINT: [f32; 4] = [1., 1., 1., 0.6];
pub const SELECTED_SCALE: f32 = 1.2;
pub const OK_IDLE_OPACITY: f32 = 0.5;
pub const PIE_INNER_RADIUS: f32 = 0.35;
pub const PIE_OUTER_RADIUS: f32 = 1.0;
pub const PIE_HARDNESS: f32 = 7.0;
pub const PIE_COLOR_LINEAR: [f32; 3] = [0.0, 0.001_214, 0.010_33];
pub const PIE_TEXTURE: &str = "radial-picker-pie-ring";

fn sphere_gradient(d: f32, radius: f32, hardness: f32) -> f32 { ((1. - d / radius.max(1e-4)) * hardness).clamp(0., 1.) }
pub fn pie_alpha(d: f32) -> f32 { sphere_gradient(d, PIE_OUTER_RADIUS, PIE_HARDNESS) * (1. - sphere_gradient(d, PIE_INNER_RADIUS, PIE_HARDNESS)) }
fn lin_to_srgb(c: f32) -> f32 { if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 } }
fn pie_texture(n: u32) -> crate::assets::Texture {
    let mut px = vec![0u8; (n * n * 4) as usize];
    let rgb = PIE_COLOR_LINEAR.map(|c| (lin_to_srgb(c) * 255.).round() as u8);
    for y in 0..n { for x in 0..n {
        let (u, v) = ((x as f32 + 0.5) / n as f32 * 2. - 1., (y as f32 + 0.5) / n as f32 * 2. - 1.);
        let a = pie_alpha((u * u + v * v).sqrt());
        let i = ((y * n + x) * 4) as usize;
        px[i..i + 3].copy_from_slice(&rgb); px[i + 3] = (a * 255.).round() as u8;
    } }
    crate::assets::Texture::new(n, n, px, crate::assets::Sampler::LINEAR)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RadialOption { pub label: String, pub icon: Option<String>, pub enabled: bool }

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RadialView { pub options: Vec<RadialOption>, pub active: Option<usize>, pub pointer_deg: f32, pub pointer_visible: bool, pub gamepad: bool, pub confirm_key: String, pub cancel_key: String, pub option_keys: Vec<String> }

pub fn option_angle(i: usize, n: usize) -> f32 { 90. - (360. / n.max(1) as f32) * i as f32 }
pub fn option_position(i: usize, n: usize) -> (f32, f32) { let a = option_angle(i, n).to_radians(); (a.cos() * OPTION_RADIUS, a.sin() * -OPTION_RADIUS) }

struct Item { root: usize, image: Option<usize>, label: Option<usize>, keybind: Option<usize>, switcher: Option<usize> }

struct Cooked { ui: Ui, root: usize, overlay: Option<usize>, radial: Option<usize>, pie: Option<crate::draw::TextureId>, canvas: Option<usize>, items: Vec<Item>, pointer: Option<usize>, ok_text: Option<usize>, ok_key: Option<usize>, cancel_key: Option<usize>, mats: HashMap<String, Option<Rc<SlateMat>>>, missing: Vec<String> }

fn keycap(ui: &mut Ui, k: usize) {
    ui.w[k].class = "Spacer".into();
    ui.w[k].props["Size"] = json!([basic::KEYCAP_MIN, basic::KEYCAP_MIN]);
    for i in 0..ui.w.len() { if i != k && ui.is_descendant(i, k) { ui.w[i].vis = Vis::Collapsed; } }
}

impl Cooked {
    fn load(install: &Path) -> Result<Cooked, String> {
        let store = Rc::new(Store::open(install)?);
        let mut ui = Ui::new(store);
        let root = ui.add_class_instance(CLASS, None, "", json!({}), None).ok_or("cannot compose RadialPicker_C")?;
        let v = |ui: &Ui, owner: usize, n: &str| ui.var(owner, n).or_else(|| ui.find_in(owner, n));
        let mut items = vec![];
        for i in 0..OPTIONS {
            let Some(r) = v(&ui, root, &format!("PickerOption{i}")) else { return Err(format!("RadialPicker_C has no PickerOption{i}")) };
            let it = Item { root: r, image: v(&ui, r, "Image"), label: v(&ui, r, "TextLabel"), keybind: v(&ui, r, "KeybindWidget"), switcher: v(&ui, r, "ImageOrItemSwitcher") };
            if let Some(k) = it.keybind { keycap(&mut ui, k); }
            items.push(it);
        }
        let pointer = v(&ui, root, "ImageRadialPointer");
        let ok_text = v(&ui, root, "Text_OK");
        let ok_key = v(&ui, root, "KeybindWidget_81");
        let cancel_key = ui.w.iter().find(|w| w.name == "KeybindWidget" && ui.is_descendant(w.id, root) && !items.iter().any(|it| ui.is_descendant(w.id, it.root))).map(|w| w.id);
        for k in [ok_key, cancel_key].into_iter().flatten() { keycap(&mut ui, k); }
        let canvas = v(&ui, root, "CanvasPanel_0");
        let (overlay, radial) = (v(&ui, root, "Overlay_0"), v(&ui, root, "Image_Radial"));
        Ok(Cooked { ui, root, overlay, radial, pie: None, canvas, items, pointer, ok_text, ok_key, cancel_key, mats: HashMap::new(), missing: vec![] })
    }

    fn apply(&mut self, v: &RadialView) {
        let n = v.options.len().min(OPTIONS);
        self.ui.w[self.root].vis = Vis::Visible;
        for (i, it) in self.items.iter().enumerate() {
            if i >= n { self.ui.w[it.root].vis = Vis::Collapsed; continue; }
            let o = &v.options[i];
            self.ui.w[it.root].vis = Vis::HitTestInvisible;
            let (x, y) = option_position(i, n);
            let slot = &mut self.ui.w[it.root].slot;
            slot["LayoutData"]["Offsets"]["Left"] = json!(x);
            slot["LayoutData"]["Offsets"]["Top"] = json!(y);
            slot["LayoutData"]["Anchors"] = json!({"Minimum": [0.5, 0.5], "Maximum": [0.5, 0.5]});
            slot["LayoutData"]["Alignment"] = json!([0.5, 0.5]);
            slot["bAutoSize"] = json!(true);
            if let Some(s) = it.switcher { self.ui.w[s].props["ActiveWidgetIndex"] = json!(0); }
            let selected = v.active == Some(i);
            let tint = if selected && o.enabled { SELECTED_TINT } else { IDLE_TINT };
            if let Some(l) = it.label { self.ui.w[l].text = Some(o.label.clone()); self.ui.w[l].props["ColorAndOpacity"] = json!(tint); }
            if let Some(im) = it.image {
                let base = self.ui.w[im].brush.clone().or_else(|| self.ui.w[im].props.get("Brush").and_then(Brush::from_json));
                if let Some(mut b) = base { if o.icon.is_some() { b.resource = o.icon.clone(); } b.tint = tint; self.ui.w[im].brush = Some(b); }
                self.ui.w[im].transform.scale = if selected { [SELECTED_SCALE; 2] } else { [1.; 2] };
            }
            self.ui.w[it.root].enabled = o.enabled;
            if let Some(k) = it.keybind { self.ui.w[k].vis = if v.gamepad { Vis::Collapsed } else { Vis::SelfHitTestInvisible }; }
        }
        if let Some(p) = self.pointer {
            self.ui.w[p].vis = if v.pointer_visible { Vis::SelfHitTestInvisible } else { Vis::Hidden };
            let a = v.pointer_deg.to_radians();
            self.ui.w[p].transform.translation = [a.cos() * POINTER_RADIUS, a.sin() * -POINTER_RADIUS];
            self.ui.w[p].transform.angle = -v.pointer_deg;
        }
        let label = v.active.and_then(|a| v.options.get(a)).map(|o| o.label.clone()).unwrap_or_default();
        if let Some(t) = self.ok_text { self.ui.w[t].text = Some(label); }
    }

    fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, v: &RadialView) -> RenderList {
        let s = dpi_scale(w, h);
        self.apply(v);
        let (sw, sh) = (w / s, h / s);
        let n = self.ui.w.len();
        let mut lay = ul::new_layout(n, s);
        for _ in 0..2 {
            let wrap = lay.wrap_w.clone();
            lay = ul::new_layout(n, s);
            lay.wrap_w = wrap;
            let mut c = Ctx { ui: &mut self.ui, uic: &mut *uic, lay };
            let d = c.desired(self.root);
            let x = (((sw - d[0]) * 0.5) * s).round() / s;
            let y = (((sh - d[1]) * 0.5) * s).round() / s;
            c.arrange(self.root, [x, y, d[0], d[1]]);
            if let Some(cv) = self.canvas {
                let r = c.lay.rect[cv];
                let (cx, cy) = (r[0] + r[2] * 0.5, r[1] + r[3] * 0.5);
                let no = v.options.len().min(OPTIONS);
                for (i, it) in self.items.iter().enumerate().take(no) {
                    let (ox, oy) = option_position(i, no);
                    let d = c.desired(it.root);
                    c.arrange(it.root, [cx + ox - d[0] * 0.5, cy + oy - d[1] * 0.5, d[0], d[1]]);
                }
            }
            lay = c.lay;
        }
        let mut caps: Vec<(usize, String, f32)> = vec![];
        for (i, it) in self.items.iter().enumerate() { if let Some(k) = it.keybind { if i < v.options.len() && !v.gamepad { caps.push((k, v.option_keys.get(i).cloned().unwrap_or_else(|| (i + 1).to_string()), 1.)); } } }
        if let Some(k) = self.ok_key { caps.push((k, v.confirm_key.clone(), if v.active.is_some() { 1. } else { OK_IDLE_OPACITY })); }
        if let Some(k) = self.cancel_key { caps.push((k, v.cancel_key.clone(), 1.)); }
        let caps: Vec<([f32; 4], String, f32)> = caps.into_iter().filter(|(k, _, _)| self.ui.w[*k].vis.draws()).map(|(k, t, a)| (lay.rect[k], t, a)).collect();
        let mut pt = Painter { c: Ctx { ui: &mut self.ui, uic, lay }, out: RenderList::new(), mats: &mut self.mats, blur: vec![], missing: vec![], scale: s, disabled: false };
        match (self.overlay, self.radial) {
            (Some(ov), Some(rad)) => {
                let pie = *self.pie.get_or_insert_with(|| pt.c.uic.assets.store.insert_named(PIE_TEXTURE, pie_texture(256)));
                let alpha = pt.c.ui.w[rad].props.get("ColorAndOpacity").and_then(|c| c.as_array()).and_then(|a| a.get(3)).and_then(|x| x.as_f64()).unwrap_or(1.) as f32;
                for k in pt.c.ui.w[ov].kids.clone() {
                    if pt.c.ui.w[k].vis == Vis::Collapsed { continue; }
                    if k == rad { let r = pt.c.lay.rect[rad]; pt.out.sprite(pie, Rect::new(r[0] * s, r[1] * s, r[2] * s, r[3] * s), [1., 1., 1., alpha]); continue; }
                    pt.paint(k, Tf::ID, [1.; 4]);
                }
            }
            _ => pt.paint(self.root, Tf::ID, [1.; 4]),
        }
        for (r, key, alpha) in caps {
            let kc = Rect::new((r[0] * s).round(), (r[1] * s).round(), r[2] * s, r[3] * s);
            if let Some(icon) = crate::screens::target_info::key_icon(&key) { crate::screens::target_info::draw_key_icon(pt.c.uic, &mut pt.out, icon, kc, s); continue; }
            let key = crate::screens::target_info::hud_key_text(&key);
            let frame = art(pt.c.uic, basic::FRAME_TEXTURE);
            pt.out.nine_slice(frame, kc, [basic::FRAME_MARGIN; 4], s, [1., 1., 1., basic::FRAME_ALPHA * alpha]);
            let kpx = atlas_px(basic::KEYCAP_FONT, s);
            let kw = text_width(pt.c.uic, FontId::Bold, &key, kpx);
            let (lh, asc) = (line_height(pt.c.uic, FontId::Bold, kpx), ascent(pt.c.uic, FontId::Bold, kpx));
            put_text(pt.c.uic, &mut pt.out, FontId::Bold, kpx, kc.x + (kc.w - kw) * 0.5, (kc.y + (kc.h - lh) * 0.5 + asc).round(), &key, [WHITE[0], WHITE[1], WHITE[2], WHITE[3] * alpha], None, Snap::PerGlyph);
        }
        let missing = std::mem::take(&mut pt.missing);
        let out = std::mem::take(&mut pt.out);
        for m in missing { if !self.missing.contains(&m) { eprintln!("[radial] not drawn: {m}"); self.missing.push(m); } }
        out
    }
}

#[derive(Default)]
pub struct RadialPicker { install: Option<PathBuf>, cooked: Option<Option<Cooked>> }
impl Clone for RadialPicker { fn clone(&self) -> Self { RadialPicker { install: self.install.clone(), cooked: None } } }
impl RadialPicker {
    pub fn set_install(&mut self, install: PathBuf) { if self.install.as_ref() != Some(&install) { self.install = Some(install); self.cooked = None; } }
    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, v: &RadialView, out: &mut RenderList) -> bool {
        if self.cooked.is_none() {
            let c = match self.install.as_ref() { Some(i) => Cooked::load(i), None => Err("no install set".into()) };
            self.cooked = Some(match c { Ok(c) => { eprintln!("[radial] RadialPicker_C composed ({} widgets)", c.ui.w.len()); Some(c) } Err(e) => { eprintln!("[radial] RadialPicker not composed: {e}"); None } });
        }
        let Some(Some(c)) = self.cooked.as_mut() else { return false };
        out.extend(c.draw(ctx, w, h, v));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_cooked_radial_picker_composes_with_its_named_widgets() {
        let inst = crate::pak_assets::default_install();
        if !inst.exists() { return; }
        let c = match Cooked::load(&inst) { Ok(c) => c, Err(e) => panic!("{e}") };
        assert_eq!(c.items.len(), OPTIONS);
        assert!(c.items.iter().all(|i| i.image.is_some() && i.label.is_some() && i.switcher.is_some()));
        assert!(c.pointer.is_some() && c.ok_text.is_some() && c.ok_key.is_some() && c.cancel_key.is_some());
    }
    #[test] fn the_radial_draws_two_emote_options() {
        let Some(mut uic) = crate::screens::hud_cooked::tests::ctx() else { return };
        let inst = crate::pak_assets::default_install();
        let mut r = RadialPicker::default();
        r.set_install(inst);
        for (name, active) in [("radial_none", None), ("radial_dance", Some(0)), ("radial_gunshow", Some(1))] {
            let v = RadialView { options: vec![RadialOption { label: "Dance Moves".into(), icon: Some("/Game/Athena/Textures/Icons/Dances/Icon-DanceMoves.Icon-DanceMoves".into()), enabled: true }, RadialOption { label: "GUN SHOW (x1)".into(), icon: Some("/Game/Athena/Textures/Icons/ConsumableEmotes/Icon-GunShow.Icon-GunShow".into()), enabled: true }],
                active, pointer_deg: if active == Some(1) { -90. } else { 90. }, pointer_visible: active.is_some(), gamepad: false, confirm_key: "LeftMouseButton".into(), cancel_key: "RightMouseButton".into(), option_keys: vec!["1".into(), "2".into()] };
            let mut out = RenderList::new();
            assert!(r.build(&mut uic, 1920., 1080., &v, &mut out));
            assert!(!out.cmds.is_empty());
            crate::screens::hud_cooked::tests::save(name, &mut uic, &out, 1920, 1080);
        }
        if let Some(Some(c)) = r.cooked.as_ref() { eprintln!("missing: {:?}", c.missing); }
    }
}
