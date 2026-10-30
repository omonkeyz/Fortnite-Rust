//! Cooked 1.8 data behind editing: the player building classes (`PossibleEditClasses` source), their edit patterns
//! (`EditModePatternData` -> `/Game/Building/EditModePatterns/**/EMP_*`), the tile grids and the edit-tool assets.
//!
//! Sources (exe = FortniteClient-Win64-Shipping.exe 1.8, names from `C:\FortniteRef\decomp\named`):
//! * Class set: `AFortGameStateZone::vfunc_294` @1405c0750 registers every bIsPlayerBuildable `ABuildingSMActor` class
//!   returned by `FUN_140556fb0` (asset-manager list filtered on the `bIsPlayerBuildable` registry tag, `FUN_14055f860`)
//!   into `PlayerBuildableClasses[ResourceType]` (only when `ResourceType` (+0x77c) < 4).
//!   `UBuildingEditModeSupport::vfunc_83` @1404b71b0 then asks `FUN_1405d5160` for the classes of the edited actor's
//!   ResourceType whose `BuildingType` (+0x3f0) and building level (actor vfunc 0x610) equal the edited actor's.
//!   Native `ResourceType` default is 4 = None (`ABuildingSMActor::ctor` @14045b960 `+0x77c = 4`), so the
//!   `BuildingGuides/PBWA_BG_*` classes (no cooked ResourceType) are never candidates.
//! * BuildingType per native parent: `ABuildingWall::ctor` @14049fe20 (+0x3f0 = 0), `ABuildingFloor` @140459d10 (1),
//!   `ABuildingStairs` @14049f370 (5), `ABuildingRoof` @14045b660 (6).
//! * Level: CDO `CurrentBuildingLevel` (SDK ABuildingActor +0x498; 0 when not cooked).
//! * Order: the exe keeps the asset-manager order, which the cooked data does not record; candidates are kept in pak
//!   path order. No two candidates of one (type, material, level) share a pattern (asserted by the data test), so the
//!   order cannot change which class a selection resolves to.

use std::collections::HashMap;

use glam::Vec3;

use crate::edit::pattern::{Pattern, Tiles, BTV_ACTIVE, BTV_EITHER, BTV_INACTIVE};
use crate::pieces::{Material, PieceKind};

/// Where player building BPs live in 1.8 (`Wood|Stone|Metal/L1..L3`, `BuildingGuides`, `Master`).
pub const PLAYER_BP_GLOB: &str = "FortniteGame/Content/Building/ActorBlueprints/Player/**";

/// `UFortGameData::EditModeCancelDistance` (+0x4c0). Not cooked in `/Game/Balance/DefaultGameData` nor
/// `AthenaGameData`, so the C++ default applies: `UFortGameData::ctor` @1405751d0 `*(param_1 + 0x98) = 0x443b8000`
/// (= 750.0). Used by `FUN_140832610` (pawn to building centroid, `<=`) at edit start and every PlayerTick
/// (`FUN_14084f8a0`, cancels the edit when out of range).
pub const EDIT_MODE_CANCEL_DISTANCE: f32 = 750.0;

/// `AFortPlayerController::EditModeDistance` (+0x1a50): `AFortPlayerController::ctor` @140810140 writes 0x45000000
/// (2048.0); `AFortPlayerController::vfunc_685` @140827a80 ends the edit trace at camera + dir * EditModeDistance.
pub const EDIT_MODE_TRACE_DISTANCE: f32 = 2048.0;

/// `UFortGameData::InvalidEditPatternColor` (+0x1c8), not cooked; ctor @1405751d0: `param_1[0x39] = 0x3e800000`
/// (R 0.25, G 0), `+0x1d0 = 0` (B), `+0x1d4 = 1.0` (A).
pub const INVALID_EDIT_PATTERN_COLOR: [f32; 4] = [0.25, 0.0, 0.0, 1.0];

