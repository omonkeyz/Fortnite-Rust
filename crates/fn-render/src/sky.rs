//! Athena day sky: dome + sun billboard + cloud dome, ported from the prototype (src/sky.rs, shaders/sky_*.frag).
//!
//!  * sky dome  : FortniteSkyDome_Optimized (StarMapMeshComponent, scale 37500/37500/18750) + M_AridSky_Inst_Day
//!  * cloud dome: FortniteSkyDome_Optimized (SkyDomeMeshComp, scale 2000/2000/167.7, 1000 m below the origin) + M_AridClouds_Inst_Day
//!  * sun       : SM_FlatSunMoon_Arid + M_Sun_Arid (SunScale 2000, parent offset 3.6e6 cm), Day time-of-day weights
//! The cooked packages carry no editor material graph, so the material maths is ported from the pixel shaders UE compiled into
//! them (DXBC disassembly), with the material constant buffers in `sky_params` (generated from the serialized uniform sets).
//! Time of day: everything phase-dependent (sun direction, dome colours, cloud colours, sun colour weights) is read from
//! `crate::time_of_day` (default = the 1.8 sunset look). The Day constants in `sky_params` are only the parent-material defaults.
//! NOT recovered (provisional): the exact native motion of the sun over the day (see time_of_day::sun_rotation_pyr_deg_at), mesh tangent frames (rebuilt from uv0), the
//! volumetric-fog tail of the cloud shader.
use crate::sky_params;
use crate::texture::{self, MipFilter};
use glam::{Mat4, Vec3};
use std::path::{Path, PathBuf};

// ---- recovered placement data (prototype reports/sky-TODM_A*.json) ----
/// FortniteSkyDome_Optimized.ExtendedBounds sphere radius (cm).
pub const DOME_RADIUS_CM: f32 = 4152.375;
/// TODM_A StarMapMeshComponent.RelativeScale3D (carries M_AridSky_Inst_Day in TODM_A_Child_Athena).
pub const STAR_DOME_SCALE: [f32; 3] = [37500., 37500., 18750.];
/// TODM_A_Child_Athena SkyDomeMeshComp.RelativeScale3D / RelativeLocation.z (carries M_AridClouds_Inst_Day).
pub const CLOUD_DOME_SCALE: [f32; 3] = [2000., 2000., 167.699_97];
pub const CLOUD_DOME_CENTER_Z_CM: f32 = -100000.;
/// TODM_A_Child_Athena SunScale (also MoonScale): the sun / moon component's scale (exe FUN_14064f2e0 sets it from SunScale / MoonScale, replacing the
/// SunMeshComponent template's RelativeScale3D 800).
pub const SUN_SCALE: f32 = 2000.;
/// MATERIAL PARAMETERS (2026-10-06): distance of the sun / moon model from the world origin = `AFortTimeOfDayManager::DistanceToSunOrMoon` (7.92e6 cm,
/// [`crate::time_of_day::DISTANCE_TO_SUN_OR_MOON_CM`]): `PositionTheSunAndMoonModels` sets the component's WORLD location to `dir * DistanceToSunOrMoon` every tick,
/// replacing the SunMeshComponent template's RelativeLocation (0, 0, 3.6e6) that this used to be (the disc was drawn 2.2x too close, so 2.2x too large).
pub const SUN_DISTANCE_CM: f32 = crate::time_of_day::DISTANCE_TO_SUN_OR_MOON_CM;
/// Day-phase weights for the Material Parameter Collection values the sun shader reads (morning, day, evening, night).
/// MATERIAL PARAMETERS: `FortniteMaterialParameters.FortniteActiveTimeOfDay`, written by the manager tick (exe FUN_14064da00, FUN_140635510 per phase) as the
/// blended phase weights, which the sky uses at runtime (`time_of_day::TodState::weights`); this is the pure-Day value kept for tests/tools.
pub const SUN_TOD_DAY: [f32; 4] = [0., 1., 0., 0.];

/// Layer mask for debug captures.
pub const LAYER_DOME: u32 = 1;
pub const LAYER_SUN: u32 = 2;
pub const LAYER_CLOUDS: u32 = 4;
pub const LAYER_DOME_FOG: u32 = 8;
pub const LAYER_ALL: u32 = 0xF;

