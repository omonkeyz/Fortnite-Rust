//! The gun pass: draws the held firearms with their ORIGINAL material, i.e. the base-pass vertex / pixel programs of the cooked `WeaponMat_Base` shader map
//! translated from DXBC to WGSL by `fn-dxbc` and assembled by `fn-matshader`. The renderer knows nothing about Unreal here either: it owns GPU textures / samplers /
//! uniform buffers / meshes / pipelines created from plain descriptions and draws the [`GunFrame`] the application submits for the next frame with
//! `Renderer::set_gun_frame`, inside the SCENE pass (colour = the HDR target, depth = the scene depth), so the original GBuffer outputs are resolved with the scene's
//! own sun / cascade shadows / sky / fog (`shaders/gun_resolve.wgsl`, appended after `common.wgsl` + `lit_common.wgsl`; group 0 is the renderer's globals bind group).
//!
//! Bind group 1 of every gun pipeline holds all the program resources (uniform buffers, 2D textures, samplers); the layout comes from the pipeline description.
//! GUN SHADOW: a pipeline with `shadow_vs_entry` also gets a depth-only twin that the renderer records into every CSM cascade pass ([`GunRenderer::draw_shadow`]).
//! Skeletal meshes have no mesh distance field in UE 4.16 (only `FStaticMeshSceneProxy` sets bSupportsDistanceFieldRepresentation, StaticMeshRender.cpp:142), so the guns cast nothing
//! into the ray-traced DF cascade, like the characters; they receive it (gun_resolve.wgsl `sun_visibility`).
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct GunTexId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct GunBufId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct GunMeshId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct GunPipeId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct GunBindId(pub usize);

