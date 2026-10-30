//! GPU material parameter packing: turns a `fn_ui::draw::MaterialCmd` into (shader id, per-draw uniform, texture slots) for the WGSL pipelines.
//! Everything evaluated on the CPU here (constant buffers via `uimat::material_uniforms` / `ui_shaders::*_cb`, brush tint packing, the menu look tables)
//! is uploaded as plain `vec4`s; the shaders only do per-pixel work.
use crate::gen_tables::*;
use fn_ui::assets::UiAssets;
use fn_ui::draw::{MaterialCmd, MaterialKind, TextureId};
use fn_ui::material::types::{PanelKind, ProcStyle, Vis};
use fn_ui::material::{ui_shaders, uimat};

/// Which material evaluation backs `MaterialCmd`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialMode {
    /// `fn_ui::material::bake` once per (cache key, w, h), uploaded as a premultiplied texture and drawn as a quad.
    CpuBake,
    /// WGSL translations evaluated per pixel (falls back to `CpuBake` per material when no translation exists / it failed to compile).
    Gpu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShaderId {
    /// Settings-screen uimat pixel shader `ps_N`.
    Uimat(u32),
    /// HUD material: 0 item card, 1 compass, 2 hud bar, 3 panel line, 4 panel vgradient, 5 ([`RADIAL_PROGRESS`], hand-written `hud_radial.wgsl`).
    Hud(u32),
    /// Procedural outline / Primary-M / panels.
    Proc,
}

/// `ShaderId::Hud` index of the radial progress material (M_HealProgress); its WGSL is `shaders/hud_radial.wgsl` (not part of the generated `HUD_WGSL` table).
pub const RADIAL_PROGRESS: u32 = 5;
pub const MINIMAP_STORMS: u32 = 6;
pub const PHASE_ICON_BASE: u32 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexSrc {
    Dummy,
    /// Embedded Settings texture index (`UIMAT_TEX`).
    Embedded(u32),
    /// Texture of the store (straight RGBA8), with the version it was uploaded at.
    Store(u32),
}

pub struct MatDraw {
    pub shader: ShaderId,
    /// per-draw uniform in vec4 units (len >= 16, see mat_prelude.wgsl)
    pub p: Vec<[f32; 4]>,
    pub tex: [TexSrc; 4],
}

const GAMMA: [f32; 4] = [2.2 / 2.2, 1.0 / 2.2, 0.0, 0.0];

fn head(cmd: &MaterialCmd, boxed: bool, kind: f32, native: (f32, f32), size: (f32, f32), margin: [f32; 4], v1: [f32; 4]) -> Vec<[f32; 4]> {
    let mut p = vec![[0.0f32; 4]; 16];
    p[0] = [cmd.rect.x, cmd.rect.y, cmd.rect.w, cmd.rect.h];
    p[1] = cmd.tint;
    p[2] = v1;
    p[3] = [if cmd.flip_x { 1.0 } else { 0.0 }, if boxed { 1.0 } else { 0.0 }, kind, 0.0];
    p[4] = [native.0, native.1, size.0, size.1];
    p[5] = margin;
    p[10] = GAMMA;
    p
}

fn set_meta(p: &mut [[f32; 4]], slot: usize, wrap_x: bool, wrap_y: bool, srgb: bool) {
    p[12 + slot] = [wrap_x as u32 as f32, wrap_y as u32 as f32, srgb as u32 as f32, 0.0];
}