// ---- pure helpers (unit tested) ----
/// Port of the horizon band: f = 1 - max(1-(uv2y*X+Add),0)^Pow, clamped >= 0 (M_AridSky_Inst_Day: X 2, Add -1, Pow 5).
pub fn horizon_factor(uv2y: f32, x: f32, add: f32, pow: f32) -> f32 { (1. - (1. - (uv2y * x + add)).max(0.).powf(pow)).max(0.) }
/// uv2.y of the dome for an elevation angle as seen from the sphere centre (0.5 at the horizon, 1 at the zenith).
pub fn uv2y_for_latitude(latitude_rad: f32) -> f32 { 0.5 + 0.5 * latitude_rad / std::f32::consts::FRAC_PI_2 }
/// Latitude (radians) on the unit sphere mesh of a point displayed at `elevation_rad` once the mesh is scaled by (1,1,sz/sxy).
pub fn mesh_latitude_for_elevation(elevation_rad: f32, z_over_xy: f32) -> f32 { elevation_rad.sin().atan2(elevation_rad.cos() * z_over_xy) }
/// Same as [`dome_base_color`] for an arbitrary phase blend (e.g. `time_of_day::current().sky_dome`).
pub fn dome_base_color_for(d: &crate::time_of_day::SkyDome, uv2y: f32) -> [f32; 3] {
    let f = horizon_factor(uv2y, d.horiz_x, d.horiz_add, d.horiz_pow);
    std::array::from_fn(|i| f * (d.zenith[i] - d.horizon[i]) + d.horizon[i])
}
/// Pixel-shader constant buffer of M_AridClouds for a phase blend: the Day instance buffer with the per-phase parameters substituted
/// (slot layout from `sky_params::ARIDCLOUDS_DAY_NAMES`: each vector parameter is stored twice, rgb with w=1 then rgb with w=0).
pub fn cloud_constants(c: &crate::time_of_day::CloudPhase) -> [[f32; 4]; 22] {
    let mut cb = sky_params::ARIDCLOUDS_DAY;
    let mut set = |slot: usize, rgb: [f32; 3]| { let w = cb[slot][3]; cb[slot] = [rgb[0], rgb[1], rgb[2], w]; };
    for (slots, rgb) in [([1, 2], c.sun_tint), ([3, 4], c.normal_modulator), ([5, 6], c.ground_cloud), ([7, 8], c.direct_sun_light), ([9, 12], c.zenith), ([10, 11], c.horizon), ([13, 14], c.fog_color)] {
        for s in slots { set(s, rgb); }
    }
    cb[21][2] = c.opacity_master;
    cb
}
/// Linear sky colour of the dome material alone (no stars/galaxy/fog) for uv2y, from the Day instance constants.
pub fn dome_base_color(uv2y: f32) -> [f32; 3] {
    let p = &sky_params::ARIDSKY_DAY;
    let f = horizon_factor(uv2y, p[6][1], p[6][2], p[6][3]);
    std::array::from_fn(|i| f * (p[3][i] - p[4][i]) + p[4][i])
}

// ---- mesh ----
#[derive(Clone, Copy, Debug)]
pub struct MeshVertex { pub pos: [f32; 3], pub uv0: [f32; 2], pub uv1: [f32; 2], pub uv2: [f32; 2], pub color: [f32; 4] }
pub struct Mesh { pub verts: Vec<MeshVertex>, pub indices: Vec<u32> }
/// Parses assets/sky/*.bin ("SKYM", counts, 13 f32 per vertex, u32 indices) as written by the prototype's tools/sky-mesh-export.cjs.
pub fn parse_mesh(b: &[u8]) -> Result<Mesh, String> {
    if b.len() < 16 || &b[0..4] != b"SKYM" { return Err("not a SKYM mesh".into()); }
    let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let (nv, ni, ch) = (u(4) as usize, u(8) as usize, u(12));
    if ch != 3 { return Err(format!("unexpected uv channel count {ch}")); }
    if b.len() != 16 + nv * 52 + ni * 4 { return Err(format!("size mismatch: {} bytes for {nv} vertices / {ni} indices", b.len())); }
    let verts = (0..nv).map(|i| {
        let o = 16 + i * 52;
        let g = |k: usize| f(o + k * 4);
        MeshVertex { pos: [g(0), g(1), g(2)], uv0: [g(3), g(4)], uv1: [g(5), g(6)], uv2: [g(7), g(8)], color: [g(9), g(10), g(11), g(12)] }
    }).collect();
    let io = 16 + nv * 52;
    let indices: Vec<u32> = (0..ni).map(|i| u(io + i * 4)).collect();
    if indices.iter().any(|&i| i as usize >= nv) { return Err("index out of range".into()); }
    Ok(Mesh { verts, indices })
}
fn ue_to_gl(v: Vec3) -> Vec3 { crate::lighting::ue_to_world(v) }
/// Dome vertex position (metres, renderer world, relative to the TOD origin) for a mesh vertex scaled by `scale` (cm) and offset by `center_z_cm`.
pub fn dome_position_m(pos: [f32; 3], scale: [f32; 3], center_z_cm: f32) -> Vec3 {
    ue_to_gl(Vec3::new(pos[0] * scale[0], pos[1] * scale[1], pos[2] * scale[2] + center_z_cm) / 100.)
}
/// Cloud dome centre in world metres: TOD origin (assumed world origin) offset by SkyDomeMeshComp.RelativeLocation.z.
pub fn dome_center_world() -> Vec3 { ue_to_gl(Vec3::new(0., 0., CLOUD_DOME_CENTER_Z_CM) / 100.) }
/// Per-vertex tangent frame (tangent Z, tangent X) in world space for the cloud dome.
/// APPROXIMATION: the mesh's own tangents are not in the PSK export. Z is the inward sphere normal, X follows the uv0 u gradient.
pub fn cloud_frames(mesh: &Mesh, scale: [f32; 3]) -> Vec<([f32; 3], [f32; 3])> {
    let mut acc = vec![Vec3::ZERO; mesh.verts.len()];
    for t in mesh.indices.chunks(3) {
        let (a, b, c) = (&mesh.verts[t[0] as usize], &mesh.verts[t[1] as usize], &mesh.verts[t[2] as usize]);
        let p = |v: &MeshVertex| Vec3::from(v.pos);
        let (e1, e2) = (p(b) - p(a), p(c) - p(a));
        let (du1, dv1, du2, dv2) = (b.uv0[0] - a.uv0[0], b.uv0[1] - a.uv0[1], c.uv0[0] - a.uv0[0], c.uv0[1] - a.uv0[1]);
        let r = du1 * dv2 - du2 * dv1;
        if r.abs() < 1e-12 { continue; }
        let tan = (e1 * dv2 - e2 * dv1) / r;
        for &i in t { acc[i as usize] += tan; }
    }
    let s = Vec3::from(scale);
    mesh.verts.iter().zip(acc).map(|(v, tan)| {
        let n = -Vec3::from(v.pos).normalize_or_zero();
        let mut tx = tan - n * tan.dot(n);
        if tx.length_squared() < 1e-12 { tx = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y }; tx -= n * tx.dot(n); }
        let tx_w = ue_to_gl(tx.normalize() * s).normalize();
        let tz_w = ue_to_gl(n / s).normalize();
        let tx_w = (tx_w - tz_w * tx_w.dot(tz_w)).normalize();
        (tz_w.to_array(), tx_w.to_array())
    }).collect()
}

