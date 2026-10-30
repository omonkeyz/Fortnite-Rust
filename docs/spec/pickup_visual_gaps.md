# Pickup visuals: verified gaps and fix spec (2026-10-05)

Verifier pass on two changes: Change A (`pickup_orig.rs`: dropped weapons drawn with their cooked `WeaponMat_Base` material in the gun pass) and Change B (the hologram rim: `CullingSign`, skin `MeshOrigin`/`MeshExtension`). The pass was read-only. Evidence is in `verification/pickups/verify/`, and the scripts are `close.script` and `framed.script` in that folder.

## 1. Why dropped weapons look too dark ("shadering too much")

**Root cause:** the gun pass has no image-based specular term. Severity: high. This is the dominant visual error of `--original-gun-material`.

* The cooked base-pass PS (`WeaponMat_Base`, for example `shader_110111`, a `TBasePassPSFNoLightMapPolicySkylight` program) writes `Metallic = saturate(SpecularMasks.g)`, `Specular = SpecularMasks.r * cb2[19].y`, `Roughness = lerp(cb2[19].w, cb2[19].z, SpecularMasks.b)`, `BaseColor = saturate(Diffuse)` and AO = 1.
* GBuffer dump (`FNRT_GUN_DBG_TMP=2/3`, `gbuffer_dump_ar.png`): the M4 receiver, handguard and barrel come out with metallic close to 1 and a light-grey base colour.
* In `gun_resolve.wgsl` a metallic pixel has `DiffuseColor = 0`. Its only light is the sun GGX lobe, which is a tiny highlight. The UE4.16 deferred `ReflectionEnvironment` pass would add `SpecularColor * EnvBRDF * (sky-light cubemap + reflection captures + SSR) * specular occlusion`, and that term is missing.
* Measured at 13h with the hologram off, on the same pixel: the original material gives (13,14,10) and the default renderer model gives (54,52,42). The original material is about 4x darker and reads as black (`cmp_h13_noholo.png`).

**Fix:** add the sky light's specular to `gun_resolve`. This is the 4.16 `ReflectionEnvironmentShared.usf` / `ReflectionEnvironmentPixelShader.usf` path:
* `GetSkyLightReflection(R, Roughness)`: sample the prefiltered `T_hdrsky_test03_low` cube (the SLS_SpecifiedCubemap capture, filtered like `FilterReflectionEnvironment` at `CubemapResolution` 128). Use mip `ComputeReflectionCaptureMipFromRoughness(Roughness, View.ReflectionCubemapMaxMip)` and multiply by `View.SkyLightColor`.
* Multiply by `SpecularColor * EnvBRDFApprox` (or the 4.16 `EnvBRDF` LUT), and by specular occlusion `GetSpecularOcclusion(NoV, Roughness*Roughness, AO)`, where AO is the DFAO value.
* Reflection captures and SSR are a separate, smaller gap. Outdoors the sky term dominates. Indoors (the reference shot), Athena reflection captures matter, so port them later.
* This term is missing for every surface of the renderer (`sky_light.rs`: "SPECULAR contribution ... NOT ported"). Metals in the gun pass are the case where it is fatal.

## 2. Flag-default decision (`--original-gun-material`)

* **Current state:** `cli.rs` defaults to `original_gun_material: false`, documented as "Diagnostic ... until a capture proves that it produces pixels" (`app.rs` ~line 528). The default path, `Scene::install_guns` plus `pickup_mesh.rs`, is the renderer `Prop` shading. It uses the original Diffuse and Normals textures and treats everything as dielectric, ignoring metallic, specular and roughness, so it is **not 1:1**. It looks brighter because metal is lit as diffuse.
* **Decision: keep it OFF by default for now.** It now produces pixels, but turning it on today makes every held and dropped gun near-black (section 1) and removes their shadows (section 3). That is a regression the user would see at once.
* **Flip it ON by default** for the held gun and dropped weapons together once these land in this order:
  1. sky-light specular (section 1);
  2. gun-pass shadow casting (section 3);
  3. DFAO applied to the gun pass (section 4).
  Then re-run `close.script` at 13h and 22h. The receiver should read mid-grey, not black.
* `pickup_orig::draw` already keys off `scene.orig_guns.is_some()`, so held and dropped weapons always share one lighting path. CONFIRMED.

## 3. Gun pass casts no shadow

