//! The FX pass: translucent / unlit / vertex-animated (world position offset) / masked material draws that follow the SCENE pass and precede the HDR mip chain, driven by
//! shaders the application hands over as WGSL (the original materials' DXBC translated by `fn-dxbc`, assembled by `fn-fx`). The pass knows nothing about Unreal:
//! it owns GPU textures / samplers / uniform buffers / meshes / pipelines created from plain descriptions and draws the [`FxFrame`] the application submits for the
//! next frame with [`crate::Renderer::set_fx_frame`].
//!
//! Two render passes are recorded when the frame has draws of the kind:
//! * `fx opaque`      colour + the scene depth buffer writable (masked materials: the original base-pass `BLEND_Masked` draws);
//! * `fx translucent` colour + the scene depth buffer READ-ONLY (translucent / modulate draws; their pixel shaders may sample the scene depth, [`FxRes::SceneDepth`]).
//!
//! Blend state, depth test / write, culling and the topology come from the material flags the caller read from the package (see [`FxBlend`], [`FxDepth`], [`FxCull`]).
use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct FxTexId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct FxBufId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct FxMeshId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct FxPipeId(pub usize);
/// Which of a program's pipelines is being created (BROWSER PORT: async pipelines, fx_async.rs; natively unused).
#[cfg(target_arch = "wasm32")]
use fx_async::Slot as FxSlot;
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)] enum FxSlot { Main, Depth, Velocity }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct FxBindId(pub usize);
/// STORM TAA: fragment entry appended to a responsive-AA program without a discard (its mask twin, [`FxRenderer::create_pipeline`]).
const RESPONSIVE_AA_FS: &str = "fs_responsive_aa";

#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum FxTexDim { D2, D3, Cube }
/// One mip level of a texture (RGBA8; cube: the six faces concatenated, +X -X +Y -Y +Z -Z; 3D: depth slices concatenated). For a block-compressed
/// [`FxTexFormat`] `rgba8` holds the level's cooked 4x4 blocks as they are (`ceil(width / 4) * ceil(height / 4)` blocks, also for the 2x2 / 1x1 levels).
#[derive(Clone, Debug)] pub struct FxLevel { pub width: u32, pub height: u32, pub depth: u32, pub rgba8: Vec<u8> }
/// A texture with the mip chain the package stores (nothing is generated).
#[derive(Clone, Debug)]
pub struct FxTextureDesc { pub label: String, pub dim: FxTexDim, pub srgb: bool, pub levels: Vec<FxLevel>, pub format: FxTexFormat }

/// TEXTURE MEMORY (2026-10-06, BC passthrough): the GPU format of an [`FxTextureDesc`]. `Rgba8` = RGBA8 pixels; the BC formats are the cooked `PF_DXT1` /
/// `PF_DXT3` / `PF_DXT5` / `PF_BC4` / `PF_BC5` / `PF_BC7` blocks uploaded as they are (4-8x less memory than RGBA8, no CPU decode). They need
/// `Features::TEXTURE_COMPRESSION_BC`, a 2D texture and a level-0 size that is a multiple of 4 (wgpu validation on every backend, WebGPU included):
/// [`FxRenderer::accepts_bc`]. The producer (fn-fx `TextureCache::load`) decodes the blocks to RGBA8 on the CPU when one of those does not hold;
/// [`FxRenderer::create_texture`] turns blocks it cannot take into a 1x1 black texture and counts them (never expected).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum FxTexFormat { #[default] Rgba8, Bc1, Bc2, Bc3, Bc4, Bc5, Bc7 }
impl FxTexFormat {
    pub fn is_bc(self) -> bool { self != FxTexFormat::Rgba8 }
    /// Bytes of one 4x4 block (BC) or one pixel (RGBA8).
    pub fn unit_bytes(self) -> usize { match self { FxTexFormat::Rgba8 => 4, FxTexFormat::Bc1 | FxTexFormat::Bc4 => 8, _ => 16 } }
    /// Bytes of one layer of a `w` x `h` level.
    pub fn level_bytes(self, w: u32, h: u32) -> usize {
        if self.is_bc() { (w.div_ceil(4) * h.div_ceil(4)) as usize * self.unit_bytes() } else { (w * h) as usize * 4 }
    }
    /// The wgpu format. sRGB: UE4.16's D3D11 RHI maps PF_DXT1 / PF_DXT3 / PF_DXT5 / PF_BC7 to TYPELESS platform formats whose shader-resource view is the
    /// `_UNORM_SRGB` one for an sRGB texture, but PF_BC4 / PF_BC5 to `DXGI_FORMAT_BC4_UNORM` / `BC5_UNORM` (D3D11Device.cpp:118-175), which
    /// `FindShaderResourceDXGIFormat` returns unchanged (D3D11RHIPrivate.h:912-949): BC4 / BC5 always sample linear, whatever the texture's SRGB flag.
    /// D3D samples BC4 as (r, 0, 0, 1) and BC5 as (r, g, 0, 1); so do these formats (the materials rebuild a normal's Z themselves).
    pub fn wgpu(self, srgb: bool) -> wgpu::TextureFormat {
        use wgpu::TextureFormat as T;
        match (self, srgb) {
            (FxTexFormat::Rgba8, false) => T::Rgba8Unorm, (FxTexFormat::Rgba8, true) => T::Rgba8UnormSrgb,
            (FxTexFormat::Bc1, false) => T::Bc1RgbaUnorm, (FxTexFormat::Bc1, true) => T::Bc1RgbaUnormSrgb,
            (FxTexFormat::Bc2, false) => T::Bc2RgbaUnorm, (FxTexFormat::Bc2, true) => T::Bc2RgbaUnormSrgb,
            (FxTexFormat::Bc3, false) => T::Bc3RgbaUnorm, (FxTexFormat::Bc3, true) => T::Bc3RgbaUnormSrgb,
            (FxTexFormat::Bc7, false) => T::Bc7RgbaUnorm, (FxTexFormat::Bc7, true) => T::Bc7RgbaUnormSrgb,
            (FxTexFormat::Bc4, _) => T::Bc4RUnorm,
            (FxTexFormat::Bc5, _) => T::Bc5RgUnorm,
        }
    }
}
/// TEXTURE MEMORY: whether the device takes BC blocks (`Features::TEXTURE_COMPRESSION_BC` and not `FN_DEBUG_NO_BC=1`), for texture decodes that run before /
/// away from the renderer (the fn-fx predecode workers start during the loading screen, before the renderer exists). Set when the device is created
/// ([`set_bc_native_hint`], gpu.rs) and again by [`FxRenderer::new`]. A hint only: fn-fx decodes blocks the renderer does not take
/// ([`FxRenderer::accepts_bc`]).
static BC_NATIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn bc_native_hint() -> bool { BC_NATIVE.load(std::sync::atomic::Ordering::Relaxed) }
/// `device_has_bc` = the device's `Features::TEXTURE_COMPRESSION_BC`; returns the BC passthrough state (off with `FN_DEBUG_NO_BC=1`, debug-only A/B switch).
pub fn set_bc_native_hint(device_has_bc: bool) -> bool {
    let on = device_has_bc && fn_live::var("FN_DEBUG_NO_BC").map_or(true, |v| v != "1");
    BC_NATIVE.store(on, std::sync::atomic::Ordering::Relaxed);
    on
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum FxFilter { Nearest, Linear }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum FxAddress { Wrap, Clamp, Mirror }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FxSamplerDesc { pub mag: FxFilter, pub min: FxFilter, pub mip: FxFilter, pub address: FxAddress, pub aniso: u16 }
impl FxSamplerDesc {
    pub const LINEAR_WRAP: FxSamplerDesc = FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: FxFilter::Linear, address: FxAddress::Wrap, aniso: 1 };
    pub const LINEAR_CLAMP: FxSamplerDesc = FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: FxFilter::Linear, address: FxAddress::Clamp, aniso: 1 };
    pub const NEAREST_CLAMP: FxSamplerDesc = FxSamplerDesc { mag: FxFilter::Nearest, min: FxFilter::Nearest, mip: FxFilter::Nearest, address: FxAddress::Clamp, aniso: 1 };
    fn non_filtering(&self) -> bool { self.mag == FxFilter::Nearest && self.min == FxFilter::Nearest && self.mip == FxFilter::Nearest }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxFmt { Float32x2, Float32x3, Float32x4, Unorm8x4, Unorm8x4Bgra, Float16x2, Float16x4, Sint32x4, Uint32x4, Uint8x4, /** INSTANCING: the per-instance id (fn-app statics_render.rs) */ Uint32, /// GRASS: `VET_Short4N` (the instance stream's `InstanceLightmapAndShadowMapUVBias`).
    Snorm16x4 }
impl FxFmt { fn wgpu(self) -> wgpu::VertexFormat { match self { FxFmt::Float32x2 => wgpu::VertexFormat::Float32x2, FxFmt::Float32x3 => wgpu::VertexFormat::Float32x3, FxFmt::Float32x4 => wgpu::VertexFormat::Float32x4, FxFmt::Unorm8x4 => wgpu::VertexFormat::Unorm8x4, FxFmt::Unorm8x4Bgra => wgpu::VertexFormat::Unorm8x4Bgra, FxFmt::Float16x2 => wgpu::VertexFormat::Float16x2, FxFmt::Float16x4 => wgpu::VertexFormat::Float16x4, FxFmt::Sint32x4 => wgpu::VertexFormat::Sint32x4, FxFmt::Uint32x4 => wgpu::VertexFormat::Uint32x4, FxFmt::Uint8x4 => wgpu::VertexFormat::Uint8x4, FxFmt::Uint32 => wgpu::VertexFormat::Uint32, FxFmt::Snorm16x4 => wgpu::VertexFormat::Snorm16x4 } } }
#[derive(Clone, Copy, Debug)] pub struct FxAttr { pub location: u32, pub offset: u32, pub format: FxFmt }
#[derive(Clone, Debug)] pub struct FxVertexBuffer { pub stride: u32, pub instance: bool, pub attrs: Vec<FxAttr> }

/// A mesh: vertex bytes (one interleaved stream described by the pipeline's first vertex buffer) + u16 / u32 indices.
#[derive(Clone, Debug)] pub struct FxMeshDesc { pub label: String, pub vertices: Vec<u8>, pub indices: Vec<u32> }

/// Blend modes of the material's serialized `BlendMode` (UE 4.16 base-pass render states).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxBlend {
    /// `BLEND_Opaque` / `BLEND_Masked`: no blending.
    Opaque,
    /// `BLEND_Translucent`: colour `SrcAlpha, InvSrcAlpha`, alpha `Zero, InvSrcAlpha`.
    Translucent,
    /// `BLEND_Additive`: colour `One, One`, alpha `Zero, One`.
    Additive,
    /// `BLEND_Modulate`: colour `DestColor, Zero`, alpha `Zero, One`.
    Modulate,
}
/// Depth state: `test` = false is `bDisableDepthTest` (compare Always); `write` only exists in the opaque pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct FxDepth { pub test: bool, pub write: bool }
/// Face culling, named by the FRONT face winding in framebuffer space (unlike `gun_material::GunCull`, named by the UE cull mode): `BackCcw` = front faces
/// counter-clockwise, clockwise culled = UE `CM_CW` (one-sided default: UE4.16 DrawingPolicy.cpp:45, D3D11State.cpp:41 `CM_CW -> D3D11_CULL_BACK` with
/// :200 `FrontCounterClockwise = true`); `BackCw` = the reverse (`CM_CCW`, a mirrored / ReverseCulling draw, StaticMeshRender.cpp:284).
/// MIRRORFIX: D3D11 keeps `FrontCounterClockwise = true` and switches the cull mode to `D3D11_CULL_FRONT` for `CM_CCW` (D3D11State.cpp:41), so `SV_IsFrontFace`
/// stays "counter-clockwise" on a mirrored draw (the material undoes it with `Primitive.LocalToWorldDeterminantSign`, MaterialTemplate.usf:1841). `BackCw` is
/// therefore built as front face CCW + cull FRONT, not front face CW + cull back (same culled set, UE's facing bit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum FxCull { None, BackCw, BackCcw }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxBindTy {
    Uniform,
    Tex2D { filterable: bool },
    Tex3D { filterable: bool },
    Cube { filterable: bool },
    /// The scene depth (Depth32Float) read as an unfilterable `texture_2d<f32>`.
    SceneDepth,
    Sampler { filtering: bool },
    /// A read-only storage buffer (the vertex / pixel shader's `Buffer<float4>` SRV).
    StorageRead,
}
#[derive(Clone, Copy, Debug)] pub struct FxLayoutEntry { pub binding: u32, pub ty: FxBindTy, pub vertex: bool, pub fragment: bool }

#[derive(Clone, Debug)]
pub struct FxPipelineDesc {
    pub label: String,
    pub wgsl: String,
    pub vs_entry: String,
    pub fs_entry: String,
    pub vertex_buffers: Vec<FxVertexBuffer>,
    /// Bind group layouts by group index.
    pub groups: Vec<Vec<FxLayoutEntry>>,
    pub blend: FxBlend,
    pub depth: FxDepth,
    pub cull: FxCull,
    /// Drawn in the `fx translucent` pass (depth read-only, may sample the scene depth) instead of `fx opaque`.
    pub translucent: bool,
    /// Translucent material with `bEnableSeparateTranslucency` (UMaterial default true, Material.cpp:739; `FMaterialResource::IsSeparateTranslucencyEnabled`,
    /// MaterialShared.cpp:902): drawn in the separate translucency pass ([`FxRenderer::draw_separate`]) and composited after the standard translucency
    /// (TranslucentRendering.cpp:889 `PlaceScenePrimitive`, r.SeparateTranslucency 1).
    pub separate_translucency: bool,
    /// STORM TAA (2026-10-07): translucent material with `bEnableResponsiveAA`: its draws also tag their pixels in the TAA's responsive mask
    /// ([`FxRenderer::draw_responsive_aa`]), UE's `STENCIL_TEMPORAL_RESPONSIVE_AA_MASK` write (TranslucentRendering.cpp:531-557).
    pub responsive_aa: bool,
}

/// What a bind group entry points at.
#[derive(Clone, Copy, Debug)]
pub enum FxRes {
    Buffer(FxBufId), Texture(FxTexId), Sampler(FxSamplerDesc), SceneDepth,
    /// TRANSLUCENCY VOLUME (MIRRORFIX 2026-10-05): one of this frame's translucency lighting volume textures, [`TLV_AMBIENT_INNER`] ..
    /// [`TLV_DIRECTIONAL_OUTER`] (`FTranslucentLightingVolumeParameters`, UE4.16 BasePassRendering.h:227; filled by `crate::translucent_lighting`). Resolved
    /// when the bind group is built, like [`FxRes::SceneDepth`]; black (the cleared volume) until the renderer has produced one.
    TranslucencyVolume(u8),
    /// FOG INPUTS (2026-10-05): `FogInscatteringColorCubemap` of `FExponentialHeightFogShaderParameters::Set` with no inscattering cubemap = `GWhiteTextureCube`
    /// (FogRendering.h:35), a 1x1 white cube.
    FogCube,
    /// FOG INPUTS: `IntegratedLightScattering` (VolumetricFog.h:51-58): this frame's integrated light-scattering volume of the volumetric fog
    /// ([`FxRenderer::set_view_fog`], `crate::volfog`), `GBlackVolumeTexture` (the black 1x1x1 volume) while the volumetric fog is off.
    FogVolume,
}
/// FOG INPUTS: number of loose height-fog values (`crate::fog::shader_constants` order).
pub const FX_FOG_FIELDS: usize = 9;
/// FOG INPUTS: one bound loose height-fog value in a `$Globals` buffer: `field` = index into [`FxViewFog::values`], at `byte_offset` (`FShaderParameter::BaseIndex`),
/// `num_bytes` long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FxFogSlot { pub field: u8, pub byte_offset: u32, pub num_bytes: u32 }
/// FOG INPUTS: `View` float4 slots of the volumetric fog (`VolumetricFogInvGridSize`, `VolumetricFogGridZParams`, `VolumetricFogSVPosToVolumeUV` +
/// `VolumetricFogMaxDistance`; the VS reads [175].z, [176].xyz, [177].xyz), filled like `SetupVolumetricFogUniformBufferParameters` (VolumetricFog.cpp:841-863).
pub const VIEW_VOLUMETRIC_FOG_SLOT: usize = 175;
/// FOG INPUTS: what the renderer hands the FX pass every frame: the loose height-fog values (`crate::fog::shader_constants`) and the three `View` slots.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FxViewFog { pub values: [[f32; 4]; FX_FOG_FIELDS], pub view: [[f32; 4]; 3] }
/// [`FxRes::TranslucencyVolume`] indices, in `FTranslucentLightingVolumeParameters` order (`GetTranslucencyVolumeAmbient(TVC_Inner)`, ...).
pub const TLV_AMBIENT_INNER: u8 = 0;
pub const TLV_AMBIENT_OUTER: u8 = 1;
pub const TLV_DIRECTIONAL_INNER: u8 = 2;
pub const TLV_DIRECTIONAL_OUTER: u8 = 3;
#[derive(Clone, Copy, Debug)] pub struct FxBindEntry { pub binding: u32, pub res: FxRes }

#[derive(Clone, Debug)]
pub enum FxGeom {
    Mesh(FxMeshId),
    /// A range of the mesh's index buffer (one section of a multi-material mesh).
    MeshRange { mesh: FxMeshId, first_index: u32, index_count: u32 },
    /// Per-frame vertices (e.g. camera facing sprites) + u32 indices, uploaded with the frame.
    Dynamic { vertices: Vec<u8>, indices: Vec<u32> },
}
/// Per-instance vertex data for vertex buffer slot 1 (the pipeline's second vertex buffer, `instance: true`).
#[derive(Clone, Debug)] pub struct FxInstances { pub bytes: Vec<u8>, pub count: u32 }

#[derive(Clone, Debug)]
pub struct FxDraw {
    pub pipe: FxPipeId,
    /// (group index, bind group); every group of the pipeline layout must be present.
    pub binds: Vec<(u32, FxBindId)>,
    pub geom: FxGeom,
    pub instances: Option<FxInstances>,
    /// The translucency sort data of the draw's primitive (required: a draw cannot be built without it, see [`FxSort`]).
    pub sort: FxSort,
}
/// Everything the FX pass does in one frame.
#[derive(Clone, Debug, Default)]
pub struct FxFrame {
    /// Uniform buffer contents written before the passes (whole buffer from offset 0).
    pub buffer_writes: Vec<(FxBufId, Vec<u8>)>,
    /// Draws in submission order (opaque pass); the translucent passes order them by [`FxDraw::sort`].
    pub draws: Vec<FxDraw>,
    /// FX SHADOW (2026-10-05): the shadow-depth draws of the frame's opaque / masked shadow casters, recorded into every CSM cascade ([`FxRenderer::draw_shadow`]).
    pub shadow_draws: Vec<FxShadowDraw>,
    /// FX SHADOW: the distance-field objects of the frame's FX static meshes (`FStaticMeshSceneProxy` with a cooked distance field and
    /// `bAffectDistanceFieldLighting`), added to the renderer's distance-field scene (DF shadows + DFAO) next to the level's statics.
    pub distance_field_objects: Vec<FxDfInstance>,
}