// ---- CPU mirror of the fog in shaders/common.wgsl (UE ExponentialHeightFog structure) ----
/// (inscattered rgb, transmittance) for a point `world_m` seen from `cam_m` (metres), mirroring `height_fog()`.
pub fn fog_cpu(cam_m: Vec3, world_m: Vec3, to_sun: Vec3) -> ([f32; 3], f32) {
    // Delegates to the exact port of the compiled pixel shader (fog.rs); the view axis is the ray itself (a sky-light capture looks along every ray).
    let ray = world_m - cam_m;
    let dir = ray.normalize_or_zero();
    let ue = |v: Vec3, s: f32| [v.x * s, v.z * s, v.y * s]; // GL (Y up) -> Unreal (Z up); only Z matters to the height fog
    crate::fog::HeightFogPs::active(true).eval(ue(ray, 100.), ue(dir, 1.), ue(to_sun, 1.), cam_m.y * 100.)
}

// ---- assets ----
/// (relative path under assets/sky, sRGB flag from the texture's serialized SRGB property)
pub const TEXTURES: [(&str, bool); 6] = [
    ("Game/Packages/Fortress_Sky/Textures/Galaxy_Background.png", true),
    ("Game/Packages/Fortress_Sky/Textures/Galaxy_Background_Tiling.png", true),
    ("Game/VisualThreatManager/StormVisuals/LargeClouds/T_StormCloudDetail01_M.png", false),
    ("Game/Packages/Fortress_Sky/Textures/T_Sky_Stars_Sharp.png", false),
    ("Game/Packages/Fortress_Sky/Materials/Noise32x32.png", false),
    ("Game/Packages/Fortress_Sky/Materials/FinalCloudNormalAndHeightMap.png", false),
];
/// CPU-side sky assets (read from `<assets>/sky`).
pub struct SkyAssets { pub dome: Mesh, pub sun: Mesh, pub textures: Vec<image::RgbaImage>, pub mips: Vec<Vec<image::RgbaImage>> }
impl SkyAssets {
    pub fn prepare_mips(&mut self) { self.mips = self.textures.iter().zip(TEXTURES).map(|(img, (_, srgb))| texture::mip_chain(img, srgb, MipFilter::Box)).collect(); }
    /// `root` is the `assets/sky` folder of the prototype (contains dome_optimized.bin, sunmoon.bin and Game/...).
    pub fn load(root: &Path) -> Result<SkyAssets, String> {
        let read = |n: &str| std::fs::read(root.join(n)).map_err(|e| format!("{}: {e}", root.join(n).display()));
        let dome = parse_mesh(&read("dome_optimized.bin")?)?;
        let sun = parse_mesh(&read("sunmoon.bin")?)?;
        let mut textures = vec![];
        for (name, _) in TEXTURES {
            let p: PathBuf = root.join(name);
            textures.push(std::fs::read(&p).map_err(image::ImageError::IoError).and_then(|b| image::load_from_memory(&b)).map_err(|e| format!("{}: {e}", p.display()))?.to_rgba8());
        }
        Ok(SkyAssets { dome, sun, textures, mips: vec![] })
    }
}

