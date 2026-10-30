//! The Fortnite 1.8 Battle Royale LOCKER tab: `/Game/Athena/UI/Customization/AthenaCustomizationTab` (UMG) composed by the generic interpreter, plus the little
//! the 1.8 client does natively around it. Sources (all decoded, nothing typed in):
//! * the tab list switches `TopTabContentWidgetSwitcher` to the content class of the registration (`AthenaTabsScreen.PreregisteredTabInfoArray[2].TabContentType`
//!   = `AthenaCustomizationTab_C`); `AthenaCustomizationTab::OnActivated` (Kismet) calls `SetFrontEndCamera(16)` (Cosmetics) and `SelectFirstWidget`;
//! * slots: `CharacterSlot` -> `BeginPickingCustomization(5)`, `PickaxeSlot` -> 2, `GliderSlot` -> 1 (bound `CommonButtonClicked` events); the header text
//!   (`HeaderItemType`) switches 5 "OUTFIT" / 2 "HARVESTING TOOL" / 1 "GLIDER" (FTexts of the Kismet, locres ns [FD9A3096...]); `IntroPicker` (WidgetAnimation, 0.25 s)
//!   slides `SizeBox_72` (the picker) from `Anchors (-1,0)-(-0.5,1)` to `(0,0)-(0.5,1)` and fades `Border_0`'s content (the slot panel) out; the reverse plays on
//!   `EndPickingCustomization`; `HandleCancel` (input action "Cancel") ends picking; a saved selection (`OnCosmeticChangeSaveEvent`) ends picking;
//! * the picker is `AthenaCustomizationPicker_C`: a `FortItemTileView` (`ListItemClass = AthenaCustomizationPickerTileButton_C`, `DisplayType = Account`) that the
//!   native code fills with the account's items of the category; the tile button shows `ItemWidget` (`FortMultiSizeItemCard`, `BPItemCardSize = L`, native) when it has an
//!   item and is `bSelectable` (the equipped item is the selected tile); button sounds are the serialized `Hovered/PressedSlateSoundOverride`.
//! Native glue in this module (each piece says what it stands in for): the item cards (`card.rs`), the tile view grid (`grid`), slot / tile state from [`LockerView`].
//! Offline there is no account profile: `LockerView::items` is supplied by the caller (the offline profile's owned items).
pub mod anim;
pub mod banner;
pub mod card;
pub mod controller;
pub mod hit;
#[cfg(test)]
mod tests;

use crate::layout::Rect;
use crate::screens::athena_lobby::layout as ui_layout;
use crate::screens::athena_lobby::styles::BtnState;
use crate::screens::athena_lobby::umg::{Ui, Vis};
use crate::scene::UiContext;
use serde_json::json;

pub const TAB_CLASS: &str = "/Game/Athena/UI/Customization/AthenaCustomizationTab.AthenaCustomizationTab_C";
pub const TILE_CLASS: &str = "/Game/Athena/UI/Customization/AthenaCustomizationPickerTileButton.AthenaCustomizationPickerTileButton_C";
/// namespace of the header FTexts in `AthenaCustomizationTab::BeginPickingCustomization`
pub const HEADER_NS: &str = "[FD9A309645763A0867C778A2C7A28A92]";
/// `IntroPicker` playback range end (seconds)
pub const INTRO_PICKER_END: f32 = 0.25;

/// `AthenaCustomizationSlotSelectorButton` / `AthenaCustomizationPickerTileButton` (native roots of the slot / tile Blueprints) are `UCommonButton` subclasses: their Blueprint CDOs carry the
/// CommonButton properties (`Style`, `bSelectable`, `bInteractableWhenSelected`, `HoveredSlateSoundOverride`, `PressedSlateSoundOverride`) but the package chain only reaches the native
/// class. Declare the base so the interpreter lays them out / paints them as buttons.
fn mark_common_button(ui: &mut Ui, id: usize) {
    if let Some(u) = ui.w[id].user.as_mut() { if !u.chain.iter().any(|c| c == "/Script/CommonUI.CommonButton") { u.chain.push("/Script/CommonUI.CommonButton".into()); } }
}

