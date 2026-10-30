//! VOLUMETRIC FOG HOOK: the GPU passes of UE 4.16's volumetric fog (live time-of-day phase, sunset Evening by default), ported from the disassembled 1.8 global shaders
//! (`shaders/volfog.wgsl`, constants and CPU ports in [`crate::fog`]).
//!
//! Per frame (all inside `Renderer::render_scene`, only when `RenderOptions::fog` is on):
//! 1. `vfog` compute pass: `material_setup` (VBufferA = global height-fog density split into scattering / absorption), `scatter` (directional light with the
//!    renderer's sun cascades + the flat sky-light term, Henyey-Greenstein phase, temporal history reprojection with weight 0.9), `integrate` (front-to-back
//!    integration to (light rgb, transmittance) per slice, `FVolumetricFogFinalIntegrationCS`).
//! 2. `vfog composite` render pass right after the opaque scene: the scene depth selects the slice and the HDR target is blended with
//!    `dst * V.a + V.rgb` (fixed-function blend; the HDR alpha, the ambient share used by the AO pass, is scaled by V.a too).
//!
//! The grid is `r.VolumetricFog.GridPixelSize` px squared x `GridSizeZ` slices of the shadow scalability level (8 x 128 at Epic, 16 x 64 at High), exponentially distributed over the near plane .. 4500 cm
//! (`VolumetricFogDistance`), all stored as `Rgba16Float` exactly like the engine's PF_FloatRGBA volumes.
//!
//! What the recovered values do (measured by the tests): the density written by the material setup is `FogDensity / 1000 * 0.5 = 1.1e-7 /cm` AT the fog plane
//! (650 m up) and 57.7x that at ground level (`2^(65000 * 9e-5)`), i.e. ~6.4e-6 /cm: the volume dims the scene by ~3-4 % over its 45 m. The scattering
//! (`* ScatteringScale 0.1`, ~6e-7 /cm at ground level) times the in-scattered light lands in the half-float subnormal range, so the in-scattered
//! colour of the volume is ~1e-4 of the scene (quantisation noise): visually it is the small transmittance dim only. The passes are complete and
//! parameter-driven ([`VolFogParams`]).
use crate::camera::CameraParams;
use crate::fog;
use bytemuck::{Pod, Zeroable};
use glam::Mat4;

pub const VOLUME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Runtime parameters of the volume (defaults = the recovered Athena Day values; tests override them).
#[derive(Clone, Copy, Debug)]
pub struct VolFogParams {
    /// Density / falloff after the engine's /1000 remap.
    pub density: f32,
    pub falloff: f32,
    pub fog_height_cm: f32,
    pub scattering_distribution: f32,
    pub scattering_scale: f32,
    pub absorption_scale: f32,
    pub distance_cm: f32,
    /// Directional light colour after the override rule (luma(light) * DirectionalInscatteringColor), and its `VolumetricScatteringIntensity`.
    pub dir_color: [f32; 3],
    pub dir_intensity: f32,
    pub dir_enabled: bool,
    /// Flat sky term (FogInscatteringColor * Y00) and `SkyLightVolumetricScatteringIntensity`.
    pub sky_color: [f32; 3],
    pub sky_intensity: f32,
    pub history_weight: f32,
    pub jitter: bool,
}
impl VolFogParams {
    /// Volume parameters for a time-of-day phase (see `fog::PhaseFog`); the light colour is the phase's own LightColor * Brightness.
    pub fn from_phase(p: &fog::PhaseFog) -> Self {
        let l = fog::ue_luma(p.sun_light_rgb());
        VolFogParams {
            density: p.fog_density * fog::engine::DENSITY_REMAP, falloff: p.fog_height_falloff * fog::engine::DENSITY_REMAP,
            fog_height_cm: fog::day::HEIGHT_FOG_Z_OFFSET, scattering_distribution: p.vol_scattering_distribution,
            scattering_scale: p.vol_scattering_scale, absorption_scale: p.vol_absorption_scale,
            distance_cm: fog::day::VOLUMETRIC_FOG_DISTANCE,
            dir_color: p.dir_color.map(|c| l * c), dir_intensity: p.sun_vol_intensity, dir_enabled: true,
            sky_color: fog::sky_scatter_override(p.fog_color, 1.0), sky_intensity: p.sky_vol_intensity,
            history_weight: fog::engine::HISTORY_WEIGHT, jitter: fog::engine::JITTER,
        }
    }
    pub fn athena_day() -> Self { Self::from_phase(&fog::PhaseFog::DAY) }
    /// The phase the renderer shows (live time-of-day clock, sunset by default).
    /// FOG HEIGHT RE-BASE (2026-10-07, MATCH track): the fog plane in the renderer's frame (`fog::fog_height_cm`, absolute HeightFogZOffset - ground ref).
    pub fn active() -> Self { Self { fog_height_cm: fog::fog_height_cm(fog::world_z_ref_cm()), ..Self::from_phase(&fog::active()) } }
}

/// `VolumetricFogTemporalRandom` (recalled engine C++, the shader only adds the value to the integer grid coordinate): Halton(2, 3, 5), or the voxel centre without jitter.
pub fn jitter_offsets(frame: u64, jitter: bool) -> [f32; 3] {
    fn halton(mut i: u64, base: u64) -> f32 {
        let (mut r, inv, mut f) = (0f64, 1.0 / base as f64, 1.0 / base as f64);
        while i > 0 { r += (i % base) as f64 * f; i /= base; f *= inv; }
        r as f32
    }
    if !jitter { return [0.5; 3]; }
    let i = frame & 1023;
    [halton(i, 2), halton(i, 3), halton(i, 5)]
}

/// The directional light function of the volumetric fog is rendered into a light-space texture (`RenderLightFunctionForVolumetricFog`,
/// VolumetricFogLightFunction.cpp:156-180): a whole-scene projection around `GetShadowSplitBoundsDepthRange(View, ViewOrigin, 0, VolumetricFogMaxDistance)`,
/// each texel evaluated at `SceneDepth = 0.5` of that projection (VolumetricFogLightFunction.usf:17-21). The depth range is symmetric around the sphere centre
/// (MaxSubjectZ = max(W, 5000), MinSubjectZ = -MaxSubjectZ, SetupWholeSceneProjection), so depth 0.5 is the plane through the centre perpendicular to the light
/// (the 4-texel origin snap only moves the centre inside that plane). A voxel reads the texel it projects onto along the light, so its `ViewDistance` fade
/// (usf:45) uses the point of that plane. Returns the sphere centre in world metres. (The G8 texture's 8-bit quantisation and bilinear texel filtering
/// are not reproduced: the Athena light function is a constant colour, only this distance fade varies.)
pub fn light_function_plane_point(cam: &CameraParams, volumetric_fog_distance_cm: f32) -> glam::Vec3 {
    let (tx, ty) = cam.tan_half();
    let (centre_z, _) = crate::lighting::split_sphere(tx, tx / ty, 0., volumetric_fog_distance_cm * 0.01);
    cam.eye + cam.forward.normalize() * centre_z
}

/// GPU block (struct VFog in volfog.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct VFogUniform {
    pub grid: [f32; 4],
    pub z_params: [f32; 4],
    pub jitter: [f32; 4],
    pub fog: [f32; 4],
    pub scale: [f32; 4],
    pub dir_color: [f32; 4],
    pub sky_color: [f32; 4],
    pub prev_view_proj: [[f32; 4]; 4],
    /// xyz: world point (metres) on the light-function plane (centre of the `GetShadowSplitBoundsDepthRange(0, VolumetricFogMaxDistance)` sphere), w unused.
    pub lf_plane: [f32; 4],
}

/// Everything `prepare` needs from the frame.
pub struct VolFogFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub camera: &'a CameraParams,
    pub frame_index: u64,
    pub shadow_view: &'a wgpu::TextureView,
    pub shadow_size: u32,
    pub depth_view: &'a wgpu::TextureView,
    /// Changes whenever the scene depth texture is recreated.
    pub depth_gen: u64,
    /// `r.VolumetricFog.GridPixelSize` / `r.VolumetricFog.GridSizeZ` of the shadow scalability level (`quality::ShadowScalability`).
    pub grid_pixel_size: u32,
    pub grid_size_z: u32,
}

struct Volume {
    grid: [u32; 3],
    vbuf_a: wgpu::Texture,
    light: [wgpu::Texture; 2],
    integrated: wgpu::Texture,
    setup_bg: wgpu::BindGroup,
    scatter_bg: [Option<wgpu::BindGroup>; 2],
    integrate_bg: [wgpu::BindGroup; 2],
    composite_bg: Option<wgpu::BindGroup>,
    key: (u32, u64),
}

pub struct VolFog {
    device: wgpu::Device,
    pub params: VolFogParams,
    setup_pipe: wgpu::ComputePipeline,
    scatter_pipe: wgpu::ComputePipeline,
    integrate_pipe: wgpu::ComputePipeline,
    composite_pipe: wgpu::RenderPipeline,
    bgl_setup: wgpu::BindGroupLayout,
    bgl_scatter: wgpu::BindGroupLayout,
    bgl_integrate: wgpu::BindGroupLayout,
    bgl_composite: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    camera_buf: wgpu::Buffer,
    light_buf: wgpu::Buffer,
    cmp_sampler: wgpu::Sampler,
    lin_sampler: wgpu::Sampler,
    volume: Option<Volume>,
    prev_view_proj: Option<Mat4>,
    last_frame: Option<u64>,
    /// Clock hour the params were last derived from (params follow `time_of_day::hour()` when it changes; tests that set `params` by hand are unaffected).
    last_hour: f32,
    /// FOG HEIGHT RE-BASE (MATCH track): `fog::world_z_ref_cm` the params were last derived with (the backdrop installs it after `new`).
    last_z_ref: f32,
    parity: usize,
    active: bool,
    /// FOG INPUTS (2026-10-05): what translucent programs need of this frame's volume ([`Self::view_constants`], [`Self::integrated_view`]): the serial of the
    /// current volume textures (bumped when they are recreated), `r.VolumetricFog.GridPixelSize` and the grid Z params of the last `prepare`.
    serial: u64,
    grid_pixel_size: u32,
    z_params: [f32; 3],
    hdr_format: wgpu::TextureFormat,
}

const C: wgpu::ShaderStages = wgpu::ShaderStages::COMPUTE;
const F: wgpu::ShaderStages = wgpu::ShaderStages::FRAGMENT;
fn e_uniform(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }
}
fn e_tex3d(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false }, count: None }
}
fn e_store3d(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: C, ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: VOLUME_FORMAT, view_dimension: wgpu::TextureViewDimension::D3 }, count: None }
}
fn e_depth(binding: u32, vis: wgpu::ShaderStages, dim: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: dim, multisampled: false }, count: None }
}
fn e_sampler(binding: u32, vis: wgpu::ShaderStages, ty: wgpu::SamplerBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Sampler(ty), count: None }
}
fn b_buf(binding: u32, b: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> { wgpu::BindGroupEntry { binding, resource: b.as_entire_binding() } }
fn b_tex(binding: u32, v: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> { wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(v) } }
fn b_samp(binding: u32, s: &wgpu::Sampler) -> wgpu::BindGroupEntry<'_> { wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::Sampler(s) } }

fn volfog_pipelines(device: &wgpu::Device, hdr_format: wgpu::TextureFormat, bgl_setup: &wgpu::BindGroupLayout, bgl_scatter: &wgpu::BindGroupLayout, bgl_integrate: &wgpu::BindGroupLayout, bgl_composite: &wgpu::BindGroupLayout) -> (wgpu::ComputePipeline, wgpu::ComputePipeline, wgpu::ComputePipeline, wgpu::RenderPipeline) {
    let sm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("volumetric fog"), source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(fn_live::wgsl!("../shaders/volfog.wgsl"))) });
    let pl = |label: &str, b: &wgpu::BindGroupLayout| device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[Some(b)], immediate_size: 0 });
    let cp = |label: &str, entry: &str, b: &wgpu::BindGroupLayout| device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(label), layout: Some(&pl(label, b)), module: &sm, entry_point: Some(entry), compilation_options: Default::default(), cache: None });
    let setup_pipe = cp("vfog material setup", "material_setup", bgl_setup);
    let scatter_pipe = cp("vfog light scattering", "scatter", bgl_scatter);
    let integrate_pipe = cp("vfog final integration", "integrate", bgl_integrate);
    let one_src_alpha = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::SrcAlpha, operation: wgpu::BlendOperation::Add };
    let alpha_scale = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::SrcAlpha, operation: wgpu::BlendOperation::Add };
    let composite_layout = pl("vfog composite", bgl_composite);
    let composite_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("vfog composite"), layout: Some(&composite_layout),
        vertex: wgpu::VertexState { module: &sm, entry_point: Some("vs_composite"), compilation_options: Default::default(), buffers: &[] },
        primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &sm, entry_point: Some("fs_composite"), compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: hdr_format, blend: Some(wgpu::BlendState { color: one_src_alpha, alpha: alpha_scale }), write_mask: wgpu::ColorWrites::ALL })] }),
        multiview_mask: None, cache: None,
    });
    (setup_pipe, scatter_pipe, integrate_pipe, composite_pipe)
}