/// FX SHADOW: one draw of a pipeline built with [`FxRenderer::create_shadow_pipeline`] (the material's cooked `TShadowDepthVS` / `TShadowDepthPS`) into the sun's
/// cascades: `FShadowDepthDrawingPolicy` of a whole-scene directional shadow (UE4.16 ShadowDepthRendering.cpp:667). Group 3 (the cascade's LightViewProj) is set by the pass.
#[derive(Clone, Debug)]
pub struct FxShadowDraw { pub pipe: FxPipeId, pub binds: Vec<(u32, FxBindId)>, pub geom: FxGeom,
    /// PERF (2026-10-06): the caster's world bounding sphere (renderer world, metres: centre, radius). With it a cascade only draws the caster when the sphere
    /// meets the cascade's light-space prism and it survives the small-caster cull, as the renderer's own static meshes do (statics.rs prepare: UE4.16
    /// FProjectedShadowInfo::AddSubjectPrimitive + ShadowSetup.cpp:2454). `None` = drawn into every cascade.
    pub bounds: Option<[f32; 4]>,
    /// INSTANCING (2026-10-07): one draw for many placements of the same caster (the statics path, fn-app statics_render.rs): per instance its id (vertex
    /// buffer slot 1, `u32`) and its world bounding sphere (as [`Self::bounds`]); each cascade draws only the instances that pass that cascade's test
    /// ([`FxRenderer::set_shadow_cull`]), so the casters of a cascade are exactly those of one draw per placement. `None` = one instance, no slot 1.
    pub instances: Option<FxShadowInstances> }
/// INSTANCING: see [`FxShadowDraw::instances`].
#[derive(Clone, Debug, Default)]
pub struct FxShadowInstances { pub ids: Vec<u32>, pub bounds: Vec<[f32; 4]> }
/// FX SHADOW: a placed FX static mesh in the distance-field scene: its cooked distance field (one `Arc` per mesh: the atlas holds it once) and its
/// `LocalToWorld` (UE cm = the renderer's DF space, `distance_field::to_df`).
#[derive(Clone, Debug)]
pub struct FxDfInstance { pub mesh: std::sync::Arc<crate::distance_field::MeshDistanceField>, pub local_to_world: glam::Mat4 }

/// PERF: the per-cascade caster test of one world bounding sphere `b` (renderer world, metres): the small-caster cull, then the cascade's side planes and
/// the UE4.16 caster test (ShadowSetup.cpp:2416-2438, lighting::Cascade::caster_in_bounds). `cull` / `small` = [`FxRenderer::set_shadow_cull`]'s.
fn sphere_in_cascade_of(cull: &[([glam::Vec4; 6], crate::lighting::Cascade)], small: Option<(glam::Vec3, f32, f32)>, b: [f32; 4], cascade: usize) -> bool {
    let (c, r) = (glam::Vec3::new(b[0], b[1], b[2]), b[3]);
    if let Some((eye, threshold, lod)) = small { if crate::quality::shadow_caster_screen_size_culled(r, (c - eye).length_squared(), threshold, lod) { return false; } }
    cull.get(cascade).map_or(true, |(p, k)| crate::static_mesh::sphere_in_planes(p, 4, c, r) && k.caster_in_bounds(c, r))
}
/// FX SHADOW: `FN_FX_SHADOWS=0` turns the FX shadow-depth draws off (debug / cost comparison only).
fn fx_shadows_enabled() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_FX_SHADOWS").map_or(true, |v| v != "0")) }
/// FX SHADOW: bind group of a shadow pipeline that holds the cascade's LightViewProj (the renderer's shadow vp buffer, shadow.wgsl layout: mat4 + depth bias).
pub const FX_SHADOW_GROUP: u32 = 3;
/// FX SHADOW: WGSL the vertex entry of a shadow pipeline applies to the cooked program's `SV_Position`. The program's `ProjectionMatrix` maps the translated UE
/// world (cm) to the PORT world (m, w = 1), its `ShadowParams` = (0, 1) and `bClampToNearPlane` = 0 (fn-fx `MaterialGpu::shadow_globals_writes`), so its output
/// is the world position; this finishes `SetShadowDepthOutputs` (ShadowDepthVertexShader.usf:20) with the cascade: `OutPosition = mul(WorldPosition, ProjectionMatrix)`,
/// the whole-scene directional near-plane clamp (ShadowRendering.h:1100) and `ShadowDepth = z * InvMaxSubjectDepth + DepthBias` (the cascade matrix already
/// normalises the depth) -- the same maths as gun_resolve.wgsl `gun_shadow_depth_outputs` and shadow.wgsl.
pub const FX_SHADOW_WGSL: &str = "
struct FxShadowVp { m: mat4x4<f32>, depth_bias: vec4<f32> }
@group(3) @binding(0) var<uniform> fx_shadow_vp: FxShadowVp;
fn fx_shadow_depth_outputs(world_m: vec4<f32>) -> vec4<f32> {
    var p = fx_shadow_vp.m * vec4<f32>(world_m.xyz / world_m.w, 1.0);
    if (p.z < 0.0) { p.z = 0.000001; p.w = 1.0; }
    p.z = (p.z + fx_shadow_vp.depth_bias.x) * p.w;
    return p;
}
";

/// `r.TranslucentSortPolicy` (UE4.16 RendererSettings.h:255, `ETranslucentSortPolicy`). The 1.8 cooked RendererSettings set it to 0 = `SortByDistance`
/// (`CookedIniVersion.txt`: `r.TranslucentSortPolicy:0=0`, `TranslucentSortAxis:0=(X=0,Y=-1,Z=0)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TranslucentSortPolicy { SortByDistance, SortByProjectedZ, SortAlongAxis([f32; 3]) }
/// The policy of the 1.8 cooked config (see [`TranslucentSortPolicy`]).
pub const TRANSLUCENT_SORT_POLICY: TranslucentSortPolicy = TranslucentSortPolicy::SortByDistance;

/// What `FTranslucentPrimSet` sorts a translucent primitive by (UE4.16 SceneRendering.h:302-331 / :395): the pass, then `SortPriority` =
/// `Proxy->GetTranslucencySortPriority()` (`UPrimitiveComponent::TranslucencySortPriority`, default 0) ascending, then `SortKey` descending (back to front).
/// There is no default: every FX / particle draw names the bounds origin of its primitive ([`FxSort::new`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxSort { pub priority: i16, pub key: f32 }
impl FxSort {
    /// `CalculateTranslucentSortKey` (UE4.16 TranslucentRendering.cpp:858) of a primitive whose `Proxy->GetBounds().Origin` is `bounds_origin` for a view at
    /// `view_origin` looking along `view_forward` (all UE world cm / unit vector) under [`TRANSLUCENT_SORT_POLICY`]: `SortByDistance` = `(Origin - ViewOrigin).Size()`
    /// (the true length, not squared), `SortAlongAxis` = `Dot(Origin - ViewOrigin, TranslucentSortAxis)`, `SortByProjectedZ` = the view-space Z of the origin.
    pub fn new(bounds_origin: [f32; 3], view_origin: [f32; 3], view_forward: [f32; 3], priority: i16) -> FxSort {
        let d = [bounds_origin[0] - view_origin[0], bounds_origin[1] - view_origin[1], bounds_origin[2] - view_origin[2]];
        let dot = |a: [f32; 3]| d[0] * a[0] + d[1] * a[1] + d[2] * a[2];
        let key = match TRANSLUCENT_SORT_POLICY {
            TranslucentSortPolicy::SortByDistance => dot(d).sqrt(),
            TranslucentSortPolicy::SortAlongAxis(axis) => dot(axis),
            // ViewMatrix.TransformPosition(Origin).Z: the UE view matrix's Z row is the view direction through the view origin
            TranslucentSortPolicy::SortByProjectedZ => dot(view_forward),
        };
        FxSort { priority, key }
    }
}
/// `FCompareFTranslucentSortedPrim` (SceneRendering.h:395) for one pass: lower priority first, then the larger key first.
pub fn sort_order(a: &FxSort, b: &FxSort) -> std::cmp::Ordering { a.priority.cmp(&b.priority).then(b.key.partial_cmp(&a.key).unwrap_or(std::cmp::Ordering::Equal)) }

impl FxFrame {
    /// Adds a draw (its sort data travels in [`FxDraw::sort`]).
    pub fn push(&mut self, draw: FxDraw) { self.draws.push(draw); }
    /// Moves `other`'s writes and draws after this frame's.
    pub fn append(&mut self, other: &mut FxFrame) { self.buffer_writes.append(&mut other.buffer_writes); self.draws.append(&mut other.draws); self.shadow_draws.append(&mut other.shadow_draws); self.distance_field_objects.append(&mut other.distance_field_objects); }
}

/// One entry of a merged translucency pass: an FX draw or a particle batch of the particle pass (both are primitives of the same `FTranslucentPrimSet`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TItem { Fx(usize), Particle(usize) }

struct GpuTex { tex: wgpu::Texture, view: wgpu::TextureView, dim: FxTexDim, layer: Option<(usize, u32)> }
type PoolKey = (wgpu::TextureFormat, u32, u32, u32);
struct TexPool { tex: Option<wgpu::Texture>, free: Vec<u32>, live: u32, layers: u32, bytes: u64 }
#[derive(Default)]
struct TexPools { on: bool, pools: Vec<TexPool>, by_key: HashMap<PoolKey, Vec<usize>>, loose: HashMap<PoolKey, Vec<usize>>, freeing: Vec<(usize, u32)> }
const POOL_MAX_BYTES: u64 = 4 << 20;
fn pools_enabled() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_FX_TEX_POOLS").is_ok_and(|v| v == "1")) }
fn dawn_slot(s: u64) -> u64 { let a = s.max(1).div_ceil(65536) * 65536; if a <= POOL_MAX_BYTES { a.next_power_of_two() } else { a } }
fn mip_chain_bytes(format: wgpu::TextureFormat, w: u32, h: u32, levels: u32) -> u64 {
    let (bw, bh) = format.block_dimensions();
    let bs = format.block_copy_size(None).unwrap_or(4) as u64;
    (0..levels).map(|m| (w >> m).max(1).div_ceil(bw) as u64 * (h >> m).max(1).div_ceil(bh) as u64 * bs).sum()
}
fn pool_capacity(s: u64, max_layers: u32) -> Option<u32> {
    if s == 0 || s > POOL_MAX_BYTES || dawn_slot(s) * 100 <= s * 112 { return None; }
    let mut c = 2u64;
    while c < max_layers as u64 && dawn_slot(c * s) * 100 > c * s * 112 { c += 1; }
    (c >= 2 && c <= max_layers as u64).then_some(c as u32)
}
fn layer_view(tex: &wgpu::Texture, layer: u32) -> wgpu::TextureView {
    tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2), base_array_layer: layer, array_layer_count: Some(1), ..Default::default() })
}
struct GpuMesh { vb: wgpu::Buffer, ib: wgpu::Buffer, index_count: u32, vr: (u64, u64), ir: (u64, u64) }
impl GpuMesh {
    fn vbs(&self) -> wgpu::BufferSlice<'_> { self.vb.slice(self.vr.0..self.vr.0 + self.vr.1) }
    fn ibs(&self) -> wgpu::BufferSlice<'_> { self.ib.slice(self.ir.0..self.ir.0 + self.ir.1) }
}
struct GpuPipe { pipe: wgpu::RenderPipeline, layouts: Vec<wgpu::BindGroupLayout>, desc_groups: Vec<Vec<FxLayoutEntry>>, translucent: bool, instanced: bool, separate: bool, /** FX SHADOW: a depth-only cascade pipeline */ shadow: bool,
    /** FX RECEIVE: an opaque / masked lit pipeline drawn in the renderer's scene pass with the globals at [`FX_SCENE_GROUP`] ([`FxRenderer::create_scene_lit_pipeline`]) */ scene_lit: bool }

/// FX RECEIVE (2026-10-05): bind group index of the renderer's scene globals (camera, light, cascades, distance fields, sky reflection: lit_common.wgsl group 0)
/// in a scene-lit FX pipeline; the translated DXBC resources own groups 0..2 (fn-fx `program::rebind`).
pub const FX_SCENE_GROUP: u32 = 3;
/// FX RECEIVE: what [`FxRenderer::set_scene_lighting`] hands over: the globals layout and the WGSL prelude of the scene-lit FX programs.
struct SceneLit { bgl_globals: wgpu::BindGroupLayout, prelude: String }
/// FX RECEIVE: the WGSL prelude of a scene-lit FX program: `common`, `lit_common` with its globals moved from group 0 to [`FX_SCENE_GROUP`] (the DXBC
/// resources own 0..2; whole-token replace of the group index), then fx_resolve.wgsl (`fx_scene_lit`).
pub fn scene_lit_prelude_wgsl(common: &str, lit_common: &str) -> String {
    let lit = lit_common.replace("@group(0)", &format!("@group({FX_SCENE_GROUP})"));
    // SHADING MODELS (2026-10-05): + the per-shading-model terms (shading_models.wgsl) and `fx_scene_lit_sm` (fx_resolve_sm.wgsl), used by the tail of a program
    // that writes GBufferD (fn-fx program.rs)
    [common, &lit, fn_live::wgsl!("../shaders/fx_resolve.wgsl"), fn_live::wgsl!("../shaders/shading_models.wgsl"), fn_live::wgsl!("../shaders/fx_resolve_sm.wgsl")].join("\n")
}
/// FX RECEIVE: [`scene_lit_prelude_wgsl`] of the renderer's own common.wgsl / lit_common.wgsl (tests, tools).
pub fn default_scene_lit_prelude() -> String { scene_lit_prelude_wgsl(fn_live::wgsl!("../shaders/common.wgsl"), fn_live::wgsl!("../shaders/lit_common.wgsl")) }

/// The separate translucency target (`FSceneRenderTargets::GetSeparateTranslucency`: scene-colour format, cleared to `FLinearColor::Black` = (0, 0, 0, 1)) and the
/// recombine (PostProcessBokehDOF.usf:456 `OutColor.rgb = OutColor.rgb * SeparateTranslucency.a + SeparateTranslucency.rgb`, done with the blend unit: src One, dst SrcAlpha).
struct SeparateTarget { view: wgpu::TextureView, size: (u32, u32), bind: wgpu::BindGroup }
const COMPOSITE_WGSL: &str = "
@group(0) @binding(0) var sep: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> { let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u)); return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0); }
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> { return textureLoad(sep, vec2<i32>(pos.xy), 0); }
";
/// `built` = (scene depth generation, translucency volume generation) it was built with + the group; rebuilt when a resource it uses changed.
#[derive(Default)]
struct TexResidency { frame: u32, bind_used: Vec<u32>, state: Vec<u8>, wanted: Vec<usize>, evicted: usize, evicted_bytes: u64, restored: usize, pressured: usize, pressure_bytes: u64, full_bytes: u64 }
static FRAME_UPLOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FLUSH_UPLOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const UPLOAD_FLUSH_BYTES: u64 = 48 << 20;
pub fn frame_upload_bytes() -> u64 { FRAME_UPLOAD.load(std::sync::atomic::Ordering::Relaxed) }
fn note_upload(queue: &wgpu::Queue, bytes: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    FRAME_UPLOAD.fetch_add(bytes, Relaxed);
    if FLUSH_UPLOAD.fetch_add(bytes, Relaxed) + bytes >= UPLOAD_FLUSH_BYTES { FLUSH_UPLOAD.store(0, Relaxed); queue.submit(None); }
}
const RES_SCAN_FRAMES: u32 = 60;
const RES_AGE_FRAMES: u32 = 1200;
const RES_MIN_BYTES: u64 = 256 << 10;
const RES_KEEP_PX: u32 = 64;
const RES_PRESSURE_IDLE_FRAMES: u32 = 240;
const RES_PRESSURE_MAX: usize = 48;
const RES_SHRINK_PER_SCAN: usize = 6;
const RES_LOG_FRAMES: u32 = 1200;
fn res_budget_bytes() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| fn_live::var("FN_TEX_RES_BUDGET_MB").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(if cfg!(target_arch = "wasm32") { 512 } else { 0 }) << 20)
}

struct BindGroupRec { pipe: FxPipeId, group: u32, entries: Vec<FxBindEntry>, built: Option<((u64, u64), wgpu::BindGroup)>, uses_depth: bool, uses_volume: bool }

/// BROWSER PORT (wasm32): FX pipelines through createRenderPipelineAsync (fx_async.rs).
#[cfg(target_arch = "wasm32")]
#[path = "fx_async.rs"]
mod fx_async;
#[path = "fx_pso.rs"]
mod fx_pso;
pub fn pso_prefetch() { fx_pso::prefetch() }
pub use fx_pso::{pso_recording, pso_recorded};