/// What textures a GPU material needs from the store (all must exist, else the caller falls back to CpuBake).
pub fn gpu_material(cmd: &MaterialCmd, assets: &UiAssets) -> Option<MatDraw> {
    let r = cmd.rect;
    match &cmd.kind {
        MaterialKind::Settings { mat, state, fill, scale } => {
            let scale = scale.max(1e-3);
            let brush = uimat::default_brush(*mat, *state)?;
            let size = (r.w / scale, r.h / scale);
            let params = uimat::MatParams { fill_percent: fill.clamp(0.0, 1.0), size_px: size, ..Default::default() };
            let cb = uimat::material_uniforms(*mat, *state, &params)?;
            let name = uimat::material_name(*mat, *state)?;
            let inst = UIMAT_INSTS.iter().find(|i| i.0 == name)?;
            let disabled = *state == uimat::MatState::Disabled;
            let ps = if disabled && inst.2 != u32::MAX { inst.2 } else { inst.1 };
            let boxed = brush.draw == uimat::Draw::Box && brush.margin.iter().any(|m| *m != 0.0);
            let t = brush.tint;
            let v1 = uimat::pack_tint([params.tint[0] * t[0], params.tint[1] * t[1], params.tint[2] * t[2], params.tint[3] * t[3]]);
            let mut p = head(cmd, boxed, 0.0, brush.native_size, size, brush.margin, v1);
            let mut tex = [TexSrc::Dummy; 4];
            for (slot, ti) in inst.3.iter().enumerate().take(4) {
                let m = UIMAT_TEX[*ti as usize];
                set_meta(&mut p, slot, m.3, m.4, m.2);
                tex[slot] = TexSrc::Embedded(*ti);
            }
            if cb.len() > 240 { return None; }
            p.extend_from_slice(&cb);
            Some(MatDraw { shader: ShaderId::Uimat(ps), p, tex })
        }
        MaterialKind::Procedural { style, vis, unit } => {
            let (mu, mv, image) = match style {
                ProcStyle::PrimaryM => (PM_MARGIN.0, PM_MARGIN.1, PM_IMAGE),
                s if s.left_shade() => (OUTLINE_SHADE_MARGIN.0, OUTLINE_SHADE_MARGIN.1, OUTLINE_IMAGE),
                _ => (OUTLINE_MARGIN.0, OUTLINE_MARGIN.1, OUTLINE_IMAGE),
            };
            let mut p = head(cmd, false, if *style == ProcStyle::PrimaryM { 1.0 } else { 0.0 }, (0.0, 0.0), (r.w, r.h), [mu, mv, 0.0, 0.0], [1.0; 4]);
            p[4] = [mu * image * unit, mv * image * unit, r.w, r.h];
            if *style == ProcStyle::PrimaryM {
                let (c0, c1, sh, a) = primary_m_look(*vis);
                p[6] = [c0[0], c0[1], c0[2], a];
                p[7] = [c1[0], c1[1], c1[2], 0.0];
                p[8] = [sh[0], sh[1], sh[2], 0.0];
            } else {
                let l = look(*style, *vis);
                p[6] = [l.primary[0], l.primary[1], l.primary[2], l.dark_base_alpha];
                p[7] = [l.base[0], l.base[1], l.base[2], l.gradient_alpha];
                p[8] = [l.gradient_min_alpha, l.outline_alpha, l.inner_fill_offset, l.switch_offset];
                p[9] = [l.box_size, 0.0, 0.0, 0.0];
            }
            Some(MatDraw { shader: ShaderId::Proc, p, tex: [TexSrc::Dummy; 4] })
        }
        MaterialKind::Panel { kind } => {
            let k = match kind { PanelKind::Backdrop => 2.0, _ => 3.0 };
            let mut p = head(cmd, false, k, (0.0, 0.0), (r.w, r.h), [0.0; 4], [1.0; 4]);
            match kind {
                PanelKind::ModalPanel => { p[6] = [MODAL_RGBA0[0], MODAL_RGBA0[1], MODAL_RGBA0[2], 0.0]; p[7] = [MODAL_RGBA1[0], MODAL_RGBA1[1], MODAL_RGBA1[2], 0.0]; }
                PanelKind::ModalHeader => { p[6] = [HEADER_RGBA0[0], HEADER_RGBA0[1], HEADER_RGBA0[2], 0.0]; p[7] = [HEADER_RGBA1[0], HEADER_RGBA1[1], HEADER_RGBA1[2], 0.0]; }
                PanelKind::Backdrop => {}
            }
            Some(MatDraw { shader: ShaderId::Proc, p, tex: [TexSrc::Dummy; 4] })
        }
        MaterialKind::ItemCard { colors, icon, hero_tri } => {
            let (icon, tri) = (store_tex(assets, *icon)?, store_tex(assets, *hero_tri)?);
            let c = *colors;
            let ov = move |name: &str, d: [f32; 4]| -> [f32; 4] {
                match name {
                    "bIsHero" => [0.; 4], "bIsItem" => [1.; 4], "ItemBackDensity" => [1.5; 4], "ItemBackRadius" => [0.6490830183029175; 4], "gStrokeThickness" => [0.475; 4],
                    "Rarity 1" => [c[0][0], c[0][1], c[0][2], 1.], "Rarity 2" => [c[1][0], c[1][1], c[1][2], 1.],
                    "Rarity 3" => [c[2][0], c[2][1], c[2][2], 1.], "Rarity 4" => [c[3][0], c[3][1], c[3][2], 1.],
                    _ => d,
                }
            };
            let cb = ui_shaders::item_card_cb(&ov);
            let mut p = head(cmd, false, 0.0, (0.0, 0.0), (r.w, r.h), [0.0; 4], [1.0; 4]);
            set_meta(&mut p, 0, true, true, true); // hero triangle: wrap
            set_meta(&mut p, 1, false, false, true); // icon: clamp
            p.extend_from_slice(&cb);
            Some(MatDraw { shader: ShaderId::Hud(0), p, tex: [tri, icon, TexSrc::Dummy, TexSrc::Dummy] })
        }
        MaterialKind::Compass { pan, strip, feather } => {
            let (strip, feather) = (store_tex(assets, *strip)?, store_tex(assets, *feather)?);
            let pan = *pan;
            let ov = move |name: &str, d: [f32; 4]| if name == "CompassPan" { [pan; 4] } else { d };
            let cb = ui_shaders::compass_cb(&ov);
            let mut p = head(cmd, false, 0.0, (0.0, 0.0), (r.w, r.h), [0.0; 4], [1.0; 4]);
            set_meta(&mut p, 0, true, true, true); // strip wraps
            set_meta(&mut p, 1, false, false, true); // feather clamps
            p.extend_from_slice(&cb);
            Some(MatDraw { shader: ShaderId::Hud(1), p, tex: [strip, feather, TexSrc::Dummy, TexSrc::Dummy] })
        }
        MaterialKind::HudBar { a, b } => {
            let (a, b) = (*a, *b);
            let ov = move |name: &str, d: [f32; 4]| match name { "ColorA" => [a[0], a[1], a[2], 1.], "ColorB" => [b[0], b[1], b[2], 1.], _ => d };
            let cb = ui_shaders::hud_bar_cb(&ov);
            let mut p = head(cmd, false, 0.0, (0.0, 0.0), (r.w, r.h), [0.0; 4], [1.0; 4]);
            p.extend_from_slice(&cb);
            Some(MatDraw { shader: ShaderId::Hud(2), p, tex: [TexSrc::Dummy; 4] })
        }
        MaterialKind::PanelLine { mask, global_alpha } => {
            let info = assets.store.info(*mask);
            let m = store_tex(assets, *mask)?;
            let ga = *global_alpha;
            let ov = move |name: &str, d: [f32; 4]| match name { "Global Alpha" => [ga; 4], "RGBA0" | "RGBA1" => [1.; 4], _ => d };
            let cb = ui_shaders::panel_line_cb(&ov);
            let mut p = head(cmd, true, 0.0, (info.w as f32, info.h as f32), (r.w, r.h), [0.49; 4], [1.0; 4]);
            set_meta(&mut p, 0, false, false, true);
            p.extend_from_slice(&cb);
            Some(MatDraw { shader: ShaderId::Hud(3), p, tex: [m, TexSrc::Dummy, TexSrc::Dummy, TexSrc::Dummy] })
        }
        MaterialKind::PanelVGrad | MaterialKind::PanelVGradHighlighted => {
            let q = if matches!(cmd.kind, MaterialKind::PanelVGradHighlighted) { fn_ui::material::hud::VGRAD_HIGHLIGHTED } else { fn_ui::material::hud::VGRAD_RESOURCE_BG };
            let ov = move |name: &str, d: [f32; 4]| match name {
                "Global Alpha" => [q.global_alpha; 4], "Rotation" => [q.rotation; 4], "Scale" => [q.scale; 4], "RGBA0" => q.rgba0, "RGBA1" => q.rgba1, _ => d };
            let cb = ui_shaders::panel_vgrad_cb(&ov);
            let mut p = head(cmd, false, 0.0, (0.0, 0.0), (r.w, r.h), [0.0; 4], [1.0; 4]);
            p.extend_from_slice(&cb);
            Some(MatDraw { shader: ShaderId::Hud(4), p, tex: [TexSrc::Dummy; 4] })
        }
        MaterialKind::RadialProgress { percentage, texture } => {
            let t = store_tex(assets, *texture)?;
            let mut p = head(cmd, false, 0.0, (0.0, 0.0), (r.w, r.h), [0.0; 4], [1.0; 4]);
            p[6] = [(percentage.clamp(0., 1.) * 1000.).round() / 1000., 0.0, 0.0, 0.0]; // quantised like MaterialKind::cache_key / the CPU bake
            set_meta(&mut p, 0, false, false, true); // Texture: clamp, sRGB
            Some(MatDraw { shader: ShaderId::Hud(RADIAL_PROGRESS), p, tex: [t, TexSrc::Dummy, TexSrc::Dummy, TexSrc::Dummy] })
        }
        MaterialKind::MinimapStorms { map, current } => {
            let t = store_tex(assets, *map)?;
            let c = *current;
            let ov = move |name: &str, d: [f32; 4]| if name == "CurrentCircle" { [c[0], c[1], c[2], 1.] } else { d };
            let cb = fn_ui::material::storm_ui::minimap_storms_cb(&ov);
            let mut p = head(cmd, false, 0.0, (0.0, 0.0), (r.w, r.h), [0.0; 4], [1.0; 4]);
            set_meta(&mut p, 0, false, false, true);
            p.extend_from_slice(&cb);
            Some(MatDraw { shader: ShaderId::Hud(MINIMAP_STORMS), p, tex: [t, TexSrc::Dummy, TexSrc::Dummy, TexSrc::Dummy] })
        }
        MaterialKind::GamePhaseIcon { shader, bg, icon } => {
            let (b, i) = (store_tex(assets, *bg)?, store_tex(assets, *icon)?);
            let mut p = head(cmd, false, 0.0, (0.0, 0.0), (r.w, r.h), [0.0; 4], [1.0; 4]);
            set_meta(&mut p, 0, false, false, true);
            set_meta(&mut p, 1, false, false, true);
            p.extend_from_slice(&[[0.0, 0.0, 0.0, 1.0], [0.0; 4], [0.0; 4]]);
            Some(MatDraw { shader: ShaderId::Hud(PHASE_ICON_BASE + (*shader as u32).min(4)), p, tex: [b, i, TexSrc::Dummy, TexSrc::Dummy] })
        }
    }
}

