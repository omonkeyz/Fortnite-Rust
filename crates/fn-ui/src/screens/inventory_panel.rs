//! `AthenaInventoryPanel_C` (`/Game/Athena/HUD/Inventory/AthenaInventoryPanel`): the full inventory screen of the Athena HUD (the `ToggleInventory` binding,
//! cooked default `I`), as state + input -> [`RenderList`].
//!
//! INVENTORY FIX2: the screen is DRAWN from the cooked widget trees (`screens::inventory_cooked`: `AthenaInventoryPanel_C` with every nested widget, the
//! `AthenaInventoryFortItemTileButton_C` entries, the `QuantitySelector_C` modal), composed / laid out / painted by the generic UMG runtime; nothing is
//! recreated here. Without the install nothing is drawn (reported once). This file keeps the panel's state and input (selection, drag & drop, the
//! modal), the native tile-view fill (filter + sorter of `UAthenaInventoryPanelBase`) and the actions the host runs on its inventory.
//!
//! SOURCES of the behaviour (Kismet via `fn-inspect disasm`, natives via `C:\FortniteRef\decomp\named`):
//! * the tile view content: `FUN_142db1690` (the panel's filter / sorter refresh) = `UFortItemTileView::SetFilterAndSorter` with the item-type filter
//!   {WeaponMelee 16, WeaponRanged 17, Ammo 1, Trap 14, WorldResource 19} (the pickaxe, WeaponHarvest 18, is not a backpack tile) and the sorter
//!   `FUN_1430383a0(context, SortType, false)` whose world-item comparison is `FUN_1406334e0` ([`compare_items`]);
//! * `SortType` (+0x408) defaults to 8 = BySubtype (`UAthenaInventoryPanelBase` constructor 0x142dada70); `GetSupportedSortTypesForCurrentInventory`
//!   (0x142dc3ba0) = [ByRating, ByRarity, ByLevel, ByName, BySubtype]; `CycleSortType` (0x142dba420) steps through them (nothing in the 1.8 Blueprint calls it);
//! * selection: `OnInventoryItemSelected` -> `AthenaHUDContext.SetSelectedInventoryItem` + `ItemDetails.SetItemToDetail`; `Equip` action = `PerformEquip`;
//!   double click = `HandleInventoryItemDoubleClicked` (0x142dc7da0) -> `RequestEquip`;
//! * drags: equip slot `OnDragDetected` (Tag "FromQuickbar", no split flag), tile `OnDragDetected` (`SplitTheStack = InputEvent_IsShiftDown`); drop slot
//!   `OnDrop`: `DropItem(item, SplitTheStack || IsShiftDown(drop event) ? max(stack / 2, 1) : stack)`; equip slot `OnDrop` -> `DropItemOnQuickBar(item, 0, SlotIndex)`;
//! * `HandleDrop` (X): stack > 1 -> `CreateQuantitySelectorWidget(item, "Drop")` -> `QuantitySelector.SetSplitData(stack / 2, stack, item, "Drop Items", "Drop")`
//!   in the modal layer; confirm -> `HandleDropItemConfirmationCallback` -> `DropItem(item, CurrentValue)`; cancel (row `Cancel`, PC key Escape) -> `DropItem(item, 0)`;
//!   `DropItem` advances the selection first when the whole stack goes (`AdvanceSelection` 0x142db5fb0: next tile, else the previous one).
use crate::assets::TexInfo;
use crate::draw::{Cmd, MaterialKind, RenderList, TextureId};
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::athena_lobby::styles::BtnState;
use crate::screens::inventory_cooked::{self as cooked, CookedInventory, DrawState, Geo, QselButton, QselState, TileState};
use crate::screens::quickbar::RARITY;
use crate::screens::{atlas_px, centred_baseline, line_height, mat_tex, put_text, text_width, Snap, WHITE};
use crate::text::FontId;

// ---- fallback geometry (hit tests while no cooked frame was drawn, unit tests): the cooked layout at 1920 x 1080 (Slate units, scale 1) -------------
pub const HEADER_H: f32 = 40.; // HorizontalBox_1: desired height of the title row (SizeBox_2 HeightOverride 48 is NOT applied: bOverride_HeightOverride is unset)
pub const COLUMN_MIN_W: f32 = 1100.; // SizeBox_22 MinDesiredWidth
pub const DETAILS_W: f32 = 700.; // SizeBox_1 MinDesiredWidth
pub const DETAILS_PAD: (f32, f32, f32) = (12., 16., 4.); // VerticalBox_1 slot padding Left, Top, Bottom
pub const COLUMN_TOP_PAD: f32 = 16.; // VerticalBox_2 slot padding Top
pub const EQUIP_BOX: f32 = 130.; // AthenaInventoryEquipSlot SizeBox_8
pub const EQUIP_SLOT_PAD: f32 = 4.; // EquipSlots HorizontalBoxSlot Padding L/R
pub const EQUIP_OUTER_PAD: f32 = 40.; // CommonBorderEquipSlots slot padding bottom
pub const DROP_BAND_MIN_H: f32 = 175.; // SizeBox_4 MinDesiredHeight; the drop slot user widget fills it (SizeBoxSlot Fill: its SizeBox_8 118 x 118 only sets the desired size)
pub const TILE_BOTTOM_PAD: f32 = 8.; // TileViewLoadGuard slot padding Bottom
/// `FortItemTileView` `ItemWidth` / `ItemHeight` of the cooked `TileView` (`DesiredItemPadding` is the `UCommonListView` default 0): the cell of one tile.
pub const TILE_W: f32 = 254.;
pub const TILE_H: f32 = 439.;
pub const BACKPACK_LINE: (f32, f32, f32, f32) = (3., 4., 8., 8.); // Image_2 height, padding Top, Bottom, Right
pub const DRAG_START_UNITS: f32 = 5.; // UE default drag trigger distance
/// `RarityData` `RarityCollection[N].Name` (source strings; the cooked tree shows the localized ones).
pub const RARITY_NAMES: [&str; 5] = ["Common", "Uncommon", "Rare", "Epic", "Legendary"];
/// The tile view filter (`FUN_142db1690`): WeaponMelee, WeaponRanged, Ammo, Trap, WorldResource.
pub const TILE_TYPES: [u8; 5] = [16, 17, 1, 14, 19];