pub struct FxRenderer {
    instance: u64,
    device: wgpu::Device,
    queue: wgpu::Queue,
    hdr_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    textures: Vec<GpuTex>,
    buffers: Vec<FxBuf>,
    buf_slab: Option<(wgpu::Buffer, u64)>,
    mesh_slab: Option<(wgpu::Buffer, u64)>,
    meshes: Vec<GpuMesh>,
    pipes: Vec<GpuPipe>,
    binds: Vec<BindGroupRec>,
    bind_pending: Vec<u32>,
    bind_dep: Vec<u32>,
    tex_binds: HashMap<usize, Vec<u32>, crate::distance_field::FxBuild>,
    samplers: HashMap<FxSamplerDesc, wgpu::Sampler>,
    depth_sampler: wgpu::Sampler,
    /// Frame data (set by `set_frame`, consumed by the passes).
    /// (buffer writes, draws) of the last `set_frame`: `append_frame` replaces everything after them, so a repeated submit of the same rendered frame does not double the appended draws.
    base_len: (usize, usize),
    frame: FxFrame,
    dyn_vb: wgpu::Buffer,
    dyn_ib: wgpu::Buffer,
    inst_vb: wgpu::Buffer,
    /// Per draw: (dynamic vertex offset, dynamic index offset / count, instance offset)
    placed: Vec<Placed>,
    /// INSTANCING: per shadow draw with instances, per cascade (offset in `inst_vb`, instance count) of the ids that pass that cascade (`prepare`).
    shadow_placed: Vec<Vec<(u64, u32)>>,
    shadow_ids: Vec<u32>,
    depth_gen: u64,
    /// Last error text of a failed pipeline / bind group creation (also returned by the create calls).
    pub last_error: Option<String>,
    /// Draw indices of the standard / separate translucency passes in UE sort order (built by `prepare`).
    order_standard: Vec<usize>,
    order_separate: Vec<usize>,
    /// The same passes with the particle pass' translucent batches merged in (built by `merge_particles`).
    merged_standard: Vec<TItem>,
    merged_separate: Vec<TItem>,
    separate_target: Option<SeparateTarget>,
    composite: Option<(wgpu::RenderPipeline, wgpu::BindGroupLayout)>,
    /// TRANSLUCENCY VOLUME: the four volume views ([`FxRes::TranslucencyVolume`]) and their generation; `None` = the black 1x1x1 stand-in (the cleared volume).
    volume: Option<[wgpu::TextureView; 4]>,
    volume_gen: u64,
    volume_black: wgpu::TextureView,
    /// FX SHADOW: layout of [`FX_SHADOW_GROUP`], one bind group per cascade vp buffer ([`Self::set_shadow_vp`]), and the shadow draws of the last
    /// `set_frame` (`append_frame` keeps them, like `base_len`).
    bgl_shadow_vp: wgpu::BindGroupLayout,
    shadow_vp_bgs: Vec<wgpu::BindGroup>,
    base_shadow: (usize, usize),
    /// FX RECEIVE: set by the renderer ([`Self::set_scene_lighting`]); `None` (standalone FX tools) = lit FX materials keep fn-fx's simple sun + sky tail.
    scene: Option<SceneLit>,
    /// FOG INPUTS (2026-10-05): the `$Globals` buffers holding bound height-fog values (registered by fn-fx when a program binds `FHeightFogShaderParameters`),
    /// the `View` buffers of the FX programs, this frame's values ([`Self::set_view_fog`]), the integrated scattering volume (+ the volfog volume serial it came
    /// from) and the white cube. Written over the frame's own buffer writes in [`Self::prepare`].
    fog_targets: HashMap<FxBufId, Vec<FxFogSlot>>,
    fog_last: Option<[[f32; 4]; FX_FOG_FIELDS]>,
    fog_patched: Vec<bool>,
    fog_flags: Vec<u8>,
    fog_gen: u32,
    bind_fog: Vec<(u32, Vec<FxBufId>)>,
    fog_dirty: std::sync::Mutex<Vec<FxBufId>>,
    fog_views: std::collections::HashSet<FxBufId>,
    /// VELOCITY (2026-10-05): velocity twins of the scene-lit pipelines (by pipe index) and the previous-view history of the `View` buffers (`fog_views`).
    velocity_pipes: HashMap<usize, wgpu::RenderPipeline>,
    /// PREPASS (2026-10-06): depth-only twins of the masked scene-lit pipelines (by pipe index), drawn in the depth pre-pass ([`Self::draw_scene_lit_depth`]).
    depth_pipes: HashMap<usize, wgpu::RenderPipeline>,
    /// STORM TAA (2026-10-07): responsive-AA mask twins of the translucent `bEnableResponsiveAA` pipelines (by pipe index), [`Self::draw_responsive_aa`].
    responsive_pipes: HashMap<usize, wgpu::RenderPipeline>,
    view_history: crate::taa::ViewHistory,
    /// GRASS MOTION (2026-10-07): this frame's TAA projection jitter (NDC), applied to the `View` buffers in [`Self::prepare`] (`crate::taa::apply_view_jitter`).
    view_jitter: [f32; 2],
    view_size: (u32, u32),
    /// BACKDROP REAL (2026-10-07): scene-lit pipelines whose draws belong to the BACKDROP pass (the far skybox terraces / rocks on their real cooked material,
    /// fn-app backdrop_real.rs): skipped by the scene pass / pre-pass / velocity, drawn by [`Self::draw_backdrop_lit`] (depth twin first, then Equal colour).
    backdrop_pipes: std::collections::HashSet<usize>,
    /// BACKDROP REAL: [`Self::create_scene_lit_pipeline`] builds backdrop pipelines while set ([`Self::set_building_backdrop`]): no pre-pass split (LessEqual + depth
    /// write, the full pixel program), its own dedup key, marked as a backdrop pipeline.
    building_backdrop: bool,
    /// PERF (FPS investigation 2026-10-06): the bytes each buffer last received through a whole-buffer write ([`Self::write_buffer`] or a frame's
    /// `buffer_writes`). [`Self::prepare`] skips a frame write identical to what the buffer holds (static props re-send unchanged primitive / material /
    /// velocity constants every frame, and every `queue.write_buffer` costs a staging copy at submit). Buffers that also get partial writes in `prepare`
    /// (the fog slots, the View history patch) are never skipped.
    uploaded: std::sync::Mutex<Vec<Vec<u8>>>,
    upload_skipped: usize,
    /// PERF (2026-10-06, frame-time spikes): with [`Self::set_staged_uploads`] (the renderer), [`Self::prepare`] stages the frame's writes (whole-buffer, fog
    /// slots, dynamic geometry, instances) in ONE mapped buffer and [`Self::encode_uploads`] records the copies at the start of the frame's command encoder,
    /// before every pass, in the same order. `queue.write_buffer` creates and later destroys a staging buffer per call in wgpu-core (Queue::write_buffer ->
    /// StagingBuffer::new); the FX pass made about 300 of them per frame. Same bytes, same destination, same order: the passes read the same data.
    staged: bool,
    stage_bytes: Vec<u8>,
    stage_copies: Vec<(wgpu::Buffer, u64, u64, u64)>,
    stage_buf: Option<wgpu::Buffer>,
    stage_ready: bool,
    retired: Vec<wgpu::Texture>,
    tex_pools: TexPools,
    res: TexResidency,
    /// TEXTURE MEMORY (2026-10-06): BC textures are uploaded as blocks ([`FxTexFormat`]); `FN_DEBUG_NO_BC=1` (debug-only A/B switch) decodes them like before.
    bc: bool,
    /// Textures created so far per GPU format: (count, bytes of all levels), BC blocks rejected by `create_texture` (never expected, see there),
    /// and the BC4 / BC5 textures flagged sRGB (sampled linear like D3D11, see [`FxTexFormat::wgpu`]).
    tex_stats: HashMap<FxTexFormat, (usize, u64)>,
    tex_bc_rejected: usize,
    tex_bc_srgb_linear: usize,
    /// PERF: per-cascade side planes and small-caster cull inputs of this frame ([`Self::set_shadow_cull`]).
    shadow_cull: Vec<([glam::Vec4; 6], crate::lighting::Cascade)>,
    shadow_small: Option<(glam::Vec3, f32, f32)>,
    /// MATERIAL PARAMETERS (2026-10-06): the one uniform buffer of each material parameter collection (by object path) that every program reading it binds as
    /// `MaterialCollectionN`, like the world's single `UMaterialParameterCollectionInstance` buffer (fn_fx::gpu creates it with the collection's defaults).
    collections: HashMap<String, FxBufId>,
    view_fog: FxViewFog,
    fog_volume: Option<(wgpu::TextureView, u64)>,
    white_cube: wgpu::TextureView,
    /// BROWSER PORT (wasm32): pipelines still compiling in the browser (fx_async.rs)
    #[cfg(target_arch = "wasm32")]
    web: fx_async::WebPipes,
    /// PIPELINE DEDUP (2026-10-06): (kind, content hash of the whole [`FxPipelineDesc`] but its label) -> the pipe already made from it, and the reuses.
    /// Several first-use builds make byte-identical programs (a pickup hologram per weapon mesh, the blueprint MID per piece class, mirrored and
    /// unmirrored variants with the same cull); FX pipelines are never destroyed and bind groups are made against the pipe's layouts, so they share it.
    pipe_cache: HashMap<(u8, u64, u64), FxPipeId>,
    module_cache: HashMap<(u64, u64, usize), wgpu::ShaderModule>,
    pipe_cache_hits: usize,
    pso_warm: Option<fx_pso::Warm>,
}
#[derive(Clone, Copy, Default)]
struct Placed { vb: u64, ib: u64, index_count: u32, inst: u64, /** INSTANCING: the instance bytes fit `inst_vb` this frame (else the draw is skipped) */ inst_ok: bool }
/// PERF (2026-10-07): what the previous draw of a pass set (pipeline address, bind group id + 1 per group 0..3, mesh id + 1; 0 = nothing), see `draw_one_cached`.
#[derive(Clone, Copy, Default)]
struct PassState { pipe: usize, binds: [usize; 4], mesh: usize, globals: usize }
fn keep_binds() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_FX_KEEP_BINDS").map_or(true, |v| v != "0")) }
impl PassState {
    fn change_pipe(&mut self, key: usize) { if keep_binds() { self.pipe = key; } else { *self = PassState { pipe: key, ..PassState::default() }; } }
}

const DYN_CAPACITY: u64 = 1 << 20;
/// INSTANCING (2026-10-07): the per-frame instance buffer: the grass (up to 768 KB per draw, grass_render.rs) + the statics' instance ids (scene + per-cascade shadow lists, ~20-40 KB) + the mesh particles.
const INST_CAPACITY: u64 = 2 << 20;

struct FxBuf { buf: wgpu::Buffer, off: u64, size: u64 }
impl FxBuf {
    fn size(&self) -> u64 { self.size }
    fn binding(&self) -> wgpu::BindingResource<'_> { wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &self.buf, offset: self.off, size: std::num::NonZeroU64::new(self.size) }) }
}
const BUF_SLAB_BYTES: u64 = 1 << 20;
const BUF_SLAB_MAX: u64 = 16 << 10;
const MESH_SLAB_BYTES: u64 = 4 << 20;
const MESH_SLAB_MAX: u64 = 256 << 10;
fn buffer(device: &wgpu::Device, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer { device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false }) }

