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
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const TEAM_INDICATOR_CLASS: &str = "/Game/Athena/HUD/Team/AthenaTeamMemberIndicator.AthenaTeamMemberIndicator_C";
pub const RELATIVE_LOCATION_STANDING_CM: [f32; 3] = [0., 0., 100.];
pub const RELATIVE_LOCATION_DBNO_CM: [f32; 3] = [0., 0., 30.];
pub const SCREEN_SPACE_RELATIVE_OFFSET: (f32, f32) = (0., -30.);

#[derive(Clone, Debug, PartialEq)]
pub struct TeamIndicator { pub name: String, pub anchor: (f32, f32), pub dbno: bool, pub reviving: bool }

pub struct CookedTeamIndicator { ui: Ui, root: usize, name: Option<usize>, state: Option<usize>, dbno_img: Option<usize>, timer: Option<usize>, not_dbno: Vis, mats: HashMap<String, Option<Rc<SlateMat>>>, pub missing: Vec<String> }

impl CookedTeamIndicator {
    pub fn load(install: &Path) -> Result<CookedTeamIndicator, String> {
        let store = Rc::new(Store::open(install)?);
        let mut ui = Ui::new(store);
        let root = ui.add_class_instance(TEAM_INDICATOR_CLASS, None, "", json!({}), None).ok_or("cannot compose AthenaTeamMemberIndicator_C")?;
        let v = |ui: &Ui, owner: usize, n: &str| ui.var(owner, n).or_else(|| ui.find_in(owner, n));
        let name = v(&ui, root, "PlayerName");
        let state = v(&ui, root, "DBNOIndicator");
        let dbno_img = state.and_then(|s| ui.find_in(s, "DBNOIndicator").filter(|&i| i != s));
        let timer = state.and_then(|s| ui.find_in(s, "ImageTimer"));
        let not_dbno = state.and_then(|s| ui.w[s].props.get("Visibility When Not DBNO").and_then(|x| x.as_str()).map(Vis::parse)).unwrap_or(Vis::Collapsed);
        Ok(CookedTeamIndicator { ui, root, name, state, dbno_img, timer, not_dbno, mats: HashMap::new(), missing: vec![] })
    }

    fn apply(&mut self, t: &TeamIndicator) {
        self.ui.w[self.root].vis = Vis::HitTestInvisible;
        if let Some(n) = self.name { self.ui.w[n].text = Some(t.name.clone()); }
        if let Some(i) = self.dbno_img { self.ui.w[i].vis = if t.dbno { Vis::HitTestInvisible } else { self.not_dbno }; }
        if let Some(i) = self.timer { self.ui.w[i].vis = if t.reviving { Vis::HitTestInvisible } else { Vis::Collapsed }; }
        let _ = self.state;
    }

    pub fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, list: &[TeamIndicator]) -> RenderList {
        let s = dpi_scale(w, h);
        let mut out = RenderList::new();
        for t in list {
            self.apply(t);
            let (cx, cy) = (t.anchor.0 / s + SCREEN_SPACE_RELATIVE_OFFSET.0, t.anchor.1 / s + SCREEN_SPACE_RELATIVE_OFFSET.1);
            let n = self.ui.w.len();
            let mut lay = ul::new_layout(n, s);
            for _ in 0..2 {
                let wrap = lay.wrap_w.clone();
                lay = ul::new_layout(n, s);
                lay.wrap_w = wrap;
                let mut c = Ctx { ui: &mut self.ui, uic: &mut *uic, lay };
                let d = c.desired(self.root);
                let x = ((cx - d[0] * 0.5) * s).round() / s;
                let y = ((cy - d[1] * 0.5) * s).round() / s;
                c.arrange(self.root, [x, y, d[0], d[1]]);
                lay = c.lay;
            }
            let mut p = Painter { c: Ctx { ui: &mut self.ui, uic: &mut *uic, lay }, out: RenderList::new(), mats: &mut self.mats, blur: vec![], missing: vec![], scale: s, disabled: false };
            p.paint(self.root, Tf::ID, [1.; 4]);
            let missing = std::mem::take(&mut p.missing);
            out.extend(std::mem::take(&mut p.out));
            for m in missing { if !self.missing.contains(&m) { eprintln!("[team-indicator] not drawn: {m}"); self.missing.push(m); } }
        }
        out
    }
}

#[derive(Default)]
pub struct TeamIndicators { install: Option<PathBuf>, cooked: Option<Option<CookedTeamIndicator>> }
impl Clone for TeamIndicators { fn clone(&self) -> Self { TeamIndicators { install: self.install.clone(), cooked: None } } }
impl TeamIndicators {
    pub fn set_install(&mut self, install: PathBuf) { if self.install.as_ref() != Some(&install) { self.install = Some(install); self.cooked = None; } }
    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, list: &[TeamIndicator], out: &mut RenderList) -> bool {
        if list.is_empty() { return false; }
        if self.cooked.is_none() {
            let r = self.install.as_ref().map(|i| CookedTeamIndicator::load(i));
            self.cooked = Some(match r { Some(Ok(c)) => Some(c), Some(Err(e)) => { eprintln!("[team-indicator] {e}"); None } None => None });
        }
        let Some(Some(c)) = self.cooked.as_mut() else { return false };
        out.extend(c.draw(ctx, w, h, list));
        true
    }
}
