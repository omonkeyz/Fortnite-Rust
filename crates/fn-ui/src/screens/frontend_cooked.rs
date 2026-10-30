//! FRONT END: the "Logging In..." screen and the game picker drawn from the COOKED 1.8 widget trees with the generic UMG runtime of `athena_lobby`
//! (`umg::Ui`), nothing recreated.
//!
//! * LOGIN: `LoginScreen_C` (`/Game/UI/Foundation/StateLayouts/LoginScreen`) = VerticalBox_0 > SafeZone_1 > Overlay_0 [`LoginBackground` (an empty NamedSlot),
//!   `LoginFlowStack` (CommonWidgetStack), `ConsoleProfileWidget` (Collapsed), `Widget_BuildWatermark`] + `BackBorder` (Collapsed). Its Blueprint
//!   (`ExecuteUbergraph_LoginScreen`, `fn-inspect disasm`): `StartAutoLogin` -> `UpdateStatusTitle("Logging In...")`; `ShowStatusWindow(text)` creates
//!   `StatusWidget_C`, `SetTitleText(text)` and pushes it on `LoginFlowStack`. `StatusWidget_C` (`/Game/UI/Login/Status/StatusWidget`): Spacer 256,
//!   `CommonLazyImage_0` (`T-Logo-Wordmark-1024`, 1024x320), HorizontalBox_1 [`Image_0` (`Throbber_Material-Monochrome`, 64x64), `TitleText`
//!   (TextStyle-Header-L, "Logging In...")], `StatusText` (TextStyle-Base-S) bound to `UpdateLoginStatusText` = `UFortMcpContext::GetLoginStatusText`
//!   (exe 0x140709f00: the online login task's status text, an empty FText without one -> empty here).
//! * PICKER: `SubgameSelect_C` (`/Game/UI/Foundation/StateLayouts/SubgameSelect`): `OnEnterState` pushes the cached `SubgameSelectScreen_C` with
//!   `PushContentWidgetAdvanced(w, true, true, false)` -> `UpdateHeaderAndFooter(bHideHeader, bHideFooter)`: `TopBar` and `BottomBar` Collapsed.
//!   `SubgameSelectScreen_C` (`/Game/UI/Login/SubGameSelect/SubgameSelectScreen`): `BGImage` (ScaleBox ScaleToFill, `M_SGS-Back-Inst` 1920x1080) under
//!   `SwitcherMenu` [VboxMainMenu { ConsoleProfileWidget, `TitleImage` (wordmark 768x240), [CampaignBtn + "On sale!"], [`AthenaBtn` "BATTLE ROYALE" +
//!   `PVPDescriptionBorder`], [`ButtonExit` "QUIT"], `FullDescription` }, the upsell page]. Behaviour: hovering `AthenaBtn` -> `SetDescriptionForSubGame(1)`
//!   + `SwitchMaterial` forward (`bGameMode` 0 -> 1 in 0.25 s: the material shows `T_BattleRoyaleKeyArt`); hovering `ButtonExit` -> `FullDescription`
//!   = "Exit Fortnite."; `AthenaBtn` clicked -> `SafeSetSubGame(1)`; `ButtonExit` clicked -> `QuitGame`. `GetFullDescription` / `GetShortDescription`
//!   (exe 0x1430e2610 / 0x1430e28c0 -> 0x1430e2260) read the Epic CMS JSON (`EpicCMSUIFramework`): offline they return false and the texts stay empty.
//!   `BuildWatermark.Update Watermark` = `GetWatermark` (exe 0x142e350b0): "Ver-CL-..." only with a logged-in net id, else empty.
//!
//! * CMS: `GetShortDescription` / `GetFullDescription` (exe 0x1430e28c0 / 0x1430e2610 -> 0x1430e2260) read the Epic CMS page `subgameselectdata`
//!   (`EpicCMSUIFramework`), which is online data, not in the 1.8 folder: offline they return false, `InitializeShortDescriptions` empties
//!   `PVEDescriptionText` / `PVPDescriptionText` and `SetDescriptionForSubGame` writes the unset (empty) text.
//! * `ConsoleProfileWidget` ("Signed in as:") shows `GetPlatformDisplayName` (the signed-in account's name; the live 1.8 client shows it on PC).
//! * Save the World: `CampaignBtn` is shown and centred first as in 1.8 (`OnActivated`: description 0, `bGameMode` 0 = its art); clicking it does
//!   nothing (this port runs Battle Royale only).
//! NOT RENDERED YET: the 3D front-end level (`Frontend_BG_Main` through `FrontendCamera_Main`, `SetFrontEndCamera(7)`) behind the empty
//! `LoginBackground` and the partly transparent picker background.
use super::athena_lobby::layout::{self as ul};
use super::athena_lobby::paint::{Painter, Tf};
use super::athena_lobby::slate_mat::SlateMat;
use super::athena_lobby::store::Store;
use super::athena_lobby::styles::BtnState;
use super::athena_lobby::umg::{Ui, Vis};
use super::picker::{Item, Layout, Picker};
use crate::draw::RenderList;
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

