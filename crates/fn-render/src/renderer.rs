//! The renderer: owns every pipeline and render target and records one frame as a [`FrameGraph`]:
//!
//! ```text
//! shadow cascade 0..2 (depth, skinned casters)        -> 3 x 2048^2 depth array
//! sky (dome, sun, clouds; colour only)                 -> HDR mip 0 (Rgba16Float)
//! scene (ground + skinned character parts, fog)        -> HDR mip 0 + depth (Depth32Float)
//! HDR mip chain (box downsample, bloom taps + metering)
//! exposure (compute: 1x1 mean -> adapted exposure)
//! post (AO, bloom, exposure, vignette, white balance, ACES film curve, sRGB) -> output view
//! ```
use crate::camera::{CameraParams, CameraUniform};
use crate::graph::{FrameGraph, PassTiming};
use crate::lighting::{self, Cascade, CASCADE_COUNT};
use crate::mesh::{MaterialKind, SkinnedPrimitive, SkinnedVertex};
use crate::profiler::{GpuProfiler, PassTimings};
use crate::props::{PropInstance, PropTemplates};
use crate::sky::{self, SkyAssets, SkyRenderer};
use crate::static_mesh::{StaticMaterialData, StaticMaterialHandle, StaticMeshData, StaticMeshHandle, StaticMeshInstance};
use crate::statics::{StaticRenderer, StaticStats};
use crate::texture::{self, MipFilter};
use crate::quality::{Backdrop, FxaaParams};
use crate::tonemap::{self, MeterParams, PostParams};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use image::RgbaImage;

pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const SHADOW_MAP_SIZE: u32 = 2048;
/// Format of the intermediate tone-mapped image FXAA reads.
pub const LDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const PROP_INITIAL_CAPACITY: usize = 4096;
/// Half edge of the 512 m baseplate and the extent of the visual ground ring (metres).
pub const GROUND_HALF: f32 = 256.0;
pub const GROUND_OUTER: f32 = 6000.0;

/// Runtime switches (the prototype's debug / settings toggles).
#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub shadows: bool,
    pub ao: bool,
    pub bloom: bool,
    pub vignette: bool,
    pub fog: bool,
    /// Sky layer mask (`sky::LAYER_*`).
    pub sky_layers: u32,
    /// Fixed exposure scale; `None` = auto exposure.
    pub fixed_exposure: Option<f32>,
    /// 0 final, 1 no film curve, 2 AO only, 3 bloom only, 4 exposure-scaled linear, 5 vignette only.
    pub debug_mode: i32,
    /// Material debug bits: 1 plain grass, 2 plain character, 4 fog off, 8 fog amount.
    pub material_debug: u32,
    /// Exponent applied after the sRGB encode (Brightness setting): 1.0 = unchanged.
    pub display_exponent: f32,
    /// FXAA resolve of the tone-mapped image (`quality::aa_params(level)`); `None` = off (the post pass writes straight to the target).
    pub fxaa: Option<FxaaParams>,
    /// Pause-menu backdrop (dim + blur of the finished scene); `Backdrop::OFF` normally.
    pub backdrop: Backdrop,
    /// Shadow map edge in texels (`quality::shadow_map_size`): 512 / 1024 / 2048 (any 256..=4096); the shadow texture is re-created when it changes.
    pub shadow_map_size: u32,
    /// Anisotropic filtering of the scene textures (`quality::texture_params().1`, 1 = off).
    pub anisotropy: u16,
    /// Top mip levels of the scene textures that are never sampled (`quality::texture_params().0`, sampler minimum LOD).
    pub skip_mips: u32,
    /// `sg.ShadowQuality` cvars (`quality::ShadowScalability::level`): cascade count / distance / transition, PCF kernel, small-caster cull, DF shadows / DFAO,
    /// volumetric fog and its grid, light functions. `shadows` / `shadow_map_size` stay the on/off switch and the map size.
    pub shadow_scalability: crate::quality::ShadowScalability,
    /// LIGHT SHAFT HOOK: `r.LightShaftQuality` (cooked scalability: 1 only at PostProcessQuality@3 = Epic, 0 below). `FN_LIGHT_SHAFTS=0` also turns it off.
    pub light_shafts: bool,
    /// SCALABILITY: `sg.AntiAliasingQuality` (`quality::AaScalability`): none / FXAA console / fast TAA / TAA + `r.Tonemapper.Sharpen`. A `Some` in
    /// `fxaa` (debug `--fxaa-level`) overrides it with the old FXAA-quality pass.
    pub aa: crate::quality::AaScalability,
    /// SCALABILITY: `sg.PostProcessQuality` (`quality::PostProcessScalability`): bloom quality / fast-blur threshold / filter size scale, grain
    /// quantization, eye adaptation, upscale quality. `ao` / `bloom` / `vignette` above stay the on/off switches.
    pub post_scalability: crate::quality::PostProcessScalability,
    /// SCALABILITY: `r.ViewDistanceScale` (`quality::ViewDistanceScalability`) applied to every static / foliage cull distance.
    pub view_distance_scale: f32,
    /// SCALABILITY: `r.ScreenPercentage` (`quality::screen_percentage`): the 3D view renders at this share of the window and is upscaled.
    pub screen_percentage: f32,
    /// REFLECTION APPLY: `sg.EffectsQuality` cvars the renderer implements (`quality::EffectsScalability`): `r.SSR.Quality` (screen space reflections).
    pub effects: crate::quality::EffectsScalability,
    /// POSTFX: `EngineShowFlags.MotionBlur` = the user's Motion Blur setting (`UFortGameUserSettings::bMotionBlur`, exe default true); the pass also needs
    /// `r.MotionBlurQuality` > 0 of `post_scalability` and the volume's MotionBlurAmount / Max (effect_settings.rs).
    pub motion_blur: bool,
    /// POSTFX: blend weight of the storm post-process component (`OutsideSafeZonePP0` of PlayerPawn_Athena), 0 = inside the safe zone (effect_settings.rs).
    pub storm_pp_weight: f32,
    /// POSTFX: blend weight of the scoped weapon's `DownScopePostProcess` component (B_Ranged_Generic, `AnimateScopePostProcess` timeline), 0 = not scoped.
    pub scope_pp_weight: f32,
}
impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            shadows: true, ao: true, bloom: true, vignette: true, fog: true, sky_layers: sky::LAYER_ALL, fixed_exposure: None, debug_mode: 0, material_debug: 0,
            display_exponent: 1.0, fxaa: None, backdrop: Backdrop::OFF, shadow_map_size: SHADOW_MAP_SIZE, anisotropy: 8, skip_mips: 0,
            shadow_scalability: crate::quality::ShadowScalability::level(3),
            light_shafts: true,
            aa: crate::quality::AaScalability::level(3), post_scalability: crate::quality::PostProcessScalability::level(3), view_distance_scale: 1.0, screen_percentage: 100.,
            effects: crate::quality::EffectsScalability::level(3),
            motion_blur: true, storm_pp_weight: 0., scope_pp_weight: 0.,
        }
    }
}

const PROP_CHUNK_VERTS: usize = 3 * 1024;
fn prop_cull_off() -> bool { static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *OFF.get_or_init(|| fn_live::var("FN_PROP_CULL").is_ok_and(|v| v == "0")) }

/// One vertex of the procedural props (trees, rocks): world-space position, flat normal, vertex colour (linear albedo).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct PropVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
}

/// Hook for a 2D overlay drawn into the swapchain view AFTER the post chain (UI is composited in sRGB/gamma space over the tone-mapped
/// frame, as the old runtime did). Implemented by `fn_ui_wgpu::UiPass`.
pub trait UiOverlay {
    /// Draw into `target` (already holding the finished 3D frame; load, do not clear). May encode and submit its own command buffers.
    fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::TextureView, width: u32, height: u32);
}

