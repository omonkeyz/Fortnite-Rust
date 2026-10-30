//! The native item card (`UFortMultiSizeItemCard`, `ItemWidget` of the slot button and of the picker tile): a leaf widget with no widget tree in the data. Stand-in
//! for its Slate construction (native C++, so not decodable from the packages): the card is the cooked cosmetic card material
//! `/Game/UI/Foundation/Items/Materials/M-Item-Card-Cosmetic-Inst-<size>` (parent `M-Item-Card-CosmeticTypes-Master`: parameters `Icon` (texture), `Rarity 1..4`
//! (vectors), `bIsHero` / `bIsItem` flags), evaluated by the generic Slate material evaluator (`SlateMat`: instance chain + uniform expression set + the compiled
//! pixel shader on the `fn-dxbc` interpreter) with the item's runtime parameters, exactly what `UMaterialInstanceDynamic::Set*ParameterValue` does:
//! * `Icon` = `GetSmallPreviewImage` of the item (the `ItemIconWidget` Kismet does `GetSmallPreviewImage` -> `MakeBrushFromTexture`);
//! * `Rarity 1..5` = `Color1..Color5` of the item's `EFortRarity` row in `/Game/Balance/RarityData` (exe `0x2f6bc90`: copied from the row at offsets +0x18..+0x58);
//! * `bIsHero = 1` for Hero / AthenaCharacter items, `bIsItem = 0` for cosmetics (types of `ItemTypesThatShouldUseCosmeticItemCardMaterials`), `bIsSchematic = 0`,
//!   `bIsLead = 0`, `sSchematicAnimate = 0`, `bAnimateBorder = 1` for rarity >= 4 (the byte at [rsp+0x46] compared with 4), `bBorderOn = 1`, `bIsLoading = 0` once the icon loaded;
//!   `gTime` / `Desat` are never set natively (material defaults).
//! The card's desired size per `EFortItemCardSize` is a native table: see [`CARD_SIZES`].
use super::{LockerItem, Slot};
use crate::assets::{Sampler, Texture};
use crate::draw::{RenderList, TextureId};
use crate::layout::Rect;
use crate::scene::UiContext;
use crate::screens::athena_lobby::slate_mat::SlateMat;
use crate::screens::athena_lobby::umg::{Ui, Vis};
use fn_dxbc_cli::uniform::Overrides;
use serde_json::json;
use std::collections::HashMap;

/// Card baking budget: the generic material evaluator runs the cooked pixel shader on the CPU, so a frame bakes at most `max_per_frame` new cards (the rest pop in on
/// the following frames; `pending` tells the application to keep drawing).
pub struct CardMats { pub max_per_frame: usize, pub baked: usize, pub pending: bool, jobs: HashMap<String, std::sync::mpsc::Receiver<(u32, u32, Vec<u8>)>>, templates: HashMap<String, SlateMat> }
impl Default for CardMats { fn default() -> Self { CardMats { max_per_frame: 4, baked: 0, pending: false, jobs: HashMap::new(), templates: HashMap::new() } } }
pub const CARD_JOBS: usize = 16;

/// Desired size (Slate units) per `EFortItemCardSize` (XXS XS S M L XL; the enum has a seventh XXL that reuses XL) of a COSMETIC card. RECOVERED from the exe:
/// `UFortMultiSizeItemCard::RebuildWidget` (RVA 0x3050b90) builds an `SBox` whose Width/HeightOverride come from the size lookup `0x2fa66e0`, which reads two
/// `TMap<EFortItemCardSize, FVector2D>` of the `UFortUIDataConfiguration` singleton, loaded from `/Game/UI/Foundation/Config/DefaultUIDataConfiguration`:
/// `PersonnelAndSchematicCardSizes` (+0xB90, the tall cards) and `OtherItemCardSizes` (+0xBE0). The selector `0x2fa93c0` uses the tall table for every type in
/// `ItemTypesThatShouldUseCosmeticItemCardMaterials` (all the Athena cosmetic types), so these are the tall sizes.
pub const CARD_SIZES: [(&str, [f32; 2]); 6] = [("XXS", [44.0, 72.0]), ("XS", [60.0, 98.0]), ("S", [88.0, 142.0]), ("M", [128.0, 207.0]), ("L", [160.0, 258.0]), ("XL", [256.0, 414.0])];

