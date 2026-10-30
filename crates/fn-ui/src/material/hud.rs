//! CPU bake of `MaterialKind::{ItemCard, Compass, HudBar, PanelLine, PanelVGrad}` (owned by the screens port). Returns `None` for kinds not handled here.
//!
//! Port of the old runtime's `ui_material.rs` (minus the GL upload). The generated functions in [`crate::material::ui_shaders`] ARE the cooked pixel
//! shaders. This module supplies their inputs the way Slate's vertex stage does and bakes the result into PREMULTIPLIED RGBA8:
//!  * `cb0[2]` = Slate GammaAndAlphaValues (2.2/DisplayGamma = 1, 1/DisplayGamma = 0.4545 != 1 -> the shader applies the exact sRGB encode itself),
//!  * `v1` (COLOR) = widget tint in LINEAR space (the Slate vertex shader decodes the packed sRGB tint, verified in the disassembled VS),
//!  * `v6` = (u, v, 1, 1) brush UV, `v8` = (u, v) material UV, both 0..1 over the element; the clip-quad test is dropped (the element is axis aligned),
//!  * `cb1[131].z` = View time (fixed at [`TIME`]), textures are sampled bilinearly with sRGB decode (UE default `SRGB=true` textures).
//! ASSUMED (not in the data): the exact Slate vertex values for UV (u,v,1,1) on 9-slice elements, DisplayGamma 2.2 (UE default).
use crate::assets::{TextureStore, UiAssets};
use crate::draw::{MaterialKind, TextureId};
use crate::material::ui_shaders as sh;
use std::sync::OnceLock;

pub const GAMMA_AND_ALPHA: [f32; 4] = [1.0, 1.0 / 2.2, 0.0, 0.0];
/// View.GameTime fed to cb1[131].z (the item card has slow animated shimmer; frozen for a deterministic HUD).
pub const TIME: f32 = 0.0;

/// Decoded float image (components 0..1) that the shader texture callbacks sample.
#[derive(Clone)]
pub struct Img { pub w: usize, pub h: usize, pub px: Vec<[f32; 4]> }
fn srgb_to_lin(c: f32) -> f32 { if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } }
fn srgb_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| { let mut t = [0f32; 256]; for (i, v) in t.iter_mut().enumerate() { *v = srgb_to_lin(i as f32 / 255.); } t })
}
impl Img {
    /// From straight RGBA8; `srgb` decodes RGB (alpha stays linear) like a UE texture with SRGB=true.
    pub fn from_rgba8(w: usize, h: usize, rgba: &[u8], srgb: bool) -> Img {
        let lut = srgb_lut();
        let px = rgba.chunks_exact(4).take(w * h).map(|p| {
            let f = |c: u8| if srgb { lut[c as usize] } else { c as f32 / 255. };
            [f(p[0]), f(p[1]), f(p[2]), p[3] as f32 / 255.]
        }).collect();
        Img { w, h, px }
    }
    /// Pixels of a store texture (straight RGBA8). `None` for missing ids.
    pub fn from_texture(store: &TextureStore, id: TextureId, srgb: bool) -> Option<Img> {
        let t = store.get(id)?;
        t.mark_cpu();
        if !t.has_pixels() { return None; }
        Some(Img::from_rgba8(t.w as usize, t.h as usize, &t.rgba, srgb))
    }
    pub fn solid(c: [f32; 4]) -> Img { Img { w: 1, h: 1, px: vec![c] } }
    /// Bilinear sample at texel-centre convention; `wrap` repeats, otherwise clamps (UE sampler address mode).
    pub fn sample(&self, u: f32, v: f32, wrap: bool) -> [f32; 4] {
        let (x, y) = (u * self.w as f32 - 0.5, v * self.h as f32 - 0.5);
        let (x0, y0) = (x.floor(), y.floor()); let (fx, fy) = (x - x0, y - y0);
        let at = |xi: i64, yi: i64| -> [f32; 4] {
            let (xi, yi) = if wrap { (xi.rem_euclid(self.w as i64), yi.rem_euclid(self.h as i64)) } else { (xi.clamp(0, self.w as i64 - 1), yi.clamp(0, self.h as i64 - 1)) };
            self.px[yi as usize * self.w + xi as usize]
        };
        let (a, b, c, d) = (at(x0 as i64, y0 as i64), at(x0 as i64 + 1, y0 as i64), at(x0 as i64, y0 as i64 + 1), at(x0 as i64 + 1, y0 as i64 + 1));
        let mut o = [0.; 4];
        for k in 0..4 { o[k] = (a[k] * (1. - fx) + b[k] * fx) * (1. - fy) + (c[k] * (1. - fx) + d[k] * fx) * fy; }
        o
    }
}

