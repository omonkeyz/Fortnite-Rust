//! UMG widget tree composition: the runtime widget hierarchy of a Blueprint user widget (its cooked `WidgetArchetype` tree, nested user widgets
//! expanded through the trees the package serialized for them, classes created natively expanded through their own package), plus the
//! small set of Blueprint behaviours that decide what the *settled* screen shows (decoded from the Kismet of each class; see UI.md).
use super::store::{jenum, jf, jmargin, jvec, split_object_path, Exp, Pkg, Store};
use super::styles::{Brush, BtnState, ButtonStyle, DrawAs, StyleCache, TextStyle};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis { Visible, Collapsed, Hidden, HitTestInvisible, SelfHitTestInvisible }
impl Vis {
    pub fn parse(s: &str) -> Vis {
        match s.rsplit("::").next().unwrap_or(s) {
            "Collapsed" => Vis::Collapsed, "Hidden" => Vis::Hidden, "HitTestInvisible" => Vis::HitTestInvisible,
            "SelfHitTestInvisible" => Vis::SelfHitTestInvisible, _ => Vis::Visible,
        }
    }
    pub fn takes_space(self) -> bool { self != Vis::Collapsed }
    pub fn draws(self) -> bool { !matches!(self, Vis::Collapsed | Vis::Hidden) }
}

#[derive(Clone)]
pub struct Widget {
    pub id: usize,
    pub parent: Option<usize>,
    pub class: String,
    pub name: String,
    pub path: String,
    pub props: Value,
    pub slot_class: String,
    pub slot: Value,
    pub kids: Vec<usize>,
    pub pkg: Rc<Pkg>,
    /// `Some` for Blueprint user widgets (`*_C`).
    pub user: Option<User>,
    /// nearest enclosing user widget (whose Blueprint variables name this widget)
    pub owner: Option<usize>,
    pub vis: Vis,
    // ---- values the Blueprint logic of the owning user widget sets on this widget ----
    pub text: Option<String>,
    pub text_style: Option<String>,
    pub brush: Option<Brush>,
    pub btn_state: BtnState,
    /// `UWidget::bIsEnabled` of this widget itself (serialized only when false); Slate paints a disabled widget and everything under it with the disabled draw effect
    pub enabled: bool,
    /// `UWidget::RenderTransform` (translation, scale, shear, angle degrees)
    pub transform: Transform,
    pub desired_scale: [f32; 2],
    /// scalar parameters the Blueprint set on this image's dynamic material (`SetScalarParameterValue`, e.g. `fillPercent` of the settings slider)
    pub mat_scalars: Vec<(String, f32)>,
    /// INVENTORY FIX2: vector parameters the Blueprint set on this border's / image's dynamic material (`GetDynamicMaterial` + `SetVectorParameterValue`, e.g.
    /// `ItemDetailsHeader.UpdateItemsForWidgets`: `RarityBorderMID` "Rarity 2..4", `Level Bar MID` RGBA0 / RGBA1)
    pub mat_vectors: Vec<(String, [f32; 4])>,
    pub mat_textures: Vec<(String, String)>,
    pub slot_halign: Option<String>,
    pub slot_valign: Option<String>,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct Transform { pub translation: [f32; 2], pub scale: [f32; 2], pub angle: f32 }
impl Transform {
    pub fn identity() -> Transform { Transform { translation: [0.0; 2], scale: [1.0; 2], angle: 0.0 } }
    pub fn is_identity(&self) -> bool { self.translation == [0.0; 2] && self.scale == [1.0; 2] && self.angle == 0.0 }
    pub fn from_json(v: Option<&Value>) -> Transform {
        let mut t = Transform::identity();
        if let Some(v) = v {
            if let Some(a) = jvec::<2>(v.get("Translation")) { t.translation = a; }
            if let Some(a) = jvec::<2>(v.get("Scale")) { t.scale = a; }
            if let Some(a) = v.get("Angle").and_then(jf) { t.angle = a; }
        }
        t
    }
}

#[derive(Clone)]
pub struct User {
    /// `/Game/..X.X_C` (None when the class package could not be located)
    pub class_path: Option<String>,
    pub chain: Vec<String>,
    /// path prefix (with trailing `:`) of the widgets of this user widget's own tree
    pub tree_prefix: String,
}

pub struct Ui {
    pub store: Rc<Store>,
    pub w: Vec<Widget>,
    pub by_path: HashMap<String, Vec<usize>>,
    pub styles: StyleCache,
    class_paths: HashMap<String, Option<String>>,
    pub log: Vec<String>,
    /// locres lookups that failed (text shown empty)
    pub unresolved: std::cell::RefCell<Vec<String>>,
    /// scalar parameters of the image being painted (set by the painter around `draw_brush`)
    pub cur_scalars: Vec<(String, f32)>,
    /// INVENTORY FIX2: vector parameters of the border / image being painted (`Widget::mat_vectors`)
    pub cur_vectors: Vec<(String, [f32; 4])>,
    pub cur_textures: Vec<(String, String)>,
    /// FRONT END: material time (seconds) for the animated material brushes in `timed_mats` (re-rastered every paint, e.g. the login throbber)
    pub mat_time: f32,
    pub timed_mats: Vec<String>,
    /// FRONT END: texture object path -> replacement path for the material brushes (BR only: a Save the World texture a material samples at weight 0)
    pub tex_subst: HashMap<String, String>,
    /// FRONT END: material brushes rastered at a fraction of their pixel size (drawn stretched; the CPU raster of a full-screen material is slow)
    pub mat_res: Vec<(String, f32)>,
}

/// The 1.8 `ESubGame::Athena`-time state the screens are built for (see `settled` notes in UI.md).
#[derive(Clone, Debug)]
pub struct Settled {
    pub selected_tab: usize,
}

pub fn is_styled_common_text(class: &str, props: &Value) -> bool {
    matches!(class, "CommonTextBlock" | "CommonNumericTextBlock") && props.get("Style").and_then(|s| s.as_str()).map_or(false, |s| !s.is_empty())
}
fn exp_vis(props: &Value) -> Vis { props.get("Visibility").and_then(|v| v.as_str()).map(Vis::parse).unwrap_or(Vis::Visible) }

impl Ui {
    pub fn new(store: Rc<Store>) -> Ui {
        Ui { store, w: vec![], by_path: HashMap::new(), styles: StyleCache::default(), class_paths: HashMap::new(), log: vec![], unresolved: Default::default(), cur_scalars: vec![], cur_vectors: vec![], cur_textures: vec![], mat_time: 0.0, timed_mats: vec![], tex_subst: HashMap::new(), mat_res: vec![] }
    }