impl FxRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, hdr_format: wgpu::TextureFormat, depth_format: wgpu::TextureFormat) -> FxRenderer {
        let depth_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("fx scene depth sampler"), mag_filter: wgpu::FilterMode::Nearest, min_filter: wgpu::FilterMode::Nearest, mipmap_filter: wgpu::MipmapFilterMode::Nearest, address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, ..Default::default() });
        // TRANSLUCENCY VOLUME: a zero (cleared, ClearTranslucentVolumeLighting) PF_FloatRGBA volume until the renderer hands over its own
        let black = device.create_texture(&wgpu::TextureDescriptor { label: Some("fx translucency volume (cleared)"), size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D3, format: wgpu::TextureFormat::Rgba16Float, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &black, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, &[0u8; 8], wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(1) }, wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 });
        let volume_black = black.create_view(&Default::default());
        let bgl_shadow_vp = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fx shadow vp"), entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0, visibility: wgpu::ShaderStages::VERTEX, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }] });
        // FOG INPUTS: GWhiteTextureCube (1x1 per face, white)
        let white = device.create_texture(&wgpu::TextureDescriptor { label: Some("fx white cube (GWhiteTextureCube)"), size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 6 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &white, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, &[255u8; 24], wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) }, wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 6 });
        let white_cube = white.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::Cube), ..Default::default() });
        // TEXTURE MEMORY (2026-10-06): BC blocks go to the GPU as they are when the device has TEXTURE_COMPRESSION_BC (fn-render gpu.rs requests it when the
        // adapter has it); `FN_DEBUG_NO_BC=1` (debug-only A/B switch) keeps the CPU decode to RGBA8 for every texture
        let bc = set_bc_native_hint(device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC));
        static NEXT_INSTANCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        FxRenderer {
            instance: NEXT_INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            fog_targets: Default::default(), fog_last: None, fog_patched: Default::default(), fog_flags: Default::default(), fog_gen: 1, bind_fog: Default::default(), fog_dirty: Default::default(), fog_views: Default::default(), view_fog: FxViewFog::default(), fog_volume: None, white_cube,
            velocity_pipes: HashMap::new(), view_history: Default::default(), view_jitter: [0.0; 2], view_size: (0, 0), // VELOCITY
            depth_pipes: HashMap::new(), // PREPASS
            responsive_pipes: HashMap::new(), // STORM TAA
            backdrop_pipes: Default::default(), building_backdrop: false, // BACKDROP REAL
            #[cfg(target_arch = "wasm32")] web: Default::default(), // BROWSER PORT: async pipelines
            pipe_cache: HashMap::new(), module_cache: HashMap::new(), pipe_cache_hits: 0, pso_warm: None, // PIPELINE DEDUP
            collections: HashMap::new(), // MATERIAL PARAMETERS
            uploaded: Default::default(), upload_skipped: 0, shadow_cull: vec![], shadow_small: None, // PERF
            staged: false, stage_bytes: vec![], stage_copies: vec![], stage_buf: None, stage_ready: false, retired: vec![], tex_pools: TexPools { on: pools_enabled(), ..Default::default() }, res: TexResidency::default(), // PERF: staged frame uploads
            bc, tex_stats: HashMap::new(), tex_bc_rejected: 0, tex_bc_srgb_linear: 0, // TEXTURE MEMORY
            bgl_shadow_vp, shadow_vp_bgs: vec![], base_shadow: (0, 0), scene: None,
            volume: None, volume_gen: 0, volume_black,
            device: device.clone(), queue: queue.clone(), hdr_format, depth_format,
            textures: vec![], buffers: vec![], buf_slab: None, mesh_slab: None, meshes: vec![], pipes: vec![], binds: vec![], bind_pending: vec![], bind_dep: vec![], tex_binds: Default::default(), samplers: HashMap::new(), depth_sampler,
            frame: FxFrame::default(), base_len: (0, 0),
            dyn_vb: buffer(device, "fx dynamic vertices", DYN_CAPACITY, wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST),
            dyn_ib: buffer(device, "fx dynamic indices", DYN_CAPACITY, wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST),
            inst_vb: buffer(device, "fx instances", INST_CAPACITY, wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST),
            placed: vec![], shadow_placed: vec![], shadow_ids: vec![], depth_gen: 0, last_error: None,
            order_standard: vec![], order_separate: vec![], merged_standard: vec![], merged_separate: vec![], separate_target: None, composite: None,
        }
    }

    // ---- resources ---------------------------------------------------------------------------------------------------------
    /// TEXTURE MEMORY: this renderer uploads BC blocks as they are (`Features::TEXTURE_COMPRESSION_BC`, not `FN_DEBUG_NO_BC=1`).
    pub fn supports_bc(&self) -> bool { self.bc }
    /// A texture of `dim` whose level 0 is `width` x `height` can be a BC texture here (2D, level-0 size a multiple of the 4x4 block: wgpu rejects
    /// anything else on every backend).
    pub fn accepts_bc(&self, dim: FxTexDim, width: u32, height: u32) -> bool { self.bc && dim == FxTexDim::D2 && width % 4 == 0 && height % 4 == 0 && width > 0 && height > 0 }
    /// Textures created so far: per GPU format (count, bytes of all levels), BC textures rejected (never expected), BC4 / BC5 textures flagged sRGB.
    pub fn texture_stats(&self) -> (Vec<(FxTexFormat, usize, u64)>, usize, usize) {
        let mut v: Vec<(FxTexFormat, usize, u64)> = self.tex_stats.iter().map(|(f, (n, b))| (*f, *n, *b)).collect();
        v.sort_by_key(|x| format!("{:?}", x.0));
        (v, self.tex_bc_rejected, self.tex_bc_srgb_linear)
    }
    /// One line of [`Self::texture_stats`] (reports).
    pub fn texture_report(&self) -> String {
        let (v, rejected, srgb_linear) = self.texture_stats();
        let total: u64 = v.iter().map(|x| x.2).sum();
        let parts: Vec<String> = v.iter().map(|(f, n, b)| format!("{f:?} {n} ({:.1} MB)", *b as f64 / 1048576.)).collect();
        format!("fx textures {:.1} MB: {} | BC passthrough {}, BC rejected {rejected}, BC4/BC5 flagged sRGB (sampled linear) {srgb_linear}", total as f64 / 1048576., parts.join(", "), if self.bc { "on" } else { "off" })
    }
    pub fn instance(&self) -> u64 { self.instance }
    pub fn create_texture(&mut self, d: &FxTextureDesc) -> FxTexId {
        // TEXTURE MEMORY: BC blocks this device cannot take (no BC feature, not 2D, level 0 not a multiple of 4, short level data) never come here (the
        // producer decodes them first, fn-fx TextureCache::load); if they do, a 1x1 black texture instead of a wgpu validation panic, counted in the report
        if d.format.is_bc() {
            let l0 = &d.levels[0];
            let complete = d.levels.iter().all(|l| l.rgba8.len() >= d.format.level_bytes(l.width, l.height));
            if !(self.accepts_bc(d.dim, l0.width, l0.height) && complete) {
                eprintln!("[fx] {}: {:?} blocks this device cannot take ({}x{}, {:?}); 1x1 black instead", d.label, d.format, l0.width, l0.height, d.dim);
                self.tex_bc_rejected += 1;
                return self.create_texture(&FxTextureDesc { label: d.label.clone(), dim: d.dim, srgb: d.srgb, levels: vec![FxLevel { width: 1, height: 1, depth: 1, rgba8: vec![0; 4 * if d.dim == FxTexDim::Cube { 6 } else { 1 }] }], format: FxTexFormat::Rgba8 });
            }
            if d.srgb && matches!(d.format, FxTexFormat::Bc4 | FxTexFormat::Bc5) { self.tex_bc_srgb_linear += 1; }
        }
        let l0 = &d.levels[0];
        let (dimension, depth_or_layers, view_dim) = match d.dim { FxTexDim::D2 => (wgpu::TextureDimension::D2, 1, wgpu::TextureViewDimension::D2), FxTexDim::D3 => (wgpu::TextureDimension::D3, l0.depth.max(1), wgpu::TextureViewDimension::D3), FxTexDim::Cube => (wgpu::TextureDimension::D2, 6, wgpu::TextureViewDimension::Cube) };
        let format = d.format.wgpu(d.srgb);
        let mut enc = None;
        let (tex, layer, loose) = if d.dim == FxTexDim::D2 { self.place_d2(&d.label, format, l0.width, l0.height, d.levels.len() as u32, &mut enc) } else {
            (self.device.create_texture(&wgpu::TextureDescriptor { label: Some(&d.label), size: wgpu::Extent3d { width: l0.width, height: l0.height, depth_or_array_layers: depth_or_layers }, mip_level_count: d.levels.len() as u32, sample_count: 1, dimension, format, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC, view_formats: &[] }), None, None)
        };
        if let Some(e) = enc { self.queue.submit(Some(e.finish())); }
        let origin = wgpu::Origin3d { x: 0, y: 0, z: layer.map_or(0, |l| l.1) };
        let mut bytes = 0u64;
        for (i, l) in d.levels.iter().enumerate() {
            let layers = match d.dim { FxTexDim::D2 => 1, FxTexDim::D3 => l.depth.max(1), FxTexDim::Cube => 6 };
            if d.format.is_bc() {
                // whole 4x4 blocks: rows of ceil(w / 4) blocks, ceil(h / 4) block rows, the copy covering the level's physical (block-rounded) size
                let (bw, bh) = (l.width.div_ceil(4), l.height.div_ceil(4));
                let n = d.format.level_bytes(l.width, l.height);
                self.queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin, aspect: wgpu::TextureAspect::All }, &l.rgba8[..n], wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bw * d.format.unit_bytes() as u32), rows_per_image: Some(bh) }, wgpu::Extent3d { width: bw * 4, height: bh * 4, depth_or_array_layers: 1 });
                bytes += n as u64;
            } else {
                self.queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin, aspect: wgpu::TextureAspect::All }, &l.rgba8, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * l.width), rows_per_image: Some(l.height) }, wgpu::Extent3d { width: l.width, height: l.height, depth_or_array_layers: layers });
                bytes += (4 * l.width * l.height * layers) as u64;
            }
        }
        let e = self.tex_stats.entry(d.format).or_insert((0, 0)); e.0 += 1; e.1 += bytes;
        note_upload(&self.queue, bytes);
        let view = match layer { Some((_, l)) => layer_view(&tex, l), None => tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(view_dim), ..Default::default() }) };
        self.textures.push(GpuTex { tex, view, dim: d.dim, layer });
        let id = self.textures.len() - 1;
        if let Some(k) = loose { self.tex_pools.loose.entry(k).or_default().push(id); }
        FxTexId(id)
    }
    fn take_layer(&mut self, key: &PoolKey) -> Option<(usize, u32)> {
        let ps = self.tex_pools.by_key.get(key)?;
        for &p in ps {
            let pool = &mut self.tex_pools.pools[p];
            if pool.tex.is_none() { continue; }
            if let Some(l) = pool.free.pop() { pool.live += 1; return Some((p, l)); }
        }
        None
    }
    fn place_d2(&mut self, label: &str, format: wgpu::TextureFormat, w: u32, h: u32, levels: u32, enc: &mut Option<wgpu::CommandEncoder>) -> (wgpu::Texture, Option<(usize, u32)>, Option<PoolKey>) {
        let key = (format, w, h, levels);
        let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC;
        let cap = if self.tex_pools.on { pool_capacity(mip_chain_bytes(format, w, h, levels), self.device.limits().max_texture_array_layers.min(256)) } else { None };
        if let Some(cap) = cap {
            if let Some((p, l)) = self.take_layer(&key) { return (self.tex_pools.pools[p].tex.clone().unwrap(), Some((p, l)), None); }
            if self.tex_pools.loose.get(&key).map_or(0, |v| v.len()) + 1 >= cap as usize {
                let p = self.tex_pools.pools.len();
                let tex = self.device.create_texture(&wgpu::TextureDescriptor { label: Some("fx tex pool"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: cap }, mip_level_count: levels, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] });
                self.tex_pools.pools.push(TexPool { tex: Some(tex.clone()), free: (0..cap).rev().collect(), live: 0, layers: cap, bytes: mip_chain_bytes(format, w, h, levels) * cap as u64 });
                self.tex_pools.by_key.entry(key).or_default().push(p);
                let moved = self.tex_pools.loose.remove(&key).unwrap_or_default();
                let (bw, bh) = format.block_dimensions();
                for id in moved {
                    let Some((_, l)) = self.take_layer(&key) else { self.tex_pools.loose.entry(key).or_default().push(id); continue };
                    let e = enc.get_or_insert_with(|| self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fx tex pool fill") }));
                    for m in 0..levels {
                        let (cw, ch) = ((w >> m).max(1).next_multiple_of(bw), (h >> m).max(1).next_multiple_of(bh));
                        e.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo { texture: &self.textures[id].tex, mip_level: m, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: m, origin: wgpu::Origin3d { x: 0, y: 0, z: l }, aspect: wgpu::TextureAspect::All },
                            wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
                        );
                    }
                    let view = layer_view(&tex, l);
                    let old = std::mem::replace(&mut self.textures[id], GpuTex { tex: tex.clone(), view, dim: FxTexDim::D2, layer: Some((p, l)) });
                    self.retired.push(old.tex);
                    self.invalidate_binds(id);
                }
                if let Some((p, l)) = self.take_layer(&key) { return (tex, Some((p, l)), None); }
            }
        }
        let tex = self.device.create_texture(&wgpu::TextureDescriptor { label: Some(label), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: levels, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] });
        (tex, None, cap.map(|_| key))
    }
    fn unloose(&mut self, id: usize) {
        let Some(g) = self.textures.get(id) else { return };
        if g.layer.is_some() || g.dim != FxTexDim::D2 { return; }
        let k = (g.tex.format(), g.tex.width(), g.tex.height(), g.tex.mip_level_count());
        if let Some(v) = self.tex_pools.loose.get_mut(&k) { v.retain(|&x| x != id); }
    }
    fn release_tex(&mut self, g: GpuTex) {
        match g.layer { Some(l) => self.tex_pools.freeing.push(l), None => self.retired.push(g.tex) }
    }
    fn invalidate_binds(&mut self, id: usize) {
        if let Some(v) = self.tex_binds.get(&id) { for &i in v { if let Some(b) = self.binds.get_mut(i as usize) { b.built = None; self.bind_pending.push(i); } } }
    }
    fn drain_pool_frees(&mut self) {
        for (p, l) in std::mem::take(&mut self.tex_pools.freeing) {
            let pool = &mut self.tex_pools.pools[p];
            pool.free.push(l);
            pool.live = pool.live.saturating_sub(1);
            if pool.live == 0 { if let Some(t) = pool.tex.take() { pool.free.clear(); self.retired.push(t); } }
        }
    }
    pub fn pool_stats(&self) -> (usize, u64, u32, u32, usize) {
        let live: Vec<&TexPool> = self.tex_pools.pools.iter().filter(|p| p.tex.is_some()).collect();
        (live.len(), live.iter().map(|p| p.bytes).sum(), live.iter().map(|p| p.live).sum(), live.iter().map(|p| p.layers).sum(), self.tex_pools.loose.values().map(|v| v.len()).sum())
    }
    pub fn replace_texture(&mut self, id: FxTexId, d: &FxTextureDesc) -> bool {
        if id.0 >= self.textures.len() { return false; }
        self.unloose(id.0);
        let new = self.create_texture(d);
        let Some(g) = self.textures.pop() else { return false };
        debug_assert_eq!(new.0, self.textures.len());
        if g.layer.is_none() && g.dim == FxTexDim::D2 {
            let k = (g.tex.format(), g.tex.width(), g.tex.height(), g.tex.mip_level_count());
            if let Some(v) = self.tex_pools.loose.get_mut(&k) { for x in v.iter_mut() { if *x == new.0 { *x = id.0; } } }
        }
        let old = std::mem::replace(&mut self.textures[id.0], g);
        self.release_tex(old);
        self.invalidate_binds(id.0);
        if let Some(st) = self.res.state.get_mut(id.0) { if *st >= 2 { *st = 1; self.res.restored += 1; } }
        true
    }
    pub fn set_evictable(&mut self, id: FxTexId) {
        if self.res.state.len() <= id.0 { self.res.state.resize(id.0 + 1, 0); }
        if self.res.state[id.0] == 0 { self.res.state[id.0] = 1; }
    }
    pub fn touch_binds(&mut self, binds: &[(u32, FxBindId)]) {
        let f = self.res.frame;
        for (_, b) in binds { if let Some(u) = self.res.bind_used.get_mut(b.0) { *u = f; } }
    }
    pub fn take_wanted_textures(&mut self) -> Vec<FxTexId> { std::mem::take(&mut self.res.wanted).into_iter().map(FxTexId).collect() }
    pub fn residency_stats(&self) -> (usize, u64, usize) { (self.res.evicted, self.res.evicted_bytes, self.res.restored) }
    fn residency_mark(&mut self) {
        self.res.frame += 1;
        let f = self.res.frame;
        if self.res.bind_used.len() < self.binds.len() { self.res.bind_used.resize(self.binds.len(), f); }
        if f % 4 != 0 { return; }
        let used = &mut self.res.bind_used;
        for d in &self.frame.draws { for (_, b) in &d.binds { if let Some(u) = used.get_mut(b.0) { *u = f; } } }
        for d in &self.frame.shadow_draws { for (_, b) in &d.binds { if let Some(u) = used.get_mut(b.0) { *u = f; } } }
        if f % RES_SCAN_FRAMES != 0 { return; }
        let mut shrink = vec![];
        let mut idle: Vec<(u32, u64, usize)> = vec![];
        let mut full = 0u64;
        for t in 0..self.res.state.len() {
            let st = self.res.state[t];
            if st == 0 { continue; }
            let Some(bs) = self.tex_binds.get(&t) else { continue };
            let last = bs.iter().filter_map(|&b| self.res.bind_used.get(b as usize).copied()).max().unwrap_or(f);
            match st {
                1 if f.saturating_sub(last) > RES_AGE_FRAMES => shrink.push(t),
                1 => if let Some((_, bytes, _)) = self.shrink_plan(t) {
                    full += bytes;
                    if f.saturating_sub(last) >= RES_PRESSURE_IDLE_FRAMES { idle.push((last, bytes, t)); }
                },
                2 if f.saturating_sub(last) < RES_SCAN_FRAMES => { self.res.state[t] = 3; self.res.wanted.push(t); }
                _ => {}
            }
        }
        let budget = res_budget_bytes();
        if budget > 0 && full > budget && !idle.is_empty() {
            idle.sort_unstable_by_key(|e| e.0);
            let target = budget - budget / 8;
            for (_, bytes, t) in idle.into_iter().take(RES_PRESSURE_MAX) {
                if full <= target { break; }
                shrink.push(t);
                full -= bytes;
                self.res.pressured += 1;
                self.res.pressure_bytes += bytes;
            }
        }
        self.res.full_bytes = full;
        shrink.truncate(RES_SHRINK_PER_SCAN);
        let mut enc = None;
        for t in shrink { if self.shrink_texture(t, &mut enc) { self.res.state[t] = 2; } }
        if let Some(e) = enc { self.queue.submit(Some(e.finish())); }
        if cfg!(target_arch = "wasm32") && f % RES_LOG_FRAMES == 0 {
            let mb = |b: u64| b as f64 / 1048576.;
            let (np, pb, pl, pt, nl) = self.pool_stats();
            eprintln!("[tex residency] full-res evictable {:.0} MB (budget {:.0} MB); shrunk {} ({:.0} MB), {} of them for the budget ({:.0} MB); restored {}; pools {np} ({:.0} MB, {pl}/{pt} layers used), {nl} poolable outside pools", mb(full), mb(budget), self.res.evicted, mb(self.res.evicted_bytes), self.res.pressured, mb(self.res.pressure_bytes), self.res.restored, mb(pb));
        }
    }
    fn shrink_plan(&self, t: usize) -> Option<(u32, u64, u64)> {
        let g = self.textures.get(t)?;
        if g.dim != FxTexDim::D2 || (g.layer.is_none() && g.tex.depth_or_array_layers() != 1) { return None; }
        let (w, h, levels, format) = (g.tex.width(), g.tex.height(), g.tex.mip_level_count(), g.tex.format());
        let (bw, bh) = format.block_dimensions();
        let bytes_of = |w: u32, h: u32, n: u32| -> u64 { (0..n).map(|m| { let (mw, mh) = ((w >> m).max(1).div_ceil(bw) as u64, (h >> m).max(1).div_ceil(bh) as u64); mw * mh * format.block_copy_size(None).unwrap_or(4) as u64 }).sum() };
        let full = bytes_of(w, h, levels);
        if full < RES_MIN_BYTES { return None; }
        let mut k = 0;
        while k + 1 < levels && (w >> k).max(h >> k) > RES_KEEP_PX { k += 1; }
        while k > 0 && ((w >> k) % bw != 0 || (h >> k) % bh != 0 || (w >> k) == 0 || (h >> k) == 0) { k -= 1; }
        if k == 0 { return None; }
        Some((k, full, bytes_of(w >> k, h >> k, levels - k)))
    }
    fn shrink_texture(&mut self, t: usize, enc: &mut Option<wgpu::CommandEncoder>) -> bool {
        let Some((k, full, low)) = self.shrink_plan(t) else { return false };
        let (src, sz) = { let g = &self.textures[t]; (g.tex.clone(), g.layer.map_or(0, |l| l.1)) };
        let (w, h, levels, format) = (src.width(), src.height(), src.mip_level_count(), src.format());
        let (bw, bh) = format.block_dimensions();
        let (nw, nh, nl) = (w >> k, h >> k, levels - k);
        self.unloose(t);
        let (new, layer, loose) = self.place_d2("fx tex (resident low mips)", format, nw, nh, nl, enc);
        let dz = layer.map_or(0, |l| l.1);
        let e = enc.get_or_insert_with(|| self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fx tex shrink") }));
        for m in 0..nl {
            let (mw, mh) = ((w >> (k + m)).max(1), (h >> (k + m)).max(1));
            let (cw, ch) = (mw.next_multiple_of(bw), mh.next_multiple_of(bh));
            e.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo { texture: &src, mip_level: k + m, origin: wgpu::Origin3d { x: 0, y: 0, z: sz }, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyTextureInfo { texture: &new, mip_level: m, origin: wgpu::Origin3d { x: 0, y: 0, z: dz }, aspect: wgpu::TextureAspect::All },
                wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
            );
        }
        let view = match layer { Some((_, l)) => layer_view(&new, l), None => new.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2), ..Default::default() }) };
        let old = std::mem::replace(&mut self.textures[t], GpuTex { tex: new, view, dim: FxTexDim::D2, layer });
        if let Some(key) = loose { self.tex_pools.loose.entry(key).or_default().push(t); }
        self.release_tex(old);
        self.invalidate_binds(t);
        self.res.evicted += 1;
        self.res.evicted_bytes += full - low;
        true
    }
    /// A uniform buffer of `size` bytes (rounded up to 16; WGSL arrays of vec4 need no more).
    pub fn create_buffer(&mut self, label: &str, size: usize) -> FxBufId {
        let size = (size.max(16).div_ceil(16) * 16) as u64;
        let usage = wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let b = if size <= BUF_SLAB_MAX {
            let slot = size.div_ceil(256) * 256;
            if self.buf_slab.as_ref().is_none_or(|(_, at)| at + slot > BUF_SLAB_BYTES) { self.buf_slab = Some((buffer(&self.device, "fx uniform slab", BUF_SLAB_BYTES, usage), 0)); }
            let (slab, at) = self.buf_slab.as_mut().unwrap();
            let off = *at;
            *at += slot;
            FxBuf { buf: slab.clone(), off, size }
        } else { FxBuf { buf: buffer(&self.device, label, size, usage), off: 0, size } };
        self.buffers.push(b);
        FxBufId(self.buffers.len() - 1)
    }
    /// Writes a uniform buffer right away (static contents such as a material's constant buffer).
    pub fn write_buffer(&self, b: FxBufId, bytes: &[u8]) {
        if let Some(buf) = self.buffers.get(b.0) {
            let n = (bytes.len() as u64).min(buf.size()) as usize;
            self.queue.write_buffer(&buf.buf, buf.off, &bytes[..n]);
            if self.fog_flag(b) & 1 != 0 { if let Ok(mut d) = self.fog_dirty.lock() { d.push(b); } }
            if let Ok(mut u) = self.uploaded.lock() { if u.len() <= b.0 { u.resize(b.0 + 1, Vec::new()); } let e = &mut u[b.0]; e.clear(); e.extend_from_slice(&bytes[..n]); } // PERF
        }
    }
    /// INSTANCING (2026-10-07): writes `bytes` at `offset` of a buffer right away (a part of a large static buffer, e.g. one placement's `Primitive` in the
    /// statics' instance storage). The buffer is left out of the unchanged-write skip of [`Self::prepare`] from then on (its stored copy is dropped).
    pub fn write_buffer_range(&self, b: FxBufId, offset: u64, bytes: &[u8]) {
        if let Some(buf) = self.buffers.get(b.0) {
            if offset >= buf.size() { return; }
            let n = (bytes.len() as u64).min(buf.size() - offset) as usize & !3;
            if n > 0 { self.queue.write_buffer(&buf.buf, buf.off + offset, &bytes[..n]); }
            if self.fog_flag(b) & 1 != 0 { if let Ok(mut d) = self.fog_dirty.lock() { d.push(b); } }
            if let Ok(mut u) = self.uploaded.lock() { if let Some(e) = u.get_mut(b.0) { *e = Vec::new(); } }
        }
    }
    fn mesh_space(&mut self, label: &str, len: u64, usage: wgpu::BufferUsages) -> (wgpu::Buffer, u64) {
        if len > MESH_SLAB_MAX { return (buffer(&self.device, label, len, usage | wgpu::BufferUsages::COPY_DST), 0); }
        let slot = len.div_ceil(256) * 256;
        if self.mesh_slab.as_ref().is_none_or(|(_, at)| at + slot > MESH_SLAB_BYTES) { self.mesh_slab = Some((buffer(&self.device, "fx mesh slab", MESH_SLAB_BYTES, wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST), 0)); }
        let (slab, at) = self.mesh_slab.as_mut().unwrap();
        let off = *at;
        *at += slot;
        (slab.clone(), off)
    }
    pub fn create_mesh(&mut self, d: &FxMeshDesc) -> FxMeshId {
        let (vlen, ilen) = (d.vertices.len().max(4) as u64, (d.indices.len() * 4).max(4) as u64);
        let (vb, vo) = self.mesh_space(&d.label, vlen, wgpu::BufferUsages::VERTEX);
        self.queue.write_buffer(&vb, vo, &d.vertices);
        let (ib, io) = self.mesh_space(&d.label, ilen, wgpu::BufferUsages::INDEX);
        self.queue.write_buffer(&ib, io, bytemuck::cast_slice(&d.indices));
        note_upload(&self.queue, (d.vertices.len() + d.indices.len() * 4) as u64);
        self.meshes.push(GpuMesh { vb, ib, index_count: d.indices.len() as u32, vr: (vo, vlen), ir: (io, ilen) });
        FxMeshId(self.meshes.len() - 1)
    }
    /// A pipeline of the program being created (it gets pipe index `self.pipes.len()`). BROWSER PORT: on wasm32 through createRenderPipelineAsync, a
    /// placeholder until it resolves (fx_async.rs; the program's draws are skipped meanwhile); natively the usual synchronous call.
    fn render_pipeline(&mut self, slot: FxSlot, desc: &wgpu::RenderPipelineDescriptor) -> wgpu::RenderPipeline {
        #[cfg(target_arch = "wasm32")]
        return self.web.start(&self.device, self.depth_format, self.pipes.len(), slot, desc);
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = slot; self.device.create_render_pipeline(desc) }
    }
    /// PIPELINE DEDUP: the cache key of `d` for pipeline kind `kind` (0 FX pass, 1 scene-lit, 2 shadow): two 64-bit hashes of everything but the label.
    fn module_for(&mut self, label: &str, src: &str) -> wgpu::ShaderModule {
        let k = (fast_hash(0x51ed_27a3, src.as_bytes()), fast_hash(0x9d1f_c3b5, src.as_bytes()), src.len());
        if let Some(m) = self.module_cache.get(&k) { return m.clone(); }
        let m = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(src.into()) });
        self.module_cache.insert(k, m.clone());
        m
    }
    fn pipe_key(kind: u8, d: &FxPipelineDesc) -> (u8, u64, u64) {
        let small = format!("{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}", d.vs_entry, d.fs_entry, d.vertex_buffers, d.groups, d.blend, d.depth, d.cull, d.translucent, d.separate_translucency, d.responsive_aa);
        let (w0, w1) = (fast_hash(0x2545_f491, d.wgsl.as_bytes()), fast_hash(0x6a09_e667, d.wgsl.as_bytes()));
        let (s0, s1) = (fast_hash(0x3c6e_f372, small.as_bytes()), fast_hash(0xa54f_f53a, small.as_bytes()));
        (kind, mix64(w0 ^ mix64(s0 ^ d.wgsl.len() as u64)), mix64(s1 ^ mix64(w1.rotate_left(17) ^ small.len() as u64)))
    }
    /// PIPELINE DEDUP: the pipe already made from `key`, counted as a reuse.
    fn cached_pipe(&mut self, key: &(u8, u64, u64)) -> Option<FxPipeId> {
        let id = self.pipe_cache.get(key).copied()?;
        self.pipe_cache_hits += 1;
        Some(id)
    }
    /// PIPELINE DEDUP: one line: pipelines made, requests answered with an existing one.
    pub fn pipeline_cache_report(&self) -> String {
        let (made, hits) = (self.pipe_cache.len(), self.pipe_cache_hits);
        format!("[fx] pipeline dedup: {made} pipelines made, {hits} identical requests reused one ({:.0}% of {} requests)", 100. * hits as f32 / (made + hits).max(1) as f32, made + hits)
    }
    /// BROWSER PORT: FX pipelines the browser is still compiling (always 0 natively, where they are created synchronously). Installs the finished
    /// ones first (the loading screen polls this while no scene frame runs [`Self::prepare`]).
    pub fn pipe_pending(&self, id: FxPipeId) -> bool {
        #[cfg(target_arch = "wasm32")]
        { return self.web.is_pending(id.0); }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = id; false }
    }
    pub fn pending_pipelines(&mut self) -> usize {
        #[cfg(target_arch = "wasm32")]
        { self.web_poll(); return self.web.pending_count(); }
        #[cfg(not(target_arch = "wasm32"))]
        0
    }
    /// Creates the pipeline; a WGSL / validation error is returned (and kept in `last_error`) instead of panicking.
    pub fn create_pipeline(&mut self, d: &FxPipelineDesc) -> Result<FxPipeId, String> {
        // DEBUG (2026-10-06, debug-only): `FN_FX_DUMP_WGSL=<dir>` writes every FX program to <dir>/<label>.wgsl with its texture bindings (reading what a
        // translated program does with a texture, e.g. the BC5 normal-map channels)
        if let Ok(dir) = fn_live::var("FN_FX_DUMP_WGSL") {
            let name: String = d.label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{}.wgsl", &name[..name.len().min(150)])), &d.wgsl);
        }
        let key = Self::pipe_key(0, d); // PIPELINE DEDUP
        if let Some(id) = self.cached_pipe(&key) { return Ok(id); }
        #[cfg(target_arch = "wasm32")] self.web_stage_limits(d, None)?; // BROWSER PORT
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        // STORM TAA: a responsive-AA program without a discard gets a trivial mask entry (UE's stencil write happens on every depth-passing fragment, whatever
        // its alpha); with a discard the mask twin runs the program's own fragment entry so the clipped pixels stay untagged
        let responsive = d.responsive_aa && d.translucent;
        let mask_fs = if responsive && !d.wgsl.contains("discard") { Some(RESPONSIVE_AA_FS) } else if responsive { Some(d.fs_entry.as_str()) } else { None };
        let src: std::borrow::Cow<str> = if mask_fs == Some(RESPONSIVE_AA_FS) { format!("{}\n@fragment fn {RESPONSIVE_AA_FS}() -> @location(0) vec4<f32> {{ return vec4<f32>(0.0); }}\n", d.wgsl).into() } else { d.wgsl.as_str().into() };
        let module = self.module_for(&d.label, &src);
        let layouts: Vec<wgpu::BindGroupLayout> = d.groups.iter().map(|g| {
            let entries: Vec<wgpu::BindGroupLayoutEntry> = g.iter().map(|e| {
                let mut vis = wgpu::ShaderStages::empty();
                if e.vertex { vis |= wgpu::ShaderStages::VERTEX; }
                if e.fragment { vis |= wgpu::ShaderStages::FRAGMENT; }
                let ty = match e.ty {
                    FxBindTy::Uniform => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    FxBindTy::Tex2D { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    FxBindTy::Tex3D { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false },
                    FxBindTy::Cube { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::Cube, multisampled: false },
                    FxBindTy::SceneDepth => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    FxBindTy::Sampler { filtering } => wgpu::BindingType::Sampler(if filtering { wgpu::SamplerBindingType::Filtering } else { wgpu::SamplerBindingType::NonFiltering }),
                    FxBindTy::StorageRead => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                };
                wgpu::BindGroupLayoutEntry { binding: e.binding, visibility: vis, ty, count: None }
            }).collect();
            self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(&d.label), entries: &entries })
        }).collect();
        let layout_refs: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().map(Some).collect();
        let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(&d.label), bind_group_layouts: &layout_refs, immediate_size: 0 });
        let attrs: Vec<Vec<wgpu::VertexAttribute>> = d.vertex_buffers.iter().map(|b| b.attrs.iter().map(|a| wgpu::VertexAttribute { format: a.format.wgpu(), offset: a.offset as u64, shader_location: a.location }).collect()).collect();
        let vbs: Vec<Option<wgpu::VertexBufferLayout>> = d.vertex_buffers.iter().zip(&attrs).map(|(b, a)| Some(wgpu::VertexBufferLayout { array_stride: b.stride as u64, step_mode: if b.instance { wgpu::VertexStepMode::Instance } else { wgpu::VertexStepMode::Vertex }, attributes: a })).collect();
        let blend = match d.blend {
            FxBlend::Opaque => None,
            FxBlend::Translucent => Some(wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::SrcAlpha, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add }, alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add } }),
            // alpha Zero / InvSrcAlpha as UE4.16 BasePassRendering.h:1099 (the separate translucency target's alpha is the background weight of the recombine)
            FxBlend::Additive => Some(wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add }, alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add } }),
            FxBlend::Modulate => Some(wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Dst, dst_factor: wgpu::BlendFactor::Zero, operation: wgpu::BlendOperation::Add }, alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add } }),
        };
        let write = d.depth.write && !d.translucent;
        // MIRRORFIX: `BackCw` = D3D11 `FrontCounterClockwise` + `D3D11_CULL_FRONT` (see FxCull), keeping UE's SV_IsFrontFace convention
        let (cull_mode, front_face) = match d.cull { FxCull::None => (None, wgpu::FrontFace::Ccw), FxCull::BackCw => (Some(wgpu::Face::Front), wgpu::FrontFace::Ccw), FxCull::BackCcw => (Some(wgpu::Face::Back), wgpu::FrontFace::Ccw) };
        let pipe = self.render_pipeline(FxSlot::Main, &wgpu::RenderPipelineDescriptor {
            label: Some(&d.label), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &vbs },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(write), depth_compare: Some(if d.depth.test { wgpu::CompareFunction::LessEqual } else { wgpu::CompareFunction::Always }), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(&d.fs_entry), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: self.hdr_format, blend, write_mask: wgpu::ColorWrites::ALL })] }),
            multiview_mask: None, cache: None,
        });
        // STORM TAA (2026-10-07): the responsive-AA mask twin: same module / layout / vertex state / culling / depth test (CF_DepthNearOrEqual, or Always with
        // bDisableDepthTest: TranslucentRendering.cpp:536-557), no depth write, writes 0 (= tagged) into the TAA mask cleared to 1 (taa.rs RESPONSIVE_MASK_FORMAT).
        // Natively only (BROWSER PORT pipelines are async, fx_async.rs; the browser keeps the untagged TAA).
        #[cfg(not(target_arch = "wasm32"))]
        let mask_twin = mask_fs.map(|fs| self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{} responsive aa", d.label)), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &vbs },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(false), depth_compare: Some(if d.depth.test { wgpu::CompareFunction::LessEqual } else { wgpu::CompareFunction::Always }), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(fs), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: crate::taa::RESPONSIVE_MASK_FORMAT,
                blend: Some(wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::Zero, operation: wgpu::BlendOperation::Add }, alpha: wgpu::BlendComponent::REPLACE }), write_mask: wgpu::ColorWrites::RED })] }),
            multiview_mask: None, cache: None,
        }));
        #[cfg(target_arch = "wasm32")]
        let mask_twin: Option<wgpu::RenderPipeline> = { let _ = mask_fs; None };
        if let Some(e) = crate::gpu::pop_error_scope(scope) {
            let msg = format!("{}: {e}", d.label);
            self.last_error = Some(msg.clone());
            return Err(msg);
        }
        if let Some(t) = mask_twin { self.responsive_pipes.insert(self.pipes.len(), t); } // STORM TAA
        self.pipes.push(GpuPipe { pipe, layouts, desc_groups: d.groups.clone(), translucent: d.translucent, instanced: d.vertex_buffers.iter().any(|b| b.instance), separate: d.translucent && d.separate_translucency, shadow: false, scene_lit: false });
        fx_pso::record(0, d);
        self.pipe_cache.insert(key, FxPipeId(self.pipes.len() - 1)); // PIPELINE DEDUP
        Ok(FxPipeId(self.pipes.len() - 1))
    }
    /// FX RECEIVE (2026-10-05): the renderer's scene globals layout (group 0 of the scene shaders) and the shared WGSL (`common.wgsl`, `lit_common.wgsl`), so
    /// opaque / masked lit FX materials are lit like the static meshes ([`Self::create_scene_lit_pipeline`]). UE4.16 draws them in the deferred base pass
    /// (BasePassRendering.cpp) and lights them from the GBuffer with the projected sun shadow, the light function, the DFAO-occluded sky light and
    /// ReflectionApplyPS; the port's lit passes do that forward in one shader (lit_common.wgsl), which these pipelines now share.
    pub fn set_scene_lighting(&mut self, bgl_globals: &wgpu::BindGroupLayout, common: &str, lit_common: &str) {
        self.scene = Some(SceneLit { bgl_globals: bgl_globals.clone(), prelude: scene_lit_prelude_wgsl(common, lit_common) });
    }
    /// FX RECEIVE: the WGSL a scene-lit FX program is assembled with (fn-fx `program::assemble_scene_lit`), when the renderer installed it.
    pub fn scene_lit_prelude(&self) -> Option<&str> { self.scene.as_ref().map(|s| s.prelude.as_str()) }
    /// FX RECEIVE: a pipeline of an opaque / masked lit FX material drawn in the renderer's `scene` pass ([`Self::draw_scene_lit`]): `d.groups` (at most 3) are the
    /// program's resources, group [`FX_SCENE_GROUP`] the renderer's globals; the colour targets are the lit passes' (HDR + the reflection inputs,
    /// `crate::reflection::lit_targets`) because the fragment entry returns lit_common.wgsl `LitOut`. Depth / cull / vertex state as [`Self::create_pipeline`].
    pub fn create_scene_lit_pipeline(&mut self, d: &FxPipelineDesc) -> Result<FxPipeId, String> {
        let Some(scene) = self.scene.as_ref() else { return Err(format!("{}: no scene lighting installed (FxRenderer::set_scene_lighting)", d.label)) };
        if d.translucent || d.blend != FxBlend::Opaque { return Err(format!("{}: a scene-lit pipeline is opaque / masked only", d.label)); }
        if d.groups.len() > FX_SCENE_GROUP as usize { return Err(format!("{}: a scene-lit pipeline has at most {FX_SCENE_GROUP} program groups", d.label)); }
        let bgl_globals = scene.bgl_globals.clone();
        let backdrop = self.building_backdrop; // BACKDROP REAL
        let key = Self::pipe_key(if backdrop { 7 } else { 1 }, d); // PIPELINE DEDUP
        if let Some(id) = self.cached_pipe(&key) { return Ok(id); }
        #[cfg(target_arch = "wasm32")] self.web_stage_limits(d, Some(FX_SCENE_GROUP))?; // BROWSER PORT
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self.module_for(&d.label, &d.wgsl);
        let mut layouts: Vec<wgpu::BindGroupLayout> = d.groups.iter().map(|g| self.group_layout(&d.label, g)).collect();
        while layouts.len() < FX_SCENE_GROUP as usize { layouts.push(self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fx scene-lit empty"), entries: &[] })); }
        let mut layout_refs: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().map(Some).collect();
        layout_refs.push(Some(&bgl_globals));
        let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(&d.label), bind_group_layouts: &layout_refs, immediate_size: 0 });
        let attrs: Vec<Vec<wgpu::VertexAttribute>> = d.vertex_buffers.iter().map(|b| b.attrs.iter().map(|a| wgpu::VertexAttribute { format: a.format.wgpu(), offset: a.offset as u64, shader_location: a.location }).collect()).collect();
        let vbs: Vec<Option<wgpu::VertexBufferLayout>> = d.vertex_buffers.iter().zip(&attrs).map(|(b, a)| Some(wgpu::VertexBufferLayout { array_stride: b.stride as u64, step_mode: if b.instance { wgpu::VertexStepMode::Instance } else { wgpu::VertexStepMode::Vertex }, attributes: a })).collect();
        let (cull_mode, front_face) = match d.cull { FxCull::None => (None, wgpu::FrontFace::Ccw), FxCull::BackCw => (Some(wgpu::Face::Front), wgpu::FrontFace::Ccw), FxCull::BackCcw => (Some(wgpu::Face::Back), wgpu::FrontFace::Ccw) };
        // PREPASS (2026-10-06): a masked program assembled with UE's pre-pass split (fn-fx program.rs `fs_depth` / `fs_main_eq`) gets a depth-only twin drawn in the
        // renderer's depth pre-pass (the clip, depth written, DepthNearOrEqual) and its colour pipeline draws the surviving surface with CF_Equal and no depth write
        // and no clip, as UE's base pass does after the masked pre-pass (cooked r.EarlyZPass 2 / r.EarlyZPassOnlyMaterialMasking 1, BasePassRendering.cpp:308-361)
        // FULL PREPASS (2026-10-06): UE4.16 1.8 runs a FULL pre-pass (cooked r.SelectiveBasePassOutputs 1 / r.StencilForLODDither 1 -> ShouldForceFullDepthPass ->
        // DDM_AllOpaque + bEarlyZPassMovable, DeferredShadingRenderer.cpp:146-182; read-only base-pass depth, :773), so OPAQUE programs are in it as well: fn-fx emits
        // `fs_main_eq` for every scene-pass program; the twin runs `fs_depth` when the module has one (a masked clip, or a base-pass program that discards by itself)
        // and no pixel stage otherwise (the position-only depth of an opaque material, DepthRendering.cpp:652-669).
        let prepass = !backdrop && d.depth.test && d.depth.write && d.wgsl.contains("fn fs_main_eq("); // BACKDROP REAL: no pre-pass in the backdrop pass
        let twin_fs = d.wgsl.contains("fn fs_depth(");
        let (fs_entry, depth_write, depth_compare) = if prepass { ("fs_main_eq", false, wgpu::CompareFunction::Equal) } else { (d.fs_entry.as_str(), d.depth.write, if d.depth.test { wgpu::CompareFunction::LessEqual } else { wgpu::CompareFunction::Always }) };
        let pipe = self.render_pipeline(FxSlot::Main, &wgpu::RenderPipelineDescriptor {
            label: Some(&d.label), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &vbs },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(depth_write), depth_compare: Some(depth_compare), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(fs_entry), compilation_options: Default::default(), targets: &crate::reflection::lit_targets(self.hdr_format) }),
            multiview_mask: None, cache: None,
        });
        let depth_twin = prepass.then(|| self.render_pipeline(FxSlot::Depth, &wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{} depth", d.label)), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &vbs },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: twin_fs.then(|| wgpu::FragmentState { module: &module, entry_point: Some("fs_depth"), compilation_options: Default::default(), targets: &[] }),
            multiview_mask: None, cache: None,
        }));
        // VELOCITY (2026-10-05): the base pass of these materials writes the scene velocity (r.BasePassOutputsVelocity = 1: GBuffer MRT4 = the program's
        // SV_Target4, BasePassPixelShader.usf:796-810 / :1068). The port keeps a separate velocity pass, so a module with fn-fx's `fs_velocity` entry (the same
        // translated pixel program, returning its SV_Target4) gets a twin pipeline: same module / layout / vertex state / culling, the velocity target, depth
        // tested against the finished scene depth (same vertex program -> the same depth), no depth write. Drawn by [`Self::draw_velocity`].
        let velocity = (!backdrop && d.wgsl.contains("fn fs_velocity(")).then(|| self.render_pipeline(FxSlot::Velocity, &wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{} velocity", d.label)), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &vbs },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(false), depth_compare: Some(if d.depth.test { wgpu::CompareFunction::LessEqual } else { wgpu::CompareFunction::Always }), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_velocity"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: crate::taa::VELOCITY_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
            multiview_mask: None, cache: None,
        }));
        if let Some(e) = crate::gpu::pop_error_scope(scope) {
            let msg = format!("{}: {e}", d.label);
            self.last_error = Some(msg.clone());
            return Err(msg);
        }
        layouts.truncate(d.groups.len());
        self.pipes.push(GpuPipe { pipe, layouts, desc_groups: d.groups.clone(), translucent: false, instanced: d.vertex_buffers.iter().any(|b| b.instance), separate: false, shadow: false, scene_lit: true });
        if let Some(v) = velocity { self.velocity_pipes.insert(self.pipes.len() - 1, v); } // VELOCITY
        if let Some(t) = depth_twin { self.depth_pipes.insert(self.pipes.len() - 1, t); } // PREPASS
        fx_pso::record(if backdrop { 7 } else { 1 }, d);
        self.pipe_cache.insert(key, FxPipeId(self.pipes.len() - 1)); // PIPELINE DEDUP
        if backdrop { self.backdrop_pipes.insert(self.pipes.len() - 1); } // BACKDROP REAL
        Ok(FxPipeId(self.pipes.len() - 1))
    }
    /// BACKDROP REAL: while on, scene-lit pipelines are built as backdrop pipelines (see `building_backdrop`).
    pub fn set_building_backdrop(&mut self, on: bool) { self.building_backdrop = on; }
    /// PREPASS (2026-10-06): records the frame's scene-lit draws that have a depth twin (masked materials: their FDepthOnlyPS clip; FULL PREPASS: opaque ones too,
    /// position only) into the renderer's depth
    /// pre-pass (`globals` at [`FX_SCENE_GROUP`]: the twin shares the colour pipeline's layout). Their colour pipelines test `Equal` against this depth, so the
    /// pre-pass must record exactly the draws [`Self::draw_scene_lit`] records afterwards. Call after [`Self::prepare`].
    pub fn draw_scene_lit_depth<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, globals: &'a wgpu::BindGroup) {
        let mut st = PassState::default();
        for (n, d) in self.frame.draws.iter().enumerate() {
            let Some(t) = self.depth_pipes.get(&d.pipe.0) else { continue };
            if self.backdrop_pipes.contains(&d.pipe.0) { continue; } // BACKDROP REAL
            // PERF (2026-10-07): repeated state skipped, the globals set after each pipeline change (draw_one_cached)
            self.draw_one_cached(pass, n, Some(t), &mut st, Some(globals));
        }
    }
    /// VELOCITY (2026-10-05): records the frame's scene-lit draws that have a velocity twin into the velocity pass (`globals` at [`FX_SCENE_GROUP`]); each
    /// writes UE's base-pass velocity output of its own program (`EncodeVelocityToTexture(Calculate2DVelocity(..)) * (1 - SkipOutputVelocity)`).
    pub fn draw_velocity<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, globals: &'a wgpu::BindGroup) {
        let mut st = PassState::default();
        for (n, d) in self.frame.draws.iter().enumerate() {
            let Some(v) = self.velocity_pipes.get(&d.pipe.0) else { continue };
            if self.backdrop_pipes.contains(&d.pipe.0) { continue; } // BACKDROP REAL: no velocity (static, far)
            if !self.pipes.get(d.pipe.0).map_or(false, |p| p.scene_lit) { continue; }
            // PERF (2026-10-07): see draw_scene_lit_depth
            self.draw_one_cached(pass, n, Some(v), &mut st, Some(globals));
        }
    }
    /// VELOCITY: the frame has draws with a velocity twin.
    pub fn has_velocity_draws(&self) -> bool { self.frame.draws.iter().any(|d| self.velocity_pipes.contains_key(&d.pipe.0)) }
    /// VELOCITY: this frame's velocity state; the `View` buffers' previous-view slots are filled from it in [`Self::prepare`] ([`crate::taa::ViewHistory`]).
    pub fn set_velocity_state(&mut self, s: crate::taa::VelocityState) { self.view_history.begin_frame(s); }
    /// GRASS MOTION (2026-10-07): this frame's TAA jitter in NDC (0 without TAA); every registered `View` buffer gets it in [`Self::prepare`].
    pub fn set_view_jitter(&mut self, j: [f32; 2]) { self.view_jitter = j; }
    pub fn set_view_size(&mut self, size: (u32, u32)) { self.view_size = size; }
    /// FX RECEIVE: the frame has draws of scene-lit pipelines.
    pub fn has_scene_lit_draws(&self) -> bool { self.frame.draws.iter().any(|d| self.pipes.get(d.pipe.0).map_or(false, |p| p.scene_lit)) }
    /// FX RECEIVE: records the scene-lit draws (submission order) into the renderer's scene pass (HDR + reflection targets, depth written), `globals` = the
    /// scene globals bind group, set at [`FX_SCENE_GROUP`]. Call after [`Self::prepare`].
    pub fn draw_scene_lit<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, globals: &'a wgpu::BindGroup) {
        let mut st = PassState::default();
        for (n, d) in self.frame.draws.iter().enumerate() {
            if !self.pipes.get(d.pipe.0).map_or(false, |p| p.scene_lit) { continue; }
            if self.backdrop_pipes.contains(&d.pipe.0) { continue; } // BACKDROP REAL: drawn in the backdrop pass
            // PERF (2026-10-07): see draw_scene_lit_depth
            self.draw_one_cached(pass, n, None, &mut st, Some(globals));
        }
    }
    /// BACKDROP REAL (2026-10-07): marks a scene-lit pipeline as a backdrop pipeline (its draws use a View buffer with the backdrop projection, near 20 m /
    /// far 2e6 m, and go to the renderer's backdrop pass instead of the scene pass / pre-pass / velocity pass).
    pub fn mark_backdrop_pipe(&mut self, pipe: FxPipeId) { self.backdrop_pipes.insert(pipe.0); }
    /// BACKDROP REAL: records the frame's backdrop draws into the backdrop pass (its own depth target, HDR + reflection targets), LessEqual + depth write
    /// (the backdrop pass has no pre-pass; pipelines built under [`Self::set_building_backdrop`]).
    pub fn draw_backdrop_lit<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, globals: &'a wgpu::BindGroup) {
        if self.backdrop_pipes.is_empty() { return; }
        let mut st = PassState::default();
        for (n, d) in self.frame.draws.iter().enumerate() {
            if !self.backdrop_pipes.contains(&d.pipe.0) { continue; }
            // PERF (2026-10-07): see draw_scene_lit_depth
            self.draw_one_cached(pass, n, None, &mut st, Some(globals));
        }
    }
    fn group_layout(&self, label: &str, g: &[FxLayoutEntry]) -> wgpu::BindGroupLayout {
        let entries: Vec<wgpu::BindGroupLayoutEntry> = g.iter().map(|e| {
            let mut vis = wgpu::ShaderStages::empty();
            if e.vertex { vis |= wgpu::ShaderStages::VERTEX; }
            if e.fragment { vis |= wgpu::ShaderStages::FRAGMENT; }
            let ty = match e.ty {
                FxBindTy::Uniform => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                FxBindTy::Tex2D { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                FxBindTy::Tex3D { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false },
                FxBindTy::Cube { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::Cube, multisampled: false },
                FxBindTy::SceneDepth => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                FxBindTy::Sampler { filtering } => wgpu::BindingType::Sampler(if filtering { wgpu::SamplerBindingType::Filtering } else { wgpu::SamplerBindingType::NonFiltering }),
                FxBindTy::StorageRead => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
            };
            wgpu::BindGroupLayoutEntry { binding: e.binding, visibility: vis, ty, count: None }
        }).collect();
        self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries: &entries })
    }
    /// FX SHADOW: a depth-only pipeline for the sun's cascades from a description whose `wgsl` holds a shadow-depth program pair (fn-fx `assemble_shadow`:
    /// `vs_entry` writes `fx_shadow_depth_outputs`, [`FX_SHADOW_WGSL`]); `fs_entry` empty = no pixel shader (UE's null pixel shader for opaque materials,
    /// ShadowDepthRendering.cpp:813), else the masked material's `TShadowDepthPS` (clip only, no colour target). Groups 0..2 come from `d.groups`, group 3 is
    /// the cascade's LightViewProj. Same depth state as the renderer's shadow pipelines: `Less`, no hardware bias (the bias is in the stored depth). Culling as the
    /// base pass: `d.cull` (TwoSided -> none, ShadowDepthRendering.cpp:1059; `ReverseCulling` of a mirrored primitive -> the reversed mode, :686, MIRRORFIX
    /// convention of [`FxCull`]); D3D11 does not reverse the shadow cull (`bPlatformReversesCulling` false, :843).
    /// BROWSER PORT (wasm32): the per-stage sampler / sampled-texture counts of `d`'s layout (its groups' entries, plus the declarations of the shared
    /// group `extra` in its WGSL) against the device's limits. WebGPU in Chromium allows 16 of each per stage (D3D11 allows 16 samplers / 128 SRVs, so a
    /// few cooked materials with the scene prelude need up to 18); an over-limit pipeline is not rejected synchronously on the web (validation is
    /// reported asynchronously, `gpu::pop_error_scope`) and every command buffer that draws it would be invalid (the whole frame lost). Such a program is
    /// refused here instead: its caller logs it and skips its draws, as natively for a program that fails validation.
    #[cfg(target_arch = "wasm32")]
    fn web_stage_limits(&self, d: &FxPipelineDesc, extra: Option<u32>) -> Result<(), String> {
        let (mut vs, mut fs) = ([0u32; 2], [0u32; 2]); // [samplers, textures]
        for e in d.groups.iter().flatten() {
            let k = match e.ty { FxBindTy::Sampler { .. } => 0, FxBindTy::Tex2D { .. } | FxBindTy::Tex3D { .. } | FxBindTy::Cube { .. } | FxBindTy::SceneDepth => 1, _ => continue };
            if e.vertex { vs[k] += 1; }
            if e.fragment { fs[k] += 1; }
        }
        if let Some(g) = extra {
            let tag = format!("@group({g}) ");
            for l in d.wgsl.lines().map(str::trim_start).filter(|l| l.starts_with(&tag) && l.contains(" var ")) {
                if l.contains(": sampler") { fs[0] += 1; } else if l.contains(": texture_") { fs[1] += 1; }
            }
        }
        let lim = self.device.limits();
        let (ms, mt) = (lim.max_samplers_per_shader_stage, lim.max_sampled_textures_per_shader_stage);
        for (stage, c) in [("vertex", vs), ("fragment", fs)] {
            if c[0] > ms || c[1] > mt { return Err(format!("{}: {stage} stage binds {} samplers / {} textures, over this browser's WebGPU limits ({ms} / {mt}): not built", d.label, c[0], c[1])); }
        }
        Ok(())
    }
    pub fn create_shadow_pipeline(&mut self, d: &FxPipelineDesc) -> Result<FxPipeId, String> {
        if d.groups.len() > FX_SHADOW_GROUP as usize { return Err(format!("{}: a shadow pipeline has at most {FX_SHADOW_GROUP} program groups", d.label)); }
        let key = Self::pipe_key(2, d); // PIPELINE DEDUP
        if let Some(id) = self.cached_pipe(&key) { return Ok(id); }
        #[cfg(target_arch = "wasm32")] self.web_stage_limits(d, None)?; // BROWSER PORT
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self.module_for(&d.label, &d.wgsl);
        let mut layouts: Vec<wgpu::BindGroupLayout> = d.groups.iter().map(|g| {
            let entries: Vec<wgpu::BindGroupLayoutEntry> = g.iter().map(|e| {
                let mut vis = wgpu::ShaderStages::empty();
                if e.vertex { vis |= wgpu::ShaderStages::VERTEX; }
                if e.fragment { vis |= wgpu::ShaderStages::FRAGMENT; }
                let ty = match e.ty {
                    FxBindTy::Uniform => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    FxBindTy::Tex2D { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    FxBindTy::Tex3D { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false },
                    FxBindTy::Cube { filterable } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: wgpu::TextureViewDimension::Cube, multisampled: false },
                    FxBindTy::SceneDepth => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    FxBindTy::Sampler { filtering } => wgpu::BindingType::Sampler(if filtering { wgpu::SamplerBindingType::Filtering } else { wgpu::SamplerBindingType::NonFiltering }),
                    FxBindTy::StorageRead => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                };
                wgpu::BindGroupLayoutEntry { binding: e.binding, visibility: vis, ty, count: None }
            }).collect();
            self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(&d.label), entries: &entries })
        }).collect();
        while layouts.len() < FX_SHADOW_GROUP as usize { layouts.push(self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fx shadow empty"), entries: &[] })); }
        let mut layout_refs: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().map(Some).collect();
        layout_refs.push(Some(&self.bgl_shadow_vp));
        let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(&d.label), bind_group_layouts: &layout_refs, immediate_size: 0 });
        let attrs: Vec<Vec<wgpu::VertexAttribute>> = d.vertex_buffers.iter().map(|b| b.attrs.iter().map(|a| wgpu::VertexAttribute { format: a.format.wgpu(), offset: a.offset as u64, shader_location: a.location }).collect()).collect();
        let vbs: Vec<Option<wgpu::VertexBufferLayout>> = d.vertex_buffers.iter().zip(&attrs).map(|(b, a)| Some(wgpu::VertexBufferLayout { array_stride: b.stride as u64, step_mode: if b.instance { wgpu::VertexStepMode::Instance } else { wgpu::VertexStepMode::Vertex }, attributes: a })).collect();
        let (cull_mode, front_face) = match d.cull { FxCull::None => (None, wgpu::FrontFace::Ccw), FxCull::BackCw => (Some(wgpu::Face::Front), wgpu::FrontFace::Ccw), FxCull::BackCcw => (Some(wgpu::Face::Back), wgpu::FrontFace::Ccw) };
        let pipe = self.render_pipeline(FxSlot::Main, &wgpu::RenderPipelineDescriptor {
            label: Some(&d.label), layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &vbs },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode, front_face, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: self.depth_format, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: wgpu::DepthBiasState::default() }),
            multisample: Default::default(),
            fragment: if d.fs_entry.is_empty() { None } else { Some(wgpu::FragmentState { module: &module, entry_point: Some(&d.fs_entry), compilation_options: Default::default(), targets: &[] }) },
            multiview_mask: None, cache: None,
        });
        if let Some(e) = crate::gpu::pop_error_scope(scope) {
            let msg = format!("{}: {e}", d.label);
            self.last_error = Some(msg.clone());
            return Err(msg);
        }
        layouts.truncate(d.groups.len());
        self.pipes.push(GpuPipe { pipe, layouts, desc_groups: d.groups.clone(), translucent: false, instanced: false, separate: false, shadow: true, scene_lit: false });
        fx_pso::record(2, d);
        self.pipe_cache.insert(key, FxPipeId(self.pipes.len() - 1)); // PIPELINE DEDUP
        Ok(FxPipeId(self.pipes.len() - 1))
    }
    /// FX SHADOW: the renderer's per-cascade `LightViewProj` buffers (shadow.wgsl layout: mat4 + depth bias vec4), bound as [`FX_SHADOW_GROUP`] of the shadow draws.
    pub fn set_shadow_vp(&mut self, bufs: &[wgpu::Buffer]) {
        self.shadow_vp_bgs = bufs.iter().map(|b| self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("fx shadow vp"), layout: &self.bgl_shadow_vp, entries: &[wgpu::BindGroupEntry { binding: 0, resource: b.as_entire_binding() }] })).collect();
    }
    /// PERF (2026-10-06): this frame's cascade LightViewProj matrices and small-caster cull inputs (camera, `r.Shadow.RadiusThreshold`, LOD distance factor) for the
    /// per-cascade culling of shadow draws that carry bounds ([`FxShadowDraw::bounds`]); the same tests as statics.rs `prepare`.
    pub fn set_shadow_cull(&mut self, cascades: &[crate::lighting::Cascade], eye: glam::Vec3, radius_threshold: f32, lod_distance_factor: f32) {
        self.shadow_cull = cascades.iter().map(|c| (crate::static_mesh::frustum_planes(&c.light_view_proj), *c)).collect();
        self.shadow_small = Some((eye, radius_threshold, lod_distance_factor));
    }
    /// PERF: whether a shadow draw goes into cascade `cascade` (see [`Self::set_shadow_cull`]).
    fn shadow_in_cascade(&self, d: &FxShadowDraw, cascade: usize) -> bool {
        // INSTANCING: an instanced draw goes in when any of its instances does (`shadow_placed`, filled by `prepare`)
        if d.instances.is_some() { return true; }
        let Some(b) = d.bounds else { return true };
        self.sphere_in_cascade(b, cascade)
    }
    /// PERF: the per-cascade caster test of one world bounding sphere (see [`Self::set_shadow_cull`]).
    fn sphere_in_cascade(&self, b: [f32; 4], cascade: usize) -> bool { sphere_in_cascade_of(&self.shadow_cull, self.shadow_small, b, cascade) }
    /// PERF (2026-10-08): the last frame's `r.Shadow.RadiusThreshold` of the small-caster cull (None before the first shadowed frame): producers drop casters the
    /// renderer would drop from every cascade before submitting them (statics_render).
    pub fn shadow_radius_threshold(&self) -> Option<f32> { self.shadow_small.map(|s| s.1) }
    /// PERF (report only, `FN_SHADOW_STATS=1`): this frame's shadow draws of cascade `cascade` as [`Self::draw_shadow`] records them:
    /// [(draws, triangles); 2] for draws with bounds (placed statics) and without (building pieces, pickups). INSTANCING: an instanced draw counts its
    /// instances in the cascade.
    pub fn shadow_cascade_stats(&self, cascade: usize) -> [(u64, u64); 2] {
        let mut out = [(0u64, 0u64); 2];
        if !fx_shadows_enabled() { return out; }
        for (n, d) in self.frame.shadow_draws.iter().enumerate() {
            if !self.shadow_in_cascade(d, cascade) { continue; }
            let tris = match &d.geom { FxGeom::Mesh(m) => self.meshes.get(m.0).map_or(0, |m| m.index_count), FxGeom::MeshRange { index_count, .. } => *index_count, FxGeom::Dynamic { .. } => continue } as u64 / 3;
            let k = (d.bounds.is_none() && d.instances.is_none()) as usize;
            let count = if d.instances.is_some() { self.shadow_placed.get(n).and_then(|c| c.get(cascade)).map_or(0, |c| c.1 as u64) } else { 1 };
            out[k].0 += count; out[k].1 += tris * count;
        }
        out
    }
    /// FX SHADOW: the frame has shadow-depth draws (`FN_FX_SHADOWS=0`: debug switch for before / after comparisons, no FX shadow draws).
    pub fn has_shadow_draws(&self) -> bool { fx_shadows_enabled() && !self.frame.shadow_draws.is_empty() }
    /// FX SHADOW: the distance-field objects of the frame (see [`FxFrame::distance_field_objects`]).
    pub fn distance_field_objects(&self) -> &[FxDfInstance] { &self.frame.distance_field_objects }
    /// FX SHADOW: records the frame's shadow draws into cascade `cascade`'s depth pass (viewport already set; sets every group itself). Call after [`Self::prepare`].
    pub fn draw_shadow<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, cascade: usize) {
        let Some(vp) = self.shadow_vp_bgs.get(cascade) else { return };
        if !fx_shadows_enabled() { return; }
        let mut st = PassState::default(); // PERF (2026-10-07): repeated pipeline / bind group / mesh sets skipped (as draw_one_cached)
        for (n, d) in self.frame.shadow_draws.iter().enumerate() {
            if !self.shadow_in_cascade(d, cascade) { continue; } // PERF: per-cascade caster culling
            // INSTANCING: this cascade's instances of the draw (uploaded by `prepare`), none = no draw
            let inst = if d.instances.is_some() { match self.shadow_placed.get(n).and_then(|c| c.get(cascade)) { Some(&(at, k)) if k > 0 => Some((at, k)), _ => continue } } else { None };
            #[cfg(target_arch = "wasm32")]
            if self.web.skip(d.pipe.0) { continue; } // BROWSER PORT: still compiling
            let Some(p) = self.pipes.get(d.pipe.0).filter(|p| p.shadow) else { continue };
            let mut ok = true;
            let mut bgs = vec![];
            for (g, b) in &d.binds { match self.binds.get(b.0).and_then(|r| r.built.as_ref()) { Some((_, bg)) => bgs.push((*g, b.0, bg)), None => ok = false } }
            if !ok { continue; }
            let (mesh, range) = match &d.geom { FxGeom::Mesh(m) => (*m, None), FxGeom::MeshRange { mesh, first_index, index_count } => (*mesh, Some((*first_index, *index_count))), FxGeom::Dynamic { .. } => continue };
            let Some(m) = self.meshes.get(mesh.0) else { continue };
            let key = &p.pipe as *const wgpu::RenderPipeline as usize;
            if st.pipe != key {
                pass.set_pipeline(&p.pipe); st.change_pipe(key);
                let vk = vp as *const wgpu::BindGroup as usize;
                if st.globals != vk { pass.set_bind_group(FX_SHADOW_GROUP, vp, &[]); st.globals = vk; st.binds[FX_SHADOW_GROUP as usize] = 0; }
            }
            for (g, id, bg) in bgs { let gi = g as usize; if gi < st.binds.len() && st.binds[gi] == id + 1 { continue; } pass.set_bind_group(g, bg, &[]); if gi < st.binds.len() { st.binds[gi] = id + 1; } if g == FX_SHADOW_GROUP { st.globals = 0; } }
            let (first, count) = range.unwrap_or((0, m.index_count));
            if st.mesh != mesh.0 + 1 { pass.set_vertex_buffer(0, m.vbs()); pass.set_index_buffer(m.ibs(), wgpu::IndexFormat::Uint32); st.mesh = mesh.0 + 1; }
            let instances = match inst { Some((at, k)) => { pass.set_vertex_buffer(1, self.inst_vb.slice(at..at + 4 * k as u64)); k } None => 1 };
            pass.draw_indexed(first..(first + count).min(m.index_count), 0, 0..instances);
        }
    }
    /// Bind group `group` of `pipe`. `SceneDepth` entries are re-created whenever the renderer's targets are (the id stays valid).
    pub fn create_bind_group(&mut self, pipe: FxPipeId, group: u32, entries: Vec<FxBindEntry>) -> FxBindId {
        let uses_depth = entries.iter().any(|e| matches!(e.res, FxRes::SceneDepth));
        let uses_volume = entries.iter().any(|e| matches!(e.res, FxRes::TranslucencyVolume(_) | FxRes::FogVolume)); // FOG INPUTS: shares the volume generation
        let bi = self.binds.len() as u32;
        for e in &entries { if let FxRes::Texture(t) = e.res { let v = self.tex_binds.entry(t.0).or_default(); if v.last() != Some(&bi) { v.push(bi); } } }
        self.binds.push(BindGroupRec { pipe, group, entries, built: None, uses_depth, uses_volume });
        self.bind_pending.push(bi);
        if uses_depth || uses_volume { self.bind_dep.push(bi); }
        FxBindId(self.binds.len() - 1)
    }
    /// TRANSLUCENCY VOLUME: this frame's volume textures (`crate::translucent_lighting`, [`FxRes::TranslucencyVolume`] order); bind groups that read them are rebuilt
    /// by the next [`Self::prepare`]. Call when the volume textures are (re)created.
    pub fn set_translucency_volume(&mut self, views: [wgpu::TextureView; 4]) {
        self.volume = Some(views);
        self.volume_gen += 1;
    }
    /// FOG INPUTS (2026-10-05): a `$Globals` buffer whose program binds height-fog values at `slots` (fn-fx, from the serialized `FHeightFogShaderParameters`);
    /// [`Self::prepare`] writes this frame's values there after the frame's own writes (`HeightFogParameters.Set`, BasePassRendering.h:377).
    pub fn register_height_fog(&mut self, buf: FxBufId, slots: Vec<FxFogSlot>) {
        if slots.is_empty() { return; }
        if self.fog_targets.is_empty() { eprintln!("[fx] height-fog inputs bound: first $Globals buffer with {} fog values (FogRendering.h:26 Set)", slots.len()); }
        if let Some(p) = self.fog_patched.get_mut(buf.0) { *p = false; }
        self.fog_targets.insert(buf, slots);
        self.set_fog_flag(buf, 1);
        self.fog_gen += 1;
    }
    fn fog_flag(&self, b: FxBufId) -> u8 { self.fog_flags.get(b.0).copied().unwrap_or(0) }
    fn set_fog_flag(&mut self, b: FxBufId, bit: u8) { if self.fog_flags.len() <= b.0 { self.fog_flags.resize(b.0 + 1, 0); } self.fog_flags[b.0] |= bit; }
    /// FOG INPUTS: a `View` uniform buffer of FX programs: [`Self::prepare`] writes its volumetric-fog slots ([`VIEW_VOLUMETRIC_FOG_SLOT`]..+3).
    pub fn register_view_buffer(&mut self, buf: FxBufId) { self.fog_views.insert(buf); self.set_fog_flag(buf, 2); }
    /// MATERIAL PARAMETERS: the shared uniform buffer of the parameter collection `path` (object path), if one exists.
    pub fn collection_buffer(&self, path: &str) -> Option<FxBufId> { self.collections.get(path).copied() }
    /// MATERIAL PARAMETERS: makes `buf` the shared uniform buffer of the parameter collection `path` (a caller that writes the collection itself, e.g. the storm's
    /// `MPC_SafeZoneClouds`, registers its buffer so every other program reading that collection binds the same one).
    pub fn register_collection_buffer(&mut self, path: &str, buf: FxBufId) { self.collections.insert(path.to_owned(), buf); }
    /// MATERIAL PARAMETERS: the registered collection buffers (path, buffer).
    pub fn collection_buffers(&self) -> impl Iterator<Item = (&str, FxBufId)> { self.collections.iter().map(|(p, b)| (p.as_str(), *b)) }
    /// FOG INPUTS: this frame's fog (call before [`Self::prepare`]): the values, and the volumetric fog's integrated scattering volume with the serial of the
    /// volume it views (`None` = the volumetric fog did not run: the black volume). Bind groups that read it are rebuilt when the volume changes.
    pub fn set_view_fog(&mut self, fog: FxViewFog, volume: Option<(wgpu::TextureView, u64)>) {
        self.view_fog = fog;
        let (old, new) = (self.fog_volume.as_ref().map(|v| v.1), volume.as_ref().map(|v| v.1));
        if old != new { self.fog_volume = volume; self.volume_gen += 1; }
    }
    /// FOG INPUTS: this frame's fog values ([`Self::set_view_fog`]).
    pub fn view_fog(&self) -> &FxViewFog { &self.view_fog }
    fn sampler(&mut self, d: FxSamplerDesc) -> &wgpu::Sampler {
        let device = &self.device;
        self.samplers.entry(d).or_insert_with(|| {
            let f = |x: FxFilter| match x { FxFilter::Nearest => wgpu::FilterMode::Nearest, FxFilter::Linear => wgpu::FilterMode::Linear };
            let a = match d.address { FxAddress::Wrap => wgpu::AddressMode::Repeat, FxAddress::Clamp => wgpu::AddressMode::ClampToEdge, FxAddress::Mirror => wgpu::AddressMode::MirrorRepeat };
            let aniso = if d.non_filtering() { 1 } else { d.aniso.max(1) };
            // BROWSER PORT: WebGPU rejects anisotropy unless min, mag AND mip filters are all linear (an invalid sampler invalidates every bind group
            // and frame that uses it); clamp to 1 there, as gun_material.rs does for its samplers
            #[cfg(target_arch = "wasm32")]
            let aniso = if matches!((d.mag, d.min, d.mip), (FxFilter::Linear, FxFilter::Linear, FxFilter::Linear)) { aniso } else { 1 };
            device.create_sampler(&wgpu::SamplerDescriptor { label: Some("fx sampler"), address_mode_u: a, address_mode_v: a, address_mode_w: a, mag_filter: f(d.mag), min_filter: f(d.min), mipmap_filter: match d.mip { FxFilter::Nearest => wgpu::MipmapFilterMode::Nearest, FxFilter::Linear => wgpu::MipmapFilterMode::Linear }, anisotropy_clamp: aniso, ..Default::default() })
        })
    }

    // ---- per frame ----------------------------------------------------------------------------------------------------------
    pub fn set_frame(&mut self, frame: FxFrame) { self.base_len = (frame.buffer_writes.len(), frame.draws.len()); self.base_shadow = (frame.shadow_draws.len(), frame.distance_field_objects.len()); self.frame = frame; }
    /// Adds draws and buffer writes to the frame already handed over (a second producer: the pickup effects, drawn after `set_frame`).
    pub fn append_frame(&mut self, mut frame: FxFrame) { self.frame.buffer_writes.truncate(self.base_len.0); self.frame.draws.truncate(self.base_len.1); self.frame.shadow_draws.truncate(self.base_shadow.0); self.frame.distance_field_objects.truncate(self.base_shadow.1); self.frame.append(&mut frame); }
    /// `translucent` false: the masked pass; true: the STANDARD translucency pass (separate translucency draws are [`Self::has_separate`]).
    pub fn has_draws(&self, translucent: bool) -> bool { self.frame.draws.iter().any(|d| self.pipes.get(d.pipe.0).map_or(false, |p| p.translucent == translucent && !(translucent && p.separate) && !p.scene_lit)) } // FX RECEIVE: scene-lit draws go to the scene pass
    /// The frame has separate translucency draws (the `fx separate translucency` pass + its recombine run).
    pub fn has_separate(&self) -> bool { self.frame.draws.iter().any(|d| self.pipes.get(d.pipe.0).map_or(false, |p| p.separate)) }
    pub fn any(&self) -> bool { !self.frame.draws.is_empty() }

    /// Uploads the frame's buffers / dynamic geometry and (re)builds the bind groups that reference the scene depth. Call once per rendered frame before the passes.
    pub fn prepare(&mut self, depth_view: &wgpu::TextureView, depth_gen: u64) {
        #[cfg(target_arch = "wasm32")]
        self.web_poll(); // BROWSER PORT: install the pipelines that finished compiling
        let mut frame = std::mem::take(&mut self.frame);
        // VELOCITY: the View buffers' PrevTranslatedWorldToClip / PrevPreViewTranslation = last frame's own TranslatedWorldToClip / PreViewTranslation (taa.rs ViewHistory)
        let flags = std::mem::take(&mut self.fog_flags);
        let flag = |b: &FxBufId| flags.get(b.0).copied().unwrap_or(0);
        for (b, bytes) in frame.buffer_writes.iter_mut() { if flag(b) & 2 != 0 { crate::taa::apply_view_size(bytes, self.view_size); crate::taa::apply_view_jitter(bytes, self.view_jitter); self.view_history.patch(b.0, bytes); } } // GRASS MOTION: jitter first (the history keeps jittered matrices)
        // PERF (2026-10-06): every write below goes through `put`: queue.write_buffer, or (staged uploads, the renderer) appended to this frame's one staging
        // buffer and copied by `encode_uploads` in the same order
        self.stage_bytes.clear();
        self.stage_copies.clear();
        self.stage_ready = false;
        for t in self.retired.drain(..) { t.destroy(); }
        self.drain_pool_frees();
        FRAME_UPLOAD.store(0, std::sync::atomic::Ordering::Relaxed);
        let (staged, queue, stage_bytes, stage_copies) = (self.staged, &self.queue, &mut self.stage_bytes, &mut self.stage_copies);
        let mut put = |dst: &wgpu::Buffer, offset: u64, data: &[u8]| {
            if data.is_empty() { return; }
            if !staged { queue.write_buffer(dst, offset, data); return; }
            let src = stage_bytes.len() as u64;
            stage_bytes.extend_from_slice(data);
            stage_bytes.resize(stage_bytes.len().div_ceil(4) * 4, 0); // COPY_BUFFER_ALIGNMENT for the next source offset
            stage_copies.push((dst.clone(), src, offset, data.len() as u64));
        };
        // PERF: a whole-buffer write identical to what the buffer already holds is skipped (see `uploaded`)
        self.upload_skipped = 0;
        // FOG INPUTS: the height-fog values and the View volumetric-fog slots over the frame's whole-buffer writes (queue order)
        if self.fog_last.as_ref() != Some(&self.view_fog.values) { self.fog_patched.fill(false); self.fog_last = Some(self.view_fog.values); }
        if self.fog_patched.len() < self.buffers.len() { self.fog_patched.resize(self.buffers.len(), false); }
        if let Ok(mut d) = self.fog_dirty.lock() { for b in d.drain(..) { if let Some(p) = self.fog_patched.get_mut(b.0) { *p = false; } } }
        let fog_values = self.view_fog.values;
        let patch_fog = |bytes: &mut [u8], base: usize, slots: &[FxFogSlot]| {
            for s in slots {
                let n = (s.num_bytes as usize / 4).min(4);
                let Some(o) = (s.byte_offset as usize).checked_sub(base) else { continue };
                if o + 4 * n > bytes.len() { continue; }
                bytes[o..o + 4 * n].copy_from_slice(bytemuck::cast_slice(&fog_values[s.field as usize][..n]));
            }
        };
        {
            let mut uploaded = self.uploaded.lock().unwrap_or_else(|e| e.into_inner());
            if uploaded.len() < self.buffers.len() { uploaded.resize(self.buffers.len(), Vec::new()); }
            for (b, bytes) in &frame.buffer_writes {
                let Some(buf) = self.buffers.get(b.0) else { continue };
                let n = (bytes.len() as u64).min(buf.size()) as usize;
                let fl = flag(b);
                let fog = if fl & 1 != 0 { self.fog_targets.get(b) } else { None };
                let patched = fl != 0;
                if !patched && uploaded.get(b.0).map_or(false, |prev| !prev.is_empty() && prev.as_slice() == &bytes[..n]) { self.upload_skipped += 1; continue; }
                match fog {
                    Some(slots) => { let mut v = bytes[..n].to_vec(); patch_fog(&mut v, 0, slots); put(&buf.buf, buf.off, &v); if let Some(p) = self.fog_patched.get_mut(b.0) { *p = true; } }
                    None => put(&buf.buf, buf.off, &bytes[..n]),
                }
                if let Some(e) = uploaded.get_mut(b.0) { e.clear(); e.extend_from_slice(&bytes[..n]); }
            }
            let mut want: Vec<FxBufId> = vec![];
            if self.bind_fog.len() < self.binds.len() { self.bind_fog.resize(self.binds.len(), (0, Vec::new())); }
            let gen = self.fog_gen;
            for binds in frame.draws.iter().map(|d| &d.binds).chain(frame.shadow_draws.iter().map(|d| &d.binds)) {
                for (_, id) in binds {
                    let Some(rec) = self.binds.get(id.0) else { continue };
                    let bf = &mut self.bind_fog[id.0];
                    if bf.0 != gen { bf.1.clear(); for e in &rec.entries { if let FxRes::Buffer(b) = e.res { if flag(&b) & 1 != 0 { bf.1.push(b); } } } bf.0 = gen; }
                    for &b in &bf.1 { if self.fog_patched.get_mut(b.0).is_some_and(|p| !std::mem::replace(p, true)) { want.push(b); } }
                }
            }
            for b in want {
                let (Some(buf), Some(slots)) = (self.buffers.get(b.0), self.fog_targets.get(&b)) else { continue };
                let fits = |s: &FxFogSlot| s.byte_offset as u64 + 4 * (s.num_bytes as u64 / 4).min(4) <= buf.size();
                let lo = slots.iter().filter(|s| fits(s)).map(|s| s.byte_offset as usize).min().unwrap_or(0);
                let hi = slots.iter().filter(|s| fits(s)).map(|s| s.byte_offset as usize + 4 * (s.num_bytes as usize / 4).min(4)).max().unwrap_or(0);
                if hi <= lo { continue; }
                match uploaded.get(b.0).filter(|c| c.len() >= hi) {
                    Some(c) => { let mut v = c[lo..hi].to_vec(); patch_fog(&mut v, lo, slots); put(&buf.buf, buf.off + lo as u64, &v); }
                    None => {
                        for s in slots.iter().filter(|s| fits(s)) {
                            let n = (s.num_bytes as usize / 4).min(4);
                            put(&buf.buf, buf.off + s.byte_offset as u64, bytemuck::cast_slice(&fog_values[s.field as usize][..n]));
                        }
                    }
                }
            }
        }
        self.fog_flags = flags;
        for b in &self.fog_views {
            let Some(buf) = self.buffers.get(b.0) else { continue };
            if ((VIEW_VOLUMETRIC_FOG_SLOT + 3) * 16) as u64 <= buf.size() { put(&buf.buf, buf.off + (VIEW_VOLUMETRIC_FOG_SLOT * 16) as u64, bytemuck::cast_slice(&self.view_fog.view)); }
        }
        // dynamic geometry / instances
        self.placed.clear();
        let (mut vb_at, mut ib_at, mut inst_at) = (0u64, 0u64, 0u64);
        for d in &frame.draws {
            let mut p = Placed::default();
            if let FxGeom::Dynamic { vertices, indices } = &d.geom {
                let vlen = vertices.len() as u64; let ilen = (indices.len() * 4) as u64;
                if vb_at + vlen <= DYN_CAPACITY && ib_at + ilen <= DYN_CAPACITY {
                    put(&self.dyn_vb, vb_at, vertices); put(&self.dyn_ib, ib_at, bytemuck::cast_slice(indices));
                    p.vb = vb_at; p.ib = ib_at; p.index_count = indices.len() as u32; vb_at += (vlen + 15) / 16 * 16; ib_at += (ilen + 15) / 16 * 16;
                }
            }
            if let Some(i) = &d.instances {
                let n = i.bytes.len() as u64;
                if inst_at + n <= INST_CAPACITY { put(&self.inst_vb, inst_at, &i.bytes); p.inst = inst_at; p.inst_ok = true; inst_at += (n + 15) / 16 * 16; } else { p.index_count = 0; }
            }
            self.placed.push(p);
        }
        // INSTANCING (2026-10-07): per instanced shadow draw and cascade, the ids of the instances that pass that cascade's caster test (the same test a draw
        // per placement gets, `sphere_in_cascade`), each list after the other in `inst_vb`
        let mut shadow_placed = std::mem::take(&mut self.shadow_placed);
        let (cull, small) = (&self.shadow_cull, self.shadow_small);
        let ncasc = cull.len().max(1);
        let mut ids: Vec<u32> = std::mem::take(&mut self.shadow_ids);
        let mut used = 0usize;
        for d in &frame.shadow_draws {
            if shadow_placed.len() <= used { shadow_placed.push(vec![]); }
            let per = &mut shadow_placed[used];
            used += 1;
            per.clear();
            if let Some(si) = &d.instances {
                for c in 0..ncasc {
                    let start = inst_at;
                    let mut k = 0u32;
                    ids.clear();
                    for (id, b) in si.ids.iter().zip(&si.bounds) { if sphere_in_cascade_of(cull, small, *b, c) { ids.push(*id); k += 1; } }
                    let n = 4 * k as u64;
                    if k > 0 && start + n <= INST_CAPACITY { put(&self.inst_vb, start, bytemuck::cast_slice(&ids)); inst_at += (n + 15) / 16 * 16; per.push((start, k)); } else { per.push((0, 0)); }
                }
            }
        }
        shadow_placed.truncate(used);
        self.shadow_ids = ids;
        self.shadow_placed = shadow_placed;
        // PERF (2026-10-06): the frame's staged bytes in one mapped upload buffer (copied by `encode_uploads`)
        if self.stage_copies.len() > 1 {
            let mut merged: Vec<(wgpu::Buffer, u64, u64, u64)> = Vec::with_capacity(self.stage_copies.len());
            for c in self.stage_copies.drain(..) {
                if let Some(l) = merged.last_mut() { if l.0 == c.0 && l.1 + l.3 == c.1 && l.2 + l.3 == c.2 { l.3 += c.3; continue; } }
                merged.push(c);
            }
            self.stage_copies = merged;
        }
        if !self.stage_copies.is_empty() {
            let size = (self.stage_bytes.len() as u64).div_ceil(wgpu::COPY_BUFFER_ALIGNMENT) * wgpu::COPY_BUFFER_ALIGNMENT;
            if self.stage_buf.as_ref().is_none_or(|b| b.size() < size) {
                if let Some(b) = self.stage_buf.take() { b.destroy(); }
                let cap = size.next_power_of_two().max(256 << 10);
                self.stage_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("fx frame uploads"), size: cap, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false }));
            }
            if let Some(b) = &self.stage_buf {
                self.stage_bytes.resize(size as usize, 0);
                self.queue.write_buffer(b, 0, &self.stage_bytes);
                self.stage_ready = true;
            }
        }
        // PERF (FPS investigation 2026-10-06): `FN_FX_UPLOAD_STATS=1` prints the frame's queue.write_buffer count / bytes every 120 frames
        static UPLOAD_STATS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        static UPLOAD_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if *UPLOAD_STATS.get_or_init(|| fn_live::var("FN_FX_UPLOAD_STATS").map_or(false, |v| v == "1")) && UPLOAD_FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 120 == 0 {
            let full = frame.buffer_writes.len(); let full_bytes: usize = frame.buffer_writes.iter().map(|(_, b)| b.len()).sum();
            let fog: usize = self.fog_targets.values().map(|s| s.len()).sum::<usize>() + self.fog_views.len();
            let distinct = frame.buffer_writes.iter().map(|(b, _)| b.0).collect::<std::collections::HashSet<_>>().len();
            eprintln!("[fx upload] staged copies {}, full writes {full} ({distinct} distinct buffers, {:.1} KB; {} skipped as unchanged), fog slot writes {fog}, draws {}, dynamic/instance writes {} | totals: {} buffers, {} bind groups, {} pipelines, {} meshes, {} textures", self.stage_copies.len(), full_bytes as f32 / 1024., self.upload_skipped, frame.draws.len(), self.placed.iter().filter(|p| p.index_count > 0 || p.inst > 0).count(), self.buffers.len(), self.binds.len(), self.pipes.len(), self.meshes.len(), self.textures.len());
        }
        self.frame = frame;
        self.residency_mark();
        // translucent draw order: FCompareFTranslucentSortedPrim (SceneRendering.h:395), per pass: priority ascending, then the sort key descending (back to front);
        // a stable sort keeps the elements of one primitive (equal keys) in submission order (RenderPrimitive draws a primitive's mesh batches in order)
        self.order_standard.clear(); self.order_separate.clear();
        for (n, d) in self.frame.draws.iter().enumerate() {
            let Some(p) = self.pipes.get(d.pipe.0) else { continue };
            if !p.translucent { continue; }
            if p.separate { self.order_separate.push(n) } else { self.order_standard.push(n) }
        }
        let draws = &self.frame.draws;
        let cmp = |a: &usize, b: &usize| sort_order(&draws[*a].sort, &draws[*b].sort);
        self.order_standard.sort_by(cmp); self.order_separate.sort_by(cmp);
        self.merged_standard = self.order_standard.iter().map(|&n| TItem::Fx(n)).collect();
        self.merged_separate = self.order_separate.iter().map(|&n| TItem::Fx(n)).collect();
        // bind groups
        self.depth_gen = depth_gen;
        let volume_gen = self.volume_gen;
        let mut check = std::mem::take(&mut self.bind_pending);
        check.extend_from_slice(&self.bind_dep);
        let mut retry: Vec<u32> = vec![];
        for &ci in &check {
            let i = ci as usize;
            let Some(b) = self.binds.get(i) else { continue };
            let needs = match &b.built { None => true, Some(((g, v), _)) => (b.uses_depth && *g != depth_gen) || (b.uses_volume && *v != volume_gen) };
            if !needs { continue; }
            let (pipe, group, entries) = { let b = &self.binds[i]; (b.pipe, b.group, b.entries.clone()) };
            if self.pipes.get(pipe.0).is_none() { retry.push(ci); continue; }
            // sampler objects must outlive the entry list: resolve them first
            for e in &entries { if let FxRes::Sampler(s) = e.res { let _ = self.sampler(s); } }
            let mut bge: Vec<wgpu::BindGroupEntry> = vec![];
            for e in &entries {
                let resource = match e.res {
                    FxRes::Buffer(b) => self.buffers[b.0].binding(),
                    FxRes::Texture(t) => wgpu::BindingResource::TextureView(&self.textures[t.0].view),
                    FxRes::Sampler(s) => wgpu::BindingResource::Sampler(self.samplers.get(&s).unwrap()),
                    FxRes::SceneDepth => wgpu::BindingResource::TextureView(depth_view),
                    FxRes::TranslucencyVolume(k) => wgpu::BindingResource::TextureView(self.volume.as_ref().and_then(|v| v.get(k as usize)).unwrap_or(&self.volume_black)),
                    FxRes::FogCube => wgpu::BindingResource::TextureView(&self.white_cube),
                    FxRes::FogVolume => wgpu::BindingResource::TextureView(self.fog_volume.as_ref().map(|v| &v.0).unwrap_or(&self.volume_black)),
                };
                bge.push(wgpu::BindGroupEntry { binding: e.binding, resource });
            }
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("fx bind group"), layout: &self.pipes[pipe.0].layouts[group as usize], entries: &bge });
            self.binds[i].built = Some(((depth_gen, volume_gen), bg));
        }
        check.clear();
        self.bind_pending = if retry.is_empty() { check } else { retry };
    }

    /// PERF (2026-10-06, frame-time spikes): stage [`Self::prepare`]'s writes and copy them with [`Self::encode_uploads`] instead of `queue.write_buffer`
    /// (the renderer; tools that never call `encode_uploads` keep the direct writes).
    pub fn set_staged_uploads(&mut self, on: bool) { self.staged = on; }
    /// This frame's staged uploads exist (see [`Self::set_staged_uploads`]).
    pub fn has_uploads(&self) -> bool { self.stage_ready }
    /// Records this frame's staged uploads (the copies `prepare` would have queued with `queue.write_buffer`, same bytes, destinations and order). Call
    /// once, on the frame's encoder, before any pass that reads the FX buffers.
    pub fn encode_uploads(&self, enc: &mut wgpu::CommandEncoder) {
        let Some(src) = self.stage_buf.as_ref().filter(|_| self.stage_ready) else { return };
        for (dst, src_off, dst_off, size) in &self.stage_copies { enc.copy_buffer_to_buffer(src, *src_off, dst, *dst_off, Some(*size)); }
    }

    /// Records the draws of one pass into `pass`: `translucent` false = the masked draws in submission order, true = the STANDARD translucency draws in UE sort
    /// order (see `prepare`); the separate translucency draws are [`Self::draw_separate`].
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, translucent: bool) {
        if translucent { for &n in &self.order_standard { self.draw_one(pass, n); } return; }
        for (n, d) in self.frame_draws().iter().enumerate() { if self.pipes.get(d.pipe.0).map_or(false, |p| !p.translucent && !p.scene_lit) { self.draw_one(pass, n); } } // FX RECEIVE: scene-lit ones are draw_scene_lit
    }
    /// The separate translucency draws (sorted), into the target of [`Self::separate_view`] (cleared to (0, 0, 0, 1)), scene depth read-only.
    pub fn draw_separate<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) { for &n in &self.order_separate { self.draw_one(pass, n); } }
    /// STORM TAA (2026-10-07): this frame has translucent draws of a `bEnableResponsiveAA` material (standard or separate translucency).
    pub fn has_responsive_aa_draws(&self) -> bool { !self.responsive_pipes.is_empty() && self.order_standard.iter().chain(&self.order_separate).any(|&n| self.responsive_pipes.contains_key(&self.frame.draws[n].pipe.0)) }
    /// STORM TAA (2026-10-07): records the responsive-AA mask twins of this frame's tagged translucent draws (both translucency sets: UE writes the stencil bit
    /// in the standard and the separate translucency pass alike, SceneRenderTargets.cpp:1370 / :1410 `DepthRead_StencilWrite`) into the TAA mask pass
    /// (`crate::taa::TemporalAa::begin_responsive_pass`, scene depth read-only).
    pub fn draw_responsive_aa<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        for &n in self.order_standard.iter().chain(&self.order_separate) { if let Some(t) = self.responsive_pipes.get(&self.frame.draws[n].pipe.0) { self.draw_one_with(pass, n, Some(t)); } }
    }
    /// Merges the particle pass' translucent batches `(sort data, separate translucency, batch index)` into this frame's two translucency passes. UE4.16 places a particle
    /// system like any other translucent primitive (SceneVisibility.cpp:1538 `PlaceScenePrimitive` with the proxy's normal / separate relevance from its emitters'
    /// materials) and sorts the whole set at once (`FTranslucentPrimSet::SortPrimitives`): the particles are not a pass of their own. Call after `prepare`.
    pub(crate) fn merge_particles(&mut self, items: &[(FxSort, bool, usize)]) {
        for separate in [false, true] {
            let fx = if separate { &self.order_separate } else { &self.order_standard };
            let mut v: Vec<(FxSort, TItem)> = fx.iter().map(|&n| (self.frame.draws[n].sort, TItem::Fx(n))).collect();
            v.extend(items.iter().filter(|i| i.1 == separate).map(|i| (i.0, TItem::Particle(i.2))));
            v.sort_by(|a, b| sort_order(&a.0, &b.0));
            let out: Vec<TItem> = v.into_iter().map(|x| x.1).collect();
            if separate { self.merged_separate = out } else { self.merged_standard = out }
        }
    }
    /// The merged standard (`separate` false) / separate translucency pass has something to draw (see [`Self::merge_particles`]).
    pub(crate) fn has_merged(&self, separate: bool) -> bool { !(if separate { &self.merged_separate } else { &self.merged_standard }).is_empty() }
    /// Records one merged translucency pass (FX draws and particle batches in the one UE sort order).
    pub(crate) fn draw_merged<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, separate: bool, particles: &'a crate::particles::ParticleRenderer, globals: &'a wgpu::BindGroup) {
        for t in if separate { &self.merged_separate } else { &self.merged_standard } {
            match *t { TItem::Fx(n) => self.draw_one(pass, n), TItem::Particle(i) => particles.draw_cmd(pass, globals, i) }
        }
    }
    /// (Re)creates the separate translucency target at the scene colour size (call once per frame before the passes).
    pub fn prepare_separate(&mut self, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if self.composite.is_none() {
            let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("fx separate translucency recombine"), source: wgpu::ShaderSource::Wgsl(COMPOSITE_WGSL.into()) });
            let bgl = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fx separate recombine"), entries: &[wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None }] });
            let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("fx separate recombine"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
            // scene * sep.a + sep.rgb; scene alpha kept
            let blend = wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::SrcAlpha, operation: wgpu::BlendOperation::Add }, alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add } };
            let pipe = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("fx separate recombine"), layout: Some(&pl),
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                primitive: Default::default(), depth_stencil: None, multisample: Default::default(),
                fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: self.hdr_format, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })] }),
                multiview_mask: None, cache: None,
            });
            self.composite = Some((pipe, bgl));
        }
        if self.separate_target.as_ref().map_or(true, |s| s.size != size) {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor { label: Some("fx separate translucency"), size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: self.hdr_format, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] });
            let view = tex.create_view(&Default::default());
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("fx separate recombine"), layout: &self.composite.as_ref().unwrap().1, entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }] });
            self.separate_target = Some(SeparateTarget { view, size, bind });
        }
    }
    /// The separate translucency target (after [`Self::prepare_separate`]).
    pub fn separate_view(&self) -> Option<&wgpu::TextureView> { self.separate_target.as_ref().map(|s| &s.view) }
    /// The recombine into the scene colour (a full-screen draw into a pass on the HDR target, no depth).
    pub fn composite_separate<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        let (Some((pipe, _)), Some(t)) = (self.composite.as_ref(), self.separate_target.as_ref()) else { return };
        pass.set_pipeline(pipe); pass.set_bind_group(0, &t.bind, &[]); pass.draw(0..3, 0..1);
    }
    fn draw_one<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, n: usize) { self.draw_one_with(pass, n, None) }
    /// [`Self::draw_one`] with another pipeline of the same layout (VELOCITY: the velocity twin).
    fn draw_one_with<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, n: usize, pipe: Option<&'a wgpu::RenderPipeline>) { self.draw_one_cached(pass, n, pipe, &mut PassState::default(), None) }
    /// PERF (2026-10-07): [`Self::draw_one_with`] that skips the pipeline / bind group / mesh buffer sets equal to what the previous draw of the same pass set
    /// (`st`; every group is set again after a pipeline change), and sets `globals` at [`FX_SCENE_GROUP`] after each pipeline change. Same commands otherwise.
    fn draw_one_cached<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, n: usize, pipe: Option<&'a wgpu::RenderPipeline>, st: &mut PassState, globals: Option<&'a wgpu::BindGroup>) {
        {
            let Some(d) = self.frame.draws.get(n) else { return };
            #[cfg(target_arch = "wasm32")]
            if self.web.skip(d.pipe.0) { return; } // BROWSER PORT: still compiling
            let Some(p) = self.pipes.get(d.pipe.0) else { return };
            let placed = self.placed.get(n).copied().unwrap_or_default();
            if d.instances.is_some() && !placed.inst_ok { return; } // INSTANCING: its instances did not fit this frame's buffer (never draw someone else's bytes)
            let rp = pipe.unwrap_or(&p.pipe);
            let key = rp as *const wgpu::RenderPipeline as usize;
            if st.pipe != key {
                pass.set_pipeline(rp); st.change_pipe(key);
                if let Some(g) = globals { let gk = g as *const wgpu::BindGroup as usize; if st.globals != gk { pass.set_bind_group(FX_SCENE_GROUP, g, &[]); st.globals = gk; st.binds[FX_SCENE_GROUP as usize] = 0; } }
            }
            let mut ok = true;
            for (g, b) in &d.binds {
                let gi = *g as usize;
                if gi < st.binds.len() && st.binds[gi] == b.0 + 1 { continue; }
                match self.binds.get(b.0).and_then(|r| r.built.as_ref()) { Some((_, bg)) => { pass.set_bind_group(*g, bg, &[]); if gi < st.binds.len() { st.binds[gi] = b.0 + 1; } if *g == FX_SCENE_GROUP { st.globals = 0; } } None => ok = false }
            }
            if !ok { return; }
            let instances = match &d.instances { Some(i) => { if p.instanced { pass.set_vertex_buffer(1, self.inst_vb.slice(placed.inst..placed.inst + i.bytes.len() as u64)); } i.count } None => 1 };
            match &d.geom {
                FxGeom::MeshRange { mesh, first_index, index_count } => { let Some(m) = self.meshes.get(mesh.0) else { return }; if st.mesh != mesh.0 + 1 { pass.set_vertex_buffer(0, m.vbs()); pass.set_index_buffer(m.ibs(), wgpu::IndexFormat::Uint32); st.mesh = mesh.0 + 1; } pass.draw_indexed(*first_index..(first_index + index_count).min(m.index_count), 0, 0..instances); }
                FxGeom::Mesh(m) => { let Some(mm) = self.meshes.get(m.0) else { return }; if st.mesh != m.0 + 1 { pass.set_vertex_buffer(0, mm.vbs()); pass.set_index_buffer(mm.ibs(), wgpu::IndexFormat::Uint32); st.mesh = m.0 + 1; } pass.draw_indexed(0..mm.index_count, 0, 0..instances); }
                FxGeom::Dynamic { vertices, indices } => { if placed.index_count == 0 { return; } st.mesh = 0; pass.set_vertex_buffer(0, self.dyn_vb.slice(placed.vb..placed.vb + vertices.len() as u64)); pass.set_index_buffer(self.dyn_ib.slice(placed.ib..placed.ib + (indices.len() * 4) as u64), wgpu::IndexFormat::Uint32); pass.draw_indexed(0..placed.index_count, 0, 0..instances); }
            }
        }
    }
    fn frame_draws(&self) -> &[FxDraw] { &self.frame.draws }
    pub fn depth_format(&self) -> wgpu::TextureFormat { self.depth_format }
    pub fn texture_dim(&self, t: FxTexId) -> FxTexDim { self.textures[t.0].dim }
    /// Keeps the unused-field lint quiet for the textures' own handle.
    pub fn texture_count(&self) -> usize { self.textures.iter().filter(|t| t.tex.width() > 0).count() }
}