// ---- GPU ----
/// Uniform block shared by the sky passes (struct SkyU in sky_common.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkyUniform {
    pub vp: [[f32; 4]; 4],
    pub cam_world: [f32; 4],
    pub to_sun: [f32; 4],
    pub horizon: [f32; 4],
    pub zenith: [f32; 4],
    pub params: [f32; 4],
    pub tod: [f32; 4],
    pub sun_c: [f32; 4],
    pub sun_r: [f32; 4],
    pub sun_u: [f32; 4],
    pub center_world: [f32; 4],
    pub cam_local: [f32; 4],
    /// MATERIAL PARAMETERS: `FortniteMaterialParameters.SunAndMoonModelWorldPosition` (metres, renderer world; the cloud dome's `MaterialCollection0[27]`).
    pub sun_model_world: [f32; 4],
    /// TOD LIVE: this frame's height-fog view uniforms (struct HeightFogU of common.wgsl): the dome / sun / cloud fog follows the clock
    pub fog: crate::fog::HeightFogUniform,
}
impl SkyUniform {
    pub fn new(camera: &crate::camera::CameraParams, time: f32, layers: u32) -> Self {
        let tod = crate::time_of_day::current();
        let sun = tod.to_sun; // the sun MODEL (disc); the fog's directional inscattering follows the LIGHT (`to_sun` field below)
        let light = crate::lighting::sun_direction_at(tod.hour);
        let d = &tod.sky_dome;
        let up_ref = if sun.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
        let right = sun.cross(up_ref).normalize();
        let up = right.cross(sun);
        let center = dome_center_world();
        let v4 = |v: Vec3, w: f32| [v.x, v.y, v.z, w];
        SkyUniform {
            vp: camera.sky_view_proj().to_cols_array_2d(),
            cam_world: v4(camera.eye, time),
            to_sun: v4(light, if layers & LAYER_DOME_FOG != 0 { 1. } else { 0. }),
            horizon: [d.horizon[0], d.horizon[1], d.horizon[2], 0.],
            zenith: [d.zenith[0], d.zenith[1], d.zenith[2], 0.],
            params: [d.star_brightness, d.horiz_x, d.horiz_add, d.horiz_pow],
            tod: tod.weights,
            sun_c: v4(sun * (SUN_DISTANCE_CM / 100.), 0.),
            sun_r: v4(right, 0.),
            sun_u: v4(up, SUN_SCALE / 100.),
            center_world: v4(center, 0.),
            cam_local: v4(camera.eye - center, 0.),
            // MATERIAL PARAMETERS: PositionTheSunAndMoonModels' SunAndMoonModelWorldPosition = model direction * DistanceToSunOrMoon (world origin = the TOD origin here)
            sun_model_world: v4(crate::lighting::ue_to_world(crate::time_of_day::sun_model_direction_ue(tod.hour)) * (crate::time_of_day::DISTANCE_TO_SUN_OR_MOON_CM / 100.), 1.),
            fog: crate::fog::live_uniform(true),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DomeVertex { pos: [f32; 3], tc: [f32; 4] }
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CloudVertex { pos: [f32; 3], tc: [f32; 4], tz: [f32; 3], tx: [f32; 3] }
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SunVertex { pos: [f32; 2], uv1: [f32; 2], vcol: [f32; 2] }

pub struct SkyRenderer {
    dome_pipeline: wgpu::RenderPipeline,
    sun_pipeline: wgpu::RenderPipeline,
    /// LIGHT SHAFT HOOK: the same sun draw as SEPARATE translucency (M_Sun_Arid: BLEND_Translucent, bEnableSeparateTranslucency not serialized =
    /// UMaterial ctor true; M_AridSky / M_AridClouds serialize it false), depth-tested against a sky mask (`Renderer`, sun_mask.wgsl)
    sun_sep_pipeline: wgpu::RenderPipeline,
    cloud_pipeline: wgpu::RenderPipeline,
    dome_vb: wgpu::Buffer, dome_count: u32,
    cloud_vb: wgpu::Buffer, cloud_count: u32,
    sun_vb: wgpu::Buffer, sun_count: u32,
    uniform: wgpu::Buffer,
    cloud_cb: wgpu::Buffer,
    dome_bg: wgpu::BindGroup,
    sun_bg: wgpu::BindGroup,
    cloud_bg: wgpu::BindGroup,
    bgls: [wgpu::BindGroupLayout; 3],
    hdr_format: wgpu::TextureFormat,
}

fn shader(device: &wgpu::Device, label: &str, parts: &[&str]) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(parts.join("\n").into()) })
}
fn bgl_uniform(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }
}
fn bgl_tex(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None }
}
fn bgl_sampler(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None }
}
fn attr(location: u32, offset: u64, format: wgpu::VertexFormat) -> wgpu::VertexAttribute { wgpu::VertexAttribute { format, offset, shader_location: location } }
fn sky_pipelines(device: &wgpu::Device, hdr_format: wgpu::TextureFormat, dome_bgl: &wgpu::BindGroupLayout, sun_bgl: &wgpu::BindGroupLayout, cloud_bgl: &wgpu::BindGroupLayout) -> [wgpu::RenderPipeline; 4] {
    let common = fn_live::wgsl!("../shaders/common.wgsl"); // TOD LIVE: the fog is per-frame uniform data (SkyUniform::fog)
    let sky_common = fn_live::wgsl!("../shaders/sky_common.wgsl");
    let dome_sm = shader(device, "sky_dome", &[common, sky_common, fn_live::wgsl!("../shaders/sky_dome.wgsl")]);
    let sun_sm = shader(device, "sky_sun", &[common, sky_common, fn_live::wgsl!("../shaders/sky_sun.wgsl")]);
    let cloud_sm = shader(device, "sky_clouds", &[common, sky_common, fn_live::wgsl!("../shaders/sky_clouds.wgsl")]);
    let layout = |bgl: &wgpu::BindGroupLayout, label: &str| device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[Some(bgl)], immediate_size: 0 });
    // pipelines
    let alpha_blend = wgpu::BlendState {
        color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::SrcAlpha, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
        alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::SrcAlpha, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
    };
    // PERF (2026-10-06): the sky pass runs after the depth pre-pass with the scene depth attached (read only). The dome and the clouds sit on the far plane
    // (their vertex shaders) and test LessEqual, so only the pixels nothing opaque covers are shaded; the sun pipeline of this pass ignores the depth (Always).
    let make = |label: &str, sm: &wgpu::ShaderModule, lay: &wgpu::PipelineLayout, buffers: &[wgpu::VertexBufferLayout], blend: Option<wgpu::BlendState>, compare: wgpu::CompareFunction| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(lay),
            vertex: wgpu::VertexState { module: sm, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &buffers.iter().cloned().map(Some).collect::<Vec<_>>() },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: wgpu::TextureFormat::Depth32Float, depth_write_enabled: Some(false), depth_compare: Some(compare), stencil: Default::default(), bias: Default::default() }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState { module: sm, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: hdr_format, blend, write_mask: wgpu::ColorWrites::ALL })] }),
            multiview_mask: None,
            cache: None,
        })
    };
    let dome_attrs = [attr(0, 0, wgpu::VertexFormat::Float32x3), attr(1, 12, wgpu::VertexFormat::Float32x4)];
    let cloud_attrs = [attr(0, 0, wgpu::VertexFormat::Float32x3), attr(1, 12, wgpu::VertexFormat::Float32x4), attr(2, 28, wgpu::VertexFormat::Float32x3), attr(3, 40, wgpu::VertexFormat::Float32x3)];
    let sun_attrs = [attr(0, 0, wgpu::VertexFormat::Float32x2), attr(1, 8, wgpu::VertexFormat::Float32x2), attr(2, 16, wgpu::VertexFormat::Float32x2)];
    fn vbl<'a>(stride: usize, attrs: &'a [wgpu::VertexAttribute]) -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout { array_stride: stride as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: attrs }
    }
    let dome_pipeline = make("sky dome", &dome_sm, &layout(dome_bgl, "sky dome layout"), &[vbl(std::mem::size_of::<DomeVertex>(), &dome_attrs)], None, wgpu::CompareFunction::LessEqual);
    let sun_pipeline = make("sky sun", &sun_sm, &layout(sun_bgl, "sky sun layout"), &[vbl(std::mem::size_of::<SunVertex>(), &sun_attrs)], Some(alpha_blend), wgpu::CompareFunction::Always);
    // LIGHT SHAFT HOOK: separate-translucency sun = the sun pipeline with a read-only Less depth test against the sky mask depth
    let sun_sep_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sky sun (separate translucency)"), layout: Some(&layout(sun_bgl, "sky sun sep layout")),
        vertex: wgpu::VertexState { module: &sun_sm, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[Some(vbl(std::mem::size_of::<SunVertex>(), &sun_attrs))] },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState { format: wgpu::TextureFormat::Depth32Float, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: Default::default() }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState { module: &sun_sm, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: hdr_format, blend: Some(alpha_blend), write_mask: wgpu::ColorWrites::ALL })] }),
        multiview_mask: None, cache: None,
    });
    let cloud_pipeline = make("sky clouds", &cloud_sm, &layout(cloud_bgl, "sky cloud layout"), &[vbl(std::mem::size_of::<CloudVertex>(), &cloud_attrs)], Some(alpha_blend), wgpu::CompareFunction::LessEqual);
    [dome_pipeline, sun_pipeline, sun_sep_pipeline, cloud_pipeline]
}