/// The three customization slots of the 1.8 Locker (the fourth, `BannerSlot`, is account banner data).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot { Outfit = 0, Pickaxe = 1, Glider = 2 }
impl Slot {
    pub const ALL: [Slot; 3] = [Slot::Outfit, Slot::Pickaxe, Slot::Glider];
    pub fn index(self) -> usize { self as usize }
    /// widget name of the slot button in `AthenaCustomizationTab`
    pub fn widget(self) -> &'static str { match self { Slot::Outfit => "CharacterSlot", Slot::Pickaxe => "PickaxeSlot", Slot::Glider => "GliderSlot" } }
    /// `Category` byte of `BeginPickingCustomization` (Kismet of the tab)
    pub fn category(self) -> u8 { match self { Slot::Outfit => 5, Slot::Pickaxe => 2, Slot::Glider => 1 } }
    /// (key, source text) of the `HeaderItemType` FText of this category (`BeginPickingCustomization`, namespace [`HEADER_NS`])
    pub fn header_key(self) -> &'static str {
        match self { Slot::Outfit => "F9C190C54F7F9256EEED0989F7CA3DEF", Slot::Pickaxe => "9BB26C55451788ABBB11618932A3231F", Slot::Glider => "54F896CB4486D3EE69F504B649E975DA" }
    }
}

/// One item the picker can show: what `GetSmallPreviewImage` / `Rarity` / `DisplayName` of the item definition give.
#[derive(Clone, Debug, PartialEq)]
pub struct LockerItem {
    /// stable id (the asset name, e.g. `CID_030_Athena_Commando_M_Halloween`)
    pub id: String,
    pub name: String,
    /// row of `/Game/Balance/RarityData` (`RarityCollection[n]`)
    pub rarity: usize,
    /// `/Game/..Name.Name` of the preview texture (the card's `Icon`)
    pub icon: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hover { Slot(Slot), Tile(Slot, usize) }

/// Runtime state of the Locker tab (everything else is the widget data).
#[derive(Clone, Debug, Default)]
pub struct LockerView {
    /// items the picker lists per slot (the offline profile's owned items)
    pub items: [Vec<LockerItem>; 3],
    /// index into `items[slot]` of the equipped item
    pub equipped: [Option<usize>; 3],
    /// the category being picked (`PickingCosmetic`)
    pub picking: Option<Slot>,
    /// `IntroPicker` playback position in seconds (0 = closed, [`INTRO_PICKER_END`] = open)
    pub picker_t: f32,
    /// first visible row of the picker grid
    pub scroll_row: usize,
    pub null_tile: [bool; 3],
    pub selected: [Option<usize>; 3],
    pub banner_icon: String,
    pub banner_color: String,
    pub banner_editor: Option<banner::EditorView>,
}
impl LockerView {
    pub fn picker_visible(&self) -> bool { self.picking.is_some() || self.picker_t > 0.0 }
}

/// Widgets of the composed tab.
pub struct LockerWidgets {
    pub tab: usize,
    pub slots: [usize; 3],
    slot_cards: [Option<usize>; 3],
    slot_empty: [Option<usize>; 3],
    picker: Option<usize>,
    tile_view: Option<usize>,
    header: Option<usize>,
    /// widgets animated by `IntroPicker`
    tracks: Vec<anim::Track>,
    anim_widgets: Vec<Option<usize>>,
    /// pooled tile buttons per slot: (button, its `ItemWidget`)
    tiles: [Vec<(usize, Option<usize>)>; 3],
    /// pooled-tile geometry cache: desired size of one tile (Slate units)
    tile_size: Option<[f32; 2]>,
    /// cards to paint this frame (rebuilt by `apply`)
    pub cards: Vec<card::CardSpec>,
    /// grid metrics of the last `apply`: (columns, visible rows, total rows)
    pub grid: (usize, usize, usize),
    pub log: Vec<String>,
    tile_offset: [usize; 3],
    tv_size: std::cell::Cell<Option<[f32; 2]>>,
    pub banner_slot: Option<usize>,
    pub banner: banner::BannerUi,
    scale_hint: std::cell::Cell<f32>,
}

impl LockerWidgets {
    /// Compose `AthenaCustomizationTab_C` as a page of `switcher` (the way `FortTabListWidgetBase` creates the `TabContentType` of a registration).
    pub fn install(ui: &mut Ui, switcher: usize) -> Option<LockerWidgets> {
        let tab = ui.add_class_instance(TAB_CLASS, Some(switcher), "WidgetSwitcherSlot", json!({}), None)?;
        let slots = Slot::ALL.map(|s| ui.find_in(tab, s.widget()).unwrap_or(tab));
        for s in slots { mark_common_button(ui, s); }
        let slot_cards = slots.map(|s| ui.find_in(s, "ItemWidget"));
        let slot_empty = slots.map(|s| ui.find_in(s, "EmptyImage"));
        let picker = ui.find_in(tab, "SelectionPicker");
        let tile_view = ui.find_in(tab, "PickerTileView");
        let header = ui.find_in(tab, "HeaderItemType");
        let banner_slot = ui.find_in(tab, "BannerSlot");
        if let Some(b) = banner_slot { mark_common_button(ui, b); }
        let banner = banner::BannerUi::load(&ui.store.clone());
        let mut log = vec![];
        // the native item card gets its size from `BPItemCardSize` / `SlottedItemCardSize`: it is a leaf with a fixed desired size (see `card`)
        let mut lw = LockerWidgets { tab, slots, slot_cards, slot_empty, picker, tile_view, header, tracks: vec![], anim_widgets: vec![], tiles: Default::default(), tile_size: None, cards: vec![], grid: (1, 1, 0), log: vec![], tile_offset: [0; 3], tv_size: std::cell::Cell::new(None), banner_slot, banner, scale_hint: std::cell::Cell::new(1.0) };
        for (i, c) in slot_cards.iter().enumerate() {
            if let Some(c) = *c {
                let size = ui.w[slots[i]].props.get("SlottedItemCardSize").and_then(|v| v.as_str()).map(card::size_name).unwrap_or("L");
                card::make_card_leaf(ui, c, size);
            }
        }
        if let Some(pkg) = ui.w[tab].pkg.clone().into() {
            let pkg: std::rc::Rc<crate::screens::athena_lobby::store::Pkg> = pkg;
            match anim::load(&pkg, "IntroPicker") {
                Some((tracks, _end)) => {
                    lw.anim_widgets = tracks.iter().map(|t| ui.find_in(tab, &t.widget)).collect();
                    lw.tracks = tracks;
                }
                None => log.push("IntroPicker animation not found".into()),
            }
        }
        lw.log = log;
        Some(lw)
    }

