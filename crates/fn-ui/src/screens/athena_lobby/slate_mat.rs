//! Slate material brushes evaluated from the COOKED data: the material instance chain, the `FMaterialUniformExpressionSet` of the root material
//! evaluated with the instance's effective parameters, and the compiled D3D11 Slate pixel shader (the `FSlateMaterialShaderPS<Default>` permutation
//! the material shader map carries) run on the CPU with `fn-dxbc`'s interpreter.
//!
//! What Slate feeds the pixel shader (verified in the disassembled `FSlateElementVS`, see `material/uimat.rs`): `COLOR` v1 = brush tint x widget
//! colour packed to sRGB bytes and decoded to linear, `v6` = (brush uv, 1, 1), `v7.zw` = element size in Slate units, `v8` = `MaterialTexCoords`
//! (linear 0..1 over the element), `cb0[2]` = (2.2 / gamma, 1 / gamma); the result is gamma-encoded straight RGBA.
use super::store::{jf, jvec, split_object_path, Pkg, Store};
use crate::assets::UiAssets;
use fn_dxbc::extract::scan_blobs;
use fn_dxbc::interp::{Env, Machine, Options, TexOp, TexReq};
use fn_dxbc::unreal_packet::{named_bindings, optional_fields, uniform_buffer_names, ResourceKind, ResourceTable};
use fn_dxbc::{Opcode, OperandType, ProgramType, Shader};
use fn_dxbc_cli::uniform::{evaluate, package_names, parse_set, Expr, Overrides, UniformSet};
use serde_json::Value;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone)]
pub struct MatTex { pub w: usize, pub h: usize, pub rgba: Vec<u8>, pub srgb: bool, pub wrap_x: bool, pub wrap_y: bool }

struct Prog {
    shader: Shader,
    /// pixel shader texture register -> `Texture2D_k`
    tex_regs: Vec<(u32, usize)>,
    /// slot of the material uniform buffer / of the View uniform buffer (if any)
    mat_slot: usize,
    view_slot: Option<usize>,
    file: usize,
}

pub struct SlateMat {
    pub path: String,
    pub root: String,
    default: Option<std::sync::Arc<Prog>>,
    disabled: Option<std::sync::Arc<Prog>>,
    pub cb: Vec<[f32; 4]>,
    pub tex: std::sync::Arc<Vec<Option<MatTex>>>,
    pub warnings: Vec<String>,
    pub textures: Vec<String>,
    pub params: Overrides,
    pub time_dependent: bool,
    set: UniformSet,
    chain_params: Overrides,
    chain_tex: HashMap<String, String>,
}

fn srgb_to_lin(c: f32) -> f32 { if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } }
fn lin_to_srgb(c: f32) -> f32 { if c <= 0.0031308 { c * 12.92 } else { c.powf(1.0 / 2.4) * 1.055 - 0.055 } }

/// Slate packs the linear tint as `FLinearColor::ToFColor(true)`; the vertex shader decodes it back to linear (constants = the hex literals of the
/// disassembled `FSlateElementVS`).
pub fn pack_tint(t: [f32; 4]) -> [f32; 4] {
    let mut o = [0.0f32; 4];
    for k in 0..3 {
        let byte = (lin_to_srgb(t[k].clamp(0.0, 1.0)) * 255.999).floor().clamp(0.0, 255.0) / 255.0;
        let c = byte.max(f32::from_bits(0x38800006));
        let hi = ((c * f32::from_bits(0x3f72a76e) + f32::from_bits(0x3d558919)).log2() * 2.4).exp2();
        let lo = c * f32::from_bits(0x3d9e8391);
        o[k] = if f32::from_bits(0x3d25aee6) < c { hi } else { lo };
    }
    o[3] = (t[3].clamp(0.0, 1.0) * 255.999).floor().clamp(0.0, 255.0) / 255.0;
    o
}

/// Slate nine-patch (AddBoxElement): element-local position (Slate units) -> brush uv (see `material::uimat::nine_slice_uv`).
pub fn nine_slice_uv(pos: (f32, f32), size: (f32, f32), native: (f32, f32), margin: [f32; 4]) -> [f32; 2] {
    fn axis(p: f32, size: f32, native: f32, m0: f32, m1: f32) -> f32 {
        let (mut a, mut b) = (m0 * native, m1 * native);
        if a + b > size && a + b > 0.0 { let s = size / (a + b); a *= s; b *= s; }
        if p < a { if a > 0.0 { p / a * m0 } else { 0.0 } }
        else if p > size - b { if b > 0.0 { (1.0 - m1) + (p - (size - b)) / b * m1 } else { 1.0 } }
        else { let w = size - a - b; if w > 0.0 { m0 + (p - a) / w * (1.0 - m0 - m1) } else { m0 } }
    }
    [axis(pos.0, size.0, native.0, margin[0], margin[2]), axis(pos.1, size.1, native.1, margin[1], margin[3])]
}