    /// `/Game/..` class path of a Blueprint class name (`IconTabButton_C`): the first package of that stem under /Game.
    pub fn find_class_path(&mut self, class: &str) -> Option<String> {
        if let Some(c) = self.class_paths.get(class) { return c.clone(); }
        let base = class.strip_suffix("_C").unwrap_or(class);
        let mut found: Option<String> = None;
        for (_, e) in self.store.pak_set().by_file_name(&format!("{base}.uasset")) {
            if let Some(g) = fn_pak::pak_path_to_game(e.rel_path()).or_else(|| fn_pak::pak_path_to_game(&e.path)) {
                let long = g.trim_end_matches(".uasset").to_owned();
                if long.starts_with("/Game/") && long.rsplit('/').next() == Some(base) { found = Some(format!("{long}.{class}")); break; }
            }
        }
        self.class_paths.insert(class.to_owned(), found.clone());
        found
    }

    pub fn text_of(&self, v: &Value) -> Option<String> {
        let o = v.as_object()?;
        // the editor placeholder of an unset text block is not content: such text widgets are filled at runtime (names, counts)
        if o.get("key").and_then(|t| t.as_str()) == Some("TextBlockDefaultValue") && o.get("namespace").and_then(|t| t.as_str()) == Some("UMG") { return Some(String::new()); }
        let text = o.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let key = o.get("key").and_then(|t| t.as_str()).unwrap_or("");
        let ns = o.get("namespace").and_then(|t| t.as_str()).unwrap_or("");
        // an FText without a key is a literal (not localized): its text is the value. Keyed texts come from Game.locres ONLY: no fallback to the source
        // string; a failed lookup shows nothing and is reported through `Ui::unresolved`.
        if key.is_empty() { return Some(text.to_owned()); }
        match self.store.locres().as_ref().and_then(|l| l.get(ns, key).map(|s| s.to_owned())) {
            Some(s) => Some(s),
            None => { let m = format!("locres miss: {ns} / {key} (source \"{text}\")"); let mut u = self.unresolved.borrow_mut(); if !u.contains(&m) { u.push(m); } Some(String::new()) }
        }
    }