/// `EFortItemCardSize::L` -> `"L"`.
pub fn size_name(s: &str) -> &'static str {
    let t = s.rsplit("::").next().unwrap_or(s);
    CARD_SIZES.iter().find(|(n, _)| *n == t).map(|(n, _)| *n).unwrap_or("L")
}
pub fn size_of(name: &str) -> [f32; 2] { CARD_SIZES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s).unwrap_or([128.0, 128.0]) }

/// Turn the native card widget into a fixed-size leaf for the generic layout (`SizeBox` with width / height overrides and no child).
pub fn make_card_leaf(ui: &mut Ui, id: usize, size: &str) {
    let s = size_of(size);
    ui.w[id].class = "SizeBox".into();
    let p = &mut ui.w[id].props;
    p["WidthOverride"] = json!(s[0]);
    p["HeightOverride"] = json!(s[1]);
    p["bOverride_WidthOverride"] = json!(true);
    p["bOverride_HeightOverride"] = json!(true);
    p["_card_size"] = json!(size);
}

#[derive(Clone, Debug)]
pub struct CardSpec { pub widget: usize, pub slot: Slot, pub item: LockerItem, pub clip: Option<Rect> }

/// The widget and all its ancestors draw (Slate paints nothing below a collapsed / hidden widget; only the active page of a switcher).
pub fn effectively_visible(ui: &Ui, id: usize) -> bool {
    let mut cur = id;
    loop {
        let w = &ui.w[cur];
        if !w.vis.draws() { return false; }
        if let Some(p) = w.parent {
            if matches!(ui.w[p].class.as_str(), "WidgetSwitcher" | "CommonWidgetSwitcher") {
                let idx = ui.w[p].props.get("ActiveWidgetIndex").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize;
                if ui.w[p].kids.get(idx) != Some(&cur) { return false; }
            }
            cur = p;
        } else { return true; }
    }
}

/// Product of the alphas the painter applies to the widget: user widget `ColorAndOpacity` and border `ContentColorAndOpacity` of the ancestors.
pub fn content_alpha(ui: &Ui, id: usize) -> f32 {
    let mut a = 1.0f32;
    let mut cur = Some(id);
    while let Some(c) = cur {
        let w = &ui.w[c];
        if w.user.is_some() { if let Some(v) = w.props.get("ColorAndOpacity").and_then(|v| v.as_array()).and_then(|v| v.get(3)).and_then(|v| v.as_f64()) { a *= v as f32; } }
        if matches!(w.class.as_str(), "Border" | "CommonBorder") { if let Some(v) = w.props.get("ContentColorAndOpacity").and_then(|v| v.as_array()).and_then(|v| v.get(3)).and_then(|v| v.as_f64()) { a *= v as f32; } }
        cur = w.parent;
    }
    a
}

/// `FortRarityItemData.Color1..4` of a `RarityData` row (linear RGBA).
fn rarity_colors(ui: &Ui, rarity: usize) -> [[f32; 4]; 5] {
    let mut out = [[1.0; 4]; 5];
    if let Some(p) = ui.store.pkg("/Game/Balance/RarityData") {
        if let Some(e) = p.named("RarityData") {
            let key = if rarity == 0 { "RarityCollection".to_owned() } else { format!("RarityCollection[{rarity}]") };
            if let Some(row) = e.props.get(&key) {
                for (i, o) in out.iter_mut().enumerate() {
                    if let Some(c) = row.get(format!("Color{}", i + 1)).and_then(|v| v.as_array()) { for k in 0..4 { o[k] = c.get(k).and_then(|x| x.as_f64()).unwrap_or(1.0) as f32; } }
                }
            }
        }
    }
    out
}

