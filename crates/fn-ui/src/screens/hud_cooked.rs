use super::athena_lobby::layout::{self as ul, Ctx};
use super::athena_lobby::paint::{Painter, Tf};
use super::athena_lobby::slate_mat::SlateMat;
use super::athena_lobby::store::Store;
use super::athena_lobby::umg::{Ui, Vis};
use crate::draw::RenderList;
use crate::layout::dpi_scale;
use crate::scene::UiContext;
use serde_json::json;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align { Fill, Start, Center, End }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot { pub h: Align, pub v: Align, pub pad: [f32; 4] }
impl Slot {
    pub const FILL: Slot = Slot { h: Align::Fill, v: Align::Fill, pad: [0.; 4] };
    pub fn place(&self, avail: [f32; 2], desired: [f32; 2]) -> [f32; 4] {
        let axis = |a: Align, lo: f32, hi: f32, size: f32, d: f32| -> (f32, f32) {
            let room = (size - lo - hi).max(0.);
            match a { Align::Fill => (lo, room), Align::Start => (lo, d.min(room)), Align::Center => (lo + (room - d.min(room)) * 0.5, d.min(room)), Align::End => (lo + room - d.min(room), d.min(room)) }
        };
        let (x, w) = axis(self.h, self.pad[0], self.pad[2], avail[0], desired[0]);
        let (y, h) = axis(self.v, self.pad[1], self.pad[3], avail[1], desired[1]);
        [x, y, w, h]
    }
}

pub struct CookedWidget { pub ui: Ui, pub root: usize, pub slot: Slot, mats: HashMap<String, Option<Rc<SlateMat>>>, pub missing: Vec<String>, pub rects: Vec<[f32; 4]>, pub scale: f32 }

impl CookedWidget {
    pub fn load(store: Rc<Store>, class: &str, slot: Slot) -> Result<CookedWidget, String> {
        let mut ui = Ui::new(store);
        let root = ui.add_class_instance(class, None, "", json!({}), None).ok_or_else(|| format!("cannot compose {class}"))?;
        Ok(CookedWidget { ui, root, slot, mats: HashMap::new(), missing: vec![], rects: vec![], scale: 1. })
    }
    pub fn id(&self, name: &str) -> Option<usize> { self.ui.var(self.root, name).or_else(|| self.ui.find_in(self.root, name)) }
    pub fn all(&self, name: &str) -> Vec<usize> { self.ui.w.iter().filter(|w| w.name == name).map(|w| w.id).collect() }
    pub fn set_text(&mut self, name: &str, t: &str) { if let Some(i) = self.id(name) { self.ui.w[i].text = Some(t.to_owned()); } }
    pub fn text(&self, name: &str) -> Option<String> { self.id(name).and_then(|i| self.ui.w[i].text.clone()) }
    pub fn set_vis(&mut self, name: &str, v: Vis) { if let Some(i) = self.id(name) { self.ui.w[i].vis = v; } }
    pub fn set_vis_id(&mut self, id: usize, v: Vis) { self.ui.w[id].vis = v; }
    pub fn vis(&self, name: &str) -> Option<Vis> { self.id(name).map(|i| self.ui.w[i].vis) }
    pub fn set_switch(&mut self, name: &str, index: i64) { if let Some(i) = self.id(name) { self.ui.w[i].props["ActiveWidgetIndex"] = json!(index); } }
    pub fn switch_index(&self, name: &str) -> Option<i64> { self.id(name).and_then(|i| self.ui.w[i].props.get("ActiveWidgetIndex").and_then(|v| v.as_i64())) }
    pub fn rect(&self, name: &str) -> Option<[f32; 4]> { let i = self.id(name)?; let r = *self.rects.get(i)?; Some([r[0] * self.scale, r[1] * self.scale, r[2] * self.scale, r[3] * self.scale]) }
    pub fn shown(&self, id: usize) -> bool {
        let mut cur = Some(id);
        while let Some(i) = cur { if !self.ui.w[i].vis.draws() { return false; } cur = self.ui.w[i].parent; }
        true
    }

    pub fn layout(&mut self, uic: &mut UiContext, w: f32, h: f32) {
        let s = dpi_scale(w, h);
        let (sw, sh) = (w / s, h / s);
        let n = self.ui.w.len();
        let mut lay = ul::new_layout(n, s);
        for _ in 0..2 {
            let wrap = lay.wrap_w.clone();
            lay = ul::new_layout(n, s);
            lay.wrap_w = wrap;
            let mut c = Ctx { ui: &mut self.ui, uic: &mut *uic, lay };
            let d = c.desired(self.root);
            let r = self.slot.place([sw, sh], d);
            c.arrange(self.root, [(r[0] * s).round() / s, (r[1] * s).round() / s, r[2], r[3]]);
            lay = c.lay;
        }
        self.rects = lay.rect.clone();
        self.scale = s;
    }