Severity: medium. Dropped (and held) weapons in the original-material path draw no CSM, DF or capsule shadow. The default model does cast one (`cmp_h13_noholo.png`, `cmp_legendary_noon.png`).

**Fix:** add a depth-only pipeline per gun section that uses the same translated VS (or the cooked `TShadowDepthVS*` of the `WeaponMat_Base` map, which is in the export) and draws into the cascade maps the renderer already renders.

**Open:** whether `B_Pickups`' `SkeletalMeshComponent` has `CastShadow` / `bCastDynamicShadow` set. The pickup component setup is Blueprint data, not native. `AFortPickup`'s native ctor is trivial (decomp `AFortPickup__InternalConstructor` @1409a4dc0). Read it from the cooked `B_Pickups` CDO before shipping the fix.

## 4. Gun-pass lighting vs UE4.16 deferred

| term | status | note |
|---|---|---|
| sun BRDF | CONFIRMED | Lambert, D_GGX, Vis_SmithJointApprox and F_Schlick match 4.16 DefaultLit. Not reproduced: the directional `SourceRadius` energy normalisation (LightSourceAngle). Minor. |
| CSM / DF shadow (received) | CONFIRMED | Uses the shared `sun_visibility`, so `sun_shadow` includes the ray-traced DF cascade. |
| sky SH diffuse | CONFIRMED (equivalent) | Uses `view_buffer_sh(sky_sh)`, unrotated, which matches `sky_lookup`. UE 4.16 does NOT evaluate a Movable sky light in the base pass (`ShouldRenderSkylightInBasePass_Internal`: opaque requires stationary or forward). It uses the non-Skylight PS and adds `SkySH * DiffuseColor * AO` in the deferred sky pass. The math is the same apart from AO and DFAO. |
| DFAO | MISMATCH (low outdoors, medium indoors) | `gun_resolve` never calls `ambient_light()`, so `g_amb_set` stays false and `df_sky_occlusion` returns the term unoccluded. **Fix:** pass `n` through `ambient_light(n)` once (or set `g_amb_n = n; g_amb_set = true;`) before `output_hdr`. Also apply the DFAO to the specular term once it exists. |
| specular IBL / captures / SSR | MISMATCH (high) | See section 1. |
| shadow casting | MISMATCH (medium) | See section 3. |
| `View[60]` | harmless | `fn-matshader` writes the eye position to slot 60 (TranslatedWorldCameraOrigin, which UE sets to 0). No cooked `WeaponMat_Base` GBuffer PS reads 59..62; only the port's own world rebuild uses slot 60. |

**Ammo, consumables and resources** (static meshes, renderer model): they go through the scene `Prop` path, so they get the same sun, CSM, DF, sky and DFAO as the world, and they cast shadows (`medkit_static_noon.png`). They are correctly lit for the port's model, but not with their cooked materials (the `FLocalVertexFactory` permutation is not wired). Severity: low to medium.

## 5. Hologram (Change B)

* **CullingSign: CONFIRMED.** `View[130].w = 1`. GameTime and RealTime sit at `131.z`/`131.w`, the same place the gun programs read time, and `char_scene.wgsl` independently writes `[130] = (0,0,0,1)`. Minor gap: `NearPlane` (`131.x`) is left at 0. No hologram program was seen reading it.
* **Skin MeshOrigin / MeshExtension: CONFIRMED.** The cooked 4-influence skin VS (`M_Pickup_Blueprint_01` `shader_69273`, `CB0[16]`) applies `cb0[7] * v0 + cb0[6]`, and `pickups_dxbc.rs` fills them per mesh.
* **Rim:** crisp and rarity-coloured at noon, night (22h) and legendary (`cmp_holo_noon_night.png`, `cmp_legendary_noon.png`).
  * The "common rim is about 0.25 HDR" claim was not re-measured. It is plausible against the dark reference.
  * Open visual item: on the rare AR the additive sweep and fill make the interior read strongly cyan at noon. In the reference the mesh stays mostly visible. Most of that difference is the near-black gun under the fill (section 1) plus exposure. Re-judge after section 1.
* **Storm fixer's shared `primitive_ub` and the 18-float4 Primitive: CONFIRMED not broken.** Hologram, dome grid (`M_Pickup_Blueprint_BG_01`) and god-rays (`M_Pickup_RaysBG_01`) all render: rays and dome are visible on the legendary item in both modes.