pub fn scale_to_fit(r: Rect, w: f32, h: f32) -> Rect {
    if w <= 0. || h <= 0. { return r; }
    let s = (r.w / w).min(r.h / h);
    let (dw, dh) = (w * s, h * s);
    Rect::new(r.x + (r.w - dw) * 0.5, r.y + (r.h - dh) * 0.5, dw, dh)
}

/// One inventory entry as the panel shows it (the game side fills it from its live inventory, see `fn-app/src/inventory.rs`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct InvItem {
    pub id: u32,
    /// Item definition `DisplayName` / `Description` texts.
    pub name: String,
    pub description: String,
    /// 0 Common .. 4 Legendary (`RarityData` order).
    pub rarity: u8,
    /// PNG name of the `SmallPreviewImage` texture.
    pub icon: String,
    pub count: u32,
    /// `EFortItemType` number (declaration order of the exe's enum name table).
    pub item_type: u8,
    /// `IsEquipable(controller)` and "matches the search string `Resource`" (the two inputs of the `Equip` action's enabled state).
    pub equipable: bool,
    pub is_resource: bool,
    /// `FortWorldItem::CanBeDropped()`.
    pub can_be_dropped: bool,
    /// Equip slot 1..=5 (primary quickbar slot index + 0 ... 1..5; the pickaxe slot 0 is not an equip slot) holding this item.
    pub slot: Option<u8>,
    /// INVENTORY FIX2: object path of the item definition (its cooked `GameplayTags` feed `GetUserFriendlyTags`); empty when unknown.
    pub def_path: String,
}

/// Everything the panel shows: the entries and the backpack counter (`GetBackpackItemCounts`: items + overflow, max).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InventoryView { pub items: Vec<InvItem>, pub backpack: (u32, u32) }

/// What the panel asks the host to do (the host owns the inventory: the native context calls are not part of the screen).
#[derive(Clone, Debug, PartialEq)]
pub enum PanelAction {
    /// A tile was clicked: `AthenaHUDContext.SetSelectedInventoryItem` + details.
    Select(u32),
    /// The `Equip` input action on the selected item: the host runs `PerformEquip` (`fn_weapons::recovered_panel::perform_equip`).
    Equip(u32),
    /// `AthenaInventoryEquipSlot.OnDrop`: `DropItemOnQuickBar(item, 0, slot)` with `slot` = SlotIndex 1..=5.
    DropOnSlot { item: u32, slot: u8 },
    /// `AthenaInventoryDropSlot.OnDrop`: `DropItem(item, quantity)`; `split` = `SplitTheStack || IsShiftDown` (the host applies `drag_drop_quantity`).
    Drop { item: u32, split: bool },
    /// `HandleClose` (`EnterCameraMode`): Back / LeaveInventory.
    Close,
    /// The panel's `DropItem(item, quantity)`: X on a stack of 1, or the quantity selector's confirm (`CurrentValue`) / cancel (0).
    DropQuantity { item: u32, quantity: u32 },
}

/// A point of the panel a script can aim the cursor at (`--script` inventory actions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelTarget { EquipSlot(u8), Tile(usize), DropSlot, QselConfirm, QselCancel, QselMax }

/// Windows double-click rules that produce Slate's `OnMouseButtonDoubleClick` (`WM_LBUTTONDBLCLK`): `GetDoubleClickTime` default 500 ms, `SM_CXDOUBLECLK` / `SM_CYDOUBLECLK` default 4 px.
pub const DOUBLE_CLICK_S: f64 = 0.5;
pub const DOUBLE_CLICK_PX: f32 = 4.;
/// `InputActionDataTable` rows (`PlatformSpecificKeys[0]`, the PC key) the panel registers in `TrySetupInputHandling`: `Back` = Escape, `Equip` / `Unequip` = E, `Drop` = X;
/// the selector's `Cancel` = Escape (`AcceptQuantity` / `MaxAmount` have no PC key: mouse only).
pub const KEY_EQUIP: &str = "E";
pub const KEY_DROP: &str = "X";
pub const KEY_BACK: &str = "Escape";

/// `EInventoryContentSortType` (enum construction 0x140aaaa00, declaration order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortType { ByName = 0, ByRating = 1, ByLevel = 2, ByCategory = 3, ByRarity = 4, ByLocation = 5, ByPersonality = 6, ByBonus = 7, BySubtype = 8 }
/// `GetSupportedSortTypesForCurrentInventory` (0x142dc3ba0): bytes {1, 4, 2, 0, 8}.
pub const SUPPORTED_SORTS: [SortType; 5] = [SortType::ByRating, SortType::ByRarity, SortType::ByLevel, SortType::ByName, SortType::BySubtype];