/// Cooked 4.16 FRichCurve cubic ease (RCIM_Cubic with zero tangents): 3s^2 - 2s^3.
pub fn cubic_ease(s: f32) -> f32 { let s = s.clamp(0., 1.); s * s * (3. - 2. * s) }

/// Straight float RGBA (already shader-encoded) -> PREMULTIPLIED RGBA8, rounding exactly like the old `upload_rgba`.
pub fn to_premultiplied_u8(rgba: &[[f32; 4]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(rgba.len() * 4);
    for p in rgba { let a = p[3].clamp(0., 1.); for k in 0..3 { bytes.push(((p[k].clamp(0., 1.) * a) * 255. + 0.5) as u8); } bytes.push((a * 255. + 0.5) as u8); }
    bytes
}

/// Slate constant buffers shared by all material pixel shaders.
fn cb0() -> Vec<[f32; 4]> { vec![[0.; 4], [0.; 4], GAMMA_AND_ALPHA] }
fn cb1_view() -> Vec<[f32; 4]> { let mut v = vec![[0.; 4]; 132]; v[131] = [0., 0., TIME, 0.]; v }

/// Run a generated shader over a `w`x`h` element. `px(u, v)` gets the element UV of the pixel centre; returns straight RGBA.
fn bake_px(w: usize, h: usize, mut px: impl FnMut(f32, f32) -> [f32; 4]) -> Vec<[f32; 4]> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h { for x in 0..w { out.push(px((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32)); } }
    out
}

/// Item card (FortMultiSizeItemCard size S material instance M-Item-Inst-S: bIsItem=1, ItemBackDensity 1.5, ItemBackRadius 0.64908, stroke 0.475).
/// `colors` = RarityData Color1..Color4 (linear) feeding the material parameters "Rarity 1".."Rarity 4"; `icon` = the item icon bound to "Icon".
pub fn item_card(w: usize, h: usize, colors: [[f32; 3]; 4], icon: &Img, hero_tri: &Img) -> Vec<[f32; 4]> {
    let ov = move |name: &str, d: [f32; 4]| -> [f32; 4] {
        match name {
            "bIsHero" => [0.; 4], "bIsItem" => [1.; 4], "ItemBackDensity" => [1.5; 4], "ItemBackRadius" => [0.6490830183029175; 4], "gStrokeThickness" => [0.475; 4],
            "Rarity 1" => [colors[0][0], colors[0][1], colors[0][2], 1.], "Rarity 2" => [colors[1][0], colors[1][1], colors[1][2], 1.],
            "Rarity 3" => [colors[2][0], colors[2][1], colors[2][2], 1.], "Rarity 4" => [colors[3][0], colors[3][1], colors[3][2], 1.],
            _ => d,
        }
    };
    let cb2 = sh::item_card_cb(&ov);
    let (c0, c1) = (cb0(), cb1_view());
    let t0 = |u: f32, v: f32| hero_tri.sample(u, v, true);
    let t1 = |u: f32, v: f32| icon.sample(u, v, false);
    let tint = [1.0f32; 4];
    bake_px(w, h, |u, v| {
        let mut vin = [[0.0f32; 4]; 9];
        vin[1] = tint; vin[6] = [u, v, 1., 1.]; vin[8] = [u, v, 0., 0.];
        sh::item_card_ps(&vin, [&c0, &c1, &cb2], [&t0, &t1]).unwrap_or([0.; 4])
    })
}

/// M_Compass strip: `pan` = CompassPan (yaw/360 + 0.25, from AthenaCompass.UpdateCompassMaterial), `strip` = T_Compass (wrap), `feather` = T_CompassFeather (clamp).
pub fn compass(w: usize, h: usize, pan: f32, strip: &Img, feather: &Img) -> Vec<[f32; 4]> {
    let ov = move |name: &str, d: [f32; 4]| if name == "CompassPan" { [pan; 4] } else { d };
    let cb = sh::compass_cb(&ov);
    let c0 = cb0();
    let t0 = |u: f32, v: f32| strip.sample(u, v, true);
    let t1 = |u: f32, v: f32| feather.sample(u, v, false);
    bake_px(w, h, |u, v| {
        let mut vin = [[0.0f32; 4]; 7]; vin[1] = [1.; 4]; vin[6] = [u, v, 1., 1.];
        sh::compass_ps(&vin, [&c0, &cb], [&t0, &t1]).unwrap_or([0.; 4])
    })
}

/// HUD-Bar_Material: colour = lerp(ColorA, ColorB, u) over the filled quad, then sRGB encode (shader).
pub fn hud_bar(w: usize, h: usize, a: [f32; 3], b: [f32; 3]) -> Vec<[f32; 4]> {
    let ov = move |name: &str, d: [f32; 4]| match name { "ColorA" => [a[0], a[1], a[2], 1.], "ColorB" => [b[0], b[1], b[2], 1.], _ => d };
    let cb = sh::hud_bar_cb(&ov);
    let c0 = cb0();
    bake_px(w, h, |u, v| {
        let mut vin = [[0.0f32; 4]; 7]; vin[1] = [1.; 4]; vin[6] = [u, v, 1., 1.];
        sh::hud_bar_ps(&vin, [&c0, &cb], []).unwrap_or([0.; 4])
    })
}

/// M_Border_HUD_Line_Vert (instance of M_UI_GradientPanel: Global Alpha 0.2, RGBA0 = RGBA1 = white, mask T-Border-HUD-Line-Vert, Box margin 0.49).
pub fn panel_line(w: usize, h: usize, mask: &Img, global_alpha: f32) -> Vec<[f32; 4]> {
    let ov = move |name: &str, d: [f32; 4]| match name { "Global Alpha" => [global_alpha; 4], "RGBA0" | "RGBA1" => [1.; 4], _ => d };
    let cb = sh::panel_line_cb(&ov);
    let c0 = cb0();
    let t0 = |u: f32, v: f32| mask.sample(u, v, false);
    bake_px(w, h, |u, v| {
        // 9-slice brush UV (margin 0.49 of the 4x8 texture): borders keep texture-pixel size, the thin centre stretches.
        let bu = box_uv(u, w as f32, mask.w as f32, 0.49); let bv = box_uv(v, h as f32, mask.h as f32, 0.49);
        let mut vin = [[0.0f32; 4]; 9]; vin[1] = [1.; 4]; vin[6] = [bu, bv, 1., 1.]; vin[8] = [u, v, 0., 0.];
        sh::panel_line_ps(&vin, [&c0, &cb], [&t0]).unwrap_or([0.; 4])
    })
}
/// Box-brush UV along one axis: margin `m` (fraction of the texture) on each side drawn at texture-pixel size, middle stretched.
pub fn box_uv(t: f32, dest_px: f32, tex_px: f32, m: f32) -> f32 {
    let p = t * dest_px; let mp = m * tex_px;
    if p < mp { p / tex_px } else if p > dest_px - mp { 1. - (dest_px - p) / tex_px } else { m + (p - mp) / (dest_px - 2. * mp).max(1e-3) * (1. - 2. * m) }
}

/// M_Border_HUD_VGradient (Border_HUD_ResourceBg): Global Alpha 0.15, Rotation 0, Scale 0.75, RGBA0 = (0,0.00121,0.01033,0), RGBA1 = same with alpha 1.
pub fn panel_vgrad(w: usize, h: usize) -> Vec<[f32; 4]> { panel_vgrad_with(w, h, VGRAD_RESOURCE_BG) }

/// `Border_HUD_ResourceBg_Highlighted` = `M_Border_LightNavy_VGrad`: an instance of the same compiled `M_Border_HUD_VGradient` pixel shader (verified byte-identical
/// by the old tools) with its own parameters: Global Alpha 0.7, Rotation 0, Scale 1 (parent default), RGBA0 (0.122139, 0.14996, 0.266356, 1), RGBA1 (0.08022, 0.102242,
/// 0.201556, 1) (all four read from the cooked instance with fn-inspect). `PackResource.UpdateCount` picks it for the current resource's pack.
pub const VGRAD_HIGHLIGHTED: VGradParams = VGradParams { global_alpha: 0.7, rotation: 0., scale: 1., rgba0: [0.122139, 0.14996, 0.266356, 1.], rgba1: [0.08022, 0.102242, 0.201556, 1.] };
/// `Border_HUD_ResourceBg` = `M_Border_HUD_VGradient`.
pub const VGRAD_RESOURCE_BG: VGradParams = VGradParams { global_alpha: 0.15, rotation: 0., scale: 0.75, rgba0: [0., 0.00121, 0.01033, 0.], rgba1: [0., 0.00121, 0.01033, 1.] };
/// The parameters of the translated `M_Border_HUD_VGradient` shader (what its material instances override).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VGradParams { pub global_alpha: f32, pub rotation: f32, pub scale: f32, pub rgba0: [f32; 4], pub rgba1: [f32; 4] }
pub fn panel_vgrad_with(w: usize, h: usize, p: VGradParams) -> Vec<[f32; 4]> {
    let ov = move |name: &str, d: [f32; 4]| match name {
        "Global Alpha" => [p.global_alpha; 4], "Rotation" => [p.rotation; 4], "Scale" => [p.scale; 4], "RGBA0" => p.rgba0, "RGBA1" => p.rgba1, _ => d };
    let cb = sh::panel_vgrad_cb(&ov);
    let c0 = cb0();
    bake_px(w, h, |u, v| {
        let mut vin = [[0.0f32; 4]; 9]; vin[1] = [1.; 4]; vin[8] = [u, v, 0., 0.];
        sh::panel_vgrad_ps(&vin, [&c0, &cb], []).unwrap_or([0.; 4])
    })
}
pub fn panel_vgrad_highlighted(w: usize, h: usize) -> Vec<[f32; 4]> { panel_vgrad_with(w, h, VGRAD_HIGHLIGHTED) }

/// The angle of `M_HealProgress` / `CircularProgressBar` (`VectorToRadialValue`): the cooked pixel shader's HLSL `atan2` expansion, instruction by instruction
/// (`reports/hud-shaders/M_HealProgress/shader_5125.asm`, the Slate variant). `dx, dy` = UV - 0.5 (v down). Result in (-pi, pi].
pub fn radial_angle(dx: f32, dy: f32) -> f32 {
    let (ax, ay) = (dx.abs(), dy.abs());                                  // mov r0.zw, |r0.xxxy|
    if ay.max(ax) <= 0. { return 0.; }                                    // (the exact centre divides 0/0 in the shader: guarded)
    let inv = 1. / ay.max(ax); let mn = ay.min(ax); let flag = ay < ax;   // max/div/min/lt
    let t = inv * mn; let t2 = t * t;
    let mut p = t2 * 0.020835 - 0.085133; p = t2 * p + 0.180141; p = t2 * p - 0.330299; p = t2 * p + 0.999866; // minimax atan polynomial
    let a = t * p;
    let mut z = a + if flag { a * -2. + 1.570796 } else { 0. };           // and r0.z, flag, r1.y ; mad r0.z, r0.w, r1.x, r0.z
    if -dy < dy { z += f32::from_bits(0xc0490fdb); }                      // lt r0.w, -y, y ; and r0.w, r0.w, -pi
    let (nx, ny) = (-dx, -dy);
    let neg = nx.max(ny) >= -nx.max(ny) && nx.min(ny) < -nx.min(ny);      // ge r0.x, max, -max ; lt r0.y, min, -min ; and
    if neg { -z } else { z }                                              // movc r0.x, cond, -r0.z, r0.z
}
/// Mask of the radial wipe: `floor(frc(angle / 2pi) + Percentage)` (0 or 1 for Percentage in 0..=1).
pub fn radial_mask(u: f32, v: f32, percentage: f32) -> f32 {
    let x = radial_angle(u - 0.5, v - 0.5) * 0.159155; // mul r0.x, r0.x, l(0.159155)
    (x - x.floor() + percentage).floor()                // frc ; add cb1[2].x (Percentage) ; round_ni
}
fn srgb_encode_shader(c: f32) -> f32 { let c = c.max(0.); if c >= 0.003131 { 1.055 * c.powf(0.416667) - 0.055 } else { c * 12.92 } }
/// `M_HealProgress` (instance of `CircularProgressBar`: Percentage animated by `AthenaEquipProgress`, `Texture` = `Results_TimerFill`, SelectionColor 0): per pixel
/// `rgb = tex.rgb * mask + SelectionColor.rgb`, `a = sat(mask * tex.a)`, then the exact sRGB encode (GammaAndAlphaValues.y != 1). The Slate vertex tint is NOT applied by
/// this shader variant (`v1` is unused in `shader_5125`).
pub fn radial_progress(w: usize, h: usize, percentage: f32, tex: &Img) -> Vec<[f32; 4]> {
    bake_px(w, h, |u, v| {
        let m = radial_mask(u, v, percentage);
        let t = tex.sample(u, v, false);
        let a = (m * t[3]).clamp(0., 1.);
        [srgb_encode_shader(t[0] * m), srgb_encode_shader(t[1] * m), srgb_encode_shader(t[2] * m), a]
    })
}

/// Premultiplied RGBA8 (`w*h*4`, row 0 = top) of the HUD material kinds, or `None` for kinds owned elsewhere.
/// Missing optional textures (compass strip/feather, line mask) bake to fully transparent, as the old runtime skipped those elements.
pub fn bake(kind: &MaterialKind, w: u32, h: u32, assets: &mut UiAssets) -> Option<Vec<u8>> {
    let (wu, hu) = (w as usize, h as usize);
    let empty = || vec![0u8; wu * hu * 4];
    match kind {
        MaterialKind::ItemCard { colors, icon, hero_tri } => {
            if wu == 0 || hu == 0 { return Some(empty()); }
            // Test slots without an exported icon: the material still draws the rarity card (blank icon, old behaviour).
            let blank = Img::solid([0.; 4]);
            let icon = Img::from_texture(&assets.store, *icon, true).unwrap_or_else(|| blank.clone());
            let tri = Img::from_texture(&assets.store, *hero_tri, true).unwrap_or(blank);
            Some(to_premultiplied_u8(&item_card(wu, hu, *colors, &icon, &tri)))
        }
        MaterialKind::Compass { pan, strip, feather } => {
            if wu == 0 || hu == 0 { return Some(empty()); }
            let (Some(s), Some(f)) = (Img::from_texture(&assets.store, *strip, true), Img::from_texture(&assets.store, *feather, true)) else { return Some(empty()) };
            Some(to_premultiplied_u8(&compass(wu, hu, *pan, &s, &f)))
        }
        MaterialKind::HudBar { a, b } => { if wu == 0 || hu == 0 { return Some(empty()); } Some(to_premultiplied_u8(&hud_bar(wu, hu, *a, *b))) }
        MaterialKind::PanelLine { mask, global_alpha } => {
            if wu == 0 || hu == 0 { return Some(empty()); }
            let Some(m) = Img::from_texture(&assets.store, *mask, true) else { return Some(empty()) };
            Some(to_premultiplied_u8(&panel_line(wu, hu, &m, *global_alpha)))
        }
        MaterialKind::PanelVGrad => { if wu == 0 || hu == 0 { return Some(empty()); } Some(to_premultiplied_u8(&panel_vgrad(wu, hu))) }
        MaterialKind::PanelVGradHighlighted => { if wu == 0 || hu == 0 { return Some(empty()); } Some(to_premultiplied_u8(&panel_vgrad_highlighted(wu, hu))) }
        MaterialKind::RadialProgress { percentage, texture } => {
            if wu == 0 || hu == 0 { return Some(empty()); }
            let Some(t) = Img::from_texture(&assets.store, *texture, true) else { return Some(empty()) };
            Some(to_premultiplied_u8(&radial_progress(wu, hu, (percentage.clamp(0., 1.) * 1000.).round() / 1000., &t))) // quantised like `MaterialKind::cache_key`
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::DirAssets;
    #[test] fn cubic_ease_is_smoothstep() { assert_eq!(cubic_ease(0.), 0.); assert_eq!(cubic_ease(1.), 1.); assert!((cubic_ease(0.5) - 0.5).abs() < 1e-6); assert!(cubic_ease(0.25) < 0.25); }
    #[test] fn hud_bar_gradient_runs_a_to_b_in_srgb() {
        let px = hud_bar(64, 4, [0.07619, 0.50289, 0.0356], [0.17789, 0.73046, 0.08022]);
        let enc = |c: f32| if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 };
        let (l, r) = (px[0], px[63]);
        assert!((l[1] - enc(0.50289 + (0.73046 - 0.50289) * (0.5 / 64.))).abs() < 2e-3, "{l:?}");
        assert!((r[1] - enc(0.73046 - (0.73046 - 0.50289) * (0.5 / 64.))).abs() < 2e-3, "{r:?}");
        assert_eq!(l[3], 1.);
    }
    #[test] fn box_uv_keeps_border_pixels() { assert!((box_uv(0.0, 100., 8., 0.25) - 0.0).abs() < 1e-6); assert!((box_uv(1.0, 100., 8., 0.25) - 1.0).abs() < 1e-6); assert!((box_uv(0.5, 100., 8., 0.25) - 0.5).abs() < 1e-4); }
    #[test] fn premultiply_rounds_like_old_upload() {
        assert_eq!(to_premultiplied_u8(&[[1., 0.5, 0.25, 0.5]]), vec![128, 64, 32, 128]);
        assert_eq!(to_premultiplied_u8(&[[2., -1., 0.5, 7.]]), vec![255, 0, 128, 255]);
    }
    #[test] fn bake_ignores_foreign_kinds_and_handles_zero_size() {
        let mut a = UiAssets::new(Box::new(crate::assets::MemAssets::default()));
        assert!(bake(&MaterialKind::PanelVGrad, 0, 5, &mut a).unwrap().is_empty());
        assert!(bake(&MaterialKind::Panel { kind: crate::material::types::PanelKind::Backdrop }, 4, 4, &mut a).is_none());
        let b = bake(&MaterialKind::HudBar { a: [0.1; 3], b: [0.9; 3] }, 8, 2, &mut a).unwrap();
        assert_eq!(b.len(), 64); assert!(b.chunks_exact(4).all(|p| p[3] == 255));
        // Missing compass textures bake transparent (old runtime drew nothing).
        let c = bake(&MaterialKind::Compass { pan: 0.25, strip: TextureId::NONE, feather: TextureId::NONE }, 4, 4, &mut a).unwrap();
        assert!(c.iter().all(|v| *v == 0));
    }
    #[test] fn panel_vgrad_and_line_bake_without_assets_are_finite_and_translucent() {
        let px = panel_vgrad(16, 32);
        assert!(px.iter().all(|p| p.iter().all(|c| c.is_finite())));
        let mask = Img::solid([1., 1., 1., 1.]);
        let l = panel_line(4, 16, &mask, 0.2);
        assert!(l.iter().all(|p| p.iter().all(|c| c.is_finite())));
    }
    fn assets() -> Option<UiAssets> {
        let root = DirAssets::default_root();
        if !root.is_dir() { eprintln!("old asset tree {} not found, skipping", root.display()); return None; }
        Some(UiAssets::from_dir(root))
    }
    /// Renders the translated item-card shader for the five rarities and checks it is a lit, non-flat, opaque square (old `item_card_shader_renders_rarity_cards`).
    #[test] fn item_card_shader_renders_rarity_cards() {
        let Some(mut a) = assets() else { return };
        let icon = a.texture("T-Icon-Weapons-SK-Pick.png"); let tri = a.texture("T-Card-Hero-Anim-Triangle.png");
        assert!(icon.id != TextureId::NONE && tri.id != TextureId::NONE, "pickaxe icon + hero triangle must exist");
        let icon_img = Img::from_texture(&a.store, icon.id, true).unwrap(); let tri_img = Img::from_texture(&a.store, tri.id, true).unwrap();
        let rar = crate::screens::quickbar::RARITY;
        let mut first = None;
        for r in 0..5 {
            let px = item_card(76, 76, [rar[r][0], rar[r][1], rar[r][2], rar[r][3]], &icon_img, &tri_img);
            assert!(px.iter().all(|p| p.iter().all(|c| c.is_finite())));
            if r == 0 { first = Some(px); }
        }
        let px = first.unwrap();
        let (mn, mx) = px.iter().fold((1f32, 0f32), |a, p| (a.0.min(p[1]), a.1.max(p[1])));
        assert!(mx - mn > 0.05, "card must not be flat: {mn} {mx}");
        // The same through the public bake entry point.
        let k = MaterialKind::ItemCard { colors: [rar[2][0], rar[2][1], rar[2][2], rar[2][3]], icon: icon.id, hero_tri: tri.id };
        let b = bake(&k, 76, 76, &mut a).unwrap();
        assert_eq!(b.len(), 76 * 76 * 4); assert!(b.chunks_exact(4).filter(|p| p[3] > 0).count() > 76 * 76 / 2);
    }
    #[test] fn compass_shader_samples_the_strip_and_feathers_the_edges() {
        let Some(mut a) = assets() else { return };
        let (s, f) = (a.texture("T_Compass.png"), a.texture("T_CompassFeather.png"));
        assert_eq!((s.w, s.h, f.w, f.h), (1536, 128, 768, 128));
        let strip = Img::from_texture(&a.store, s.id, true).unwrap(); let feather = Img::from_texture(&a.store, f.id, true).unwrap();
        let pan = |yaw: f32| yaw / 360. + 0.25;
        let px = compass(768, 128, pan(0.), &strip, &feather);
        let edge = px[64 * 768 + 1][3]; let mid = px[64 * 768 + 384][3]; let rgb_mid = px.iter().skip(64 * 768 + 300).take(168).map(|p| p[0] + p[1] + p[2]).fold(0f32, f32::max);
        assert!(edge < 0.2, "feather mask fades the strip at the left edge: {edge}");
        assert!(mid >= 0. && px.iter().all(|p| p.iter().all(|c| c.is_finite())), "centre alpha {mid}");
        assert!(rgb_mid > 0.5, "strip ticks/letters are drawn near the centre");
        let px2 = compass(768, 128, pan(360.), &strip, &feather);
        let d: f32 = px.iter().zip(px2.iter()).map(|(a, b)| (a[0] - b[0]).abs() + (a[3] - b[3]).abs()).sum::<f32>() / px.len() as f32;
        assert!(d < 1e-3, "{d}");
    }
}