pub const LOGIN_SCREEN: &str = "/Game/UI/Foundation/StateLayouts/LoginScreen.LoginScreen_C";
pub const STATUS_WIDGET: &str = "/Game/UI/Login/Status/StatusWidget.StatusWidget_C";
pub const SUBGAME_SELECT: &str = "/Game/UI/Foundation/StateLayouts/SubgameSelect.SubgameSelect_C";
pub const SELECT_SCREEN: &str = "/Game/UI/Login/SubGameSelect/SubgameSelectScreen.SubgameSelectScreen_C";
pub const THROBBER: &str = "/Game/UI/Foundation/Materials/Throbber_Material-Monochrome.Throbber_Material-Monochrome";
/// `BGImage`'s material, rastered at half its pixel size (a smooth photo; the full-size CPU raster takes ~1 s natively)
pub const BG_MATERIAL: &str = "/Game/UI/Login/SubGameSelect/Materials/M_SGS-Back-Inst.M_SGS-Back-Inst";
const BG_RASTER: f32 = 0.5;
/// "Exit Fortnite." (`ExecuteUbergraph_SubgameSelectScreen` 0xe4e, the `ButtonExit` hover): its cooked FText (namespace, key, source)
const EXIT_TEXT: (&str, &str, &str) = ("[3D1736F94943220411E725AB39A6039E]", "27EE4ECC419D23205564B695969090C5", "Exit Fortnite.");
/// `GetLoginStatusText` of the PC login steps in the order the live 1.8 client runs them (its FortniteGame.log, `[UOnlineAccountCommon::
/// ContinueLoggingIn] Login: <text> (<step>)`, recorded with the ogfn-launch skill 2026-10-06): (namespace, key, source) of the 1.8 localization;
/// `CheckPlatformPlayAllowed` shows an empty text. Step texts: `UOnlineAccountCommon` (exe 0x142d7dfb0 registers its steps) and the Fortnite steps
/// (exe 0x140765400).
pub const LOGIN_STEPS: [(&str, &str, &str); 14] = [
    ("OnlineAccount", "WaitingRoom", "Checking Epic services queue..."), ("OnlineAccount", "LoggingIn", "Signing in to Epic services..."),
    ("OnlineAccount", "EulaCheck", "Verifying EULA..."), ("", "", ""),
    ("OnlineAccount", "CheckServiceAvailability", "Contacting services..."), ("OnlineAccount", "CheckEntitledToPlay", "Checking entitlements..."),
    ("FortOnlineAccount", "DownloadingClientSettings", "Downloading user settings..."), ("FortOnlineAccount", "QueryProfile", "Downloading profile..."),
    ("OnlineAccount", "RedeemOfflinePurchases", "Redeeming offline purchases..."), ("OnlineAccount", "CheckingRejoin", "Searching for session to rejoin..."),
    ("OnlineAccount", "DoQosPingTests", "Checking connection to datacenters..."), ("FortOnlineAccount", "CreatingParty", "Creating party..."),
    ("FortOnlineAccount", "ConnectToVivox", "Connecting audio..."), ("OnlineAccount", "SignInCompleting", "Completing Sign-in..."),
];
/// `SwitchMaterial` steps pre-rastered (0, 0.25, .. 1)
const MODE_STEPS: usize = 4;
const TEXT_STYLE_S: &str = "/Game/UI/Foundation/Text/TextStyle-Base-S.TextStyle-Base-S_C";

