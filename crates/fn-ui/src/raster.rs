//! Software reference renderer for a [`RenderList`]: no GPU, deterministic. Used for headless screenshots when no adapter exists, as the
//! ground truth GPU backends are compared with, and by the pixel-diff tooling.
//!
//! Semantics = the old GL `UiBatch`: premultiplied textures, `ONE, ONE_MINUS_SRC_ALPHA` blending in gamma space, trilinear sampling (mip chain
//! built in premultiplied space with a triangle filter) for mip-mapped art, plain bilinear otherwise, clamp-to-edge, pixel-centre sampling.
//! Output is the PREMULTIPLIED RGBA8 framebuffer (clear = transparent black unless given).
use crate::assets::{premultiply_rgba8, Filter, TextureStore, UiAssets};
use crate::draw::{Cmd, MaterialKind, Quad, RenderList, TextureId};
use crate::layout::Rect;
use std::collections::HashMap;

struct Level { w: usize, h: usize, px: Vec<u8> }
struct Sampled { levels: Vec<Level>, linear: bool, wrap: bool }

fn build(store: &TextureStore, id: TextureId) -> Option<Sampled> {
    let t = store.get(id)?;
    t.mark_cpu();
    if !t.has_pixels() { return None; }
    let pm = premultiply_rgba8(&t.rgba);
    let mut levels = vec![Level { w: t.w as usize, h: t.h as usize, px: pm.clone() }];
    if t.sampler.mips {
        let mut im = image::RgbaImage::from_raw(t.w, t.h, pm).unwrap();
        while im.width() > 1 || im.height() > 1 {
            im = image::imageops::resize(&im, (im.width() / 2).max(1), (im.height() / 2).max(1), image::imageops::FilterType::Triangle);
            levels.push(Level { w: im.width() as usize, h: im.height() as usize, px: im.as_raw().clone() });
        }
    }
    Some(Sampled { levels, linear: t.sampler.filter == Filter::Linear, wrap: t.sampler.wrap })
}

fn texel(l: &Level, x: i64, y: i64, wrap: bool) -> [f32; 4] {
    let (x, y) = if wrap { (x.rem_euclid(l.w as i64), y.rem_euclid(l.h as i64)) } else { (x.clamp(0, l.w as i64 - 1), y.clamp(0, l.h as i64 - 1)) };
    let i = (y as usize * l.w + x as usize) * 4;
    [l.px[i] as f32, l.px[i + 1] as f32, l.px[i + 2] as f32, l.px[i + 3] as f32]
}
fn bilinear(l: &Level, u: f32, v: f32, linear: bool, wrap: bool) -> [f32; 4] {
    let (x, y) = (u * l.w as f32, v * l.h as f32);
    if !linear { return texel(l, x.floor() as i64, y.floor() as i64, wrap); }
    let (x, y) = (x - 0.5, y - 0.5);
    let (x0, y0) = (x.floor(), y.floor()); let (fx, fy) = (x - x0, y - y0);
    let (a, b, c, d) = (texel(l, x0 as i64, y0 as i64, wrap), texel(l, x0 as i64 + 1, y0 as i64, wrap), texel(l, x0 as i64, y0 as i64 + 1, wrap), texel(l, x0 as i64 + 1, y0 as i64 + 1, wrap));
    let mut o = [0.; 4];
    for k in 0..4 { o[k] = (a[k] * (1. - fx) + b[k] * fx) * (1. - fy) + (c[k] * (1. - fx) + d[k] * fx) * fy; }
    o
}
fn sample(s: &Sampled, u: f32, v: f32, lod: f32) -> [f32; 4] {
    let n = s.levels.len();
    if n == 1 || lod <= 0. { return bilinear(&s.levels[0], u, v, s.linear, s.wrap); }
    let l = lod.min((n - 1) as f32); let (l0, f) = (l.floor() as usize, l.fract());
    let a = bilinear(&s.levels[l0], u, v, s.linear, s.wrap);
    if f < 1e-4 || l0 + 1 >= n { return a; }
    let b = bilinear(&s.levels[l0 + 1], u, v, s.linear, s.wrap);
    let mut o = [0.; 4]; for k in 0..4 { o[k] = a[k] * (1. - f) + b[k] * f; } o
}

/// Cached sampler state for repeated renders (mip chains, baked materials).
#[derive(Default)]
pub struct Rasterizer { tex: HashMap<(u32, u32), Sampled>, baked: HashMap<(u64, u32, u32), Sampled> }