## 6. Hologram VS fog inputs: exact spec

The cooked VS (`shader_69273`; same layout in `24156`, `39028` and `83711`) reads the loose `$Globals` (`CB0`, the buffer that already holds MeshOrigin/MeshExtension) and two resources. Today they are zero, and `t1`/`t2` are unbound.

**Why the hologram still shows:**
1. With falloff 0 the line integral is `0/0 = NaN`.
2. `min(NaN, 1)` returns 1 on D3D (and on most GPUs), so the transmittance comes out as 1.
3. The hologram therefore shows by accident. UE's no-fog defaults would be `ExponentialFogParameters = (0,1,1,0)` (SceneRendering.cpp:461).

Athena has height fog and volumetric fog (`fn-render/src/fog.rs`), so feed the real values.

Source: `FExponentialHeightFogShaderParameters::Set` (Engine/Public/FogRendering.h:26), `FSceneRenderer::InitFogConstants` (FogRendering.cpp:268) and `FVolumetricFogParameters::Set` (VolumetricFog.h:45).

Values: `p = fog::active()` (`PhaseFog`). `D = p.fog_density * DENSITY_REMAP` and `F = p.fog_height_falloff * DENSITY_REMAP`. `H = fog::day::HEIGHT_FOG_Z_OFFSET` (cm, as `HeightFogPs`). `CamZ` is the camera Z in UE cm.

| CB0 slot | member | value |
|---|---|---|
| 8 | ExponentialFogParameters | `(D * 2^clamp(-F*(CamZ - H), -125, 126), F, CosTerminatorAngle = clamp(cos(LightTerminatorAngle deg), -1+δ, 1-δ), p.start_distance)` |
| 9 | ExponentialFogColorParameter | `(p.fog_color, 1 - p.fog_max_opacity)` |
| 10 | ExponentialFogParameters3 | `(D, H, 0 /* no inscattering cubemap */, 0 /* FogCutoffDistance */)` |
| 11.xy | SinCosInscatteringColorCubemapRotation | `(0, 1)` (angle 0; unused while 10.z = 0) |
| 12 | InscatteringLightDirection | `(direction TO the sun in UE axes = -Proxy->GetDirection(), 1)` when the sun is the atmosphere sun light and `luminance(dir_color) > KINDA_SMALL_NUMBER`, else `(0,0,0,0)` |
| 13 | DirectionalInscatteringColor | `(p.dir_color * Luminance(sun light colour = p.sun_light_rgb()), clamp(p.dir_exponent, 1e-6, 1000))` |
| 14 | `.x` DirectionalInscatteringStartDistance, `.yzw` FogInscatteringTextureParameters | `(p.dir_start_distance, InvRange, -NonDirDist*InvRange, NumMips = 1)`, where `InvRange = 1/max(FullyDirDist - NonDirDist, 1e-5)` (only used when 10.z > 0) |
| 15.x | ApplyVolumetricFog | `1.0` when the renderer's volumetric fog ran this frame (`VolFog::active()`), else `0` |

Resources:
* `t1` (TextureCube) = a 1x1 white cube (`GWhiteTextureCube`), with `s0` trilinear clamp.
* `t2` (Texture3D) = the renderer's integrated light-scattering volume (`volfog.rs`, Rgba16Float), with `s1` bilinear clamp. When the volumetric fog is off, bind a 1x1x1 black volume (`GBlackVolumeTexture`).

View slots the VS reads (fill them in `fn_fx::view::ViewParams`):
* `[175].z` = `VolumetricFogInvGridSize.z`. Fill all of `[175].xyz` = `1/grid`.
* `[176].xyz` = `VolumetricFogGridZParams (B, O, S)`, the same values volfog uses.
* `[177].xy` = `VolumetricFogSVPosToVolumeUV`. `[177].z` = `VolumetricFogMaxDistance` = `fog::day::VOLUMETRIC_FOG_DISTANCE` (4500) when volumetric fog is on, else 0.
* `[52]` (ViewForward), `[59]` (WorldCameraOrigin), `[60]` (TranslatedWorldCameraOrigin = 0), `[62]` (PreViewTranslation) and `[4..7]` (WorldToClip) are already filled.

