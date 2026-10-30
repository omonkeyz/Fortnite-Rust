//! GPU side of the textured static-mesh path: pipelines (scene + alpha-tested shadow variants), mesh / material storage, per-frame instance
//! culling (frustum, distance fade / cull, LOD with hysteresis, per-cascade light-box culling) and the batched instanced draws.
//!
//! Frame flow (`Renderer::render_scene`): `prepare` (CPU) writes ONE instance buffer holding, back to back, the instance lists of the main
//! view and of the three shadow cascades, grouped per (mesh, LOD); `draw_scene` / `draw_shadow` then issue one `draw_indexed` per
//! (mesh, LOD, section) with `first_instance..first_instance + count`, so draw calls scale with the number of distinct meshes, not with
//! the number of props.
use crate::camera::CameraParams;
use crate::lighting::{Cascade, CASCADE_COUNT};
use crate::static_mesh::*;
use bytemuck::{Pod, Zeroable};
use glam::{Quat, Vec3};
use image::RgbaImage;

/// Back-face culling of one-sided materials (the exported glTF winding is counter-clockwise outward).
const CULL_ONE_SIDED: Option<wgpu::Face> = Some(wgpu::Face::Back);
/// MIRRORFIX: a mirrored instance (`StaticMeshInstance::mirror_x`, negative determinant) reverses the cull mode in the scene AND the shadow depth passes
/// (`ReverseCulling` -> `EDrawingPolicyOverrideFlags::ReverseCullMode` -> `InverseCullMode`, StaticMeshRender.cpp:284, DrawingPolicy.h:247 / :345; the shadow
/// depth policy takes the same flag, ShadowDepthRendering.cpp:686); front faces stay counter-clockwise (D3D11 `CM_CCW` = `D3D11_CULL_FRONT`, D3D11State.cpp:42).
const CULL_ONE_SIDED_REVERSED: Option<wgpu::Face> = Some(wgpu::Face::Front);
/// Growth margin of the bounding sphere for wind / shake displacement (share of the mesh height).
const SWAY_MARGIN: f32 = 0.06;
const INITIAL_INSTANCE_CAPACITY: usize = 4096;

/// What the last `prepare` did (for tools, tests and the viewer HUD).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StaticStats {
    /// Instances submitted with `set_static_instances`.
    pub instances: u32,
    /// Instances drawn in the main view.
    pub visible: u32,
    pub culled_distance: u32,
    pub culled_frustum: u32,
    /// Instances in the main view per LOD.
    pub lod_counts: [u32; MAX_LODS],
    /// Instance draws summed over the three shadow cascades.
    pub shadow_instances: u32,
    /// `draw_indexed` calls of the main view / of the three cascades together.
    pub draw_calls: u32,
    pub shadow_draw_calls: u32,
    /// Triangles submitted in the main view (instances x triangles of the chosen LOD).
    pub triangles: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialGpu { tint: [f32; 4], p0: [f32; 4], p1: [f32; 4], p2: [f32; 4] }

struct GpuMaterial {
    buf: wgpu::Buffer,
    base: wgpu::TextureView,
    normal: wgpu::TextureView,
    bg: wgpu::BindGroup,
    masked: bool,
    two_sided: bool,
}
struct GpuSection { first: u32, count: u32, material: usize }
struct GpuLod { vb: wgpu::Buffer, ib: wgpu::Buffer, sections: Vec<GpuSection>, tris: u32 }
struct GpuMesh { lods: Vec<GpuLod>, limits: Vec<f32>, center: Vec3, radius: f32, height: f32, cull: f32, fade: f32, cast_shadow: bool, fort_default_cull: bool }
/// `mirror`: the group's instances are mirrored (reversed culling, MIRRORFIX).
#[derive(Clone, Copy)]
struct Group { mesh: u32, lod: u32, first: u32, count: u32, mirror: bool }

const VERTEX_ATTRS: [wgpu::VertexAttribute; 4] = [
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 2 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Unorm8x4, offset: 32, shader_location: 3 },
];
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 3] = [
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 4 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 5 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 32, shader_location: 6 },
];

/// The mip chains of a material's colour and normal textures (see [`prebuilt_mips`]).
pub type PrebuiltMips = (Option<Vec<RgbaImage>>, Option<Vec<RgbaImage>>);

thread_local! {
    /// LOAD TIME (2026-10-06): the chains [`with_prebuilt_mips`] hands to the next `create_material` on this thread.
    static PREBUILT: std::cell::RefCell<Option<PrebuiltMips>> = const { std::cell::RefCell::new(None) };
}

/// LOAD TIME (2026-10-06): the mip chains [`StaticRenderer::create_material`] builds for `d` (sRGB colour with its alpha coverage kept, triangle-filtered
/// normal map), built ahead on a loading thread (`static_loader::prepare_kind`): the same function on the same images, so the same texels. In the browser
/// build the material creation runs on the page's main thread, where these chains cost 1-3 s of a load (verification/browser/launch5, launch7).
pub fn prebuilt_mips(d: &StaticMaterialData) -> PrebuiltMips {
    let masked = d.alpha_mode == AlphaMode::Masked;
    (d.base_color.as_ref().map(|img| build_mips_srgb(img, masked.then_some(d.alpha_cutoff))), d.normal.as_ref().map(|img| crate::texture::build_mips(img, crate::texture::MipFilter::Triangle)))
}