impl Rasterizer {
    pub fn new() -> Self { Self::default() }

    /// Render to a premultiplied RGBA8 buffer (w*h*4, row 0 = top).
    pub fn render(&mut self, list: &RenderList, assets: &mut UiAssets, w: u32, h: u32, clear: Option<[f32; 4]>) -> Vec<u8> {
        let mut fb = vec![[0f32; 4]; w as usize * h as usize];
        if let Some(c) = clear { let a = c[3]; fb.iter_mut().for_each(|p| *p = [c[0] * a * 255., c[1] * a * 255., c[2] * a * 255., a * 255.]); }
        self.run(list, assets, w, h, fb, true)
    }
    /// INVENTORY FIX4 (inventory_gaps verify3 V1): render `list` OVER `under` (premultiplied RGBA8, w*h*4: the scene already in the frame), in one framebuffer,
    /// the way the game draws the UI into the frame (`UiRenderer::render_texture`): `Cmd::Blur` (`SBackgroundBlur`) then blurs the scene behind it and the
    /// widgets paint over it (UE 4.16 `SBackgroundBlur::OnPaint` -> `FSlatePostProcessor::BlurRect` over the back buffer).
    pub fn render_over(&mut self, list: &RenderList, assets: &mut UiAssets, w: u32, h: u32, under: &[u8]) -> Vec<u8> {
        let fb = under.chunks_exact(4).map(|p| [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]).collect::<Vec<_>>();
        assert_eq!(fb.len(), w as usize * h as usize, "render_over: background size");
        self.run(list, assets, w, h, fb, true)
    }
    /// INVENTORY FIX4 (verify3 V1): a UI-only layer on transparent black (`ui-*.png` captures): there is no scene behind it to blur, so `Cmd::Blur` is
    /// skipped (a blur of the empty layer would write an opaque box over the region; composite with [`Rasterizer::render_over`] instead).
    pub fn render_layer(&mut self, list: &RenderList, assets: &mut UiAssets, w: u32, h: u32) -> Vec<u8> {
        self.run(list, assets, w, h, vec![[0f32; 4]; w as usize * h as usize], false)
    }

    fn run(&mut self, list: &RenderList, assets: &mut UiAssets, w: u32, h: u32, mut fb: Vec<[f32; 4]>, blur: bool) -> Vec<u8> {
        let (wu, hu) = (w as usize, h as usize);
        let mut clips: Vec<Rect> = vec![Rect::new(0., 0., w as f32, h as f32)];
        for c in &list.cmds {
            match c {
                Cmd::PushClip(r) => { let cur = *clips.last().unwrap(); let (x0, y0) = (r.x.floor().max(cur.x), r.y.floor().max(cur.y)); let (x1, y1) = ((r.x + r.w).ceil().min(cur.right()), (r.y + r.h).ceil().min(cur.bottom())); clips.push(Rect::new(x0, y0, (x1 - x0).max(0.), (y1 - y0).max(0.))); }
                Cmd::PopClip => { if clips.len() > 1 { clips.pop(); } }
                Cmd::Blur(b) => if blur { blur_rect(&mut fb, wu, hu, b, *clips.last().unwrap()) }, // INVENTORY FIX3
                Cmd::Quad(q) => {
                    let id = q.tex;
                    let key = (id.0, assets.store.get(id).map_or(0, |t| t.version));
                    if !self.tex.contains_key(&key) { self.tex.retain(|k, _| k.0 != id.0); if let Some(s) = build(&assets.store, id) { self.tex.insert(key, s); } }
                    if let Some(s) = self.tex.get(&key) { draw_quad(&mut fb, wu, hu, q, s, *clips.last().unwrap()); }
                }
                Cmd::Material(m) => {
                    let (mw, mh) = (m.rect.w.round().max(1.) as u32, m.rect.h.round().max(1.) as u32);
                    let key = (m.kind.cache_key(), mw, mh);
                    if !self.baked.contains_key(&key) {
                        if self.baked.len() > 400 { self.baked.clear(); }
                        let px = crate::material::bake(&m.kind, mw, mh, assets);
                        self.baked.insert(key, Sampled { levels: vec![Level { w: mw as usize, h: mh as usize, px }], linear: true, wrap: false });
                    }
                    let uv = if m.flip_x { [1., 0., 0., 1.] } else { [0., 0., 1., 1.] };
                    let q = Quad::rect(TextureId::NONE, m.rect, uv, m.tint);
                    draw_quad(&mut fb, wu, hu, &q, &self.baked[&key], *clips.last().unwrap());
                }
            }
        }
        let mut out = Vec::with_capacity(wu * hu * 4);
        for p in &fb { for k in 0..4 { out.push((p[k] + 0.5).clamp(0., 255.) as u8); } }
        out
    }
}
/// One-shot convenience.
pub fn render(list: &RenderList, assets: &mut UiAssets, w: u32, h: u32, clear: Option<[f32; 4]>) -> Vec<u8> { Rasterizer::new().render(list, assets, w, h, clear) }
/// One-shot [`Rasterizer::render_over`] (INVENTORY FIX4).
pub fn render_over(list: &RenderList, assets: &mut UiAssets, w: u32, h: u32, under: &[u8]) -> Vec<u8> { Rasterizer::new().render_over(list, assets, w, h, under) }
/// One-shot [`Rasterizer::render_layer`] (INVENTORY FIX4).
pub fn render_layer(list: &RenderList, assets: &mut UiAssets, w: u32, h: u32) -> Vec<u8> { Rasterizer::new().render_layer(list, assets, w, h) }