/// Edit-tool asset paths (static soft pointers built in the support constructors, strings in `FUN_1404a1d90`,
/// `FUN_1404a1e70`, `FUN_1404a2140`, `FUN_1404a2740`, `FUN_1404a2220`).
pub mod assets {
    /// `EditTileMesh` (+0x60), `UBuildingEditModeSupport_Wall::ctor` @1404a3a10 via `FUN_1404a2740`.
    pub const TILE_MESH_WALL: &str = "/Game/Packages/DS_Fortnite_Mesh/S_BlueprintSlab_Wall";
    /// `EditTileMesh`, Floor @1404a3360 and Roof @1404a34b0 via `FUN_1404a2140`.
    pub const TILE_MESH_FLOOR: &str = "/Game/Packages/DS_Fortnite_Mesh/S_BlueprintSlab_Floor";
    /// `EditTileMesh`, Stair @1404a3630 via `FUN_1404a2220`.
    pub const TILE_MESH_STAIR: &str = "/Game/Packages/DS_Fortnite_Mesh/S_Blueprint_Stairs_Corner";
    /// Stair `AuxiliaryIndicatorMesh` (+0x2a0).
    pub const STAIR_AUX_MESH: &str = "/Game/Packages/DS_Fortnite_Mesh/S_Blueprint_Stairs_Center_New";
    /// Stair `AuxIndicatorComponentParentMaterial` (+0x2d8).
    pub const STAIR_AUX_MATERIAL: &str = "/Game/Packages/DS_Fortnite_Mesh/M_Blueprint_StairsBaseShader";
    /// Stair `AuxIndicatorArrowTexture` (+0x2f8) / `AuxIndicatorGridTexture` (+0x318).
    pub const STAIR_AUX_ARROW_TEXTURE: &str = "/Game/Packages/DS_Fortnite_Mesh/GroundArrowHighlight";
    pub const STAIR_AUX_GRID_TEXTURE: &str = "/Game/Packages/DS_Fortnite_Mesh/GroundArrowPatternGrid";
    /// `TileMaterialParent` (+0x90): BinaryToggle ctor @1404a31e0 (`FUN_1404a1e70`) and Stair ctor.
    pub const TILE_MATERIAL: &str = "/Game/Packages/Fortnite_UniqueMaterials_1/Effects/MGreen";
    /// `ActiveTileTexture` (+0xb0) / `InactiveTileTexture` (+0xd0), BinaryToggle ctor.
    pub const ACTIVE_TILE_TEXTURE: &str = "/Game/Packages/DS_Fortnite_Mesh/BlueprintTest1";
    pub const INACTIVE_TILE_TEXTURE: &str = "/Game/Packages/DS_Fortnite_Mesh/BlueprintTestX";
    /// `PreviewMaterial` (+0x108) put on the preview of the matched class.
    pub const PREVIEW_MATERIAL: &str = "/Game/Packages/Fortnite_UniqueMaterials_1/Effects/MPreviewPulse";
    /// `SuccessfulPatternMatchCurve` (+0x138), base ctor @1404a2eb0 (`FUN_1404a1d90`): drives the tiles' interact
    /// flash (`OnSuccessfulMatchInteractParamUpdate` @1404bd9a0).
    pub const MATCH_FLASH_CURVE: &str = "/Game/GameplayCurves/ValidBluePrintInteractValueFlash";
    /// `FortGameData::EditToolItem` (cooked in AthenaGameData and DefaultGameData).
    pub const EDIT_TOOL_ITEM: &str = "/Game/Items/Weapons/BuildingTools/EditTool";
    /// `FortGameData::UIFeedbackBank` (DefaultGameData) holding the edit sounds.
    pub const UI_FEEDBACK_BANK: &str = "/Game/Characters/Feedback/PlayerUIFeedback";
}

/// Tile-material parameters. Names come from the cooked `MGreen` material's parameter expressions; which exe FName
/// global feeds which parameter is matched by use (the FName static initialisers are not in the decompile):
/// * texture `Diffuse` <- `DAT_145220090` (`BinaryToggle::vfunc_98` @1404c45a0, Active/Inactive tile texture)
/// * scalar `isX` <- `DAT_145220098` (same function: 0 active, 1 inactive; stair `vfunc_99`: 0 on path, 1 off)
/// * scalar `InteractParameter` <- `DAT_1452200a0` (hover 1/0 in `vfunc_96` @1404c4aa0, match flash curve)
/// * vector `BlueColor` <- `DAT_1452200a8` (`BinaryToggle::vfunc_99` @1404c4690: valid/invalid colour)
pub mod material_params {
    pub const TEXTURE: &str = "Diffuse";
    pub const IS_X: &str = "isX";
    pub const INTERACT: &str = "InteractParameter";
    pub const COLOR: &str = "BlueColor";
    /// Stair indicator scalars, `UBuildingEditModeSupport_Stair::vfunc_103` @1404c3da0 writes them in this order from its 3 float
    /// arguments (FName globals DAT_1452200c8 / DAT_1452200d0 / DAT_1452200d8; names resolved by the verifier from the cooked
    /// `M_Blueprint_StairsBaseShader` parameters, checked by `real_18_edit_data`). Values per indicator mode: [`super::stair::aux_scalars`].
    pub const AUX_SCALARS: [&str; 3] = ["sineOffset", "isblinkingSign", "isX"];
}

/// `UBuildingEditModeSupport_Roof` ctor @1404a34b0: `+0x2c4` = (0, 0, DAT_1440b289c = 32.0) is added to the target position of
/// every roof tile whose value is `BTV_Active` (0) (`Roof::vfunc_84` @1404b8a70 at spawn, `Roof::vfunc_97` @1404c4480 on change).
pub const ROOF_ACTIVE_TILE_RAISE: Vec3 = Vec3::new(0.0, 0.0, 32.0);
/// Roof ctor `+0x2c0` = 0x40800000 (4.0): `Roof::vfunc_76` @1404bdc90 eases each tile's RelativeLocation to its target with
/// `FMath::VInterpTo(current, target, dt, 4)` (`FUN_1412f3930`) every tick.
pub const ROOF_TILE_INTERP_SPEED: f32 = 4.0;

/// `FMath::VInterpTo` as `FUN_1412f3930` @1412f3930: speed <= 0 or `|target - current|^2 < 1e-4` (DAT_143ff9010) snaps to the
/// target, else `current + (target - current) * clamp(dt * speed, 0, 1)`.
pub fn vinterp_to(current: Vec3, target: Vec3, dt: f32, speed: f32) -> Vec3 {
    if speed <= 0.0 { return target; }
    let d = target - current;
    if d.length_squared() < 1.0e-4 { return target; }
    current + d * (dt * speed).clamp(0.0, 1.0)
}