impl SkyRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, assets: &SkyAssets, hdr_format: wgpu::TextureFormat) -> SkyRenderer {
        // textures (6): galaxy, galaxy tile, storm, stars, noise, cloud
        let views: Vec<wgpu::TextureView> = assets.textures.iter().zip(TEXTURES).enumerate().map(|(i, (img, (name, srgb)))| {
            match assets.mips.get(i) { Some(l) => texture::upload_mip_chain(device, queue, img, l, srgb, name), None => texture::upload_rgba8(device, queue, img, srgb, MipFilter::Box, name) }.create_view(&Default::default())
        }).collect::<Vec<_>>();
        let _ = views.len();
        let sampler = texture::repeat_sampler(device, 8, "sky sampler");
        // meshes
        let dome_verts: Vec<DomeVertex> = assets.dome.indices.iter().map(|&i| {
            let v = &assets.dome.verts[i as usize];
            let p = dome_position_m(v.pos, STAR_DOME_SCALE, 0.);
            DomeVertex { pos: p.to_array(), tc: [v.uv0[0], v.uv0[1], v.uv1[1], v.uv2[1]] }
        }).collect();
        let frames = cloud_frames(&assets.dome, CLOUD_DOME_SCALE);
        let cloud_verts: Vec<CloudVertex> = assets.dome.indices.iter().map(|&i| {
            let v = &assets.dome.verts[i as usize];
            let p = dome_position_m(v.pos, CLOUD_DOME_SCALE, 0.);
            let (tz, tx) = frames[i as usize];
            CloudVertex { pos: p.to_array(), tc: [v.uv0[0], v.uv0[1], v.uv1[1], v.uv2[1]], tz, tx }
        }).collect();
        let sun_verts: Vec<SunVertex> = assets.sun.indices.iter().map(|&i| {
            let v = &assets.sun.verts[i as usize];
            SunVertex { pos: [v.pos[1], v.pos[2]], uv1: v.uv1, vcol: [v.color[0], v.color[1]] }
        }).collect();
        let dome_vb = texture::buffer_init(device, &dome_verts, wgpu::BufferUsages::VERTEX, "sky dome vb");
        let cloud_vb = texture::buffer_init(device, &cloud_verts, wgpu::BufferUsages::VERTEX, "sky cloud vb");
        let sun_vb = texture::buffer_init(device, &sun_verts, wgpu::BufferUsages::VERTEX, "sky sun vb");
        // uniforms
        let uniform = device.create_buffer(&wgpu::BufferDescriptor { label: Some("sky uniform"), size: std::mem::size_of::<SkyUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let cb2 = texture::buffer_init(device, &sky_params::SUNARID, wgpu::BufferUsages::UNIFORM, "sun cb2");
        let cb3 = texture::buffer_init(device, &cloud_constants(&crate::time_of_day::current().clouds), wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, "cloud cb3");
        // shaders
        let both = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        // layouts
        let dome_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("sky dome bgl"), entries: &[bgl_uniform(0, both), bgl_tex(1), bgl_tex(2), bgl_tex(3), bgl_tex(4), bgl_tex(5), bgl_sampler(6)] });
        let sun_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("sky sun bgl"), entries: &[bgl_uniform(0, both), bgl_uniform(1, wgpu::ShaderStages::FRAGMENT), bgl_tex(2), bgl_sampler(3)] });
        let cloud_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("sky cloud bgl"), entries: &[bgl_uniform(0, both), bgl_uniform(1, wgpu::ShaderStages::FRAGMENT), bgl_tex(2), bgl_sampler(3)] });
        fn bg_entry<'a>(binding: u32, view: &'a wgpu::TextureView) -> wgpu::BindGroupEntry<'a> { wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(view) } }
        let dome_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("sky dome bg"), layout: &dome_bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
            bg_entry(1, &views[0]), bg_entry(2, &views[1]), bg_entry(3, &views[2]), bg_entry(4, &views[3]), bg_entry(5, &views[4]),
            wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&sampler) } ] });
        let sun_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("sky sun bg"), layout: &sun_bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: cb2.as_entire_binding() },
            bg_entry(2, &views[2]), wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&sampler) } ] });
        let cloud_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("sky cloud bg"), layout: &cloud_bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: cb3.as_entire_binding() },
            bg_entry(2, &views[5]), wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&sampler) } ] });
        let [dome_pipeline, sun_pipeline, sun_sep_pipeline, cloud_pipeline] = sky_pipelines(device, hdr_format, &dome_bgl, &sun_bgl, &cloud_bgl);
        SkyRenderer {
            dome_pipeline, sun_pipeline, sun_sep_pipeline, cloud_pipeline,
            dome_count: dome_verts.len() as u32, cloud_count: cloud_verts.len() as u32, sun_count: sun_verts.len() as u32,
            dome_vb, cloud_vb, sun_vb, uniform, cloud_cb: cb3, dome_bg, sun_bg, cloud_bg,
            bgls: [dome_bgl, sun_bgl, cloud_bgl], hdr_format,
        }
    }
    pub fn rebuild_pipelines(&mut self, device: &wgpu::Device) -> Result<(), String> {
        let [a, b, c, d] = crate::renderer::checked_build(device, || sky_pipelines(device, self.hdr_format, &self.bgls[0], &self.bgls[1], &self.bgls[2]))?;
        (self.dome_pipeline, self.sun_pipeline, self.sun_sep_pipeline, self.cloud_pipeline) = (a, b, c, d);
        Ok(())
    }
    /// `fog`: the frame's height-fog view uniforms (the renderer passes the block it gives the scene).
    pub fn update(&self, queue: &wgpu::Queue, camera: &crate::camera::CameraParams, time: f32, layers: u32, fog: &crate::fog::HeightFogUniform) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&SkyUniform { fog: *fog, ..SkyUniform::new(camera, time, layers) }));
        queue.write_buffer(&self.cloud_cb, 0, bytemuck::cast_slice(&cloud_constants(&crate::time_of_day::current().clouds)));
    }
    /// Draws the enabled layers into a pass that has the HDR colour target only (no depth).
    /// LIGHT SHAFT HOOK: the sun as separate translucency (UE: after the light-shaft downsample, over the standard-translucent clouds). The pass must
    /// have a Depth32Float attachment holding the sky mask (1 = nothing opaque in front of the 36 km sun, 0 = covered).
    pub fn draw_sun_separate<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, layers: u32) {
        if layers & LAYER_SUN == 0 { return; }
        pass.set_pipeline(&self.sun_sep_pipeline);
        pass.set_bind_group(0, &self.sun_bg, &[]);
        pass.set_vertex_buffer(0, self.sun_vb.slice(..));
        pass.draw(0..self.sun_count, 0..1);
    }
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, layers: u32) {
        if layers & LAYER_DOME != 0 {
            pass.set_pipeline(&self.dome_pipeline);
            pass.set_bind_group(0, &self.dome_bg, &[]);
            pass.set_vertex_buffer(0, self.dome_vb.slice(..));
            pass.draw(0..self.dome_count, 0..1);
        }
        // draw order: sun first, clouds over it (translucent sort: sun mesh ~36 km away, cloud dome centre ~1 km)
        if layers & LAYER_SUN != 0 {
            pass.set_pipeline(&self.sun_pipeline);
            pass.set_bind_group(0, &self.sun_bg, &[]);
            pass.set_vertex_buffer(0, self.sun_vb.slice(..));
            pass.draw(0..self.sun_count, 0..1);
        }
        if layers & LAYER_CLOUDS != 0 {
            pass.set_pipeline(&self.cloud_pipeline);
            pass.set_bind_group(0, &self.cloud_bg, &[]);
            pass.set_vertex_buffer(0, self.cloud_vb.slice(..));
            pass.draw(0..self.cloud_count, 0..1);
        }
    }
}