fn draw_quad(fb: &mut [[f32; 4]], fw: usize, fh: usize, q: &Quad, s: &Sampled, clip: Rect) {
    let b = q.bounds();
    let x0 = b.x.floor().max(clip.x).max(0.) as i64; let y0 = b.y.floor().max(clip.y).max(0.) as i64;
    let x1 = (b.right().ceil()).min(clip.right()).min(fw as f32) as i64; let y1 = (b.bottom().ceil()).min(clip.bottom()).min(fh as f32) as i64;
    if x1 <= x0 || y1 <= y0 { return; }
    // parallelogram basis: P(s,t) = TL + s*(TR-TL) + t*(BL-TL)
    let (tl, tr, bl) = (q.pos[0], q.pos[1], q.pos[3]);
    let (ex, ey) = ([tr[0] - tl[0], tr[1] - tl[1]], [bl[0] - tl[0], bl[1] - tl[1]]);
    let det = ex[0] * ey[1] - ex[1] * ey[0];
    if det.abs() < 1e-9 { return; }
    let tex = s.levels[0].w.max(1) as f32; let _ = tex;
    let (du, dv) = (q.uv[2] - q.uv[0], q.uv[3] - q.uv[1]);
    // texels per pixel along each quad axis -> lod
    let (l0, lx, ly) = (&s.levels[0], (ex[0] * ex[0] + ex[1] * ex[1]).sqrt(), (ey[0] * ey[0] + ey[1] * ey[1]).sqrt());
    let rx = (du.abs() * l0.w as f32) / lx.max(1e-6); let ry = (dv.abs() * l0.h as f32) / ly.max(1e-6);
    let lod = rx.max(ry).max(1e-6).log2();
    let ta = [q.tint[0] * q.tint[3], q.tint[1] * q.tint[3], q.tint[2] * q.tint[3], q.tint[3]];
    for y in y0..y1 { for x in x0..x1 {
        let (px, py) = (x as f32 + 0.5 - tl[0], y as f32 + 0.5 - tl[1]);
        let ss = (px * ey[1] - py * ey[0]) / det; let tt = (ex[0] * py - ex[1] * px) / det;
        if !(0. ..=1.).contains(&ss) || !(0. ..=1.).contains(&tt) { continue; }
        let t = sample(s, q.uv[0] + ss * du, q.uv[1] + tt * dv, lod);
        let src = [t[0] * ta[0], t[1] * ta[1], t[2] * ta[2], t[3] * ta[3]];
        let d = &mut fb[y as usize * fw + x as usize];
        let inv = 1. - src[3] / 255.;
        for k in 0..4 { d[k] = src[k] + d[k] * inv; }
    } }
}