/// The subtype key of `FUN_140632c50` (item vfunc 100 -> definition vfunc 97 -> the type map `FUN_140687180`).
pub fn subtype_key(item_type: u8) -> u8 {
    match item_type { 1 => 2, 6 => 7, 8 | 19 => 5, 9 => 4, 10..=12 => 6, 14 => 3, 16 | 18 => 0, 17 => 1, 21 => 0xd, 27 => 9, 41 => 0xc, 42 => 0xe, _ => 0xf }
}
/// `FUN_1406334e0` (two `UFortWorldItem`s): the key of `sort` first, then rating (desc), rarity (desc), level (desc), name (asc), subtype, a final string key.
/// Recovered keys: subtype (`FUN_140632c50`: the subtype map, then the item type, then a definition tag name), rarity (`FUN_14068be00`: definition rarity),
/// name (`FUN_140632a60`: ordinal compare of the display names). NOT recovered for BR world items (treated equal): rating (`GetRatingAtLevel`), level
/// (`FUN_14068d3b0` + vfunc 70), the tag-name key of the subtype compare and the final key (`FUN_1406322d0`); the item id keeps the order stable.
pub fn compare_items(a: &InvItem, b: &InvItem, sort: SortType) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let subtype = |x: &InvItem, y: &InvItem| (subtype_key(x.item_type), x.item_type).cmp(&(subtype_key(y.item_type), y.item_type));
    let rarity = |x: &InvItem, y: &InvItem| y.rarity.cmp(&x.rarity);
    let name = |x: &InvItem, y: &InvItem| { let (p, q): (Vec<u16>, Vec<u16>) = (x.name.encode_utf16().collect(), y.name.encode_utf16().collect()); p.cmp(&q) };
    let first = match sort { SortType::ByName => name(a, b), SortType::ByRarity => rarity(a, b), SortType::BySubtype => subtype(a, b), _ => Ordering::Equal };
    first.then_with(|| if sort != SortType::ByRarity { rarity(a, b) } else { Ordering::Equal })
        .then_with(|| if sort != SortType::ByName { name(a, b) } else { Ordering::Equal })
        .then_with(|| if sort != SortType::BySubtype { subtype(a, b) } else { Ordering::Equal })
        .then_with(|| a.id.cmp(&b.id))
}

#[derive(Clone, Copy, Debug)]
struct Press { item: u32, at: (f32, f32), shift: bool, from_slot: bool }
/// `from_slot` = the operation's Tag "FromQuickbar" (set by `AthenaInventoryEquipSlot.OnDragDetected`).
#[derive(Clone, Copy, Debug)]
struct Drag { item: u32, split: bool, from_slot: bool }

/// Fallback geometry in pixels (see the constants).
#[derive(Clone, Debug)]
pub struct Layout { pub scale: f32, pub panel_left: f32, pub equip: [Rect; 5], pub backpack_line: Rect, pub tiles: Rect, pub drop: Rect }

/// Fallback geometry for a client size. `backpack_text_h` = line height of the backpack label in pixels.
pub fn layout(w: f32, h: f32, backpack_text_h: f32) -> Layout {
    let s = dpi_scale(w, h);
    let (sw, sh) = (w / s, h / s);
    let panel_w = (COLUMN_MIN_W + DETAILS_PAD.0 + DETAILS_W).min(sw);
    let left = ((sw - panel_w) * 0.5) * s;
    let col_w = (panel_w - DETAILS_PAD.0 - DETAILS_W) * s;
    let top = (HEADER_H + COLUMN_TOP_PAD) * s;
    let row_w = 5. * (EQUIP_BOX + 2. * EQUIP_SLOT_PAD) * s;
    let eq_x = left + (col_w - row_w) * 0.5;
    let equip: [Rect; 5] = std::array::from_fn(|i| Rect::new(eq_x + (i as f32 * (EQUIP_BOX + 2. * EQUIP_SLOT_PAD) + EQUIP_SLOT_PAD) * s, top, EQUIP_BOX * s, EQUIP_BOX * s));
    let bp_y = top + (EQUIP_BOX + EQUIP_OUTER_PAD) * s;
    let backpack_line = Rect::new(left, bp_y + backpack_text_h + BACKPACK_LINE.1 * s, col_w - BACKPACK_LINE.3 * s, BACKPACK_LINE.0 * s);
    let band_y = (sh - DROP_BAND_MIN_H) * s;
    let drop = Rect::new(left, band_y, col_w, DROP_BAND_MIN_H * s);
    let tiles_y = backpack_line.bottom() + BACKPACK_LINE.2 * s;
    let tiles = Rect::new(left, tiles_y, col_w, (band_y - TILE_BOTTOM_PAD * s - tiles_y).max(0.));
    Layout { scale: s, panel_left: left, equip, backpack_line, tiles, drop }
}

/// Tile rectangles (pixels) of `n` tiles in `area`, scrolled by `scroll` pixels: the `SListPanel` cells of [`cooked::tile_cells`].
pub fn tile_rects(area: Rect, s: f32, n: usize, scroll: f32) -> Vec<Rect> {
    cooked::tile_cells([area.w / s, area.h / s], TILE_W, TILE_H, n, scroll / s).into_iter().map(|c| Rect::new(area.x + c[0] * s, area.y + c[1] * s, c[2] * s, c[3] * s)).collect()
}

pub struct InventoryPanel {
    pub open: bool,
    view: InventoryView,
    selected: Option<u32>,
    cursor: (f32, f32),
    press: Option<Press>,
    drag: Option<Drag>,
    scroll: f32,
    size: (f32, f32),
    s_line: f32,
    actions: Vec<PanelAction>,
    /// Last left press on a tile (item, time s, position) for the double-click test.
    last_down: Option<(u32, f64, (f32, f32))>,
    install: Option<std::path::PathBuf>,
    /// the composed cooked trees (None = not tried yet, Some(None) = failed, reported once)
    cooked: Option<Option<CookedInventory>>,
    /// geometry of the last drawn frame
    geo: Option<Geo>,
    /// `SortType` of the panel (`UAthenaInventoryPanelBase` +0x408)
    sort: SortType,
    /// the `QuantitySelector_C` modal, its slider capture and the button under a press
    qsel: Option<QselState>,
    qsel_slider: bool,
}
impl Default for InventoryPanel {
    fn default() -> Self { InventoryPanel { open: false, view: InventoryView::default(), selected: None, cursor: (0., 0.), press: None, drag: None, scroll: 0., size: (1920., 1080.), s_line: 24.5, actions: vec![], last_down: None, install: None, cooked: None, geo: None, sort: SortType::BySubtype, qsel: None, qsel_slider: false } }
}