impl VolFog {
    pub fn new(device: &wgpu::Device, camera_buf: &wgpu::Buffer, light_buf: &wgpu::Buffer, hdr_format: wgpu::TextureFormat) -> VolFog {
        let layout = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries });
        let bgl_setup = layout("vfog setup bgl", &[e_uniform(0, C), e_uniform(1, C), e_store3d(9)]);
        let bgl_scatter = layout("vfog scatter bgl", &[
            e_uniform(0, C), e_uniform(1, C), e_uniform(2, C), e_depth(3, C, wgpu::TextureViewDimension::D2Array), e_sampler(4, C, wgpu::SamplerBindingType::Comparison),
            e_sampler(5, C, wgpu::SamplerBindingType::Filtering), e_tex3d(6, C), e_tex3d(7, C), e_store3d(8),
        ]);
        let bgl_integrate = layout("vfog integrate bgl", &[e_uniform(0, C), e_uniform(1, C), e_store3d(10), e_tex3d(11, C)]);
        let bgl_composite = layout("vfog composite bgl", &[e_uniform(0, F), e_uniform(1, F), e_sampler(5, F, wgpu::SamplerBindingType::Filtering), e_tex3d(12, F), e_depth(13, F, wgpu::TextureViewDimension::D2)]);
        let (setup_pipe, scatter_pipe, integrate_pipe, composite_pipe) = volfog_pipelines(device, hdr_format, &bgl_setup, &bgl_scatter, &bgl_integrate, &bgl_composite);
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("vfog params"), size: std::mem::size_of::<VFogUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let cmp_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("vfog shadow compare"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, compare: Some(wgpu::CompareFunction::LessEqual), ..Default::default() });
        let lin_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("vfog volume"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        VolFog {
            device: device.clone(), params: VolFogParams::active(), setup_pipe, scatter_pipe, integrate_pipe, composite_pipe, bgl_setup, bgl_scatter, bgl_integrate, bgl_composite,
            uniform_buf, camera_buf: camera_buf.clone(), light_buf: light_buf.clone(), cmp_sampler, lin_sampler, volume: None, prev_view_proj: None, last_frame: None, last_hour: crate::time_of_day::hour(), last_z_ref: fog::world_z_ref_cm(), parity: 0, active: false,
            serial: 0, grid_pixel_size: fog::engine::GRID_PIXEL_SIZE, z_params: [0.0; 3],
            hdr_format,
        }
    }

    pub fn rebuild_pipelines(&mut self) -> Result<(), String> {
        let (a, b, c, d) = crate::renderer::checked_build(&self.device, || volfog_pipelines(&self.device, self.hdr_format, &self.bgl_setup, &self.bgl_scatter, &self.bgl_integrate, &self.bgl_composite))?;
        (self.setup_pipe, self.scatter_pipe, self.integrate_pipe, self.composite_pipe) = (a, b, c, d);
        Ok(())
    }

    /// FOG INPUTS: `View.VolumetricFogInvGridSize` / `VolumetricFogGridZParams` / `VolumetricFogSVPosToVolumeUV` + `VolumetricFogMaxDistance` (float4s 175..177)
    /// as `FViewInfo::SetupVolumetricFogUniformBufferParameters` sets them (VolumetricFog.cpp:841-863) for the volume this renderer built this frame (the
    /// grid Z params are the ones its passes use); all zero when the volumetric fog did not run (:859-862).
    pub fn view_constants(&self) -> [[f32; 4]; 3] {
        let Some(v) = self.volume.as_ref().filter(|_| self.active) else { return [[0.0; 4]; 3] };
        let g = v.grid.map(|x| x.max(1) as f32);
        let px = self.grid_pixel_size.max(1) as f32;
        [[1.0 / g[0], 1.0 / g[1], 1.0 / g[2], 0.0], [self.z_params[0], self.z_params[1], self.z_params[2], 0.0], [1.0 / (g[0] * px), 1.0 / (g[1] * px), self.params.distance_cm, 0.0]]
    }
    /// FOG INPUTS: this frame's integrated light-scattering volume (`View.VolumetricFogResources.IntegratedLightScattering`) and the serial of the volume
    /// textures it belongs to; `None` when the volumetric fog did not run.
    pub fn integrated_view(&self) -> Option<(wgpu::TextureView, u64)> { self.volume.as_ref().filter(|_| self.active).map(|v| (v.integrated.create_view(&Default::default()), self.serial)) }

    /// True once `prepare` ran for the frame being recorded (the renderer adds the passes only then).
    pub fn active(&self) -> bool { self.active }
    /// Called instead of `prepare` for a frame without fog: the next frame must not reproject the stale history.
    pub fn skip_frame(&mut self) { self.active = false; self.last_frame = None; }
    pub fn grid(&self) -> Option<[u32; 3]> { self.volume.as_ref().map(|v| v.grid) }

    fn create_volume(&self, grid: [u32; 3], key: (u32, u64)) -> Volume {
        let d = &self.device;
        let mk = |label: &str| d.create_texture(&wgpu::TextureDescriptor {
            label: Some(label), size: wgpu::Extent3d { width: grid[0], height: grid[1], depth_or_array_layers: grid[2] }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D3, format: VOLUME_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
        });
        let (vbuf_a, l0, l1, integrated) = (mk("vfog VBufferA"), mk("vfog light scattering 0"), mk("vfog light scattering 1"), mk("vfog integrated light scattering"));
        let v = |t: &wgpu::Texture| t.create_view(&Default::default());
        let setup_bg = d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("vfog setup bg"), layout: &self.bgl_setup, entries: &[b_buf(0, &self.uniform_buf), b_buf(1, &self.camera_buf), b_tex(9, &v(&vbuf_a))] });
        let integrate_bg = [&l0, &l1].map(|l| d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("vfog integrate bg"), layout: &self.bgl_integrate, entries: &[
            b_buf(0, &self.uniform_buf), b_buf(1, &self.camera_buf), b_tex(10, &v(&integrated)), b_tex(11, &v(l)),
        ] }));
        Volume { grid, vbuf_a, light: [l0, l1], integrated, setup_bg, scatter_bg: [None, None], integrate_bg, composite_bg: None, key }
    }

    /// Uploads the frame's parameters and (re)builds the volume / bind groups when the viewport, the shadow map or the scene depth changed.
    pub fn prepare(&mut self, queue: &wgpu::Queue, f: &VolFogFrame) {
        let hour = crate::time_of_day::hour();
        if hour != self.last_hour || fog::world_z_ref_cm() != self.last_z_ref { self.last_hour = hour; self.last_z_ref = fog::world_z_ref_cm(); self.params = VolFogParams::active(); }
        let grid = fog::grid_size_for(f.width, f.height, f.grid_pixel_size, f.grid_size_z);
        let key = (f.shadow_size, f.depth_gen);
        let fresh = !matches!(&self.volume, Some(v) if v.grid == grid);
        if fresh { self.volume = Some(self.create_volume(grid, key)); self.last_frame = None; self.serial += 1; }
        self.grid_pixel_size = f.grid_pixel_size; // FOG INPUTS
        let consecutive = self.last_frame.map_or(false, |l| l + 1 == f.frame_index);
        self.parity = (f.frame_index & 1) as usize;
        let vol = self.volume.as_mut().unwrap();
        if fresh || vol.key != key || vol.scatter_bg[0].is_none() || vol.composite_bg.is_none() {
            let d = &self.device;
            let v = |t: &wgpu::Texture| t.create_view(&Default::default());
            let (va, l0, l1, ig) = (v(&vol.vbuf_a), v(&vol.light[0]), v(&vol.light[1]), v(&vol.integrated));
            let lights = [&l0, &l1];
            vol.scatter_bg = [0usize, 1].map(|c| Some(d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("vfog scatter bg"), layout: &self.bgl_scatter, entries: &[
                b_buf(0, &self.uniform_buf), b_buf(1, &self.camera_buf), b_buf(2, &self.light_buf), b_tex(3, f.shadow_view), b_samp(4, &self.cmp_sampler), b_samp(5, &self.lin_sampler),
                b_tex(6, &va), b_tex(7, lights[1 - c]), b_tex(8, lights[c]),
            ] })));
            vol.composite_bg = Some(d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("vfog composite bg"), layout: &self.bgl_composite, entries: &[
                b_buf(0, &self.uniform_buf), b_buf(1, &self.camera_buf), b_samp(5, &self.lin_sampler), b_tex(12, &ig), b_tex(13, f.depth_view),
            ] }));
            vol.key = key;
        }
        let p = &self.params;
        let near_cm = f.camera.near * 100.0;
        let z = fog::grid_z_params(near_cm, p.distance_cm, grid[2], fog::engine::DEPTH_DISTRIBUTION_SCALE);
        self.z_params = z; // FOG INPUTS
        let j = jitter_offsets(f.frame_index, p.jitter);
        let u = VFogUniform {
            grid: [grid[0] as f32, grid[1] as f32, grid[2] as f32, if consecutive && self.prev_view_proj.is_some() { 1.0 } else { 0.0 }],
            z_params: [z[0], z[1], z[2], near_cm],
            jitter: [j[0], j[1], j[2], p.history_weight],
            fog: [p.density, p.falloff, p.fog_height_cm, p.scattering_distribution],
            scale: [p.scattering_scale, p.absorption_scale, if p.dir_enabled { 1.0 } else { 0.0 }, 0.0],
            dir_color: [p.dir_color[0], p.dir_color[1], p.dir_color[2], p.dir_intensity],
            sky_color: [p.sky_color[0], p.sky_color[1], p.sky_color[2], p.sky_intensity],
            prev_view_proj: self.prev_view_proj.unwrap_or(Mat4::IDENTITY).to_cols_array_2d(),
            lf_plane: { let c = light_function_plane_point(f.camera, p.distance_cm); [c.x, c.y, c.z, 0.] },
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
        self.prev_view_proj = Some(f.camera.proj_matrix() * f.camera.view_matrix());
        self.last_frame = Some(f.frame_index);
        self.active = true;
    }

    /// The three compute dispatches (record them inside ONE compute pass: wgpu scopes storage usage per dispatch).
    pub fn encode_compute(&self, pass: &mut wgpu::ComputePass<'_>) {
        let Some(v) = &self.volume else { return };
        let g = v.grid;
        pass.set_pipeline(&self.setup_pipe);
        pass.set_bind_group(0, &v.setup_bg, &[]);
        pass.dispatch_workgroups(g[0].div_ceil(4), g[1].div_ceil(4), g[2].div_ceil(4));
        pass.set_pipeline(&self.scatter_pipe);
        pass.set_bind_group(0, v.scatter_bg[self.parity].as_ref(), &[]);
        pass.dispatch_workgroups(g[0].div_ceil(4), g[1].div_ceil(4), g[2].div_ceil(4));
        pass.set_pipeline(&self.integrate_pipe);
        pass.set_bind_group(0, &v.integrate_bg[self.parity], &[]);
        pass.dispatch_workgroups(g[0].div_ceil(8), g[1].div_ceil(8), 1);
    }

    /// `dst * V.a + V.rgb` over the whole viewport (the pass must have no depth attachment: the scene depth is sampled).
    pub fn encode_composite(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(v) = &self.volume else { return };
        pass.set_pipeline(&self.composite_pipe);
        pass.set_bind_group(0, v.composite_bg.as_ref(), &[]);
        pass.draw(0..3, 0..1);
    }
}

