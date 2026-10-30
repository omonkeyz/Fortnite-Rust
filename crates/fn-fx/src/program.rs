//! Assembles one render pipeline's WGSL from a (vertex shader, pixel shader) pair of the original cooked programs: both translated by `fn_dxbc::codegen`, their resource
//! declarations renamed / re-bound into one module (`vcb1` / `pt0` ..., VS bindings 0.., PS bindings 16..), and an entry point pair written around the translated
//! functions (vertex attributes -> the `v[]` input registers, outputs -> inter-stage locations equal to the output register numbers, `SV_Position`).
use crate::cooked::{container_input_registers, container_output_registers, translated_function_wgsl, Program};
use fn_dxbc::{OperandType, ProgramType};
use std::collections::BTreeMap;
use std::fmt::Write;

/// Where the shader stage reads a vertex attribute from: the `ATTRIBUTE n` index of the vertex declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VsInput { pub attribute: u32, pub register: u32, pub mask: u8, /// Signature component type: 0 / 3 float, 1 uint, 2 sint (integer attributes are declared `vec4<u32>` / `vec4<i32>` and bit-cast into the register).
    pub comp_type: u32 }

/// One declared resource of the assembled module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResDecl {
    pub stage: Stage,
    /// Slot of the DXBC declaration (`cb<slot>`, `t<slot>`, `s<slot>`).
    pub slot: u32,
    pub kind: ResKind,
    pub group: u32,
    pub binding: u32,
    /// Array length for uniform buffers (float4s).
    pub size: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Pixel,
    /// The depth-only pixel shader (the masked-material clip, see [`assemble`]).
    ClipPixel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum ResKind { Uniform, Tex2D, Tex3D, Cube, Sampler, StorageBuf }

pub const PIXEL_BINDING_BASE: u32 = 16;
/// SHADING MODELS: base-pass render targets of the cooked 1.8 config (r.BasePassOutputsVelocity=True, r.AllowStaticLighting=False; BasePassPixelShader.usf:1066-1092):
/// SV_Target4 = the velocity (WRITES_VELOCITY_TO_GBUFFER), SV_Target5 = GBufferD (CustomData: subsurface colour + opacity, ...). Shared with the velocity pass.
pub const GBUFFER_VELOCITY_TARGET: u32 = 4;
pub const GBUFFER_D_TARGET: u32 = 5;
pub const CLIP_BINDING_BASE: u32 = 32;

pub struct Assembled {
    pub wgsl: String,
    pub resources: Vec<ResDecl>,
    pub vs_inputs: Vec<VsInput>,
    /// Output registers of the vertex shader that are interpolated to the pixel shader (everything but SV_Position).
    pub varyings: Vec<u32>,
    /// Pixel shader outputs (SV_Target registers).
    pub targets: u32,
    /// The pixel shader reads `SV_Position` (its `.w` is handed over as the view depth, see [`assemble`]).
    pub ps_uses_position: bool,
}

/// Replaces the identifiers `cb<N>` / `t<N>` / `s<N>` / `icb` of a translated module by `<prefix>cb<N>` ... (whole-identifier matches only).
fn rename_resources(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() || c == '_' {
            let s = i;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') { i += 1; }
            let id = &text[s..i];
            let digits = |p: &str| id.strip_prefix(p).map_or(false, |r| !r.is_empty() && r.bytes().all(|d| d.is_ascii_digit()));
            if digits("cb") || digits("t") || digits("s") || id == "icb" { out.push_str(prefix); }
            out.push_str(id);
        } else { out.push(c); i += 1; }
    }
    out
}

/// Re-binds the `@group(G) @binding(B) var ...` lines of one stage (`base` added to the binding) and collects them.
fn rebind(text: &str, stage: Stage, base: u32, res: &mut Vec<ResDecl>) -> Result<String, String> {
    let mut out = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("@group(") {
            let (g, rest) = rest.split_once(") @binding(").ok_or("bad group line")?;
            let (b, rest) = rest.split_once(')').ok_or("bad binding line")?;
            let (group, binding): (u32, u32) = (g.parse().map_err(|_| "group")?, b.parse().map_err(|_| "binding")?);
            if group > 2 { return Err(format!("unsupported resource group {group}: {line}")); }
            let name_decl = rest.trim();
            let (kind, size, slot) = if name_decl.starts_with("var<uniform>") {
                let sz = name_decl.split("vec4<f32>, ").nth(1).and_then(|r| r.split('>').next()).and_then(|n| n.parse::<u32>().ok()).ok_or("uniform size")?;
                (ResKind::Uniform, sz, binding)
            } else if name_decl.starts_with("var<storage, read>") { (ResKind::StorageBuf, 0, binding) }
            else if name_decl.contains("texture_2d<f32>") { (ResKind::Tex2D, 0, binding) }
            else if name_decl.contains("texture_3d<f32>") { (ResKind::Tex3D, 0, binding) }
            else if name_decl.contains("texture_cube<f32>") { (ResKind::Cube, 0, binding) }
            else if name_decl.contains(": sampler;") { (ResKind::Sampler, 0, binding) }
            else { return Err(format!("unsupported resource declaration: {line}")); };
            res.push(ResDecl { stage, slot, kind, group, binding: binding + base, size });
            let _ = writeln!(out, "@group({group}) @binding({}){rest}", binding + base);
        } else { out.push_str(line); out.push('\n'); }
    }
    Ok(out)
}

fn function_array_len(text: &str, name: &str) -> (Option<usize>, usize) {
    // `fn NAME(v: array<vec4<f32>, N>) -> array<vec4<f32>, M>`
    let head = text.find(&format!("fn {name}(")).map(|p| &text[p..]).unwrap_or("");
    let line = head.lines().next().unwrap_or("");
    let nums: Vec<usize> = line.split("vec4<f32>, ").skip(1).filter_map(|r| r.split('>').next().and_then(|n| n.parse().ok())).collect();
    if line.contains("(v:") { (nums.first().copied(), nums.get(1).copied().unwrap_or(1)) } else { (None, nums.first().copied().unwrap_or(1)) }
}

/// Builds the module. `float_attr` marks the attributes that are integer vectors in the vertex declaration (unused ones are skipped).
/// `clip`: the `FDepthOnlyPS` of a masked material. With `r.EarlyZPassOnlyMaterialMasking` the base-pass pixel shader of a masked material has NO `discard` (the depth pre-pass
/// clipped); the FX pass has no pre-pass, so the depth-only pixel shader (same material graph, same interpolants: matched by semantic) runs first in the fragment entry.
pub fn assemble(vs: &Program, ps: &Program, lit: bool, clip: Option<&Program>) -> Result<Assembled, String> {
    assemble_tail(vs, ps, if lit { LitTail::Simple } else { LitTail::Unlit }, clip)
}

/// FX RECEIVE (2026-10-05): [`assemble`] of an opaque / masked lit (GBuffer-writing) material for the renderer's scene pass: `prelude` (fn-render
/// `FxRenderer::scene_lit_prelude`: common.wgsl + lit_common.wgsl with the globals at group 3 + fx_resolve.wgsl) is prepended and the fragment entry resolves the
/// GBuffer the original program wrote with `fx_scene_lit` (the CSM / DF sun shadow, light function, DFAO sky diffuse, sky reflection inputs and fog of the static
/// meshes), returning lit_common.wgsl `LitOut`. UE4.16 lights these materials in the deferred passes from the same GBuffer (DeferredShadingCommon.usf EncodeGBuffer).
pub fn assemble_scene_lit(vs: &Program, ps: &Program, clip: Option<&Program>, prelude: &str) -> Result<Assembled, String> {
    assemble_tail(vs, ps, LitTail::Scene(prelude), clip)
}