/// Runs `f` with `mips` (from [`prebuilt_mips`] of the same material) as the chains of the material it creates, instead of building them there.
pub fn with_prebuilt_mips<R>(mips: PrebuiltMips, f: impl FnOnce() -> R) -> R {
    PREBUILT.with(|p| *p.borrow_mut() = Some(mips));
    let r = f();
    PREBUILT.with(|p| *p.borrow_mut() = None);
    r
}

fn upload_levels(device: &wgpu::Device, queue: &wgpu::Queue, levels: &[RgbaImage], format: wgpu::TextureFormat, label: &str) -> wgpu::TextureView {
    let base = &levels[0];
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label), size: wgpu::Extent3d { width: base.width(), height: base.height(), depth_or_array_layers: 1 }, mip_level_count: levels.len() as u32,
        sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[],
    });
    for (i, l) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            l.as_raw(), wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * l.width()), rows_per_image: Some(l.height()) },
            wgpu::Extent3d { width: l.width(), height: l.height(), depth_or_array_layers: 1 },
        );
    }
    tex.create_view(&Default::default())
}

pub(crate) struct StaticRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bgl_material: wgpu::BindGroupLayout,
    /// [cull back, two-sided, cull front (one-sided, mirrored instances)]
    scene_pipes: [wgpu::RenderPipeline; 3],
    /// PREPASS: depth-only pre-pass pipelines, same cull variants as `scene_pipes` (UE4.16 r.EarlyZPass 2).
    depth_pipes: [wgpu::RenderPipeline; 3],
    /// [opaque (vertex only), masked (alpha tested)] x [one-sided (back faces culled), two-sided, one-sided mirrored (front faces culled)]:
    /// index = masked + 2 * (two_sided ? 1 : mirrored ? 2 : 0)
    shadow_pipes: [wgpu::RenderPipeline; 6],
    sampler: wgpu::Sampler,
    white: wgpu::TextureView,
    flat_normal: wgpu::TextureView,
    materials: Vec<GpuMaterial>,
    meshes: Vec<GpuMesh>,
    instances: Vec<StaticMeshInstance>,
    /// SCALABILITY: `r.ViewDistanceScale` (quality::ViewDistanceScalability).
    view_distance_scale: f32,
    lod_state: Vec<u8>,
    inst_buf: wgpu::Buffer,
    inst_capacity: usize,
    all: Vec<InstanceGpu>,
    buckets: Vec<Vec<InstanceGpu>>,
    /// [main, cascade 0, 1, 2]
    groups: [Vec<Group>; 1 + CASCADE_COUNT],
    stats: StaticStats,
    /// OUTLINE: what the custom depth pass needs to build its pipelines lazily (the scene module / layout), its pipelines (stencil value, cull variants
    /// like `depth_pipes`), its instances and their groups.
    scene_sm: wgpu::ShaderModule,
    scene_layout: wgpu::PipelineLayout,
    custom_pipes: Option<(f32, [wgpu::RenderPipeline; 3])>,
    custom_instances: Vec<StaticMeshInstance>,
    custom_buf: wgpu::Buffer,
    custom_capacity: usize,
    custom_groups: Vec<Group>,
    shadow_layout: wgpu::PipelineLayout,
    formats: (wgpu::TextureFormat, wgpu::TextureFormat),
}

/// OUTLINE: the custom depth target (UE CustomDepth) and the custom stencil target (the stencil value in `.xy`).
pub const CUSTOM_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const CUSTOM_STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Uint;