// =================================================================================================================================================
// EXTERNAL LIGHT LIST (2026-10-07, additive entry point for the BR lobby, fn-app `lobby_scene.rs`): the same UE 4.16 volumetric fog for a renderer
// with its own camera, light list and shadow map, in UE world space (cm, Z up). Ported from the 1.8 global shaders (the typed-UAV programs do not
// translate yet, so the maths is a hand port of their disassembly, `fn-dxbc disasm`):
// * `FVolumetricFogMaterialSetupCS` #751700: as [`VolFog`] (`fog::material_setup`).
// * `TVolumetricFogLightScatteringCS<true,false>` #314004: directional light (one shadow map, point sampled, lit outside it; no light function), sky
//   light (the SH branch, `bOverrideLightColorsWithFogInscatteringColors` off: `cb0[80].x * max(dot(SkySH, (1, g*V.y, g*V.z, g*V.x)), 0) * SkyLightColor / pi`),
//   and the forward-light-grid loop over the local lights (`ForwardLocalLightBuffer` t3, 4 float4 per light; the grid only culls, so every light is
//   looped here): VolumetricScatteringIntensity = high half of `SpotAnglesAndSourceRadiusPacked.z` (`f16tof32(z >> 16)`, skipped when <= 0), inverse squared
//   `1 / (d^2 + bias^2)` with bias = `max(|cell(id + 1 + jitter) - cell(id + jitter)| * InverseSquaredLightDistanceBiasScale, 1)` (or the capsule term when
//   SourceLength > 0) times `saturate(1 - (d^2 / R^2)^2)^2`, legacy `pow(1 - saturate(|d / R|^2), FalloffExponent)`, spot `saturate((dot(L, -forward) - cos outer)
//   * inv range)^2`, then `colour * attenuation * HG(g, dot(L, -V)) * VSI`.
// * Shadow-casting local lights: `TInjectShadowedLocalLightPS<*,true>` #489730 is the same attenuation / phase / VSI times the light's shadow; the external
//   renderer has no shadow maps for them, so they are looped unshadowed here (the caller labels that).
// * `FVolumetricFogFinalIntegrationCS` #689183 and the composite (CombineVolumetricFog: dst * V.a + V.rgb): as [`VolFog`].
// =================================================================================================================================================

/// One local light as `FForwardLocalLightData` packs it for the light scattering CS.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct ExtLight {
    /// xyz world position (cm), w 1 / AttenuationRadius.
    pub pos_inv_radius: [f32; 4],
    /// rgb the colour the light setup uploads (x16 for inverse-squared lights), w LightFalloffExponent (0 = inverse squared).
    pub color_falloff: [f32; 4],
    /// xyz the light's forward (travel) direction, w unused.
    pub forward: [f32; 4],
    /// x cos(outer cone) (-2 = point light: no spot term), y 1 / (cos inner - cos outer), z SourceLength (cm), w VolumetricScatteringIntensity.
    pub spot: [f32; 4],
}

/// Parameters of the external volume (all values as the engine uploads them: density / falloff after the /1000 remap).
#[derive(Clone, Copy, Debug)]
pub struct ExtVolFogParams {
    pub density: f32,
    pub falloff: f32,
    pub fog_height_cm: f32,
    pub scattering_distribution: f32,
    pub scattering_scale: f32,
    pub absorption_scale: f32,
    pub distance_cm: f32,
    /// Unit vector towards the directional light (UE axes), its colour (LightColor * Intensity, linear) and `VolumetricScatteringIntensity`.
    pub to_light: [f32; 3],
    pub dir_color: [f32; 3],
    pub dir_intensity: f32,
    pub dir_enabled: bool,
    /// The sky light's 2-band SH per channel (`FSHVectorRGB3::V[0..4]` of the captured `IrradianceEnvironmentMap`, UE basis, see [`ue_two_band_sh`]) times
    /// `SkyLightColor`, and its `VolumetricScatteringIntensity`.
    pub sky_sh: [[f32; 4]; 3],
    pub sky_intensity: f32,
    pub history_weight: f32,
    pub jitter: bool,
    /// `r.VolumetricFog.InverseSquaredLightDistanceBiasScale` (cb0[75].x).
    pub inv_sq_bias_scale: f32,
}

/// Everything [`ExtVolFog::prepare`] needs from the external frame (UE world space, cm).
pub struct ExtVolFogFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub eye: glam::Vec3,
    pub forward: glam::Vec3,
    pub right: glam::Vec3,
    pub up: glam::Vec3,
    /// tan of the half field of view (x, y); near / far plane (cm) of the scene depth (wgpu 0..1, finite far, `clip.w` = view depth).
    pub tan_half: (f32, f32),
    pub near_cm: f32,
    pub far_cm: f32,
    /// World -> clip of this frame (the next frame's history reprojection).
    pub view_proj: Mat4,
    pub frame_index: u64,
    pub depth_view: &'a wgpu::TextureView,
    /// Bump whenever `depth_view` or the shadow view is recreated.
    pub depth_gen: u64,
    /// The directional light's shadow map (Depth 2D), world -> shadow clip (uv = (x * 0.5 + 0.5, 0.5 - y * 0.5), depth z), its size and the depth bias
    /// subtracted from the stored depth (`cb2[29].x`).
    pub shadow: Option<(&'a wgpu::TextureView, Mat4, u32, f32)>,
    pub lights: &'a [ExtLight],
    pub grid_pixel_size: u32,
    pub grid_size_z: u32,
    pub voxel: Option<ExtVoxelFrame<'a>>,
    pub device_z: [f32; 2],
}

/// GPU block (struct XV in [`EXT_WGSL`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ExtVFogUniform {
    pub grid: [f32; 4],
    pub z_params: [f32; 4],
    pub jitter: [f32; 4],
    pub fog: [f32; 4],
    pub scale: [f32; 4],
    pub eye: [f32; 4],
    pub fwd: [f32; 4],
    pub right: [f32; 4],
    pub up: [f32; 4],
    pub depth: [f32; 4],
    pub to_light: [f32; 4],
    pub dir_color: [f32; 4],
    pub sky_r: [f32; 4],
    pub sky_g: [f32; 4],
    pub sky_b: [f32; 4],
    pub sky: [f32; 4],
    pub shadow_vp: [[f32; 4]; 4],
    pub prev_view_proj: [[f32; 4]; 4],
}

pub const EXT_MAX_LIGHTS: usize = 256;

pub const EXT_WGSL: &str = r#"
struct XV {
    grid: vec4<f32>,       // gx, gy, gz, history valid
    z_params: vec4<f32>,   // B, O, S, near (cm)
    jitter: vec4<f32>,     // xyz cell offset (0.5 = centre), w history weight
    fog: vec4<f32>,        // density, falloff (after /1000), fog height (cm), scattering distribution g
    scale: vec4<f32>,      // scattering scale, absorption scale, directional light enabled, local light count
    eye: vec4<f32>,        // camera (cm), w InverseSquaredLightDistanceBiasScale
    fwd: vec4<f32>,        // w tan half fov x
    right: vec4<f32>,      // w tan half fov y
    up: vec4<f32>,
    depth: vec4<f32>,      // near, far (cm), viewport w, h
    to_light: vec4<f32>,   // w shadow map bound
    dir_color: vec4<f32>,  // w VolumetricScatteringIntensity
    sky_r: vec4<f32>, sky_g: vec4<f32>, sky_b: vec4<f32>,
    sky: vec4<f32>,        // x SkyLightVolumetricScatteringIntensity, y shadow map size, z shadow depth bias
    shadow_vp: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
}
struct XL { pos: vec4<f32>, color: vec4<f32>, fwd: vec4<f32>, spot: vec4<f32> }
@group(0) @binding(0) var<uniform> xv: XV;
@group(0) @binding(1) var<storage, read> xl: array<XL>;
@group(0) @binding(3) var shadow_map: texture_depth_2d;
@group(0) @binding(5) var lin_samp: sampler;
@group(0) @binding(6) var vbuf_a_in: texture_3d<f32>;
@group(0) @binding(7) var history_in: texture_3d<f32>;
@group(0) @binding(8) var light_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(9) var vbuf_a_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(10) var integrated_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(11) var light_in: texture_3d<f32>;
@group(0) @binding(12) var integrated_in: texture_3d<f32>;
@group(0) @binding(13) var scene_depth: texture_depth_2d;

const LN2_RCP: f32 = 1.442695;
fn slice_depth_cm(s: f32) -> f32 { return (exp2(s / xv.z_params.z) - xv.z_params.y) / xv.z_params.x; }
fn depth_to_w(d: f32) -> f32 { return log2(max(d * xv.z_params.x + xv.z_params.y, 1e-30)) * xv.z_params.z / xv.grid.z; }
fn cell_world(uv: vec2<f32>, d: f32) -> vec3<f32> {
    let ndc = (uv * 2.0 - 1.0) * vec2<f32>(1.0, -1.0);
    return xv.eye.xyz + xv.fwd.xyz * d + xv.right.xyz * (ndc.x * xv.fwd.w * d) + xv.up.xyz * (ndc.y * xv.right.w * d);
}
fn cell_at(c: vec3<f32>) -> vec3<f32> { return cell_world(c.xy / xv.grid.xy, slice_depth_cm(c.z)); }
fn ue_luma(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.3, 0.59, 0.11)); }
fn hg_phase(g: f32, cos_theta: f32) -> f32 { return (1.0 - g * g) / (12.566371 * pow(1.0 + g * g + 2.0 * g * cos_theta, 1.5)); }
fn in_grid(id: vec3<u32>) -> bool { return all(id < vec3<u32>(u32(xv.grid.x), u32(xv.grid.y), u32(xv.grid.z))); }

@compute @workgroup_size(4, 4, 4)
fn x_material_setup(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_grid(id)) { return; }
    let w = cell_at(vec3<f32>(id) + 0.5);
    let d = exp2((w.z - xv.fog.z) * -xv.fog.y) * (xv.fog.x * 0.5);
    textureStore(vbuf_a_out, vec3<i32>(id), vec4<f32>(vec3<f32>(d * xv.scale.x), d * xv.scale.y));
}

// one shadow map, point sampled: lit outside it, else when receiver < stored - bias
fn x_dir_visibility(world: vec3<f32>) -> f32 {
    if (xv.to_light.w < 0.5) { return 1.0; }
    let p = xv.shadow_vp * vec4<f32>(world, 1.0);
    let uv = vec2<f32>(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) || p.z > 1.0 || p.z < 0.0) { return 1.0; }
    let d = textureLoad(shadow_map, vec2<i32>(floor(uv * xv.sky.y)), 0);
    return select(0.0, 1.0, p.z < d - xv.sky.z);
}