    fn push(&mut self, pkg: &Rc<Pkg>, e: &Exp, parent: Option<usize>, owner: Option<usize>, slot_class: String, slot: Value) -> usize {
        let id = self.w.len();
        let mut props = e.props.clone();
        if !props.is_object() { props = json!({}); }
        let mut vis = exp_vis(&props);
        // FRONT END: UCommonVisibilityWidget (ctor exe 0x143929cb0: VisibleType SelfHitTestInvisible, HiddenType Collapsed; update 0x14395ae10) on PC:
        // hidden when its serialized bShowForPC is false (the 1.8 picker's QUIT button, absent in the live PC client)
        if e.class == "CommonVisibilityWidget" && props.get("bShowForPC").and_then(|b| b.as_bool()) == Some(false) { vis = Vis::Collapsed; }
        if is_styled_common_text(&e.class, &props) { if let Some(m) = props.as_object_mut() { m.remove("ColorAndOpacity"); } }
        let transform = Transform::from_json(props.get("RenderTransform"));
        let ds = jvec::<2>(props.get("DesiredSizeScale")).unwrap_or([1.0, 1.0]);
        let enabled = props.get("bIsEnabled").and_then(|b| b.as_bool()).unwrap_or(true);
        self.w.push(Widget {
            id, parent, class: e.class.clone(), name: e.name.clone(), path: e.path.clone(), props, slot_class, slot, kids: vec![], pkg: pkg.clone(),
            user: None, owner, vis, text: None, text_style: None, brush: None, btn_state: BtnState::Normal, enabled, transform, desired_scale: ds, mat_scalars: vec![], mat_vectors: vec![], mat_textures: vec![],
            slot_halign: None, slot_valign: None, note: String::new(),
        });
        self.by_path.entry(e.path.clone()).or_default().push(id);
        if let Some(p) = parent { self.w[p].kids.push(id); }
        id
    }

    fn slot_of(&self, pkg: &Pkg, e: &Exp) -> (String, Value) {
        match e.props.get("Slot").and_then(|s| s.as_str()).and_then(|s| pkg.get(s)) {
            Some(s) => (s.class.clone(), if s.props.is_object() { s.props.clone() } else { json!({}) }),
            None => (String::new(), json!({})),
        }
    }

    /// Owner of a widget at `path`: the nearest user widget on the ancestor chain of `parent` whose own tree contains `path`.
    fn owner_for(&self, parent: Option<usize>, path: &str) -> Option<usize> {
        let mut cur = parent;
        while let Some(p) = cur {
            if let Some(u) = &self.w[p].user { if path.starts_with(&u.tree_prefix) { return Some(p); } }
            cur = self.w[p].parent;
        }
        None
    }

    /// Compose a user widget created from its class (the way `CreateWidget` / `FortTabListWidgetBase` builds it).
    pub fn add_class_instance(&mut self, class_path: &str, parent: Option<usize>, slot_class: &str, slot: Value, inst_props: Option<Value>) -> Option<usize> {
        let (long, cname) = split_object_path(class_path);
        let pkg = self.store.pkg(long)?;
        let arch_path = format!("{long}.WidgetArchetype");
        let arch = pkg.get(&arch_path).or_else(|| pkg.named("WidgetArchetype"))?;
        let tree = pkg.get(&format!("{long}.WidgetArchetype:WidgetTree_0"))?;
        let root_path = tree.props.get("RootWidget")?.as_str()?.to_owned();
        let mut e = Exp { name: cname.trim_end_matches("_C").to_owned(), class: cname.to_owned(), path: arch.path.clone(), outer: String::new(), super_path: String::new(), props: arch.props.clone() };
        // class defaults under the archetype's own variable table / instance values
        let mut merged = self.store.class_defaults(class_path);
        if let (Some(m), Some(a)) = (merged.as_object_mut(), arch.props.as_object()) { for (k, v) in a { m.insert(k.clone(), v.clone()); } }
        if let (Some(m), Some(a)) = (merged.as_object_mut(), inst_props.as_ref().and_then(|x| x.as_object())) { for (k, v) in a { m.insert(k.clone(), v.clone()); } }
        e.props = merged;
        let owner = self.owner_for(parent, &e.path);
        let id = self.push(&pkg, &e, parent, owner, slot_class.to_owned(), slot);
        self.w[id].user = Some(User { class_path: Some(class_path.to_owned()), chain: self.store.class_chain(class_path), tree_prefix: format!("{}:WidgetTree_0:", arch_path) });
        self.build(&pkg, &root_path, Some(id), String::new(), json!({}));
        self.hooks(id);
        Some(id)
    }