    pub fn apply_banner(&mut self, ui: &mut Ui, uic: &mut UiContext, view: &LockerView, scale: f32) {
        self.scale_hint.set(scale);
        let icon = if view.banner_icon.is_empty() { banner::DEFAULT_ICON } else { view.banner_icon.as_str() };
        let color = if view.banner_color.is_empty() { banner::DEFAULT_COLOR } else { view.banner_color.as_str() };
        self.banner.apply(ui, uic, view.banner_editor.as_ref(), (icon, color), scale);
        self.banner.apply_banners(ui, icon, color, view.banner_editor.as_ref());
    }

    pub fn is_open_anim_widget(&self, id: usize) -> bool { self.anim_widgets.contains(&Some(id)) }

    /// Slot button widget of `slot`.
    pub fn slot_widget(&self, slot: Slot) -> usize { self.slots[slot.index()] }
    /// `Some((slot, item index))` for a pooled tile button.
    pub fn tile_of(&self, id: usize) -> Option<(Slot, usize)> {
        for s in Slot::ALL {
            if let Some(i) = self.tiles[s.index()].iter().position(|t| t.0 == id) {
                let off = self.tile_offset[s.index()];
                return Some((s, if i < off { usize::MAX } else { i - off }));
            }
        }
        None
    }
    pub fn slot_of(&self, id: usize) -> Option<Slot> { Slot::ALL.into_iter().find(|s| self.slots[s.index()] == id) }
    pub fn tile_view(&self) -> Option<usize> { self.tile_view }

    /// Tile button of `(slot, index)`.
    pub fn tile_button(&self, slot: Slot, i: usize) -> Option<usize> {
        let t = if i == usize::MAX { 0 } else { i + self.tile_offset[slot.index()] };
        self.tiles[slot.index()].get(t).map(|t| t.0)
    }
    pub fn tile_offset(&self, slot: Slot) -> usize { self.tile_offset[slot.index()] }

    fn ensure_tiles(&mut self, ui: &mut Ui, slot: Slot, n: usize) {
        let Some(tv) = self.tile_view else { return };
        while self.tiles[slot.index()].len() < n {
            let Some(t) = ui.add_class_instance(TILE_CLASS, Some(tv), "", json!({}), None) else { self.log.push("cannot compose a picker tile".into()); return };
            mark_common_button(ui, t);
            let card_w = ui.find_in(t, "ItemWidget");
            if let Some(c) = card_w { card::make_card_leaf(ui, c, "M"); }
            // `UpdateBangState`: no bang (account item "seen" state is runtime data)
            self.tiles[slot.index()].push((t, card_w));
        }
    }

