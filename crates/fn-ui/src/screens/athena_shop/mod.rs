//! The Fortnite 1.8 Battle Royale ITEM SHOP tab, ripped from the cooked widgets and drawn by the generic UMG interpreter.
//!
//! What the "Item Shop" tab is (read from the data):
//! * `AthenaTabsScreen.PreregisteredTabInfoArray`: the registration `TabNameId = AthenaDirectAcquisition` ("Item Shop") has
//!   `TabContentType = /Game/Athena/UI/Frontend/AthenaDirectAcquisitionScreen.AthenaDirectAcquisitionScreen_C`; `HorizontalTabList_C` switches
//!   `TopTabContentWidgetSwitcher` to an instance of it (page 0 here: the other tab pages are other screens).
//! * `AthenaDirectAcquisitionScreen` (UMG): `SwitcherItemsProgress` { `VBOffers` { `HBoxOffers` { VerticalBox_2 { `BorderWeeklyItems` header "WEEKLY ITEMS" +
//!   `WeekCountdown` (CommonDateTimeTextBlock), `LargeTileHB` }, VerticalBox_4 { `BorderDailyItems` header "DAILY ITEMS" + `DayCountdown`, `SmallTileGP`
//!   (GridPanel) } }, disclaimer "Note: These items are cosmetic only and grant no competitive advantage." }, `ProgressSpinner`, `NoOffersBox` }.
//!   `StorefrontNames = [BRWeeklyStorefront, BRDailyStorefront]`.
//! * the tiles: `GenerateOfferWidget` (Kismet) creates one `AthenaDirectAcquisitionOfferWidget_C` per offer (the native `FortDirectAcquisitionScreen` calls it
//!   for each offer of a storefront), sets `LargeTile` (storefront == StorefrontNames[0]) and `Style = ButtonStyle-Solid-SquareM`; large tiles go into
//!   `LargeTileHB` (`HorizontalBox` slot, padding `TilePadding`), small tiles into `SmallTileGP` (`GridSlot` row = (n-1) % 2, column = (n-1) / 2, vertical
//!   alignment Fill, padding `TilePadding`). `OfferSet` / `UpdateStyle` / `SetupPrice` / `UpdateOwnedState` (Kismet) fill the tile: `SBTileSize` 384x608 (large)
//!   or 256x292 (small), `NameText` = `TextToUpper(GetName)` with `TextStyle-Header-M-Horizon` / `-S-Horizon`, `ShortDescriptionText`, `MainImage` = tile art,
//!   `Price1` (`ItemCount_C`: V-Bucks icon + `SetOverrideValue(price)`), `BorderRarityColored` / `BottomBorder` dynamic materials parametrised with the
//!   item's `RarityData` colours (`RGB_1..4`, `RGBA0 = Color5`, `s_AnimateBorder = 1`), `OwnedSwitcher` index 1 when owned ("OWNED" + check), `Hover` animation
//!   (HighlightGlow alpha 0 -> 0.25, `ScB_AnimateScale` 1.0 -> 1.15 over 1 s).
//! * clicking a tile (`OnClicked`): creates `AthenaDirectAcquisitionOfferDetailsWidget_C` and pushes it on the UI manager stack: `UpdateFromOffer` sets the rarity
//!   coloured header (`TextName`), the native item card, descriptions, attribute rows (`StoreMain_OfferDetailsAttribute_C`), the owned state and `SetupPrice`
//!   (price text + V-Bucks icon; the purchase button reads "PURCHASE" when `CanPurchaseOffer == 0`, "Get V-bucks" / "Cannot Afford" otherwise).
//!   `PurchaseButton1` click -> `AttemptPurchase(0)` -> `HandlePurchaseOfferComplete(true)` pushes `AthenaDirectAcquisitionOfferPurchasedWidget_C` and calls
//!   `AutoEquip()`; `HandleBack` (input action "Back") pops the widgets (`PopDetails` / `PopScreen`).
//!
//! Native glue in THIS module (each piece says what it stands in for): composition (`FrontEnd` -> tabs screen -> shop page), tab buttons (as in the lobby),
//! the tile pool (`GenerateOfferWidget` replay), grid / scroll layout (`layout.rs`), dynamic material parameters, native `FortMultiSizeItemCard` / `FortItemIcon` /
//! `FortItemCountTextBlock`, countdown text. `layout.rs` / `paint.rs` are a fork of the lobby interpreter's files with those additions (every addition is marked
//! `SHOP`); the lobby files are owned by another agent.
//! DATA (offers, prices, ownership, balance) is supplied through [`ShopState`]; offline it comes from `fn-app/src/lobby_shop.rs` (clearly labelled substitutes).
pub mod art;
pub mod layout;
pub mod paint;
pub mod view;

pub use view::{Affordability, Dialog, OfferView, ShopState, ShopView};

use crate::draw::RenderList;
use crate::layout::Rect;
use crate::scene::UiContext;
use crate::screens::athena_lobby::slate_mat::SlateMat;
use crate::screens::athena_lobby::store::{jenum, Store};
use crate::screens::athena_lobby::styles::{Brush, BtnState, DrawAs};
use crate::screens::athena_lobby::umg::{Ui, Vis};
use crate::screens::athena_lobby::{FRONTEND, ICON_TAB_BUTTON, TABS_SCREEN};
use fn_dxbc_cli::uniform::Overrides;
use paint::{CardSpec, DynMat};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

const INPUT_ACTIONS: &str = "/Game/UI/Foundation/Input/InputActionDataTable";
pub const SHOP_SCREEN: &str = "/Game/Athena/UI/Frontend/AthenaDirectAcquisitionScreen.AthenaDirectAcquisitionScreen_C";
pub const OFFER_WIDGET: &str = "/Game/Athena/UI/Frontend/AthenaDirectAcquisitionOfferWidget.AthenaDirectAcquisitionOfferWidget_C";
pub const DETAILS_WIDGET: &str = "/Game/Athena/UI/Frontend/AthenaDirectAcquisitionOfferDetailsWidget.AthenaDirectAcquisitionOfferDetailsWidget_C";
pub const PURCHASED_WIDGET: &str = "/Game/Athena/UI/Frontend/AthenaDirectAcquisitionOfferPurchasedWidget.AthenaDirectAcquisitionOfferPurchasedWidget_C";
pub const ATTRIBUTE_WIDGET: &str = "/Game/UI/Frontend/StoreMain/StoreMain_OfferDetailsAttribute.StoreMain_OfferDetailsAttribute_C";
/// `GenerateOfferWidget`: `SetClassPropertyByName(widget, Style, ButtonStyle-Solid-SquareM_C)`
const TILE_STYLE_CLASS: &str = "ButtonStyle-Solid-SquareM_C";
/// the V-Bucks currency item (`GetPriceItem`): its `SmallPreviewImage` is the price icon
const VBUCKS_ITEM: &str = "/Game/Items/Currency/MtxPurchased";
/// `UpdateStyle`: `SBTileSize.SetWidthOverride / SetHeightOverride` per tile kind
const LARGE_TILE: [f32; 2] = [384.0, 608.0];
const SMALL_TILE: [f32; 2] = [256.0, 292.0];