/// INVENTORY FIX3: `FSlatePostProcessor::BlurRect` on the CPU framebuffer (premultiplied 0..255): the region under `b.rect` (inside `clip`) is
/// (box-downsampled by `b.downsample`,) blurred horizontally then vertically with the taps of [`crate::draw::BlurCmd::taps`] (sample positions clamped to
/// the region, like the shader's `UVBounds`), (bilinearly upsampled,) and written back with alpha 1.
pub fn blur_rect(fb: &mut [[f32; 4]], w: usize, h: usize, b: &crate::draw::BlurCmd, clip: Rect) {
    let x0 = b.rect.x.max(clip.x).max(0.).round() as usize; let y0 = b.rect.y.max(clip.y).max(0.).round() as usize;
    let x1 = (b.rect.right().min(clip.right()).round().max(0.) as usize).min(w); let y1 = (b.rect.bottom().min(clip.bottom()).round().max(0.) as usize).min(h);
    if x1 <= x0 || y1 <= y0 { return; }
    let (rw, rh) = (x1 - x0, y1 - y0);
    let ds = b.downsample.max(1) as usize;
    let (lw, lh) = (rw.div_ceil(ds), rh.div_ceil(ds));
    // source region (downsampled: the mean of each ds x ds cell, clamped to the region)
    let mut src = vec![[0f32; 4]; lw * lh];
    for ly in 0..lh { for lx in 0..lw {
        let (mut acc, mut n) = ([0f32; 4], 0f32);
        for sy in ly * ds..((ly + 1) * ds).min(rh) { for sx in lx * ds..((lx + 1) * ds).min(rw) { let p = fb[(y0 + sy) * w + x0 + sx]; for k in 0..4 { acc[k] += p[k]; } n += 1.; } }
        src[ly * lw + lx] = acc.map(|v| v / n.max(1.));
    } }
    let taps = b.taps();
    let pass = |src: &[[f32; 4]], horiz: bool| -> Vec<[f32; 4]> {
        let mut out = vec![[0f32; 4]; lw * lh];
        for y in 0..lh { for x in 0..lw {
            let mut acc = [0f32; 4];
            for &(d, wt) in &taps {
                let offs: &[i32] = if d == 0 { &[0] } else { &[d, -d] };
                for &o in offs {
                    let (sx, sy) = if horiz { ((x as i32 + o).clamp(0, lw as i32 - 1) as usize, y) } else { (x, (y as i32 + o).clamp(0, lh as i32 - 1) as usize) };
                    let p = src[sy * lw + sx]; for k in 0..4 { acc[k] += p[k] * wt; }
                }
            }
            out[y * lw + x] = acc;
        } }
        out
    };
    let blurred = pass(&pass(&src, true), false);
    for y in 0..rh { for x in 0..rw {
        let c = if ds == 1 { blurred[y * lw + x] } else {
            // bilinear upsample (texel centres)
            let fx = ((x as f32 + 0.5) / ds as f32 - 0.5).clamp(0., (lw - 1) as f32); let fy = ((y as f32 + 0.5) / ds as f32 - 0.5).clamp(0., (lh - 1) as f32);
            let (ix, iy) = (fx.floor() as usize, fy.floor() as usize); let (tx, ty) = (fx - ix as f32, fy - iy as f32);
            let (ix1, iy1) = ((ix + 1).min(lw - 1), (iy + 1).min(lh - 1));
            let g = |xx: usize, yy: usize| blurred[yy * lw + xx];
            let mut o = [0f32; 4];
            for k in 0..4 { o[k] = (g(ix, iy)[k] * (1. - tx) + g(ix1, iy)[k] * tx) * (1. - ty) + (g(ix, iy1)[k] * (1. - tx) + g(ix1, iy1)[k] * tx) * ty; }
            o
        };
        fb[(y0 + y) * w + x0 + x] = [c[0], c[1], c[2], 255.];
    } }
}

