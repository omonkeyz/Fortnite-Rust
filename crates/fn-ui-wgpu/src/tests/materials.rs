//! GPU (WGSL) vs CPU reference for every translated material. The CPU references are:
//!  * Settings: `fn_ui::material::uimat::raster_brush` (the cooked pixel shaders executed in Rust, straight RGBA8) premultiplied like `Texture::premultiplied`;
//!  * HUD materials: `fn_ui::material::ui_shaders::*_ps` driven exactly like the OLD `ui_material.rs` (inputs per its module doc);
//!  * Procedural/Panel: a Rust port of the OLD `menu/outline.rs` (`outline_pixel`, `primary_pixel`, `panel_pixel`, `expand`).
//! Per-material max / mean absolute error (in 1/255 units over all four premultiplied channels) is printed; run with `-- --nocapture` to see the table.
use super::*;
use crate::materials::{look, primary_m_look, HEADER_RGBA0, HEADER_RGBA1, MODAL_RGBA0, MODAL_RGBA1, OUTLINE_IMAGE, OUTLINE_MARGIN, OUTLINE_SHADE_MARGIN, PM_IMAGE, PM_MARGIN};
use crate::MaterialMode;
use fn_ui::assets::Sampler;
use fn_ui::draw::{MaterialKind, RenderList};
use fn_ui::layout::Rect;
use fn_ui::material::types::{PanelKind, ProcStyle, Vis};
use fn_ui::material::{ui_shaders, uimat};
use std::collections::BTreeMap;

/// Error bounds asserted for every translated material (max over all channels in 1/255, and the mean).
const MAX_BOUND: u32 = 2;
const MEAN_BOUND: f64 = 0.6;

pub fn premult(straight: &[u8]) -> Vec<u8> {
    let mut o = straight.to_vec();
    for p in o.chunks_exact_mut(4) { let a = p[3] as u32; for c in 0..3 { p[c] = ((p[c] as u32 * a + 127) / 255) as u8; } }
    o
}

fn q8(c: f32) -> u8 { (c.clamp(0., 1.) * 255. + 0.5) as u8 }

/// Draw one material alone into a `w x h` transparent image with the GPU path; asserts the GPU path (not the bake fallback) was used.
fn gpu_one(r: &mut UiRenderer, a: &mut UiAssets, kind: MaterialKind, rect: Rect, tint: [f32; 4], flip: bool, w: u32, h: u32) -> Vec<u8> {
    r.set_material_mode(MaterialMode::Gpu);
    let mut l = RenderList::new();
    l.material(kind, rect, tint, flip);
    let img = r.render_to_image(&l, a, w, h, None);
    let st = r.stats();
    assert_eq!((st.gpu_materials, st.baked_materials), (1, 0), "material was not drawn by the GPU path: {st:?}");
    img
}

#[derive(Default)]
struct Table(BTreeMap<String, (u32, f64, u32)>);
impl Table {
    fn add(&mut self, name: &str, d: (u32, f64)) { let e = self.0.entry(name.to_owned()).or_insert((0, 0.0, 0)); e.0 = e.0.max(d.0); e.1 += d.1; e.2 += 1; }
    fn print_and_assert(&self, title: &str) {
        eprintln!("---- {title}: GPU vs CPU (max / mean abs diff in 1/255) ----");
        let mut worst = (0u32, 0f64);
        for (k, (mx, sum, n)) in &self.0 {
            let mean = sum / *n as f64;
            eprintln!("{k:<44} max {mx:>3}  mean {mean:>7.4}  ({n} cases)");
            worst.0 = worst.0.max(*mx); worst.1 = worst.1.max(mean);
        }
        eprintln!("{title}: worst max {} worst mean {:.4}", worst.0, worst.1);
        assert!(worst.0 <= MAX_BOUND && worst.1 <= MEAN_BOUND, "{title}: GPU deviates from the CPU reference (max {} > {MAX_BOUND} or mean {:.4} > {MEAN_BOUND})", worst.0, worst.1);
    }
}

const ALL_MATS: [uimat::Mat; 23] = {
    use uimat::Mat::*;
    [BorderMainL, SolidBgShellBlue, TabManage, TabSecondary, TabMain, ItemInfoBlankDkBlue, ItemInfoBlank, ItemInfoUnlocked, ItemInfoLocked, StatItem, SettingsSelect, RotatorBorder,
     SliderFill, SliderFillLight, RadioM, ButtonBar, BorderTabM, BorderTabMSolid, BorderTabMSolid10, ArrowLeft, ArrowRight, BottomBoxRounded, SecondaryM]
};