/// `UFortMultiSizeItemCard` sizes (`EFortItemCardSize`) of cosmetic cards, recovered from the exe by the Locker port (`athena_locker/card.rs`:
/// `UFortUIDataConfiguration.PersonnelAndSchematicCardSizes`, the tall table that cosmetic types use).
pub const CARD_SIZES: [(&str, [f32; 2]); 6] = [("XXS", [44.0, 72.0]), ("XS", [60.0, 98.0]), ("S", [88.0, 142.0]), ("M", [128.0, 207.0]), ("L", [160.0, 258.0]), ("XL", [256.0, 414.0])];
pub(crate) fn card_size(enum_value: &str) -> [f32; 2] {
    let t = enum_value.rsplit("::").next().unwrap_or(enum_value);
    CARD_SIZES.iter().find(|(n, _)| *n == t).map(|(_, s)| *s).unwrap_or([160.0, 258.0])
}
fn card_name(enum_value: &str) -> String {
    let t = enum_value.rsplit("::").next().unwrap_or(enum_value);
    CARD_SIZES.iter().find(|(n, _)| *n == t).map(|(n, _)| (*n).to_owned()).unwrap_or_else(|| "L".into())
}

/// `FText::AsNumber` of an integer (digit grouping).
pub fn group_digits(v: i64) -> String {
    let s = v.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 { out.push(','); }
        out.push(c);
    }
    if v < 0 { format!("-{out}") } else { out }
}

/// Countdown text of `CommonDateTimeTextBlock.SetDateTimeValue(endDate, true)`. The native format is not in the Blueprint data; the strings come from the 1.8 `Game.locres`:
/// a day or more left -> `"{0} {0}|plural(one=Day,other=Days)"` (`[Fortnite.AthenaLeaderboardScreenBase] LastUpdateTimeDays`), else the timespan pattern
/// `"{Hours}:{Minutes}:{Seconds}"` (namespace `[4E0E3E33460EB7E4748F50B72ECFCB47]`, key `42EB0AF74AAAC88EFC3887BF9F44C165`) with two-digit minutes and seconds.
/// WHICH pattern the block uses (and the one-day threshold) is taken from a reference screenshot of the real 1.8 shop ("9 Days", "14:13:17"): reference-based, not read from code.
pub fn countdown_text(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m, sec) = (s / 86_400, (s / 3_600) % 24, (s / 60) % 60, s % 60);
    if d > 0 { format!("{d} {}", if d == 1 { "Day" } else { "Days" }) } else { format!("{h}:{m:02}:{sec:02}") }
}
/// One drawn frame.
pub struct ShopFrame {
    pub list: RenderList,
    pub blur: Vec<paint::BlurRegion>,
    pub rects: HashMap<String, Rect>,
    pub tabs: Vec<(String, Rect)>,
    /// (offer id, rectangle) of every visible tile (empty while a dialog covers the storefront)
    pub offers: Vec<(String, Rect)>,
    /// `PurchaseButton1` of the details widget when it is shown
    pub purchase_button: Option<Rect>,
    /// bottom bar "Back" button while a dialog is up
    pub back_button: Option<Rect>,
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShopHit { Tab(usize), Offer(String), Purchase, Back, None }

/// The clickable rectangles of a frame (keep the last one to resolve pointer events).
#[derive(Clone, Debug, Default)]
pub struct HitMap {
    pub tabs: Vec<(String, Rect)>,
    pub offers: Vec<(String, Rect)>,
    pub purchase_button: Option<Rect>,
    pub back_button: Option<Rect>,
}

impl HitMap {
    pub fn hit(&self, x: f32, y: f32) -> ShopHit {
        if let Some(r) = self.back_button { if r.contains(x, y) { return ShopHit::Back; } }
        if let Some(r) = self.purchase_button { if r.contains(x, y) { return ShopHit::Purchase; } }
        if let Some(i) = self.tabs.iter().position(|(_, r)| r.contains(x, y)) { return ShopHit::Tab(i); }
        if let Some((id, _)) = self.offers.iter().find(|(_, r)| r.contains(x, y)) { return ShopHit::Offer(id.clone()); }
        ShopHit::None
    }
}

impl ShopFrame {
    pub fn rect(&self, name: &str) -> Option<Rect> { self.rects.get(name).copied() }
    pub fn hit_map(&self) -> HitMap { HitMap { tabs: self.tabs.clone(), offers: self.offers.clone(), purchase_button: self.purchase_button, back_button: self.back_button } }
    pub fn hit(&self, x: f32, y: f32) -> ShopHit { self.hit_map().hit(x, y) }
}
pub struct AthenaShopUi {
    pub ui: Ui,
    pub root: usize,
    tabs_screen: usize,
    tabs: Vec<usize>,
    tab_labels: Vec<String>,
    /// visible-tab index of the "Item Shop" registration
    pub item_shop_tab: usize,
    screen: usize,
    large_hb: usize,
    small_gp: usize,
    large_pool: Vec<usize>,
    small_pool: Vec<usize>,
    details: usize,
    purchased: usize,
    /// bottom bar "Back" button (`InputReflector`: the details / purchased widgets register the input action `Back`); shown while one of them is up
    back_button: Option<usize>,
    attr_pool: Vec<usize>,
    mats: HashMap<String, Option<Rc<SlateMat>>>,
    rarity: Vec<[[f32; 4]; 5]>,
    vbucks_icon: Option<String>,
    tile_sizes: Option<HashMap<String, [f32; 2]>>,
    tile_padding: Value,
    pub composition_log: Vec<String>,
}

/// `M_Font_Horizon_Dynamic` (the font material of the `TextStyle-*-Horizon` styles) takes the rarity colours as `RGB_1` / `RGB_2` and keeps `s_WhiteFill = 0.65`
/// of the glyph white. The glyph quads of this painter cannot run a material, so the text is drawn with the white-filled mix of `RGB_1` (APPROXIMATION, see SHOP.md).
fn font_fill(rgb1: [f32; 4]) -> [f32; 4] { const WHITE_FILL: f32 = 0.65; [rgb1[0] * (1.0 - WHITE_FILL) + WHITE_FILL, rgb1[1] * (1.0 - WHITE_FILL) + WHITE_FILL, rgb1[2] * (1.0 - WHITE_FILL) + WHITE_FILL, 1.0] }

fn vec4(v: Option<&Value>) -> Option<[f32; 4]> {
    let a = v?.as_array()?;
    Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32, a.get(3)?.as_f64()? as f32])
}

impl AthenaShopUi {
    pub fn load(install: &Path) -> Result<AthenaShopUi, String> { Self::from_store(Rc::new(Store::open(install)?)) }