type Mats = HashMap<String, Option<Rc<SlateMat>>>;

fn var(ui: &Ui, owner: usize, n: &str) -> Option<usize> { ui.var(owner, n).or_else(|| ui.find_in(owner, n)) }
fn collapse(ui: &mut Ui, id: Option<usize>) { if let Some(i) = id { ui.w[i].vis = Vis::Collapsed; } }
fn set_text(ui: &mut Ui, id: Option<usize>, t: &str) { if let Some(i) = id { ui.w[i].text = Some(t.to_owned()); } }

/// Full-screen layout + paint of a composed tree (two passes, as `layout::run`).
fn draw_tree(ui: &mut Ui, uic: &mut UiContext, mats: &mut Mats, root: usize, w: f32, h: f32, missing: &mut Vec<String>) -> (RenderList, Vec<[f32; 4]>) {
    let s = dpi_scale(w, h);
    let lay = ul::run(ui, uic, root, [w / s, h / s], s, &[]);
    let rects = lay.rect.clone();
    let mut p = Painter { c: ul::Ctx { ui, uic, lay }, out: RenderList::new(), mats, blur: vec![], missing: vec![], scale: s, disabled: false };
    p.paint(root, Tf::ID, [1.; 4]);
    for m in std::mem::take(&mut p.missing) { if !missing.contains(&m) { eprintln!("[front-end] not drawn: {m}"); missing.push(m); } }
    (std::mem::take(&mut p.out), rects)
}

/// `LoginScreen_C` with `StatusWidget_C` ("Logging In...") on its `LoginFlowStack`.
pub struct CookedLogin { ui: Ui, root: usize, status: Option<usize>, steps: Vec<String>, mats: Mats, missing: Vec<String> }
impl CookedLogin {
    pub fn load(store: Rc<Store>) -> Result<CookedLogin, String> {
        let mut ui = Ui::new(store);
        let root = ui.add_class_instance(LOGIN_SCREEN, None, "", json!({}), None).ok_or("cannot compose LoginScreen_C")?;
        let stack = var(&ui, root, "LoginFlowStack").ok_or("LoginScreen has no LoginFlowStack")?;
        // ShowStatusWindow("Logging In..."): Create(StatusWidget_C), SetTitleText, LoginFlowStack.PushWidget
        let status_w = ui.add_class_instance(STATUS_WIDGET, Some(stack), "PanelSlot", json!({}), None).ok_or("cannot compose StatusWidget_C")?;
        let status = var(&ui, status_w, "StatusText");
        let wm = var(&ui, root, "Widget_BuildWatermark").and_then(|b| var(&ui, b, "Text"));
        set_text(&mut ui, wm, ""); // GetWatermark: no net id -> empty
        ui.timed_mats.push(THROBBER.to_owned());
        let steps = LOGIN_STEPS.iter().map(|(ns, k, s)| super::item_header::loc(&ui.store, ns, k, s)).collect();
        Ok(CookedLogin { ui, root, status, steps, mats: HashMap::new(), missing: vec![] })
    }
    /// `t` = seconds the screen has been up (the throbber material's time); `step` = the running login step ([`LOGIN_STEPS`]: `GetLoginStatusText`).
    pub fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, t: f32, step: usize) -> RenderList {
        self.ui.mat_time = t;
        let s = self.status;
        let text = self.steps.get(step.min(self.steps.len().saturating_sub(1))).cloned().unwrap_or_default();
        set_text(&mut self.ui, s, &text);
        draw_tree(&mut self.ui, uic, &mut self.mats, self.root, w, h, &mut self.missing).0
    }
}

