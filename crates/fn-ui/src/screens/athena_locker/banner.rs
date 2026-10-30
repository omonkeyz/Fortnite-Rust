use crate::screens::athena_lobby::store::{jvec, Store};
use crate::screens::athena_lobby::styles::BtnState;
use crate::screens::athena_lobby::umg::{Ui, Vis};
use crate::scene::UiContext;
use serde_json::{json, Value};

pub const DEFAULT_ICON: &str = "StandardBanner1";
pub const DEFAULT_COLOR: &str = "DefaultColor1";
pub const MODAL_CLASS: &str = "/Game/Athena/UI/Customization/AthenaBannerSelectModal.AthenaBannerSelectModal_C";
pub const TILE_CLASS: &str = "/Game/UI/Foundation/Profile/BP_BannerEditorTile.BP_BannerEditorTile_C";
const SCROLLBAR: f32 = 16.0;
pub const PLAYER_BANNER: &str = "/Game/UI/Foundation/Profile/PlayerBanner.PlayerBanner_C";

#[derive(Clone, Debug)]
pub struct IconRow { pub row: String, pub small: String, pub large: String, pub cat: String }
#[derive(Clone, Debug)]
pub struct ColorRow { pub row: String, pub primary: [f32; 4], pub secondary: [f32; 4], pub cat: String }

#[derive(Clone, Debug, Default)]
pub struct BannerData {
    pub icons: Vec<IconRow>,
    pub colors: Vec<ColorRow>,
    pub icon_cats: Vec<(String, Value)>,
    pub color_cats: Vec<(String, Value)>,
}

fn table_rows(store: &Store, long: &str) -> Vec<(String, Value)> {
    let Ok(data) = fn_uasset::PackageData::load(store.pak_set(), long) else { return vec![] };
    let Ok(pkg) = data.package() else { return vec![] };
    let Some(i) = (0..pkg.exports.len()).find(|&i| pkg.export_class_name(i) == "DataTable") else { return vec![] };
    let Ok(t) = fn_uasset::classes::read_table(&pkg, i) else { return vec![] };
    t.rows.iter().map(|(n, p)| (n.clone(), p.to_json(&pkg))).collect()
}

fn s(v: &Value, k: &str) -> String { v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned() }