    pub fn clone_subtree(&mut self, src: usize, parent: usize, name: &str) -> usize {
        let mut order = vec![src];
        let mut i = 0;
        while i < order.len() { let k = self.w[order[i]].kids.clone(); order.extend(k); i += 1; }
        let base = self.w.len();
        let map: HashMap<usize, usize> = order.iter().enumerate().map(|(k, &o)| (o, base + k)).collect();
        for &o in &order {
            let mut c = self.w[o].clone();
            c.id = map[&o];
            c.parent = if o == src { Some(parent) } else { c.parent.map(|p| map.get(&p).copied().unwrap_or(p)) };
            c.owner = c.owner.map(|p| map.get(&p).copied().unwrap_or(p));
            c.kids = c.kids.iter().map(|k| map[k]).collect();
            if o == src { c.name = name.to_owned(); }
            self.by_path.entry(c.path.clone()).or_default().push(c.id);
            self.w.push(c);
        }
        self.w[parent].kids.push(base);
        base
    }

    /// Compose the widget at `path` (and its subtree) under `parent`.
    fn build(&mut self, pkg: &Rc<Pkg>, path: &str, parent: Option<usize>, slot_class_override: String, slot_override: Value) -> Option<usize> {
        let e = match pkg.get(path) { Some(e) => e, None => { self.log.push(format!("missing export {path}")); return None } };
        let (mut slot_class, mut slot) = self.slot_of(pkg, e);
        if !slot_class_override.is_empty() { slot_class = slot_class_override; slot = slot_override; }
        let owner = self.owner_for(parent, path);
        let id = self.push(pkg, e, parent, owner, slot_class, slot);
        let class = e.class.clone();
        if class.ends_with("_C") {
            // user widget instance: nested tree serialized in this package, else the class' own package
            let nested = pkg.children_of(path).find(|c| c.class == "WidgetTree" && c.props.get("RootWidget").is_some()).map(|c| (c.path.clone(), c.props.get("RootWidget").and_then(|r| r.as_str()).unwrap_or("").to_owned()));
            let class_path = self.find_class_path(&class);
            let mut chain = vec![];
            if let Some(cp) = &class_path {
                chain = self.store.class_chain(cp);
                let mut merged = self.store.class_defaults(cp);
                if let (Some(m), Some(a)) = (merged.as_object_mut(), self.w[id].props.as_object()) { for (k, v) in a { m.insert(k.clone(), v.clone()); } }
                self.w[id].props = merged;
                self.w[id].vis = exp_vis(&self.w[id].props);
            }
            if let Some((tree_path, root)) = nested {
                self.w[id].user = Some(User { class_path, chain, tree_prefix: format!("{tree_path}:") });
                self.build(pkg, &root, Some(id), String::new(), json!({}));
            } else if let Some(cp) = class_path {
                // class created from its own package: reuse the class loader but keep this export's slot / properties
                let (long, _) = split_object_path(&cp);
                let long = long.to_owned();
                if let Some(cpkg) = self.store.pkg(&long) {
                    let arch = format!("{long}.WidgetArchetype");
                    if let Some(root) = cpkg.get(&format!("{arch}:WidgetTree_0")).and_then(|t| t.props.get("RootWidget")).and_then(|r| r.as_str()) {
                        let root = root.to_owned();
                        if let Some(a) = cpkg.get(&arch) { if let (Some(m), Some(src)) = (self.w[id].props.as_object_mut(), a.props.as_object()) { for (k, v) in src { m.entry(k.clone()).or_insert(v.clone()); } } }
                        self.w[id].user = Some(User { class_path: Some(cp.clone()), chain, tree_prefix: format!("{arch}:WidgetTree_0:") });
                        self.build(&cpkg, &root, Some(id), String::new(), json!({}));
                    }
                }
            } else {
                self.w[id].user = Some(User { class_path: None, chain, tree_prefix: format!("{path}:") });
                self.log.push(format!("user widget {class} at {path}: no nested tree and class not found"));
            }
        } else {
            // native panel: children through the slots, else the single `Content`
            let mut kids: Vec<String> = vec![];
            if let Some(slots) = e.props.get("Slots").and_then(|s| s.as_array()) {
                for s in slots { if let Some(c) = s.as_str().and_then(|p| pkg.get(p)).and_then(|se| se.props.get("Content")).and_then(|c| c.as_str()) { kids.push(c.to_owned()); } }
            }
            for k in kids { self.build(pkg, &k, Some(id), String::new(), json!({})); }
        }
        if self.w[id].user.is_some() { self.hooks(id); }
        Some(id)
    }

