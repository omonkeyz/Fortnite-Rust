# Building core: gaps found by the verifier (1.8 BR / Athena)

Written by the independent verifier on 2026-10-05. Sources: the 1.8 exe decompile (addresses below), the SDK dump, and
`CookedIniVersion.txt` / cooked assets read with `fn-inspect`. This file gives behaviour only. No decompiled code is copied here.

Severity: **H** = changes how building feels in normal play. **M** = visible in specific situations. **L** = edge case or a doc fix.

---

## G1 (H). The camera must collide with built pieces

**1.8 behaviour.** The camera is `UFortCameraMode_ThirdPerson`. Its penetration avoidance is `FUN_1404f85a0`, with the FName `"CameraPen"`.
- **Sweep:** each feeler is a sphere sweep (`SweepSingleByChannel`, shape type 2) on **channel 4 = ECC_Camera**. It runs from the safe location to the feeler target.
- **Response of placed pieces:** they block it.
  - The `ABuildingSMActor` ctor @14045b960 gives `StaticMeshComponent0` the profile `FortBuildingMesh`.
  - The cooked `PBWA_*` components are `Custom` but have no response overrides, so they keep that profile's responses.
  - `FortBuildingMesh` (CookedIniVersion.txt line 1353) is ObjectType WorldStatic. It does not override Camera, so Camera = **Block**.
- **Response of props:** trees and rocks (`BuildingProp`) also use `FortBuildingMesh`, so they block the camera too.

**Port today.** `GameSim::camera_world` (`crates/fn-game/src/sim.rs`) traces only the baseplate and the harvestables. It never traces `building_hulls`, so the camera passes through walls.

**Spec.**
- Add the placed pieces' hulls to the camera trace closure, with the same sphere radius per feeler (`Feeler.extent`).
- Use the pieces' current class convexes (edit variants included) at their world transforms. These are the same hulls the movement uses.

**Feeler details from the native ctor @1404d43b0.** The Athena BP overrides the feeler array, and the port already reads it.
- Native defaults: 7 feelers.
  - Feeler 0: extent 14, weights 1/1, interval 0.
  - Yaw ±16: weights 0.75, interval 3.
  - Yaw ±32: weights 0.5, interval 5.
  - Pitch ±20: weights 1 / 0.5, interval 4.
- Other native defaults:
  - `CollisionPushOutDistance` 2.
  - `PenetrationBlendInTime` 0.1.
  - `PenetrationBlendOutTime` 0.15.
  - `bPreventPenetration` and `bDoPredictiveAvoidance` = 1.

**Blend logic (exe).** These are camera-crate mismatches noticed in passing (M):
- **Blending out.** The new blocked % (min over the feelers of `(hitDist - PushOut) / rayLen`, clamped to ≤ 1) can be greater than the current one. If so, it blends toward the new value at `dt / BlendOutTime`, or jumps to it when `BlendOutTime <= dt`.
- **Blending in.** If the hard (feeler 0) % is lower than the current one, it snaps to it. Otherwise, if the soft (other feelers) % is lower, it blends in at `dt / BlendInTime` (0.1 s). The port snaps immediately to the soft value instead.
- **Weights.** The exe does not scale the hit fraction by `WorldWeight`. The weight only selects world vs pawn, and a weight of 0 skips the hit. The port's `t + (1-t)(1-w)` softening has no exe basis.
- **Clamp and threshold.** The result is clamped to [0, 1]. It is applied only when it is below 0.99999 (DAT_1440c5e20).

## G2 (M). Trees and rocks obstruct placement; the "never blocks" note is wrong