/// `SuccessfulPatternMatchCurve` (`/Game/GameplayCurves/ValidBluePrintInteractValueFlash`, a `CurveFloat`) as the support's
/// `FTimeline` (+0x160) plays it: `vfunc_88` @1404b8cb0 sets the length mode to `TL_LastKeyFrame` (`FUN_1424e0e60(tl, 1)`) and binds
/// `OnSuccessfulMatchInteractParamUpdate` @1404bd9a0 (writes `InteractParameter` = value on every tile except the hovered one);
/// `vfunc_90` @1404c5bb0 restarts it (`FUN_1424da730` = `PlayFromStart`); `vfunc_76` ticks it (`FUN_1424e3730`). The finished
/// event `OnSuccessfulMatchInteractComplete` is the shared empty native stub (LAB_14263e320), so the last value stays.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlashCurve {
    /// (time, value, interp mode (0 linear, 1 constant, 2 cubic), arrive tangent, leave tangent), sorted by time.
    pub keys: Vec<(f32, f32, u8, f32, f32)>,
}

impl FlashCurve {
    /// `TL_LastKeyFrame` length.
    pub fn length(&self) -> f32 { self.keys.last().map_or(0.0, |k| k.0) }

    /// UE4.16 `FRichCurve::Eval`: constant before the first / after the last key; per segment the left key's interp mode
    /// (cubic = Bezier with the tangents scaled by the segment length / 3, the same curve as a Hermite).
    pub fn eval(&self, t: f32) -> f32 {
        let k = &self.keys;
        let (Some(first), Some(last)) = (k.first(), k.last()) else { return 0.0 };
        if t <= first.0 { return first.1; }
        if t >= last.0 { return last.1; }
        let i = k.partition_point(|x| x.0 <= t).saturating_sub(1);
        let (a, b) = (k[i], k[(i + 1).min(k.len() - 1)]);
        let dt = b.0 - a.0;
        if dt <= 0.0 { return a.1; }
        let u = (t - a.0) / dt;
        match a.2 {
            1 => a.1,
            2 => {
                let (p0, p3) = (a.1, b.1);
                let (p1, p2) = (p0 + a.4 * dt / 3.0, p3 - b.3 * dt / 3.0);
                let v = 1.0 - u;
                v * v * v * p0 + 3.0 * v * v * u * p1 + 3.0 * v * u * u * p2 + u * u * u * p3
            }
            _ => a.1 + (b.1 - a.1) * u,
        }
    }

    pub fn load(set: &fn_pak::PakSet) -> Result<FlashCurve, String> {
        let path = assets::MATCH_FLASH_CURVE;
        let data = fn_uasset::PackageData::load(set, path).map_err(|e| format!("{path}: {e}"))?;
        let pkg = data.package().map_err(|e| format!("{path}: {e}"))?;
        let i = pkg.exports_of_class("CurveFloat").next().ok_or_else(|| format!("{path}: no CurveFloat export"))?;
        let obj = pkg.read_export(i).map_err(|e| format!("{path}: {e}"))?;
        let keys = obj.get("FloatCurve").and_then(|c| c.field("Keys")).and_then(|v| v.as_array()).ok_or_else(|| format!("{path}: no FloatCurve.Keys"))?;
        let interp = |v: Option<&fn_uasset::Value>| -> u8 {
            match v {
                Some(fn_uasset::Value::Byte(b)) => *b,
                Some(fn_uasset::Value::Enum { value, .. }) | Some(fn_uasset::Value::Name(value)) => match value.rsplit("::").next().unwrap_or(value) {
                    "RCIM_Constant" => 1,
                    "RCIM_Cubic" => 2,
                    "RCIM_None" => 3,
                    _ => 0,
                },
                Some(v) => v.as_i64().unwrap_or(0) as u8,
                None => 0, // RichCurveKey default InterpMode = RCIM_Linear
            }
        };
        let mut out: Vec<(f32, f32, u8, f32, f32)> = keys.iter().filter_map(|k| Some((
            k.field("Time")?.as_f32()?, k.field("Value")?.as_f32()?, interp(k.field("InterpMode")),
            k.field("ArriveTangent").and_then(|v| v.as_f32()).unwrap_or(0.0), k.field("LeaveTangent").and_then(|v| v.as_f32()).unwrap_or(0.0),
        ))).collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(FlashCurve { keys: out })
    }
}

/// Build numbers of an edit class, what `ABuildingSMActor::vfunc_247` @14047bed0 reads from the NEW actor of an edit
/// (`ReplaceBuildingActor` -> InitReason 2): MaxHealth (vfunc 0x728), `FUN_14046f440` = MaxHealth * InitialHealthPct, BuildTime
/// (`vfunc_371` @14046c580: `FortBuildingActorSet.BuildTime`), and `vfunc_370` (`AnimatingMaterial != null`, +0xc40).
/// Same curve rows as `crate::pieces::PieceCatalog::load` (the class's `AttributeInitKey`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClassBuild {
    pub max_health: f32,
    pub initial_health_pct: f32,
    pub build_time: f32,
    pub animates: bool,
}

/// `MGreen` default of `BlueColor` (cooked material expression default), the valid-pattern tile colour
/// (`DefaultBlueTileColor` +0x29c is not written by any constructor; the material default is what the tile shows).
pub const DEFAULT_BLUE_TILE_COLOR: [f32; 4] = [0.012932429, 0.089637518, 0.28999999, 1.0];