    // ---------------------------------------------------------------------------------------------------------------------------------------
    // lookups

    pub fn var(&self, owner: usize, name: &str) -> Option<usize> {
        let p = self.w[owner].props.get(name)?.as_str()?;
        let ids = self.by_path.get(p)?;
        // several instances of the same class tree share paths: pick the one owned by `owner`
        ids.iter().copied().find(|&i| self.is_descendant(i, owner)).or_else(|| ids.first().copied())
    }
    pub fn is_descendant(&self, mut id: usize, anc: usize) -> bool {
        loop { if id == anc { return true; } match self.w[id].parent { Some(p) => id = p, None => return false } }
    }
    pub fn find_by_name(&self, name: &str) -> Option<usize> { self.w.iter().find(|w| w.name == name).map(|w| w.id) }
    pub fn find_in(&self, root: usize, name: &str) -> Option<usize> { self.w.iter().find(|w| w.name == name && self.is_descendant(w.id, root)).map(|w| w.id) }
    pub fn prop(&self, id: usize, key: &str) -> Option<&Value> { self.w[id].props.get(key) }
    pub fn chain_has(&self, id: usize, class: &str) -> bool { self.w[id].user.as_ref().map_or(false, |u| u.chain.iter().any(|c| c == class)) }

    pub fn button_style_class(&self, id: usize) -> Option<String> {
        // INVENTORY FIX3 (inventory_gaps G5): a `UCommonButton` whose Style is not serialized anywhere (instance or Blueprint CDO chain) keeps its native
        // constructor value: `Style = UCommonUISettings CDO->DefaultButtonStyle` (+0x290 <- settings +0x50, 1.8 exe 0x143928dd0; SDK `UCommonUISettings`),
        // ini `/Script/CommonUI.CommonUISettings:DefaultButtonStyle_StringRef = ButtonStyle-Primary-M` (CookedIniVersion.txt line 3979). Only when the
        // nearest native class is `UCommonButton` itself (a native subclass may set its own Style in its constructor).
        // INVENTORY FIX4 (inventory_gaps verify3 V3): `UCommonRotator` too: its ctor 0x1439298e0 chains to the CommonButton ctor 0x143928dd0 and does not
        // write +0x290, so an unstyled rotator also inherits the settings' DefaultButtonStyle.
        match self.w[id].props.get("Style") {
            Some(s) => s.as_str().filter(|s| !s.is_empty()).map(|s| s.to_owned()),
            None => {
                let native = self.w[id].user.as_ref().and_then(|u| u.chain.iter().find(|c| !c.starts_with("/Game/")).cloned());
                matches!(native.as_deref(), Some("/Script/CommonUI.CommonButton" | "/Script/CommonUI.CommonRotator")).then(|| "/Game/UI/Foundation/Buttons/ButtonStyles/Primary/ButtonStyle-Primary-M.ButtonStyle-Primary-M_C".to_owned())
            }
        }
    }
    pub fn button_style(&mut self, id: usize) -> Option<ButtonStyle> {
        let cls = self.button_style_class(id)?;
        let store = self.store.clone();
        Some(self.styles.button(&store, &cls))
    }
    pub fn is_common_button(&self, id: usize) -> bool {
        self.chain_has(id, "/Script/CommonUI.CommonButton") || self.chain_has(id, "/Script/CommonUI.CommonRotator") || self.chain_has(id, "/Script/FortniteUI.FortPartyTreeItemBase")
    }

    // ---------------------------------------------------------------------------------------------------------------------------------------
    // Blueprint behaviours (settled state)

    fn set_vis(&mut self, id: Option<usize>, v: Vis) { if let Some(i) = id { self.w[i].vis = v; } }
    pub fn set_visibility(&mut self, id: Option<usize>, v: Vis) { self.set_vis(id, v) }

