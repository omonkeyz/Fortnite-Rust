//! Slate layout of the composed UMG tree: desired sizes bottom-up (`ComputeDesiredSize`), arrangement top-down (`ArrangeChildren`), all in
//! Slate units (the DPI scale is applied when painting). The rules are the ones of `SOverlay`, `SBoxPanel`, `SBox`, `SBorder`, `SSpacer`,
//! `SImage`, `STextBlock` and `SButton` (UE 4.16).
use super::store::{jenum, jf, jmargin, jvec};
use super::styles::{Brush, BtnState, TextStyle};
use super::umg::{Ui, Vis};
use crate::scene::UiContext;
use crate::text::FontId;
use serde_json::Value;

pub type R = [f32; 4];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HA { Fill, Left, Center, Right }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VA { Fill, Top, Center, Bottom }

fn ha(s: &str) -> HA { match s.rsplit("::").next().unwrap_or(s) { "HAlign_Left" => HA::Left, "HAlign_Center" => HA::Center, "HAlign_Right" => HA::Right, _ => HA::Fill } }
fn va(s: &str) -> VA { match s.rsplit("::").next().unwrap_or(s) { "VAlign_Top" => VA::Top, "VAlign_Center" => VA::Center, "VAlign_Bottom" => VA::Bottom, _ => VA::Fill } }

pub struct Layout {
    pub rect: Vec<R>,
    pub desired: Vec<Option<[f32; 2]>>,
    /// measured width of auto-wrapping text blocks of the previous pass
    pub wrap_w: Vec<f32>,
    pub scale: f32,
}

pub struct Ctx<'a> { pub ui: &'a mut Ui, pub uic: &'a mut UiContext, pub lay: Layout }

pub const EDITABLE_HINT_ALPHA: f32 = 0.35;
pub fn text_px(size: f32, scale: f32) -> u32 { crate::screens::atlas_px(size, scale) }