/// One player-buildable class that can be an edit target.
#[derive(Clone, Debug)]
pub struct EditClass {
    /// Generated class object path (`/Game/.../PBWA_W1_Windows.PBWA_W1_Windows_C`).
    pub class_path: String,
    /// Package (`/Game/.../PBWA_W1_Windows`).
    pub package: String,
    pub kind: PieceKind,
    pub material: Material,
    /// `CurrentBuildingLevel` of the CDO.
    pub level: i32,
    /// `EditModePatternData` package (`/Game/Building/EditModePatterns/Wall/EMP_Wall_Windows`), if any.
    pub pattern: Option<String>,
    /// CDO `StaticMesh` (the preview mesh, `ABuildingSMActor +0x760`).
    pub static_mesh: Option<String>,
    /// Health / build numbers of a new actor of this class (edits rebuild it, see [`ClassBuild`]); `None` when a curve row is missing.
    pub build: Option<ClassBuild>,
}

/// Placement of the N x N edit tiles in the edited actor's local space (`UBuildingEditModeSupport::vfunc_89` @1404a90e0):
/// `pos(row, col) = col * axis_a * size_a + row * axis_b * size_b + origin` (+ the stair corner offset). Tile index
/// = row * N + col (`vfunc_84` @1404b8670 spawns rows outer, columns inner).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileLayout {
    pub n: usize,
    /// `+0x244`
    pub axis_a: Vec3,
    /// `+0x250`
    pub axis_b: Vec3,
    /// `+0x25c`
    pub origin: Vec3,
    /// Component relative scale `+0x268` (0.95 base ctor 0x3f733333; 1.0 stairs).
    pub scale: Vec3,
    /// `+0x274` / `+0x278` = `|2 * |axis * mesh bounds extent||` (`vfunc_82` @1404a92d0).
    pub size_a: f32,
    pub size_b: f32,
    /// Stair only (`UBuildingEditModeSupport_Stair::vfunc_89` @1404a91f0): `+= axis * 118.5` (DAT_1440b28ac) on the
    /// last column / last row.
    pub last_offset: f32,
    /// Tile mesh bounds (origin, extent) in mesh space, for ray picking.
    pub mesh_origin: Vec3,
    pub mesh_extent: Vec3,
}

impl TileLayout {
    pub fn tile_position(&self, index: usize) -> Vec3 {
        let row = (index / self.n) as f32;
        let col = (index % self.n) as f32;
        let mut p = self.axis_a * (col * self.size_a) + self.axis_b * (row * self.size_b) + self.origin;
        if self.last_offset != 0. {
            if index % self.n == self.n - 1 { p += self.axis_a * self.last_offset; }
            if index / self.n == self.n - 1 { p += self.axis_b * self.last_offset; }
        }
        p
    }

    /// The raw constructor constants of each support class (the bounds-derived sizes are filled by the loader).
    /// Wall @1404a3a10: axis_a (1,0,0), axis_b (0,0,-1), origin (-170.75, 0, 314.8801) (DAT_1440b28d8, DAT_1440b28b4).
    /// Floor @1404a3360 / Roof @1404a34b0: axis_a (1,0,0), axis_b (0,1,0), origin (-122, 122, 16)
    /// (DAT_1440b28cc, DAT_1440b28b0, DAT_144040c9c). Stair @1404a3630: axes as floor, origin (-144, 112, 16)
    /// (DAT_1440b28d0, DAT_1440b28a8), scale 1.
    pub fn constants(kind: PieceKind) -> TileLayout {
        let base = TileLayout {
            n: 2, axis_a: Vec3::X, axis_b: Vec3::Y, origin: Vec3::new(-122.0, 122.0, 16.0), scale: Vec3::splat(0.95),
            size_a: 0., size_b: 0., last_offset: 0., mesh_origin: Vec3::ZERO, mesh_extent: Vec3::ZERO,
        };
        match kind {
            PieceKind::Wall => TileLayout { n: 3, axis_b: Vec3::new(0., 0., -1.), origin: Vec3::new(-170.75, 0.0, 314.8801), ..base },
            PieceKind::Floor | PieceKind::Roof => base,
            PieceKind::Stairs => TileLayout { origin: Vec3::new(-144.0, 112.0, 16.0), scale: Vec3::ONE, last_offset: 118.5, ..base },
        }
    }

    pub fn tile_mesh(kind: PieceKind) -> &'static str {
        match kind {
            PieceKind::Wall => assets::TILE_MESH_WALL,
            PieceKind::Floor | PieceKind::Roof => assets::TILE_MESH_FLOOR,
            PieceKind::Stairs => assets::TILE_MESH_STAIR,
        }
    }
}