@compute @workgroup_size(4, 4, 4)
fn x_scatter(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_grid(id)) { return; }
    let c0 = vec3<f32>(id) + xv.jitter.xyz;
    let world = cell_at(c0);
    let ray = normalize(world - xv.eye.xyz);
    let g = xv.fog.w;
    var lighting = vec3<f32>(0.0);
    if (xv.scale.z > 0.5) {
        lighting = lighting + xv.dir_color.rgb * (x_dir_visibility(world) * xv.dir_color.w * hg_phase(g, dot(xv.to_light.xyz, -ray)));
    }
    if (xv.sky.x > 0.0) {
        let h = vec4<f32>(1.0, ray.y * g, ray.z * g, ray.x * g);
        lighting = lighting + xv.sky.x * max(vec3<f32>(dot(xv.sky_r, h), dot(xv.sky_g, h), dot(xv.sky_b, h)), vec3<f32>(0.0)) * 0.318310;
    }
    // the forward light grid loop
    let bias = max(length(cell_at(c0 + 1.0) - world) * xv.eye.w, 1.0);
    let n = u32(xv.scale.w);
    for (var i = 0u; i < n; i = i + 1u) {
        let l = xl[i];
        if (l.spot.w <= 0.0) { continue; }
        let tl = l.pos.xyz - world;
        let d2 = dot(tl, tl);
        if (d2 * l.pos.w * l.pos.w >= 1.0) { continue; }
        let L = tl * inverseSqrt(d2);
        var att = 1.0;
        if (l.color.w == 0.0) {
            var dist = 1.0 / (d2 + bias * bias);
            if (l.spot.z > 0.0) {
                let s = l.spot.z * -l.fwd.xyz;
                let a = tl - s * 0.5;
                let b = tl + s * 0.5;
                dist = 1.0 / ((length(a) * length(b) + dot(a, b)) * 0.5 + 1.0);
            }
            let r = l.pos.w * l.pos.w * d2;
            let m = max(1.0 - r * r, 0.0);
            att = dist * m * m;
        } else {
            let q = 1.0 - min(dot(tl * l.pos.w, tl * l.pos.w), 1.0);
            att = select(0.0, exp2(log2(q) * l.color.w), q > 0.0);
        }
        if (l.spot.x > -2.0) {
            let s = clamp((dot(L, -l.fwd.xyz) - l.spot.x) * l.spot.y, 0.0, 1.0);
            att = att * s * s;
        }
        lighting = lighting + l.color.rgb * (att * hg_phase(g, dot(L, -ray)) * l.spot.w);
    }
    let vb = textureLoad(vbuf_a_in, vec3<i32>(id), 0);
    let cur = vec4<f32>(lighting * vb.rgb, ue_luma(vb.rgb) + vb.a);
    // temporal history: the unjittered cell centre reprojected into the previous frame's volume
    let wc = cell_at(vec3<f32>(id) + 0.5);
    let pc = xv.prev_view_proj * vec4<f32>(wc, 1.0);
    let uvw = vec3<f32>(pc.x / pc.w * 0.5 + 0.5, -pc.y / pc.w * 0.5 + 0.5, depth_to_w(pc.w));
    let inside = all(uvw >= vec3<f32>(0.0)) && all(uvw <= vec3<f32>(1.0)) && pc.w > 0.0;
    let weight = select(0.0, xv.jitter.w, inside && xv.grid.w > 0.5);
    var res = mix(cur, textureSampleLevel(history_in, lin_samp, uvw, 0.0), weight);
    if (any(res != res) || any(abs(res) > vec4<f32>(3.0e38))) { res = vec4<f32>(0.0); }
    textureStore(light_out, vec3<i32>(id), res);
}

@compute @workgroup_size(8, 8, 1)
fn x_integrate(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u32(xv.grid.x) || id.y >= u32(xv.grid.y)) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / xv.grid.xy;
    var prev = xv.eye.xyz;
    var acc = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    for (var z = 0u; z < u32(xv.grid.z); z = z + 1u) {
        let s = textureLoad(light_in, vec3<i32>(vec2<i32>(id.xy), i32(z)), 0);
        let pos = cell_world(uv, slice_depth_cm(f32(z) + 0.5));
        let t = exp2(-s.a * length(pos - prev) * LN2_RCP);
        acc = vec4<f32>(acc.rgb + (s.rgb - s.rgb * t) / max(s.a, 0.00001) * acc.a, acc.a * t);
        textureStore(integrated_out, vec3<i32>(vec2<i32>(id.xy), i32(z)), acc);
        prev = pos;
    }
}

