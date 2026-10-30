//! The Fortnite 1.8 Battle Royale lobby screen (UMG) decoded from the cooked widget blueprints and rendered through the UI draw list.
//!
//! The generic interpreter (`umg`, `layout`, `paint`, `styles`, `slate_mat`) renders the packages as they are. The code in THIS file only reproduces what the
//! 1.8 client does natively around them; each piece names its source:
//! * composition: `FrontEnd::InitForSubGame` (Kismet) creates `AthenaTabsScreen_C` for `ESubGame::Athena` and pushes it into `MainContentStack`;
//! * tab buttons: `HorizontalTabList_C` (`FortTabListWidgetBase`) creates one `TabButtonType` per `PreregisteredTabInfoArray` entry that is not `bHidden`,
//!   `AthenaTabsScreen::HandleTabCreated` (Kismet) then calls `SetStyle(ButtonStyle-Tab-Main)`, `ButtonText = TextToUpper(ButtonText)` and `ShowText`;
//! * the first tab content is `AthenaLobby_C` (`TabContentType` of the first registration) in `TopTabContentWidgetSwitcher`;
//! * bottom bar buttons: `InputReflector::OnButtonAdded` (Kismet) for the actions `AthenaLobby::InitializeInput` / `RefreshInput` (Kismet) registered, text from
//!   `InputActionDataTable` (`DisplayName`), visibility from the row's keyboard state (`OverrrideStateForInputType[0]`) unless the handler state was set;
//! * rotator labels: `AthenaMatchmakingWidget::SetAutoFillOptions` (Kismet) and the native `FortAthenaMatchmakingWidget` playlist strings (locres ns
//!   `Fortnite.FortAthenaMatchmakingWidget`, keys also present as UTF-16 literals in the exe at RVA 0x4ab1d68 / 0x4ade360);
//! * player pads: `LobbyPlayerPadTop` / `LobbyPlayerAddPlayer` (the widgets the lobby pad actors host; placed by the caller at projected screen points).
pub mod layout;
pub mod paint;
pub mod slate_mat;
pub mod social;
pub mod store;
pub mod styles;
pub mod umg;
#[cfg(test)]
mod tests;

use crate::draw::RenderList;
use crate::layout::Rect;
use crate::scene::UiContext;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use store::Store;
use styles::BtnState;
use umg::{Ui, Vis};
pub use social::{ErrorDialogView, FinderButton, InviteRow, PartyFinderView};

pub const FRONTEND: &str = "/Game/UI/Foundation/StateLayouts/FrontEnd.FrontEnd_C";
pub const TABS_SCREEN: &str = "/Game/Athena/UI/Frontend/AthenaTabsScreen.AthenaTabsScreen_C";
pub const LOBBY: &str = "/Game/Athena/UI/Frontend/AthenaLobby.AthenaLobby_C";
pub const ICON_TAB_BUTTON: &str = "/Game/UI/Foundation/Buttons/IconTabButton.IconTabButton_C";
pub const PAD_TOP: &str = "/Game/UI/Frontend/Lobby/PlayerPad/LobbyPlayerPadTop.LobbyPlayerPadTop_C";
pub const PAD_ADD: &str = "/Game/UI/Frontend/Lobby/PlayerPad/LobbyPlayerAddPlayer.LobbyPlayerAddPlayer_C";
const INPUT_ACTIONS: &str = "/Game/UI/Foundation/Input/InputActionDataTable";

/// A world-space lobby pad widget (screen-space `UWidgetComponent`) placed by the caller at the projected component location (pixels).
#[derive(Clone, Debug)]
pub enum PadKind {
    /// `LobbyPlayerPadTop`: crown (party leader), name (None = the serialized design-time text), ready state
    Player { name: Option<String>, leader: bool, ready: bool },
    /// `LobbyPlayerAddPlayer`: the empty party slot "+" marker
    Empty,
}
#[derive(Clone, Debug)]
pub struct Pad {
    /// projected location of the pad's `UWidgetComponent` (pixels)
    pub x: f32, pub y: f32,
    /// `UWidgetComponent.Pivot` (read from the pad blueprint: name plate (0.5, 1.0), "+" marker default (0.5, 0.5)); the widget's desired-size box is placed at `anchor - size * pivot`
    pub pivot: [f32; 2],
    pub kind: PadKind }