    fn hooks(&mut self, id: usize) {
        let class = self.w[id].class.clone();
        match class.as_str() {
            "IconTextButton_C" => self.hook_icon_text_button(id),
            "NormalBangWrapper_C" => {
                // `Update Bang State(false)`: no bang is shown (the counter badges are driven by the account's unclaimed items)
                if let Some(ms) = self.find_in(id, "ModeSwitcher") { self.w[ms].vis = Vis::Collapsed; self.w[ms].note = "no bang (runtime)".into(); }
                if let Some(tb) = self.find_in(id, "TutorialBorder") { self.w[tb].vis = Vis::Collapsed; }
            }
            "IconTabButton_C" => self.hook_icon_tab_button(id),
            "TopBar_C" => {
                // `TopBar::ConfigureVisibleItemsForSubGame` for ESubGame::Athena (the branch the Kismet takes for byte 1): SetVisibility(1 = Collapsed) on
                // TopBarSkillPoints, MiniPartyBar, PowerWidget and BRCurrency; FortniteLogo gets SetVisibility(4 = SelfHitTestInvisible)
                for n in ["TopBarSkillPoints", "MiniPartyBar", "PowerWidget", "BRCurrency"] { let v = self.var(id, n); self.set_vis(v, Vis::Collapsed); }
                let v = self.var(id, "FortniteLogo"); self.set_vis(v, Vis::SelfHitTestInvisible);
                // `Construct` -> `ForceMenuClosed` -> `CloseMainMenu`: the MainMenu panel is folded away (IntroMainMenu animates MainMenu.Border_0.DesiredSizeScale
                // from (1, 0) to (1, 1); the closed state is the first key)
                if let Some(mm) = self.var(id, "MainMenu") { if let Some(b) = self.find_in(mm, "Border_0") { self.w[b].desired_scale = [1.0, 0.0]; self.w[b].note = "menu closed (IntroMainMenu t=0)".into(); } }
            }
            "DailyQuestsPanel_C" => {
                // `OnQuestsUpdated` shows `ClaimButton` only while a quest reward can be claimed; the quest list is built from the account's quests
                let v = self.var(id, "ClaimButton"); self.set_vis(v, Vis::Collapsed);
                if let Some(v) = v { self.w[v].note = "no claimable quest (runtime)".into(); }
            }
            "MTXButton_C" => {
                // `MTXButton::UpdateState` (HidePlusButton = false, not in a zone, mouse + keyboard): `Button.SetStyle(OutlineEmptyFill)` then
                // `Button.SetControllerStyle(OutlineEmptyFill)`. `IconTextButton::SetControllerStyle` (Kismet) only stores `ControllerInputStyle` and calls
                // `UpdateStyle(IsUsingGamepad())`, which for mouse + keyboard runs `SetStyle(MouseKeyboardStyle)`; `MouseKeyboardStyle` was captured from the
                // serialized `Style` by `SetInitialMouseKeyboardStyle` in the button's `Construct` (before the parent's `Construct` -> `UpdateState`). Net result:
                // the button keeps its serialized `ButtonStyle-OutlineRightShade` (the shaded "+" section); EmptyFill is only the gamepad style.
                if let Some(b) = self.var(id, "Button") {
                    self.w[b].props["ControllerInputStyle"] = json!("/Game/UI/Foundation/Buttons/ButtonStyles/TopBar/ButtonStyle-OutlineEmptyFill.ButtonStyle-OutlineEmptyFill_C");
                    self.refresh_button_text(b);
                }
                let v = self.var(id, "TextPlus"); self.set_vis(v, Vis::SelfHitTestInvisible);
            }
            "LobbyTimer_C" => {
                // Construct: `LobbyTimerBorder.SetVisibility(2 = Hidden)` until a lobby timer runs
                if let Some(b) = self.var(id, "LobbyTimerBorder") { self.w[b].vis = Vis::Hidden; }
            }
            "ChatWidget_C" => {
                // ChatWidget::InitializeChat (Kismet): `Set Chat Shortcut Visibility(IsUsingGamepad ? 4 : 2)` sets CursorModeSwitcher to Hidden for keyboard + mouse,
                // and `SetActiveWidget(IsInZone && !IsInCursorMode ? HudContentHBox : ConsoleChatShortcut)`: the lobby is not in a zone -> ConsoleChatShortcut.
                if let Some(sw) = self.var(id, "CursorModeSwitcher") {
                    self.w[sw].vis = Vis::Hidden;
                    if let Some(c) = self.var(id, "ConsoleChatShortcut") { if let Some(i) = self.w[sw].kids.iter().position(|&k| k == c) { self.w[sw].props["ActiveWidgetIndex"] = json!(i); } }
                }
            }
            "LobbyPlayerPadTop_C" => self.hook_pad_top(id, None, false, false),
            "AthenaRotator_C" => self.hook_rotator(id),            "AthenaMatchmakingWidget_C" => self.hook_matchmaking(id),
            _ => {}
        }
    }