The pixel side of an additive material is `Color * Fogging.a * Opacity` (BasePassPixelShader.usf:993). Only the transmittance (`o4.w`) changes the hologram, and inside the 40 m cull distance the effect is small. Severity: low. The fix is still needed to stop relying on the NaN accident.

The same block applies to `M_Pickup_Blueprint_BG_01` and `M_Pickup_RaysBG_01` (particle VFs, also translucent base pass) if their programs declare it. Check their `CB0` reads the same way: `fn-dxbc material <out> <package>`, then grep for `cb0[8..15]`.

## 7. Status (2026-10-05, gun-pass lighting pass)

Sections 1, 3 and 4 are implemented and `--original-gun-material` is now the default (held gun, held pickaxe and dropped weapons). `--standin-gun-material` forces the renderer Prop model; a material that fails to load falls back to it and is logged.

* **Sky-light specular (section 1):** `fn-render/src/sky_reflection.rs` (shared) + `lit_common.wgsl` `sky_specular`. The processed cube is `T_hdrsky_test03_low` through `FilterReflectionEnvironment` (128^2, 8 mips, `FilterPS` GGX importance sampling), looked up with `ComputeReflectionCaptureMipFromRoughness(R, 7)` x `View.SkyLightColor`, applied like `ReflectionApplyPS` with APPLY_SKYLIGHT (EnvBRDF with the CPU-built `PreIntegratedGF` LUT, off-specular-peak R, AO specular occlusion, DFAO specular occlusion with `r.AOSpecularOcclusionMode=0` from CookedIniVersion.txt:977). Wired into the gun pass, `char_scene.wgsl`, `scene.wgsl` (`shade_character`, `shade_architect`) and `static_scene.wgsl`. The cube is loaded by `fn-app/src/sky_refl_hook.rs`. Not reproduced: SSR (r.SSR.Quality 2/3 at High/Epic) and the SSAO factor of the specular occlusion. `FN_SKY_REFLECTION=0` is a debug switch.
* **Reflection captures:** none. No `*ReflectionCapture*` export exists in any of the 1,076 cooked `.umap` files except `/Game/Maps/FrontEnd/Maps/FrontEndStore` (a BoxReflectionCapture in the Save the World front end). That includes `Athena_Terrain` and its 35 `LevelStreamingAlwaysLoaded` sublevels. With no captures, `ShouldDoReflectionEnvironment()` is false (ReflectionEnvironment.cpp:907), so the sky term above is the whole 4.16 reflection environment for Athena. Blueprint class templates were not scanned.
* **Shadow casting (section 3):** every gun pipeline has a depth-only twin (the same translated VS, a shadow View buffer that makes SV_Position the world position, then `shadow.wgsl`'s cascade transform + ShaderDepthBias) drawn into each CSM cascade, culled by the material's TwoSided flag like the base pass. The guns do not cast into the DF cascade: skeletal meshes have no distance field in 4.16. Open question answered: `B_Pickups`' construction-script mesh templates (`NODE_AddSkeletalMeshComponent-0/1`) serialize only BodyInstance / CanCharacterStepUpOn / bReceivesDecals, and the bytecode never touches shadows, so `CastShadow` = `UMeshComponent` default true (MeshComponent.cpp:16) and `bCastDynamicShadow` = true (PrimitiveComponent.cpp:171).
* **DFAO (section 4):** `gun_resolve` sets the shading normal of the sky term, so `output_hdr` applies the DFAO to the program's sky diffuse. The specular gets the DFAO specular occlusion. SSAO reaches the diffuse through the ambient share as before.
* Evidence: `verification/gunpass/`. In `cmp_dropped_h13.png` the receiver mean is (14,19,10) in the earlier capture and (96,89,66) now, against (115,106,80) for the Prop model. With only the sky specular switched off on the current tree it is (79,69,39), so part of the earlier darkness was fixed by other changes made since that capture (for example the sky-axis fix). The sky specular adds about 20% on the receiver and turns the pickaxe blade from black to a sky reflection (`cmp_pickaxe_sky_reflection.png`).

## 8. Re-verification of section 7 (independent verifier, 2026-10-05)

I rebuilt the current tree and re-ran `verification/pickups/verify/close.script` with `WID_Assault_Auto_Athena_R_Ore_T03` (`v2_*` folders). Comparison strips: `v2_cmp_noholo.png` and `v2_cmp_holo.png`.

**CONFIRMED: the default is the cooked material.**
* The log shows `ORIGINAL gun material ON` with no flag.
* The dropped rifle (`SK_MachineGun_01` / `MI_FORT_Rifles_MachineGun_1`) and the pickaxe go through the gun pass.

**CONFIRMED: dropped-weapon brightness.** Receiver pixel at (853,493) / (900,488), 13h, hologram off:

| run | (853,493) | (900,488) |
|---|---|---|
| cooked, now | (35,33,26) | (77,73,54) |
| cooked, first capture | (13,14,10) | (3,5,2) |
| `--standin-gun-material` | (52,49,41) | (102,91,70) |

* The metal now reads mid-grey with sky reflection. It is about 70-75% of the Prop stand-in, which is expected: the stand-in wrongly lights metal as diffuse.
* It is no longer near-black. The "too dark" complaint is resolved for weapons.

**CONFIRMED: shadows and night.**
* The dropped rifle now casts a CSM shadow, as the Prop path does.
* At 22h the gun stays readable.

**CONFIRMED: hologram, dome and rays.**
* The rim stays crisp at 13h and 22h.
* The legendary god-rays and the dome grid render.

**STILL OPEN:**
* **Section 6, hologram VS fog inputs (low).** `CB0[8..15]`, `t1`, `t2` and View `[175..177]` are still zero or unbound (no fog code in `pickups_dxbc.rs` or `fn_fx::view`). The transmittance is still 1 only through the NaN path.
* **Rare/epic hologram fill strength (low/medium, unverified).** On the rare AR the additive sweep and fill tint most of the receiver cyan. The reference legendary shows more of the bare mesh, but it is an indoor, low-exposure shot. Judge this against a capture with matched exposure, or measure the cooked `Sweep Brightness` per rarity against the material output, before changing anything.

## 9. Pickup flight, backpack shrink and pickup messages (2026-10-24)

* **Flight:** `FUN_140696f90` builds a 5-point `FortCatmullRomSpline` (vtable 0x14415ccb8; eval `FUN_140665d30`/`FUN_140662120`, uniform over `n-3` segments, duration = FlyTime at +0xc): P0 = start + k*(0,0,1) (every `ServerHandlePickup` caller passes Up: 0x14043ac20, 0x1406841d0), P1 = start, P2/P3/P4 = T - k*D, T, T + k*D. k = `FortGameData.PickupSplineOffsetRange[1]` (+0xd4, property offset 0xd0 ArrayDim 2, = 175). `FUN_14069a150` re-tracks P2..P4 each tick and places the actor at spline(Duration * LootBackpackCurves.X(alpha)). T/D = pawn vfunc +0x8b0 (`FUN_1407dc8a0`): socket `LootEntryPosition` (spine_04, (13.93,-12.92,0) cm) of the backpack part / pawn mesh, D = pawn rotation * Down. Ported in `fn_game::pickups::flight_point`.
* **Backpack shrink:** `FUN_14069a150` writes (T, LootBackpackCurves.Y) to the effect's `BackpackWorldPosition`; `B_Pickups` ReceiveTick copies it to every item MID as `WorldPostionOfBlackHole` (`BlackHoleFunction`, in `WeaponMat_Base`; other materials are reparented to `BackPackShrinkingEffect`). Ported as a per-draw Material uniform override (gun pass and static-item pass). The ammo / wood materials of the test read it.
* **SizeSphere:** `1 - ShrinkParticleWhenPickedUpCurve(TickAccumulation)` on P_Pickup after pickup. Ported.
* **Pickup messages:** `AthenaPickupManager.HandleItemPickedUp` -> `UFortUIMessageManager::AddMessageItem` (0x142edb890: same name merges, `StackSize += Count`, display timer restarts 0x142edca60/0x142f41260), max 3 displayed, `PickupMessageItem_C` DisplayDuration 2.5, RemoveDuration 0.2 (ctor 0x142ed19e0), Appear 0.3 s / Disappear 0.2 s tracks. Ported in `fn-ui/src/screens/pickup_stream.rs`, composed from the cooked tree.
* **Open:** `P_PickupRibbon_01/Ribbon` (ribbon type data not decoded), the exact semantics of `BlackHoleFunction` (editor expressions stripped; only the compiled programs exist) were not checked against a real 1.8 capture, multi-pickup roll angle (+40 per queued entry, `FUN_1407cd790`) not ported, merging into a message already in Disappear.