impl InventoryPanel {
    pub fn new() -> Self { Self::default() }
    /// The install (or its Paks directory) the cooked widget trees are read from.
    pub fn set_install(&mut self, install: std::path::PathBuf) { if self.install.as_ref() != Some(&install) { self.install = Some(install); self.cooked = None; } }
    pub fn selected(&self) -> Option<u32> { self.selected }
    pub fn dragging(&self) -> Option<u32> { self.drag.map(|d| d.item) }
    pub fn view(&self) -> &InventoryView { &self.view }
    pub fn scroll(&self) -> f32 { self.scroll }
    pub fn sort_type(&self) -> SortType { self.sort }
    /// The open quantity selector (item, current value, max).
    pub fn quantity_selector(&self) -> Option<(u32, i32, i32)> { self.qsel.as_ref().map(|q| (q.item.id, q.current, q.max)) }
    /// `SetSortType` (0x142de7960): store and refresh the tile view sorter.
    pub fn set_sort_type(&mut self, s: SortType) { self.sort = s; }
    /// `CycleSortType` (0x142dba420): the next supported type after the current one (wrapping), the first when the current one is not supported.
    pub fn cycle_sort_type(&mut self) {
        let next = match SUPPORTED_SORTS.iter().position(|s| *s == self.sort) { Some(i) => SUPPORTED_SORTS[(i + 1) % SUPPORTED_SORTS.len()], None => SUPPORTED_SORTS[0] };
        self.set_sort_type(next);
    }
    /// Open (`OnActivated`) / close (`OnDeactivated`: handlers removed; the drag, press and the modal are dropped).
    pub fn set_open(&mut self, open: bool) { self.open = open; if !open { self.press = None; self.drag = None; self.qsel = None; self.qsel_slider = false; } }
    pub fn toggle(&mut self) { let o = !self.open; self.set_open(o); }
    /// Hand over the live inventory of this frame. A selection whose item vanished is cleared.
    pub fn set_view(&mut self, v: InventoryView) {
        self.view = v;
        if let Some(id) = self.selected { if !self.view.items.iter().any(|i| i.id == id) { self.selected = None; } }
        if self.drag.map_or(false, |d| !self.view.items.iter().any(|i| i.id == d.item)) { self.drag = None; }
        if self.press.map_or(false, |p| !self.view.items.iter().any(|i| i.id == p.item)) { self.press = None; }
        if let Some(q) = self.qsel.as_mut() { match self.view.items.iter().find(|i| i.id == q.item.id) { Some(it) => q.item = it.clone(), None => { self.qsel = None; self.qsel_slider = false; } } }
    }
    pub fn select(&mut self, id: u32) { if self.view.items.iter().any(|i| i.id == id) { self.selected = Some(id); } }
    pub fn take_actions(&mut self) -> Vec<PanelAction> { std::mem::take(&mut self.actions) }
    pub fn set_viewport(&mut self, w: f32, h: f32) { if (w, h) != self.size { self.geo = None; } self.size = (w.max(1.), h.max(1.)); }

    fn item(&self, id: u32) -> Option<&InvItem> { self.view.items.iter().find(|i| i.id == id) }
    fn equip_item(&self, slot: u8) -> Option<&InvItem> { self.view.items.iter().find(|i| i.slot == Some(slot)) }
    /// The backpack tiles in display order: the tile view filter then the sorter.
    pub fn tile_items(&self) -> Vec<&InvItem> {
        let mut v: Vec<&InvItem> = self.view.items.iter().filter(|i| TILE_TYPES.contains(&i.item_type)).collect();
        v.sort_by(|a, b| compare_items(a, b, self.sort));
        v
    }
    fn lay(&self) -> Layout { layout(self.size.0, self.size.1, self.s_line) }
    fn equip_rects(&self) -> [Rect; 5] { self.geo.as_ref().map(|g| g.equip).unwrap_or_else(|| self.lay().equip) }
    fn drop_rect(&self) -> Rect { self.geo.as_ref().map(|g| g.drop).unwrap_or_else(|| self.lay().drop) }
    fn tile_view_rect(&self) -> Rect { self.geo.as_ref().map(|g| g.tile_view).unwrap_or_else(|| self.lay().tiles) }
    fn tile_rect_list(&self) -> Vec<Rect> {
        let n = self.tile_items().len();
        match &self.geo { Some(g) if g.tiles.len() == n => g.tiles.clone(), _ => { let l = self.lay(); tile_rects(l.tiles, l.scale, n, self.scroll) } }
    }
    fn hit_tile(&self, x: f32, y: f32) -> Option<u32> {
        if !self.tile_view_rect().contains(x, y) { return None; }
        let items = self.tile_items();
        self.tile_rect_list().iter().position(|r| r.contains(x, y)).and_then(|i| items.get(i).map(|it| it.id))
    }
    fn max_scroll(&self) -> f32 {
        let tv = self.tile_view_rect();
        let content = match &self.geo { Some(g) => g.content_h, None => { let s = dpi_scale(self.size.0, self.size.1); cooked::rows_height(tv.w / s, TILE_W, TILE_H, self.tile_items().len()) * s } };
        (content - tv.h).max(0.)
    }