/// 4 x f32 helper for callers that fill uniform buffers.
#[repr(C)] #[derive(Clone, Copy, Debug, Default, Pod, Zeroable)] pub struct Vec4f(pub [f32; 4]);

#[cfg(test)]
mod tests {
    use super::*;
    /// SortByDistance keys on the true distance of the bounds origin (TranslucentRendering.cpp:864); the far wall sorts before the near beam (back to front), and a
    /// lower `TranslucencySortPriority` goes first whatever the distance (SceneRendering.h:395).
    #[test] fn far_primitives_draw_first_and_priority_wins() {
        let eye = [0.0, 0.0, 170.0]; let fwd = [1.0, 0.0, 0.0];
        let wall = FxSort::new([1200.0, 0.0, 170.0], eye, fwd, 0);
        let beam = FxSort::new([300.0, 0.0, 70.0 + 170.0], eye, fwd, 0);
        assert!((wall.key - 1200.0).abs() < 1e-3 && (beam.key - (300f32 * 300. + 70. * 70.).sqrt()).abs() < 1e-3);
        let mut v = vec![beam, wall];
        v.sort_by(sort_order);
        assert_eq!(v, vec![wall, beam]);
        let late = FxSort::new([5000.0, 0.0, 0.0], eye, fwd, 1);
        let mut v = vec![late, beam, wall];
        v.sort_by(sort_order);
        assert_eq!(v, vec![wall, beam, late]);
    }

    fn chain(w: u32, h: u32, seed: u8) -> Vec<FxLevel> {
        let n = 32 - w.max(h).leading_zeros();
        (0..n).map(|m| { let (mw, mh) = ((w >> m).max(1), (h >> m).max(1)); FxLevel { width: mw, height: mh, depth: 1, rgba8: (0..mw * mh * 4).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed).wrapping_add(m as u8 * 17)).collect() } }).collect()
    }
    fn read_level(fx: &FxRenderer, id: FxTexId, mip: u32) -> Vec<u8> {
        let g = &fx.textures[id.0];
        let (w, h) = ((g.tex.width() >> mip).max(1), (g.tex.height() >> mip).max(1));
        let row = (w * 4).next_multiple_of(256);
        let buf = fx.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = fx.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(wgpu::TexelCopyTextureInfo { texture: &g.tex, mip_level: mip, origin: wgpu::Origin3d { x: 0, y: 0, z: g.layer.map_or(0, |l| l.1) }, aspect: wgpu::TextureAspect::All }, wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } }, wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
        fx.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        fx.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = slice.get_mapped_range().unwrap();
        (0..h).flat_map(|y| data[(y * row) as usize..(y * row + w * 4) as usize].to_vec()).collect()
    }
    #[test] fn pooled_textures_keep_their_pixels_through_migration_shrink_and_replace() {
        let Ok(g) = crate::gpu::Gpu::headless() else { eprintln!("no GPU: skipped"); return };
        let mut fx = FxRenderer::new(&g.device, &g.queue, wgpu::TextureFormat::Rgba16Float, wgpu::TextureFormat::Depth32Float);
        fx.tex_pools.on = true;
        let desc = |w: u32, seed: u8| FxTextureDesc { label: format!("t{seed}"), dim: FxTexDim::D2, srgb: false, levels: chain(w, w, seed), format: FxTexFormat::Rgba8 };
        let mut want: Vec<(FxTexId, Vec<FxLevel>)> = vec![];
        for s in 0..14u8 { let d = desc(256, s); want.push((fx.create_texture(&d), d.levels)); }
        for s in 20..27u8 { let d = desc(64, s); want.push((fx.create_texture(&d), d.levels)); }
        let (pools, _, used, _, _) = fx.pool_stats();
        assert!(pools >= 2 && used >= 12, "pools {pools}, layers used {used}");
        assert!(fx.textures[want[0].0 .0].layer.is_some() && fx.textures[want[15].0 .0].layer.is_some());
        let mut enc = None;
        assert!(fx.shrink_texture(want[3].0 .0, &mut enc));
        if let Some(e) = enc { fx.queue.submit(Some(e.finish())); }
        let shrunk = want[3].1.split_off(2);
        want[3].1 = shrunk;
        let d = desc(128, 99);
        assert!(fx.replace_texture(want[5].0, &d));
        want[5].1 = d.levels;
        fx.drain_pool_frees();
        let d = desc(256, 77);
        want.push((fx.create_texture(&d), d.levels));
        for (id, levels) in &want { for (m, l) in levels.iter().enumerate() { assert_eq!(read_level(&fx, *id, m as u32), l.rgba8, "texture {} mip {m}", id.0); } }
    }
}




fn mix64(mut x: u64) -> u64 { x ^= x >> 33; x = x.wrapping_mul(0xff51_afd7_ed55_8ccd); x ^= x >> 33; x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53); x ^ (x >> 33) }
fn fast_hash(seed: u64, b: &[u8]) -> u64 {
    let mut h = mix64(seed ^ b.len() as u64);
    let mut c = b.chunks_exact(8);
    for w in &mut c { let v = u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]); h = (h ^ mix64(v ^ seed)).rotate_left(27).wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0x52dc_e729); }
    let mut t = [0u8; 8]; t[..c.remainder().len()].copy_from_slice(c.remainder());
    mix64(h ^ u64::from_le_bytes(t) ^ 0x8ebc_6af0_9c88_c6e3)
}
