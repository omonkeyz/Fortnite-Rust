use super::athena_lobby::layout::{self as ul, Ctx};
use super::athena_lobby::paint::{Painter, Tf};
use super::athena_lobby::slate_mat::SlateMat;
use super::athena_lobby::store::Store;
use super::athena_lobby::umg::{Ui, Vis};
use super::interaction as basic;
use super::item_header::loc;
use crate::draw::RenderList;
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::{art, ascent, atlas_px, line_height, put_text, text_width, Snap, WHITE};
use crate::text::FontId;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const CLASS: &str = "/Game/Athena/HUD/AthenaAerialFeedback.AthenaAerialFeedback_C";
pub const NAMESPACE: &str = "[7542B0684376D8F3BF6D9B886221F029]";
pub const SLOT_PADDING_LEFT: f32 = 287.;
pub const SLOT_PADDING_BOTTOM: f32 = 50.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerialAction { Jump, DeployGlider, Skydive }

#[derive(Clone, Debug, PartialEq)]
pub struct AerialPrompt { pub key: String, pub action: AerialAction }

struct Cooked { ui: Ui, root: usize, feedback: Option<usize>, keybind: Option<usize>, text: Option<usize>, mats: HashMap<String, Option<Rc<SlateMat>>>, missing: Vec<String> }

