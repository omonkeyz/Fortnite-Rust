//! Video-settings quality tables expressed as renderer inputs: Fortnite's own `Scalability` sg.* groups, read from the cooker's ini record
//! `FortniteGame/CookedIniVersion.txt` (pakchunk0, unencrypted; extracted copy `C:\FortniteRef\scratch\CookedIniVersion.txt`, line numbers
//! cited as L<n>). Levels are 0 (low) .. 3 (epic). Only `aa_params` (FXAA debug tables of the old runtime) is not from the ini.
//!
//! fn-app maps `fn_settings` levels onto these helpers and passes the results through [`crate::RenderOptions`] and the camera far plane.

/// Shadows level -> CSM shadow map edge in texels (`None` = shadows off): `r.Shadow.MaxCSMResolution` of `[ShadowQuality@N]`, None when its
/// `r.ShadowQuality` is 0 (see [`ShadowScalability`]).
pub fn shadow_map_size(level: u32) -> Option<u32> { let s = ShadowScalability::level(level); if s.shadows_enabled() { Some(s.csm_resolution()) } else { None } }

/// Every cvar of the `sg.ShadowQuality` scalability group: FORTNITE's own `Scalability:ShadowQuality@0..3` values, read from the cooker's ini record
/// `FortniteGame/CookedIniVersion.txt` (pakchunk0, unencrypted; extracted to `C:\FortniteRef\scratch\CookedIniVersion.txt` lines 5103-5158, every line
/// prefixed `Mac.` by the cooker). They differ from UE4.16 BaseScalability.ini (lines 75-126) in: @0 `r.LightFunctionQuality` 1 (engine 0) and
/// `r.SimpleForwardShading` 1, @2 `r.Shadow.CSM.MaxCascades` 2 (engine 4) and `r.LightMaxDrawDistanceScale` .5 (engine unset = 1).
/// The Shadows video setting is `sg.ShadowQuality` (level 3 = Epic; @Cine is not selectable).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowScalability {
    /// `r.LightFunctionQuality`: <= 0 clears `EngineShowFlags.LightFunctions` (ShowFlags.cpp:359), i.e. the sun's light function is not applied.
    pub light_function_quality: i32,
    /// `r.ShadowQuality`: 0 clears `EngineShowFlags.DynamicShadows` (ShowFlags.cpp:375, LightRendering.cpp:336); otherwise it is the `SHADOW_QUALITY`
    /// permutation of the CSM projection shader (ShadowRendering.cpp:882-912: whole-scene directional shadows keep `GetShadowQuality()` unchanged).
    pub shadow_quality: i32,
    /// `r.Shadow.CSM.MaxCascades` (-> `View.MaxShadowCascades`, SceneRendering.cpp:485).
    pub csm_max_cascades: i32,
    /// `r.Shadow.MaxResolution`: per-object / spot / point shadows only; the local lights are unshadowed here (local_lights.rs), so nothing reads it.
    pub max_resolution: u32,
    /// `r.Shadow.MaxCSMResolution`: the CSM shadow buffer edge (ShadowSetup.cpp:2648).
    pub max_csm_resolution: u32,
    /// `r.Shadow.RadiusThreshold` (`GMinScreenRadiusForShadowCaster`, ShadowSetup.cpp:34): small-caster screen-size cull ([`shadow_caster_screen_size_culled`]).
    pub radius_threshold: f32,
    /// `r.Shadow.DistanceScale`: `GetCSMMaxDistance` = WholeSceneDynamicShadowRadius * clamp(scale, 0, 2) (DirectionalLightComponent.cpp:364).
    pub distance_scale: f32,
    /// `r.Shadow.CSM.TransitionScale`: `CascadeTransitionFraction * clamp(scale, 0, 2)` (DirectionalLightComponent.cpp:386 / 635).
    pub csm_transition_scale: f32,
    /// `r.DistanceFieldShadowing` (`SupportsDistanceFieldShadows`, DistanceFieldShadowing.cpp:774): the ray-traced cascade is still CREATED by the proxy
    /// (`ShouldCreateRayTracedCascade` does not read it), it is only not projected.
    pub distance_field_shadowing: bool,
    /// `r.DistanceFieldAO`.
    pub distance_field_ao: bool,
    /// `r.VolumetricFog`.
    pub volumetric_fog: bool,
    /// `r.VolumetricFog.GridPixelSize` / `r.VolumetricFog.GridSizeZ`; @0 / @1 do not set them (cvar defaults 16 / 64, `fog::engine`), and the fog is off there.
    pub volumetric_fog_grid_pixel_size: u32,
    pub volumetric_fog_grid_size_z: u32,
    /// `r.LightMaxDrawDistanceScale`: point / spot lights only (SceneVisibility.cpp:2714, ShadowRendering.cpp:123); read by local_lights.rs.
    pub light_max_draw_distance_scale: f32,
    /// `r.SimpleForwardShading` (needs `r.SupportSimpleForwardShading` = 1, CookedIniVersion.txt:978): a whole-renderer lighting-path switch of the Low
    /// level; this renderer has no simple-forward path, so nothing reads it (at @0 dynamic shadows are off anyway).
    pub simple_forward_shading: bool,
}
impl ShadowScalability {
    /// Fortnite's `Scalability:ShadowQuality@level` (CookedIniVersion.txt; levels above 3 = @3).
    pub const fn level(level: u32) -> ShadowScalability {
        use crate::fog::engine::{GRID_PIXEL_SIZE, GRID_SIZE_Z};
        match level {
            // CookedIniVersion.txt:5103-5115 (GridPixelSize / GridSizeZ not set: cvar defaults)
            0 => ShadowScalability { light_function_quality: 1, shadow_quality: 0, csm_max_cascades: 1, max_resolution: 512, max_csm_resolution: 512, radius_threshold: 0.06,
                distance_scale: 0.6, csm_transition_scale: 0., distance_field_shadowing: false, distance_field_ao: false, volumetric_fog: false,
                volumetric_fog_grid_pixel_size: GRID_PIXEL_SIZE, volumetric_fog_grid_size_z: GRID_SIZE_Z, light_max_draw_distance_scale: 0., simple_forward_shading: true },
            // CookedIniVersion.txt:5116-5128
            1 => ShadowScalability { light_function_quality: 1, shadow_quality: 3, csm_max_cascades: 1, max_resolution: 1024, max_csm_resolution: 1024, radius_threshold: 0.05,
                distance_scale: 0.7, csm_transition_scale: 0.25, distance_field_shadowing: false, distance_field_ao: false, volumetric_fog: false,
                volumetric_fog_grid_pixel_size: GRID_PIXEL_SIZE, volumetric_fog_grid_size_z: GRID_SIZE_Z, light_max_draw_distance_scale: 0.5, simple_forward_shading: false },
            // CookedIniVersion.txt:5129-5143
            2 => ShadowScalability { light_function_quality: 1, shadow_quality: 5, csm_max_cascades: 2, max_resolution: 1024, max_csm_resolution: 2048, radius_threshold: 0.04,
                distance_scale: 0.85, csm_transition_scale: 0.8, distance_field_shadowing: true, distance_field_ao: true, volumetric_fog: true,
                volumetric_fog_grid_pixel_size: 16, volumetric_fog_grid_size_z: 64, light_max_draw_distance_scale: 0.5, simple_forward_shading: false },
            // CookedIniVersion.txt:5144-5158
            _ => ShadowScalability { light_function_quality: 1, shadow_quality: 5, csm_max_cascades: 10, max_resolution: 2048, max_csm_resolution: 2048, radius_threshold: 0.03,
                distance_scale: 1.0, csm_transition_scale: 1.0, distance_field_shadowing: true, distance_field_ao: true, volumetric_fog: true,
                volumetric_fog_grid_pixel_size: 8, volumetric_fog_grid_size_z: 128, light_max_draw_distance_scale: 1., simple_forward_shading: false },
        }
    }
    /// `EngineShowFlags.DynamicShadows` survives `r.ShadowQuality` (ShowFlags.cpp:375).
    pub fn shadows_enabled(&self) -> bool { self.shadow_quality > 0 }
    /// MATERIAL PARAMETERS (2026-10-06): the `sg.ShadowQuality` level these values are (`UGameUserSettings::GetShadowQuality()`), `None` for a custom set.
    pub fn level_of(&self) -> Option<u32> { (0..4).find(|&l| Self::level(l) == *self) }
    /// `EngineShowFlags.LightFunctions` survives `r.LightFunctionQuality` (ShowFlags.cpp:359).
    pub fn light_functions_enabled(&self) -> bool { self.light_function_quality > 0 }
    /// CSM buffer edge: `MaxCSMShadowResolution` clamped to the max shadow buffer size (ShadowSetup.cpp:2648; this renderer's limit is 4096).
    pub fn csm_resolution(&self) -> u32 { self.max_csm_resolution.clamp(1, 4096) }
    /// `SHADOW_QUALITY` of the CSM projection (`ManualPCF`, ShadowFilteringCommon.usf:265): 1 no filtering, 2 2x2 texels (1 bilinear tap), 3 4x4 texels
    /// (3x3 bilinear taps, `PCF3x3gather`), 4 / 5 6x6 texels (5x5 bilinear taps, the FEATURE_GATHER4 path).
    pub fn pcf_quality(&self) -> i32 { self.shadow_quality.clamp(1, 5) }
    /// CSM split setup for this level.
    pub fn csm(&self) -> crate::lighting::CsmSetup {
        use crate::lighting as l;
        // GetCSMMaxDistance (DirectionalLightComponent.cpp:364)
        let distance_m = l::SHADOW_DISTANCE_M * self.distance_scale.clamp(0., 2.);
        // GetNumShadowMappedCascades: min(distance > 0 ? DynamicShadowCascades : 0, View.MaxShadowCascades = clamp(r.Shadow.CSM.MaxCascades, 1, 10))
        let max = self.csm_max_cascades.clamp(1, 10) as usize;
        let num_cascades = if distance_m > 0. { l::CASCADE_COUNT.min(max) } else { 0 };
        // GetShadowTransitionScale (DirectionalLightComponent.cpp:386)
        l::CsmSetup { num_cascades, distance_m, transition_fraction: l::CASCADE_TRANSITION_FRACTION * self.csm_transition_scale.clamp(0., 2.) }
    }
}
impl Default for ShadowScalability { fn default() -> Self { ShadowScalability::level(3) } }