#[allow(clippy::type_complexity)]
fn static_pipelines(device: &wgpu::Device, common: &str, lit_common: &str, scene_layout: &wgpu::PipelineLayout, shadow_layout: &wgpu::PipelineLayout, hdr: wgpu::TextureFormat, depth: wgpu::TextureFormat) -> (wgpu::ShaderModule, [wgpu::RenderPipeline; 3], [wgpu::RenderPipeline; 3], [wgpu::RenderPipeline; 6]) {
    let static_common = fn_live::wgsl!("../shaders/static_common.wgsl");
    let scene_sm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("static scene"), source: wgpu::ShaderSource::Wgsl([common, lit_common, static_common, fn_live::wgsl!("../shaders/static_scene.wgsl")].join("\n").into()) });
    let shadow_sm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("static shadow"), source: wgpu::ShaderSource::Wgsl([static_common, fn_live::wgsl!("../shaders/static_shadow.wgsl")].join("\n").into()) });
    let buffers = [
        Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<StaticVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &VERTEX_ATTRS }),
        Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<InstanceGpu>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &INSTANCE_ATTRS }),
    ];
    let scene_pipe = |label: &str, cull: Option<wgpu::Face>| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label), layout: Some(scene_layout),
        vertex: wgpu::VertexState { module: &scene_sm, entry_point: Some("vs_static"), compilation_options: Default::default(), buffers: &buffers },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: cull, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::Equal) /* PREPASS: base pass after the full pre-pass, depth read only (masked: CF_Equal with the mask only in the pre-pass, cooked r.EarlyZPassOnlyMaterialMasking; opaque: CF_GreaterEqual on reversed Z = LessEqual, identical after the pre-pass) BasePassRendering.cpp:308-361 */, stencil: Default::default(), bias: Default::default() }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &scene_sm, entry_point: Some("fs_static"), compilation_options: wgpu::PipelineCompilationOptions { constants: &[("EARLY_Z_MASKING", 1.0)], ..Default::default() }, targets: &crate::reflection::lit_targets(hdr) }), // REFLECTION APPLY: + reflection inputs
        multiview_mask: None, cache: None,
    });
    // PREPASS: depth-only (+ the base pass clip tests) variant of `scene_pipe`
    let depth_pipe = |label: &str, cull: Option<wgpu::Face>| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label), layout: Some(scene_layout),
        vertex: wgpu::VertexState { module: &scene_sm, entry_point: Some("vs_static"), compilation_options: Default::default(), buffers: &buffers },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: cull, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: Default::default() }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &scene_sm, entry_point: Some("fs_static_depth"), compilation_options: Default::default(), targets: &[] }),
        multiview_mask: None, cache: None,
    });
    let depth_pipes = [depth_pipe("static depth (one-sided)", CULL_ONE_SIDED), depth_pipe("static depth (two-sided)", None), depth_pipe("static depth (one-sided, mirrored)", CULL_ONE_SIDED_REVERSED)];
    let shadow_pipe = |label: &str, masked: bool, cull: Option<wgpu::Face>| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label), layout: Some(shadow_layout),
        vertex: wgpu::VertexState { module: &shadow_sm, entry_point: Some("vs_static_shadow"), compilation_options: Default::default(), buffers: &buffers },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: cull, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: wgpu::DepthBiasState::default() }),
        multisample: Default::default(),
        fragment: if masked { Some(wgpu::FragmentState { module: &shadow_sm, entry_point: Some("fs_static_shadow"), compilation_options: Default::default(), targets: &[] }) } else { None },
        multiview_mask: None, cache: None,
    });
    let scene_pipes = [scene_pipe("static (one-sided)", CULL_ONE_SIDED), scene_pipe("static (two-sided)", None), scene_pipe("static (one-sided, mirrored)", CULL_ONE_SIDED_REVERSED)];
    // UE4.16 culls back faces of one-sided materials in the shadow depth pass too (SetViewFlagsForShadowPass: only two-sided materials / CastsShadowAsTwoSided draw both),
    // and the depth pass has NO hardware depth bias: the CSM bias is added to the stored depth (shadow_static.wgsl).
    let shadow_pipes = [shadow_pipe("static shadow (opaque)", false, CULL_ONE_SIDED), shadow_pipe("static shadow (masked)", true, CULL_ONE_SIDED), shadow_pipe("static shadow (opaque, two-sided)", false, None), shadow_pipe("static shadow (masked, two-sided)", true, None),
        shadow_pipe("static shadow (opaque, mirrored)", false, CULL_ONE_SIDED_REVERSED), shadow_pipe("static shadow (masked, mirrored)", true, CULL_ONE_SIDED_REVERSED)];
    (scene_sm, scene_pipes, depth_pipes, shadow_pipes)
}