    pub fn paint(&mut self, uic: &mut UiContext, w: f32, h: f32, tag: &str) -> RenderList {
        self.layout(uic, w, h);
        let s = self.scale;
        let n = self.ui.w.len();
        let mut lay = ul::new_layout(n, s);
        lay.rect = self.rects.clone();
        let mut pt = Painter { c: Ctx { ui: &mut self.ui, uic, lay }, out: RenderList::new(), mats: &mut self.mats, blur: vec![], missing: vec![], scale: s, disabled: false };
        pt.paint(self.root, Tf::ID, [1.; 4]);
        let missing = std::mem::take(&mut pt.missing);
        let out = std::mem::take(&mut pt.out);
        for m in missing { if !self.missing.contains(&m) { eprintln!("[{tag}] not drawn: {m}"); self.missing.push(m); } }
        out
    }
}

use super::athena_locker::anim::Curve;
use super::athena_lobby::store::{jf, split_object_path};
use super::athena_lobby::umg::Transform;
use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct AnimTrack { pub widget: usize, pub xf: Option<[Option<Curve>; 5]>, pub color: Option<[Curve; 4]> }
#[derive(Clone, Debug)]
pub struct WidgetAnim { pub name: String, pub len: f32, pub tracks: Vec<AnimTrack> }

impl CookedWidget {
    pub fn id_in(&self, owner: &str, name: &str) -> Option<usize> { let o = self.id(owner)?; self.ui.find_in(o, name) }
    pub fn class_path(&self) -> Option<String> { self.ui.w[self.root].user.as_ref()?.class_path.clone() }
    pub fn loc_prop(&self, key: &str) -> Option<String> { let v = self.ui.w[self.root].props.get(key)?.clone(); self.ui.text_of(&v) }
    pub fn loc(&self, ns: &str, key: &str) -> Option<String> { self.ui.store.locres().and_then(|l| l.get(ns, key).map(str::to_owned)) }
    pub fn set_brush_resource(&mut self, id: usize, res: Option<&str>) {
        match res {
            Some(r) => { let b = &mut self.ui.w[id].props["Brush"]; if !b.is_object() { *b = json!({}); } b["ResourceObject"] = json!(r); if let Some(o) = b.as_object_mut() { o.remove("DrawAs"); } self.ui.w[id].brush = None; }
            None => { self.ui.w[id].props["Brush"] = json!({ "DrawAs": "ESlateBrushDrawType::NoDrawType" }); self.ui.w[id].brush = None; }
        }
    }
    pub fn anim(&self, name: &str) -> Option<WidgetAnim> {
        let cp = self.class_path()?;
        let (long, cname) = split_object_path(&cp);
        let pkg = self.ui.store.pkg(long)?;
        let ms = pkg.get(&format!("{long}.{cname}:{name}_INST:{name}"))?;
        let len = ms.props.get("PlaybackRange").and_then(|r| r.get("UpperBound")).and_then(|b| b.get("Value")).and_then(jf).unwrap_or(0.);
        let mut tracks = vec![];
        for b in ms.props.get("ObjectBindings").and_then(|v| v.as_array()).into_iter().flatten() {
            let bname = b.get("BindingName").and_then(|v| v.as_str()).unwrap_or("");
            let widget = self.id(bname).unwrap_or(self.root);
            for tp in b.get("Tracks").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|t| t.as_str()) {
                let Some(track) = pkg.get(tp) else { continue };
                for sp in track.props.get("Sections").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|s| s.as_str()) {
                    let Some(sec) = pkg.get(sp) else { continue };
                    let c = |k: &str| sec.props.get(k).filter(|v| v.get("Keys").and_then(|k| k.as_array()).map_or(false, |a| !a.is_empty())).map(|v| Curve::from_json(Some(v)));
                    let e = || Curve { keys: vec![] };
                    match track.class.as_str() {
                        "MovieScene2DTransformTrack" => tracks.push(AnimTrack { widget, xf: Some([c("Translation"), c("Translation[1]"), c("Scale"), c("Scale[1]"), c("Rotation")]), color: None }),
                        "MovieSceneColorTrack" => tracks.push(AnimTrack { widget, xf: None, color: Some([c("RedCurve").unwrap_or_else(e), c("GreenCurve").unwrap_or_else(e), c("BlueCurve").unwrap_or_else(e), c("AlphaCurve").unwrap_or_else(e)]) }),
                        _ => {}
                    }
                }
            }
        }
        Some(WidgetAnim { name: name.to_owned(), len, tracks })
    }
    pub fn apply_anim(&mut self, a: &WidgetAnim, t: f32, base: &HashMap<usize, (Transform, serde_json::Value)>) {
        for tr in &a.tracks {
            let (bt, bc) = base.get(&tr.widget).cloned().unwrap_or((Transform::identity(), serde_json::Value::Null));
            if let Some(xf) = &tr.xf {
                let mut x = bt.clone();
                let ev = |c: &Option<Curve>, d: f32| c.as_ref().filter(|c| !c.keys.is_empty()).map_or(d, |c| c.eval(t));
                x.translation = [ev(&xf[0], bt.translation[0]), ev(&xf[1], bt.translation[1])];
                x.scale = [ev(&xf[2], bt.scale[0]), ev(&xf[3], bt.scale[1])];
                x.angle = ev(&xf[4], bt.angle);
                self.ui.w[tr.widget].transform = x;
            }
            if let Some(c) = &tr.color {
                let d = super::athena_lobby::store::jvec::<4>(Some(&bc)).unwrap_or([1.; 4]);
                let ev = |c: &Curve, d: f32| if c.keys.is_empty() { d } else { c.eval(t) };
                self.ui.w[tr.widget].props["ColorAndOpacity"] = json!([ev(&c[0], d[0]), ev(&c[1], d[1]), ev(&c[2], d[2]), ev(&c[3], d[3])]);
            }
        }
    }
    pub fn base_of(&self, a: &WidgetAnim) -> HashMap<usize, (Transform, serde_json::Value)> {
        a.tracks.iter().map(|t| (t.widget, (self.ui.w[t.widget].transform.clone(), self.ui.w[t.widget].props.get("ColorAndOpacity").cloned().unwrap_or(serde_json::Value::Null)))).collect()
    }
}