/// `SubgameSelect_C` with `SubgameSelectScreen_C` on its `MainContentStack`.
pub struct CookedPicker { ui: Ui, root: usize, campaign: usize, athena: usize, exit: usize, full_desc: Option<usize>, logo: Option<usize>, bg: Option<usize>, exit_text: String, mats: Mats, missing: Vec<String>, rects: Vec<[f32; 4]>, scale: f32 }
impl CookedPicker {
    pub fn load(store: Rc<Store>) -> Result<CookedPicker, String> {
        let mut ui = Ui::new(store);
        let root = ui.add_class_instance(SUBGAME_SELECT, None, "", json!({}), None).ok_or("cannot compose SubgameSelect_C")?;
        let stack = var(&ui, root, "MainContentStack").ok_or("SubgameSelect has no MainContentStack")?;
        let screen = ui.add_class_instance(SELECT_SCREEN, Some(stack), "PanelSlot", json!({}), None).ok_or("cannot compose SubgameSelectScreen_C")?;
        // UpdateHeaderAndFooter(true, true)
        let (top, bottom) = (var(&ui, root, "TopBar"), var(&ui, root, "BottomBar"));
        collapse(&mut ui, top); collapse(&mut ui, bottom);
        let wm = var(&ui, root, "BuildWatermark").and_then(|b| var(&ui, b, "Text"));
        set_text(&mut ui, wm, "");
        let campaign = var(&ui, screen, "CampaignBtn").ok_or("no CampaignBtn")?;
        let athena = var(&ui, screen, "AthenaBtn").ok_or("no AthenaBtn")?;
        let exit = var(&ui, screen, "ButtonExit").ok_or("no ButtonExit")?;
        // USER CHOICE (2026-10-07, "remove the save the world thing and add a exit"): the CampaignBtn row (its HorizontalBox) is collapsed, and the
        // ButtonExit row (collapsed on PC by its CommonVisibilityWidget, bShowForPC false) is shown with that widget's VisibleType.
        let row = ui.w[campaign].parent; collapse(&mut ui, row); // HorizontalBox_1: CampaignBtn + its "On sale!" / PvE tag
        let mut a = Some(exit);
        while let Some(i) = a { if ui.w[i].name == "VboxMainMenu" { break; } if ui.w[i].vis == Vis::Collapsed { ui.w[i].vis = Vis::SelfHitTestInvisible; } a = ui.w[i].parent; }
        // InitializeShortDescriptions: GetShortDescription(0 / 1) = the CMS `subgameselectdata` titles
        let (pve, pvp) = (var(&ui, screen, "PVEDescriptionText"), var(&ui, screen, "PVPDescriptionText"));
        set_text(&mut ui, pve, "");
        set_text(&mut ui, pvp, "");
        let exit_text = super::item_header::loc(&ui.store, EXIT_TEXT.0, EXIT_TEXT.1, EXIT_TEXT.2);
        let full_desc = var(&ui, screen, "FullDescription");
        let (logo, bg) = (var(&ui, screen, "TitleImage"), var(&ui, screen, "BGImage"));
        if let Some(b) = bg { ui.w[b].mat_scalars = vec![("bGameMode".into(), 0.0)]; }
        ui.mat_res.push((BG_MATERIAL.to_owned(), BG_RASTER));
        Ok(CookedPicker { ui, root, campaign, athena, exit, full_desc, logo, bg, exit_text, mats: HashMap::new(), missing: vec![], rects: vec![], scale: 1. })
    }
    /// `ConsoleProfileWidget.SetDisplayName(GetPlatformDisplayName())` (the signed-in account's display name).
    pub fn set_display_name(&mut self, name: &str) {
        let n = self.ui.w.iter().position(|w| w.name == "ConsoleProfileWidget").and_then(|c| var(&self.ui, c, "DisplayName"));
        set_text(&mut self.ui, n, name);
    }
    fn item_widget(&self, i: Item) -> usize { match i { Item::Campaign => self.campaign, Item::BattleRoyale => self.athena, Item::Quit => self.exit } }
    /// The picker state -> button states, `FullDescription`, the background's `bGameMode` (quantized to [`MODE_STEPS`] pre-rastered states), then
    /// layout and paint.
    pub fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, p: &Picker) -> RenderList {
        for i in [Item::Campaign, Item::BattleRoyale, Item::Quit] {
            let id = self.item_widget(i);
            let hovered = p.highlighted && p.focus == i;
            self.ui.w[id].btn_state = if p.pressed == Some(i) { BtnState::Pressed } else if hovered { BtnState::Hovered } else { BtnState::Normal };
        }
        // SetDescriptionForSubGame(n): GetFullDescription(n) false offline -> empty; the ButtonExit hover: "Exit Fortnite."
        let d = if p.description == 2 { self.exit_text.clone() } else { String::new() };
        if let Some(f) = self.full_desc { self.ui.w[f].text = Some(d); self.ui.w[f].text_style = Some(TEXT_STYLE_S.to_owned()); }
        let m = (p.game_mode.clamp(0., 1.) * MODE_STEPS as f32).round() / MODE_STEPS as f32;
        if let Some(b) = self.bg { self.ui.w[b].mat_scalars = vec![("bGameMode".into(), m)]; }
        self.scale = dpi_scale(w, h);
        let (list, rects) = draw_tree(&mut self.ui, uic, &mut self.mats, self.root, w, h, &mut self.missing);
        self.rects = rects;
        list
    }
    /// Rasters the background at every `bGameMode` step once (the CPU raster is slow; the switch animation then only picks textures).
    pub fn warm(&mut self, uic: &mut UiContext, w: f32, h: f32) { for k in 0..=MODE_STEPS { self.warm_step(uic, w, h, k); } }
    pub fn warm_step(&mut self, uic: &mut UiContext, w: f32, h: f32, k: usize) -> Option<bool> {
        if k > MODE_STEPS { return Some(true); }
        let mut p = Picker::new();
        p.game_mode = k as f32 / MODE_STEPS as f32;
        let _ = crate::screens::athena_lobby::paint::with_raster_budget(25, || self.draw(uic, w, h, &p));
        if crate::screens::athena_lobby::paint::raster_pending() { return None; }
        Some(k >= MODE_STEPS)
    }
    /// Pixel layout of the buttons after the last draw (the picker state machine's hit test).
    pub fn layout(&self, w: f32, h: f32) -> Layout {
        let s = self.scale;
        let px = |id: Option<usize>| id.and_then(|i| self.rects.get(i)).map(|r| Rect::new(r[0] * s, r[1] * s, r[2] * s, r[3] * s)).unwrap_or(Rect::new(0., 0., 0., 0.));
        let buttons = [Item::Campaign, Item::BattleRoyale, Item::Quit].into_iter().map(|i| (i, px(Some(self.item_widget(i))))).collect();
        Layout { scale: s, bg: px(self.bg).max_or(Rect::new(0., 0., w, h)), logo: px(self.logo), buttons, hint_baseline: h, header_px: 0, base_px: 0 }
    }
    pub fn laid_out(&self) -> bool { !self.rects.is_empty() }
}