/// Linear HDR scene colour read back from the HDR target (debug / reference comparison).
#[derive(Clone, Debug)]
pub struct HdrImage {
    pub width: u32,
    pub height: u32,
    /// Row-major, top row first; RGB linear scene radiance, A = ambient share.
    pub pixels: Vec<[f32; 4]>,
}
impl HdrImage {
    pub fn get(&self, x: u32, y: u32) -> [f32; 4] { self.pixels[(y * self.width + x) as usize] }
    /// Mean RGB over `[x0,x1) x [y0,y1)`.
    pub fn region_mean(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> [f64; 3] {
        let mut s = [0f64; 3];
        let mut n = 0f64;
        for y in y0..y1.min(self.height) { for x in x0..x1.min(self.width) { let p = self.get(x, y); for c in 0..3 { s[c] += p[c] as f64; } n += 1.; } }
        s.map(|v| v / n.max(1.))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelId(pub usize);

pub struct FrameParams<'a> {
    pub camera: &'a CameraParams,
    /// View.RealTime in seconds (drives sky / cloud animation).
    pub time: f32,
    /// Frame delta in seconds (eye adaptation).
    pub dt: f32,
    pub models: &'a [ModelId],
    pub options: &'a RenderOptions,
}

pub struct RendererAssets {
    pub sky: SkyAssets,
    /// T_Grasslands_D.png
    pub ground_texture: RgbaImage,
    pub ground_mips: Option<Vec<RgbaImage>>,
}

#[derive(Clone, Debug)]
pub struct FrameStats {
    pub cascades: Option<[Cascade; CASCADE_COUNT]>,
    pub passes: Vec<PassTiming>,
    /// Total CPU time recording + submitting the frame.
    pub cpu_ms: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct ExposureInfo { pub ev: f32, pub target_ev: f32, pub exposure: f32, pub avg_luminance: f32 }

/// Light block (struct Light in scene.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LightUniform {
    pub sun_dir: [f32; 4],
    pub sun_radiance: [f32; 4],
    pub sky_radiance: [f32; 4],
    pub shadow_matrix: [[[f32; 4]; 4]; CASCADE_COUNT],
    pub cascade_far: [f32; 4],
    pub cascade_texel: [f32; 4],
    pub shadow_params: [f32; 4],
    pub misc: [f32; 4],
    /// xyz: `SplitFarFadeRegion` (m) of each cascade, w: `ShadowDistanceFadeoutFraction`
    pub cascade_ext: [f32; 4],
    /// xyz: `ShaderDepthBias` (== transition size) of each cascade in normalised depth units
    pub cascade_bias: [f32; 4],
    /// x: light-function grey value of the volumetric fog (`light_function::eval_cpu` near the camera; 1 = off), y: `DisabledBrightness`, z: `LightFunctionFadeDistance` (m),
    /// w: `View.GameTime` (s) the High light-function program scrolls with
    pub light_fn: [f32; 4],
    /// SKY LIGHT: `View.SkyIrradianceEnvironmentMap[7]` with `View.SkyLightColor` folded in (`lighting::sky_sh`), evaluated by `sky_lookup`
    pub sky_sh: [[f32; 4]; 7],
    /// TOD LIVE: this frame's height-fog view uniforms (`fog::live_uniform`, struct HeightFogU of common.wgsl)
    pub fog: crate::fog::HeightFogUniform,
    /// LIGHT FUNCTION (2026-10-07): xyz = `light_function::ue_offset_cm` (renderer world -> absolute UE world), w = 1 when the textured High light function runs
    /// (`EngineShowFlags.LightFunctions`), 0 = no light function (attenuation 1). Appended last: the shaders that mirror only a prefix of the block stay valid.
    pub light_fn_world: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GroundVertex { pos: [f32; 3], uv: [f32; 2] }
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ObjectUniform { model: [[f32; 4]; 4] }
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialUniform { kind: [u32; 4] }

#[derive(Clone)]
struct GpuPrimitive {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    index_count: u32,
    material_bg: wgpu::BindGroup,
    /// BUILD ANIM HOOK: the material uniform (`kind`, then the per-model scalar of `set_model_scalar`), kept for rewrites.
    kind_buf: wgpu::Buffer, kind: MaterialKind,
    /// CHAR MATERIAL HOOK: the original compiled character material of this primitive (bind group 3 of the character pipeline), see char_material.rs
    char_bg: Option<(wgpu::BindGroup, wgpu::Buffer)>,
    char_src: Option<std::sync::Arc<crate::char_material::CharSrc>>,
    char_cb: Option<wgpu::Buffer>,
    /// CHAR MATERIAL HOOK: an eye-region modulate overlay (not drawn in the scene / shadow passes, drawn after the scene with its own pipeline)
    overlay: Option<crate::char_material::GpuOverlay>,
    /// The material's `TwoSided` flag: both faces go into the shadow depth (UE4.16 ShadowDepthRendering.cpp:1059 `Material->IsTwoSided() || CastsShadowAsTwoSided()`).
    two_sided: bool,
}
struct Model {
    prims: Vec<GpuPrimitive>,
    obj: wgpu::Buffer,
    bones: wgpu::Buffer,
    bone_count: usize,
    obj_bg: wgpu::BindGroup,
    /// Bounding sphere (centre, radius) of the reference-pose vertices in model space, like `FBoxSphereBounds(Points)` (box centre, max distance). UE's skinned
    /// bounds come from the physics asset (`USkinnedMeshComponent::CalcBounds`), which this renderer does not have; used by the small-caster shadow cull.
    bounds: (Vec3, f32),
    /// Last model-to-world matrix from `update_model` (the shadow cull reads it).
    world: std::sync::Mutex<Mat4>,
    /// TEMPORAL AA: the previous frame's object matrix / bones (velocity pass group 2), copied from `obj` / `bones` after each frame; `prev_valid`
    /// false until the first copy (a new model then has no motion of its own).
    prev_obj: wgpu::Buffer,
    prev_bones: wgpu::Buffer,
    prev_bg: wgpu::BindGroup,
    prev_valid: std::sync::atomic::AtomicBool,
}

/// See [`Renderer::model_builder`].
#[derive(Clone)]
pub struct ModelBuilder { device: wgpu::Device, queue: wgpu::Queue, bgl_material: wgpu::BindGroupLayout, bgl_obj_skinned: wgpu::BindGroupLayout, white: wgpu::Texture, neutral: [wgpu::Texture; 3], char_bgl: wgpu::BindGroupLayout, overlay_bgl: wgpu::BindGroupLayout }
/// A model uploaded by a [`ModelBuilder`], not yet installed in a [`Renderer`].
pub struct BuiltModel(Model);
impl ModelBuilder {
    /// Same upload as `Renderer::create_model`, without touching the renderer.
    pub fn build(&self, prims: &[SkinnedPrimitive], bone_count: usize, label: &str) -> BuiltModel { BuiltModel(self.build_model(prims, bone_count, label)) }
    fn build_model(&self, prims: &[SkinnedPrimitive], bone_count: usize, id: &str) -> Model {
        let device = &self.device;
        let queue = &self.queue;
        let mut gpu_prims = vec![];
        for (k, p) in prims.iter().enumerate() {
            let label = format!("{id} prim {k}");
            let vb = texture::buffer_init(device, &p.vertices, wgpu::BufferUsages::VERTEX, &label);
            let ib = texture::buffer_init(device, &p.indices, wgpu::BufferUsages::INDEX, &label);
            // TEXTURE MEMORY (2026-10-06): a slot's cooked mip chain (`MaterialImages::cooked`) replaces the image and the mips generated from it
            let upload = |img: &Option<RgbaImage>, cooked: &Option<std::sync::Arc<crate::mesh::CookedTexture>>, fallback: &wgpu::Texture| -> wgpu::TextureView {
                match (img, cooked) {
                    (Some(_), Some(c)) => c.upload(device, queue, false, &label).create_view(&Default::default()),
                    (Some(i), None) => texture::upload_rgba8(device, queue, i, false, MipFilter::Triangle, &label).create_view(&Default::default()),
                    (None, _) => fallback.create_view(&Default::default()),
                }
            };
            // a primitive without a diffuse texture is drawn untextured (generic path), like the prototype
            let kind = if p.material.diffuse.is_none() { MaterialKind::Generic } else { p.kind };
            // BUILD ANIM HOOK: kind.y = f32 bits of the per-model scalar (Architect: EmissiveMultiply, the MIC value 1.0 until set_model_scalar)
            let kind_buf = texture::buffer_init(device, &[MaterialUniform { kind: [kind as u32, 1f32.to_bits(), 0, 0] }], wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, &label);
            let c = &p.material.cooked;
            let (d, n, s, m) = (upload(&p.material.diffuse, &c[0], &self.white), upload(&p.material.normal, &c[1], &self.neutral[0]), upload(&p.material.spec, &c[2], &self.neutral[1]), upload(&p.material.mask, &c[3], &self.neutral[2]));
            let material_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(&label), layout: &self.bgl_material, entries: &[bg_buf(0, &kind_buf), bg_tex(1, &d), bg_tex(2, &n), bg_tex(3, &s), bg_tex(4, &m)] });
            // CHAR MATERIAL HOOK: the original compiled character material (scene pass) or the eyeshadow modulate overlay (own pass)
            let mut char_src = None;
            let mut char_cb = None;
            let (char_bg, overlay) = match p.material.ue.as_deref() {
                Some(crate::char_material::UeMaterial::Char(c)) => { let (bg, src, buf) = crate::char_material::char_binding_with(&self.char_bgl, device, queue, c, &label); char_src = Some(src); char_cb = Some(buf); (Some((bg, crate::char_material::char_tangent_buffer(device, c, p.vertices.len(), &label))), None) }
                Some(crate::char_material::UeMaterial::Eyeshadow(e)) => (None, Some(crate::char_material::overlay_with(&self.overlay_bgl, device, queue, e, &label))),
                None => (None, None),
            };
            gpu_prims.push(GpuPrimitive { vb, ib, index_count: p.indices.len() as u32, material_bg, kind_buf, kind, char_bg, char_src, char_cb, overlay, two_sided: p.material.two_sided });
        }
        let obj = device.create_buffer(&wgpu::BufferDescriptor { label: Some("model object"), size: 64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let bone_count = bone_count.max(1);
        let identity: Vec<[[f32; 4]; 4]> = vec![Mat4::IDENTITY.to_cols_array_2d(); bone_count];
        let bones = texture::buffer_init(device, &identity, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, "model bones");
        // TEMPORAL AA: previous-frame copies for the velocity pass
        let prev_obj = device.create_buffer(&wgpu::BufferDescriptor { label: Some("model object (prev)"), size: 64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let prev_bones = texture::buffer_init(device, &identity, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, "model bones (prev)");
        let prev_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("model object bg (prev)"), layout: &self.bgl_obj_skinned, entries: &[bg_buf(0, &prev_obj), bg_buf(1, &prev_bones)] });
        queue.write_buffer(&obj, 0, bytemuck::bytes_of(&ObjectUniform { model: Mat4::IDENTITY.to_cols_array_2d() }));
        let obj_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("model object bg"), layout: &self.bgl_obj_skinned, entries: &[bg_buf(0, &obj), bg_buf(1, &bones)] });
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in prims { for v in &p.vertices { lo = lo.min(Vec3::from(v.pos)); hi = hi.max(Vec3::from(v.pos)); } }
        let bounds = if lo.x <= hi.x {
            let c = (lo + hi) * 0.5;
            (c, prims.iter().flat_map(|p| p.vertices.iter()).map(|v| (Vec3::from(v.pos) - c).length()).fold(0f32, f32::max))
        } else { (Vec3::ZERO, 0.) };
        Model { prims: gpu_prims, obj, bones, bone_count, obj_bg, bounds, world: std::sync::Mutex::new(Mat4::IDENTITY), prev_obj, prev_bones, prev_bg, prev_valid: std::sync::atomic::AtomicBool::new(false) }
    }
}

struct Targets {
    width: u32,
    height: u32,
    hdr: wgpu::Texture,
    hdr_mip_views: Vec<wgpu::TextureView>,
    depth_view: wgpu::TextureView,
    post_bg: wgpu::BindGroup,
    downsample_bgs: Vec<wgpu::BindGroup>,
    meter_bg: wgpu::BindGroup,
    /// LDR (Rgba8Unorm) tone-mapped image + FXAA bind group, only while FXAA is on.
    ldr: Option<(wgpu::TextureView, wgpu::BindGroup)>,
    /// SCOPE HOOK: second LDR image (output of the scope post program), only while FXAA and the scope pass are both on.
    ldr2: Option<(wgpu::TextureView, wgpu::BindGroup)>,
    /// SCALABILITY (r.ScreenPercentage < 100): the view-size LDR image the upscale pass reads (the post chain writes it instead of the window).
    up_src: Option<wgpu::TextureView>,
    /// REFLECTION APPLY: the lit passes' reflection-input targets (reflection.rs)
    refl: crate::reflection::ReflTargets,
}

/// PERF (report only): the caster sources of the per-cascade shadow statistics (`FN_SHADOW_STATS=1`).
const SHADOW_STAT_SOURCES: [&str; 7] = ["characters (skinned)", "props (merged buffer)", "statics instanced opaque", "statics instanced masked", "fx placed statics", "fx building / pickups", "gun"];
pub struct Renderer {
    batch: std::sync::Mutex<Option<Vec<wgpu::CommandBuffer>>>,
    batch_after: std::sync::atomic::AtomicBool,
    device: wgpu::Device,
    queue: wgpu::Queue,
    output_format: wgpu::TextureFormat,
    // layouts
    bgl_material: wgpu::BindGroupLayout,
    bgl_obj_skinned: wgpu::BindGroupLayout,
    bgl_post: wgpu::BindGroupLayout,
    bgl_downsample: wgpu::BindGroupLayout,
    bgl_meter: wgpu::BindGroupLayout,
    /// TERRAIN MAIN PASS (2026-10-07): the cascades' LightViewProj layout, kept for the landscape caster pipeline (backdrop.rs, built in set_backdrop).
    bgl_shadow: wgpu::BindGroupLayout,
    // pipelines
    ground_pipeline: wgpu::RenderPipeline,
    /// PREPASS: depth-only ground pipeline of the depth pre-pass.
    ground_depth_pipeline: wgpu::RenderPipeline,
    skinned_pipeline: wgpu::RenderPipeline,
    prop_pipeline: wgpu::RenderPipeline,
    /// FULL PREPASS (2026-10-06): the depth pre-pass twins of `skinned_pipeline` / `prop_pipeline` (same vertex stage, no pixel stage).
    skinned_depth_pipeline: wgpu::RenderPipeline,
    prop_depth_pipeline: wgpu::RenderPipeline,
    /// [one-sided material (back faces culled), two-sided material (no culling)]
    shadow_pipeline: [wgpu::RenderPipeline; 2],
    shadow_prop_pipeline: wgpu::RenderPipeline,
    downsample_pipeline: wgpu::RenderPipeline,
    meter_pipeline: wgpu::ComputePipeline,
    /// PERF: the many-workgroup histogram pass of the exposure meter and its histogram buffer (meter.wgsl).
    meter_hist_pipeline: wgpu::ComputePipeline,
    meter_hist_buf: wgpu::Buffer,
    post_pipeline: wgpu::RenderPipeline,
    post_ldr_pipeline: wgpu::RenderPipeline,
    fxaa_pipeline: wgpu::RenderPipeline,
    bgl_fxaa: wgpu::BindGroupLayout,
    fxaa_buf: wgpu::Buffer,
    bgl_globals: wgpu::BindGroupLayout,
    shadow_cmp: wgpu::Sampler,
    /// PERF (2026-10-06): the point sampler of the shadow-depth gathers (globals binding 15).
    shadow_point: wgpu::Sampler,
    tex_sampler: wgpu::Sampler,
    shadow_size: u32,
    sampler_key: (u16, u32),
    prop_vb: wgpu::Buffer,
    prop_capacity: usize,
    prop_count: u32,
    prop_chunks: Vec<(u32, u32, Vec3, f32)>,
    prop_draws: [Vec<std::ops::Range<u32>>; 1 + CASCADE_COUNT],
    /// PERF (report only, `FN_SHADOW_STATS=1`): frames and per-cascade (draws, triangles) sums by caster source ([`SHADOW_STAT_SOURCES`]).
    shadow_stats: (u64, [[(u64, u64); SHADOW_STAT_SOURCES.len()]; CASCADE_COUNT]),
    sky: SkyRenderer,
    // shared resources
    camera_buf: wgpu::Buffer,
    light_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    #[allow(dead_code)]
    shadow_array_view: wgpu::TextureView,
    shadow_layer_views: Vec<wgpu::TextureView>,
    shadow_vp_bufs: Vec<wgpu::Buffer>,
    shadow_vp_bgs: Vec<wgpu::BindGroup>,
    clamp_sampler: wgpu::Sampler,
    neutral: [wgpu::Texture; 3],
    white: wgpu::Texture,
    post_buf: wgpu::Buffer,
    meter_buf: wgpu::Buffer,
    exposure_buf: wgpu::Buffer,
    // ground
    ground_vb: wgpu::Buffer,
    ground_count: u32,
    ground_material_bg: wgpu::BindGroup,
    ground_obj_bg: wgpu::BindGroup,
    // state
    models: Vec<Model>,
    targets: Option<Targets>,
    frame_index: u64,
    profiler: GpuProfiler,
    prop_templates: PropTemplates,
    prop_scratch: Vec<PropVertex>,
    statics: StaticRenderer,
    // RENDER FX HOOK: the translucent / unlit / WPO / masked material pass (fx.rs) and the generation of the targets its scene-depth bind groups were built for
    fx: crate::fx::FxRenderer,
    fx_gen: u64,
    // PARTICLE HOOK: the particle pass (particles.rs), drawn after the FX pass
    particles: crate::particles::ParticleRenderer,
    // GUN MATERIAL HOOK: the held firearms drawn with their original material programs (gun_material.rs), inside the scene pass
    gun: crate::gun_material::GunRenderer,
    // SKY REFLECTION HOOK: the sky light's processed specular cube + PreIntegratedGF (globals bindings 11..14), see sky_reflection.rs
    sky_refl: crate::sky_reflection::SkyReflection,
    // LIGHT FUNCTION (2026-10-07): the sun light function textures (globals bindings 20..22), see light_function.rs
    light_function: crate::light_function::LightFunctionGpu,
    local_lights: crate::local_lights::LocalLightsGpu,
    sss: crate::subsurface::Subsurface,
    sss_on: bool,
    // SCOPE HOOK: the scoped weapon's post-process material pass + mesh (scope_pass.rs), between `post` and FXAA
    scope: crate::scope_pass::ScopeRenderer,
    /// OUTLINE: the build preview's highlight (custom depth + SkinAndMeshOutlinePostProcess), see outline_pass.rs
    outline: crate::outline_pass::OutlineRenderer,
    decals: crate::decal_pass::DecalRenderer,
    // CHAR MATERIAL HOOK: the BR character materials' pipelines (head / hair / body skin, eyeshadow modulate overlay), see char_material.rs
    char_gpu: crate::char_material::CharGpu,
    // VOLUMETRIC FOG HOOK: the volumetric fog volume + passes (volfog.rs), composited right after the opaque scene
    volfog: crate::volfog::VolFog,
    // MIRRORFIX (translucency volume): the translucency lighting volume the translucent lit FX materials read (translucent_lighting.rs)
    tlv: crate::translucent_lighting::TranslucentLighting,
    // BACKDROP HOOK: the landscape / ocean / far skybox pass between `sky` and `scene` (backdrop.rs)
    backdrop: Option<crate::backdrop::BackdropRenderer>,
    // DISTANCE FIELD HOOK: mesh distance fields, the ray-traced DF shadow cascade and DFAO (distance_field.rs / distance_field_ao.rs)
    df: crate::distance_field::DistanceFields,
    // BLOOM HOOK: the UE 4.16 bloom chain (SceneColorHalfRes, 5 downsamples, Bloom6..1 Gaussians) over the finished HDR scene (fn_postfx::bloom),
    // and the chain output the post / meter bind groups were built for
    bloom: fn_postfx::bloom::BloomChain,
    // COLOR LUT (GLOBAL LOOK): FCombineLUTs volume LUT, rebuilt from post_buf every frame inside the post node, sampled by post.wgsl (binding 10)
    color_lut: crate::color_lut::ColorLut,
    bloom_bound: Option<(usize, usize, u64, usize)>,
    // LIGHT SHAFT HOOK: light-shaft bloom of the sun (light_shafts.rs), downsample + radial blur before the separate-translucency recombine, applied after it
    light_shafts: crate::light_shafts::LightShafts,
    // AO ORDER: the screen-space AO pass right after the opaque scene (ssao_pass.rs; was in post.wgsl after the half-res downsample); REFLECTION APPLY: it also
    // composites the deferred reflections (sky specular + SSR, reflection.rs)
    ssao: crate::ssao_pass::SsaoPass,
    // UE SSAO: the UE 4.16 screen-space AO passes (setup, AO per resolution level, full-res ScreenSpaceAO), ssao_ue.rs
    ssao_ue: crate::ssao_ue::UeSsao,
    // REFLECTION APPLY: the screen space reflections (HZB, trace, SSR temporal filter), reflection.rs
    ssr: crate::reflection::Ssr,
    /// OCCLUSION (2026-10-07): the HZB readback ring of the statics' occlusion culling (occlusion.rs) and the camera of this frame's HZB.
    occlusion: crate::occlusion::Occlusion, occl_cam: Option<crate::camera::CameraParams>,
    // SCALABILITY: temporal AA (High / Epic), its velocity pass, the Medium FXAA console pass and the screen-percentage upscale (taa.rs)
    taa: crate::taa::TemporalAa,
    // LIGHT SHAFT HOOK: sky mask of the separate-translucency sun (light_shafts::SunMask)
    sun_mask: crate::light_shafts::SunMask,
    bgl_obj_static: wgpu::BindGroupLayout,
    shader_gen: u64,
}

/// BLOOM HOOK: the Athena bloom settings (identical in the four TOD phases: TODM_A Bloom3Tint 0.5, Bloom6Size/Tint 0, BloomSizeScale 1, the
/// rest exe ctor defaults incl. BloomThreshold -1), see fn_postfx::params::athena_day_cm. BloomIntensity is applied by post.wgsl (tonemap::BLOOM_INTENSITY).
fn bloom_params() -> fn_postfx::params::BloomParams { fn_postfx::params::athena_day_cm().2 }

fn create_shadow_views(device: &wgpu::Device, size: u32) -> (wgpu::TextureView, Vec<wgpu::TextureView>) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shadow cascades"), size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: CASCADE_COUNT as u32 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
    });
    let array = tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
    let layers = (0..CASCADE_COUNT as u32).map(|i| tex.create_view(&wgpu::TextureViewDescriptor { label: Some("shadow layer"), dimension: Some(wgpu::TextureViewDimension::D2), base_array_layer: i, array_layer_count: Some(1), ..Default::default() })).collect();
    (array, layers)
}
fn wgsl(device: &wgpu::Device, label: &str, parts: &[&str]) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(parts.join("\n").into()) })
}
pub(crate) fn checked_build<T>(device: &wgpu::Device, build: impl FnOnce() -> T) -> Result<T, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let out = build();
        match pollster::block_on(scope.pop()) {
            Some(e) => {
                let mut s = e.to_string();
                let mut src = std::error::Error::source(&e);
                while let Some(x) = src { s.push_str(": "); s.push_str(&x.to_string()); src = x.source(); }
                Err(s)
            }
            None => Ok(out),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = device;
        Ok(build())
    }
}
struct MainPipes {
    ground_pipeline: wgpu::RenderPipeline,
    ground_depth_pipeline: wgpu::RenderPipeline,
    skinned_pipeline: wgpu::RenderPipeline,
    prop_pipeline: wgpu::RenderPipeline,
    skinned_depth_pipeline: wgpu::RenderPipeline,
    prop_depth_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: [wgpu::RenderPipeline; 2],
    shadow_prop_pipeline: wgpu::RenderPipeline,
    downsample_pipeline: wgpu::RenderPipeline,
    meter_pipeline: wgpu::ComputePipeline,
    meter_hist_pipeline: wgpu::ComputePipeline,
    post_pipeline: wgpu::RenderPipeline,
    post_ldr_pipeline: wgpu::RenderPipeline,
    fxaa_pipeline: wgpu::RenderPipeline,
}
#[allow(clippy::too_many_arguments)]
fn main_pipelines(device: &wgpu::Device, output_format: wgpu::TextureFormat, bgl_globals: &wgpu::BindGroupLayout, bgl_material: &wgpu::BindGroupLayout, bgl_obj_static: &wgpu::BindGroupLayout, bgl_obj_skinned: &wgpu::BindGroupLayout, bgl_shadow: &wgpu::BindGroupLayout, bgl_post: &wgpu::BindGroupLayout, bgl_fxaa: &wgpu::BindGroupLayout, bgl_downsample: &wgpu::BindGroupLayout, bgl_meter: &wgpu::BindGroupLayout, progress: &mut dyn FnMut(&'static str)) -> MainPipes {
    let common = fn_live::wgsl!("../shaders/common.wgsl");
    let lit_common = fn_live::wgsl!("../shaders/lit_common.wgsl");
    let scene_sm = wgsl(&device, "scene", &[common, lit_common, fn_live::wgsl!("../shaders/scene.wgsl")]);
    let shadow_sm = wgsl(&device, "shadow", &[fn_live::wgsl!("../shaders/shadow.wgsl")]);
    let post_sm = wgsl(&device, "post", &[common, fn_live::wgsl!("../shaders/post.wgsl")]);
    let down_sm = wgsl(&device, "downsample", &[fn_live::wgsl!("../shaders/downsample.wgsl")]);
    let meter_sm = wgsl(&device, "meter", &[fn_live::wgsl!("../shaders/meter.wgsl")]);
    let fxaa_sm = wgsl(&device, "fxaa", &[fn_live::wgsl!("../shaders/fxaa.wgsl")]);

    // ---- pipelines ----
    let pl = |label: &str, bgls: &[&wgpu::BindGroupLayout]| {
        let entries: Vec<Option<&wgpu::BindGroupLayout>> = bgls.iter().map(|b| Some(*b)).collect();
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &entries, immediate_size: 0 })
    };
    let scene_depth = wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: Default::default() };
    // FULL PREPASS (2026-10-06): UE4.16 1.8 runs a FULL depth pre-pass (cooked r.SelectiveBasePassOutputs 1 / r.StencilForLODDither 1 -> ShouldForceFullDepthPass ->
    // EarlyZPassMode DDM_AllOpaque, bEarlyZPassMovable, DeferredShadingRenderer.cpp:146-182) and the base pass reads the depth only (:773 bAllowReadonlyDepthBasePass):
    // the skinned prims without a CharacterShader owner and the procedural props get depth-only twins in the pre-pass (same vertex stage, no pixel stage: the
    // position-only depth of an opaque material, DepthRendering.cpp:652-669, CF_DepthNearOrEqual) and these lit pipelines test CF_Equal without depth writes
    // (were: Less + depth write in the scene pass).
    let scene_pipeline = |label: &str, vs: &str, fs: &str, layout: &wgpu::PipelineLayout, stride: u64, attrs: &[wgpu::VertexAttribute]| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label), layout: Some(layout),
            vertex: wgpu::VertexState { module: &scene_sm, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: stride, step_mode: wgpu::VertexStepMode::Vertex, attributes: attrs })] },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { depth_compare: Some(wgpu::CompareFunction::Equal), depth_write_enabled: Some(false), ..scene_depth.clone() }), multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &scene_sm, entry_point: Some(fs), compilation_options: Default::default(), targets: &crate::reflection::lit_targets(HDR_FORMAT) }), // REFLECTION APPLY: + the reflection inputs
            multiview_mask: None, cache: None,
        })
    };
    let depth_twin = |label: &str, vs: &str, layout: &wgpu::PipelineLayout, stride: u64, attrs: &[wgpu::VertexAttribute]| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label), layout: Some(layout),
            vertex: wgpu::VertexState { module: &scene_sm, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: stride, step_mode: wgpu::VertexStepMode::Vertex, attributes: attrs })] },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { depth_compare: Some(wgpu::CompareFunction::LessEqual), ..scene_depth.clone() }), multisample: Default::default(),
            fragment: None, multiview_mask: None, cache: None,
        })
    };
    progress("Compiling shaders: ground, character and prop pipelines");
    let ground_layout = pl("ground layout", &[bgl_globals, bgl_material, bgl_obj_static]);
    let skinned_layout = pl("skinned layout", &[bgl_globals, bgl_material, bgl_obj_skinned]);
    // PREPASS (UE4.16 r.EarlyZPass 2): the ground's depth-only pre-pass pipeline (same vertex stage, no fragment stage) and the lit pass testing CF_Equal without depth writes (BasePassRendering.cpp:308-361)
    let ground_depth_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ground depth"), layout: Some(&ground_layout),
        vertex: wgpu::VertexState { module: &scene_sm, entry_point: Some("vs_ground"), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<GroundVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &GROUND_ATTRS })] },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
        depth_stencil: Some(scene_depth.clone()), multisample: Default::default(), fragment: None, multiview_mask: None, cache: None,
    });
    let ground_pipeline = { device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ground"), layout: Some(&ground_layout),
        vertex: wgpu::VertexState { module: &scene_sm, entry_point: Some("vs_ground"), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<GroundVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &GROUND_ATTRS })] },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState { depth_compare: Some(wgpu::CompareFunction::Equal), depth_write_enabled: Some(false), ..scene_depth.clone() }), multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &scene_sm, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &crate::reflection::lit_targets(HDR_FORMAT) }),
        multiview_mask: None, cache: None,
    }) };
    let skinned_pipeline = scene_pipeline("skinned", "vs_skinned", "fs_main", &skinned_layout, std::mem::size_of::<SkinnedVertex>() as u64, &SKINNED_ATTRS);
    let skinned_depth_pipeline = depth_twin("skinned depth", "vs_skinned", &skinned_layout, std::mem::size_of::<SkinnedVertex>() as u64, &SKINNED_ATTRS); // FULL PREPASS
    let prop_layout = pl("prop layout", &[bgl_globals]);
    let prop_pipeline = scene_pipeline("props", "vs_prop", "fs_prop", &prop_layout, std::mem::size_of::<PropVertex>() as u64, &PROP_ATTRS);
    let prop_depth_pipeline = depth_twin("props depth", "vs_prop", &prop_layout, std::mem::size_of::<PropVertex>() as u64, &PROP_ATTRS); // FULL PREPASS
    progress("Compiling shaders: shadow pipelines");
    let shadow_layout = pl("shadow layout", &[bgl_shadow, bgl_obj_skinned]);
    let shadow_pipeline = [Some(wgpu::Face::Back), None].map(|cull_mode| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if cull_mode.is_some() { "shadow" } else { "shadow (two-sided)" }), layout: Some(&shadow_layout),
        vertex: wgpu::VertexState { module: &shadow_sm, entry_point: Some("vs_skinned"), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<SkinnedVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &SKINNED_ATTRS })] },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, ..Default::default() },
        // UE4.16: no hardware depth bias in the depth pass (ShaderDepthBias is added in shadow.wgsl); one-sided materials cull back faces, two-sided ones
        // (`Material->IsTwoSided() || CastsShadowAsTwoSided()`, ShadowDepthRendering.cpp:1059) draw both (SetViewFlagsForShadowPass: EDrawingPolicyOverrideFlags::TwoSided).
        depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: wgpu::DepthBiasState::default() }),
        multisample: Default::default(), fragment: None, multiview_mask: None, cache: None,
    }));
    let shadow_prop_layout = pl("shadow prop layout", &[bgl_shadow]);
    let shadow_prop_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("shadow props"), layout: Some(&shadow_prop_layout),
        vertex: wgpu::VertexState { module: &shadow_sm, entry_point: Some("vs_prop"), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<PropVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &PROP_ATTRS[..1] })] },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: wgpu::DepthBiasState::default() }),
        multisample: Default::default(), fragment: None, multiview_mask: None, cache: None,
    });
    let fullscreen = |label: &str, sm: &wgpu::ShaderModule, fs: &str, layout: &wgpu::PipelineLayout, format: wgpu::TextureFormat| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label), layout: Some(layout),
            vertex: wgpu::VertexState { module: sm, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(), depth_stencil: None, multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: sm, entry_point: Some(fs), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
            multiview_mask: None, cache: None,
        })
    };
    progress("Compiling shaders: post-processing pipelines");
    let post_layout = pl("post layout", &[bgl_post]);
    let post_pipeline = fullscreen("post", &post_sm, "fs_main", &post_layout, output_format);
    let post_ldr_pipeline = fullscreen("post (ldr, fxaa input)", &post_sm, "fs_main", &post_layout, LDR_FORMAT);
    let fxaa_layout = pl("fxaa layout", &[bgl_fxaa]);
    let fxaa_pipeline = fullscreen("fxaa", &fxaa_sm, "fs_main", &fxaa_layout, output_format);
    let down_layout = pl("downsample layout", &[bgl_downsample]);
    let downsample_pipeline = fullscreen("downsample", &down_sm, "fs_downsample", &down_layout, HDR_FORMAT);
    let meter_layout = pl("meter layout", &[bgl_meter]);
    let meter_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("meter"), layout: Some(&meter_layout), module: &meter_sm, entry_point: Some("meter"), compilation_options: Default::default(), cache: None });
    let meter_hist_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("meter histogram"), layout: Some(&meter_layout), module: &meter_sm, entry_point: Some("meter_histogram"), compilation_options: Default::default(), cache: None }); // PERF
    MainPipes { ground_pipeline, ground_depth_pipeline, skinned_pipeline, prop_pipeline, skinned_depth_pipeline, prop_depth_pipeline, shadow_pipeline, shadow_prop_pipeline, downsample_pipeline, meter_pipeline, meter_hist_pipeline, post_pipeline, post_ldr_pipeline, fxaa_pipeline }
}
fn entry_uniform(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }
}
fn entry_storage(binding: u32, vis: wgpu::ShaderStages, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None }, count: None }
}
fn entry_texture(binding: u32, vis: wgpu::ShaderStages, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None }
}
fn entry_sampler(binding: u32, vis: wgpu::ShaderStages, ty: wgpu::SamplerBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Sampler(ty), count: None }
}
fn scene_globals_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    let vf = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
    let f = wgpu::ShaderStages::FRAGMENT;
    let mut globals_entries = vec![
        entry_uniform(0, vf), entry_uniform(1, vf),
        wgpu::BindGroupLayoutEntry { binding: 2, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2Array, multisampled: false }, count: None },
        entry_sampler(3, f, wgpu::SamplerBindingType::Comparison),
        entry_sampler(4, f, wgpu::SamplerBindingType::Filtering),
        entry_sampler(15, f, wgpu::SamplerBindingType::NonFiltering), // PERF (2026-10-06): shadow_point_samp (lit_common.wgsl csm_gather)
    ];
    globals_entries.extend(crate::distance_field::globals_layout_entries()); // DISTANCE FIELD HOOK: bindings 5..10
    globals_entries.extend(crate::sky_reflection::globals_layout_entries()); // SKY REFLECTION HOOK: bindings 11..14
    globals_entries.extend(crate::light_function::globals_layout_entries()); // LIGHT FUNCTION: bindings 20..22
    globals_entries.extend(crate::local_lights::globals_layout_entries());
    globals_entries
}
pub fn pso_prewarm_fx(device: &wgpu::Device, queue: &wgpu::Queue) -> crate::fx::FxRenderer {
    let bgl_globals = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("globals bgl"), entries: &scene_globals_entries() });
    let mut fx = crate::fx::FxRenderer::new(device, queue, HDR_FORMAT, DEPTH_FORMAT);
    fx.set_scene_lighting(&bgl_globals, fn_live::wgsl!("../shaders/common.wgsl"), fn_live::wgsl!("../shaders/lit_common.wgsl"));
    fx
}
fn bg_buf<'a>(binding: u32, b: &'a wgpu::Buffer) -> wgpu::BindGroupEntry<'a> { wgpu::BindGroupEntry { binding, resource: b.as_entire_binding() } }
fn bg_tex<'a>(binding: u32, v: &'a wgpu::TextureView) -> wgpu::BindGroupEntry<'a> { wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(v) } }
fn bg_samp<'a>(binding: u32, s: &'a wgpu::Sampler) -> wgpu::BindGroupEntry<'a> { wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::Sampler(s) } }