/// Draw one card over `rect` (pixels): the material is baked once per (size, kind, rarity, icon) into a texture of the store.
#[allow(clippy::too_many_arguments)]
pub fn draw_card(ui: &Ui, uic: &mut UiContext, mats: &mut CardMats, out: &mut RenderList, rect: Rect, spec: &CardSpec, alpha: f32, missing: &mut Vec<String>) {
    let size = ui.w[spec.widget].props.get("_card_size").and_then(|v| v.as_str()).unwrap_or("L").to_owned();
    let (w, h) = (rect.w.round().max(1.0) as u32, rect.h.round().max(1.0) as u32);
    let hero = spec.slot == Slot::Outfit;
    let key = format!("lockercard:{size}:{hero}:{}:{:?}:{w}x{h}", spec.item.rarity, spec.item.icon);
    let tid: TextureId = match uic.assets.store.named(&key) {
        Some(t) => t,
        None => {
            if let Some(rx) = mats.jobs.get(&key) {
                match rx.try_recv() {
                    Ok((tw, th, px)) => { mats.jobs.remove(&key); uic.assets.store.insert_named(&key, Texture::new(tw, th, px, Sampler::LINEAR)) }
                    Err(std::sync::mpsc::TryRecvError::Empty) => { mats.pending = true; return; }
                    Err(_) => { mats.jobs.remove(&key); return; }
                }
            } else {
            if mats.baked >= mats.max_per_frame || mats.jobs.len() >= CARD_JOBS { mats.pending = true; return; }
            mats.baked += 1;
            let res = format!("/Game/UI/Foundation/Items/Materials/M-Item-Card-Cosmetic-Inst-{size}.M-Item-Card-Cosmetic-Inst-{size}");
            let mut ov = Overrides::default();
            let cols = rarity_colors(ui, spec.item.rarity);
            for (i, c) in cols.iter().enumerate() { ov.vectors.insert(format!("Rarity {}", i + 1), *c); }
            for (k, v) in [("bIsHero", if hero { 1.0 } else { 0.0 }), ("bIsItem", 0.0), ("bIsSchematic", 0.0), ("bIsLead", 0.0), ("sSchematicAnimate", 0.0), ("bAnimateBorder", if spec.item.rarity >= 4 { 1.0 } else { 0.0 }), ("bBorderOn", 1.0), ("bIsLoading", 0.0)] { ov.scalars.insert(k.into(), v); }
            let mut tex = HashMap::new();
            if let Some(i) = &spec.item.icon { tex.insert("Icon".to_owned(), i.clone()); }
            let store = ui.store.clone();
            let made = match mats.templates.get(&res) {
                Some(t) => Ok(t.respecialize(&store, &mut uic.assets, &ov, &tex)),
                None => SlateMat::load_with(&store, &mut uic.assets, &res, &ov, &tex).map(|m| { mats.templates.insert(res.clone(), m.respecialize(&store, &mut uic.assets, &Overrides::default(), &HashMap::new())); m }),
            };
            match made {
                Ok(m) if m.is_ready() => {
                    for wm in &m.warnings { let s = format!("card material: {wm}"); if !missing.contains(&s) { missing.push(s); } }
                    let (tx, rx) = std::sync::mpsc::channel();
                    let (rw, rh) = (rect.w, rect.h);
                    if std::thread::Builder::new().name("locker-card".into()).spawn(move || { let px = m.raster(w, h, (rw, rh), (rw, rh), [0.0; 4], false, [1.0; 4], false, 0.0); let _ = tx.send((w, h, px)); }).is_ok() {
                        mats.jobs.insert(key, rx);
                    }
                    mats.pending = true;
                    return;
                }
                Ok(_) => { missing.push(format!("{res}: no pixel shader")); return; }
                Err(e) => { let s = format!("{res}: {e}"); if !missing.contains(&s) { missing.push(s); } return; }
            }
            }
        }
    };
    out.quad(tid, rect, [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 1.0, alpha]);
}

/// Everything a hidden state needs is in the widget; kept for the controller's hover rule (cards are not interactive on their own).
pub fn is_card(ui: &Ui, id: usize) -> bool { ui.w[id].props.get("_card_size").is_some() }

#[allow(dead_code)]
fn _unused(_: Vis) {}