fn store_tex(assets: &UiAssets, id: TextureId) -> Option<TexSrc> {
    if id == TextureId::NONE { return None; }
    assets.store.get(id)?;
    Some(TexSrc::Store(id.0))
}

// ---- menu procedural look tables (copied from the OLD src/menu/outline.rs, which fn-ui::menu::outline ports) -------------------------------
pub const OUTLINE_BASE_COLOR: [f32; 3] = [0., 0.002125, 0.017642];
pub const OUTLINE_BLUE: [f32; 3] = [0.318547, 0.603828, 1.];
pub const OUTLINE_BLUE_PRESSED: [f32; 3] = [0.445201, 0.693872, 1.];
pub const OUTLINE_YELLOW_HOVER: [f32; 3] = [1., 1., 0.072272];
pub const OUTLINE_RED: [f32; 3] = [1., 0.155926, 0.122139];
pub const OUTLINE_BOX_SIZE: f32 = 0.433;
pub const OUTLINE_IMAGE: f32 = 48.;
pub const OUTLINE_MARGIN: (f32, f32) = (0.1, 0.1);
pub const OUTLINE_SHADE_MARGIN: (f32, f32) = (0.25, 0.1);
pub const PM_IMAGE: f32 = 48.;
pub const PM_MARGIN: (f32, f32) = (0.25, 0.5);
pub const MODAL_RGBA0: [f32; 3] = [0.965, 0.965, 0.965];
pub const MODAL_RGBA1: [f32; 3] = [0.5616, 0.633331, 0.72];
pub const HEADER_RGBA0: [f32; 3] = [0.0045, 0.006909, 0.025];
pub const HEADER_RGBA1: [f32; 3] = [0.017642, 0.022174, 0.049707];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatLook {
    pub primary: [f32; 3], pub base: [f32; 3], pub dark_base_alpha: f32,
    pub gradient_alpha: f32, pub gradient_min_alpha: f32, pub outline_alpha: f32,
    pub inner_fill_offset: f32, pub switch_offset: f32, pub box_size: f32,
}

