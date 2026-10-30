//! fn-render: the pure-Rust wgpu renderer of the Fortnite 1.8 runtime.
//!
//! * [`gpu`]      device / adapter setup (Vulkan + DX12), offscreen target with read-back
//! * [`camera`]   camera parameters, projection maths, the GPU camera uniform
//! * [`lighting`] recovered sun / sky-light constants, terrain colour variation, cascaded shadow map fitting
//! * [`sky`]      dome + sun + clouds (ported from the recovered UE 4.16 pixel shaders)
//! * [`tonemap`]  UE 4.16 tonemapper / exposure constants and CPU mirrors of the post chain
//! * [`graph`]    the small frame graph
//! * [`renderer`] pipelines, render targets and the frame description
//! * [`texture`], [`mesh`] upload helpers and CPU mesh/material descriptions
//!
//! All shader code lives in `crates/fn-render/shaders/*.wgsl` (see the crate README for the rust-gpu migration plan).
pub mod backdrop; // BACKDROP HOOK: landscape / ocean / skybox beyond the plate (backdrop.rs)
pub mod camera;
pub mod char_material; // CHAR MATERIAL HOOK
pub mod color_lut; // COLOR LUT (GLOBAL LOOK): the 32^3 colour-grading LUT of FCombineLUTs the tonemapper samples (color_lut.rs)
pub mod distance_field; // DISTANCE FIELD HOOK: mesh distance fields, ray-traced DF shadow cascade (distance_field.rs)
pub mod distance_field_ao; // DISTANCE FIELD HOOK: distance-field AO of the movable sky light (distance_field_ao.rs)
pub mod distance_field_shadow; // DF SHADOW SCREEN PASS (2026-10-06): the half-resolution screen-space ray trace of the DF cascade (distance_field_shadow.rs)
pub mod distance_field_atlas; // DF ATLAS (2026-10-06, freezes): UE4.16 FTextureLayout3d + UpdateAllocations (distance_field_atlas.rs)
pub mod global_distance_field; // GDF PARTIAL UPDATES (2026-10-06): the clipmaps' update regions (scrolled partial + staggered updates, global_distance_field.rs)
pub mod effect_settings; // POSTFX SETTINGS: the final post-process settings of motion blur / DOF / lens flare / fringe / vignette (effect_settings.rs)
pub mod fog; // FOG / ATMOSPHERE HOOK: recovered Athena fog data + exact CPU ports of the fog / volumetric fog shaders
pub mod fx; // RENDER FX HOOK
pub mod gpu;
pub mod graph;
pub mod gun_material; // GUN MATERIAL HOOK
pub mod lighting;
pub mod light_function; // LIGHT FUNCTION (2026-10-07): the sun light function, High material quality program (light_function.rs)
pub mod local_lights;
pub mod subsurface;
pub mod light_shafts; // LIGHT SHAFT HOOK: light-shaft bloom of the sun (UE 4.16 LightShaftRendering.cpp)
pub mod ssao_pass; // AO ORDER: screen-space AO applied to the HDR scene before fog / translucency / bloom
pub mod ssao_ue; // UE SSAO: the UE 4.16 screen-space ambient occlusion passes (PostProcessAmbientOcclusion.usf)
pub mod mesh;
pub mod particles; // PARTICLE HOOK
pub mod primitive_ub; // PRIMITIVE UB: the one `Primitive` uniform buffer builder (UE4.16 GetPrimitiveUniformShaderParameters), used by every original-program draw
pub mod profiler;
pub mod props;
pub mod quality;
pub mod occlusion; // OCCLUSION (2026-10-07): HZB readback occlusion culling of the placed statics (occlusion.rs)
pub mod reflection; // REFLECTION APPLY: deferred sky specular + screen space reflections (reflection.rs, docs/spec/reflection_gaps.md)
pub mod renderer;
pub mod scope_pass; // SCOPE HOOK: the post-process material pass of a scoped weapon (scope_pass.rs)
pub mod decal_pass;
pub mod outline_pass; // OUTLINE: the build preview highlight (custom depth + the outline post-process material), outline_pass.rs
pub mod sky;
pub mod sky_light; // SKY LIGHT: SLS_SpecifiedCubemap SH diffuse of the Athena sky light (sky_light.rs)
pub mod sky_reflection; // SKY REFLECTION HOOK: the sky light's specular (processed cube, PreIntegratedGF, lit_common.wgsl reflection_inputs + reflection.rs)
pub mod static_loader;
pub mod static_mesh;
pub mod static_gdf;
pub mod statics;
pub mod storm; // STORM HOOK: safe-zone wall + storm cloud constants / buffer contents (the draws go through the FX pass, see fn-matshader::storm)
pub mod synthetic;
#[allow(dead_code)]
pub mod sky_params;
pub mod time_of_day;
pub mod texture;
pub mod translucent_lighting; // MIRRORFIX: the translucency lighting volume of translucent lit FX materials (translucent_lighting.rs)
pub mod tonemap;
pub mod taa; // SCALABILITY: temporal AA (High / Epic), FXAA console (Medium), screen-percentage upscale (taa.rs)
pub mod volfog; // VOLUMETRIC FOG HOOK: GPU volumetric fog passes (material setup, light scattering, final integration, composite)

pub use camera::{CameraParams, CameraUniform};
pub use gpu::{Gpu, GpuErrorSink, OffscreenTarget};
pub use mesh::{MaterialImages, MaterialKind, SkinnedPrimitive, SkinnedVertex};
pub use quality::{Backdrop, FxaaParams};
pub use renderer::{BuiltModel, ExposureInfo, FrameParams, FrameStats, HdrImage, ModelBuilder, ModelId, PropVertex, RenderOptions, Renderer, RendererAssets, UiOverlay};
pub use profiler::{GpuPassTime, PassTimings};
pub use props::{PropInstance, PropTemplates};
pub use sky::SkyAssets;
pub use static_mesh::{AlphaMode, StaticLod, StaticMaterialData, StaticMaterialHandle, StaticMeshData, StaticMeshHandle, StaticMeshInstance, StaticSection, StaticVertex};
pub use statics::StaticStats;
pub use wgpu;