#[test]
fn settings_uimat_gpu_matches_cpu() {
    let Some(table) = with_gpu(|r| {
        let mut a = new_assets();
        let mut t = Table::default();
        let configs: [((u32, u32), f32, f32); 3] = [((96, 40), 1.0, 0.5), ((120, 56), 2.0, 0.8), ((64, 64), 1.0, 0.0)];
        let mut drawn = 0;
        for mat in ALL_MATS {
            for state in uimat::MatState::ALL {
                let Some(brush) = uimat::default_brush(mat, state) else { continue };
                let name = uimat::material_name(mat, state).unwrap();
                for ((w, h), scale, fill) in configs {
                    let p = uimat::MatParams { fill_percent: fill, size_px: (w as f32 / scale, h as f32 / scale), ..Default::default() };
                    let want = premult(&uimat::raster_brush(&brush, w, h, &p));
                    let (iw, ih) = (w + 16, h + 12);
                    let img = gpu_one(r, &mut a, MaterialKind::Settings { mat, state, fill, scale }, Rect::new(8., 6., w as f32, h as f32), [1.; 4], false, iw, ih);
                    // crop the rect out of the image
                    let mut got = Vec::with_capacity((w * h * 4) as usize);
                    for y in 0..h { let o = (((y + 6) * iw + 8) * 4) as usize; got.extend_from_slice(&img[o..o + (w * 4) as usize]); }
                    let d = diff(&got, &want);
                    if d.0 > MAX_BOUND { eprintln!("  !! {name} {mat:?}/{state:?} {w}x{h} scale {scale} fill {fill}: max {} mean {:.3}", d.0, d.1); }
                    t.add(name, d);
                    // outside the rect nothing is drawn
                    assert!(img[..(8 * 4) as usize].iter().all(|b| *b == 0));
                    drawn += 1;
                }
            }
        }
        eprintln!("settings: {drawn} (material, state, size) cases rendered on the GPU");
        t
    }) else { return };
    table.print_and_assert("Settings uimat (30 pixel shaders)");
}