/// A 2D texture with the mip chain the package stores (nothing is generated): `(width, height, rgba8)` per level.
#[derive(Clone, Debug)]
pub struct GunTextureDesc { pub label: String, pub srgb: bool, pub levels: Vec<(u32, u32, Vec<u8>)> }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum GunFilter { Nearest, Linear }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum GunAddress { Wrap, Clamp, Mirror }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GunSamplerDesc { pub mag: GunFilter, pub min: GunFilter, pub mip: GunFilter, pub address: GunAddress, pub aniso: u16 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum GunBindTy { Uniform, Tex2D, Sampler }
#[derive(Clone, Copy, Debug)] pub struct GunLayoutEntry { pub binding: u32, pub ty: GunBindTy, pub vertex: bool, pub fragment: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GunFmt { Float32x2, Float32x3, Float32x4, Unorm8x4, Unorm8x4Bgra, Uint8x4 }
impl GunFmt { fn wgpu(self) -> wgpu::VertexFormat { match self { GunFmt::Float32x2 => wgpu::VertexFormat::Float32x2, GunFmt::Float32x3 => wgpu::VertexFormat::Float32x3, GunFmt::Float32x4 => wgpu::VertexFormat::Float32x4, GunFmt::Unorm8x4 => wgpu::VertexFormat::Unorm8x4, GunFmt::Unorm8x4Bgra => wgpu::VertexFormat::Unorm8x4Bgra, GunFmt::Uint8x4 => wgpu::VertexFormat::Uint8x4 } } }
#[derive(Clone, Copy, Debug)] pub struct GunAttr { pub location: u32, pub offset: u32, pub format: GunFmt }

/// Face culling of the draw: UE `CM_CW` for one-sided materials (`BackCw`) or none (two-sided). `CM_CW` CULLS clockwise triangles: UE4.16 D3D11State.cpp:41
/// `CM_CW -> D3D11_CULL_BACK` with `FrontCounterClockwise = true` (:200), so front faces are counter-clockwise on screen = `wgpu::FrontFace::Ccw` (`CM_CCW`,
/// `BackCcw`, culls the counter-clockwise ones). Checked against the data: the cooked LOD-0 triangles of SK_M_Med_Soldier_04 / SK_M_MED_Head_01_ATH / SK_Pick
/// wind clockwise about their TangentZ in UE space (fn-pose `skinned_vertex_data_winds_ccw_about_the_normal`), which a left-handed view shows counter-clockwise;
/// and in game the held SK_Pick only shows its near-side bolt with `Ccw` (verification/smallfix/followups/held_after_orig vs held_after_orig_cw_before_cullfix: with `Cw` the pass
/// drew the far faces). docs/spec/smallfix_followups.md follow-up 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum GunCull { None, BackCw, BackCcw }

#[derive(Clone, Debug)]
pub struct GunPipelineDesc {
    pub label: String,
    /// Module text WITHOUT the renderer's common / lighting code (prepended here): translated programs + `vs_main` / `fs_main`, resources in `@group(1)`.
    pub wgsl: String,
    pub vs_entry: String,
    pub fs_entry: String,
    pub vertex_stride: u32,
    pub vertex_attrs: Vec<GunAttr>,
    pub layout: Vec<GunLayoutEntry>,
    pub cull: GunCull,
    /// GUN SHADOW: vertex entry of the depth-only program drawn into the sun's cascades (`None` = the material casts no shadow). It must write the
    /// cascade clip position (gun_resolve.wgsl `gun_shadow_depth_outputs`); group 2 is the cascade's LightViewProj.
    pub shadow_vs_entry: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum GunRes { Buffer(GunBufId), Texture(GunTexId), Sampler(GunSamplerDesc) }
#[derive(Clone, Copy, Debug)] pub struct GunBindEntry { pub binding: u32, pub res: GunRes }

/// One indexed draw (a mesh section) of the frame.
#[derive(Clone, Debug)]
pub struct GunDraw { pub pipe: GunPipeId, pub bind: GunBindId, pub mesh: GunMeshId, pub first_index: u32, pub index_count: u32, /** GUN SHADOW: bind group 1 of the depth pass (the shadow View buffer); `None` = no shadow */ pub shadow_bind: Option<GunBindId> }
/// Everything the gun pass does in one frame.
#[derive(Clone, Debug, Default)]
pub struct GunFrame {
    /// Uniform buffer contents written before the pass (whole buffer from offset 0).
    pub buffer_writes: Vec<(GunBufId, Vec<u8>)>,
    pub draws: Vec<GunDraw>,
}

struct GpuTex { view: wgpu::TextureView }
struct GpuMesh { vb: wgpu::Buffer, ib: wgpu::Buffer }
struct GpuPipe { pipe: wgpu::RenderPipeline, layout: wgpu::BindGroupLayout, /** GUN SHADOW */ shadow: Option<wgpu::RenderPipeline>, /** FULL PREPASS: the depth pre-pass twin */ depth: wgpu::RenderPipeline }

pub struct GunRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    hdr_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    common: String,
    bgl_globals: wgpu::BindGroupLayout,
    textures: Vec<GpuTex>,
    buffers: Vec<wgpu::Buffer>,
    meshes: Vec<GpuMesh>,
    pipes: Vec<GpuPipe>,
    binds: Vec<wgpu::BindGroup>,
    samplers: HashMap<GunSamplerDesc, wgpu::Sampler>,
    frame: GunFrame,
    /// GUN SHADOW: group 0 of the depth pipelines (empty: the cascade pass must not bind the globals, whose shadow array is the pass attachment) and group 2
    /// (the cascade's LightViewProj, one bind group per renderer shadow vp buffer, [`set_shadow_vp`](Self::set_shadow_vp)).
    bgl_empty: wgpu::BindGroupLayout,
    bg_empty: wgpu::BindGroup,
    bgl_shadow_vp: wgpu::BindGroupLayout,
    shadow_vp_bgs: Vec<wgpu::BindGroup>,
    /// Last error text of a failed pipeline creation.
    pub last_error: Option<String>,
    /// TEXTURE MEMORY (2026-10-06): textures created per GPU format (count, bytes of all levels), for reports (`texture_report`).
    tex_stats: HashMap<crate::fx::FxTexFormat, (usize, u64)>,
    /// VELOCITY (2026-10-05): velocity twins by pipe index, the registered `View` buffers and their previous-view history.
    velocity_pipes: HashMap<usize, wgpu::RenderPipeline>,
    view_bufs: std::collections::HashSet<usize>,
    view_history: crate::taa::ViewHistory,
    view_size: (u32, u32),
    write_seen: Vec<u64>,
    uploaded: Vec<Vec<u8>>,
    write_epoch: u64,
    dedup: HashMap<u64, usize>,
    #[cfg(target_arch = "wasm32")] web_pending: HashMap<usize, Vec<(u8, wgpu::PendingRenderPipeline, Option<wgpu::RenderPipeline>)>>,
    #[cfg(target_arch = "wasm32")] web_failed: std::collections::HashSet<usize>,
    #[cfg(target_arch = "wasm32")] placeholder: Option<wgpu::RenderPipeline>,
}

fn buffer(device: &wgpu::Device, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer { device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false }) }