pub const SIGN_IN_WIDGET: &str = "/Game/UI/Login/SignIn/SignInWidget.SignInWidget_C";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInHit { Email, Password, SignIn }

#[derive(Clone, Debug, Default)]
pub struct SignInState { pub email: String, pub password: String, pub focus: u8, pub caret: usize, pub button_enabled: bool, pub button_hovered: bool, pub button_pressed: bool, pub t: f32 }

pub struct CookedSignIn { ui: Ui, root: usize, email: Option<usize>, password: Option<usize>, email_border: Option<usize>, password_border: Option<usize>, button: Option<usize>, normal: Option<String>, highlight: Option<String>, mats: Mats, missing: Vec<String>, rects: Vec<[f32; 4]>, scale: f32 }
impl CookedSignIn {
    pub fn load(store: Rc<Store>) -> Result<CookedSignIn, String> {
        let mut ui = Ui::new(store);
        let root = ui.add_class_instance(LOGIN_SCREEN, None, "", json!({}), None).ok_or("cannot compose LoginScreen_C")?;
        let stack = var(&ui, root, "LoginFlowStack").ok_or("LoginScreen has no LoginFlowStack")?;
        let w = ui.add_class_instance(SIGN_IN_WIDGET, Some(stack), "PanelSlot", json!({}), None).ok_or("cannot compose SignInWidget_C")?;
        let wm = var(&ui, root, "Widget_BuildWatermark").and_then(|b| var(&ui, b, "Text"));
        set_text(&mut ui, wm, "");
        let (email, password) = (var(&ui, w, "Email"), var(&ui, w, "Password"));
        let (email_border, password_border) = (var(&ui, w, "EmailBorder"), var(&ui, w, "PasswordBorder"));
        let button = var(&ui, w, "SignInButton");
        let normal = ui.w[w].props.get("NormalBorderStyle").and_then(|v| v.as_str()).map(|s| s.to_owned());
        let highlight = ui.w[w].props.get("HighlightBorderStyle").and_then(|v| v.as_str()).map(|s| s.to_owned());
        let rows = ui.store.pkg("/Game/UI/Foundation/Input/InputActionDataTable").and_then(|p| p.named("InputActionDataTable").map(|e| e.props.clone())).unwrap_or(serde_json::Value::Null);
        let label = rows.get("Rows").and_then(|r| r.get("Login")).and_then(|r| r.get("DisplayName")).and_then(|t| ui.text_of(t)).unwrap_or_default();
        if let Some(b) = button { if let Some(tw) = ui.var(b, "CenterButtonTextWidget") { ui.w[tw].text = Some(label.clone()); ui.w[tw].vis = if label.is_empty() { Vis::Collapsed } else { Vis::SelfHitTestInvisible }; } }        Ok(CookedSignIn { ui, root, email, password, email_border, password_border, button, normal, highlight, mats: HashMap::new(), missing: vec![], rects: vec![], scale: 1. })
    }
    pub fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, st: &SignInState) -> RenderList {
        if let Some(e) = self.email { self.ui.w[e].text = Some(st.email.clone()); }
        if let Some(p) = self.password { self.ui.w[p].text = Some(st.password.clone()); }
        for (b, focused) in [(self.email_border, st.focus == 0), (self.password_border, st.focus == 1)] {
            let style = if focused { self.highlight.clone() } else { self.normal.clone() };
            if let (Some(b), Some(s)) = (b, style) { self.ui.w[b].props["Style"] = json!(s); }
        }
        if let Some(b) = self.button {
            self.ui.w[b].enabled = st.button_enabled;
            self.ui.w[b].btn_state = if !st.button_enabled { BtnState::Disabled } else if st.button_pressed { BtnState::Pressed } else if st.button_hovered { BtnState::Hovered } else { BtnState::Normal };
            self.ui.refresh_button_text(b);
        }
        self.scale = dpi_scale(w, h);
        let (mut list, rects) = draw_tree(&mut self.ui, uic, &mut self.mats, self.root, w, h, &mut self.missing);
        self.rects = rects;
        let field = if st.focus == 0 { self.email } else { self.password };
        if let Some(f) = field.filter(|_| (st.t * 2.0).fract() < 0.5) {
            if let Some(r) = self.rects.get(f).copied() {
                let s = self.scale;
                let shown: String = if st.focus == 0 { st.email.chars().take(st.caret).collect() } else { "•".repeat(st.caret) };
                let font = crate::text::FontId::Regular;
                let px = crate::screens::atlas_px(20.0, s);
                let x = r[0] * s + uic.fonts.measure(font, &shown, px as f32);
                list.fill(Rect::new(x, r[1] * s, (1.0 * s).max(1.0), r[3] * s), [0.0, 0.0, 0.0, 1.0]);
            }
        }
        list
    }
    pub fn hit(&self, x: f32, y: f32) -> Option<SignInHit> {
        let s = self.scale;
        let inside = |id: Option<usize>| id.and_then(|i| self.rects.get(i)).map_or(false, |r| x >= r[0] * s && y >= r[1] * s && x < (r[0] + r[2]) * s && y < (r[1] + r[3]) * s);
        if inside(self.button) { Some(SignInHit::SignIn) } else if inside(self.email_border) { Some(SignInHit::Email) } else if inside(self.password_border) { Some(SignInHit::Password) } else { None }
    }
}