impl BannerData {
    pub fn load(store: &Store) -> BannerData {
        let mut d = BannerData::default();
        for (row, v) in table_rows(store, "/Game/Banners/BannerIcons") {
            d.icons.push(IconRow { row, small: s(&v, "SmallImage"), large: s(&v, "LargeImage"), cat: s(&v, "CategoryRowName") });
        }
        let map = store.object("/Game/Banners/BannerColorMap.BannerColorMap").and_then(|(p, i)| p.exports[i].props.get("ColorMap").cloned()).unwrap_or(Value::Null);
        let lookup = |key: &str| -> Option<([f32; 4], [f32; 4])> {
            let e = map.as_array()?.iter().find(|e| e.get("key").and_then(|k| k.as_str()) == Some(key))?;
            let v = e.get("value")?;
            Some((jvec::<4>(v.get("PrimaryColor"))?, jvec::<4>(v.get("SecondaryColor"))?))
        };
        for (row, v) in table_rows(store, "/Game/Banners/BannerColors") {
            let (p, sc) = lookup(&s(&v, "ColorKeyName")).unwrap_or(([0.0; 4], [0.0; 4]));
            d.colors.push(ColorRow { row, primary: p, secondary: sc, cat: s(&v, "CategoryRowName") });
        }
        let icon_cat_rows = table_rows(store, "/Game/Banners/BannerIconCategories");
        for ic in &d.icons {
            if d.icon_cats.iter().any(|(c, _)| *c == ic.cat) { continue; }
            let name = icon_cat_rows.iter().find(|(r, _)| *r == ic.cat).and_then(|(_, v)| v.get("CategoryDisplayName").cloned()).unwrap_or(Value::Null);
            d.icon_cats.push((ic.cat.clone(), name));
        }
        let color_cat_rows = table_rows(store, "/Game/Banners/BannerColorCategories");
        for c in &d.colors {
            if d.color_cats.iter().any(|(k, _)| *k == c.cat) { continue; }
            let name = color_cat_rows.iter().find(|(r, _)| *r == c.cat).and_then(|(_, v)| v.get("CategoryDisplayName").cloned()).unwrap_or(Value::Null);
            d.color_cats.push((c.cat.clone(), name));
        }
        d
    }
    pub fn icon(&self, row: &str) -> Option<&IconRow> { self.icons.iter().find(|i| i.row == row) }
    pub fn color(&self, row: &str) -> Option<&ColorRow> { self.colors.iter().find(|c| c.row == row) }
    pub fn icons_of(&self, cat: usize) -> Vec<&IconRow> { let c = self.icon_cats.get(cat).map(|c| c.0.as_str()).unwrap_or(""); self.icons.iter().filter(|i| i.cat == c).collect() }
    pub fn colors_of(&self, cat: usize) -> Vec<&ColorRow> { let c = self.color_cats.get(cat).map(|c| c.0.as_str()).unwrap_or(""); self.colors.iter().filter(|i| i.cat == c).collect() }
    pub fn icon_cat_of(&self, row: &str) -> usize { self.icon(row).and_then(|i| self.icon_cats.iter().position(|c| c.0 == i.cat)).unwrap_or(0) }
    pub fn color_cat_of(&self, row: &str) -> usize { self.color(row).and_then(|i| self.color_cats.iter().position(|c| c.0 == i.cat)).unwrap_or(0) }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorView { pub color_page: bool, pub cat: usize, pub icon: String, pub color: String, pub scroll_row: usize }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerButton { EditIcon, EditColor, Left, Right, Apply, Close, Tile(usize) }

struct Editor {
    modal: usize,
    tile_view: Option<usize>,
    tiles: Vec<usize>,
    category_text: Option<usize>,
    buttons: Vec<(usize, BannerButton)>,
    preview: Option<usize>,
}

#[derive(Default)]
pub struct BannerUi {
    pub data: BannerData,
    editor: Option<Editor>,
    pub grid: (usize, usize, usize),
    tv_size: std::cell::Cell<Option<[f32; 2]>>,
    natural: [Option<[f32; 2]>; 2],
}

fn mark_common_button(ui: &mut Ui, id: usize) {
    if let Some(u) = ui.w[id].user.as_mut() { if !u.chain.iter().any(|c| c == "/Script/CommonUI.CommonButton") { u.chain.push("/Script/CommonUI.CommonButton".into()); } }
}

pub fn set_banner_image(ui: &mut Ui, data: &BannerData, banner: usize, icon: &str, color: &str) {
    let Some(img) = ui.var(banner, "BannerImage").or_else(|| ui.find_in(banner, "BannerImage")) else { return };
    let large = ui.w[banner].props.get("bUseLargeTexture").and_then(|v| v.as_bool()).unwrap_or(false);
    let tex = data.icon(icon).map(|i| if large { i.large.clone() } else { i.small.clone() }).unwrap_or_default();
    ui.w[img].mat_textures = if tex.is_empty() { vec![] } else { vec![("BannerIcon".to_owned(), tex)] };
    let (p, sc) = data.color(color).map(|c| (c.primary, c.secondary)).unwrap_or(([0.0; 4], [0.0; 4]));
    ui.w[img].mat_vectors = vec![("RGBA0".to_owned(), p), ("RGBA1".to_owned(), sc)];
}

impl BannerUi {
    pub fn load(store: &Store) -> BannerUi { BannerUi { data: BannerData::load(store), ..Default::default() } }

    pub fn modal(&self) -> Option<usize> { self.editor.as_ref().map(|e| e.modal) }
    pub fn tile_view(&self) -> Option<usize> { self.editor.as_ref().and_then(|e| e.tile_view) }
    pub fn button_of(&self, id: usize) -> Option<BannerButton> { self.editor.as_ref().and_then(|e| e.buttons.iter().find(|b| b.0 == id).map(|b| b.1).or_else(|| e.tiles.iter().position(|&t| t == id).map(BannerButton::Tile))) }
    pub fn note_tile_view_rect(&self, r: Option<crate::layout::Rect>, scale: f32) { if let Some(r) = r { if r.w > 0.0 && r.h > 0.0 { self.tv_size.set(Some([r.w / scale.max(1e-3), r.h / scale.max(1e-3)])); } } }