`placement.rs` says player pieces and props keep `bDoNotBlockBuildings = 1` from `ABuildingActor` (`+0x564 |= 5`). That is not the final value.
- **Flag bits** (`+0x564`, SDK): bit0 `bDoNotBlockBuildings`, bit1 `bDestroyOnPlayerBuildingPlacement`, bit2 `bUseCentroidForBlockBuildingsCheck`, bit3 `bIsPlayerBuildable`.
- **`ABuildingSMActor` ctor @14045b960** runs after the base ctor and does `&= ~3; |= 4`. Every SM building actor therefore starts with **bDoNotBlockBuildings = 0** and **bUseCentroid = 1**.
- **`ABuildingProp` ctor @14045adb0** clears bit2. Props therefore block with **no** centroid test.
- **`ABuildingContainer`** also clears bit2.
- **`ABuildingPropCorner`** sets bit0, so it does not block.
- **Cooked overrides:** `Athena_Tree_Large_1` (→ `Parent_Tree`) and `Athena_Prop_Rocks_StoneLarge01` (→ `BuildingProp`) do not override any of these bits.

**The WorldOverlapCheck sweep** (`FUN_1404df7e0`, FName `"WorldOverlapCheck"`).
- **Query:** `SweepMultiByObjectType(AllStaticObjects = WorldStatic only, no custom static channels in the ini)`, box shape, swept from `gridZ + up` down to `gridZ - 18`. Up is 375 for walls and centre cells, 15 for floors.
- **Box half extents:**
  - Walls: (6, 248, 0) or (248, 6, 0), across / along the edge (DAT_144078d7c = 6, DAT_1440c5e38 = 248).
  - Floors and centre cells: (248, 248, 0).
- **Base:** gridZ + 10 for walls and centre cells (DAT_143fe3714), gridZ + 13 for floors (DAT_1440c5e2c).

**Per-hit rules, in order:**
1. **Building actor whose hit component Blocks `FCC_Trace_World` (ECC_GameTraceChannel11 = 24):**
   - If bDoNotBlockBuildings = 1, or the call is in the ignore mode (`param_7`), the hit is ignored. It does **not** count as ground either.
   - If bUseCentroid = 1, the hit counts only when the actor's `GetCentroid()` is strictly inside the swept AABB and at or above the base Z.
   - If the hit counts and bit1 (`bDestroyOnPlayerBuildingPlacement`) = 0, the result is **Obstructed (2)**. The actor is listed.
   - If bit1 = 1, the actor is listed for destruction and the result is not blocked.
2. **`AStaticMeshActor` with a `UStaticMeshComponent`:** if the impact Z is above the base and the call is not in ignore mode, the result is **Obstructed**. Otherwise it is **ground**.
3. **Any `ULandscapeHeightfieldCollisionComponent` hit:** **ground**.

**Consequences.**
- **Player pieces never obstruct each other.** The reason is the centroid test, not the flag: no neighbour's centroid can lie inside these thin boxes above the base. This matches the port's behaviour, but the comment's reasoning is wrong.
- **Trees, rocks and other `BuildingProp`s obstruct** any piece whose sweep box touches their collision. Typical cases are a wall through a trunk, or a ramp or cone over a rock. A floor is obstructed only when the prop crosses the thin slab within +15 / −18 of the floor plane.
- **Containers (chests, ammo boxes) also obstruct.**
- **POI buildings:** pre-placed POI walls and floors are `ABuildingSMActor` with bUseCentroid = 1, so they behave like player pieces.

**Spec for the port.**
- Give `PlacementEnv` the live prop / container colliders, i.e. the real props when `props real` is used.
- In `query`, after the slot check, sweep the box described above against them:
  - any overlap → `QueryResult::Obstructed`;
  - a prop overlap does **not** count as ground.
- Re-word the `placement.rs` module comment to match the flag analysis above.

## G3 (L). The target trace also hits world buildings and static meshes

The target trace is `FUN_140833cb0`, FName `"BuildingPlaceTrace"`. It is `LineTraceMultiByObjectType` with `FCollisionObjectQueryParams::AllStaticObjects`.
- The getter @140297ab0 is the static that `UCollisionProfile::LoadProfileConfig` fills.
- It is WorldStatic only, because the ini declares no `bStaticObject` channels.
- An object-type multi trace reports every hit without stopping. The hits are scanned in distance order. Hits with bStartPenetrating are skipped.

**Hit rules:**
- **Accepted:**
  - a hit on an `ABuildingSMActor` through a `UStaticMeshComponent`, with type not 4 (Prop) and not 9 (Container);
  - **or** a hit on a non-building actor through a `UStaticMeshComponent` (e.g. `StaticMeshActor`).