/// Convenience used by tests: material cache key helper re-export.
pub fn key_of(k: &MaterialKind) -> u64 { k.cache_key() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{MemAssets, Sampler, Texture};
    fn assets() -> UiAssets { UiAssets::new(Box::new(MemAssets::default())) }
    #[test] fn solid_fill_and_blend() {
        let mut a = assets(); let mut l = RenderList::new();
        l.fill(Rect::new(2., 2., 4., 4.), [1., 0., 0., 1.]);
        l.fill(Rect::new(4., 4., 4., 4.), [0., 0., 1., 0.5]);
        let px = render(&l, &mut a, 10, 10, None);
        let at = |x: usize, y: usize| { let i = (y * 10 + x) * 4; [px[i], px[i + 1], px[i + 2], px[i + 3]] };
        assert_eq!(at(3, 3), [255, 0, 0, 255]); assert_eq!(at(0, 0), [0, 0, 0, 0]);
        assert_eq!(at(7, 7), [0, 0, 128, 128]);              // blue 50% over nothing (premultiplied)
        let o = at(5, 5); assert_eq!(o[0], 128); assert_eq!(o[2], 128); assert_eq!(o[3], 255); // blue 50% over red
    }
    /// INVENTORY FIX4 (verify3 V1): a blur over a transparent layer used to come out as an opaque black box; `render_over` blurs the scene under it
    /// (same pixels as drawing the scene in the same list) and `render_layer` leaves the empty layer empty.
    #[test] fn blur_over_the_scene_and_on_a_layer() {
        let mut a = assets();
        let mut scene = RenderList::new();
        scene.fill(Rect::new(0., 0., 8., 16.), [1., 0., 0., 1.]); scene.fill(Rect::new(8., 0., 8., 16.), [0., 0., 1., 1.]);
        let under = render(&scene, &mut a, 16, 16, None);
        let mut ui = RenderList::new();
        ui.blur(Rect::new(2., 2., 12., 12.), 2., None);
        ui.fill(Rect::new(0., 0., 2., 2.), [0., 1., 0., 1.]);
        let over = render_over(&ui, &mut a, 16, 16, &under);
        let mut one = scene.clone(); one.extend(ui.clone());
        assert_eq!(over, render(&one, &mut a, 16, 16, None), "one framebuffer: the blur reads the scene");
        let at = |px: &[u8], x: usize, y: usize| { let i = (y * 16 + x) * 4; [px[i], px[i + 1], px[i + 2], px[i + 3]] };
        let c = at(&over, 8, 8); assert!(c[0] > 40 && c[2] > 40 && c[3] == 255, "red and blue mixed at the seam: {c:?}");
        let layer = render_layer(&ui, &mut a, 16, 16);
        assert_eq!(at(&layer, 8, 8), [0, 0, 0, 0], "no opaque box on the transparent layer");
        assert_eq!(at(&layer, 0, 0), [0, 255, 0, 255]);
    }
    #[test] fn clip_limits_drawing() {
        let mut a = assets(); let mut l = RenderList::new();
        l.push_clip(Rect::new(0., 0., 5., 10.)); l.fill(Rect::new(0., 0., 10., 10.), [1.; 4]); l.pop_clip();
        let px = render(&l, &mut a, 10, 10, None);
        assert_eq!(px[(0 * 10 + 4) * 4 + 3], 255); assert_eq!(px[(0 * 10 + 5) * 4 + 3], 0);
    }
    #[test] fn texture_orientation_and_premultiply() {
        let mut a = assets();
        // 2x1 texture: left red opaque, right green 50% alpha
        let id = a.store.insert(Texture::new(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 128], Sampler::NEAREST));
        let mut l = RenderList::new(); l.sprite(id, Rect::new(0., 0., 4., 2.), [1.; 4]);
        let px = render(&l, &mut a, 4, 2, None);
        assert_eq!(&px[0..4], &[255, 0, 0, 255]); let i = 3 * 4; assert_eq!(&px[i..i + 4], &[0, 128, 0, 128]);
        let mut l = RenderList::new(); l.sprite_flip(id, Rect::new(0., 0., 4., 2.), true, [1.; 4]);
        let px = render(&l, &mut a, 4, 2, None); assert_eq!(&px[0..4], &[0, 128, 0, 128]);
    }
    #[test] fn rotated_quad_covers_expected_area() {
        let mut a = assets(); let mut l = RenderList::new();
        l.rotated(TextureId::WHITE, (16., 16.), (16., 8.), std::f32::consts::FRAC_PI_2, [1.; 4]);
        let px = render(&l, &mut a, 32, 32, None);
        let n = px.chunks(4).filter(|p| p[3] == 255).count();
        assert!((n as i32 - 128).abs() <= 8, "{n}");
        // 90 degrees: 8 wide x 16 tall around (16,16) -> x 12..20, y 8..24
        assert_eq!(px[(16 * 32 + 16) * 4 + 3], 255); assert_eq!(px[(16 * 32 + 26) * 4 + 3], 0); assert_eq!(px[(26 * 32 + 16) * 4 + 3], 0); assert_eq!(px[(10 * 32 + 16) * 4 + 3], 255);
    }
}