/// Stair auxiliary indicators (the arrows between two tiles). Exe tables, read from the shipped image:
/// * `DAT_1451804e0`: tile pair per indicator: (0,1) (1,3) (2,3) (0,2).
/// * `DAT_145180750`: relative locations (0,112,16) (144,256,16) (0,400,16) (-144,256,16); relative rotation yaw = 90 * i
///   (`DAT_145180500` = rotator (0, 90, 0) scaled by i, `Stair::vfunc_97` @1404b6050); +180 yaw when the arrow's
///   direction flag is set (`vfunc_103` @1404c3da0, DAT_143ff9038; the flag is `ADJACENCY_REVERSED`, not the highlight).
/// * `DAT_1440994c0`: `[from][to]` -> indicator joining the two tiles or -1 (`vfunc_104` @1404a9a00 adjacency test).
/// * Reset pattern (`Stair::vfunc_75` @1404bbe20): tiles DAT_1451804f0/4f4 = (2, 3) get 1, pair 0 = (0, 1) gets 2.
/// * Max path length `+0x33c = 4` (Stair ctor).
pub mod stair {
    pub const AUX_PAIRS: [(usize, usize); 4] = [(0, 1), (1, 3), (2, 3), (0, 2)];
    pub const AUX_LOCATIONS: [[f32; 3]; 4] = [[0., 112., 16.], [144., 256., 16.], [0., 400., 16.], [-144., 256., 16.]];
    pub const AUX_YAW_STEP: f32 = 90.0;
    /// `vfunc_103` adds DAT_143ff9038 (180) to the indicator yaw when its direction flag is set (the arrow points the other way;
    /// the flag comes from [`ADJACENCY_REVERSED`], see `EditSession::aux_visuals`). Not a highlight.
    pub const AUX_HIGHLIGHT_YAW: f32 = 180.0;
    /// First int of each 8-byte `DAT_1440994c0` entry, index `[from * 4 + to]`.
    pub const ADJACENCY: [[i32; 4]; 4] = [[-1, 0, 3, -1], [0, -1, -1, 1], [3, -1, -1, 2], [-1, 1, 2, -1]];
    /// Byte 4 of each `DAT_1440994c0` entry (`DAT_1440994c4`, read from the shipped image): the arrow direction flag that
    /// `Stair::vfunc_102` @1404c8d60 passes to `vfunc_103` for path (1) / hint (0) indicators.
    pub const ADJACENCY_REVERSED: [[bool; 4]; 4] = [[false, false, true, false], [true, false, false, false], [false, false, false, true], [false, true, false, false]];
    /// Byte 5 (`DAT_1440994c5`): when set and the edited actor is `bMirrored` (+0x780 bit 1), `vfunc_102` inverts the direction flag.
    pub const ADJACENCY_MIRROR_FLIPS: [[bool; 4]; 4] = [[false, true, false, false], [true, false, false, false], [false, false, false, true], [false, false, true, false]];
    /// The three scalars `vfunc_102` passes to `vfunc_103` per indicator mode (0 hint arrow, 1 path arrow, 2 idle grid,
    /// 3 active grid), in [`super::material_params::AUX_SCALARS`] order: mode 0 (0, 1, 0), 1 (1, 0, 0), 2 (0, 0, 1), 3 (0, 0, 0).
    pub fn aux_scalars(mode: u8) -> [f32; 3] {
        match mode { 0 => [0., 1., 0.], 1 => [1., 0., 0.], 2 => [0., 0., 1.], _ => [0., 0., 0.] }
    }
    pub const RESET_ONE: [usize; 2] = [2, 3];
    pub const MAX_STEPS: i32 = 4;
}