fn is_slate_ps(sh: &Shader) -> bool {
    sh.program_type == ProgramType::Pixel && sh.major == 5
        && sh.body().any(|i| i.op == Opcode::Discard)
        && sh.decls.inputs.iter().any(|d| d.reg == 1) && !sh.decls.inputs.iter().any(|d| d.reg == 0)
}
fn is_disabled_effect(sh: &Shader) -> bool {
    sh.body().any(|i| i.op == Opcode::Dp3 && i.src().iter().any(|o| o.is_imm() && o.imm.iter().any(|&b| b == 0x3e99999a)))
}
fn is_font_variant(sh: &Shader) -> bool {
    let b: Vec<_> = sh.body().collect();
    let n = b.len();
    if n < 3 { return false; }
    let last = b[n - 2];
    last.op == Opcode::Mul && last.dst().first().map_or(false, |d| d.is(OperandType::OUTPUT) && d.mask() == 0b1000) && b[n - 3].op == Opcode::Sample
}
fn reads_cb0_3(sh: &Shader) -> bool {
    sh.body().any(|i| i.src().iter().any(|o| o.is(OperandType::CONSTANT_BUFFER) && o.index.len() >= 2 && o.index[0].imm == 0 && o.index[1].imm == 3))
}

fn obj_path(v: &Value) -> Option<String> { v.as_str().map(|s| s.to_owned()).filter(|s| !s.is_empty()) }

