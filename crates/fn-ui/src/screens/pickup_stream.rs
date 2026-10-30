use super::athena_lobby::layout::{self as ul, Ctx};
use super::athena_lobby::paint::{Painter, Tf};
use super::athena_lobby::slate_mat::SlateMat;
use super::athena_lobby::store::Store;
use super::athena_lobby::styles::Brush;
use super::athena_lobby::umg::{Ui, Vis};
use crate::draw::RenderList;
use crate::layout::dpi_scale;
use crate::scene::UiContext;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

pub const MESSAGE_CLASS: &str = "/Game/UI/InGame/HUD/PickupMessageItem.PickupMessageItem_C";
pub const MAX_DISPLAYED: usize = 3;
pub const DISPLAY_DURATION: f32 = 2.5;
pub const REMOVE_DURATION: f32 = 0.2;
pub const APPEAR_LEN: f32 = 0.3;
pub const DISAPPEAR_LEN: f32 = 0.2;
pub const MESSAGE_HEIGHT: f32 = 56.;
pub const MESSAGE_PAD_BOTTOM: f32 = 8.;
pub const SETUP_HEIGHT: f32 = 0.1;

#[derive(Clone, Debug, PartialEq)]
pub struct PickedUp { pub name: String, pub icon: String, pub count: u32 }

#[derive(Clone, Debug, PartialEq)]
pub struct Message { pub name: String, pub icon: String, pub stack: u32, pub shown_for: f32, pub remaining: f32, pub removing: Option<f32> }

impl Message {
    pub fn height(&self) -> f32 {
        match self.removing {
            Some(t) => lerp(MESSAGE_HEIGHT, SETUP_HEIGHT, (t / DISAPPEAR_LEN).clamp(0., 1.)),
            None => lerp(0., MESSAGE_HEIGHT, (self.shown_for / APPEAR_LEN).clamp(0., 1.)),
        }
    }
    pub fn pad_bottom(&self) -> f32 { match self.removing { Some(t) => lerp(MESSAGE_PAD_BOTTOM, 0., (t / DISAPPEAR_LEN).clamp(0., 1.)), None => MESSAGE_PAD_BOTTOM } }
    pub fn content_alpha(&self) -> f32 { match self.removing { Some(t) => lerp(1., 0., (t / DISAPPEAR_LEN).clamp(0., 1.)), None => 1. } }
}
fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamState { pub shown: Vec<Message>, pub queued: Vec<Message> }

impl StreamState {
    pub fn add(&mut self, p: &PickedUp) {
        if p.count == 0 { return; }
        if let Some(m) = self.shown.iter_mut().find(|m| m.name == p.name) {
            m.stack += p.count;
            m.removing = None;
            m.remaining = DISPLAY_DURATION;
            return;
        }
        if let Some(m) = self.queued.iter_mut().find(|m| m.name == p.name) { m.stack += p.count; return; }
        self.queued.push(Message { name: p.name.clone(), icon: p.icon.clone(), stack: p.count, shown_for: 0., remaining: DISPLAY_DURATION, removing: None });
        self.fill();
    }
    fn fill(&mut self) { while self.shown.len() < MAX_DISPLAYED && !self.queued.is_empty() { self.shown.push(self.queued.remove(0)); } }
    pub fn tick(&mut self, dt: f32) {
        for m in self.shown.iter_mut() {
            match m.removing.as_mut() {
                Some(t) => *t += dt,
                None => { m.shown_for += dt; m.remaining -= dt; if m.remaining <= 0. { m.removing = Some(-m.remaining); } }
            }
        }
        self.shown.retain(|m| m.removing.map_or(true, |t| t < REMOVE_DURATION));
        self.fill();
    }
    pub fn height(&self) -> f32 { self.shown.iter().map(|m| m.height() + m.pad_bottom()).sum() }
}