- **Never accepted:** landscape (not a static-mesh component), props and containers. Terrain and props do not occlude the hits behind them.

**Port.** It traces only the placed pieces. That is correct while the world is the flat baseplate with harvestables. It must be extended when POI buildings or static-mesh actors exist: those are accepted targets in 1.8.

## G4 (M). Edit reach along the camera ray

`AFortPlayerController` vfunc_685 @140827a80 computes the end of the ray as `ViewPoint.Location + ViewRot.Vector() * EditModeDistance`.
- `ViewPoint` comes from `GetPlayerViewPoint`, vtable slot 213, i.e. the camera.
- `EditModeDistance` is `+0x1A50`, which is 2048.

The edit trace `FUN_14084f8a0` (FName `"EditModeTrace"`, object-type multi trace, AllStaticObjects, ignores the pawn) runs:
- **from** the pawn location projected onto the view ray: `Cam + Dir * dot(Pawn - Cam, Dir)`;
- **to** that end point.

**Port.** `building_hook.rs` uses `reach = |pawn - camera| + 2048` from the camera itself.

**Spec.**
- Start at the projected pawn point and end at `camera + dir * 2048`. The reach measured from the camera is 2048, not D + 2048.
- Pieces between the camera and the pawn must not be hit.

This is the in-edit tile trace. Which piece edit mode starts on uses the general targeting and was not re-derived here.

## G5 (L). Structural grid bounds and neighbour checks

**Grid origin and bounds.** `UBuildingStructuralSupportSystem` is initialised in `FUN_1404f2160`, called from `AFortWorldManager` vfunc_188.
- **Origin X/Y:** whole multiples of 512.
- **Origin Z:** **−3840** (`0xC5700000`, = −10 × 384).
- **Consequence:** the lattice coincides with the world-origin snap lattice, which confirms the port's anchoring.
- **Bounds:** cell counts at least the world-manager size, with **at least 20 levels**. Points outside the box make the query return 4 (InvalidActor).
- **Port:** no bounds. This only matters above about Z = 3840 + extra levels, or outside the map.

**Neighbour support.** `FUN_1405047a0` tests a neighbour only if it is structurally supported (`IsBuildingActorStructurallySupported`) when the role is Authority. The port tests every placed piece. Unsupported pieces are destroyed by the damage hook, so there is no visible difference.

**BuildTime ≤ 0.** vfunc_372 then returns rate = Max − Start (it does not divide). The port returns 0 for that case. No 1.8 piece has BuildTime 0.

---

## Fix status (building-core fixer, 2026-10-05)

Addresses re-checked in `C:\FortniteRef\decomp\named` (round-3 names).

- **G1 done.** `fn_camera::Camera::update` now follows @1404f85a0 and its caller `UFortCameraMode_ThirdPerson` vfunc_69 @1405020b0:
  - Safe location: the aim-line point closest to the capsule centre, moved onto the capsule (`GetSquaredDistanceToCollision`, slot +0x628) and pushed in by `Feeler[0].Extent + CollisionPushOutDistance`. The Z clamp computed before that query is overwritten (dead).
  - Feeler rays: `RotateAngleAxis` by yaw about the ray frame's Z, then by pitch about its Y (positive pitch turns down).
  - FramesUntilNextTrace: reset to 0 on a hit; a skipped feeler contributes nothing.
  - Fraction: `(|Hit.Location - safe| - PushOut) / |target - safe|` for every feeler. WorldWeight / PawnWeight are not used at all: the decompile computes nothing from them, so the "weight 0 skips the hit" note above has no basis in this function either.
  - Blend: soft blend-in over 0.1 s, blend-out over 0.15 s, a hard snap, a first-frame snap (mode +0x48 bit 0), clamped to [0, 1], applied below 0.99999.
  - `GameSim::camera_world` sweeps the baseplate, the props and `building_hulls`.