    pub fn apply_banners(&self, ui: &mut Ui, icon: &str, color: &str, editor: Option<&EditorView>) {
        let preview = self.editor.as_ref().and_then(|e| e.preview);
        let banners: Vec<usize> = (0..ui.w.len()).filter(|&i| ui.chain_has(i, PLAYER_BANNER)).collect();
        for b in banners {
            match (preview == Some(b), editor) {
                (true, Some(e)) => set_banner_image(ui, &self.data, b, &e.icon, &e.color),
                _ => set_banner_image(ui, &self.data, b, icon, color),
            }
        }
    }

    fn compose(&mut self, ui: &mut Ui) -> Option<()> {
        if self.editor.is_some() { return Some(()); }
        let modal = ui.add_class_instance(MODAL_CLASS, None, "", json!({}), None)?;
        let ed = ui.find_in(modal, "BP_LocalPlayerBannerEditor").unwrap_or(modal);
        let mut buttons = vec![];
        for (n, b) in [("EditIconButton", BannerButton::EditIcon), ("EditColorButton", BannerButton::EditColor), ("LeftArrowButton", BannerButton::Left), ("RightArrowButton", BannerButton::Right), ("ApplyButton", BannerButton::Apply), ("CloseEditorButton", BannerButton::Close)] {
            if let Some(id) = ui.find_in(ed, n) { mark_common_button(ui, id); buttons.push((id, b)); }
        }
        let tile_view = ui.find_in(ed, "CommonTileView_0");
        if let Some(tv) = tile_view { ui.w[tv].class = "Overlay".into(); ui.w[tv].enabled = true; }
        self.editor = Some(Editor { modal, tile_view, tiles: vec![], category_text: ui.find_in(ed, "CategoryText"), buttons, preview: ui.find_in(ed, "HomebaseBanner") });
        Some(())
    }

    fn ensure_tiles(&mut self, ui: &mut Ui, n: usize) {
        let Some(e) = self.editor.as_mut() else { return };
        let Some(tv) = e.tile_view else { return };
        while e.tiles.len() < n {
            let Some(t) = ui.add_class_instance(TILE_CLASS, Some(tv), "", json!({}), None) else { return };
            mark_common_button(ui, t);
            e.tiles.push(t);
        }
    }