// The sky light is NOT a capture of this sky: the Athena sky light uses SLS_SpecifiedCubemap (see sky_light.rs).

/// Convenience for callers that only need the sky matrix.
pub fn sky_matrix(camera: &crate::camera::CameraParams) -> Mat4 { camera.sky_view_proj() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn horizon_band_hits_both_ends() {
        let p = &sky_params::ARIDSKY_DAY;
        assert!(horizon_factor(0.5, p[6][1], p[6][2], p[6][3]).abs() < 1e-6, "horizon uv2y 0.5 -> f 0");
        assert!((horizon_factor(1.0, p[6][1], p[6][2], p[6][3]) - 1.).abs() < 1e-6, "zenith -> f 1");
        let h = dome_base_color(0.5);
        for i in 0..3 { assert!((h[i] - p[4][i]).abs() < 1e-6); }
        let z = dome_base_color(1.0);
        for i in 0..3 { assert!((z[i] - p[3][i]).abs() < 1e-6); }
    }
    #[test]
    fn latitude_helpers_roundtrip_at_cardinal_points() {
        assert!((uv2y_for_latitude(0.) - 0.5).abs() < 1e-6 && (uv2y_for_latitude(std::f32::consts::FRAC_PI_2) - 1.).abs() < 1e-6);
        assert!((mesh_latitude_for_elevation(0., 0.5)).abs() < 1e-6);
        assert!((mesh_latitude_for_elevation(std::f32::consts::FRAC_PI_2, 0.5) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }
    #[test]
    fn rejects_malformed_meshes() {
        assert!(parse_mesh(b"nope").is_err());
        let mut b = b"SKYM".to_vec();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&3u32.to_le_bytes());
        assert!(parse_mesh(&b).is_err());
        b.extend_from_slice(&[0u8; 52]);
        let m = parse_mesh(&b).unwrap();
        assert_eq!(m.verts.len(), 1);
    }
    #[test]
    fn dome_top_is_straight_up_and_horizon_is_plus_x_for_ue_plus_x() {
        let top = dome_position_m([0., 0., DOME_RADIUS_CM], STAR_DOME_SCALE, 0.);
        assert!(top.x.abs() < 1e-3 && top.z.abs() < 1e-3 && top.y > 0.);
        // SUN-AXIS FIX (2026-10-05): UE +X (north) is renderer +X under the canonical (x, z, y) map of lighting::ue_to_world (was -Z, 90 deg off the props)
        let fwd = dome_position_m([DOME_RADIUS_CM, 0., 0.], STAR_DOME_SCALE, 0.);
        assert!(fwd.x > 0. && fwd.z.abs() < 1e-3 && fwd.y.abs() < 1e-3);
        assert!((dome_center_world().y - -1000.).abs() < 1e-3);
    }
    #[test]
    fn fog_is_monotonic_and_capped() {
        let cam = Vec3::new(0., 2., 0.);
        let sun = crate::lighting::sun_direction();
        let t = |d: f32| fog_cpu(cam, cam + Vec3::new(0., 0., -d), sun).1;
        assert!(t(10.) > t(1000.) && t(1000.) >= t(100_000.));
        // the live phase is Evening (default clock): FogMaxOpacity 1.0 lets the transmittance fall to 0
        let max_opacity = crate::fog::active().fog_max_opacity;
        assert!((t(1.0e6) - (1. - max_opacity)).abs() < 1e-4, "FogMaxOpacity {max_opacity} floors transmittance at {}, got {}", 1. - max_opacity, t(1.0e6));
        let (ins, _) = fog_cpu(cam, cam + Vec3::new(0., 0., -5.0e5), sun);
        assert!(ins[0] > ins[1] && ins[1] > ins[2], "warm Evening fog {ins:?}");
    }
    #[test]
    fn default_sky_is_the_sunset_instance() {
        let cam = crate::camera::CameraParams::from_yaw_pitch(Vec3::new(0., 2., 0.), 0., 0., 1280, 720);
        let u = SkyUniform::new(&cam, 0., LAYER_ALL);
        assert_eq!(u.tod, [0., 0., 1., 0.]);
        assert_eq!(&u.horizon[..3], &crate::time_of_day::SKY_DOME[crate::time_of_day::EVENING].horizon);
        assert_eq!(u.params[3], 7.);
        let cb = cloud_constants(&crate::time_of_day::current().clouds);
        assert_eq!(cb[21][2], 1.0); // OpacityMaster Evening
        assert_eq!(cb[7][0], 5.0); // Direct sun light colour r
        assert_eq!(cb[7][3], 1.0); assert_eq!(cb[8][3], 0.0); // w components keep their slot roles
        assert_eq!(cb[16], sky_params::ARIDCLOUDS_DAY[16]);
    }
    #[test]
    fn sun_uniform_places_the_billboard_towards_the_sun() {
        let cam = crate::camera::CameraParams::from_yaw_pitch(Vec3::new(0., 2., 0.), 0., 0., 1280, 720);
        let u = SkyUniform::new(&cam, 1.5, LAYER_ALL);
        let sun = crate::time_of_day::sun_direction();
        let c = Vec3::new(u.sun_c[0], u.sun_c[1], u.sun_c[2]);
        assert!((c.normalize() - sun).length() < 1e-5);
        assert!((c.length() - 79200.).abs() < 1.0, "DistanceToSunOrMoon 7.92e6 cm"); // MATERIAL PARAMETERS: was the template's 3.6e6
        // SunAndMoonModelWorldPosition (the cloud dome's MaterialCollection0[27]): the same model position, from the world origin
        let m = Vec3::new(u.sun_model_world[0], u.sun_model_world[1], u.sun_model_world[2]);
        assert!((m - sun * 79200.).length() < 0.1 && u.sun_model_world[3] == 1.);
        let (r, up) = (Vec3::new(u.sun_r[0], u.sun_r[1], u.sun_r[2]), Vec3::new(u.sun_u[0], u.sun_u[1], u.sun_u[2]));
        assert!(r.dot(sun).abs() < 1e-5 && up.dot(sun).abs() < 1e-5 && r.dot(up).abs() < 1e-5);
        assert!((u.cam_world[3] - 1.5).abs() < 1e-6 && u.to_sun[3] == 1.);
        // the uniform's `to_sun` (fog directional inscattering) is the LIGHT direction, which differs from the disc by design (SunObjectDivergence)
        let light = crate::lighting::sun_direction();
        assert!((Vec3::new(u.to_sun[0], u.to_sun[1], u.to_sun[2]) - light).length() < 1e-6);
        eprintln!("disc vs light angle at the default hour: {:.2} deg", sun.dot(light).clamp(-1., 1.).acos().to_degrees());
        assert_eq!(std::mem::size_of::<SkyUniform>() % 16, 0);
    }
}