impl Cooked {
    fn load(install: &Path) -> Result<Cooked, String> {
        let store = Rc::new(Store::open(install)?);
        let mut ui = Ui::new(store);
        let root = ui.add_class_instance(CLASS, None, "", json!({}), None).ok_or("cannot compose AthenaAerialFeedback_C")?;
        let v = |ui: &Ui, n: &str| ui.var(root, n).or_else(|| ui.find_in(root, n));
        let (feedback, keybind, text) = (v(&ui, "Feedback"), v(&ui, "KeybindWidget_0"), v(&ui, "FeedbackText"));
        if let Some(k) = keybind { ui.w[k].class = "Spacer".into(); ui.w[k].props["Size"] = json!([basic::KEYCAP_MIN, basic::KEYCAP_MIN]); }
        if let Some(k) = keybind { for i in 0..ui.w.len() { if i != k && ui.is_descendant(i, k) { ui.w[i].vis = Vis::Collapsed; } } }
        Ok(Cooked { ui, root, feedback, keybind, text, mats: HashMap::new(), missing: vec![] })
    }
    fn action_text(&self, a: AerialAction, glider: &str) -> String {
        let s = &self.ui.store;
        match a {
            AerialAction::Jump => loc(s, NAMESPACE, "377864294736257A6FC15C93BFB21574", "To Jump"),
            AerialAction::DeployGlider => loc(s, NAMESPACE, "69B4CD994A4389965BA22D98DD59B1F5", "To Deploy {Glider}").replace("{Glider}", glider),
            AerialAction::Skydive => loc(s, NAMESPACE, "06FB95D54EA2E03D88BFE1AF883A8B81", "To Skydive"),
        }
    }
    fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, p: &AerialPrompt, glider: &str) -> RenderList {
        let s = dpi_scale(w, h);
        let t = self.action_text(p.action, glider);
        if let Some(i) = self.text { self.ui.w[i].text = Some(t); }
        if let Some(i) = self.feedback { self.ui.w[i].vis = Vis::HitTestInvisible; }
        self.ui.w[self.root].vis = Vis::SelfHitTestInvisible;
        if let Some(k) = self.keybind {
            let kpx = atlas_px(basic::KEYCAP_FONT, s);
            let kw = if crate::screens::target_info::key_icon(&p.key).is_some() { 0. } else { text_width(uic, FontId::Bold, &crate::screens::target_info::hud_key_text(&p.key), kpx) / s };
            let kh = line_height(uic, FontId::Bold, kpx) / s;
            self.ui.w[k].props["Size"] = json!([(kw + 2. * basic::KEYCAP_PAD).max(basic::KEYCAP_MIN), (kh + 2. * basic::KEYCAP_PAD).max(basic::KEYCAP_MIN)]);
        }
        let (sw, sh) = (w / s, h / s);
        let n = self.ui.w.len();
        let mut lay = ul::new_layout(n, s);
        for _ in 0..2 {
            let wrap = lay.wrap_w.clone();
            lay = ul::new_layout(n, s);
            lay.wrap_w = wrap;
            let mut c = Ctx { ui: &mut self.ui, uic: &mut *uic, lay };
            let d = c.desired(self.root);
            let (aw, ah) = (sw - SLOT_PADDING_LEFT, sh - SLOT_PADDING_BOTTOM);
            let x = ((SLOT_PADDING_LEFT + (aw - d[0]) * 0.5) * s).round() / s;
            let y = (((ah - d[1]) * 0.5) * s).round() / s;
            c.arrange(self.root, [x, y, d[0], d[1]]);
            lay = c.lay;
        }
        let key_rect = self.keybind.filter(|&k| self.ui.w[k].vis.draws()).map(|k| lay.rect[k]);
        let mut pt = Painter { c: Ctx { ui: &mut self.ui, uic, lay }, out: RenderList::new(), mats: &mut self.mats, blur: vec![], missing: vec![], scale: s, disabled: false };
        pt.paint(self.root, Tf::ID, [1.; 4]);
        if let Some(r) = key_rect {
            let ks = pt.c.ui.w[self.keybind.unwrap()].props["Size"].as_array().map(|a| [a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32]).unwrap_or([r[2], r[3]]);
            let kc = Rect::new(((r[0] + (r[2] - ks[0]) * 0.5) * s).round(), ((r[1] + (r[3] - ks[1]) * 0.5) * s).round(), ks[0] * s, ks[1] * s);
            if let Some(icon) = crate::screens::target_info::key_icon(&p.key) { crate::screens::target_info::draw_key_icon(pt.c.uic, &mut pt.out, icon, kc, s); } else {
                let key = &crate::screens::target_info::hud_key_text(&p.key);
                let frame = art(pt.c.uic, basic::FRAME_TEXTURE);
                pt.out.nine_slice(frame, kc, [basic::FRAME_MARGIN; 4], s, [1., 1., 1., basic::FRAME_ALPHA]);
                let kpx = atlas_px(basic::KEYCAP_FONT, s);
                let kw = text_width(pt.c.uic, FontId::Bold, key, kpx);
                let (lh, asc) = (line_height(pt.c.uic, FontId::Bold, kpx), ascent(pt.c.uic, FontId::Bold, kpx));
                put_text(pt.c.uic, &mut pt.out, FontId::Bold, kpx, kc.x + (kc.w - kw) * 0.5, (kc.y + (kc.h - lh) * 0.5 + asc).round(), key, WHITE, None, Snap::PerGlyph);
            }
        }
        let missing = std::mem::take(&mut pt.missing);
        let out = std::mem::take(&mut pt.out);
        for m in missing { if !self.missing.contains(&m) { eprintln!("[aerial] not drawn: {m}"); self.missing.push(m); } }
        out
    }
}

#[derive(Default)]
pub struct AerialFeedback { install: Option<PathBuf>, cooked: Option<Option<Cooked>> }
impl Clone for AerialFeedback { fn clone(&self) -> Self { AerialFeedback { install: self.install.clone(), cooked: None } } }
impl AerialFeedback {
    pub fn store(&self) -> Option<Rc<Store>> { match &self.cooked { Some(Some(c)) => Some(c.ui.store.clone()), _ => None } }
    pub fn set_install(&mut self, install: PathBuf) { if self.install.as_ref() != Some(&install) { self.install = Some(install); self.cooked = None; } }
    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, p: &AerialPrompt, glider: &str, out: &mut RenderList) -> bool {
        if self.cooked.is_none() {
            let c = match self.install.as_ref() { Some(i) => Cooked::load(i), None => Err("no install set".into()) };
            self.cooked = Some(match c { Ok(c) => Some(c), Err(e) => { eprintln!("[aerial] AthenaAerialFeedback not composed: {e}"); None } });
        }
        let Some(Some(c)) = self.cooked.as_mut() else { return false };
        out.extend(c.draw(ctx, w, h, p, glider));
        true
    }
}
