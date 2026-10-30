use crate::assets::{Sampler, TexInfo, Texture};
use crate::draw::{MaterialKind, RenderList, TextureId};
use crate::layout::{dpi_scale, Rect};
use crate::scene::UiContext;
use crate::screens::{art, atlas_px, put_text, srgb3, Snap, SLATE_PT_TO_PX};
use crate::text::FontId;
use std::collections::HashMap;

pub const OVERLAY_PAD: (f32, f32) = (20., 10.);
pub const TEAM_PAD_TOP: f32 = 10.;
pub const ROW_PAD_TOP: f32 = 8.;
pub const PLAYER_INFO_PT: f32 = 10.;
pub const PLAYER_INFO_COLOR: [f32; 4] = [1., 1., 1., 0.4];
pub const MARKER_CELL_W: f32 = 24.;
pub const MARKER: f32 = 18.;
pub const SPEAKER: f32 = 16.;
pub const SPEAKER_DX: f32 = -20.;
pub const NAME_PT: f32 = 14.;
pub const NAME_MARGIN: [f32; 4] = [0., -2., 2., -1.];
pub const OUTLINE_SIZE: f32 = 1.;
pub const OUTLINE_COLOR: [f32; 4] = [0., 0., 0., 0.75];
pub const STATUS_CELL: f32 = 24.;
pub const BAR_BOX_W: f32 = 250.;
pub const BAR_PAD_L: f32 = 2.;
pub const SHIELD_H: f32 = 6.;
pub const BAR_GAP: f32 = 2.;
pub const HEALTH_H: f32 = 12.;
pub const DBNO_H: f32 = 20.;
pub const UNFILLED: [f32; 4] = [0., 0., 0., 0.25];
pub const DEAD_NAME: [f32; 3] = [1., 0., 0.007843];
pub const DEAD_ICON_TINT: [f32; 3] = [1., 0., 0.008024];
pub const MIC_DIM_ALPHA: f32 = 0.6;
pub const DYING_K: [f32; 3] = [0.8, 0., 0.];
pub const HEALING_K: [f32; 3] = [0., 0.545724, 0.011494];
pub const NOTO_UPEM: f32 = 2048.;
pub const NOTO_BOLD_BBOX_Y: (f32, f32) = (2189., -806.);
pub const NOTO_REGULAR_BBOX_Y: (f32, f32) = (2146., -806.);
pub const PLAYER_TINTS: [[f32; 3]; 4] = [[0.008023, 0.205079, 0.814847], [1., 0., 0.05448], [0.401978, 0., 1.], [0.991102, 0.679543, 0.000607]];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarColors { pub a: [f32; 3], pub b: [f32; 3] }
pub const HEALTH_BAR: BarColors = BarColors { a: [0.07451, 0.50196, 0.03529], b: [0.17647, 0.72941, 0.07843] };
pub const SHIELD_BAR: BarColors = BarColors { a: [0.03529, 0.18039, 0.83137], b: [0.08627, 0.43922, 0.91373] };
pub const DBNO_BAR: BarColors = BarColors { a: [0.78431, 0.05098, 0.05098], b: [0.78431, 0.05098, 0.05098] };

#[derive(Clone, Debug, PartialEq)]
pub struct TeamMember {
    pub name: String,
    pub player_index: u8,
    pub health: f32,
    pub shield: f32,
    pub dbno: bool,
    pub dead: bool,
    pub reviving: bool,
    pub talking: bool,
    pub muted: bool,
}
impl TeamMember {
    pub fn new(name: &str, player_index: u8, health: f32, shield: f32) -> Self {
        TeamMember { name: name.to_owned(), player_index, health, shield, dbno: false, dead: false, reviving: false, talking: false, muted: false }
    }
    pub fn sanitized(&self) -> Self {
        let f = |v: f32| if v.is_finite() { v.clamp(0., 1.) } else { 0. };
        TeamMember { health: f(self.health), shield: f(self.shield), player_index: self.player_index.min(3), ..self.clone() }
    }
}

pub fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] { [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t] }