pub fn look(style: ProcStyle, vis: Vis) -> MatLook {
    let (shade, red) = (style.left_shade(), style.red());
    let (primary, outline_alpha, ga, gm) = match vis {
        Vis::Normal => (if red { OUTLINE_RED } else { OUTLINE_BLUE }, 0.8, 0.4, 0.75),
        Vis::Hover => (OUTLINE_YELLOW_HOVER, 1., 0.4, 0.75),
        Vis::Pressed => (if red { OUTLINE_RED } else { OUTLINE_BLUE_PRESSED }, 1., 0.8, 1.),
        Vis::Disabled => (OUTLINE_BLUE, 0.4, 0.2, 0.75),
    };
    MatLook { primary, base: OUTLINE_BASE_COLOR, dark_base_alpha: 0.6, gradient_alpha: ga, gradient_min_alpha: gm, outline_alpha,
        inner_fill_offset: if shade { 0.5 } else { 0. }, switch_offset: if shade { 1. } else { 0. }, box_size: OUTLINE_BOX_SIZE }
}

/// (RGBA0, RGBA1, drop shadow, alpha) per state of M-Button-Primary-M-*.
pub fn primary_m_look(vis: Vis) -> ([f32; 3], [f32; 3], [f32; 3], f32) {
    match vis {
        Vis::Normal => ([0.723055, 0.799103, 0.913099], [0.417885, 0.571125, 0.799103], [0.097587, 0.162029, 0.332452], 1.),
        Vis::Hover => ([1., 0.955973, 0.445201], [1., 0.879622, 0.076185], [0.323143, 0.238398, 0.012983], 1.),
        Vis::Pressed => ([1., 1., 1.], [0.973445, 1., 0.651406], [1., 1., 0.], 1.),
        Vis::Disabled => ([0.254152, 0.423268, 0.715694], [0.093059, 0.158961, 0.332452], [0., 0., 0.], 0.6),
    }
}