/// `APlayerCameraManager::DefaultFOV` = 90: exe ctor `APlayerCameraManager__APlayerCameraManager` (0x142383670) stores 0x42b40000 at +0x3a0; neither
/// `AFortPlayerCameraBase` nor the stock UE ctor changes it.
pub const PLAYER_CAMERA_DEFAULT_FOV_DEG: f32 = 90.;
/// `View.LODDistanceFactor` = `ViewInfo.FOV / DefaultFOV` (LocalPlayer.cpp:1000, `LocalPlayerCachedLODDistanceFactor`; UE uses the previous frame's value).
pub fn lod_distance_factor(fov_deg: f32) -> f32 { fov_deg / PLAYER_CAMERA_DEFAULT_FOV_DEG.max(0.01) }
/// The small-caster cull of whole-scene shadows (ShadowSetup.cpp:2454, `bScreenSpaceSizeCulled`):
/// `Square(PrimitiveBounds.SphereRadius) < Square(r.Shadow.RadiusThreshold) * DistanceSquared * LODDistanceFactorSquared`, with `DistanceSquared` from the
/// primitive's bounds origin to the shadow view origin (the camera). Units cancel (any consistent length unit).
pub fn shadow_caster_screen_size_culled(sphere_radius: f32, distance_sq: f32, radius_threshold: f32, lod_distance_factor: f32) -> bool {
    sphere_radius * sphere_radius < radius_threshold * radius_threshold * distance_sq * (lod_distance_factor * lod_distance_factor)
}
/// View Distance level -> camera far plane in metres. SCALABILITY: Fortnite's `ViewDistanceQuality@N` does NOT move the far plane (it sets
/// `r.ViewDistanceScale`, see [`ViewDistanceScalability`]); the main pass keeps `camera::FAR` at every level.
pub fn far_plane_m(_level: u32) -> f32 { crate::camera::FAR }