    /// `IconTextButton` (Kismet: `Set Text`, `UpdateTextStyle`, `InitializeButton`, `UpdateContentAlignment`, `Construct`).
    fn hook_icon_text_button(&mut self, id: usize) {
        let text_w = self.var(id, "CenterButtonTextWidget");
        let action = self.w[id].props.get("TriggeringInputAction").and_then(|t| t.get("RowName")).and_then(|r| r.as_str()).map(|r| r.to_owned());
        let action_text = action.and_then(|row| self.store.pkg("/Game/UI/Foundation/Input/InputActionDataTable").and_then(|p| p.named("InputActionDataTable").and_then(|e| e.props.get("Rows").and_then(|r| r.get(&row)).and_then(|r| r.get("DisplayName")).cloned()))).and_then(|t| self.text_of(&t)).filter(|t| !t.is_empty());
        let btn_text = action_text.unwrap_or_else(|| self.w[id].props.get("ButtonText").and_then(|t| self.text_of(t)).unwrap_or_default());
        if let Some(t) = text_w {
            self.w[t].text = Some(btn_text.clone());
            self.w[t].vis = if btn_text.is_empty() { Vis::Collapsed } else { Vis::SelfHitTestInvisible };
        }
        // text style of the button's current state (Normal)
        if let (Some(t), Some(st)) = (text_w, self.button_style(id)) {
            if let Some(cls) = st.text_style(BtnState::Normal) { self.w[t].text_style = Some(cls.to_owned()); }
        }
        // LeftSideImage <- IconBrush; collapsed when no icon
        let icon = self.w[id].props.get("IconBrush").and_then(Brush::from_json);
        if let Some(img) = self.var(id, "LeftSideImage") {
            match icon {
                Some(b) if b.draw != DrawAs::NoDraw && b.resource.is_some() => { self.w[img].brush = Some(b); self.w[img].vis = Vis::Visible; }
                _ => self.w[img].vis = Vis::Collapsed,
            }
        }
        // the key prompt (CommonActionWidget) needs an input binding; the settled keyboard/mouse screen shows none
        if let Some(a) = self.var(id, "InputActionWidget") { self.w[a].vis = Vis::Collapsed; self.w[a].note = "input prompt (runtime)".into(); }
        if let Some(n) = self.var(id, "RightExtraContentSlot") {
            let has = !self.w[n].kids.is_empty();
            self.w[n].vis = if has { Vis::SelfHitTestInvisible } else { Vis::Collapsed };
        }
        // ContentBorder content slot alignment <- ContentAlignment (BP variable; absent = EHorizontalAlignment 0 = Fill)
        let align = self.w[id].props.get("ContentAlignment").and_then(|v| v.as_str()).unwrap_or("HAlign_Fill").to_owned();
        if let Some(cb) = self.var(id, "ContentBorder") {
            if let Some(&k) = self.w[cb].kids.first() { self.w[k].slot_halign = Some(align); }
        }
        // `bIsEnabled` etc. stay at their defaults
    }

    /// Re-apply the text style of the button's current state to its text widget (`GetCurrentTextStyleClass` -> `CenterButtonTextWidget.SetStyle`).
    pub fn refresh_button_text(&mut self, id: usize) {
        let text_w = self.var(id, "CenterButtonTextWidget");
        let state = self.w[id].btn_state;
        if let (Some(t), Some(st)) = (text_w, self.button_style(id)) {
            if let Some(cls) = st.text_style(state) { self.w[t].text_style = Some(cls.to_owned()); }
        }
    }

    /// `IconTabButton` (Kismet `SetTabLabelInfo` -> `Set Text(DisplayName)`, `Set Icon`, `Update Bang State(false)`).
    fn hook_icon_tab_button(&mut self, id: usize) {
        let text_w = self.var(id, "CenterButtonTextWidget");
        let label = self.w[id].props.get("ButtonText").and_then(|t| self.text_of(t)).unwrap_or_default();
        if let Some(t) = text_w {
            self.w[t].text = Some(label.clone());
            self.w[t].vis = if label.is_empty() { Vis::Collapsed } else { Vis::SelfHitTestInvisible };
        }
        // `Set Icon(IconBrush)`: the registrations carry no icon (DrawAs NoDrawType) -> `LeftSideImage` collapsed
        if let Some(img) = self.var(id, "LeftSideImage") { self.w[img].vis = Vis::Collapsed; }
        self.refresh_button_text(id);
    }