struct Composed { ui: Ui, root: usize, size_box: Option<usize>, border: Option<usize>, name: Option<usize>, image: Option<usize>, counter: Option<usize>, counter_box: Option<usize>, counter_border: Option<usize>, counter_text: Option<usize>, brush_s: Option<Brush>, margin_s: serde_json::Value }

impl Composed {
    fn load(store: &Rc<Store>) -> Result<Composed, String> {
        let mut ui = Ui::new(store.clone());
        let root = ui.add_class_instance(MESSAGE_CLASS, None, "", json!({}), None).ok_or("cannot compose PickupMessageItem_C")?;
        let v = |ui: &Ui, owner: usize, n: &str| ui.var(owner, n).or_else(|| ui.find_in(owner, n));
        let counter = v(&ui, root, "[Stack Counter] Count");
        let cv = |ui: &Ui, n: &str| counter.and_then(|c| v(ui, c, n));
        let (counter_box, counter_border, counter_text) = (cv(&ui, "[SizeBox]"), cv(&ui, "BorderBase"), cv(&ui, "TextCounter"));
        let base = counter.and_then(|c| ui.w[c].props.get("MultiSizeBase").cloned());
        let brush_s = base.as_ref().and_then(|b| b.get("Brush_S")).and_then(Brush::from_json);
        let margin_s = counter.and_then(|c| ui.w[c].props.get("MultiSizeMargin").and_then(|m| m.get("Margin_S")).cloned()).unwrap_or(json!({}));
        let c = Composed { size_box: v(&ui, root, "[Size Box] Message Size"), border: v(&ui, root, "BorderBackground"), name: v(&ui, root, "TextItemName"), image: v(&ui, root, "Image_0"),
            counter, counter_box, counter_border, counter_text, brush_s, margin_s, ui, root };
        Ok(c)
    }

    fn apply(&mut self, m: &Message) {
        let ui = &mut self.ui;
        if let Some(i) = self.name { ui.w[i].text = Some(m.name.clone()); }
        if let Some(i) = self.image { ui.w[i].brush = Brush::from_json(&json!({ "ResourceObject": m.icon, "ImageSize": [32.0, 32.0] })).filter(|_| !m.icon.is_empty()); }
        if let Some(i) = self.counter { ui.w[i].vis = if m.stack > 1 { Vis::Visible } else { Vis::Collapsed }; }
        if let Some(i) = self.counter_text { ui.w[i].text = Some(group(m.stack)); ui.w[i].slot["Padding"] = self.margin_s.clone(); }
        if let (Some(i), Some(b)) = (self.counter_box, self.brush_s.as_ref()) {
            ui.w[i].props["MinDesiredWidth"] = json!(b.image_size[0]); ui.w[i].props["bOverride_MinDesiredWidth"] = json!(true);
            ui.w[i].props["HeightOverride"] = json!(b.image_size[1]); ui.w[i].props["bOverride_HeightOverride"] = json!(true);
            ui.w[i].props["bOverride_WidthOverride"] = json!(false);
        }
        if let (Some(i), Some(b)) = (self.counter_border, self.brush_s.clone()) { ui.w[i].brush = Some(b); ui.w[i].props["Background"] = json!(null); }
        if let Some(i) = self.size_box {
            ui.w[i].props["HeightOverride"] = json!(m.height()); ui.w[i].props["bOverride_HeightOverride"] = json!(true);
            ui.w[i].slot["Padding"] = json!({ "Left": 0.0, "Top": 0.0, "Right": 0.0, "Bottom": m.pad_bottom() });
        }
        if let Some(i) = self.border { ui.w[i].props["ContentColorAndOpacity"] = json!([1.0, 1.0, 1.0, m.content_alpha()]); }
    }
}

fn group(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() { if i > 0 && (s.len() - i) % 3 == 0 { out.push(','); } out.push(c); }
    out
}