/// SCALABILITY (`sg.ViewDistanceQuality`): Fortnite's `Scalability:ViewDistanceQuality@0..3` (CookedIniVersion.txt): `r.ViewDistanceScale`
/// 0.5 / 0.67 / 0.83 / 1.0 (L5079 / 5084 / 5089 / 5094), `r.SkeletalMeshLODBias` 1 / 0 / 0 / 0 (L5078 / 5083 / 5088 / 5093). The scale multiplies
/// every primitive's MaxDrawDistance (`FrustumCull` / `IsDistanceCulled`, SceneVisibility.cpp:252-254 / 308-343, `max(cvar, 0)` in
/// UnrealEngine.cpp:337) and the foliage `InstanceEndCullDistance` (HierarchicalInstancedStaticMesh.cpp:1468-1479); primitives with no cull
/// distance (0) stay unculled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewDistanceScalability {
    pub view_distance_scale: f32,
    /// `r.SkeletalMeshLODBias` (skinned meshes have a single LOD in this renderer: nothing reads it).
    pub skeletal_mesh_lod_bias: i32,
}
impl ViewDistanceScalability {
    pub const fn level(level: u32) -> ViewDistanceScalability {
        match level {
            0 => ViewDistanceScalability { view_distance_scale: 0.5, skeletal_mesh_lod_bias: 1 },
            1 => ViewDistanceScalability { view_distance_scale: 0.67, skeletal_mesh_lod_bias: 0 },
            2 => ViewDistanceScalability { view_distance_scale: 0.83, skeletal_mesh_lod_bias: 0 },
            _ => ViewDistanceScalability { view_distance_scale: 1.0, skeletal_mesh_lod_bias: 0 },
        }
    }
}
/// `MaxDrawDistance * r.ViewDistanceScale` (0 = never culled stays 0).
pub fn scaled_cull_distance(cull: f32, view_distance_scale: f32) -> f32 { cull * view_distance_scale.max(0.) }

/// FORTGLOBALS CULL CURVE: `ABuildingActor` (every Athena prop: trees, rocks, containers are BuildingActors) recomputes its `CullDistance` at
/// run time (exe `ABuildingActor__vfunc_253` @ 0x140498440) from `UFortGlobals` (`FUN_1405c9540` = UFortGlobals::Get, offsets from the 1.8 SDK
/// dump) = `Game:/Script/FortniteGame.FortGlobals` of CookedIniVersion.txt L3338-3343:
/// * start: `DefaultCullDistance` (18000 cm); `AFortWorldSettings::bDisableCullDistance` -> 0 (never culled); actor `LDMaxDrawDistance` > 0 -> that;
/// * otherwise, unless `bUseDefaultCullDistance` (then 18000 stays), per primitive component without an HLOD parent: `size = Bounds.SphereRadius`
///   (world cm, component +0x138) clamped to [MinCullObjectSize 64, MaxCullObjectSize 1024]; below `DefaultCullObjectSize` (450) the pair
///   (Min 64 cm -> MinCullDistance 6000 cm, Default 450 -> 18000), above it (450 -> 18000, Max 1024 -> MaxCullDistance 24000);
///   `dist = log10(1 + 9 t) * (d_hi - d_lo) + d_lo`, `t = (size - s_lo) / (s_hi - s_lo)` (exe constants `DAT_14409936c` = 9, `DAT_143fe36fc` = 1,
///   `DAT_14409935c` = 0.4342945 = 1/ln 10); the largest component value wins and is written to every component's LDMaxDrawDistance /
///   CachedMaxDrawDistance (`FUN_1420e5230` = SetCachedMaxDrawDistance).
/// The port applies it to props whose cooked cull distance is 0 (sizes in metres here).
pub mod fort_cull {
    pub const DEFAULT_CULL_OBJECT_SIZE_M: f32 = 4.5;
    pub const DEFAULT_CULL_DISTANCE_M: f32 = 180.;
    pub const MIN_CULL_OBJECT_SIZE_M: f32 = 0.64;
    pub const MIN_CULL_DISTANCE_M: f32 = 60.;
    pub const MAX_CULL_OBJECT_SIZE_M: f32 = 10.24;
    pub const MAX_CULL_DISTANCE_M: f32 = 240.;
    /// Cull distance (m) of a primitive whose bounds sphere radius is `radius_m` (world).
    pub fn cull_distance_m(radius_m: f32) -> f32 {
        let s = radius_m.clamp(MIN_CULL_OBJECT_SIZE_M, MAX_CULL_OBJECT_SIZE_M);
        let (s_lo, s_hi, d_lo, d_hi) = if s < DEFAULT_CULL_OBJECT_SIZE_M { (MIN_CULL_OBJECT_SIZE_M, DEFAULT_CULL_OBJECT_SIZE_M, MIN_CULL_DISTANCE_M, DEFAULT_CULL_DISTANCE_M) }
            else { (DEFAULT_CULL_OBJECT_SIZE_M, MAX_CULL_OBJECT_SIZE_M, DEFAULT_CULL_DISTANCE_M, MAX_CULL_DISTANCE_M) };
        ((s - s_lo) / (s_hi - s_lo) * 9. + 1.).ln() * 0.434_294_5 * (d_hi - d_lo) + d_lo
    }
}

