# Global look audit (whole-frame pipeline vs UE 4.16 / Fortnite 1.8), 2026-10-05

This audit was stopped early when the coordinator called a wrap-up. Rows marked OPEN were found but not fixed. Rows marked "edited, unverified" were changed by an audit sub-agent that was stopped before it could build or test the change. Captures are in `verification/global_look/`:
- `run.sh` and `look.script` set up the runs.
- `before_h13`, `after_h13`, `before_h20.5` and `after_h20.5` hold the screenshots.
- `sbs_h*_yaw*.png` are side-by-side images, before on the left and after on the right.

The "before" build was taken from the shared working tree, so before/after also includes edits by the other agents working in parallel. For example, the tree canopies that were grey before are fixed in "after"; that is most likely the SpecularMasks sRGB fix in row 4 below.

## Mismatches, ranked by visual impact

| # | Item | Evidence (UE / DXBC / cooked / exe) | Port before | Status |
|---|---|---|---|---|
| 1 | **CombineLUTs `/ 1.05` missing**: the whole frame was 5 % too bright in display space and highlights clipped early | 1.8 CombineLUTs DXBC `GlobalShaderCache#1182809` ends with `mul o0.xyz, r0.xyzx, l(0.952381)`; the 4.16 source is `PostProcessCombineLUTs.usf:409 OutColor.rgb = OutDeviceColor / 1.05`. The tonemap PS `#1070630` then does `* 1.05`. | `post.wgsl` did `min(saturate(enc) * 1.05, 1)`. Mid grey came out at 123/255 instead of 117/255; linear 1.0 at 225 instead of 215; everything above 0.952 encoded clipped to white. | **FIXED** |
| 2 | **No real colour-grading LUT**: the port evaluated the transform analytically per pixel | UE bakes a 32^3 `PF_A2B10G10R10` volume LUT (`PostProcessCombineLUTs.cpp:1022`, `GLUTSize` 32) and samples it trilinearly at `saturate(log2(c)/14 + 0.610727)*31/32 + 0.5/32` (tonemap DXBC). The 1.8 build does NOT subtract `LogToLin(0)` (no such add in either DXBC). | A CPU model of the LUT vs the direct evaluation: mean 0.32/255; 5 % of colours differ by more than 1/255; max 29/255 (saturated darks). | **FIXED**: new `src/color_lut.rs` + `shaders/color_lut.wgsl` build the LUT every frame from the post uniform block (10-bit rounding); post.wgsl binding 10 samples it. The grain is now added after `LUT * 1.05`, before the unorm write (DXBC order). |
| 3 | **Time-of-day post-process blend mechanism**: `AutoExposureBias` 0.47 applies in every phase, not only Day | Exe: TODM ctor `14062c240` sets phase PPC `Priority = i - 4.0` (`DAT_143ff9024` = 4.0, read from the exe) and creates `<Phase>LowPriClonedPPC` for Day/Evening/Night with `Priority = i - 8`, all `bUnbound`. `FUN_14064ee70` sets `PPC[i].BlendWeight = (i <= to) ? 1 : 0`, copies `PPC[i].Settings` into each clone, and gives the clone weight 1. `FUN_14064e920` then sets `PPC[to].BlendWeight = t` during a transition. UE applies volumes in ascending priority (`UnrealClient.cpp:1022 InsertVolume`, `SceneView.cpp:1612`). So the order is: clones Day, Evening, Night (weight 1), then Morning (1), Day, Evening, Night (by weight). Only Day overrides `AutoExposureBias`, and its clone always applies, so the bias is 0.47 at every hour. Every other modelled field is overridden by every phase, so those results are unchanged. | `tonemap::PostSettings::blend` = `from` at weight 1 then `to` at weight t. Evening / Night / Morning get bias 0, which makes them 2^0.47 = 1.39x darker than 1.8. | **FIXED** (checked 2026-10-24): `tonemap::PostSettings::blend` applies `time_of_day::pp_components` in `pp_volume_order` (clones first). Tests `tonemap` (Day's bias 0.47 at every hour, no pop from 16 h to 20 h) cover it. |
| 4 | Prop/foliage SpecularMasks and opacity masks with SRGB=true read raw | 15 SpecularMasks (e.g. `Tree_Large_Canopy_S`, `T_Tree_Large_01_S`, `Shrub_02_S`) and 6 masks (e.g. `HeroTrees0_M`, `Tree_Large_Canopy_M`) are SRGB=true in the paks. UE decodes them when sampling (`D3D11RHIPrivate.h:912`). | `static_loader.rs` used the raw means. Specular and roughness came out too high on canopies, and leaf edges clipped differently. | Edited, unverified (sub-agent): `static_loader.rs` `sampled_channel`. It compiled in my build and the canopies changed; not otherwise verified. |
| 5 | Sun specular min roughness 0.08 missing | `DeferredLightingCommon.usf:164 max(LobeRoughness, MinRoughness)`. The 1.8 deferred light DXBC has a `max … cb1[4].x`; exe `ULightComponent` ctor `0x1420a4c90` stores 0.08 at +0x2d0, and TODM does not override it. | `lit_common.wgsl ggx_spec` had no clamp; the gun path clamped at 0.02. Smooth surfaces got highlights that were too sharp and too bright. | Edited, unverified (sub-agent). `lit_common.wgsl` constant `LIGHT_MIN_ROUGHNESS` 0.08; I renamed it from `SUN_MIN_ROUGHNESS`, which clashed with `backdrop.wgsl` and broke shader validation. Also NoV `saturate(abs(NoV)+1e-5)`, no Vis epsilon, `gun_resolve`/`scene.wgsl` routed through `ggx_spec_color`. Builds and runs; not otherwise tested. |
| 6 | Ocean is a hand-written approximation | `SM_Athena_Water_Ocean` / `SM_OceanFar` actors in `Athena_Terrain` use `M_Athena_Water_Ocean` (exact package path unconfirmed). | `backdrop.wgsl` / `backdrop_load.rs:67-71`; normal tiling and colour mix are assumed. | OPEN |
| 7 | Sky dome / clouds / sun use the wrong shader variant, and the phase interpolation is partial | `Athena_Terrain` WorldSettings `WorldTimeOfDayManager = TODM_A_Child_Athena_C`. Its materials are `M_AridSky_Inst_*` (dome), `M_AridClouds_Inst_*` (clouds) and `M_Sun_Arid`. The deferred base pass uses the `TBasePassPSFNoLightMapPolicy` programs: sky 10238, clouds 22102, sun 29957. The exe `FUN_14063eba0` blends phases with `K2_InterpolateMaterialInstanceParams` over every parameter. | Hand ports of the simple-lighting variants: 6890 (`sky_dome.wgsl`), 17639 (`sky_clouds.wgsl`), 25910 (`sky_sun.wgsl`). Only 8 parameters are lerped; the sun is drawn as a billboard rather than at `dir * distance` with `dir.Rotation()`. | OPEN. An unwired, uncompiled port is in `crates/fn-matshader/src/sky.rs`; it is not in `lib.rs`, so delete it or finish it. |
| 8 | Backdrop terrain/skybox textures decoded after filtering | All terrain layers and skybox textures are SRGB=true DXT. | `backdrop.rs:404` uploads them as linear and `backdrop.wgsl` decodes after filtering; the AO texture is never decoded. 0.4-5 % darkening in mips. | OPEN |
| 9 | Mips of sRGB textures built in gamma space | UE builds mips in linear float (`TextureCompressorModule.cpp:2030-2086`). | `texture.rs upload_rgba8` | Edited, unverified (sub-agent): uses `build_mips_srgb` |
| 10 | Fallback ground (`T_Grasslands_D`) decoded after filtering | SRGB=true DXT5 | `renderer.rs:251,635` + `scene.wgsl` `to_linear` | OPEN (under 1 %) |
| 11 | Storm post-process / storm fog | Not identified (hint: `fn-postfx/src/evidence.rs`, PlayerPawn_Athena) | Not implemented | OPEN, unproven |
| 12 | PreIntegratedGF stored as Rgba16Float instead of G16R16 | `SystemTextures.cpp:298-302` | `sky_reflection.rs` | Negligible (0.05 %) |
| 13 | Scene colour R11G11B10 at Effects@0-2 | cooked ini `r.SceneColorFormat` 3 / 4 | always Rgba16Float | Matches at Epic only |
| 14 | Back buffer 8-bit vs `PF_A2B10G10R10` | `D3D11Viewport.cpp:485` | Bgra8Unorm | Gamma is correct; banding only |

## Verified to match (no change)

- **Tonemapper and colour**:
  - White balance: the full `WhiteBalance`, incl. the 1.4388/1.438 correction (WhiteTint 0 in every phase).
  - ColorCorrectAll: per-region saturation, contrast and gamma; gain 1 / offset 0.
  - The ACES red modifier, which is active in the 1.8 DXBC although the 4.16 source has it `#if 0`.
  - RRT 0.96 / ODT 0.93 saturation.
  - The Film* curve. BlackClip 0 and WhiteClip 0.04 come from the exe ctor.
  - `InverseGamma.y = 2.2/DisplayGamma` (DisplayGamma 2.2 in the cooked ini).
  - MappingPolynomial: `r.Color.*` is not in the cooked ini, so it is identity.
  - `r.TonemapperFilm=1` (cooked ini L81).
  - Sharpen: 0.5/6 at AA@2/3.
  - Grain quantization (PostProcessQuality@2/3).
  - Vignette 0.01.
  - SceneFringeIntensity 0, GrainIntensity 0, SceneColorTint white.
  - The per-phase values in `tonemap::PHASES`, which match `reports/postfx-evidence.json`. There is no blue correction or expand gamut in 4.16.
- **Exposure**:
  - The histogram method (`r.DefaultFeature.AutoExposure.Method` is not in the cooked ini, so it is 0; `r.EyeAdaptationQuality` 2 at @2/3), on `max(r,g,b)` of SceneColorHalfRes.
  - 64 buckets from -8 to 4, with bucket 0 dropped.
  - Low/High 80 (Morning/Evening 50) / 98.3, Min/Max in luminance units, `2^bias / L`.
  - Speeds 3/1, exe-proven.
  - The bias issue in row 3 is still OPEN.
- **Bloom**:
  - Threshold -1 means no setup pass.
  - Intensity 0.675, from the exe ctor.
  - Tints: 1,2,4,5 from the ctor; Bloom3 0.5 and Bloom6 0 from TODM_A. Bloom6Size 0.
  - TintScale 1/6, sizes, Gaussian kernel, `r.BloomQuality` 5.
  - No dirt mask texture.
- **BRDF / sky light**:
  - D_GGX, Diffuse_Lambert, ComputeF0, Specular default 0.5, F_Schlick's `saturate(50*g)`.
  - The PreIntegratedGF LUT, `ComputeReflectionCaptureMipFromRoughness` (1 / 1.2, max mip 7), specular occlusion.
  - SH diffuse.
  - The sky light colour via `ToFColor`, times intensity.
  - Lower hemisphere not black.
  - No reflection captures in any Athena map; AmbientCubemapIntensity 0.
- **Colour spaces**:
  - Cooked-material textures follow the cooked SRGB flag.
  - BC5 normals: `rg*2-1`, `sqrt(saturate(1-dot))`, unorm, all SRGB=false.
  - The sky-light cube is linear f16.
  - Non-sRGB swapchain with a single encode in post.wgsl.
  - Vertex colours are raw /255.

## Files changed by this audit
- `crates/fn-render/src/color_lut.rs` (new) and `crates/fn-render/shaders/color_lut.wgsl` (new).
- `crates/fn-render/shaders/post.wgsl`: `lut_device_color`, `color_lookup_table`, binding 10, grain order.
- `crates/fn-render/src/renderer.rs`, at 6 marked spots `// COLOR LUT`: the field, the bgl entry, the ctor, the two post bind groups, and the dispatch in the post node.
- `crates/fn-render/src/lib.rs`: `pub mod color_lut`.
- `crates/fn-render/src/tonemap.rs`: the CPU mirror now includes the `/1.05`, docs updated, new test `lut_round_trip_is_identity_below_white`. All 14 tonemap tests pass.
- Sub-agent edits (rows 4, 5, 9): `static_loader.rs`, `texture.rs`, `lit_common.wgsl`, `scene.wgsl`, `gun_resolve.wgsl`.
- Orphan file, not compiled: `crates/fn-matshader/src/sky.rs`.

## Character lighting audit (2026-10-24)

Scope: how 1.8 lights the BR pawn (skeletal mesh) over the day, against the port. No in-match real-client reference could be captured (see the report of that task), so every row below is backed by cooked data, the decompile or the UE 4.16 source only.

| Item | Evidence | Status |
|---|---|---|
| Sun / moon colour, intensity, direction | `TODM_A` + `TODM_A_Child_Athena` phases; `FUN_14064d5d0` (FColor byte lerp, Intensity at +0x2a4); live-dump tests at 12 h and 17.5 h (`lighting.rs`, `fog.rs`) | Matches at 12 h / 17.5 h. Night (Brightness 8 inherited from the parent, colour 76/97/177) is data-backed but has no live dump. |
| Sky light | `SkyLightComponent` Movable, `T_hdrsky_test03_low` SH, `SetLightColor(ToFColor)` x alpha, OcclusionTint / MinOcclusion per phase | Matches (sky_light.rs, distance_field_ao.rs). |
| Indirect lighting cache / volumetric lightmap | `Athena_Terrain` has no `MapBuildDataRegistry` export; the building sublevels' registries are 482 bytes (no light volume); the cooked character programs are `TBasePassPSFNoLightMapPolicy` | Not used by 1.8: nothing to port. |
| Capsule shadows | `r.CapsuleIndirectShadowMinVisibility .5` in the cooked ini, but the BR body / head meshes serialize no `ShadowPhysicsAsset` | Not active in 1.8. |
| Contact shadows | `DayNightDirectionalLightComponent` serializes no `ContactShadowLength` (class default 0) | Off in 1.8. |
| `Primitive.PerObjectGBufferData` (body `o1.w`) | 0 for a skeletal mesh (no distance field); DFAO only encodes it | Correct as 0. |
| Pass `$Globals[3].x` of the head / hair / body programs | `PerFrameScalarExpression4` (shader map: byte 48 of `$Globals`) = `(1.5 + sin(fmod(Time, 0.5) * 4 pi)) * 0.4`; it scales `lerp(HitGlowOuter, HitGlow)` | FIXED: evaluated in `char_scene.wgsl` (was a constant 1.0; no visible change at rest, HitGlow is 0). |
| Tangent frame | The cooked `TangentX` / `TangentZ.w`; on the default soldier body / head the cooked `cross(Z, X) * w` runs along +dP/dV on 14006 / 6733 triangles (39 / 31 against) | FIXED: the char pass reads the cooked skinned `TangentX` and sign (vertex buffer 1); was the per-triangle screen-derivative frame (same sign, faceted). |
| `Code_FresnelColor` emissive | Only material packages reference it; no exe string, no Blueprint | Material constant, as ported. |
| Screen-space subsurface (`SS_HeroSkin_01`: ScatterRadius 2.5, SubsurfaceColor 0.6/0.514/0.349, FalloffColor 1/0.565/0.471; Epic `r.SSS.Scale 1`, `SampleSet 2`, `Quality 1`, `HalfRes 1`) | `PostProcessSubsurface.usf`, cooked profile and ini | PORTED 2026-10-24 (see "Local lights and subsurface (2026-10-24)" below). The hair uses `Character_SubsurfaceProfile` (1.7 / 0.85,0.698,0.561 / 1,0.433,0.37), not SS_HeroSkin_01. |
| Local lights | 1755 spot + 502 point light components in the exported POIs (cars, street lights, chest `TrasureLight` 1700 / 300 cm, house lamps). | PORTED 2026-10-24 (unshadowed; see below). |

## Local lights and subsurface (2026-10-24)

### Local lights (fn-render `local_lights.rs`, `lit_common.wgsl` `local_lighting`; fn-app `local_lights_hook.rs`)

* Model: UE 4.16 `GetDynamicLighting` for radial lights (DeferredLightingCommon.usf:332-560): `GetLocalLightAttenuation` (inverse squared with DistanceBiasSqr 1 and the
  `Square(saturate(1 - Square(d2 / r2)))` mask, SourceLength line irradiance, SourceRadius horizon NoL; legacy `RadialAttenuation` pow falloff), `SpotAttenuation`, `AreaLightSpecular`
  (MinRoughness, sphere / line energy and closest point), `StandardShading`, `SubsurfaceShading` (SUBSURFACE / TWOSIDED_FOLIAGE), `ComputeLightProfileMultiplier` (IES).
  CPU side: `GetParameters` of Point / SpotLightComponent.cpp (cone clamps :58-59, point SpotAngles (-2, 1), MinRoughness max(x, .04)), x16 for inverse-squared lights and
  x `GetLightFadeFactor` (ShadowRendering.h:128-146, ShadowRendering.cpp:112-127), visibility of SceneVisibility.cpp:2700-2717 (frustum on (origin, radius), MinScreenRadiusForLights
  0.03, MaxDrawDistance x r.LightMaxDrawDistanceScale on the bounding sphere, spot sphere SpotLightComponent.cpp:151), `ComputeLightBrightness` x IES `TextureMultiplier`
  (LightComponent.cpp:389-408). Verified by `local_lighting_matches_the_ue_reference` (GPU vs an f64 transcription of the .usf, 5 cases).
* Culling: LightGridInjection.cpp grid (64 px, 32 slices, GetLightGridZParams), binned on the CPU with conservative sphere bounds and no per-cell cap
  (`every_pixel_cell_lists_every_light_that_reaches_it`). Every lit pass: fx_scene_lit / sm_lit_world (POI statics, buildings, grass, scene-lit FX, character), gun_resolve,
  terrain, scene.wgsl, static_scene.wgsl.
* Blueprints: `Parent_BuildingPropActor` / `ParentBuildingContainerBlueprint` (every street lamp, lamp, desk, sign and shelf light): the day-phase intensity switch
  (`OnDayPhaseChanged` -> ubergraph disk 0xae0..0xb07) is `popflowifnot(Animate Emissive and Lights Over Time)` then `jumpIfNot(Animate Lights With A Curve OR Animate Mesh MID
  Emissive Value with a Curve)`; the parent CDO sets `Animate Mesh MID Emissive Value with a Curve - Disables time of day light controls` = true and no child CDO nor the Salty
  instances override it, `LightAnimationCurve` is unset, so no `SetIntensity` / `AddLightAnimationIntensity` ever runs: the lights keep the component Intensity at every hour
  (memory-offset check: the jump targets 4468 / 4531 map to disk 0xae0 / 0xb07, +24 bytes for the 6 object refs between them). The `DayPhaseFloats` defaults that would apply
  are the parent CDO's explicit Day 0 / Evening 0 (child structs are delta-serialized against it): Street_Light__2 Morning 80 / Day 0 / Evening 0 / Night 80.
* `Tiered_Chest_6_Parent`: UCS destroys `TrasureLight` at GetVisualEffectQuality 0 / 1; BeginPlay SetLightVisibility(!bAlreadySearched); OnLoot / OnSetSearched / ReceiveDestroyed
  SetLightVisibility(false): the light follows chest_hook's live / searched / destroyed state.
* Cars (`Car_Copper` and children): `NODE_AddSpotLightComponent-1/-2` (HLight1/2) and `-3/-5` (Taillight1/2) are added by the UCS with Intensity 0; BeginPlay with
  `Randomly start with some cars lights on` turns them on when `SetCarAlarmEnabledBasedOnLocation(ActorLocation + (114.2469, 82.7856, 45.5213)) > Percentage chance of lights being
  left off` (0.8), at HeadLightLightBrightness 2400 / TailLightBrightness 800 x `Final Random Light Intensity Variance` (FRandRange(0.8, 1): a per-actor hash stands in, CHOSEN).
* r.DetailMode (EffectsQuality 0/1/1/2) gates the `DM_Medium` lights; r.LightMaxDrawDistanceScale 0 / .5 / .5 / 1 (CookedIniVersion 5114 / 5127 / 5142 / 5157).
* OPEN: local-light shadows (every light keeps CastShadows true and r.AllowStaticLighting = 0 gives them whole-scene shadows in 1.8; here they are unshadowed, light leaks through
  walls); the 33 CastShadows = false lights should take SSAO as SurfaceShadow (1 here); volumetric fog / translucency volume injection of local lights; car hit / alarm flashing.

### Screen-space subsurface (fn-render `subsurface.rs`, `subsurface.wgsl`, char_scene.wgsl `fs_char_sss`)

* Profiles: `__SubsurfaceProfile` = allocation id of the chain's profile (MaterialInstance.cpp:171-187, `bOverrideSubsurfaceProfile` walk); head / body MIs override SS_HeroSkin_01,
  hair resolves to the root `CharacterShader`'s `Character_SubsurfaceProfile`. Kernel: SeparableSSS.cpp `ComputeMirroredSSSKernel` + SubsurfaceProfile.cpp `CreateTexture`
  (clamps, 16-bit truncation, TableMax (1, 1, 1, 3), ScatterRadius / 1024) per profile row.
* Passes (CompositionLighting.cpp:423-462, PostProcessSubsurface.usf): the non-checkerboard SUBSURFACE_CHANNEL_MODE 1 path of Epic (SceneColorFormat 4): scene colour = lights with
  BaseColor white + sky diffuse (DFAO, min(GBufferAO, SSAO), OcclusionTint) + reflections, alpha = luminance (0.3, 0.59, 0.11) of the scatterable light; SetupPS (half res),
  SSSSBlurPS X / Y (SSSS_FOLLOW_SURFACE, bilinear border), SubsurfaceRecombinePS (ComputeFullResLerp, StoredBaseColor), then the height fog (UE fogs after the pass). Cvars per level:
  r.SSS.Scale 0 / .75 / 1 / 1, SampleSet 0 / 0 / 1 / 2, Quality 0 / 0 / -1 / 1, HalfRes 1 (CookedIniVersion 5291-5333).
* OPEN: SceneColorFormat 3 (Low..High) runs the checkerboard mode in 1.8; the port uses the non-checkerboard reconstruction at every level.