- **G2 done.**
  - `PlacementEnv::props` takes the live prop collision (real convexes, or the placeholder trunk / rock frusta). `PlacementCollision` feeds them to `structural::world_overlap_check` as non-centroid blocking building actors.
  - `query` returns Obstructed when the sweep fails. Ground comes from the same sweep.
  - `PlacementEnv::static_meshes` is the `AStaticMeshActor` hook: obstructs above the base, is ground at or below it.
- **G3 hook.** `static_meshes` are also accepted `BuildingPlaceTrace` targets. POI building actors belong in the `BuildingWorld`.
- **G4 done.** In `building_hook.rs`, the edit target ray starts at the pawn projected onto the view ray and ends at camera + dir × 2048. vfunc_685 is only called from `FUN_14084f8a0`, the in-edit trace. Edit start goes through the interaction system (`FUN_140833550`, range check `FUN_140832610`), whose focus trace was not re-derived.
- **G5.**
  - Neighbour support was already fixed by the structural fixer (`structural::component_supported`).
  - Bounds: `PlacementEnv::grid_bounds` makes the query return InvalidActor outside them. The app sets only the certain lower edge, Z −3840.
  - The X / Y size and the height come from the world manager: the native defaults in ctor @14062ce50 are X_Size 10, Y_Size 10, Z 50, TileEdgeSize 25. `Athena_Terrain.umap` has no placed WorldManager, so a possible Athena override is unknown.

### PlayerBuildingSkillLevel in Athena

- **Hierarchy (SDK).** `AFortGameStateAthena : AFortGameStatePvP : AFortGameStateZone`.
- **No overrides.** Neither Athena nor PvP overrides slots 271, 272 or 280 (no `AFortGameStateAthena/PvP__vfunc_271/272/280` in SYMBOLS.tsv). Only Outpost overrides 271. So Athena runs Zone vfunc_271 @1405e0760.
- **Callers of slot 271 on a game state.** `UFortRegisteredPlayerInfo::ApplyHomebaseEffects` @1405fbab0 and `RemoveHomebaseEffects` @140617fa0 call it after an `IsA(AFortGameStateZone)` test, and Athena passes that test. These run from the player-info setup paths (`FUN_14060d200` / `FUN_140610b70`), with no game-mode gate.
  - Correction to building_structural_gaps.md §7: the slot +0x878 call in `AFortGameStateAthena::OnRep_GamePhase` @1404369d0 goes to `AFortPlayerPawnAthena` actors from an actor iterator, not to the game state.
- **So Athena does write +0x85C** on authority. The value is the max over the PCs' `MyPlayerInfo` ability actors of `FortConstructionSet.BuildingSkill` (vfunc_280 @1405c0e80, starting from −FLT_MAX). The write happens only when some PC qualifies and the value changed by more than 1e-8.
- **Where BuildingSkill comes from.**
  - Those attributes are initialised only by `UFortRegisteredPlayerInfo::InitializeAbilitySystemActor` @1405bc080, per the STW hero type's attribute category. Without a hero it logs "hero or herotype is not set".
  - The only BuildingSkill rows (`AttributesHeroNonScaling`) are 0, per damage.rs.
- **Result.** In BR the stored value is 0, the same as the ctor value, so `ATHENA_BUILDING_LEVEL = 0` stands. This is a static proof of the code path. The 0 rests on the data rows; there was no runtime observation.

### Fix 2 (after the second verifier pass)

- **Capsule radius 42.** `AFortPawn` ctor @1407c9ff0 writes CapsuleComponent (+0x3F8) +0x694 = 42.0 and +0x690 = 88. No pawn Blueprint serializes a radius; only `CapsuleHalfHeight` 75 is set (PlayerPawn_Athena_Generic, PlayerPawn_Generic).
  - Changed to 42: `fn_movement::params::CAPSULE_RADIUS`, `scene::PLAYER_RADIUS` (now derived from it), `fn_camera::CAPSULE_RADIUS`, `fn_game::pickups::PAWN_RADIUS`.
  - Crouched half height is max(40, 42) = 42, because UE4.16 `SetCapsuleSize` clamps it to the radius. The CMC already did this; the camera and hitreg now do too.