/// Runtime-driven parts of the screen (everything else is the widget data). Defaults = the settled state of a freshly entered lobby.
#[derive(Clone, Debug)]
pub struct LobbyState {
    /// index into the visible tab registrations (0 = Lobby)
    pub selected_tab: usize,
    pub hovered_tab: Option<usize>,
    /// `AthenaRotator` of the playlist: 0 = Solo, 1 = Duo, 2 = Squad
    pub playlist: usize,
    /// `TeamFillRotator` selection the player chose for Squad (`bCurrentSquadFill`): 0 = first `SetAutoFillOptions` entry ("No Fill"), 1 = second ("Fill").
    /// Solo / Duo force their own index (see [`team_fill_rule`]).
    pub team_fill: usize,
    /// `PartyGameState::IsLocalPlayerPartyLeader` (a local party of one is its own leader): non-leaders get both rotators disabled
    pub party_leader: bool,
    /// `FortPrivateAccountInfo.MtxBalance` (V-Bucks) shown by `MTXButton.Amount` (`CommonNumericTextBlock::SetCurrentValue(float)` from `Construct` /
    /// `UpdateAmount`); runtime account data, default 0
    pub mtx_balance: i64,
    /// `PlayerPanel` ("PLAYER MENU"): hidden until a lobby player pad is hovered / clicked (`AthenaLobby` Kismet)
    pub player_panel: bool,
    pub play_hovered: bool,
    /// world-space pad widgets (name plates / empty party slots) at projected screen points
    pub pads: Vec<Pad>,
    /// the `CommonButton` under the pointer / pressed (widget ids of the composed tree, set by `athena_locker::controller`): hover and press button states
    pub hover_widget: Option<usize>,
    pub press_widget: Option<usize>,
    /// the Locker tab (`athena_locker`): owned items, equipped items, the open picker
    pub locker: crate::screens::athena_locker::LockerView,
    pub can_matchmake: bool,
    pub matchmaking: Option<MatchmakingView>,
    pub cancel_hovered: bool,
    pub quests: Vec<QuestRow>,
    pub season: Option<SeasonView>,
    pub chat: Option<(String, String)>,
    pub chat_input: Option<String>,
    pub join_code_input: Option<String>,
    pub party_finder: Option<PartyFinderView>,
    pub error_dialog: Option<ErrorDialogView>,
    pub mat_time: f32,
    pub options: Vec<MatchOption>,
}
impl Default for LobbyState {
    fn default() -> Self { LobbyState { selected_tab: 0, hovered_tab: None, playlist: 0, team_fill: 0, party_leader: true, mtx_balance: 0, player_panel: false, play_hovered: false, pads: vec![], hover_widget: None, press_widget: None, locker: Default::default(), can_matchmake: true, matchmaking: None, cancel_hovered: false, quests: vec![], season: None, chat: None, chat_input: None, join_code_input: None, party_finder: None, error_dialog: None, mat_time: 0.0, options: MatchOption::defaults() } }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchOption { pub id: &'static str, pub label: &'static str, pub values: Vec<OptionText>, pub selected: usize }

#[derive(Clone, Debug, PartialEq)]
pub enum OptionText { Locres { ns: &'static str, key: &'static str }, Literal(&'static str) }

pub const ENHANCED_MODE: &str = "enhanced_mode";
pub const BOTS: &str = "bots";
pub const BOT_COUNTS: [usize; 5] = [0, 10, 25, 50, 99];
pub const OPTION_BOTS: [OptionText; 5] = [OptionText::Literal("Off"), OptionText::Literal("10"), OptionText::Literal("25"), OptionText::Literal("50"), OptionText::Literal("99")];
pub const OPTION_OFF_ON: [OptionText; 2] = [OptionText::Literal("Off"), OptionText::Literal("On")];

impl MatchOption {
    pub fn defaults() -> Vec<MatchOption> {
        vec![MatchOption { id: ENHANCED_MODE, label: "Enhanced Mode", values: OPTION_OFF_ON.to_vec(), selected: 0 }, MatchOption { id: BOTS, label: "Bots", values: OPTION_BOTS.to_vec(), selected: 0 }]
    }
    pub fn step(&mut self, d: i8) { let n = self.values.len().max(1) as i32; self.selected = (self.selected as i32 + d as i32).rem_euclid(n) as usize; }
    pub fn is_on(&self) -> bool { self.selected != 0 }
}
impl LobbyState {
    pub fn option(&self, id: &str) -> Option<&MatchOption> { self.options.iter().find(|o| o.id == id) }
    pub fn option_mut(&mut self, id: &str) -> Option<&mut MatchOption> { self.options.iter_mut().find(|o| o.id == id) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchmakingView {
    pub header: MmHeader,
    pub message: String,
    pub is_matchmaking: bool,
    pub cancel_enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmHeader { Matchmaking, MatchmakingError, NotMatchmaking }
impl MmHeader {
    pub fn key(self) -> &'static str { match self { MmHeader::Matchmaking => "Header.Matchmaking", MmHeader::MatchmakingError => "Header.MatchmakingError", MmHeader::NotMatchmaking => "Header.NotMatchmaking" } }
}

pub const MM_NS: &str = "Fortnite.FortAthenaMatchmakingWidget";

#[derive(Clone, Debug, PartialEq)]
pub struct SeasonView { pub level: i32, pub max_level: i32, pub level_xp: i32, pub level_xp_for_level: i32, pub chase_reward_level: Option<i32> }

#[derive(Clone, Debug, PartialEq)]
pub struct QuestRow { pub name: String, pub task: String, pub achieved: i32, pub required: i32, pub reward_xp: i32 }

pub const THROBBER_MONO: &str = "/Game/UI/Foundation/Materials/Throbber_Material-Monochrome.Throbber_Material-Monochrome";


/// The native `FortAthenaMatchmakingWidget` refresh (exe RVA 0x2eb0a20, called from `OnPlaylistRotatorChanged` 0x2e96470 / the party-state updates) for
/// `CurrentPlaylist` (`+0x250`: 0 Solo, 1 Duo, 2 Squad) and `bCurrentSquadFill` (`+0x251`), on `TeamFillRotator` (`+0x288`) / `PlaylistRotator` (`+0x280`):
/// * Squad (`cmp [this+0x250], 2`): `TeamFillRotator.SetIsEnabled(true)` (0x3934ad0) + `SetSelectedItem(bCurrentSquadFill)` (0x3954d20);
/// * otherwise `SetSelectedItem(CurrentPlaylist == 1)` (Duo = "Fill", Solo = "No Fill") then `SetIsEnabled(false)` (0x39341c0);
/// * then, unless the local player is the party leader (`PartyGameState::IsLocalPlayerPartyLeader`, 0x3bd0ca0), both rotators are disabled.
/// Returns (selected index, enabled).
pub fn team_fill_rule(playlist: usize, squad_fill: usize, party_leader: bool) -> (usize, bool) {
    let (idx, en) = match playlist { 2 => (squad_fill.min(1), true), 1 => (1, false), _ => (0, false) };
    (idx, en && party_leader)
}

/// `FText::AsNumber` of an integer (default formatting options: digit grouping with ",").
fn group_digits(v: i64) -> String {
    let s = v.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 { out.push(','); }
        out.push(c);
    }
    if v < 0 { format!("-{out}") } else { out }
}

fn paint_chat_strip(uic: &mut UiContext, out: &mut RenderList, w: f32, h: f32, scale: f32, channel: &str, hint: &str, input: Option<&str>) {
    use crate::text::FontId;
    let px = crate::screens::atlas_px(16.0, scale);
    let bar_top = h - 60.0 * scale;
    let y0 = bar_top + 2.0 * scale;
    let pad = 15.0 * scale;
    let cw = uic.fonts.measure(FontId::Bold, channel, px as f32);
    let srgb = |r: f32, g: f32, b: f32| [r / 255.0, g / 255.0, b / 255.0, 1.0];
    let enc = |c: f32| if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    out.fill(Rect::new(0.0, y0, pad * 2.0 + cw, (h - y0).max(0.0)), [enc(0.005), enc(0.012), enc(0.022), 1.0]);
    let cap = px as f32 * 0.714;
    let baseline = (y0 + (h - y0 + cap) * 0.5).round();
    crate::screens::put_text(uic, out, FontId::Bold, px, pad, baseline, channel, [1.0, 1.0, 1.0, 1.0], None, crate::screens::Snap::Pen);
    let x = pad * 2.0 + cw + 14.0 * scale;
    match input {
        Some(t) => {
            let tw = uic.fonts.measure(FontId::Bold, t, px as f32);
            crate::screens::put_text(uic, out, FontId::Bold, px, x, baseline, t, [1.0, 1.0, 1.0, 1.0], None, crate::screens::Snap::Pen);
            out.fill(Rect::new(x + tw + 1.0 * scale, baseline - cap - 2.0 * scale, (1.5 * scale).max(1.0), cap + 4.0 * scale), [1.0, 1.0, 1.0, 1.0]);
        }
        None => crate::screens::put_text(uic, out, FontId::Bold, px, x, baseline, hint, srgb(99.0, 101.0, 112.0), None, crate::screens::Snap::Pen),
    }
    let _ = w;
}

pub const JOIN_CODE_LABEL: &str = "Join code";

/// One drawn frame.
pub struct Frame {
    pub list: RenderList,
    /// `BackgroundBlur` regions (pixels) the backend may blur behind the UI
    pub blur: Vec<paint::BlurRegion>,
    /// rectangle (pixels) of every widget by plain widget name (first match wins)
    pub rects: HashMap<String, Rect>,
    pub tabs: Vec<(String, Rect)>,
    /// rectangle of each pad widget, in the order of `LobbyState::pads`
    pub pads: Vec<Rect>,
    /// pixel rectangle of every widget of the composed tree by widget id (for hit testing, `athena_locker::hit`)
    pub id_rects: Vec<Rect>,
    /// assets / materials / strings that could not be resolved (empty when everything was found)
    pub missing: Vec<String>,
}
impl Frame {
    pub fn rect(&self, name: &str) -> Option<Rect> { self.rects.get(name).copied() }
}

pub fn tab_shown(r: &Value) -> bool {
    !r.get("bHidden").and_then(|b| b.as_bool()).unwrap_or(false) && !r.get("TabContentType").and_then(|c| c.as_str()).map_or(false, |c| c.contains("StoreMain_Root"))
}

pub struct AthenaLobbyUi {
    pub ui: Ui,
    pub root: usize,
    tabs: Vec<usize>,
    tab_labels: Vec<String>,
    /// `TopTabContentWidgetSwitcher` of `AthenaTabsScreen` and, per visible tab registration, its `TabContentType` class and the composed page (pages are composed
    /// the first time their tab is selected, like the tab list creating the content widget on demand)
    switcher: usize,
    tab_content: Vec<String>,
    pages: Vec<Option<usize>>,
    /// the Locker page (`AthenaCustomizationTab_C`) once composed
    locker: Option<crate::screens::athena_locker::LockerWidgets>,
    last_hover: [Option<usize>; 2],
    quest_rows: Vec<usize>,
    /// pooled pad widgets: (pad index, is_player) -> root widget
    pad_roots: HashMap<(usize, bool), usize>,
    mats: HashMap<String, Option<Rc<slate_mat::SlateMat>>>,
    card_mats: crate::screens::athena_locker::card::CardMats,
    social: social::Social,
    join_code: Option<usize>,
    option_rows: Vec<(usize, usize)>,
    pub composition_log: Vec<String>,
}

impl AthenaLobbyUi {
    pub fn release_material_cache(&mut self) -> usize { let n = self.mats.len(); self.mats.clear(); n }
    pub fn load(install: &Path) -> Result<AthenaLobbyUi, String> {
        let store = Rc::new(Store::open(install)?);
        Self::from_store(store)
    }

    pub fn from_store(store: Rc<Store>) -> Result<AthenaLobbyUi, String> {
        let mut ui = Ui::new(store);
        let root = ui.add_class_instance(FRONTEND, None, "", json!({}), None).ok_or("cannot compose FrontEnd")?;
        // FrontEnd::InitForSubGame (Athena): AthenaTabsScreen_C pushed into MainContentStack
        let stack = ui.find_in(root, "MainContentStack").ok_or("FrontEnd has no MainContentStack")?;
        let tabs_screen = ui.add_class_instance(TABS_SCREEN, Some(stack), "PanelSlot", json!({}), None).ok_or("cannot compose AthenaTabsScreen")?;
        // the first tab content (`TabContentType` of the Lobby registration): AthenaLobby_C in the TopTabContentWidgetSwitcher
        let switcher = ui.var(tabs_screen, "TopTabContentWidgetSwitcher").or_else(|| ui.find_in(tabs_screen, "TopTabContentWidgetSwitcher")).ok_or("no TopTabContentWidgetSwitcher")?;
        let lobby_page = ui.add_class_instance(LOBBY, Some(switcher), "WidgetSwitcherSlot", json!({}), None).ok_or("cannot compose AthenaLobby")?;
        // HorizontalTabList_C: one TabButtonType (IconTabButton_C) per registration that is not bHidden
        let list = ui.var(tabs_screen, "TopTabList").or_else(|| ui.find_in(tabs_screen, "TopTabList")).ok_or("no TopTabList")?;
        let tab_box = ui.var(list, "TabButtonBox").or_else(|| ui.find_in(list, "TabButtonBox")).ok_or("no TabButtonBox")?;
        let style = ui.w[list].props.get("ButtonStyle").cloned().unwrap_or(json!(null));
        let pad = ui.w[list].props.get("TabButtonPadding").cloned().unwrap_or(json!({}));
        let regs = ui.w[list].props.get("PreregisteredTabInfoArray").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        let mut tabs = vec![];
        let mut labels = vec![];
        let mut content = vec![];
        for r in regs {
            if !tab_shown(&r) { continue; }
            content.push(r.get("TabContentType").and_then(|c| c.as_str()).unwrap_or("").to_owned());
            let dn = r.get("TabLabelInfo").and_then(|l| l.get("DisplayName")).cloned().unwrap_or(json!(null));
            // AthenaTabsScreen::HandleTabCreated: SetStyle(ButtonStyle-Tab-Main); ButtonText = TextToUpper(ButtonText); ShowText()
            let upper = ui.text_of(&dn).unwrap_or_default().to_uppercase();
            let inst = json!({ "Style": style, "ButtonText": null });
            let t = ui.add_class_instance(ICON_TAB_BUTTON, Some(tab_box), "HorizontalBoxSlot", json!({ "Padding": pad }), Some(inst)).ok_or("cannot compose IconTabButton")?;
            if let Some(tw) = ui.var(t, "CenterButtonTextWidget") { ui.w[tw].text = Some(upper.clone()); ui.w[tw].vis = if upper.is_empty() { Vis::Collapsed } else { Vis::SelfHitTestInvisible }; }
            tabs.push(t);
            labels.push(upper);
        }
        let mut pages = vec![None; tabs.len()];
        if let Some(p) = pages.first_mut() { *p = Some(lobby_page); }
        let mut me = AthenaLobbyUi { ui, root, tabs, tab_labels: labels, switcher, tab_content: content, pages, locker: None, last_hover: [None; 2], quest_rows: vec![], pad_roots: HashMap::new(), mats: HashMap::new(), card_mats: Default::default(), social: Default::default(), join_code: None, option_rows: vec![], composition_log: vec![] };
        me.reflect_input_actions()?;
        me.install_option_rows(MatchOption::defaults().len());
        me.composition_log = me.ui.log.clone();
        Ok(me)
    }

    /// `InputReflector` (bottom bar): one `IconTextButton_C` per registered input action that is visible for mouse + keyboard.
    /// Actions registered by `AthenaLobby::InitializeInput` (Kismet): Scroll (no handler), Cancel, PlaylistChange, FillChange, AthenaNews. `RefreshInput`
    /// (Kismet) sets the handler state of Cancel (`ShouldAddCancel ? 2 : 3`), Scroll (`ShouldAddScroll ? 2 : 3`) and News (0 = Enabled); the others keep the
    /// row state of the `InputActionDataTable` (`OverrrideStateForInputType[0]`, mouse + keyboard). States: 0 Enabled, 1 Disabled, 2 Hidden, 3 HiddenAndDisabled.
    /// `ShouldAddCancel` / `ShouldAddScroll` are read as false in the idle lobby (matchmaking / scrolling lists only).
    fn reflect_input_actions(&mut self) -> Result<(), String> {
        let ui = &mut self.ui;
        let Some(refl) = ui.find_by_name("InputReflector") else { return Ok(()) };
        let Some(bbox) = ui.var(refl, "ButtonBox") else { return Ok(()) };
        let margin = ui.w[refl].props.get("DefaultButtonMargin").cloned().unwrap_or(json!({}));
        let style = ui.find_class_path("ButtonStyle-BottomBar_C");
        let Some(btn_class) = ui.find_class_path("IconTextButton_C") else { return Ok(()) };
        let table = ui.store.pkg(INPUT_ACTIONS).and_then(|p| p.named("InputActionDataTable").map(|e| e.props.clone())).unwrap_or(Value::Null);
        let handler: [(&str, u8); 5] = [("Cancel", 3), ("Scroll", 3), ("AthenaNews", 0), ("PlaylistChange", 255), ("SquadFillChange", 255)];
        for (row, hs) in handler {
            if row == "AthenaNews" {
                let inst = json!({ "ButtonText": { "text": JOIN_CODE_LABEL }, "Style": style });
                let slot = json!({ "HorizontalAlignment": "HAlign_Center", "VerticalAlignment": "VAlign_Center", "Padding": margin });
                self.join_code = Some(ui.add_class_instance(&btn_class, Some(bbox), "HorizontalBoxSlot", slot, Some(inst)).ok_or("cannot compose the join code box")?);
            }
            let Some(r) = table.get("Rows").and_then(|t| t.get(row)) else { continue };
            let state = if hs != 255 { hs } else {
                match r.get("OverrrideStateForInputType").and_then(|s| s.as_str()).map(|s| s.rsplit("::").next().unwrap_or(s)) { Some("Enabled") => 0, Some("Disabled") => 1, Some("Hidden") => 2, _ => 3 }
            };
            if state > 1 { continue; }
            let text = r.get("DisplayName").cloned().unwrap_or(Value::Null);
            // OnButtonAdded: SetMouseKeyboardStyle(ButtonStyle-BottomBar); AddChildToHorizontalBox -> VAlign Center, HAlign Center, padding DefaultButtonMargin
            let inst = json!({ "ButtonText": text, "Style": style });
            let slot = json!({ "HorizontalAlignment": "HAlign_Center", "VerticalAlignment": "VAlign_Center", "Padding": margin });
            ui.add_class_instance(&btn_class, Some(bbox), "HorizontalBoxSlot", slot, Some(inst)).ok_or("cannot compose a reflected input button")?;
        }
        Ok(())
    }

    pub fn tab_labels(&self) -> &[String] { &self.tab_labels }
    pub fn option_rows(&self) -> &[(usize, usize)] { &self.option_rows }

    fn install_option_rows(&mut self, n: usize) {
        let Some(fill) = self.ui.find_by_name("TeamFillRow") else { return };
        let Some(parent) = self.ui.w[fill].parent else { return };
        for i in 0..n {
            let row = self.ui.clone_subtree(fill, parent, &format!("MatchOptionRow{i}"));
            let Some(rot) = self.ui.find_in(row, "TeamFillRotator") else { continue };
            self.ui.w[rot].name = format!("MatchOptionRotator{i}");
            self.option_rows.push((row, rot));
        }
    }

    fn apply_options(&mut self, st: &LobbyState) {
        let locres = self.ui.store.locres();
        let enabled = st.party_leader && st.matchmaking.is_none();
        for (i, &(row, rot)) in self.option_rows.clone().iter().enumerate() {
            let Some(o) = st.options.get(i) else { self.ui.w[row].vis = Vis::Collapsed; continue };
            self.ui.w[row].vis = Vis::Visible;
            if let Some(label) = self.ui.w[row].kids.iter().copied().find(|&k| self.ui.w[k].class == "CommonTextBlock") { self.ui.w[label].text = Some(o.label.to_owned()); }
            let value = match o.values.get(o.selected) {
                Some(OptionText::Locres { ns, key }) => locres.as_ref().and_then(|l| l.get(ns, key)).map(|s| s.to_owned()).unwrap_or_default(),
                Some(OptionText::Literal(s)) => (*s).to_owned(),
                None => String::new(),
            };
            if let Some(t) = self.ui.var(rot, "MyText") { self.ui.w[t].text = Some(value); }
            self.ui.w[rot].enabled = enabled;
            self.ui.w[rot].btn_state = if enabled { BtnState::Normal } else { BtnState::Disabled };
        }
    }
    pub fn join_code_widget(&self) -> Option<usize> { self.join_code }
    /// How many new item cards one `draw` may bake (see `athena_locker::card::CardMats`); `usize::MAX` for captures.
    pub fn set_card_budget(&mut self, n: usize) { self.card_mats.max_per_frame = n; }
    /// Some item cards were not baked yet this frame (budget): draw again.
    pub fn cards_pending(&self) -> bool { self.card_mats.pending }
    /// The tab button widgets (one per visible tab registration, in order).
    pub fn tab_widgets(&self) -> &[usize] { &self.tabs }
    /// The Locker page once it has been composed (first time its tab is selected).
    pub fn locker(&self) -> Option<&crate::screens::athena_locker::LockerWidgets> { self.locker.as_ref() }
    /// Content class path (`TabContentType`) of a visible tab.
    pub fn tab_content(&self, i: usize) -> Option<&str> { self.tab_content.get(i).map(|s| s.as_str()) }
    /// The composed page widget of a tab (None until it was selected, or when its class could not be composed).
    pub fn page(&self, i: usize) -> Option<usize> { self.pages.get(i).copied().flatten() }

    /// Compose the content widget of tab `i` into `TopTabContentWidgetSwitcher` the first time it is needed (`FortTabListWidgetBase` creates the
    /// `TabContentType` widget of a registration on demand and the switcher activates it).
    pub fn ensure_page(&mut self, i: usize) {
        if i >= self.pages.len() || self.pages[i].is_some() { return; }
        let class = self.tab_content[i].clone();
        if class == crate::screens::athena_locker::TAB_CLASS {
            if let Some(lw) = crate::screens::athena_locker::LockerWidgets::install(&mut self.ui, self.switcher) { self.pages[i] = Some(lw.tab); self.locker = Some(lw); }
            return;
        }
        let id = self.ui.add_class_instance(&class, Some(self.switcher), "WidgetSwitcherSlot", json!({}), None);
        if id.is_none() { self.composition_log.push(format!("tab {i}: cannot compose {class}")); }
        self.pages[i] = id;
    }

    fn apply_state(&mut self, uic: &mut UiContext, st: &LobbyState, viewport: [f32; 2], scale: f32) {
        // the previous frame's hover / press overlay is undone first (the managed states below are reassigned every frame)
        for id in self.last_hover.into_iter().flatten() { if self.ui.w[id].btn_state != BtnState::Disabled { self.ui.w[id].btn_state = BtnState::Normal; self.ui.refresh_button_text(id); } }
        // the tab list activates the page of the selected tab in the content switcher
        self.ensure_page(st.selected_tab);
        let active = self.pages.get(st.selected_tab).copied().flatten().and_then(|pg| self.ui.w[self.switcher].kids.iter().position(|&k| k == pg));
        self.ui.w[self.switcher].props["ActiveWidgetIndex"] = json!(active.map_or(9999, |i| i as i64));
        if self.tab_content.get(st.selected_tab).is_some_and(|c| c == crate::screens::athena_locker::TAB_CLASS) {
            if let Some(l) = self.locker.as_mut() { l.apply(&mut self.ui, uic, &st.locker, viewport, scale); }
        }
        if self.locker.is_none() { if let Some(i) = self.tab_content.iter().position(|c| c == crate::screens::athena_locker::TAB_CLASS) { self.ensure_page(i); } }
        if let Some(l) = self.locker.as_mut() { l.apply_banner(&mut self.ui, uic, &st.locker, scale); }
        let locres = self.ui.store.locres();
        let find = |ui: &Ui, n: &str| ui.find_by_name(n);
        let miss = |ui: &Ui, ns: &str, key: &str| { let m = format!("locres miss: {ns} / {key}"); let mut u = ui.unresolved.borrow_mut(); if !u.contains(&m) { u.push(m); } };
        for (i, &t) in self.tabs.iter().enumerate() {
            let state = if i == st.selected_tab { if st.hovered_tab == Some(i) { BtnState::SelectedHovered } else { BtnState::Selected } } else if st.hovered_tab == Some(i) { BtnState::Hovered } else { BtnState::Normal };
            self.ui.w[t].btn_state = state;
            self.ui.refresh_button_text(t);
        }
        // matchmaking rotators (no fallback strings: a failed lookup shows nothing and is reported)
        if let Some(pr) = find(&self.ui, "PlaylistRotator") {
            let ns = "Fortnite.FortAthenaMatchmakingWidget";
            let key = ["SquadPlaylistSolo", "SquadPlaylistDuo", "SquadPlaylistSquad"][st.playlist.min(2)];
            let label = match locres.as_ref().and_then(|l| l.get(ns, key)) { Some(s) => s.to_owned(), None => { miss(&self.ui, ns, key); String::new() } };
            if let Some(t) = self.ui.var(pr, "MyText") { self.ui.w[t].text = Some(label); }
        }
        if let Some(fr) = find(&self.ui, "TeamFillRotator") {
            // SetAutoFillOptions: PopulateTextLabels(["No Fill", "Fill"]) (FText literals of the Kismet, ns [570C64F3...])
            let ns = "[570C64F344756CC8FFA9A3B51340E2A4]";
            let (fill_idx, fill_enabled) = team_fill_rule(st.playlist, st.team_fill, st.party_leader && st.matchmaking.is_none());
            let key = if fill_idx == 0 { "5FB65AF549F646DD0AB3AAA5E705C869" } else { "B5C6FB4943BBF71C5310128DBF9C6968" };
            let label = match locres.as_ref().and_then(|l| l.get(ns, key)) { Some(s) => s.to_owned(), None => { miss(&self.ui, ns, key); String::new() } };
            if let Some(t) = self.ui.var(fr, "MyText") { self.ui.w[t].text = Some(label); }
            // SetIsEnabled(false): CommonButton state Disabled (own disabled brush / text styles) and the Slate disabled effect on the subtree
            self.ui.w[fr].enabled = fill_enabled;
            self.ui.w[fr].btn_state = if fill_enabled { BtnState::Normal } else { BtnState::Disabled };
        }
        if let Some(pr) = find(&self.ui, "PlaylistRotator") {
            let en = st.party_leader && st.matchmaking.is_none();
            self.ui.w[pr].enabled = en;
            self.ui.w[pr].btn_state = if en { BtnState::Normal } else { BtnState::Disabled };
        }
        // MTXButton.Amount.SetCurrentValue(Conv_IntToFloat(MtxBalance)); NumericType Number -> FText::AsNumber (digit grouping)
        if let Some(m) = find(&self.ui, "MTXButton") {
            if let Some(a) = self.ui.var(m, "Amount") { self.ui.w[a].text = Some(group_digits(st.mtx_balance)); }
        }
        if let Some(pp) = find(&self.ui, "PlayerPanel") { self.ui.w[pp].vis = if st.player_panel { Vis::SelfHitTestInvisible } else { Vis::Hidden }; }
        if let Some(pb) = find(&self.ui, "PlayButton") { self.ui.w[pb].btn_state = if st.play_hovered { BtnState::Hovered } else { BtnState::Normal }; }
        self.apply_join_code(st);
        self.apply_options(st);
        self.apply_matchmaking(st);
        self.apply_season(st);
        self.apply_quests(st);
        // pointer overlay: `UCommonButton` hovered / pressed state (selected buttons keep their selected variants)
        self.last_hover = [st.hover_widget, st.press_widget];
        for (id, pressed) in [(st.hover_widget, false), (st.press_widget, true)] {
            let Some(id) = id.filter(|&i| i < self.ui.w.len()) else { continue };
            let cur = self.ui.w[id].btn_state;
            if cur == BtnState::Disabled || !self.ui.w[id].enabled { continue; }
            let sel = matches!(cur, BtnState::Selected | BtnState::SelectedHovered | BtnState::SelectedPressed);
            self.ui.w[id].btn_state = match (sel, pressed) { (false, false) => BtnState::Hovered, (false, true) => BtnState::Pressed, (true, false) => BtnState::SelectedHovered, (true, true) => BtnState::SelectedPressed };
            self.ui.refresh_button_text(id);
        }
    }

    fn apply_join_code(&mut self, st: &LobbyState) {
        let Some(b) = self.join_code else { return };
        let text = match st.join_code_input.as_deref() { Some(t) => format!("{t}_"), None => JOIN_CODE_LABEL.to_owned() };
        if let Some(tw) = self.ui.var(b, "CenterButtonTextWidget") { self.ui.w[tw].text = Some(text); self.ui.w[tw].vis = Vis::SelfHitTestInvisible; }
        self.ui.w[b].btn_state = if st.join_code_input.is_some() { BtnState::Hovered } else { BtnState::Normal };
        self.ui.refresh_button_text(b);
    }

    fn apply_season(&mut self, st: &LobbyState) {
        let Some(sv) = &st.season else { return };
        let Some(pe) = self.ui.find_by_name("LevelText").and_then(|c| self.ui.w[c].owner) else { return };
        let level_text = self.ui.var(pe, "LevelText");
        let bar = self.ui.var(pe, "XpBar");
        let needed = self.ui.var(pe, "XPNeeded");
        let divider = self.ui.var(pe, "Divider");
        let owned = self.ui.var(pe, "XPOwned");
        if let Some(t) = level_text { self.ui.w[t].text = Some(group_digits(sv.level as i64)); }
        let progress = if sv.level >= sv.max_level { 1.0 } else { sv.level_xp as f32 / sv.level_xp_for_level as f32 };
        if let Some(b) = bar { self.ui.w[b].mat_scalars.retain(|(n, _)| n != "Progress"); self.ui.w[b].mat_scalars.push(("Progress".to_owned(), progress)); }
        if sv.level >= sv.max_level {
            self.ui.set_visibility(needed, Vis::Collapsed);
            self.ui.set_visibility(divider, Vis::Collapsed);
            let max = self.ui.text_of(&json!({ "key": "0BEF62C94E1E0E69C6A44793DA09720E", "namespace": "[F5CB7F194AC4C965DAF989B6538A0C1A]", "text": "" })).unwrap_or_default();
            if let Some(o) = owned { self.ui.w[o].text = Some(max); self.ui.w[o].text_style = self.ui.find_class_path("TextStyle-Base-XS-40pc_C"); }
        } else {
            if let Some(n) = needed { self.ui.w[n].text = Some(group_digits(sv.level_xp_for_level as i64)); }
            if let Some(o) = owned { self.ui.w[o].text = Some(group_digits(sv.level_xp as i64)); }
        }
        let rewards = self.ui.var(pe, "LevelRewardsVB");
        let chase_hb = self.ui.var(pe, "ChaseRewardHB");
        match sv.chase_reward_level {
            Some(l) => {
                self.ui.set_visibility(rewards, Vis::SelfHitTestInvisible);
                self.ui.set_visibility(chase_hb, Vis::SelfHitTestInvisible);
                if let Some(t) = self.ui.var(pe, "NextChaseRewardText") { self.ui.w[t].text = Some(group_digits(l as i64)); }
            }
            None => { self.ui.set_visibility(rewards, Vis::Collapsed); }
        }
    }

    fn apply_quests(&mut self, st: &LobbyState) {
        let Some(list) = self.ui.find_by_name("DailyQuestList") else { return };
        if self.quest_rows.len() != st.quests.len() {
            for &r in &self.quest_rows { self.ui.w[r].vis = Vis::Collapsed; }
            self.ui.w[list].kids.retain(|k| !self.quest_rows.contains(k));
            self.quest_rows.clear();
            let Some(row_class) = self.ui.find_class_path("DailyQuestWidget_C") else { return };
            let entry_class = self.ui.find_class_path("DailyQuestObjectiveEntry_C");
            for _ in &st.quests {
                let Some(row) = self.ui.add_class_instance(&row_class, Some(list), "VerticalBoxSlot", json!({}), None) else { continue };
                if let (Some(ec), Some(ob)) = (entry_class.as_ref(), self.ui.var(row, "ObjectivesBox")) { let _ = self.ui.add_class_instance(ec, Some(ob), "VerticalBoxSlot", json!({}), None); }
                self.quest_rows.push(row);
            }
        }
        for (i, q) in st.quests.iter().enumerate() {
            let Some(&row) = self.quest_rows.get(i) else { continue };
            if let Some(t) = self.ui.var(row, "QuestName") { self.ui.w[t].text = Some(q.name.clone()); }
            let Some(ob) = self.ui.var(row, "ObjectivesBox") else { continue };
            let Some(&entry) = self.ui.w[ob].kids.first() else { continue };
            if let Some(t) = self.ui.var(entry, "TaskName") { self.ui.w[t].text = Some(q.task.clone()); }
            self.ui.w[entry].vis = if q.task.is_empty() { Vis::Collapsed } else { Vis::SelfHitTestInvisible };
            let Some(pw) = self.ui.var(entry, "DailyQuestProgressWidget") else { continue };
            let progress = q.achieved as f32 / q.required.max(1) as f32;
            if let Some(b) = self.ui.var(pw, "XpBar") { self.ui.w[b].mat_scalars.retain(|(n, _)| n != "Progress"); self.ui.w[b].mat_scalars.push(("Progress".to_owned(), progress)); }
            let fmt = format!("{} / {}", group_digits(q.achieved as i64), group_digits(q.required as i64));
            let pv = self.ui.var(pw, "ProgressValue");
            let check = self.ui.var(pw, "Check");
            if let Some(t) = pv { self.ui.w[t].text = Some(fmt); }
            if let Some(sw) = self.ui.var(pw, "CompletionSwitcher") {
                let target = if q.achieved >= q.required { check } else { pv };
                if let Some(i) = target.and_then(|t| self.ui.w[sw].kids.iter().position(|&k| k == t)) { self.ui.w[sw].props["ActiveWidgetIndex"] = json!(i); }
            }
        }
    }

    fn apply_matchmaking(&mut self, st: &LobbyState) {
        let Some(mm) = self.ui.find_by_name("SpinnerAndTextContainer").and_then(|c| self.ui.w[c].owner) else { return };
        let ns = "[570C64F344756CC8FFA9A3B51340E2A4]";
        let lit = |ui: &Ui, ns: &str, key: &str| ui.text_of(&json!({ "key": key, "namespace": ns, "text": "" })).unwrap_or_default();
        let play_key = if !st.can_matchmake { "60D973CA48CE30503032A48CED00089D" } else { ["FAD003004CD43D8DB49EBE96C9CB08C2", "209FEA4E4A83DFD8FCC08D87144BF68D", "D354E85C4F069A4181A671B4D88C8E59"][st.playlist.min(2)] };
        let play_text = lit(&self.ui, ns, play_key);
        let is_mm = st.matchmaking.as_ref().map_or(false, |m| m.is_matchmaking);
        let cancel_key = if is_mm { "FA22A1F64A1D58B0349CB7B59142D2A6" } else { ["BC6D90B143AD58151EFBE1B53A0F9655", "1037E8784F2940860B86028C7B364E9D", "E7858BFC474F8B90C82CE391A5DF9DC7"][st.playlist.min(2)] };
        let cancel_text = lit(&self.ui, ns, cancel_key);
        let play = self.ui.var(mm, "PlayButton");
        let cancel = self.ui.var(mm, "CancelButton");
        let container = self.ui.var(mm, "SpinnerAndTextContainer");
        let header = self.ui.var(mm, "MatchmakingHeaderText");
        let message = self.ui.var(mm, "MatchmakingMessageText");
        for (b, t) in [(play, &play_text), (cancel, &cancel_text)] {
            if let Some(b) = b { if let Some(tw) = self.ui.var(b, "CenterButtonTextWidget") { self.ui.w[tw].text = Some(t.clone()); self.ui.w[tw].vis = if t.is_empty() { Vis::Collapsed } else { Vis::SelfHitTestInvisible }; } }
        }
        match &st.matchmaking {
            Some(m) => {
                self.ui.set_visibility(container, Vis::SelfHitTestInvisible);
                self.ui.set_visibility(play, Vis::Collapsed);
                self.ui.set_visibility(cancel, Vis::Visible);
                if let Some(h) = header { self.ui.w[h].text = Some(lit(&self.ui, MM_NS, m.header.key())); }
                if let Some(t) = message { self.ui.w[t].text = Some(m.message.clone()); }
                if let Some(c) = cancel {
                    self.ui.w[c].enabled = m.cancel_enabled;
                    self.ui.w[c].btn_state = if !m.cancel_enabled { BtnState::Disabled } else if st.cancel_hovered { BtnState::Hovered } else { BtnState::Normal };
                    self.ui.refresh_button_text(c);
                }
                if !self.ui.timed_mats.iter().any(|x| x == THROBBER_MONO) { self.ui.timed_mats.push(THROBBER_MONO.to_owned()); }
                self.ui.mat_time = st.mat_time;
            }
            None => {
                self.ui.set_visibility(container, Vis::Collapsed);
                self.ui.set_visibility(play, Vis::Visible);
                self.ui.set_visibility(cancel, Vis::Collapsed);
                if let Some(c) = cancel { self.ui.w[c].enabled = true; }
            }
        }
        if let Some(p) = play { self.ui.refresh_button_text(p); }
    }

    /// Pad roots for this frame (composed once per slot, reused), with their anchors in Slate units.
    fn prepare_pads(&mut self, st: &LobbyState, scale: f32) -> Vec<(usize, [f32; 2], [f32; 2])> {
        let mut out = vec![];
        for (i, p) in st.pads.iter().enumerate() {
            let player = matches!(p.kind, PadKind::Player { .. });
            let id = match self.pad_roots.get(&(i, player)) {
                Some(&id) => id,
                None => {
                    let class = if player { PAD_TOP } else { PAD_ADD };
                    let Some(id) = self.ui.add_class_instance(class, None, "", json!({}), None) else { continue };
                    self.pad_roots.insert((i, player), id);
                    id
                }
            };
            if let PadKind::Player { name, leader, ready } = &p.kind { self.ui.hook_pad_top(id, name.as_deref(), *leader, *ready); }
            out.push((id, [p.x / scale, p.y / scale], p.pivot));
        }
        out
    }

    /// Lay out and paint the screen for a `w` x `h` pixel viewport (transparent: nothing is drawn behind the widgets).
    pub fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, st: &LobbyState) -> Frame {
        let scale = crate::layout::dpi_scale(w, h);
        self.card_mats.baked = 0;
        self.card_mats.pending = false;
        self.apply_state(uic, st, [w / scale, h / scale], scale);
        let pads = self.prepare_pads(st, scale);
        let mut overlays = self.apply_social(st.party_finder.as_ref(), st.error_dialog.as_ref());
        if st.locker.banner_editor.is_some() { if let Some(m) = self.locker.as_ref().and_then(|l| l.banner.modal()) { overlays.push(m); } }
        let lay = layout::run_with(&mut self.ui, uic, self.root, [w / scale, h / scale], scale, &pads, &overlays);
        let px = |r: [f32; 4]| Rect::new(r[0] * scale, r[1] * scale, r[2] * scale, r[3] * scale);
        let id_rects: Vec<Rect> = lay.rect.iter().map(|r| px(*r)).collect();
        let mut p = paint::Painter { c: layout::Ctx { ui: &mut self.ui, uic, lay }, out: RenderList::new(), mats: &mut self.mats, blur: vec![], missing: vec![], scale, disabled: false };
        p.paint(self.root, paint::Tf::ID, [1.0; 4]);
        for &(id, _, _) in &pads { p.paint(id, paint::Tf::ID, [1.0; 4]); }
        // the native item cards of the Locker (slot buttons and picker tiles) are painted over their (empty) `FortMultiSizeItemCard` rectangles
        if let Some(l) = self.locker.as_ref() { l.paint_cards(&*p.c.ui, &mut *p.c.uic, &mut self.card_mats, &mut p.out, &id_rects, &mut p.missing); }
        if let Some((channel, hint)) = st.chat.as_ref() { paint_chat_strip(&mut *p.c.uic, &mut p.out, w, h, scale, channel, hint, st.chat_input.as_deref()); }
        for &o in &overlays { p.paint(o, paint::Tf::ID, [1.0; 4]); }
        let (list, blur, mut missing) = (std::mem::take(&mut p.out), std::mem::take(&mut p.blur), std::mem::take(&mut p.missing));
        let lay = p.c.lay;
        missing.extend(self.ui.unresolved.borrow().iter().cloned());
        let mut rects = HashMap::new();
        for wd in &self.ui.w { if wd.id < lay.rect.len() { rects.entry(wd.name.clone()).or_insert(px(lay.rect[wd.id])); } }
        let tabs = self.tabs.iter().zip(&self.tab_labels).map(|(&t, n)| (n.clone(), px(lay.rect[t]))).collect();
        let pad_rects = pads.iter().map(|&(id, _, _)| px(lay.rect[id])).collect();
        Frame { list, blur, rects, tabs, pads: pad_rects, id_rects, missing }
    }

    /// Text dump of the composed tree with arranged rectangles (Slate units) for a viewport.
    pub fn dump(&mut self, uic: &mut UiContext, w: f32, h: f32, st: &LobbyState) -> String {
        let scale = crate::layout::dpi_scale(w, h);
        self.apply_state(uic, st, [w / scale, h / scale], scale);
        let pads = self.prepare_pads(st, scale);
        let lay = layout::run(&mut self.ui, uic, self.root, [w / scale, h / scale], scale, &pads);
        let mut s = String::new();
        self.ui.dump(self.root, &mut s, 0, Some(&lay.rect));
        for &(id, _, _) in &pads { self.ui.dump(id, &mut s, 0, Some(&lay.rect)); }
        s
    }
}

