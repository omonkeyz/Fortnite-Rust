//! BR character materials (head / hair / body skin, eyeshadow) on the GPU.
//!
//! The materials are NOT shaded by an approximation: the original compiled base-pass pixel shaders (translated from the cooked DXBC by `fn-dxbc`, generated into
//! `shaders/char_material_gen.wgsl` / `shaders/eyeshadow_gen.wgsl`) are evaluated per pixel. The CPU side hands over, per primitive, what the shader reads from its
//! material uniform buffer (the cooked uniform expression set evaluated with the material instance parameters; see `fn_pose::br_material`) and the fixed textures that are
//! not material parameters. The four material-parameter textures (`Diffuse`, `Normals`, `SpecularMasks`, `M`) are the primitive's [`crate::MaterialImages`] as before.
//!
//! * `UeMaterial::Char`: drawn in the scene pass (`char_scene.wgsl`): translated PS -> G-buffer values -> sun + sky ambient + the shader's own emissive output.
//!   CHAR PREPASS (2026-10-06): the three materials are `BLEND_Masked` and the cooked `r.EarlyZPassOnlyMaterialMasking=True` (+ `r.EarlyZPass=2`,
//!   `r.EarlyZPassMovable=True`) puts their coverage clip in the depth pre-pass only: the pre-pass runs the material's own depth-only pixel shader
//!   (`FDepthOnlyPS`, `shaders/char_depth_gen.wgsl`, [`CharGpu::char_depth_pipeline`]) and the base pass draws with depth Equal and no depth write
//!   ([`CharGpu::char_pipeline`]; BasePassRendering.cpp:308-361), so every visible pixel is shaded once.
//! * `UeMaterial::Eyeshadow`: the plain `BLEND_Modulate` unlit material of the eye region, drawn after the scene (`char_overlay.wgsl`) with `DestColor, Zero` blending; its
//!   per-vertex colour (R = shadow mask) comes with the primitive.
use image::RgbaImage;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharPerm { Head = 0, Hair = 1, Body = 2 }

/// Sampler addressing of a fixed texture (the texture asset's own `AddressX/Y`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addr { Wrap, Clamp, Mirror }
impl Addr { fn wgpu(self) -> wgpu::AddressMode { match self { Addr::Wrap => wgpu::AddressMode::Repeat, Addr::Clamp => wgpu::AddressMode::ClampToEdge, Addr::Mirror => wgpu::AddressMode::MirrorRepeat } } }

/// A texture that is not a material parameter (fixed in the material graph). `srgb`: hardware sRGB decode on sampling (the texture's SRGB flag, unless the
/// sampler type of the graph node is LinearColor).
#[derive(Clone, Debug)]
/// TEXTURE MEMORY (2026-10-06): `cooked` = the stored mip chain, uploaded instead of `image` and the mips generated from it (`crate::mesh::CookedTexture`).
pub struct FixedTexture { pub image: RgbaImage, pub srgb: bool, pub address: [Addr; 2], pub cooked: Option<std::sync::Arc<crate::mesh::CookedTexture>> }

/// Constants of one character material permutation.
#[derive(Clone, Debug)]
pub struct CharMaterial {
    pub perm: CharPerm,
    /// Registers of the material uniform buffer (`cb3` of the pixel shader, at most 32).
    pub cb: Vec<[f32; 4]>,
    /// Fixed texture slot 7 (`/Game/UI/UI_Faces/Noise`, the super-charged flicker noise).
    pub noise: FixedTexture,
    /// CHAR PREPASS (2026-10-06): fixed texture slots 8, 9, 10 of the depth-only coverage shader (CharacterResInEffect: T_NoisePack_01, T_Slowfield_GridSquares_01,
    /// ShieldPowerAnim; `fn_pose::br_material`).
    pub res_in: [FixedTexture; 3],
    pub tangents: Vec<[f32; 4]>,
}

/// The eye-region modulate overlay.
#[derive(Clone, Debug)]
pub struct EyeshadowOverlay {
    /// Registers of the material uniform buffer (`cb1` of the pixel shader, 10).
    pub cb: Vec<[f32; 4]>,
    /// Vertex colour (R, G, B, A bytes) of every vertex of the primitive.
    pub colors: Vec<[u8; 4]>,
    /// Fixed texture slots by texture index: 0 `T_NoisePack_01`, 1 `T_Slowfield_GridSquares_01`, 2 `ShieldPowerAnim`.
    pub tex0: FixedTexture,
    pub tex1: FixedTexture,
    pub tex2: FixedTexture,
}