- **Pawn hide is strict.** vfunc_69 @1405020b0 hides the pawn only when `DistBlockedPct < HidePawnPenetrationPercent`.
- **Live capsule for the safe location.** The camera now uses the crouched capsule (centre feet + 42) when crouched. `CamState` carries `crouched` for `compose`.
- **Floor at spawn.** The flat plate now sits on the building level lattice: `BackdropData::build` floors the terrain height under the layout centre to a multiple of 384 (917 → 768). The mapping offset, terrain and backdrop meshes all follow from that value. The grid anchor formula is unchanged.
  - The harvestables already stand on the plate at y = 0; they never used their real UE Z. So they stay on the plate.
  - The terrain outside the plate and the ocean / far meshes rise 1.49 m relative to the plate. The terrain blends into the plate over `PLATE_BLEND_M`, so no seam appears.

---

## Fix status (main chat, 2026-10-06): placement flags, Target Info, preview highlight

- **G2 follow-up (per-actor flags).** The props' +0x564 bits now come from the cooked class chain (`props::build_flags_of`, `props.json`
  `class_chain` + merged CDO). Every `Plant_Shrub_*` (BuildingContainer), `Hedge_*`, `Tree_Small_1`, `Tree_Bush_*`, the small rocks, crates,
  furniture and trash cans have `bDestroyOnPlayerBuildingPlacement`: they no longer obstruct, and `ServerCreateBuildingActor` @140845d70 destroys
  them after a CanAdd placement (`placement::placement_destroys`, `building_hook::destroy_for_placement`: death particles + their `DeathSound`
  only, no resources). Big trees / rocks / poles / cars still obstruct.