@vertex
fn x_vs_composite(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment
fn x_fs_composite(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy / xv.depth.zw;
    let z = textureLoad(scene_depth, vec2<i32>(pos.xy), 0);
    let n = xv.depth.x; let f = xv.depth.y;
    let d = n * f / (f - z * (f - n));
    return textureSampleLevel(integrated_in, lin_samp, vec3<f32>(uv, depth_to_w(d)), 0.0);
}
"#;

struct ExtVolume {
    grid: [u32; 3],
    vbuf_a: wgpu::Texture,
    light: [wgpu::Texture; 2],
    integrated: wgpu::Texture,
    setup_bg: wgpu::BindGroup,
    integrate_bg: [wgpu::BindGroup; 2],
    scatter_bg: [Option<wgpu::BindGroup>; 2],
    composite_bg: Option<wgpu::BindGroup>,
    key: u64,
    vbuf_v: Option<wgpu::Texture>,
    vox_bg: Option<wgpu::BindGroup>,
}

/// The volumetric fog of an external renderer (see the section comment): `prepare` -> `encode_compute` (one compute pass) -> `encode_composite` (a render
/// pass on the HDR target without depth attachment, after the opaque scene and before the translucency).
pub struct ExtVolFog {
    device: wgpu::Device,
    pub params: ExtVolFogParams,
    setup_pipe: wgpu::ComputePipeline,
    scatter_pipe: wgpu::ComputePipeline,
    integrate_pipe: wgpu::ComputePipeline,
    composite_pipe: wgpu::RenderPipeline,
    bgl_setup: wgpu::BindGroupLayout,
    bgl_scatter: wgpu::BindGroupLayout,
    bgl_integrate: wgpu::BindGroupLayout,
    bgl_composite: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    light_buf: wgpu::Buffer,
    lin_sampler: wgpu::Sampler,
    dummy_shadow: wgpu::TextureView,
    volume: Option<ExtVolume>,
    prev_view_proj: Option<Mat4>,
    last_frame: Option<u64>,
    parity: usize,
    active: bool,
    vox: Option<ExtVoxelizer>,
}

impl ExtVolFog {
    pub fn new(device: &wgpu::Device, hdr_format: wgpu::TextureFormat, params: ExtVolFogParams) -> ExtVolFog {
        let sm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("volumetric fog (external lights)"), source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(EXT_WGSL)) });
        let layout = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries });
        let e_lights = wgpu::BindGroupLayoutEntry { binding: 1, visibility: C, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None };
        let bgl_setup = layout("xvfog setup bgl", &[e_uniform(0, C), e_store3d(9)]);
        let bgl_scatter = layout("xvfog scatter bgl", &[e_uniform(0, C), e_lights, e_depth(3, C, wgpu::TextureViewDimension::D2), e_sampler(5, C, wgpu::SamplerBindingType::Filtering), e_tex3d(6, C), e_tex3d(7, C), e_store3d(8)]);
        let bgl_integrate = layout("xvfog integrate bgl", &[e_uniform(0, C), e_store3d(10), e_tex3d(11, C)]);
        let bgl_composite = layout("xvfog composite bgl", &[e_uniform(0, F), e_sampler(5, F, wgpu::SamplerBindingType::Filtering), e_tex3d(12, F), e_depth(13, F, wgpu::TextureViewDimension::D2)]);
        let pl = |label: &str, b: &wgpu::BindGroupLayout| device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[Some(b)], immediate_size: 0 });
        let cp = |label: &str, entry: &str, b: &wgpu::BindGroupLayout| device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(label), layout: Some(&pl(label, b)), module: &sm, entry_point: Some(entry), compilation_options: Default::default(), cache: None });
        let setup_pipe = cp("xvfog material setup", "x_material_setup", &bgl_setup);
        let scatter_pipe = cp("xvfog light scattering", "x_scatter", &bgl_scatter);
        let integrate_pipe = cp("xvfog final integration", "x_integrate", &bgl_integrate);
        let one_src_alpha = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::SrcAlpha, operation: wgpu::BlendOperation::Add };
        let alpha_scale = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::SrcAlpha, operation: wgpu::BlendOperation::Add };
        let composite_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("xvfog composite"), layout: Some(&pl("xvfog composite", &bgl_composite)),
            vertex: wgpu::VertexState { module: &sm, entry_point: Some("x_vs_composite"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &sm, entry_point: Some("x_fs_composite"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: hdr_format, blend: Some(wgpu::BlendState { color: one_src_alpha, alpha: alpha_scale }), write_mask: wgpu::ColorWrites::ALL })] }),
            multiview_mask: None, cache: None,
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("xvfog params"), size: std::mem::size_of::<ExtVFogUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let light_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("xvfog lights"), size: (EXT_MAX_LIGHTS * std::mem::size_of::<ExtLight>()) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let lin_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("xvfog volume"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let dummy_shadow = device.create_texture(&wgpu::TextureDescriptor { label: Some("xvfog no shadow"), size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Depth32Float, usage: wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] }).create_view(&Default::default());
        ExtVolFog { device: device.clone(), params, setup_pipe, scatter_pipe, integrate_pipe, composite_pipe, bgl_setup, bgl_scatter, bgl_integrate, bgl_composite,
            uniform_buf, light_buf, lin_sampler, dummy_shadow, volume: None, prev_view_proj: None, last_frame: None, parity: 0, active: false, vox: None }
    }

    pub fn active(&self) -> bool { self.active }
    pub fn destroy_volumes(&mut self) {
        if let Some(v) = self.volume.take() {
            v.vbuf_a.destroy();
            for l in &v.light { l.destroy(); }
            v.integrated.destroy();
            if let Some(t) = &v.vbuf_v { t.destroy(); }
        }
    }
    pub fn set_voxelizer(&mut self, v: ExtVoxelizer) { self.vox = Some(v); self.volume = None; }
    pub fn voxelizer(&self) -> Option<&ExtVoxelizer> { self.vox.as_ref() }
    pub fn grid(&self) -> Option<[u32; 3]> { self.volume.as_ref().map(|v| v.grid) }
    /// True when a `prepare` with `frame_index` reprojects a history (the previous `prepare` was the frame before).
    pub fn has_history(&self, frame_index: u64) -> bool { self.volume.is_some() && self.last_frame.is_some_and(|l| l + 1 == frame_index) }
    /// The integrated light scattering volume of the last `prepare` (what translucent programs sample).
    pub fn integrated_view(&self) -> Option<wgpu::TextureView> { self.volume.as_ref().filter(|_| self.active).map(|v| v.integrated.create_view(&Default::default())) }

    pub fn prepare(&mut self, queue: &wgpu::Queue, f: &ExtVolFogFrame) {
        let grid = fog::grid_size_for(f.width, f.height, f.grid_pixel_size, f.grid_size_z);
        let fresh = !matches!(&self.volume, Some(v) if v.grid == grid);
        let d = &self.device;
        if fresh {
            let mk = |label: &str| d.create_texture(&wgpu::TextureDescriptor {
                label: Some(label), size: wgpu::Extent3d { width: grid[0], height: grid[1], depth_or_array_layers: grid[2] }, mip_level_count: 1, sample_count: 1,
                dimension: wgpu::TextureDimension::D3, format: VOLUME_FORMAT, usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
            });
            let (vbuf_a, l0, l1, integrated) = (mk("xvfog VBufferA"), mk("xvfog light scattering 0"), mk("xvfog light scattering 1"), mk("xvfog integrated light scattering"));
            let v = |t: &wgpu::Texture| t.create_view(&Default::default());
            let setup_bg = d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("xvfog setup bg"), layout: &self.bgl_setup, entries: &[b_buf(0, &self.uniform_buf), b_tex(9, &v(&vbuf_a))] });
            let integrate_bg = [&l0, &l1].map(|l| d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("xvfog integrate bg"), layout: &self.bgl_integrate, entries: &[b_buf(0, &self.uniform_buf), b_tex(10, &v(&integrated)), b_tex(11, &v(l))] }));
            let vbuf_v = self.vox.as_ref().map(|_| mk("xvfog VBufferA (voxelized)"));
            let vox_bg = match (&self.vox, &vbuf_v) {
                (Some(vx), Some(t)) => Some(d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("xvfog voxelize bg"), layout: &vx.bgl3, entries: &[
                    b_buf(0, &vx.u_buf), b_buf(1, &vx.p_buf), b_tex(2, &v(&vbuf_a)), b_tex(3, &v(t)),
                ] })),
                _ => None,
            };
            self.volume = Some(ExtVolume { grid, vbuf_a, light: [l0, l1], integrated, setup_bg, integrate_bg, scatter_bg: [None, None], composite_bg: None, key: u64::MAX, vbuf_v, vox_bg });
            self.last_frame = None;
        }
        let consecutive = self.last_frame.is_some_and(|l| l + 1 == f.frame_index);
        self.parity = (f.frame_index & 1) as usize;
        let vol = self.volume.as_mut().unwrap();
        let key = f.depth_gen.wrapping_mul(2) + f.shadow.is_some() as u64;
        if vol.key != key || vol.scatter_bg[0].is_none() {
            let v = |t: &wgpu::Texture| t.create_view(&Default::default());
            let (va, l0, l1, ig) = (v(vol.vbuf_v.as_ref().unwrap_or(&vol.vbuf_a)), v(&vol.light[0]), v(&vol.light[1]), v(&vol.integrated));
            let lights = [&l0, &l1];
            let shadow = f.shadow.map_or(&self.dummy_shadow, |s| s.0);
            vol.scatter_bg = [0usize, 1].map(|c| Some(d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("xvfog scatter bg"), layout: &self.bgl_scatter, entries: &[
                b_buf(0, &self.uniform_buf), b_buf(1, &self.light_buf), b_tex(3, shadow), b_samp(5, &self.lin_sampler), b_tex(6, &va), b_tex(7, lights[1 - c]), b_tex(8, lights[c]),
            ] })));
            vol.composite_bg = Some(d.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("xvfog composite bg"), layout: &self.bgl_composite, entries: &[
                b_buf(0, &self.uniform_buf), b_samp(5, &self.lin_sampler), b_tex(12, &ig), b_tex(13, f.depth_view),
            ] }));
            vol.key = key;
        }
        let p = &self.params;
        let z = fog::grid_z_params(f.near_cm, p.distance_cm, grid[2], fog::engine::DEPTH_DISTRIBUTION_SCALE);
        let j = jitter_offsets(f.frame_index, p.jitter);
        let n = f.lights.len().min(EXT_MAX_LIGHTS);
        if n > 0 { queue.write_buffer(&self.light_buf, 0, bytemuck::cast_slice(&f.lights[..n])); }
        let (sh_vp, sh_size, sh_bias) = f.shadow.map_or((Mat4::IDENTITY, 1, 0.0), |s| (s.1, s.2, s.3));
        let v3 = |v: glam::Vec3, w: f32| [v.x, v.y, v.z, w];
        let u = ExtVFogUniform {
            grid: [grid[0] as f32, grid[1] as f32, grid[2] as f32, if consecutive && self.prev_view_proj.is_some() { 1.0 } else { 0.0 }],
            z_params: [z[0], z[1], z[2], f.near_cm],
            jitter: [j[0], j[1], j[2], p.history_weight],
            fog: [p.density, p.falloff, p.fog_height_cm, p.scattering_distribution],
            scale: [p.scattering_scale, p.absorption_scale, if p.dir_enabled { 1.0 } else { 0.0 }, n as f32],
            eye: v3(f.eye, p.inv_sq_bias_scale), fwd: v3(f.forward, f.tan_half.0), right: v3(f.right, f.tan_half.1), up: v3(f.up, 0.0),
            depth: [f.near_cm, f.far_cm, f.width as f32, f.height as f32],
            to_light: [p.to_light[0], p.to_light[1], p.to_light[2], if f.shadow.is_some() { 1.0 } else { 0.0 }],
            dir_color: [p.dir_color[0], p.dir_color[1], p.dir_color[2], p.dir_intensity],
            sky_r: p.sky_sh[0], sky_g: p.sky_sh[1], sky_b: p.sky_sh[2],
            sky: [p.sky_intensity, sh_size as f32, sh_bias, 0.0],
            shadow_vp: sh_vp.to_cols_array_2d(),
            prev_view_proj: self.prev_view_proj.unwrap_or(Mat4::IDENTITY).to_cols_array_2d(),
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
        if let Some(vx) = self.vox.as_mut() {
            let fit = |src: &[[f32; 4]], n: u32| { let mut o = vec![[0f32; 4]; n as usize]; for (d, s) in o.iter_mut().zip(src) { *d = *s; } o };
            let px = f.grid_pixel_size.max(1);
            let bits = |i: u32| f32::from_bits(i);
            let vfog = [
                [bits(grid[0]), bits(grid[1]), bits(grid[2]), 0.],
                [grid[0] as f32, grid[1] as f32, grid[2] as f32, bits(31 - px.leading_zeros())],
                [z[0], z[1], z[2], 0.],
                [1. / (grid[0] * px) as f32, 1. / (grid[1] * px) as f32, bits(px), bits(px)],
                [p.distance_cm, 0., 0., 0.],
            ];
            queue.write_buffer(&vx.vfog_buf, 0, bytemuck::cast_slice(&fit(&vfog, vx.vfog_size)));
            vx.count = 0;
            if let Some(vf) = &f.voxel {
                queue.write_buffer(&vx.view_buf, 0, bytemuck::cast_slice(&fit(vf.view_cb, vx.view_size)));
                queue.write_buffer(&vx.mat_buf, 0, bytemuck::cast_slice(&fit(vf.material_cb, vx.mat_size)));
                let n = vf.particles.len().min(VOX_MAX_PARTICLES);
                if n > 0 { queue.write_buffer(&vx.p_buf, 0, bytemuck::cast_slice(&vf.particles[..n])); }
                vx.count = n as u32;
            }
            let (tx, ty) = f.tan_half;
            let vu = VoxU { grid: [grid[0] as f32, grid[1] as f32, grid[2] as f32, vx.count as f32], zp: [z[0], z[1], z[2], 0.], jit: [j[0], j[1], j[2], 0.], proj: [tx, ty, f.device_z[0], f.device_z[1]] };
            queue.write_buffer(&vx.u_buf, 0, bytemuck::bytes_of(&vu));
        }
        self.prev_view_proj = Some(f.view_proj);
        self.last_frame = Some(f.frame_index);
        self.active = true;
    }

    /// The three compute dispatches (record them inside ONE compute pass).
    pub fn encode_compute(&self, pass: &mut wgpu::ComputePass<'_>) {
        let Some(v) = &self.volume else { return };
        let g = v.grid;
        pass.set_pipeline(&self.setup_pipe);
        pass.set_bind_group(0, &v.setup_bg, &[]);
        pass.dispatch_workgroups(g[0].div_ceil(4), g[1].div_ceil(4), g[2].div_ceil(4));
        if let (Some(vx), Some(bg)) = (&self.vox, &v.vox_bg) {
            pass.set_pipeline(&vx.pipe);
            for (gi, b) in &vx.bgs { pass.set_bind_group(*gi, b, &[]); }
            pass.set_bind_group(3, bg, &[]);
            pass.dispatch_workgroups(g[0].div_ceil(4), g[1].div_ceil(4), g[2].div_ceil(4));
        }
        pass.set_pipeline(&self.scatter_pipe);
        pass.set_bind_group(0, v.scatter_bg[self.parity].as_ref(), &[]);
        pass.dispatch_workgroups(g[0].div_ceil(4), g[1].div_ceil(4), g[2].div_ceil(4));
        pass.set_pipeline(&self.integrate_pipe);
        pass.set_bind_group(0, &v.integrate_bg[self.parity], &[]);
        pass.dispatch_workgroups(g[0].div_ceil(8), g[1].div_ceil(8), 1);
    }

    /// `dst * V.a + V.rgb` over the whole viewport (the pass must have no depth attachment: the scene depth is sampled).
    pub fn encode_composite(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(v) = &self.volume else { return };
        pass.set_pipeline(&self.composite_pipe);
        pass.set_bind_group(0, v.composite_bg.as_ref(), &[]);
        pass.draw(0..3, 0..1);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct ExtVolParticle {
    pub c01: [f32; 4],
    pub c23: [f32; 4],
    pub origin_radius: [f32; 4],
    pub depth: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoxRes { ViewCb, VolumetricFogCb, MaterialCb, Texture(usize), Sampler(usize) }

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct VoxU { grid: [f32; 4], zp: [f32; 4], jit: [f32; 4], proj: [f32; 4] }

pub const VOX_MAX_PARTICLES: usize = 256;

const VOX_WGSL: &str = r#"
struct VXU { grid: vec4<f32>, zp: vec4<f32>, jit: vec4<f32>, proj: vec4<f32> }
struct VXP { c01: vec4<f32>, c23: vec4<f32>, o: vec4<f32>, z: vec4<f32> }
@group(3) @binding(0) var<uniform> vxu: VXU;
@group(3) @binding(1) var<storage, read> vxp: array<VXP>;
@group(3) @binding(2) var vx_in: texture_3d<f32>;
@group(3) @binding(3) var vx_out: texture_storage_3d<rgba16float, write>;
fn vx_slice(d: f32) -> i32 {
    let a = d * vxu.zp.x + vxu.zp.y;
    if (a < 0.0 || a != a) { return 0; }
    if (a == 0.0) { return -2147483647 - 1; }
    let s = log2(a) * vxu.zp.z;
    if (s >= 2147483520.0) { return 2147483647; }
    if (s <= -2147483648.0) { return -2147483647 - 1; }
    return i32(s);
}
fn vx_depth(s: f32) -> f32 { return (exp2(s / vxu.zp.z) - vxu.zp.y) / vxu.zp.x; }
fn vx_cross(a: vec2<f32>, b: vec2<f32>) -> f32 { return a.x * b.y - a.y * b.x; }
fn vx_tri(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, c: vec2<f32>) -> bool {
    let d1 = vx_cross(b - a, p - a);
    let d2 = vx_cross(c - b, p - b);
    let d3 = vx_cross(a - c, p - c);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    return !(neg && pos);
}
@compute @workgroup_size(4, 4, 4)
fn x_voxelize(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u32(vxu.grid.x) || id.y >= u32(vxu.grid.y) || id.z >= u32(vxu.grid.z)) { return; }
    var acc = textureLoad(vx_in, vec3<i32>(id), 0);
    let gz = i32(vxu.grid.z);
    let sd = vx_depth(f32(id.z) + vxu.jit.z);
    let ndc = vec2<f32>((f32(id.x) + 0.5) / vxu.grid.x * 2.0 - 1.0, 1.0 - (f32(id.y) + 0.5) / vxu.grid.y * 2.0);
    let p = vec2<f32>((ndc.x - vxu.jit.x / vxu.grid.x) * sd * vxu.proj.x, (ndc.y - vxu.jit.y / vxu.grid.y) * sd * vxu.proj.y);
    var count = 0.0;
    let n = u32(vxu.grid.w);
    for (var i = 0u; i < n; i = i + 1u) {
        let q = vxp[i];
        let r = q.o.w;
        let cu = vx_slice(q.z.x - r);
        let fu = vx_slice(q.z.y + r);
        if (cu >= gz || fu < 0) { continue; }
        let zi = i32(id.z);
        if (zi < max(cu, 0) || zi > min(fu, gz - 1)) { continue; }
        let off = abs(sd - q.o.z);
        if (r != 0.0 && off > r) { continue; }
        let ratio = select(1.0, sqrt(max(r * r - off * off, 0.0)) / r, r > 0.0);
        let c0 = q.o.xy + ratio * (q.c01.xy - q.o.xy);
        let c1 = q.o.xy + ratio * (q.c01.zw - q.o.xy);
        let c2 = q.o.xy + ratio * (q.c23.xy - q.o.xy);
        let c3 = q.o.xy + ratio * (q.c23.zw - q.o.xy);
        if (vx_tri(p, c0, c1, c2) || vx_tri(p, c0, c2, c3)) { count = count + 1.0; }
    }
    if (count > 0.0) {
        var v: array<vec4<f32>, VX_N>;
        v[VX_POS] = vec4<f32>(f32(id.x) + 0.5, f32(id.y) + 0.5, vxu.proj.z + vxu.proj.w / sd, sd);
        let o = VX_FN(v);
        acc = acc + o[0] * count;
    }
    textureStore(vx_out, vec3<i32>(id), acc);
}
"#;

struct VoxDecl { group: u32, binding: u32, res: VoxRes, kind: u8, size: u32, dim: wgpu::TextureViewDimension }

pub struct ExtVoxelizer {
    pipe: wgpu::ComputePipeline,
    bgl3: wgpu::BindGroupLayout,
    bgs: Vec<(u32, wgpu::BindGroup)>,
    view_buf: wgpu::Buffer,
    vfog_buf: wgpu::Buffer,
    mat_buf: wgpu::Buffer,
    u_buf: wgpu::Buffer,
    p_buf: wgpu::Buffer,
    pub view_size: u32,
    pub mat_size: u32,
    vfog_size: u32,
    count: u32,
    pub notes: Vec<String>,
}

fn parse_vox_decls(wgsl: &str, roles: &dyn Fn(&str) -> Option<VoxRes>) -> Result<Vec<VoxDecl>, String> {
    let mut out = vec![];
    for line in wgsl.lines() {
        let Some(rest) = line.strip_prefix("@group(") else { continue };
        let (g, rest) = rest.split_once(") @binding(").ok_or("bad group line")?;
        let (b, rest) = rest.split_once(')').ok_or("bad binding line")?;
        let (group, binding): (u32, u32) = (g.parse().map_err(|_| "group")?, b.parse().map_err(|_| "binding")?);
        let decl = rest.trim();
        let name = decl.split(':').next().unwrap_or("").rsplit(' ').next().unwrap_or("").trim().to_owned();
        let res = roles(&name).ok_or_else(|| format!("no resource for {name}"))?;
        let (kind, size, dim) = if decl.starts_with("var<uniform>") {
            let sz = decl.split("vec4<f32>, ").nth(1).and_then(|r| r.split('>').next()).and_then(|n| n.parse::<u32>().ok()).ok_or("uniform size")?;
            (0u8, sz, wgpu::TextureViewDimension::D2)
        } else if decl.contains("texture_3d") { (1, 0, wgpu::TextureViewDimension::D3) }
        else if decl.contains("texture_2d") { (1, 0, wgpu::TextureViewDimension::D2) }
        else if decl.contains(": sampler") { (2, 0, wgpu::TextureViewDimension::D2) }
        else { return Err(format!("unsupported declaration {decl}")); };
        if group > 2 { return Err(format!("resource group {group}")); }
        out.push(VoxDecl { group, binding, res, kind, size, dim });
    }
    Ok(out)
}

impl ExtVoxelizer {
    pub fn new(device: &wgpu::Device, ps_wgsl: &str, ps_fn: &str, roles: &dyn Fn(&str) -> Option<VoxRes>, textures: &[&wgpu::TextureView], samplers: &[&wgpu::Sampler]) -> Result<ExtVoxelizer, String> {
        if ps_wgsl.contains("textureSample(") || ps_wgsl.contains("textureSampleBias(") || ps_wgsl.contains("dpdx") || ps_wgsl.contains("dpdy") || ps_wgsl.contains("discard") {
            return Err("the voxelize program needs derivatives / discard (not a compute program)".into());
        }
        let n = ps_wgsl.split(&format!("fn {ps_fn}(v: array<vec4<f32>, ")).nth(1).and_then(|r| r.split('>').next()).and_then(|x| x.parse::<u32>().ok()).ok_or("voxelize program input array")?;
        if n < 4 { return Err(format!("voxelize program reads {n} input registers, SV_Position is v3")); }
        let decls = parse_vox_decls(ps_wgsl, roles)?;
        let src = format!("{ps_wgsl}\n{}", VOX_WGSL.replace("VX_N", &n.to_string()).replace("VX_POS", "3").replace("VX_FN", ps_fn));
        let sm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("volume voxelization"), source: wgpu::ShaderSource::Wgsl(src.into()) });
        let size_of = |r: VoxRes| decls.iter().find(|d| d.res == r).map_or(1, |d| d.size.max(1));
        let (view_size, vfog_size, mat_size) = (size_of(VoxRes::ViewCb), size_of(VoxRes::VolumetricFogCb), size_of(VoxRes::MaterialCb));
        let ub = |label: &str, n: u32| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: n as u64 * 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let (view_buf, vfog_buf, mat_buf) = (ub("voxelize View", view_size), ub("voxelize VolumetricFog", vfog_size), ub("voxelize Material", mat_size));
        let mut layouts = vec![];
        let mut bgs_src: Vec<(u32, Vec<wgpu::BindGroupEntry>)> = vec![];
        for g in 0..3u32 {
            let ds: Vec<&VoxDecl> = decls.iter().filter(|d| d.group == g).collect();
            let entries: Vec<wgpu::BindGroupLayoutEntry> = ds.iter().map(|d| wgpu::BindGroupLayoutEntry { binding: d.binding, visibility: C, count: None, ty: match d.kind {
                0 => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                1 => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: d.dim, multisampled: false },
                _ => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            } }).collect();
            layouts.push(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("voxelize program"), entries: &entries }));
            let mut be = vec![];
            for d in ds {
                let r = match d.res {
                    VoxRes::ViewCb => view_buf.as_entire_binding(),
                    VoxRes::VolumetricFogCb => vfog_buf.as_entire_binding(),
                    VoxRes::MaterialCb => mat_buf.as_entire_binding(),
                    VoxRes::Texture(i) => wgpu::BindingResource::TextureView(textures.get(i).ok_or_else(|| format!("texture {i} missing"))?),
                    VoxRes::Sampler(i) => wgpu::BindingResource::Sampler(samplers.get(i).ok_or_else(|| format!("sampler {i} missing"))?),
                };
                be.push(wgpu::BindGroupEntry { binding: d.binding, resource: r });
            }
            bgs_src.push((g, be));
        }
        let bgl3 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("voxelize volume"), entries: &[
            e_uniform(0, C),
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: C, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: C, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false }, count: None },
            e_store3d(3),
        ] });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("voxelize"), bind_group_layouts: &[Some(&layouts[0]), Some(&layouts[1]), Some(&layouts[2]), Some(&bgl3)], immediate_size: 0 });
        let pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("volume voxelization"), layout: Some(&pl), module: &sm, entry_point: Some("x_voxelize"), compilation_options: Default::default(), cache: None });
        let bgs = bgs_src.into_iter().map(|(g, be)| (g, device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("voxelize program"), layout: &layouts[g as usize], entries: &be }))).collect();
        let u_buf = ub("voxelize params", 4);
        let p_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("voxelize particles"), size: (VOX_MAX_PARTICLES * std::mem::size_of::<ExtVolParticle>()) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let notes = vec![format!("voxelize program {ps_fn}: {n} input registers, View {view_size} / VolumetricFog {vfog_size} / Material {mat_size} float4, {} resources", decls.len())];
        Ok(ExtVoxelizer { pipe, bgl3, bgs, view_buf, vfog_buf, mat_buf, u_buf, p_buf, view_size, mat_size, vfog_size, count: 0, notes })
    }
}