    // ---------------------------------------------------------------- the quantity selector (modal layer: it takes all panel input while open)
    fn qsel_geo(&self) -> Option<cooked::QselGeo> { self.geo.as_ref().and_then(|g| g.qsel) }
    fn qsel_button_at(&self, x: f32, y: f32) -> Option<QselButton> {
        let g = self.qsel_geo()?;
        [(g.confirm, QselButton::Confirm), (g.cancel, QselButton::Cancel), (g.max, QselButton::Max)].into_iter().find(|(r, _)| r.w > 0. && r.contains(x, y)).map(|(_, b)| b)
    }
    /// `QuantitySelector` slider: `SSlider::PositionToValue` (IndentHandle) then `OnFloatValueChanged`: `CurrentValue = Round((MaxValue - 1) * Value) + 1` (bAllowZeroQuantity false).
    fn qsel_slide(&mut self, x: f32) {
        let Some(g) = self.qsel_geo() else { return };
        let Some(q) = self.qsel.as_mut() else { return };
        let v = ((x - g.slider.x - 0.5 * g.thumb_w) / (g.slider.w - g.thumb_w).max(1.)).clamp(0., 1.);
        q.slider = v;
        q.current = ((q.max - 1) as f32 * v).round() as i32 + 1;
    }
    fn qsel_press(&mut self, b: QselButton) {
        let Some(q) = self.qsel.clone() else { return };
        match b {
            // HandleMaxPressed: CurrentValue = MaxValue, slider 1
            QselButton::Max => if let Some(q) = self.qsel.as_mut() { q.current = q.max; q.slider = 1.; },
            // HandleTransferPressed / HandleCancelPressed: CloseSelector, then the dispatcher (CurrentValue / 0) -> HandleDropItemConfirmationCallback -> DropItem
            QselButton::Confirm => { self.qsel = None; self.drop_item(&q.item, q.current.max(0) as u32); }
            QselButton::Cancel => { self.qsel = None; self.drop_item(&q.item, 0); }
        }
    }
    /// The panel Blueprint's `DropItem(item, quantity)`: `AdvanceSelection` first when the whole stack goes, then `FortInventoryContext.DropItem`.
    fn drop_item(&mut self, it: &InvItem, quantity: u32) {
        if quantity == it.count && self.selected == Some(it.id) {
            let ids: Vec<u32> = self.tile_items().iter().map(|i| i.id).collect();
            if let Some(i) = ids.iter().position(|&x| x == it.id) {
                if i + 1 < ids.len() { self.selected = Some(ids[i + 1]); } else if i > 0 { self.selected = Some(ids[i - 1]); }
            }
        }
        self.actions.push(PanelAction::DropQuantity { item: it.id, quantity });
    }