#[test]
fn settings_tint_flip_and_scissor() {
    let _ = with_gpu(|r| {
        let mut a = new_assets();
        let (mat, state) = (uimat::Mat::TabSecondary, uimat::MatState::Hovered);
        let kind = MaterialKind::Settings { mat, state, fill: 0.5, scale: 1.0 };
        let base = gpu_one(r, &mut a, kind.clone(), Rect::new(4., 4., 64., 32.), [1.; 4], false, 72, 40);
        // tint = premultiplied multiply of the (bake-equivalent) result
        let tinted = gpu_one(r, &mut a, kind.clone(), Rect::new(4., 4., 64., 32.), [0.5, 1.0, 0.25, 0.6], false, 72, 40);
        let mut want = base.clone();
        for p in want.chunks_exact_mut(4) {
            let (tr, tg, tb, ta) = (0.5f32 * 0.6, 1.0f32 * 0.6, 0.25f32 * 0.6, 0.6f32);
            p[0] = q8(p[0] as f32 / 255. * tr); p[1] = q8(p[1] as f32 / 255. * tg); p[2] = q8(p[2] as f32 / 255. * tb); p[3] = q8(p[3] as f32 / 255. * ta);
        }
        let d = diff(&tinted, &want);
        eprintln!("[materials] tint: max {} mean {:.4}", d.0, d.1);
        assert!(d.0 <= 2);
        // flip_x mirrors the image
        let flipped = gpu_one(r, &mut a, kind.clone(), Rect::new(4., 4., 64., 32.), [1.; 4], true, 72, 40);
        let mut mirrored = base.clone();
        for y in 0..40usize { for x in 0..64usize { let (s, d) = ((y * 72 + 4 + x) * 4, (y * 72 + 4 + 63 - x) * 4); mirrored[d..d + 4].copy_from_slice(&base[s..s + 4]); } }
        let d = diff(&flipped, &mirrored);
        eprintln!("[materials] flip_x: max {} mean {:.4}", d.0, d.1);
        assert!(d.0 <= 3, "flip must mirror the evaluation");
        // clip: scissor applies to material draws
        let mut l = RenderList::new();
        l.push_clip(Rect::new(0., 0., 30., 40.));
        l.material(kind, Rect::new(4., 4., 64., 32.), [1.; 4], false);
        l.pop_clip();
        r.set_material_mode(MaterialMode::Gpu);
        let clipped = r.render_to_image(&l, &mut a, 72, 40, None);
        for y in 0..40usize { for x in 0..72usize { let o = (y * 72 + x) * 4; let want_px = if x < 30 { &base[o..o + 4] } else { &[0u8, 0, 0, 0][..] }; assert_eq!(&clipped[o..o + 4], want_px, "pixel ({x},{y})"); } }
    });
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// Procedural + Panel: Rust port of the old menu/outline.rs (reference)
// ---------------------------------------------------------------------------------------------------------------------------------------------
fn srgb(c: f32) -> f32 { let c = c.clamp(0., 1.); if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 } }
fn lin_to_srgb(c: [f32; 3]) -> [f32; 3] { [srgb(c[0]), srgb(c[1]), srgb(c[2])] }
fn smooth01(x: f32) -> f32 { let x = x.clamp(0., 1.); x * x * (3. - 2. * x) }
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] { [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t] }
fn slice_uv(p: f32, size: f32, margin_px: f32, margin_uv: f32) -> f32 {
    let m = margin_px.min(size * 0.5).max(1e-4);
    if p < m { p / m * margin_uv } else if p > size - m { 1. - (size - p) / m * margin_uv } else { let mid = (size - 2. * m).max(1e-4); margin_uv + (p - m) / mid * (1. - 2. * margin_uv) }
}
fn outline_pixel(l: &crate::materials::MatLook, u: f32, v: f32) -> [f32; 4] {
    let (cx, cy) = ((u - 0.5).abs() - l.box_size, (v - 0.5).abs() - l.box_size);
    let ceil01 = |x: f32| if x > 0. { 1.0f32 } else { 0.0 };
    let band = (ceil01(cx).hypot(ceil01(cy))).min(1.);
    let fill = if u > l.inner_fill_offset { 1. - l.switch_offset } else { l.switch_offset };
    let s = smooth01(v);
    let g = (1. - s) + l.gradient_min_alpha * s;
    let a0 = (1. - band) * g * l.gradient_alpha + band * l.outline_alpha;
    let rgb = lin_to_srgb(lerp3(l.primary, l.base, (1. - band) * s * l.dark_base_alpha));
    [rgb[0], rgb[1], rgb[2], ((band + fill).clamp(0., 1.) * a0).clamp(0., 1.)]
}
fn primary_pixel(vis: Vis, u: f32, v: f32, v1: f32) -> [f32; 4] {
    const SIZE: f32 = 0.3; const SHADOW_DIST: f32 = 0.22; const SS_MIN: f32 = 0.18; const SS_MAX: f32 = 0.15;
    let (rgba0, rgba1, shadow, alpha) = primary_m_look(vis);
    let (px, py) = (u - 0.5, v - 0.5);
    let sdf = |ex: f32, ey: f32| ((px.abs() - ex).max(0.)).hypot((py.abs() - ey).max(0.));
    let inv = |d: f32| smooth01((d - SS_MIN) / (SS_MAX - SS_MIN));
    let shadow_mix = (1. - inv(sdf(SIZE, SHADOW_DIST))) * if v + 0.7 >= 1. { 1. } else { 0. } * 1.0;
    let face = inv(sdf(SIZE, SIZE));
    let g = smooth01(v1);
    let base = lerp3(rgba0, rgba1, g);
    let c = lerp3(lerp3(shadow, base, face), shadow, shadow_mix);
    let rgb = lin_to_srgb([c[0].max(0.), c[1].max(0.), c[2].max(0.)]);
    [rgb[0], rgb[1], rgb[2], (face * alpha).clamp(0., 1.)]
}
fn panel_pixel(kind: PanelKind, u: f32, v: f32) -> [f32; 4] {
    match kind {
        PanelKind::Backdrop => {
            let (rgba0, rgba1) = ([0., 0.002125, 0.017642, 1.], [0., 0.004391, 0.064803, 0.6]);
            let d = (u - 1.).hypot(v - 2.2);
            let n = 1. - d / 2.;
            let y = (n * 1.).powi(2) * 1.442_695;
            let w = (1. - 1. / y.exp()).max(1e-6);
            let lg = if n >= 0. && n.abs() > 1e-5 { w.ln() } else { -19.931_568 };
            let t = ((lg * 0.54317).exp() * 1.886526).clamp(0., 1.);
            let rgb = lin_to_srgb(lerp3([rgba0[0], rgba0[1], rgba0[2]], [rgba1[0], rgba1[1], rgba1[2]], t));
            [rgb[0], rgb[1], rgb[2], (rgba0[3] + (rgba1[3] - rgba0[3]) * t).clamp(0., 1.)]
        }
        PanelKind::ModalPanel | PanelKind::ModalHeader => {
            let (c0, c1) = if kind == PanelKind::ModalPanel { (MODAL_RGBA0, MODAL_RGBA1) } else { (HEADER_RGBA0, HEADER_RGBA1) };
            let a = 1.0f32 * 0.25;
            let dir = [a.cos(), -a.sin()];
            let x = dir[0] * (u - 0.5) + dir[1] * (v - 0.5) + 0.5;
            let t = smooth01(x);
            let rgb = lin_to_srgb(lerp3(c0, c1, t));
            [rgb[0], rgb[1], rgb[2], 1.]
        }
    }
}