#[derive(Default)]
pub struct PickupStream {
    pub state: StreamState,
    install: Option<PathBuf>,
    store: Option<Option<Rc<Store>>>,
    composed: Vec<Composed>,
    mats: HashMap<String, Option<Rc<SlateMat>>>,
    sizes: Vec<[f32; 2]>,
    pub missing: Vec<String>,
}
impl Clone for PickupStream { fn clone(&self) -> Self { PickupStream { state: self.state.clone(), install: self.install.clone(), ..Default::default() } } }

impl PickupStream {
    pub fn set_install(&mut self, install: PathBuf) { if self.install.as_ref() != Some(&install) { self.install = Some(install); self.store = None; self.composed.clear(); } }
    pub fn push(&mut self, p: &PickedUp) { self.state.add(p); }
    pub fn tick(&mut self, dt: f32) { self.state.tick(dt); }

    fn ensure(&mut self, uic: &mut UiContext, n: usize) -> bool {
        if self.store.is_none() {
            let s = match self.install.as_deref() { Some(i) => Store::open(i).map(Rc::new), None => Err("no install set".into()) };
            #[cfg(feature = "pak")]
            if s.is_ok() { if let Some(i) = self.install.as_deref() { if let Err(e) = super::inventory_cooked::CookedInventory::attach_paks(uic, i) { eprintln!("[pickup-stream] pak textures unavailable: {e}"); } } }
            self.store = Some(match s { Ok(s) => Some(s), Err(e) => { eprintln!("[pickup-stream] PickupMessageItem_C not composed: {e}"); None } });
        }
        let _ = &uic;
        let Some(Some(store)) = self.store.clone() else { return false };
        while self.composed.len() < n {
            match Composed::load(&store) { Ok(c) => self.composed.push(c), Err(e) => { eprintln!("[pickup-stream] {e}"); self.store = Some(None); return false; } }
        }
        true
    }

    pub fn prepare(&mut self, uic: &mut UiContext, w: f32, h: f32) -> f32 {
        let n = self.state.shown.len();
        self.sizes.clear();
        if n == 0 || !self.ensure(uic, n) { return 0.; }
        let s = dpi_scale(w, h);
        let shown = self.state.shown.clone();
        for (k, m) in shown.iter().enumerate() {
            let c = &mut self.composed[k];
            c.apply(m);
            let lay = ul::new_layout(c.ui.w.len(), s);
            let mut cx = Ctx { ui: &mut c.ui, uic: &mut *uic, lay };
            let d = cx.desired(c.root);
            self.sizes.push(d);
        }
        self.sizes.iter().map(|d| d[1]).sum()
    }

    pub fn width(&self) -> f32 { self.sizes.iter().map(|d| d[0]).fold(0., f32::max) }