    // ---------------------------------------------------------------- input
    pub fn mouse_move(&mut self, x: f32, y: f32) {
        self.cursor = (x, y);
        if self.qsel.is_some() {
            if self.qsel_slider { self.qsel_slide(x); }
            let h = self.qsel_button_at(x, y);
            if let Some(q) = self.qsel.as_mut() { q.hover = h; }
            return;
        }
        if self.drag.is_none() {
            if let Some(p) = self.press {
                let s = dpi_scale(self.size.0, self.size.1);
                if (x - p.at.0).hypot(y - p.at.1) >= DRAG_START_UNITS * s && self.item(p.item).is_some() {
                    // only the tile's OnDragDetected sets SplitTheStack (= IsShiftDown); the equip slot's operation (Tag "FromQuickbar") never splits by itself
                    self.drag = Some(Drag { item: p.item, split: !p.from_slot && p.shift, from_slot: p.from_slot });
                }
            }
        }
    }
    /// Left button down at (x, y): arms a drag on an item tile / filled equip slot (`DetectDrag` / `DetectDragIfPressed`).
    pub fn mouse_down(&mut self, x: f32, y: f32, shift: bool) {
        self.cursor = (x, y);
        if self.qsel.is_some() {
            match self.qsel_button_at(x, y) {
                Some(b) => if let Some(q) = self.qsel.as_mut() { q.pressed = Some(b); },
                None => if self.qsel_geo().map_or(false, |g| g.slider.contains(x, y)) { self.qsel_slider = true; self.qsel_slide(x); },
            }
            return;
        }
        let eq = self.equip_rects();
        let tile = self.hit_tile(x, y);
        let slot = if tile.is_none() { (0..5).find(|i| eq[*i].contains(x, y)).and_then(|i| self.equip_item(i as u8 + 1).map(|it| it.id)) } else { None };
        self.press = tile.or(slot).map(|id| Press { item: id, at: (x, y), shift, from_slot: slot.is_some() });
    }
    /// Left button down at time `now` (seconds, any monotonic clock). A second press on the same TILE within the Windows double-click time / rectangle is Slate's
    /// `OnMouseButtonDoubleClick` instead of a press: the tile view's `OnItemDoubleClicked` -> `UAthenaInventoryPanelBase::HandleInventoryItemDoubleClicked` (exe 0x142dc7da0) ->
    /// `RequestEquip(item)` (0x142dddc60) -> the Blueprint's `RequestEquip` event -> `PerformEquip(item)` (no Equip-action state check, unlike the E key). No drag is armed by it.
    pub fn mouse_down_at(&mut self, x: f32, y: f32, shift: bool, now: f64) {
        if self.qsel.is_some() { self.mouse_down(x, y, shift); return; }
        let tile = self.hit_tile(x, y);
        if let (Some(id), Some((last, t, at))) = (tile, self.last_down) {
            if id == last && now - t <= DOUBLE_CLICK_S && (x - at.0).abs() <= DOUBLE_CLICK_PX && (y - at.1).abs() <= DOUBLE_CLICK_PX {
                self.cursor = (x, y); self.last_down = None; self.press = None;
                self.selected = Some(id);
                self.actions.push(PanelAction::Equip(id));
                return;
            }
        }
        self.last_down = tile.map(|id| (id, now, (x, y)));
        self.mouse_down(x, y, shift);
    }
    /// The `Drop` input action (X): `HandleDrop` on the selected item (`AthenaHUDContext.GetSelectedInventoryItem`): stack > 1 opens the quantity selector.
    pub fn key_drop(&mut self) {
        if self.qsel.is_some() { return; }
        let Some(it) = self.selected.and_then(|id| self.item(id)).cloned() else { return };
        if it.count > 1 {
            let (cur, max) = ((it.count / 2) as i32, it.count as i32);
            // SetSplitData: CurrentValue = StartingCount, ValueSlider.SetValue((Current - 1) / (Max - 1))
            self.qsel = Some(QselState { slider: (cur - 1) as f32 / (max - 1).max(1) as f32, item: it, current: cur, max, hover: None, pressed: None });
            self.press = None; self.drag = None;
        } else { self.drop_item(&it, it.count); }
    }
    /// Centre of a panel target in pixels (for scripted runs); None when it is not on screen.
    pub fn target_center(&self, t: PanelTarget) -> Option<(f32, f32)> {
        match t {
            PanelTarget::EquipSlot(n) if (1..=5).contains(&n) => Some(self.equip_rects()[n as usize - 1].center()),
            PanelTarget::Tile(k) => { let tv = self.tile_view_rect(); self.tile_rect_list().get(k).map(|r| r.center()).filter(|c| tv.contains(c.0, c.1)) }
            PanelTarget::DropSlot => Some(self.drop_rect().center()),
            PanelTarget::QselConfirm => self.qsel_geo().map(|g| g.confirm.center()),
            PanelTarget::QselCancel => self.qsel_geo().map(|g| g.cancel.center()),
            PanelTarget::QselMax => self.qsel_geo().map(|g| g.max.center()),
            _ => None,
        }
    }
    /// Pixel position on the selector's slider for a value 0..1 (scripted runs).
    pub fn qsel_slider_point(&self, v: f32) -> Option<(f32, f32)> { self.qsel_geo().map(|g| (g.slider.x + 0.5 * g.thumb_w + v.clamp(0., 1.) * (g.slider.w - g.thumb_w), g.slider.center().1)) }
    /// Centre of the tile view in pixels (scripted wheel steps).
    pub fn tile_view_center(&self) -> (f32, f32) { self.tile_view_rect().center() }
    /// Index of the tile showing item `id`.
    pub fn tile_of(&self, id: u32) -> Option<usize> { self.tile_items().iter().position(|i| i.id == id) }
    /// Left button up: a drag is resolved on the equip slot / drop slot under the cursor, a plain click on a tile selects it.
    pub fn mouse_up(&mut self, x: f32, y: f32, shift: bool) {
        self.cursor = (x, y);
        if self.qsel.is_some() {
            if self.qsel_slider { self.qsel_slide(x); self.qsel_slider = false; return; }
            let pressed = self.qsel.as_ref().and_then(|q| q.pressed);
            if let Some(q) = self.qsel.as_mut() { q.pressed = None; }
            // CommonButton click: released over the button that was pressed
            if let Some(b) = pressed.filter(|b| self.qsel_button_at(x, y) == Some(*b)) { self.qsel_press(b); }
            return;
        }
        if let Some(d) = self.drag.take() {
            self.press = None;
            let eq = self.equip_rects();
            if let Some(i) = (0..5).find(|i| eq[*i].contains(x, y)) { self.actions.push(PanelAction::DropOnSlot { item: d.item, slot: i as u8 + 1 }); }
            else if self.drop_rect().contains(x, y) { self.actions.push(PanelAction::Drop { item: d.item, split: d.split || shift }); }
            return;
        }
        if let Some(p) = self.press.take() {
            if let Some(id) = self.hit_tile(x, y).filter(|id| *id == p.item) { self.selected = Some(id); self.actions.push(PanelAction::Select(id)); }
        }
    }
    /// Mouse wheel over the tile view: `STableViewBase::OnMouseWheel` scrolls `WheelDelta * Slate.GlobalScrollAmount` (32 units) per notch.
    pub fn wheel(&mut self, notches: f32) {
        if self.qsel.is_some() || !self.tile_view_rect().contains(self.cursor.0, self.cursor.1) { return; }
        let s = dpi_scale(self.size.0, self.size.1);
        self.scroll = (self.scroll - notches * cooked::WHEEL_SCROLL * s).clamp(0., self.max_scroll());
    }
    /// The `Equip` input action (`RegisterEquip`): only live for an equipable, non-resource selected item.
    pub fn key_equip(&mut self) {
        if self.qsel.is_some() { return; }
        let id = self.selected.and_then(|id| self.item(id)).filter(|it| it.equipable && !it.is_resource).map(|it| it.id);
        if let Some(id) = id { self.actions.push(PanelAction::Equip(id)); }
    }
    /// Escape: the selector's `Cancel` row while it is open, else Back / LeaveInventory (`HandleClose`).
    pub fn key_close(&mut self) { if self.qsel.is_some() { self.qsel_press(QselButton::Cancel); } else { self.actions.push(PanelAction::Close); } }
    /// Scripted selector input: click a button / set the slider.
    pub fn qsel_click(&mut self, b: QselButton) { if self.qsel.is_some() { self.qsel_press(b); } }

    // ---------------------------------------------------------------- drawing
    /// The item card over `r` (the port's translated item card shader) + the stack count.
    pub(crate) fn card(ctx: &mut UiContext, out: &mut RenderList, r: Rect, it: &InvItem, count: bool) {
        let hero = mat_tex(ctx, "T-Card-Hero-Anim-Triangle.png", true).id;
        let icon = if it.icon.is_empty() { TexInfo::NONE } else { mat_tex(ctx, &it.icon, false) };
        let c = RARITY[it.rarity.min(4) as usize];
        out.material(MaterialKind::ItemCard { colors: [c[0], c[1], c[2], c[3]], icon: TextureId::NONE, hero_tri: hero }, r, WHITE, false);
        if icon.id != TextureId::NONE { out.sprite(icon.id, scale_to_fit(r, icon.w as f32, icon.h as f32), WHITE); }
        if count && it.count > 1 {
            let s = r.h.min(r.w) / 124.;
            let px = atlas_px(crate::screens::quickbar::KEY_FONT, s.max(0.5));
            let txt = it.count.to_string();
            let tw = text_width(ctx, FontId::Bold, &txt, px);
            let lh = line_height(ctx, FontId::Bold, px);
            let base = centred_baseline(ctx, FontId::Bold, px, r.bottom() - 2. * s - lh, lh);
            put_text(ctx, out, FontId::Bold, px, r.right() - 4. * s - tw, base, &txt, WHITE, None, Snap::PerGlyph);
        }
    }