    /// Slate-unit viewport -> canvas slot of the picker: `CanvasPanelSlot` anchors / offsets expressed as the equivalent `OverlaySlot` padding (the layout of the
    /// generic interpreter has no canvas panel; the slot's anchors are the values `IntroPicker` animates).
    fn place_canvas_child(ui: &mut Ui, id: usize, viewport: [f32; 2], min: [f32; 2], max: [f32; 2], offs: [f32; 4]) {
        let (l, t) = (min[0] * viewport[0] + offs[0], min[1] * viewport[1] + offs[1]);
        let (r, b) = (max[0] * viewport[0] - offs[2], max[1] * viewport[1] - offs[3]);
        ui.w[id].slot_class = "OverlaySlot".into();
        ui.w[id].slot = json!({ "Padding": { "Left": l, "Top": t, "Right": (viewport[0] - r).max(0.0), "Bottom": (viewport[1] - b).max(0.0) }, "HorizontalAlignment": "HAlign_Fill", "VerticalAlignment": "VAlign_Fill" });
    }

    /// Push the runtime state into the widgets (call before layout every frame). `viewport` = viewport in Slate units, `scale` = DPI scale.
    pub fn apply(&mut self, ui: &mut Ui, uic: &mut UiContext, view: &LockerView, viewport: [f32; 2], scale: f32) {
        self.cards.clear();
        // ---- IntroPicker at its current position ----
        let t = view.picker_t.clamp(0.0, INTRO_PICKER_END);
        let (mut amin, mut amax) = ([0.0f32; 2], [0.0f32; 2]);
        let mut slot_anim = None;
        for (tr, id) in self.tracks.iter().zip(self.anim_widgets.clone()) {
            let Some(id) = id else { continue };
            let v = tr.value(t);
            match tr.kind {
                anim::TrackKind::AnchorMin => { amin = [v[0], v[1]]; slot_anim = Some(id); }
                anim::TrackKind::AnchorMax => { amax = [v[0], v[1]]; slot_anim = Some(id); }
                anim::TrackKind::ContentColor => { ui.w[id].props["ContentColorAndOpacity"] = json!([v[0], v[1], v[2], v[3]]); }
                anim::TrackKind::Color => { ui.w[id].props["ColorAndOpacity"] = json!([v[0], v[1], v[2], v[3]]); }
            }
        }
        if let Some(id) = slot_anim {
            // CanvasPanelSlot_0 of SizeBox_72: `Offsets` = (32, 32, 32, 32) in the data (`LayoutData`)
            Self::place_canvas_child(ui, id, viewport, amin, amax, [32.0; 4]);
        }
        // header text of the category (BeginPickingCustomization)
        if let (Some(h), Some(slot)) = (self.header, view.picking) {
            let txt = ui.text_of(&json!({ "key": slot.header_key(), "namespace": HEADER_NS, "text": "" })).unwrap_or_default();
            ui.w[h].text = Some(txt);
        }
        // ---- slots: ShouldShowEmptyImage = !IsValid(ItemToRepresent); the card shows the equipped item ----
        for s in Slot::ALL {
            let i = s.index();
            let item = view.equipped[i].and_then(|k| view.items[i].get(k));
            if let Some(e) = self.slot_empty[i] { ui.w[e].vis = if item.is_some() { Vis::Collapsed } else { Vis::Visible }; }
            if let Some(c) = self.slot_cards[i] {
                ui.w[c].vis = Vis::Visible;
                if let Some(item) = item { self.cards.push(card::CardSpec { widget: c, slot: s, item: item.clone(), clip: None }); }
            }
        }
        // ---- picker tiles of the category being picked (native FortItemTileView fill) ----
        for s in Slot::ALL { if Some(s) != view.picking { for t in &self.tiles[s.index()] { ui.w[t.0].vis = Vis::Collapsed; } } }
        let (Some(slot), Some(tv)) = (view.picking, self.tile_view) else { return };
        let items = &view.items[slot.index()];
        let off = view.null_tile[slot.index()] as usize;
        self.tile_offset[slot.index()] = off;
        self.ensure_tiles(ui, slot, items.len() + off);
        // tile geometry: the desired size of one tile (card + button padding), one pass with a scratch layout
        if self.tile_size.is_none() {
            if let Some(&(t0, _)) = self.tiles[slot.index()].first() {
                ui.w[t0].vis = Vis::Visible;
                let mut c = ui_layout::Ctx { ui, uic, lay: ui_layout::new_layout(0, scale) };
                c.lay = ui_layout::new_layout(c.ui.w.len(), scale);
                let d = c.desired(t0);
                self.tile_size = Some(d);
            }
        }
        let tsz = self.tile_size.unwrap_or([128.0, 128.0]);
        // the grid area: PickerTileView = picker rect minus the CommonBorder padding (24 / 72 / 24 / 24 of `AthenaCustomizationPicker.CommonBorder_0`) inside the
        // canvas slot (offsets 32) at its open position
        let measured = self.tv_size.get().map(|v| [v[0] / scale.max(1e-3), v[1] / scale.max(1e-3)]);
        let pw = measured.map_or((1.0 * viewport[0] * 0.5 - 64.0).max(1.0) - 48.0, |m| m[0]);
        let ph = measured.map_or(viewport[1] - 64.0 - 72.0 - 24.0, |m| m[1]);
        let cols = ((pw / tsz[0]).floor() as usize).max(1);
        let vis_rows = ((ph / tsz[1]).floor() as usize).max(1);
        let count = items.len() + off;
        let total_rows = count.div_ceil(cols);
        let pad = if count > cols { (pw - 0.001 - cols as f32 * tsz[0]) / cols as f32 } else { 0.0 };
        self.grid = (cols, vis_rows, total_rows);
        let first = view.scroll_row.min(total_rows.saturating_sub(vis_rows));
        // one `OverlaySlot` placement per tile: whole rows only, so nothing needs clipping
        ui.w[tv].class = "Overlay".into();
        for t in 0..count.min(self.tiles[slot.index()].len()) {
            let (btn, cardw) = self.tiles[slot.index()][t];
            let (row, col) = (t / cols, t % cols);
            let shown = row >= first && row < first + vis_rows;
            ui.w[btn].vis = if shown { Vis::Visible } else { Vis::Collapsed };
            ui.w[btn].slot_class = "OverlaySlot".into();
            ui.w[btn].slot = json!({ "Padding": { "Left": col as f32 * (tsz[0] + pad) + pad * 0.5, "Top": (row - first.min(row)) as f32 * tsz[1], "Right": 0.0, "Bottom": 0.0 }, "HorizontalAlignment": "HAlign_Left", "VerticalAlignment": "VAlign_Top" });
            if t < off {
                let (l, tp) = (col as f32 * (tsz[0] + pad) + pad * 0.5, (row - first.min(row)) as f32 * tsz[1]);
                let [tw, th] = self.tv_size.get().map(|v| [v[0] / scale.max(1e-3), v[1] / scale.max(1e-3)]).unwrap_or([pw, ph]);
                ui.w[btn].slot = json!({ "Padding": { "Left": l, "Top": tp, "Right": (tw - l - tsz[0]).max(0.0), "Bottom": (th - tp - tsz[1]).max(0.0) }, "HorizontalAlignment": "HAlign_Fill", "VerticalAlignment": "VAlign_Fill" });
                ui.w[btn].btn_state = if view.selected[slot.index()].is_none() { BtnState::Selected } else { BtnState::Normal };
                if let Some(rw) = ui.find_in(btn, "RootWidgetSwitcher") { ui.w[rw].props["ActiveWidgetIndex"] = json!(1); }
                if let Some(c) = cardw { ui.w[c].vis = Vis::Collapsed; }
                continue;
            }
            let i = t - off;
            let item = &items[i];
            // tile state: the equipped item is the selected tile (`bSelectable`)
            ui.w[btn].btn_state = if view.selected[slot.index()] == Some(i) { BtnState::Selected } else { BtnState::Normal };
            if let Some(rw) = ui.find_in(btn, "RootWidgetSwitcher") { ui.w[rw].props["ActiveWidgetIndex"] = json!(0); }
            if let Some(c) = cardw { ui.w[c].vis = Vis::Visible; if shown { self.cards.push(card::CardSpec { widget: c, slot, item: item.clone(), clip: None }); } }
        }
    }

    /// Paint the item cards (call after the generic paint, with the pixel rectangle of each widget by id).
    pub fn paint_cards(&self, ui: &Ui, uic: &mut UiContext, mats: &mut card::CardMats, out: &mut crate::draw::RenderList, rects: &[Rect], missing: &mut Vec<String>) {
        if let Some(r) = self.tile_view.and_then(|tv| rects.get(tv)) { if r.w > 0.0 && r.h > 0.0 { self.tv_size.set(Some([r.w, r.h])); } }
        self.banner.note_tile_view_rect(self.banner.tile_view().and_then(|tv| rects.get(tv).copied()), self.scale_hint.get());
        for spec in &self.cards {
            if !card::effectively_visible(ui, spec.widget) { continue; }
            let Some(r) = rects.get(spec.widget).copied() else { continue };
            if r.w <= 0.0 || r.h <= 0.0 { continue; }
            // the slot panel's cards fade with `Border_0`'s content colour (IntroPicker); tiles are inside the picker
            let alpha = card::content_alpha(ui, spec.widget);
            if alpha <= 0.0 { continue; }
            card::draw_card(ui, uic, mats, out, r, spec, alpha, missing);
        }
    }
}