    pub fn from_store(store: Rc<Store>) -> Result<AthenaShopUi, String> {
        let mut ui = Ui::new(store.clone());
        let root = ui.add_class_instance(FRONTEND, None, "", json!({}), None).ok_or("cannot compose FrontEnd")?;
        // FrontEnd::InitForSubGame (Athena): AthenaTabsScreen_C pushed into MainContentStack
        let stack = ui.find_in(root, "MainContentStack").ok_or("FrontEnd has no MainContentStack")?;
        let tabs_screen = ui.add_class_instance(TABS_SCREEN, Some(stack), "PanelSlot", json!({}), None).ok_or("cannot compose AthenaTabsScreen")?;
        // the tab list builds the content of the Item Shop registration: AthenaDirectAcquisitionScreen_C in the content switcher
        let switcher = ui.var(tabs_screen, "TopTabContentWidgetSwitcher").or_else(|| ui.find_in(tabs_screen, "TopTabContentWidgetSwitcher")).ok_or("no TopTabContentWidgetSwitcher")?;
        let screen = ui.add_class_instance(SHOP_SCREEN, Some(switcher), "WidgetSwitcherSlot", json!({}), None).ok_or("cannot compose AthenaDirectAcquisitionScreen")?;
        // HorizontalTabList_C: one IconTabButton_C per registration that is not bHidden (same as the lobby)
        let list = ui.var(tabs_screen, "TopTabList").or_else(|| ui.find_in(tabs_screen, "TopTabList")).ok_or("no TopTabList")?;
        let tab_box = ui.var(list, "TabButtonBox").or_else(|| ui.find_in(list, "TabButtonBox")).ok_or("no TabButtonBox")?;
        let style = ui.w[list].props.get("ButtonStyle").cloned().unwrap_or(json!(null));
        let pad = ui.w[list].props.get("TabButtonPadding").cloned().unwrap_or(json!({}));
        let regs = ui.w[list].props.get("PreregisteredTabInfoArray").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        let (mut tabs, mut labels, mut item_shop_tab) = (vec![], vec![], 0usize);
        for r in regs {
            if !crate::screens::athena_lobby::tab_shown(&r) { continue; }
            if r.get("TabContentType").and_then(|c| c.as_str()).map_or(false, |c| c.contains("AthenaDirectAcquisitionScreen")) { item_shop_tab = tabs.len(); }
            let dn = r.get("TabLabelInfo").and_then(|l| l.get("DisplayName")).cloned().unwrap_or(json!(null));
            // AthenaTabsScreen::HandleTabCreated: SetStyle(ButtonStyle-Tab-Main); ButtonText = TextToUpper(ButtonText); ShowText()
            let upper = ui.text_of(&dn).unwrap_or_default().to_uppercase();
            let inst = json!({ "Style": style, "ButtonText": null });
            let t = ui.add_class_instance(ICON_TAB_BUTTON, Some(tab_box), "HorizontalBoxSlot", json!({ "Padding": pad }), Some(inst)).ok_or("cannot compose IconTabButton")?;
            if let Some(tw) = ui.var(t, "CenterButtonTextWidget") { ui.w[tw].text = Some(upper.clone()); ui.w[tw].vis = if upper.is_empty() { Vis::Collapsed } else { Vis::SelfHitTestInvisible }; }
            tabs.push(t);
            labels.push(upper);
        }
        let large_hb = ui.var(screen, "LargeTileHB").ok_or("no LargeTileHB")?;
        let small_gp = ui.var(screen, "SmallTileGP").ok_or("no SmallTileGP")?;
        let tile_padding = ui.w[screen].props.get("TilePadding").cloned().unwrap_or(json!({}));
        // UIManager.PushContentWidgetAdvanced targets: the details / purchased widgets live on the same stack, above the tabs screen (inactive = Collapsed)
        let details = ui.add_class_instance(DETAILS_WIDGET, Some(stack), "PanelSlot", json!({}), None).ok_or("cannot compose the details widget")?;
        let purchased = ui.add_class_instance(PURCHASED_WIDGET, Some(stack), "PanelSlot", json!({}), None).ok_or("cannot compose the purchased widget")?;
        ui.w[details].vis = Vis::Collapsed;
        ui.w[purchased].vis = Vis::Collapsed;
        // FortRarityData.RarityCollection (BPGetRarityData): Color1..5 of every rarity row
        let mut rarity = vec![];
        if let Some(p) = store.pkg("/Game/Balance/RarityData") {
            if let Some(e) = p.named("RarityData") {
                for n in 0..16 {
                    let key = if n == 0 { "RarityCollection".to_owned() } else { format!("RarityCollection[{n}]") };
                    let Some(row) = e.props.get(&key) else { break };
                    let mut c = [[1.0; 4]; 5];
                    for (i, o) in c.iter_mut().enumerate() { if let Some(v) = vec4(row.get(format!("Color{}", i + 1))) { *o = v; } }
                    rarity.push(c);
                }
            }
        }
        // the price item's icon: `MtxCurrency` item definition -> SmallPreviewImage
        let vbucks_icon = store.pkg(VBUCKS_ITEM).and_then(|p| p.exports.iter().find(|e| e.name == "MtxPurchased").and_then(|e| e.props.get("SmallPreviewImage").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned)));
        let mut me = AthenaShopUi { ui, root, tabs_screen, tabs, tab_labels: labels, item_shop_tab, screen, large_hb, small_gp, large_pool: vec![], small_pool: vec![], details, purchased, back_button: None, attr_pool: vec![],
            mats: HashMap::new(), rarity, vbucks_icon, tile_sizes: None, tile_padding, composition_log: vec![] };
        me.back_button = me.reflect_back();
        me.composition_log = me.ui.log.clone();
        Ok(me)
    }

    pub fn tab_labels(&self) -> &[String] { &self.tab_labels }

    /// `InputReflector::OnButtonAdded` (Kismet) for the input action `Back` that `AthenaDirectAcquisitionOfferDetailsWidget` / `...PurchasedWidget` register with
    /// `SetInputActionHandler(InputActionDataTable.Back, HandleBack)`: an `IconTextButton_C` (style `ButtonStyle-BottomBar`, centred, `DefaultButtonMargin`) with the
    /// row's `DisplayName`. Same construction as the lobby's News button.
    fn reflect_back(&mut self) -> Option<usize> {
        let ui = &mut self.ui;
        let refl = ui.find_by_name("InputReflector")?;
        let bbox = ui.var(refl, "ButtonBox")?;
        let margin = ui.w[refl].props.get("DefaultButtonMargin").cloned().unwrap_or(json!({}));
        let style = ui.find_class_path("ButtonStyle-BottomBar_C");
        let btn_class = ui.find_class_path("IconTextButton_C")?;
        let table = ui.store.pkg(INPUT_ACTIONS).and_then(|p| p.named("InputActionDataTable").map(|e| e.props.clone()))?;
        let text = table.get("Rows")?.get("Back")?.get("DisplayName").cloned()?;
        let slot = json!({ "HorizontalAlignment": "HAlign_Center", "VerticalAlignment": "VAlign_Center", "Padding": margin });
        let id = ui.add_class_instance(&btn_class, Some(bbox), "HorizontalBoxSlot", slot, Some(json!({ "ButtonText": text, "Style": style })))?;
        ui.w[id].vis = Vis::Collapsed;
        Some(id)
    }

    /// A named widget of `owner`: its Blueprint variable when it is one, else the first widget of that name inside it (`ScB_AnimateScale` is not a variable).
    fn w(&self, owner: usize, name: &str) -> Option<usize> { self.ui.var(owner, name).or_else(|| self.ui.find_in(owner, name)) }

    fn rarity_colors(&self, r: usize) -> [[f32; 4]; 5] { self.rarity.get(r).or(self.rarity.first()).copied().unwrap_or([[1.0; 4]; 5]) }

    fn text_of_key(&self, key: &str, ns: &str) -> String { self.ui.text_of(&json!({ "key": key, "namespace": ns, "text": "" })).unwrap_or_default() }

    fn style_path(&mut self, class: &str) -> Option<String> { self.ui.find_class_path(class) }

    // ---------------------------------------------------------------------------------------------------------------------------------------
    // tiles: GenerateOfferWidget replay + OfferSet / UpdateStyle / SetupPrice / UpdateOwnedState

    fn new_tile(&mut self, large: bool, index: usize) -> Option<usize> {
        let style = self.style_path(TILE_STYLE_CLASS).map(|p| json!(p)).unwrap_or(json!(null));
        let inst = json!({ "LargeTile": large, "Style": style });
        if large {
            // AddChildToHorizontalBox(widget).SetPadding(TilePadding)
            self.ui.add_class_instance(OFFER_WIDGET, Some(self.large_hb), "HorizontalBoxSlot", json!({ "Padding": self.tile_padding }), Some(inst))
        } else {
            // AddChildToGrid(widget): SetVerticalAlignment(Fill), SetPadding(TilePadding), SetRow(count-1 % 2), SetColumn(count-1 / 2)
            let slot = json!({ "Row": index % 2, "Column": index / 2, "VerticalAlignment": "VAlign_Fill", "Padding": self.tile_padding });
            self.ui.add_class_instance(OFFER_WIDGET, Some(self.small_gp), "GridSlot", slot, Some(inst))
        }
    }

    fn set_override(&mut self, id: usize, key: &str, v: f32) { self.ui.w[id].props[key] = json!(v); self.ui.w[id].props[format!("bOverride_{key}")] = json!(true); }

    fn price_widget(&mut self, price_w: usize, price: i64, small: bool) {
        // ItemCount_C: SetBrushAndTextStyle(size, TextStyle-Base-{S,M}-B) -> CountText style + icon brush size; SetItemDefinition(V-Bucks) -> icons; SetOverrideValue(price)
        let style = self.style_path(if small { "TextStyle-Base-S-B_C" } else { "TextStyle-Base-M-B_C" });
        let size = if small { 24.0 } else { 32.0 }; // EFortIconSize byte 2 / 3: the table is native; sizes chosen to match the text height (flagged in SHOP.md)
        self.ui.w[price_w].vis = Vis::SelfHitTestInvisible;
        let left = self.ui.w[price_w].props.get("DisplayIconLeftSide").and_then(|v| v.as_bool()).unwrap_or(false);
        let brush = self.vbucks_icon.clone().map(|res| Brush { draw: DrawAs::Image, image_size: [size, size], margin: [0.0; 4], tint: [1.0; 4], resource: Some(res), flip_x: false, uv_region: None });
        for (name, show) in [("IconLeft", left), ("IconRight", !left)] {
            if let Some(i) = self.w(price_w, name) {
                self.ui.w[i].vis = if show { Vis::Visible } else { Vis::Collapsed };
                self.ui.w[i].brush = if show { brush.clone() } else { None };
            }
        }
        if let Some(c) = self.w(price_w, "CountText") { self.ui.w[c].text = Some(group_digits(price)); self.ui.w[c].text_style = style; self.ui.w[c].vis = Vis::SelfHitTestInvisible; }
        if let Some(d) = self.w(price_w, "DisplayName") { self.ui.w[d].vis = Vis::Collapsed; }
    }

    fn configure_tile(&mut self, tile: usize, o: &OfferView, large: bool, hovered: bool, dm: &mut paint::DynMats) {
        let rc = self.rarity_colors(o.rarity);
        self.ui.w[tile].vis = Vis::Visible;
        // UpdateStyle: tile size
        if let Some(sb) = self.w(tile, "SBTileSize") {
            let s = if large { LARGE_TILE } else { SMALL_TILE };
            self.set_override(sb, "WidthOverride", s[0]);
            self.set_override(sb, "HeightOverride", s[1]);
        }
        if let Some(p) = self.w(tile, "SBPriceBox") { self.set_override(p, "MinDesiredHeight", if large { 64.0 } else { 40.0 }); }
        // dynamic materials: BorderRarityColored (RGB_1..4, s_AnimateBorder), BottomBorder (RGBA0 = Color5)
        if let Some(b) = self.w(tile, "BorderRarityColored") {
            let mut ov = Overrides::default();
            for k in 0..4 { ov.vectors.insert(format!("RGB_{}", k + 1), rc[k]); }
            ov.scalars.insert("s_AnimateBorder".into(), 1.0);
            dm.mats.insert(b, DynMat { overrides: ov, textures: HashMap::new(), key: format!("tileborder{}", o.rarity) });
        }
        if let Some(b) = self.w(tile, "BottomBorder") {
            let mut ov = Overrides::default();
            ov.vectors.insert("RGBA0".into(), rc[4]);
            dm.mats.insert(b, DynMat { overrides: ov, textures: HashMap::new(), key: format!("tilebottom{}", o.rarity) });
        }
        // OfferSet: NameText = TextToUpper(GetName) (style Header-M / Header-S Horizon + font material RGB_1 / RGB_2), ShortDescriptionText
        let name_style = self.style_path(if large { "TextStyle-Header-M-Horizon_C" } else { "TextStyle-Header-S-Horizon_C" });
        if let Some(n) = self.w(tile, "NameText") { self.ui.w[n].text = Some(o.name.to_uppercase()); self.ui.w[n].text_style = name_style; dm.text_color.insert(n, font_fill(rc[0])); }
        if let Some(n) = self.w(tile, "ShortDescriptionText") { self.ui.w[n].text = Some(o.short_description.clone()); }
        // tile art: HasDisplayAsset -> TileImage, else the item's large preview (LargeImageLoaded -> SetBrushFromTexture)
        if let Some(img) = self.w(tile, "MainImage") {
            // a display asset's `TileImage` brush carries its own ImageSize (`SetBrush`); the large preview path is `SetBrushFromTexture(tex, bMatchSize = false)`: the widget's 512 x 512
            let size = o.tile_image.as_deref().and_then(|r| self.tile_size(r)).unwrap_or([512.0, 512.0]);
            self.ui.w[img].brush = Some(match o.tile_image.clone() {
                Some(res) => Brush { draw: DrawAs::Image, image_size: size, margin: [0.0; 4], tint: [1.0; 4], resource: Some(res), flip_x: false, uv_region: None },
                None => Brush { draw: DrawAs::NoDraw, image_size: [512.0, 512.0], margin: [0.0; 4], tint: [1.0; 4], resource: None, flip_x: false, uv_region: None },
            });
        }
        // the tile is a CommonButton: hovering shows the style's `NormalHovered` brush (`M_Solid_Square_S_Edit`) around the padded button box
        self.ui.w[tile].btn_state = if hovered { BtnState::Hovered } else { BtnState::Normal };
        // Hover animation (end values: HighlightGlow alpha 0.25, ScB_AnimateScale 1.15)
        if let Some(g) = self.w(tile, "HighlightGlow") { self.ui.w[g].props["ColorAndOpacity"] = json!([1.0, 1.0, 1.0, if hovered { 0.25 } else { 0.0 }]); }
        if let Some(s) = self.w(tile, "ScB_AnimateScale") {
            let sc = if hovered { 1.15 } else { 1.0 };
            self.ui.w[s].transform = crate::screens::athena_lobby::umg::Transform { translation: [0.0; 2], scale: [sc, sc], angle: 0.0 };
        }
        // SetupPrice(0, Price1)
        if let Some(p) = self.w(tile, "Price1") { self.price_widget(p, o.price, !large); }
        // UpdateOwnedState: OwnedSwitcher index 1 (OWNED + check) when owned
        if let Some(sw) = self.w(tile, "OwnedSwitcher") { self.ui.w[sw].props["ActiveWidgetIndex"] = json!(if o.owned { 1 } else { 0 }); }
        if let Some(a) = self.w(tile, "InputActionWidget") { self.ui.w[a].vis = Vis::Collapsed; }
    }

    /// `TileImage.ImageSize` of the display asset (`/Game/Catalog/DisplayAssets/DA_*`) whose tile image is `res`; None when no display asset uses it.
    fn tile_size(&mut self, res: &str) -> Option<[f32; 2]> {
        if self.tile_sizes.is_none() {
            let store = self.ui.store.clone();
            let mut m = HashMap::new();
            for n in art::list_display_assets(&store) { if let Some(a) = art::display_asset(&store, &n) { if let (Some(t), Some(s)) = (a.tile_image, a.tile_image_size) { m.insert(t, s); } } }
            self.tile_sizes = Some(m);
        }
        self.tile_sizes.as_ref().and_then(|m| m.get(res).copied())
    }

    fn hide_pool(&mut self, pool: &[usize], from: usize) { for &t in pool.iter().skip(from) { self.ui.w[t].vis = Vis::Collapsed; } }

    // ---------------------------------------------------------------------------------------------------------------------------------------
    // details / purchased widgets

    fn purchase_button_state(&mut self, details: usize, o: &OfferView, balance: i64, hovered: bool) {
        // SetupPrice(PriceIndex 0, PurchaseButton1, Currency1VB, TextOriginalPrice1, OverlaySalePrice1, TextOfferDisplayPrice1, Currency1ItemImage2)
        if let Some(c) = self.w(details, "Currency1VB") { self.ui.w[c].vis = Vis::SelfHitTestInvisible; }
        if let Some(t) = self.w(details, "TextOfferDisplayPrice1") { self.ui.w[t].text = Some(group_digits(o.price)); }
        if let Some(s) = self.w(details, "OverlaySalePrice1") { self.ui.w[s].vis = Vis::Collapsed; } // GetSalePrice(0) false: no sale price offline
        if let Some(i) = self.w(details, "Currency1ItemImage2") {
            self.ui.w[i].vis = Vis::SelfHitTestInvisible;
            self.ui.w[i].brush = self.vbucks_icon.clone().map(|res| Brush { draw: DrawAs::Image, image_size: [40.0, 40.0], margin: [0.0; 4], tint: [1.0; 4], resource: Some(res), flip_x: false, uv_region: None });
        }
        let ns = "[0277A2374C4DF7CFB214BB9B701515DB]";
        let can = Affordability::of(o, balance);
        let (label, enabled) = match can {
            // CanPurchaseOffer == 0 -> "PURCHASE" + EnableButton
            Affordability::Can | Affordability::Owned => (self.text_of_key("752081D7461D391F84DE19874D95B49D", ns), true),
            // otherwise IsPriceInMTX(0) -> "Get V-bucks" + EnableButton (the V-Bucks price of the offer can be topped up in the store)
            Affordability::CannotAfford => (self.text_of_key("16E73F2E4D0C0661D9FEAE85D2AA9557", ns), true),
        };
        if let Some(b) = self.w(details, "PurchaseButton1") {
            if let Some(t) = self.w(b, "CenterButtonTextWidget") { self.ui.w[t].text = Some(label); self.ui.w[t].vis = Vis::SelfHitTestInvisible; }
            self.ui.w[b].enabled = enabled;
            self.ui.w[b].btn_state = if !enabled { BtnState::Disabled } else if hovered { BtnState::Hovered } else { BtnState::Normal };
            self.ui.refresh_button_text(b);
        }
    }

    fn attribute_rows(&mut self, details: usize, rows: &[(String, String)]) {
        let Some(vb) = self.w(details, "VBoxOfferInfo") else { return };
        // UpdateFromOffer: VBoxOfferInfo.ClearChildren(); one StoreMain_OfferDetailsAttribute per GetDetailsAttributes row
        while self.attr_pool.len() < rows.len() {
            match self.ui.add_class_instance(ATTRIBUTE_WIDGET, None, "VerticalBoxSlot", json!({}), None) { Some(id) => self.attr_pool.push(id), None => break }
        }
        let kids: Vec<usize> = self.attr_pool.iter().copied().take(rows.len()).collect();
        for (k, (name, value)) in kids.iter().zip(rows) {
            self.ui.w[*k].parent = Some(vb);
            if let Some(n) = self.w(*k, "Name") { self.ui.w[n].text = Some(name.clone()); }
            if let Some(v) = self.w(*k, "Value") { self.ui.w[v].text = Some(value.clone()); }
        }
        self.ui.w[vb].kids = kids;
    }

    fn configure_details(&mut self, o: &OfferView, balance: i64, purchase_hovered: bool, dm: &mut paint::DynMats) {
        let d = self.details;
        let rc = self.rarity_colors(o.rarity);
        // dynamic materials of UpdateFromOffer
        if let Some(b) = self.w(d, "BorderRarityColored") {
            let mut ov = Overrides::default();
            for k in 0..4 { ov.vectors.insert(format!("RGB_{}", k + 1), rc[k]); }
            ov.scalars.insert("s_AnimateBorder".into(), 1.0);
            dm.mats.insert(b, DynMat { overrides: ov, textures: HashMap::new(), key: format!("detborder{}", o.rarity) });
        }
        if let Some(b) = self.w(d, "BorderDarkRarity") {
            let mut ov = Overrides::default();
            ov.vectors.insert("RGBA0".into(), rc[3]);
            dm.mats.insert(b, DynMat { overrides: ov, textures: HashMap::new(), key: format!("detdark{}", o.rarity) });
        }
        let style = self.style_path("TextStyle-Header-L-Horizon_C");
        if let Some(n) = self.w(d, "TextName") { self.ui.w[n].text = Some(o.name.to_uppercase()); self.ui.w[n].text_style = style; dm.text_color.insert(n, font_fill(rc[0])); }
        if let Some(n) = self.w(d, "TextDescription") { self.ui.w[n].text = Some(o.description.clone()); }
        if let Some(n) = self.w(d, "ShortDescription") { self.ui.w[n].text = Some(o.short_description.clone()); }
        // ItemCard.SetItemToRepresent(CreateTemporaryItemInstanceBP(first grant item))
        if let Some(c) = self.w(d, "ItemCard") { self.set_card(c, o, dm); }
        self.attribute_rows(d, &o.attributes);
        if let Some(sw) = self.w(d, "OwnedSwitcher") { self.ui.w[sw].props["ActiveWidgetIndex"] = json!(if o.owned { 1 } else { 0 }); }
        if let Some(i) = self.w(d, "NotUmbrellaIcon") { self.ui.w[i].vis = if o.is_glider { Vis::SelfHitTestInvisible } else { Vis::Collapsed }; }
        self.purchase_button_state(d, o, balance, purchase_hovered);
    }

    fn set_card(&mut self, widget: usize, o: &OfferView, dm: &mut paint::DynMats) {
        let size = card_name(self.ui.w[widget].props.get("BPItemCardSize").and_then(|v| v.as_str()).unwrap_or("L"));
        dm.cards.insert(widget, CardSpec { size, rarity_colors: self.rarity_colors(o.rarity), rarity: o.rarity, icon: o.icon.clone(), hero: o.is_outfit });
    }

    fn configure_purchased(&mut self, o: &OfferView, dm: &mut paint::DynMats) {
        let d = self.purchased;
        let rc = self.rarity_colors(o.rarity);
        if let Some(c) = self.w(d, "ItemCard") { self.set_card(c, o, dm); }
        if let Some(g) = self.w(d, "RarityGlow") {
            let mut ov = Overrides::default();
            ov.vectors.insert("RGBA1".into(), rc[0]);
            ov.vectors.insert("RGBA0".into(), [rc[1][0], rc[1][1], rc[1][2], 0.0]);
            ov.scalars.insert("Global Alpha".into(), 0.33);
            dm.mats.insert(g, DynMat { overrides: ov, textures: HashMap::new(), key: format!("purglow{}", o.rarity) });
        }
        if let Some(g) = self.w(d, "ImageRarityFlare") {
            let mut ov = Overrides::default();
            ov.vectors.insert("Rarity Color 1".into(), rc[1]);
            ov.vectors.insert("Rarity Color 2".into(), rc[2]);
            dm.mats.insert(g, DynMat { overrides: ov, textures: HashMap::new(), key: format!("purflare{}", o.rarity) });
        }
        let style = self.style_path("TextStyle-Header-XL-Horizon_C");
        if let Some(n) = self.w(d, "TextName") { self.ui.w[n].text = Some(o.name.to_uppercase()); self.ui.w[n].text_style = style; dm.text_color.insert(n, font_fill(rc[0])); }
    }

    // ---------------------------------------------------------------------------------------------------------------------------------------

    fn apply_state(&mut self, st: &ShopState) -> paint::DynMats {
        let mut dm = paint::DynMats::default();
        for (i, &t) in self.tabs.iter().enumerate() {
            let state = if i == st.selected_tab { if st.hovered_tab == Some(i) { BtnState::SelectedHovered } else { BtnState::Selected } } else if st.hovered_tab == Some(i) { BtnState::Hovered } else { BtnState::Normal };
            self.ui.w[t].btn_state = state;
            self.ui.refresh_button_text(t);
        }
        // MTXButton.Amount.SetCurrentValue(MtxBalance)
        if let Some(m) = self.ui.find_by_name("MTXButton") { if let Some(a) = self.w(m, "Amount") { self.ui.w[a].text = Some(group_digits(st.mtx_balance)); } }
        // storefront tiles
        for (large, offers) in [(true, &st.view.weekly), (false, &st.view.daily)] {
            let pool_len = if large { self.large_pool.len() } else { self.small_pool.len() };
            for i in pool_len..offers.len() {
                if let Some(t) = self.new_tile(large, i) { if large { self.large_pool.push(t) } else { self.small_pool.push(t) } }
            }
            let pool = if large { self.large_pool.clone() } else { self.small_pool.clone() };
            for (i, o) in offers.iter().enumerate() {
                let Some(&t) = pool.get(i) else { break };
                let hovered = st.hovered_offer.as_deref() == Some(o.offer_id.as_str());
                self.configure_tile(t, o, large, hovered, &mut dm);
            }
            self.hide_pool(&pool, offers.len());
        }
        // countdowns: SetDateTimeValue(GetWeeklyStoreEndDate / GetDailyStoreEndDate, true)
        if let Some(w) = self.w(self.screen, "WeekCountdown") { self.ui.w[w].text = Some(countdown_text(st.view.week_ends_in)); }
        if let Some(w) = self.w(self.screen, "DayCountdown") { self.ui.w[w].text = Some(countdown_text(st.view.day_ends_in)); }
        // SwitcherItemsProgress: VBOffers when there are offers, else NoOffersBox (NoOffersAvailable -> SetActiveWidget(NoOffersBox)); the spinner is page 1
        if let Some(sw) = self.w(self.screen, "SwitcherItemsProgress") {
            let want = if st.view.weekly.is_empty() && st.view.daily.is_empty() { "NoOffersBox" } else { "VBOffers" };
            let idx = self.w(self.screen, want).and_then(|w| self.ui.w[sw].kids.iter().position(|&k| k == w)).unwrap_or(0);
            self.ui.w[sw].props["ActiveWidgetIndex"] = json!(idx);
        }
        // pushed widgets (PushContentWidgetAdvanced): the UI manager shows the top of the content stack only
        let (show_details, show_purchased, tabs_visible) = match &st.dialog { Dialog::None => (false, false, true), Dialog::Details { .. } => (true, false, false), Dialog::Purchased { .. } => (false, true, false) };
        if let Some(b) = self.back_button { self.ui.w[b].vis = if tabs_visible { Vis::Collapsed } else { Vis::Visible }; self.ui.w[b].btn_state = if st.back_hovered { BtnState::Hovered } else { BtnState::Normal }; self.ui.refresh_button_text(b); }
        self.ui.w[self.tabs_screen].vis = if tabs_visible { Vis::Visible } else { Vis::Collapsed };
        self.ui.w[self.details].vis = if show_details { Vis::Visible } else { Vis::Collapsed };
        self.ui.w[self.purchased].vis = if show_purchased { Vis::Visible } else { Vis::Collapsed };
        match &st.dialog {
            Dialog::Details { offer, purchase_hovered } => self.configure_details(offer, st.mtx_balance, *purchase_hovered, &mut dm),
            Dialog::Purchased { offer } => self.configure_purchased(offer, &mut dm),
            Dialog::None => {}
        }
        dm
    }

    /// Lay out and paint the screen for a `w` x `h` pixel viewport (transparent: composite over the 3D scene).
    pub fn draw(&mut self, uic: &mut UiContext, w: f32, h: f32, st: &ShopState) -> ShopFrame {
        let dm = self.apply_state(st);
        let scale = crate::layout::dpi_scale(w, h);
        let lay = layout::run(&mut self.ui, uic, self.root, [w / scale, h / scale], scale, &[]);
        let mut p = paint::Painter { c: layout::Ctx { ui: &mut self.ui, uic, lay }, out: RenderList::new(), mats: &mut self.mats, blur: vec![], missing: vec![], scale, disabled: false, dynm: dm };
        p.paint(self.root, paint::Tf::ID, [1.0; 4]);
        let (list, blur, mut missing) = (std::mem::take(&mut p.out), std::mem::take(&mut p.blur), std::mem::take(&mut p.missing));
        let lay = p.c.lay;
        missing.extend(self.ui.unresolved.borrow().iter().cloned());
        let px = |r: [f32; 4]| Rect::new(r[0] * scale, r[1] * scale, r[2] * scale, r[3] * scale);
        let mut rects = HashMap::new();
        for wd in &self.ui.w { if wd.id < lay.rect.len() { rects.entry(wd.name.clone()).or_insert(px(lay.rect[wd.id])); } }
        let tabs = self.tabs.iter().zip(&self.tab_labels).map(|(&t, n)| (n.clone(), px(lay.rect[t]))).collect();
        let mut offers = vec![];
        if matches!(st.dialog, Dialog::None) {
            for (pool, list) in [(&self.large_pool, &st.view.weekly), (&self.small_pool, &st.view.daily)] {
                for (&t, o) in pool.iter().zip(list.iter()) { offers.push((o.offer_id.clone(), px(lay.rect[t]))); }
            }
        }
        let purchase_button = if matches!(st.dialog, Dialog::Details { .. }) { self.w(self.details, "PurchaseButton1").map(|b| px(lay.rect[b])) } else { None };
        let back_button = if matches!(st.dialog, Dialog::None) { None } else { self.back_button.map(|b| px(lay.rect[b])) };
        ShopFrame { list, blur, rects, tabs, offers, purchase_button, back_button, missing }
    }

    /// Dev tool: compose any widget class standalone and dump its tree with arranged rectangles (Slate units).
    pub fn dump_class(&mut self, uic: &mut UiContext, w: f32, h: f32, class_path: &str) -> String {
        let scale = crate::layout::dpi_scale(w, h);
        let Some(id) = self.ui.add_class_instance(class_path, None, "", json!({}), None) else { return format!("cannot compose {class_path}") };
        let lay = layout::run(&mut self.ui, uic, id, [w / scale, h / scale], scale, &[]);
        let mut s = String::new();
        self.ui.dump(id, &mut s, 0, Some(&lay.rect));
        s
    }

    /// Text dump of the composed storefront tree with arranged rectangles (Slate units) for a viewport.
    pub fn dump(&mut self, uic: &mut UiContext, w: f32, h: f32, st: &ShopState) -> String {
        let _ = self.apply_state(st);
        let scale = crate::layout::dpi_scale(w, h);
        let lay = layout::run(&mut self.ui, uic, self.root, [w / scale, h / scale], scale, &[]);
        let mut s = String::new();
        let top = match &st.dialog { Dialog::Details { .. } => self.details, Dialog::Purchased { .. } => self.purchased, Dialog::None => self.screen };
        self.ui.dump(top, &mut s, 0, Some(&lay.rect));
        s
    }
}