#[derive(Clone, Debug)]
pub enum UeMaterial { Char(CharMaterial), Eyeshadow(EyeshadowOverlay) }

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CharMatUniform { cb: [[f32; 4]; 32], meta: [u32; 4] }

pub struct CharGpu {
    pub char_bgl: wgpu::BindGroupLayout,
    pub char_pipeline: wgpu::RenderPipeline,
    /// CHAR PREPASS (2026-10-06): the depth pre-pass draw of a character material primitive (its `FDepthOnlyPS`); `char_pipeline` then draws with depth Equal.
    pub char_depth_pipeline: wgpu::RenderPipeline,
    pub char_sss_pipeline: wgpu::RenderPipeline,
    pub overlay_bgl: wgpu::BindGroupLayout,
    pub overlay_pipeline: wgpu::RenderPipeline,
}

#[derive(Clone)]
pub struct GpuOverlay { pub bg: wgpu::BindGroup, pub color_vb: wgpu::Buffer }

fn entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry { wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty, count: None } }
fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry { entry(binding, wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }) }
fn samp_entry(binding: u32) -> wgpu::BindGroupLayoutEntry { entry(binding, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)) }
fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry { entry(binding, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }) }

fn upload_fixed(device: &wgpu::Device, queue: &wgpu::Queue, t: &FixedTexture, label: &str) -> (wgpu::TextureView, wgpu::Sampler) {
    let tex = match &t.cooked { Some(c) => c.upload(device, queue, t.srgb, label), None => crate::texture::upload_rgba8(device, queue, &t.image, t.srgb, crate::texture::MipFilter::Triangle, label) };
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label), address_mode_u: t.address[0].wgpu(), address_mode_v: t.address[1].wgpu(), address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, mipmap_filter: wgpu::MipmapFilterMode::Linear, anisotropy_clamp: 8, ..Default::default()
    });
    (tex.create_view(&Default::default()), sampler)
}

const SKINNED_ATTRS: [wgpu::VertexAttribute; 5] = [
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 2 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint16x4, offset: 32, shader_location: 3 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 40, shader_location: 4 },
];
const TANGENT_ATTR: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 6 }];
const COLOR_ATTR: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute { format: wgpu::VertexFormat::Unorm8x4, offset: 0, shader_location: 5 }];

/// One constant-buffer array of a generated (translated) program: its name, the slots the wrapper writes per pixel (`char_scene.wgsl` / `char_overlay.wgsl`),
/// and the uniform the wrapper copies the whole array from (`None` = only the written slots; every other slot is never written: zero).
pub(crate) struct CbFold { pub name: String, pub live: Vec<u32>, pub uniform: Option<String> }

/// PERF (2026-10-06): the generated programs keep each constant buffer as a `var<private>` array (`head_cb1: array<vec4<f32>, 175>` and so on, about 650 vec4s
/// per invocation over head / hair / body) that the wrapper fills per pixel: a few View slots, and the material registers copied from the uniform. Every index
/// in the programs is a literal (checked here: a non-literal index panics), so the arrays fold away: a written slot becomes a private vec4 `<name>_<k>`, a
/// copied array reads its uniform directly (`<uniform>[k]`), any other slot is the zero it always held. Every expression sees the same values: the image
/// is identical, the compiler no longer has to keep the arrays in local memory.
pub(crate) fn fold_private_cbs(src: &str, folds: &[CbFold]) -> String {
    let mut out = src.to_owned();
    for f in folds {
        // the declaration -> the written slots as private vec4s
        let decl_prefix = format!("var<private> {}: array<vec4<f32>, ", f.name);
        let Some(start) = out.find(&decl_prefix) else { panic!("fold_private_cbs: no declaration of {}", f.name) };
        let end = start + out[start..].find(";\n").expect("declaration end") + 2;
        let decls: String = f.live.iter().map(|k| format!("var<private> {}_{k}: vec4<f32>;\n", f.name)).collect();
        out.replace_range(start..end, &decls);
        // every `<name>[<literal>]`
        let pat = format!("{}[", f.name);
        let mut res = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(&pat) {
            let before_ok = i == 0 || !rest[..i].chars().last().map_or(false, |c| c.is_ascii_alphanumeric() || c == '_');
            res.push_str(&rest[..i]);
            let after = &rest[i + pat.len()..];
            if !before_ok { res.push_str(&pat); rest = after; continue; }
            let close = after.find(']').expect("index end");
            let k: u32 = after[..close].trim().parse().unwrap_or_else(|_| panic!("fold_private_cbs: non-literal index {}[{}]", f.name, &after[..close]));
            if f.live.contains(&k) { res.push_str(&format!("{}_{k}", f.name)); }
            else if let Some(u) = &f.uniform { res.push_str(&format!("{u}[{k}]")); }
            else { res.push_str("vec4<f32>(0.0)"); }
            rest = &after[close + 1..];
        }
        res.push_str(rest);
        out = res;
    }
    out
}