impl<'a> Ctx<'a> {
    pub fn text_style(&mut self, id: usize) -> TextStyle {
        let store = self.ui.store.clone();
        let cls = self.ui.w[id].text_style.clone().or_else(|| self.ui.w[id].props.get("Style").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_owned()));
        // a plain `TextBlock` (no style class) draws its own `Font` (`FSlateFontInfo`; UMG default Roboto Bold 24)
        let mut st = match cls { Some(c) => self.ui.styles.text(&store, &c), None => { let mut t = TextStyle::plain(); if let Some(f) = self.ui.w[id].props.get("Font") { t.apply_font(f); } t } };
        if self.ui.w[id].class == "EditableText" {
            if let Some(ws) = self.ui.w[id].props.get("WidgetStyle") {
                if let Some(f) = ws.get("Font") { st.apply_font(f); }
                if let Some(c) = ws.get("ColorAndOpacity").and_then(|c| c.get("SpecifiedColor")).and_then(|c| jvec::<4>(Some(c))) { st.color = c; }
            }
            if self.ui.w[id].text.as_deref().map_or(true, |t| t.is_empty()) { st.color[3] = EDITABLE_HINT_ALPHA; }
        }
        if let Some(sc) = self.ui.w[id].props.get("ShadowOffset").and_then(|_| jvec::<2>(self.ui.w[id].props.get("ShadowOffset"))) { st.shadow_offset = sc; st.uses_shadow = true; if st.shadow_color[3] == 0.0 { st.shadow_color = [0.0, 0.0, 0.0, 1.0]; } }
        if let Some(c) = jvec::<4>(self.ui.w[id].props.get("ShadowColorAndOpacity")) { st.shadow_color = c; }
        st
    }

    pub fn text_value(&self, id: usize) -> String {
        let w = &self.ui.w[id];
        if w.class == "EditableText" {
            return match w.text.as_deref() {
                Some(t) if !t.is_empty() => if w.props.get("IsPassword").and_then(|b| b.as_bool()).unwrap_or(false) { "•".repeat(t.chars().count()) } else { t.to_owned() },
                _ => w.props.get("HintText").and_then(|t| self.ui.text_of(t)).unwrap_or_default(),
            };
        }
        if let Some(t) = &w.text { return t.clone(); }
        w.props.get("Text").and_then(|t| self.ui.text_of(t)).unwrap_or_default()
    }

    /// Lines of a text block after wrapping at `wrap_w` (Slate units), with the style, atlas pixel size and line height (Slate units).
    pub fn text_lines(&mut self, id: usize, wrap_w: Option<f32>) -> (Vec<String>, TextStyle, u32, f32) {
        let st = self.text_style(id);
        let text = self.text_value(id);
        let scale = self.lay.scale;
        let px = text_px(st.size, scale);
        let lh = self.uic.fonts.line_height(st.font, px as f32) / scale;
        let ms = |s: &str, uic: &UiContext| uic.fonts.measure(st.font, s, px as f32) / scale;
        let mut lines: Vec<String> = vec![];
        for line in text.split('\n') {
            match wrap_w {
                Some(ww) if ww > 0.0 => {
                    let mut cur = String::new();
                    for word in line.split(' ') {
                        let cand = if cur.is_empty() { word.to_owned() } else { format!("{cur} {word}") };
                        if !cur.is_empty() && ms(&cand, self.uic) > ww { lines.push(std::mem::take(&mut cur)); cur = word.to_owned(); } else { cur = cand; }
                    }
                    lines.push(cur);
                }
                _ => lines.push(line.to_owned()),
            }
        }
        (lines, st, px, lh)
    }

    pub fn measure_text(&mut self, id: usize, wrap_w: Option<f32>) -> [f32; 2] {
        let (lines, st, px, lh) = self.text_lines(id, wrap_w);
        let scale = self.lay.scale;
        let width = lines.iter().map(|l| self.uic.fonts.measure(st.font, l, px as f32) / scale).fold(0.0f32, f32::max);
        let m = jmargin(self.ui.w[id].props.get("Margin"));
        [width + m[0] + m[2], lines.len() as f32 * lh + m[1] + m[3]]
    }
    fn wrap_width(&self, id: usize) -> Option<f32> {
        let p = &self.ui.w[id].props;
        if let Some(w) = p.get("WrapTextAt").and_then(jf).filter(|w| *w > 0.0) { return Some(w); }
        if p.get("AutoWrapText").and_then(|b| b.as_bool()).unwrap_or(false) {
            let w = self.lay.wrap_w[id];
            if w > 0.0 { return Some(w); }
        }
        None
    }

    // ---- slot helpers ---------------------------------------------------------------------------------------------------------------------

    pub fn slot_padding(&self, id: usize) -> [f32; 4] {
        match self.ui.w[id].slot_class.as_str() {
            "" | "CanvasPanelSlot" => [0.0; 4],
            _ => jmargin(self.ui.w[id].slot.get("Padding")),
        }
    }
    pub fn slot_align(&self, id: usize) -> (HA, VA) {
        let w = &self.ui.w[id];
        let (dh, dv) = match w.slot_class.as_str() {
            "OverlaySlot" => (HA::Left, VA::Top),
            _ => (HA::Fill, VA::Fill),
        };
        let s = &w.slot;
        let h = s.get("HorizontalAlignment").or_else(|| s.get("HAlign")).and_then(|v| v.as_str()).map(ha);
        let v = s.get("VerticalAlignment").or_else(|| s.get("VAlign")).and_then(|v| v.as_str()).map(va);
        let h = w.slot_halign.as_deref().map(ha).or(h).unwrap_or(dh);
        let v = w.slot_valign.as_deref().map(va).or(v).unwrap_or(dv);
        (h, v)
    }
    /// (fill?, value) of a box slot.
    pub fn slot_size(&self, id: usize) -> (bool, f32) {
        let s = self.ui.w[id].slot.get("Size");
        let fill = s.and_then(|o| o.get("SizeRule")).and_then(|v| v.as_str()).map_or(false, |r| r.ends_with("Fill"));
        let val = s.and_then(|o| o.get("Value")).and_then(jf).unwrap_or(1.0);
        (fill, val)
    }

    fn visible_kids(&self, id: usize) -> Vec<usize> { self.ui.w[id].kids.iter().copied().filter(|&k| self.ui.w[k].vis.takes_space()).collect() }

    // ---- desired size ---------------------------------------------------------------------------------------------------------------------

    pub fn desired(&mut self, id: usize) -> [f32; 2] {
        if let Some(d) = self.lay.desired[id] { return d; }
        let d = self.compute_desired(id);
        let ds = self.ui.w[id].desired_scale;
        let d = [d[0] * ds[0], d[1] * ds[1]];
        self.lay.desired[id] = Some(d);
        d
    }

    fn child_total(&mut self, k: usize) -> [f32; 2] {
        let d = self.desired(k);
        let p = self.slot_padding(k);
        [d[0] + p[0] + p[2], d[1] + p[1] + p[3]]
    }

    fn sizebox_flag(p: &Value, key: &str) -> Option<f32> {
        // `USizeBox::bOverride_*` (default false, serialized only when true): a stored value whose flag is not set is ignored by `SBox`
        // (e.g. `SizeBox_231.HeightOverride = 88` of the tab list and `SizeBox_2.MinDesiredHeight = 200` of the daily challenges panel).
        let v = p.get(key).and_then(jf)?;
        if p.get(&format!("bOverride_{key}")).and_then(|b| b.as_bool()).unwrap_or(false) { Some(v) } else { None }
    }

    fn compute_desired(&mut self, id: usize) -> [f32; 2] {
        let class = self.ui.w[id].class.clone();
        match class.as_str() {
            "Spacer" => jvec::<2>(self.ui.w[id].props.get("Size")).unwrap_or([1.0, 1.0]),
            "Image" | "FortSZAwareImage" | "CommonLazyImage" => { // FRONT END: `UCommonLazyImage` lays out as its brush (the lazy load only defers the texture)
                let b = self.brush_of(id);
                b.map(|b| b.image_size).unwrap_or([32.0, 32.0])
            }
            "TextBlock" | "CommonTextBlock" | "CommonNumericTextBlock" | "FortItemCountTextBlock" | "SubtitleDisplay" | "EditableText" => {
                let ww = self.wrap_width(id);
                self.measure_text(id, ww)
            }
            "CommonActionWidget" | "FortMultiSizeItemCard" | "FortItemIcon" | "FortChatWidget" | "ProgressBar" => [0.0, 0.0],
            "Overlay" | "SafeZone" | "BackgroundBlur" | "NamedSlot" | "CommonWidgetStack" | "CommonLoadGuard" | "MenuAnchor" | "CommonVisibilityWidget" => {
                let mut m = [0.0f32; 2];
                for k in self.visible_kids(id) { let d = self.child_total(k); m[0] = m[0].max(d[0]); m[1] = m[1].max(d[1]); }
                if class == "CommonVisibilityWidget" { let p = jmargin(self.ui.w[id].props.get("Padding")); m = [m[0] + p[0] + p[2], m[1] + p[1] + p[3]]; }
                m
            }
            "WidgetSwitcher" | "CommonWidgetSwitcher" => {
                let kids = self.ui.w[id].kids.clone();
                let idx = self.ui.w[id].props.get("ActiveWidgetIndex").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize;
                match kids.get(idx) { Some(&k) if self.ui.w[k].vis.takes_space() => self.child_total(k), _ => [0.0, 0.0] }
            }
            "VerticalBox" | "HorizontalBox" | "CommonTreeView" | "CommonListView" => {
                let horiz = class == "HorizontalBox";
                let (mut along, mut cross) = (0.0f32, 0.0f32);
                for k in self.visible_kids(id) {
                    let d = self.child_total(k);
                    if horiz { along += d[0]; cross = cross.max(d[1]); } else { along += d[1]; cross = cross.max(d[0]); }
                }
                if horiz { [along, cross] } else { [cross, along] }
            }
            "SizeBox" => {
                let kid = self.visible_kids(id).first().copied();
                let c = kid.map(|k| self.child_total(k)).unwrap_or([0.0, 0.0]);
                let p = self.ui.w[id].props.clone();
                let mut out = [c[0], c[1]];
                for (i, (ov, mn, mx)) in [("WidthOverride", "MinDesiredWidth", "MaxDesiredWidth"), ("HeightOverride", "MinDesiredHeight", "MaxDesiredHeight")].iter().enumerate() {
                    out[i] = match Self::sizebox_flag(&p, ov) {
                        Some(v) => v,
                        None => {
                            let mut v = c[i];
                            if let Some(m) = Self::sizebox_flag(&p, mn) { v = v.max(m); }
                            if let Some(m) = Self::sizebox_flag(&p, mx) { v = v.min(m); }
                            v
                        }
                    };
                }
                out
            }
            "Border" | "CommonBorder" => {
                let pad = self.border_padding(id);
                let kid = self.visible_kids(id).first().copied();
                let c = kid.map(|k| self.desired(k)).unwrap_or([0.0, 0.0]);
                [c[0] + pad[0] + pad[2], c[1] + pad[1] + pad[3]]
            }
            // `UButton` / `SButton`: the content (with its `ButtonSlot` padding) plus the style's `NormalPadding` (`SButton::GetCombinedPadding` with the button not pressed)
            "Button" => {
                let np = self.plain_button_padding(id);
                let kid = self.visible_kids(id).first().copied();
                let c = kid.map(|k| self.child_total(k)).unwrap_or([0.0, 0.0]);
                [c[0] + np[0] + np[2], c[1] + np[1] + np[3]]
            }
            _ => {
                if self.ui.w[id].user.is_some() { return self.user_desired(id); }
                let mut m = [0.0f32; 2];
                for k in self.visible_kids(id) { let d = self.child_total(k); m[0] = m[0].max(d[0]); m[1] = m[1].max(d[1]); }
                m
            }
        }
    }

    /// Border content padding: the border's own `Padding` (identical to its `BorderSlot` padding in the data).
    pub fn border_padding(&self, id: usize) -> [f32; 4] {
        let w = &self.ui.w[id];
        if let Some(k) = w.kids.first() { if self.ui.w[*k].slot_class == "BorderSlot" { return jmargin(self.ui.w[*k].slot.get("Padding")); } }
        jmargin(w.props.get("Padding"))
    }

    fn user_desired(&mut self, id: usize) -> [f32; 2] {
        let kid = self.visible_kids(id).first().copied();
        let c = kid.map(|k| self.desired(k)).unwrap_or([0.0, 0.0]);
        if self.ui.is_common_button(id) {
            let st = self.ui.button_style(id);
            let (pad, mw, mh) = st.map(|s| (s.padding, s.min_width, s.min_height)).unwrap_or(([0.0; 4], 0.0, 0.0));
            let p = &self.ui.w[id].props;
            let imw = p.get("MinWidth").and_then(jf).unwrap_or(0.0);
            let imh = p.get("MinHeight").and_then(jf).unwrap_or(0.0);
            let w = (c[0] + pad[0] + pad[2]).max(mw).max(imw);
            let h = (c[1] + pad[1] + pad[3]).max(mh).max(imh);
            return [w, h];
        }
        c
    }

    pub fn plain_button_padding(&self, id: usize) -> [f32; 4] { jmargin(self.ui.w[id].props.get("WidgetStyle").and_then(|s| s.get("NormalPadding"))) }
    pub fn button_padding(&mut self, id: usize) -> [f32; 4] {
        if self.ui.is_common_button(id) { self.ui.button_style(id).map(|s| s.padding).unwrap_or([0.0; 4]) } else { [0.0; 4] }
    }

    pub fn brush_of(&mut self, id: usize) -> Option<Brush> {
        if let Some(b) = &self.ui.w[id].brush { return Some(b.clone()); }
        self.ui.w[id].props.get("Brush").and_then(Brush::from_json)
    }

    // ---- arrangement ----------------------------------------------------------------------------------------------------------------------

    /// Place a child of desired size `d` inside `area` (already reduced by the slot padding) per the slot alignment.
    fn align_in(&self, k: usize, d: [f32; 2], area: R) -> R {
        let (h, v) = self.slot_align(k);
        let (x, w) = match h { HA::Fill => (area[0], area[2]), HA::Left => (area[0], d[0].min(area[2])), HA::Center => { let w = d[0].min(area[2]); (area[0] + (area[2] - w) * 0.5, w) } HA::Right => { let w = d[0].min(area[2]); (area[0] + area[2] - w, w) } };
        let (y, hh) = match v { VA::Fill => (area[1], area[3]), VA::Top => (area[1], d[1].min(area[3])), VA::Center => { let hh = d[1].min(area[3]); (area[1] + (area[3] - hh) * 0.5, hh) } VA::Bottom => { let hh = d[1].min(area[3]); (area[1] + area[3] - hh, hh) } };
        [x, y, w, hh]
    }

    fn arrange_slot(&mut self, k: usize, area: R) {
        let p = self.slot_padding(k);
        let inner = [area[0] + p[0], area[1] + p[1], (area[2] - p[0] - p[2]).max(0.0), (area[3] - p[1] - p[3]).max(0.0)];
        let d = self.desired(k);
        let r = self.align_in(k, d, inner);
        self.arrange(k, r);
    }

    pub fn arrange(&mut self, id: usize, rect: R) {
        self.lay.rect[id] = rect;
        let class = self.ui.w[id].class.clone();
        if class == "AthenaRotator_C" { /* handled as a user widget below */ }
        match class.as_str() {
            "VerticalBox" | "HorizontalBox" | "CommonTreeView" | "CommonListView" => {
                let horiz = class == "HorizontalBox";
                let kids = self.visible_kids(id);
                let avail = if horiz { rect[2] } else { rect[3] };
                let mut fixed = 0.0f32;
                let mut stretch = 0.0f32;
                let mut info = vec![];
                // INVENTORY FIX4 (UE 4.16 `SBoxPanel.cpp` ArrangeChildrenAlong): EVERY visible slot's padding counts as fixed space, a stretch slot gets
                // `NonFixedSpace * coeff / total` for its widget plus its own padding (the port gave stretch slots their padding out of the shared space)
                for &k in &kids {
                    let d = self.child_total(k);
                    let (fill, val) = self.slot_size(k);
                    let p = self.slot_padding(k);
                    let pad = if horiz { p[0] + p[2] } else { p[1] + p[3] };
                    let along = if horiz { d[0] } else { d[1] };
                    if fill { stretch += val.max(0.0); fixed += pad; } else { fixed += along; }
                    info.push((k, fill, val, along, pad));
                }
                let remain = (avail - fixed).max(0.0);
                let mut pos = if horiz { rect[0] } else { rect[1] };
                for (k, fill, val, along, pad) in info {
                    let size = if fill { (if stretch > 0.0 { remain * val.max(0.0) / stretch } else { 0.0 }) + pad } else { along };
                    let area = if horiz { [pos, rect[1], size, rect[3]] } else { [rect[0], pos, rect[2], size] };
                    self.arrange_slot(k, area);
                    pos += size;
                }
            }
            "WidgetSwitcher" | "CommonWidgetSwitcher" => {
                let kids = self.ui.w[id].kids.clone();
                let idx = self.ui.w[id].props.get("ActiveWidgetIndex").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize;
                if let Some(&k) = kids.get(idx) { if self.ui.w[k].vis.takes_space() { self.arrange_slot(k, rect); } }
            }
            "Border" | "CommonBorder" => {
                let pad = self.border_padding(id);
                let inner = [rect[0] + pad[0], rect[1] + pad[1], (rect[2] - pad[0] - pad[2]).max(0.0), (rect[3] - pad[1] - pad[3]).max(0.0)];
                for k in self.visible_kids(id) { let d = self.desired(k); let r = self.align_in(k, d, inner); self.arrange(k, r); }
            }
            "Button" => {
                let np = self.plain_button_padding(id);
                let inner = [rect[0] + np[0], rect[1] + np[1], (rect[2] - np[0] - np[2]).max(0.0), (rect[3] - np[1] - np[3]).max(0.0)];
                for k in self.visible_kids(id) { self.arrange_slot(k, inner); }
            }
            _ if self.ui.w[id].user.is_some() => {
                let pad = self.button_padding(id);
                let inner = [rect[0] + pad[0], rect[1] + pad[1], (rect[2] - pad[0] - pad[2]).max(0.0), (rect[3] - pad[1] - pad[3]).max(0.0)];
                for k in self.visible_kids(id) { self.arrange(k, inner); }
            }
            "ScaleBox" => {
                let p = self.ui.w[id].props.clone();
                let stretch = p.get("Stretch").and_then(|v| v.as_str()).unwrap_or("EStretch::ScaleToFit").rsplit("::").next().unwrap_or("ScaleToFit").to_owned();
                let dir = p.get("StretchDirection").and_then(|v| v.as_str()).unwrap_or("EStretchDirection::Both").rsplit("::").next().unwrap_or("Both").to_owned();
                let user = p.get("UserSpecifiedScale").and_then(jf).unwrap_or(1.0);
                for k in self.visible_kids(id) {
                    let d = self.child_total(k);
                    let (sx, sy) = (if d[0] > 0.0 { rect[2] / d[0] } else { 1.0 }, if d[1] > 0.0 { rect[3] / d[1] } else { 1.0 });
                    let (mut ax, mut ay) = match stretch.as_str() {
                        "None" => (1.0, 1.0),
                        "Fill" => (sx, sy),
                        "ScaleToFitX" => (sx, sx),
                        "ScaleToFitY" => (sy, sy),
                        "ScaleToFill" => { let m = sx.max(sy); (m, m) }
                        "UserSpecified" => (user, user),
                        _ => { let m = sx.min(sy); (m, m) }
                    };
                    match dir.as_str() { "DownOnly" => { ax = ax.min(1.0); ay = ay.min(1.0); } "UpOnly" => { ax = ax.max(1.0); ay = ay.max(1.0); } _ => {} }
                    let (w, h) = (d[0] * ax, d[1] * ay);
                    let s = &self.ui.w[k].slot;
                    let hal = s.get("HorizontalAlignment").and_then(|v| v.as_str()).map(ha).unwrap_or(HA::Center);
                    let val = s.get("VerticalAlignment").and_then(|v| v.as_str()).map(va).unwrap_or(VA::Center);
                    let x = match hal { HA::Left | HA::Fill => rect[0], HA::Center => rect[0] + (rect[2] - w) * 0.5, HA::Right => rect[0] + rect[2] - w };
                    let y = match val { VA::Top | VA::Fill => rect[1], VA::Center => rect[1] + (rect[3] - h) * 0.5, VA::Bottom => rect[1] + rect[3] - h };
                    self.arrange(k, [x, y, w, h]);
                }
            }
            "Image" | "FortSZAwareImage" | "CommonLazyImage" | "TextBlock" | "CommonTextBlock" | "CommonNumericTextBlock" | "Spacer" | "CommonActionWidget" => {}
            "SizeBox" | "SafeZone" | "BackgroundBlur" | "Overlay" | "NamedSlot" | "CommonWidgetStack" | "CommonLoadGuard" | "MenuAnchor" | "CommonVisibilityWidget" | _ => {
                for k in self.visible_kids(id) { self.arrange_slot(k, rect); }
            }
        }
        if self.ui.w[id].class == "CommonTextBlock" || self.ui.w[id].class == "TextBlock" { self.lay.wrap_w[id] = rect[2]; }
    }
}