const SKINNED_ATTRS: [wgpu::VertexAttribute; 5] = [
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 2 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint16x4, offset: 32, shader_location: 3 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 40, shader_location: 4 },
];
const PROP_ATTRS: [wgpu::VertexAttribute; 3] = [
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 24, shader_location: 2 },
];
const GROUND_ATTRS: [wgpu::VertexAttribute; 2] = [
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 12, shader_location: 1 },
];

/// Visual ground: the 512 m plate + the four ring rectangles out to `outer`, as a triangle list with the prototype's 2 m UV repeat.
pub fn ground_triangles(half: f32, outer: f32) -> Vec<([f32; 3], [f32; 2])> {
    let v = |x: f32, z: f32| ([x, 0., z], [(x + half) * 0.5, (z + half) * 0.5]);
    let mut quads = vec![[(-half, -half), (-half, half), (half, half), (half, -half)]];
    quads.extend(lighting::ground_ring(half, outer));
    let mut out = vec![];
    for q in quads {
        for k in [0, 1, 2, 0, 2, 3] { out.push(v(q[k].0, q[k].1)); }
    }
    out
}

/// PERF (FPS investigation 2026-10-06): debug-only `FN_SCENE_SKIP=ground,terrain,props,statics,gun,char,particles,fx` leaves those draws out of the scene pass (their
/// pre-pass depth stays), to measure each part's shading cost A/B under the same conditions. Never set in play.
/// TERRAIN MAIN PASS: `terrain-shadow` drops the landscape casters from the cascades, `terrain-depth` its pre-pass draw (A/B of their cost).
fn scene_skip(part: &str) -> bool {
    static SKIP: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    SKIP.get_or_init(|| fn_live::var("FN_SCENE_SKIP").map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()).iter().any(|s| s == part)
}
impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, output_format: wgpu::TextureFormat, assets: &RendererAssets) -> Result<Renderer, String> {
        Self::new_with_progress(device, queue, output_format, assets, &mut |_| {})
    }

    /// [Renderer::new] that announces every phase BEFORE running it (progress(label)), so a loading screen can show what is being built and time each phase.
    /// The phases are the shader-module, per-pipeline-group, target / buffer, texture and sky steps; each is one call into the driver and can take 100s of ms.
    pub fn new_with_progress(device: &wgpu::Device, queue: &wgpu::Queue, output_format: wgpu::TextureFormat, assets: &RendererAssets, progress: &mut dyn FnMut(&'static str)) -> Result<Renderer, String> {
        let (device, queue) = (device.clone(), queue.clone());
        progress("Creating bind group layouts");
        let vf = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let (v, f) = (wgpu::ShaderStages::VERTEX, wgpu::ShaderStages::FRAGMENT);
        // ---- bind group layouts ----
        let globals_entries = scene_globals_entries();
        let bgl_globals = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("globals bgl"), entries: &globals_entries });
        let bgl_material = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("material bgl"), entries: &[
            entry_uniform(0, f), entry_texture(1, f, true), entry_texture(2, f, true), entry_texture(3, f, true), entry_texture(4, f, true),
        ] });
        let bgl_obj_static = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("object (static) bgl"), entries: &[entry_uniform(0, v)] });
        let bgl_obj_skinned = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("object (skinned) bgl"), entries: &[entry_uniform(0, v), entry_storage(1, v, true)] });
        let bgl_shadow = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("shadow vp bgl"), entries: &[entry_uniform(0, v)] });
        let bgl_post = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("post bgl"), entries: &[
            entry_uniform(0, f), entry_texture(1, f, true),
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            entry_storage(3, f, true), entry_sampler(4, f, wgpu::SamplerBindingType::Filtering),
            entry_texture(5, f, true), // BLOOM HOOK: the bloom image (half res, PostprocessInput1 of the tonemapper)
            wgpu::BindGroupLayoutEntry { binding: 10, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false }, count: None }, // COLOR LUT
        ] });
        let bgl_downsample = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("downsample bgl"), entries: &[entry_texture(0, f, true), entry_sampler(1, f, wgpu::SamplerBindingType::Filtering)] });
        let c = wgpu::ShaderStages::COMPUTE;
        let bgl_meter = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("meter bgl"), entries: &[entry_uniform(0, c), entry_texture(1, c, false), entry_storage(2, c, false), entry_storage(3, c, false)] }); // PERF: 3 = the histogram (meter.wgsl)

        // ---- shaders ----
        progress("Compiling shader modules");
        let common = fn_live::wgsl!("../shaders/common.wgsl"); // TOD LIVE: the height fog is per-frame uniform data (LightUniform::fog), no longer baked here
        let lit_common = fn_live::wgsl!("../shaders/lit_common.wgsl");
        let bgl_fxaa = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fxaa bgl"), entries: &[entry_uniform(0, f), entry_texture(1, f, true), entry_sampler(2, f, wgpu::SamplerBindingType::Filtering)] });
        let MainPipes { ground_pipeline, ground_depth_pipeline, skinned_pipeline, prop_pipeline, skinned_depth_pipeline, prop_depth_pipeline, shadow_pipeline, shadow_prop_pipeline, downsample_pipeline, meter_pipeline, meter_hist_pipeline, post_pipeline, post_ldr_pipeline, fxaa_pipeline } = main_pipelines(&device, output_format, &bgl_globals, &bgl_material, &bgl_obj_static, &bgl_obj_skinned, &bgl_shadow, &bgl_post, &bgl_fxaa, &bgl_downsample, &bgl_meter, progress);
        let meter_hist_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("exposure histogram"), size: 64 * 4, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }); // PERF

        // ---- shared buffers / textures ----
        progress("Creating render buffers and shadow maps");
        let ub = |label: &str, size: usize| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: size as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let camera_buf = ub("camera", std::mem::size_of::<CameraUniform>());
        let light_buf = ub("light", std::mem::size_of::<LightUniform>());
        let post_buf = ub("post params", std::mem::size_of::<PostParams>());
        let fxaa_buf = ub("fxaa params", 32);
        let meter_buf = ub("meter params", std::mem::size_of::<MeterParams>());
        let exposure_init = [tonemap::AUTO_EXPOSURE_MIN, tonemap::AUTO_EXPOSURE_MIN, 1f32, 0f32]; // smoothed luminance, target luminance, exposure, average (histogram metering)
        let exposure_buf = texture::buffer_init(&device, &exposure_init, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, "exposure state");
        let (shadow_array_view, shadow_layer_views) = create_shadow_views(&device, SHADOW_MAP_SIZE);
        let shadow_vp_bufs: Vec<_> = (0..CASCADE_COUNT).map(|_| ub("shadow vp", 80)).collect(); // mat4 + depth bias vec4 (shadow.wgsl LightViewProj)
        let shadow_vp_bgs: Vec<_> = shadow_vp_bufs.iter().map(|b| device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("shadow vp bg"), layout: &bgl_shadow, entries: &[bg_buf(0, b)] })).collect();
        let shadow_cmp = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("shadow compare"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, compare: Some(wgpu::CompareFunction::LessEqual), ..Default::default() });
        let shadow_point = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("shadow point (gather)"), mag_filter: wgpu::FilterMode::Nearest, min_filter: wgpu::FilterMode::Nearest, mipmap_filter: wgpu::MipmapFilterMode::Nearest, address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, address_mode_w: wgpu::AddressMode::ClampToEdge, ..Default::default() }); // PERF: csm_gather
        let tex_sampler = texture::repeat_sampler_ex(&device, 8, 0, "scene sampler");
        let clamp_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("clamp linear"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, mipmap_filter: wgpu::MipmapFilterMode::Linear, ..Default::default() });
        let mut df = crate::distance_field::DistanceFields::new(&device); // DISTANCE FIELD HOOK
        let mut ge = vec![bg_buf(0, &camera_buf), bg_buf(1, &light_buf), bg_tex(2, &shadow_array_view), bg_samp(3, &shadow_cmp), bg_samp(4, &tex_sampler), bg_samp(15, &shadow_point)];
        ge.extend(df.bind_entries());
        let sky_refl = crate::sky_reflection::SkyReflection::new(&device, &queue); // SKY REFLECTION HOOK
        ge.extend(sky_refl.bind_entries());
        let light_function = crate::light_function::LightFunctionGpu::new(&device, &queue); // LIGHT FUNCTION
        ge.extend(light_function.bind_entries());
        let local_lights = crate::local_lights::LocalLightsGpu::new(&device, &queue);
        let sss = crate::subsurface::Subsurface::new(&device, &queue, HDR_FORMAT);
        ge.extend(local_lights.bind_entries());
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("globals bg"), layout: &bgl_globals, entries: &ge });
        drop(ge);
        df.take_bind_dirty();
        let neutral = [
            texture::solid_rgba8(&device, &queue, [128, 128, 255, 255], "neutral normal"),
            texture::solid_rgba8(&device, &queue, [255, 0, 128, 255], "neutral spec"),
            texture::solid_rgba8(&device, &queue, [255, 0, 0, 255], "neutral mask"),
        ];
        let white = texture::solid_rgba8(&device, &queue, [255, 255, 255, 255], "white");

        // ---- ground ----
        progress("Uploading the ground texture");
        let verts: Vec<GroundVertex> = ground_triangles(GROUND_HALF, GROUND_OUTER).into_iter().map(|(pos, uv)| GroundVertex { pos, uv }).collect();
        let ground_vb = texture::buffer_init(&device, &verts, wgpu::BufferUsages::VERTEX, "ground vb");
        let ground_tex = match &assets.ground_mips { Some(l) => texture::upload_mip_chain(&device, &queue, &assets.ground_texture, l, true, "T_Grasslands_D"), None => texture::upload_rgba8(&device, &queue, &assets.ground_texture, true /* SRGB BEFORE FILTER: cooked SRGB=true (scene.wgsl shade_ground) */, MipFilter::Triangle, "T_Grasslands_D") };
        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        let kind_buf = |k: MaterialKind| texture::buffer_init(&device, &[MaterialUniform { kind: [k as u32, 0, 0, 0] }], wgpu::BufferUsages::UNIFORM, "material kind");
        let ground_kind = kind_buf(MaterialKind::Ground);
        let ground_material_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("ground material"), layout: &bgl_material, entries: &[
            bg_buf(0, &ground_kind), bg_tex(1, &view(&ground_tex)), bg_tex(2, &view(&neutral[0])), bg_tex(3, &view(&neutral[1])), bg_tex(4, &view(&neutral[2])),
        ] });
        let ground_obj = texture::buffer_init(&device, &[ObjectUniform { model: Mat4::IDENTITY.to_cols_array_2d() }], wgpu::BufferUsages::UNIFORM, "ground object");
        let ground_obj_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("ground object bg"), layout: &bgl_obj_static, entries: &[bg_buf(0, &ground_obj)] });
        let prop_vb = device.create_buffer(&wgpu::BufferDescriptor { label: Some("props vb"), size: (PROP_INITIAL_CAPACITY * std::mem::size_of::<PropVertex>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });

        progress("Compiling shaders: sky");
        let sky = SkyRenderer::new(&device, &queue, &assets.sky, HDR_FORMAT);
        progress("Starting the GPU profiler");
        progress("Compiling shaders: textured static meshes");
        let statics = StaticRenderer::new(&device, &queue, common, lit_common, &bgl_globals, &bgl_shadow, HDR_FORMAT, DEPTH_FORMAT, texture::repeat_sampler_ex(&device, 8, 0, "static sampler"));
        let profiler = GpuProfiler::new(&device, &queue);
        let mut fx = crate::fx::FxRenderer::new(&device, &queue, HDR_FORMAT, DEPTH_FORMAT); // RENDER FX HOOK
        fx.set_shadow_vp(&shadow_vp_bufs); // FX SHADOW: the cascades' LightViewProj for the FX shadow-depth draws (fx.rs draw_shadow)
        fx.set_scene_lighting(&bgl_globals, common, lit_common); // FX RECEIVE: opaque / masked lit FX materials lit like the statics (fx.rs, fx_resolve.wgsl)
        // PERF (2026-10-06, frame-time spikes): the FX frame writes go through one staging buffer copied at the start of the frame encoder (fx.rs
        // `set_staged_uploads`); `FN_DEBUG_FX_DIRECT_UPLOADS=1` (debug-only A/B switch) keeps the old queue.write_buffer per write
        fx.set_staged_uploads(fn_live::var("FN_DEBUG_FX_DIRECT_UPLOADS").map_or(true, |v| v != "1"));
        let particles =crate::particles::ParticleRenderer::new(&device, &queue, common, lit_common, &bgl_globals, HDR_FORMAT, DEPTH_FORMAT); // PARTICLE HOOK
        let mut gun = crate::gun_material::GunRenderer::new(&device, &queue, common, lit_common, &bgl_globals, HDR_FORMAT, DEPTH_FORMAT); // GUN MATERIAL HOOK
        gun.set_shadow_vp(&shadow_vp_bufs); // GUN SHADOW: the cascades' LightViewProj for the gun depth pass
        let scope = crate::scope_pass::ScopeRenderer::new(&device, &queue); // SCOPE HOOK
        let outline = crate::outline_pass::OutlineRenderer::new(&device, &queue); // OUTLINE
        let decals = crate::decal_pass::DecalRenderer::new(&device, &queue);
        let char_gpu = crate::char_material::CharGpu::new(&device, common, lit_common, &bgl_globals, &bgl_material, &bgl_obj_skinned, HDR_FORMAT, DEPTH_FORMAT); // CHAR MATERIAL HOOK
        let volfog = crate::volfog::VolFog::new(&device, &camera_buf, &light_buf, HDR_FORMAT); // VOLUMETRIC FOG HOOK
        let tlv = crate::translucent_lighting::TranslucentLighting::new(&device); // MIRRORFIX (translucency volume)
        let bloom = fn_postfx::bloom::BloomChain::new(&device, HDR_FORMAT); // BLOOM HOOK
        let light_shafts = crate::light_shafts::LightShafts::new(&device, HDR_FORMAT); // LIGHT SHAFT HOOK
        let ssao = crate::ssao_pass::SsaoPass::new(&device, common, HDR_FORMAT); // AO ORDER
        let color_lut = crate::color_lut::ColorLut::new(&device, common, &post_buf); // COLOR LUT
        let ssr = crate::reflection::Ssr::new(&device, &queue); // REFLECTION APPLY
        let mut ssao_ue = crate::ssao_ue::UeSsao::new(&device, &queue); // UE SSAO
        ssao_ue.prewarm();
        let mut taa = crate::taa::TemporalAa::new(&device, &bgl_obj_skinned, HDR_FORMAT, DEPTH_FORMAT, &SKINNED_ATTRS, std::mem::size_of::<SkinnedVertex>() as u64); // SCALABILITY
        taa.prewarm(output_format, LDR_FORMAT);
        let sun_mask = crate::light_shafts::SunMask::new(&device); // LIGHT SHAFT HOOK
        Ok(Renderer {
            batch: std::sync::Mutex::new(None), batch_after: std::sync::atomic::AtomicBool::new(false),
            device, queue, output_format, bgl_material, bgl_obj_skinned, bgl_post, bgl_downsample, bgl_meter, bgl_shadow,
            ground_pipeline, ground_depth_pipeline, skinned_pipeline, prop_pipeline, skinned_depth_pipeline, prop_depth_pipeline, shadow_pipeline, shadow_prop_pipeline, downsample_pipeline, meter_pipeline, meter_hist_pipeline, meter_hist_buf, post_pipeline, post_ldr_pipeline,
            fxaa_pipeline, bgl_fxaa, fxaa_buf, bgl_globals, shadow_cmp, shadow_point, tex_sampler, shadow_size: SHADOW_MAP_SIZE, sampler_key: (8, 0), prop_vb,
            prop_capacity: PROP_INITIAL_CAPACITY, prop_count: 0, prop_chunks: Vec::new(), prop_draws: Default::default(), shadow_stats: Default::default(), sky,
            camera_buf, light_buf, globals_bg, shadow_array_view, shadow_layer_views, shadow_vp_bufs, shadow_vp_bgs,
            clamp_sampler, neutral, white, post_buf, meter_buf, exposure_buf, color_lut,
            ground_vb, ground_count: verts.len() as u32, ground_material_bg, ground_obj_bg,
            models: vec![], targets: None, frame_index: 0,
            profiler, prop_templates: PropTemplates::default(), prop_scratch: Vec::new(), statics,
            fx, fx_gen: 0, // RENDER FX HOOK
            particles, // PARTICLE HOOK
            gun, sky_refl, // GUN MATERIAL HOOK, SKY REFLECTION HOOK
            light_function, // LIGHT FUNCTION
            local_lights,
            sss, sss_on: false,
            scope, // SCOPE HOOK
            outline, // OUTLINE
            decals,
            char_gpu, // CHAR MATERIAL HOOK
            volfog, // VOLUMETRIC FOG HOOK
            tlv, // MIRRORFIX (translucency volume)
            backdrop: None, // BACKDROP HOOK
            df, // DISTANCE FIELD HOOK
            bloom, bloom_bound: None, // BLOOM HOOK
            light_shafts, ssao, ssao_ue, ssr, sun_mask, occlusion: crate::occlusion::Occlusion::new(), occl_cam: None, /* OCCLUSION */ // LIGHT SHAFT HOOK, AO ORDER, UE SSAO, REFLECTION APPLY
            taa, // SCALABILITY
            bgl_obj_static, shader_gen: fn_live::shader_generation(),
        })
    }

    /// BACKDROP HOOK: installs the real landscape / ocean / skybox geometry (see `backdrop.rs`); the visual ground ring around the plate is dropped (the real terrain replaces it).
    /// TERRAIN MAIN PASS (2026-10-07): with a landscape the flat plate is not drawn at all (the main pass draws the real landscape near the camera, backdrop.rs) and
    /// the global distance field gets its heightfield instead of the plate's flat one; without terrain.bin the plate quad stays (fallback).
    pub fn set_backdrop(&mut self, data: &crate::backdrop::BackdropData) {
        self.backdrop = Some(crate::backdrop::BackdropRenderer::new(&self.device, &self.queue, &self.bgl_globals, &self.bgl_shadow, HDR_FORMAT, DEPTH_FORMAT, data));
        let terrain = self.backdrop.as_ref().is_some_and(|b| b.has_main_terrain());
        self.ground_count = if terrain { 0 } else { 6 }; // the plate quad only (no terrain) / nothing
        self.df.ao.set_heightfield(&self.device, &self.queue, if terrain { data.height_grid.as_ref() } else { None });
        crate::fog::set_world_z_ref_cm(data.ground_ref_cm); // FOG HEIGHT RE-BASE (2026-10-07): world y = 0 is at this absolute UE Z (fog::fog_height_cm)
    }
    pub fn set_backdrop_prepared(&mut self, data: &crate::backdrop::BackdropData, prep: crate::backdrop::BackdropPrep) {
        self.backdrop = Some(crate::backdrop::BackdropRenderer::new_prepared(&self.device, &self.queue, &self.bgl_globals, &self.bgl_shadow, HDR_FORMAT, DEPTH_FORMAT, data, prep, true));
        let terrain = self.backdrop.as_ref().is_some_and(|b| b.has_main_terrain());
        self.ground_count = if terrain { 0 } else { 6 };
        self.df.ao.set_heightfield(&self.device, &self.queue, if terrain { data.height_grid.as_ref() } else { None });
        crate::fog::set_world_z_ref_cm(data.ground_ref_cm);
    }
    pub fn has_backdrop(&self) -> bool { self.backdrop.is_some() }
    /// LANDSCAPE MATERIAL (2026-10-07): the landscape component bounds of the backdrop (game m, component order), empty without one.
    pub fn landscape_terrain_bounds(&self) -> Vec<(glam::Vec3, f32)> { self.backdrop.as_ref().map(|b| b.terrain_bounds()).unwrap_or_default() }
    /// LANDSCAPE MATERIAL: the main-pass landscape components are drawn by the FX path (the backdrop pass skips their depth / lit draws).
    pub fn set_landscape_main_suppressed(&mut self, on: bool) { if let Some(b) = self.backdrop.as_mut() { b.suppress_main = on; } }
    /// OCCLUSION (2026-10-08): pick up the HZB readbacks mapped by now, before the app culls the statics (occlusion.rs `pickup`; `FN_OCCLUSION_PICKUP=0`: off).
    pub fn occlusion_pickup(&mut self) { if crate::occlusion::early_pickup() { self.occlusion.pickup(&self.device); } }

    pub fn output_format(&self) -> wgpu::TextureFormat { self.output_format }
    /// Forgets the adapted exposure so the next frame snaps to the metered value.
    pub fn reset_exposure(&mut self) { self.frame_index = 0; self.taa.reset(); self.ssr.reset(); self.light_shafts.reset(); }
    pub fn model_count(&self) -> usize { self.models.len() }

    /// Uploads a skinned model (several primitives sharing one bone palette of `bone_count` matrices).
    pub fn create_model(&mut self, prims: &[SkinnedPrimitive], bone_count: usize) -> ModelId {
        let id = self.models.len();
        let m = self.model_builder().build_model(prims, bone_count, &format!("model {id}"));
        self.models.push(m);
        ModelId(id)
    }
    /// COSMETIC SWAP: a cheap, `Send` handle (clones of the device / queue / bind group layouts / neutral textures) that uploads a skinned model on ANY thread
    /// (decoded textures with their CPU mip chains are the slow part of a model upload); install the result with [`Renderer::replace_model`] on the render thread.
    pub fn model_builder(&self) -> ModelBuilder {
        ModelBuilder { device: self.device.clone(), queue: self.queue.clone(), bgl_material: self.bgl_material.clone(), bgl_obj_skinned: self.bgl_obj_skinned.clone(),
            white: self.white.clone(), neutral: self.neutral.clone(), char_bgl: self.char_gpu.char_bgl.clone(), overlay_bgl: self.char_gpu.overlay_bgl.clone() }
    }
    /// COSMETIC SWAP: the slot `id` now draws `built` (uploaded by a [`ModelBuilder`]); the previous model's GPU resources are dropped. Its object matrix / bones start at
    /// identity, so the caller updates the model (`update_model`) before the next draw, as it does every frame.
    pub fn replace_model(&mut self, id: ModelId, built: BuiltModel) { self.models[id.0] = built.0; }
    pub fn instance_model(&mut self, src: ModelId) -> ModelId {
        let device = &self.device;
        let s = &self.models[src.0];
        let identity: Vec<[[f32; 4]; 4]> = vec![Mat4::IDENTITY.to_cols_array_2d(); s.bone_count];
        let obj = device.create_buffer(&wgpu::BufferDescriptor { label: Some("instance object"), size: 64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let bones = texture::buffer_init(device, &identity, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, "instance bones");
        let prev_obj = device.create_buffer(&wgpu::BufferDescriptor { label: Some("instance object (prev)"), size: 64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let prev_bones = texture::buffer_init(device, &identity, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, "instance bones (prev)");
        let prev_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("instance bg (prev)"), layout: &self.bgl_obj_skinned, entries: &[bg_buf(0, &prev_obj), bg_buf(1, &prev_bones)] });
        self.queue.write_buffer(&obj, 0, bytemuck::bytes_of(&ObjectUniform { model: Mat4::IDENTITY.to_cols_array_2d() }));
        let obj_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("instance bg"), layout: &self.bgl_obj_skinned, entries: &[bg_buf(0, &obj), bg_buf(1, &bones)] });
        let mut prims = s.prims.clone();
        for p in prims.iter_mut() {
            if let (Some(src), Some(cbg)) = (p.char_src.clone(), p.char_bg.as_ref()) {
                let buf = crate::char_material::char_instance_buffer(device, &src, "instance char cb");
                let bg = crate::char_material::char_bind_group_from(&self.char_gpu.char_bgl, device, &src, &buf, "instance char bg");
                p.char_bg = Some((bg, cbg.1.clone()));
                p.char_cb = Some(buf);
            }
        }
        let m = Model { prims, obj, bones, bone_count: s.bone_count, obj_bg, bounds: s.bounds, world: std::sync::Mutex::new(Mat4::IDENTITY), prev_obj, prev_bones, prev_bg, prev_valid: std::sync::atomic::AtomicBool::new(false) };
        self.models.push(m);
        ModelId(self.models.len() - 1)
    }
    /// Sets the model-to-world matrix and the skin matrices (`globals[joint] * inverse_bind`, character space) of a model.
    pub fn update_model(&self, id: ModelId, model: Mat4, skin: &[Mat4]) {
        let m = &self.models[id.0];
        self.queue.write_buffer(&m.obj, 0, bytemuck::bytes_of(&ObjectUniform { model: model.to_cols_array_2d() }));
        if let Ok(mut w) = m.world.lock() { *w = model; }
        let n = skin.len().min(m.bone_count);
        let data: Vec<[[f32; 4]; 4]> = skin[..n].iter().map(|x| x.to_cols_array_2d()).collect();
        self.queue.write_buffer(&m.bones, 0, bytemuck::cast_slice(&data));
    }
    /// BUILD ANIM HOOK: the per-model material scalar of every primitive of model `id` (`MaterialKind::Architect`: `EmissiveMultiply`, what
    /// `AFortWeap_BuildingTool::OnBlueprintPaperPulseUpdate` 0x14092f190 sets on the paper's MID).
    pub fn set_model_char_cb(&self, id: ModelId, f: &dyn Fn(crate::char_material::CharPerm, &[[f32; 4]]) -> Option<Vec<[f32; 4]>>) -> usize {
        let mut n = 0;
        for p in &self.models[id.0].prims {
            let (Some(src), Some(buf)) = (&p.char_src, &p.char_cb) else { continue };
            let cb = f(src.perm, &src.base).unwrap_or_else(|| src.base.clone());
            crate::char_material::char_write(&self.queue, buf, src.perm, &cb);
            n += 1;
        }
        n
    }
    pub fn set_model_scalar(&self, id: ModelId, v: f32) {
        for p in &self.models[id.0].prims { self.queue.write_buffer(&p.kind_buf, 0, bytemuck::bytes_of(&MaterialUniform { kind: [p.kind as u32, v.to_bits(), 0, 0] })); }
    }

    /// Replaces the dynamic prop geometry (world-space triangle list: trees, rocks; rebuild each frame for shake sway). Props receive and cast shadows.
    pub fn set_props(&mut self, verts: &[PropVertex]) {
        if verts.len() > self.prop_capacity {
            self.prop_capacity = verts.len().next_power_of_two();
            self.prop_vb = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("props vb"), size: (self.prop_capacity * std::mem::size_of::<PropVertex>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        }
        if !verts.is_empty() { self.queue.write_buffer(&self.prop_vb, 0, bytemuck::cast_slice(verts)); }
        self.prop_count = verts.len() as u32;
        self.prop_chunks.clear();
        for (k, c) in verts.chunks(PROP_CHUNK_VERTS).enumerate() {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for v in c { let p = Vec3::from(v.pos); lo = lo.min(p); hi = hi.max(p); }
            let center = (lo + hi) * 0.5;
            let radius = if lo.is_finite() && hi.is_finite() { (hi - lo).length() * 0.5 + 0.01 } else { f32::MAX };
            self.prop_chunks.push(((k * PROP_CHUNK_VERTS) as u32, c.len() as u32, center, radius));
        }
    }

    /// Replaces the prop geometry from a list of placed props (position, yaw, scale, shake) of trees and rocks (`fn_combat::prop_mesh`
    /// shapes). Equivalent to `set_props(PropTemplates::build(..))` without per-call allocations; call it each frame while something shakes.
    pub fn set_prop_instances(&mut self, instances: &[PropInstance]) {
        let mut scratch = std::mem::take(&mut self.prop_scratch);
        scratch.clear();
        let mut order: Vec<&PropInstance> = instances.iter().collect();
        order.sort_by_key(|i| ((i.pos.z / 32.).floor() as i32, (i.pos.x / 32.).floor() as i32));
        for i in order { self.prop_templates.append(i, &mut scratch); }
        self.set_props(&scratch);
        self.prop_scratch = scratch;
    }
    /// Uploads a textured static-mesh material (see static_mesh::StaticMaterialData).
    pub fn create_static_material(&mut self, data: &StaticMaterialData) -> StaticMaterialHandle { self.statics.create_material(data) }
    /// Uploads a static mesh (all LODs). materials[section.material] is the material of each section.
    pub fn create_static_mesh(&mut self, data: &StaticMeshData, materials: &[StaticMaterialHandle]) -> StaticMeshHandle { self.statics.create_mesh(data, materials) }
    /// Replaces the placed static meshes (real trees / rocks). Cheap to call every frame (a copy of the list); culling, LOD choice, fading and the
    /// batched instanced draws happen per frame inside `render`. LOD hysteresis state is kept by list index, so keep the order stable.
    pub fn release_static_meshes(&mut self, meshes: &[StaticMeshHandle]) -> (usize, u64) { self.statics.release_meshes(meshes) }
    pub fn set_static_instances(&mut self, instances: &[StaticMeshInstance]) { self.statics.set_instances(instances); self.df.set_instances(instances); }
    /// DISTANCE FIELD HOOK: the cooked distance field of a static mesh (fn-app reads it from the paks); instances of the mesh then cast ray-traced
    /// distance-field shadows beyond the CSM and occlude the sky light (DFAO).
    pub fn set_static_mesh_distance_field(&mut self, mesh: StaticMeshHandle, df: crate::distance_field::MeshDistanceField) { self.df.set_mesh(mesh.0, df); }
    /// DISTANCE FIELD HOOK: numbers of the last frame (meshes, objects, culled, light tiles, atlas size).
    pub fn distance_field_stats(&self) -> crate::distance_field::DfStats { let mut s = self.df.stats.clone(); s.atlas = self.df.atlas_size(); s }
    /// Culling / LOD / draw-call numbers of the last rendered frame.
    pub fn static_stats(&self) -> &StaticStats { self.statics.stats() }
    pub fn static_mesh_count(&self) -> usize { self.statics.mesh_count() }
    pub fn static_material_count(&self) -> usize { self.statics.material_count() }
    /// Number of placed static instances (set_static_instances).
    pub fn static_instance_count(&self) -> usize { self.statics.instance_count() }
    // RENDER FX HOOK: the FX pass (see fx.rs). `fx()` creates textures / buffers / meshes / pipelines / bind groups; `set_fx_frame` hands over the draws of the next frame.
    pub fn fx(&mut self) -> &mut crate::fx::FxRenderer { &mut self.fx }
    pub fn set_fx_frame(&mut self, frame: crate::fx::FxFrame) { self.fx.set_frame(frame); }
    pub fn append_fx_frame(&mut self, frame: crate::fx::FxFrame) { self.fx.append_frame(frame); }
    // PARTICLE HOOK: textures / materials / meshes of the particle pass and the batches of the next frame (see particles.rs)
    pub fn create_particle_texture(&mut self, width: u32, height: u32, rgba8: &[u8], srgb: bool, label: &str) -> crate::particles::ParticleTexId { self.particles.create_texture(width, height, rgba8, srgb, label) }
    pub fn create_particle_material(&mut self, d: &crate::particles::ParticleMaterialDesc) -> crate::particles::ParticleMatId { self.particles.create_material(d) }
    pub fn create_particle_mesh(&mut self, d: &crate::particles::ParticleMeshDesc) -> crate::particles::ParticleMeshId { self.particles.create_mesh(d) }
    pub fn set_particle_frame(&mut self, frame: crate::particles::ParticleFrame) { self.particles.set_frame(frame); }
    pub fn particle_stats(&self) -> &crate::particles::ParticleStats { self.particles.stats() }
    // GUN MATERIAL HOOK: textures / buffers / meshes / pipelines of the gun pass (`gun()`) and the draws of the next frame (`set_gun_frame`), see gun_material.rs
    pub fn gun(&mut self) -> &mut crate::gun_material::GunRenderer { &mut self.gun }
    pub fn set_gun_frame(&mut self, frame: crate::gun_material::GunFrame) { self.gun.set_frame(frame); }
    /// SKY REFLECTION HOOK: the processed sky cube (`sky_reflection::filter_reflection_environment` of the specified cubemap), once it is built.
    /// LIGHT FUNCTION: installs the decoded T_spiralLF / T_FORT_LF (the High light-function program of the sun, light_function.rs).
    pub fn set_subsurface_profiles(&mut self, profiles: &[crate::subsurface::SssProfile]) { self.sss.set_profiles(&self.queue, profiles); }
    pub fn set_local_lights(&mut self, lights: Vec<crate::local_lights::LocalLight>) { self.local_lights.list = lights; }
    pub fn local_lights(&self) -> &[crate::local_lights::LocalLight] { &self.local_lights.list }
    pub fn local_light_stats(&self) -> (usize, usize, [u32; 3], bool) { (self.local_lights.list.len(), self.local_lights.last_visible, self.local_lights.last_dims, self.local_lights.last_overflow) }
    pub fn set_ies_profiles(&mut self, rows: &[Vec<f32>]) { self.local_lights.set_ies_profiles(&self.device, &self.queue, rows); self.rebuild_globals(); }
    pub fn set_light_function_textures(&mut self, spiral: crate::light_function::GreyImage, fort: crate::light_function::GreyImage) { self.light_function.set_textures(&self.device, &self.queue, spiral, fort); self.rebuild_globals(); }
    pub fn set_sky_reflection(&mut self, mips: &[crate::sky_light::Cube]) { self.sky_refl.set_cube(&self.device, &self.queue, mips); self.rebuild_globals(); }
    // SCOPE HOOK: textures / buffers / pipelines of the scope pass (`scope()`) and the draws of the next frame (`set_scope_frame`), see scope_pass.rs
    pub fn scope(&mut self) -> &mut crate::scope_pass::ScopeRenderer { &mut self.scope }
    pub fn set_scope_frame(&mut self, frame: crate::scope_pass::ScopeFrame) { self.scope.set_frame(frame); }
    // OUTLINE: the highlight pass (`outline()` to install the program, `set_outline_frame` + `set_custom_depth_instances` every frame), see outline_pass.rs
    pub fn outline(&mut self) -> &mut crate::outline_pass::OutlineRenderer { &mut self.outline }
    pub fn decals(&mut self) -> &mut crate::decal_pass::DecalRenderer { &mut self.decals }
    pub fn set_outline_frame(&mut self, frame: crate::outline_pass::OutlineFrame) { self.outline.set_frame(frame); }
    /// OUTLINE: the static mesh instances drawn into the custom depth / stencil targets next frame (empty = none).
    pub fn set_custom_depth_instances(&mut self, instances: &[StaticMeshInstance]) { self.statics.set_custom_instances(instances); }

    /// Number of prop vertices currently uploaded.
    pub fn prop_vertex_count(&self) -> u32 { self.prop_count }

    fn rebuild_globals(&mut self) {
        let shadow_view = &self.shadow_array_view;
        let mut ge = vec![bg_buf(0, &self.camera_buf), bg_buf(1, &self.light_buf), bg_tex(2, shadow_view), bg_samp(3, &self.shadow_cmp), bg_samp(4, &self.tex_sampler), bg_samp(15, &self.shadow_point)];
        ge.extend(self.df.bind_entries()); // DISTANCE FIELD HOOK
        ge.extend(self.sky_refl.bind_entries()); // SKY REFLECTION HOOK
        ge.extend(self.light_function.bind_entries()); // LIGHT FUNCTION
        ge.extend(self.local_lights.bind_entries());
        self.globals_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("globals bg"), layout: &self.bgl_globals, entries: &ge });
    }
    /// Applies the quality options that need new GPU objects (shadow map size, texture sampler).
    fn apply_quality(&mut self, opt: &RenderOptions) {
        let size = opt.shadow_map_size.clamp(256, 4096);
        let aniso = opt.anisotropy.clamp(1, 16);
        let mut dirty = false;
        if size != self.shadow_size {
            let (array, layers) = create_shadow_views(&self.device, size);
            self.shadow_array_view = array;
            self.shadow_layer_views = layers;
            self.shadow_size = size;
            dirty = true;
        }
        if (aniso, opt.skip_mips) != self.sampler_key {
            self.tex_sampler = texture::repeat_sampler_ex(&self.device, aniso, opt.skip_mips, "scene sampler");
            self.sampler_key = (aniso, opt.skip_mips);
            self.statics.set_sampler(texture::repeat_sampler_ex(&self.device, aniso, opt.skip_mips, "static sampler"));
            dirty = true;
        }
        if dirty { self.rebuild_globals(); }
    }

    /// Reads the linear HDR scene target (mip 0) of the last rendered frame back to the CPU (synchronous; reference comparison / debugging).
    pub fn read_hdr(&self) -> Option<HdrImage> {
        if cfg!(target_arch = "wasm32") { return None; }
        let t = self.targets.as_ref()?;
        let (w, h) = (t.width, t.height);
        let bpr = (w * 8).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("hdr readback"), size: (bpr * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &t.hdr, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.device.poll(crate::gpu::bounded_wait()).ok()?; // DEVICE LOST: bounded
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let row = &data[(y * bpr) as usize..(y * bpr + w * 8) as usize];
            for px in row.chunks_exact(8) {
                let c = |i: usize| half_to_f32(u16::from_le_bytes([px[i * 2], px[i * 2 + 1]]));
                pixels.push([c(0), c(1), c(2), c(3)]);
            }
        }
        drop(data);
        buffer.unmap();
        Some(HdrImage { width: w, height: h, pixels })
    }

    fn ensure_targets(&mut self, w: u32, h: u32) {
        if matches!(&self.targets, Some(t) if t.width == w && t.height == h) { return; }
        let device = &self.device;
        let mips = tonemap::mip_count(w, h);
        let hdr = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr scene"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: mips, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format: HDR_FORMAT, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
        });
        let hdr_mip_views: Vec<_> = (0..mips).map(|i| hdr.create_view(&wgpu::TextureViewDescriptor { label: Some("hdr mip"), base_mip_level: i, mip_level_count: Some(1), ..Default::default() })).collect();
        let hdr_full = hdr.create_view(&Default::default());
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene depth"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format: DEPTH_FORMAT, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
        });
        let depth_view = depth.create_view(&Default::default());
        // BLOOM HOOK: (re)build the bloom chain on the new scene target; the tonemapper reads its output, the histogram its SceneColorHalfRes
        self.bloom.prepare(&self.queue, &hdr_mip_views[0], w, h, self.fx_gen + 1, &bloom_params(), 1.0);
        self.bloom_bound = self.bloom.output_id();
        let (bloom_view, half_res) = (self.bloom.output().unwrap(), self.bloom.half_res().unwrap());
        let post_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("post bg"), layout: &self.bgl_post, entries: &[
            bg_buf(0, &self.post_buf), bg_tex(1, &hdr_full), bg_tex(2, &depth_view), bg_buf(3, &self.exposure_buf), bg_samp(4, &self.clamp_sampler), bg_tex(5, bloom_view), bg_tex(10, self.color_lut.view()),
        ] });
        let downsample_bgs: Vec<_> = (1..mips as usize).map(|i| device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("downsample bg"), layout: &self.bgl_downsample, entries: &[bg_tex(0, &hdr_mip_views[i - 1]), bg_samp(1, &self.clamp_sampler)] })).collect();
        let meter_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("meter bg"), layout: &self.bgl_meter, entries: &[bg_buf(0, &self.meter_buf), bg_tex(1, half_res), bg_buf(2, &self.exposure_buf), bg_buf(3, &self.meter_hist_buf)] }); // histogram input = SceneColorHalfRes (4-tap FRCPassPostProcessDownsample, BLOOM HOOK)
        self.fx_gen += 1; // RENDER FX HOOK: the FX bind groups that sample the scene depth are rebuilt for the new depth texture
        let refl = crate::reflection::ReflTargets::new(device, w, h); // REFLECTION APPLY
        self.targets = Some(Targets { width: w, height: h, hdr, hdr_mip_views, depth_view, post_bg, downsample_bgs, meter_bg, ldr: None, ldr2: None, up_src: None, refl });
    }

    /// Renders one frame into `target` (its size must equal the camera's width x height, and its format `output_format`).
    pub fn render(&mut self, frame: &FrameParams, target: &wgpu::TextureView) -> FrameStats { self.render_with_overlay(frame, target, None) }

    pub fn reload_shaders(&mut self) {
        self.shader_gen = fn_live::shader_generation();
        let t = std::time::Instant::now();
        let mut failed = vec![];
        let main = checked_build(&self.device, || main_pipelines(&self.device, self.output_format, &self.bgl_globals, &self.bgl_material, &self.bgl_obj_static, &self.bgl_obj_skinned, &self.bgl_shadow, &self.bgl_post, &self.bgl_fxaa, &self.bgl_downsample, &self.bgl_meter, &mut |_| {}));
        match main {
            Ok(p) => {
                (self.ground_pipeline, self.ground_depth_pipeline, self.skinned_pipeline, self.prop_pipeline, self.skinned_depth_pipeline, self.prop_depth_pipeline, self.shadow_pipeline) =
                    (p.ground_pipeline, p.ground_depth_pipeline, p.skinned_pipeline, p.prop_pipeline, p.skinned_depth_pipeline, p.prop_depth_pipeline, p.shadow_pipeline);
                (self.shadow_prop_pipeline, self.downsample_pipeline, self.meter_pipeline, self.meter_hist_pipeline, self.post_pipeline, self.post_ldr_pipeline, self.fxaa_pipeline) =
                    (p.shadow_prop_pipeline, p.downsample_pipeline, p.meter_pipeline, p.meter_hist_pipeline, p.post_pipeline, p.post_ldr_pipeline, p.fxaa_pipeline);
            }
            Err(e) => failed.push(format!("scene/shadow/post: {e}")),
        }
        if let Err(e) = self.sky.rebuild_pipelines(&self.device) { failed.push(format!("sky: {e}")); }
        let (common, lit_common) = (fn_live::wgsl!("../shaders/common.wgsl"), fn_live::wgsl!("../shaders/lit_common.wgsl"));
        if let Err(e) = self.statics.rebuild_pipelines(common, lit_common) { failed.push(format!("statics: {e}")); }
        if let Err(e) = self.volfog.rebuild_pipelines() { failed.push(format!("volfog: {e}")); }
        if let Some(b) = self.backdrop.as_mut() { if let Err(e) = b.rebuild_pipelines(&self.device) { failed.push(format!("backdrop/terrain: {e}")); } }
        if failed.is_empty() {
            eprintln!("[fn-live] shaders reloaded (scene, shadow, post, sky, statics, volfog, backdrop/terrain) in {:.0} ms", t.elapsed().as_secs_f32() * 1000.);
        } else {
            for f in &failed { eprintln!("[fn-live] shader reload failed, the old pipelines stay: {f}"); }
        }
    }

    /// ONE call per frame for the application: 3D scene + post chain (+ FXAA) into `target`, then the optional 2D overlay (HUD / menus,
    /// `fn_ui_wgpu::UiPass`) composited over the finished image in sRGB space. Submits everything before returning.
    pub fn render_with_overlay(&mut self, frame: &FrameParams, target: &wgpu::TextureView, overlay: Option<&mut dyn UiOverlay>) -> FrameStats {
        if fn_live::shader_generation() != self.shader_gen { self.reload_shaders(); }
        let stats = self.render_scene(frame, target);
        if let Some(o) = overlay {
            // The overlay records and submits its own command buffers: bracket them with queue-ordered timestamps.
            let ui = if self.profiler.inside_encoders() { self.profiler.slot("ui overlay", "ui") } else { None };
            if let Some(s) = ui { self.profiler.write_marker(&self.device, &self.queue, s); }
            o.draw(&self.device, &self.queue, target, frame.camera.width, frame.camera.height);
            if let Some(s) = ui { self.profiler.write_marker(&self.device, &self.queue, s + 1); }
        }
        let batched = self.batch.lock().map_or(false, |b| b.is_some());
        if batched { let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("timestamp resolve") }); if self.profiler.encode_end_frame(&mut enc) { self.defer_submit(enc.finish()); } }
        else { self.profiler.end_frame(&self.device, &self.queue); }
        self.profiler.collect(&self.device, false);
        stats
    }

    pub fn begin_batch(&self) { if let Ok(mut b) = self.batch.lock() { if b.is_none() { *b = Some(vec![]); } } }
    pub fn take_batch(&self) -> Vec<wgpu::CommandBuffer> { self.batch.lock().ok().and_then(|mut b| b.take()).unwrap_or_default() }
    pub fn after_batch_submitted(&mut self) {
        if self.batch_after.swap(false, std::sync::atomic::Ordering::Relaxed) { self.occlusion.after_submit(); }
        self.profiler.start_map();
    }
    fn defer_submit(&self, cb: wgpu::CommandBuffer) -> bool {
        if let Ok(mut g) = self.batch.lock() { if let Some(v) = g.as_mut() { v.push(cb); self.batch_after.store(true, std::sync::atomic::Ordering::Relaxed); return true; } }
        self.queue.submit([cb]);
        false
    }

    /// True when the device has timestamp queries (`Gpu::new` requests them when the adapter supports them).
    pub fn gpu_timing_supported(&self) -> bool { self.profiler.supported() }
    /// GPU time per pass of the most recent frame whose timestamps have been read back (lags the CPU by 1-3 frames; empty until the first
    /// result arrives or when unsupported, see `PassTimings::supported`). Passes: `shadow cascade 0..2`, `sky`, `scene`, `hdr mip n`,
    /// `exposure`, `post`, `fxaa`, `ui overlay` (needs TIMESTAMP_QUERY_INSIDE_ENCODERS). Groups: shadow / sky / scene / post / ui.
    pub fn last_pass_timings(&self) -> &PassTimings { self.profiler.last() }
    /// Blocks until every in-flight frame finished and its timestamps were read, so `last_pass_timings` is the frame just rendered
    /// (benchmarks and tests; never call it in the frame loop).
    pub fn flush_gpu_timings(&mut self) { self.profiler.collect(&self.device, true); }
    /// Every frame whose timestamps arrived since the previous call (oldest first) - for tables / histograms; combine with lush_gpu_timings.
    /// Frames are only measured while one of the 8 staging buffers is free, so flush at least every 8 frames to measure all of them.
    pub fn take_pass_timings(&mut self) -> Vec<PassTimings> { self.profiler.take_completed() }

    fn render_scene(&mut self, frame: &FrameParams, target: &wgpu::TextureView) -> FrameStats {
        let t0 = std::time::Instant::now();
        self.occlusion.begin_frame(&self.device); self.occl_cam = None; // OCCLUSION: pick up mapped HZB readbacks (occlusion.rs)
        let mut prep: Vec<PassTiming> = vec![]; let mut pt = t0; // PERF: CPU split of the prepare phase
        macro_rules! mark { ($n:expr) => {{ let now = std::time::Instant::now(); prep.push(PassTiming { name: concat!("prep ", $n).to_string(), encode_ms: (now - pt).as_secs_f32() * 1000. }); pt = now; }} }
        let opt = frame.options;
        // SCALABILITY: r.ScreenPercentage -> the 3D view size (quality::scaled_view_size); the scene and the post chain run at it, the upscale pass
        // writes the window. AA method of r.PostProcessAAQuality (quality::AaScalability): TAA jitters this frame's projection (taa.rs).
        let (view_w, view_h) = crate::quality::scaled_view_size(frame.camera.width, frame.camera.height, opt.screen_percentage);
        let upscaling = (view_w, view_h) != (frame.camera.width, frame.camera.height);
        let aa_method = if opt.fxaa.is_some() || opt.debug_mode != 0 { crate::quality::AaMethod::None } else { opt.aa.method() };        let taa_mode = match aa_method { crate::quality::AaMethod::TemporalAa { fast } => Some(fast), _ => None };
        let fxaa_console = matches!(aa_method, crate::quality::AaMethod::Fxaa { .. });
        let mut view_cam = *frame.camera;
        view_cam.width = view_w;
        view_cam.height = view_h;
        view_cam.jitter = self.taa.begin_frame(taa_mode, view_w, view_h);
        let cam = &view_cam;
        self.statics.set_view_distance_scale(opt.view_distance_scale); // SCALABILITY: r.ViewDistanceScale on every cull distance
        self.apply_quality(opt);
        self.profiler.begin_frame();
        self.ensure_targets(cam.width, cam.height);
        mark!("targets");
        // the debug FXAA tables of the old runtime (`--fxaa-level`) only on the window-size path
        let fxaa = opt.fxaa.filter(|_| opt.debug_mode == 0 && !upscaling);
        let fxaa_any = fxaa.is_some() || fxaa_console;
        // SCOPE HOOK: the scope post program needs the LDR image too (post -> ldr -> scope -> FXAA input / target)
        let scope_on = self.scope.active();
        if let Some(t) = self.targets.as_mut() {
            let (need_ldr, need_ldr2) = (fxaa_any || scope_on, fxaa_any && scope_on);
            for (slot, need, label) in [(&mut t.ldr, need_ldr, "ldr (fxaa / scope input)"), (&mut t.ldr2, need_ldr2, "ldr2 (scope output, fxaa input)")] {
                if !need { *slot = None; }
                else if slot.is_none() {
                    let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some(label), size: wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
                        dimension: wgpu::TextureDimension::D2, format: LDR_FORMAT, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
                    });
                    let view = tex.create_view(&Default::default());
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("fxaa bg"), layout: &self.bgl_fxaa, entries: &[bg_buf(0, &self.fxaa_buf), bg_tex(1, &view), bg_samp(2, &self.clamp_sampler)] });
                    *slot = Some((view, bg));
                }
            }
            // SCALABILITY: view-size LDR image of the upscale pass
            if !upscaling { t.up_src = None; } else if t.up_src.is_none() {
                let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("upscale source"), size: wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2, format: LDR_FORMAT, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
                });
                t.up_src = Some(tex.create_view(&Default::default()));
            }
        }
        let queue = &self.queue;

        // ---- uniforms ----
        let cu = cam.uniform(frame.time);
        queue.write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&cu));
        self.local_lights.prepare(queue, cam, opt.shadow_scalability.light_max_draw_distance_scale, crate::quality::lod_distance_factor(cam.fov_deg));
        let sun = lighting::sun_direction();
        let sr = lighting::sun_radiance();
        let sky_rad = lighting::sky_radiance(); // time-of-day sky light, up-facing SH value (sky_light.rs: SLS_SpecifiedCubemap, no scene capture)
        let (tx, ty) = cam.tan_half();
        let casters: Vec<ModelId> = frame.models.to_vec();
        let prop_count = self.prop_count;
        // sg.ShadowQuality (quality::ShadowScalability): r.ShadowQuality 0 clears EngineShowFlags.DynamicShadows (ShowFlags.cpp:375)
        let scal = opt.shadow_scalability;
        let csm = scal.csm();
        let num_cascades = csm.num_cascades.min(CASCADE_COUNT);
        let shadows_on = opt.shadows && scal.shadows_enabled() && num_cascades > 0 && (!casters.is_empty() || prop_count > 0 || self.statics.any() || self.fx.has_shadow_draws() || self.backdrop.as_ref().is_some_and(|b| b.has_main_terrain())); // FX SHADOW: FX casters too; TERRAIN MAIN PASS: the landscape casts
        let shadow_size = self.shadow_size;
        // DISTANCE FIELD HOOK: r.DistanceFieldShadowing / r.DistanceFieldAO of the scalability level; DFAO also needs the AO show flag
        // (`RenderDynamicSkyLighting` checks EngineShowFlags.AmbientOcclusion). `FN_DISTANCE_FIELDS=0`: debug switch (before / after comparisons), both off.
        let df_debug = fn_live::var("FN_DISTANCE_FIELDS").map_or(true, |v| v != "0");
        self.df.set_dynamic(self.fx.distance_field_objects()); // FX SHADOW: the FX static meshes (player-built pieces) join the distance-field scene
        {
            let t = self.targets.as_ref().unwrap();
            self.df.prepare(&self.device, &self.queue, cam, sun, shadow_size, shadows_on && scal.distance_field_shadowing && df_debug, opt.ao && scal.distance_field_ao && df_debug,
                &t.depth_view, self.fx_gen, if self.backdrop.as_ref().is_some_and(|b| b.has_main_terrain()) { 0. } else { GROUND_HALF } /* TERRAIN MAIN PASS: no flat plate in the GDF over the landscape (its heightfield instead, set_backdrop) */, &csm);
        }
        if self.df.take_bind_dirty() { self.rebuild_globals(); }
        mark!("distance fields");
        // PERF (2026-10-06, freezes): the split of "prep distance fields" (distance_field.rs prep_ms), report-only
        for (n, ms) in self.df.prep_ms.iter() { prep.push(PassTiming { name: format!("prep df {n}"), encode_ms: *ms }); }
        let queue = &self.queue;
        // The proxy creates the ray-traced cascade whether or not r.DistanceFieldShadowing projects it (ShouldCreateRayTracedCascade ignores the cvar): the last
        // CSM cascade gets its fade extension and the whole-scene fade runs to the DF distance (GetDirectionalLightDistanceFadeParameters, DirectionalLightComponent.cpp:308).
        let rt_proxy = crate::distance_field::should_create_ray_traced_cascade(&csm);
        let cascades = lighting::fit_cascades_ex(cam.view_matrix().inverse(), tx, tx / ty, cam.near, sun, shadow_size, rt_proxy, &csm);
        let fade_far_m = if rt_proxy { crate::distance_field::ue::DISTANCE_FIELD_SHADOW_DISTANCE_CM * 0.01 } else { csm.distance_m };
        // r.LightFunctionQuality <= 0 clears EngineShowFlags.LightFunctions (ShowFlags.cpp:359): grey = disabled = 1 -> no light function
        // LIGHT FUNCTION (2026-10-07): the High-quality program of the sun's light function (light_function.rs; r.MaterialQualityLevel 1 in the live Epic dump) runs in the lit
        // shaders from the absolute world position; the volumetric fog takes its value at the camera (CPU, light_function::eval_cpu) until it renders its own light-function texture.
        let lf_on = scal.light_functions_enabled();
        let lf_time = crate::light_function::game_time(frame.time);
        let lf_vol = match (&self.light_function.cpu, lf_on) {
            (Some((sp, fo)), true) => crate::light_function::eval_cpu(sp, fo, crate::light_function::to_ue_abs_cm(cam.eye), lf_time).clamp(0., 1.),
            _ => 1.,
        };
        let lf_off = crate::light_function::ue_offset_cm();
        // TOD LIVE: this frame's height-fog view uniforms (InitFogConstants / FExponentialHeightFogShaderParameters every frame) from the clock's phase blend;
        // View.VolumetricFogMaxDistance only while the volumetric fog runs (the same condition as the volfog.prepare below, VolumetricFog.cpp:843-862)
        let view_fog = crate::fog::live_uniform(opt.fog && scal.volumetric_fog);
        let light = LightUniform {
            sun_dir: [sun.x, sun.y, sun.z, 0.], sun_radiance: [sr[0], sr[1], sr[2], 0.], sky_radiance: [sky_rad[0], sky_rad[1], sky_rad[2], 0.],
            shadow_matrix: std::array::from_fn(|i| lighting::texture_matrix(&cascades[i]).to_cols_array_2d()),
            // w: FarDistance of GetDirectionalLightDistanceFadeParameters (CSM max distance, or the DF distance with the ray-traced cascade)
            cascade_far: [cascades[0].far, cascades[1].far, cascades[2].far, fade_far_m],
            cascade_texel: [cascades[0].texel, cascades[1].texel, cascades[2].texel, lighting::shadow_sharpen_factor()],
            // y: shadow-mapped cascade count, z: SHADOW_QUALITY of the projection (ManualPCF kernel)
            shadow_params: [if shadows_on { 1. } else { 0. }, num_cascades as f32, scal.pcf_quality() as f32, 0.],
            misc: [if fn_live::var_os("FN_DEBUG_UNLIT").is_some() { 0. } else { 1. } /* PERF: debug-only A/B of the lighting cost */, (opt.material_debug | fn_live::var("FN_MATERIAL_DEBUG").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0)) as f32, shadow_size as f32, if opt.fog { 1. } else { 0. }],
            cascade_ext: [cascades[0].fade_extension, cascades[1].fade_extension, cascades[2].fade_extension, lighting::SHADOW_DISTANCE_FADEOUT_FRACTION],
            cascade_bias: [cascades[0].depth_bias, cascades[1].depth_bias, cascades[2].depth_bias, 0.],
            light_fn: [lf_vol, lighting::LIGHT_FUNCTION_DISABLED_BRIGHTNESS, lighting::LIGHT_FUNCTION_FADE_DISTANCE_M, lf_time],
            sky_sh: lighting::sky_sh(), // SKY LIGHT: View.SkyIrradianceEnvironmentMap x SkyLightColor (sky_light.rs)
            fog: view_fog,
            light_fn_world: [lf_off[0], lf_off[1], lf_off[2], if lf_on { 1. } else { 0. }],
        };
        queue.write_buffer(&self.light_buf, 0, bytemuck::bytes_of(&light));
        self.sky_refl.update(queue); // SKY REFLECTION HOOK: View.SkyLightColor of the clock
        let lod_factor = crate::quality::lod_distance_factor(cam.fov_deg);
        self.statics.prepare(cam, &cascades, num_cascades, shadows_on, frame.time, scal.radius_threshold, lod_factor);
        {
            let view_planes = crate::static_mesh::frustum_planes(&(cam.proj_matrix() * cam.view_matrix()));
            let light_planes: Vec<_> = cascades.iter().map(|c| crate::static_mesh::frustum_planes(&c.light_view_proj)).collect();
            let cull = !prop_cull_off();
            for (s, d) in self.prop_draws.iter_mut().enumerate() {
                d.clear();
                for &(start, n, c, r) in &self.prop_chunks {
                    let keep = !cull || if s == 0 { crate::static_mesh::sphere_in_planes(&view_planes, 6, c, r) } else { crate::static_mesh::sphere_in_planes(&light_planes[s - 1], 4, c, r) };
                    if !keep { continue; }
                    match d.last_mut() { Some(l) if l.end == start => l.end = start + n, _ => d.push(start..start + n) }
                }
            }
        }
        // OUTLINE: the highlighted actors' instances + the pass's targets / uniforms / bind groups
        self.statics.prepare_custom(cam, self.outline.stencil());
        { let t = self.targets.as_ref().unwrap(); self.outline.prepare(t.width, t.height, HDR_FORMAT, &t.depth_view); }
        { let t = self.targets.as_ref().unwrap(); self.decals.prepare(&t.depth_view); }
        mark!("statics");
        // ShadowSetup.cpp:2454 small-caster cull of the skinned models (bounds origin to the shadow view origin = the camera); the same for every cascade
        let casters: Vec<ModelId> = casters.into_iter().filter(|id| {
            let m = &self.models[id.0];
            let w = m.world.lock().map(|w| *w).unwrap_or(Mat4::IDENTITY);
            let scale = w.x_axis.truncate().length().max(w.y_axis.truncate().length()).max(w.z_axis.truncate().length());
            let centre = w.transform_point3(m.bounds.0);
            !crate::quality::shadow_caster_screen_size_culled(m.bounds.1 * scale, (centre - cam.eye).length_squared(), scal.radius_threshold, lod_factor)
        }).collect();
        for (i, c) in cascades.iter().enumerate() {
            // LightViewProj { m, depth_bias }: ShaderDepthBias is added to the stored depth by the depth pass (shadow.wgsl)
            let mut vp = [0f32; 20];
            vp[..16].copy_from_slice(&c.light_view_proj.to_cols_array());
            vp[16] = c.depth_bias;
            queue.write_buffer(&self.shadow_vp_bufs[i], 0, bytemuck::cast_slice(&vp));
        }
        // PERF (2026-10-06): the FX shadow draws with bounds are culled per cascade like the static meshes above (fx.rs set_shadow_cull)
        self.fx.set_shadow_cull(&cascades[..num_cascades], cam.eye, scal.radius_threshold, lod_factor);
        mark!("casters");
        // PERF (report only): per-cascade draws / triangles by caster source, averaged and logged every 300 frames with `FN_SHADOW_STATS=1`
        if shadows_on && fn_live::var_os("FN_SHADOW_STATS").is_some() {
            let chars = casters.iter().flat_map(|id| self.models[id.0].prims.iter()).filter(|p| p.overlay.is_none()).fold((0u64, 0u64), |a, p| (a.0 + 1, a.1 + p.index_count as u64 / 3));
            let gun = self.gun.shadow_stats();
            self.shadow_stats.0 += 1;
            for c in 0..num_cascades {
                let st = self.statics.shadow_cascade_stats(c);
                let fx = self.fx.shadow_cascade_stats(c);
                let row = [chars, if prop_count > 0 { (1, prop_count as u64 / 3) } else { (0, 0) }, st[0], st[1], fx[0], fx[1], gun];
                for (acc, v) in self.shadow_stats.1[c].iter_mut().zip(row) { acc.0 += v.0; acc.1 += v.1; }
            }
            if self.shadow_stats.0 % 300 == 0 {
                let n = self.shadow_stats.0 as f64;
                for c in 0..num_cascades {
                    let a = &self.shadow_stats.1[c];
                    let (td, tt) = a.iter().fold((0, 0), |s, v| (s.0 + v.0, s.1 + v.1));
                    let parts: Vec<String> = SHADOW_STAT_SOURCES.iter().zip(a).map(|(name, v)| format!("{name} {:.1} draws / {:.0} tris", v.0 as f64 / n, v.1 as f64 / n)).collect();
                    eprintln!("[shadow stats] cascade {c} ({} frames avg): total {:.1} draws / {:.0} tris; {}", self.shadow_stats.0, td as f64 / n, tt as f64 / n, parts.join("; "));
                }
            }
        }
        self.sky.update(queue, cam, frame.time, opt.sky_layers, &view_fog); // TOD LIVE: the same fog view uniforms as the scene
        if let Some(b) = self.backdrop.as_mut() { b.prepare(queue, cam, frame.time, &cascades[..num_cascades], shadows_on && !scene_skip("terrain-shadow") /* TERRAIN MAIN PASS: casters per cascade; debug-only A/B switch */); b.ensure_depth(&self.device, cam.width, cam.height); if let Some(t) = self.targets.as_ref() { b.ensure_scene_depth(&self.device, &t.depth_view, self.fx_gen); } } // BACKDROP HOOK (+ SSR BACKDROP: its own depth target)
        mark!("sky+backdrop");
        // POST-PROCESS WIRING: the stacked TOD post-process components at the live clock (UE 4.16 volume blending), see tonemap::PostSettings::blend
        let ps = tonemap::PostSettings::current().with_dynamic();
        let post = ps.post_params(
            opt.bloom, opt.vignette, opt.ao,
            [tx, ty, cam.near, cam.far],
            [cam.width as f32, cam.height as f32, opt.display_exponent, fn_live::var("FN_POST_DEBUG").ok().and_then(|v| v.parse::<i32>().ok()).unwrap_or(opt.debug_mode) as f32], // FN_POST_DEBUG=2: the screen-space AO alone (verification aid),
            [opt.backdrop.dim.clamp(0., 1.), opt.backdrop.blur.clamp(0., 1.), 0., 0.],
        );
        // SCALABILITY: r.Tonemapper.Sharpen / 6 and r.Tonemapper.GrainQuantization with GrainRandomFull of FrameIndexMod8 (advanced before use)
        let mut post = post;
        let grain = crate::taa::grain_random(((self.frame_index + 1) % 8) as u32);
        post.tonemap_ext = [opt.aa.sharpen() / 6., if opt.post_scalability.grain_quantization() { 1. } else { 0. }, grain[0], grain[1]];
        queue.write_buffer(&self.post_buf, 0, bytemuck::bytes_of(&post));
        if let Some(p) = fxaa {
            queue.write_buffer(&self.fxaa_buf, 0, bytemuck::cast_slice(&[1. / cam.width as f32, 1. / cam.height as f32, 0., 0., p.subpix, p.edge_threshold, p.edge_threshold_min, p.search_steps as f32]));
        }
        // SCALABILITY: r.EyeAdaptationQuality 0 (PostProcessQuality@0/@1) clears EngineShowFlags.EyeAdaptation (ShowFlags.cpp:366-371): the exposure is then
        // fixed at Min = Max = 2^0 times ExposureOffsetMultipler 2^AutoExposureBias (ComputeEyeAdaptationValues, PostProcessEyeAdaptation.cpp:25-51)
        let fixed_exposure = opt.fixed_exposure.or_else(|| (!opt.post_scalability.eye_adaptation()).then(|| 2f32.powf(ps.ae_bias)));
        let meter = ps.meter_params(frame.dt.clamp(0., 0.25), self.frame_index == 0, fixed_exposure.unwrap_or(0.));
        queue.write_buffer(&self.meter_buf, 0, bytemuck::bytes_of(&meter));

        // MIRRORFIX (translucency volume): this frame's translucency lighting volume (UE4.16 TranslucentLighting.cpp), injected after the shadow depths, read by the
        // translucent lit FX programs; the window camera (unjittered, = the app's FX View buffers) places it. Only built when translucent FX draws exist.
        {
            let c = frame.camera;
            let needed = self.fx.has_draws(true) || self.fx.has_separate();
            let vf = crate::translucent_lighting::VolumeFrame { eye: c.eye, forward: c.forward, up: c.up, tan_half: c.tan_half(), near_m: cam.near, cascades: &cascades,
                num_shadowed: if shadows_on { num_cascades } else { 0 }, transition_fraction: csm.transition_fraction, fade_far_m, fade_fraction: lighting::SHADOW_DISTANCE_FADEOUT_FRACTION,
                light_function: lf_vol, affects_translucent: crate::translucent_lighting::SUN_AFFECTS_TRANSLUCENT_LIGHTING, shadow_size: self.shadow_size, shadow_view: &self.shadow_array_view, light_buf: &self.light_buf };
            self.tlv.prepare(&self.device, &self.queue, &vf, &mut self.fx, needed);
        }
        // VOLUMETRIC FOG HOOK: size the volume to the viewport, upload the frame's parameters (the passes below are added only while it is active)
        // r.VolumetricFog / r.VolumetricFog.GridPixelSize / GridSizeZ of the shadow scalability level
        // FOG INPUTS (2026-10-05): prepared before the FX pass (was after it), which binds this frame's integrated volume (UE: ComputeVolumetricFog runs before the
        // translucency that reads it, DeferredShadingRenderer.cpp)
        if opt.fog && scal.volumetric_fog {
            let t = self.targets.as_ref().unwrap();
            self.volfog.prepare(&self.queue, &crate::volfog::VolFogFrame { width: t.width, height: t.height, camera: cam, frame_index: self.frame_index, shadow_view: &self.shadow_array_view, shadow_size: self.shadow_size, depth_view: &t.depth_view, depth_gen: self.fx_gen,
                grid_pixel_size: scal.volumetric_fog_grid_pixel_size, grid_size_z: scal.volumetric_fog_grid_size_z });
        } else { self.volfog.skip_frame(); }
        mark!("tlv+volfog");
        // FOG INPUTS: the height-fog constants of this view (`FSceneRenderer::InitFogConstants`, FogRendering.cpp:258, at the window camera's Z = the FX View
        // buffers' ViewOrigin; the live clock's phase; the sun the renderer's own fog uses for its directional inscattering) and the volumetric fog's view slots +
        // integrated volume, for every FX program that binds `FHeightFogShaderParameters` (fx.rs register_height_fog)
        {
            let vol = self.volfog.active();
            let to_sun = lighting::sun_direction();
            let values = if opt.fog { crate::fog::shader_constants(&crate::fog::active(), frame.camera.eye.y * 100.0, crate::fog::world_z_ref_cm() /* FOG HEIGHT RE-BASE */, [to_sun.x, to_sun.z, to_sun.y], vol) } else { crate::fog::no_fog_constants(vol) };
            // `FN_FX_FOG_INPUTS=0`: debug switch, the old all-zero inputs (before / after comparisons only)
            let fog = if fn_live::var("FN_FX_FOG_INPUTS").map_or(false, |v| v == "0") { crate::fx::FxViewFog::default() } else { crate::fx::FxViewFog { values, view: self.volfog.view_constants() } };
            self.fx.set_view_fog(fog, self.volfog.integrated_view());
        }
        // RENDER FX HOOK: upload this frame's FX buffers / dynamic geometry, (re)build the bind groups that read the scene depth
        // VELOCITY: ShouldRenderVelocities (motion blur from the integrator, TAA, DFAO, SSR temporal = SSR on without TAA) + this frame's previous view (taa.rs)
        {
            let ssr_temporal = !self.taa.active() && crate::quality::ssr_shader_quality(opt.effects.ssr_quality, crate::quality::SSR_PP_QUALITY, crate::quality::SSR_PP_INTENSITY).is_some() && fn_live::var("FN_SSR").map_or(true, |v| v != "0");
            self.taa.begin_velocity(&self.queue, cam, self.df.ao_active(), ssr_temporal);
            let vs = self.taa.velocity_state();
            self.fx.set_velocity_state(vs);
            self.gun.set_velocity_state(vs);
            // GRASS MOTION (2026-10-07): the FX View buffers take this frame's TAA jitter like every other primitive of the view (taa.rs apply_view_jitter);
            // `FN_FX_JITTER=0` (debug A/B only) leaves them unjittered
            static FX_JIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            let jit = *FX_JIT.get_or_init(|| fn_live::var("FN_FX_JITTER").map_or(true, |v| v != "0"));
            self.fx.set_view_jitter(if jit { cam.jitter } else { [0.0; 2] });
            self.fx.set_view_size((cam.width, cam.height));
            self.gun.set_view_size((cam.width, cam.height));
        }
        { let t = self.targets.as_ref().unwrap(); self.fx.prepare(&t.depth_view, self.fx_gen); self.fx.prepare_separate(t.width, t.height); } // (+ the separate translucency target, fx.rs)
        mark!("fx");
        // BLOOM HOOK: this frame's bloom uniforms (the chain is rebuilt with the targets; rebind post / meter if its output ever changes)
        {
            let t = self.targets.as_mut().unwrap();
            // SCALABILITY: r.BloomQuality / r.FastBlurThreshold / r.Filter.SizeScale of the PostProcessQuality level
            self.bloom.prepare_with(&self.queue, &t.hdr_mip_views[0], t.width, t.height, self.fx_gen, &bloom_params(), 1.0, &opt.post_scalability.bloom_scalability());
            if self.bloom.output_id() != self.bloom_bound {
                self.bloom_bound = self.bloom.output_id();
                let hdr_full = t.hdr.create_view(&Default::default());
                t.post_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("post bg"), layout: &self.bgl_post, entries: &[
                    bg_buf(0, &self.post_buf), bg_tex(1, &hdr_full), bg_tex(2, &t.depth_view), bg_buf(3, &self.exposure_buf), bg_samp(4, &self.clamp_sampler), bg_tex(5, self.bloom.output().unwrap()), bg_tex(10, self.color_lut.view()),
                ] });
                t.meter_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("meter bg"), layout: &self.bgl_meter, entries: &[bg_buf(0, &self.meter_buf), bg_tex(1, self.bloom.half_res().unwrap()), bg_buf(2, &self.exposure_buf), bg_buf(3, &self.meter_hist_buf)] });
            }
        }
        {
            let t = self.targets.as_ref().unwrap();
            // LIGHT SHAFT HOOK: the shaft origin lies towards the sun MODEL (AFortTimeOfDayManager sets LightShaftOverrideDirection, see light_shafts.rs)
            let shafts_on = opt.light_shafts && fn_live::var("FN_LIGHT_SHAFTS").map_or(true, |v| v != "0");
            // LIGHT SHAFT TAA: ApplyTemporalAA runs for AAM_TemporalAA views (taa.rs active), see light_shafts.rs
            let ls_taa = self.taa.active().then(|| crate::light_shafts::LsTaaFrame { cam, jitter_px: self.taa.jitter_px() });
            self.light_shafts.prepare(&self.device, &self.queue, shafts_on, cam.proj_matrix() * cam.view_matrix(), crate::time_of_day::sun_direction(), cam.near, cam.far,
                &t.hdr_mip_views[0], &t.depth_view, t.width, t.height, self.fx_gen, ls_taa, &self.exposure_buf);
            self.sun_mask.prepare(&self.device, &t.depth_view, t.width, t.height, self.fx_gen); // LIGHT SHAFT HOOK
        }
        mark!("bloom+shafts");
        self.particles.prepare(); // PARTICLE HOOK: upload this frame's particle instances
        { let items = self.particles.translucent_items(); self.fx.merge_particles(&items); } // SORTFIX: translucent particle batches join the FX translucency sets (fx.rs merge_particles)
        self.gun.prepare(); // GUN MATERIAL HOOK: upload this frame's gun uniform buffers
        self.scope.upload(); // SCOPE HOOK: this frame's scope uniform buffers
        mark!("particles+gun");
        if scope_on { // SCOPE HOOK: pipelines for the target formats + bind groups around the LDR image
            let fmt = if fxaa_any || upscaling { LDR_FORMAT } else { self.output_format };
            if let Some(l) = self.targets.as_ref().and_then(|t| t.ldr.as_ref()) { self.scope.prepare(&l.0, LDR_FORMAT, fmt); }
        }
        // (VOLUMETRIC FOG HOOK: prepared above, before the FX pass: FOG INPUTS)
        // SCALABILITY: TAA targets / uniforms, the Medium FXAA console pass and the upscale pass (pipelines + bind groups need &mut self.taa)
        {
            let t = self.targets.as_ref().unwrap();
            self.taa.prepare(&self.queue, cam, &t.depth_view, self.fx_gen, &self.exposure_buf);
        }
        // REFLECTION APPLY: the SSR of this frame (r.SSR.Quality of the Effects level; colour = the previous TAA history when there is one, reflection.rs), then
        // the AO / reflection apply pass's scene copy + bind group (AO ORDER)
        {
            let t = self.targets.as_ref().unwrap();
            self.ssr.prepare(&self.queue, &crate::reflection::SsrFrame {
                cam, cvar_quality: opt.effects.ssr_quality, depth_view: &t.depth_view, depth_gen: self.fx_gen, gbuf_view: &t.refl.views[2],
                taa_history: self.taa.prev_history(), taa_active: self.taa.active(), velocity: self.taa.velocity_view(), jitter_px: self.taa.jitter_px(),
                frame_index: self.frame_index, exposure_buf: &self.exposure_buf,
            });
            // UE SSAO: the HZB (also the SSR's) and the screen-space AO passes when PostProcessQuality enables them (r.AmbientOcclusionLevels != 0)
            let mut ao_on = false;
            if opt.ao {
                self.ssr.prepare_hzb(&self.queue, cam, &t.depth_view, self.fx_gen);
                self.occl_cam = Some(*cam); // OCCLUSION: the camera of this HZB
                if let Some(hzb) = self.ssr.hzb() {
                    ao_on = self.ssao_ue.prepare(&self.queue, &crate::ssao_ue::SsaoFrame {
                        cam, depth_view: &t.depth_view, gbuf_view: &t.refl.views[2], hzb, gen: self.fx_gen, intensity: ps.ao_intensity,
                        scalability: opt.post_scalability, taa_sample_index: self.taa.sample_index(),
                    });
                }
            }
            if !ao_on { self.ssao_ue.disable(); }
            let has_char = frame.models.iter().any(|id| self.models[id.0].prims.iter().any(|p| p.char_bg.is_some()));
            self.sss_on = has_char && opt.effects.sss_scale > 0.;
            if self.sss_on {
                self.sss.ensure_targets(t.width, t.height, opt.effects.sss_half_res);
                self.sss.prepare(&self.queue, cam.tan_half().0, opt.effects.sss_scale, opt.effects.sss_sample_set, cam.near, cam.far, self.ssr.active(), ao_on);
            }
            self.ssao.prepare(&self.device, &self.queue, &self.post_buf, &t.depth_view, [&t.refl.views[0], &t.refl.views[1]], self.ssr.output(), self.ssr.output_key(), self.ssr.active(),
                ao_on, self.ssao_ue.ao_view(), self.ssao_ue.ao_key(), HDR_FORMAT, t.width, t.height, self.fx_gen);
        }
        mark!("taa+ssr+ssao");
        let final_fmt = if upscaling { LDR_FORMAT } else { self.output_format };
        let console_pass = if fxaa_console {
            let t = self.targets.as_ref().unwrap();
            let src = if scope_on { t.ldr2.as_ref() } else { t.ldr.as_ref() }.map(|l| l.0.clone());
            src.map(|v| self.taa.fxaa_console(&self.queue, &v, (t.width, t.height), final_fmt, &self.clamp_sampler))
        } else { None };
        let upscale_pass = if upscaling {
            let t = self.targets.as_ref().unwrap();
            t.up_src.clone().map(|v| self.taa.upscale(&self.queue, &v, (t.width, t.height), opt.post_scalability.upscale_quality, self.output_format, &self.clamp_sampler))
        } else { None };
        mark!("aa passes");
        // ---- frame graph ----
        let this: &Renderer = &*self;
        let targets = this.targets.as_ref().unwrap();
        let mut graph = FrameGraph::new();
        // PERF (2026-10-06): this frame's staged FX buffer writes (fx.rs `encode_uploads`), before every pass that reads them (the cascades' FX casters first)
        graph.add_if(this.fx.has_uploads(), "fx uploads", move |enc| { this.fx.encode_uploads(enc); });
        for i in 0..CASCADE_COUNT {
            let casters = &casters;
            graph.add_if(shadows_on && i < num_cascades, format!("shadow cascade {i}"), move |enc| {
                let ts = this.profiler.slot(&format!("shadow cascade {i}"), "shadow");
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shadow cascade"), color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &this.shadow_layer_views[i], depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                    timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
                });
                // The cascade occupies the centre of the map: ResolutionX = size - 2 * SHADOW_BORDER texels inside a cleared border (ShadowSetup.cpp:2649).
                let vp = (this.shadow_size.saturating_sub(2 * lighting::SHADOW_BORDER)).max(1) as f32;
                pass.set_viewport(lighting::SHADOW_BORDER as f32, lighting::SHADOW_BORDER as f32, vp, vp, 0., 1.);
                let mut sided = None;
                pass.set_bind_group(0, &this.shadow_vp_bgs[i], &[]);
                for id in casters {
                    let m = &this.models[id.0];
                    pass.set_bind_group(1, &m.obj_bg, &[]);
                    for p in &m.prims {
                        if p.overlay.is_some() { continue; } // CHAR MATERIAL HOOK: a modulate overlay casts no shadow
                        if sided != Some(p.two_sided) { sided = Some(p.two_sided); pass.set_pipeline(&this.shadow_pipeline[p.two_sided as usize]); }
                        pass.set_vertex_buffer(0, p.vb.slice(..));
                        pass.set_index_buffer(p.ib.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..p.index_count, 0, 0..1);
                    }
                }
                if prop_count > 0 {
                    pass.set_pipeline(&this.shadow_prop_pipeline);
                    pass.set_bind_group(0, &this.shadow_vp_bgs[i], &[]);
                    pass.set_vertex_buffer(0, this.prop_vb.slice(..));
                    for r in &this.prop_draws[1 + i] { pass.draw(r.clone(), 0..1); }
                }
                this.statics.draw_shadow(&mut pass, i, &this.shadow_vp_bgs[i]);
                this.fx.draw_shadow(&mut pass, i); // FX SHADOW: the FX pass' opaque / masked casters with their cooked TShadowDepthVS / PS (sets its own groups)
                this.gun.draw_shadow(&mut pass, i); // GUN SHADOW: the original-material held / dropped weapons (sets its own groups, last)
                if let Some(b) = this.backdrop.as_ref() { b.draw_main_shadow(&mut pass, i, &this.shadow_vp_bgs[i]); } // TERRAIN MAIN PASS: the landscape casters (sets its own groups)
            });
        }
        let layers = opt.sky_layers;
        let models = frame.models;
        let has_backdrop_pass = this.backdrop.is_some();
        // PREPASS: UE4.16 depth pre-pass (FDeferredShadingSceneRenderer::RenderPrePass, DeferredShadingRenderer.cpp:750 / DepthRendering.cpp:1233; cooked RendererSettings r.EarlyZPass = 2
        // = opaque AND masked, r.EarlyZPassMovable = True) for the ground and the static meshes: it clears the scene depth (the scene pass used to) and the lit
        // pass below shades only the surviving surface (CF_Equal, no depth writes), as the base pass does after UE's pre-pass (BasePassRendering.cpp:308-361).
        // PERF (2026-10-06): the pre-pass runs FIRST and clears the depth; the backdrop (next) skips the fragments it covers (backdrop.wgsl covered_by_scene) and
        // its depth merge keeps the nearer depth (Less), then the sky fills what is left. Same depth / colour as the old order (backdrop merge, then the pre-pass
        // with its Less test), without shading the covered backdrop and sky pixels.
        graph.add("prepass", move |enc| {
            let ts = this.profiler.slot("prepass", "scene");
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("depth prepass"), color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &targets.depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
            });
            // TERRAIN MAIN PASS (2026-10-07): the landscape components within the far plane (backdrop.rs prepare), depth-only twin of the lit draw in the scene pass
            if !scene_skip("terrain-depth") { if let Some(b) = this.backdrop.as_ref() { b.draw_main_depth(&mut pass, &this.globals_bg); } }
            pass.set_bind_group(0, &this.globals_bg, &[]);
            if this.ground_count > 0 { // the flat plate: only without a landscape (set_backdrop)
                pass.set_pipeline(&this.ground_depth_pipeline);
                pass.set_bind_group(1, &this.ground_material_bg, &[]);
                pass.set_bind_group(2, &this.ground_obj_bg, &[]);
                pass.set_vertex_buffer(0, this.ground_vb.slice(..));
                pass.draw(0..this.ground_count, 0..1);
            }
            // FULL PREPASS (2026-10-06): UE4.16 1.8 runs a FULL pre-pass (cooked r.SelectiveBasePassOutputs 1 / r.StencilForLODDither 1 -> ShouldForceFullDepthPass ->
            // DDM_AllOpaque + bEarlyZPassMovable, DeferredShadingRenderer.cpp:146-182; RenderPrePassViewDynamic draws every opaque / masked dynamic mesh element,
            // DepthRendering.cpp:849-874) and its base pass reads the depth only (:773). So every opaque draw of the scene pass below is in here, and their base-pass
            // pipelines test Equal: the procedural props, the skinned prims without a CharacterShader owner, the gun / pickup programs, the opaque / masked particles and
            // the opaque scene-lit FX programs (were: Less / LessEqual + depth write in the scene pass). The screen-space distance-field shadow pass reads this depth.
            if prop_count > 0 {
                pass.set_pipeline(&this.prop_depth_pipeline);
                pass.set_vertex_buffer(0, this.prop_vb.slice(..));
                for r in &this.prop_draws[0] { pass.draw(r.clone(), 0..1); }
            }
            this.statics.draw_depth(&mut pass);
            // PREPASS (2026-10-06): the masked scene-lit FX draws (real-material props, the grass) clip here with their own FDepthOnlyPS; the scene pass then draws
            // them with depth Equal and no clip (fx.rs draw_scene_lit_depth / create_scene_lit_pipeline); FULL PREPASS: the opaque ones too (position only)
            this.fx.draw_scene_lit_depth(&mut pass, &this.globals_bg);
            this.gun.draw_depth(&mut pass, &this.globals_bg); // FULL PREPASS: the held / dropped weapon programs (sets group 0 itself)
            // CHAR PREPASS (2026-10-06): the BR character materials are BLEND_Masked; with the cooked r.EarlyZPass=2 + r.EarlyZPassMovable=True +
            // r.EarlyZPassOnlyMaterialMasking=True the skinned meshes are in the pre-pass with their FDepthOnlyPS (the coverage clip) and the base pass below draws
            // them with depth Equal (char_material.rs). Was: no pre-pass entry, every overlapping layer of the hero fully lit (about 3.3 ms of the scene pass at 2544x1353).
            // FULL PREPASS: the prims without a CharacterShader owner (skinned_pipeline in the scene pass) with their position-only twin.
            if !scene_skip("char") {
                pass.set_bind_group(0, &this.globals_bg, &[]);
                let mut on_char: Option<bool> = None;
                for id in models.iter() {
                    let m = &this.models[id.0];
                    pass.set_bind_group(2, &m.obj_bg, &[]);
                    for p in &m.prims {
                        if p.overlay.is_some() { continue; } // drawn by the "char overlay" pass (modulate, no depth write)
                        let is_char = p.char_bg.is_some();
                        if on_char != Some(is_char) { on_char = Some(is_char); pass.set_pipeline(if is_char { &this.char_gpu.char_depth_pipeline } else { &this.skinned_depth_pipeline }); }
                        if let Some(cbg) = &p.char_bg { pass.set_bind_group(3, &cbg.0, &[]); pass.set_vertex_buffer(1, cbg.1.slice(..)); }
                        pass.set_bind_group(1, &p.material_bg, &[]);
                        pass.set_vertex_buffer(0, p.vb.slice(..));
                        pass.set_index_buffer(p.ib.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..p.index_count, 0, 0..1);
                    }
                }
            }
            this.particles.draw_opaque_depth(&mut pass, &this.globals_bg); // FULL PREPASS: opaque / masked particle batches (set their own groups)
        });
        // PERF (2026-10-06): the sky pass moved after the depth pre-pass (graph node "sky" below), so the backdrop is the first colour writer: it clears.
        // BACKDROP HOOK: landscape / ocean / skybox, own projection + own depth target (SSR BACKDROP: merged into the scene depth below). It is opaque, so drawing
        // it before the sky changes nothing: the sky only fills the pixels left at the far depth.
        graph.add_if(this.backdrop.is_some(), "backdrop", move |enc| {
            let ts = this.profiler.slot("backdrop", "sky");
            let [r0, r1, r2] = targets.refl.attachments(true); // REFLECTION APPLY: the reflection inputs, cleared here (0 = nothing to reflect)
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0., g: 0., b: 0., a: 1. }), store: wgpu::StoreOp::Store } }), r0, r1, r2],
                depth_stencil_attachment: this.backdrop.as_ref().and_then(|b| b.depth_view()).map(|view| wgpu::RenderPassDepthStencilAttachment { view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
            });
            if let Some(b) = &this.backdrop { b.draw(&mut pass, &this.globals_bg); }
            this.fx.draw_backdrop_lit(&mut pass, &this.globals_bg); // BACKDROP REAL (2026-10-07): the far skybox terraces / rocks on their real cooked material (fn-app backdrop_real.rs)
        });
        // SSR BACKDROP (2026-10-05): UE4.16 has ONE scene depth: the backdrop's depth is written into it (converted to the main projection, backdrop.rs
        // encode_depth_merge; it used to be cleared by the pre-pass), so the HZB / SSR, AO, fog, TAA reprojection, light-shaft and sun masks and every later depth
        // test see it as geometry. It also replaces the old "sun mask (backdrop)" pass: the sky sun's mask reads the scene depth, which now holds the backdrop.
        graph.add_if(this.backdrop.is_some(), "backdrop depth", move |enc| { if let Some(b) = &this.backdrop { b.encode_depth_merge(enc, &targets.depth_view); } });
        // DF SHADOW SCREEN PASS (2026-10-06): UE4.16 RayTraceShadows (DistanceFieldShadowing.cpp:813, the scatter-tile CS) at half resolution over the ONE scene depth:
        // the full pre-pass + the merged backdrop depth above (UE runs it after its base pass; the full pre-pass gives this forward renderer the same depth before
        // its lit pass). The lit shaders of the scene pass upsample it (lit_common.wgsl df_shadow_upsample, DistanceFieldShadowingUpsamplePS).
        graph.add_if(this.df.shadow_pass_active(), "df shadow", move |enc| { let ts = this.profiler.slot("df shadow", "scene"); this.df.encode_shadow(enc, this.profiler.render_writes(ts)); });
        // DISTANCE FIELD HOOK: DFAO from this frame's depth (global distance field updates, normals, tile culling, cone traces, upsample, history).
        // DFAO LAG (2026-10-07): right after the full depth pre-pass + backdrop merge (as "df shadow"), so the lit shaders of THIS frame read it, like UE4.16
        // where RenderDynamicSkyLighting applies the AO of the same frame (DeferredShadingRenderer.cpp:1097, after the base pass :909). Was: after the scene pass, read by the NEXT frame through a
        // world-position reprojection: every pixel the moving character (or anything) disoccluded found no AO sample and took V = 1, a lighter silhouette of the
        // character trailing one frame behind it ("an outline of myself"; gone with FN_DISTANCE_FIELDS=0, verification/ghost notes in the report).
        graph.add_if(this.df.ao_active(), "dfao", move |enc| {
            // PERF (debug only): `FN_DFAO_SPLIT=1` times every DFAO stage on its own
            static SPLIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *SPLIT.get_or_init(|| fn_live::var_os("FN_DFAO_SPLIT").is_some()) {
                let ts = |name: &str| this.profiler.compute_writes(this.profiler.slot(name, "scene"));
                this.df.encode_ao_split(enc, &ts);
            } else {
                let ts = this.profiler.slot("dfao", "scene");
                this.df.encode_ao(enc, this.profiler.compute_writes(ts));
            }
        });
        // PERF (2026-10-06): the sky after the pre-pass, against the scene depth (read only): the dome and clouds sit on the far plane and test LessEqual, so the
        // pixels the pre-pass (or the merged backdrop depth) covered are rejected before shading, as UE's base-pass sky behind the early-Z. Same image: those pixels
        // were overdrawn by the backdrop / scene before. Clears the colour when no backdrop pass ran.
        graph.add("sky", move |enc| {
            let ts = this.profiler.slot("sky", "sky");
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sky"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: if has_backdrop_pass { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(wgpu::Color { r: 0., g: 0., b: 0., a: 1. }) }, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &targets.depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
            });
            // LIGHT SHAFT HOOK: the sun disc is separate translucency in UE (M_Sun_Arid), drawn after the light-shaft downsample ("sky sun" below)
            this.sky.draw(&mut pass, layers & !sky::LAYER_SUN);
        });
        graph.add("scene", move |enc| {
            let ts = this.profiler.slot("scene", "scene");
            let [r0, r1, r2] = targets.refl.attachments(!has_backdrop_pass); // REFLECTION APPLY: cleared by the backdrop pass when it ran
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } }), r0, r1, r2],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &targets.depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }) /* PREPASS: written by the depth pre-pass */, stencil_ops: None }),
                timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
            });
            // TERRAIN MAIN PASS (2026-10-07): the landscape base pass (depth Equal against its pre-pass twin; sets its own pipeline / groups 0 and 1)
            if !scene_skip("terrain") { if let Some(b) = this.backdrop.as_ref() { b.draw_main(&mut pass, &this.globals_bg); } }
            pass.set_bind_group(0, &this.globals_bg, &[]);
            if this.ground_count > 0 && !scene_skip("ground") { // the flat plate: only without a landscape (set_backdrop)
            pass.set_pipeline(&this.ground_pipeline);
            pass.set_bind_group(1, &this.ground_material_bg, &[]);
            pass.set_bind_group(2, &this.ground_obj_bg, &[]);
            pass.set_vertex_buffer(0, this.ground_vb.slice(..));
            pass.draw(0..this.ground_count, 0..1);
            }
            if prop_count > 0 && !scene_skip("props") {
                pass.set_pipeline(&this.prop_pipeline);
                pass.set_vertex_buffer(0, this.prop_vb.slice(..));
                for r in &this.prop_draws[0] { pass.draw(r.clone(), 0..1); }
            }
            if !scene_skip("statics") { this.statics.draw_scene(&mut pass); }
            if !scene_skip("gun") { this.gun.draw(&mut pass); } // GUN MATERIAL HOOK: the held firearms (group 0 = the globals bound above)
            pass.set_bind_group(0, &this.globals_bg, &[]);
            pass.set_pipeline(&this.skinned_pipeline);
            let mut on_char = false; // CHAR MATERIAL HOOK: which of the two skinned pipelines is bound
            for id in models.iter().filter(|_| !scene_skip("char")) {
                let m = &this.models[id.0];
                pass.set_bind_group(2, &m.obj_bg, &[]);
                for p in &m.prims {
                    if p.overlay.is_some() { continue; } // CHAR MATERIAL HOOK: drawn by the "char overlay" pass
                    if p.char_bg.is_some() != on_char {
                        on_char = p.char_bg.is_some();
                        pass.set_pipeline(if on_char { &this.char_gpu.char_pipeline } else { &this.skinned_pipeline });
                        pass.set_bind_group(0, &this.globals_bg, &[]);
                        pass.set_bind_group(2, &m.obj_bg, &[]);
                    }
                    if let Some(cbg) = &p.char_bg { pass.set_bind_group(3, &cbg.0, &[]); pass.set_vertex_buffer(1, cbg.1.slice(..)); }
                    pass.set_bind_group(1, &p.material_bg, &[]);
                    pass.set_vertex_buffer(0, p.vb.slice(..));
                    pass.set_index_buffer(p.ib.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, 0..1);
                }
            }
            // PARTICLE BASE PASS: opaque / masked particle batches are base-pass dynamic mesh elements (BasePassRendering.cpp:1062), depth written, before SSAO /
            // DFAO / volumetric fog / velocity (particles.rs module docs); each batch sets its own pipeline and bind groups
            if !scene_skip("particles") { this.particles.draw_opaque(&mut pass, &this.globals_bg); }
            // FX RECEIVE: opaque / masked lit FX materials (building pieces, lit debris / props / pickups through the FX pass) are base-pass draws in UE4.16
            // (deferred, lit from the GBuffer after the shadow projection, before SSAO / DFAO / SSR / fog): drawn here with the globals at group 3, writing
            // the reflection inputs and the depth the AO / SSR / DFAO / volumetric fog passes read (fx.rs draw_scene_lit)
            if !scene_skip("fx") { this.fx.draw_scene_lit(&mut pass, &this.globals_bg); }
        });
        // FX ORDER (2026-10-05): opaque / masked FX draws are base-pass draws in UE4.16 (before the AO, reflections, DFAO and fog). Lit and unlit ones are in the
        // scene pass above (fn-fx assemble_scene_lit / assemble_scene_unlit); a pipeline built without the scene lighting (single colour target) still draws here,
        // right after the base pass, before the velocity / SSR / AO / DFAO / fog passes (it used to run after the volumetric fog composite).
        graph.add_if(this.fx.has_draws(false), "fx opaque", move |enc| {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx opaque"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &targets.depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            this.fx.draw(&mut pass, false);
        });
        // SCALABILITY (temporal AA): scene velocity of the skinned models, depth-tested against the opaque scene (velocity.wgsl); a model without a
        // previous transform yet takes its current one (no motion of its own)
        graph.add_if(this.taa.active(), "velocity", move |enc| {
            for id in models {
                let m = &this.models[id.0];
                if !m.prev_valid.load(std::sync::atomic::Ordering::Relaxed) {
                    enc.copy_buffer_to_buffer(&m.obj, 0, &m.prev_obj, 0, 64);
                    enc.copy_buffer_to_buffer(&m.bones, 0, &m.prev_bones, 0, (m.bone_count * 64) as u64);
                }
            }
            let mut on_char = false; // VELOCITY (2026-10-07): which twin is bound (begin_velocity_pass binds the plain one)
            // `FN_FX_VELOCITY=0`: debug switch (before / after comparisons only), the old velocity pass (plain skinned twin only, no FX velocity); default: + the FX
            // twins below (GRASS MOTION, 2026-10-07: ON); `FN_FX_VELOCITY=1` also draws the weapon twins (still OFF by default, see there)
            static FX_VEL: std::sync::OnceLock<(bool, bool, bool)> = std::sync::OnceLock::new();
            let (new_vel, fx_vel, gun_vel) = *FX_VEL.get_or_init(|| { let v = fn_live::var("FN_FX_VELOCITY").ok(); (v.as_deref() != Some("0"), v.as_deref() != Some("0"), v.as_deref() == Some("1")) });
            let Some(mut pass) = this.taa.begin_velocity_pass(enc, &targets.depth_view) else { return };
            for id in models {
                let m = &this.models[id.0];
                pass.set_bind_group(1, &m.obj_bg, &[]);
                pass.set_bind_group(2, &m.prev_bg, &[]);
                for p in &m.prims {
                    if p.overlay.is_some() { continue; }
                    if new_vel && p.char_bg.is_some() != on_char { on_char = p.char_bg.is_some(); this.taa.set_velocity_char(&mut pass, on_char); }
                    pass.set_vertex_buffer(0, p.vb.slice(..));
                    pass.set_index_buffer(p.ib.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, 0..1);
                }
            }
            // VELOCITY (2026-10-07): the base-pass velocity of the original programs (r.BasePassOutputsVelocity = 1, SV_Target4): the scene-lit FX draws
            // (WPO grass / statics, buildings: `fs_velocity` of fn-fx, SkipOutputVelocity per primitive) and the held weapon / pickaxe programs
            // (`gun_velocity_main`). Was: their twins were built but never drawn, so the weapon and the wind-animated foliage reprojected with the camera only.
            // GRASS MOTION (2026-10-07): the FX twins are ON. The trunk / canopy > 2 px on still statics came from the View buffers' previous-camera members left 0
            // (PrevWorldCameraOrigin / PrevWorldViewOrigin / WorldViewOrigin: the previous-frame WPO saw another camera; taa.rs PREV_PAIRS, fn-fx view.rs); with
            // them filled a still camera gives 0 own velocity on every static and only the WPO grass / foliage moves (FN_TAA_DEBUG=2). Without this velocity
            // the TAA reprojected the swaying blades as static: ghosting / smearing. The weapon twins stay opt-in (`FN_FX_VELOCITY=1`, not re-verified).
            if fx_vel && !scene_skip("statics") { this.fx.draw_velocity(&mut pass, &this.globals_bg); }
            if gun_vel && !scene_skip("gun") { this.gun.draw_velocity(&mut pass, &this.globals_bg); }
        });
        // AO ORDER: screen-space AO on the finished opaque scene (UE: inside the lighting), before fog / translucency / light shafts / bloom / histogram
        // REFLECTION APPLY: the screen space reflections (HZB + trace (+ SSR temporal without TAA), reflection.rs) after the velocity pass, then UE's
        // ReflectionApplyPS in the AO pass: SO(GBufferAO x SSAO) x (SSR x EnvBRDF + (1 - SSR.a) x sky specular), before fog / translucency (ssao_apply.wgsl)
        graph.add_if(this.ssr.hzb_this_frame(), "hzb", move |enc| { let ts = this.profiler.slot("hzb", "scene"); this.ssr.encode_hzb(enc, this.profiler.render_writes(ts)); if let (Some(c), Some((tex, vs))) = (this.occl_cam.as_ref(), this.ssr.hzb_texture()) { this.occlusion.encode_copy(&this.device, enc, tex, vs, c, crate::reflection::UE_NEAR_M); } }); // OCCLUSION: + the readback copy of one HZB mip
        graph.add_if(this.ssr.active(), "ssr", move |enc| { let ts = this.profiler.slot("ssr", "scene"); this.ssr.encode(enc, &targets.hdr, this.profiler.render_writes(ts)); });
        // UE SSAO: AmbientOcclusionSetup / AmbientOcclusion mips / ScreenSpaceAO (ssao_ue.rs), then the apply (ssao_apply.wgsl) reads ScreenSpaceAO
        let ao_on = this.ssao_ue.active();
        graph.add_if(ao_on, "ssao", move |enc| {
            // PERF (debug only): `FN_SSAO_SPLIT=1` times every SSAO sub-pass on its own
            static SPLIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *SPLIT.get_or_init(|| fn_live::var_os("FN_SSAO_SPLIT").is_some()) {
                let ts = |name: &str| this.profiler.render_writes(this.profiler.slot(name, "scene"));
                this.ssao_ue.encode_split(enc, &ts);
            } else {
                let ts = this.profiler.slot("ssao", "scene");
                this.ssao_ue.encode(enc, this.profiler.render_writes(ts));
            }
        });
        graph.add("ssao + reflections", move |enc| { let ts = this.profiler.slot("reflection apply", "scene"); this.ssao.encode(enc, &targets.hdr, &targets.hdr_mip_views[0], ao_on, this.profiler.render_writes(ts)); });
        let sss_on = this.sss_on;
        graph.add_if(sss_on, "subsurface", move |enc| {
            if let Some(att) = this.sss.input_attachments() {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("char subsurface inputs"), color_attachments: &att,
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &targets.depth_view, depth_ops: None, stencil_ops: None }),
                    timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
                });
                pass.set_pipeline(&this.char_gpu.char_sss_pipeline);
                pass.set_bind_group(0, &this.globals_bg, &[]);
                for id in models {
                    let m = &this.models[id.0];
                    pass.set_bind_group(2, &m.obj_bg, &[]);
                    for p in &m.prims {
                        let Some(cbg) = &p.char_bg else { continue };
                        if p.overlay.is_some() { continue; }
                        pass.set_bind_group(3, &cbg.0, &[]);
                        pass.set_vertex_buffer(1, cbg.1.slice(..));
                        pass.set_bind_group(1, &p.material_bg, &[]);
                        pass.set_vertex_buffer(0, p.vb.slice(..));
                        pass.set_index_buffer(p.ib.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..p.index_count, 0, 0..1);
                    }
                }
            }
            let ssr = this.ssr.active().then(|| this.ssr.output());
            let ao = ao_on.then(|| this.ssao_ue.ao_view());
            this.sss.encode(enc, &crate::subsurface::SssFrame { depth: &targets.depth_view, ao, refl_sky: &targets.refl.views[0], refl_brdf: &targets.refl.views[1], ssr, hdr: &targets.hdr_mip_views[0] });
        });
        graph.add_if(this.decals.active(), "decals", move |enc| { let ts = this.profiler.slot("decals", "scene"); this.decals.encode(enc, &targets.hdr_mip_views[0], this.profiler.render_writes(ts)); });
        // CHAR MATERIAL HOOK: the eye-region modulate overlays (scene colour *= the original Eyeshadow shader's output), depth read-only, right after the scene pass
        let overlay_on = models.iter().any(|id| this.models[id.0].prims.iter().any(|p| p.overlay.is_some()));
        graph.add_if(overlay_on, "char overlay", move |enc| {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("char overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &targets.depth_view, depth_ops: None, stencil_ops: None }),
                timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(&this.char_gpu.overlay_pipeline);
            pass.set_bind_group(0, &this.globals_bg, &[]);
            for id in models {
                let m = &this.models[id.0];
                pass.set_bind_group(1, &m.obj_bg, &[]);
                for p in &m.prims {
                    let Some(o) = &p.overlay else { continue };
                    pass.set_bind_group(2, &o.bg, &[]);
                    pass.set_vertex_buffer(0, p.vb.slice(..));
                    pass.set_vertex_buffer(1, o.color_vb.slice(..));
                    pass.set_index_buffer(p.ib.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, 0..1);
                }
            }
        });
        // VOLUMETRIC FOG HOOK: material setup + light scattering + final integration (compute), then `dst * V.a + V.rgb` over the opaque scene (scene depth selects the slice)
        let vfog_on = this.volfog.active();
        graph.add_if(vfog_on, "vfog", move |enc| {
            let ts = this.profiler.slot("vfog", "scene");
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vfog"), timestamp_writes: this.profiler.compute_writes(ts) });
            this.volfog.encode_compute(&mut pass);
        });
        graph.add_if(vfog_on, "vfog composite", move |enc| {
            let ts = this.profiler.slot("vfog composite", "scene");
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vfog composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
            });
            this.volfog.encode_composite(&mut pass);
        });
        // OUTLINE: BL_BeforeTranslucency post-process material (SkinAndMeshOutlinePostProcess) over the opaque scene + fog: custom depth of the highlighted actors,
        // then the program rewrites the scene colour (identity where no custom stencil value is set, so nothing runs without highlighted actors)
        graph.add_if(this.outline.active(this.statics.has_custom()), "outline", move |enc| {
            this.outline.encode_custom_depth(enc, |pass| { pass.set_bind_group(0, &this.globals_bg, &[]); this.statics.draw_custom(pass); });
            this.outline.encode_post(enc, &targets.hdr, &targets.hdr_mip_views[0]);
        });
        // MIRRORFIX (translucency volume): clear + sun injection per CSM cascade (+ blur) after the shadow depths (UE: inside RenderLights / FilterTranslucentVolumeLighting,
        // DeferredShadingRenderer.cpp:1062-1080), before the translucency that reads it
        graph.add_if(this.tlv.active(), "translucency volume", move |enc| {
            let ts = this.profiler.slot("translucency volume", "scene");
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("translucency volume"), timestamp_writes: this.profiler.compute_writes(ts) });
            this.tlv.encode(&mut pass);
        });
        // RENDER FX HOOK: translucent / modulate FX materials (depth read-only, may sample the scene depth) over the finished scene. (FX ORDER: the opaque / masked
        // FX draws moved up, right after the scene pass.)
        // PARTICLE BASE PASS: the opaque / masked particle batches moved into the "scene" pass (base pass, depth written, before AO / fog / velocity)
        // SORTFIX: the standard translucency set (TPT_StandardTranslucency) = FX draws + translucent particle batches in one FTranslucentPrimSet order (fx.rs merge_particles)
        graph.add_if(this.fx.has_merged(false), "fx translucent", move |enc| {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx translucent"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &targets.depth_view, depth_ops: None, stencil_ops: None }),
                timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            this.fx.draw_merged(&mut pass, false, &this.particles, &this.globals_bg);
        });
        // RENDER FX HOOK (separate translucency, UE4.16 TranslucentRendering.cpp / PostProcessing.cpp:1437): bEnableSeparateTranslucency FX draws go into their own
        // target (cleared to (0,0,0,1), scene depth read-only), recombined after the standard translucency and before bloom (no DOF in the port: the
        // `FRCPassPostProcessBokehDOFRecombine` without DOF is exactly scene * sep.a + sep.rgb). Lives here, not in fn-postfx: the bloom pass reads the result.
        // LIGHT SHAFT HOOK: RenderLightShaftBloom runs after RenderTranslucency (DeferredShadingRenderer.cpp:1209); its downsample reads the scene colour
        // WITHOUT the separate translucency (own buffer until the recombine)
        graph.add_if(this.light_shafts.active(), "light shafts", move |enc| { this.light_shafts.encode_downsample_blur(enc); });
        // LIGHT SHAFT HOOK: the sun disc as separate translucency (after the shaft source, over the clouds, behind anything opaque incl. the backdrop)
        graph.add("sky sun", move |enc| {
            this.sun_mask.encode(enc, true); // SSR BACKDROP: a fresh mask from the scene depth, which holds the backdrop now
            let Some(mask) = this.sun_mask.view() else { return };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sky sun (separate translucency)"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: mask, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            this.sky.draw_sun_separate(&mut pass, layers);
        });
        graph.add_if(this.fx.has_merged(true), "fx separate translucency", move |enc| {
            let Some(sep) = this.fx.separate_view() else { return };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx separate translucency"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: sep, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0., g: 0., b: 0., a: 1. }), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &targets.depth_view, depth_ops: None, stencil_ops: None }),
                timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            this.fx.draw_merged(&mut pass, true, &this.particles, &this.globals_bg); // SORTFIX: TPT_SeparateTranslucency = FX draws + particle batches, one sort
        });
        graph.add_if(this.fx.has_merged(true), "fx separate recombine", move |enc| {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx separate recombine"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[0], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            this.fx.composite_separate(&mut pass);
        });
        // BLOOM HOOK: UE 4.16 order - every translucent / FX / particle pass is in the scene colour BEFORE SceneColorHalfRes, i.e. bright
        // additive emissive FX feed bloom and the eye-adaptation histogram (PostProcessing.cpp: separate translucency is recombined before
        // the half-res downsample). Also produces the histogram input, so it runs even with bloom switched off.
        // LIGHT SHAFT HOOK: r.LightShaftRenderToSeparateTranslucency = 1 (cooked RendererSettings) adds the shafts to the separate-translucency RGB, i.e. after the
        // recombine's `scene * sep.a`: identical to adding them to the scene colour here. Before SceneColorHalfRes, so they feed bloom and the histogram.
        graph.add_if(this.light_shafts.active(), "light shafts apply", move |enc| { this.light_shafts.encode_apply(enc, &targets.hdr_mip_views[0]); });
        // SCALABILITY: temporal AA on the HDR scene colour after every translucency and before the half-res downsample / bloom / histogram
        // (PostProcessing.cpp:1449-1465)
        // STORM TAA (2026-10-07): the responsive-AA mask of this frame (UE's stencil bit written by the bEnableResponsiveAA translucent draws, e.g. the storm
        // wall; cleared to "untagged" every TAA frame, taa.rs / fx.rs draw_responsive_aa), read by the resolve below. `FN_TAA_RESPONSIVE=0`: debug switch
        // (before / after comparisons only), nothing tagged = the old resolve
        static RESPONSIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let responsive_on = *RESPONSIVE.get_or_init(|| fn_live::var("FN_TAA_RESPONSIVE").map_or(true, |v| v != "0"));
        graph.add_if(this.taa.active(), "taa responsive mask", move |enc| {
            let Some(mut pass) = this.taa.begin_responsive_pass(enc, &targets.depth_view) else { return };
            if responsive_on && this.fx.has_responsive_aa_draws() { this.fx.draw_responsive_aa(&mut pass); }
        });
        graph.add_if(this.taa.active(), "temporal aa", move |enc| { this.taa.encode_resolve(enc, &targets.hdr, &targets.hdr_mip_views[0]); });
        graph.add("bloom", move |enc| { this.bloom.encode(enc); });
        for i in 1..targets.hdr_mip_views.len() {
            graph.add(format!("hdr mip {i}"), move |enc| {
                let ts = this.profiler.slot(&format!("hdr mip {i}"), "post");
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("hdr downsample"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &targets.hdr_mip_views[i], depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                    depth_stencil_attachment: None, timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
                });
                pass.set_pipeline(&this.downsample_pipeline);
                pass.set_bind_group(0, &targets.downsample_bgs[i - 1], &[]);
                pass.draw(0..3, 0..1);
            });
        }
        graph.add("exposure", move |enc| {
            let ts = this.profiler.slot("exposure", "post");
            // PERF: UE4.16 histogram split (PostProcessHistogram.cpp tiles + PostProcessHistogramReduce.cpp): many workgroups, then the one-workgroup reduce
            enc.clear_buffer(&this.meter_hist_buf, 0, None);
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("exposure"), timestamp_writes: this.profiler.compute_writes(ts) });
            pass.set_bind_group(0, &targets.meter_bg, &[]);
            pass.set_pipeline(&this.meter_hist_pipeline);
            let (hw, hh) = (targets.width.div_ceil(2), targets.height.div_ceil(2)); // the half-res histogram input (bounds-checked in the shader)
            pass.dispatch_workgroups(hw.div_ceil(64), hh.div_ceil(64), 1);
            pass.set_pipeline(&this.meter_pipeline);
            pass.dispatch_workgroups(1, 1, 1);
        });
        let ldr = targets.ldr.as_ref();
        let scope_fmt = if fxaa_any { LDR_FORMAT } else { final_fmt };
        // SCALABILITY: the last full-screen pass before the upscale writes the view-size LDR image instead of the window
        let final_target: &wgpu::TextureView = if upscaling { targets.up_src.as_ref().unwrap_or(target) } else { target };
        graph.add("post", move |enc| {
            let view = ldr.map_or(final_target, |l| &l.0);
            this.color_lut.dispatch(enc); // COLOR LUT: CombineLUTs from this frame's post_buf, before the tonemapper samples it
            let ts = this.profiler.slot("post", "post");
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("post"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(if ldr.is_some() || upscaling { &this.post_ldr_pipeline } else { &this.post_pipeline });
            pass.set_bind_group(0, &targets.post_bg, &[]);
            pass.draw(0..3, 0..1);
        });
        // SCOPE HOOK: the scoped weapon's mesh into the LDR image, then its post-process material (LDR image -> ldr2 when FXAA follows, else the target)
        graph.add_if(scope_on, "scope ring", move |enc| { if let Some(l) = ldr { this.scope.encode_ring(enc, &l.0, LDR_FORMAT); } });
        graph.add_if(scope_on, "scope post", move |enc| { let out = if fxaa_any { targets.ldr2.as_ref().map(|l| &l.0) } else { Some(final_target) }; if let Some(o) = out { this.scope.encode_post(enc, o, scope_fmt); } });
        if let Some(l) = if fxaa.is_some() { if scope_on { targets.ldr2.as_ref() } else { ldr } } else { None } {
            graph.add("fxaa", move |enc| {
                let ts = this.profiler.slot("fxaa", "post");
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fxaa"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                    depth_stencil_attachment: None, timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
                });
                pass.set_pipeline(&this.fxaa_pipeline);
                pass.set_bind_group(0, &l.1, &[]);
                pass.draw(0..3, 0..1);
            });
        }
        // SCALABILITY: Medium = FXAA 3.11 console (FXAA_PRESET 1), LDR image -> window / upscale source
        if let Some((pipe, bg)) = console_pass.as_ref() {
            graph.add("fxaa", move |enc| {
                let ts = this.profiler.slot("fxaa", "post");
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fxaa console"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: final_target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                    depth_stencil_attachment: None, timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
                });
                pass.set_pipeline(pipe);
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..3, 0..1);
            });
        }
        // SCALABILITY: r.ScreenPercentage upscale (r.Upscale.Quality method) to the window
        if let Some((pipe, bg)) = upscale_pass.as_ref() {
            graph.add("upscale", move |enc| {
                let ts = this.profiler.slot("upscale", "post");
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("upscale"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                    depth_stencil_attachment: None, timestamp_writes: this.profiler.render_writes(ts), occlusion_query_set: None, multiview_mask: None,
                });
                pass.set_pipeline(pipe);
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..3, 0..1);
            });
        }
        // SCALABILITY (temporal AA): this frame's object matrices / bones become the previous ones of the next frame's velocity pass
        graph.add_if(this.taa.active(), "velocity history", move |enc| {
            for id in models {
                let m = &this.models[id.0];
                enc.copy_buffer_to_buffer(&m.obj, 0, &m.prev_obj, 0, 64);
                enc.copy_buffer_to_buffer(&m.bones, 0, &m.prev_bones, 0, (m.bone_count * 64) as u64);
                m.prev_valid.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        let mut encoder = this.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        // PERF: `FN_GPU_NODES=1` brackets every graph node with encoder timestamps (group "node"), so untimed passes show up in the profiler too
        static GPU_NODES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let mut passes = if *GPU_NODES.get_or_init(|| fn_live::var("FN_GPU_NODES").map_or(false, |v| v == "1")) && this.profiler.inside_encoders() {
            let mut open: Option<u32> = None;
            graph.execute_with(&mut encoder, |enc, name, begin| {
                if begin { open = this.profiler.slot(&format!("node {name}"), "node"); if let Some(s) = open { this.profiler.encoder_marker(enc, s); } }
                else if let Some(s) = open.take() { this.profiler.encoder_marker(enc, s + 1); }
            })
        } else { graph.execute(&mut encoder) };
        pt = std::time::Instant::now();
        // PERF (2026-10-06, frame-time spikes): "prep finish" (CommandEncoder::finish), "prep submit" (Queue::submit: validation, barriers, vkQueueSubmit,
        // device maintain), "prep end frame" (the passes' end_frame)
        let cb = encoder.finish();
        mark!("finish");
        if !this.defer_submit(cb) { this.occlusion.after_submit(); } // OCCLUSION: map this frame's HZB readback (occlusion.rs)
        mark!("submit");
        self.df.end_frame(); // DISTANCE FIELD HOOK
        self.taa.end_frame(); // SCALABILITY
        self.ssr.end_frame(); // REFLECTION APPLY
        self.light_shafts.end_frame(); // LIGHT SHAFT TAA
        self.frame_index += 1;
        mark!("end frame"); let _ = pt; passes.extend(prep);
        // TEXTURE MEMORY (2026-10-06): `FN_GPU_MEM=1` (report-only) prints the device's GPU memory (gpu-allocator report: allocated / reserved) and the FX
        // textures per format at frame 60 and every 300 frames
        static GPU_MEM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *GPU_MEM.get_or_init(|| fn_live::var("FN_GPU_MEM").map_or(false, |v| v == "1")) && (self.frame_index == 60 || self.frame_index % 300 == 0) {
            let (alloc, reserved) = self.device.generate_allocator_report().map_or((0, 0), |r| (r.total_allocated_bytes, r.total_reserved_bytes));
            eprintln!("[gpu mem] frame {}: allocated {:.1} MB, reserved {:.1} MB | {} | {}", self.frame_index, alloc as f64 / 1048576., reserved as f64 / 1048576., self.fx.texture_report(), self.gun.texture_report());
        }
        FrameStats { cascades: if shadows_on { Some(cascades) } else { None }, passes, cpu_ms: t0.elapsed().as_secs_f32() * 1000. }
    }

    /// Blocks until the GPU is idle (benchmarking / screenshots).
    pub fn wait_idle(&self) { let _ = self.device.poll(crate::gpu::bounded_wait()); } // DEVICE LOST: bounded

    /// Reads the adapted exposure state back (synchronous; for tests and reports, not for the frame loop).
    pub fn read_exposure(&self) -> ExposureInfo {
        if cfg!(target_arch = "wasm32") { return ExposureInfo { ev: 0., target_ev: 0., exposure: 1., avg_luminance: 0. }; }
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("exposure readback"), size: 16, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(&self.exposure_buf, 0, &staging, 0, 16);
        self.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.device.poll(crate::gpu::bounded_wait()).expect("exposure read-back: GPU wait timed out (device lost or hung)"); // DEVICE LOST: bounded
        rx.recv().unwrap().expect("map");
        let data = slice.get_mapped_range().expect("mapped range");
        let f: &[f32] = bytemuck::cast_slice(&data);
        ExposureInfo { ev: f[0], target_ev: f[1], exposure: f[2], avg_luminance: f[3] }
    }
}

/// IEEE binary16 to f32 (HDR read-back).
pub fn half_to_f32(h: u16) -> f32 {
    let (s, e, m) = (((h >> 15) & 1) as u32, ((h >> 10) & 0x1F) as u32, (h & 0x3FF) as u32);
    let bits = if e == 0 {
        if m == 0 { s << 31 } else { let p = 31 - m.leading_zeros(); (s << 31) | ((p + 103) << 23) | ((m & !(1 << p)) << (23 - p)) }
    } else if e == 31 { (s << 31) | 0x7F80_0000 | (m << 13) } else { (s << 31) | ((e + 112) << 23) | (m << 13) };
    f32::from_bits(bits)
}

/// Convenience: the camera-space frustum for tests / viewers that want the sun in view space.
pub fn sun_dir() -> Vec3 { lighting::sun_direction() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gpu_blocks_are_vec4_aligned() {
        assert_eq!(std::mem::size_of::<LightUniform>() % 16, 0);
        assert_eq!(std::mem::size_of::<LightUniform>(), 3 * 16 + 3 * 64 + 7 * 16 + 7 * 16 + 4 * 16 + 16); // + TOD LIVE fog block + LIGHT FUNCTION world block
        assert_eq!(std::mem::size_of::<ObjectUniform>(), 64);
        assert_eq!(std::mem::size_of::<GroundVertex>(), 20);
    }
    #[test]
    fn ground_triangle_list_covers_plate_and_ring() {
        let t = ground_triangles(GROUND_HALF, GROUND_OUTER);
        assert_eq!(t.len(), 5 * 6);
        // UV repeat: 2 m per tile across the whole plate (256 tiles)
        let max_uv = t.iter().take(6).map(|v| v.1[0]).fold(0f32, f32::max);
        assert!((max_uv - 256.).abs() < 1e-3);
        assert!(t.iter().all(|v| v.0[1] == 0.));
    }
    #[test]
    fn wgsl_layouts_match_rust_blocks() {
        // the WGSL struct member order is the contract for the uniform uploads
        let s = fn_live::wgsl!("../shaders/lit_common.wgsl");
        let order = ["view_proj", "view:", "view_inverse", "position", "viewport", "proj:"];
        let mut at = s.find("struct Camera").unwrap();
        for m in order { let p = s[at..].find(m).unwrap_or_else(|| panic!("{m} missing")); at += p; }
        let l = s.find("struct Light").unwrap();
        for m in ["sun_dir", "sun_radiance", "sky_radiance", "shadow_matrix", "cascade_far", "cascade_texel", "shadow_params", "misc"] { assert!(s[l..].contains(m)); }
    }
}