fn snap(r: Rect) -> Rect {
    let (x0, y0, x1, y1) = (r.x.round(), r.y.round(), r.right().round(), r.bottom().round());
    Rect::new(x0, y0, (x1 - x0).max(0.), (y1 - y0).max(0.))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowLayout { pub row: Rect, pub marker: Rect, pub speaker: Rect, pub name_x: f32, pub name_baseline: f32, pub cell: Rect, pub shield: Rect, pub health: Rect, pub dbno: Rect }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics { pub scale: f32, pub info_line: f32, pub info_ascent: f32, pub name_line: f32, pub name_ascent: f32 }

pub fn slate_line(bbox_y: (f32, f32), size_px: f32) -> (f32, f32) {
    let line = ((bbox_y.0 - bbox_y.1) / NOTO_UPEM * size_px).round();
    let base = (bbox_y.1 / NOTO_UPEM * size_px).round();
    (line, line + base)
}

pub fn font_px(pt: f32, scale: f32) -> f32 { pt * SLATE_PT_TO_PX * scale }

pub fn metrics_at(scale: f32) -> Metrics {
    let (il, ia) = slate_line(NOTO_REGULAR_BBOX_Y, font_px(PLAYER_INFO_PT, scale));
    let (nl, na) = slate_line(NOTO_BOLD_BBOX_Y, font_px(NAME_PT, scale));
    Metrics { scale, info_line: il, info_ascent: ia, name_line: nl, name_ascent: na }
}

pub fn player_info_baseline(m: &Metrics) -> (f32, f32) { (OVERLAY_PAD.0 * m.scale, OVERLAY_PAD.1 * m.scale + m.info_ascent) }

pub fn team_top(m: &Metrics) -> f32 { OVERLAY_PAD.1 * m.scale + 2. * m.info_line + TEAM_PAD_TOP * m.scale }

pub fn row_height(m: &Metrics) -> f32 { name_line_height(m) + STATUS_CELL * m.scale }

fn name_line_height(m: &Metrics) -> f32 {
    let s = m.scale;
    let text_h = m.name_line + (NAME_MARGIN[1] + NAME_MARGIN[3]) * s;
    text_h.max(MARKER * s)
}

pub fn row_layout(m: &Metrics, index: usize) -> RowLayout {
    let s = m.scale;
    let x0 = OVERLAY_PAD.0 * s;
    let rh = row_height(m);
    let y0 = team_top(m) + ROW_PAD_TOP * s + index as f32 * (rh + ROW_PAD_TOP * s);
    let h = name_line_height(m);
    let text_h = m.name_line + (NAME_MARGIN[1] + NAME_MARGIN[3]) * s;
    let overlay_h = MARKER.max(SPEAKER) * s;
    let overlay_top = y0 + (h - overlay_h) * 0.5;
    let marker = Rect::new(x0 + (MARKER_CELL_W - MARKER) * 0.5 * s, overlay_top + (overlay_h - MARKER * s) * 0.5, MARKER * s, MARKER * s);
    let speaker = Rect::new(x0 + (MARKER_CELL_W - SPEAKER) * 0.5 * s + SPEAKER_DX * s, overlay_top + (overlay_h - SPEAKER * s) * 0.5, SPEAKER * s, SPEAKER * s);
    let text_top = y0 + (h - text_h) * 0.5 + NAME_MARGIN[1] * s;
    let status = y0 + h;
    let bx = x0 + (STATUS_CELL + BAR_PAD_L) * s;
    let bw = (BAR_BOX_W - BAR_PAD_L) * s;
    let bars_top = status + (STATUS_CELL - DBNO_H) * 0.5 * s;
    RowLayout {
        row: Rect::new(x0, y0, (STATUS_CELL + BAR_BOX_W) * s, rh),
        marker, speaker,
        name_x: x0 + (MARKER_CELL_W + NAME_MARGIN[0]) * s,
        name_baseline: text_top + m.name_ascent,
        cell: Rect::new(x0, status, STATUS_CELL * s, STATUS_CELL * s),
        shield: Rect::new(bx, bars_top, bw, SHIELD_H * s),
        health: Rect::new(bx, bars_top + (SHIELD_H + BAR_GAP) * s, bw, HEALTH_H * s),
        dbno: Rect::new(bx, bars_top, bw, DBNO_H * s),
    }
}

pub fn bar_cmds(bar: Rect, progress: f32, c: BarColors) -> (Option<(Rect, MaterialKind)>, Option<Rect>) {
    let bar = snap(bar);
    let p = if progress.is_finite() { progress.clamp(0., 1.) } else { 0. };
    let split = (bar.x + bar.w * p).round();
    let filled = Rect::new(bar.x, bar.y, split - bar.x, bar.h);
    let rest = Rect::new(split, bar.y, bar.right() - split, bar.h);
    let fill = (filled.w > 0.).then(|| (filled, MaterialKind::HudBar { a: c.a, b: lerp3(c.a, c.b, filled.w / bar.w.max(1e-6)) }));
    (fill, (rest.w > 0.).then_some(rest))
}

#[derive(Clone, Copy)]
struct Tex { marker: TexInfo, eliminated: TexInfo, blur: TexInfo, cross: TexInfo, mic_notalk: TexInfo, mic_active: TexInfo, mic_muted: TexInfo }

#[derive(Clone, Default)]
pub struct TeamInfo { tex: Option<Tex>, names: HashMap<(String, u32, u32), (TexInfo, f32, f32)> }

fn tint(c: [f32; 3], a: f32) -> [f32; 4] { let t = srgb3(c); [t[0], t[1], t[2], a] }

impl TeamInfo {
    pub fn new() -> Self { Self::default() }

    pub fn prewarm(&mut self, ctx: &mut UiContext) { let _ = self.ensure_tex(ctx); }
    fn ensure_tex(&mut self, ctx: &mut UiContext) -> Tex {
        if let Some(t) = self.tex { return t; }
        let t = Tex {
            marker: art(ctx, "squad_PinDiamond.png"), eliminated: art(ctx, "squad_Eliminated.png"), blur: art(ctx, "squad_DBNOIcon_Blur.png"), cross: art(ctx, "squad_DBNOIcon_Cross.png"),
            mic_notalk: art(ctx, "mic-notalk.png"), mic_active: art(ctx, "mic-active.png"), mic_muted: art(ctx, "mic-muted.png"),
        };
        self.tex = Some(t); t
    }

    pub fn metrics(w: f32, h: f32) -> Metrics {
        let s = dpi_scale(w, h);
        metrics_at(s)
    }

    fn outlined_name(&mut self, ctx: &mut UiContext, text: &str, pxf: f32, radius: f32) -> (TexInfo, f32, f32) {
        let key = (text.to_owned(), (pxf * 100.).round() as u32, (radius * 100.).round() as u32);
        let pad = radius.ceil() as i32 + 1;
        if let Some(t) = self.names.get(&key) { return *t; }
        let font = ctx.fonts.font(FontId::Bold);
        let mut glyphs = Vec::new();
        let mut pen = 0f32;
        let (mut x_min, mut x_max, mut y_min, mut y_max) = (0i32, 1i32, 0i32, 1i32);
        for ch in text.chars() {
            let (m, bmp) = font.rasterize(ch, pxf);
            let gx = pen.round() as i32 + m.xmin;
            let gy = -m.ymin - m.height as i32;
            if m.width > 0 && m.height > 0 {
                x_min = x_min.min(gx); y_min = y_min.min(gy); x_max = x_max.max(gx + m.width as i32); y_max = y_max.max(gy + m.height as i32);
                glyphs.push((gx, gy, m.width, m.height, bmp));
            }
            pen += m.advance_width;
        }
        x_min = x_min.min(0);
        y_min = y_min.min(0);
        let (w, h) = ((x_max - x_min + 2 * pad) as usize, (y_max - y_min + 2 * pad) as usize);
        let mut cov = vec![0f32; w * h];
        for (gx, gy, gw, gh, bmp) in &glyphs {
            for yy in 0..*gh { for xx in 0..*gw {
                let (tx, ty) = ((gx - x_min + pad) as usize + xx, (gy - y_min + pad) as usize + yy);
                let c = &mut cov[ty * w + tx];
                *c = (*c + bmp[yy * gw + xx] as f32 / 255.).min(1.);
            } }
        }
        let r = pad;
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h as i32 { for x in 0..w as i32 {
            let mut o = 0f32;
            for dy in -r..=r { for dx in -r..=r {
                let (sx, sy) = (x + dx, y + dy);
                if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 { continue; }
                let wgt = (radius + 1. - ((dx * dx + dy * dy) as f32).sqrt()).clamp(0., 1.);
                o = o.max(cov[sy as usize * w + sx as usize] * wgt);
            } }
            let f = cov[y as usize * w + x as usize];
            let oa = OUTLINE_COLOR[3] * o;
            let a = f + oa * (1. - f);
            let i = (y as usize * w + x as usize) * 4;
            if a > 0. {
                let white = f / a;
                let v = white + (1. - white) * OUTLINE_COLOR[0];
                out[i] = (v * 255.).round() as u8; out[i + 1] = out[i]; out[i + 2] = out[i];
                out[i + 3] = (a * 255.).round() as u8;
            }
        } }
        let id = ctx.assets.store.insert(Texture::new(w as u32, h as u32, out, Sampler::LINEAR));
        let t = TexInfo { id, w: w as u32, h: h as u32 };
        let v = (t, (pad - x_min) as f32, (pad - y_min) as f32);
        self.names.insert(key, v);
        v
    }

    pub fn build(&mut self, ctx: &mut UiContext, w: f32, h: f32, player_name: &str, show_player_info: bool, team: &[TeamMember], out: &mut RenderList) {
        if player_name.is_empty() && team.is_empty() { return; }
        let m = Self::metrics(w, h);
        let s = m.scale;
        if show_player_info && !player_name.is_empty() {
            let (x, base) = player_info_baseline(&m);
            put_text(ctx, out, FontId::Regular, atlas_px(PLAYER_INFO_PT, s), x.round(), base.round(), player_name, PLAYER_INFO_COLOR, None, Snap::Pen);
        }
        if team.is_empty() { return; }
        let tex = self.ensure_tex(ctx);
        let np = font_px(NAME_PT, s);
        for (i, mem) in team.iter().enumerate() {
            let mem = mem.sanitized();
            let l = row_layout(&m, i);
            let (mic, mic_a) = if mem.muted { (tex.mic_muted, MIC_DIM_ALPHA) } else if mem.talking { (tex.mic_active, 1.) } else { (tex.mic_notalk, MIC_DIM_ALPHA) };
            if mic.id != TextureId::NONE { out.sprite(mic.id, snap(l.speaker), [1., 1., 1., mic_a]); }
            if tex.marker.id != TextureId::NONE { out.sprite(tex.marker.id, snap(l.marker), tint(PLAYER_TINTS[mem.player_index as usize], 1.)); }
            if !mem.name.is_empty() {
                let (nt, ox, oy) = self.outlined_name(ctx, &mem.name, np, OUTLINE_SIZE * s);
                let color = if mem.dead { tint(DEAD_NAME, 1.) } else { [1., 1., 1., 1.] };
                let x = (l.name_x - ox).round();
                let y = (l.name_baseline - oy).round();
                out.sprite(nt.id, Rect::new(x, y, nt.w as f32, nt.h as f32), color);
            }
            if mem.dbno {
                let k = if mem.reviving { HEALING_K } else { DYING_K };
                if tex.blur.id != TextureId::NONE { out.sprite(tex.blur.id, snap(l.cell), tint(k, 1.)); }
                if tex.cross.id != TextureId::NONE { out.sprite(tex.cross.id, snap(l.cell), [1., 1., 1., 1.]); }
            }
            if mem.dead && tex.eliminated.id != TextureId::NONE { out.sprite(tex.eliminated.id, snap(l.cell), tint(DEAD_ICON_TINT, 1.)); }
            let bars: Vec<(Rect, f32, BarColors)> = if mem.dbno { vec![(l.dbno, mem.health, DBNO_BAR)] } else { vec![(l.shield, mem.shield, SHIELD_BAR), (l.health, mem.health, HEALTH_BAR)] };
            for (r, p, c) in bars {
                let (fill, rest) = bar_cmds(r, p, c);
                if let Some((fr, kind)) = fill { out.material(kind, fr, [1., 1., 1., 1.], false); }
                if let Some(rr) = rest { out.fill(rr, UNFILLED); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Cmd;
    use crate::screens::testutil::ctx;
    #[test] fn player_colours_are_the_cdo_brushes() {
        let s: Vec<[u8; 3]> = PLAYER_TINTS.iter().map(|c| { let t = srgb3(*c); [(t[0] * 255.).round() as u8, (t[1] * 255.).round() as u8, (t[2] * 255.).round() as u8] }).collect();
        assert_eq!(s, vec![[22, 125, 233], [255, 0, 66], [170, 0, 255], [254, 215, 2]]);
    }
    #[test] fn row_geometry_follows_the_widget_tree() {
        let m = metrics_at(1.);
        let l = row_layout(&m, 0);
        assert!((l.shield.w - 248.).abs() < 1e-3 && (l.shield.x - 46.).abs() < 1e-3);
        assert!((l.shield.h - 6.).abs() < 1e-4 && (l.health.h - 12.).abs() < 1e-4 && (l.health.y - l.shield.bottom() - 2.).abs() < 1e-4);
        assert!((l.dbno.y - l.shield.y).abs() < 1e-4 && (l.dbno.h - 20.).abs() < 1e-4);
        assert!((l.shield.y - l.cell.y - 2.).abs() < 1e-4, "20-tall bar stack centred in the 24-tall status line");
        assert!((l.marker.x - 23.).abs() < 1e-4 && (l.speaker.x - 4.).abs() < 1e-4 && (l.name_x - 44.).abs() < 1e-4);
        assert!((l.row.y - team_top(&m) - 8.).abs() < 1e-4, "every row, the first included, has 8 above it");
        let l1 = row_layout(&m, 1);
        assert!((l1.row.y - l.row.bottom() - 8.).abs() < 1e-3);
    }
    #[test] fn slate_line_uses_the_font_bbox() {
        let m = metrics_at(dpi_scale(2544., 1353.));
        assert_eq!((m.info_line, m.info_ascent), (24., 17.));
        assert_eq!((m.name_line, m.name_ascent), (34., 25.));
    }
    #[test] fn matches_the_real_client_at_2544x1353() {
        let m = metrics_at(dpi_scale(2544., 1353.));
        let l = row_layout(&m, 0);
        let (sh, he) = (snap(l.shield), snap(l.health));
        assert_eq!((sh.x, sh.y, sh.w), (58., 116., 310.));
        assert!((he.y - 126.).abs() <= 1. && (he.bottom() - 141.).abs() <= 1.);
        assert!((l.marker.center().1 - 98.3).abs() < 1.);
        assert!((l.name_baseline - 106.7).abs() < 1.5);
    }
    #[test] fn gradient_is_indexed_by_absolute_u() {
        let bar = Rect::new(0., 0., 200., 10.);
        let (fill, rest) = bar_cmds(bar, 0.5, HEALTH_BAR);
        let (r, k) = fill.unwrap();
        assert_eq!(r.w, 100.); assert_eq!(rest.unwrap(), Rect::new(100., 0., 100., 10.));
        match k { MaterialKind::HudBar { a, b } => { assert_eq!(a, HEALTH_BAR.a); let mid = lerp3(HEALTH_BAR.a, HEALTH_BAR.b, 0.5); for i in 0..3 { assert!((b[i] - mid[i]).abs() < 1e-6); } } _ => panic!() }
        let (f0, r0) = bar_cmds(bar, 0., SHIELD_BAR); assert!(f0.is_none() && r0.unwrap().w == 200.);
        let (f1, r1) = bar_cmds(bar, 1., SHIELD_BAR); assert!(r1.is_none()); assert!(matches!(f1.unwrap().1, MaterialKind::HudBar { b, .. } if b == SHIELD_BAR.b));
    }
    #[test] fn sanitize_clamps() {
        let t = TeamMember { health: f32::NAN, shield: 7., player_index: 9, ..TeamMember::new("a", 0, 0., 0.) }.sanitized();
        assert_eq!((t.health, t.shield, t.player_index), (0., 1., 3));
    }
    #[test] fn builds_rows_and_dbno_replaces_bars() {
        let Some(mut c) = ctx() else { return };
        let mut ti = TeamInfo::new();
        let mats = |l: &RenderList| l.cmds.iter().filter(|c| matches!(c, Cmd::Material(m) if matches!(m.kind, MaterialKind::HudBar { .. }))).count();
        let mut out = RenderList::new();
        ti.build(&mut c, 1920., 1080., "Player", true, &[TeamMember::new("Player", 0, 1., 0.5)], &mut out);
        assert_eq!(mats(&out), 2);
        let mut dn = TeamMember::new("Player", 1, 0.4, 0.5); dn.dbno = true;
        let mut out2 = RenderList::new();
        ti.build(&mut c, 1920., 1080., "Player", true, &[dn], &mut out2);
        assert_eq!(mats(&out2), 1);
        let mut empty = RenderList::new();
        ti.build(&mut c, 1920., 1080., "", true, &[], &mut empty);
        assert!(empty.is_empty());
    }
}