/// The folds of `char_material_gen.wgsl` (base pass: View slots 43 SvPositionToTranslatedWorld row, 62 PreViewTranslation, 130 CullingSign (body), 131 time,
/// pass slot 3, primitive slot 9 (body), material = `cmat.cb`), `char_depth_gen.wgsl` (View 43 / 62, material = `cmat.cb`) and `eyeshadow_gen.wgsl`
/// (View 62, material = `emat.cb1`), matching what `char_scene.wgsl` / `char_overlay.wgsl` write.
pub(crate) fn char_folds() -> (Vec<CbFold>, Vec<CbFold>, Vec<CbFold>) {
    let fold = |name: String, live: &[u32], uniform: Option<&str>| CbFold { name, live: live.to_vec(), uniform: uniform.map(str::to_owned) };
    let mut base = vec![];
    let mut depth = vec![];
    for p in ["head", "hair", "body"] {
        let body = p == "body";
        base.push(fold(format!("{p}_cb0"), &[3], None));
        base.push(fold(format!("{p}_cb1"), if body { &[43, 62, 130, 131] } else { &[43, 62, 131] }, None));
        base.push(fold(format!("{p}_cb2"), if body { &[9] } else { &[] }, None));
        base.push(fold(format!("{p}_cb3"), &[], Some("cmat.cb")));
        depth.push(fold(format!("{p}_dcb0"), &[43, 62], None));
        depth.push(fold(format!("{p}_dcb1"), &[], Some("cmat.cb")));
    }
    let eye = vec![fold("eyeshadow_cb0".into(), &[62], None), fold("eyeshadow_cb1".into(), &[], Some("emat.cb1"))];
    (base, depth, eye)
}

fn module(device: &wgpu::Device, label: &str, parts: &[&str]) -> wgpu::ShaderModule {
    // the translated shaders start with `diagnostic(off, derivative_uniformity);`, which must precede every declaration of the module
    let body = parts.join("\n").replace("diagnostic(off, derivative_uniformity);", "");
    device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(format!("diagnostic(off, derivative_uniformity);\n{body}").into()) })
}