/// SCALABILITY (`sg.AntiAliasingQuality`): Fortnite's `AntiAliasingQuality@0..3` (CookedIniVersion.txt L5068-5075): `r.PostProcessAAQuality`
/// 0 / 2 / 3 / 4 and `r.Tonemapper.Sharpen` 0 / 0 / 0.5 / 0.5, with `r.DefaultFeature.AntiAliasing` = 2 (TemporalAA, L70).
/// Method (SceneView.cpp:778-796): quality <= 0 -> none; TemporalAA with quality < 3 -> FXAA (`FRCPassPostProcessAA(Quality)`, FXAA_PRESET =
/// quality - 1, FXAAShader.usf:18-35: preset 1 = the FXAA 3.11 "console" path); quality 3 -> `MainFastTemporalAA`, >= 4 -> `MainTemporalAA`
/// (PostProcessTemporalAA.cpp:857-860).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AaScalability {
    pub post_process_aa_quality: i32,
    /// `r.Tonemapper.Sharpen` (clamped 0..10, `TonemapperParams.y = Sharpen / 6`, PostProcessTonemap.cpp:950-953).
    pub tonemapper_sharpen: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AaMethod { None, Fxaa { preset: i32 }, TemporalAa { fast: bool } }
impl AaScalability {
    pub const fn level(level: u32) -> AaScalability {
        match level {
            0 => AaScalability { post_process_aa_quality: 0, tonemapper_sharpen: 0. },
            1 => AaScalability { post_process_aa_quality: 2, tonemapper_sharpen: 0. },
            2 => AaScalability { post_process_aa_quality: 3, tonemapper_sharpen: 0.5 },
            _ => AaScalability { post_process_aa_quality: 4, tonemapper_sharpen: 0.5 },
        }
    }
    pub fn method(&self) -> AaMethod {
        let q = self.post_process_aa_quality.clamp(0, 6);
        if q <= 0 { AaMethod::None } else if q < 3 { AaMethod::Fxaa { preset: q - 1 } } else { AaMethod::TemporalAa { fast: q == 3 } }
    }
    pub fn sharpen(&self) -> f32 { self.tonemapper_sharpen.clamp(0., 10.) }
}
impl Default for AaScalability { fn default() -> Self { AaScalability::level(3) } }
/// Anti-Aliasing level -> FXAA parameters of the old FXAA-only path (kept for `--fxaa-level` debugging; the settings now go through [`AaScalability`]).
pub fn aa_params(level: u32) -> Option<FxaaParams> {
    match level {
        0 => None,
        1 => Some(FxaaParams { subpix: 0.5, edge_threshold: 0.25, edge_threshold_min: 0.0625, search_steps: 4 }),
        2 => Some(FxaaParams { subpix: 0.75, edge_threshold: 0.166, edge_threshold_min: 0.0833, search_steps: 8 }),
        _ => Some(FxaaParams { subpix: 1.0, edge_threshold: 0.125, edge_threshold_min: 0.0312, search_steps: 12 }),
    }
}

/// SCALABILITY (`sg.TextureQuality`): Fortnite's `TextureQuality@0..3` (CookedIniVersion.txt): `r.Streaming.MipBias` 1.5 / 1 / 0 / 0
/// (L5259 / 5264 / 5269 / 5274), `r.MaxAnisotropy` 0 / 2 / 4 / 8 (L5260 / 5265 / 5270 / 5275), `r.Streaming.PoolSize` 400 / 600 / 800 / 1000 MB.
/// `MaxAnisotropy`: the sampler's `Clamp(r.MaxAnisotropy, 1, 16)` (0 -> 1 = no anisotropy). `MipBias`: with `r.Streaming.UsePerTextureBias` = 1
/// (exe default, exe-defaults.json) the GLOBAL bias is 0 (`GlobalMipBias()`, TextureStreamingHelpers.h:73-75) and `floor(MipBias)` is only the
/// per-texture allowance the streamer may drop WHEN the pool is over budget (`MaxExpectedPerTextureMipBias`, :77; AsyncTextureStreaming.cpp:248-
/// 278). Every texture of this renderer is fully resident and there is no pool budget, i.e. it is never over budget: no mip is dropped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextureScalability { pub mip_bias: f32, pub max_anisotropy: i32, pub pool_size_mb: u32 }
impl TextureScalability {
    pub const fn level(level: u32) -> TextureScalability {
        match level {
            0 => TextureScalability { mip_bias: 1.5, max_anisotropy: 0, pool_size_mb: 400 },
            1 => TextureScalability { mip_bias: 1., max_anisotropy: 2, pool_size_mb: 600 },
            2 => TextureScalability { mip_bias: 0., max_anisotropy: 4, pool_size_mb: 800 },
            _ => TextureScalability { mip_bias: 0., max_anisotropy: 8, pool_size_mb: 1000 },
        }
    }
    /// Sampler anisotropy clamp.
    pub fn anisotropy(&self) -> u16 { self.max_anisotropy.clamp(1, 16) as u16 }
    /// Most mips the streamer may drop from one texture when over budget (`floor(MipBias)`).
    pub fn over_budget_mip_drop(&self) -> u32 { self.mip_bias.max(0.).floor() as u32 }
    /// Top mips never sampled: 0 (fully resident, never over the pool budget, see the type docs).
    pub fn resident_mip_drop(&self) -> u32 { 0 }
}
/// Textures level -> (top mip levels skipped, anisotropy) from [`TextureScalability`].
pub fn texture_params(level: u32) -> (u32, u16) { let t = TextureScalability::level(level); (t.resident_mip_drop(), t.anisotropy()) }