- **Target Info on builds** (`fn-app/src/building_hud.rs`): `TargetedBuilding` (FUN_1408537d0) from the held tool's `DetermineReticleColor`
  (melee target in reach, ranged first trace actor within `GetRange`, the building tool's range 0 -> the preview marker), and
  `GetFocusedBuildingInfo` @142e267c0: player pieces `HealthDisplayRule` Always, interaction BeingModified while `bUnderConstruction`, Edit
  after, ConfirmEdit (hidden) while editing; the preview shows Build ("{MaxHealth} HP"); anchor = `CentroidOffset`; `MaxDistance` 1000 cm.
  The pickaxe reticle turns `ReticleBuildingColor` on pieces too.
- **Preview highlight** (`fn-render/src/outline_pass.rs`, `fn-matshader/src/outline.rs`, `fn-app/src/outline_hook.rs`): vfunc_672 @140849310:
  obstructing actors -> `HighlightColors[0]` Invalid, actors the placement destroys -> `HighlightColors[2]` Valid; custom depth with stencil
  `SelectionOutlineID_CodeReferenced` (2) and the cooked `SkinAndMeshOutlinePostProcess` program (BL_BeforeTranslucency) run unchanged.
- **Shader-map reader fix** (`fn-dxbc-cli/src/uniform.rs`): the compile description sits 56 + 16 n bytes after the uniform set, n = the
  material's parameter collection count (was tied to the cube count, which only matched the scope material).

---

## Audit 2026-10-24 (build / edit feel, browser build)

Sources: the 1.8 exe decompile (`D:\Fortnite\FortniteRef\decomp\named`), the exe's vtables and static FName initialisers, and the cooked
`/Game/Characters/Feedback/PlayerUIFeedback` (`fn-inspect props`).

- **Controller vtable check.** `FortPlayerControllerAthena` (ctor @140413a60, vtable 0x144070898) keeps the base slots 672 / 677 / 685 / 687 @140816bd0 /
  688 / 689 @140816430; slot 679 = @142286760 `return 0`. It overrides only 684 / 686, which first pop its own input component (slot 660 @140437540).
  The other vtables that override 671 / 674 / 675 / 679 / 687 / 689 are not Athena's (Outpost = @14085ca30 -> 0x1442495f0, Zone / PvP = 0x14423e630 / 0x14423ff40).
- **A1 (fixed). The occupant of the preview's slot is the focused building.** FUN_140835750, query 1 (ExistingActor) with a player-buildable occupant
  (or the same building type): FUN_140849930 writes the occupant into the controller's focus (+0x1790), the marker is hidden (vfunc 0x2a8).
  `TargetedBuilding` (FUN_1408537d0) takes +0x1790 first, and the edit start FUN_1408316c0 edits only +0x1790. The port showed the preview's
  "{MaxHealth} HP / BUILD" over a placed piece, and started the edit on the camera-ray piece.
  Now: `building_hook::preview_focus` is the occupant. Target Info shows it (health, Edit / BeingModified), and G edits it.
- **A2 (fixed). Build-mode UI feedback sounds were missing.** `FUN_140850520(PC, FName)` plays the `PlayerUIFeedback` entry of that name.
  FName globals, from the static initialiser at @1400aa566..@1400aa646:

  | FName global | Entry | Played by | Cue |
  |---|---|---|---|
  | DAT_1466290f0 | BuildPreviewChange | SetBuildableClass @140835390 (a new non-null class) | `Fort_Build_BluePrint_Select_Cue` |
  | DAT_1466290f8 | BuildPreviewGridSnap | @140835750, when `LastBuildPreviewGridSnapLoc/Rot` change | `Fort_Build_BluePrint_Select_Cue` |
  | DAT_146629120 | BuildPreviewRotate | @1408352a0 | `Fort_Build_Rotate_Cue` |
  | DAT_1466290e0 | BuildPreviewUnableToPlace | Fire @1408336f0, query != 0 | `UI_CantBuild_Cue` |
  | DAT_1466290e8 | BuildPreviewUnableToAfford | Fire @1408336f0, vfunc_677 false | `UI_NotEnoughResources_Cue` |
  | DAT_146629100 / 108 / 110 | BuildSelectWood / Stone / Metal | @140834fc0, resource changed | `Fort_Build_SelectWood_Cue` / `Fort_Build_SelectRock_Cue` / `Fort_Build_SelectMetal_Cue` |

  The query != 0 text (slot 692 @140827c30) is empty for results 1-3, so only the sound plays.
- **A3 (fixed, pass 2). The occupant is outlined.** In the same branch, vfunc_672 is called with [occupant], mode 0 and Friendly = (team attitude == 0),
  which FUN_140588150 maps to HighlightColors +0x1d8. The port's outline pass draws custom depth for real props only (statics renderer), not for
  FX-pass building pieces. Pass 2: `outline_hook` registers the occupant class's StaticMesh (LOD 0, opaque two-sided, so masked gaps between
  planks are filled in the custom depth) in the statics renderer and draws it into the custom depth / stencil with HighlightColors[0] Valid.
- **A4 (unverified). Combat-mode edit focus.** Outside build mode, +0x1790 comes from the interaction scorer FUN_140840d70
  ("ReticleAimingAtInteractionActorTrace"), fed by FUN_14081f700 ("FindInteractActorsLineTrace" / "FindInteractActorsSphereTrace", channel 14).
  It scores every interactable (pickups, containers, doors, buildings) in one pass; the port has separate per-hook focuses, so porting it means one
  shared interaction-focus system replacing those. Not done; the port still uses the EditModeTrace ray.
- **A5 (removed, pass 2).** `Game.TurboBuild`, `ConfirmEditOnRelease` and `ResetEditOnRelease` (`fn_settings::extras`) have no string in the 1.8 exe.
  They were settings rows (extension rows of the Game tab); rows, settings keys and code paths are removed. Old settings files that still hold the
  keys load unchanged (kept as unknown extras).
- **A6 (fixed, pass 2). F2 was taken by the port's console.** In ConfigD_Athena, F2 = HotbarKeyF2 = the Floor slot. `console_hook::keys_from_winit` maps F2 to
  ToggleConsole first, so the default Floor key opened the command bar. The console key is now Tilde (Backquote), the cooked
  `InputSettings:ConsoleKeys:0=Tilde` (CookedIniVersion.txt line 1980); F2 reaches the game as HotbarKeyF2.