impl CharGpu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(device: &wgpu::Device, common: &str, lit_common: &str, bgl_globals: &wgpu::BindGroupLayout, bgl_material: &wgpu::BindGroupLayout, bgl_obj_skinned: &wgpu::BindGroupLayout, hdr: wgpu::TextureFormat, depth: wgpu::TextureFormat) -> CharGpu {
        let char_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("char material bgl"), entries: &[uniform_entry(0), tex_entry(1), samp_entry(2), tex_entry(3), samp_entry(4), tex_entry(5), samp_entry(6), tex_entry(7), samp_entry(8)] }); // CHAR PREPASS: + fixed textures 8, 9, 10 (depth-only PS)
        let overlay_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("eyeshadow bgl"), entries: &[uniform_entry(0), tex_entry(1), samp_entry(2), tex_entry(3), samp_entry(4), tex_entry(5), samp_entry(6)] });
        // SHADING MODELS (2026-10-05): the GBuffer resolve per ShadingModelID shared with the scene-lit FX programs (fx_resolve.wgsl + shading_models.wgsl +
        // fx_resolve_sm.wgsl `sm_lit_world`)
        // PERF (2026-10-06): the generated programs' private constant-buffer arrays folded away (fold_private_cbs, same values)
        let (base_folds, depth_folds, eye_folds) = char_folds();
        let char_gen = fold_private_cbs(fn_live::wgsl!("../shaders/char_material_gen.wgsl"), &base_folds);
        let depth_gen = fold_private_cbs(fn_live::wgsl!("../shaders/char_depth_gen.wgsl"), &depth_folds);
        let eye_gen = fold_private_cbs(fn_live::wgsl!("../shaders/eyeshadow_gen.wgsl"), &eye_folds);
        let char_sm = module(device, "char scene", &[common, lit_common, fn_live::wgsl!("../shaders/fx_resolve.wgsl"), fn_live::wgsl!("../shaders/shading_models.wgsl"), fn_live::wgsl!("../shaders/fx_resolve_sm.wgsl"), &char_gen, &depth_gen, fn_live::wgsl!("../shaders/char_scene.wgsl")]);
        let overlay_sm = module(device, "char overlay", &[common, lit_common, &eye_gen, fn_live::wgsl!("../shaders/char_overlay.wgsl")]);
        let layout = |label: &str, bgls: &[&wgpu::BindGroupLayout]| {
            let entries: Vec<Option<&wgpu::BindGroupLayout>> = bgls.iter().map(|b| Some(*b)).collect();
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &entries, immediate_size: 0 })
        };
        let char_layout = layout("char scene layout", &[bgl_globals, bgl_material, bgl_obj_skinned, &char_bgl]);
        let stride = std::mem::size_of::<crate::mesh::SkinnedVertex>() as u64;
        let vbufs = [Some(wgpu::VertexBufferLayout { array_stride: stride, step_mode: wgpu::VertexStepMode::Vertex, attributes: &SKINNED_ATTRS }), Some(wgpu::VertexBufferLayout { array_stride: 16, step_mode: wgpu::VertexStepMode::Vertex, attributes: &TANGENT_ATTR })];
        let vertex = || wgpu::VertexState { module: &char_sm, entry_point: Some("vs_char"), compilation_options: Default::default(), buffers: &vbufs };
        // CHAR PREPASS (2026-10-06): BLEND_Masked base pass after the pre-pass: depth Equal, no depth write, no clip (the translated base-pass PS has none)
        let char_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("char scene"), layout: Some(&char_layout), vertex: vertex(),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::Equal), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &char_sm, entry_point: Some("fs_char"), compilation_options: Default::default(), targets: &crate::reflection::lit_targets(hdr) }), // REFLECTION APPLY: + reflection inputs
            multiview_mask: None, cache: None,
        });
        let sss_target = Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::Rgba16Float, blend: None, write_mask: wgpu::ColorWrites::ALL });
        let char_sss_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("char subsurface inputs"), layout: Some(&char_layout), vertex: vertex(),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::Equal), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &char_sm, entry_point: Some("fs_char_sss"), compilation_options: Default::default(), targets: &[sss_target.clone(), sss_target.clone(), sss_target.clone(), sss_target] }),
            multiview_mask: None, cache: None,
        });
        // CHAR PREPASS (2026-10-06): the depth pre-pass draw (FDepthOnlyPS: the material's coverage clip; CF_DepthNearOrEqual like the other pre-pass draws)
        let char_depth_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("char depth"), layout: Some(&char_layout), vertex: vertex(),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &char_sm, entry_point: Some("fs_char_depth"), compilation_options: Default::default(), targets: &[] }),
            multiview_mask: None, cache: None,
        });
        // overlay: globals, object (skinned), eyeshadow resources
        let overlay_layout = layout("eyeshadow layout", &[bgl_globals, bgl_obj_skinned, &overlay_bgl]);
        let modulate = wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Dst, dst_factor: wgpu::BlendFactor::Zero, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
        };
        let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("eyeshadow modulate"), layout: Some(&overlay_layout),
            vertex: wgpu::VertexState { module: &overlay_sm, entry_point: Some("vs_overlay"), compilation_options: Default::default(), buffers: &[
                Some(wgpu::VertexBufferLayout { array_stride: stride, step_mode: wgpu::VertexStepMode::Vertex, attributes: &SKINNED_ATTRS }),
                Some(wgpu::VertexBufferLayout { array_stride: 4, step_mode: wgpu::VertexStepMode::Vertex, attributes: &COLOR_ATTR }),
            ] },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &overlay_sm, entry_point: Some("fs_overlay"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: hdr, blend: Some(modulate), write_mask: wgpu::ColorWrites::ALL })] }),
            multiview_mask: None, cache: None,
        });
        CharGpu { char_bgl, char_pipeline, char_depth_pipeline, char_sss_pipeline, overlay_bgl, overlay_pipeline }
    }

    /// Bind group 3 of a character material primitive.
    pub fn char_bind_group(&self, device: &wgpu::Device, queue: &wgpu::Queue, m: &CharMaterial, label: &str) -> wgpu::BindGroup { char_bind_group_with(&self.char_bgl, device, queue, m, label) }

    /// Bind group 2 + the colour stream of an eyeshadow overlay primitive.
    pub fn overlay(&self, device: &wgpu::Device, queue: &wgpu::Queue, e: &EyeshadowOverlay, label: &str) -> GpuOverlay { overlay_with(&self.overlay_bgl, device, queue, e, label) }
}