trait MaxOr { fn max_or(self, d: Rect) -> Rect; }
impl MaxOr for Rect { fn max_or(self, d: Rect) -> Rect { if self.w > 0. && self.h > 0. { self } else { d } } }

/// Both screens, composed from one store (opened once; a failed load leaves the old screens in place).
pub struct FrontEnd { pub login: Option<CookedLogin>, pub picker: Option<CookedPicker>, pub sign_in: Option<CookedSignIn>, pub note: Option<String> }
fn mats_bytes(m: &Mats) -> usize { m.values().flatten().filter(|r| Rc::strong_count(r) == 1).map(|r| r.tex.iter().flatten().map(|t| t.rgba.len()).sum::<usize>()).sum() }
impl FrontEnd {
    pub fn release_materials(&mut self) -> (usize, usize) {
        let mut n = 0;
        let mut bytes = 0;
        for m in [self.login.as_mut().map(|c| &mut c.mats), self.picker.as_mut().map(|c| &mut c.mats), self.sign_in.as_mut().map(|c| &mut c.mats)].into_iter().flatten() { n += m.len(); bytes += mats_bytes(m); m.clear(); }
        (n, bytes)
    }
    pub fn load(install: &Path) -> FrontEnd {
        let store = match Store::open(install) { Ok(s) => Rc::new(s), Err(e) => return FrontEnd { login: None, picker: None, sign_in: None, note: Some(e) } };
        let mut note = None;
        let login = CookedLogin::load(store.clone()).map_err(|e| note = Some(e)).ok();
        let picker = CookedPicker::load(store.clone()).map_err(|e| note = Some(e)).ok();
        let sign_in = CookedSignIn::load(store).map_err(|e| note = Some(e)).ok();
        FrontEnd { login, picker, sign_in, note }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn front_end_composes_from_the_cooked_trees() {
        let install = crate::pak_assets::default_install();
        if !install.exists() { eprintln!("no install: skipped"); return; }
        let fe = FrontEnd::load(&install);
        let (Some(mut login), Some(mut picker)) = (fe.login, fe.picker) else { panic!("{:?}", fe.note) };
        let mut uic = UiContext::from_install(&install, None).expect("ui context");
        let l = login.draw(&mut uic, 1920., 1080., 0.5, 1);
        assert!(!l.cmds.is_empty());
        let p = Picker::new();
        let _ = picker.draw(&mut uic, 1920., 1080., &p);
        let lay = picker.layout(1920., 1080.);
        let (stw, br, quit) = (lay.rect(Item::Campaign).unwrap(), lay.rect(Item::BattleRoyale).unwrap(), lay.rect(Item::Quit).unwrap());
        assert!(br.w > 100. && br.h > 30., "{br:?}");
        assert!(stw.w == 0. && stw.h == 0., "Save the World is removed: {stw:?}");
        assert!(quit.w > 50. && quit.y > br.y, "QUIT is shown under BATTLE ROYALE: {quit:?} {br:?}");
    }
}