    pub fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, left_px: f32, top_px: f32, out: &mut RenderList) {
        let s = dpi_scale(w, h);
        let mut y = top_px / s;
        for k in 0..self.sizes.len().min(self.composed.len()) {
            let d = self.sizes[k];
            let c = &mut self.composed[k];
            let n = c.ui.w.len();
            let mut lay = ul::new_layout(n, s);
            for _ in 0..2 {
                let wrap = lay.wrap_w.clone();
                lay = ul::new_layout(n, s);
                lay.wrap_w = wrap;
                let mut cx = Ctx { ui: &mut c.ui, uic: &mut *uic, lay };
                cx.desired(c.root);
                cx.arrange(c.root, [left_px / s, y, d[0], d[1]]);
                lay = cx.lay;
            }
            let mut p = Painter { c: Ctx { ui: &mut c.ui, uic: &mut *uic, lay }, out: RenderList::new(), mats: &mut self.mats, blur: vec![], missing: vec![], scale: s, disabled: false };
            p.paint(c.root, Tf::ID, [1.; 4]);
            let missing = std::mem::take(&mut p.missing);
            out.extend(std::mem::take(&mut p.out));
            for m in missing { if !self.missing.contains(&m) { eprintln!("[pickup-stream] not drawn: {m}"); self.missing.push(m); } }
            y += d[1];
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    fn pu(n: &str, c: u32) -> PickedUp { PickedUp { name: n.into(), icon: String::new(), count: c } }
    #[test] fn the_message_item_composes_from_the_cooked_tree() {
        let p = crate::pak_assets::default_install();
        if !p.exists() { return; }
        let mut uic = UiContext::from_install(&p, None).expect("ui");
        let mut st = PickupStream::default();
        st.set_install(p.clone());
        st.push(&PickedUp { name: "Wood".into(), icon: "/Game/UI/Foundation/Textures/Icons/Items/T-Icon-Logs.T-Icon-Logs".into(), count: 30 });
        st.tick(1.0);
        let h = st.prepare(&mut uic, 1920., 1080.);
        let c = &st.composed[0];
        assert!(c.size_box.is_some() && c.name.is_some() && c.image.is_some() && c.counter.is_some() && c.counter_text.is_some() && c.counter_border.is_some(), "widgets found");
        assert_eq!(c.ui.w[c.name.unwrap()].text.as_deref(), Some("Wood"));
        assert_eq!(c.ui.w[c.counter_text.unwrap()].text.as_deref(), Some("30"));
        assert!((h - (MESSAGE_HEIGHT + MESSAGE_PAD_BOTTOM)).abs() < 0.5, "height {h}");
        assert!(st.width() > 32. && st.width() <= 600., "width {}", st.width());
        let mut out = RenderList::new();
        st.draw(&mut uic, 1920., 1080., 1500., 700., &mut out);
        assert!(!out.is_empty());
        assert!(st.missing.is_empty(), "{:?}", st.missing);
    }
    #[test] fn a_merge_restarts_the_display_timer() {
        let mut s = StreamState::default();
        s.add(&pu("Wood", 10));
        for _ in 0..200 { s.tick(0.01); }
        s.add(&pu("Wood", 10));
        for _ in 0..200 { s.tick(0.01); }
        assert_eq!((s.shown.len(), s.shown[0].stack), (1, 20));
        assert!(s.shown[0].removing.is_none());
        for _ in 0..80 { s.tick(0.01); }
        assert!(s.shown.is_empty());
    }
    #[test] fn messages_merge_queue_and_expire_like_the_message_manager() {
        let mut s = StreamState::default();
        s.add(&pu("Wood", 30)); s.add(&pu("Ammo", 12)); s.add(&pu("Wood", 5));
        assert_eq!(s.shown.len(), 2); assert_eq!(s.shown[0].stack, 35, "same name stacks into the displayed message");
        s.add(&pu("Stone", 1)); s.add(&pu("Metal", 2));
        assert_eq!((s.shown.len(), s.queued.len()), (3, 1), "at most 3 displayed, the rest queued");
        s.tick(APPEAR_LEN);
        assert!((s.shown[0].height() - MESSAGE_HEIGHT).abs() < 1e-4);
        for _ in 0..((DISPLAY_DURATION - APPEAR_LEN) * 100.) as usize + 1 { s.tick(0.01); }
        assert!(s.shown[0].removing.is_some(), "Disappear starts after DisplayDuration");
        for _ in 0..25 { s.tick(0.01); }
        assert_eq!(s.shown.len(), 1, "removed after RemoveDuration; the queued message joins: {:?}", s.shown.iter().map(|m| &m.name).collect::<Vec<_>>());
        assert_eq!(s.shown[0].name, "Metal");
    }
    #[test] fn appear_and_disappear_follow_the_cooked_tracks() {
        let mut m = Message { name: "x".into(), icon: String::new(), stack: 1, shown_for: 0.15, remaining: DISPLAY_DURATION, removing: None };
        assert!((m.height() - 28.).abs() < 1e-4);
        m.removing = Some(0.1);
        assert!((m.height() - 28.05).abs() < 1e-3 && (m.pad_bottom() - 4.).abs() < 1e-4 && (m.content_alpha() - 0.5).abs() < 1e-4);
        assert_eq!(group(1234567), "1,234,567");
    }
}