fn collect_textures(store: &Store, pkg: &Pkg, expressions: &[Value], out: &mut Vec<String>, depth: usize) {
    if depth > 8 { return; }
    for e in expressions {
        let Some(path) = obj_path(e) else { continue };
        let Some(ex) = pkg.get(&path) else { continue };
        if ex.class.starts_with("MaterialExpressionTextureSample") || ex.class == "MaterialExpressionTextureObject" || ex.class == "MaterialExpressionTextureObjectParameter" {
            if let Some(t) = ex.props.get("Texture").and_then(obj_path) { if !out.contains(&t) { out.push(t); } }
        } else if ex.class == "MaterialExpressionMaterialFunctionCall" {
            if let Some(f) = ex.props.get("MaterialFunction").and_then(obj_path) {
                let (long, _) = split_object_path(&f);
                if let Some(fp) = store.pkg(long) {
                    if let Some(fe) = fp.exports.iter().find(|x| x.class == "MaterialFunction") {
                        let list: Vec<Value> = fe.props.get("FunctionExpressions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                        collect_textures(store, &fp, &list, out, depth + 1);
                    }
                }
            }
        }
    }
}

impl SlateMat {
    pub fn load(store: &Store, assets: &mut UiAssets, object_path: &str) -> Result<SlateMat, String> { Self::load_with(store, assets, object_path, &Overrides::default(), &HashMap::new()) }

    /// `load` with runtime parameter values on top of the instance chain: what `UMaterialInstanceDynamic::SetScalar/Vector/TextureParameterValue` does
    /// (scalars / vectors by name, textures by parameter name -> `/Game/..Name.Name` object path). Used by the native item card (rarity colours, icon).
    pub fn load_with(store: &Store, assets: &mut UiAssets, object_path: &str, extra: &Overrides, extra_tex: &HashMap<String, String>) -> Result<SlateMat, String> {
        // ---- instance chain ----
        let mut chain: Vec<(Rc<Pkg>, usize)> = vec![];
        let mut cur = object_path.to_owned();
        let (root_path, root_pkg, root_i) = loop {
            if chain.len() > 16 { return Err(format!("{object_path}: material parent chain too deep")); }
            let (pkg, i) = store.object(&cur).ok_or_else(|| format!("{cur}: not in the paks"))?;
            match pkg.exports[i].class.as_str() {
                "Material" => break (cur, pkg, i),
                "MaterialInstanceConstant" => {
                    let parent = pkg.exports[i].props.get("Parent").and_then(obj_path).ok_or_else(|| format!("{cur}: instance without parent"))?;
                    chain.push((pkg, i));
                    cur = parent;
                }
                other => return Err(format!("{cur}: unsupported material class {other}")),
            }
        };
        let mut ov = Overrides::default();
        let mut texture_overrides: HashMap<String, String> = HashMap::new();
        for (pkg, i) in chain.iter().rev() {
            let props = &pkg.exports[*i].props;
            if let Some(a) = props.get("ScalarParameterValues").and_then(|v| v.as_array()) {
                for p in a { if let (Some(n), Some(v)) = (p.get("ParameterName").and_then(|v| v.as_str()), p.get("ParameterValue").and_then(jf)) { ov.scalars.insert(n.to_owned(), v); } }
            }
            if let Some(a) = props.get("VectorParameterValues").and_then(|v| v.as_array()) {
                for p in a { if let (Some(n), Some(v)) = (p.get("ParameterName").and_then(|v| v.as_str()), jvec::<4>(p.get("ParameterValue"))) { ov.vectors.insert(n.to_owned(), v); } }
            }
            if let Some(a) = props.get("TextureParameterValues").and_then(|v| v.as_array()) {
                for p in a { if let (Some(n), Some(t)) = (p.get("ParameterName").and_then(|v| v.as_str()), p.get("ParameterValue").and_then(obj_path)) { texture_overrides.insert(n.to_owned(), t); } }
            }
        }
        let (chain_params, chain_tex) = (ov.clone(), texture_overrides.clone());
        for (k, v) in &extra.scalars { ov.scalars.insert(k.clone(), *v); }
        for (k, v) in &extra.vectors { ov.vectors.insert(k.clone(), *v); }
        for (k, v) in extra_tex { texture_overrides.insert(k.clone(), v.clone()); }
        // ---- shader owner: the first package of the chain (instance first) that carries a shader map (instances with a static permutation own one) ----
        let mut paths: Vec<String> = chain.iter().map(|(p, i)| p.exports[*i].path.clone()).collect();
        paths.push(root_path.clone());
        let mut owner: Option<(String, Vec<u8>, Vec<u8>)> = None;
        for p in &paths {
            let (long, _) = split_object_path(p);
            let (Ok(a), Ok(x)) = (store.pak_set().read_game(long, "uasset"), store.pak_set().read_game(long, "uexp")) else { continue };
            let (a, x) = (a.into_owned(), x.into_owned());
            if !scan_blobs(&x).is_empty() { owner = Some((long.to_owned(), a, x)); break; }
        }
        let (owner_long, uasset, uexp) = owner.ok_or_else(|| format!("{object_path}: no shader map in the material chain"))?;
        let names = package_names(&uasset, &uexp)?;
        let set: UniformSet = parse_set(&uexp, &names).map_err(|e| format!("{owner_long}: {e}"))?;
        let exprs: Vec<Value> = root_pkg.exports[root_i].props.get("Expressions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let mut referenced = vec![];
        collect_textures(store, &root_pkg, &exprs, &mut referenced, 0);        let packed = evaluate(&set, &ov, 0.0);
        let time_dependent = set.vectors.iter().chain(set.scalars.iter()).any(expr_uses_time);
        let mut warnings = vec![];

        // texture slots
        let mut tex_paths: Vec<Option<String>> = vec![];
        for e in &set.textures2d {
            tex_paths.push(match e {
                Expr::Texture { index, .. } => referenced.get(*index as usize).cloned(),
                Expr::TextureParameter { name, index, .. } => texture_overrides.get(name).cloned().or_else(|| referenced.get(*index as usize).cloned()),
                _ => None,
            });
        }
        // FRONT END: whole-path substitutions (`/Game/..Name.Name` keys), after the parameter overrides
        for p in tex_paths.iter_mut() { if let Some(s) = p.as_ref().and_then(|s| extra_tex.get(s)) { *p = Some(s.clone()); } }
        let mut tex: Vec<Option<MatTex>> = vec![];
        for p in &tex_paths {
            tex.push(p.as_ref().and_then(|p| load_tex(store, assets, p, &mut warnings)));
        }

        // ---- shaders: classify the Slate permutations (default / disabled effect) ----
        let blobs = scan_blobs(&uexp);
        let mut default: Option<std::sync::Arc<Prog>> = None;
        let mut disabled: Option<std::sync::Arc<Prog>> = None;
        for b in &blobs {
            let Ok(sh) = Shader::parse(&b.bytes) else { continue };
            if !is_slate_ps(&sh) || is_font_variant(&sh) || reads_cb0_3(&sh) { continue; }
            // the permutations come in (default, disabled-effect) order; a material that itself uses the 0.3 / 0.59 / 0.11 luminance weights (the item cards' `Desat`)
            // has them in the default permutation too, so the first candidate is the default one whatever it contains
            let dis = is_disabled_effect(&sh) && default.is_some();
            let slot = if dis { &mut disabled } else { &mut default };
            if slot.is_some() { continue; }
            let mut cbs: Vec<usize> = sh.decls.cbuffers.iter().map(|c| c.slot as usize).collect();
            cbs.sort();
            let mat_slot = *cbs.last().unwrap_or(&1);
            let view_slot = if cbs.len() == 3 { Some(cbs[1]) } else { None };
            let mut tex_regs = vec![];
            if let (Ok(table), Ok(opt)) = (ResourceTable::parse(&b.header), optional_fields(&b.trailer)) {
                if let Ok(Some(ubo)) = uniform_buffer_names(&opt) {
                    if let Ok(binds) = named_bindings(&table, Some(&ubo), &sh.decls) {
                        for nb in binds { if nb.kind == ResourceKind::Texture { tex_regs.push((nb.binding.register as u32, (nb.binding.member / 2) as usize)); } }
                    }
                }
            }
            if tex_regs.is_empty() && !sh.decls.resources.is_empty() {
                // fall back to declaration order
                for (i, r) in sh.decls.resources.iter().enumerate() { tex_regs.push((r.slot, i)); }
                warnings.push(format!("shader {}: texture bindings by declaration order", b.id));
            }
            *slot = Some(std::sync::Arc::new(Prog { shader: sh, tex_regs, mat_slot, view_slot, file: b.id }));
        }
        if default.is_none() { warnings.push("no default Slate pixel shader permutation".into()); }
        Ok(SlateMat { path: object_path.to_owned(), root: format!("{root_path} (shaders of {owner_long})"), default, disabled, cb: packed.cb, tex: std::sync::Arc::new(tex), warnings, textures: referenced, params: ov, time_dependent, set, chain_params, chain_tex })
    }

    pub fn respecialize(&self, store: &Store, assets: &mut UiAssets, extra: &Overrides, extra_tex: &HashMap<String, String>) -> SlateMat {
        let mut ov = self.chain_params.clone();
        let mut texture_overrides = self.chain_tex.clone();
        for (k, v) in &extra.scalars { ov.scalars.insert(k.clone(), *v); }
        for (k, v) in &extra.vectors { ov.vectors.insert(k.clone(), *v); }
        for (k, v) in extra_tex { texture_overrides.insert(k.clone(), v.clone()); }
        let packed = evaluate(&self.set, &ov, 0.0);
        let mut warnings = vec![];
        let mut tex = vec![];
        for e in &self.set.textures2d {
            let mut p = match e {
                Expr::Texture { index, .. } => self.textures.get(*index as usize).cloned(),
                Expr::TextureParameter { name, index, .. } => texture_overrides.get(name).cloned().or_else(|| self.textures.get(*index as usize).cloned()),
                _ => None,
            };
            if let Some(s) = p.as_ref().and_then(|s| extra_tex.get(s)) { p = Some(s.clone()); }
            tex.push(p.as_ref().and_then(|p| load_tex(store, assets, p, &mut warnings)));
        }
        SlateMat { path: self.path.clone(), root: self.root.clone(), default: self.default.clone(), disabled: self.disabled.clone(), cb: packed.cb, tex: std::sync::Arc::new(tex), warnings, textures: self.textures.clone(), params: ov, time_dependent: self.time_dependent, set: self.set.clone(), chain_params: self.chain_params.clone(), chain_tex: self.chain_tex.clone() }
    }

    pub fn is_ready(&self) -> bool { self.default.is_some() }

    /// Human-readable summary (parameters, textures, shader permutations) for diagnostics.
    pub fn describe(&self) -> String {
        let mut s = format!("{} (root {})\n  referenced textures: {:?}\n", self.path, self.root, self.textures);
        for (k, t) in self.tex.iter().enumerate() { s += &format!("  Texture2D_{k}: {}\n", t.as_ref().map(|t| format!("{}x{} srgb={} wrap={}/{}", t.w, t.h, t.srgb, t.wrap_x, t.wrap_y)).unwrap_or_else(|| "MISSING".into())); }
        for (n, p) in [("default", &self.default), ("disabled", &self.disabled)] {
            match p { Some(p) => s += &format!("  {n}: shader {} tex regs {:?} mat cb slot {} view {:?}\n", p.file, p.tex_regs, p.mat_slot, p.view_slot), None => s += &format!("  {n}: none\n") }
        }
        s += &format!("  cb ({} regs): {:?}\n", self.cb.len(), &self.cb[..self.cb.len().min(12)]);
        s += &format!("  warnings: {:?}\n", self.warnings);
        s
    }

    /// Straight RGBA8 (gamma-encoded) of the material over `w` x `h` pixels. `size` = element size in Slate units, `native`/`margin`/`boxed` = the
    /// brush (nine-patch uv mapping for Box brushes), `tint` = linear brush tint x widget colour.
    #[allow(clippy::too_many_arguments)]
    pub fn raster(&self, w: u32, h: u32, size: (f32, f32), native: (f32, f32), margin: [f32; 4], boxed: bool, tint: [f32; 4], disabled: bool, time: f32) -> Vec<u8> {
        self.raster_rows(w, h, size, native, margin, boxed, tint, disabled, time, 0, h.max(1))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn raster_rows(&self, w: u32, h: u32, size: (f32, f32), native: (f32, f32), margin: [f32; 4], boxed: bool, tint: [f32; 4], disabled: bool, time: f32, r0: u32, r1: u32) -> Vec<u8> {
        self.job().raster_rows(w, h, size, native, margin, boxed, tint, disabled, time, r0, r1)
    }
    pub fn job(&self) -> RasterJob { RasterJob { default: self.default.clone(), disabled: self.disabled.clone(), cb: self.cb.clone(), tex: self.tex.clone() } }
}

#[derive(Clone)]
pub struct RasterJob { default: Option<std::sync::Arc<Prog>>, disabled: Option<std::sync::Arc<Prog>>, cb: Vec<[f32; 4]>, tex: std::sync::Arc<Vec<Option<MatTex>>> }
impl RasterJob {
    #[allow(clippy::too_many_arguments)]
    pub fn raster_rows(&self, w: u32, h: u32, size: (f32, f32), native: (f32, f32), margin: [f32; 4], boxed: bool, tint: [f32; 4], disabled: bool, time: f32, r0: u32, r1: u32) -> Vec<u8> {
        let (w, h) = (w.max(1), h.max(1));
        let (r0, r1) = (r0.min(h), r1.min(h).max(r0.min(h)));
        let mut out = vec![0u8; (w * (r1 - r0) * 4) as usize];
        let prog = match (disabled, &self.disabled, &self.default) { (true, Some(d), _) => d, (_, _, Some(d)) => d, _ => return out };
        let gamma = 2.2f32;
        let cb0 = [[0.0f32; 4], [0.0; 4], [2.2 / gamma, 1.0 / gamma, 0.0, 0.0]];
        let mut view = vec![[0.0f32; 4]; 132];
        view[131] = [0.0, 0.0, time, 0.0];
        let tint_p = pack_tint(tint);
        struct E<'a> { mat: &'a RasterJob, prog: &'a Prog }
        impl<'a> Env for E<'a> {
            fn texture(&mut self, req: &TexReq) -> [f32; 4] {
                let k = self.prog.tex_regs.iter().find(|(r, _)| *r == req.texture).map(|(_, k)| *k);
                let Some(t) = k.and_then(|k| self.mat.tex.get(k)).and_then(|t| t.as_ref()) else { return [0.0; 4] };
                match req.op { TexOp::Sample | TexOp::SampleB | TexOp::SampleL | TexOp::SampleD => sample_tex(t, req.coord[0], req.coord[1]), TexOp::Ld => texel(t, req.icoord[0] as i64, req.icoord[1] as i64), _ => [0.0; 4] }
            }
        }
        // rows y0..y1 (every pixel is independent: the material is evaluated per pixel), one interpreter per band
        let band = |y0: u32, y1: u32, dst: &mut [u8]| {
            let mut m = Machine::new(&prog.shader);
            m.opts = Options { ignore_discard: true, ..Options::default() };
            m.set_cb(0, &cb0);
            m.set_cb(prog.mat_slot, &self.cb);
            if let Some(v) = prog.view_slot { m.set_cb(v, &view); }
            let mut env = E { mat: self, prog };
            for y in y0..y1 {
                for x in 0..w {
                    let lin = [(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32];
                    let uv_brush = if boxed { nine_slice_uv((lin[0] * size.0, lin[1] * size.1), size, native, margin) } else { lin };
                    m.set_input(1, tint_p);
                    m.set_input(6, [uv_brush[0], uv_brush[1], 1.0, 1.0]);
                    m.set_input(7, [0.0, 0.0, size.0, size.1]);
                    m.set_input(8, [lin[0], lin[1], 0.0, 0.0]);
                    let c = match m.run(&mut env) { Ok(o) => o.o.first().copied().unwrap_or([0.0; 4]), Err(_) => [1.0, 0.0, 1.0, 1.0] };
                    let o = (((y - y0) * w + x) * 4) as usize;
                    for k in 0..4 { dst[o + k] = (c[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8; }
                }
            }
        };
        let n = r1 - r0;
        if n == 0 { return out; }
        let threads = if w * n >= 65536 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8) } else { 1 };
        if threads <= 1 { band(r0, r1, &mut out); return out; }
        let rows = n.div_ceil(threads as u32);
        std::thread::scope(|s| {
            for (i, chunk) in out.chunks_mut((rows * w * 4) as usize).enumerate() {
                let y0 = r0 + i as u32 * rows;
                let y1 = (y0 + rows).min(r1);
                let band = &band;
                s.spawn(move || band(y0, y1, chunk));
            }
        });
        out
    }
}

fn expr_uses_time(e: &Expr) -> bool {
    match e {
        Expr::Time | Expr::RealTime => true,
        Expr::ComponentSwizzle { e, .. } => expr_uses_time(e),
        Expr::FoldedMath { a, b, .. } | Expr::AppendVector { a, b, .. } => expr_uses_time(a) || expr_uses_time(b),
        Expr::TrigMath { x, y, .. } => expr_uses_time(x) || expr_uses_time(y),
        _ => false,
    }
}

fn load_tex(store: &Store, assets: &mut UiAssets, object_path: &str, warnings: &mut Vec<String>) -> Option<MatTex> {
    let name = object_path.rsplit('.').next()?;
    // INVENTORY FIX3 (inventory_gaps G2): resolve the texture parameter by its long package path (`/Game/UI/Style/BasicStrokeBox`), not its stem
    let img = match assets.image(&crate::assets::texture_name_of(object_path)) { Some(i) => i, None => { warnings.push(format!("texture {name} not decodable")); return None } };
    let (mut srgb, mut wx, mut wy) = (true, true, true);
    if let Some((pkg, i)) = store.object(object_path) {
        let p = &pkg.exports[i].props;
        if let Some(b) = p.get("SRGB").and_then(|b| b.as_bool()) { srgb = b; }
        let addr = |k: &str| p.get(k).and_then(|v| v.as_str()).map(|s| !s.ends_with("TA_Clamp")).unwrap_or(true);
        wx = addr("AddressX"); wy = addr("AddressY");
    }
    Some(MatTex { w: img.w as usize, h: img.h as usize, rgba: img.rgba, srgb, wrap_x: wx, wrap_y: wy })
}

fn texel(t: &MatTex, x: i64, y: i64) -> [f32; 4] {
    let xi = if t.wrap_x { x.rem_euclid(t.w as i64) } else { x.clamp(0, t.w as i64 - 1) } as usize;
    let yi = if t.wrap_y { y.rem_euclid(t.h as i64) } else { y.clamp(0, t.h as i64 - 1) } as usize;
    let p = &t.rgba[(yi * t.w + xi) * 4..(yi * t.w + xi) * 4 + 4];
    let f = |c: u8| { let x = c as f32 / 255.0; if t.srgb { srgb_to_lin(x) } else { x } };
    [f(p[0]), f(p[1]), f(p[2]), p[3] as f32 / 255.0]
}

fn sample_tex(t: &MatTex, u: f32, v: f32) -> [f32; 4] {
    let (x, y) = (u * t.w as f32 - 0.5, v * t.h as f32 - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (a, b, c, d) = (texel(t, x0 as i64, y0 as i64), texel(t, x0 as i64 + 1, y0 as i64), texel(t, x0 as i64, y0 as i64 + 1), texel(t, x0 as i64 + 1, y0 as i64 + 1));
    let mut o = [0.0f32; 4];
    for k in 0..4 { o[k] = (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy; }
    o
}