    pub fn apply(&mut self, ui: &mut Ui, uic: &mut UiContext, view: Option<&EditorView>, equipped: (&str, &str), scale: f32) {
        let Some(v) = view else {
            if let Some(e) = self.editor.as_ref() { ui.w[e.modal].vis = Vis::Collapsed; }
            return;
        };
        if self.compose(ui).is_none() { return; }
        let n = if v.color_page { self.data.colors_of(v.cat).len() } else { self.data.icons_of(v.cat).len() };
        self.ensure_tiles(ui, n);
        let e = self.editor.as_ref().unwrap();
        let (modal, tiles, tv, cat_text, buttons) = (e.modal, e.tiles.clone(), e.tile_view, e.category_text, e.buttons.clone());
        ui.w[modal].vis = Vis::SelfHitTestInvisible;
        let cat_name = if v.color_page { self.data.color_cats.get(v.cat) } else { self.data.icon_cats.get(v.cat) }.map(|c| ui.text_of(&c.1).filter(|t| !t.is_empty()).unwrap_or_else(|| c.0.clone())).unwrap_or_default();
        if let Some(t) = cat_text { ui.w[t].text = Some(cat_name); }
        let cats = if v.color_page { self.data.color_cats.len() } else { self.data.icon_cats.len() };
        let changed = v.icon != equipped.0 || v.color != equipped.1;
        for (id, b) in buttons {
            match b {
                BannerButton::EditIcon => ui.w[id].btn_state = if v.color_page { BtnState::Normal } else { BtnState::Selected },
                BannerButton::EditColor => ui.w[id].btn_state = if v.color_page { BtnState::Selected } else { BtnState::Normal },
                BannerButton::Left | BannerButton::Right => ui.w[id].vis = if cats > 1 { Vis::Visible } else { Vis::Hidden },
                BannerButton::Apply => { ui.w[id].enabled = changed; ui.w[id].btn_state = if changed { BtnState::Normal } else { BtnState::Disabled }; }
                _ => {}
            }
            ui.refresh_button_text(id);
        }
        if let Some(f) = ui.find_in(modal, "FoundersNoteText") { ui.w[f].vis = Vis::Collapsed; }
        let pad = if v.color_page { [8.0; 4] } else { tv.and_then(|tv| ui.w[tv].props.get("DesiredItemPadding").cloned()).map(|p| crate::screens::athena_lobby::store::jmargin(Some(&p))).unwrap_or([8.0; 4]) };
        let item = if v.color_page { [64.0, 64.0] } else { [96.0, 120.0] };
        let cell = [item[0] + pad[0] + pad[2], item[1] + pad[1] + pad[3]];
        let [tw, th] = self.tv_size.get().unwrap_or([cell[0] * 6.0, cell[1] * 3.0]);
        let cols = (((tw - SCROLLBAR) / cell[0]).floor() as usize).max(1);
        let vis_rows = ((th / cell[1]).floor() as usize).max(1);
        let total = n.div_ceil(cols);
        self.grid = (cols, vis_rows, total);
        let first = v.scroll_row.min(total.saturating_sub(vis_rows));
        let icons = self.data.icons_of(v.cat);
        let colors = self.data.colors_of(v.cat);
        for (i, &t) in tiles.iter().enumerate() {
            if i >= n { ui.w[t].vis = Vis::Collapsed; continue; }
            let (row, col) = (i / cols, i % cols);
            let shown = row >= first && row < first + vis_rows;
            ui.w[t].vis = if shown { Vis::Visible } else { Vis::Collapsed };
            ui.w[t].slot_class = "OverlaySlot".into();
            let (l, tp) = (col as f32 * cell[0] + pad[0], (row.saturating_sub(first)) as f32 * cell[1] + pad[1]);
            ui.w[t].slot = json!({ "Padding": { "Left": l, "Top": tp, "Right": 0.0, "Bottom": 0.0 }, "HorizontalAlignment": "HAlign_Left", "VerticalAlignment": "VAlign_Top" });
            let switcher = ui.find_in(t, "CommonWidgetSwitcher_0");
            let icon_img = ui.find_in(t, "IconImage");
            let color_img = ui.find_in(t, "ColorImage");
            let target = if v.color_page { color_img } else { icon_img };
            if let (Some(sw), Some(tg)) = (switcher, target) {
                if let Some(k) = ui.w[sw].kids.iter().position(|&c| c == tg || ui.is_descendant(tg, c)) { ui.w[sw].props["ActiveWidgetIndex"] = json!(k); }
            }
            let page = v.color_page as usize;
            if self.natural[page].is_none() {
                ui.w[t].desired_scale = [1.0, 1.0];
                let keep = ui.w[t].vis;
                ui.w[t].vis = Vis::Visible;
                let mut c = crate::screens::athena_lobby::layout::Ctx { ui: &mut *ui, uic: &mut *uic, lay: crate::screens::athena_lobby::layout::new_layout(0, scale) };
                c.lay = crate::screens::athena_lobby::layout::new_layout(c.ui.w.len(), scale);
                self.natural[page] = Some(c.desired(t));
                ui.w[t].vis = keep;
            }
            let nat = self.natural[page].unwrap_or(item);
            ui.w[t].desired_scale = [item[0] / nat[0].max(1.0), item[1] / nat[1].max(1.0)];
            let selected = if v.color_page {
                let c = colors[i];
                if let Some(ci) = color_img { ui.w[ci].mat_vectors = vec![("RGBA0".into(), c.primary), ("RGBA1".into(), c.secondary)]; }
                c.row == v.color
            } else {
                let ic = icons[i];
                if let Some(ii) = icon_img {
                    let mut b = ui.w[ii].props.get("Brush").cloned().unwrap_or(json!({}));
                    b["ResourceObject"] = json!(ic.small);
                    ui.w[ii].props["Brush"] = b;
                    ui.w[ii].brush = None;
                }
                ic.row == v.icon
            };
            ui.w[t].btn_state = if selected { BtnState::Selected } else { BtnState::Normal };
        }
    }

    pub fn tile_row(&self, v: &EditorView, i: usize) -> Option<String> {
        if v.color_page { self.data.colors_of(v.cat).get(i).map(|c| c.row.clone()) } else { self.data.icons_of(v.cat).get(i).map(|c| c.row.clone()) }
    }
    pub fn categories(&self, color_page: bool) -> usize { if color_page { self.data.color_cats.len() } else { self.data.icon_cats.len() } }
}
