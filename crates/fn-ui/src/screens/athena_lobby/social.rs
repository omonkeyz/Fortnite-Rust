use super::styles::BtnState;
use super::umg::Vis;
use super::AthenaLobbyUi;
use serde_json::json;

pub const PARTY_FINDER: &str = "/Game/UI/Frontend/Parties/PartyFinder.PartyFinder_C";
pub const PARTY_FINDER_ITEM: &str = "/Game/UI/Frontend/Parties/PartyFinderListItem.PartyFinderListItem_C";
pub const ERROR_WINDOW: &str = "/Game/UI/Foundation/Modals/ErrorWindow/ErrorWindow.ErrorWindow_C";
pub const ERROR_ENTRY: &str = "/Game/UI/Foundation/Modals/ErrorWindow/ErrorEntry.ErrorEntry_C";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InviteRow { pub name: String, pub joinable: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinderButton { Join, Ignore, Close }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartyFinderView { pub invites: Vec<InviteRow>, pub friends: usize, pub selected: usize, pub hovered: Option<usize>, pub hovered_button: Option<FinderButton>, pub pressed_button: Option<FinderButton> }

impl PartyFinderView {
    pub fn rows(&self) -> usize { self.invites.len() + 2 }
    pub fn invite_at(&self, row: usize) -> Option<&InviteRow> { if row >= 1 && row <= self.invites.len() { self.invites.get(row - 1) } else { None } }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ErrorDialogView { pub message: String, pub close_hovered: bool }

pub(crate) struct Finder { pub root: usize, pub tree: usize, pub rows: Vec<usize> }
pub(crate) struct ErrWin { pub root: usize, pub entry: usize }

#[derive(Default)]
pub(crate) struct Social { pub finder: Option<Finder>, pub error: Option<ErrWin> }

impl AthenaLobbyUi {
    pub(crate) fn apply_social(&mut self, finder: Option<&PartyFinderView>, error: Option<&ErrorDialogView>) -> Vec<usize> {
        let mut out = vec![];
        if let Some(v) = finder { if let Some(root) = self.apply_finder(v) { out.push(root); } }
        if let Some(e) = error { if let Some(root) = self.apply_error(e) { out.push(root); } }
        out
    }

    fn header_text(&self, key: &str, source: &str, n: usize) -> String {
        self.ui.text_of(&json!({ "key": key, "namespace": "PartyContext", "text": source })).unwrap_or_default().replace("{0}", &n.to_string())
    }

    fn apply_finder(&mut self, v: &PartyFinderView) -> Option<usize> {
        if self.social.finder.is_none() {
            let root = self.ui.add_class_instance(PARTY_FINDER, None, "", json!({}), None)?;
            let tree = self.ui.var(root, "SocialTreeView").or_else(|| self.ui.find_in(root, "SocialTreeView"))?;
            for (n, vis) in [("PartyInfo", Vis::Hidden), ("InviteIgnoreSwitcher", Vis::Collapsed), ("JoinButton", Vis::Collapsed)] {
                let w = self.ui.var(root, n).or_else(|| self.ui.find_in(root, n));
                self.ui.set_visibility(w, vis);
            }
            self.social.finder = Some(Finder { root, tree, rows: vec![] });
        }
        let (root, tree) = { let f = self.social.finder.as_ref()?; (f.root, f.tree) };
        let need = v.rows();
        while self.social.finder.as_ref().map_or(0, |f| f.rows.len()) < need {
            let pad = self.ui.w[tree].props.get("DesiredItemPadding").cloned().unwrap_or(json!({}));
            let Some(row) = self.ui.add_class_instance(PARTY_FINDER_ITEM, Some(tree), "VerticalBoxSlot", json!({ "Padding": pad }), None) else { break };
            if let Some(f) = self.social.finder.as_mut() { f.rows.push(row); }
        }
        let rows = self.social.finder.as_ref().map(|f| f.rows.clone()).unwrap_or_default();
        let header_style = self.ui.find_class_path("ButtonStyle-QuestPrimary-L2_C");
        let player_style = self.ui.find_class_path("ButtonStyle-BasicVerticalList_C");
        for (i, &row) in rows.iter().enumerate() {
            if i >= need { self.ui.w[row].vis = Vis::Collapsed; continue; }
            self.ui.w[row].vis = Vis::Visible;
            let header = i == 0 || i == need - 1;
            let switcher = self.ui.var(row, "PlayerHeaderSwitcher");
            let target = if header { self.ui.var(row, "Header") } else { self.ui.var(row, "PlayerStatus") };
            if let (Some(sw), Some(t)) = (switcher, target) { if let Some(k) = self.ui.w[sw].kids.iter().position(|&c| c == t) { self.ui.w[sw].props["ActiveWidgetIndex"] = json!(k); } }
            if header {
                let text = if i == 0 { self.header_text("PartyInvitesHeader", "Party Invites ({0})", v.invites.len()) } else { self.header_text("FriendsHeader", "Friends ({0})", v.friends) };
                if let Some(t) = self.ui.var(row, "HeaderText") { self.ui.w[t].text = Some(text); }
                if let Some(a) = self.ui.var(row, "Arrow") { self.ui.w[a].transform.angle = 0.0; }
                if let Some(s) = header_style.as_ref() { self.ui.w[row].props["Style"] = json!(s); }
            } else if let Some(inv) = v.invite_at(i) {
                if let Some(t) = self.ui.var(row, "PlayerName") { self.ui.w[t].text = Some(inv.name.clone()); }
                if let Some(t) = self.ui.var(row, "Location") { self.ui.w[t].text = Some(String::new()); }
                let state = self.ui.var(row, "TextState");
                self.ui.set_visibility(state, if inv.joinable { Vis::Collapsed } else { Vis::SelfHitTestInvisible });
                if !inv.joinable {
                    let t = self.ui.text_of(&json!({ "key": "B34E9ED241A0D897340E1FA07EBE5764", "namespace": "[E346916646C989D3512D318360DCBA4E]", "text": "CANNOT JOIN" })).unwrap_or_default();
                    if let Some(s) = state { self.ui.w[s].text = Some(t); }
                }
                if let Some(s) = player_style.as_ref() { self.ui.w[row].props["Style"] = json!(s); }
            }
            self.ui.w[row].btn_state = if i == v.selected { if v.hovered == Some(i) { BtnState::SelectedHovered } else { BtnState::Selected } } else if v.hovered == Some(i) { BtnState::Hovered } else { BtnState::Normal };
            self.ui.refresh_button_text(row);
        }
        let selected_invite = v.invite_at(v.selected).cloned();
        let switcher = self.ui.var(root, "InviteIgnoreSwitcher");
        let join = self.ui.var(root, "JoinButton");
        let ignore = self.ui.var(root, "IgnoreButton");
        let profile = self.ui.var(root, "ViewProfileButton");
        let close = self.ui.var(root, "CloseButton");
        match selected_invite.as_ref() {
            Some(inv) => {
                self.ui.set_visibility(switcher, Vis::SelfHitTestInvisible);
                if let (Some(sw), Some(ig)) = (switcher, ignore) { if let Some(k) = self.ui.w[sw].kids.iter().position(|&c| c == ig) { self.ui.w[sw].props["ActiveWidgetIndex"] = json!(k); } }
                self.ui.set_visibility(join, Vis::SelfHitTestInvisible);
                if let Some(j) = join { self.ui.w[j].enabled = inv.joinable; }
                self.ui.set_visibility(profile, Vis::SelfHitTestInvisible);
            }
            None => {
                self.ui.set_visibility(switcher, Vis::Collapsed);
                self.ui.set_visibility(join, Vis::Collapsed);
                self.ui.set_visibility(profile, Vis::Collapsed);
            }
        }
        for (b, which) in [(join, FinderButton::Join), (ignore, FinderButton::Ignore), (close, FinderButton::Close)] {
            let Some(b) = b else { continue };
            let enabled = self.ui.w[b].enabled;
            self.ui.w[b].btn_state = if !enabled { BtnState::Disabled } else if v.pressed_button == Some(which) { BtnState::Pressed } else if v.hovered_button == Some(which) { BtnState::Hovered } else { BtnState::Normal };
            self.ui.refresh_button_text(b);
        }
        Some(root)
    }

    fn apply_error(&mut self, e: &ErrorDialogView) -> Option<usize> {
        if self.social.error.is_none() {
            let root = self.ui.add_class_instance(ERROR_WINDOW, None, "", json!({}), None)?;
            let list = self.ui.var(root, "MessageList").or_else(|| self.ui.find_in(root, "MessageList"))?;
            let entry = self.ui.add_class_instance(ERROR_ENTRY, Some(list), "VerticalBoxSlot", json!({}), None)?;
            let b = self.ui.var(entry, "IconTextButton");
            self.ui.set_visibility(b, Vis::Collapsed);
            let logout = self.ui.var(root, "LogoutButton");
            self.ui.set_visibility(logout, Vis::Collapsed);
            self.social.error = Some(ErrWin { root, entry });
        }
        let (root, entry) = { let w = self.social.error.as_ref()?; (w.root, w.entry) };
        if let Some(t) = self.ui.var(entry, "Message") { self.ui.w[t].text = Some(e.message.clone()); }
        if let Some(c) = self.ui.var(root, "CloseButton") { self.ui.w[c].btn_state = if e.close_hovered { BtnState::Hovered } else { BtnState::Normal }; self.ui.refresh_button_text(c); }
        Some(root)
    }

    pub fn social_hit_ids(&self, rows: usize) -> (Vec<usize>, [Option<usize>; 3], Option<usize>) {
        let (mut r, mut b) = (vec![], [None; 3]);
        if let Some(f) = self.social.finder.as_ref() {
            r = f.rows.iter().take(rows).copied().collect();
            b = [self.ui.var(f.root, "JoinButton"), self.ui.var(f.root, "IgnoreButton"), self.ui.var(f.root, "CloseButton")];
        }
        let e = self.social.error.as_ref().and_then(|w| self.ui.var(w.root, "CloseButton"));
        (r, b, e)
    }
}