/// SCALABILITY (`sg.PostProcessQuality`): every cvar of Fortnite's `PostProcessQuality@0..3` (CookedIniVersion.txt L5174-5241).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PostProcessScalability {
    pub motion_blur_quality: i32,
    pub ao_mip_level_factor: f32,
    pub ao_max_quality: f32,
    /// `r.AmbientOcclusionLevels` (-1 = automatic, 0 = SSAO off).
    pub ao_levels: i32,
    pub ao_radius_scale: f32,
    pub depth_of_field_quality: i32,
    pub render_target_pool_min: i32,
    pub lens_flare_quality: i32,
    pub scene_color_fringe_quality: i32,
    /// `r.EyeAdaptationQuality` (0 = auto exposure off: `IsAutoExposureMethodSupported` / the EyeAdaptation show flag).
    pub eye_adaptation_quality: i32,
    pub bloom_quality: i32,
    pub fast_blur_threshold: f32,
    pub upscale_quality: i32,
    pub tonemapper_grain_quantization: i32,
    pub light_shaft_quality: i32,
    pub filter_size_scale: f32,
    /// `r.Tonemapper.Quality` (vignette needs >= 2: `TonemapperVignette` is masked below quality 2, PostProcessTonemap.cpp).
    pub tonemapper_quality: i32,
}
impl PostProcessScalability {
    pub const fn level(level: u32) -> PostProcessScalability {
        match level {
            // L5174-5190
            0 => PostProcessScalability { motion_blur_quality: 0, ao_mip_level_factor: 1.0, ao_max_quality: 0., ao_levels: 0, ao_radius_scale: 1.2, depth_of_field_quality: 0, render_target_pool_min: 300,
                lens_flare_quality: 0, scene_color_fringe_quality: 0, eye_adaptation_quality: 0, bloom_quality: 4, fast_blur_threshold: 0., upscale_quality: 1, tonemapper_grain_quantization: 0,
                light_shaft_quality: 0, filter_size_scale: 0.6, tonemapper_quality: 0 },
            // L5191-5207
            1 => PostProcessScalability { motion_blur_quality: 3, ao_mip_level_factor: 1.0, ao_max_quality: 60., ao_levels: -1, ao_radius_scale: 1.5, depth_of_field_quality: 1, render_target_pool_min: 350,
                lens_flare_quality: 0, scene_color_fringe_quality: 0, eye_adaptation_quality: 0, bloom_quality: 4, fast_blur_threshold: 2., upscale_quality: 2, tonemapper_grain_quantization: 0,
                light_shaft_quality: 0, filter_size_scale: 0.7, tonemapper_quality: 2 },
            // L5208-5224
            2 => PostProcessScalability { motion_blur_quality: 3, ao_mip_level_factor: 0.6, ao_max_quality: 100., ao_levels: -1, ao_radius_scale: 1.5, depth_of_field_quality: 2, render_target_pool_min: 400,
                lens_flare_quality: 2, scene_color_fringe_quality: 1, eye_adaptation_quality: 2, bloom_quality: 5, fast_blur_threshold: 3., upscale_quality: 2, tonemapper_grain_quantization: 1,
                light_shaft_quality: 0, filter_size_scale: 0.8, tonemapper_quality: 5 },
            // L5225-5241
            _ => PostProcessScalability { motion_blur_quality: 4, ao_mip_level_factor: 0.4, ao_max_quality: 100., ao_levels: -1, ao_radius_scale: 1.0, depth_of_field_quality: 2, render_target_pool_min: 400,
                lens_flare_quality: 2, scene_color_fringe_quality: 1, eye_adaptation_quality: 2, bloom_quality: 5, fast_blur_threshold: 100., upscale_quality: 3, tonemapper_grain_quantization: 1,
                light_shaft_quality: 1, filter_size_scale: 1., tonemapper_quality: 5 },
        }
    }
    /// SSAO runs (`r.AmbientOcclusionLevels` != 0).
    pub fn ambient_occlusion(&self) -> bool { self.ao_levels != 0 }
    /// Bloom runs (`r.BloomQuality` > 0; 4 or 5 at every Fortnite level).
    pub fn bloom(&self) -> bool { self.bloom_quality > 0 }
    /// The tonemapper's vignette permutation survives (`r.Tonemapper.Quality` >= 2).
    pub fn vignette(&self) -> bool { self.tonemapper_quality >= 2 }
    /// Grain quantization (dither) in the tonemapper (`TonemapperGenerateBitmaskPC`, PostProcessTonemap.cpp:609-617).
    pub fn grain_quantization(&self) -> bool { self.tonemapper_grain_quantization > 0 }
    /// Eye adaptation (auto exposure) is on (`r.EyeAdaptationQuality` > 0).
    pub fn eye_adaptation(&self) -> bool { self.eye_adaptation_quality > 0 }
    pub fn bloom_scalability(&self) -> fn_postfx::bloom::BloomScalability {
        fn_postfx::bloom::BloomScalability { quality: self.bloom_quality, fast_blur_threshold: self.fast_blur_threshold, filter_size_scale: self.filter_size_scale }
    }
}
impl Default for PostProcessScalability { fn default() -> Self { PostProcessScalability::level(3) } }
/// Post Processing level -> (ao, bloom, vignette) from [`PostProcessScalability`]: bloom at every level, SSAO and vignette from level 1.
pub fn post_flags(level: u32) -> (bool, bool, bool) { let p = PostProcessScalability::level(level); (p.ambient_occlusion(), p.bloom(), p.vignette()) }