/// Reference of `outline::expand` + fills: premultiplied image (w x h) with the procedural box drawn at `rect`.
fn proc_reference(style: ProcStyle, vis: Vis, rect: Rect, unit: f32, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    let (mu, mv, image) = match style {
        ProcStyle::PrimaryM => (PM_MARGIN.0, PM_MARGIN.1, PM_IMAGE),
        s if s.left_shade() => (OUTLINE_SHADE_MARGIN.0, OUTLINE_SHADE_MARGIN.1, OUTLINE_IMAGE),
        _ => (OUTLINE_MARGIN.0, OUTLINE_MARGIN.1, OUTLINE_IMAGE),
    };
    let lk = if style == ProcStyle::PrimaryM { None } else { Some(look(style, vis)) };
    let (x0, x1, y0, y1) = (rect.x.round() as i32, (rect.x + rect.w).round() as i32, rect.y.round() as i32, (rect.y + rect.h).round() as i32);
    for y in y0..y1 {
        let py = y as f32 + 0.5 - rect.y;
        let v = slice_uv(py, rect.h, mv * image * unit, mv);
        let v1 = (py / rect.h).clamp(0., 1.);
        for x in x0..x1 {
            let u = slice_uv(x as f32 + 0.5 - rect.x, rect.w, mu * image * unit, mu);
            let c = match &lk { Some(l) => outline_pixel(l, u, v), None => primary_pixel(vis, u, v, v1) };
            // old expand quantised the straight colour to 8 bit, then the quad premultiplied it
            let (r8, g8, b8, a8) = (q8(c[0]) as f32 / 255., q8(c[1]) as f32 / 255., q8(c[2]) as f32 / 255., q8(c[3]) as f32 / 255.);
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 { continue; }
            let o = ((y as u32 * w + x as u32) * 4) as usize;
            out[o] = q8(r8 * a8); out[o + 1] = q8(g8 * a8); out[o + 2] = q8(b8 * a8); out[o + 3] = q8(a8);
        }
    }
    out
}

#[test]
fn procedural_outline_and_primary_gpu_matches_cpu() {
    let Some(table) = with_gpu(|r| {
        let mut a = new_assets();
        let mut t = Table::default();
        let styles = [ProcStyle::Outline, ProcStyle::OutlineRed, ProcStyle::OutlineLeftShade, ProcStyle::OutlineLeftShadeRed, ProcStyle::PrimaryM];
        let rects = [(Rect::new(8., 6., 160., 48.), 1.0f32), (Rect::new(10.4, 9.6, 150.3, 45.2), 1.5), (Rect::new(4., 4., 40., 40.), 2.0)];
        for style in styles {
            for vis in [Vis::Normal, Vis::Hover, Vis::Pressed, Vis::Disabled] {
                for (rect, unit) in rects {
                    let (w, h) = (184u32, 64u32);
                    let img = gpu_one(r, &mut a, MaterialKind::Procedural { style, vis, unit }, rect, [1.; 4], false, w, h);
                    let want = proc_reference(style, vis, rect, unit, w, h);
                    t.add(&format!("{style:?}"), diff(&img, &want));
                }
            }
        }
        t
    }) else { return };
    table.print_and_assert("Procedural (outline family, Primary-M)");
}