pub struct Playing { pub anim: WidgetAnim, pub t: f32, base: HashMap<usize, (Transform, serde_json::Value)> }
impl Playing {
    pub fn start(w: &CookedWidget, name: &str) -> Option<Playing> { let anim = w.anim(name)?; let base = w.base_of(&anim); Some(Playing { anim, t: 0., base }) }
    pub fn advance(&mut self, w: &mut CookedWidget, dt: f32) -> bool {
        self.t = (self.t + dt.max(0.)).min(self.anim.len);
        w.apply_anim(&self.anim, self.t, &self.base);
        self.t >= self.anim.len
    }
}

pub const DEATH_WIDGET: &str = "/Game/Athena/HUD/AthenaDeathWidget.AthenaDeathWidget_C";
pub const WIN_WIDGET: &str = "/Game/Athena/HUD/AthenaWinWidget.AthenaWinWidget_C";
pub const ACTION_ALERT_WIDGET: &str = "/Game/Athena/HUD/AthenaPlayerActionAlert.AthenaPlayerActionAlert_C";
pub const RETICLE_STATUS_WIDGET: &str = "/Game/UI/InGame/HUD/ReticleStatusWidget.ReticleStatusWidget_C";
pub const ACTION_ALERT_SLOT: Slot = Slot { h: Align::Center, v: Align::End, pad: [0., 0., 0., 300.] };
pub const RETICLE_STATUS_SLOT: Slot = Slot { h: Align::Center, v: Align::Center, pad: [287., 0., 0., -20.] };
pub const DEATH_TEXT_NS: &str = "[93D806474F4F51D5E48311955BF83597]";
pub const PLACE_FORMAT: (&str, &str) = ("0A510F3C41FF3B33B10EE7890032B707", "#{Place}");
pub const YOU_PLACED: (&str, &str) = ("C5979B664E69014E43FAACA00E24A7DA", "YOU PLACED");
pub const YOUR_TEAM_PLACED: (&str, &str) = ("E0D0F13D451688A244AB37BC364D6C31", "YOUR TEAM PLACED");
pub const KILLER_SWITCH_KILLER: i64 = 0;
pub const KILLER_SWITCH_SELF: i64 = 1;
pub const KILLER_SWITCH_STORM: i64 = 2;
pub const STATUS_NS: &str = "Fort.Player.Defaults";
pub const UNABLE_TO_AFFORD: (&str, &str) = ("UnableToAfford", "Not enough {0} to build! Change building material or gather more!");
pub const BUILDING_EDIT_FAILED: (&str, &str) = ("BuildingEditFailed", "Unable to Edit Building");

#[derive(Clone, Debug, PartialEq)]
pub enum DeathKind { Storm, SelfInflicted, Killer(String) }

#[derive(Clone, Debug, PartialEq)]
pub enum GameOver { Died { player: String, kind: DeathKind, place: u32, team_size: u32 }, Won }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionAlert { Down = 0, Eliminated = 1, Storm = 2 }

pub fn phase_material(step: u8) -> Option<&'static str> {
    match step {
        1 => Some("/Game/Athena/HUD/Phase/M_GamePhase_Setup.M_GamePhase_Setup"),
        2 => Some("/Game/Athena/HUD/Phase/M_GamePhase_Warmup.M_GamePhase_Warmup"),
        3 | 4 => Some("/Game/Athena/HUD/Phase/M_GamePhase_Bus.M_GamePhase_Bus"),
        5 => Some("/Game/Athena/HUD/Phase/M_GamePhase_Jump.M_GamePhase_Jump"),
        6 | 7 => Some("/Game/Athena/HUD/Phase/M_GamePhase_StormHolding.M_GamePhase_StormHolding"),
        8 => Some("/Game/Athena/HUD/Phase/M_GamePhase_StormMoving.M_GamePhase_StormMoving"),
        9 => Some("/Game/Athena/HUD/Phase/M_GamePhase_End.M_GamePhase_End"),
        _ => None,
    }
}

pub struct HudFacts { pub players_left: Option<u32>, pub kills: u32, pub phase: Option<(u8, f32)> }

impl Clone for HudCooked { fn clone(&self) -> Self { HudCooked { install: self.install.clone(), ..Default::default() } } }