impl StaticRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, common: &str, lit_common: &str, bgl_globals: &wgpu::BindGroupLayout, bgl_shadow: &wgpu::BindGroupLayout, hdr: wgpu::TextureFormat, depth: wgpu::TextureFormat, sampler: wgpu::Sampler) -> StaticRenderer {
        let (v, f, vf) = (wgpu::ShaderStages::VERTEX, wgpu::ShaderStages::FRAGMENT, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT);
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry { binding, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None };
        let bgl_material = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("static material bgl"), entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: vf, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            tex_entry(1), tex_entry(2),
            wgpu::BindGroupLayoutEntry { binding: 3, visibility: f, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
        ] });
        let _ = v;
        let layout = |label: &str, g0: &wgpu::BindGroupLayout| device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[Some(g0), Some(&bgl_material)], immediate_size: 0 });
        let scene_layout = layout("static scene layout", bgl_globals);
        let shadow_layout = layout("static shadow layout", bgl_shadow);
        let (scene_sm, scene_pipes, depth_pipes, shadow_pipes) = static_pipelines(device, common, lit_common, &scene_layout, &shadow_layout, hdr, depth);
        let one = |px: [u8; 4], srgb: bool, label: &str| upload_levels(device, queue, &[RgbaImage::from_pixel(1, 1, image::Rgba(px))], if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm }, label);
        let inst_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("static instances"), size: (INITIAL_INSTANCE_CAPACITY * std::mem::size_of::<InstanceGpu>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let custom_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("static custom depth instances"), size: (64 * std::mem::size_of::<InstanceGpu>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        StaticRenderer {
            device: device.clone(), queue: queue.clone(), bgl_material, scene_pipes, depth_pipes, shadow_pipes, sampler,
            white: one([255; 4], true, "static white"), flat_normal: one([128, 128, 255, 255], false, "static flat normal"),
            materials: vec![], meshes: vec![], instances: vec![], lod_state: vec![], inst_buf, inst_capacity: INITIAL_INSTANCE_CAPACITY,
            all: vec![], buckets: vec![], groups: Default::default(), stats: StaticStats::default(), view_distance_scale: 1.0,
            scene_sm, scene_layout, custom_pipes: None, custom_instances: vec![], custom_buf, custom_capacity: 64, custom_groups: vec![],
            shadow_layout, formats: (hdr, depth),
        }
    }

    pub fn rebuild_pipelines(&mut self, common: &str, lit_common: &str) -> Result<(), String> {
        let (scene_sm, scene_pipes, depth_pipes, shadow_pipes) = crate::renderer::checked_build(&self.device, || static_pipelines(&self.device, common, lit_common, &self.scene_layout, &self.shadow_layout, self.formats.0, self.formats.1))?;
        (self.scene_sm, self.scene_pipes, self.depth_pipes, self.shadow_pipes, self.custom_pipes) = (scene_sm, scene_pipes, depth_pipes, shadow_pipes, None);
        Ok(())
    }

    fn material_bg(&self, buf: &wgpu::Buffer, base: &wgpu::TextureView, normal: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("static material"), layout: &self.bgl_material, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(base) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(normal) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
        ] })
    }
    /// Replaces the texture sampler (anisotropy / minimum mip changed) and rebuilds every material bind group.
    /// SCALABILITY: `r.ViewDistanceScale` of the View Distance level (multiplies every cull distance).
    pub fn set_view_distance_scale(&mut self, s: f32) { self.view_distance_scale = s.max(0.); }
    pub fn set_sampler(&mut self, sampler: wgpu::Sampler) {
        self.sampler = sampler;
        let bgs: Vec<_> = self.materials.iter().map(|m| self.material_bg(&m.buf, &m.base, &m.normal)).collect();
        for (m, bg) in self.materials.iter_mut().zip(bgs) { m.bg = bg; }
    }

    pub fn create_material(&mut self, d: &StaticMaterialData) -> StaticMaterialHandle {
        let masked = d.alpha_mode == AlphaMode::Masked;
        // LOAD TIME: chains built ahead on a loading thread ([`with_prebuilt_mips`]); else built here as before
        let (pre_base, pre_normal) = PREBUILT.with(|p| p.borrow_mut().take()).unwrap_or((None, None));
        let base = match &d.base_color {
            Some(img) => upload_levels(&self.device, &self.queue, &pre_base.unwrap_or_else(|| build_mips_srgb(img, masked.then_some(d.alpha_cutoff))), wgpu::TextureFormat::Rgba8UnormSrgb, &d.name),
            None => self.white.clone(),
        };
        let normal = match &d.normal {
            Some(img) => upload_levels(&self.device, &self.queue, &pre_normal.unwrap_or_else(|| crate::texture::build_mips(img, crate::texture::MipFilter::Triangle)), wgpu::TextureFormat::Rgba8Unorm, &d.name),
            None => self.flat_normal.clone(),
        };
        let u = MaterialGpu {
            tint: [d.tint[0], d.tint[1], d.tint[2], d.translucency],
            p0: [d.alpha_cutoff, masked as u32 as f32, d.two_sided as u32 as f32, d.normal.is_some() as u32 as f32],
            p1: [d.roughness, d.specular, d.sway, d.flutter],
            p2: [d.wind_from_vertex_color as u32 as f32, d.vertex_color_albedo as u32 as f32, 0., 0.],
        };
        let buf = crate::texture::buffer_init(&self.device, &[u], wgpu::BufferUsages::UNIFORM, "static material uniform");
        let bg = self.material_bg(&buf, &base, &normal);
        self.materials.push(GpuMaterial { buf, base, normal, bg, masked, two_sided: d.two_sided });
        StaticMaterialHandle(self.materials.len() - 1)
    }

    /// Uploads a mesh; `materials[section.material]` is the material of every section (missing indices fall back to the first handle).
    pub fn create_mesh(&mut self, d: &StaticMeshData, materials: &[StaticMaterialHandle]) -> StaticMeshHandle {
        assert!(!materials.is_empty(), "a static mesh needs at least one material");
        let (center, radius, height) = mesh_bounds(d);
        let lods: Vec<&StaticLod> = d.lods.iter().take(MAX_LODS).collect();
        let mut gpu_lods = vec![];
        for (i, l) in lods.iter().enumerate() {
            let label = format!("{} lod {i}", d.name);
            let vb = crate::texture::buffer_init(&self.device, &l.vertices, wgpu::BufferUsages::VERTEX, &label);
            let ib = crate::texture::buffer_init(&self.device, &l.indices, wgpu::BufferUsages::INDEX, &label);
            let sections = l.sections.iter().filter(|s| s.index_count > 0).map(|s| GpuSection { first: s.first_index, count: s.index_count, material: materials.get(s.material).copied().unwrap_or(materials[0]).0 }).collect();
            gpu_lods.push(GpuLod { vb, ib, sections, tris: (l.indices.len() / 3) as u32 });
        }
        let limits = lods.iter().take(lods.len().saturating_sub(1)).map(|l| l.max_distance).collect();
        self.meshes.push(GpuMesh { lods: gpu_lods, limits, center, radius, height, cull: d.cull_distance, fade: d.fade_range, cast_shadow: d.cast_shadow, fort_default_cull: d.fort_default_cull });
        StaticMeshHandle(self.meshes.len() - 1)
    }

    pub fn release_meshes(&mut self, meshes: &[StaticMeshHandle]) -> (usize, u64) {
        let gone: std::collections::HashSet<usize> = meshes.iter().map(|h| h.0).filter(|&i| i < self.meshes.len()).collect();
        if gone.is_empty() { return (0, 0); }
        let mut keep = vec![false; self.materials.len()];
        let mut drop = vec![false; self.materials.len()];
        for (i, m) in self.meshes.iter().enumerate() {
            let flag = if gone.contains(&i) { &mut drop } else { &mut keep };
            for l in &m.lods { for s in &l.sections { if let Some(f) = flag.get_mut(s.material) { *f = true; } } }
        }
        let mut bytes = 0u64;
        let tiny = || self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("static released"), size: 16, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX, mapped_at_creation: false });
        for &i in &gone {
            for l in self.meshes[i].lods.iter_mut() {
                bytes += l.vb.size() + l.ib.size();
                l.vb.destroy();
                l.ib.destroy();
                l.vb = tiny();
                l.ib = tiny();
                l.sections.clear();
                l.tris = 0;
            }
        }
        let mut n = 0;
        for (mi, m) in self.materials.iter_mut().enumerate() {
            if !drop[mi] || keep[mi] { continue; }
            let mut changed = false;
            if m.base != self.white {
                let t = m.base.texture();
                bytes += (t.width() as u64 * t.height() as u64 * 4 * 4) / 3;
                t.destroy();
                m.base = self.white.clone();
                changed = true;
            }
            if m.normal != self.flat_normal {
                let t = m.normal.texture();
                bytes += (t.width() as u64 * t.height() as u64 * 4 * 4) / 3;
                t.destroy();
                m.normal = self.flat_normal.clone();
                changed = true;
            }
            if changed { n += 1; }
        }
        let bgs: Vec<(usize, wgpu::BindGroup)> = self.materials.iter().enumerate().filter(|(i, _)| drop[*i] && !keep[*i]).map(|(i, m)| (i, self.material_bg(&m.buf, &m.base, &m.normal))).collect();
        for (i, bg) in bgs { self.materials[i].bg = bg; }
        (n, bytes)
    }
    pub fn mesh_count(&self) -> usize { self.meshes.len() }
    pub fn material_count(&self) -> usize { self.materials.len() }
    pub fn stats(&self) -> &StaticStats { &self.stats }
    pub fn instance_count(&self) -> usize { self.instances.len() }

    pub fn set_instances(&mut self, instances: &[StaticMeshInstance]) {
        // keep the LOD state for the common case of an unchanged list; reset when the list shrinks / grows
        if self.lod_state.len() != instances.len() { self.lod_state = vec![0; instances.len()]; }
        self.instances.clear();
        self.instances.extend_from_slice(instances);
    }

    /// True when anything will be drawn (instances exist).
    pub fn any(&self) -> bool { !self.instances.is_empty() && !self.meshes.is_empty() }

    /// CPU culling + instance upload for this frame (see the module docs). Only the first `num_cascades` cascades exist (`r.Shadow.CSM.MaxCascades`);
    /// `radius_threshold` / `lod_distance_factor` drive the small-caster cull (`quality::shadow_caster_screen_size_culled`).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(&mut self, cam: &CameraParams, cascades: &[Cascade; CASCADE_COUNT], num_cascades: usize, shadows_on: bool, time: f32, radius_threshold: f32, lod_distance_factor: f32) {
        self.stats = StaticStats { instances: self.instances.len() as u32, ..Default::default() };
        self.all.clear();
        for g in &mut self.groups { g.clear(); }
        if !self.any() { return; }
        let nm = self.meshes.len();
        // MIRRORFIX: (mesh, LOD, mirrored) buckets: mirrored instances draw with the reversed-culling pipelines
        let set_len = nm * MAX_LODS * 2;
        if self.buckets.len() != (1 + CASCADE_COUNT) * set_len { self.buckets = vec![Vec::new(); (1 + CASCADE_COUNT) * set_len]; }
        for b in &mut self.buckets { b.clear(); }
        let planes = frustum_planes(&(cam.proj_matrix() * cam.view_matrix()));
        let light_planes: Vec<_> = cascades.iter().map(|c| frustum_planes(&c.light_view_proj)).collect();
        for (i, inst) in self.instances.iter().enumerate() {
            let Some(mesh) = self.meshes.get(inst.mesh.0) else { continue };
            if mesh.lods.is_empty() { continue; }
            let rot = Quat::from_rotation_y(inst.yaw);
            let center = inst.pos + rot * (mesh.center * inst.scale);
            let r = (mesh.radius + mesh.height * SWAY_MARGIN) * inst.scale;
            let d = (center - cam.eye).length();
            // SCALABILITY: MaxDrawDistance * r.ViewDistanceScale (SceneVisibility.cpp:343, HierarchicalInstancedStaticMesh.cpp:1477); a BuildingActor prop
            // without a cooked cull distance takes the FortGlobals curve of its bounds radius (quality::fort_cull)
            let base_cull = if mesh.cull <= 0. && mesh.fort_default_cull { crate::quality::fort_cull::cull_distance_m(mesh.radius * inst.scale) } else { mesh.cull };
            let cull = crate::quality::scaled_cull_distance(base_cull, self.view_distance_scale);
            let fade_len = if mesh.fade > 0. { mesh.fade * self.view_distance_scale } else { 0. };
            if cull > 0. && d - r >= cull { self.stats.culled_distance += 1; continue; }
            let fade = distance_fade(d, cull, fade_len);
            if fade <= 0. { self.stats.culled_distance += 1; continue; }
            let lod = select_lod(&mesh.limits, d, self.lod_state[i] as usize, LOD_HYSTERESIS);
            self.lod_state[i] = lod as u8;
            let mut g = InstanceGpu {
                // MIRRORFIX: the sign of w carries the local-X mirror (static_common.wgsl)
                pos_scale: [inst.pos.x, inst.pos.y, inst.pos.z, if inst.mirror_x { -inst.scale } else { inst.scale }],
                rot_fade: [inst.yaw.cos(), inst.yaw.sin(), fade, shake_amplitude(inst.shake) * inst.scale],
                wind: [wind_phase(inst.pos), time, inst.sway, 1. / mesh.height.max(0.1)],
            };
            let slot = (inst.mesh.0 * MAX_LODS + lod) * 2 + inst.mirror_x as usize;
            if sphere_in_planes(&planes, 6, center, r) { self.buckets[slot].push(g); } else { self.stats.culled_frustum += 1; }
            // UE (FProjectedShadowInfo::AddSubjectPrimitive) tests casters against the light-space prism of the cascade, not against the camera distance:
            // a tree beyond the shadow distance still throws its shadow into the near cascades.
            // ShadowSetup.cpp:2454 small-caster cull on the primitive bounds (FBoxSphereBounds of the vertices = mesh_bounds, times the instance scale; the
            // sway margin above is this port's addition and is not part of the bounds); the same for every cascade of the view.
            let screen_culled = crate::quality::shadow_caster_screen_size_culled(mesh.radius * inst.scale, (center - cam.eye).length_squared(), radius_threshold, lod_distance_factor);
            if shadows_on && mesh.cast_shadow && !screen_culled {
                g.rot_fade[2] = 1.;
                for c in 0..num_cascades.min(CASCADE_COUNT) {
                    // PERF (2026-10-07): + UE4.16 ShadowSetup.cpp:2416-2438 cylinder / cap / ShadowBoundsAccurate test (lighting::Cascade::caster_in_bounds)
                    if sphere_in_planes(&light_planes[c], 4, center, r) && cascades[c].caster_in_bounds(center, r) { self.buckets[(1 + c) * set_len + slot].push(g); }
                }
            }
        }
        for s in 0..(1 + CASCADE_COUNT) {
            for slot in 0..set_len {
                let b = &self.buckets[s * set_len + slot];
                if b.is_empty() { continue; }
                let (mesh, lod, mirror) = (slot / 2 / MAX_LODS, slot / 2 % MAX_LODS, slot % 2 == 1);
                let gm = &self.meshes[mesh];
                self.groups[s].push(Group { mesh: mesh as u32, lod: lod as u32, first: self.all.len() as u32, count: b.len() as u32, mirror });
                let calls = gm.lods[lod].sections.len() as u32;
                if s == 0 {
                    self.stats.visible += b.len() as u32;
                    self.stats.lod_counts[lod] += b.len() as u32;
                    self.stats.draw_calls += calls;
                    self.stats.triangles += gm.lods[lod].tris as u64 * b.len() as u64;
                } else {
                    self.stats.shadow_instances += b.len() as u32;
                    self.stats.shadow_draw_calls += calls;
                }
                self.all.extend_from_slice(b);
            }
        }
        if self.all.len() > self.inst_capacity {
            self.inst_capacity = self.all.len().next_power_of_two();
            self.inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("static instances"), size: (self.inst_capacity * std::mem::size_of::<InstanceGpu>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        }
        if !self.all.is_empty() { self.queue.write_buffer(&self.inst_buf, 0, bytemuck::cast_slice(&self.all)); }
    }

    /// Main-view draws; bind group 0 (globals) must already be set.
    pub fn draw_scene(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.groups[0].is_empty() { return; }
        pass.set_vertex_buffer(1, self.inst_buf.slice(..));
        let (mut cur_pipe, mut cur_mat) = (usize::MAX, usize::MAX);
        for g in &self.groups[0] {
            let lod = &self.meshes[g.mesh as usize].lods[g.lod as usize];
            pass.set_vertex_buffer(0, lod.vb.slice(..));
            pass.set_index_buffer(lod.ib.slice(..), wgpu::IndexFormat::Uint32);
            for s in &lod.sections {
                let m = &self.materials[s.material];
                let pi = if m.two_sided { 1 } else if g.mirror { 2 } else { 0 };
                if pi != cur_pipe { pass.set_pipeline(&self.scene_pipes[pi]); cur_pipe = pi; }
                if s.material != cur_mat { pass.set_bind_group(1, &m.bg, &[]); cur_mat = s.material; }
                pass.draw_indexed(s.first..s.first + s.count, 0, g.first..g.first + g.count);
            }
        }
    }
    /// PREPASS: the depth pre-pass draws of this frame's visible instances (same groups / order as [`Self::draw_scene`]; group 0 = globals, bound by the caller).
    pub fn draw_depth(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.groups[0].is_empty() { return; }
        pass.set_vertex_buffer(1, self.inst_buf.slice(..));
        let (mut cur_pipe, mut cur_mat) = (usize::MAX, usize::MAX);
        for g in &self.groups[0] {
            let lod = &self.meshes[g.mesh as usize].lods[g.lod as usize];
            pass.set_vertex_buffer(0, lod.vb.slice(..));
            pass.set_index_buffer(lod.ib.slice(..), wgpu::IndexFormat::Uint32);
            for s in &lod.sections {
                let m = &self.materials[s.material];
                let pi = if m.two_sided { 1 } else if g.mirror { 2 } else { 0 };
                if pi != cur_pipe { pass.set_pipeline(&self.depth_pipes[pi]); cur_pipe = pi; }
                if s.material != cur_mat { pass.set_bind_group(1, &m.bg, &[]); cur_mat = s.material; }
                pass.draw_indexed(s.first..s.first + s.count, 0, g.first..g.first + g.count);
            }
        }
    }
    /// OUTLINE: the instances drawn into the custom depth / stencil targets next frame (highlighted building actors); empty = no custom depth pass.
    pub fn set_custom_instances(&mut self, instances: &[StaticMeshInstance]) { self.custom_instances.clear(); self.custom_instances.extend_from_slice(instances); }
    pub fn has_custom(&self) -> bool { !self.custom_instances.is_empty() && !self.meshes.is_empty() }

    /// OUTLINE: the custom depth pipelines writing `stencil` (built on first use / when the value changes).
    fn ensure_custom_pipes(&mut self, stencil: f32) {
        if self.custom_pipes.as_ref().is_some_and(|(s, _)| *s == stencil) { return; }
        let buffers = [
            Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<StaticVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &VERTEX_ATTRS }),
            Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<InstanceGpu>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &INSTANCE_ATTRS }),
        ];
        let constants = [("CUSTOM_STENCIL", stencil as f64)];
        let pipe = |label: &str, cull: Option<wgpu::Face>| self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label), layout: Some(&self.scene_layout),
            vertex: wgpu::VertexState { module: &self.scene_sm, entry_point: Some("vs_static"), compilation_options: Default::default(), buffers: &buffers },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: cull, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: CUSTOM_DEPTH_FORMAT, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &self.scene_sm, entry_point: Some("fs_static_custom"), compilation_options: wgpu::PipelineCompilationOptions { constants: &constants, ..Default::default() }, targets: &[Some(wgpu::ColorTargetState { format: CUSTOM_STENCIL_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
            multiview_mask: None, cache: None,
        });
        let pipes = [pipe("static custom depth (one-sided)", CULL_ONE_SIDED), pipe("static custom depth (two-sided)", None), pipe("static custom depth (one-sided, mirrored)", CULL_ONE_SIDED_REVERSED)];
        self.custom_pipes = Some((stencil, pipes));
    }

    /// OUTLINE: LOD selection (no culling: the list is a few actors) + instance upload of the custom depth instances, pipelines for `stencil`.
    pub fn prepare_custom(&mut self, cam: &CameraParams, stencil: f32) {
        self.custom_groups.clear();
        if !self.has_custom() { return; }
        self.ensure_custom_pipes(stencil);
        let mut list: Vec<(usize, usize, bool, InstanceGpu)> = vec![];
        for inst in &self.custom_instances {
            let Some(mesh) = self.meshes.get(inst.mesh.0) else { continue };
            if mesh.lods.is_empty() { continue; }
            let center = inst.pos + Quat::from_rotation_y(inst.yaw) * (mesh.center * inst.scale);
            let lod = select_lod(&mesh.limits, (center - cam.eye).length(), 0, LOD_HYSTERESIS);
            let g = InstanceGpu {
                pos_scale: [inst.pos.x, inst.pos.y, inst.pos.z, if inst.mirror_x { -inst.scale } else { inst.scale }],
                rot_fade: [inst.yaw.cos(), inst.yaw.sin(), 1., shake_amplitude(inst.shake) * inst.scale],
                wind: [wind_phase(inst.pos), 0., inst.sway, 1. / mesh.height.max(0.1)],
            };
            list.push((inst.mesh.0, lod, inst.mirror_x, g));
        }
        list.sort_by_key(|(m, l, mi, _)| (*m, *l, *mi));
        let mut all = vec![];
        for (m, l, mi, g) in list {
            match self.custom_groups.last_mut() { Some(last) if last.mesh == m as u32 && last.lod == l as u32 && last.mirror == mi => last.count += 1, _ => self.custom_groups.push(Group { mesh: m as u32, lod: l as u32, first: all.len() as u32, count: 1, mirror: mi }) }
            all.push(g);
        }
        if all.len() > self.custom_capacity {
            self.custom_capacity = all.len().next_power_of_two();
            self.custom_buf = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("static custom depth instances"), size: (self.custom_capacity * std::mem::size_of::<InstanceGpu>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        }
        if !all.is_empty() { self.queue.write_buffer(&self.custom_buf, 0, bytemuck::cast_slice(&all)); }
    }

    /// OUTLINE: the custom depth draws (group 0 = the main view's globals, bound by the caller; targets: CUSTOM_DEPTH_FORMAT + CUSTOM_STENCIL_FORMAT).
    pub fn draw_custom(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some((_, pipes)) = self.custom_pipes.as_ref() else { return };
        if self.custom_groups.is_empty() { return; }
        pass.set_vertex_buffer(1, self.custom_buf.slice(..));
        let (mut cur_pipe, mut cur_mat) = (usize::MAX, usize::MAX);
        for g in &self.custom_groups {
            let lod = &self.meshes[g.mesh as usize].lods[g.lod as usize];
            pass.set_vertex_buffer(0, lod.vb.slice(..));
            pass.set_index_buffer(lod.ib.slice(..), wgpu::IndexFormat::Uint32);
            for s in &lod.sections {
                let m = &self.materials[s.material];
                let pi = if m.two_sided { 1 } else if g.mirror { 2 } else { 0 };
                if pi != cur_pipe { pass.set_pipeline(&pipes[pi]); cur_pipe = pi; }
                if s.material != cur_mat { pass.set_bind_group(1, &m.bg, &[]); cur_mat = s.material; }
                pass.draw_indexed(s.first..s.first + s.count, 0, g.first..g.first + g.count);
            }
        }
    }

    /// Shadow-cascade draws (bind group 0 = the cascade's light view-projection is set here).
    /// PERF (report only, `FN_SHADOW_STATS=1`): this frame's draws / triangles of cascade `cascade` as [`Self::draw_shadow`] records them,
    /// split (opaque, masked): [(draws, triangles); 2].
    pub fn shadow_cascade_stats(&self, cascade: usize) -> [(u64, u64); 2] {
        let mut out = [(0u64, 0u64); 2];
        for g in self.groups.get(1 + cascade).map(|v| v.as_slice()).unwrap_or(&[]) {
            for s in &self.meshes[g.mesh as usize].lods[g.lod as usize].sections {
                let k = self.materials[s.material].masked as usize;
                out[k].0 += 1;
                out[k].1 += s.count as u64 / 3 * g.count as u64;
            }
        }
        out
    }
    pub fn draw_shadow(&self, pass: &mut wgpu::RenderPass<'_>, cascade: usize, vp_bg: &wgpu::BindGroup) {
        let groups = &self.groups[1 + cascade];
        if groups.is_empty() { return; }
        pass.set_bind_group(0, vp_bg, &[]);
        pass.set_vertex_buffer(1, self.inst_buf.slice(..));
        let (mut cur_pipe, mut cur_mat) = (usize::MAX, usize::MAX);
        for g in groups {
            let lod = &self.meshes[g.mesh as usize].lods[g.lod as usize];
            pass.set_vertex_buffer(0, lod.vb.slice(..));
            pass.set_index_buffer(lod.ib.slice(..), wgpu::IndexFormat::Uint32);
            for s in &lod.sections {
                let m = &self.materials[s.material];
                let pi = m.masked as usize + 2 * if m.two_sided { 1 } else if g.mirror { 2 } else { 0 };
                if pi != cur_pipe { pass.set_pipeline(&self.shadow_pipes[pi]); cur_pipe = pi; }
                if s.material != cur_mat { pass.set_bind_group(1, &m.bg, &[]); cur_mat = s.material; }
                pass.draw_indexed(s.first..s.first + s.count, 0, g.first..g.first + g.count);
            }
        }
    }
}