pub struct ExtVoxelFrame<'a> {
    pub view_cb: &'a [[f32; 4]],
    pub material_cb: &'a [[f32; 4]],
    pub particles: &'a [ExtVolParticle],
}

/// UE `SHBasisFunction` signs (SHMath.h: V[1] = -0.488603 y, V[3] = -0.488603 x; RECALLED engine source, the shader only consumes the coefficients):
/// the first two bands of a real-SH radiance projection made with positive basis signs (`c[k] = sum L * Y_k`, Y1 = +0.488603 y, Y3 = +0.488603 x) as
/// the per-channel `FSHVectorRGB3::V[0..4]` the scattering CS dots with `(1, g * V.y, g * V.z, g * V.x)`.
pub fn ue_two_band_sh(c: &[[f32; 3]]) -> [[f32; 4]; 3] {
    std::array::from_fn(|ch| [c[0][ch], -c[1][ch], c[2][ch], -c[3][ch]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::CameraUniform;
    use crate::gpu::Gpu;
    use glam::Vec3;

    fn f16_round(x: f32) -> f32 {
        if x == 0.0 || !x.is_finite() { return x; }
        let a = x.abs();
        if a >= 65520.0 { return f32::INFINITY.copysign(x); }
        let e = (a.log2().floor() as i32).max(-14);
        let q = 2f32.powi(e - 10);
        ((a / q).round_ties_even() * q).copysign(x)
    }
    fn f16_to_f32(h: u16) -> f32 {
        let (s, e, m) = ((h >> 15) as u32, ((h >> 10) & 31) as i32, (h & 1023) as f32);
        let v = if e == 0 { m * 2f32.powi(-24) } else if e == 31 { if m == 0.0 { f32::INFINITY } else { f32::NAN } } else { (1.0 + m / 1024.0) * 2f32.powi(e - 15) };
        if s == 1 { -v } else { v }
    }

    #[test]
    fn jitter_is_halton_and_centre_without_jitter() {
        assert_eq!(jitter_offsets(5, false), [0.5; 3]);
        let j = jitter_offsets(1, true);
        assert!((j[0] - 0.5).abs() < 1e-6 && (j[1] - 1.0 / 3.0).abs() < 1e-6 && (j[2] - 0.2).abs() < 1e-6, "{j:?}");
        assert_eq!(jitter_offsets(0, true), [0.0; 3]);
    }
    #[test]
    fn uniform_block_is_vec4_aligned_and_wgsl_mirrors_it() {
        assert_eq!(std::mem::size_of::<VFogUniform>(), 8 * 16 + 64);
        let src = fn_live::wgsl!("../shaders/volfog.wgsl");
        let s = &src[src.find("struct VFog").unwrap()..];
        let s = &s[..s.find("}\n").unwrap()];
        let order = ["grid", "z_params", "jitter", "fog", "scale", "dir_color", "sky_color", "prev_view_proj", "lf_plane"];
        let mut at = 0;
        for n in order { let i = s[at..].find(&format!("{n}:")).unwrap_or_else(|| panic!("{n} missing or out of order")); at += i; }
    }
    #[test]
    fn athena_day_params_are_the_recovered_values() {
        let p = VolFogParams::athena_day();
        assert!((p.density - 2.2e-7).abs() < 1e-12 && (p.falloff - 9e-5).abs() < 1e-10 && p.fog_height_cm == 65000.0);
        assert_eq!((p.scattering_distribution, p.scattering_scale, p.absorption_scale, p.distance_cm), (0.05, 0.1, 1.0, 4500.0));
        // override rule: luma(sun colour * brightness 3) * DirectionalInscatteringColor
        assert!(p.dir_color[2] > p.dir_color[1] && p.dir_color[0] > 0.0);
        assert!((p.sky_color[1] - 0.277_685 * 0.282_095).abs() < 1e-6 && p.sky_intensity == 0.65);
        // the fog plane (HeightFogZOffset 650 m) is far above the play space: at ground level the density is 2^(65000 * 9e-5) = 57.7x the plane value
        let (s, a) = fog::material_setup(0.0, p.density, p.falloff, p.fog_height_cm, p.scattering_scale, p.absorption_scale);
        assert!((a - 6.35e-6).abs() < 1e-7 && (s[0] - a * 0.1).abs() < 1e-9, "{a} {s:?}");
        // the half-float volumes flush the scattering (1.1e-8 at the plane) but keep the absorption (see the module docs)
        assert_eq!(f16_round(p.density * 0.5 * p.scattering_scale), 0.0);
        assert!(f16_round(a) > 0.0);
    }

    fn gpu() -> Option<Gpu> { Gpu::headless().ok() }
    fn storage(device: &wgpu::Device, label: &str, data: &[f32]) -> wgpu::Buffer {
        crate::texture::buffer_init(device, data, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, label)
    }
    fn read_buf(g: &Gpu, buf: &wgpu::Buffer, bytes: u64) -> Vec<u8> {
        let staging = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("readback"), size: bytes, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = g.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(buf, 0, &staging, 0, bytes);
        g.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let out = slice.get_mapped_range().unwrap().to_vec();
        staging.unmap();
        out
    }

    /// GL (Y up, metres) -> Unreal (Z up) axes for the CPU ports; the horizontal handedness is irrelevant to the fog.
    fn ue(v: Vec3, s: f32) -> [f32; 3] { [v.x * s, v.z * s, v.y * s] }

    #[test]
    fn height_fog_wgsl_matches_the_cpu_port() {
        let Some(g) = gpu() else { eprintln!("no GPU adapter: skipped"); return };
        let src = format!("{}\n{}", fn_live::wgsl!("../shaders/common.wgsl"), r#"
struct In { world: vec4<f32>, cam: vec4<f32>, fwd: vec4<f32>, sun: vec4<f32> }
@group(0) @binding(0) var<storage, read> ins: array<In>;
@group(0) @binding(1) var<storage, read_write> outs: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> fog_u: HeightFogU;
@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) { let i = ins[id.x]; outs[id.x] = height_fog(fog_u, i.world.xyz, i.cam.xyz, i.sun.xyz, i.fwd.xyz); }
"#);
        let sm = g.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("fog test"), source: wgpu::ShaderSource::Wgsl(src.into()) });
        let sun = Vec3::new(0.3, 0.8, -0.52).normalize();
        let mut cases: Vec<(Vec3, Vec3, Vec3)> = vec![]; // camera, forward, receiver
        for cam_y in [1.75f32, 120.0, 650.0, 900.0] {
            let cam = Vec3::new(10.0, cam_y, -4.0);
            for fwd in [Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.6, 0.2, -0.77).normalize(), Vec3::new(0.0, -0.5, -0.87).normalize(), Vec3::new(-0.3, 0.7, 0.2).normalize()] {
                for dist in [0.2f32, 3.0, 30.0, 44.0, 46.0, 90.0, 400.0, 3000.0, 60000.0, 2.0e6] {
                    cases.push((cam, fwd, cam + fwd * dist));
                    // off-axis receivers (the exclusion distance depends on the angle to the view axis)
                    let side = fwd.cross(Vec3::Y).normalize_or_zero() * 0.4 * dist;
                    cases.push((cam, fwd, cam + fwd * dist + side));
                }
            }
        }
        let mut data = vec![];
        for (cam, fwd, world) in &cases { data.extend_from_slice(&[world.x, world.y, world.z, 0., cam.x, cam.y, cam.z, 0., fwd.x, fwd.y, fwd.z, 0., sun.x, sun.y, sun.z, 0.]); }
        let n = cases.len();
        let ins = storage(&g.device, "ins", &data);
        let outs = storage(&g.device, "outs", &vec![0f32; n * 4]);
        let bgl = g.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: C, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: C, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: C, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
        ] });
        let pl = g.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipe = g.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: None, layout: Some(&pl), module: &sm, entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        // TOD LIVE: the fog arrives as the per-frame HeightFogU block; one uniform buffer rewritten per state, like the renderer's light / sky buffers
        let fog_buf = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("fog"), size: std::mem::size_of::<fog::HeightFogUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bg = g.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl, entries: &[b_buf(0, &ins), b_buf(1, &outs), b_buf(2, &fog_buf)] });
        let states = [("Evening", fog::HeightFogPs::from_phase(&fog::PhaseFog::EVENING, true)), ("Day", fog::HeightFogPs::from_phase(&fog::PhaseFog::DAY, true)),
            ("Night, no volume", fog::HeightFogPs::from_phase(&fog::PhaseFog::NIGHT, false)), ("18.5 h blend", fog::HeightFogPs::from_phase(&fog::PhaseFog::at_hour(18.5), true))];
        for (label, ps) in states {
        g.queue.write_buffer(&fog_buf, 0, bytemuck::bytes_of(&ps.uniform()));
        let mut enc = g.device.create_command_encoder(&Default::default());
        { let mut p = enc.begin_compute_pass(&Default::default()); p.set_pipeline(&pipe); p.set_bind_group(0, &bg, &[]); p.dispatch_workgroups(n as u32, 1, 1); }
        g.queue.submit([enc.finish()]);
        let out: Vec<f32> = bytemuck::cast_slice(&read_buf(&g, &outs, (n * 16) as u64)).to_vec();
        // The line integral (1 - 2^-x) / x cancels catastrophically for tiny x = falloff * dz (near-horizontal rays): an f32 2^-x has a ~6e-8 spacing, i.e. up to
        // ~6e-8 / x relative error in the integral, and the GPU's exp2 and the CPU's differ in the last bits (the original shader has exactly this property on
        // real hardware). The tolerance therefore widens with that conditioning, and is a hard 2e-5 everywhere else.
        let (mut max_t, mut max_c, mut worst_ratio, mut strict) = (0f32, 0f32, 0f32, 0usize);
        for (i, (cam, fwd, world)) in cases.iter().enumerate() {
            let c = ue(*world - *cam, 100.0);
            let (rgb, t) = ps.eval(c, ue(*fwd, 1.0), ue(sun, 1.0), cam.y * 100.0);
            let len = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
            let along = c[0] * fwd.x + c[1] * fwd.z + c[2] * fwd.y;
            let ex = (len / along * ps.volumetric_max_distance_cm).max(0.0);
            let dz = c[2] * (1.0 - (ex / len).min(1.0));
            let x = (if dz.abs() > 0.01 { dz.abs() } else { 0.01 }) * ps.falloff;
            let budget = 2e-5 + (1.0 - t) * 3e-7 / x.max(1e-9);
            let dt = (out[i * 4 + 3] - t).abs();
            let dc = (0..3).map(|k| (out[i * 4 + k] - rgb[k]).abs()).fold(0f32, f32::max);
            if budget < 1e-4 { strict += 1; max_t = max_t.max(dt); max_c = max_c.max(dc); }
            worst_ratio = worst_ratio.max(dt / budget).max(dc / budget);
        }
        eprintln!("[{label}] height fog WGSL vs fog.rs over {n} samples ({strict} well-conditioned): max |dT| = {max_t:.2e}, max |d rgb| = {max_c:.2e}, worst error / conditioning budget = {worst_ratio:.2}");
        assert!(strict > n / 4, "[{label}] too few well-conditioned samples");
        assert!(max_t < 2e-5 && max_c < 2e-5, "[{label}] well-conditioned samples: T {max_t} rgb {max_c}");
        assert!(worst_ratio < 1.0, "[{label}] an ill-conditioned sample exceeded its budget: {worst_ratio}");
        }
    }

    /// Mirrors volfog.wgsl for one frame without history on the CPU, using the fog.rs ports, with half-float rounding at every texture store.
    struct CpuVolume { grid: [u32; 3], integrated: Vec<[f32; 4]> }
    #[allow(clippy::too_many_arguments)]
    fn cpu_volume(cam: &CameraParams, p: &VolFogParams, grid: [u32; 3], j: [f32; 3], to_sun: Vec3, cascade_vis: &dyn Fn(f32) -> f32) -> CpuVolume {
        let (tx, ty) = cam.tan_half();
        let vi = cam.view_matrix().inverse();
        let zp = fog::grid_z_params(cam.near * 100.0, p.distance_cm, grid[2], fog::engine::DEPTH_DISTRIBUTION_SCALE);
        let world = |uv: [f32; 2], depth_cm: f32| {
            let ndc = [(uv[0] * 2.0 - 1.0), -(uv[1] * 2.0 - 1.0)];
            let d = depth_cm * 0.01;
            vi.transform_point3(Vec3::new(ndc[0] * tx * d, ndc[1] * ty * d, -d))
        };
        let n = (grid[0] * grid[1] * grid[2]) as usize;
        let idx = |x: u32, y: u32, z: u32| ((z * grid[1] + y) * grid[0] + x) as usize;
        let mut lightv = vec![[0f32; 4]; n];
        for z in 0..grid[2] { for y in 0..grid[1] { for x in 0..grid[0] {
            // material setup (cell centre)
            let c = [x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5];
            let wc = world([c[0] / grid[0] as f32, c[1] / grid[1] as f32], fog::depth_from_slice(zp, c[2]));
            let (s, a) = fog::material_setup(wc.y * 100.0, p.density, p.falloff, p.fog_height_cm, p.scattering_scale, p.absorption_scale);
            let (s, a) = (s.map(f16_round), f16_round(a));
            // light scattering (jittered cell)
            let cj = [x as f32 + j[0], y as f32 + j[1], z as f32 + j[2]];
            let depth_cm = fog::depth_from_slice(zp, cj[2]);
            let w = world([cj[0] / grid[0] as f32, cj[1] / grid[1] as f32], depth_cm);
            let ray = (w - cam.eye).normalize();
            let mut l = [0f32; 3];
            if p.dir_enabled {
                let vis = cascade_vis(depth_cm * 0.01);
                let k = vis * p.dir_intensity * fog::hg_phase(p.scattering_distribution, to_sun.dot(-ray));
                for i in 0..3 { l[i] += p.dir_color[i] * k; }
            }
            if p.sky_intensity > 0.0 { for i in 0..3 { l[i] += p.sky_intensity * p.sky_color[i].max(0.0); } }
            let ext = fog::ue_luma(s) + a;
            lightv[idx(x, y, z)] = [l[0] * s[0], l[1] * s[1], l[2] * s[2], ext].map(f16_round);
        } } }
        let mut integrated = vec![[0f32; 4]; n];
        for y in 0..grid[1] { for x in 0..grid[0] {
            let uv = [(x as f32 + 0.5) / grid[0] as f32, (y as f32 + 0.5) / grid[1] as f32];
            let mut prev = cam.eye;
            let (mut slices, mut steps, mut pos_all) = (vec![], vec![], vec![]);
            for z in 0..grid[2] {
                let pos = world(uv, fog::depth_from_slice(zp, z as f32 + 0.5));
                let v = lightv[idx(x, y, z)];
                slices.push(([v[0], v[1], v[2]], v[3]));
                steps.push((pos - prev).length() * 100.0);
                prev = pos;
                pos_all.push(pos);
            }
            for (z, (l, t)) in fog::final_integration(&slices, &steps).into_iter().enumerate() {
                integrated[idx(x, y, z as u32)] = [l[0], l[1], l[2], t].map(f16_round);
            }
        } }
        CpuVolume { grid, integrated }
    }

    fn read_volume(g: &Gpu, tex: &wgpu::Texture, grid: [u32; 3]) -> Vec<[f32; 4]> {
        let bpr = (grid[0] * 8).div_ceil(256) * 256;
        let size = (bpr * grid[1] * grid[2]) as u64;
        let buf = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("vol readback"), size, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = g.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(grid[1]) } },
            wgpu::Extent3d { width: grid[0], height: grid[1], depth_or_array_layers: grid[2] },
        );
        g.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let data = slice.get_mapped_range().unwrap();
        let mut out = vec![];
        for z in 0..grid[2] { for y in 0..grid[1] { for x in 0..grid[0] {
            let o = (((z * grid[1] + y) * bpr) + x * 8) as usize;
            let c = |i: usize| f16_to_f32(u16::from_le_bytes([data[o + i * 2], data[o + i * 2 + 1]]));
            out.push([c(0), c(1), c(2), c(3)]);
        } } }
        out
    }

    fn run_volume_case(g: &Gpu, params: VolFogParams, shadows: bool, label: &str) -> (f32, f32, f32) {
        let (w, h) = (192u32, 108u32);
        let cam = CameraParams::from_yaw_pitch(Vec3::new(3.0, 1.75, -2.0), 25.0, -8.0, w, h);
        let to_sun = Vec3::new(0.3, 0.8, -0.52).normalize();
        // camera + light blocks (zero light block with only what the volume reads)
        let cu: CameraUniform = cam.uniform(0.0);
        let camera_buf = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("camera"), size: std::mem::size_of::<CameraUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        g.queue.write_buffer(&camera_buf, 0, bytemuck::bytes_of(&cu));
        let mut lb = vec![0f32; 88];
        lb[0..3].copy_from_slice(&to_sun.to_array());
        // every cascade matrix maps any point to uv (0.5, 0.5), depth 0.5
        for c in 0..3 { let m = Mat4::from_translation(Vec3::splat(0.5)) * Mat4::from_scale(Vec3::ZERO); lb[12 + c * 16..12 + c * 16 + 16].copy_from_slice(&m.to_cols_array()); }
        lb[60..64].copy_from_slice(&[10.0, 20.0, 45.0, 45.0]);
        lb[64..68].copy_from_slice(&[0.02, 0.04, 0.08, 0.0]);
        lb[68..72].copy_from_slice(&[if shadows { 1.0 } else { 0.0 }, 3.0, 5.0, 0.0]);
        lb[74] = 16.0; // misc.z: shadow map edge (the 4-texel SHADOW_BORDER rectangle test of VolumetricFog.usf needs uv 0.5 inside it)
        lb[84..88].copy_from_slice(&[1.0, 1.0, 250.0, 0.0]); // light function: grey 1 = no modulation
        let light_buf = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("light"), size: 352, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        g.queue.write_buffer(&light_buf, 0, bytemuck::cast_slice(&lb));
        // shadow array: layer 0 and 2 hold depth 0.25 (the receiver at 0.5 is shadowed), layer 1 holds 1.0 (lit)
        let sh = g.device.create_texture(&wgpu::TextureDescriptor { label: Some("shadow"), size: wgpu::Extent3d { width: 16, height: 16, depth_or_array_layers: 3 }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Depth32Float, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] });
        let mut enc = g.device.create_command_encoder(&Default::default());
        for (layer, depth) in [(0u32, 0.25f32), (1, 1.0), (2, 0.25)] {
            let lv = sh.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2), base_array_layer: layer, array_layer_count: Some(1), ..Default::default() });
            let _p = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: None, color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &lv, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(depth), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
        }
        g.queue.submit([enc.finish()]);
        let shadow_view = sh.create_view(&Default::default());
        let depth = g.device.create_texture(&wgpu::TextureDescriptor { label: Some("depth"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Depth32Float, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] });
        let depth_view = depth.create_view(&Default::default());

        let mut vf = VolFog::new(&g.device, &camera_buf, &light_buf, wgpu::TextureFormat::Rgba16Float);
        vf.params = params;
        vf.prepare(&g.queue, &VolFogFrame { width: w, height: h, camera: &cam, frame_index: 3, shadow_view: &shadow_view, shadow_size: 16, depth_view: &depth_view, depth_gen: 1, grid_pixel_size: fog::engine::GRID_PIXEL_SIZE, grid_size_z: fog::engine::GRID_SIZE_Z });
        let mut enc = g.device.create_command_encoder(&Default::default());
        { let mut pass = enc.begin_compute_pass(&Default::default()); vf.encode_compute(&mut pass); }
        g.queue.submit([enc.finish()]);
        let grid = vf.grid().unwrap();
        let gpu_vol = read_volume(g, &vf.volume.as_ref().unwrap().integrated, grid);

        let j = jitter_offsets(3, params.jitter);
        // the shadow test texture: layer 0 (< 10 m) and 2 (>= 20 m, < 45 m) shadowed, layer 1 lit, >= 45 m lit
        let vis = move |d_m: f32| if !shadows || d_m >= 45.0 { 1.0 } else if d_m < 10.0 || d_m >= 20.0 { 0.0 } else { 1.0 };
        let cpu = cpu_volume(&cam, &params, grid, j, to_sun, &vis);
        assert_eq!(cpu.grid, grid);
        let (mut max_t, mut max_l, mut max_rel) = (0f32, 0f32, 0f32);
        let mut lmax = 0f32;
        for (a, b) in gpu_vol.iter().zip(&cpu.integrated) {
            max_t = max_t.max((a[3] - b[3]).abs());
            for c in 0..3 { max_l = max_l.max((a[c] - b[c]).abs()); lmax = lmax.max(b[c]); if b[c] > 1e-4 { max_rel = max_rel.max((a[c] - b[c]).abs() / b[c]); } }
        }
        let last = gpu_vol.last().unwrap();
        eprintln!("[{label}] grid {grid:?}: max |dT| {max_t:.2e}, max |d light| {max_l:.2e} (peak light {lmax:.3e}, max rel above 1e-4: {max_rel:.2e}); far corner voxel (light {:.3e} {:.3e} {:.3e}, T {:.5})", last[0], last[1], last[2], last[3]);
        (max_t, max_l / lmax.max(1e-9), lmax)
    }

    /// Every shader that calls `height_fog` / `output_hdr` still validates with the new 4-argument fog (the uncaptured-error handler of `Gpu::new` panics on an invalid module).
    #[test]
    fn every_height_fog_caller_compiles() {
        let Some(g) = gpu() else { eprintln!("no GPU adapter: skipped"); return };
        let (common, lit, sky_common) = (fn_live::wgsl!("../shaders/common.wgsl"), fn_live::wgsl!("../shaders/lit_common.wgsl"), fn_live::wgsl!("../shaders/sky_common.wgsl"));
        let sets: [(&str, Vec<&str>); 6] = [
            ("scene", vec![common, lit, fn_live::wgsl!("../shaders/scene.wgsl")]),
            ("particles", vec![common, lit, fn_live::wgsl!("../shaders/particles.wgsl")]),
            ("static scene", vec![common, lit, fn_live::wgsl!("../shaders/static_common.wgsl"), fn_live::wgsl!("../shaders/static_scene.wgsl")]),
            ("sky dome", vec![common, sky_common, fn_live::wgsl!("../shaders/sky_dome.wgsl")]),
            ("sky sun", vec![common, sky_common, fn_live::wgsl!("../shaders/sky_sun.wgsl")]),
            ("sky clouds", vec![common, sky_common, fn_live::wgsl!("../shaders/sky_clouds.wgsl")]),
        ];
        for (label, parts) in sets {
            let _ = g.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(parts.join("\n").into()) });
        }
        g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }

    /// Frame cost of the volumetric fog at 1920x1080 (grid 120 x 68 x 64), real 3 x 2048^2 shadow array: submit + wait, averaged (upper bound: includes
    /// the CPU submit latency; the in-renderer GPU timestamps are the `vfog` / `vfog composite` passes of `last_pass_timings`).
    #[test]
    fn frame_cost_at_1080p() {
        let Some(g) = gpu() else { eprintln!("no GPU adapter: skipped"); return };
        let (w, h) = (1920u32, 1080u32);
        let cam = CameraParams::from_yaw_pitch(Vec3::new(3.0, 1.75, -2.0), 25.0, -8.0, w, h);
        let camera_buf = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("camera"), size: std::mem::size_of::<CameraUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        g.queue.write_buffer(&camera_buf, 0, bytemuck::bytes_of(&cam.uniform(0.0)));
        let mut lb = vec![0f32; 88];
        lb[0..3].copy_from_slice(&Vec3::new(0.3, 0.8, -0.52).normalize().to_array());
        for c in 0..3 { let m = Mat4::from_translation(Vec3::splat(0.5)) * Mat4::from_scale(Vec3::ZERO); lb[12 + c * 16..12 + c * 16 + 16].copy_from_slice(&m.to_cols_array()); }
        lb[60..64].copy_from_slice(&[10.0, 20.0, 45.0, 45.0]);
        lb[68] = 1.0; lb[69] = 3.0; lb[70] = 5.0;
        lb[74] = 2048.0;
        lb[84..88].copy_from_slice(&[1.0, 1.0, 250.0, 0.0]);
        let light_buf = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("light"), size: 352, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        g.queue.write_buffer(&light_buf, 0, bytemuck::cast_slice(&lb));
        let tex2d = |label: &str, w: u32, h: u32, layers: u32, format: wgpu::TextureFormat| g.device.create_texture(&wgpu::TextureDescriptor { label: Some(label), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: layers }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] });
        let shadow_view = tex2d("shadow", 2048, 2048, 3, wgpu::TextureFormat::Depth32Float).create_view(&Default::default());
        let depth_view = tex2d("depth", w, h, 1, wgpu::TextureFormat::Depth32Float).create_view(&Default::default());
        let hdr = tex2d("hdr", w, h, 1, wgpu::TextureFormat::Rgba16Float);
        let hdr_view = hdr.create_view(&Default::default());
        let mut vf = VolFog::new(&g.device, &camera_buf, &light_buf, wgpu::TextureFormat::Rgba16Float);
        let (mut total, mut n) = (0f64, 0u32);
        for i in 0..70u64 {
            vf.prepare(&g.queue, &VolFogFrame { width: w, height: h, camera: &cam, frame_index: i, shadow_view: &shadow_view, shadow_size: 2048, depth_view: &depth_view, depth_gen: 1, grid_pixel_size: fog::engine::GRID_PIXEL_SIZE, grid_size_z: fog::engine::GRID_SIZE_Z });
            let t0 = std::time::Instant::now();
            let mut enc = g.device.create_command_encoder(&Default::default());
            { let mut pass = enc.begin_compute_pass(&Default::default()); vf.encode_compute(&mut pass); }
            { let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: None, depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &hdr_view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })] });
              vf.encode_composite(&mut pass); }
            g.queue.submit([enc.finish()]);
            g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            if i >= 10 { total += t0.elapsed().as_secs_f64() * 1000.0; n += 1; }
        }
        eprintln!("volumetric fog 1920x1080, grid {:?}: {:.3} ms per frame (submit + wait, 3 compute dispatches + composite)", vf.grid().unwrap(), total / n as f64);
    }

    #[test]
    fn volume_passes_match_the_cpu_ports() {
        let Some(g) = gpu() else { eprintln!("no GPU adapter: skipped"); return };
        // a dense, bright test medium so the half-float volumes carry signal (the recovered Athena density flushes to zero scattering)
        let mut dense = VolFogParams::athena_day();
        dense.density = 6.0e-3; dense.falloff = 1.0e-4; dense.fog_height_cm = 0.0;
        dense.scattering_scale = 1.0; dense.history_weight = 0.0;
        dense.dir_color = [0.5, 0.45, 0.4]; dense.sky_color = [0.3, 0.4, 0.6];
        let (dt, dl, peak) = run_volume_case(&g, dense, false, "dense, no shadows");
        assert!(peak > 1e-3, "test medium must scatter, peak {peak}");
        assert!(dt < 4e-3 && dl < 3e-2, "dT {dt} relative light {dl}");
        let (dt, dl, _) = run_volume_case(&g, dense, true, "dense, cascade shadows");
        assert!(dt < 4e-3 && dl < 3e-2, "dT {dt} relative light {dl}");
        // Recovered Athena values: the transmittance must agree; the in-scattered light is ~1e-4 of the scene, made of half-float subnormal quanta, where the
        // GPU and the CPU round borderline values differently, so only its magnitude is asserted.
        let (dt, _, peak) = run_volume_case(&g, VolFogParams::athena_day(), true, "recovered Athena Day values");
        assert!(dt < 4e-3, "dT {dt}");
        assert!(peak < 1e-2, "peak in-scattered light {peak}");
    }
}