#[derive(Default)]
pub struct HudCooked {
    install: Option<PathBuf>,
    store: Option<Option<Rc<Store>>>,
    shown: Option<GameOver>,
    screen: Option<CookedWidget>,
    alert: Option<CookedWidget>,
    alert_queue: VecDeque<(ActionAlert, String)>,
    alert_anims: Vec<Playing>,
    status: Option<CookedWidget>,
    status_t: f32,
    spectating: Option<String>,
    extra_hidden: bool,
    preloaded: Vec<(&'static str, CookedWidget)>,
    unpainted: Vec<(&'static str, CookedWidget)>,
    warm_failed: Vec<&'static str>,
}

impl HudCooked {
    pub fn set_install(&mut self, install: PathBuf) { if self.install.as_ref() != Some(&install) { self.install = Some(install); self.store = None; } }
    pub fn set_store(&mut self, store: Rc<Store>) { self.store = Some(Some(store)); }
    pub fn share_store(&mut self, store: Option<Rc<Store>>) { if !matches!(self.store, Some(Some(_))) { if let Some(s) = store { self.store = Some(Some(s)); } } }
    fn store(&mut self) -> Option<Rc<Store>> {
        if self.store.is_none() {
            let s = match self.install.as_ref() { Some(i) => Store::open(i).map_err(|e| eprintln!("[hud] cooked HUD widgets unavailable: {e}")).ok(), None => None };
            self.store = Some(s.map(Rc::new));
        }
        self.store.clone().flatten()
    }
    pub fn game_over(&self) -> Option<&GameOver> { self.shown.as_ref() }
    pub fn screen(&self) -> Option<&CookedWidget> { self.screen.as_ref() }

    pub fn set_game_over(&mut self, go: Option<&GameOver>) {
        match (go, self.shown.as_ref()) {
            (None, Some(_)) => { self.shown = None; self.screen = None; }
            (Some(GameOver::Won), Some(GameOver::Won)) => {}
            (Some(GameOver::Died { .. }), Some(_)) => {}
            (Some(g), _) => {
                let class = if *g == GameOver::Won { WIN_WIDGET } else { DEATH_WIDGET };
                let pre = self.preloaded.iter().position(|(c, _)| *c == class).map(|i| self.preloaded.remove(i).1);
                let loaded = match pre { Some(w) => Ok(w), None => { let Some(st) = self.store() else { return }; CookedWidget::load(st, class, Slot::FILL) } };
                match loaded {
                    Ok(mut w) => {
                        if let GameOver::Died { player, kind, place, team_size } = g { on_pawn_died(&mut w, player, kind, *team_size); on_place_changed(&mut w, *place, *team_size); }
                        self.screen = Some(w);
                        self.shown = Some(g.clone());
                    }
                    Err(e) => eprintln!("[hud] {class}: {e}"),
                }
            }
            (None, None) => {}
        }
    }

    pub fn prewarm_game_over(&mut self, uic: &mut UiContext, w: f32, h: f32) -> bool {
        if let Some((class, mut wd)) = self.unpainted.pop() {
            let _ = crate::screens::athena_lobby::paint::with_raster_budget(12, || wd.paint(uic, w, h, "game over prewarm"));
            if crate::screens::athena_lobby::paint::raster_pending() { self.unpainted.push((class, wd)); } else { self.preloaded.push((class, wd)); }
            return false;
        }
        for class in [DEATH_WIDGET, WIN_WIDGET] {
            if self.preloaded.iter().any(|(c, _)| *c == class) || self.warm_failed.contains(&class) { continue; }
            let Some(st) = self.store() else { return true };
            match CookedWidget::load(st, class, Slot::FILL) {
                Ok(wd) => { self.unpainted.push((class, wd)); return false; }
                Err(e) => { eprintln!("[hud] {class}: {e}"); self.warm_failed.push(class); }
            }
        }
        true
    }

    pub fn set_view_target_vitals(&mut self, vitals: Option<(f32, f32)>) {
        if self.spectating.is_none() { return; }
        let (Some(w), Some((h, s))) = (self.screen.as_mut(), vitals) else { return };
        view_target_hit_points(w, h, s);
    }
    pub fn set_spectating(&mut self, view: Option<(&str, f32)>) {
        if !matches!(self.shown, Some(GameOver::Died { .. })) { self.spectating = None; self.extra_hidden = false; return; }
        let Some(w) = self.screen.as_mut() else { return };
        let Some((name, t)) = view.filter(|v| !v.0.is_empty()) else { return };
        if self.spectating.as_deref() != Some(name) { on_view_target_changed(w, name); self.spectating = Some(name.to_owned()); self.extra_hidden = false; }
        if !self.extra_hidden && t >= SPECTATE_HIDE_EXTRA_S { hide_extra_stuff_for_spectating(w); self.extra_hidden = true; }
    }
    pub fn spectating(&self) -> Option<&str> { self.spectating.as_deref() }

    pub fn alert_player(&mut self, alert: ActionAlert, detail: &str) { self.alert_queue.push_back((alert, detail.to_owned())); }

    pub fn reticle_status(&mut self, text: &str) {
        if self.status.is_none() { if let Some(st) = self.store() { self.status = CookedWidget::load(st, RETICLE_STATUS_WIDGET, RETICLE_STATUS_SLOT).map_err(|e| eprintln!("[hud] {e}")).ok(); } }
        let Some(w) = self.status.as_mut() else { return };
        w.set_text("ReticleStatusText", text);
        w.set_vis("ReticleStatusText", Vis::SelfHitTestInvisible);
        self.status_t = w.ui.w[w.root].props.get("StatusDisplayDuration").and_then(jf).unwrap_or(1.2);
    }
    pub fn status_text(&mut self, key: (&str, &str)) -> String { self.store().and_then(|s| s.locres()).and_then(|l| l.get(STATUS_NS, key.0).map(str::to_owned)).unwrap_or_else(|| key.1.to_owned()) }

    pub fn tick(&mut self, dt: f32) {
        if self.status_t > 0. { self.status_t -= dt; if self.status_t <= 0. { if let Some(w) = self.status.as_mut() { w.set_vis("ReticleStatusText", Vis::Hidden); } } }
        if self.alert_anims.is_empty() {
            if let Some((a, detail)) = self.alert_queue.pop_front() {
                if self.alert.is_none() { if let Some(st) = self.store() { self.alert = CookedWidget::load(st, ACTION_ALERT_WIDGET, ACTION_ALERT_SLOT).map_err(|e| eprintln!("[hud] {e}")).ok(); } }
                if let Some(w) = self.alert.as_mut() { self.alert_anims = alert_player(w, a, &detail); }
            }
        }
        if let Some(w) = self.alert.as_mut() {
            let mut finished_show = false;
            for p in self.alert_anims.iter_mut() { if p.advance(w, dt) && p.anim.name == "ShowAlert" { finished_show = true; } }
            if finished_show { w.set_vis("MessageSwitcher", Vis::Collapsed); self.alert_anims.clear(); }
        }
    }

    pub fn build_game_over(&mut self, uic: &mut UiContext, w: f32, h: f32, facts: &HudFacts, out: &mut RenderList) -> Option<GameOverRects> {
        let died = matches!(self.shown, Some(GameOver::Died { .. }));
        let screen = self.screen.as_mut()?;
        fill_counters(screen, facts);
        if died { fill_phase(screen, facts.phase); }
        for k in ["KillFeed1", "KillFeed2", "KillFeed3", "KillFeed4", "KillFeed5"] { for id in screen.all(k) { screen.set_vis_id(id, Vis::Collapsed); } }
        out.extend(screen.paint(uic, w, h, "game over"));
        let minimap = screen.id("FortSlateWidgetMinimap").filter(|&i| screen.shown(i)).and_then(|_| screen.rect("FortSlateWidgetMinimap"));
        let phase_icon = screen.id("AthenaGamePhaseWidget").and_then(|p| screen.ui.find_in(p, "Icon")).filter(|&i| screen.shown(i)).and_then(|i| screen.rects.get(i).map(|r| [r[0] * screen.scale, r[1] * screen.scale, r[2] * screen.scale, r[3] * screen.scale]));
        Some(GameOverRects { minimap, phase_icon, scale: screen.scale })
    }
    pub fn build_overlays(&mut self, uic: &mut UiContext, w: f32, h: f32, out: &mut RenderList) {
        if let Some(a) = self.alert.as_mut() { if !self.alert_anims.is_empty() && a.vis("MessageSwitcher").map_or(false, |v| v.draws()) { out.extend(a.paint(uic, w, h, "action alert")); } }
        if self.status_t > 0. { if let Some(s) = self.status.as_mut() { out.extend(s.paint(uic, w, h, "reticle status")); } }
    }
}

pub struct GameOverRects { pub minimap: Option<[f32; 4]>, pub phase_icon: Option<[f32; 4]>, pub scale: f32 }

fn fill_counters(w: &mut CookedWidget, f: &HudFacts) {
    let left = f.players_left.map(|n| n.to_string()).unwrap_or_else(|| super::hud_extra::PLAYERS_LEFT_DEFAULT.to_owned());
    for id in w.all("PlayersLeftText") { w.ui.w[id].text = Some(left.clone()); }
    for id in w.all("KillsText") { w.ui.w[id].text = Some(f.kills.to_string()); }
    for id in w.all("AthenaSessionId") { if let Some(t) = w.ui.find_in(id, "Text") { w.ui.w[t].text = Some(String::new()); } }
}

fn fill_phase(w: &mut CookedWidget, phase: Option<(u8, f32)>) {
    let Some(p) = w.id("AthenaGamePhaseWidget") else { return };
    let time = phase.map_or("0:00".to_owned(), |(_, t)| super::game_phase::small_time(t));
    if let Some(t) = w.ui.find_in(p, "TimeText") { w.ui.w[t].text = Some(time); }
    if let Some(i) = w.ui.find_in(p, "Icon") { w.set_brush_resource(i, None); w.ui.w[i].props["Brush"]["ImageSize"] = json!([32.0, 32.0]); }
}

pub fn on_pawn_died(w: &mut CookedWidget, player: &str, kind: &DeathKind, team_size: u32) {
    let you_died = w.loc_prop("YouDied").unwrap_or_else(|| "YOU DIED".into());
    w.set_text("YouDiedOrSpectating", &you_died);
    w.set_text("CommonTextBlock_PrimaryDeathText", player);
    if let Some(kp) = w.id("AthenaKillerPortraitWidget") {
        let idx = match kind { DeathKind::Storm => KILLER_SWITCH_STORM, DeathKind::SelfInflicted => KILLER_SWITCH_SELF, DeathKind::Killer(_) => KILLER_SWITCH_KILLER };
        if let Some(s) = w.ui.find_in(kp, "WidgetSwitcher_0") { w.ui.w[s].props["ActiveWidgetIndex"] = json!(idx); }
        if let DeathKind::Killer(k) = kind { if let Some(t) = w.ui.find_in(kp, "KillerNameText") { w.ui.w[t].text = Some(k.clone()); } }
    }
    w.set_vis("YouPlacedBox", if team_size > 1 { Vis::Hidden } else { Vis::Visible });
    w.set_vis("FollowNextTeammate", Vis::Hidden);
    w.set_vis("WinnerBox", Vis::Hidden);
}

pub const SPECTATE_HIDE_EXTRA_S: f32 = 20.;

pub fn on_view_target_changed(w: &mut CookedWidget, spectated: &str) {
    let t = w.loc_prop("Spectating").unwrap_or_else(|| "SPECTATING".into());
    w.set_text("YouDiedOrSpectating", &t);
    w.set_text("CommonTextBlock_PrimaryDeathText", spectated);
    w.set_vis("AthenaViewTargetHitPointInfo", Vis::Visible);
}

pub fn view_target_hit_points(w: &mut CookedWidget, health: f32, shield: f32) {
    let Some(info) = w.id("AthenaViewTargetHitPointInfo") else { return };
    let num = |v: f32| format!("{}", v.max(0.).round() as i64);
    for (bar, cur, max) in [("HitPointBarHealth", health, Some(100.)), ("HitPointBarShield", shield, None)] {
        let Some(b) = w.ui.find_in(info, bar) else { continue };
        if let Some(t) = w.ui.find_in(b, "TextCurrentStat") { w.ui.w[t].text = Some(num(cur)); }
        if let (Some(m), Some(t)) = (max, w.ui.find_in(b, "TextMaxStat")) { w.ui.w[t].text = Some(num(m)); }
    }
}

pub fn hide_extra_stuff_for_spectating(w: &mut CookedWidget) {
    w.set_vis("AthenaKillerPortraitWidget", Vis::Collapsed);
    w.set_vis("YouPlacedBox", Vis::Collapsed);
}

pub fn on_place_changed(w: &mut CookedWidget, place: u32, team_size: u32) {
    let f = w.loc(DEATH_TEXT_NS, PLACE_FORMAT.0).unwrap_or_else(|| PLACE_FORMAT.1.into());
    w.set_text("PlaceText", &f.replace("{Place}", &place.to_string()));
    let t = if team_size > 1 { w.loc(DEATH_TEXT_NS, YOUR_TEAM_PLACED.0).unwrap_or_else(|| YOUR_TEAM_PLACED.1.into()) } else { w.loc(DEATH_TEXT_NS, YOU_PLACED.0).unwrap_or_else(|| YOU_PLACED.1.into()) };
    w.set_text("YouPlacedText", &t);
}

pub fn on_winner_announced(w: &mut CookedWidget, winner: &str) {
    w.set_vis("WinnerBox", Vis::SelfHitTestInvisible);
    w.set_text("WinnerPlayerName", winner);
}

pub fn alert_player(w: &mut CookedWidget, alert: ActionAlert, detail: &str) -> Vec<Playing> {
    w.set_vis("MessageSwitcher", Vis::HitTestInvisible);
    w.set_switch("MessageSwitcher", alert as i64);
    let mut anims: Vec<Playing> = Playing::start(w, "ShowAlert").into_iter().collect();
    match alert {
        ActionAlert::Eliminated => w.set_text("PlayerEliminatedText", detail),
        ActionAlert::Down => { w.set_text("PlayerDownText", detail); anims.extend(Playing::start(w, "CrawlDown")); }
        ActionAlert::Storm => anims.extend(Playing::start(w, "FlashRun")),
    }
    anims
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusEvent { UnableToAfford(usize), BuildingEditFailed }

pub const RESOURCE_DISPLAY_NAMES: [(&str, &str); 3] = [("182B6FC24CA7CCE45B37C48DCD9E5AF0", "Wood"), ("7194C5F1497E3BAB503ADAAB48D8CE70", "Stone"), ("7A6BCEBB4434E06B46C108810CCD2CA9", "Metal")];

impl HudCooked {
    pub fn status_event(&mut self, e: StatusEvent) {
        let text = match e {
            StatusEvent::UnableToAfford(r) => {
                let (key, src) = RESOURCE_DISPLAY_NAMES[r.min(2)];
                let name = self.store().and_then(|s| s.locres()).and_then(|l| l.get("", key).map(str::to_owned)).unwrap_or_else(|| src.to_owned());
                self.status_text(UNABLE_TO_AFFORD).replace("{0}", &name)
            }
            StatusEvent::BuildingEditFailed => self.status_text(BUILDING_EDIT_FAILED),
        };
        self.reticle_status(&text);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub fn store() -> Option<Rc<Store>> {
        let p = crate::pak_assets::default_install();
        if !p.exists() { return None; }
        Some(Rc::new(Store::open(&p).ok()?))
    }
    pub fn ctx() -> Option<UiContext> {
        let p = crate::pak_assets::default_install();
        let pak = crate::pak_assets::PakAssets::open(&p).ok()?;
        let dirs = crate::testutil::assets_root().map(crate::assets::DirAssets::new);
        let mut layers: Vec<Box<dyn crate::assets::AssetSource>> = vec![Box::new(pak)];
        if let Some(d) = dirs { layers.insert(0, Box::new(d)); }
        UiContext::new(Box::new(crate::assets::LayeredAssets::new(layers))).ok()
    }
    pub fn save(name: &str, uic: &mut UiContext, l: &RenderList, w: u32, h: u32) {
        let dir = std::env::var("FN_HUD_SHOTS").unwrap_or_default();
        if dir.is_empty() { return; }
        let px = crate::raster::render(l, &mut uic.assets, w, h, Some([0.12, 0.16, 0.12, 1.]));
        let _ = std::fs::create_dir_all(&dir);
        let _ = image::save_buffer(format!("{dir}/{name}.png"), &px, w, h, image::ExtendedColorType::Rgba8);
    }

    fn hud(st: &Rc<Store>) -> HudCooked { let mut h = HudCooked::default(); h.set_store(st.clone()); h }
    #[test] fn death_screen_follows_the_cooked_logic() {
        let (Some(st), Some(mut uic)) = (store(), ctx()) else { return };
        for (name, kind) in [("death_storm", DeathKind::Storm), ("death_self", DeathKind::SelfInflicted), ("death_killer", DeathKind::Killer("Raptor".into()))] {
            let mut h = hud(&st);
            h.set_game_over(Some(&GameOver::Died { player: "Player".into(), kind: kind.clone(), place: 37, team_size: 1 }));
            let mut out = RenderList::new();
            let r = h.build_game_over(&mut uic, 1920., 1080., &HudFacts { players_left: Some(36), kills: 2, phase: Some((7, 75.)) }, &mut out).unwrap();
            let w = h.screen().unwrap();
            assert_eq!(w.text("PlaceText").as_deref(), Some("#37"));
            assert_eq!(w.text("YouPlacedText").as_deref(), Some("YOU PLACED"));
            assert_eq!(w.text("YouDiedOrSpectating").as_deref(), Some("YOU DIED"));
            assert_eq!(w.text("CommonTextBlock_PrimaryDeathText").as_deref(), Some("Player"));
            assert!(!w.vis("FollowNextTeammate").unwrap().draws() && !w.vis("WinnerBox").unwrap().draws());
            let kp = w.id("AthenaKillerPortraitWidget").unwrap();
            let sw = w.ui.find_in(kp, "WidgetSwitcher_0").unwrap();
            assert_eq!(w.ui.w[sw].props["ActiveWidgetIndex"].as_i64(), Some(match kind { DeathKind::Storm => 2, DeathKind::SelfInflicted => 1, DeathKind::Killer(_) => 0 }));
            assert!(r.minimap.is_some());
            let pi = r.phase_icon.expect("phase icon rect");
            assert!((pi[2] - 32.).abs() < 0.5 && (pi[3] - 32.).abs() < 0.5, "{pi:?}");
            save(name, &mut uic, &out, 1920, 1080);
        }
    }
    #[test] fn victory_royale_screen() {
        let (Some(st), Some(mut uic)) = (store(), ctx()) else { return };
        let mut h = hud(&st);
        h.set_game_over(Some(&GameOver::Won));
        let mut out = RenderList::new();
        h.build_game_over(&mut uic, 1920., 1080., &HudFacts { players_left: Some(1), kills: 5, phase: None }, &mut out).unwrap();
        let w = h.screen().unwrap();
        assert!(w.all("PlayersLeftText").iter().all(|&i| w.ui.w[i].text.as_deref() == Some("1")));
        let n = w.id("TextBlock_0").unwrap();
        assert_eq!(w.ui.w[n].transform.angle, -10.);
        save("victory_royale", &mut uic, &out, 1920, 1080);
    }
    #[test] fn action_alert_plays_show_alert_and_collapses() {
        let (Some(st), Some(mut uic)) = (store(), ctx()) else { return };
        let mut h = hud(&st);
        h.alert_player(ActionAlert::Storm, "");
        h.tick(0.);
        assert_eq!(h.alert_anims.iter().map(|p| p.anim.name.as_str()).collect::<Vec<_>>(), ["ShowAlert", "FlashRun"]);
        assert!((h.alert_anims[0].anim.len - 2.75).abs() < 1e-4);
        for (t, name) in [(0.15, "alert_storm_0.15s"), (0.5, "alert_storm_0.65s"), (1.5, "alert_storm_2.15s")] {
            h.tick(t);
            let mut out = RenderList::new();
            h.build_overlays(&mut uic, 1920., 1080., &mut out);
            assert!(!out.cmds.is_empty(), "{name}");
            save(name, &mut uic, &out, 1920, 1080);
        }
        h.tick(1.0);
        let mut out = RenderList::new();
        h.build_overlays(&mut uic, 1920., 1080., &mut out);
        assert!(out.cmds.is_empty(), "collapsed after ShowAlert (2.75 s)");
    }
    #[test] fn reticle_status_shows_for_the_display_duration() {
        let (Some(st), Some(mut uic)) = (store(), ctx()) else { return };
        let mut h = hud(&st);
        let t = h.status_text(UNABLE_TO_AFFORD).replace("{0}", "Wood");
        assert_eq!(t, "Not enough Wood to build! Change building material or gather more!");
        h.reticle_status(&t);
        let mut out = RenderList::new();
        h.build_overlays(&mut uic, 1920., 1080., &mut out);
        assert!(!out.cmds.is_empty());
        save("reticle_status", &mut uic, &out, 1920, 1080);
        h.tick(1.25);
        let mut out = RenderList::new();
        h.build_overlays(&mut uic, 1920., 1080., &mut out);
        assert!(out.cmds.is_empty(), "hidden after StatusDisplayDuration 1.2 s");
    }
    #[test] fn storm_rings_and_lines_follow_the_native_minimap_paint() {
        use crate::screens::hud_extra::{HudExtra, HudExtraState, StormMap};
        let Some(mut uic) = ctx() else { return };
        let sm = StormMap { current: [0.45, 0.5, 0.2], next: [0.5, 0.55, 0.08] };
        let mut h = HudExtra::with_state(HudExtraState { storm_map: Some(sm), map_centre: (0.30, 0.30), arrow_deg: Some(30.), ..Default::default() });
        let count_white_rotated = |l: &RenderList| l.cmds.iter().filter(|c| matches!(c, crate::draw::Cmd::Quad(q) if q.tex == crate::draw::TextureId::WHITE && q.pos[0][1] != q.pos[1][1])).count();
        let mut full = RenderList::new();
        h.build_full_map(&mut uic, 1920., 1080., 0., &mut full);
        assert_eq!(count_white_rotated(&full), 2 * crate::screens::hud_extra::STORM_RING_SEGMENTS + 2, "100-segment blue + white rings and both lines (player outside both circles)");
        save("storm_rings_full_map", &mut uic, &full, 1920, 1080);
        let mut mini = RenderList::new();
        h.build(&mut uic, 1920., 1080., 0., &mut mini);
        save("storm_rings_minimap", &mut uic, &mini, 1920, 1080);
        let mut inside = HudExtra::with_state(HudExtraState { storm_map: Some(sm), map_centre: (0.5, 0.55), ..Default::default() });
        let mut l = RenderList::new();
        inside.build_full_map(&mut uic, 1920., 1080., 0., &mut l);
        assert_eq!(count_white_rotated(&l), 2 * crate::screens::hud_extra::STORM_RING_SEGMENTS, "no lines when the player is inside both circles");
    }
    #[test] fn kill_feed_styles_match_the_cooked_style_set_rows() {
        use crate::screens::kill_feed::{Style, FONT_PT};
        let Some(st) = store() else { return };
        let p = st.pkg("/Game/Athena/HUD/AthenaKillFeedStyleSet").unwrap();
        let rows = &p.named("AthenaKillFeedStyleSet").unwrap().props["Rows"];
        for s in Style::ALL {
            let ts = &rows[s.name()]["TextStyle"];
            let font = ts["Font"]["TypefaceFontName"].as_str().unwrap();
            assert_eq!(font == "NotoSans-Bold", s.bold(), "{}", s.name());
            assert_eq!(ts["Font"]["Size"].as_f64().unwrap() as f32, FONT_PT);
            let c = &ts["ColorAndOpacity"]["SpecifiedColor"];
            for (i, v) in s.linear_color().iter().enumerate() { assert!((c[i].as_f64().unwrap() as f32 - v).abs() < 1e-5, "{} channel {i}", s.name()); }
            assert_eq!(ts["ShadowOffset"], json!([0.0, 0.0]));
        }
    }
    #[test] fn probe_compose_all() {
        let (Some(st), Some(mut uic)) = (store(), ctx()) else { return };
        for c in ["/Game/Athena/HUD/AthenaDeathWidget.AthenaDeathWidget_C", "/Game/Athena/HUD/AthenaWinWidget.AthenaWinWidget_C", "/Game/Athena/HUD/AthenaKillerPortraitWidget.AthenaKillerPortraitWidget_C",
            "/Game/Athena/HUD/AthenaPlayerActionAlert.AthenaPlayerActionAlert_C", "/Game/Athena/HUD/AthenaWatchers.AthenaWatchers_C", "/Game/Athena/HUD/AthenaSessionId.AthenaSessionId_C",
            "/Game/UI/InGame/HUD/ReticleStatusWidget.ReticleStatusWidget_C", "/Game/UI/Foundation/Subtitles/Subtitles.Subtitles_C", "/Game/UI/Foundation/Watermark/BuildWatermark.BuildWatermark_C"] {
            let mut w = match CookedWidget::load(st.clone(), c, Slot::FILL) { Ok(w) => w, Err(e) => { eprintln!("{c}: {e}"); continue } };
            let l = w.paint(&mut uic, 1920., 1080., "probe");
            let name = c.rsplit('.').next().unwrap();
            eprintln!("{name}: {} widgets, {} cmds, missing {:?}, log {:?}", w.ui.w.len(), l.cmds.len(), w.missing, w.ui.log.iter().take(5).collect::<Vec<_>>());
            save(name, &mut uic, &l, 1920, 1080);
        }
    }
}