impl GunRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, common: &str, lit_common: &str, bgl_globals: &wgpu::BindGroupLayout, hdr_format: wgpu::TextureFormat, depth_format: wgpu::TextureFormat) -> GunRenderer {
        let resolve = fn_live::wgsl!("../shaders/gun_resolve.wgsl");
        let bgl_empty = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("gun shadow empty"), entries: &[] });
        let bg_empty = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("gun shadow empty"), layout: &bgl_empty, entries: &[] });
        let bgl_shadow_vp = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("gun shadow vp"), entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0, visibility: wgpu::ShaderStages::VERTEX, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }] });
        GunRenderer {
            device: device.clone(), queue: queue.clone(), hdr_format, depth_format,
            common: [common, lit_common, resolve].join("\n"), bgl_globals: bgl_globals.clone(),
            textures: vec![], buffers: vec![], meshes: vec![], pipes: vec![], binds: vec![], samplers: HashMap::new(), frame: GunFrame::default(),
            bgl_empty, bg_empty, bgl_shadow_vp, shadow_vp_bgs: vec![], last_error: None, tex_stats: HashMap::new(),
            velocity_pipes: HashMap::new(), view_bufs: Default::default(), view_history: Default::default(), view_size: (0, 0), write_seen: Vec::new(), uploaded: Vec::new(), write_epoch: 0, // VELOCITY
            dedup: HashMap::new(),
            #[cfg(target_arch = "wasm32")] web_pending: HashMap::new(),
            #[cfg(target_arch = "wasm32")] web_failed: Default::default(),
            #[cfg(target_arch = "wasm32")] placeholder: None,
        }
    }

    pub fn create_texture(&mut self, d: &GunTextureDesc) -> GunTexId { self.create_texture_fmt(d, crate::fx::FxTexFormat::Rgba8) }
    /// TEXTURE MEMORY (2026-10-06, BC passthrough): a texture whose level 0 is `width` x `height` can be uploaded here as cooked BC blocks
    /// ([`Self::create_texture_fmt`]): the device takes BC (`fx::bc_native_hint`, `FN_DEBUG_NO_BC=1` turns it off) and the size is a multiple of 4.
    pub fn accepts_bc(&self, width: u32, height: u32) -> bool {
        static BC: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        crate::fx::bc_native_hint() && *BC.get_or_init(|| self.device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC)) && width % 4 == 0 && height % 4 == 0 && width > 0 && height > 0
    }
    /// [`Self::create_texture`] with the levels in `format`: RGBA8 pixels, or a BC format's cooked 4x4 blocks (`ceil(w / 4) * ceil(h / 4)` blocks per level,
    /// sRGB / UNORM as `fx::FxTexFormat::wgpu`: BC4 / BC5 always linear like UE4.16's D3D11 RHI). Check [`Self::accepts_bc`] first: blocks this renderer
    /// cannot take become a 1x1 black texture (logged) instead of a wgpu validation error.
    pub fn create_texture_fmt(&mut self, d: &GunTextureDesc, fmt: crate::fx::FxTexFormat) -> GunTexId {
        let (w0, h0, _) = &d.levels[0];
        if fmt.is_bc() && !(self.accepts_bc(*w0, *h0) && d.levels.iter().all(|(w, h, b)| b.len() >= fmt.level_bytes(*w, *h))) {
            eprintln!("[gun] {}: {fmt:?} blocks this renderer cannot take ({w0}x{h0}); 1x1 black instead", d.label);
            return self.create_texture(&GunTextureDesc { label: d.label.clone(), srgb: d.srgb, levels: vec![(1, 1, vec![0, 0, 0, 255])] });
        }
        let format = fmt.wgpu(d.srgb);
        let tex = self.device.create_texture(&wgpu::TextureDescriptor { label: Some(&d.label), size: wgpu::Extent3d { width: *w0, height: *h0, depth_or_array_layers: 1 }, mip_level_count: d.levels.len() as u32, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        for (i, (w, h, data)) in d.levels.iter().enumerate() {
            if fmt.is_bc() {
                // whole 4x4 blocks: rows of ceil(w / 4) blocks, the copy covering the level's block-rounded (physical) size
                let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
                self.queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, &data[..fmt.level_bytes(*w, *h)], wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bw * fmt.unit_bytes() as u32), rows_per_image: Some(bh) }, wgpu::Extent3d { width: bw * 4, height: bh * 4, depth_or_array_layers: 1 });
            } else {
                self.queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, data, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(*h) }, wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 });
            }
        }
        let bytes: u64 = d.levels.iter().map(|(w, h, _)| fmt.level_bytes(*w, *h) as u64).sum();
        let e = self.tex_stats.entry(fmt).or_insert((0, 0)); e.0 += 1; e.1 += bytes;
        self.textures.push(GpuTex { view: tex.create_view(&Default::default()) });
        GunTexId(self.textures.len() - 1)
    }
    /// TEXTURE MEMORY: one report line of the textures created so far, per GPU format.
    pub fn texture_report(&self) -> String {
        let mut v: Vec<_> = self.tex_stats.iter().collect();
        v.sort_by_key(|x| format!("{:?}", x.0));
        let total: u64 = v.iter().map(|x| x.1 .1).sum();
        format!("gun textures {:.1} MB: {}", total as f64 / 1048576., v.iter().map(|(f, (n, b))| format!("{f:?} {n} ({:.1} MB)", *b as f64 / 1048576.)).collect::<Vec<_>>().join(", "))
    }
    /// A uniform buffer of `size` bytes (rounded up to 16).
    pub fn create_buffer(&mut self, label: &str, size: usize) -> GunBufId {
        self.buffers.push(buffer(&self.device, label, (size.max(16).div_ceil(16) * 16) as u64, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST));
        GunBufId(self.buffers.len() - 1)
    }
    /// Vertex bytes (the pipeline's single interleaved stream) + u32 indices.
    pub fn create_mesh(&mut self, label: &str, vertices: &[u8], indices: &[u32]) -> GunMeshId {
        let vb = buffer(&self.device, label, vertices.len().max(4) as u64, wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST);
        self.queue.write_buffer(&vb, 0, vertices);
        let ib = buffer(&self.device, label, (indices.len() * 4).max(4) as u64, wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST);
        self.queue.write_buffer(&ib, 0, bytemuck::cast_slice(indices));
        self.meshes.push(GpuMesh { vb, ib });
        GunMeshId(self.meshes.len() - 1)
    }
    /// Creates the pipeline; a WGSL / validation error is returned (and kept in `last_error`) instead of panicking.
    pub fn create_pipeline(&mut self, d: &GunPipelineDesc) -> Result<GunPipeId, String> {
        let key = { use std::hash::{Hash, Hasher}; let mut h = std::collections::hash_map::DefaultHasher::new(); format!("{:?}", GunPipelineDesc { label: String::new(), ..d.clone() }).hash(&mut h); h.finish() };
        if let Some(&i) = self.dedup.get(&key) { return Ok(GunPipeId(i)); }
        let idx = self.pipes.len();
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        // the translated programs use screen-space derivatives / sampling in non-uniform control flow, as the HLSL they come from did
        let source = format!("diagnostic(off, derivative_uniformity);\n{}\n{}", self.common, d.wgsl);
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(&d.label), source: wgpu::ShaderSource::Wgsl(source.into()) });
        let entries: Vec<wgpu::BindGroupLayoutEntry> = d.layout.iter().map(|e| {
            let mut vis = wgpu::ShaderStages::empty();
            if e.vertex { vis |= wgpu::ShaderStages::VERTEX; }
            if e.fragment { vis |= wgpu::ShaderStages::FRAGMENT; }
            let ty = match e.ty {
                GunBindTy::Uniform => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                GunBindTy::Tex2D => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                GunBindTy::Sampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            };
            wgpu::BindGroupLayoutEntry { binding: e.binding, visibility: vis, ty, count: None }
        }).collect();
        let layout = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(&d.label), entries: &entries });
        let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(&d.label), bind_group_layouts: &[Some(&self.bgl_globals), Some(&layout)], immediate_size: 0 });
        let attrs: Vec<wgpu::VertexAttribute> = d.vertex_attrs.iter().map(|a| wgpu::VertexAttribute { format: a.format.wgpu(), offset: a.offset as u64, shader_location: a.location }).collect();
        let (cull_mode, front_face) = match d.cull { GunCull::None => (None, wgpu::FrontFace::Ccw), GunCull::BackCw => (Some(wgpu::Face::Back), wgpu::FrontFace::Ccw), GunCull::BackCcw => (Some(wgpu::Face::Back), wgpu::FrontFace::Cw) };
        // FULL PREPASS (2026-10-06): UE4.16 1.8 runs a FULL depth pre-pass (cooked r.SelectiveBasePassOutputs 1 / r.StencilForLODDither 1 -> ShouldForceFullDepthPass ->
        // DDM_AllOpaque, bEarlyZPassMovable, DeferredShadingRenderer.cpp:146-182) and a read-only-depth base pass (:773): the gun / pickup meshes (opaque only,
        // fn-matshader gun.rs refuses other blend modes) are drawn into the renderer's pre-pass by the `depth` twin below (same vertex program, no pixel shader: the
        // position-only depth path of an opaque material, DepthRendering.cpp:652-669) and this base-pass pipeline tests Equal without depth writes (was: Less + write).
        let pipe = self.rp(idx, 0, &wgpu::RenderPipelineDescriptor {
            label: Some(&d.label), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: d.vertex_stride as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })] },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::Equal), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(&d.fs_entry), compilation_options: Default::default(), targets: &crate::reflection::lit_targets(self.hdr_format) }), // REFLECTION APPLY: + reflection inputs
            multiview_mask: None, cache: None,
        });
        let depth = self.rp(idx, 1, &wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{} depth", d.label)), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: d.vertex_stride as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })] },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(), fragment: None, multiview_mask: None, cache: None,
        });
        // GUN SHADOW: the depth-only pipeline of the same programs. UE4.16 FShadowDepthDrawingPolicy draws the material's own vertex factory into whole-scene
        // directional shadows, culled like the base pass for one-sided materials and both faces for TwoSided ones (`Material->IsTwoSided()`,
        // ShadowDepthRendering.cpp:1059 -> EDrawingPolicyOverrideFlags::TwoSided; D3D11 reverses no culling, `bPlatformReversesCulling` false, :843); no hardware
        // depth bias (ShaderDepthBias is added by the program). Same depth state as the renderer's shadow pipelines (renderer.rs).
        let shadow = d.shadow_vs_entry.as_ref().map(|entry| {
            let spl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(&d.label), bind_group_layouts: &[Some(&self.bgl_empty), Some(&layout), Some(&self.bgl_shadow_vp)], immediate_size: 0 });
            self.rp(idx, 2, &wgpu::RenderPipelineDescriptor {
                label: Some(&format!("{} shadow", d.label)), layout: Some(&spl),
                vertex: wgpu::VertexState { module: &module, entry_point: Some(entry), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: d.vertex_stride as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })] },
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: wgpu::DepthBiasState::default() }),
                multisample: Default::default(), fragment: None, multiview_mask: None, cache: None,
            })
        });
        // VELOCITY (2026-10-05): with r.BasePassOutputsVelocity = 1 the original base-pass pixel program writes the velocity as its SV_Target4
        // (BasePassPixelShader.usf:796-810, :1068); the port's separate velocity pass draws the same programs with fn-matshader's `gun_velocity_main` (that
        // program's SV_Target4) into the velocity target: same module / layout / vertex state / culling, depth tested against the finished scene depth (the
        // same vertex program gives the same depth), no depth write.
        let velocity = d.wgsl.contains("fn gun_velocity_main(").then(|| self.rp(idx, 3, &wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{} velocity", d.label)), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: d.vertex_stride as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })] },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("gun_velocity_main"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: crate::taa::VELOCITY_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
            multiview_mask: None, cache: None,
        }));
        if let Some(e) = crate::gpu::pop_error_scope(scope) {
            let msg = format!("{}: {e}", d.label);
            self.last_error = Some(msg.clone());
            #[cfg(target_arch = "wasm32")] self.web_pending.remove(&idx);
            return Err(msg);
        }
        self.dedup.insert(key, idx);
        self.pipes.push(GpuPipe { pipe, layout, shadow, depth });
        if let Some(v) = velocity { self.velocity_pipes.insert(self.pipes.len() - 1, v); } // VELOCITY
        Ok(GunPipeId(self.pipes.len() - 1))
    }
    fn rp(&mut self, idx: usize, slot: u8, desc: &wgpu::RenderPipelineDescriptor) -> wgpu::RenderPipeline {
        #[cfg(target_arch = "wasm32")]
        {
            let p = self.device.fnweb_create_render_pipeline_async(desc);
            self.web_pending.entry(idx).or_default().push((slot, p, None));
            if self.placeholder.is_none() {
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("gun pending placeholder"), source: wgpu::ShaderSource::Wgsl("@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }".into()) });
                let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("gun pending placeholder"), bind_group_layouts: &[], immediate_size: 0 });
                self.placeholder = Some(self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("gun pending placeholder"), layout: Some(&layout),
                    vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                    primitive: Default::default(),
                    depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::Always), stencil: Default::default(), bias: Default::default() }),
                    multisample: Default::default(), fragment: None, multiview_mask: None, cache: None,
                }));
            }
            return self.placeholder.clone().unwrap();
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = (idx, slot); self.device.create_render_pipeline(desc) }
    }
    fn skip(&self, i: usize) -> bool {
        #[cfg(target_arch = "wasm32")]
        return self.web_pending.contains_key(&i) || self.web_failed.contains(&i);
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = i; false }
    }
    #[cfg(target_arch = "wasm32")]
    fn web_poll(&mut self) {
        if self.web_pending.is_empty() { return; }
        let mut done = vec![];
        for (idx, slots) in self.web_pending.iter_mut() {
            let mut err = None;
            for (_, p, got) in slots.iter_mut() { if got.is_none() { match p.poll() { Some(Ok(x)) => *got = Some(x), Some(Err(m)) => err = Some(m), None => {} } } }
            if let Some(m) = err { done.push((*idx, Some(m))); } else if slots.iter().all(|s| s.2.is_some()) { done.push((*idx, None)); }
        }
        for (idx, err) in done {
            let Some(slots) = self.web_pending.remove(&idx) else { continue };
            if let Some(m) = err { eprintln!("[gun] pipeline failed (its draws stay off): {m}"); self.last_error = Some(m); self.web_failed.insert(idx); continue; }
            for (slot, _, got) in slots {
                let Some(p) = got else { continue };
                match slot {
                    0 => { if let Some(gp) = self.pipes.get_mut(idx) { gp.pipe = p; } }
                    1 => { if let Some(gp) = self.pipes.get_mut(idx) { gp.depth = p; } }
                    2 => { if let Some(gp) = self.pipes.get_mut(idx) { gp.shadow = Some(p); } }
                    _ => { self.velocity_pipes.insert(idx, p); }
                }
            }
        }
    }
    pub fn pending_pipelines(&self) -> usize {
        #[cfg(target_arch = "wasm32")]
        return self.web_pending.len();
        #[cfg(not(target_arch = "wasm32"))]
        0
    }
    /// VELOCITY: marks `buf` as a `View` uniform buffer: its previous-view members are filled by the renderer ([`crate::taa::ViewHistory`]).
    pub fn register_view_buffer(&mut self, buf: GunBufId) { self.view_bufs.insert(buf.0); }
    /// VELOCITY: this frame's velocity state (call once per rendered frame before [`Self::prepare`]).
    pub fn set_velocity_state(&mut self, s: crate::taa::VelocityState) { self.view_history.begin_frame(s); }
    pub fn set_view_size(&mut self, size: (u32, u32)) { self.view_size = size; }
    /// VELOCITY: the frame has draws whose pipeline has a velocity twin.
    pub fn has_velocity_draws(&self) -> bool { self.frame.draws.iter().any(|d| !self.skip(d.pipe.0) && self.velocity_pipes.contains_key(&d.pipe.0)) }
    /// VELOCITY: records the frame's draws into the velocity pass (group 0 = `globals`, set here).
    pub fn draw_velocity<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, globals: &'a wgpu::BindGroup) {
        let mut bound = false;
        for d in &self.frame.draws {
            if self.skip(d.pipe.0) { continue; }
            let (Some(v), Some(m), Some(b)) = (self.velocity_pipes.get(&d.pipe.0), self.meshes.get(d.mesh.0), self.binds.get(d.bind.0)) else { continue };
            pass.set_pipeline(v);
            if !bound { pass.set_bind_group(0, globals, &[]); bound = true; }
            pass.set_bind_group(1, b, &[]);
            pass.set_vertex_buffer(0, m.vb.slice(..));
            pass.set_index_buffer(m.ib.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(d.first_index..d.first_index + d.index_count, 0, 0..1);
        }
    }
    fn sampler(&mut self, d: GunSamplerDesc) {
        let device = &self.device;
        self.samplers.entry(d).or_insert_with(|| {
            let f = |x: GunFilter| match x { GunFilter::Nearest => wgpu::FilterMode::Nearest, GunFilter::Linear => wgpu::FilterMode::Linear };
            let a = match d.address { GunAddress::Wrap => wgpu::AddressMode::Repeat, GunAddress::Clamp => wgpu::AddressMode::ClampToEdge, GunAddress::Mirror => wgpu::AddressMode::MirrorRepeat };
            let all_linear = d.mag == GunFilter::Linear && d.min == GunFilter::Linear && d.mip == GunFilter::Linear;
            device.create_sampler(&wgpu::SamplerDescriptor { label: Some("gun sampler"), address_mode_u: a, address_mode_v: a, address_mode_w: a, mag_filter: f(d.mag), min_filter: f(d.min), mipmap_filter: match d.mip { GunFilter::Nearest => wgpu::MipmapFilterMode::Nearest, GunFilter::Linear => wgpu::MipmapFilterMode::Linear }, anisotropy_clamp: if all_linear { d.aniso.max(1) } else { 1 }, ..Default::default() })
        });
    }
    /// Bind group 1 of `pipe`.
    pub fn create_bind_group(&mut self, pipe: GunPipeId, entries: &[GunBindEntry]) -> GunBindId {
        for e in entries { if let GunRes::Sampler(s) = e.res { self.sampler(s); } }
        let bge: Vec<wgpu::BindGroupEntry> = entries.iter().map(|e| wgpu::BindGroupEntry { binding: e.binding, resource: match e.res {
            GunRes::Buffer(b) => self.buffers[b.0].as_entire_binding(),
            GunRes::Texture(t) => wgpu::BindingResource::TextureView(&self.textures[t.0].view),
            GunRes::Sampler(s) => wgpu::BindingResource::Sampler(self.samplers.get(&s).unwrap()),
        } }).collect();
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("gun bind group"), layout: &self.pipes[pipe.0].layout, entries: &bge });
        self.binds.push(bg);
        GunBindId(self.binds.len() - 1)
    }

    // ---- per frame -------------------------------------------------------------------------------------------------------------
    pub fn set_frame(&mut self, frame: GunFrame) { self.frame = frame; }
    /// Adds more draws to the frame set by [`set_frame`](Self::set_frame) (PICKUP ITEM LIGHTING: the world items' weapon meshes after the held gun).
    pub fn append_frame(&mut self, frame: GunFrame) { self.frame.buffer_writes.extend(frame.buffer_writes); self.frame.draws.extend(frame.draws); }
    pub fn any(&self) -> bool { !self.frame.draws.is_empty() }
    /// GUN SHADOW: the renderer's per-cascade `LightViewProj` buffers (shadow.wgsl layout: mat4 + depth bias), bound as group 2 of the depth pass.
    pub fn set_shadow_vp(&mut self, bufs: &[wgpu::Buffer]) {
        self.shadow_vp_bgs = bufs.iter().map(|b| self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("gun shadow vp"), layout: &self.bgl_shadow_vp, entries: &[wgpu::BindGroupEntry { binding: 0, resource: b.as_entire_binding() }] })).collect();
    }
    /// GUN SHADOW: records the frame's shadow-casting draws into cascade `cascade`'s depth pass (viewport already set). Sets groups 0..2 itself: call it last in the pass.
    /// PERF (report only, `FN_SHADOW_STATS=1`): (draws, triangles) [`Self::draw_shadow`] records per cascade.
    pub fn shadow_stats(&self) -> (u64, u64) {
        self.frame.draws.iter().filter(|d| !self.skip(d.pipe.0) && d.shadow_bind.is_some() && self.pipes.get(d.pipe.0).is_some_and(|p| p.shadow.is_some())).fold((0, 0), |a, d| (a.0 + 1, a.1 + d.index_count as u64 / 3))
    }
    pub fn draw_shadow<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, cascade: usize) {
        let Some(vp) = self.shadow_vp_bgs.get(cascade) else { return };
        let mut bound = false;
        for d in &self.frame.draws {
            if self.skip(d.pipe.0) { continue; }
            let (Some(p), Some(m), Some(sb)) = (self.pipes.get(d.pipe.0), self.meshes.get(d.mesh.0), d.shadow_bind) else { continue };
            let (Some(sp), Some(b)) = (&p.shadow, self.binds.get(sb.0)) else { continue };
            if !bound { pass.set_bind_group(0, &self.bg_empty, &[]); pass.set_bind_group(2, vp, &[]); bound = true; }
            pass.set_pipeline(sp);
            pass.set_bind_group(1, b, &[]);
            pass.set_vertex_buffer(0, m.vb.slice(..));
            pass.set_index_buffer(m.ib.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(d.first_index..d.first_index + d.index_count, 0, 0..1);
        }
    }
    /// Uploads the frame's uniform buffers. Call once per rendered frame before the scene pass.
    pub fn prepare(&mut self) {
        #[cfg(target_arch = "wasm32")] self.web_poll();
        // VELOCITY: PrevTranslatedWorldToClip / PrevPreViewTranslation of the View buffers = their last frame's own matrices (taa.rs ViewHistory)
        self.write_epoch += 1;
        if self.write_seen.len() < self.buffers.len() { self.write_seen.resize(self.buffers.len(), 0); }
        if self.uploaded.len() < self.buffers.len() { self.uploaded.resize(self.buffers.len(), Vec::new()); }
        for (b, bytes) in self.frame.buffer_writes.iter_mut().rev() {
            let Some(seen) = self.write_seen.get_mut(b.0) else { continue };
            if *seen == self.write_epoch { continue; }
            *seen = self.write_epoch;
            if self.view_bufs.contains(&b.0) { crate::taa::apply_view_size(bytes, self.view_size); self.view_history.patch(b.0, bytes); }
            if let Some(buf) = self.buffers.get(b.0) {
                let n = (bytes.len() as u64).min(buf.size()) as usize;
                let prev = &mut self.uploaded[b.0];
                if n > 0 && prev.as_slice() == &bytes[..n] { continue; }
                self.queue.write_buffer(buf, 0, &bytes[..n]);
                prev.clear(); prev.extend_from_slice(&bytes[..n]);
            }
        }
        self.frame.buffer_writes.clear();
    }
    /// FULL PREPASS (2026-10-06): records the frame's draws with their depth twins into the renderer's depth pre-pass (group 0 = `globals`, set here). The base-pass
    /// pipelines test Equal against this depth, so it records exactly the draws [`Self::draw`] records.
    pub fn draw_depth<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, globals: &'a wgpu::BindGroup) {
        let mut bound = false;
        for d in &self.frame.draws {
            if self.skip(d.pipe.0) { continue; }
            let (Some(p), Some(m), Some(b)) = (self.pipes.get(d.pipe.0), self.meshes.get(d.mesh.0), self.binds.get(d.bind.0)) else { continue };
            pass.set_pipeline(&p.depth);
            if !bound { pass.set_bind_group(0, globals, &[]); bound = true; }
            pass.set_bind_group(1, b, &[]);
            pass.set_vertex_buffer(0, m.vb.slice(..));
            pass.set_index_buffer(m.ib.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(d.first_index..d.first_index + d.index_count, 0, 0..1);
        }
    }
    /// Records the draws into the scene pass (group 0, the renderer's globals, is already set).
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        for d in &self.frame.draws {
            if self.skip(d.pipe.0) { continue; }
            let (Some(p), Some(m), Some(b)) = (self.pipes.get(d.pipe.0), self.meshes.get(d.mesh.0), self.binds.get(d.bind.0)) else { continue };
            pass.set_pipeline(&p.pipe);
            pass.set_bind_group(1, b, &[]);
            pass.set_vertex_buffer(0, m.vb.slice(..));
            pass.set_index_buffer(m.ib.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(d.first_index..d.first_index + d.index_count, 0, 0..1);
        }
    }
}