    /// `AthenaRotator` (CommonRotator): MyText shows the selected label; the right button is the mirrored left one (BP `Construct` branch for the
    /// mouse / keyboard path: `ButtonRight.SetRenderTransform(Scale(-1, 1))`).
    fn hook_rotator(&mut self, id: usize) {
        if let Some(r) = self.var(id, "ButtonRight") { self.w[r].transform = Transform { translation: [0.0; 2], scale: [-1.0, 1.0], angle: 0.0 }; self.w[r].vis = Vis::Visible; }
        if let Some(l) = self.var(id, "ButtonLeft") { self.w[l].vis = Vis::Visible; }
    }

    /// `AthenaMatchmakingWidget` (BP `SetPlayButtonState` / `SetCancelButtonState`, native widget visibility): idle state.
    fn hook_matchmaking(&mut self, id: usize) {
        // idle: the matchmaking spinner / message block is not shown and the cancel button is not on top of the play button. NOT backed by Kismet: the native\n        // FortAthenaMatchmakingWidget owns these widgets (names registered by the class at exe RVA 0x31e24b0); inferred, flagged in UI.md.
        self.set_vis(self.var(id, "SpinnerAndTextContainer"), Vis::Collapsed);
        self.set_vis(self.var(id, "CancelButton"), Vis::Collapsed);
    }

    /// `LobbyPlayerPadTop::Refresh` for `ESubGame::Athena` (the Kismet branch taken for byte 1) + `RefreshPlayerName` + `RefreshReadyState` + `Construct`:
    /// PlayerBanner_92 and PowerBox Collapsed (1), ReadyStatusBox HitTestInvisible (3), OverlayInputAction Hidden (not hovered); `TextPlayerName` = the member's
    /// name (None keeps the serialized design-time text), `LeaderImage` SelfHitTestInvisible (4) for the party leader else Collapsed (1), `ReadyUpStatusText` =
    /// "Ready" / "Not Ready" (ns [EE36DBE7...]) with `TextStyle-Base-XS-B-S-Green` / `-Red`.
    pub fn hook_pad_top(&mut self, id: usize, name: Option<&str>, leader: bool, ready: bool) {
        for n in ["PlayerBanner_92", "PowerBox"] { let v = self.var(id, n).or_else(|| self.find_in(id, n)); self.set_vis(v, Vis::Collapsed); }
        let v = self.var(id, "ReadyStatusBox"); self.set_vis(v, Vis::HitTestInvisible);
        let v = self.var(id, "OverlayInputAction"); self.set_vis(v, Vis::Hidden);
        if let Some(t) = self.var(id, "TextPlayerName") { if let Some(n) = name { self.w[t].text = Some(n.to_owned()); } }
        let v = self.var(id, "LeaderImage"); self.set_vis(v, if leader { Vis::SelfHitTestInvisible } else { Vis::Collapsed });
        if let Some(t) = self.var(id, "ReadyUpStatusText") {
            let (key, style) = if ready { ("605AAA0540A88F8665F063988F6F4B47", "TextStyle-Base-XS-B-S-Green_C") } else { ("E53C283045D3D79966E4DEB66D636E48", "TextStyle-Base-XS-B-S-Red_C") };
            let txt = self.text_of(&json!({"key": key, "namespace": "[EE36DBE742AE16818DE48981C190F626]", "text": ""})).unwrap_or_default();
            self.w[t].text = Some(txt);
            self.w[t].text_style = self.find_class_path(style);
        }
    }

    pub fn dump(&self, root: usize, out: &mut String, depth: usize, rects: Option<&[[f32; 4]]>) {
        let w = &self.w[root];
        let r = rects.map(|r| format!(" [{:.1},{:.1} {:.1}x{:.1}]", r[root][0], r[root][1], r[root][2], r[root][3])).unwrap_or_default();
        out.push_str(&format!("{}{} \"{}\" {:?}{}{}{}\n", "  ".repeat(depth), w.class, w.name, w.vis, r,
            w.text.as_ref().map(|t| format!(" text={t:?}")).unwrap_or_default(), if w.note.is_empty() { String::new() } else { format!(" ({})", w.note) }));
        if w.vis == Vis::Collapsed { return; }
        for &k in &w.kids { self.dump(k, out, depth + 1, rects); }
    }
}

pub fn halign(v: Option<&Value>, default: &str) -> String { jenum(v).unwrap_or(default).to_owned() }
pub fn margin_of(v: Option<&Value>) -> [f32; 4] { jmargin(v) }