/// REFLECTION APPLY (SSR): the screen space reflection cvars of Fortnite's `EffectsQuality@0..3` / `@Cine`: `r.SSR.Quality` 0 / 0 / 2 / 3 / 4
/// (CookedIniVersion.txt L5286 / 5300 / 5314 / 5328 / 5342). No other `r.SSR.*` is cooked: `r.SSR.Temporal 0`, `r.SSR.Stencil 0`, `r.SSR.Cone 0`,
/// `r.SSR.MaxRoughness -1` (= the post-process value, SceneView.cpp:1812) are the code defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectsScalability {
    /// `r.SSR.Quality` (0 = off).
    pub ssr_quality: i32,
    pub sss_scale: f32,
    pub sss_sample_set: u32,
    pub sss_quality: i32,
    pub sss_half_res: bool,
    pub scene_color_format: u32,
}
impl EffectsScalability {
    /// `level` 0..3 = Low..Epic, 4 = Cine.
    pub const fn level(level: u32) -> EffectsScalability {
        match level {
            0 => EffectsScalability { ssr_quality: 0, sss_scale: 0., sss_sample_set: 0, sss_quality: 0, sss_half_res: true, scene_color_format: 3 },
            1 => EffectsScalability { ssr_quality: 0, sss_scale: 0.75, sss_sample_set: 0, sss_quality: 0, sss_half_res: true, scene_color_format: 3 },
            2 => EffectsScalability { ssr_quality: 2, sss_scale: 1., sss_sample_set: 1, sss_quality: -1, sss_half_res: true, scene_color_format: 3 },
            3 => EffectsScalability { ssr_quality: 3, sss_scale: 1., sss_sample_set: 2, sss_quality: 1, sss_half_res: true, scene_color_format: 4 },
            _ => EffectsScalability { ssr_quality: 4, sss_scale: 1., sss_sample_set: 2, sss_quality: 1, sss_half_res: false, scene_color_format: 4 },
        }
    }
    pub fn sss_checkerboard(&self) -> bool { self.scene_color_format != 4 && self.scene_color_format != 5 }
}
impl Default for EffectsScalability { fn default() -> Self { EffectsScalability::level(3) } }
/// The post-process SSR settings of the Athena view: the 1.8 exe `FPostProcessSettings` ctor (crates/fn-exe-defaults/reports/exe-defaults.json); none of the
/// four TOD phase components of TODM_A / TODM_A_Child_Athena sets a `bOverride_ScreenSpaceReflection*` flag (reports/postfx-evidence.json todm.phases, and
/// `fn-inspect props` of both packages: no ScreenSpaceReflection property at all), and Athena_Terrain has no PostProcessVolume.
pub const SSR_PP_INTENSITY: f32 = 100.;
pub const SSR_PP_QUALITY: f32 = 50.;
pub const SSR_PP_MAX_ROUGHNESS: f32 = 0.6;
/// `ComputeSSRQuality(PP Quality)` (ScreenSpaceReflections.cpp:315): 80+ -> 4, 60+ -> 3, 40+ -> 2, else 1, then `min(_, r.SSR.Quality)` and clamped to 1..4 by
/// `Process` (:344). `None` when `ShouldRenderScreenSpaceReflections` (:58) says no: `r.SSR.Quality <= 0` or `ScreenSpaceReflectionIntensity < 1`.
pub fn ssr_shader_quality(cvar_quality: i32, pp_quality: f32, pp_intensity: f32) -> Option<u32> {
    if cvar_quality <= 0 || pp_intensity < 1. { return None; }
    let q = if pp_quality >= 60. { if pp_quality >= 80. { 4 } else { 3 } } else if pp_quality >= 40. { 2 } else { 1 };
    Some(q.min(cvar_quality.max(0)).clamp(1, 4) as u32)
}
/// `ComputeRoughnessMaskScale` (ScreenSpaceReflections.cpp:105): `-2 / clamp(MaxRoughness, 0.01, 1) * (SSRQuality < 3 ? 2 : 1)`.
pub fn ssr_roughness_mask_scale(max_roughness: f32, shader_quality: u32) -> f32 { -2. / max_roughness.clamp(0.01, 1.) * if shader_quality < 3 { 2. } else { 1. } }