#[test]
fn panels_gpu_matches_cpu() {
    let Some(table) = with_gpu(|r| {
        let mut a = new_assets();
        let mut t = Table::default();
        for kind in [PanelKind::Backdrop, PanelKind::ModalPanel, PanelKind::ModalHeader] {
            for (w, h) in [(192u32, 108u32), (130, 33)] {
                let img = gpu_one(r, &mut a, MaterialKind::Panel { kind }, Rect::new(0., 0., w as f32, h as f32), [1.; 4], false, w, h);
                let mut straight = vec![0u8; (w * h * 4) as usize];
                for y in 0..h { for x in 0..w {
                    let c = panel_pixel(kind, (x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
                    let o = ((y * w + x) * 4) as usize;
                    for k in 0..4 { straight[o + k] = q8(c[k]); }
                } }
                t.add(&format!("{kind:?}"), diff(&img, &premult(&straight)));
            }
        }
        // backdrop with menu fade tint
        let img = gpu_one(r, &mut a, MaterialKind::Panel { kind: PanelKind::ModalPanel }, Rect::new(0., 0., 64., 64.), [1., 1., 1., 0.5], false, 64, 64);
        assert!(img.chunks_exact(4).all(|p| (p[3] as i32 - 128).abs() <= 1));
        t
    }) else { return };
    table.print_and_assert("Panel (ShellUberBG, modal body/header)");
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// HUD materials: driven like the old ui_material.rs
// ---------------------------------------------------------------------------------------------------------------------------------------------
struct Img { w: usize, h: usize, px: Vec<[f32; 4]> }
fn srgb_to_lin(c: f32) -> f32 { if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } }
impl Img {
    fn from_store(a: &UiAssets, id: fn_ui::draw::TextureId) -> Img {
        let t = a.store.get(id).unwrap();
        let px = t.rgba.chunks_exact(4).map(|p| { let f = |c: u8| srgb_to_lin(c as f32 / 255.); [f(p[0]), f(p[1]), f(p[2]), p[3] as f32 / 255.] }).collect();
        Img { w: t.w as usize, h: t.h as usize, px }
    }
    fn sample(&self, u: f32, v: f32, wrap: bool) -> [f32; 4] {
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
fn cb0() -> Vec<[f32; 4]> { vec![[0.; 4], [0.; 4], [1.0, 1.0 / 2.2, 0., 0.]] }
fn cb1_view() -> Vec<[f32; 4]> { let mut v = vec![[0.; 4]; 132]; v[131] = [0., 0., 0.0, 0.]; v }
fn bake_ref(w: u32, h: u32, mut f: impl FnMut(f32, f32) -> [f32; 4]) -> Vec<u8> {
    let mut straight = vec![0u8; (w * h * 4) as usize];
    for y in 0..h { for x in 0..w {
        let c = f((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
        let o = ((y * w + x) * 4) as usize;
        for k in 0..4 { straight[o + k] = q8(c[k]); }
    } }
    premult(&straight)
}

fn box_uv(t: f32, dest_px: f32, tex_px: f32, m: f32) -> f32 {
    let p = t * dest_px; let mp = m * tex_px;
    if p < mp { p / tex_px } else if p > dest_px - mp { 1. - (dest_px - p) / tex_px } else { m + (p - mp) / (dest_px - 2. * mp).max(1e-3) * (1. - 2. * m) }
}

/// Per-pixel reference of `MaterialKind::RadialProgress` (the CPU translation in fn-ui, evaluated at one element uv).
fn radial_px(tex: &fn_ui::material::hud::Img, u: f32, v: f32, pct: f32) -> [f32; 4] {
    let m = fn_ui::material::hud::radial_mask(u, v, pct); let t = tex.sample(u, v, false);
    let enc = |c: f32| { let c = c.max(0.); if c >= 0.003131 { 1.055 * c.powf(0.416667) - 0.055 } else { c * 12.92 } };
    [enc(t[0] * m), enc(t[1] * m), enc(t[2] * m), (m * t[3]).clamp(0., 1.)]
}

#[test]
fn hud_materials_gpu_matches_cpu() {
    let Some(table) = with_gpu(|r| {
        let mut a = new_assets();
        let mut t = Table::default();
        let noise = |x: u32, y: u32, s: u32| -> u8 { ((x.wrapping_mul(2654435761).wrapping_add(y.wrapping_mul(40503)).wrapping_add(s.wrapping_mul(97))) >> 11) as u8 };
        let icon = make_tex(&mut a, 40, 40, Sampler::UI_ART, |x, y| { let cx = (x as f32 - 20.).hypot(y as f32 - 20.); [(x * 6) as u8, (y * 6) as u8, noise(x, y, 1), if cx < 17. { 255 } else { (255. - (cx - 17.) * 60.).max(0.) as u8 }] });
        let tri = make_tex(&mut a, 16, 16, Sampler::UI_ART, |x, y| [255, 255, 255, if x + y > 15 { (x * 16) as u8 } else { 0 }]);
        let strip = make_tex(&mut a, 96, 12, Sampler::UI_ART, |x, y| [255, 255, 255, if (x / 6 + y / 3) % 3 == 0 { 255 } else { (x * 2) as u8 }]);
        let feather = make_tex(&mut a, 64, 8, Sampler::UI_ART, |x, _| { let d = (x as f32 - 31.5).abs() / 32.; [255, 255, 255, ((1. - d * d) * 255.) as u8] });
        let mask = make_tex(&mut a, 4, 8, Sampler::UI_ART, |x, y| [255, 255, 255, if x == 0 || x == 3 { 0 } else if y == 0 || y == 7 { 90 } else { 255 }]);
        let (iconi, tri_i, stripi, featheri, maski) = (Img::from_store(&a, icon), Img::from_store(&a, tri), Img::from_store(&a, strip), Img::from_store(&a, feather), Img::from_store(&a, mask));
        let sizes = [(76u32, 76u32), (130, 44)];
        let (c0, c1) = (cb0(), cb1_view());
        for (w, h) in sizes {
            let rect = Rect::new(6., 5., w as f32, h as f32);
            let (iw, ih) = (w + 12, h + 10);
            let crop = |img: &[u8]| -> Vec<u8> { let mut o = Vec::new(); for y in 0..h { let s = (((y + 5) * iw + 6) * 4) as usize; o.extend_from_slice(&img[s..s + (w * 4) as usize]); } o };
            // HudBar
            for (ca, cbb) in [([0.07619, 0.50289, 0.0356], [0.17789, 0.73046, 0.08022]), ([0.9, 0.1, 0.2], [0.0, 0.3, 0.9])] {
                let img = gpu_one(r, &mut a, MaterialKind::HudBar { a: ca, b: cbb }, rect, [1.; 4], false, iw, ih);
                let ov = move |n: &str, d: [f32; 4]| match n { "ColorA" => [ca[0], ca[1], ca[2], 1.], "ColorB" => [cbb[0], cbb[1], cbb[2], 1.], _ => d };
                let cb = ui_shaders::hud_bar_cb(&ov);
                let want = bake_ref(w, h, |u, v| { let mut vin = [[0.0f32; 4]; 7]; vin[1] = [1.; 4]; vin[6] = [u, v, 1., 1.]; ui_shaders::hud_bar_ps(&vin, [&c0, &cb], []).unwrap_or([0.; 4]) });
                t.add("HudBar", diff(&crop(&img), &want));
            }
            // PanelVGrad
            {
                let img = gpu_one(r, &mut a, MaterialKind::PanelVGrad, rect, [1.; 4], false, iw, ih);
                let ov = |n: &str, d: [f32; 4]| match n { "Global Alpha" => [0.15; 4], "Rotation" => [0.; 4], "Scale" => [0.75; 4], "RGBA0" => [0., 0.00121, 0.01033, 0.], "RGBA1" => [0., 0.00121, 0.01033, 1.], _ => d };
                let cb = ui_shaders::panel_vgrad_cb(&ov);
                let want = bake_ref(w, h, |u, v| { let mut vin = [[0.0f32; 4]; 9]; vin[1] = [1.; 4]; vin[8] = [u, v, 0., 0.]; ui_shaders::panel_vgrad_ps(&vin, [&c0, &cb], []).unwrap_or([0.; 4]) });
                t.add("PanelVGrad", diff(&crop(&img), &want));
            }
            // PanelVGradHighlighted (M_Border_LightNavy_VGrad: the same shader, other parameters)
            {
                let img = gpu_one(r, &mut a, MaterialKind::PanelVGradHighlighted, rect, [1.; 4], false, iw, ih);
                let q = fn_ui::material::hud::VGRAD_HIGHLIGHTED;
                let ov = move |n: &str, d: [f32; 4]| match n { "Global Alpha" => [q.global_alpha; 4], "Rotation" => [q.rotation; 4], "Scale" => [q.scale; 4], "RGBA0" => q.rgba0, "RGBA1" => q.rgba1, _ => d };
                let cb = ui_shaders::panel_vgrad_cb(&ov);
                let want = bake_ref(w, h, |u, v| { let mut vin = [[0.0f32; 4]; 9]; vin[1] = [1.; 4]; vin[8] = [u, v, 0., 0.]; ui_shaders::panel_vgrad_ps(&vin, [&c0, &cb], []).unwrap_or([0.; 4]) });
                t.add("PanelVGradHighlighted", diff(&crop(&img), &want));
            }
            // RadialProgress (M_HealProgress, hand-written WGSL): wipe of a noisy texture at several percentages
            for pct in [0.0f32, 0.25, 0.5, 0.8, 1.0] {
                let img = gpu_one(r, &mut a, MaterialKind::RadialProgress { percentage: pct, texture: icon }, rect, [1.; 4], false, iw, ih);
                let cpu_img = fn_ui::material::hud::Img::from_texture(&a.store, icon, true).unwrap();
                let want = bake_ref(w, h, |u, v| radial_px(&cpu_img, u, v, pct));
                t.add("RadialProgress", diff(&crop(&img), &want));
            }
            // PanelLine (9-slice brush uv over the 4x8 mask)
            for ga in [0.2f32, 0.9] {
                let img = gpu_one(r, &mut a, MaterialKind::PanelLine { mask, global_alpha: ga }, rect, [1.; 4], false, iw, ih);
                let ov = move |n: &str, d: [f32; 4]| match n { "Global Alpha" => [ga; 4], "RGBA0" | "RGBA1" => [1.; 4], _ => d };
                let cb = ui_shaders::panel_line_cb(&ov);
                let want = bake_ref(w, h, |u, v| {
                    let (bu, bv) = (box_uv(u, w as f32, maski.w as f32, 0.49), box_uv(v, h as f32, maski.h as f32, 0.49));
                    let t0 = |uu: f32, vv: f32| maski.sample(uu, vv, false);
                    let mut vin = [[0.0f32; 4]; 9]; vin[1] = [1.; 4]; vin[6] = [bu, bv, 1., 1.]; vin[8] = [u, v, 0., 0.];
                    ui_shaders::panel_line_ps(&vin, [&c0, &cb], [&t0]).unwrap_or([0.; 4])
                });
                t.add("PanelLine", diff(&crop(&img), &want));
            }
            // Compass
            for pan in [0.25f32, 0.63, 1.9] {
                let img = gpu_one(r, &mut a, MaterialKind::Compass { pan, strip, feather }, rect, [1.; 4], false, iw, ih);
                let ov = move |n: &str, d: [f32; 4]| if n == "CompassPan" { [pan; 4] } else { d };
                let cb = ui_shaders::compass_cb(&ov);
                let (t0, t1) = (|u: f32, v: f32| stripi.sample(u, v, true), |u: f32, v: f32| featheri.sample(u, v, false));
                let want = bake_ref(w, h, |u, v| { let mut vin = [[0.0f32; 4]; 7]; vin[1] = [1.; 4]; vin[6] = [u, v, 1., 1.]; ui_shaders::compass_ps(&vin, [&c0, &cb], [&t0, &t1]).unwrap_or([0.; 4]) });
                t.add("Compass", diff(&crop(&img), &want));
            }
            // ItemCard
            let rarity = [
                [[0.3564, 0.9911, 0.0], [0.1195, 0.521, 0.0], [0.0, 0.2159, 0.0015], [0.0006, 0.0782, 0.0009]],
                [[1.0, 0.5, 0.0], [0.7, 0.2, 0.0], [0.4, 0.1, 0.0], [0.2, 0.05, 0.0]],
                [[0.3, 0.1, 0.9], [0.2, 0.05, 0.6], [0.1, 0.02, 0.3], [0.05, 0.01, 0.15]],
            ];
            for colors in rarity {
                let img = gpu_one(r, &mut a, MaterialKind::ItemCard { colors, icon, hero_tri: tri }, rect, [1.; 4], false, iw, ih);
                let ov = move |name: &str, d: [f32; 4]| -> [f32; 4] {
                    match name {
                        "bIsHero" => [0.; 4], "bIsItem" => [1.; 4], "ItemBackDensity" => [1.5; 4], "ItemBackRadius" => [0.6490830183029175; 4], "gStrokeThickness" => [0.475; 4],
                        "Rarity 1" => [colors[0][0], colors[0][1], colors[0][2], 1.], "Rarity 2" => [colors[1][0], colors[1][1], colors[1][2], 1.],
                        "Rarity 3" => [colors[2][0], colors[2][1], colors[2][2], 1.], "Rarity 4" => [colors[3][0], colors[3][1], colors[3][2], 1.],
                        _ => d,
                    }
                };
                let cb2 = ui_shaders::item_card_cb(&ov);
                let (t0, t1) = (|u: f32, v: f32| tri_i.sample(u, v, true), |u: f32, v: f32| iconi.sample(u, v, false));
                let want = bake_ref(w, h, |u, v| { let mut vin = [[0.0f32; 4]; 9]; vin[1] = [1.; 4]; vin[6] = [u, v, 1., 1.]; vin[8] = [u, v, 0., 0.]; ui_shaders::item_card_ps(&vin, [&c0, &c1, &cb2], [&t0, &t1]).unwrap_or([0.; 4]) });
                t.add("ItemCard", diff(&crop(&img), &want));
            }
        }
        t
    }) else { return };
    table.print_and_assert("HUD materials");
}

/// When fn-ui's CPU bake of a kind is implemented, the GPU and CpuBake modes must agree (this is the end-to-end check of both).
#[test]
fn gpu_and_cpubake_modes_agree_where_bake_exists() {
    let _ = with_gpu(|r| {
        let mut a = new_assets();
        let mask = make_tex(&mut a, 4, 8, Sampler::UI_ART, |x, y| [255, 255, 255, if x == 0 || x == 3 { 0 } else if y == 0 || y == 7 { 90 } else { 255 }]);
        let kinds: Vec<(&str, MaterialKind, Rect)> = vec![
            ("Settings/TabSecondary", MaterialKind::Settings { mat: uimat::Mat::TabSecondary, state: uimat::MatState::Hovered, fill: 0.5, scale: 1.0 }, Rect::new(4., 4., 64., 32.)),
            ("Settings/SliderFill", MaterialKind::Settings { mat: uimat::Mat::SliderFill, state: uimat::MatState::Base, fill: 0.4, scale: 1.0 }, Rect::new(4., 4., 90., 12.)),
            ("Procedural/Outline", MaterialKind::Procedural { style: ProcStyle::Outline, vis: Vis::Normal, unit: 1.0 }, Rect::new(4., 4., 120., 40.)),
            ("Procedural/PrimaryM", MaterialKind::Procedural { style: ProcStyle::PrimaryM, vis: Vis::Hover, unit: 1.0 }, Rect::new(4., 4., 120., 40.)),
            ("Panel/ModalPanel", MaterialKind::Panel { kind: PanelKind::ModalPanel }, Rect::new(4., 4., 100., 60.)),
            ("Panel/Backdrop", MaterialKind::Panel { kind: PanelKind::Backdrop }, Rect::new(4., 4., 100., 60.)),
            ("HudBar", MaterialKind::HudBar { a: [0.07619, 0.50289, 0.0356], b: [0.17789, 0.73046, 0.08022] }, Rect::new(4., 4., 100., 8.)),
            ("PanelVGrad", MaterialKind::PanelVGrad, Rect::new(4., 4., 100., 40.)),
            ("PanelVGradHighlighted", MaterialKind::PanelVGradHighlighted, Rect::new(4., 4., 100., 40.)),
            ("RadialProgress", MaterialKind::RadialProgress { percentage: 0.4, texture: mask }, Rect::new(4., 4., 60., 60.)),
            ("PanelLine", MaterialKind::PanelLine { mask, global_alpha: 0.2 }, Rect::new(4., 4., 8., 60.)),
        ];
        for (name, kind, rect) in kinds {
            let mut l = RenderList::new();
            l.material(kind, rect, [1.; 4], false);
            r.set_material_mode(MaterialMode::CpuBake);
            let cpu = r.render_to_image(&l, &mut a, 136, 76, None);
            r.set_material_mode(MaterialMode::Gpu);
            let gpu = r.render_to_image(&l, &mut a, 136, 76, None);
            if cpu.iter().all(|b| *b == 0) { eprintln!("[bake-vs-gpu] {name}: fn_ui::material::bake not implemented yet for this kind (transparent), skipped"); continue; }
            let d = diff(&gpu, &cpu);
            eprintln!("[bake-vs-gpu] {name}: max {} mean {:.4}", d.0, d.1);
        }
        r.set_material_mode(MaterialMode::Gpu);
    });
}

#[test]
fn every_game_phase_icon_shader_draws_on_the_gpu() {
    let Some(()) = with_gpu(|r| {
        let mut a = new_assets();
        let bg = make_tex(&mut a, 64, 64, Sampler { filter: fn_ui::assets::Filter::Linear, mips: false, wrap: false }, |x, y| { let d = ((x as i32 - 32).pow(2) + (y as i32 - 32).pow(2)) as f32; if d < 900. { [40, 80, 200, 255] } else { [0, 0, 0, 0] } });
        let icon = make_tex(&mut a, 64, 64, Sampler { filter: fn_ui::assets::Filter::Linear, mips: false, wrap: false }, |x, _| if x > 20 && x < 44 { [255, 255, 255, 255] } else { [0, 0, 0, 0] });
        for shader in 0..5u8 {
            let img = gpu_one(r, &mut a, MaterialKind::GamePhaseIcon { shader, bg, icon }, Rect::new(0., 0., 64., 64.), [1.; 4], false, 64, 64);
            assert!(img.chunks_exact(4).any(|p| p[3] > 0), "phase icon shader {shader} drew nothing");
        }
    }) else { return };
}