/// [`CharGpu::char_bind_group`] from the layout alone (a `ModelBuilder` on another thread holds clones of the layouts, not the `CharGpu`).
pub fn char_bind_group_with(layout: &wgpu::BindGroupLayout, device: &wgpu::Device, queue: &wgpu::Queue, m: &CharMaterial, label: &str) -> wgpu::BindGroup { char_binding_with(layout, device, queue, m, label).0 }

pub struct CharSrc { pub perm: CharPerm, pub base: Vec<[f32; 4]>, views: [wgpu::TextureView; 4], samplers: [wgpu::Sampler; 4] }

fn char_uniform(perm: CharPerm, cb: &[[f32; 4]]) -> CharMatUniform {
    let mut u = CharMatUniform { cb: [[0.0; 4]; 32], meta: [perm as u32, 0, 0, 0] };
    for (i, r) in cb.iter().take(32).enumerate() { u.cb[i] = *r; }
    u
}

pub fn char_binding_with(layout: &wgpu::BindGroupLayout, device: &wgpu::Device, queue: &wgpu::Queue, m: &CharMaterial, label: &str) -> (wgpu::BindGroup, std::sync::Arc<CharSrc>, wgpu::Buffer) {
    let (view, sampler) = upload_fixed(device, queue, &m.noise, label);
    let [(v8, s8), (v9, s9), (v10, s10)] = [0, 1, 2].map(|i| upload_fixed(device, queue, &m.res_in[i], label));
    let src = CharSrc { perm: m.perm, base: m.cb.clone(), views: [view, v8, v9, v10], samplers: [sampler, s8, s9, s10] };
    let buf = crate::texture::buffer_init(device, &[char_uniform(m.perm, &m.cb)], wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, label);
    (char_bind_group_from(layout, device, &src, &buf, label), std::sync::Arc::new(src), buf)
}

pub fn char_instance_buffer(device: &wgpu::Device, src: &CharSrc, label: &str) -> wgpu::Buffer { crate::texture::buffer_init(device, &[char_uniform(src.perm, &src.base)], wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, label) }

pub fn char_write(queue: &wgpu::Queue, buf: &wgpu::Buffer, perm: CharPerm, cb: &[[f32; 4]]) { queue.write_buffer(buf, 0, bytemuck::bytes_of(&char_uniform(perm, cb))); }

pub fn char_bind_group_from(layout: &wgpu::BindGroupLayout, device: &wgpu::Device, src: &CharSrc, buf: &wgpu::Buffer, label: &str) -> wgpu::BindGroup {
    let [view, v8, v9, v10] = &src.views;
    let [sampler, s8, s9, s10] = &src.samplers;
    device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(label), layout, entries: &[
        wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(v8) },
        wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(s8) },
        wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(v9) },
        wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(s9) },
        wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(v10) },
        wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::Sampler(s10) },
    ] })
}

pub fn char_tangent_buffer(device: &wgpu::Device, m: &CharMaterial, vertex_count: usize, label: &str) -> wgpu::Buffer {
    let t: Vec<[f32; 4]> = if m.tangents.len() == vertex_count { m.tangents.clone() } else { vec![[0.0; 4]; vertex_count.max(1)] };
    crate::texture::buffer_init(device, &t, wgpu::BufferUsages::VERTEX, label)
}

/// [`CharGpu::overlay`] from the layout alone.
pub fn overlay_with(layout: &wgpu::BindGroupLayout, device: &wgpu::Device, queue: &wgpu::Queue, e: &EyeshadowOverlay, label: &str) -> GpuOverlay {
    {
        let mut cb = [[0.0f32; 4]; 10];
        for (i, r) in e.cb.iter().take(10).enumerate() { cb[i] = *r; }
        let buf = crate::texture::buffer_init(device, &cb, wgpu::BufferUsages::UNIFORM, label);
        let color_vb = crate::texture::buffer_init(device, &e.colors, wgpu::BufferUsages::VERTEX, label);
        let (v0, s0) = upload_fixed(device, queue, &e.tex0, label);
        let (v1, s1) = upload_fixed(device, queue, &e.tex1, label);
        let (v2, s2) = upload_fixed(device, queue, &e.tex2, label);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(label), layout, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&v1) }, wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&s1) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&v0) }, wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&s0) },
            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&v2) }, wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&s2) },
        ] });
        GpuOverlay { bg, color_vb }
    }
}