/// SCALABILITY (`sg.ResolutionQuality`): `PerfIndexValues_ResolutionQuality` 50 / 71 / 87 / 100 (CookedIniVersion.txt L5066) -> `r.ScreenPercentage`.
pub fn screen_percentage(level: u32) -> f32 { match level { 0 => 50., 1 => 71., 2 => 87., _ => 100. } }
/// The 3D view size for `r.ScreenPercentage` (SceneView.cpp:1950-2008): `Fraction = clamp(p / 100, 0.1, 4)` (1 at exactly 100), view rect max =
/// `ceil(size * Fraction)` (`FIntRect::Scale`, IntRect.h:314-320) rounded up to a multiple of 4 (`QuantizeSceneBufferSize`, RenderUtils.cpp:914-922).
/// Kept within the window (the menu never offers more than 100 %).
pub fn scaled_view_size(width: u32, height: u32, percentage: f32) -> (u32, u32) {
    if percentage == 100. { return (width.max(1), height.max(1)); }
    let f = (percentage / 100.).clamp(0.1, 4.);
    let q = |n: u32, full: u32| ((((n as f32 * f).ceil() as u32) + 3) & !3).clamp(1, full.max(1));
    (q(width, width), q(height, height))
}
/// Brightness slider 0..1 -> display exponent applied after the sRGB encode (gamma 1.7..2.7, 2.2 = identity).
pub fn display_exponent_for_brightness(brightness: f32) -> f32 { 2.2 / (1.7 + (2.7 - 1.7) * brightness.clamp(0., 1.)).max(0.1) }

/// FXAA parameters (old `settings::FxaaParams`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxaaParams {
    /// Sub-pixel aliasing removal (0..1).
    pub subpix: f32,
    /// Minimum local contrast that counts as an edge.
    pub edge_threshold: f32,
    /// Contrast floor for dark areas.
    pub edge_threshold_min: f32,
    /// Edge-end search steps in each direction (max 12).
    pub search_steps: i32,
}