/// FX ORDER (2026-10-05): [`assemble`] of an opaque / masked UNLIT material (MSM_Unlit, no GBuffer lighting) for the renderer's scene pass. UE4.16 draws it in the
/// base pass like every opaque primitive (BasePassRendering.cpp; SceneColor = emissive, ShadingModelID 0), i.e. before the AO / reflections / fog; the height and
/// volumetric fog reach it through the full-screen fog pass (FogRendering.cpp RenderFog, every opaque pixel by scene depth), which the port applies in the lit tail
/// (`output_hdr`) and the vfog composite. Same `prelude` as [`assemble_scene_lit`]; the fragment entry returns `fx_scene_unlit(in.frag, emissive)` (fx_resolve.wgsl):
/// no ambient share (the AO pass leaves it alone), no reflection inputs (ShadingModelID 0: no sky specular / SSR).
pub fn assemble_scene_unlit(vs: &Program, ps: &Program, clip: Option<&Program>, prelude: &str) -> Result<Assembled, String> {
    assemble_tail(vs, ps, LitTail::SceneUnlit(prelude), clip)
}

/// What the fragment entry does with the pixel program's outputs.
enum LitTail<'a> {
    /// `return o[0]` (unlit / translucent).
    Unlit,
    /// The GBuffer shaded by the simple sun x N.L + sky SH term (`FxLight`, group 3): standalone tools and the lobby (lobby_scene.rs patches this tail).
    Simple,
    /// FX RECEIVE: the renderer's scene lighting (see [`assemble_scene_lit`]).
    Scene(&'a str),
    /// FX ORDER: an unlit opaque / masked material in the renderer's scene pass (see [`assemble_scene_unlit`]).
    SceneUnlit(&'a str),
}

fn assemble_tail(vs: &Program, ps: &Program, tail: LitTail, clip: Option<&Program>) -> Result<Assembled, String> {
    let (vs, ps, clip) = (vs.full(), ps.full(), clip.map(Program::full));
    let (vs, ps, clip) = (&*vs, &*ps, clip.as_deref());
    let lit = !matches!(tail, LitTail::Unlit | LitTail::SceneUnlit(_));
    if vs.shader.program_type != ProgramType::Vertex || ps.shader.program_type != ProgramType::Pixel { return Err("assemble needs a vertex and a pixel program".into()); }
    let vsw = translated_function_wgsl(&vs.shader, "vs_fx")?;
    let psw = translated_function_wgsl(&ps.shader, "ps_fx")?;
    let (vsw, psw) = (rename_resources(&vsw, "v"), rename_resources(&psw, "p"));
    let mut resources = vec![];
    let vsw = rebind(&vsw, Stage::Vertex, 0, &mut resources)?;
    let psw = rebind(&psw, Stage::Pixel, PIXEL_BINDING_BASE, &mut resources)?;
    // the declarations of the two modules use the same helper names (icb); a translated module without `icb` / diagnostics duplicates is fine, but the directive may appear once only
    let (vsw, psw) = (vsw.replace("diagnostic(off, derivative_uniformity);\n", ""), psw.replace("diagnostic(off, derivative_uniformity);\n", ""));
    let mut clipw = String::new();
    let mut clip_map: Vec<(u32, u32)> = vec![]; // (clip pixel input register, vertex output register)
    // GRASS (2026-10-06): the clip program's system-value inputs (register, sysvalue: 1 SV_Position, 9 SV_IsFrontFace). The dithered-LOD clip of an instanced
    // FDepthOnlyPS hashes SV_Position.xy (`ClipLODTransition`); left at 0 every pixel of a fading instance took the same dither value.
    let mut clip_sys: Vec<(u32, u32)> = vec![];
    let mut clip_len = 1usize;
    if let Some(cp) = clip {
        let w = translated_function_wgsl(&cp.shader, "dps_fx")?;
        let w = rebind(&rename_resources(&w, "d"), Stage::ClipPixel, CLIP_BINDING_BASE, &mut resources)?.replace("diagnostic(off, derivative_uniformity);\n", "");
        clip_len = function_array_len(&w, "dps_fx").0.unwrap_or(0);
        let vso = container_output_registers(&vs.blob.bytes);
        for d in cp.shader.decls.inputs.iter().filter(|d| d.file == OperandType::INPUT && d.sysvalue.is_some()) { if !clip_sys.iter().any(|(r, _)| *r == d.reg) { clip_sys.push((d.reg, d.sysvalue.unwrap())); } }
        for d in cp.shader.decls.inputs.iter().filter(|d| d.file == OperandType::INPUT && d.sysvalue.is_none()) {
            let Some((n, i, _, _)) = container_input_registers(&cp.blob.bytes).into_iter().find(|(_, _, r, _)| *r == d.reg) else { continue };
            let Some((_, _, vreg, _)) = vso.iter().find(|(vn, vi, _, _)| *vn == n && *vi == i) else { return Err(format!("clip pixel input {n}{i} is not written by the vertex shader")) };
            clip_map.push((d.reg, *vreg));
        }
        clipw = w;
    }
    let (vin, vout) = function_array_len(&vsw, "vs_fx");
    let (pin, pout) = function_array_len(&psw, "ps_fx");
    let vin = vin.unwrap_or(0);
    let sig_in = container_input_registers(&vs.blob.bytes);
    let comp_of = |reg: u32| fn_dxbc::Container::parse(&vs.blob.bytes).ok().and_then(|c| c.input_signature()).and_then(|s| s.elements.iter().find(|e| e.name == "ATTRIBUTE" && e.register == reg).map(|e| e.comp_type)).unwrap_or(3);
    let sig_out = container_output_registers(&vs.blob.bytes);
    let _ = sig_out;
    let mut vs_inputs = vec![];
    for d in vs.shader.decls.inputs.iter().filter(|d| d.file == OperandType::INPUT && d.sysvalue.is_none()) {
        let Some((_, attr, reg, mask)) = sig_in.iter().find(|(n, _, r, _)| n == "ATTRIBUTE" && *r == d.reg) else { return Err(format!("vertex input register {} has no ATTRIBUTE signature element", d.reg)) };
        if vs_inputs.iter().any(|v: &VsInput| v.register == *reg) { continue; }
        vs_inputs.push(VsInput { attribute: *attr, register: *reg, mask: *mask, comp_type: comp_of(*reg) });
    }
    // vertex shader outputs: the signature of the VS
    let mut out_decls: BTreeMap<u32, bool> = BTreeMap::new(); // register -> is position
    for d in vs.shader.decls.outputs.iter().filter(|d| d.file == OperandType::OUTPUT) { out_decls.insert(d.reg, d.sysvalue == Some(1)); }
    let position_reg = out_decls.iter().find(|(_, p)| **p).map(|(r, _)| *r).ok_or("vertex shader has no SV_Position output")?;
    let varyings: Vec<u32> = out_decls.keys().copied().filter(|r| *r != position_reg).collect();
    // pixel shader inputs
    let mut ps_in: Vec<(u32, Option<u32>)> = vec![];
    for d in ps.shader.decls.inputs.iter().filter(|d| d.file == OperandType::INPUT) { if !ps_in.iter().any(|(r, _)| *r == d.reg) { ps_in.push((d.reg, d.sysvalue)); } }
    for (r, sv) in &ps_in { if sv.is_none() && !varyings.contains(r) { return Err(format!("pixel input register {r} is not written by the vertex shader")); } }
    let targets = ps.shader.decls.outputs.iter().filter(|d| d.file == OperandType::OUTPUT).count() as u32;
    // the lit tail reads GBuffer targets 1 and 3 (BasePassPixelShader.usf:1084-1086): a program without them is not a GBuffer writer (MaterialFlags::uses_gbuffer)
    if lit && !(ps.shader.decls.outputs.iter().any(|d| d.file == OperandType::OUTPUT && d.reg == 1) && ps.shader.decls.outputs.iter().any(|d| d.file == OperandType::OUTPUT && d.reg == 3)) {
        return Err(format!("lit assembly needs GBuffer outputs o1 / o3, the pixel shader declares {targets} target(s)"));
    }
    // FX RECEIVE: the scene resolve also reads GBufferB (metallic, specular, roughness; DeferredShadingCommon.usf:430-432)
    if matches!(tail, LitTail::Scene(_)) && !ps.shader.decls.outputs.iter().any(|d| d.file == OperandType::OUTPUT && d.reg == 2) {
        return Err("scene-lit assembly needs GBuffer output o2 (GBufferB)".into());
    }

    let mut s = String::new();
    s.push_str("diagnostic(off, derivative_uniformity);\n");
    if let LitTail::Scene(prelude) | LitTail::SceneUnlit(prelude) = &tail { s.push_str(prelude); s.push('\n'); }
    // SKY LIGHT: `sky_sh` = View.SkyIrradianceEnvironmentMap x SkyLightColor (fn_render::sky_light::fx_light_block, FX_LIGHT_BYTES); `sky` is the legacy up-facing colour
    if matches!(tail, LitTail::Simple) { s.push_str("struct FxLight { to_sun: vec4<f32>, sun: vec4<f32>, sky: vec4<f32>, sky_sh: array<vec4<f32>, 7> }\n@group(3) @binding(0) var<uniform> fx_light: FxLight;\n"); }
    s.push_str(&vsw);
    s.push('\n');
    s.push_str(&psw);
    s.push('\n');
    s.push_str(&clipw);
    s.push('\n');
    // vertex entry
    s.push_str("struct VsIn {\n");
    for v in &vs_inputs { let ty = match v.comp_type { 1 => "vec4<u32>", 2 => "vec4<i32>", _ => "vec4<f32>" }; let _ = writeln!(s, "    @location({}) a{}: {ty},", v.attribute, v.attribute); }
    let scene_tail = matches!(tail, LitTail::Scene(_) | LitTail::SceneUnlit(_));
    // PREPASS (2026-10-06): a scene-pass program's position is `@invariant`: its depth-only twin (fs_depth) and colour pipeline (fs_main_eq, depth Equal) must
    // rasterize bit-identical depth from the same vertex code (UE's INVARIANT / `precise` position for the same reason)
    s.push_str(if scene_tail { "}\nstruct VsOut {\n    @builtin(position) @invariant pos: vec4<f32>,\n" } else { "}\nstruct VsOut {\n    @builtin(position) pos: vec4<f32>,\n" });
    for r in &varyings { let _ = writeln!(s, "    @location({r}) o{r}: vec4<f32>,"); }
    s.push_str("}\n@vertex fn vs_main(in: VsIn) -> VsOut {\n");
    let _ = writeln!(s, "    var v: array<vec4<f32>, {}>;", vin.max(1));
    for v in &vs_inputs { if v.comp_type == 1 || v.comp_type == 2 { let _ = writeln!(s, "    v[{}] = bitcast<vec4<f32>>(in.a{});", v.register, v.attribute); } else { let _ = writeln!(s, "    v[{}] = in.a{};", v.register, v.attribute); } }
    s.push_str("    let o = vs_fx(v);\n    var r: VsOut;\n");
    let _ = writeln!(s, "    r.pos = o[{position_reg}];");
    for r in &varyings { let _ = writeln!(s, "    r.o{r} = o[{r}];"); }
    s.push_str("    return r;\n}\n");
    // fragment entry
    s.push_str("struct FsIn {\n    @builtin(position) frag: vec4<f32>,\n    @builtin(front_facing) front: bool,\n");
    let mut fs_regs: Vec<u32> = ps_in.iter().filter(|(_, sv)| sv.is_none()).map(|(r, _)| *r).collect();
    for (_, v) in &clip_map { if !fs_regs.contains(v) { fs_regs.push(*v); } }
    for r in &fs_regs { let _ = writeln!(s, "    @location({r}) i{r}: vec4<f32>,"); }
    s.push_str("}\n");
    // the fragment body in three pieces: the pixel program's inputs, the masked clip (the depth-only pixel program) and the shading tail
    // UE reads the pixel's view depth (W of the clip position) from SV_Position.w; WebGPU hands over 1 / W
    let sys_value = |r: u32, sv: u32| -> Result<String, String> { match sv {
        1 => Ok(format!("vec4<f32>(in.frag.x, in.frag.y, in.frag.z, 1.0 / in.frag.w)")),
        9 => Ok(format!("vec4<f32>(bitcast<f32>(select(0u, 0xffffffffu, in.front)), 0.0, 0.0, 0.0)")),
        x => Err(format!("unsupported pixel shader system input {x} (register {r})")),
    } };
    let mut setup = String::new();
    let _ = writeln!(setup, "    var v: array<vec4<f32>, {}>;", pin.unwrap_or(0).max(1));
    let mut uses_pos = false;
    for (r, sv) in &ps_in {
        match sv {
            None => { let _ = writeln!(setup, "    v[{r}] = in.i{r};"); }
            Some(x) => { if *x == 1 { uses_pos = true; } let _ = writeln!(setup, "    v[{r}] = {};", sys_value(*r, *x)?); }
        }
    }
    let mut clip_body = String::new();
    if clip.is_some() {
        if clip_len > 0 {
            let _ = writeln!(clip_body, "    var vd: array<vec4<f32>, {clip_len}>;");
            for (cr, vr) in &clip_map { let _ = writeln!(clip_body, "    vd[{cr}] = in.i{vr};"); }
            for (cr, sv) in &clip_sys { if (*cr as usize) < clip_len { let _ = writeln!(clip_body, "    vd[{cr}] = {};", sys_value(*cr, *sv)?); } }
            clip_body.push_str("    let clipped = dps_fx(vd);\n");
        } else { clip_body.push_str("    let clipped = dps_fx();\n"); }
    }
    // FX RECEIVE: the scene-lit entry writes the lit passes' targets (HDR + reflection inputs, lit_common.wgsl LitOut)
    let header = |name: &str| if scene_tail { format!("@fragment fn {name}(in: FsIn) -> LitOut {{\n") } else { format!("@fragment fn {name}(in: FsIn) -> @location(0) vec4<f32> {{\n") };
    s.push_str(&header("fs_main"));
    s.push_str(&setup);
    s.push_str(&clip_body);
    let shade_at = s.len();
    s.push_str(if pin.is_some() { "    let o = ps_fx(v);\n" } else { "    let o = ps_fx();\n" });
    if matches!(tail, LitTail::Scene(_)) {
        // FX RECEIVE: GBufferA normal (UE axes -> renderer (x, z, y)), scene colour = emissive, GBufferC = base colour + GBufferAO, GBufferB = metallic / specular /
        // roughness (DeferredShadingCommon.usf:417-442, r.AllowStaticLighting 0), resolved by fn-render fx_resolve.wgsl `fx_scene_lit`
        // SHADING MODELS (2026-10-05): a program that writes GBufferD (SV_Target5 = CustomData: cooked r.BasePassOutputsVelocity puts the velocity at 4,
        // r.SelectiveBasePassOutputs writes D only for the models that use it, BasePassPixelShader.usf:1068-1092) is resolved per ShadingModelID (GBufferB.a)
        // by fx_resolve_sm.wgsl `fx_scene_lit_sm` (subsurface / two-sided foliage transmission and sky terms)
        if ps.shader.decls.outputs.iter().any(|d| d.file == OperandType::OUTPUT && d.reg == GBUFFER_D_TARGET) {
            let _ = write!(s, "    let n_ue = normalize(o[1].xyz * 2.0 - 1.0);\n    return fx_scene_lit_sm(in.frag, vec3<f32>(n_ue.x, n_ue.z, n_ue.y), o[0].xyz, o[3].xyz, o[2].x, o[2].y, o[2].z, o[3].w, o[2].w, o[{GBUFFER_D_TARGET}]);\n}}\n");
        } else {
        s.push_str("    let n_ue = normalize(o[1].xyz * 2.0 - 1.0);\n    return fx_scene_lit(in.frag, vec3<f32>(n_ue.x, n_ue.z, n_ue.y), o[0].xyz, o[3].xyz, o[2].x, o[2].y, o[2].z, o[3].w);\n}\n");
        }
    } else if matches!(tail, LitTail::SceneUnlit(_)) {
        // FX ORDER: scene colour = the emissive the unlit base-pass program wrote (target 0), fogged like every opaque pixel (fx_resolve.wgsl `fx_scene_unlit`)
        s.push_str("    return fx_scene_unlit(in.frag, o[0].xyz);\n}\n");
    } else if lit {
        // the original program writes the deferred GBuffer: target 0 = emissive / scene colour, 1 = world normal (n * 0.5 + 0.5), 3 = base colour. The renderer's own lighting
        // (sun x N.L + the sky light) shades the base colour; the original deferred light accumulation is not run. SKY LIGHT: the sky term is SkyLighting.usf's
        // `GetSkySHDiffuse(GBuffer.WorldNormal) * View.SkyLightColor` (ReflectionEnvironmentShared.usf:67) on the GBuffer normal, which is already in UE axes.
        // (lobby_scene.rs patch_wgsl cuts the tail at the `let n_ue` line: keep it first.)
        s.push_str("    let n_ue = normalize(o[1].xyz * 2.0 - 1.0);\n    let n = vec3<f32>(n_ue.x, n_ue.z, n_ue.y);\n    let shn = vec4<f32>(n_ue, 1.0);\n    let shb = shn.xyzz * shn.yzzx;\n    let sky = max(vec3<f32>(0.0), vec3<f32>(dot(fx_light.sky_sh[0], shn), dot(fx_light.sky_sh[1], shn), dot(fx_light.sky_sh[2], shn)) + vec3<f32>(dot(fx_light.sky_sh[3], shb), dot(fx_light.sky_sh[4], shb), dot(fx_light.sky_sh[5], shb)) + fx_light.sky_sh[6].xyz * (n_ue.x * n_ue.x - n_ue.y * n_ue.y));\n    let lit = o[3].xyz * (fx_light.sun.xyz * max(dot(n, fx_light.to_sun.xyz), 0.0) + sky);\n    return vec4<f32>(o[0].xyz + lit, 1.0);\n}\n");
    } else {
        s.push_str("    return o[0];\n}\n");
    }
    // PREPASS (2026-10-06): a masked scene-pass program also gets UE's split (cooked r.EarlyZPass 2 + r.EarlyZPassOnlyMaterialMasking 1: the depth pre-pass
    // runs the material's FDepthOnlyPS and clips, DepthRendering.cpp:281 / :704; the base pass then draws the surviving surface with CF_Equal and no clip,
    // BasePassRendering.cpp:308-361): `fs_depth` = the clip program alone (no colour), `fs_main_eq` = the shading of `fs_main` without the clip. fn-render
    // builds the depth twin and the Equal colour pipeline from them (fx.rs `create_scene_lit_pipeline`); `fs_main` (clip + shade) stays for other users.
    if scene_tail && clip.is_some() {
        let shade = s[shade_at..].to_owned();
        s.push_str(&header("fs_main_eq"));
        s.push_str(&setup);
        s.push_str(&shade);
        s.push_str("@fragment fn fs_depth(in: FsIn) {\n");
        s.push_str(&clip_body);
        s.push_str("}\n");
    } else if scene_tail {
        // FULL PREPASS (2026-10-06): UE4.16 1.8 runs a FULL depth pre-pass (cooked r.SelectiveBasePassOutputs 1 / r.StencilForLODDither 1 -> ShouldForceFullDepthPass ->
        // DDM_AllOpaque + bEarlyZPassMovable, DeferredShadingRenderer.cpp:146-182) and a read-only-depth base pass (:773), so an OPAQUE scene-pass program is in the
        // pre-pass too: `fs_main_eq` (the same shading, drawn with CF_Equal) marks it for fn-render's depth twin, which has no pixel stage (the position-only depth path,
        // DepthRendering.cpp:652-669). A pixel program that clips by itself (a `discard` in the base-pass program, no FDepthOnlyPS split) gets `fs_depth` = the program
        // run for its discard alone, so the pre-pass keeps exactly the pixels the base pass keeps.
        let shade = s[shade_at..].to_owned();
        s.push_str(&header("fs_main_eq"));
        s.push_str(&setup);
        s.push_str(&shade);
        if ps.shader.body().any(|i| i.op == fn_dxbc::Opcode::Discard) {
            s.push_str("@fragment fn fs_depth(in: FsIn) {\n");
            s.push_str(&setup);
            s.push_str("    _ = ps_fx(v);\n}\n");
        }
    }
    // VELOCITY (2026-10-07): cooked r.BasePassOutputsVelocity = 1: the base-pass program writes the scene velocity as SV_Target4
    // (`GBuffer.Velocity = float4(EncodeVelocityToTexture(Calculate2DVelocity(..)), 0, 0) * VelocityPrevScreenPosition.z`, BasePassPixelShader.usf:796-810,
    // `Out.MRT[4]`, :1069; z = 1 - SkipOutputVelocity). The port renders velocity in a separate pass, so `fs_velocity` = the same program (masked clip first:
    // that pass is depth-tested LessEqual, not Equal, so a clipped texel must not write) returning its target 4, stored like the PF_G16R16 unorm target
    // (fn-render taa.rs VELOCITY_ENCODE_WGSL). fn-render builds the velocity twin from it (fx.rs `create_scene_lit_pipeline`, drawn by `draw_velocity`).
    if scene_tail && ps.shader.decls.outputs.iter().any(|d| d.file == OperandType::OUTPUT && d.reg == GBUFFER_VELOCITY_TARGET) {
        s.push_str("@fragment fn fs_velocity(in: FsIn) -> @location(0) vec4<f32> {\n");
        s.push_str(&setup);
        s.push_str(&clip_body);
        let _ = write!(s, "    let o = {};\n    return vec4<f32>(round(clamp(o[{GBUFFER_VELOCITY_TARGET}].xy, vec2<f32>(0.0), vec2<f32>(1.0)) * 65535.0) / 65535.0, 0.0, 0.0);\n}}\n", if pin.is_some() { "ps_fx(v)" } else { "ps_fx()" });
    }
    let _ = (vout, pout);
    Ok(Assembled { wgsl: s, resources, vs_inputs, varyings, targets, ps_uses_position: uses_pos })
}

/// FX SHADOW (2026-10-05): the module of a shadow-depth program pair of the material's own shader map (`TShadowDepthVS*` + the masked material's
/// `TShadowDepthPS*`, or no pixel shader: UE's null pixel shader, ShadowDepthRendering.cpp:813). `vs_main` writes the program's `SV_Position` through
/// `fx_shadow_depth_outputs`, defined by `prelude` (the renderer's `fn_render::fx::FX_SHADOW_WGSL`); `fs_main` (only with a pixel program) runs the program for its `discard`
/// (`GetMaterialClippingShadowDepth`, ShadowDepthPixelShader.usf) and writes no colour (`OutColor = 0` goes to no target in a depth-only pass).
pub fn assemble_shadow(vs: &Program, ps: Option<&Program>, prelude: &str) -> Result<Assembled, String> {
    let (vs, ps) = (vs.full(), ps.map(Program::full));
    let (vs, ps) = (&*vs, ps.as_deref());
    if vs.shader.program_type != ProgramType::Vertex || ps.map_or(false, |p| p.shader.program_type != ProgramType::Pixel) { return Err("assemble_shadow needs a vertex (and optionally a pixel) program".into()); }
    let mut resources = vec![];
    let vsw = rebind(&rename_resources(&translated_function_wgsl(&vs.shader, "vs_fx")?, "v"), Stage::Vertex, 0, &mut resources)?.replace("diagnostic(off, derivative_uniformity);\n", "");
    let psw = match ps { Some(p) => rebind(&rename_resources(&translated_function_wgsl(&p.shader, "ps_fx")?, "p"), Stage::Pixel, PIXEL_BINDING_BASE, &mut resources)?.replace("diagnostic(off, derivative_uniformity);\n", ""), None => String::new() };
    let (vin, _) = function_array_len(&vsw, "vs_fx");
    let vin = vin.unwrap_or(0);
    let sig_in = container_input_registers(&vs.blob.bytes);
    let comp_of = |reg: u32| fn_dxbc::Container::parse(&vs.blob.bytes).ok().and_then(|c| c.input_signature()).and_then(|s| s.elements.iter().find(|e| e.name == "ATTRIBUTE" && e.register == reg).map(|e| e.comp_type)).unwrap_or(3);
    let mut vs_inputs = vec![];
    for d in vs.shader.decls.inputs.iter().filter(|d| d.file == OperandType::INPUT && d.sysvalue.is_none()) {
        let Some((_, attr, reg, mask)) = sig_in.iter().find(|(n, _, r, _)| n == "ATTRIBUTE" && *r == d.reg) else { return Err(format!("vertex input register {} has no ATTRIBUTE signature element", d.reg)) };
        if vs_inputs.iter().any(|v: &VsInput| v.register == *reg) { continue; }
        vs_inputs.push(VsInput { attribute: *attr, register: *reg, mask: *mask, comp_type: comp_of(*reg) });
    }
    let mut out_decls: BTreeMap<u32, bool> = BTreeMap::new();
    for d in vs.shader.decls.outputs.iter().filter(|d| d.file == OperandType::OUTPUT) { out_decls.insert(d.reg, d.sysvalue == Some(1)); }
    let position_reg = out_decls.iter().find(|(_, p)| **p).map(|(r, _)| *r).ok_or("vertex shader has no SV_Position output")?;
    // interpolants: only those the pixel program reads (none without one)
    let mut ps_in: Vec<(u32, Option<u32>)> = vec![];
    if let Some(p) = ps { for d in p.shader.decls.inputs.iter().filter(|d| d.file == OperandType::INPUT) { if !ps_in.iter().any(|(r, _)| *r == d.reg) { ps_in.push((d.reg, d.sysvalue)); } } }
    let varyings: Vec<u32> = ps_in.iter().filter(|(_, sv)| sv.is_none()).map(|(r, _)| *r).collect();
    for r in &varyings { if !out_decls.contains_key(r) || *r == position_reg { return Err(format!("shadow pixel input register {r} is not written by the vertex shader")); } }
    let mut s = String::new();
    s.push_str("diagnostic(off, derivative_uniformity);\n");
    s.push_str(prelude);
    s.push_str(&vsw); s.push('\n'); s.push_str(&psw); s.push('\n');
    s.push_str("struct VsIn {\n");
    for v in &vs_inputs { let ty = match v.comp_type { 1 => "vec4<u32>", 2 => "vec4<i32>", _ => "vec4<f32>" }; let _ = writeln!(s, "    @location({}) a{}: {ty},", v.attribute, v.attribute); }
    s.push_str("}\nstruct VsOut {\n    @builtin(position) pos: vec4<f32>,\n");
    for r in &varyings { let _ = writeln!(s, "    @location({r}) o{r}: vec4<f32>,"); }
    s.push_str("}\n@vertex fn vs_main(in: VsIn) -> VsOut {\n");
    let _ = writeln!(s, "    var v: array<vec4<f32>, {}>;", vin.max(1));
    for v in &vs_inputs { if v.comp_type == 1 || v.comp_type == 2 { let _ = writeln!(s, "    v[{}] = bitcast<vec4<f32>>(in.a{});", v.register, v.attribute); } else { let _ = writeln!(s, "    v[{}] = in.a{};", v.register, v.attribute); } }
    s.push_str(if vin == 0 { "    let o = vs_fx();\n    var r: VsOut;\n" } else { "    let o = vs_fx(v);\n    var r: VsOut;\n" });
    let _ = writeln!(s, "    r.pos = fx_shadow_depth_outputs(o[{position_reg}]);");
    for r in &varyings { let _ = writeln!(s, "    r.o{r} = o[{r}];"); }
    s.push_str("    return r;\n}\n");
    let mut uses_pos = false;
    if ps.is_some() {
        let (pin, _) = function_array_len(&psw, "ps_fx");
        s.push_str("struct FsIn {\n    @builtin(position) frag: vec4<f32>,\n    @builtin(front_facing) front: bool,\n");
        for r in &varyings { let _ = writeln!(s, "    @location({r}) i{r}: vec4<f32>,"); }
        s.push_str("}\n@fragment fn fs_main(in: FsIn) {\n");
        let _ = writeln!(s, "    var v: array<vec4<f32>, {}>;", pin.unwrap_or(0).max(1));
        for (r, sv) in &ps_in {
            match sv {
                None => { let _ = writeln!(s, "    v[{r}] = in.i{r};"); }
                Some(1) => { uses_pos = true; let _ = writeln!(s, "    v[{r}] = vec4<f32>(in.frag.x, in.frag.y, in.frag.z, 1.0 / in.frag.w);"); }
                Some(9) => { let _ = writeln!(s, "    v[{r}] = vec4<f32>(bitcast<f32>(select(0u, 0xffffffffu, in.front)), 0.0, 0.0, 0.0);"); }
                Some(x) => return Err(format!("unsupported pixel shader system input {x}")),
            }
        }
        s.push_str(if pin.is_some() { "    _ = ps_fx(v);\n}\n" } else { "    _ = ps_fx();\n}\n" });
    }
    let targets = 0;
    Ok(Assembled { wgsl: s, resources, vs_inputs, varyings, targets, ps_uses_position: uses_pos })
}

/// INSTANCING (2026-10-07): float4s per instance of the `Primitive` storage array of an instanced module ([`instance_primitive`]): the stride of the caller's
/// storage buffer (fn-render primitive_ub writes 18; every declared `Primitive` cbuffer must fit).
pub const INSTANCE_PRIMITIVE_FLOAT4S: u32 = 32;

/// INSTANCING (2026-10-07): rewrites an assembled module (scene-lit, unlit or shadow) so ONE draw serves many placements of the same mesh section: every
/// `Primitive` cbuffer the stages declare (`prim`: the renamed names, `vcb<N>` / `pcb<N>` / `dcb<N>`) becomes a read-only storage array of
/// [`INSTANCE_PRIMITIVE_FLOAT4S`]-float4 records indexed by the instance's id (`u32` vertex attribute `attribute`, instance step), handed to the pixel stages as a
/// flat varying when they read `Primitive` too. Same programs, same per-placement `Primitive` values: only where they are read from changes. `prev_l2w` =
/// (the vertex `$Globals` name, float4 index of `PreviousLocalToWorld`): its four rows are read from the instance's `Primitive.LocalToWorld` (rows 0..3), the
/// value `TBasePassVertexShaderPolicyParamType::SetMesh` gives a static component (BasePassRendering.inl:31: previous = current transform). Errors (the caller
/// keeps the draw per placement): a `Primitive` reference that is not an indexed read, a dynamic index into that `$Globals` buffer, no free attribute /
/// inter-stage location.
pub fn instance_primitive(a: &Assembled, prim: &[String], prev_l2w: Option<(&str, u32)>) -> Result<(String, u32), String> {
    if prim.is_empty() { return Err("no Primitive buffer".into()); }
    let vprim = prim.iter().find(|p| p.starts_with('v')).cloned();
    if prev_l2w.is_some() && vprim.is_none() { return Err("PreviousLocalToWorld without a vertex Primitive buffer".into()); }
    let frag_uses = prim.iter().any(|p| !p.starts_with('v'));
    let used: Vec<u32> = a.vs_inputs.iter().map(|v| v.attribute).collect();
    let attribute = (0..16u32).rev().find(|l| !used.contains(l)).ok_or("no free vertex attribute location")?;
    let vary = a.varyings.iter().max().map_or(0, |m| m + 1);
    if frag_uses && vary > 15 { return Err(format!("no free inter-stage location ({} varyings)", a.varyings.len())); }
    let mut out = String::with_capacity(a.wgsl.len() + 1024);
    let mut header_done = false;
    for line in a.wgsl.split_inclusive('\n') {
        if !header_done && !line.starts_with("diagnostic(") && !line.starts_with("enable ") {
            let _ = writeln!(out, "struct FxInstPrim {{ d: array<vec4<f32>, {INSTANCE_PRIMITIVE_FLOAT4S}> }}\nvar<private> fx_inst: u32;");
            header_done = true;
        }
        // the declarations: `@group(G) @binding(B) var<uniform> NAME: array<vec4<f32>, N>;`
        if line.starts_with("@group(") {
            if let Some(name) = prim.iter().find(|p| line.contains(&format!("var<uniform> {p}: "))) {
                let n: u32 = line.split("vec4<f32>, ").nth(1).and_then(|r| r.split('>').next()).and_then(|x| x.trim().parse().ok()).ok_or_else(|| format!("{name}: declaration size"))?;
                if n > INSTANCE_PRIMITIVE_FLOAT4S { return Err(format!("{name}: {n} float4s > {INSTANCE_PRIMITIVE_FLOAT4S}")); }
                let head = &line[..line.find("var<uniform>").unwrap()];
                let _ = writeln!(out, "{head}var<storage, read> {name}: array<FxInstPrim>;");
                continue;
            }
            out.push_str(line);
            continue;
        }
        // the reads: `NAME[i]` -> `NAME[fx_inst].d[i]`; `$Globals[k]` (k in the PreviousLocalToWorld rows) -> the vertex Primitive's LocalToWorld row
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let c = b[i] as char;
            if c.is_ascii_alphabetic() || c == '_' {
                let s = i;
                while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') { i += 1; }
                let id = &line[s..i];
                if prim.iter().any(|p| p == id) {
                    if b.get(i) != Some(&b'[') { return Err(format!("{id}: not an indexed read: {}", line.trim())); }
                    out.push_str(id); out.push_str("[fx_inst].d");
                    continue;
                }
                if let Some((g, row0)) = prev_l2w.filter(|(g, _)| *g == id) {
                    if b.get(i) != Some(&b'[') { return Err(format!("{g}: not an indexed read: {}", line.trim())); }
                    let close = line[i..].find(']').map(|k| i + k).ok_or("unclosed index")?;
                    let Ok(k) = line[i + 1..close].trim().parse::<u32>() else { return Err(format!("{g}: dynamic index {}", &line[i..=close])) };
                    if (row0..row0 + 4).contains(&k) { let _ = write!(out, "{}[fx_inst].d[{}]", vprim.as_deref().unwrap(), k - row0); i = close + 1; } else { out.push_str(id); }
                    continue;
                }
                out.push_str(id);
            } else {
                // keep multi-byte characters whole (comments)
                let ch = line[i..].chars().next().unwrap();
                out.push(ch); i += ch.len_utf8();
            }
        }
    }
    // the instance id: vertex attribute -> private; to the pixel stages as a flat varying
    let ins = |s: &mut String, at: &str, what: &str| -> Result<(), String> { let p = s.find(at).ok_or_else(|| format!("no `{}`", at.trim()))? + at.len(); s.insert_str(p, what); Ok(()) };
    ins(&mut out, "struct VsIn {\n", &format!("    @location({attribute}) fx_inst_in: u32,\n"))?;
    ins(&mut out, "@vertex fn vs_main(in: VsIn) -> VsOut {\n", "    fx_inst = in.fx_inst_in;\n")?;
    if frag_uses {
        let p = out.find("struct VsOut {\n").ok_or("no VsOut")?;
        let p = p + out[p..].find("pos: vec4<f32>,\n").ok_or("no VsOut position")? + "pos: vec4<f32>,\n".len();
        out.insert_str(p, &format!("    @location({vary}) @interpolate(flat) fx_inst_o: u32,\n"));
        let v = out.find("@vertex fn vs_main(").unwrap();
        let r = v + out[v..].find("    return r;\n").ok_or("no vs_main return")?;
        out.insert_str(r, "    r.fx_inst_o = fx_inst;\n");
        ins(&mut out, "struct FsIn {\n    @builtin(position) frag: vec4<f32>,\n    @builtin(front_facing) front: bool,\n", &format!("    @location({vary}) @interpolate(flat) fx_inst_i: u32,\n"))?;
        let mut s = String::with_capacity(out.len() + 256);
        for line in out.split_inclusive('\n') { s.push_str(line); if line.starts_with("@fragment fn ") && line.contains("(in: FsIn)") { s.push_str("    fx_inst = in.fx_inst_i;\n"); } }
        out = s;
    }
    Ok((out, attribute))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// INSTANCING: the Primitive reads of every stage go to the instance's record, the PreviousLocalToWorld rows to its LocalToWorld, the id flows VS -> PS.
    #[test] fn instance_primitive_rewrites_the_primitive_reads() {
        let wgsl = "diagnostic(off, derivative_uniformity);\n@group(0) @binding(2) var<uniform> vcb1: array<vec4<f32>, 18>;\n@group(0) @binding(0) var<uniform> vcb0: array<vec4<f32>, 12>;\n@group(1) @binding(18) var<uniform> pcb1: array<vec4<f32>, 9>;\nfn vs_fx(v: array<vec4<f32>, 2>) -> array<vec4<f32>, 3> { var o: array<vec4<f32>, 3>; o[0] = vcb1[3] + vcb0[5] + vcb0[8] + vcb1[u32(clamp(i + 2i, 0i, 17i))]; return o; }\nfn ps_fx(v: array<vec4<f32>, 2>) -> array<vec4<f32>, 1> { var o: array<vec4<f32>, 1>; o[0] = pcb1[8]; return o; }\nstruct VsIn {\n    @location(0) a0: vec4<f32>,\n}\nstruct VsOut {\n    @builtin(position) pos: vec4<f32>,\n    @location(1) o1: vec4<f32>,\n}\n@vertex fn vs_main(in: VsIn) -> VsOut {\n    var r: VsOut;\n    return r;\n}\nstruct FsIn {\n    @builtin(position) frag: vec4<f32>,\n    @builtin(front_facing) front: bool,\n    @location(1) i1: vec4<f32>,\n}\n@fragment fn fs_main(in: FsIn) -> @location(0) vec4<f32> {\n    return vec4<f32>(0.0);\n}\n";
        let a = Assembled { wgsl: wgsl.into(), resources: vec![], vs_inputs: vec![VsInput { attribute: 0, register: 0, mask: 15, comp_type: 3 }], varyings: vec![1], targets: 1, ps_uses_position: false };
        let (o, attr) = instance_primitive(&a, &["vcb1".into(), "pcb1".into()], Some(("vcb0", 6))).unwrap();
        assert_eq!(attr, 15);
        assert!(o.starts_with("diagnostic(off, derivative_uniformity);\nstruct FxInstPrim"), "{o}");
        assert!(o.contains("var<storage, read> vcb1: array<FxInstPrim>;") && o.contains("var<storage, read> pcb1: array<FxInstPrim>;") && o.contains("var<uniform> vcb0: array<vec4<f32>, 12>;"));
        assert!(o.contains("vcb1[fx_inst].d[3] + vcb0[5] + vcb1[fx_inst].d[2] + vcb1[fx_inst].d[u32(clamp("), "{o}");
        assert!(o.contains("o[0] = pcb1[fx_inst].d[8];"));
        assert!(o.contains("@location(15) fx_inst_in: u32,") && o.contains("fx_inst = in.fx_inst_in;") && o.contains("@location(2) @interpolate(flat) fx_inst_o: u32,") && o.contains("r.fx_inst_o = fx_inst;\n    return r;"));
        assert!(o.contains("@location(2) @interpolate(flat) fx_inst_i: u32,") && o.contains("vec4<f32> {\n    fx_inst = in.fx_inst_i;\n"));
        // a dynamic index into the $Globals with PreviousLocalToWorld is refused (the rows could be read through it)
        let dynamic = Assembled { wgsl: wgsl.replace("vcb0[8]", "vcb0[u32(i)]"), ..a };
        assert!(instance_primitive(&dynamic, &["vcb1".into()], Some(("vcb0", 6))).is_err());
    }
    #[test] fn renames_whole_identifiers_only() {
        let t = "@group(0) @binding(1) var<uniform> cb1: array<vec4<f32>, 4>;\nlet x = cb1[2].x + t0.x + s10 + qt0 + icb[1].x + t0x;";
        let r = rename_resources(t, "p");
        assert!(r.contains("pcb1: array") && r.contains("pcb1[2].x + pt0.x + ps10 + qt0 + picb[1].x + t0x"), "{r}");
    }
    /// FX RECEIVE: the wood wall's base pass (the `TBasePassPSFNoLightMapPolicy` permutation of a movable sky light, building_render::material) assembles with the
    /// renderer's scene lighting: the fragment entry resolves the GBuffer with `fx_scene_lit` into `LitOut`, the program keeps groups 0..2 (the globals are
    /// group 3), no `FxLight`; the module (translated DXBC + common + lit_common + fx_resolve) passes wgpu / naga validation when a GPU is present.
    #[cfg(feature = "render")]
    #[test] fn piece_material_assembles_with_the_scene_lighting() {
        let set = fn_pak::PakSet::open_install(crate::default_install()).unwrap();
        let m = crate::material::Material::load(&set, "/Game/Packages/PBW/Wood/Materials/Wood_lvl1_Wall.Wood_lvl1_Wall").unwrap();
        assert!(m.flags.uses_gbuffer(), "a lit masked piece material");
        let vs = m.cooked.find("FLocalVertexFactory", "TBasePassVSFNoLightMapPolicy").unwrap();
        let ps = m.cooked.find("FLocalVertexFactory", "TBasePassPSFNoLightMapPolicy").expect("the non-sky-light base pass permutation");
        let clip = if m.flags.blend == "BLEND_Masked" && !ps.shader.body().any(|i| i.op == fn_dxbc::Opcode::Discard) { m.cooked.find("FLocalVertexFactory", "FDepthOnlyPS") } else { None };
        let a = assemble_scene_lit(vs, ps, clip, &fn_render::fx::default_scene_lit_prelude()).unwrap();
        assert!(a.wgsl.contains("fn fs_main(in: FsIn) -> LitOut") && a.wgsl.contains("return fx_scene_lit(in.frag,") && !a.wgsl.contains("fx_light"));
        assert!(a.wgsl.contains("@group(3) @binding(0) var<uniform> camera") && a.resources.iter().all(|r| r.group <= 2));
        let Ok(gpu) = fn_render::gpu::Gpu::new(fn_render::gpu::Gpu::create_instance(), None) else { eprintln!("no GPU adapter: WGSL validation skipped"); return };
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let _ = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("scene-lit wood wall"), source: wgpu::ShaderSource::Wgsl(a.wgsl.as_str().into()) });
        if let Some(e) = pollster::block_on(scope.pop()) { panic!("scene-lit module invalid: {e}"); }
    }
    /// HUD FIX regression (verification/building/hud/fix): the edit tile / preview materials `MGreen` and `MPreviewPulse` are DefaultLit but TRANSLUCENT, so their
    /// base-pass pixel program writes SV_Target0 only (no GBuffer, UE4.16 BasePassCommon.usf:20). The 08:59 edit run logged `fx MGreen FLocalVertexFactory:
    /// Validation Error ... let n_ue = normalize(o[1].xyz ...) Accessing index 1 is out of [21] bounds`: they were assembled with the scene-lit GBuffer tail. They
    /// must take the unlit tail (`MaterialFlags::uses_gbuffer` false -> gpu.rs never asks for the scene prelude), the GBuffer tails must refuse them, and the
    /// unlit module must pass naga validation.
    #[cfg(feature = "render")]
    #[test] fn translucent_lit_edit_materials_take_the_unlit_tail() {
        let Ok(set) = fn_pak::PakSet::open_install(crate::default_install()) else { eprintln!("no 1.8 install: skipped"); return };
        let gpu = fn_render::gpu::Gpu::new(fn_render::gpu::Gpu::create_instance(), None).ok();
        for path in ["/Game/Packages/Fortnite_UniqueMaterials_1/Effects/MGreen.MGreen", "/Game/Packages/Fortnite_UniqueMaterials_1/Effects/MPreviewPulse.MPreviewPulse"] {
            let m = crate::material::Material::load(&set, path).unwrap();
            assert!(!m.flags.uses_gbuffer(), "{path}: translucent, no GBuffer ({} / {})", m.flags.blend, m.flags.shading);
            let vs = m.cooked.find("FLocalVertexFactory", "TBasePassVSFNoLightMapPolicy").unwrap();
            let ps = m.cooked.find("FLocalVertexFactory", "TBasePassPSFNoLightMapPolicySkylight").or_else(|| m.cooked.find("FLocalVertexFactory", "TBasePassPSFNoLightMapPolicy")).unwrap();
            assert!(assemble_scene_lit(vs, ps, None, &fn_render::fx::default_scene_lit_prelude()).is_err(), "{path}: the scene-lit tail must refuse a single-target program");
            assert!(assemble(vs, ps, true, None).is_err(), "{path}: so must the simple lit tail");
            let a = assemble(vs, ps, false, None).unwrap();
            assert!(a.wgsl.trim_end().ends_with("return o[0];\n}") && !a.wgsl.contains("let n_ue"), "{path}: unlit tail");
            let Some(gpu) = gpu.as_ref() else { continue };
            let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let _ = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(path), source: wgpu::ShaderSource::Wgsl(a.wgsl.as_str().into()) });
            if let Some(e) = pollster::block_on(scope.pop()) { panic!("{path}: module invalid: {e}"); }
        }
    }
    /// GRASS + PREPASS (2026-10-06): the foliage grass (`M_FieldGrassMatching`, FInstancedStaticMeshVertexFactory) assembles scene-lit with UE's masked pre-pass
    /// split: `fs_depth` runs only the FDepthOnlyPS clip (its dithered-LOD hash reads SV_Position, now handed over), `fs_main_eq` shades without the clip, the
    /// position is `@invariant`, the VS reads the instance stream at ATTRIBUTE8..12; the module passes wgpu / naga validation when a GPU is present.
    #[cfg(feature = "render")]
    #[test] fn instanced_grass_assembles_with_the_prepass_split() {
        let Ok(set) = fn_pak::PakSet::open_install(crate::default_install()) else { eprintln!("no 1.8 install: skipped"); return };
        let m = crate::material::Material::load(&set, "/Game/Environments/Foliage/Materials/M_FieldGrassMatching.M_FieldGrassMatching").unwrap();
        let vf = "FInstancedStaticMeshVertexFactory";
        let (vs, ps, clip) = (m.cooked.find(vf, "TBasePassVSFNoLightMapPolicy").unwrap(), m.cooked.find(vf, "TBasePassPSFNoLightMapPolicy").unwrap(), m.cooked.find(vf, "FDepthOnlyPS").unwrap());
        assert!(!ps.shader.body().any(|i| i.op == fn_dxbc::Opcode::Discard), "the base pass does not clip (r.EarlyZPassOnlyMaterialMasking)");
        let a = assemble_scene_lit(vs, ps, Some(clip), &fn_render::fx::default_scene_lit_prelude()).unwrap();
        for e in ["fn fs_main(in: FsIn) -> LitOut", "fn fs_main_eq(in: FsIn) -> LitOut", "fn fs_depth(in: FsIn) {", "@builtin(position) @invariant pos"] { assert!(a.wgsl.contains(e), "missing {e}"); }
        let depth = &a.wgsl[a.wgsl.find("fn fs_depth(").unwrap()..a.wgsl.find("fn fs_velocity(").unwrap_or(a.wgsl.len())];
        // VELOCITY (2026-10-07): the WPO grass writes its base-pass velocity (SV_Target4) through `fs_velocity` (clip, then the program's target 4)
        let vel = &a.wgsl[a.wgsl.find("fn fs_velocity(in: FsIn) -> @location(0) vec4<f32>").expect("fs_velocity")..];
        assert!(vel.contains("dps_fx(vd)") && vel.contains("ps_fx(v)") && vel.contains("o[4].xy"), "{vel}");
        assert!(depth.contains("= vec4<f32>(in.frag.x, in.frag.y, in.frag.z, 1.0 / in.frag.w);") && depth.contains("dps_fx(vd)") && !depth.contains("ps_fx(v)"), "{depth}");
        let eq = &a.wgsl[a.wgsl.find("fn fs_main_eq(").unwrap()..a.wgsl.find("fn fs_depth(").unwrap()];
        assert!(!eq.contains("dps_fx") && eq.contains("ps_fx(v)"));
        let attrs: Vec<u32> = a.vs_inputs.iter().map(|v| v.attribute).collect();
        assert_eq!(attrs, vec![0, 1, 2, 3, 4, 5, 8, 9, 10, 11], "ATTRIBUTE12 (lightmap / shadow-map bias) is in the signature but not read with dynamic lighting");
        let Ok(gpu) = fn_render::gpu::Gpu::new(fn_render::gpu::Gpu::create_instance(), None) else { eprintln!("no GPU adapter: WGSL validation skipped"); return };
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let _ = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("scene-lit grass"), source: wgpu::ShaderSource::Wgsl(a.wgsl.as_str().into()) });
        if let Some(e) = pollster::block_on(scope.pop()) { panic!("scene-lit grass module invalid: {e}"); }
    }
    #[test] fn rebinding_collects_declarations() {
        let t = "@group(0) @binding(3) var<uniform> pcb3: array<vec4<f32>, 10>;\n@group(1) @binding(2) var pt2: texture_3d<f32>;\n@group(2) @binding(1) var ps1: sampler;\nfn x() {}\n";
        let mut res = vec![];
        let o = rebind(t, Stage::Pixel, 16, &mut res).unwrap();
        assert!(o.contains("@binding(19) var<uniform> pcb3") && o.contains("@binding(18) var pt2") && o.contains("@binding(17) var ps1"), "{o}");
        assert_eq!(res.iter().map(|r| (r.kind, r.slot, r.binding, r.size)).collect::<Vec<_>>(), vec![(ResKind::Uniform, 3, 19, 10), (ResKind::Tex3D, 2, 18, 0), (ResKind::Sampler, 1, 17, 0)]);
    }
}