pub fn new_layout(n: usize, scale: f32) -> Layout { Layout { rect: vec![[0.0; 4]; n], desired: vec![None; n], wrap_w: vec![0.0; n], scale } }

/// Run the layout of the whole tree for a viewport of `size` Slate units (two passes: auto-wrapping text uses the width of the previous pass).
/// `pads` = extra root widgets (world-space player pad widgets): (root, projected component location in Slate units, `UWidgetComponent.Pivot`).
/// Screen-space widget component rule (UE 4.16 `SWorldWidgetScreenLayer`): slot position = projected location, size = desired size (`bDrawAtDesiredSize`),
/// alignment = Pivot, i.e. the widget's top-left is `anchor - desired * pivot`.
pub fn run(ui: &mut Ui, uic: &mut UiContext, root: usize, size: [f32; 2], scale: f32, pads: &[(usize, [f32; 2], [f32; 2])]) -> Layout { run_with(ui, uic, root, size, scale, pads, &[]) }
pub fn run_with(ui: &mut Ui, uic: &mut UiContext, root: usize, size: [f32; 2], scale: f32, pads: &[(usize, [f32; 2], [f32; 2])], overlays: &[usize]) -> Layout {
    let n = ui.w.len();
    let mut lay = new_layout(n, scale);
    for _ in 0..2 {
        let wrap = lay.wrap_w.clone();
        lay = new_layout(n, scale);
        lay.wrap_w = wrap;
        let mut c = Ctx { ui, uic, lay };
        c.arrange(root, [0.0, 0.0, size[0], size[1]]);
        for &(p, a, pv) in pads { let d = c.desired(p); c.arrange(p, [a[0] - d[0] * pv[0], a[1] - d[1] * pv[1], d[0], d[1]]); }
        for &o in overlays { c.arrange(o, [0.0, 0.0, size[0], size[1]]); }
        lay = c.lay;
    }
    lay
}
pub fn halign_of(s: &str) -> HA { ha(s) }
pub fn valign_of(s: &str) -> VA { va(s) }
pub fn enum_of(v: Option<&Value>) -> Option<&str> { jenum(v) }
pub fn font_of(st: &TextStyle) -> FontId { st.font }
pub fn state_of(_s: BtnState) {}
pub fn vis_of(v: Vis) -> bool { v.draws() }