/// Shared handle stored in `MaterialImages::ue`.
pub type UeMaterialRef = Arc<UeMaterial>;

#[cfg(test)]
mod tests {
    use super::*;
    /// PERF (2026-10-06): the constant-buffer fold leaves no array access behind, keeps exactly the slots the wrappers write, and maps the rest to the
    /// material uniform or zero (the values the arrays held).
    #[test]
    fn private_cb_fold_is_complete() {
        let (base, depth, eye) = char_folds();
        for (src, folds) in [(fn_live::wgsl!("../shaders/char_material_gen.wgsl"), &base), (fn_live::wgsl!("../shaders/char_depth_gen.wgsl"), &depth), (fn_live::wgsl!("../shaders/eyeshadow_gen.wgsl"), &eye)] {
            let out = fold_private_cbs(src, folds);
            for f in folds.iter() {
                assert!(!out.contains(&format!("{}[", f.name)), "{} still indexed", f.name);
                assert!(!out.contains(&format!("var<private> {}: array", f.name)), "{} still declared as an array", f.name);
                for k in &f.live { assert!(out.contains(&format!("var<private> {}_{k}: vec4<f32>;", f.name)), "{}_{k} not declared", f.name); }
            }
        }
        let small = "var<private> a_cb1: array<vec4<f32>, 4>;\nfn f() -> f32 { return a_cb1[2].x + a_cb1[ 1 ].y + xa_cb1[3].z + a_cb3[0].w; }\nvar<private> a_cb3: array<vec4<f32>, 2>;\n";
        let out = fold_private_cbs(small, &[CbFold { name: "a_cb1".into(), live: vec![2], uniform: None }, CbFold { name: "a_cb3".into(), live: vec![], uniform: Some("u.cb".into()) }]);
        assert!(out.contains("return a_cb1_2.x + vec4<f32>(0.0).y + xa_cb1[3].z + u.cb[0].w;"), "{out}");
        assert!(out.starts_with("var<private> a_cb1_2: vec4<f32>;\n"), "{out}");
    }
    /// Headless GPU: creates the character scene + eyeshadow overlay pipelines exactly as the renderer does (shader validation included;
    /// the device's uncaptured-error handler panics on any wgpu / naga error, e.g. a duplicated `@builtin(position)`).
    #[test]
    fn char_pipelines_create() {
        let gpu = match crate::gpu::Gpu::headless() { Ok(g) => g, Err(e) => { eprintln!("skipped: {e}"); return; } };
        let d = &gpu.device;
        let vf = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let ent = |binding: u32, vis: wgpu::ShaderStages, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty, count: None };
        let uni = wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None };
        let tex = |dim| wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: dim, multisampled: false };
        let f = wgpu::ShaderStages::FRAGMENT;
        let mut ge = vec![
            ent(0, vf, uni), ent(1, vf, uni),
            ent(2, f, wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2Array, multisampled: false }),
            ent(3, f, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison)), ent(4, f, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ent(15, f, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering)), // PERF: the shadow gather sampler (renderer globals binding 15)
        ];
        ge.extend(crate::distance_field::globals_layout_entries()); // DISTANCE FIELD HOOK: the renderer's globals carry bindings 5..10
        ge.extend(crate::sky_reflection::globals_layout_entries()); // SKY REFLECTION HOOK: bindings 11..14
        ge.extend(crate::light_function::globals_layout_entries()); // LIGHT FUNCTION: bindings 20..22
        ge.extend(crate::local_lights::globals_layout_entries());
        let globals = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("globals"), entries: &ge });
        let d2 = wgpu::TextureViewDimension::D2;
        let material = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("material"), entries: &[ent(0, f, uni), ent(1, f, tex(d2)), ent(2, f, tex(d2)), ent(3, f, tex(d2)), ent(4, f, tex(d2))] });
        let v = wgpu::ShaderStages::VERTEX;
        let skinned = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("skinned"), entries: &[
            ent(0, v, uni), ent(1, v, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }),
        ] });
        let _ = CharGpu::new(d, fn_live::wgsl!("../shaders/common.wgsl"), fn_live::wgsl!("../shaders/lit_common.wgsl"), &globals, &material, &skinned, crate::renderer::HDR_FORMAT, crate::renderer::DEPTH_FORMAT);
    }
}