/// Edit sounds: `UBuildingEditModeSupport` calls `FUN_140850520(Controller, FName)` which plays the `FortUIFeedback` of
/// that name from `UIFeedbackBank` (`/Game/Characters/Feedback/PlayerUIFeedback`). Begin (`vfunc_71` @1404bc2a0,
/// DAT_1452200b0), end (`vfunc_72` @1404bc470, DAT_1452200b8) and tile change (`vfunc_80` @1404bc220, DAT_1452200c0);
/// the bank has exactly three Edit entries, mapped to those call sites by meaning.
pub mod sounds {
    pub const ENTER: (&str, &str) = ("BuildPreviewEditEnter", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_BluePrint_G_To_Edit_Cue");
    pub const EXIT: (&str, &str) = ("BuildPreviewEditExit", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_BluePrint_CancelEdit_Cue");
    pub const TILE: (&str, &str) = ("BuildPreviewEditTile", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_BluePrint_Edit_TurnOnTile_Cue");
    pub const CHANGE: (&str, &str) = ("BuildPreviewChange", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_BluePrint_Select_Cue");
    pub const GRID_SNAP: (&str, &str) = ("BuildPreviewGridSnap", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_BluePrint_Select_Cue");
    pub const ROTATE: (&str, &str) = ("BuildPreviewRotate", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_Rotate_Cue");
    pub const UNABLE_TO_PLACE: (&str, &str) = ("BuildPreviewUnableToPlace", "/Game/Sounds/Fort_UI_Sounds/Gameplay/UI_CantBuild_Cue");
    pub const UNABLE_TO_AFFORD: (&str, &str) = ("BuildPreviewUnableToAfford", "/Game/Sounds/Fort_UI_Sounds/Gameplay/UI_NotEnoughResources_Cue");
    pub const SELECT: [(&str, &str); 3] = [
        ("BuildSelectWood", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_SelectWood_Cue"),
        ("BuildSelectStone", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_SelectRock_Cue"),
        ("BuildSelectMetal", "/Game/Sounds/Fort_UI_Sounds/Gameplay/Fort_Build_SelectMetal_Cue"),
    ];
}

/// Everything editing needs from the paks.
#[derive(Clone, Debug, Default)]
pub struct EditDb {
    /// All player-buildable classes with ResourceType < 4, in pak path order.
    pub classes: Vec<EditClass>,
    /// Pattern package -> pattern.
    pub patterns: HashMap<String, Pattern>,
    pub layouts: HashMap<PieceKind, TileLayout>,
    /// `S_Blueprint_Stairs_Center_New` bounds (origin, extent), for picking the stair indicators.
    pub stair_aux_bounds: (Vec3, Vec3),
    /// `SuccessfulPatternMatchCurve` (the tiles' match flash).
    pub match_flash: FlashCurve,
    by_class: HashMap<String, usize>,
}

impl EditDb {
    pub fn from_parts(classes: Vec<EditClass>, patterns: HashMap<String, Pattern>, layouts: HashMap<PieceKind, TileLayout>) -> EditDb {
        let mut by_class = HashMap::new();
        for (i, c) in classes.iter().enumerate() {
            by_class.insert(c.class_path.clone(), i);
            by_class.insert(c.package.clone(), i);
        }
        EditDb { classes, patterns, layouts, stair_aux_bounds: (Vec3::ZERO, Vec3::ZERO), match_flash: FlashCurve::default(), by_class }
    }

    /// Lookup by class path (`.._C`) or package path.
    pub fn class(&self, path: &str) -> Option<&EditClass> {
        self.by_class.get(path).map(|&i| &self.classes[i]).or_else(|| {
            let pkg = path.split('.').next().unwrap_or(path);
            self.by_class.get(pkg).map(|&i| &self.classes[i])
        })
    }

    pub fn pattern_of(&self, class: &EditClass) -> Option<&Pattern> { class.pattern.as_deref().and_then(|p| self.patterns.get(p)) }

    /// `PossibleEditClasses` for an actor of this class (`FUN_1405d5160` with key {ResourceType, BuildingType, level}).
    pub fn possible_edit_classes(&self, of: &EditClass) -> Vec<&EditClass> {
        self.classes.iter().filter(|c| c.material == of.material && c.kind == of.kind && c.level == of.level).collect()
    }

    pub fn layout(&self, kind: PieceKind) -> TileLayout {
        self.layouts.get(&kind).copied().unwrap_or_else(|| TileLayout::constants(kind))
    }

    /// Loads from an open pak set (1.8 install).
    pub fn load(set: &fn_pak::PakSet) -> Result<EditDb, String> {
        let mut classes = Vec::new();
        let mut patterns: HashMap<String, Pattern> = HashMap::new();
        let mut entries: Vec<String> = set
            .glob(PLAYER_BP_GLOB)
            .into_iter()
            .map(|(_, e)| e.rel_path().to_string())
            .filter(|k| k.to_ascii_lowercase().ends_with(".uasset"))
            .collect();
        entries.sort();
        // Build numbers of every class (edits rebuild the new actor, ABuildingSMActor::vfunc_247 @14047bed0): the same
        // AttributesBuildingSection rows and InitialHealthPct handles as PieceCatalog::load, at the same curve level.
        let attr = crate::pieces::CurveTable::load(set, "/Game/Balance/DataTables/AttributesBuildingSection").ok();
        let pct: Option<[f32; 3]> = crate::pieces::initial_health_pct_handles(set).ok().and_then(|h| {
            let mut out = [0f32; 3];
            for (i, r) in h.iter().enumerate() { out[i] = r.eval(set, crate::pieces::ATHENA_BUILDING_LEVEL).ok()?; }
            Some(out)
        });
        for key in entries {
            let Some(pkg_path) = fn_pak::pak_path_to_game(&key) else { continue };
            let Ok(info) = read_class_info(set, &pkg_path) else { continue };
            if !info.player_buildable { continue; }
            let Some(kind) = info.kind else { continue };
            // ResourceType >= 4 (None, the native default) is never registered (AFortGameStateZone::vfunc_294).
            let Some(material) = info.material else { continue };
            if let Some(p) = &info.pattern {
                if !patterns.contains_key(p) {
                    patterns.insert(p.clone(), read_pattern(set, p)?);
                }
            }
            let build = match (&attr, pct) {
                (Some(attr), Some(pct)) => {
                    let lvl = crate::pieces::ATHENA_BUILDING_LEVEL;
                    let h = attr.eval(&format!("{}.{}.FortHealthSet.MaxHealth", info.attr_category, info.attr_subcategory), lvl).ok();
                    let t = attr.eval(&format!("{}.FortBuildingActorSet.BuildTime", info.attr_category), lvl).ok();
                    match (h, t) {
                        (Some(max_health), Some(build_time)) => Some(ClassBuild { max_health, initial_health_pct: pct[material.index()], build_time, animates: info.animating_material }),
                        _ => None,
                    }
                }
                _ => None,
            };
            classes.push(EditClass {
                class_path: info.class_path, package: pkg_path, kind, material, level: info.level, pattern: info.pattern,
                static_mesh: info.static_mesh, build,
            });
        }
        let mut layouts = HashMap::new();
        for kind in PieceKind::ALL {
            let mut l = TileLayout::constants(kind);
            let (o, e) = mesh_bounds(set, TileLayout::tile_mesh(kind))?;
            l.mesh_origin = o;
            l.mesh_extent = e;
            // vfunc_82: |2 * |(axis * extent)||
            l.size_a = 2.0 * (l.axis_a * e).length();
            l.size_b = 2.0 * (l.axis_b * e).length();
            layouts.insert(kind, l);
        }
        let mut db = EditDb::from_parts(classes, patterns, layouts);
        db.stair_aux_bounds = mesh_bounds(set, assets::STAIR_AUX_MESH)?;
        db.match_flash = FlashCurve::load(set)?;
        Ok(db)
    }
}

struct ClassInfo {
    class_path: String,
    kind: Option<PieceKind>,
    material: Option<Material>,
    level: i32,
    pattern: Option<String>,
    static_mesh: Option<String>,
    player_buildable: bool,
    /// `AttributeInitKey.AttributeInitCategory` / `AttributeInitSubCategory`.
    attr_category: String,
    attr_subcategory: String,
    /// CDO `AnimatingMaterial` set (+0xc40, `vfunc_370`).
    animating_material: bool,
}

/// Reads a BP class: native building type from the super chain and the CDO values (child BP overrides parent BP).
fn read_class_info(set: &fn_pak::PakSet, pkg_path: &str) -> Result<ClassInfo, String> {
    let mut info = ClassInfo {
        class_path: String::new(), kind: None, material: None, level: 0, pattern: None, static_mesh: None, player_buildable: false,
        attr_category: String::new(), attr_subcategory: String::new(), animating_material: false,
    };
    let mut chain: Vec<String> = vec![pkg_path.to_string()];
    let mut seen = (false, false, false, false, false);
    let (mut seen_attr, mut seen_anim) = (false, false);
    for depth in 0..8 {
        let cur = chain.last().unwrap().clone();
        let data = fn_uasset::PackageData::load(set, &cur).map_err(|e| format!("{cur}: {e}"))?;
        let pkg = data.package().map_err(|e| format!("{cur}: {e}"))?;
        if depth == 0 {
            let ce = pkg.class_export().ok_or_else(|| format!("{cur}: no class export"))?;
            let name = &pkg.exports[ce].name;
            info.class_path = format!("{cur}.{name}");
        }
        if let Some(Ok(cdo)) = pkg.read_default_object() {
            if !seen.0 { if let Some(v) = cdo.get("EditModePatternData").and_then(|v| v.as_object()) {
                let p = pkg.object_path(v);
                info.pattern = Some(p.split('.').next().unwrap_or(&p).to_string());
                seen.0 = true;
            } }
            if !seen.1 { if let Some(v) = cdo.get("ResourceType") {
                info.material = resource_type(v);
                seen.1 = true;
            } }
            if !seen.2 { if let Some(v) = cdo.get("CurrentBuildingLevel").and_then(|v| v.as_i64()) { info.level = v as i32; seen.2 = true; } }
            if !seen.3 { if let Some(v) = cdo.get("bIsPlayerBuildable").and_then(|v| v.as_bool()) { info.player_buildable = v; seen.3 = true; } }
            if !seen.4 { if let Some(v) = cdo.get("StaticMesh").and_then(|v| v.as_object()) {
                let p = pkg.object_path(v);
                info.static_mesh = Some(p.split('.').next().unwrap_or(&p).to_string());
                seen.4 = true;
            } }
            if !seen_attr { if let Some(k) = cdo.get("AttributeInitKey") {
                info.attr_category = k.field("AttributeInitCategory").and_then(|v| v.as_str()).unwrap_or("").to_owned();
                info.attr_subcategory = k.field("AttributeInitSubCategory").and_then(|v| v.as_str()).unwrap_or("").to_owned();
                seen_attr = true;
            } }
            if !seen_anim { if let Some(v) = cdo.get("AnimatingMaterial") {
                info.animating_material = v.as_object().map_or(false, |o| !o.is_null());
                seen_anim = true;
            } }
        }
        let sup = pkg.super_class().map(|r| r.path().to_string()).unwrap_or_default();
        if let Some(native) = sup.strip_prefix("/Script/FortniteGame.") {
            info.kind = match native {
                "BuildingWall" => Some(PieceKind::Wall),
                "BuildingFloor" => Some(PieceKind::Floor),
                "BuildingStairs" => Some(PieceKind::Stairs),
                "BuildingRoof" => Some(PieceKind::Roof),
                _ => None,
            };
            break;
        }
        if sup.starts_with("/Game/") {
            chain.push(sup.split('.').next().unwrap_or(&sup).to_string());
        } else {
            break;
        }
    }
    Ok(info)
}

fn resource_type(v: &fn_uasset::Value) -> Option<Material> {
    let s = match v {
        fn_uasset::Value::Enum { value, .. } => value.clone(),
        fn_uasset::Value::Name(n) => n.clone(),
        fn_uasset::Value::Byte(b) => return match b { 0 => Some(Material::Wood), 1 => Some(Material::Stone), 2 => Some(Material::Metal), _ => None },
        _ => return None,
    };
    match s.rsplit("::").next().unwrap_or(&s) {
        "Wood" => Some(Material::Wood),
        "Stone" => Some(Material::Stone),
        "Metal" => Some(Material::Metal),
        _ => None,
    }
}

/// Reads one EMP asset (first export) into a [`Pattern`]; missing `TileData` keeps the class default.
pub fn read_pattern(set: &fn_pak::PakSet, pkg_path: &str) -> Result<Pattern, String> {
    let data = fn_uasset::PackageData::load(set, pkg_path).map_err(|e| format!("{pkg_path}: {e}"))?;
    let pkg = data.package().map_err(|e| format!("{pkg_path}: {e}"))?;
    let obj = pkg.read_export(0).map_err(|e| format!("{pkg_path}: {e}"))?;
    let kind = match obj.class.as_str() {
        "BuildingEditModeMetadata_Wall" => PieceKind::Wall,
        "BuildingEditModeMetadata_Floor" => PieceKind::Floor,
        "BuildingEditModeMetadata_Roof" => PieceKind::Roof,
        "BuildingEditModeMetadata_Stair" => PieceKind::Stairs,
        other => return Err(format!("{pkg_path}: unsupported metadata class {other}")),
    };
    let mut p = Pattern::class_default(kind);
    if let Some(arr) = obj.get("TileData").and_then(|v| v.as_array()) {
        p.tiles = match p.tiles {
            Tiles::Binary(_) => Tiles::Binary(arr.iter().map(binary_value).collect::<Result<_, _>>().map_err(|e| format!("{pkg_path}: {e}"))?),
            Tiles::Stair(_) => Tiles::Stair(arr.iter().map(|v| v.as_i64().map(|x| x as i32).ok_or("stair tile not int")).collect::<Result<_, _>>().map_err(|e| format!("{pkg_path}: {e}"))?),
        };
    }
    Ok(p)
}

fn binary_value(v: &fn_uasset::Value) -> Result<u8, String> {
    match v {
        fn_uasset::Value::Byte(b) => Ok(*b),
        fn_uasset::Value::Enum { value, .. } | fn_uasset::Value::Name(value) => match value.rsplit("::").next().unwrap_or(value) {
            "BTV_Active" => Ok(BTV_ACTIVE),
            "BTV_Inactive" => Ok(BTV_INACTIVE),
            "BTV_Either" => Ok(BTV_EITHER),
            o => Err(format!("unknown EBinaryToggleValues {o}")),
        },
        o => Err(format!("unexpected tile value {o:?}")),
    }
}

/// `UStaticMesh::GetBounds` of a cooked mesh: (origin, extent).
pub fn mesh_bounds(set: &fn_pak::PakSet, long: &str) -> Result<(Vec3, Vec3), String> {
    let ua = set.read_game(long, "uasset").map_err(|e| format!("{long}: {e}"))?;
    let ue = set.read_game(long, "uexp").map_err(|e| format!("{long}: {e}"))?;
    let ub = set.read_game(long, "ubulk").map(|c| c.into_owned()).unwrap_or_default();
    let pkg = fn_ueio::Package::parse(&ua, &ue, &ub).map_err(|e| format!("{long}: {e:?}"))?;
    let m = fn_mesh::StaticMesh::parse(&pkg).map_err(|e| format!("{long}: {e:?}"))?;
    Ok((Vec3::from(m.bounds.origin), Vec3::from(m.bounds.extent)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Option<std::path::PathBuf> { fn_live::var_os("FN_TEST_ROOT").map(Into::into) }

    #[test]
    fn real_18_edit_data() {
        let Some(root) = root() else { eprintln!("FN_TEST_ROOT not set, skipping"); return };
        let set = fn_pak::PakSet::open_install(&root).unwrap();
        let db = EditDb::load(&set).unwrap();
        eprintln!("{} edit classes, {} patterns; glob hits {}", db.classes.len(), db.patterns.len(), set.glob(PLAYER_BP_GLOB).len());
        // Every material x level x kind group: patterns are unique and the grid size matches the kind.
        let mut groups: HashMap<(PieceKind, Material, i32), Vec<&EditClass>> = HashMap::new();
        for c in &db.classes { groups.entry((c.kind, c.material, c.level)).or_default().push(c); }
        assert!(groups.contains_key(&(PieceKind::Wall, Material::Wood, 0)));
        for ((kind, _, _), cs) in &groups {
            let pats: Vec<&Pattern> = cs.iter().filter_map(|c| db.pattern_of(c)).collect();
            for (i, a) in pats.iter().enumerate() {
                assert_eq!(a.kind, *kind);
                assert_eq!(a.len(), a.n * a.n);
                for b in &pats[i + 1..] { assert!(!a.equals(b), "ambiguous patterns in {kind:?}"); }
            }
        }
        let w1: Vec<&str> = db.possible_edit_classes(db.class("/Game/Building/ActorBlueprints/Player/Wood/L1/PBWA_W1_Solid").unwrap())
            .iter().map(|c| c.package.rsplit('/').next().unwrap()).collect();
        eprintln!("wood L1 wall edit classes: {w1:?}");
        assert!(w1.contains(&"PBWA_W1_Windows") && !w1.iter().any(|n| n.starts_with("PBWA_BG_")));
        for k in PieceKind::ALL { eprintln!("{k:?} layout {:?}", db.layout(k)); }
        // the stair indicator scalar names exist in the cooked indicator material (name table)
        let pd = fn_uasset::PackageData::load(&set, assets::STAIR_AUX_MATERIAL).unwrap();
        let pkg = pd.package().unwrap();
        for n in material_params::AUX_SCALARS { assert!(pkg.names.iter().any(|x| x == n), "{n} not in {}", assets::STAIR_AUX_MATERIAL); }
        assert!(db.classes.iter().all(|c| c.build.is_some()), "build numbers for every edit class");
    }
}