#[allow(dead_code)]
fn _enum(v: Option<&Value>) -> Option<&str> { jenum(v) }

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(id: &str, rarity: usize, price: i64, owned: bool) -> OfferView {
        OfferView { offer_id: format!("offer:{id}"), name: format!("Name {id}"), short_description: "Outfit".into(), description: "Description text.".into(), rarity, price, owned, tile_image: None, icon: None, attributes: vec![("Outfit".into(), "1".into())], is_glider: false, is_outfit: true }
    }
    fn state(shop: &AthenaShopUi) -> ShopState {
        let view = ShopView { weekly: vec![offer("w0", 2, 1200, false), offer("w1", 3, 1500, false)], daily: (0..6).map(|i| offer(&format!("d{i}"), i % 5, 800 + 100 * i as i64, i == 4)).collect(), week_ends_in: 2 * 86_400 + 3_600, day_ends_in: 7_320 };
        ShopState { selected_tab: shop.item_shop_tab, mtx_balance: 1_000_000, view, ..Default::default() }
    }
    fn load() -> Option<(AthenaShopUi, UiContext)> {
        let install = crate::pak_assets::default_install();
        if !install.exists() { eprintln!("1.8 install not found: skipped"); return None; }
        Some((AthenaShopUi::load(&install).ok()?, UiContext::from_install(&install, None).ok()?))
    }

    #[test]
    fn number_and_countdown_formats() {
        assert_eq!(group_digits(1_000_000), "1,000,000");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(0), "0");
        assert_eq!(countdown_text(9 * 86_400 + 2 * 3_600 + 59), "9 Days");
        assert_eq!(countdown_text(86_400 + 5), "1 Day");
        assert_eq!(countdown_text(14 * 3_600 + 13 * 60 + 17), "14:13:17");
        assert_eq!(countdown_text(61), "0:01:01");
        assert_eq!(countdown_text(-5), "0:00:00");
    }

    #[test]
    fn purchase_button_states_follow_can_purchase_offer() {
        let o = offer("x", 3, 1500, false);
        assert_eq!(Affordability::of(&o, 1500), Affordability::Can);
        assert_eq!(Affordability::of(&o, 1499), Affordability::CannotAfford);
        assert_eq!(Affordability::of(&OfferView { owned: true, ..o }, 10), Affordability::Owned);
    }

    #[test]
    fn the_item_shop_tab_switches_to_the_direct_acquisition_screen() {
        let Some((mut ui, mut uic)) = load() else { return };
        // the registration the tab list was built from names the screen we compose
        assert_eq!(ui.tab_labels(), ["LOBBY", "LOCKER", "ITEM SHOP", "SEASON SHOP", "STATS", "LEADERBOARDS"]);
        assert_eq!(ui.item_shop_tab, 2);
        let tabs = ui.ui.store.pkg("/Game/Athena/UI/Frontend/AthenaTabsScreen").unwrap();
        let regs = tabs.named("TopTabList").unwrap().props["PreregisteredTabInfoArray"].as_array().unwrap().clone();
        let shop_reg = regs.iter().find(|r| r["TabNameId"] == "AthenaDirectAcquisition").expect("registration");
        assert_eq!(shop_reg["TabContentType"].as_str(), Some(SHOP_SCREEN));
        let st = state(&ui);
        let f = ui.draw(&mut uic, 1920.0, 1080.0, &st);
        assert!(f.missing.is_empty(), "{:?}", f.missing);
        // the section headers and the disclaimer come from the widget text (locres)
        let text = ui.dump(&mut uic, 1920.0, 1080.0, &st);
        assert!(text.contains("2 Days"), "weekly countdown set: {text}");
    }

    #[test]
    fn tiles_use_the_sizes_and_the_grid_order_of_the_blueprints() {
        let Some((mut ui, mut uic)) = load() else { return };
        let st = state(&ui);
        let f = ui.draw(&mut uic, 1920.0, 1080.0, &st);
        assert_eq!(f.offers.len(), 8);
        for (i, (_, r)) in f.offers.iter().enumerate() {
            // the tile widget is a CommonButton: SBTileSize + ButtonPadding 8 on every side (ButtonStyle-Solid-SquareM)
            let (w, h) = if i < 2 { (384.0 + 16.0, 608.0 + 16.0) } else { (256.0 + 16.0, 292.0 + 16.0) };
            assert!((r.w - w).abs() < 1.0 && (r.h - h).abs() < 1.0, "tile {i}: {r:?}");
        }
        // SmallTileGP: row = n % 2, column = n / 2 (GenerateOfferWidget)
        let d: Vec<Rect> = f.offers[2..].iter().map(|(_, r)| *r).collect();
        for n in 0..6 { assert!((d[n].x - d[n % 2].x - (n / 2) as f32 * (d[2].x - d[0].x)).abs() < 1.0, "col of {n}"); assert!(((n % 2 == 1) == (d[n].y > d[0].y + 1.0)), "row of {n}"); }
        // the weekly header sits above the large tiles, the daily header above the grid
        let wh = f.rect("BorderWeeklyItems").unwrap();
        let dh = f.rect("BorderDailyItems").unwrap();
        assert!(wh.y + wh.h <= f.offers[0].1.y + 1.0 && dh.y + dh.h <= f.offers[2].1.y + 1.0);
        assert!(f.offers[1].1.x + f.offers[1].1.w < f.offers[2].1.x, "weekly column left of the daily column");
        // everything fits the 1920 x 1080 layout
        assert!(f.offers.iter().all(|(_, r)| r.x >= 0.0 && r.x + r.w <= 1920.0 && r.y + r.h <= 1080.0));
        // hit testing
        let (id, r) = f.offers[4].clone();
        assert_eq!(f.hit(r.x + 3.0, r.y + 3.0), ShopHit::Offer(id));
        let t = f.tabs[0].1;
        assert_eq!(f.hit(t.x + 2.0, t.y + 2.0), ShopHit::Tab(0));
        assert_eq!(f.hit(1.0, 1070.0), ShopHit::None);
    }

    /// RGBA of the software raster at a pixel.
    fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] { let o = ((y * w + x) * 4) as usize; [px[o], px[o + 1], px[o + 2], px[o + 3]] }

    #[test]
    fn rarity_materials_are_parametrised_per_offer() {
        let Some((mut ui, mut uic)) = load() else { return };
        let st = state(&ui);
        let f = ui.draw(&mut uic, 1920.0, 1080.0, &st);
        let px = crate::raster::render(&f.list, &mut uic.assets, 1920, 1080, None);
        // the tile background (`BorderRarityColored`, M_Border_BattleLootTile_ColorBG_Dynamic with RGB_1..4 = RarityData colours) is Rare = blue for w0,
        // Epic = purple for w1 (no art in these offers): blue channel dominates in w0, red + blue in w1
        let w0 = f.offers[0].1;
        let c0 = pixel(&px, 1920, (w0.x + w0.w * 0.5) as u32, (w0.y + w0.h * 0.3) as u32);
        assert!(c0[3] > 200 && c0[2] > c0[0] + 40, "rare tile is blue: {c0:?}");
        let w1 = f.offers[1].1;
        let c1 = pixel(&px, 1920, (w1.x + w1.w * 0.5) as u32, (w1.y + w1.h * 0.3) as u32);
        assert!(c1[3] > 200 && c1[0] > c1[1] + 40 && c1[2] > c1[1] + 40, "epic tile is purple: {c1:?}");
        // the bottom strip carries the V-Bucks price: 1,200 for the first weekly offer
        let text = ui.dump(&mut uic, 1920.0, 1080.0, &st);
        assert!(text.contains("\"1,200\"") && text.contains("\"1,500\""), "prices shown with digit grouping: {text}");
        assert!(text.contains("\"NAME W0\""), "names are upper-cased (TextToUpper)");
    }

    #[test]
    fn details_and_purchased_widgets_compose_and_draw() {
        let Some((mut ui, mut uic)) = load() else { return };
        let mut st = state(&ui);
        let o = st.view.weekly[0].clone();
        st.dialog = Dialog::Details { offer: o.clone(), purchase_hovered: false };
        let f = ui.draw(&mut uic, 1920.0, 1080.0, &st);
        assert!(f.missing.is_empty(), "{:?}", f.missing);
        let buy = f.purchase_button.expect("purchase button");
        assert!(f.offers.is_empty() && f.back_button.is_some());
        assert_eq!(f.hit(buy.x + 4.0, buy.y + 4.0), ShopHit::Purchase);
        let dump = ui.dump(&mut uic, 1920.0, 1080.0, &st);
        assert!(dump.contains("\"NAME W0\"") && dump.contains("\"1,200\"") && dump.contains("text=\"PURCHASE\""), "{dump}");
        // not enough V-Bucks: the button reads "Get V-bucks" (SetupPrice: IsPriceInMTX)
        st.mtx_balance = 5;
        let dump = ui.dump(&mut uic, 1920.0, 1080.0, &st);
        assert!(dump.contains("text=\"Get V-bucks\""), "{dump}");
        // owned: the OwnedSwitcher shows the owned page
        st.dialog = Dialog::Details { offer: OfferView { owned: true, ..o.clone() }, purchase_hovered: false };
        let f = ui.draw(&mut uic, 1920.0, 1080.0, &st);
        assert!(f.missing.is_empty(), "{:?}", f.missing);
        st.dialog = Dialog::Purchased { offer: OfferView { owned: true, ..o } };
        let f = ui.draw(&mut uic, 1920.0, 1080.0, &st);
        assert!(f.missing.is_empty(), "{:?}", f.missing);
        assert!(f.purchase_button.is_none());
        let dump = ui.dump(&mut uic, 1920.0, 1080.0, &st);
        assert!(dump.contains("\"NAME W0\""), "{dump}");
    }
}