/// Pause-menu backdrop: the 3D scene stays visible behind the menu, blurred and dimmed.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Backdrop {
    /// 0 = off, 1 = fully black.
    pub dim: f32,
    /// 0 = sharp .. 1 = strongest blur (HDR mip-chain blur radius).
    pub blur: f32,
}
impl Backdrop {
    pub const OFF: Backdrop = Backdrop { dim: 0., blur: 0. };
    /// A reasonable pause-menu look (scene clearly visible, soft and darker).
    pub const PAUSE_MENU: Backdrop = Backdrop { dim: 0.45, blur: 0.5 };
    pub fn active(&self) -> bool { self.dim > 0. || self.blur > 0. }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssr_scalability_follows_the_cook_and_the_pp_quality_cap() {
        // r.SSR.Quality 0 / 0 / 2 / 3 / 4: off at Low / Medium; PP quality 50 caps the shader at 2 (16 steps, 1 ray) for High, Epic and Cine
        let q: Vec<_> = (0..5).map(|l| ssr_shader_quality(EffectsScalability::level(l).ssr_quality, SSR_PP_QUALITY, SSR_PP_INTENSITY)).collect();
        assert_eq!(q, vec![None, None, Some(2), Some(2), Some(2)]);
        assert_eq!(ssr_shader_quality(3, 100., 100.), Some(3));
        assert_eq!(ssr_shader_quality(3, 50., 0.5), None);
        assert!((ssr_roughness_mask_scale(SSR_PP_MAX_ROUGHNESS, 2) + 20. / 3.).abs() < 1e-5);
        // GetRoughnessFade: 0 from roughness 0.3 up
        let fade = |r: f32| (r * ssr_roughness_mask_scale(0.6, 2) + 2.).min(1.);
        assert!(fade(0.3).abs() < 1e-5 && fade(0.1) == 1. && (fade(0.2) - 2. / 3.).abs() < 1e-5);
    }
    #[test]
    fn tables_match_the_old_runtime() {
        assert_eq!(shadow_map_size(0), None);
        assert_eq!(shadow_map_size(1), Some(1024));
        assert_eq!(shadow_map_size(2), Some(2048));
        assert_eq!(shadow_map_size(3), Some(2048));
        assert_eq!(far_plane_m(3), crate::camera::FAR);
        assert!(aa_params(0).is_none() && aa_params(3).unwrap().search_steps == 12);
        assert_eq!((0..4).map(texture_params).collect::<Vec<_>>(), vec![(0, 1), (0, 2), (0, 4), (0, 8)]);
        assert!((display_exponent_for_brightness(0.5) - 1.0).abs() < 1e-6);
    }
    #[test]
    fn scalability_groups_follow_cooked_ini_version() {
        // AntiAliasingQuality@0..3: off, FXAA preset 1 (console), fast TAA, full TAA; Sharpen 0/0/0.5/0.5
        assert_eq!((0..4).map(|l| AaScalability::level(l).method()).collect::<Vec<_>>(),
            vec![AaMethod::None, AaMethod::Fxaa { preset: 1 }, AaMethod::TemporalAa { fast: true }, AaMethod::TemporalAa { fast: false }]);
        assert_eq!((0..4).map(|l| AaScalability::level(l).sharpen()).collect::<Vec<_>>(), vec![0., 0., 0.5, 0.5]);
        // PostProcessQuality: bloom everywhere, SSAO / vignette from level 1, grain quantization from level 2
        assert_eq!((0..4).map(post_flags).collect::<Vec<_>>(), vec![(false, true, false), (true, true, true), (true, true, true), (true, true, true)]);
        assert_eq!((0..4).map(|l| PostProcessScalability::level(l).grain_quantization()).collect::<Vec<_>>(), vec![false, false, true, true]);
        assert_eq!(PostProcessScalability::level(3).bloom_scalability(), fn_postfx::bloom::BloomScalability::level(3));
        // ViewDistanceQuality: scale only, the far plane stays
        assert_eq!((0..4).map(|l| ViewDistanceScalability::level(l).view_distance_scale).collect::<Vec<_>>(), vec![0.5, 0.67, 0.83, 1.0]);
        assert!((0..4).all(|l| far_plane_m(l) == crate::camera::FAR));
        assert_eq!(scaled_cull_distance(0., 0.5), 0.);
        // ResolutionQuality: 50/71/87/100 %, quantized to multiples of 4
        assert_eq!(scaled_view_size(1920, 1080, 100.), (1920, 1080));
        assert_eq!(scaled_view_size(1920, 1080, 50.), (960, 540));
        assert_eq!(scaled_view_size(1280, 720, 71.), (912, 512));
        assert_eq!(TextureScalability::level(0).over_budget_mip_drop(), 1);
    }
    #[test]
    fn fort_globals_cull_curve_hits_its_keys() {
        use fort_cull::*;
        assert!((cull_distance_m(0.3) - 60.).abs() < 1e-3, "below MinCullObjectSize clamps to MinCullDistance");
        assert!((cull_distance_m(4.5) - 180.).abs() < 1e-3);
        assert!((cull_distance_m(10.24) - 240.).abs() < 1e-3 && (cull_distance_m(50.) - 240.).abs() < 1e-3);
        // log10 shape: half way in size is ~74 % of the way in distance
        let mid = cull_distance_m((4.5 + 10.24) * 0.5);
        assert!((mid - (180. + 60. * (5.5f32).log10())).abs() < 1e-2, "{mid}");
    }
    #[test]
    fn shadow_levels_follow_fortnite_scalability() {
        let s: Vec<_> = (0..4).map(ShadowScalability::level).collect();
        // r.ShadowQuality 0 / 3 / 5 / 5 -> off, 3x3 PCF, 6x6, 6x6; Fortnite keeps the light function on at @0 (engine: off)
        assert!(!s[0].shadows_enabled() && s[0].light_functions_enabled() && s[0].simple_forward_shading);
        assert_eq!(s.iter().map(|x| x.pcf_quality()).collect::<Vec<_>>(), vec![1, 3, 5, 5]);
        // MaxCascades 1 / 1 / 2 (Fortnite; engine 4) / 10 against DynamicShadowCascades 3
        assert_eq!(s.iter().map(|x| x.csm().num_cascades).collect::<Vec<_>>(), vec![1, 1, 2, 3]);
        // 45 m x DistanceScale 0.6 / 0.7 / 0.85 / 1.0; CascadeTransitionFraction 0.1 x TransitionScale 0 / 0.25 / 0.8 / 1.0
        for (x, d, t) in [(&s[0], 27., 0.), (&s[1], 31.5, 0.025), (&s[2], 38.25, 0.08), (&s[3], 45., 0.1)] {
            assert!((x.csm().distance_m - d).abs() < 1e-4 && (x.csm().transition_fraction - t).abs() < 1e-6, "{x:?}");
        }
        assert_eq!(s.iter().map(|x| x.radius_threshold).collect::<Vec<_>>(), vec![0.06, 0.05, 0.04, 0.03]);
        assert_eq!(s.iter().map(|x| (x.distance_field_shadowing, x.volumetric_fog)).collect::<Vec<_>>(), vec![(false, false), (false, false), (true, true), (true, true)]);
        assert_eq!((s[2].volumetric_fog_grid_pixel_size, s[2].volumetric_fog_grid_size_z, s[3].volumetric_fog_grid_pixel_size, s[3].volumetric_fog_grid_size_z), (16, 64, 8, 128));
        assert_eq!(ShadowScalability::default(), s[3]);
    }
    #[test]
    fn cascades_follow_the_level_split_setup() {
        use glam::{Mat4, Vec3};
        let to_sun = Vec3::new(0.3, 0.8, -0.5).normalize();
        // Medium: one cascade over 0.1 .. 31.5 m; with the ray-traced cascade it is not the last one and is extended by 2.5 % of its length
        let c1 = crate::lighting::fit_cascades_ex(Mat4::IDENTITY, 0.84, 1.5, 0.1, to_sun, 1024, true, &ShadowScalability::level(1).csm());
        assert!((c1[0].far - 31.5).abs() < 1e-4 && (c1[0].fade_extension - 31.4 * 0.025).abs() < 1e-4, "{:?}", c1[0]);
        assert_eq!(c1[0].resolution, 1024 - 8);
        // High: two cascades (exponent 2 -> 1/3, 1 of 38.25 m), transition 0.08
        let c2 = crate::lighting::fit_cascades_ex(Mat4::IDENTITY, 0.84, 1.5, 0.1, to_sun, 2048, false, &ShadowScalability::level(2).csm());
        assert!((c2[0].far - (0.1 + 38.15 / 3.)).abs() < 1e-3 && (c2[1].far - 38.25).abs() < 1e-3, "{} {}", c2[0].far, c2[1].far);
        assert!((c2[0].fade_extension - (c2[0].far - 0.1) * 0.08).abs() < 1e-4 && c2[1].fade_extension == 0.);
    }
    #[test]
    fn small_casters_are_culled_by_screen_radius() {
        // Epic 0.03 at FOV 90 (LODDistanceFactor 1): a 1 m sphere survives up to 33.3 m
        assert!(!shadow_caster_screen_size_culled(1., 33. * 33., 0.03, 1.));
        assert!(shadow_caster_screen_size_culled(1., 34. * 34., 0.03, 1.));
        // zooming in (smaller FOV) keeps it longer
        assert!(!shadow_caster_screen_size_culled(1., 34. * 34., 0.03, lod_distance_factor(80.)));
        assert!((lod_distance_factor(90.) - 1.).abs() < 1e-6);
    }
}