    /// Draw the open panel from the cooked trees.
    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, out: &mut RenderList) {
        if !self.open { return; }
        self.set_viewport(w, h);
        if self.cooked.is_none() {
            let c = match self.install.as_ref() { Some(i) => CookedInventory::load(i).map_err(|e| e.to_string()), None => Err("no install set".into()) };
            // INVENTORY FIX2: the trees' textures come from the paks when the UI's art source lacks them (see `CookedInventory::attach_paks`)
            #[cfg(feature = "pak")]
            if c.is_ok() { if let Some(i) = self.install.as_ref() { if let Err(e) = CookedInventory::attach_paks(ctx, i) { eprintln!("[inventory] pak textures unavailable: {e}"); } } }
            self.cooked = Some(match c { Ok(c) => Some(c), Err(e) => { eprintln!("[inventory] AthenaInventoryPanel not drawn: {e}"); None } });
        }
        self.scroll = self.scroll.clamp(0., self.max_scroll());
        let tiles: Vec<TileState> = {
            let pressed = self.press.filter(|p| !p.from_slot).map(|p| p.item);
            let hover = if self.drag.is_none() && self.qsel.is_none() { self.hit_tile(self.cursor.0, self.cursor.1) } else { None };
            self.tile_items().into_iter().map(|it| {
                let sel = self.selected == Some(it.id);
                let btn = match (sel, pressed == Some(it.id), hover == Some(it.id)) { (true, true, _) => BtnState::SelectedPressed, (true, _, true) => BtnState::SelectedHovered, (true, ..) => BtnState::Selected, (false, true, _) => BtnState::Pressed, (false, _, true) => BtnState::Hovered, _ => BtnState::Normal };
                TileState { item: it.clone(), btn, selected: sel }
            }).collect()
        };
        let Some(Some(ci)) = self.cooked.as_mut() else { return };
        let view = &self.view;
        let eq = |n: u8| view.items.iter().find(|i| i.slot == Some(n));
        let drag = self.drag.and_then(|d| view.items.iter().find(|i| i.id == d.item).map(|it| (it, d.from_slot)));
        let st = DrawState { equip: [eq(1), eq(2), eq(3), eq(4), eq(5)], tiles: &tiles, scroll: self.scroll, drag, cursor: self.cursor, backpack: view.backpack,
            selected: self.selected.and_then(|id| view.items.iter().find(|i| i.id == id)), qsel: self.qsel.as_ref() };
        let (list, geo) = ci.draw(ctx, w, h, &st, &mut |c, o, r, it, count| Self::card(c, o, r, it, count));
        out.extend(list);
        self.geo = Some(geo);
    }

    /// Number of draw commands that are quads (test helper).
    pub fn quads(out: &RenderList) -> usize { out.cmds.iter().filter(|c| matches!(c, Cmd::Quad(_))).count() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn it(id: u32, ty: u8, slot: Option<u8>, count: u32) -> InvItem { InvItem { id, name: format!("Item {id}"), description: "A description.".into(), rarity: (id % 5) as u8, icon: String::new(), count, item_type: ty, equipable: ty != 19, is_resource: ty == 19, can_be_dropped: true, slot, def_path: String::new() } }
    fn view() -> InventoryView { InventoryView { items: vec![it(1, 18, None, 1), it(2, 17, Some(1), 1), it(3, 17, Some(2), 1), it(4, 17, Some(3), 1), it(5, 19, None, 120)], backpack: (4, 5) } }
    fn panel() -> InventoryPanel { let mut p = InventoryPanel::new(); p.set_open(true); p.set_viewport(1920., 1080.); p.set_view(view()); p }
    fn tile_c(p: &InventoryPanel, k: usize) -> (f32, f32) { p.target_center(PanelTarget::Tile(k)).expect("tile on screen") }

    #[test] fn the_tile_view_filters_the_pickaxe_and_sorts_by_subtype() {
        let p = panel();
        let ids: Vec<u32> = p.tile_items().iter().map(|i| i.id).collect();
        assert_eq!(ids, [4, 3, 2, 5], "WeaponHarvest (18) is not in the filter; ranged weapons (subtype 1, rarity descending 4, 3, 2) before resources (5)");
        assert_eq!(p.sort_type(), SortType::BySubtype);
        let mut q = panel(); q.cycle_sort_type(); assert_eq!(q.sort_type(), SortType::ByRating, "BySubtype is the last supported type: wraps to the first");
        q.cycle_sort_type(); assert_eq!(q.sort_type(), SortType::ByRarity);
        q.set_sort_type(SortType::ByName);
        let ids: Vec<u32> = q.tile_items().iter().map(|i| i.id).collect(); assert_eq!(ids, [2, 3, 4, 5], "ByName: ordinal display names");
        assert!(compare_items(&it(9, 1, None, 5), &it(8, 19, None, 5), SortType::BySubtype).is_lt(), "ammo (2) before resources (5)");
    }
    #[test] fn cells_are_the_cooked_item_size() {
        let l = layout(1920., 1080., 24.5);
        let r = tile_rects(l.tiles, 1., 3, 0.);
        assert_eq!((r[0].w, r[0].h), (TILE_W, TILE_H)); assert_eq!(r[1].x - r[0].x, TILE_W);
        assert!((l.tiles.y - 265.5).abs() < 1e-3 && (l.tiles.bottom() - 897.).abs() < 1e-3, "{:?}", l.tiles);
        assert_eq!((l.drop.y, l.drop.w, l.drop.h), (905., 1100., 175.), "the drop slot fills SizeBox_4");
        assert!((l.equip[0].x - 263.).abs() < 1e-3 && l.equip[0].y == 56.);
    }
    #[test] fn click_selects_and_drag_to_an_equip_slot_asks_for_drop_item_on_quickbar() {
        let mut p = panel(); let c = tile_c(&p, 3); // wood
        p.mouse_down(c.0, c.1, false); p.mouse_up(c.0, c.1, false);
        assert_eq!(p.selected(), Some(5)); assert_eq!(p.take_actions(), vec![PanelAction::Select(5)]);
        p.mouse_down(c.0, c.1, false); p.mouse_move(c.0 + 3., c.1); assert!(p.dragging().is_none(), "below the 5 unit trigger distance");
        p.mouse_move(c.0 + 40., c.1 + 40.); assert_eq!(p.dragging(), Some(5));
        let e = layout(1920., 1080., 24.5).equip[3].center(); p.mouse_move(e.0, e.1); p.mouse_up(e.0, e.1, false);
        assert_eq!(p.take_actions(), vec![PanelAction::DropOnSlot { item: 5, slot: 4 }]); assert!(p.dragging().is_none());
    }
    #[test] fn dragging_a_slotted_item_out_onto_the_drop_slot_splits_only_with_shift_at_the_drop() {
        let mut p = panel(); let l = layout(1920., 1080., 24.5);
        let e = l.equip[0].center(); p.mouse_down(e.0, e.1, true); // shift at the press: the equip slot's operation has no SplitTheStack
        p.mouse_move(e.0 + 30., e.1 + 30.); assert_eq!(p.dragging(), Some(2));
        let d = l.drop.center(); p.mouse_up(d.0, d.1, false);
        assert_eq!(p.take_actions(), vec![PanelAction::Drop { item: 2, split: false }]);
        p.mouse_down(e.0, e.1, false); p.mouse_move(e.0 + 30., e.1); p.mouse_up(5., 5., false); assert!(p.take_actions().is_empty(), "a drop on nothing does nothing");
        p.mouse_down(e.0, e.1, false); p.mouse_move(e.0 + 30., e.1); p.mouse_up(d.0, d.1, true); assert_eq!(p.take_actions(), vec![PanelAction::Drop { item: 2, split: true }], "IsShiftDown at the drop");
        // a tile drag started with Shift carries SplitTheStack
        let c = tile_c(&p, 3); p.mouse_down(c.0, c.1, true); p.mouse_move(c.0 + 30., c.1 + 30.); p.mouse_up(d.0, d.1, false);
        assert_eq!(p.take_actions(), vec![PanelAction::Drop { item: 5, split: true }]);
    }
    #[test] fn the_equip_action_needs_a_selected_equipable_non_resource_item() {
        let mut p = panel(); p.key_equip(); assert!(p.take_actions().is_empty(), "nothing selected");
        p.select(5); p.key_equip(); assert!(p.take_actions().is_empty(), "a resource has no Equip action");
        p.select(2); p.key_equip(); assert_eq!(p.take_actions(), vec![PanelAction::Equip(2)]);
        p.key_close(); assert_eq!(p.take_actions(), vec![PanelAction::Close]);
    }
    #[test] fn double_click_on_a_tile_requests_equip() {
        let mut p = panel(); let c = tile_c(&p, 1);
        p.mouse_down_at(c.0, c.1, false, 1.0); p.mouse_up(c.0, c.1, false); assert_eq!(p.take_actions(), vec![PanelAction::Select(3)]);
        p.mouse_down_at(c.0 + 1., c.1, false, 1.3); p.mouse_up(c.0, c.1, false); assert_eq!(p.take_actions(), vec![PanelAction::Equip(3)], "second press within 500 ms / 4 px");
        p.mouse_down_at(c.0, c.1, false, 3.0); p.mouse_up(c.0, c.1, false); p.mouse_down_at(c.0, c.1, false, 3.6); p.mouse_up(c.0, c.1, false);
        assert!(!p.take_actions().contains(&PanelAction::Equip(3)), "too slow: two selects");
    }
    #[test] fn x_drops_a_single_item_and_opens_the_quantity_selector_on_a_stack() {
        let mut p = panel();
        p.select(2); p.key_drop(); assert_eq!(p.take_actions(), vec![PanelAction::DropQuantity { item: 2, quantity: 1 }]);
        assert_eq!(p.selected(), Some(5), "AdvanceSelection: the whole stack went, the next tile (order 4, 3, 2, 5) is selected");
        p.select(5); p.key_drop(); assert!(p.take_actions().is_empty()); assert_eq!(p.quantity_selector(), Some((5, 60, 120)), "SetSplitData(stack / 2, stack)");
        p.key_drop(); p.key_equip(); assert!(p.take_actions().is_empty(), "the modal takes the input");
        p.qsel_click(QselButton::Max); assert_eq!(p.quantity_selector(), Some((5, 120, 120)));
        p.qsel_click(QselButton::Confirm); assert_eq!(p.take_actions(), vec![PanelAction::DropQuantity { item: 5, quantity: 120 }]); assert!(p.quantity_selector().is_none());
        p.select(5); p.key_drop(); p.key_close(); assert_eq!(p.take_actions(), vec![PanelAction::DropQuantity { item: 5, quantity: 0 }], "Escape = Cancel row: DropItem(item, 0)");
        assert!(p.open, "the panel stays open");
    }
    #[test] fn a_vanished_item_clears_selection_and_drag() {
        let mut p = panel(); p.select(2); p.set_view(InventoryView { items: vec![it(1, 18, None, 1)], backpack: (1, 5) }); assert_eq!(p.selected(), None);
        p.set_open(false); assert!(p.dragging().is_none() && !p.open);
    }
    #[test] fn wheel_scrolls_32_units_per_notch_only_over_the_tiles() {
        let mut p = panel(); p.set_view(InventoryView { items: (0..30).map(|i| it(i, 17, None, 1)).collect(), backpack: (30, 30) });
        let l = layout(1920., 1080., 24.5); p.mouse_move(5., 5.); p.wheel(-1.); assert_eq!(p.scroll(), 0.);
        let c = l.tiles.center(); p.mouse_move(c.0, c.1); p.wheel(-1.); assert_eq!(p.scroll(), 32.); p.wheel(2.); assert_eq!(p.scroll(), 0.);
    }
}
